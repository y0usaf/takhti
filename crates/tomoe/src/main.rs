mod backend;
mod capture;
mod coords;
mod cursor;
mod foreign_toplevel;
mod handlers;
mod input;
mod layout;
mod lock;
mod lua;
mod process;
mod protocols;
mod render;
mod screenshot;
mod space;
mod state;
mod ui;
mod xwayland;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use clap::Parser;
use smithay::reexports::calloop::generic::Generic;
use smithay::reexports::calloop::timer::{TimeoutAction, Timer};
use smithay::reexports::calloop::{EventLoop, Interest, Mode, PostAction};
use smithay::reexports::wayland_server::Display;
use smithay::wayland::socket::ListeningSocketSource;
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

use crate::state::{ClientState, Tomoe};

#[derive(Parser, Debug)]
#[command(name = "tomoe", about = "A Wayland compositor with embedded Lua")]
struct Args {
    /// Path to init.lua (default: ~/.config/tomoe/init.lua)
    #[arg(long)]
    config: Option<PathBuf>,
    /// Backend: auto, winit (nested window), or tty (DRM, run from a VT)
    #[arg(long, default_value = "auto")]
    backend: String,
    /// Force the render GPU for the tty backend (a /dev/dri/card* or
    /// renderD* path). Default: the boot GPU; outputs on other GPUs still
    /// work via buffer copies.
    #[arg(long)]
    drm_device: Option<PathBuf>,
}

fn main() -> Result<()> {
    let args = Args::parse();

    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("tomoe=info")),
        )
        .init();

    let mut event_loop: EventLoop<Tomoe> =
        EventLoop::try_new().context("error creating event loop")?;
    let display: Display<Tomoe> = Display::new().context("error creating display")?;
    let display_handle = display.handle();

    let mut tomoe = Tomoe::new(
        event_loop.handle(),
        event_loop.get_signal(),
        display_handle.clone(),
    )?;

    // Wayland listening socket.
    let socket_source = ListeningSocketSource::new_auto().context("error creating socket")?;
    let socket_name = socket_source.socket_name().to_os_string();
    event_loop
        .handle()
        .insert_source(socket_source, |client, _, tomoe| {
            if let Err(err) = tomoe
                .display_handle
                .insert_client(client, Arc::new(ClientState::default()))
            {
                warn!("error adding client: {err}");
            }
        })
        .map_err(|err| anyhow!("error inserting socket source: {err}"))?;

    // Dispatch Wayland client requests.
    event_loop
        .handle()
        .insert_source(
            Generic::new(display, Interest::READ, Mode::Level),
            |_, display, tomoe| {
                // SAFETY: we don't drop the display.
                unsafe {
                    display.get_mut().dispatch_clients(tomoe).unwrap();
                }
                Ok(PostAction::Continue)
            },
        )
        .map_err(|err| anyhow!("error inserting display source: {err}"))?;

    // Config loads first so the backend can honor settings (e.g. winit_size).
    tomoe.load_config(args.config);

    // Watch the config file for changes (niri-style polling: stat + canonical
    // path survive atomic renames and Nix store symlink swaps, and parsing
    // stays off the hot path — a stat every 500ms is negligible).
    const CONFIG_POLL: Duration = Duration::from_millis(500);
    event_loop
        .handle()
        .insert_source(Timer::from_duration(CONFIG_POLL), |_, _, tomoe| {
            tomoe.check_config_reload();
            TimeoutAction::ToDuration(CONFIG_POLL)
        })
        .map_err(|err| anyhow!("error inserting config watch timer: {err}"))?;

    let use_winit = match args.backend.as_str() {
        "winit" => true,
        "tty" => false,
        _ => std::env::var_os("WAYLAND_DISPLAY").is_some() || std::env::var_os("DISPLAY").is_some(),
    };
    if use_winit {
        backend::winit::init(&mut tomoe)?;
    } else {
        backend::tty::init(&mut tomoe, args.drm_device.as_deref())?;
    }

    std::env::set_var("WAYLAND_DISPLAY", &socket_name);
    info!("listening on WAYLAND_DISPLAY={socket_name:?}");

    // xdg-desktop-portal picks its backends per desktop: this makes it read
    // tomoe-portals.conf and route ScreenCast to xdg-desktop-portal-tomoe.
    std::env::set_var("XDG_CURRENT_DESKTOP", "tomoe");

    // xwayland-satellite: the sockets exist from here on, so DISPLAY is valid
    // for every spawned child even before satellite itself runs (connections
    // queue in the kernel backlog until the first client triggers the spawn).
    xwayland::setup(&mut tomoe);
    if let Some(satellite) = &tomoe.satellite {
        let display_name = satellite.display_name().to_owned();
        std::env::set_var("DISPLAY", &display_name);
        info!("listening on X11 socket: DISPLAY={display_name}");
    } else {
        // Never point children at a host X11 session.
        std::env::remove_var("DISPLAY");
    }

    // Bus-activated services (xdg-desktop-portal + our ScreenCast backend,
    // bars, ...) run as children of the session bus, not of tomoe, so they
    // only see the session through the systemd/D-Bus activation environment.
    // Push it when we own the session; a nested winit run must not hijack
    // the host session's bus environment with its own socket.
    if !use_winit {
        import_environment();
    }

    event_loop
        .run(None, &mut tomoe, |tomoe| {
            tomoe.space.refresh();
            tomoe.popups.cleanup();
            // Idle inhibitors are re-validated (alive + visible) once per
            // iteration; the activity debounce resets alongside.
            tomoe.refresh_idle_inhibit();
            tomoe.notified_activity = false;
            if let Err(err) = tomoe.display_handle.flush_clients() {
                warn!("error flushing clients: {err}");
            }
        })
        .context("error running event loop")?;

    // Managed services belong to the session; take them down with it.
    tomoe.process.shutdown();

    Ok(())
}

/// Push the session variables into the systemd user environment and the
/// D-Bus activation environment (niri-shape), then bring the portal units
/// up. The portal frontend caches `XDG_CURRENT_DESKTOP` at startup, so a
/// stale instance from before this session is restarted — `try-restart` is
/// a no-op unless it is running; a fresh one gets bus-activated with the
/// updated environment. The GTK portal backend starts before the frontend:
/// the frontend calls its backends synchronously during activation, so a
/// backend that is merely bus-activatable but ordered after the frontend
/// (NixOS ships such an After= override) deadlocks until the D-Bus call
/// times out.
///
/// Waits for the shell only through the environment import: bus-activated
/// services must not race it. The unit starts are backgrounded — the GTK
/// backend is itself a Wayland client, so blocking on it here, before the
/// event loop dispatches, deadlocks the compositor against its own client
/// until the timeout fires.
///
/// graphical-session.target is deliberately not started: systemd refuses
/// manual start/stop for it. Activating it needs an installed session
/// target (BindsTo=graphical-session.target) pulled up here instead.
fn import_environment() {
    let variables = [
        "WAYLAND_DISPLAY",
        "DISPLAY",
        "XDG_CURRENT_DESKTOP",
        // Screencast source picker for xdg-desktop-portal-tomoe; the
        // bus-activated backend only sees it through the activation env.
        "TOMOE_PORTAL_CHOOSER",
    ]
    .iter()
    .filter(|var| std::env::var_os(var).is_some())
    .copied()
    .collect::<Vec<_>>()
    .join(" ");
    let script = format!(
        "hash systemctl 2>/dev/null && \
         systemctl --user import-environment {variables}; \
         hash dbus-update-activation-environment 2>/dev/null && \
         dbus-update-activation-environment {variables}; \
         hash systemctl 2>/dev/null && (\
         timeout 10 systemctl --user start xdg-desktop-portal-gtk.service; \
         systemctl --user try-restart xdg-desktop-portal.service\
         ) >/dev/null 2>&1 & \
         exit 0"
    );
    match std::process::Command::new("/bin/sh")
        .args(["-c", &script])
        .spawn()
    {
        Ok(mut child) => match child.wait() {
            Ok(status) => {
                if !status.success() {
                    warn!("import environment shell exited with {status}");
                }
            }
            Err(err) => warn!("error waiting for import environment shell: {err:?}"),
        },
        Err(err) => warn!("error spawning shell to import environment: {err:?}"),
    }
}
