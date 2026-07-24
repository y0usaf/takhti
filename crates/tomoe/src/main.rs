mod animation;
mod backend;
mod capture;
mod coords;
mod cursor;
#[cfg(test)]
mod docgen;
mod foreign_toplevel;
mod handlers;
mod input;
mod ipc;
mod layout;
mod lock;
mod lua;
mod process;
mod protocols;
mod render;
mod screenshot;
mod shell;
mod space;
mod state;
mod ui;
mod xwayland;

use std::collections::BTreeMap;
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

use crate::process::{Launch, ProcessDecl, ProcessSpec, RunPolicy};
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
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(clap::Subcommand, Debug)]
enum Command {
    /// Send a request to the running compositor over its IPC socket.
    ///
    /// Builtins: version, windows, outputs, view, quit, subscribe. Any other
    /// method reaches the config's `tomoe.ipc.serve` handlers. `subscribe`
    /// keeps the connection open and prints one event per line.
    Msg {
        /// Method name, e.g. "windows" or "workspace/switch".
        method: String,
        /// Params as a JSON value, e.g. '{"name": "2"}'.
        params: Option<String>,
    },
}

fn main() -> Result<()> {
    let args = Args::parse();

    if let Some(Command::Msg { method, params }) = args.command {
        return msg(&method, params.as_deref());
    }

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

    // Watch the config file for changes by polling: stat + canonical path
    // survive atomic renames and Nix store symlink swaps, and parsing stays
    // off the hot path — a stat every 500ms is negligible.
    const CONFIG_POLL: Duration = Duration::from_millis(500);
    event_loop
        .handle()
        .insert_source(Timer::from_duration(CONFIG_POLL), |_, _, tomoe| {
            tomoe.check_config_reload();
            TimeoutAction::ToDuration(CONFIG_POLL)
        })
        .map_err(|err| anyhow!("error inserting config watch timer: {err}"))?;

    // Native shell services (FUSION F3): rustbus D-Bus + sysfs sources
    // ride this event loop; snapshots publish into `shell.services.*`
    // on change — no subprocess polling, no IPC.
    let battery_source = moonshell_services::battery::start(
        &event_loop.handle(),
        |tomoe: &mut state::Tomoe, snapshot| {
            tomoe.shell_battery = Some(snapshot.clone());
            tomoe.push_shell_services();
        },
    );
    info!("shell battery service: {battery_source}");
    let network_source = moonshell_services::network::start(
        &event_loop.handle(),
        |tomoe: &mut state::Tomoe, snapshot| {
            tomoe.shell_network = Some(snapshot.clone());
            tomoe.push_shell_services();
        },
    );
    info!("shell network service: {network_source}");
    if let Err(err) = moonshell_services::mpris::start(
        &event_loop.handle(),
        |tomoe: &mut state::Tomoe, snapshot| {
            tomoe.shell_mpris = Some(snapshot.clone());
            tomoe.push_shell_services();
        },
    ) {
        info!("shell mpris service unavailable: {err}");
    }
    // The notification daemon (FUSION F3): org.freedesktop.Notifications
    // hosted in-process; popups are Lua policy (moonshell.notifications).
    if let Err(err) = moonshell_services::notifications::start(
        &event_loop.handle(),
        |tomoe: &mut state::Tomoe, snapshot| {
            tomoe.shell_notifications = Some(snapshot.clone());
            tomoe.push_shell_services();
        },
    ) {
        info!("notification daemon off: {err}");
    }
    // SNI tray host (FUSION F3): the compositor is watcher and host;
    // item state reaches Lua tray widgets, menus land at F4.
    if let Err(err) = moonshell_services::tray::start(
        &event_loop.handle(),
        |tomoe: &mut state::Tomoe, snapshot| {
            tomoe.shell_tray = Some(snapshot.clone());
            tomoe.push_shell_services();
        },
    ) {
        info!("tray host off: {err}");
    }

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

    // IPC socket (`tomoe msg`, bars, the config's `tomoe.ipc.serve`
    // endpoints). Children and bus-activated services find it through
    // $TOMOE_SOCKET; external clients can also derive it from
    // $WAYLAND_DISPLAY.
    match ipc::start(&mut tomoe, &socket_name.to_string_lossy()) {
        Ok(path) => std::env::set_var(tomoe_ipc::SOCKET_ENV, &path),
        Err(err) => warn!("error starting IPC server (continuing without): {err:#}"),
    }

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
    // the host session's bus environment with its own socket. The session
    // units come up afterwards through the process manifest — a builtin
    // consumer of the same API user configs declare through.
    if !use_winit {
        import_environment();
        tomoe.declare_builtin_process("session-units", session_units());
        tomoe.reconcile_processes();
    }

    event_loop
        .run(None, &mut tomoe, |tomoe| {
            tomoe.space.refresh();
            tomoe.popups.cleanup();
            // Taskbar listeners: diff window states/outputs once per
            // iteration (only changes hit the wire).
            tomoe.refresh_wlr_foreign_toplevels();
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
    ipc::shutdown(&mut tomoe);
    if !use_winit {
        exit_session();
    }

    Ok(())
}

/// `tomoe msg`: connect to the running compositor's socket, send one
/// request, print the result. `subscribe` then streams events, one JSON
/// object per line, until the compositor goes away.
fn msg(method: &str, params: Option<&str>) -> Result<()> {
    let params = params
        .map(serde_json::from_str::<serde_json::Value>)
        .transpose()
        .context("params must be valid JSON")?;
    let path = tomoe_ipc::find_socket()
        .context("no compositor found ($TOMOE_SOCKET and $WAYLAND_DISPLAY unset)")?;
    let mut client = tomoe_ipc::Client::connect(&path)
        .with_context(|| format!("error connecting to {}", path.display()))?;

    match client.request(method, params).context("request failed")? {
        Ok(result) => println!("{}", serde_json::to_string_pretty(&result)?),
        Err(err) => anyhow::bail!("{err}"),
    }

    if method == "subscribe" {
        loop {
            let event = client.next_event().context("event stream closed")?;
            println!("{}", serde_json::to_string(&event)?);
        }
    }
    Ok(())
}

/// Session variables pushed into (and cleared from) the systemd user
/// environment and the D-Bus activation environment. TOMOE_PORTAL_CHOOSER is
/// the screencast source picker for xdg-desktop-portal-tomoe; the
/// bus-activated backend only sees it through the activation env.
const SESSION_ENV: &[&str] = &[
    "WAYLAND_DISPLAY",
    "DISPLAY",
    "XDG_CURRENT_DESKTOP",
    "TOMOE_PORTAL_CHOOSER",
    "TOMOE_SOCKET",
];

/// Push the session variables into the systemd user environment and the
/// D-Bus activation environment. Blocks on the shell:
/// bus-activated services must not race the import, and the session units
/// (started next, via the process manifest) rely on it.
fn import_environment() {
    let variables = SESSION_ENV
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

/// The session bring-up chain, declared as a builtin `once` entry on the
/// process manifest — so it runs as a supervised, reaped child instead of an
/// orphaned background shell, and nothing here blocks the event loop (the
/// GTK portal backend is a Wayland client of ours: waiting on it before the
/// loop dispatches would deadlock). One shell so the ordering holds:
///
/// 1. tomoe-session.target: BindsTo=graphical-session.target pulls the
///    session target up (systemd refuses starting graphical-session.target
///    directly), so session-bound user units start and units ordered
///    After=graphical-session.target become startable.
/// 2. xdg-desktop-portal-gtk before the frontend: the frontend calls its
///    backends synchronously during activation, so a backend that is merely
///    bus-activatable but ordered after the frontend (NixOS ships such an
///    After= override) deadlocks until the D-Bus call times out.
/// 3. try-restart the frontend: it caches `XDG_CURRENT_DESKTOP` at startup,
///    so a stale instance from before this session must go — `try-restart`
///    is a no-op unless it is running; a fresh one gets bus-activated with
///    the updated environment.
fn session_units() -> ProcessDecl {
    ProcessDecl::Once {
        spec: ProcessSpec {
            launch: Launch::Shell(
                "hash systemctl 2>/dev/null || exit 0; \
                 systemctl --user start tomoe-session.target; \
                 timeout 10 systemctl --user start xdg-desktop-portal-gtk.service; \
                 systemctl --user try-restart xdg-desktop-portal.service"
                    .to_string(),
            ),
            cwd: None,
            env: BTreeMap::new(),
        },
        run: RunPolicy::OncePerSession,
    }
}

/// Tear the session back down: stop tomoe-session.target
/// (graphical-session.target follows once nothing binds it — it is
/// StopWhenUnneeded) and clear the session variables from the systemd user
/// environment, so units activated later don't inherit a dead socket. The
/// D-Bus activation environment has no unset operation; a fresh session
/// overwrites it on the next import.
fn exit_session() {
    let variables = SESSION_ENV.join(" ");
    let script = format!(
        "hash systemctl 2>/dev/null || exit 0; \
         systemctl --user stop tomoe-session.target; \
         systemctl --user unset-environment {variables}"
    );
    match std::process::Command::new("/bin/sh")
        .args(["-c", &script])
        .spawn()
    {
        Ok(mut child) => {
            if let Err(err) = child.wait() {
                warn!("error waiting for session teardown shell: {err:?}");
            }
        }
        Err(err) => warn!("error spawning session teardown shell: {err:?}"),
    }
}
