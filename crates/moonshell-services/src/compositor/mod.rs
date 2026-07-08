//! Compositor IPC — workspaces and the focused window, for bars.
//!
//! Auto-detects the running compositor and starts the matching backend
//! (doctrine 05: every backend is one module with one `start`, all
//! pushing [`CompositorState`] snapshots through the same callback).
//! tomoe is detected first (`$TOMOE_SOCKET`, or its derived socket
//! path existing), then Hyprland > niri > Sway (nur's precedence).
//! Every backend is thread-free: an event socket as a nonblocking
//! calloop `Generic`, a retry timer only while disconnected.

pub mod hyprland;
pub mod niri;
pub mod sway;
pub mod tomoe;
mod wire;

use calloop::LoopHandle;

/// The snapshot callback every backend pushes through.
type Notify<D> = Box<dyn FnMut(&mut D, &CompositorState)>;

/// One workspace as a bar sees it. Which workspaces to *display*
/// (all, occupied-only, …) is the widget's policy — backends report
/// everything they know.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Workspace {
    pub id: i64,
    pub name: String,
    pub active: bool,
    /// Number of windows on the workspace (0 = empty).
    pub windows: u32,
}

/// The snapshot pushed to `notify` on every change (nur's
/// `CompositorState` shape, plus `connected` so bars can show a
/// disconnected compositor honestly).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CompositorState {
    pub connected: bool,
    pub active_workspace: i64,
    pub workspaces: Vec<Workspace>,
    /// Focused window title, if any window is focused.
    pub active_window: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Compositor {
    Tomoe,
    Niri,
    Hyprland,
    Sway,
}

impl std::fmt::Display for Compositor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Compositor::Tomoe => "tomoe",
            Compositor::Niri => "niri",
            Compositor::Hyprland => "Hyprland",
            Compositor::Sway => "sway",
        })
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("event loop: {0}")]
    Loop(String),
}

/// Auto-detect the running compositor. tomoe first: it is the sibling
/// daemon and exports `$TOMOE_SOCKET` to children; the derived-path
/// check covers clients started outside its environment.
pub fn detect() -> Option<Compositor> {
    if std::env::var_os(tomoe_ipc::SOCKET_ENV).is_some()
        || tomoe_ipc::find_socket().is_some_and(|p| p.exists())
    {
        return Some(Compositor::Tomoe);
    }
    detect_env(|k| std::env::var_os(k).is_some())
}

fn detect_env(has: impl Fn(&str) -> bool) -> Option<Compositor> {
    if has("HYPRLAND_INSTANCE_SIGNATURE") {
        Some(Compositor::Hyprland)
    } else if has("NIRI_SOCKET") {
        Some(Compositor::Niri)
    } else if has("SWAYSOCK") || has("I3SOCK") {
        Some(Compositor::Sway)
    } else {
        None
    }
}

/// Detect and start the compositor backend. `notify` receives a full
/// state snapshot after every change (including disconnects). Returns
/// which compositor was detected; `Ok(None)` = none, workspace
/// tracking disabled.
pub fn start<D: 'static>(
    handle: &LoopHandle<'static, D>,
    notify: impl FnMut(&mut D, &CompositorState) + 'static,
) -> Result<Option<Compositor>, Error> {
    let Some(compositor) = detect() else {
        return Ok(None);
    };
    let notify: Notify<D> = Box::new(notify);
    match compositor {
        Compositor::Tomoe => tomoe::start(handle.clone(), notify)?,
        Compositor::Niri => niri::start(handle.clone(), notify)?,
        Compositor::Hyprland => hyprland::start(handle.clone(), notify)?,
        Compositor::Sway => sway::start(handle.clone(), notify)?,
    }
    Ok(Some(compositor))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_env_none() {
        assert_eq!(detect_env(|_| false), None);
    }

    #[test]
    fn detect_env_order() {
        // All set: Hyprland wins (nur's precedence).
        assert_eq!(detect_env(|_| true), Some(Compositor::Hyprland));
        assert_eq!(detect_env(|k| k == "NIRI_SOCKET"), Some(Compositor::Niri));
        assert_eq!(detect_env(|k| k == "SWAYSOCK"), Some(Compositor::Sway));
        assert_eq!(detect_env(|k| k == "I3SOCK"), Some(Compositor::Sway));
    }
}
