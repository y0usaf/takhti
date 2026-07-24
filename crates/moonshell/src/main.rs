//! The moonshell binary: config resolution and the calloop bootstrap.
//!
//! With a config (`--config`, `$MOONSHELL_CONFIG`, or
//! `~/.config/moonshell/init.lua`) it boots the Lua runtime, executes
//! the config, and drains the runtime's action queue — window
//! creations, timers to arm, watches to register, dirty marks, the
//! reload and quit flags — after config exec and once per loop pass
//! ([`Shell::run_with`]). The drain state lives in [`Engine`], which
//! owns the [`Vm`] so hot reload (inotify on the config tree, or
//! `shell.reload()`) can destroy the Lua windows and swap in a fresh
//! one. `exec_async` replies arrive over a calloop channel; the
//! display snapshot behind `shell.displays()` is refreshed each drain.
//! With no config it maps one layer surface and
//! draws a version string: the doctrine-06 bare-core artifact.
//! `--boot-check` exits 0 right after every window committed its first
//! frame, which is what `nix flake check` runs under a headless
//! compositor.

mod watcher;

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use anyhow::Context as _;
use calloop::generic::Generic;
use calloop::timer::{TimeoutAction, Timer};
use calloop::{Interest, LoopHandle, Mode, PostAction};
use moonshell_render::element::{Align, Edges, Flex, Spacer, Style, Text};
use moonshell_render::{Element, Renderer, Rgba, Scene, SceneDamage};
use moonshell_runtime::{LuaPainter, ShellCtx, Vm};
use moonshell_surface::{Canvas, Damage, DamageRect, Edge, LayerOptions, Painter, Shell, WindowId};
use watcher::Watcher;

/// Editors save in bursts (write, rename, chmod); coalesce them into
/// one reload.
const RELOAD_DEBOUNCE: Duration = Duration::from_millis(100);

const BG: Rgba = Rgba::new(0x14, 0x14, 0x1e, 0xff);
const FG: Rgba = Rgba::new(0xc8, 0xc8, 0xd8, 0xff);

struct VersionBar {
    renderer: Renderer,
    scene: Scene,
    root: Element,
}

impl VersionBar {
    fn new(label: String) -> Self {
        // The bare tree: bg + padding + a centered version string —
        // exercises the M1 element/layout/draw path with zero policy.
        let root = Element::HBox(Flex {
            style: Style {
                bg: Some(BG),
                ..Style::default()
            },
            padding: Edges {
                left: 8.0,
                right: 8.0,
                ..Edges::default()
            },
            align: Align::Center,
            children: vec![
                Element::Text(Text {
                    content: label,
                    size: 13.0,
                    color: FG,
                    ..Text::default()
                }),
                Element::Spacer(Spacer::default()),
            ],
            ..Flex::default()
        });
        Self {
            renderer: Renderer::new(),
            scene: Scene::new(),
            root,
        }
    }
}

impl Painter for VersionBar {
    fn paint(&mut self, canvas: Canvas<'_>) -> Damage {
        let Canvas {
            buf,
            width,
            height,
            scale,
            fresh,
        } = canvas;
        if fresh {
            // No prior content on this surface at this size — the diff
            // baseline is gone.
            self.scene.invalidate();
        }
        let damage = self.scene.render(
            &mut self.renderer,
            buf,
            width,
            height,
            scale as f32,
            &self.root,
        );
        match damage {
            SceneDamage::None => Damage::None,
            SceneDamage::Full => Damage::Full,
            SceneDamage::Rects(rects) => Damage::Rects(
                rects
                    .into_iter()
                    .map(|r| DamageRect {
                        x: r.x,
                        y: r.y,
                        width: r.w,
                        height: r.h,
                    })
                    .collect(),
            ),
        }
    }
}

/// Locate the config: explicit `--config` and `$MOONSHELL_CONFIG` must
/// exist (a typo should fail loudly); the default location is optional
/// (absent = the bare version bar). Canonicalized — the watcher
/// compares event paths literally, and the parent dir anchors the
/// config-tree watch.
fn resolve_config(cli: Option<PathBuf>) -> anyhow::Result<Option<PathBuf>> {
    let found = if let Some(p) = cli {
        anyhow::ensure!(p.is_file(), "--config: no such file: {}", p.display());
        Some(p)
    } else if let Some(p) = std::env::var_os("MOONSHELL_CONFIG") {
        let p = PathBuf::from(p);
        anyhow::ensure!(
            p.is_file(),
            "$MOONSHELL_CONFIG: no such file: {}",
            p.display()
        );
        Some(p)
    } else {
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")));
        base.map(|b| b.join("moonshell").join("init.lua"))
            .filter(|p| p.is_file())
    };
    // Absolutize *without* resolving symlinks. A Nix/home-manager
    // config is a symlink into /nix/store — canonicalizing it (a) made
    // the watch root /nix/store, so the watcher recursed the entire
    // store (~500k dir watches, the whole per-user inotify budget),
    // and (b) pinned reloads to the immutable old store file, so a
    // config switch never took effect. Keeping the symlink identity
    // fixes both: the watch root is the symlink's parent, and every
    // reload re-reads through the (possibly retargeted) link.
    found
        .map(|p| {
            std::path::absolute(&p)
                .with_context(|| format!("resolving config path {}", p.display()))
        })
        .transpose()
}

/// The config-mode loop state: the VM, the action-queue ctx, and the
/// windows/watches the current config owns. [`Engine::tick`] is the
/// per-pass drain [`Shell::run_with`] calls; [`Engine::reload`] swaps
/// the VM wholesale (fresh globals, nothing leaks across reloads).
struct Engine {
    ctx: Rc<ShellCtx>,
    vm: Vm,
    renderer: Rc<RefCell<Renderer>>,
    config: PathBuf,
    /// Windows the current config created — destroyed on reload.
    windows: Vec<WindowId>,
    /// `None` = inotify unavailable; hot reload degrades to
    /// `shell.reload()` only (warned once at boot).
    watcher: Option<Watcher>,
    loop_handle: LoopHandle<'static, Shell>,
    /// A debounce timer is already armed.
    reload_scheduled: bool,
    /// Latest battery snapshot — same re-seed contract.
    battery: Option<moonshell_services::battery::BatteryState>,
    /// Latest network snapshot — same re-seed contract.
    network: Option<moonshell_services::network::NetworkState>,
    /// Latest mpris snapshot — same re-seed contract.
    mpris: Option<moonshell_services::mpris::MprisState>,
}

impl Engine {
    /// Drain the action queue while holding `&mut Shell`. Snapshot in
    /// (displays), actions out (everything below).
    fn tick(&mut self, shell: &mut Shell) {
        self.ctx.set_displays(shell.displays());
        if self.ctx.take_reload() {
            self.reload(shell);
        }
        for p in self.ctx.take_pending() {
            let painter = LuaPainter::new(self.vm.lua().clone(), p.shared, self.renderer.clone());
            self.windows
                .push(shell.create_window(p.options, Box::new(painter)));
        }
        for t in self.ctx.take_timers() {
            // Armed only because a shell.interval/once exists — the
            // zero-idle-wakeup discipline. `fire` returning false means
            // the VM is gone; the source removes itself.
            let timer = Timer::from_duration(t.delay);
            let inserted =
                self.loop_handle
                    .insert_source(timer, move |_, _, _shell: &mut Shell| {
                        match (t.fire(), t.period) {
                            (true, Some(period)) => TimeoutAction::ToDuration(period),
                            _ => TimeoutAction::Drop,
                        }
                    });
            if let Err(e) = inserted {
                tracing::error!("inserting timer: {e}");
            }
        }
        for w in self.ctx.take_watches() {
            match &mut self.watcher {
                Some(watcher) => {
                    if let Err(e) = watcher.watch_file(&w.path, w.callback) {
                        tracing::error!("shell.watch_file({}): {e}", w.path.display());
                    }
                }
                None => tracing::error!(
                    "shell.watch_file({}): inotify unavailable",
                    w.path.display()
                ),
            }
        }
        if self.ctx.take_dirty() {
            shell.mark_all_dirty();
        }
        if self.ctx.take_quit() {
            shell.quit();
        }
    }

    /// `shell.services.battery` — same contract as [`push_compositor`].
    fn push_battery(&self) {
        if let Some(state) = &self.battery {
            if let Err(e) = moonshell_runtime::services_bridge::push_battery(self.vm.lua(), state) {
                tracing::debug!("pushing battery state: {e}");
            }
        }
    }

    /// `shell.services.network` — same contract as [`push_compositor`].
    fn push_network(&self) {
        if let Some(state) = &self.network {
            if let Err(e) = moonshell_runtime::services_bridge::push_network(self.vm.lua(), state) {
                tracing::debug!("pushing network state: {e}");
            }
        }
    }

    /// `shell.services.mpris` — same contract as [`push_compositor`].
    fn push_mpris(&self) {
        if let Some(state) = &self.mpris {
            if let Err(e) = moonshell_runtime::services_bridge::push_mpris(self.vm.lua(), state) {
                tracing::debug!("pushing mpris state: {e}");
            }
        }
    }

    /// Tear down the current config and re-exec it in a fresh VM. The
    /// windows go first (their painters hold the old VM's last strong
    /// `Lua` clones), then the ctx forgets the old VM's callbacks, then
    /// the swap drops it. Armed timers self-clean on next fire. A
    /// config error here logs and leaves the shell windowless — the
    /// watcher is still running, the next save retries.
    fn reload(&mut self, shell: &mut Shell) {
        tracing::info!(config = %self.config.display(), "reloading config");
        for id in self.windows.drain(..) {
            shell.destroy_window(id);
        }
        if let Some(w) = &mut self.watcher {
            w.clear_file_watches();
        }
        self.ctx.reset_for_reload();
        match Vm::new().and_then(|vm| {
            vm.install_shell(&self.ctx)?;
            Ok(vm)
        }) {
            Ok(vm) => self.vm = vm, // old VM dropped here
            Err(e) => {
                tracing::error!("reload: VM boot failed: {e}");
                return;
            }
        }
        self.ctx.set_displays(shell.displays());
        // Re-seed the fresh VM's facades before the config runs, so
        // top-level service reads see real state (same contract as
        // displays above).
        self.push_battery();
        self.push_network();
        self.push_mpris();
        match std::fs::read_to_string(&self.config) {
            Ok(code) => {
                if let Err(e) = self.vm.exec(&code, &self.config.to_string_lossy()) {
                    tracing::error!("reload: config error (fix and save to retry): {e}");
                }
            }
            Err(e) => tracing::error!("reload: reading {}: {e}", self.config.display()),
        }
        // Queued windows/timers from the new exec drain in the caller.
    }
}

/// mlua's error is `!Send + !Sync`; this is the one Lua→anyhow boundary.
fn lua_err(e: moonshell_runtime::mlua::Error) -> anyhow::Error {
    anyhow::anyhow!("{e}")
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("moonshell=info")),
        )
        .init();

    let mut boot_check = false;
    let mut config_arg: Option<PathBuf> = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--boot-check" => boot_check = true,
            "--config" => {
                config_arg = Some(args.next().context("--config needs a path")?.into());
            }
            "--version" | "-V" => {
                println!("moonshell {}", env!("CARGO_PKG_VERSION"));
                return Ok(());
            }
            other => {
                anyhow::bail!("unknown argument: {other} (try --version, --config, --boot-check)")
            }
        }
    }

    let config = resolve_config(config_arg)?;
    let (mut shell, event_loop) = Shell::connect()?;
    shell.exit_after_first_draw = boot_check;

    let Some(path) = config else {
        // Bare core (doctrine 06): no policy, still boots.
        let painter = VersionBar::new(format!("moonshell {}", env!("CARGO_PKG_VERSION")));
        shell.create_window(LayerOptions::bar(Edge::Top, 32, true), Box::new(painter));
        shell.run(event_loop)?;
        return Ok(());
    };

    tracing::info!(config = %path.display(), "loading config");
    let vm = Vm::new().map_err(lua_err)?;
    let ctx = ShellCtx::new();
    vm.install_shell(&ctx).map_err(lua_err)?;
    let code = std::fs::read_to_string(&path)
        .with_context(|| format!("reading config {}", path.display()))?;
    // Outputs are known (connect roundtrips) — snapshot them before the
    // config runs so top-level `shell.displays()` sees real geometry.
    ctx.set_displays(shell.displays());
    // Boot-time errors fail hard (a broken config at startup should
    // exit loudly); *reload*-time errors keep the process alive.
    vm.exec(&code, &path.to_string_lossy()).map_err(lua_err)?;

    // exec_async replies land on the loop thread through this channel;
    // idle it schedules nothing. Inserted once, before any Lua runs —
    // a reply can never beat its source.
    let exec_channel = ctx
        .take_exec_channel()
        .context("exec channel already taken")?;
    let exec_ctx = ctx.clone();
    event_loop
        .handle()
        .insert_source(exec_channel, move |event, _, _shell: &mut Shell| {
            if let calloop::channel::Event::Msg(reply) = event {
                // May queue windows / mark dirty; the tick after this
                // dispatch pass drains.
                exec_ctx.dispatch_exec_reply(reply);
            }
        })
        .map_err(|e| anyhow::anyhow!("inserting exec channel: {e}"))?;

    // The config-tree watch: inotify failure degrades to manual
    // `shell.reload()` instead of killing the shell.
    let config_root = path
        .parent()
        .map(PathBuf::from)
        .context("config path has no parent directory")?;
    let watcher = match Watcher::new(&config_root) {
        Ok(w) => Some(w),
        Err(e) => {
            tracing::warn!("inotify unavailable ({e}) — hot reload and shell.watch_file disabled");
            None
        }
    };
    let watcher_fd = watcher.as_ref().map(|w| w.loop_fd()).transpose()?;

    let engine = Rc::new(RefCell::new(Engine {
        ctx,
        vm,
        // One renderer for every window: the font system and glyph
        // caches are the dominant allocation — shared, not per-window.
        renderer: Rc::new(RefCell::new(Renderer::new())),
        config: path,
        windows: Vec::new(),
        watcher,
        loop_handle: event_loop.handle(),
        reload_scheduled: false,
        battery: None,
        network: None,
        mpris: None,
    }));

    // The battery service (M3 §3): UPower over the system bus, sysfs
    // polling fallback. Same engine contract as the compositor above.
    {
        let eng = engine.clone();
        let source = moonshell_services::battery::start(
            &event_loop.handle(),
            move |_shell: &mut Shell, state| {
                let mut e = eng.borrow_mut();
                e.battery = Some(state.clone());
                e.push_battery();
            },
        );
        tracing::info!("battery backend: {source}");
    }

    // The network service (M3 §4): NetworkManager over the system bus,
    // sysfs operstate polling fallback.
    {
        let eng = engine.clone();
        let source = moonshell_services::network::start(
            &event_loop.handle(),
            move |_shell: &mut Shell, state| {
                let mut e = eng.borrow_mut();
                e.network = Some(state.clone());
                e.push_network();
            },
        );
        tracing::info!("network backend: {source}");
    }

    // The mpris service (M3 §4): players tracked over the session bus,
    // playerctld-style. No fallback — no session bus means media
    // widgets stay on the facade defaults.
    {
        let eng = engine.clone();
        match moonshell_services::mpris::start(
            &event_loop.handle(),
            move |_shell: &mut Shell, state| {
                let mut e = eng.borrow_mut();
                e.mpris = Some(state.clone());
                e.push_mpris();
            },
        ) {
            Ok(()) => tracing::info!("mpris backend: session D-Bus"),
            Err(e) => tracing::info!("mpris unavailable ({e}) — media widgets stay empty"),
        }
    }

    // Inotify readiness: drain events (fires watch_file callbacks
    // inline), then debounce config changes into one reload request —
    // the timer raises the ctx flag and the next tick does the swap.
    if let Some(fd) = watcher_fd {
        let eng = engine.clone();
        event_loop
            .handle()
            .insert_source(
                Generic::new(fd, Interest::READ, Mode::Level),
                move |_, _, _shell: &mut Shell| {
                    let mut e = eng.borrow_mut();
                    let reload = match e.watcher.as_mut() {
                        Some(w) => w.handle_events().unwrap_or_else(|err| {
                            tracing::error!("inotify read: {err}");
                            false
                        }),
                        None => false,
                    };
                    if reload && !e.reload_scheduled {
                        e.reload_scheduled = true;
                        let eng2 = eng.clone();
                        let armed = e.loop_handle.insert_source(
                            Timer::from_duration(RELOAD_DEBOUNCE),
                            move |_, _, _shell: &mut Shell| {
                                let mut e = eng2.borrow_mut();
                                e.reload_scheduled = false;
                                e.ctx.request_reload();
                                TimeoutAction::Drop
                            },
                        );
                        if let Err(err) = armed {
                            tracing::error!("arming reload timer: {err}");
                            e.reload_scheduled = false;
                            e.ctx.request_reload(); // reload undebounced
                        }
                    }
                    Ok(PostAction::Continue)
                },
            )
            .map_err(|e| anyhow::anyhow!("inserting inotify source: {e}"))?;
    }

    // Pre-run drain so the no-window warning is accurate; run_with
    // ticks again (idempotent — the queues are drained) before its
    // first dispatch, so a config-time `shell.quit()` exits cleanly.
    engine.borrow_mut().tick(&mut shell);
    if shell.window_count() == 0 {
        tracing::warn!("config created no windows (shell.window was never called)");
    }
    let eng = engine.clone();
    shell.run_with(event_loop, move |shell| eng.borrow_mut().tick(shell))?;
    drop(engine); // outlives the loop: painters hold Lua clones
    Ok(())
}
