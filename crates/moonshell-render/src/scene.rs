//! Frame-to-frame scene cache and per-element damage diffing: retain
//! the previous element/layout trees, diff against the new ones, and
//! report only the changed regions to the compositor.
//!
//! The canvas is still repainted in full whenever anything changed:
//! shm buffers alternate, so the canvas handed to a painter holds the
//! frame from *two* commits ago, and partial repaint would need
//! per-slot damage accumulation (buffer age) in the surface crate.
//! Reported damage is the win at bar cadence — the compositor only
//! re-composites the changed rects — and an unchanged tree skips
//! layout (and its text shaping) entirely. Partial repaint is deferred
//! until profiling demands it (PLAN.md).
//!
//! Diff rules, walked over both trees in lockstep:
//! - equal subtree (element and layout) → no damage;
//! - same container "shell" (props equal except children, same child
//!   count, same rect) → recurse into children pairwise;
//! - anything else → damage the old and new subtree *bounds* (children
//!   may overflow their container, so the container rect alone is not
//!   enough).
//!
//! Rects are rounded with the draw pass's edge rule, inflated by one
//! pixel (antialiasing and small glyph-overhang insurance — extreme
//! italic overhang is a known limitation), clamped to the canvas, and
//! coalesced. Over-reporting damage is always safe because the full
//! repaint is deterministic: pixels outside the diff are bit-identical
//! to the previous frame.

use crate::draw;
use crate::element::Element;
use crate::layout::{self, LayoutNode, Rect};
use crate::{Renderer, Rgba};

/// Integer-pixel damage rect, clamped to the canvas.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PixelRect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

/// What changed between two rendered frames.
#[derive(Debug)]
pub enum SceneDamage {
    /// Nothing changed; nothing was drawn — skip the commit.
    None,
    /// Everything: first frame, geometry change, or invalidation.
    Full,
    /// Only these regions differ from the previous frame.
    Rects(Vec<PixelRect>),
}

/// The retained previous frame (also the future hit-testing input, M4).
struct Frame {
    root: Element,
    layout: LayoutNode,
    width: u32,
    height: u32,
    scale: f32,
}

/// Per-surface scene cache. Feed every paint through [`Scene::render`];
/// call [`Scene::invalidate`] when the surface lost its content (remap).
#[derive(Default)]
pub struct Scene {
    prev: Option<Frame>,
}

impl Scene {
    pub fn new() -> Self {
        Self::default()
    }

    /// Drop the cached frame: the next render repaints and reports
    /// [`SceneDamage::Full`].
    pub fn invalidate(&mut self) {
        self.prev = None;
    }

    /// Deepest element path containing the physical-px point, walked
    /// against the retained frame (the hit-testing input M4 promised).
    /// A path is the child-index chain from the root (empty = the root
    /// itself); later siblings win (stack overlays draw last). None:
    /// no retained frame, or the point misses the canvas.
    pub fn hit_path(&self, x: f32, y: f32) -> Option<Vec<usize>> {
        let frame = self.prev.as_ref()?;
        fn contains(r: &crate::layout::Rect, x: f32, y: f32) -> bool {
            x >= r.x && y >= r.y && x < r.x + r.w && y < r.y + r.h
        }
        if !contains(&frame.layout.rect, x, y) {
            return None;
        }
        let mut path = Vec::new();
        let (mut el, mut node) = (&frame.root, &frame.layout);
        loop {
            let child = el
                .children()
                .iter()
                .zip(&node.children)
                .enumerate()
                .rev()
                .find(|(_, (_, cn))| contains(&cn.rect, x, y));
            match child {
                Some((i, (c, cn))) => {
                    path.push(i);
                    el = c;
                    node = cn;
                }
                None => return Some(path),
            }
        }
    }

    /// Layout and draw `root`, returning damage relative to the
    /// previous `render` call. The canvas is repainted in full whenever
    /// the damage is non-`None` (see module docs), so pixels outside
    /// the reported damage match the previous frame by determinism.
    pub fn render(
        &mut self,
        r: &mut Renderer,
        canvas: &mut [u8],
        width: u32,
        height: u32,
        scale: f32,
        root: &Element,
    ) -> SceneDamage {
        // Identical tree at identical geometry: skip layout (and its
        // text shaping) — the zero-work steady state.
        if let Some(p) = &self.prev {
            if p.width == width && p.height == height && p.scale == scale && p.root == *root {
                return SceneDamage::None;
            }
        }
        let node = layout::compute(root, width as f32, height as f32, scale, r);
        let damage = match &self.prev {
            Some(p) if p.width == width && p.height == height && p.scale == scale => {
                let mut raw = Vec::new();
                diff(&p.root, &p.layout, root, &node, &mut raw);
                let rects = coalesce(
                    raw.into_iter()
                        .filter_map(|rect| to_pixels(rect, width, height))
                        .collect(),
                );
                if rects.is_empty() {
                    // The trees differ but every changed rect rounded or
                    // clamped away — nothing visible moved.
                    SceneDamage::None
                } else {
                    SceneDamage::Rects(rects)
                }
            }
            _ => SceneDamage::Full,
        };
        if !matches!(damage, SceneDamage::None) {
            // SlotPool recycles buffers. Clear before the full redraw so
            // transparent pixels in a new image frame erase old pixels.
            r.clear(canvas, width, height, Rgba::new(0, 0, 0, 0));
            draw::draw(r, canvas, width, height, scale, root, &node);
        }
        self.prev = Some(Frame {
            root: root.clone(),
            layout: node,
            width,
            height,
            scale,
        });
        damage
    }
}

/// Collect damage from the (old, new) tree pair into `out` (f32 rects,
/// physical px; conversion and coalescing happen after).
fn diff(oe: &Element, on: &LayoutNode, ne: &Element, nn: &LayoutNode, out: &mut Vec<Rect>) {
    if oe == ne && on == nn {
        return;
    }
    if same_shell(oe, ne) && on.rect == nn.rect {
        // Container itself unchanged (its bg repaints identically);
        // only descendants can differ.
        for ((oc, ocn), (nc, ncn)) in oe
            .children()
            .iter()
            .zip(&on.children)
            .zip(ne.children().iter().zip(&nn.children))
        {
            diff(oc, ocn, nc, ncn, out);
        }
        return;
    }
    out.push(bounds(on));
    out.push(bounds(nn));
}

/// Containers equal in everything but their children's *contents*
/// (child count must match so the pairwise walk stays in lockstep).
fn same_shell(a: &Element, b: &Element) -> bool {
    match (a, b) {
        (Element::HBox(x), Element::HBox(y)) | (Element::VBox(x), Element::VBox(y)) => {
            x.style == y.style
                && x.gap == y.gap
                && x.padding == y.padding
                && x.justify == y.justify
                && x.align == y.align
                && x.children.len() == y.children.len()
        }
        (Element::Stack(x), Element::Stack(y)) => {
            x.style == y.style && x.children.len() == y.children.len()
        }
        _ => false,
    }
}

/// Union of a node's rect with all descendant rects — children may
/// overflow their container when content doesn't fit.
fn bounds(n: &LayoutNode) -> Rect {
    n.children
        .iter()
        .fold(n.rect, |acc, c| union_f(acc, bounds(c)))
}

fn union_f(a: Rect, b: Rect) -> Rect {
    let x0 = a.x.min(b.x);
    let y0 = a.y.min(b.y);
    let x1 = (a.x + a.w).max(b.x + b.w);
    let y1 = (a.y + a.h).max(b.y + b.h);
    Rect {
        x: x0,
        y: y0,
        w: x1 - x0,
        h: y1 - y0,
    }
}

/// Draw-pass edge rounding, one pixel of inflation, canvas clamp.
/// `None` when the result is empty (off-canvas or degenerate).
fn to_pixels(r: Rect, width: u32, height: u32) -> Option<PixelRect> {
    let x0 = (r.x.round() as i32 - 1).max(0);
    let y0 = (r.y.round() as i32 - 1).max(0);
    let x1 = ((r.x + r.w).round() as i32 + 1).min(width as i32);
    let y1 = ((r.y + r.h).round() as i32 + 1).min(height as i32);
    (x1 > x0 && y1 > y0).then_some(PixelRect {
        x: x0,
        y: y0,
        w: x1 - x0,
        h: y1 - y0,
    })
}

/// Merge overlapping rects until none intersect. O(n²) per pass, but n
/// is small (damage sources per frame) and inflation makes adjacent
/// rects overlap, so runs converge fast.
fn coalesce(mut v: Vec<PixelRect>) -> Vec<PixelRect> {
    let mut i = 0;
    while i < v.len() {
        let mut merged = false;
        let mut j = i + 1;
        while j < v.len() {
            if overlaps(&v[i], &v[j]) {
                v[i] = union_px(&v[i], &v[j]);
                v.swap_remove(j);
                merged = true;
            } else {
                j += 1;
            }
        }
        // The grown rect may now overlap earlier survivors — restart.
        if merged {
            i = 0;
        } else {
            i += 1;
        }
    }
    v
}

fn overlaps(a: &PixelRect, b: &PixelRect) -> bool {
    a.x < b.x + b.w && b.x < a.x + a.w && a.y < b.y + b.h && b.y < a.y + a.h
}

fn union_px(a: &PixelRect, b: &PixelRect) -> PixelRect {
    let x0 = a.x.min(b.x);
    let y0 = a.y.min(b.y);
    let x1 = (a.x + a.w).max(b.x + b.w);
    let y1 = (a.y + a.h).max(b.y + b.h);
    PixelRect {
        x: x0,
        y: y0,
        w: x1 - x0,
        h: y1 - y0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::element::*;
    use crate::Rgba;

    const W: u32 = 40;
    const H: u32 = 8;
    const RED: Rgba = Rgba::new(255, 0, 0, 255);
    const BLUE: Rgba = Rgba::new(0, 0, 255, 255);

    fn buf() -> Vec<u8> {
        vec![0u8; (W * H * 4) as usize]
    }

    fn pixel(buf: &[u8], x: u32, y: u32) -> &[u8] {
        let i = ((y * W + x) * 4) as usize;
        &buf[i..i + 4]
    }

    fn fixed(w: f32, h: f32, bg: Rgba) -> Element {
        Element::VBox(Flex {
            style: Style {
                width: Some(w),
                height: Some(h),
                bg: Some(bg),
                ..Style::default()
            },
            ..Flex::default()
        })
    }

    /// Two 10-wide boxes with a 4px gap: second box occupies x 14..24.
    fn boxes(second: Rgba) -> Element {
        Element::HBox(Flex {
            gap: 4.0,
            children: vec![fixed(10.0, 8.0, RED), fixed(10.0, 8.0, second)],
            ..Flex::default()
        })
    }

    #[test]
    fn first_frame_full_then_none() {
        let mut r = Renderer::new();
        let mut scene = Scene::new();
        let root = boxes(BLUE);
        let mut b = buf();
        assert!(matches!(
            scene.render(&mut r, &mut b, W, H, 1.0, &root),
            SceneDamage::Full
        ));
        assert!(matches!(
            scene.render(&mut r, &mut b, W, H, 1.0, &root),
            SceneDamage::None
        ));
    }

    /// A leaf change damages that leaf's rect (inflated by 1), not its
    /// unchanged sibling — and the repaint actually lands the new color.
    #[test]
    fn leaf_change_damages_only_its_rect() {
        let mut r = Renderer::new();
        let mut scene = Scene::new();
        let mut b = buf();
        scene.render(&mut r, &mut b, W, H, 1.0, &boxes(BLUE));
        let damage = scene.render(&mut r, &mut b, W, H, 1.0, &boxes(RED));
        let SceneDamage::Rects(rects) = damage else {
            panic!("expected rects, got {damage:?}");
        };
        assert_eq!(
            rects,
            vec![PixelRect {
                x: 13,
                y: 0,
                w: 12,
                h: 8
            }]
        );
        // Sibling untouched, changed box repainted (buffer order BGRA).
        assert_eq!(pixel(&b, 1, 1), &[0, 0, 255, 255], "first box still red");
        assert_eq!(pixel(&b, 15, 1), &[0, 0, 255, 255], "second box now red");
    }

    #[test]
    fn geometry_change_is_full() {
        let mut r = Renderer::new();
        let mut scene = Scene::new();
        let root = boxes(BLUE);
        let mut b = buf();
        scene.render(&mut r, &mut b, W, H, 1.0, &root);
        let mut wider = vec![0u8; ((W + 8) * H * 4) as usize];
        assert!(matches!(
            scene.render(&mut r, &mut wider, W + 8, H, 1.0, &root),
            SceneDamage::Full
        ));
        // Back at the old size with a new scale: still Full.
        assert!(matches!(
            scene.render(&mut r, &mut b, W, H, 2.0, &root),
            SceneDamage::Full
        ));
    }

    /// Child count change breaks the shell match: the container's whole
    /// bounds are damaged.
    #[test]
    fn structure_change_damages_container_bounds() {
        let mut r = Renderer::new();
        let mut scene = Scene::new();
        let mut b = buf();
        let one = Element::HBox(Flex {
            children: vec![fixed(10.0, 8.0, RED)],
            ..Flex::default()
        });
        let two = Element::HBox(Flex {
            children: vec![fixed(10.0, 8.0, RED), fixed(10.0, 8.0, BLUE)],
            ..Flex::default()
        });
        scene.render(&mut r, &mut b, W, H, 1.0, &one);
        let damage = scene.render(&mut r, &mut b, W, H, 1.0, &two);
        let SceneDamage::Rects(rects) = damage else {
            panic!("expected rects, got {damage:?}");
        };
        assert_eq!(
            rects,
            vec![PixelRect {
                x: 0,
                y: 0,
                w: W as i32,
                h: H as i32
            }]
        );
    }

    #[test]
    fn invalidate_forces_full() {
        let mut r = Renderer::new();
        let mut scene = Scene::new();
        let root = boxes(BLUE);
        let mut b = buf();
        scene.render(&mut r, &mut b, W, H, 1.0, &root);
        scene.invalidate();
        assert!(matches!(
            scene.render(&mut r, &mut b, W, H, 1.0, &root),
            SceneDamage::Full
        ));
    }

    #[test]
    fn transparent_image_swap_clears_recycled_pixels() {
        let first_path =
            std::env::temp_dir().join(format!("moonshell-scene-{}-first.png", std::process::id()));
        let second_path =
            std::env::temp_dir().join(format!("moonshell-scene-{}-second.png", std::process::id()));

        let mut first = image::RgbaImage::from_pixel(W, H, image::Rgba([0, 0, 0, 0]));
        first.put_pixel(2, 2, image::Rgba([255, 0, 0, 255]));
        first.save(&first_path).unwrap();

        let mut second = image::RgbaImage::from_pixel(W, H, image::Rgba([0, 0, 0, 0]));
        second.put_pixel(30, 2, image::Rgba([0, 0, 255, 255]));
        second.save(&second_path).unwrap();

        let image = |src| {
            Element::Image(Image {
                style: Style {
                    width: Some(W as f32),
                    height: Some(H as f32),
                    ..Style::default()
                },
                src,
            })
        };

        let mut r = Renderer::new();
        let mut scene = Scene::new();
        let mut b = buf();
        scene.render(&mut r, &mut b, W, H, 1.0, &image(first_path.clone()));
        assert_eq!(pixel(&b, 2, 2), &[0, 0, 255, 255]);

        scene.render(&mut r, &mut b, W, H, 1.0, &image(second_path.clone()));
        assert_eq!(
            pixel(&b, 2, 2),
            &[0, 0, 0, 0],
            "old transparent pixel remains"
        );
        assert_eq!(pixel(&b, 30, 2), &[255, 0, 0, 255]);

        std::fs::remove_file(first_path).ok();
        std::fs::remove_file(second_path).ok();
    }

    #[test]
    fn coalesce_merges_overlaps() {
        let a = PixelRect {
            x: 0,
            y: 0,
            w: 10,
            h: 10,
        };
        let b = PixelRect {
            x: 5,
            y: 5,
            w: 10,
            h: 10,
        };
        let c = PixelRect {
            x: 30,
            y: 0,
            w: 5,
            h: 5,
        };
        let merged = coalesce(vec![a, b, c]);
        assert_eq!(
            merged,
            vec![
                PixelRect {
                    x: 0,
                    y: 0,
                    w: 15,
                    h: 15
                },
                c
            ]
        );
    }
}
