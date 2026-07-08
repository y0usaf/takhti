//! Draw pass: walk the element tree and its layout tree in lockstep,
//! painting each node with the [`Renderer`] primitives. Rect edges are
//! rounded to integer pixels here (x0/x1 rounding, so adjacent rects
//! meet without seams) — text origins in particular, because CPU
//! rendering makes misalignment *blurry text*.

use crate::element::Element;
use crate::layout::{self, LayoutNode, Rect};
use crate::{Renderer, Rgba};

/// Layout `root` for the whole canvas and draw it. The convenience
/// entry point for callers that don't cache the layout tree.
pub fn render_tree(
    renderer: &mut Renderer,
    canvas: &mut [u8],
    width: u32,
    height: u32,
    scale: f32,
    root: &Element,
) {
    let node = layout::compute(root, width as f32, height as f32, scale, renderer);
    draw(renderer, canvas, width, height, scale, root, &node);
}

/// Draw an element tree against a previously computed layout tree.
/// The two must come from the same `root` (structure is mirrored).
pub fn draw(
    r: &mut Renderer,
    canvas: &mut [u8],
    width: u32,
    height: u32,
    scale: f32,
    el: &Element,
    node: &LayoutNode,
) {
    let rect = rounded(node.rect);
    let style = el.style();
    if let Some(bg) = style.bg {
        fill(
            r,
            canvas,
            width,
            height,
            rect,
            style.border_radius * scale,
            bg,
        );
    }
    match el {
        Element::HBox(f) | Element::VBox(f) => {
            for (child, child_node) in f.children.iter().zip(&node.children) {
                draw(r, canvas, width, height, scale, child, child_node);
            }
        }
        Element::Stack(s) => {
            for (child, child_node) in s.children.iter().zip(&node.children) {
                draw(r, canvas, width, height, scale, child, child_node);
            }
        }
        Element::Text(t) => {
            let line = layout::text_line_px(t, scale);
            r.text_line(
                canvas,
                width,
                height,
                rect.x as i32,
                rect.y as i32,
                &t.content,
                t.size * scale,
                line,
                t.color,
            );
        }
        Element::Spacer(_) => {}
        Element::Separator(sep) => {
            fill(r, canvas, width, height, rect, 0.0, sep.color);
        }
        Element::Progress(p) => {
            let radius = p.style.border_radius * scale;
            fill(r, canvas, width, height, rect, radius, p.track);
            let value = p.value.clamp(0.0, 1.0);
            let filled = Rect {
                w: (rect.w * value).round(),
                ..rect
            };
            if filled.w > 0.0 {
                fill(r, canvas, width, height, filled, radius, p.color);
            }
        }
        Element::CircularProgress(c) => {
            let thickness = c.thickness * scale;
            let radius = ((rect.w.min(rect.h) - thickness) / 2.0).max(0.0);
            let cx = rect.x + rect.w / 2.0;
            let cy = rect.y + rect.h / 2.0;
            r.stroke_arc(
                canvas, width, height, cx, cy, radius, thickness, -90.0, 360.0, c.track,
            );
            let sweep = 360.0 * c.value.clamp(0.0, 1.0);
            if sweep > 0.0 {
                r.stroke_arc(
                    canvas, width, height, cx, cy, radius, thickness, -90.0, sweep, c.color,
                );
            }
        }
    }
}

/// Round edges (x0, y0, x1, y1) to integers, not (x, w) — adjacent
/// rects share edges instead of opening one-pixel seams.
fn rounded(r: Rect) -> Rect {
    let x0 = r.x.round();
    let y0 = r.y.round();
    Rect {
        x: x0,
        y: y0,
        w: (r.x + r.w).round() - x0,
        h: (r.y + r.h).round() - y0,
    }
}

fn fill(
    r: &mut Renderer,
    canvas: &mut [u8],
    width: u32,
    height: u32,
    rect: Rect,
    radius: f32,
    color: Rgba,
) {
    if radius > 0.5 {
        r.fill_rounded_rect(
            canvas, width, height, rect.x, rect.y, rect.w, rect.h, radius, color,
        );
    } else {
        r.fill_rect(canvas, width, height, rect.x, rect.y, rect.w, rect.h, color);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::element::*;

    const W: u32 = 40;
    const H: u32 = 8;

    fn pixel(buf: &[u8], x: u32, y: u32) -> &[u8] {
        let i = ((y * W + x) * 4) as usize;
        &buf[i..i + 4]
    }

    /// Progress: left half fill color, right half track color
    /// (ARGB8888 little-endian = [B, G, R, A]).
    #[test]
    fn progress_fills_by_value() {
        let mut r = Renderer::new();
        let mut buf = vec![0u8; (W * H * 4) as usize];
        let root = Element::Progress(Progress {
            style: Style {
                width: Some(W as f32),
                height: Some(H as f32),
                border_radius: 0.0,
                ..Style::default()
            },
            value: 0.5,
            color: Rgba::new(255, 0, 0, 255),
            track: Rgba::new(0, 0, 255, 255),
        });
        render_tree(&mut r, &mut buf, W, H, 1.0, &root);
        assert_eq!(pixel(&buf, 5, 4), &[0, 0, 255, 255], "filled part is red");
        assert_eq!(pixel(&buf, 35, 4), &[255, 0, 0, 255], "track part is blue");
    }

    /// Rounded background: corner pixel stays clear, center is filled.
    #[test]
    fn rounded_bg_clips_corners() {
        let mut r = Renderer::new();
        let size = 16u32;
        let mut buf = vec![0u8; (size * size * 4) as usize];
        let root = Element::HBox(Flex {
            style: Style {
                bg: Some(Rgba::new(255, 255, 255, 255)),
                border_radius: 6.0,
                ..Style::default()
            },
            ..Flex::default()
        });
        render_tree(&mut r, &mut buf, size, size, 1.0, &root);
        let px = |x: u32, y: u32| {
            let i = ((y * size + x) * 4) as usize;
            &buf[i..i + 4]
        };
        assert_eq!(px(0, 0), &[0, 0, 0, 0], "corner outside the radius");
        assert_eq!(px(8, 8), &[255, 255, 255, 255], "center filled");
    }

    /// Circular progress touches pixels on the ring but not the center.
    #[test]
    fn circular_progress_draws_ring() {
        let mut r = Renderer::new();
        let size = 32u32;
        let mut buf = vec![0u8; (size * size * 4) as usize];
        let root = Element::CircularProgress(CircularProgress {
            style: Style {
                width: Some(size as f32),
                height: Some(size as f32),
                ..Style::default()
            },
            value: 0.75,
            size: size as f32,
            thickness: 3.0,
            ..CircularProgress::default()
        });
        render_tree(&mut r, &mut buf, size, size, 1.0, &root);
        let px = |x: u32, y: u32| {
            let i = ((y * size + x) * 4) as usize;
            &buf[i..i + 4]
        };
        assert_ne!(px(16, 2), &[0, 0, 0, 0], "12 o'clock on the ring");
        assert_eq!(px(16, 16), &[0, 0, 0, 0], "center stays empty");
    }

    /// A bar-like tree (bg + padding + text) draws without panicking on
    /// fontless systems and fills its background.
    #[test]
    fn bar_tree_smoke() {
        let mut r = Renderer::new();
        let mut buf = vec![0u8; (W * H * 4) as usize];
        let root = Element::HBox(Flex {
            style: Style {
                bg: Some(Rgba::new(20, 20, 30, 255)),
                ..Style::default()
            },
            padding: Edges::all(2.0),
            align: Align::Center,
            children: vec![
                Element::Text(Text {
                    content: "hi".into(),
                    size: 4.0,
                    ..Text::default()
                }),
                Element::Spacer(Spacer::default()),
                Element::Separator(Separator::default()),
            ],
            ..Flex::default()
        });
        render_tree(&mut r, &mut buf, W, H, 1.0, &root);
        assert_eq!(pixel(&buf, 0, 0), &[30, 20, 20, 255], "bg fills the rect");
    }
}
