mod backend;
mod coords;
mod cursor;
mod handlers;
mod input;
mod layout;
mod lua;
mod render;
mod space;
mod state;
mod ui;

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

use crate::state::{ClientState, Takhti};

#[derive(Parser, Debug)]
#[command(name = "takhti", about = "A Wayland compositor with embedded Lua")]
struct Args {
    /// Path to init.lua (default: ~/.config/takhti/init.lua)
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
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("takhti=info")),
        )
        .init();

    let mut event_loop: EventLoop<Takhti> =
        EventLoop::try_new().context("error creating event loop")?;
    let display: Display<Takhti> = Display::new().context("error creating display")?;
    let display_handle = display.handle();

    let mut takhti = Takhti::new(
        event_loop.handle(),
        event_loop.get_signal(),
        display_handle.clone(),
    )?;

    // Wayland listening socket.
    let socket_source = ListeningSocketSource::new_auto().context("error creating socket")?;
    let socket_name = socket_source.socket_name().to_os_string();
    event_loop
        .handle()
        .insert_source(socket_source, |client, _, takhti| {
            if let Err(err) = takhti
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
            |_, display, takhti| {
                // SAFETY: we don't drop the display.
                unsafe {
                    display.get_mut().dispatch_clients(takhti).unwrap();
                }
                Ok(PostAction::Continue)
            },
        )
        .map_err(|err| anyhow!("error inserting display source: {err}"))?;

    // Config loads first so the backend can honor settings (e.g. winit_size).
    takhti.load_config(args.config);

    // Watch the config file for changes (niri-style polling: stat + canonical
    // path survive atomic renames and Nix store symlink swaps, and parsing
    // stays off the hot path — a stat every 500ms is negligible).
    const CONFIG_POLL: Duration = Duration::from_millis(500);
    event_loop
        .handle()
        .insert_source(Timer::from_duration(CONFIG_POLL), |_, _, takhti| {
            takhti.check_config_reload();
            TimeoutAction::ToDuration(CONFIG_POLL)
        })
        .map_err(|err| anyhow!("error inserting config watch timer: {err}"))?;

    let use_winit = match args.backend.as_str() {
        "winit" => true,
        "tty" => false,
        _ => std::env::var_os("WAYLAND_DISPLAY").is_some() || std::env::var_os("DISPLAY").is_some(),
    };
    if use_winit {
        backend::winit::init(&mut takhti)?;
    } else {
        backend::tty::init(&mut takhti, args.drm_device.as_deref())?;
    }

    std::env::set_var("WAYLAND_DISPLAY", &socket_name);
    info!("listening on WAYLAND_DISPLAY={socket_name:?}");

    event_loop
        .run(None, &mut takhti, |takhti| {
            takhti.space.refresh();
            takhti.popups.cleanup();
            if let Err(err) = takhti.display_handle.flush_clients() {
                warn!("error flushing clients: {err}");
            }
        })
        .context("error running event loop")?;

    Ok(())
}
