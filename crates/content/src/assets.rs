//! Shrinking large guide images on upload.

use std::io::Cursor;
use std::path::Path;

use image::codecs::jpeg::JpegEncoder;
use image::codecs::png::{CompressionType, FilterType, PngEncoder};
use image::codecs::webp::WebPEncoder;
use image::imageops::FilterType as ResizeFilter;
use image::{DynamicImage, ExtendedColorType, GenericImageView, ImageEncoder};

/// Longest edge of an optimized image.
const MAX_DIMENSION: u32 = 1600;
const JPEG_QUALITY: u8 = 85;

/// The re-encoded image and the file name to save it under.
#[derive(Debug, Clone)]
pub struct OptimizedImage {
    pub bytes: Vec<u8>,
    pub filename: String,
}

fn extension(filename: &str) -> Option<String> {
    Path::new(filename)
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
}

fn replace_extension(filename: &str, new_ext: &str) -> String {
    Path::new(filename)
        .file_stem()
        .map(|stem| format!("{}.{new_ext}", stem.to_string_lossy()))
        .unwrap_or_else(|| format!("{filename}.{new_ext}"))
}

fn resize_if_needed(img: DynamicImage) -> DynamicImage {
    let (w, h) = img.dimensions();
    let longest = w.max(h);
    if longest <= MAX_DIMENSION {
        return img;
    }
    let scale = MAX_DIMENSION as f32 / longest as f32;
    let nw = ((w as f32 * scale).round() as u32).max(1);
    let nh = ((h as f32 * scale).round() as u32).max(1);
    img.resize(nw, nh, ResizeFilter::Lanczos3)
}

fn encode_jpeg(img: &DynamicImage, out: &mut Vec<u8>) -> Result<(), String> {
    // JPEG has no alpha channel: encode RGB (an RGBA image would be rejected by the encoder).
    let rgb = img.to_rgb8();
    JpegEncoder::new_with_quality(out, JPEG_QUALITY)
        .encode_image(&rgb)
        .map_err(|e| format!("encode jpeg: {e}"))
}

/// Resize (longest edge 1600 px) and re-encode a raster guide image:
///
/// - JPEG is re-encoded at quality 85;
/// - an opaque PNG becomes a JPEG and the file name changes to `.jpg`;
/// - a PNG with transparency is recompressed;
/// - WebP is re-encoded lossless.
///
/// SVG and GIF are not touched (`Err("not optimized")`); other failures (undecodable data...)
/// are `Err(reason)`, and the caller keeps the original bytes.
pub fn optimize_guide_image(filename: &str, data: &[u8]) -> Result<OptimizedImage, String> {
    let ext = extension(filename).ok_or_else(|| "missing extension".to_string())?;
    match ext.as_str() {
        "svg" | "gif" => return Err("not optimized".to_string()),
        "png" | "jpg" | "jpeg" | "webp" => {}
        _ => return Err("unsupported".to_string()),
    }

    let img = image::load_from_memory(data).map_err(|e| format!("decode: {e}"))?;
    let img = resize_if_needed(img);
    let mut out = Vec::new();
    let mut save_filename = filename.to_string();

    match ext.as_str() {
        "png" => {
            let rgba = img.to_rgba8();
            if rgba.pixels().any(|p| p[3] < 255) {
                let (w, h) = rgba.dimensions();
                PngEncoder::new_with_quality(
                    Cursor::new(&mut out),
                    CompressionType::Best,
                    FilterType::Adaptive,
                )
                .write_image(rgba.as_raw(), w, h, ExtendedColorType::Rgba8)
                .map_err(|e| format!("encode png: {e}"))?;
            } else {
                encode_jpeg(&img, &mut out)?;
                save_filename = replace_extension(filename, "jpg");
            }
        }
        "webp" => {
            let rgba = img.to_rgba8();
            let (w, h) = rgba.dimensions();
            WebPEncoder::new_lossless(Cursor::new(&mut out))
                .encode(rgba.as_raw(), w, h, ExtendedColorType::Rgba8)
                .map_err(|e| format!("encode webp: {e}"))?;
        }
        _ => encode_jpeg(&img, &mut out)?,
    }

    Ok(OptimizedImage {
        bytes: out,
        filename: save_filename,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{ImageFormat, Rgba, RgbaImage};

    fn png(w: u32, h: u32, alpha: u8) -> Vec<u8> {
        let img = RgbaImage::from_fn(w, h, |x, y| {
            Rgba([(x % 256) as u8, (y % 256) as u8, 7, alpha])
        });
        let mut out = Cursor::new(Vec::new());
        DynamicImage::ImageRgba8(img)
            .write_to(&mut out, ImageFormat::Png)
            .unwrap();
        out.into_inner()
    }

    #[test]
    fn opaque_rgba_png_becomes_resized_jpeg() {
        let optimized = optimize_guide_image("shot.PNG", &png(2000, 1000, 255)).unwrap();
        assert_eq!(optimized.filename, "shot.jpg");
        let img = image::load_from_memory(&optimized.bytes).unwrap();
        assert_eq!(
            image::guess_format(&optimized.bytes).unwrap(),
            ImageFormat::Jpeg
        );
        assert_eq!(img.dimensions(), (1600, 800));
    }

    #[test]
    fn transparent_png_stays_png() {
        let optimized = optimize_guide_image("t.png", &png(10, 10, 100)).unwrap();
        assert_eq!(optimized.filename, "t.png");
        assert_eq!(
            image::guess_format(&optimized.bytes).unwrap(),
            ImageFormat::Png
        );
    }

    #[test]
    fn svg_gif_and_garbage_are_left_alone() {
        assert_eq!(
            optimize_guide_image("a.svg", b"<svg/>").unwrap_err(),
            "not optimized"
        );
        assert_eq!(
            optimize_guide_image("a.gif", b"GIF").unwrap_err(),
            "not optimized"
        );
        assert!(
            optimize_guide_image("a.png", b"nope")
                .unwrap_err()
                .starts_with("decode")
        );
    }
}
