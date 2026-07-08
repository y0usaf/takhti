//! The moonshell binary. With no config (there is no config yet — the
//! runtime is M2), this maps one layer surface and draws a version
//! string: the doctrine-06 bare-core artifact. `--boot-check` exits 0
//! right after the first frame is committed, which is what
//! `nix flake check` runs under a headless compositor.

use moonshell_render::element::{Align, Edges, Flex, Spacer, Style, Text};
use moonshell_render::{Element, Renderer, Rgba};
use moonshell_surface::{Canvas, Damage, LayerOptions, Painter};

const BG: Rgba = Rgba::new(0x14, 0x14, 0x1e, 0xff);
const FG: Rgba = Rgba::new(0xc8, 0xc8, 0xd8, 0xff);

struct VersionBar {
    renderer: Renderer,
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
        } = canvas;
        moonshell_render::render_tree(
            &mut self.renderer,
            buf,
            width,
            height,
            scale as f32,
            &self.root,
        );
        Damage::Full
    }
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("moonshell=info")),
        )
        .init();

    let mut boot_check = false;
    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "--boot-check" => boot_check = true,
            "--version" | "-V" => {
                println!("moonshell {}", env!("CARGO_PKG_VERSION"));
                return Ok(());
            }
            other => anyhow::bail!("unknown argument: {other} (try --version, --boot-check)"),
        }
    }

    let options = LayerOptions {
        exit_after_first_draw: boot_check,
        ..LayerOptions::default()
    };
    let painter = VersionBar::new(format!("moonshell {}", env!("CARGO_PKG_VERSION")));
    moonshell_surface::run(options, Box::new(painter))?;
    Ok(())
}
