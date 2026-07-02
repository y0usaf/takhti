//! TTY backend: DRM/GBM output + libinput input through a libseat session.
//!
//! Multi-GPU, niri-style: every DRM device on the seat is opened, rendering
//! always happens on the primary render node through smithay's `GpuManager`,
//! and frames for outputs on other devices are copied across for scanout.
//! Connector and GPU hotplug arrive via udev; zero connected outputs is a
//! wait-state, not an error. Rendering is damage-driven through a per-output
//! redraw state machine (niri-style): nothing repaints unless `queue_redraw`
//! was called, and an output with a frame in flight coalesces further
//! requests until its vblank.

use std::collections::HashMap;
use std::mem;
use std::path::Path;
use std::time::Duration;

use anyhow::{anyhow, bail, ensure, Context, Result};
use smithay::backend::allocator::dmabuf::Dmabuf;
use smithay::backend::allocator::format::FormatSet;
use smithay::backend::allocator::gbm::{GbmAllocator, GbmBufferFlags, GbmDevice};
use smithay::backend::allocator::{Fourcc, Modifier};
use smithay::backend::drm::compositor::{DrmCompositor, FrameFlags, PrimaryPlaneElement};
use smithay::backend::drm::exporter::gbm::GbmFramebufferExporter;
use smithay::backend::drm::{
    DrmDevice, DrmDeviceFd, DrmEvent, DrmEventMetadata, DrmEventTime, DrmNode, NodeType,
};
use smithay::backend::egl::context::ContextPriority;
use smithay::backend::egl::{EGLDevice, EGLDisplay};
use smithay::backend::input::InputEvent;
use smithay::backend::libinput::{LibinputInputBackend, LibinputSessionInterface};
use smithay::backend::renderer::element::solid::{SolidColorBuffer, SolidColorRenderElement};
use smithay::backend::renderer::element::surface::render_elements_from_surface_tree;
use smithay::backend::renderer::element::Kind;
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::backend::renderer::multigpu::gbm::GbmGlesBackend;
use smithay::backend::renderer::multigpu::{GpuManager, MultiRenderer};
use smithay::backend::renderer::{ImportDma, ImportEgl};
use smithay::backend::session::libseat::LibSeatSession;
use smithay::backend::session::{Event as SessionEvent, Session};
use smithay::backend::udev::{self, UdevBackend, UdevEvent};
use smithay::desktop::layer_map_for_output;
use smithay::desktop::utils::OutputPresentationFeedback;
use smithay::input::pointer::{CursorImageStatus, CursorImageSurfaceData};
use smithay::output::{Mode, Output, PhysicalProperties, Subpixel};
use smithay::reexports::calloop::timer::{TimeoutAction, Timer};
use smithay::reexports::calloop::{LoopHandle, RegistrationToken};
use smithay::reexports::drm::control::{
    connector, crtc, Mode as DrmMode, ModeFlags, ModeTypeFlags,
};
use smithay::reexports::input::{
    self as libinput, DeviceCapability, DragLockState, Libinput, SendEventsMode,
};
use smithay::reexports::rustix::fs::OFlags;
use smithay::reexports::wayland_protocols::wp::presentation_time::server::wp_presentation_feedback;
use smithay::reexports::wayland_server::backend::GlobalId;
use smithay::utils::{DeviceFd, IsAlive, Monotonic};
use smithay::wayland::compositor::with_states;
use smithay::wayland::dmabuf::DmabufFeedbackBuilder;
use smithay::wayland::drm_syncobj::{supports_syncobj_eventfd, DrmSyncobjState};
use smithay::wayland::presentation::Refresh;
use smithay_drm_extras::drm_scanner::{DrmScanEvent, DrmScanner};
use tracing::{debug, info, warn};

use crate::backend::Backend;
use crate::lua::{
    DisplaySettings, InputConfig, InputDeviceSettings, RefreshSetting, Resolution, SizeSetting,
};
use crate::render::OutputRenderElements;
use crate::space::PhysicalSpace;
use crate::state::Takhti;

const SUPPORTED_COLOR_FORMATS: [Fourcc; 4] = [
    Fourcc::Argb8888,
    Fourcc::Xrgb8888,
    Fourcc::Abgr8888,
    Fourcc::Xbgr8888,
];

const CLEAR_COLOR: [f32; 4] = [0.05, 0.05, 0.05, 1.0];

pub type TtyGpuManager = GpuManager<GbmGlesBackend<GlesRenderer, DrmDeviceFd>>;

/// Renders on the primary GPU, copies to the target GPU when they differ.
pub type TtyRenderer<'render> = MultiRenderer<
    'render,
    'render,
    GbmGlesBackend<GlesRenderer, DrmDeviceFd>,
    GbmGlesBackend<GlesRenderer, DrmDeviceFd>,
>;

pub type GbmDrmCompositor = DrmCompositor<
    GbmAllocator<DrmDeviceFd>,
    GbmFramebufferExporter<DrmDeviceFd>,
    OutputPresentationFeedback,
    DrmDeviceFd,
>;

/// Per-output redraw state machine (spec: niri's Development:-Redraw-Loop).
/// Invariant: at most one repaint is in flight per output at any time.
#[derive(Debug, Default)]
pub enum RedrawState {
    /// Nothing scheduled; the output repaints only on the next `queue_redraw`.
    #[default]
    Idle,
    /// A repaint is scheduled as an event-loop idle callback.
    Queued,
    /// A frame was queued to DRM; awaiting the vblank that presents it.
    WaitingForVBlank { redraw_needed: bool },
    /// Last render had no damage, so nothing was queued to DRM; a timer
    /// approximates the missed vblank to keep frame-callback pacing.
    WaitingForEstimatedVBlank(RegistrationToken),
    /// Same, but a redraw was requested while waiting.
    WaitingForEstimatedVBlankAndQueued(RegistrationToken),
}

pub struct TtySurface {
    pub compositor: GbmDrmCompositor,
    pub output: Output,
    /// Snapshot from connect time; modes can't change without a hotplug,
    /// which tears the surface down anyway. Lets reloads re-pick the mode.
    pub connector: connector::Info,
    pub redraw_state: RedrawState,
    /// The wl_output global, removed again when the connector disconnects.
    pub global: GlobalId,
}

/// One DRM device (GPU) on the seat.
pub struct OutputDevice {
    pub drm: DrmDevice,
    pub gbm: GbmDevice<DrmDeviceFd>,
    /// Scanout buffers come from here: this device's GBM when it can render,
    /// the primary's when it is display-only.
    pub allocator: GbmAllocator<DrmDeviceFd>,
    /// None for display-only devices (no usable EGL); their outputs render
    /// on the primary GPU and import the result.
    pub render_node: Option<DrmNode>,
    pub scanner: DrmScanner,
    pub surfaces: HashMap<crtc::Handle, TtySurface>,
    /// The DRM event source, removed with the device.
    pub token: RegistrationToken,
}

pub struct TtyData {
    pub session: LibSeatSession,
    pub libinput: Libinput,
    pub gpu_manager: TtyGpuManager,
    pub primary_node: DrmNode,
    pub primary_render_node: DrmNode,
    pub devices: HashMap<DrmNode, OutputDevice>,
    /// The dmabuf global is created once, when the primary GPU comes up.
    pub dmabuf_global_created: bool,
    /// Displays config as of the last apply; lets `apply_display_settings`
    /// (which runs after every Lua entry) bail without touching DRM.
    pub last_displays: HashMap<String, DisplaySettings>,
    /// Input config as of the last apply, same fast-bail pattern.
    pub last_input: InputConfig,
    /// Live libinput devices, for re-applying config on settings changes.
    pub input_devices: Vec<libinput::Device>,
    pub cursor_buffer: SolidColorBuffer,
}

pub fn init(takhti: &mut Takhti, drm_device: Option<&Path>) -> Result<()> {
    let (session, notifier) = LibSeatSession::new()
        .context("error creating libseat session (is seatd or logind available?)")?;
    let seat_name = session.seat();
    info!("libseat session on seat {seat_name}");

    let mut libinput = Libinput::new_with_udev(LibinputSessionInterface::from(session.clone()));
    libinput
        .udev_assign_seat(&seat_name)
        .map_err(|()| anyhow!("error assigning libinput seat"))?;
    let input_backend = LibinputInputBackend::new(libinput.clone());
    takhti
        .loop_handle
        .insert_source(input_backend, |mut event, _, takhti| {
            // Device lifecycle stays backend-side: configure new devices per
            // the current settings and track them so `apply_libinput_settings`
            // can re-apply on config changes.
            match &mut event {
                InputEvent::DeviceAdded { device } => on_device_added(takhti, device),
                InputEvent::DeviceRemoved { device } => {
                    if let Backend::Tty(data) = &mut takhti.backend {
                        data.input_devices.retain(|d| d != device);
                    }
                }
                _ => {}
            }
            takhti.process_input_event(event);
        })
        .map_err(|err| anyhow!("error inserting libinput source: {err}"))?;

    takhti
        .loop_handle
        .insert_source(notifier, |event, _, takhti| takhti.on_session_event(event))
        .map_err(|err| anyhow!("error inserting session source: {err}"))?;

    let gpu_manager = GpuManager::new(GbmGlesBackend::with_context_priority(ContextPriority::High))
        .context("error creating GPU manager")?;

    // The primary GPU is only where rendering happens (boot_vga by default,
    // --drm-device to override); outputs on other GPUs still light up via
    // cross-device buffer copies.
    let (primary_node, primary_render_node) = match drm_device {
        Some(path) => {
            let node = DrmNode::from_path(path)
                .with_context(|| format!("error opening DRM node {path:?}"))?;
            (
                node.node_with_type(NodeType::Primary)
                    .and_then(Result::ok)
                    .unwrap_or(node),
                node.node_with_type(NodeType::Render)
                    .and_then(Result::ok)
                    .unwrap_or(node),
            )
        }
        None => {
            let path = udev::primary_gpu(&seat_name)
                .context("error probing primary GPU")?
                .ok_or_else(|| anyhow!("no GPU found on seat {seat_name}"))?;
            let node = DrmNode::from_path(&path).context("error opening DRM node")?;
            let render = node
                .node_with_type(NodeType::Render)
                .and_then(Result::ok)
                .unwrap_or(node);
            (node, render)
        }
    };
    info!("rendering on {primary_render_node} (primary node {primary_node})");

    takhti.backend = Backend::Tty(TtyData {
        session,
        libinput,
        gpu_manager,
        primary_node,
        primary_render_node,
        devices: HashMap::new(),
        dmabuf_global_created: false,
        last_displays: takhti.lua.settings().displays,
        last_input: takhti.lua.settings().input,
        input_devices: Vec::new(),
        cursor_buffer: SolidColorBuffer::new((8, 16), [1.0, 1.0, 1.0, 1.0]),
    });

    let udev_backend = UdevBackend::new(&seat_name).context("error creating udev backend")?;
    let mut initial: Vec<(DrmNode, std::path::PathBuf)> = udev_backend
        .device_list()
        .filter_map(|(device_id, path)| {
            DrmNode::from_dev_id(device_id)
                .ok()
                .map(|node| (node, path.to_owned()))
        })
        .collect();
    // The primary must come up first: display-only devices allocate their
    // scanout buffers from its GBM device.
    initial.sort_by_key(|(node, _)| *node != primary_node);

    takhti
        .loop_handle
        .insert_source(udev_backend, |event, _, takhti| match event {
            UdevEvent::Added { device_id, path } => {
                if let Ok(node) = DrmNode::from_dev_id(device_id) {
                    if let Err(err) = device_added(takhti, node, &path) {
                        warn!("error adding DRM device {node}: {err:#}");
                    }
                }
            }
            UdevEvent::Changed { device_id } => {
                if let Ok(node) = DrmNode::from_dev_id(device_id) {
                    device_changed(takhti, node);
                }
            }
            UdevEvent::Removed { device_id } => {
                if let Ok(node) = DrmNode::from_dev_id(device_id) {
                    device_removed(takhti, node);
                }
            }
        })
        .map_err(|err| anyhow!("error inserting udev source: {err}"))?;

    for (node, path) in initial {
        if let Err(err) = device_added(takhti, node, &path) {
            warn!("error adding DRM device {node}: {err:#}");
        }
    }

    {
        let Backend::Tty(data) = &takhti.backend else {
            unreachable!()
        };
        if data
            .devices
            .values()
            .all(|device| device.surfaces.is_empty())
        {
            warn!("no connected outputs found; waiting for hotplug");
        }
    }

    // Runs the Lua outputs hook and, via after_lua, queues the first redraws.
    takhti.outputs_changed(true);
    queue_redraw_all(takhti);
    Ok(())
}

fn device_added(takhti: &mut Takhti, node: DrmNode, path: &Path) -> Result<()> {
    if node.ty() != NodeType::Primary {
        return Ok(());
    }
    let display_handle = takhti.display_handle.clone();
    let Takhti {
        backend,
        loop_handle,
        dmabuf_state,
        syncobj_state,
        ..
    } = takhti;
    let Backend::Tty(data) = backend else {
        return Ok(());
    };
    if data.devices.contains_key(&node) {
        return Ok(());
    }
    debug!("adding DRM device {node} ({path:?})");

    let fd = data
        .session
        .open(
            path,
            OFlags::RDWR | OFlags::CLOEXEC | OFlags::NOCTTY | OFlags::NONBLOCK,
        )
        .context("error opening DRM device through the session")?;
    let device_fd = DrmDeviceFd::new(DeviceFd::from(fd));

    let (drm, drm_notifier) =
        DrmDevice::new(device_fd.clone(), true).context("error creating DRM device")?;
    let gbm = GbmDevice::new(device_fd.clone()).context("error creating GBM device")?;

    // Bring up a renderer on this GPU if possible; display-only devices (or
    // software EGL) stay render_node=None and scan out the primary's frames.
    let mut try_renderer = || -> Result<DrmNode> {
        let display =
            unsafe { EGLDisplay::new(gbm.clone()) }.context("error creating EGL display")?;
        let egl_device =
            EGLDevice::device_for_display(&display).context("error probing EGL device")?;
        ensure!(
            !egl_device.is_software(),
            "software EGL renderers are skipped"
        );
        let render_node = egl_device
            .try_get_render_node()
            .ok()
            .flatten()
            .unwrap_or(node);
        data.gpu_manager
            .as_mut()
            .add_node(render_node, gbm.clone())
            .context("error adding node to GPU manager")?;
        Ok(render_node)
    };
    let render_node = match try_renderer() {
        Ok(render_node) => Some(render_node),
        Err(err) => {
            debug!("no renderer on {node}, using it for scanout only: {err:#}");
            None
        }
    };

    // The primary GPU is up: create the dmabuf global (default feedback
    // points clients at the render device) and explicit sync.
    if render_node == Some(data.primary_render_node) && !data.dmabuf_global_created {
        match data.gpu_manager.single_renderer(&data.primary_render_node) {
            Ok(mut renderer) => {
                if renderer.bind_wl_display(&display_handle).is_err() {
                    debug!("legacy EGL display binding unavailable (expected on modern systems)");
                }
                let formats = renderer.dmabuf_formats();
                match DmabufFeedbackBuilder::new(data.primary_render_node.dev_id(), formats).build()
                {
                    Ok(feedback) => {
                        let _global = dmabuf_state.create_global_with_default_feedback::<Takhti>(
                            &display_handle,
                            &feedback,
                        );
                        data.dmabuf_global_created = true;
                    }
                    Err(err) => warn!("error building dmabuf feedback: {err}"),
                }
            }
            Err(err) => warn!("error creating primary renderer: {err}"),
        }

        // Expose linux-drm-syncobj-v1 (explicit sync) when the GPU supports
        // syncobj_eventfd. Clients that use it (NVIDIA-driven GL/Vulkan,
        // Electron apps like Discord) then tell us exactly when a buffer is
        // ready instead of relying on implicit fences.
        if supports_syncobj_eventfd(&device_fd) {
            info!("explicit sync (linux-drm-syncobj-v1) enabled");
            *syncobj_state = Some(DrmSyncobjState::new::<Takhti>(
                &display_handle,
                device_fd.clone(),
            ));
        } else {
            info!("explicit sync unavailable: GPU lacks syncobj_eventfd support");
        }
    }

    let allocator_gbm = if render_node.is_some() {
        gbm.clone()
    } else if let Some(primary) = data.devices.get(&data.primary_node) {
        primary.gbm.clone()
    } else {
        bail!("no allocator available for display-only device {node}");
    };
    let allocator = GbmAllocator::new(
        allocator_gbm,
        GbmBufferFlags::RENDERING | GbmBufferFlags::SCANOUT,
    );

    let token = loop_handle
        .insert_source(drm_notifier, move |event, meta, takhti| match event {
            DrmEvent::VBlank(crtc) => on_vblank(takhti, node, crtc, meta.take()),
            DrmEvent::Error(err) => warn!("DRM error: {err}"),
        })
        .map_err(|err| anyhow!("error inserting DRM source: {err}"))?;

    data.devices.insert(
        node,
        OutputDevice {
            drm,
            gbm,
            allocator,
            render_node,
            scanner: DrmScanner::new(),
            surfaces: HashMap::new(),
            token,
        },
    );

    device_changed(takhti, node);
    Ok(())
}

fn device_changed(takhti: &mut Takhti, node: DrmNode) {
    let events: Vec<DrmScanEvent> = {
        let Backend::Tty(data) = &mut takhti.backend else {
            return;
        };
        let Some(device) = data.devices.get_mut(&node) else {
            return;
        };
        match device.scanner.scan_connectors(&device.drm) {
            Ok(scan) => scan.into_iter().collect(),
            Err(err) => {
                warn!("error scanning connectors on {node}: {err}");
                return;
            }
        }
    };

    let mut changed = false;
    for event in events {
        match event {
            DrmScanEvent::Connected {
                connector,
                crtc: Some(crtc),
            } => match connector_connected(takhti, node, connector, crtc) {
                Ok(()) => changed = true,
                Err(err) => warn!("error setting up connector: {err:#}"),
            },
            DrmScanEvent::Disconnected {
                crtc: Some(crtc), ..
            } => {
                connector_disconnected(takhti, node, crtc);
                changed = true;
            }
            _ => {}
        }
    }

    if changed {
        reposition_outputs(takhti);
        takhti.outputs_changed(false);
        queue_redraw_all(takhti);
    }
}

fn device_removed(takhti: &mut Takhti, node: DrmNode) {
    let crtcs: Vec<crtc::Handle> = {
        let Backend::Tty(data) = &takhti.backend else {
            return;
        };
        let Some(device) = data.devices.get(&node) else {
            return;
        };
        device.surfaces.keys().copied().collect()
    };
    let had_surfaces = !crtcs.is_empty();
    for crtc in crtcs {
        connector_disconnected(takhti, node, crtc);
    }

    {
        let Takhti {
            backend,
            loop_handle,
            ..
        } = takhti;
        let Backend::Tty(data) = backend else { return };
        let Some(device) = data.devices.remove(&node) else {
            return;
        };
        info!("DRM device removed: {node}");
        loop_handle.remove(device.token);
        if let Some(render_node) = device.render_node {
            data.gpu_manager.as_mut().remove_node(&render_node);
            // Force re-enumeration so the manager drops the device now.
            let _ = data.gpu_manager.devices();
        }
    }

    if had_surfaces {
        reposition_outputs(takhti);
        takhti.outputs_changed(false);
        queue_redraw_all(takhti);
    }
}

/// Choose a display mode per `settings.displays[name].resolution`. Resolve
/// the size first (preferred / largest area / exact), then the refresh among
/// modes of that size. Interlaced modes are skipped (they don't work — see
/// niri's pick_mode). Returns the fallback flag: true means nothing matched
/// and the EDID-preferred mode was used instead, so a config written for one
/// monitor degrades gracefully on another.
fn pick_mode(connector: &connector::Info, target: Resolution) -> Option<(DrmMode, bool)> {
    let modes = connector.modes();
    let preferred = modes
        .iter()
        .find(|m| m.mode_type().contains(ModeTypeFlags::PREFERRED))
        .or_else(|| modes.first())
        .copied()?;
    let progressive = || {
        modes
            .iter()
            .filter(|m| !m.flags().contains(ModeFlags::INTERLACE))
            .copied()
    };

    let size = match target.size {
        SizeSetting::Preferred => preferred.size(),
        SizeSetting::Max => match progressive().max_by_key(|m| {
            let (w, h) = m.size();
            w as u64 * h as u64
        }) {
            Some(m) => m.size(),
            None => return Some((preferred, true)),
        },
        SizeSetting::Exact(w, h) => (w, h),
    };
    let at_size = || progressive().filter(|m| m.size() == size);

    // Refresh comparisons in millihertz, via the wl_output conversion.
    let refresh = |m: &DrmMode| Mode::from(*m).refresh;
    let chosen = match target.refresh {
        // Bare "preferred" means the EDID mode as-is, not its size at max refresh.
        RefreshSetting::Auto if target.size == SizeSetting::Preferred => Some(preferred),
        RefreshSetting::Auto | RefreshSetting::Max => at_size().max_by_key(refresh),
        // Exact match first (niri-style), else within 1 Hz ("60" matches 59.94).
        RefreshSetting::Exact(mhz) => at_size()
            .min_by_key(|m| (refresh(m) - mhz).abs())
            .filter(|m| (refresh(m) - mhz).abs() <= 1000),
    };
    match chosen {
        Some(mode) => Some((mode, false)),
        None => Some((preferred, true)),
    }
}

fn connector_connected(
    takhti: &mut Takhti,
    node: DrmNode,
    connector: connector::Info,
    crtc: crtc::Handle,
) -> Result<()> {
    let Takhti {
        backend,
        space,
        display_handle,
        lua,
        ..
    } = takhti;
    let Backend::Tty(data) = backend else {
        bail!("tty backend not active");
    };
    let TtyData {
        devices,
        gpu_manager,
        primary_render_node,
        ..
    } = data;
    let Some(device) = devices.get_mut(&node) else {
        bail!("unknown DRM device {node}");
    };

    // Kernel connector names ("DP-1", "HDMI-A-1"): what users key
    // `settings.displays` by, matching every other compositor.
    let name = format!(
        "{}-{}",
        connector.interface().as_str(),
        connector.interface_id()
    );

    let (mode, fallback) = pick_mode(&connector, lua.settings().resolution_for(&name))
        .context("connector has no modes")?;
    if fallback {
        warn!("output {name}: no mode matches the configured resolution; using preferred");
    }
    let (w, h) = mode.size();
    info!(
        "connecting output {name}: {w}x{h}@{} on {node}",
        mode.vrefresh()
    );

    let surface = device
        .drm
        .create_surface(crtc, mode, &[connector.handle()])
        .context("error creating DRM surface")?;

    let (phys_w, phys_h) = connector.size().unwrap_or((0, 0));
    let output = Output::new(
        name,
        PhysicalProperties {
            size: (phys_w as i32, phys_h as i32).into(),
            subpixel: Subpixel::Unknown,
            make: "Unknown".into(),
            model: "Unknown".into(),
            serial_number: "Unknown".into(),
        },
    );
    let wl_mode = Mode::from(mode);
    // New outputs go on the right edge; reposition_outputs re-packs after
    // every batch of changes. Outputs live at integer physical positions; the
    // logical position (for wl_output/xdg-output) is derived at the protocol
    // boundary.
    let x = space
        .outputs()
        .filter_map(|output| space.output_geometry(output))
        .map(|geo| geo.loc.x + geo.size.w)
        .max()
        .unwrap_or(0);
    let scale = space.scale();
    let logical_loc = crate::coords::rect_to_logical(
        smithay::utils::Rectangle::new((x, 0).into(), wl_mode.size),
        scale,
    )
    .loc;
    output.change_current_state(
        Some(wl_mode),
        None,
        Some(smithay::output::Scale::Fractional(scale)),
        Some(logical_loc),
    );
    output.set_preferred(wl_mode);

    // Scanout buffer formats are negotiated against the GPU that renders
    // this output's frames; display-only devices import the primary's
    // buffers, where linear is the safe cross-device choice.
    let render_node_for_output = device.render_node.unwrap_or(*primary_render_node);
    let render_formats = {
        let renderer = gpu_manager
            .single_renderer(&render_node_for_output)
            .context("error creating renderer")?;
        renderer
            .as_ref()
            .egl_context()
            .dmabuf_render_formats()
            .iter()
            .copied()
            .filter(|format| device.render_node.is_some() || format.modifier == Modifier::Linear)
            .collect::<FormatSet>()
    };

    let new_compositor = |surface, render_formats, device: &OutputDevice| {
        DrmCompositor::new(
            smithay::output::OutputModeSource::Auto(output.downgrade()),
            surface,
            None,
            device.allocator.clone(),
            GbmFramebufferExporter::new(device.gbm.clone(), device.render_node.into()),
            SUPPORTED_COLOR_FORMATS,
            render_formats,
            device.drm.cursor_size(),
            Some(device.gbm.clone()),
        )
    };
    let compositor = match new_compositor(surface, render_formats.clone(), device) {
        Ok(compositor) => compositor,
        Err(err) => {
            // Modifier negotiation can fail (bandwidth, cross-device import);
            // retry with the invalid modifier (implicit tiling), niri-style.
            warn!("error creating DRM compositor, retrying with invalid modifier: {err}");
            let render_formats = render_formats
                .iter()
                .copied()
                .filter(|format| format.modifier == Modifier::Invalid)
                .collect::<FormatSet>();
            let surface = device
                .drm
                .create_surface(crtc, mode, &[connector.handle()])
                .context("error recreating DRM surface")?;
            new_compositor(surface, render_formats, device)
                .context("error creating DRM compositor")?
        }
    };

    let global = output.create_global::<Takhti>(display_handle);
    space.map_output(&output, (x, 0));

    device.surfaces.insert(
        crtc,
        TtySurface {
            compositor,
            output,
            connector,
            redraw_state: RedrawState::Idle,
            global,
        },
    );
    Ok(())
}

fn connector_disconnected(takhti: &mut Takhti, node: DrmNode, crtc: crtc::Handle) {
    let Takhti {
        backend,
        space,
        display_handle,
        loop_handle,
        ..
    } = takhti;
    let Backend::Tty(data) = backend else { return };
    let Some(device) = data.devices.get_mut(&node) else {
        return;
    };
    let Some(surface) = device.surfaces.remove(&crtc) else {
        return;
    };
    info!("disconnecting output {}", surface.output.name());

    match surface.redraw_state {
        RedrawState::WaitingForEstimatedVBlank(token)
        | RedrawState::WaitingForEstimatedVBlankAndQueued(token) => {
            loop_handle.remove(token);
        }
        _ => {}
    }
    space.unmap_output(&surface.output);
    display_handle.remove_global::<Takhti>(surface.global);
}

/// Re-pack outputs left-to-right (preserving connect order) and refresh
/// their logical positions; run after any change to the output set or modes.
fn reposition_outputs(takhti: &mut Takhti) {
    let outputs: Vec<Output> = takhti.space.outputs().cloned().collect();
    let scale = takhti.space.scale();
    let mut x = 0;
    for output in &outputs {
        let Some(mode) = output.current_mode() else {
            continue;
        };
        let size = output.current_transform().transform_size(mode.size);
        let logical_loc = crate::coords::rect_to_logical(
            smithay::utils::Rectangle::new((x, 0).into(), size),
            scale,
        )
        .loc;
        output.change_current_state(None, None, None, Some(logical_loc));
        takhti.space.map_output(output, (x, 0));
        x += size.w;
    }
}

/// Re-pick every output's mode against the current `settings.displays`
/// (config reload). Returns true if any mode changed; the caller re-emits
/// `outputs_changed` so the Lua WM can retile. Runs after every Lua entry,
/// so it bails immediately unless the displays config actually changed.
pub fn apply_display_settings(takhti: &mut Takhti) -> bool {
    let settings = takhti.lua.settings();
    let Backend::Tty(data) = &mut takhti.backend else {
        return false;
    };
    if settings.displays == data.last_displays {
        return false;
    }
    data.last_displays = settings.displays.clone();

    let mut changed = false;
    for device in data.devices.values_mut() {
        for surface in device.surfaces.values_mut() {
            let name = surface.output.name();
            let Some((mode, fallback)) =
                pick_mode(&surface.connector, settings.resolution_for(&name))
            else {
                continue;
            };
            if fallback {
                warn!("output {name}: no mode matches the configured resolution; using preferred");
            }
            if mode == surface.compositor.pending_mode() {
                continue;
            }
            if let Err(err) = surface.compositor.use_mode(mode) {
                warn!(
                    "output {}: error setting mode {}x{}@{}: {err}",
                    surface.output.name(),
                    mode.size().0,
                    mode.size().1,
                    mode.vrefresh(),
                );
                continue;
            }
            let (w, h) = mode.size();
            info!(
                "output {}: mode changed to {w}x{h}@{}",
                surface.output.name(),
                mode.vrefresh(),
            );
            surface
                .output
                .change_current_state(Some(Mode::from(mode)), None, None, None);
            changed = true;
        }
    }
    if !changed {
        return false;
    }

    // Widths may have changed: re-pack and repaint.
    reposition_outputs(takhti);
    queue_redraw_all(takhti);
    true
}

fn on_device_added(takhti: &mut Takhti, device: &mut libinput::Device) {
    // The name is what `settings.devices` keys on; log it for discoverability
    // (same string `libinput list-devices` prints).
    info!("input device added: {:?}", device.name());
    apply_device_config(&takhti.lua.settings().input, device);
    if let Backend::Tty(data) = &mut takhti.backend {
        data.input_devices.push(device.clone());
    }
}

/// Re-apply `settings.touchpad`/`settings.mouse`/`settings.devices` to every
/// live device. Runs after every Lua entry; bails unless the config changed.
pub fn apply_libinput_settings(takhti: &mut Takhti) {
    let config = takhti.lua.settings().input;
    let Backend::Tty(data) = &mut takhti.backend else {
        return;
    };
    if config == data.last_input {
        return;
    }
    data.last_input = config.clone();
    for device in &mut data.input_devices {
        apply_device_config(&config, device);
    }
}

/// Configure one libinput device: class settings (touchpad/mouse) overlaid
/// with any `settings.devices["<name>"]` entry. Unset fields revert to the
/// device's libinput defaults so a reload undoes removed lines; calls that a
/// device doesn't support fail silently (libinput just refuses).
fn apply_device_config(config: &InputConfig, device: &mut libinput::Device) {
    // Tap support is what distinguishes touchpads (how Mutter tells them apart).
    let is_touchpad = device.config_tap_finger_count() > 0;
    let class = if is_touchpad {
        config.touchpad
    } else if device.has_capability(DeviceCapability::Pointer) {
        config.mouse
    } else {
        InputDeviceSettings::default()
    };
    let s = match config.devices.get(device.name().as_ref()) {
        Some(per_device) => class.overridden_by(per_device),
        None => class,
    };

    let _ = device.config_send_events_set_mode(match (s.disabled, s.disabled_on_external_mouse) {
        (Some(true), _) => SendEventsMode::DISABLED,
        (_, Some(true)) => SendEventsMode::DISABLED_ON_EXTERNAL_MOUSE,
        _ => SendEventsMode::ENABLED,
    });

    let tap = s.tap.unwrap_or(device.config_tap_default_enabled());
    let _ = device.config_tap_set_enabled(tap);
    let tap_drag = s
        .tap_drag
        .unwrap_or(device.config_tap_default_drag_enabled());
    let _ = device.config_tap_set_drag_enabled(tap_drag);
    let drag_lock = match s.tap_drag_lock {
        Some(true) => DragLockState::EnabledTimeout,
        Some(false) => DragLockState::Disabled,
        None => device.config_tap_default_drag_lock_enabled(),
    };
    let _ = device.config_tap_set_drag_lock_enabled(drag_lock);

    let natural = s
        .natural_scroll
        .unwrap_or(device.config_scroll_default_natural_scroll_enabled());
    let _ = device.config_scroll_set_natural_scroll_enabled(natural);
    let speed = s.accel_speed.unwrap_or(device.config_accel_default_speed());
    let _ = device.config_accel_set_speed(speed);
    let profile = s
        .accel_profile
        .map(|p| match p {
            crate::lua::AccelProfile::Flat => libinput::AccelProfile::Flat,
            crate::lua::AccelProfile::Adaptive => libinput::AccelProfile::Adaptive,
        })
        .or_else(|| device.config_accel_default_profile());
    if let Some(profile) = profile {
        let _ = device.config_accel_set_profile(profile);
    }

    let dwt = s.dwt.unwrap_or(device.config_dwt_default_enabled());
    let _ = device.config_dwt_set_enabled(dwt);
    let left_handed = s.left_handed.unwrap_or(device.config_left_handed_default());
    let _ = device.config_left_handed_set(left_handed);
    let middle = s
        .middle_emulation
        .unwrap_or(device.config_middle_emulation_default_enabled());
    let _ = device.config_middle_emulation_set_enabled(middle);

    let method = s
        .scroll_method
        .map(|m| match m {
            crate::lua::ScrollMethod::NoScroll => libinput::ScrollMethod::NoScroll,
            crate::lua::ScrollMethod::TwoFinger => libinput::ScrollMethod::TwoFinger,
            crate::lua::ScrollMethod::Edge => libinput::ScrollMethod::Edge,
            crate::lua::ScrollMethod::OnButtonDown => libinput::ScrollMethod::OnButtonDown,
        })
        .or_else(|| device.config_scroll_default_method());
    if let Some(method) = method {
        let _ = device.config_scroll_set_method(method);
        if method == libinput::ScrollMethod::OnButtonDown {
            let button = s
                .scroll_button
                .unwrap_or(device.config_scroll_default_button());
            let _ = device.config_scroll_set_button(button);
        }
    }

    let click = s
        .click_method
        .map(|m| match m {
            crate::lua::ClickMethod::ButtonAreas => libinput::ClickMethod::ButtonAreas,
            crate::lua::ClickMethod::Clickfinger => libinput::ClickMethod::Clickfinger,
        })
        .or_else(|| device.config_click_default_method());
    if let Some(click) = click {
        let _ = device.config_click_set_method(click);
    }
}

/// Request a repaint of one output. Cheap and idempotent: every damage source
/// (commits, Lua ops, cursor motion) calls this; the state machine coalesces.
pub fn queue_redraw(takhti: &mut Takhti, node: DrmNode, crtc: crtc::Handle) {
    let Takhti {
        backend,
        loop_handle,
        ..
    } = takhti;
    let Backend::Tty(data) = backend else { return };
    let Some(surface) = data
        .devices
        .get_mut(&node)
        .and_then(|device| device.surfaces.get_mut(&crtc))
    else {
        return;
    };
    surface.redraw_state = match mem::take(&mut surface.redraw_state) {
        RedrawState::Idle => {
            loop_handle.insert_idle(move |takhti| render_surface(takhti, node, crtc));
            RedrawState::Queued
        }
        RedrawState::Queued => RedrawState::Queued,
        RedrawState::WaitingForVBlank { .. } => RedrawState::WaitingForVBlank {
            redraw_needed: true,
        },
        RedrawState::WaitingForEstimatedVBlank(token)
        | RedrawState::WaitingForEstimatedVBlankAndQueued(token) => {
            RedrawState::WaitingForEstimatedVBlankAndQueued(token)
        }
    };
}

pub fn queue_redraw_all(takhti: &mut Takhti) {
    let Backend::Tty(data) = &takhti.backend else {
        return;
    };
    let targets: Vec<(DrmNode, crtc::Handle)> = data
        .devices
        .iter()
        .flat_map(|(node, device)| device.surfaces.keys().map(move |crtc| (*node, *crtc)))
        .collect();
    for (node, crtc) in targets {
        queue_redraw(takhti, node, crtc);
    }
}

fn on_vblank(
    takhti: &mut Takhti,
    node: DrmNode,
    crtc: crtc::Handle,
    meta: Option<DrmEventMetadata>,
) {
    let now = takhti.clock.now();
    {
        let Backend::Tty(data) = &mut takhti.backend else {
            return;
        };
        let Some(surface) = data
            .devices
            .get_mut(&node)
            .and_then(|device| device.surfaces.get_mut(&crtc))
        else {
            return;
        };
        // The presented frame carries its presentation feedback as user data;
        // fire it with the hardware vblank timestamp when the kernel gave one.
        let presentation_time = meta.as_ref().and_then(|meta| match meta.time {
            DrmEventTime::Monotonic(time) => Some(time),
            DrmEventTime::Realtime(_) => None,
        });
        match surface.compositor.frame_submitted() {
            Ok(Some(mut feedback)) => {
                let refresh = surface
                    .output
                    .current_mode()
                    .filter(|mode| mode.refresh > 0)
                    .map(|mode| {
                        Refresh::fixed(Duration::from_secs_f64(1_000f64 / mode.refresh as f64))
                    })
                    .unwrap_or(Refresh::Unknown);
                let seq = meta.as_ref().map(|meta| meta.sequence as u64).unwrap_or(0);
                let mut flags = wp_presentation_feedback::Kind::Vsync
                    | wp_presentation_feedback::Kind::HwCompletion;
                if presentation_time.is_some() {
                    flags |= wp_presentation_feedback::Kind::HwClock;
                }
                let time = presentation_time.unwrap_or_else(|| now.into());
                feedback.presented::<_, Monotonic>(time, refresh, seq, flags);
            }
            Ok(None) => {}
            Err(err) => warn!("error marking frame submitted: {err}"),
        }
        match mem::take(&mut surface.redraw_state) {
            // Damage arrived while the frame was in flight: repaint again.
            RedrawState::WaitingForVBlank {
                redraw_needed: true,
            } => {}
            // Presented and nothing new: go idle until the next queue_redraw.
            RedrawState::WaitingForVBlank {
                redraw_needed: false,
            } => return,
            // Stale vblank (e.g. right after a VT switch): don't disturb.
            other => {
                surface.redraw_state = other;
                return;
            }
        }
    }
    queue_redraw(takhti, node, crtc);
}

/// The estimated-vblank timer fired: idle out, or repaint if damage arrived.
fn on_estimated_vblank(takhti: &mut Takhti, node: DrmNode, crtc: crtc::Handle) {
    {
        let Backend::Tty(data) = &mut takhti.backend else {
            return;
        };
        let Some(surface) = data
            .devices
            .get_mut(&node)
            .and_then(|device| device.surfaces.get_mut(&crtc))
        else {
            return;
        };
        match mem::take(&mut surface.redraw_state) {
            RedrawState::WaitingForEstimatedVBlank(_) => return,
            RedrawState::WaitingForEstimatedVBlankAndQueued(_) => {
                surface.redraw_state = RedrawState::Queued;
            }
            other => {
                surface.redraw_state = other;
                return;
            }
        }
    }
    render_surface(takhti, node, crtc);
}

/// After a no-damage render nothing is queued to DRM, so no vblank will
/// arrive. Schedule a timer one refresh interval out to stand in for it.
fn queue_estimated_vblank(
    loop_handle: &LoopHandle<'static, Takhti>,
    surface: &mut TtySurface,
    node: DrmNode,
    crtc: crtc::Handle,
) {
    // Reuse a timer that is already pending.
    match mem::take(&mut surface.redraw_state) {
        RedrawState::WaitingForEstimatedVBlank(token)
        | RedrawState::WaitingForEstimatedVBlankAndQueued(token) => {
            surface.redraw_state = RedrawState::WaitingForEstimatedVBlank(token);
            return;
        }
        _ => {}
    }
    let refresh_mhz = surface
        .output
        .current_mode()
        .map(|mode| mode.refresh)
        .filter(|&r| r > 0)
        .unwrap_or(60_000);
    let interval = Duration::from_secs_f64(1000.0 / refresh_mhz as f64);
    let timer = Timer::from_duration(interval);
    match loop_handle.insert_source(timer, move |_, _, takhti| {
        on_estimated_vblank(takhti, node, crtc);
        TimeoutAction::Drop
    }) {
        Ok(token) => surface.redraw_state = RedrawState::WaitingForEstimatedVBlank(token),
        Err(err) => {
            warn!("error scheduling estimated-vblank timer: {err}");
            surface.redraw_state = RedrawState::Idle;
        }
    }
}

pub fn render_surface(takhti: &mut Takhti, node: DrmNode, crtc: crtc::Handle) {
    // Data that needs shared access to `takhti`, gathered before splitting borrows.
    let output = {
        let Backend::Tty(data) = &takhti.backend else {
            return;
        };
        let Some(surface) = data
            .devices
            .get(&node)
            .and_then(|device| device.surfaces.get(&crtc))
        else {
            return;
        };
        surface.output.clone()
    };
    let (output_loc, output_size) = takhti
        .space
        .output_geometry(&output)
        .map(|geo| (geo.loc, geo.size))
        .unwrap_or_default();
    let pointer_pos = takhti
        .seat
        .get_pointer()
        .map(|p| p.current_location())
        .unwrap_or_default();
    let cursor_status = takhti.cursor_status.clone();
    let border_width = takhti.lua.settings().border_width;

    let Takhti {
        backend,
        space,
        start_time,
        loop_handle,
        cursor,
        ui,
        binds,
        border_buffers,
        ..
    } = takhti;
    let Backend::Tty(data) = backend else { return };
    let TtyData {
        gpu_manager,
        devices,
        primary_render_node,
        cursor_buffer,
        ..
    } = data;
    let Some(device) = devices.get_mut(&node) else {
        return;
    };
    // VT switched away: the device is paused, rendering would just error.
    if !device.drm.is_active() {
        return;
    }
    let Some(surface) = device.surfaces.get_mut(&crtc) else {
        return;
    };

    // Render on the primary GPU; when this output's device differs, the
    // MultiRenderer copies the finished frame across for scanout.
    let target_node = device.render_node.unwrap_or(*primary_render_node);
    let mut renderer = match gpu_manager.renderer(
        primary_render_node,
        &target_node,
        surface.compositor.format(),
    ) {
        Ok(renderer) => renderer,
        Err(err) => {
            warn!("error creating renderer: {err}");
            surface.redraw_state = RedrawState::Idle;
            return;
        }
    };

    let mut elements: Vec<OutputRenderElements<TtyRenderer<'_>>> = Vec::new();
    let scale = space.scale();

    // Cursor: client-provided surface, xcursor theme, or block fallback.
    // Pointer position converts from protocol-logical once, then everything
    // is physical and snapped to the grid.
    let cursor_phys = crate::coords::point_to_physical(pointer_pos, scale) - output_loc.to_f64();
    match &cursor_status {
        CursorImageStatus::Hidden => {}
        CursorImageStatus::Surface(cursor_surface) if cursor_surface.alive() => {
            let hotspot = with_states(cursor_surface, |states| {
                states
                    .data_map
                    .get::<CursorImageSurfaceData>()
                    .map(|data| data.lock().unwrap().hotspot)
            })
            .unwrap_or_default();
            // The hotspot is in the cursor surface's coordinates (logical).
            let hotspot_phys = crate::coords::logical_point_to_physical(hotspot.to_f64(), scale);
            let pos = (cursor_phys - hotspot_phys.to_f64()).to_i32_round();
            elements.extend(
                render_elements_from_surface_tree(
                    &mut renderer,
                    cursor_surface,
                    pos,
                    scale,
                    1.0,
                    Kind::Cursor,
                )
                .into_iter()
                .map(OutputRenderElements::Surface),
            );
        }
        _ => {
            if let Some(element) = cursor.element(&mut renderer, cursor_phys) {
                elements.push(OutputRenderElements::Memory(element));
            } else {
                elements.push(OutputRenderElements::Solid(
                    SolidColorRenderElement::from_buffer(
                        cursor_buffer,
                        cursor_phys.to_i32_round::<i32>(),
                        1.0,
                        1.0,
                        Kind::Cursor,
                    ),
                ));
            }
        }
    }

    // Compositor UI (dialogs/overlays): above windows, below the cursor.
    let ui_elements = ui.render_elements(&mut renderer, output_size, binds);
    let borders = crate::render::border_elements(space, border_buffers, border_width, output_loc);
    elements.extend(crate::render::scene_elements(
        &mut renderer,
        space,
        &surface.output,
        ui_elements,
        borders,
    ));

    match surface.compositor.render_frame(
        &mut renderer,
        &elements,
        CLEAR_COLOR,
        FrameFlags::empty(),
    ) {
        Ok(res) => {
            // KMS can't fence this frame (no IN_FENCE_FD, or the GL sync
            // isn't exportable — common on NVIDIA): wait for the render to
            // finish CPU-side or the display scans out a half-drawn buffer.
            if res.needs_sync() {
                if let PrimaryPlaneElement::Swapchain(element) = &res.primary_element {
                    if let Err(err) = element.sync.wait() {
                        warn!("error waiting for frame completion: {err:?}");
                    }
                }
            }
            send_frames(space, &surface.output, start_time.elapsed());
            if res.is_empty {
                queue_estimated_vblank(loop_handle, surface, node, crtc);
            } else {
                // Presentation feedback rides along as the frame's user data
                // and is fired from the vblank that presents it.
                let feedback =
                    crate::render::take_presentation_feedback(space, &surface.output, &res.states);
                match surface.compositor.queue_frame(feedback) {
                    Ok(()) => {
                        surface.redraw_state = RedrawState::WaitingForVBlank {
                            redraw_needed: false,
                        };
                    }
                    Err(err) => {
                        warn!("error queueing frame: {err}");
                        surface.redraw_state = RedrawState::Idle;
                    }
                }
            }
        }
        Err(err) => {
            warn!("error rendering frame: {err}");
            surface.redraw_state = RedrawState::Idle;
        }
    }
}

/// Import a client dmabuf on the primary GPU (the one that composites).
pub fn import_dmabuf(data: &mut TtyData, dmabuf: &Dmabuf) -> bool {
    let mut renderer = match data.gpu_manager.single_renderer(&data.primary_render_node) {
        Ok(renderer) => renderer,
        Err(err) => {
            debug!("error creating renderer for dmabuf import: {err}");
            return false;
        }
    };
    match renderer.import_dmabuf(dmabuf, None) {
        Ok(_texture) => {
            dmabuf.set_node(data.primary_render_node);
            true
        }
        Err(err) => {
            debug!("error importing dmabuf: {err}");
            false
        }
    }
}

fn send_frames(space: &PhysicalSpace, output: &Output, time: Duration) {
    for window in space.elements() {
        window.send_frame(output, time, Some(Duration::ZERO), |_, _| {
            Some(output.clone())
        });
    }
    // Layer surfaces (bars, launchers, wallpaper) render on frame callbacks
    // like any client; without these they freeze after their first frame.
    for layer in layer_map_for_output(output).layers() {
        layer.send_frame(output, time, Some(Duration::ZERO), |_, _| {
            Some(output.clone())
        });
    }
}

impl Takhti {
    pub fn on_session_event(&mut self, event: SessionEvent) {
        let Backend::Tty(data) = &mut self.backend else {
            return;
        };
        match event {
            SessionEvent::PauseSession => {
                info!("session paused (VT switched away)");
                data.libinput.suspend();
                for device in data.devices.values_mut() {
                    device.drm.pause();
                }
            }
            SessionEvent::ActivateSession => {
                info!("session activated");
                if data.libinput.resume().is_err() {
                    warn!("error resuming libinput");
                }
                let mut targets = Vec::new();
                for (node, device) in &mut data.devices {
                    if let Err(err) = device.drm.activate(false) {
                        warn!("error activating DRM device: {err}");
                    }
                    for (crtc, surface) in &mut device.surfaces {
                        if let Err(err) = surface.compositor.reset_state() {
                            warn!("error resetting DRM compositor state: {err}");
                        }
                        // Frames in flight were lost with the VT; cancel any
                        // pending estimated-vblank timer and start fresh.
                        match mem::take(&mut surface.redraw_state) {
                            RedrawState::WaitingForEstimatedVBlank(token)
                            | RedrawState::WaitingForEstimatedVBlankAndQueued(token) => {
                                self.loop_handle.remove(token);
                            }
                            _ => {}
                        }
                        targets.push((*node, *crtc));
                    }
                }
                for (node, crtc) in targets {
                    queue_redraw(self, node, crtc);
                }
            }
        }
    }

    pub fn change_vt(&mut self, vt: i32) {
        if let Backend::Tty(data) = &mut self.backend {
            if let Err(err) = data.session.change_vt(vt) {
                warn!("error switching VT: {err}");
            }
        }
    }
}
