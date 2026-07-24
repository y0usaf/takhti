//! moonshell's element engine as a compositor texture (FUSION.md F1).
//!
//! An element tree is CPU-rastered (tiny-skia + cosmic-text) into a
//! [`MemoryRenderBuffer`]; the scene cache diffs consecutive trees and
//! reports per-tree dirty rects, which become buffer damage — so the
//! GLES texture upload covers only the changed regions, and the
//! element composits like any other (camera transform, output scale,
//! damage tracking all downstream).
//!
//! Rasterization happens on state change ([`TreeTexture::update`]),
//! never per-frame. A raster deadline is enforced the same way the Lua
//! watchdog treats a slow hook: a tree that blows the budget logs and
//! has its *next* updates skipped (throttled) rather than stalling the
//! render loop on every state change. Raster stays on the main thread
//! by decision (DESIGN.md: worker handoff only if a real bar blows the
//! frame budget on real hardware).
//!
//! Coordinate doctrine: layout resolves to integer physical pixels
//! before raster (moonshell-render works in physical px; the scale
//! multiply happens once, in its layout pass), so trees are
//! pixel-stable at fractional output scales. The buffer is created at
//! scale 1 and positioned in physical coordinates, like every other
//! compositor-UI element.

use std::convert::Infallible;
use std::time::{Duration, Instant};

use moonshell_render::{Element, Renderer, Scene, SceneDamage};
use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::element::memory::MemoryRenderBuffer;
use smithay::utils::{Buffer as BufferCoords, Physical, Rectangle, Size, Transform};
use tracing::warn;

/// Raster budget per tree update. Generous — a bar-sized tree rasters
/// in well under a millisecond; only a pathological tree (huge canvas,
/// thousands of glyphs) trips this.
const RASTER_DEADLINE: Duration = Duration::from_millis(100);

/// How long a tree that blew the deadline is throttled before it may
/// raster again.
const SLOW_TREE_BACKOFF: Duration = Duration::from_secs(1);

/// The shared raster engine: font system, glyph caches, decoded asset
/// cache — the dominant allocation, so exactly one lives in [`super::Ui`]
/// and every tree rasters through it.
pub struct Engine {
    renderer: Renderer,
}

impl Engine {
    pub fn new() -> Self {
        Self {
            renderer: Renderer::new(),
        }
    }

    /// Intrinsic physical size of a tree at `scale` — for canvases that
    /// hug their content (dialogs) rather than fill a fixed surface.
    pub fn measure(&mut self, root: &Element, scale: f32) -> Size<i32, Physical> {
        let (w, h) = moonshell_render::intrinsic_size(root, scale, &mut self.renderer);
        Size::from((w as i32, h as i32))
    }
}

/// One element tree bound to one texture: scene cache + memory buffer.
pub struct TreeTexture {
    scene: Scene,
    buffer: MemoryRenderBuffer,
    size: Size<i32, Physical>,
    /// Set when the last raster blew [`RASTER_DEADLINE`]; updates are
    /// skipped until the backoff expires.
    throttled_until: Option<Instant>,
}

impl Default for TreeTexture {
    fn default() -> Self {
        Self::new()
    }
}

impl TreeTexture {
    pub fn new() -> Self {
        Self {
            scene: Scene::new(),
            // Placeholder until the first update; ARGB8888 matches
            // moonshell-render's output bytes (premultiplied BGRA).
            buffer: MemoryRenderBuffer::new(Fourcc::Argb8888, (1, 1), 1, Transform::Normal, None),
            size: Size::from((1, 1)),
            throttled_until: None,
        }
    }

    /// The backing buffer, for building a
    /// [`MemoryRenderBufferRenderElement`] at some location.
    ///
    /// [`MemoryRenderBufferRenderElement`]:
    /// smithay::backend::renderer::element::memory::MemoryRenderBufferRenderElement
    pub fn buffer(&self) -> &MemoryRenderBuffer {
        &self.buffer
    }

    /// Hit-test a texture-local physical point against the retained
    /// tree (FUSION F4): the deepest element path under the point.
    pub fn hit_path(&self, x: f32, y: f32) -> Option<Vec<usize>> {
        self.scene.hit_path(x, y)
    }

    /// Re-raster for a (possibly) new tree at `size`/`scale`. The scene
    /// cache makes the unchanged case free (no draw, no damage) and the
    /// small-change case cheap on the upload side (only diffed rects
    /// re-upload). Returns the buffer damage handed to the texture
    /// (empty = nothing changed), or `None` when the update was skipped
    /// by the slow-tree throttle.
    pub fn update(
        &mut self,
        engine: &mut Engine,
        root: &Element,
        size: Size<i32, Physical>,
        scale: f32,
    ) -> Option<Vec<Rectangle<i32, BufferCoords>>> {
        let now = Instant::now();
        if let Some(until) = self.throttled_until {
            if now < until {
                return None;
            }
            self.throttled_until = None;
        }
        let (w, h) = (size.w.max(1) as u32, size.h.max(1) as u32);
        let resized = self.size != size;
        if resized {
            self.size = size;
            self.scene.invalidate();
        }
        let scene = &mut self.scene;
        let mut reported = Vec::new();
        let mut ctx = self.buffer.render();
        if resized {
            ctx.resize((w as i32, h as i32));
        }
        let _ = ctx.draw::<_, Infallible>(|canvas| {
            let damage = scene.render(&mut engine.renderer, canvas, w, h, scale, root);
            reported = match damage {
                SceneDamage::None => Vec::new(),
                SceneDamage::Full => {
                    vec![Rectangle::<i32, BufferCoords>::from_size(
                        (w as i32, h as i32).into(),
                    )]
                }
                SceneDamage::Rects(rects) => rects
                    .into_iter()
                    .map(|r| Rectangle::new((r.x, r.y).into(), (r.w, r.h).into()))
                    .collect(),
            };
            Ok(reported.clone())
        });
        drop(ctx);
        let elapsed = now.elapsed();
        if elapsed > RASTER_DEADLINE {
            warn!(
                "element tree raster took {elapsed:?} (budget {RASTER_DEADLINE:?}); \
                 throttling updates for {SLOW_TREE_BACKOFF:?}"
            );
            self.throttled_until = Some(now + SLOW_TREE_BACKOFF);
        }
        Some(reported)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use moonshell_render::element::{Flex, Style};
    use moonshell_render::Rgba;

    /// Two fixed 50x20 boxes side by side; `right` colors the second.
    fn row(right: Rgba) -> Element {
        let cell = |bg: Rgba| {
            Element::VBox(Flex {
                style: Style {
                    bg: Some(bg),
                    width: Some(50.0),
                    height: Some(20.0),
                    ..Default::default()
                },
                ..Default::default()
            })
        };
        Element::HBox(Flex {
            children: vec![cell(Rgba::new(10, 10, 10, 255)), cell(right)],
            ..Default::default()
        })
    }

    /// The F1 accept case at the texture level: an animating element in
    /// one corner damages (and thus re-uploads) only its own rect.
    #[test]
    fn corner_change_uploads_only_its_rect() {
        let mut engine = Engine::new();
        let mut tex = TreeTexture::new();
        let size = Size::from((100, 20));

        let first = tex
            .update(&mut engine, &row(Rgba::new(200, 0, 0, 255)), size, 1.0)
            .unwrap();
        assert_eq!(first.len(), 1, "first raster damages the full buffer");
        assert_eq!(first[0].size, (100, 20).into());

        // Identical tree: zero damage, zero upload.
        let same = tex
            .update(&mut engine, &row(Rgba::new(200, 0, 0, 255)), size, 1.0)
            .unwrap();
        assert!(same.is_empty(), "unchanged tree must report no damage");

        // Recolor only the right cell: damage stays inside its rect
        // (inflated by the diff's 1px antialiasing margin).
        let changed = tex
            .update(&mut engine, &row(Rgba::new(0, 200, 0, 255)), size, 1.0)
            .unwrap();
        assert!(!changed.is_empty());
        for rect in &changed {
            assert!(
                rect.loc.x >= 49,
                "damage {rect:?} leaked into the unchanged left cell"
            );
        }
    }
}
