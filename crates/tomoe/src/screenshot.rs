//! Native screenshots: render the current scene offscreen, then encode a PNG
//! and copy it to the clipboard via `wl-copy` off the main thread. Nothing is
//! written to disk.

use std::io::Write;
use std::process::{Command, Stdio};

use anyhow::{Context, Result};
use smithay::output::Output;
use smithay::utils::{Physical, Rectangle, Size};
use tracing::{info, warn};

use crate::state::Tomoe;

/// Capture `output` (cropped to `region` in output-local physical coordinates
/// when given, the whole output otherwise), then encode and copy the PNG to
/// the clipboard on a detached thread so the compositor never blocks.
pub fn screenshot(
    tomoe: &mut Tomoe,
    output: &Output,
    region: Option<Rectangle<i32, Physical>>,
) -> Result<()> {
    let (size, pixels) = crate::capture::capture_rgba(tomoe, output, region)
        .context("error capturing screenshot pixels")?;

    std::thread::spawn(move || match encode_png(size, &pixels) {
        Ok(png) => copy_to_clipboard(&png),
        Err(err) => warn!("error encoding screenshot: {err:#}"),
    });

    Ok(())
}

/// Encode tightly packed RGBA8 `pixels` as an in-memory PNG.
fn encode_png(size: Size<i32, Physical>, pixels: &[u8]) -> Result<Vec<u8>> {
    let mut png = Vec::new();
    let mut encoder = png::Encoder::new(&mut png, size.w as u32, size.h as u32);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().context("error writing PNG header")?;
    writer
        .write_image_data(pixels)
        .context("error writing PNG data")?;
    writer.finish().context("error finishing PNG")?;
    Ok(png)
}

/// Pipe the PNG into `wl-copy`. Runs on the detached encode thread, so
/// waiting on the child is fine — and required, both to reap it (no zombies)
/// and to surface failures instead of logging success unconditionally.
fn copy_to_clipboard(png: &[u8]) {
    let mut child = match Command::new("wl-copy")
        .args(["-t", "image/png"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(err) => {
            warn!("wl-copy unavailable, screenshot not copied to clipboard: {err}");
            return;
        }
    };
    if let Some(mut stdin) = child.stdin.take() {
        if let Err(err) = stdin.write_all(png) {
            warn!("error writing screenshot to wl-copy: {err}");
        }
        // stdin drops here, closing the pipe so wl-copy can finish
    }
    match child.wait_with_output() {
        Ok(out) if out.status.success() => info!("screenshot copied to clipboard"),
        Ok(out) => warn!(
            "wl-copy failed ({}), screenshot not copied to clipboard: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        ),
        Err(err) => warn!("error waiting for wl-copy: {err}"),
    }
}
