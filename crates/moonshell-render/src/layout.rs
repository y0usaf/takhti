//! Flex-lite layout: element tree → tree of positioned rects in
//! physical (buffer) pixels. Two passes:
//!
//! 1. **measure** — bottom-up intrinsic size (text is shaped here, the
//!    only expensive leaf), overridden by `Style::width`/`height`;
//! 2. **place** — top-down: containers distribute leftover main-axis
//!    space to `grow` children, `justify` when nothing grows, `align`
//!    on the cross axis.
//!
//! Element props are logical px; the scale multiply happens exactly
//! once, in here. Rects stay `f32`; the draw pass rounds edges to
//! integers (rounding x0/x1 rather than x/w so adjacent rects never
//! open seams).

use crate::element::{Element, Flex, Orientation};
use crate::Renderer;

/// Physical-pixel rect.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

/// Positioned node; children mirror the element tree's structure
/// exactly, so draw walks both in lockstep.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LayoutNode {
    pub rect: Rect,
    pub children: Vec<LayoutNode>,
}

/// Physical line height for a text element: unset means `size * 1.3`,
/// ceiled after scaling so glyph rows land on whole pixels.
pub(crate) fn text_line_px(t: &crate::element::Text, scale: f32) -> f32 {
    (t.line_height.unwrap_or(t.size * 1.3) * scale).ceil()
}

/// Lay out `root` into a `width` x `height` physical-pixel canvas.
/// `renderer` is needed to shape text for measurement.
pub fn compute(
    root: &Element,
    width: f32,
    height: f32,
    scale: f32,
    renderer: &mut Renderer,
) -> LayoutNode {
    let rect = Rect {
        x: 0.0,
        y: 0.0,
        w: width,
        h: height,
    };
    place(root, rect, scale, renderer)
}

/// Intrinsic physical size of an element (before grow/stretch).
fn measure(el: &Element, scale: f32, r: &mut Renderer) -> (f32, f32) {
    let style = el.style();
    let mut intrinsic = || -> (f32, f32) {
        match el {
            Element::HBox(f) => measure_flex(f, Axis::X, scale, r),
            Element::VBox(f) => measure_flex(f, Axis::Y, scale, r),
            Element::Stack(s) => s
                .children
                .iter()
                .map(|c| measure(c, scale, r))
                .fold((0.0_f32, 0.0_f32), |(aw, ah), (w, h)| {
                    (aw.max(w), ah.max(h))
                }),
            Element::Text(t) => {
                let line = text_line_px(t, scale);
                let advance = r.measure_text(&t.content, t.size * scale, line);
                (advance.ceil(), line)
            }
            Element::Spacer(_) => (0.0, 0.0),
            Element::Separator(sep) => match sep.orientation {
                Orientation::Vertical => (sep.thickness * scale, 0.0),
                Orientation::Horizontal => (0.0, sep.thickness * scale),
            },
            Element::Progress(_) => (0.0, 0.0),
            Element::CircularProgress(c) => (c.size * scale, c.size * scale),
            Element::Icon(i) => {
                let px = (i.size * scale).ceil();
                (px, px)
            }
            // Native file pixels map 1:1 to buffer pixels (crisp by
            // default); style overrides rescale.
            Element::Image(img) => match r.assets.image_size(&img.src) {
                Some((w, h)) => (w as f32, h as f32),
                None => (0.0, 0.0),
            },
        }
    };
    // Overrides replace the intrinsic axis; the other axis still needs
    // measuring, but only when it isn't overridden too.
    match (style.width, style.height) {
        (Some(w), Some(h)) => (w * scale, h * scale),
        (Some(w), None) => (w * scale, intrinsic().1),
        (None, Some(h)) => (intrinsic().0, h * scale),
        (None, None) => intrinsic(),
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Axis {
    X,
    Y,
}

fn measure_flex(f: &Flex, axis: Axis, scale: f32, r: &mut Renderer) -> (f32, f32) {
    let (pad_main, pad_cross) = padding_sums(f, axis, scale);
    let gaps = f.gap * scale * f.children.len().saturating_sub(1) as f32;
    let (main, cross) = f
        .children
        .iter()
        .map(|c| split(measure(c, scale, r), axis))
        .fold((0.0_f32, 0.0_f32), |(am, ac), (m, c)| (am + m, ac.max(c)));
    join(main + gaps + pad_main, cross + pad_cross, axis)
}

/// (main, cross) padding totals for the given axis.
fn padding_sums(f: &Flex, axis: Axis, scale: f32) -> (f32, f32) {
    let h = (f.padding.left + f.padding.right) * scale;
    let v = (f.padding.top + f.padding.bottom) * scale;
    match axis {
        Axis::X => (h, v),
        Axis::Y => (v, h),
    }
}

fn split((w, h): (f32, f32), axis: Axis) -> (f32, f32) {
    match axis {
        Axis::X => (w, h),
        Axis::Y => (h, w),
    }
}

fn join(main: f32, cross: f32, axis: Axis) -> (f32, f32) {
    match axis {
        Axis::X => (main, cross),
        Axis::Y => (cross, main),
    }
}

fn place(el: &Element, rect: Rect, scale: f32, r: &mut Renderer) -> LayoutNode {
    match el {
        Element::HBox(f) => place_flex(f, Axis::X, rect, scale, r),
        Element::VBox(f) => place_flex(f, Axis::Y, rect, scale, r),
        Element::Stack(s) => LayoutNode {
            rect,
            children: s
                .children
                .iter()
                .map(|c| place(c, rect, scale, r))
                .collect(),
        },
        _ => LayoutNode {
            rect,
            children: Vec::new(),
        },
    }
}

fn place_flex(f: &Flex, axis: Axis, rect: Rect, scale: f32, r: &mut Renderer) -> LayoutNode {
    // Inner content box, padding removed.
    let pl = f.padding.left * scale;
    let pt = f.padding.top * scale;
    let inner = Rect {
        x: rect.x + pl,
        y: rect.y + pt,
        w: (rect.w - pl - f.padding.right * scale).max(0.0),
        h: (rect.h - pt - f.padding.bottom * scale).max(0.0),
    };
    let (inner_main, inner_cross) = split((inner.w, inner.h), axis);
    let (inner_main_start, inner_cross_start) = split((inner.x, inner.y), axis);

    let gap = f.gap * scale;
    let gaps = gap * f.children.len().saturating_sub(1) as f32;

    // Flex-grow semantics: every child starts at its measured base size;
    // grow children add a weighted share of the leftover on top.
    let measured: Vec<(f32, f32)> = f
        .children
        .iter()
        .map(|c| split(measure(c, scale, r), axis))
        .collect();
    let grow_sum: f32 = f.children.iter().map(|c| c.style().grow).sum();
    let base: f32 = measured.iter().map(|(m, _)| *m).sum();
    let leftover = (inner_main - base - gaps).max(0.0);

    let offset = if grow_sum > 0.0 {
        0.0
    } else {
        match f.justify {
            crate::element::Justify::Start => 0.0,
            crate::element::Justify::Center => leftover / 2.0,
            crate::element::Justify::End => leftover,
        }
    };

    let mut cursor = inner_main_start + offset;
    let mut children = Vec::with_capacity(f.children.len());
    for (child, &(m, c)) in f.children.iter().zip(&measured) {
        let grow = child.style().grow;
        let main = if grow > 0.0 {
            m + leftover * grow / grow_sum
        } else {
            m
        };
        // Separators stretch across the container; everything else keeps
        // its measured cross size, clamped to the content box.
        let cross = match child {
            Element::Separator(_) => inner_cross,
            _ => c.min(inner_cross),
        };
        let cross_pos = inner_cross_start
            + match f.align {
                crate::element::Align::Start => 0.0,
                crate::element::Align::Center => (inner_cross - cross) / 2.0,
                crate::element::Align::End => inner_cross - cross,
            };
        let (x, y) = join(cursor, cross_pos, axis);
        let (w, h) = join(main, cross, axis);
        children.push(place(child, Rect { x, y, w, h }, scale, r));
        cursor += main + gap;
    }

    LayoutNode { rect, children }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::element::*;

    fn fixed_box(w: f32, h: f32) -> Element {
        Element::VBox(Flex {
            style: Style {
                width: Some(w),
                height: Some(h),
                ..Style::default()
            },
            ..Flex::default()
        })
    }

    #[test]
    fn hbox_grow_gap_padding() {
        let root = Element::HBox(Flex {
            padding: Edges::all(5.0),
            gap: 10.0,
            children: vec![
                fixed_box(20.0, 10.0),
                Element::Spacer(Spacer::default()),
                fixed_box(30.0, 10.0),
            ],
            ..Flex::default()
        });
        let mut r = Renderer::new();
        let node = compute(&root, 100.0, 30.0, 1.0, &mut r);
        // inner = 90 wide at x=5; fixed 20+30, two gaps of 10 → spacer = 20.
        assert_eq!(
            node.children[0].rect,
            Rect {
                x: 5.0,
                y: 5.0,
                w: 20.0,
                h: 10.0
            }
        );
        assert_eq!(node.children[1].rect.x, 35.0);
        assert_eq!(node.children[1].rect.w, 20.0);
        assert_eq!(
            node.children[2].rect,
            Rect {
                x: 65.0,
                y: 5.0,
                w: 30.0,
                h: 10.0
            }
        );
    }

    #[test]
    fn justify_center_without_grow() {
        let root = Element::HBox(Flex {
            justify: Justify::Center,
            align: Align::Center,
            children: vec![fixed_box(20.0, 10.0)],
            ..Flex::default()
        });
        let mut r = Renderer::new();
        let node = compute(&root, 100.0, 30.0, 1.0, &mut r);
        assert_eq!(
            node.children[0].rect,
            Rect {
                x: 40.0,
                y: 10.0,
                w: 20.0,
                h: 10.0
            }
        );
    }

    #[test]
    fn vbox_stacks_on_y() {
        let root = Element::VBox(Flex {
            gap: 4.0,
            children: vec![fixed_box(10.0, 10.0), fixed_box(10.0, 20.0)],
            ..Flex::default()
        });
        let mut r = Renderer::new();
        let node = compute(&root, 50.0, 50.0, 1.0, &mut r);
        assert_eq!(node.children[0].rect.y, 0.0);
        assert_eq!(node.children[1].rect.y, 14.0);
        assert_eq!(node.children[1].rect.h, 20.0);
    }

    #[test]
    fn separator_stretches_cross_axis() {
        let root = Element::HBox(Flex {
            padding: Edges::all(2.0),
            children: vec![Element::Separator(Separator {
                thickness: 2.0,
                ..Separator::default()
            })],
            ..Flex::default()
        });
        let mut r = Renderer::new();
        let node = compute(&root, 100.0, 30.0, 1.0, &mut r);
        assert_eq!(
            node.children[0].rect,
            Rect {
                x: 2.0,
                y: 2.0,
                w: 2.0,
                h: 26.0
            }
        );
    }

    #[test]
    fn scale_multiplies_logical_props_once() {
        let root = Element::HBox(Flex {
            padding: Edges::all(4.0),
            children: vec![fixed_box(10.0, 10.0)],
            ..Flex::default()
        });
        let mut r = Renderer::new();
        let node = compute(&root, 200.0, 60.0, 2.0, &mut r);
        assert_eq!(
            node.children[0].rect,
            Rect {
                x: 8.0,
                y: 8.0,
                w: 20.0,
                h: 20.0
            }
        );
    }

    #[test]
    fn stack_gives_children_full_rect() {
        let root = Element::Stack(Stack {
            children: vec![fixed_box(10.0, 10.0), Element::Spacer(Spacer::default())],
            ..Stack::default()
        });
        let mut r = Renderer::new();
        let node = compute(&root, 40.0, 20.0, 1.0, &mut r);
        for child in &node.children {
            assert_eq!(
                child.rect,
                Rect {
                    x: 0.0,
                    y: 0.0,
                    w: 40.0,
                    h: 20.0
                }
            );
        }
    }
}
