//! The Sway / i3 backend: the i3 IPC socket (`$SWAYSOCK` / `$I3SOCK`).
//!
//! i3 IPC frames are binary: `"i3-ipc"` + payload length + message
//! type (both u32, native endian), then a JSON payload. One
//! connection subscribes to `workspace` + `window` events and lives
//! as a nonblocking calloop `Generic`; any event triggers a re-fetch
//! of `GET_WORKSPACES` + `GET_TREE` on a fresh blocking connection
//! (1 s timeout) — nur's re-fetch model, minus the thread and the
//! `swayipc` crate. Window counts and the focused title come from the
//! tree walk. `notify` fires only when the snapshot changed.

use std::cell::RefCell;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use calloop::generic::Generic;
use calloop::timer::{TimeoutAction, Timer};
use calloop::{Interest, LoopHandle, Mode, PostAction};
use serde_json::Value;

use super::wire::{self, RETRY};
use super::{CompositorState, Error, Notify, Workspace};

const MAGIC: &[u8; 6] = b"i3-ipc";
const HEADER: usize = 14; // magic + len + type
const GET_WORKSPACES: u32 = 1;
const SUBSCRIBE: u32 = 2;
const GET_TREE: u32 = 4;
/// Events echo their type with the high bit set.
const EVENT_BIT: u32 = 1 << 31;
const EV_WORKSPACE: u32 = 0;
const EV_WINDOW: u32 = 3;

const FETCH_TIMEOUT: Duration = Duration::from_secs(1);

fn frame(ty: u32, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(HEADER + payload.len());
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&(payload.len() as u32).to_ne_bytes());
    out.extend_from_slice(&ty.to_ne_bytes());
    out.extend_from_slice(payload);
    out
}

/// Split one complete frame off the front of `buf`. A magic mismatch
/// means the stream desynced — drop the buffer and let EOF/reconnect
/// recover.
fn take_frame(buf: &mut Vec<u8>) -> Option<(u32, Vec<u8>)> {
    if buf.len() < HEADER {
        return None;
    }
    if &buf[..6] != MAGIC {
        tracing::warn!("sway IPC: stream desynced (bad magic); dropping buffer");
        buf.clear();
        return None;
    }
    let len = u32::from_ne_bytes([buf[6], buf[7], buf[8], buf[9]]) as usize;
    let ty = u32::from_ne_bytes([buf[10], buf[11], buf[12], buf[13]]);
    if buf.len() < HEADER + len {
        return None;
    }
    let rest = buf.split_off(HEADER + len);
    let payload = std::mem::replace(buf, rest).split_off(HEADER);
    Some((ty, payload))
}

/// Blocking read of exactly one frame (query connections only).
fn read_frame(stream: &mut UnixStream) -> std::io::Result<(u32, Vec<u8>)> {
    let mut header = [0u8; HEADER];
    stream.read_exact(&mut header)?;
    if &header[..6] != MAGIC {
        return Err(std::io::Error::other("sway IPC: bad magic"));
    }
    let len = u32::from_ne_bytes([header[6], header[7], header[8], header[9]]) as usize;
    let ty = u32::from_ne_bytes([header[10], header[11], header[12], header[13]]);
    let mut payload = vec![0u8; len];
    stream.read_exact(&mut payload)?;
    Ok((ty, payload))
}

/// Walk the layout tree: leaf-view counts per workspace name, plus
/// the focused view's title. Workspace nodes are the scope markers;
/// only con/floating_con nodes count as views (a focused *workspace*
/// node — empty workspace — is not a window title).
fn walk_tree(
    node: &Value,
    ws: Option<&str>,
    counts: &mut HashMap<String, u32>,
    title: &mut Option<String>,
) {
    let ty = node.get("type").and_then(Value::as_str).unwrap_or("");
    let name = node.get("name").and_then(Value::as_str);
    let ws = if ty == "workspace" { name } else { ws };
    if matches!(ty, "con" | "floating_con") {
        let leaf = node
            .get("nodes")
            .and_then(Value::as_array)
            .is_none_or(Vec::is_empty)
            && node
                .get("floating_nodes")
                .and_then(Value::as_array)
                .is_none_or(Vec::is_empty);
        if leaf {
            if let Some(ws) = ws {
                *counts.entry(ws.to_owned()).or_default() += 1;
            }
        }
        if node.get("focused") == Some(&Value::Bool(true)) {
            *title = name.map(str::to_owned);
        }
    }
    for key in ["nodes", "floating_nodes"] {
        if let Some(children) = node.get(key).and_then(Value::as_array) {
            for child in children {
                walk_tree(child, ws, counts, title);
            }
        }
    }
}

/// Map `GET_WORKSPACES` + `GET_TREE` replies to a snapshot.
fn map_state(workspaces: &Value, tree: &Value) -> CompositorState {
    let mut counts = HashMap::new();
    let mut title = None;
    walk_tree(tree, None, &mut counts, &mut title);
    let list: Vec<Workspace> = workspaces
        .as_array()
        .map(|l| {
            l.iter()
                .filter_map(|w| {
                    let name = w.get("name").and_then(Value::as_str)?;
                    Some(Workspace {
                        id: w.get("num").and_then(Value::as_i64).unwrap_or(-1),
                        name: name.to_owned(),
                        active: w.get("focused") == Some(&Value::Bool(true)),
                        windows: counts.get(name).copied().unwrap_or(0),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let active = list.iter().find(|w| w.active).map(|w| w.id).unwrap_or(0);
    CompositorState {
        connected: true,
        active_workspace: active,
        workspaces: list,
        active_window: title,
    }
}

fn socket_path() -> Option<PathBuf> {
    std::env::var_os("SWAYSOCK")
        .or_else(|| std::env::var_os("I3SOCK"))
        .map(PathBuf::from)
}

fn fetch(socket: &PathBuf) -> std::io::Result<CompositorState> {
    let mut stream = UnixStream::connect(socket)?;
    stream.set_read_timeout(Some(FETCH_TIMEOUT))?;
    stream.set_write_timeout(Some(FETCH_TIMEOUT))?;
    stream.write_all(&frame(GET_WORKSPACES, b""))?;
    let (_, ws) = read_frame(&mut stream)?;
    stream.write_all(&frame(GET_TREE, b""))?;
    let (_, tree) = read_frame(&mut stream)?;
    let ws: Value = serde_json::from_slice(&ws).map_err(std::io::Error::other)?;
    let tree: Value = serde_json::from_slice(&tree).map_err(std::io::Error::other)?;
    Ok(map_state(&ws, &tree))
}

struct Backend<D> {
    handle: LoopHandle<'static, D>,
    socket: PathBuf,
    notify: Notify<D>,
    last: CompositorState,
    /// Unparsed tail of the event-frame stream.
    buf: Vec<u8>,
}

pub(super) fn start<D: 'static>(
    handle: LoopHandle<'static, D>,
    notify: Notify<D>,
) -> Result<(), Error> {
    let socket = socket_path().ok_or_else(|| Error::Loop("SWAYSOCK/I3SOCK not set".into()))?;
    let backend = Rc::new(RefCell::new(Backend {
        handle,
        socket,
        notify,
        last: CompositorState::default(),
        buf: Vec::new(),
    }));
    if let Err(e) = try_connect(&backend) {
        tracing::warn!("sway IPC connect: {e}; retrying every {RETRY:?}");
        let be = backend.clone();
        let handle = backend.borrow().handle.clone();
        wire::arm_retry(&handle, "sway IPC", Rc::new(move || try_connect(&be)));
    }
    Ok(())
}

fn try_connect<D: 'static>(be: &Rc<RefCell<Backend<D>>>) -> std::io::Result<()> {
    let (socket, handle) = {
        let b = be.borrow();
        (b.socket.clone(), b.handle.clone())
    };
    let mut stream = UnixStream::connect(&socket)?;
    // Tiny write on an empty socket buffer — cannot stall. The
    // `{"success":true}` reply arrives through the Generic callback.
    stream.write_all(&frame(SUBSCRIBE, br#"["workspace","window"]"#))?;
    stream.set_nonblocking(true)?;
    be.borrow_mut().buf.clear();

    let source = be.clone();
    handle
        .insert_source(
            Generic::new(stream, Interest::READ, Mode::Level),
            move |_, stream, data: &mut D| {
                let eof;
                {
                    let b = &mut *source.borrow_mut();
                    eof = wire::read_available(stream, &mut b.buf);
                    let mut refetch = false;
                    while let Some((ty, payload)) = take_frame(&mut b.buf) {
                        if ty & EVENT_BIT != 0 {
                            refetch |= matches!(ty & !EVENT_BIT, EV_WORKSPACE | EV_WINDOW);
                        } else if ty == SUBSCRIBE {
                            let ok = serde_json::from_slice::<Value>(&payload)
                                .ok()
                                .and_then(|v| v.get("success").and_then(Value::as_bool))
                                .unwrap_or(false);
                            if !ok {
                                tracing::warn!("sway IPC: subscribe refused");
                            }
                        }
                    }
                    let snap = if eof {
                        Some(CompositorState::default())
                    } else if refetch {
                        match fetch(&b.socket) {
                            Ok(s) => Some(s),
                            Err(e) => {
                                // Keep the last state; a dead
                                // compositor EOFs the event socket
                                // and resets it right after.
                                tracing::warn!("sway IPC fetch: {e}");
                                None
                            }
                        }
                    } else {
                        None
                    };
                    if let Some(snap) = snap {
                        if snap != b.last {
                            b.last = snap.clone();
                            (b.notify)(data, &snap);
                        }
                    }
                }
                if eof {
                    tracing::info!("sway IPC disconnected; retrying every {RETRY:?}");
                    let handle = source.borrow().handle.clone();
                    let be = source.clone();
                    wire::arm_retry(&handle, "sway IPC", Rc::new(move || try_connect(&be)));
                    Ok(PostAction::Remove)
                } else {
                    Ok(PostAction::Continue)
                }
            },
        )
        .map_err(|e| std::io::Error::other(e.to_string()))?;

    // Seed state — events only speak on change, and `notify` needs
    // the loop's `&mut D`, so the initial fetch rides an immediate
    // one-shot timer.
    let seed = be.clone();
    handle
        .insert_source(Timer::immediate(), move |_, _, data: &mut D| {
            let b = &mut *seed.borrow_mut();
            match fetch(&b.socket) {
                Ok(snap) => {
                    if snap != b.last {
                        b.last = snap.clone();
                        (b.notify)(data, &snap);
                    }
                }
                Err(e) => tracing::warn!("sway IPC initial fetch: {e}"),
            }
            TimeoutAction::Drop
        })
        .map_err(|e| std::io::Error::other(e.to_string()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_roundtrip() {
        let mut buf = frame(SUBSCRIBE, br#"["workspace"]"#);
        let tail = frame(EVENT_BIT | EV_WINDOW, b"{}");
        buf.extend_from_slice(&tail[..10]); // partial second frame
        let (ty, payload) = take_frame(&mut buf).expect("first frame complete");
        assert_eq!(ty, SUBSCRIBE);
        assert_eq!(payload, br#"["workspace"]"#);
        assert_eq!(take_frame(&mut buf), None, "second frame incomplete");
        buf.extend_from_slice(&tail[10..]);
        let (ty, payload) = take_frame(&mut buf).expect("second frame now complete");
        assert_eq!(ty, EVENT_BIT | EV_WINDOW);
        assert_eq!(payload, b"{}");
        assert!(buf.is_empty());
    }

    #[test]
    fn bad_magic_drops_buffer() {
        let mut buf = b"not-i3-ipc-at-all".to_vec();
        assert_eq!(take_frame(&mut buf), None);
        assert!(buf.is_empty());
    }

    #[test]
    fn map_state_counts_and_title() {
        let ws: Value = serde_json::from_str(
            r#"[{"num":1,"name":"1","focused":false,"visible":true},
                {"num":2,"name":"2","focused":true,"visible":true}]"#,
        )
        .unwrap();
        let tree: Value = serde_json::from_str(
            r#"{"type":"root","nodes":[{"type":"output","name":"eDP-1","nodes":[
                {"type":"workspace","name":"1","nodes":[
                    {"type":"con","name":null,"nodes":[
                        {"type":"con","name":"Alpha","nodes":[],"floating_nodes":[]},
                        {"type":"con","name":"Beta","nodes":[],"floating_nodes":[]}],
                     "floating_nodes":[]}],
                 "floating_nodes":[]},
                {"type":"workspace","name":"2","nodes":[
                    {"type":"con","name":"Gamma","focused":true,"nodes":[],"floating_nodes":[]}],
                 "floating_nodes":[]}]}]}"#,
        )
        .unwrap();
        let s = map_state(&ws, &tree);
        assert!(s.connected);
        assert_eq!(s.active_workspace, 2);
        assert_eq!(s.workspaces[0].windows, 2, "split container not counted");
        assert_eq!(s.workspaces[1].windows, 1);
        assert!(s.workspaces[1].active);
        assert_eq!(s.active_window.as_deref(), Some("Gamma"));
    }

    #[test]
    fn focused_empty_workspace_is_not_a_title() {
        let ws: Value = serde_json::from_str(r#"[{"num":3,"name":"3","focused":true}]"#).unwrap();
        let tree: Value = serde_json::from_str(
            r#"{"type":"root","nodes":[{"type":"output","name":"eDP-1","nodes":[
                {"type":"workspace","name":"3","focused":true,
                 "nodes":[],"floating_nodes":[]}]}]}"#,
        )
        .unwrap();
        let s = map_state(&ws, &tree);
        assert_eq!(s.active_window, None);
        assert_eq!(s.workspaces[0].windows, 0);
    }
}
