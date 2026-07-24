<!-- GENERATED FILE — do not edit by hand.
     Regenerate: scripts/gen-arch.sh
     Freshness is enforced by `nix flake check` (checks.arch-fresh). -->

# Architecture map (generated)

Structural map of the workspace, extracted from the code. For rationale,
invariants, and dataflow, see DESIGN.md / PLAN.md.

## Crate dependency graph

Internal (workspace-local) dependencies only. `A --> B` means A depends on B.

```mermaid
graph TD
  moonshell["moonshell"] --> moonshell_render["moonshell-render"]
  moonshell["moonshell"] --> moonshell_runtime["moonshell-runtime"]
  moonshell["moonshell"] --> moonshell_services["moonshell-services"]
  moonshell["moonshell"] --> moonshell_surface["moonshell-surface"]
  moonshell_runtime["moonshell-runtime"] --> moonshell_render["moonshell-render"]
  moonshell_runtime["moonshell-runtime"] --> moonshell_services["moonshell-services"]
  moonshell_runtime["moonshell-runtime"] --> moonshell_surface["moonshell-surface"]
  moonshell_services["moonshell-services"] --> tomoe_ipc["tomoe-ipc"]
  tomoe["tomoe"] --> moonshell_render["moonshell-render"]
  tomoe["tomoe"] --> tomoe_ipc["tomoe-ipc"]
  xdg_desktop_portal_tomoe["xdg-desktop-portal-tomoe"] --> tomoe_ipc["tomoe-ipc"]
```

## Crates

| Crate | Description |
|-------|-------------|
| `moonshell` | _no description in Cargo.toml_ |
| `moonshell-render` | _no description in Cargo.toml_ |
| `moonshell-runtime` | _no description in Cargo.toml_ |
| `moonshell-services` | _no description in Cargo.toml_ |
| `moonshell-surface` | _no description in Cargo.toml_ |
| `tomoe` | _no description in Cargo.toml_ |
| `tomoe-ipc` | _no description in Cargo.toml_ |
| `xdg-desktop-portal-tomoe` | _no description in Cargo.toml_ |

## Module structure

Per-crate module/item trees (`cargo modules structure`). Functions are
omitted; types, traits, and module boundaries are the architecture.

### `moonshell`

```

crate moonshell
├── struct Engine: pub(crate)
├── struct VersionBar: pub(crate)
└── mod watcher: pub(crate)
    ├── struct DirWatch: pub(self)
    └── struct Watcher: pub
```

### `moonshell-render`

```

crate moonshell_render
├── struct Renderer: pub
├── struct Rgba: pub
├── mod assets: pub(crate)
│   ├── struct AssetCache: pub(crate)
│   └── struct IconKey: pub(self)
├── mod draw: pub
├── mod element: pub
│   ├── enum Align: pub
│   ├── struct CircularProgress: pub
│   ├── struct Edges: pub
│   ├── enum Element: pub
│   ├── struct Flex: pub
│   ├── struct Icon: pub
│   ├── struct Image: pub
│   ├── enum Justify: pub
│   ├── enum Orientation: pub
│   ├── struct Progress: pub
│   ├── struct Separator: pub
│   ├── struct Spacer: pub
│   ├── struct Stack: pub
│   ├── struct Style: pub
│   └── struct Text: pub
├── mod layout: pub
│   ├── enum Axis: pub(self)
│   ├── struct LayoutNode: pub
│   └── struct Rect: pub
└── mod scene: pub
    ├── struct Frame: pub(self)
    ├── struct PixelRect: pub
    ├── struct Scene: pub
    └── enum SceneDamage: pub
```

### `moonshell-runtime`

```

crate moonshell_runtime
├── struct Vm: pub
├── mod api: pub
│   ├── struct PendingWindow: pub
│   └── struct ShellCtx: pub
├── mod element: pub
│   └── struct TextDefaults: pub
├── mod exec: pub
│   ├── struct ExecCallback: pub(crate)
│   └── struct ExecReply: pub
├── mod painter: pub
│   └── struct LuaPainter: pub
├── mod services_bridge: pub
├── mod state: pub
│   ├── struct LuaState: pub
│   ├── type Notifier: pub(self)
│   └── struct StateInner: pub(self)
├── mod timer: pub
│   └── struct PendingTimer: pub
├── mod watch: pub
│   ├── struct PendingWatch: pub
│   └── struct WatchCallback: pub
└── mod window: pub
    ├── struct WindowHandle: pub
    ├── struct WindowOpts: pub
    └── struct WindowShared: pub
```

### `moonshell-services`

```

crate moonshell_services
├── mod battery: pub
│   ├── struct BatteryState: pub
│   ├── struct Model: pub(self)
│   ├── type Notify: pub(self)
│   ├── enum Prop: pub(self)
│   ├── enum Source: pub
│   └── struct Upower: pub(self)
├── mod compositor: pub
│   ├── enum Compositor: pub
│   ├── struct CompositorState: pub
│   ├── enum Error: pub
│   ├── struct KeyboardActivity: pub
│   ├── enum KeyboardHand: pub
│   ├── type Notify: pub(self)
│   ├── struct Workspace: pub
│   ├── mod hyprland: pub
│   │   └── struct Backend: pub(self)
│   ├── mod niri: pub
│   │   ├── struct Backend: pub(self)
│   │   ├── struct Model: pub(self)
│   │   ├── struct Win: pub(self)
│   │   └── struct Ws: pub(self)
│   ├── mod sway: pub
│   │   └── struct Backend: pub(self)
│   ├── mod tomoe: pub
│   │   ├── struct Backend: pub(self)
│   │   └── struct Model: pub(self)
│   └── mod wire: pub(self)
├── mod dbus: pub(crate)
│   ├── struct Dbl: pub(crate)
│   └── enum DbusError: pub
├── mod mpris: pub
│   ├── struct Model: pub(self)
│   ├── struct Mpris: pub(self)
│   ├── struct MprisState: pub
│   ├── type Notify: pub(self)
│   ├── enum Pending: pub(self)
│   └── struct Player: pub(self)
└── mod network: pub
    ├── struct Model: pub(self)
    ├── struct NetworkState: pub
    ├── struct Nm: pub(self)
    ├── type Notify: pub(self)
    ├── enum Query: pub(self)
    ├── enum Scope: pub(self)
    ├── enum Source: pub
    └── enum Val: pub(self)
```

### `moonshell-surface`

```

crate moonshell_surface
├── struct Anchors: pub
├── struct Canvas: pub
├── enum Damage: pub
├── struct DamageRect: pub
├── struct DisplayInfo: pub
├── enum Edge: pub
├── enum Keyboard: pub
├── enum Layer: pub
├── struct LayerOptions: pub
├── struct Margins: pub
├── trait Painter: pub
├── struct Shell: pub
├── enum SurfaceError: pub
├── struct Window: pub(crate)
└── struct WindowId: pub
```

### `tomoe`

```

crate tomoe
├── struct Args: pub(crate)
├── enum Command: pub(crate)
├── mod animation: pub(crate)
│   ├── struct Animation: pub
│   ├── struct AnimationSettings: pub
│   ├── struct Animations: pub
│   ├── enum Config: pub
│   ├── struct CubicBezier: pub
│   ├── enum Curve: pub
│   ├── enum Kind: pub(self)
│   ├── struct Spring: pub
│   ├── struct SpringParams: pub
│   └── struct WindowAnims: pub(self)
├── mod backend: pub(crate)
│   ├── enum Backend: pub
│   ├── mod tty: pub
│   │   ├── struct GammaProps: pub
│   │   ├── type GbmDrmCompositor: pub
│   │   ├── struct OutputDevice: pub
│   │   ├── enum RedrawState: pub
│   │   ├── struct SurfaceDmabufFeedback: pub
│   │   ├── struct TtyData: pub
│   │   ├── type TtyFrame: pub
│   │   ├── type TtyGpuManager: pub
│   │   ├── type TtyRenderer: pub
│   │   ├── type TtyRendererError: pub
│   │   └── struct TtySurface: pub
│   └── mod winit: pub
│       └── struct WinitData: pub
├── mod capture: pub(crate)
│   ├── type CaptureElement: pub(self)
│   ├── enum CaptureTarget: pub(self)
│   └── struct SceneParts: pub(self)
├── mod coords: pub(crate)
├── mod cursor: pub(crate)
│   └── struct Cursor: pub
├── mod foreign_toplevel: pub(crate)
│   └── struct ForeignWindowId: pub
├── mod handlers: pub(crate)
│   └── struct UrgentOnlyMarker: pub(self)
├── mod input: pub(crate)
│   ├── enum Action: pub
│   ├── struct Bind: pub
│   ├── enum ModKey: pub
│   └── struct Mods: pub
├── mod ipc: pub(crate)
│   ├── struct Client: pub(self)
│   └── struct IpcState: pub
├── mod layout: pub(crate)
├── mod lock: pub(crate)
│   └── enum LockState: pub
├── mod lua: pub(crate)
│   ├── enum AccelProfile: pub
│   ├── struct BlurSettings: pub
│   ├── enum ClickMethod: pub
│   ├── struct DisplaySettings: pub
│   ├── struct Hooks: pub(self)
│   ├── struct InputConfig: pub
│   ├── struct InputDeviceSettings: pub
│   ├── struct KeyboardSettings: pub
│   ├── struct LuaRuntime: pub
│   ├── struct LuaScreencastRequest: pub(self)
│   ├── struct LuaWindow: pub(self)
│   ├── struct OutputProps: pub
│   ├── struct PendingBind: pub
│   ├── struct PointerAxisData: pub
│   ├── struct PointerButtonData: pub
│   ├── struct PointerGrab: pub(self)
│   ├── enum RefreshSetting: pub
│   ├── struct ReloadHooks: pub(self)
│   ├── struct Resolution: pub
│   ├── struct Rule: pub(self)
│   ├── enum ScreencastHookOutcome: pub
│   ├── enum ScreencastReply: pub
│   ├── struct ScreencastRequestState: pub(self)
│   ├── enum ScrollMethod: pub
│   ├── struct Settings: pub
│   ├── struct Shared: pub(self)
│   ├── enum SizeSetting: pub
│   ├── struct UiCallbacks: pub(self)
│   ├── struct UiHandle: pub(self)
│   ├── enum UiOp: pub
│   ├── struct WatchdogGuard: pub(self)
│   ├── struct WinProps: pub
│   ├── enum WindowOp: pub
│   └── struct WindowProperties: pub
├── mod process: pub(crate)
│   ├── enum Launch: pub
│   ├── enum ProcessDecl: pub
│   ├── struct ProcessManager: pub
│   ├── struct ProcessSpec: pub
│   ├── enum ReloadPolicy: pub
│   ├── enum RestartPolicy: pub
│   ├── enum RunPolicy: pub
│   └── struct Service: pub(self)
├── mod protocols: pub(crate)
│   ├── mod background_effect: pub
│   ├── mod gamma_control: pub
│   │   ├── trait GammaControlHandler: pub
│   │   ├── struct GammaControlManagerGlobalData: pub
│   │   ├── struct GammaControlManagerState: pub
│   │   └── struct GammaControlState: pub
│   ├── mod screencopy: pub
│   │   ├── struct Screencopy: pub
│   │   ├── enum ScreencopyBuffer: pub
│   │   ├── struct ScreencopyFrameInfo: pub
│   │   ├── enum ScreencopyFrameState: pub
│   │   ├── trait ScreencopyHandler: pub
│   │   ├── struct ScreencopyManagerGlobalData: pub
│   │   ├── struct ScreencopyManagerState: pub
│   │   └── struct ScreencopyQueue: pub
│   ├── mod tearing_control: pub
│   │   ├── struct TearingControlData: pub
│   │   ├── struct TearingControlManagerState: pub
│   │   └── struct TearingControlSurfaceData: pub(self)
│   └── mod wlr_foreign_toplevel: pub
│       ├── struct ForeignHandleId: pub
│       ├── enum ForeignRequest: pub
│       ├── struct ToplevelData: pub(self)
│       ├── struct ToplevelInfo: pub
│       ├── struct WlrForeignToplevelGlobalData: pub
│       ├── trait WlrForeignToplevelHandler: pub
│       └── struct WlrForeignToplevelState: pub
├── mod render: pub(crate)
│   ├── enum OutputRenderElements: pub
│   ├── trait TomoeRenderer: pub
│   ├── mod blur: pub
│   │   ├── struct Blur: pub
│   │   ├── struct BlurOptions: pub
│   │   ├── struct BlurProgram: pub
│   │   ├── struct BlurProgramInner: pub(self)
│   │   └── struct BlurProgramInternal: pub(self)
│   ├── mod border: pub
│   │   ├── struct BorderRenderElement: pub
│   │   └── struct Parameters: pub(self)
│   ├── mod clipped_surface: pub
│   │   └── struct ClippedSurfaceRenderElement: pub
│   ├── mod damage: pub
│   │   └── struct ExtraDamage: pub
│   ├── mod framebuffer_effect: pub
│   │   ├── struct EffectState: pub(self)
│   │   ├── struct FramebufferEffect: pub
│   │   ├── struct FramebufferEffectElement: pub
│   │   └── struct Inner: pub(self)
│   ├── mod macros: pub(self)
│   ├── mod renderer: pub
│   │   ├── trait AsGlesFrame: pub
│   │   └── trait AsGlesRenderer: pub
│   ├── mod resources: pub(self)
│   │   └── struct Resources: pub
│   ├── mod shader_element: pub(self)
│   │   ├── struct ShaderProgram: pub
│   │   ├── struct ShaderProgramInner: pub(self)
│   │   ├── struct ShaderProgramInternal: pub(self)
│   │   └── struct ShaderRenderElement: pub
│   ├── mod shaders: pub
│   │   ├── enum ProgramType: pub
│   │   └── struct Shaders: pub
│   └── mod shadow: pub
│       ├── struct Parameters: pub(self)
│       └── struct ShadowRenderElement: pub
├── mod screenshot: pub(crate)
├── mod space: pub(crate)
│   └── struct PhysicalSpace: pub
├── mod state: pub(crate)
│   ├── struct ClientState: pub
│   ├── struct ConfigFingerprint: pub(self)
│   └── struct Tomoe: pub
│       ├── type KeyboardFocus: pub(self)
│       ├── type PointerFocus: pub(self)
│       ├── type SelectionUserData: pub(self)
│       └── type TouchFocus: pub(self)
├── mod ui: pub(crate)
│   ├── struct Ui: pub
│   ├── mod element_tree: pub
│   │   ├── struct Engine: pub
│   │   └── struct TreeTexture: pub
│   ├── mod screenshot_ui: pub(self)
│   │   ├── struct ScreenshotUi: pub
│   │   └── enum State: pub(self)
│   ├── mod text: pub
│   │   ├── struct Canvas: pub
│   │   ├── enum Face: pub
│   │   ├── struct Fonts: pub
│   │   └── struct Span: pub
│   └── mod widgets: pub
│       ├── struct MenuLayout: pub(self)
│       ├── enum Tag: pub
│       ├── enum UiEvent: pub
│       ├── struct WidgetEntry: pub
│       ├── enum WidgetHandler: pub
│       ├── enum WidgetKind: pub
│       ├── enum WidgetSpec: pub
│       └── struct Widgets: pub
└── mod xwayland: pub(crate)
    ├── struct Satellite: pub
    ├── enum ToMain: pub(self)
    ├── struct Unlink: pub(self)
    └── struct X11Connection: pub(self)
```

### `tomoe-ipc`

```

crate tomoe_ipc
├── struct Client: pub
├── struct Event: pub
├── struct Output: pub
├── struct Rect: pub
├── struct Request: pub
└── struct Window: pub
```

### `xdg-desktop-portal-tomoe`

```

crate xdg_desktop_portal_tomoe
├── mod outputs: pub(crate)
│   ├── struct Enumerator: pub(self)
│   └── struct OutputInfo: pub
├── mod pipewire_stream: pub(crate)
│   ├── struct AllocatedSlot: pub(self)
│   ├── struct AppState: pub(self)
│   ├── struct BufferSlot: pub(self)
│   ├── enum BufferSlotStorage: pub(self)
│   ├── struct FdHolder: pub(self)
│   ├── struct PendingFrame: pub(self)
│   ├── struct StreamHandle: pub
│   └── struct StreamSpec: pub
├── mod screencast: pub(crate)
│   ├── enum IpcPick: pub(self)
│   ├── enum LiveStream: pub(self)
│   ├── struct ScreenCast: pub
│   ├── enum Selection: pub(self)
│   ├── mod cursor_modes: pub(self)
│   └── mod source_types: pub(self)
├── mod toplevel_stream: pub(crate)
│   ├── struct AppState: pub(self)
│   ├── struct BufferSlot: pub(self)
│   ├── struct DiscoveredToplevel: pub(self)
│   ├── struct FdHolder: pub(self)
│   ├── struct PendingFrame: pub(self)
│   ├── struct StreamHandle: pub
│   ├── struct StreamInfo: pub
│   └── struct StreamSpec: pub
└── mod toplevels: pub(crate)
    ├── struct Enumerator: pub(self)
    └── struct ToplevelInfo: pub
```
