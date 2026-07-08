//! The Hyprland backend: event lines on `.socket2.sock`, state
//! fetched from `.socket.sock` (nur's re-fetch-on-event model, minus
//! the thread and the `hyprland` crate — which drags in tokio).
//!
//! socket2 streams `event>>data\n` lines; none of them carry enough
//! state to maintain incrementally (workspace window counts,
//! especially), so any *relevant* event triggers one re-fetch of
//! `j/workspaces` + `j/activeworkspace` + `j/activewindow` over
//! socket1 — a fresh short-lived connection per request (Hyprland
//! closes it after replying), blocking with a 1 s timeout, millisecond
//! range in practice. All lines readable in one wakeup coalesce into
//! a single fetch. `notify` fires only when the snapshot changed.

use std::cell::RefCell;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use calloop::generic::Generic;
use calloop::timer::{TimeoutAction, Timer};
use calloop::{Interest, LoopHandle, Mode, PostAction};
use serde_json::Value;

use super::wire::{self, RETRY};
use super::{CompositorState, Error, Notify, Workspace};

/// Timeout on the blocking socket1 request/reply round trip.
const FETCH_TIMEOUT: Duration = Duration::from_secs(1);

/// Locate the instance's IPC directory: `$XDG_RUNTIME_DIR/hypr/$sig`
/// (modern), falling back to `/tmp/hypr/$sig` (pre-0.40).
fn ipc_dir() -> Option<PathBuf> {
    let sig = std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE")?;
    let mut candidates = Vec::new();
    if let Some(run) = std::env::var_os("XDG_RUNTIME_DIR") {
        candidates.push(Path::new(&run).join("hypr").join(&sig));
    }
    candidates.push(Path::new("/tmp/hypr").join(&sig));
    candidates
        .into_iter()
        .find(|d| d.join(".socket.sock").exists())
}

/// Does this socket2 line affect workspaces or the focused window?
fn relevant(line: &str) -> bool {
    let name = line.split(">>").next().unwrap_or("");
    matches!(
        name,
        "workspace"
            | "workspacev2"
            | "createworkspace"
            | "createworkspacev2"
            | "destroyworkspace"
            | "destroyworkspacev2"
            | "focusedmon"
            | "focusedmonv2"
            | "activewindow"
            | "openwindow"
            | "closewindow"
            | "movewindow"
            | "movewindowv2"
    )
}

/// Map the three `j/` replies to a snapshot. Special workspaces
/// (negative ids) are skipped, matching nur.
fn parse_state(workspaces: &Value, active: &Value, window: &Value) -> CompositorState {
    let active_id = active.get("id").and_then(Value::as_i64).unwrap_or(0);
    let mut list: Vec<Workspace> = workspaces
        .as_array()
        .map(|l| {
            l.iter()
                .filter_map(|w| {
                    let id = w.get("id")?.as_i64()?;
                    (id > 0).then(|| Workspace {
                        id,
                        name: w
                            .get("name")
                            .and_then(Value::as_str)
                            .map(str::to_owned)
                            .unwrap_or_else(|| id.to_string()),
                        active: id == active_id,
                        windows: w.get("windows").and_then(Value::as_u64).unwrap_or(0) as u32,
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    list.sort_by_key(|w| w.id);
    CompositorState {
        connected: true,
        active_workspace: active_id,
        workspaces: list,
        // No focused window → `j/activewindow` replies `{}`.
        active_window: window
            .get("title")
            .and_then(Value::as_str)
            .filter(|t| !t.is_empty())
            .map(str::to_owned),
    }
}

/// One socket1 request: fresh connection, write the command, read the
/// reply to EOF (Hyprland closes after answering).
fn request(ctl: &Path, cmd: &str) -> std::io::Result<Value> {
    let mut stream = UnixStream::connect(ctl)?;
    stream.set_read_timeout(Some(FETCH_TIMEOUT))?;
    stream.set_write_timeout(Some(FETCH_TIMEOUT))?;
    stream.write_all(cmd.as_bytes())?;
    let mut out = Vec::new();
    stream.read_to_end(&mut out)?;
    serde_json::from_slice(&out).map_err(std::io::Error::other)
}

fn fetch(ctl: &Path) -> std::io::Result<CompositorState> {
    let workspaces = request(ctl, "j/workspaces")?;
    let active = request(ctl, "j/activeworkspace")?;
    // `{}` when nothing is focused; treat a failed parse the same.
    let window = request(ctl, "j/activewindow").unwrap_or(Value::Null);
    Ok(parse_state(&workspaces, &active, &window))
}

struct Backend<D> {
    handle: LoopHandle<'static, D>,
    dir: PathBuf,
    notify: Notify<D>,
    last: CompositorState,
    /// Unparsed tail of the socket2 line stream.
    buf: Vec<u8>,
}

pub(super) fn start<D: 'static>(
    handle: LoopHandle<'static, D>,
    notify: Notify<D>,
) -> Result<(), Error> {
    let dir = ipc_dir().ok_or_else(|| {
        Error::Loop("no Hyprland IPC directory for $HYPRLAND_INSTANCE_SIGNATURE".into())
    })?;
    let backend = Rc::new(RefCell::new(Backend {
        handle,
        dir,
        notify,
        last: CompositorState::default(),
        buf: Vec::new(),
    }));
    if let Err(e) = try_connect(&backend) {
        tracing::warn!("Hyprland IPC connect: {e}; retrying every {RETRY:?}");
        let be = backend.clone();
        let handle = backend.borrow().handle.clone();
        wire::arm_retry(&handle, "Hyprland IPC", Rc::new(move || try_connect(&be)));
    }
    Ok(())
}

fn try_connect<D: 'static>(be: &Rc<RefCell<Backend<D>>>) -> std::io::Result<()> {
    let (dir, handle) = {
        let b = be.borrow();
        (b.dir.clone(), b.handle.clone())
    };
    let stream = UnixStream::connect(dir.join(".socket2.sock"))?;
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
                    while let Some(line) = wire::take_line(&mut b.buf) {
                        refetch |= relevant(&String::from_utf8_lossy(&line));
                    }
                    let snap = if eof {
                        Some(CompositorState::default())
                    } else if refetch {
                        match fetch(&b.dir) {
                            Ok(s) => Some(s),
                            Err(e) => {
                                // Keep the last state; if the whole
                                // compositor is gone the event socket
                                // EOFs and resets it right after.
                                tracing::warn!("Hyprland IPC fetch: {e}");
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
                    tracing::info!("Hyprland IPC disconnected; retrying every {RETRY:?}");
                    let handle = source.borrow().handle.clone();
                    let be = source.clone();
                    wire::arm_retry(&handle, "Hyprland IPC", Rc::new(move || try_connect(&be)));
                    Ok(PostAction::Remove)
                } else {
                    Ok(PostAction::Continue)
                }
            },
        )
        .map_err(|e| std::io::Error::other(e.to_string()))?;

    // Seed state — the event socket only speaks on change, and
    // `notify` needs the loop's `&mut D`, so the initial fetch rides
    // an immediate one-shot timer instead of running here.
    let seed = be.clone();
    handle
        .insert_source(Timer::immediate(), move |_, _, data: &mut D| {
            let b = &mut *seed.borrow_mut();
            match fetch(&b.dir) {
                Ok(snap) => {
                    if snap != b.last {
                        b.last = snap.clone();
                        (b.notify)(data, &snap);
                    }
                }
                Err(e) => tracing::warn!("Hyprland IPC initial fetch: {e}"),
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
    fn relevant_classifies_lines() {
        assert!(relevant("workspace>>2"));
        assert!(relevant("workspacev2>>2,2"));
        assert!(relevant("activewindow>>kitty,~/dev"));
        assert!(relevant("openwindow>>80e62e0,2,kitty,~/dev"));
        assert!(relevant("focusedmon>>DP-1,3"));
        assert!(!relevant("monitoradded>>DP-2"));
        assert!(!relevant("screencast>>1,0"));
        assert!(!relevant(""));
    }

    #[test]
    fn parse_state_maps_and_filters_special() {
        let ws: Value = serde_json::from_str(
            r#"[{"id":2,"name":"2","monitor":"DP-1","windows":3},
                {"id":-98,"name":"special:magic","monitor":"DP-1","windows":1},
                {"id":1,"name":"one","monitor":"DP-1","windows":0}]"#,
        )
        .unwrap();
        let active: Value = serde_json::from_str(r#"{"id":2,"name":"2"}"#).unwrap();
        let win: Value = serde_json::from_str(r#"{"address":"0x1","title":"Alpha"}"#).unwrap();
        let s = parse_state(&ws, &active, &win);
        assert!(s.connected);
        assert_eq!(s.active_workspace, 2);
        assert_eq!(s.workspaces.len(), 2, "special workspace skipped");
        assert_eq!(s.workspaces[0].id, 1, "sorted by id");
        assert_eq!(s.workspaces[0].name, "one");
        assert!(!s.workspaces[0].active);
        assert!(s.workspaces[1].active);
        assert_eq!(s.workspaces[1].windows, 3);
        assert_eq!(s.active_window.as_deref(), Some("Alpha"));
    }

    #[test]
    fn parse_state_no_focused_window() {
        let ws: Value = serde_json::from_str(r#"[{"id":1,"name":"1","windows":0}]"#).unwrap();
        let active: Value = serde_json::from_str(r#"{"id":1}"#).unwrap();
        let s = parse_state(&ws, &active, &serde_json::json!({}));
        assert_eq!(s.active_window, None);
        let s = parse_state(&ws, &active, &Value::Null);
        assert_eq!(s.active_window, None);
    }
}
