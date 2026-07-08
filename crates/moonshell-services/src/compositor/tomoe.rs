//! The tomoe backend: moonshell as the doctrine-03 thin client, and
//! the first external consumer of the `tomoe-ipc` wire crate.
//!
//! Wire discipline: `subscribe` is sent *first*, then the snapshot
//! requests (`windows`, `wm_state`) — an event racing the snapshot is
//! applied on top of it instead of being lost. After connect the
//! socket is nonblocking and lives as a calloop `Generic` source: no
//! reader thread, no idle wakeups while the compositor is quiet.
//!
//! Vocabulary split (doctrine 03): `window_open`/`window_close`/
//! `focus_change` are core tomoe events; `wm_state` (workspace list +
//! active) is *policy* served and broadcast by tomoe's `wm.lua` — a
//! custom tomoe config that doesn't serve it degrades to an empty
//! workspace list, warned once.
//!
//! Disconnect (tomoe restart) resets the state to `Default`
//! (`connected = false`), notifies, and arms a retry timer — the only
//! periodic wakeup, and only while disconnected.

use std::cell::RefCell;
use std::collections::HashMap;
use std::io::{ErrorKind, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use calloop::generic::Generic;
use calloop::timer::{TimeoutAction, Timer};
use calloop::{Interest, LoopHandle, Mode, PostAction};
use serde_json::{json, Value};

use super::{CompositorState, Error, Workspace};

/// Reconnect cadence while the compositor is gone.
const RETRY: Duration = Duration::from_secs(2);
/// Connect-time snapshot request ids (the only requests we ever send).
const ID_WINDOWS: u64 = 1;
const ID_WM_STATE: u64 = 2;

type Notify<D> = Box<dyn FnMut(&mut D, &CompositorState)>;

/// The pure protocol model: frames in, state out. No sockets, no
/// loop — unit-testable in isolation.
#[derive(Default)]
struct Model {
    state: CompositorState,
    /// window id → title, maintained from the `windows` snapshot plus
    /// `window_open`/`window_close`. Title *changes* after open don't
    /// propagate yet (no core event for them — tracked in tomoe's
    /// "moonshell-driven" list).
    titles: HashMap<u64, String>,
    focused: Option<u64>,
    /// `wm_state` errored — warn once, not per retry.
    warned_no_wm: bool,
}

impl Model {
    /// Apply one wire frame. Returns true if the snapshot changed.
    fn handle_frame(&mut self, frame: &Value) -> bool {
        let changed = if let Some(event) = frame.get("event").and_then(Value::as_str) {
            self.handle_event(event, frame.get("payload").unwrap_or(&Value::Null))
        } else {
            match frame.get("id").and_then(Value::as_u64) {
                Some(ID_WINDOWS) => match response(frame) {
                    Ok(result) => self.apply_windows(result),
                    Err(e) => {
                        tracing::warn!("tomoe IPC: windows request failed: {e}");
                        false
                    }
                },
                Some(ID_WM_STATE) => match response(frame) {
                    Ok(result) => self.apply_wm_state(result),
                    Err(e) => {
                        if !self.warned_no_wm {
                            self.warned_no_wm = true;
                            tracing::warn!(
                                "tomoe IPC: wm_state not served ({e}) — the running config \
                                 doesn't broadcast workspaces (wm.lua does); workspace list \
                                 stays empty"
                            );
                        }
                        false
                    }
                },
                _ => false,
            }
        };
        if changed {
            self.state.connected = true;
            self.state.active_window = self.focused.and_then(|id| self.titles.get(&id).cloned());
        }
        changed
    }

    fn handle_event(&mut self, event: &str, payload: &Value) -> bool {
        match event {
            "wm_state" => self.apply_wm_state(payload),
            "window_open" => match serde_json::from_value::<tomoe_ipc::Window>(payload.clone()) {
                Ok(win) => {
                    if win.focused {
                        self.focused = Some(win.id);
                    }
                    self.titles.insert(win.id, win.title);
                    true
                }
                Err(e) => {
                    tracing::warn!("tomoe IPC: bad window_open payload: {e}");
                    false
                }
            },
            "window_close" => {
                let Some(id) = payload.get("id").and_then(Value::as_u64) else {
                    return false;
                };
                self.titles.remove(&id);
                if self.focused == Some(id) {
                    self.focused = None;
                }
                true
            }
            "focus_change" => {
                self.focused = payload.get("id").and_then(Value::as_u64);
                true
            }
            _ => false,
        }
    }

    fn apply_windows(&mut self, result: &Value) -> bool {
        let Ok(windows) = serde_json::from_value::<Vec<tomoe_ipc::Window>>(result.clone()) else {
            tracing::warn!("tomoe IPC: bad windows response");
            return false;
        };
        self.focused = windows.iter().find(|w| w.focused).map(|w| w.id);
        self.titles = windows.into_iter().map(|w| (w.id, w.title)).collect();
        true
    }

    /// `{ active = n, workspaces = { { id, windows }, ... } }` — the
    /// vocabulary wm.lua serves and broadcasts.
    fn apply_wm_state(&mut self, payload: &Value) -> bool {
        let active = payload.get("active").and_then(Value::as_i64).unwrap_or(1);
        self.state.active_workspace = active;
        self.state.workspaces = payload
            .get("workspaces")
            .and_then(Value::as_array)
            .map(|list| {
                list.iter()
                    .filter_map(|w| {
                        let id = w.get("id")?.as_i64()?;
                        Some(Workspace {
                            id,
                            name: id.to_string(),
                            active: id == active,
                            windows: w.get("windows").and_then(Value::as_u64).unwrap_or(0) as u32,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        true
    }

    /// The compositor went away: back to `Default` (`connected =
    /// false`), so bars render the disconnect instead of stale state.
    fn disconnect(&mut self) {
        let warned = self.warned_no_wm;
        *self = Model::default();
        self.warned_no_wm = warned;
    }
}

/// Result frame → `result` value or the server's error string.
fn response(frame: &Value) -> Result<&Value, String> {
    if let Some(err) = frame.get("error").and_then(Value::as_str) {
        return Err(err.to_string());
    }
    Ok(frame.get("result").unwrap_or(&Value::Null))
}

struct Backend<D> {
    handle: LoopHandle<'static, D>,
    socket: PathBuf,
    notify: Notify<D>,
    model: Model,
    /// Unparsed tail of the ndjson stream.
    buf: Vec<u8>,
}

pub(super) fn start<D: 'static>(
    handle: LoopHandle<'static, D>,
    notify: Notify<D>,
) -> Result<(), Error> {
    let socket = tomoe_ipc::find_socket()
        .ok_or_else(|| Error::Loop("no $TOMOE_SOCKET and no $WAYLAND_DISPLAY".into()))?;
    let backend = Rc::new(RefCell::new(Backend {
        handle,
        socket,
        notify,
        model: Model::default(),
        buf: Vec::new(),
    }));
    if let Err(e) = try_connect(&backend) {
        tracing::warn!("tomoe IPC connect: {e}; retrying every {RETRY:?}");
        arm_retry(&backend);
    }
    Ok(())
}

/// Connect, send the subscribe + snapshot requests, and insert the
/// nonblocking socket as a `Generic` source.
fn try_connect<D: 'static>(be: &Rc<RefCell<Backend<D>>>) -> std::io::Result<()> {
    let (socket, handle) = {
        let b = be.borrow();
        (b.socket.clone(), b.handle.clone())
    };
    let mut stream = UnixStream::connect(&socket)?;
    // Subscribe first: events that race the snapshots get applied on
    // top of them, never lost. The requests are tiny and the socket
    // buffer is empty — blocking writes here cannot stall.
    for line in [
        json!({ "method": "subscribe", "params": { "events":
            ["wm_state", "window_open", "window_close", "focus_change"] } }),
        json!({ "id": ID_WINDOWS, "method": "windows" }),
        json!({ "id": ID_WM_STATE, "method": "wm_state" }),
    ] {
        let mut msg = line.to_string();
        msg.push('\n');
        stream.write_all(msg.as_bytes())?;
    }
    stream.set_nonblocking(true)?;
    be.borrow_mut().buf.clear();

    let be = be.clone();
    handle
        .insert_source(
            Generic::new(stream, Interest::READ, Mode::Level),
            move |_, stream, data: &mut D| {
                let mut eof = false;
                let mut changed = false;
                {
                    let b = &mut *be.borrow_mut();
                    loop {
                        let mut chunk = [0u8; 4096];
                        // NoIoDrop has no DerefMut; &UnixStream is Read.
                        match (&**stream).read(&mut chunk) {
                            Ok(0) => {
                                eof = true;
                                break;
                            }
                            Ok(n) => b.buf.extend_from_slice(&chunk[..n]),
                            Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                            Err(e) if e.kind() == ErrorKind::Interrupted => continue,
                            Err(e) => {
                                tracing::warn!("tomoe IPC read: {e}");
                                eof = true;
                                break;
                            }
                        }
                    }
                    while let Some(line) = take_line(&mut b.buf) {
                        match serde_json::from_slice::<Value>(&line) {
                            Ok(frame) => changed |= b.model.handle_frame(&frame),
                            Err(e) => tracing::warn!("tomoe IPC: bad frame: {e}"),
                        }
                    }
                    if eof {
                        b.model.disconnect();
                        changed = true;
                    }
                    if changed {
                        let state = b.model.state.clone();
                        (b.notify)(data, &state);
                    }
                }
                if eof {
                    tracing::info!("tomoe IPC disconnected; retrying every {RETRY:?}");
                    arm_retry(&be);
                    Ok(PostAction::Remove)
                } else {
                    Ok(PostAction::Continue)
                }
            },
        )
        .map_err(|e| std::io::Error::other(e.to_string()))?;
    Ok(())
}

/// The reconnect timer — the only periodic wakeup, alive only while
/// the compositor is unreachable.
fn arm_retry<D: 'static>(be: &Rc<RefCell<Backend<D>>>) {
    let handle = be.borrow().handle.clone();
    let be = be.clone();
    let armed = handle.insert_source(
        Timer::from_duration(RETRY),
        move |_, _, _| match try_connect(&be) {
            Ok(()) => {
                tracing::info!("tomoe IPC reconnected");
                TimeoutAction::Drop
            }
            Err(_) => TimeoutAction::ToDuration(RETRY),
        },
    );
    if let Err(e) = armed {
        tracing::error!("tomoe IPC: arming retry timer: {e} — reconnect disabled");
    }
}

/// Split one `\n`-terminated line off the front of `buf`.
fn take_line(buf: &mut Vec<u8>) -> Option<Vec<u8>> {
    let pos = buf.iter().position(|&b| b == b'\n')?;
    let rest = buf.split_off(pos + 1);
    let mut line = std::mem::replace(buf, rest);
    line.pop(); // the newline
    Some(line)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(s: &str) -> Value {
        serde_json::from_str(s).unwrap()
    }

    #[test]
    fn take_line_splits_and_keeps_tail() {
        let mut buf = b"one\ntwo\nthr".to_vec();
        assert_eq!(take_line(&mut buf).as_deref(), Some(&b"one"[..]));
        assert_eq!(take_line(&mut buf).as_deref(), Some(&b"two"[..]));
        assert_eq!(take_line(&mut buf), None);
        assert_eq!(buf, b"thr");
    }

    #[test]
    fn wm_state_event_populates_workspaces() {
        let mut m = Model::default();
        assert!(m.handle_frame(&frame(
            r#"{"event":"wm_state","payload":{"active":2,
                "workspaces":[{"id":1,"windows":0},{"id":2,"windows":3}]}}"#
        )));
        assert!(m.state.connected);
        assert_eq!(m.state.active_workspace, 2);
        assert_eq!(m.state.workspaces.len(), 2);
        assert!(!m.state.workspaces[0].active);
        assert!(m.state.workspaces[1].active);
        assert_eq!(m.state.workspaces[1].windows, 3);
        assert_eq!(m.state.workspaces[1].name, "2");
    }

    #[test]
    fn focus_tracking_from_snapshot_and_events() {
        let mut m = Model::default();
        // Snapshot: two windows, second focused.
        assert!(m.handle_frame(&frame(
            r#"{"id":1,"result":[
                {"id":10,"app_id":"a","title":"Alpha","geometry":null,
                 "mapped":true,"focused":false,"fullscreen":false,"maximized":false},
                {"id":11,"app_id":"b","title":"Beta","geometry":null,
                 "mapped":true,"focused":true,"fullscreen":false,"maximized":false}]}"#
        )));
        assert_eq!(m.state.active_window.as_deref(), Some("Beta"));
        // Focus moves to the first.
        assert!(m.handle_frame(&frame(r#"{"event":"focus_change","payload":{"id":10}}"#)));
        assert_eq!(m.state.active_window.as_deref(), Some("Alpha"));
        // It closes; nothing focused.
        assert!(m.handle_frame(&frame(r#"{"event":"window_close","payload":{"id":10}}"#)));
        assert_eq!(m.state.active_window, None);
        // A new focused window opens.
        assert!(m.handle_frame(&frame(
            r#"{"event":"window_open","payload":
                {"id":12,"app_id":"c","title":"Gamma","geometry":null,
                 "mapped":true,"focused":true,"fullscreen":false,"maximized":false}}"#
        )));
        assert_eq!(m.state.active_window.as_deref(), Some("Gamma"));
    }

    #[test]
    fn wm_state_error_degrades_quietly() {
        let mut m = Model::default();
        assert!(!m.handle_frame(&frame(r#"{"id":2,"error":"unknown method: wm_state"}"#)));
        assert!(m.warned_no_wm);
        assert!(m.state.workspaces.is_empty());
        // Core events still work without wm.lua.
        assert!(m.handle_frame(&frame(r#"{"event":"focus_change","payload":{"id":null}}"#)));
        assert!(m.state.connected);
    }

    #[test]
    fn disconnect_resets_but_keeps_warning() {
        let mut m = Model::default();
        m.handle_frame(&frame(r#"{"id":2,"error":"unknown method: wm_state"}"#));
        m.handle_frame(&frame(
            r#"{"event":"wm_state","payload":{"active":1,"workspaces":[{"id":1,"windows":1}]}}"#,
        ));
        m.disconnect();
        assert_eq!(m.state, CompositorState::default());
        assert!(!m.state.connected);
        assert!(m.warned_no_wm, "warn-once survives reconnects");
    }

    #[test]
    fn unknown_frames_change_nothing() {
        let mut m = Model::default();
        assert!(!m.handle_frame(&frame(r#"{"event":"outputs_changed","payload":[]}"#)));
        assert!(!m.handle_frame(&frame(r#"{"id":99,"result":true}"#)));
        assert!(!m.state.connected);
    }
}
