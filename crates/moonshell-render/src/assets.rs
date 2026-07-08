//! Decoded/rasterized asset cache: SVG icons (resvg) and raster
//! images (png/jpeg via `image`). Decoding and rasterization happen
//! once per (source, size, tint) key — never per paint (standing
//! lesson: never regenerate buffers per frame). Redraws blit cached
//! pixmaps.
//!
//! Cached pixmaps are premultiplied and already swizzled into the
//! wl_shm byte order [B, G, R, A], so blitting them over the canvas is
//! a channel-agnostic src-over with no conversion pass.
//!
//! Failures are cached too (`None`): a missing icon must not re-walk
//! the XDG dirs on every redraw. The maps are unbounded but bar-scale
//! — a config references a handful of assets at one or two sizes.
//! Revisit when the M3 tray (many app icons) lands; cache size is part
//! of the memory budget (PLAN.md).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use tiny_skia::{FilterQuality, IntSize, Pixmap, PixmapPaint, Transform};

use crate::Rgba;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct IconKey {
    /// Explicit file path or theme name.
    source: String,
    /// Physical square edge (px).
    px: u32,
    tint: Option<Rgba>,
}

#[derive(Default)]
pub(crate) struct AssetCache {
    icons: HashMap<IconKey, Option<Rc<Pixmap>>>,
    images: HashMap<(PathBuf, u32, u32), Option<Rc<Pixmap>>>,
    /// Native pixel dimensions, read from the file header only —
    /// cheap enough for the layout measure pass.
    image_dims: HashMap<PathBuf, Option<(u32, u32)>>,
}

impl AssetCache {
    /// Native (width, height) of a raster image, for intrinsic sizing.
    pub(crate) fn image_size(&mut self, src: &Path) -> Option<(u32, u32)> {
        if let Some(dims) = self.image_dims.get(src) {
            return *dims;
        }
        let dims = image::ImageReader::open(src)
            .ok()
            .and_then(|r| r.into_dimensions().ok());
        if dims.is_none() {
            tracing::warn!(src = %src.display(), "image not readable");
        }
        self.image_dims.insert(src.to_path_buf(), dims);
        dims
    }

    /// Raster image decoded and scaled to exactly `w` x `h` physical px.
    pub(crate) fn image(&mut self, src: &Path, w: u32, h: u32) -> Option<Rc<Pixmap>> {
        let key = (src.to_path_buf(), w, h);
        if let Some(pm) = self.images.get(&key) {
            return pm.clone();
        }
        let pm = decode_scaled(src, w, h).map(Rc::new);
        self.images.insert(key, pm.clone());
        pm
    }

    /// SVG icon rasterized at `px` x `px`, optionally tinted. `path`
    /// wins over theme lookup by `name` (the `ui.icon` contract).
    pub(crate) fn icon(
        &mut self,
        name: &str,
        path: Option<&Path>,
        px: u32,
        tint: Option<Rgba>,
    ) -> Option<Rc<Pixmap>> {
        let source = match path {
            Some(p) => p.display().to_string(),
            None => name.to_string(),
        };
        let key = IconKey { source, px, tint };
        if let Some(pm) = self.icons.get(&key) {
            return pm.clone();
        }
        let data = match path {
            Some(p) => std::fs::read(p).ok(),
            None => find_system_icon_svg(name),
        };
        let pm = data.and_then(|d| rasterize_svg(&d, px, tint)).map(Rc::new);
        if pm.is_none() {
            tracing::warn!(name, ?path, "icon not found or not renderable");
        }
        self.icons.insert(key, pm.clone());
        pm
    }
}

/// Decode → premultiply → swizzle to buffer byte order → scale.
fn decode_scaled(src: &Path, w: u32, h: u32) -> Option<Pixmap> {
    if w == 0 || h == 0 {
        return None;
    }
    let img = image::open(src).ok()?.into_rgba8();
    let (iw, ih) = img.dimensions();
    let mut data = img.into_raw();
    // Straight RGBA → premultiplied [B, G, R, A].
    for px in data.chunks_exact_mut(4) {
        let a = px[3] as u32;
        let r = ((px[0] as u32 * a + 127) / 255) as u8;
        px[1] = ((px[1] as u32 * a + 127) / 255) as u8;
        px[0] = ((px[2] as u32 * a + 127) / 255) as u8;
        px[2] = r;
    }
    let native = Pixmap::from_vec(data, IntSize::from_wh(iw, ih)?)?;
    if (iw, ih) == (w, h) {
        return Some(native);
    }
    let mut scaled = Pixmap::new(w, h)?;
    let paint = PixmapPaint {
        quality: FilterQuality::Bilinear,
        ..PixmapPaint::default()
    };
    scaled.draw_pixmap(
        0,
        0,
        native.as_ref(),
        &paint,
        Transform::from_scale(w as f32 / iw as f32, h as f32 / ih as f32),
        None,
    );
    Some(scaled)
}

/// Rasterize SVG data into a `px` x `px` pixmap in buffer byte order.
fn rasterize_svg(data: &[u8], px: u32, tint: Option<Rgba>) -> Option<Pixmap> {
    if px == 0 {
        return None;
    }
    let tree = resvg::usvg::Tree::from_data(data, &resvg::usvg::Options::default()).ok()?;
    let size = tree.size();
    if size.width() <= 0.0 || size.height() <= 0.0 {
        return None;
    }
    let mut pixmap = Pixmap::new(px, px)?;
    resvg::render(
        &tree,
        Transform::from_scale(px as f32 / size.width(), px as f32 / size.height()),
        &mut pixmap.as_mut(),
    );
    let data = pixmap.data_mut();
    match tint {
        // Monochrome tint: keep the icon's alpha (scaled by the tint's),
        // replace its color — premultiplied, buffer byte order.
        Some(t) => {
            for px in data.chunks_exact_mut(4) {
                let a = px[3] as u32 * t.a as u32 / 255;
                px[0] = ((t.b as u32 * a + 127) / 255) as u8;
                px[1] = ((t.g as u32 * a + 127) / 255) as u8;
                px[2] = ((t.r as u32 * a + 127) / 255) as u8;
                px[3] = a as u8;
            }
        }
        // resvg writes RGBA byte order; swizzle once into [B, G, R, A].
        None => {
            for px in data.chunks_exact_mut(4) {
                px.swap(0, 2);
            }
        }
    }
    Some(pixmap)
}

/// Search XDG icon theme dirs for `{name}.svg` (ported verbatim from
/// nur's asset source — the inherited lookup contract).
fn find_system_icon_svg(name: &str) -> Option<Vec<u8>> {
    let data_dirs =
        std::env::var("XDG_DATA_DIRS").unwrap_or_else(|_| "/usr/share:/usr/local/share".into());

    let search_dirs: Vec<String> = data_dirs
        .split(':')
        .flat_map(|base| {
            ["hicolor", "Adwaita", "breeze"]
                .iter()
                .map(move |theme| format!("{base}/icons/{theme}/scalable"))
        })
        .collect();

    let categories = [
        "actions",
        "apps",
        "categories",
        "devices",
        "emblems",
        "mimetypes",
        "places",
        "status",
        "panel",
    ];

    for dir in &search_dirs {
        for cat in &categories {
            if let Ok(data) = std::fs::read(format!("{dir}/{cat}/{name}.svg")) {
                return Some(data);
            }
        }
        if let Ok(data) = std::fs::read(format!("{dir}/{name}.svg")) {
            return Some(data);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("moonshell-assets-{}-{name}", std::process::id()))
    }

    fn write_png(path: &Path, w: u32, h: u32, rgba: [u8; 4]) {
        let img = image::RgbaImage::from_pixel(w, h, image::Rgba(rgba));
        img.save(path).unwrap();
    }

    /// PNG decode premultiplies and lands in [B, G, R, A] byte order.
    #[test]
    fn png_decode_swizzles_and_premultiplies() {
        let path = tmp("red.png");
        write_png(&path, 2, 2, [255, 0, 0, 128]);
        let mut cache = AssetCache::default();
        assert_eq!(cache.image_size(&path), Some((2, 2)));
        let pm = cache.image(&path, 2, 2).unwrap();
        // r premultiplied: (255*128+127)/255 = 128, in the R slot (idx 2).
        assert_eq!(&pm.data()[0..4], &[0, 0, 128, 128]);
        std::fs::remove_file(&path).ok();
    }

    /// Requesting a different size rescales; cache returns the same Rc.
    #[test]
    fn image_scales_and_caches() {
        let path = tmp("scale.png");
        write_png(&path, 2, 2, [0, 255, 0, 255]);
        let mut cache = AssetCache::default();
        let pm = cache.image(&path, 4, 4).unwrap();
        assert_eq!((pm.width(), pm.height()), (4, 4));
        let again = cache.image(&path, 4, 4).unwrap();
        assert!(Rc::ptr_eq(&pm, &again), "second lookup hits the cache");
        std::fs::remove_file(&path).ok();
    }

    /// SVG rasterizes at the requested size; tint replaces color but
    /// keeps alpha.
    #[test]
    fn svg_rasterizes_and_tints() {
        let path = tmp("icon.svg");
        std::fs::write(
            &path,
            r##"<svg xmlns="http://www.w3.org/2000/svg" width="8" height="8">
                 <rect width="8" height="8" fill="#00ff00"/></svg>"##,
        )
        .unwrap();
        let mut cache = AssetCache::default();
        let plain = cache.icon("x", Some(&path), 16, None).unwrap();
        assert_eq!((plain.width(), plain.height()), (16, 16));
        // Green in buffer order [B, G, R, A].
        assert_eq!(&plain.data()[0..4], &[0, 255, 0, 255]);
        let tinted = cache
            .icon("x", Some(&path), 16, Some(Rgba::new(255, 0, 0, 255)))
            .unwrap();
        assert_eq!(&tinted.data()[0..4], &[0, 0, 255, 255], "tinted red");
        std::fs::remove_file(&path).ok();
    }

    /// Misses are negative-cached: no repeated filesystem walks.
    #[test]
    fn missing_icon_is_negative_cached() {
        let mut cache = AssetCache::default();
        assert!(cache
            .icon("moonshell-definitely-missing", None, 16, None)
            .is_none());
        let key = IconKey {
            source: "moonshell-definitely-missing".into(),
            px: 16,
            tint: None,
        };
        assert!(cache.icons.contains_key(&key), "miss recorded");
    }
}
