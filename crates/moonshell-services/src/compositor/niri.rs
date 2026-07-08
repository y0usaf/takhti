//! The niri backend: the event-stream socket (`$NIRI_SOCKET`).
//!
//! One request (`"EventStream"`) on connect; niri replies with a
//! handshake ack and then streams ndjson events, opening with a full
//! state burst (`WorkspacesChanged` + `WindowsChanged`) — no separate
//! snapshot requests needed. Events are hand-parsed from
//! `serde_json::Value` instead of pulling the `niri-ipc` crate: the
//! handful of fields bars need is stable across niri versions, and
//! unknown events skip instead of failing deserialization on version
//! skew.
//!
//! Like every backend: nonblocking socket as a calloop `Generic`, no
//! reader thread, retry timer only while disconnected. `notify` fires
//! only when the mapped snapshot actually changed.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::io::Write;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::rc::Rc;

use calloop::generic::Generic;
use calloop::{Interest, LoopHandle, Mode, PostAction};
use serde_json::Value;

use super::wire::{self, RETRY};
use super::{CompositorState, Error, Notify, Workspace};

#[derive(Debug, Default, Clone, PartialEq)]
struct Ws {
    idx: u64,
    name: Option<String>,
    output: Option<String>,
    is_focused: bool,
}

#[derive(Debug, Clone, PartialEq)]
struct Win {
    title: Option<String>,
    workspace_id: Option<i64>,
}

/// The pure event model: niri events in, [`CompositorState`] out.
/// No sockets, no loop — unit-testable in isolation.
#[derive(Default)]
struct Model {
    workspaces: BTreeMap<i64, Ws>,
    windows: HashMap<u64, Win>,
    focused: Option<u64>,
}

impl Model {
    /// Apply one event frame (an object with a single variant key).
    /// Unknown variants — including the `{"Ok":…}` handshake ack —
    /// are skipped; whether the snapshot changed is decided by
    /// comparing [`Model::snapshot`] output, not here.
    fn apply(&mut self, frame: &Value) {
        let Some((name, payload)) = frame.as_object().and_then(|o| o.iter().next()) else {
            return;
        };
        match name.as_str() {
            "WorkspacesChanged" => {
                self.workspaces = payload
                    .get("workspaces")
                    .and_then(Value::as_array)
                    .map(|list| list.iter().filter_map(parse_workspace).collect())
                    .unwrap_or_default();
            }
            "WorkspaceActivated" => {
                let Some(id) = payload.get("id").and_then(Value::as_i64) else {
                    return;
                };
                let focused = payload
                    .get("focused")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                if focused {
                    for (wid, w) in self.workspaces.iter_mut() {
                        w.is_focused = *wid == id;
                    }
                }
            }
            "WindowsChanged" => {
                let list = payload.get("windows").and_then(Value::as_array);
                self.windows = list
                    .map(|l| l.iter().filter_map(parse_window).collect())
                    .unwrap_or_default();
                self.focused = list.and_then(|l| {
                    l.iter()
                        .find(|w| w.get("is_focused") == Some(&Value::Bool(true)))
                        .and_then(|w| w.get("id"))
                        .and_then(Value::as_u64)
                });
            }
            "WindowOpenedOrChanged" => {
                let Some(win) = payload.get("window") else {
                    return;
                };
                if let Some((id, w)) = parse_window(win) {
                    if win.get("is_focused") == Some(&Value::Bool(true)) {
                        self.focused = Some(id);
                    }
                    self.windows.insert(id, w);
                }
            }
            "WindowClosed" => {
                let Some(id) = payload.get("id").and_then(Value::as_u64) else {
                    return;
                };
                self.windows.remove(&id);
                if self.focused == Some(id) {
                    self.focused = None;
                }
            }
            "WindowFocusChanged" => {
                self.focused = payload.get("id").and_then(Value::as_u64);
            }
            _ => {}
        }
    }

    fn snapshot(&self) -> CompositorState {
        let active = self
            .workspaces
            .iter()
            .find(|(_, w)| w.is_focused)
            .map(|(id, _)| *id)
            .unwrap_or(0);
        let workspaces = self
            .workspaces
            .iter()
            .map(|(id, w)| Workspace {
                id: *id,
                name: w.name.clone().unwrap_or_else(|| w.idx.to_string()),
                active: w.is_focused,
                windows: self
                    .windows
                    .values()
                    .filter(|win| win.workspace_id == Some(*id))
                    .count() as u32,
            })
            .collect();
        CompositorState {
            connected: true,
            active_workspace: active,
            workspaces,
            active_window: self
                .focused
                .and_then(|id| self.windows.get(&id))
                .and_then(|w| w.title.clone()),
        }
    }
}

fn parse_workspace(w: &Value) -> Option<(i64, Ws)> {
    let id = w.get("id")?.as_i64()?;
    Some((
        id,
        Ws {
            idx: w.get("idx").and_then(Value::as_u64).unwrap_or(0),
            name: w.get("name").and_then(Value::as_str).map(str::to_owned),
            output: w.get("output").and_then(Value::as_str).map(str::to_owned),
            is_focused: w
                .get("is_focused")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        },
    ))
}

fn parse_window(w: &Value) -> Option<(u64, Win)> {
    let id = w.get("id")?.as_u64()?;
    Some((
        id,
        Win {
            title: w.get("title").and_then(Value::as_str).map(str::to_owned),
            workspace_id: w.get("workspace_id").and_then(Value::as_i64),
        },
    ))
}

struct Backend<D> {
    handle: LoopHandle<'static, D>,
    socket: PathBuf,
    notify: Notify<D>,
    model: Model,
    /// Last snapshot pushed — notify fires only on change.
    last: CompositorState,
    /// Unparsed tail of the ndjson stream.
    buf: Vec<u8>,
}

pub(super) fn start<D: 'static>(
    handle: LoopHandle<'static, D>,
    notify: Notify<D>,
) -> Result<(), Error> {
    let socket = std::env::var_os("NIRI_SOCKET")
        .map(PathBuf::from)
        .ok_or_else(|| Error::Loop("NIRI_SOCKET not set".into()))?;
    let backend = Rc::new(RefCell::new(Backend {
        handle,
        socket,
        notify,
        model: Model::default(),
        last: CompositorState::default(),
        buf: Vec::new(),
    }));
    if let Err(e) = try_connect(&backend) {
        tracing::warn!("niri IPC connect: {e}; retrying every {RETRY:?}");
        let be = backend.clone();
        let handle = backend.borrow().handle.clone();
        wire::arm_retry(&handle, "niri IPC", Rc::new(move || try_connect(&be)));
    }
    Ok(())
}

fn try_connect<D: 'static>(be: &Rc<RefCell<Backend<D>>>) -> std::io::Result<()> {
    let (socket, handle) = {
        let b = be.borrow();
        (b.socket.clone(), b.handle.clone())
    };
    let mut stream = UnixStream::connect(&socket)?;
    // The one request; niri streams state from here on. The write is
    // tiny and the socket buffer empty — a blocking write can't stall.
    stream.write_all(b"\"EventStream\"\n")?;
    stream.set_nonblocking(true)?;
    {
        let b = &mut *be.borrow_mut();
        b.buf.clear();
        b.model = Model::default();
    }

    let be = be.clone();
    handle
        .insert_source(
            Generic::new(stream, Interest::READ, Mode::Level),
            move |_, stream, data: &mut D| {
                let eof;
                {
                    let b = &mut *be.borrow_mut();
                    eof = wire::read_available(stream, &mut b.buf);
                    while let Some(line) = wire::take_line(&mut b.buf) {
                        match serde_json::from_slice::<Value>(&line) {
                            Ok(frame) => {
                                if let Some(err) = frame.get("Err") {
                                    tracing::warn!("niri IPC refused event stream: {err}");
                                } else {
                                    b.model.apply(&frame);
                                }
                            }
                            Err(e) => tracing::warn!("niri IPC: bad frame: {e}"),
                        }
                    }
                    let snap = if eof {
                        b.model = Model::default();
                        CompositorState::default()
                    } else {
                        b.model.snapshot()
                    };
                    if snap != b.last {
                        b.last = snap.clone();
                        (b.notify)(data, &snap);
                    }
                }
                if eof {
                    tracing::info!("niri IPC disconnected; retrying every {RETRY:?}");
                    let handle = be.borrow().handle.clone();
                    let be = be.clone();
                    wire::arm_retry(&handle, "niri IPC", Rc::new(move || try_connect(&be)));
                    Ok(PostAction::Remove)
                } else {
                    Ok(PostAction::Continue)
                }
            },
        )
        .map_err(|e| std::io::Error::other(e.to_string()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn apply(m: &mut Model, s: &str) {
        m.apply(&serde_json::from_str(s).unwrap());
    }

    fn burst(m: &mut Model) {
        apply(
            m,
            r#"{"WorkspacesChanged":{"workspaces":[
                {"id":1,"idx":1,"name":null,"output":"eDP-1",
                 "is_active":true,"is_focused":true,"active_window_id":10},
                {"id":2,"idx":2,"name":"web","output":"eDP-1",
                 "is_active":false,"is_focused":false,"active_window_id":null}]}}"#,
        );
        apply(
            m,
            r#"{"WindowsChanged":{"windows":[
                {"id":10,"title":"Alpha","app_id":"a","workspace_id":1,"is_focused":true},
                {"id":11,"title":"Beta","app_id":"b","workspace_id":1,"is_focused":false}]}}"#,
        );
    }

    #[test]
    fn initial_burst_maps_to_snapshot() {
        let mut m = Model::default();
        burst(&mut m);
        let s = m.snapshot();
        assert!(s.connected);
        assert_eq!(s.active_workspace, 1);
        assert_eq!(s.workspaces.len(), 2);
        assert_eq!(s.workspaces[0].name, "1", "unnamed falls back to idx");
        assert_eq!(s.workspaces[0].windows, 2);
        assert!(s.workspaces[0].active);
        assert_eq!(s.workspaces[1].name, "web");
        assert_eq!(s.workspaces[1].windows, 0);
        assert_eq!(s.active_window.as_deref(), Some("Alpha"));
    }

    #[test]
    fn workspace_activated_moves_focus() {
        let mut m = Model::default();
        burst(&mut m);
        apply(&mut m, r#"{"WorkspaceActivated":{"id":2,"focused":true}}"#);
        let s = m.snapshot();
        assert_eq!(s.active_workspace, 2);
        assert!(!s.workspaces[0].active);
        assert!(s.workspaces[1].active);
    }

    #[test]
    fn workspace_activated_unfocused_changes_nothing_focused() {
        // Another output's workspace became active there; focus stays.
        let mut m = Model::default();
        burst(&mut m);
        apply(&mut m, r#"{"WorkspaceActivated":{"id":2,"focused":false}}"#);
        assert_eq!(m.snapshot().active_workspace, 1);
    }

    #[test]
    fn window_focus_close_and_open() {
        let mut m = Model::default();
        burst(&mut m);
        apply(&mut m, r#"{"WindowFocusChanged":{"id":11}}"#);
        assert_eq!(m.snapshot().active_window.as_deref(), Some("Beta"));
        apply(&mut m, r#"{"WindowClosed":{"id":11}}"#);
        let s = m.snapshot();
        assert_eq!(s.active_window, None);
        assert_eq!(s.workspaces[0].windows, 1);
        apply(
            &mut m,
            r#"{"WindowOpenedOrChanged":{"window":
                {"id":12,"title":"Gamma","workspace_id":2,"is_focused":true}}}"#,
        );
        let s = m.snapshot();
        assert_eq!(s.active_window.as_deref(), Some("Gamma"));
        assert_eq!(s.workspaces[1].windows, 1);
    }

    #[test]
    fn unknown_and_handshake_frames_are_skipped() {
        let mut m = Model::default();
        burst(&mut m);
        let before = m.snapshot();
        apply(&mut m, r#"{"Ok":"Handled"}"#);
        apply(
            &mut m,
            r#"{"KeyboardLayoutsChanged":{"keyboard_layouts":{}}}"#,
        );
        assert_eq!(m.snapshot(), before);
    }
}
