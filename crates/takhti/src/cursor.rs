//! XCursor theme loading and cursor render elements.

use std::io::Read;

use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::element::memory::{
    MemoryRenderBuffer, MemoryRenderBufferRenderElement,
};
use smithay::backend::renderer::element::Kind;
use smithay::utils::{Logical, Physical, Point, Transform};
use tracing::warn;
use xcursor::parser::{parse_xcursor, Image};
use xcursor::CursorTheme;

pub struct Cursor {
    /// Buffer + hotspot; None if no theme could be loaded (block fallback used).
    frame: Option<(MemoryRenderBuffer, Point<i32, Logical>)>,
}

impl Cursor {
    pub fn load() -> Self {
        let theme = std::env::var("XCURSOR_THEME").unwrap_or_else(|_| "default".to_string());
        let size: u32 = std::env::var("XCURSOR_SIZE")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(24);

        let frame = load_image(&theme, size).map(|image| {
            let buffer = MemoryRenderBuffer::from_slice(
                &image.pixels_rgba,
                Fourcc::Abgr8888,
                (image.width as i32, image.height as i32),
                1,
                Transform::Normal,
                None,
            );
            (buffer, Point::from((image.xhot as i32, image.yhot as i32)))
        });
        if frame.is_none() {
            warn!("no xcursor theme found; using block cursor");
        }
        Self { frame }
    }

    pub fn element<R: crate::render::TakhtiRenderer>(
        &self,
        renderer: &mut R,
        pos: Point<f64, Physical>,
    ) -> Option<MemoryRenderBufferRenderElement<R>> {
        let (buffer, hotspot) = self.frame.as_ref()?;
        // xcursor images are raw pixels; the hotspot is physical. Snap the
        // final position to the grid so the cursor image stays crisp.
        let location = (pos - hotspot.to_f64().to_physical(1.0))
            .to_i32_round::<i32>()
            .to_f64();
        MemoryRenderBufferRenderElement::from_buffer(
            renderer,
            location,
            buffer,
            None,
            None,
            None,
            Kind::Cursor,
        )
        .ok()
    }
}

fn load_image(theme: &str, size: u32) -> Option<Image> {
    let theme = CursorTheme::load(theme);
    let path = theme
        .load_icon("default")
        .or_else(|| theme.load_icon("left_ptr"))?;
    let mut data = Vec::new();
    std::fs::File::open(path)
        .ok()?
        .read_to_end(&mut data)
        .ok()?;
    let images = parse_xcursor(&data)?;
    // Pick the size closest to the requested one.
    images
        .into_iter()
        .min_by_key(|image| (image.size as i32 - size as i32).abs())
}
