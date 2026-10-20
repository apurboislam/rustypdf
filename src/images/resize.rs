//! `max_px` downscaling: large photos are shrunk once, before caching, which
//! keeps PDFs small and renders fast.

use std::io::Cursor;

use image::codecs::jpeg::JpegEncoder;
use image::{DynamicImage, ImageDecoder, ImageReader, Limits};
use typst::foundations::Bytes;

use super::assets::{Image, ImageFormat};

/// Largest width or height accepted for decoding (decompression-bomb guard).
const MAX_DIMENSION: u32 = 20_000;
/// Largest decode buffer.
const MAX_ALLOC: u64 = 512 * 1024 * 1024;
const JPEG_QUALITY: u8 = 85;

/// Downscales `image` so its longest side is at most `max_px`, keeping the
/// aspect ratio. Smaller images and SVGs are returned unchanged. JPEGs stay
/// JPEG (with EXIF orientation applied); other formats become PNG.
pub fn downscale(image: Image, max_px: u32) -> Result<Image, String> {
    if image.format == ImageFormat::Svg {
        return Ok(image);
    }
    let format = match image.format {
        ImageFormat::Png => image::ImageFormat::Png,
        ImageFormat::Jpeg => image::ImageFormat::Jpeg,
        ImageFormat::Gif => image::ImageFormat::Gif,
        ImageFormat::Webp => image::ImageFormat::WebP,
        ImageFormat::Svg => unreachable!(),
    };
    let bytes = image.bytes.clone();
    let mut reader = ImageReader::with_format(Cursor::new(bytes.as_slice()), format);
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_DIMENSION);
    limits.max_image_height = Some(MAX_DIMENSION);
    limits.max_alloc = Some(MAX_ALLOC);
    reader.limits(limits);
    let mut decoder = reader
        .into_decoder()
        .map_err(|e| format!("cannot decode image: {e}"))?;
    let (w, h) = decoder.dimensions();
    if w.max(h) <= max_px {
        return Ok(image);
    }
    let orientation = decoder.orientation().ok();
    let mut decoded =
        DynamicImage::from_decoder(decoder).map_err(|e| format!("cannot decode image: {e}"))?;
    if let Some(orientation) = orientation {
        decoded.apply_orientation(orientation);
    }
    // Area averaging, made for downscaling: about 2x faster than Lanczos3
    // with the same result to within a fraction of a level per channel.
    let resized = decoded.thumbnail(max_px, max_px);

    let mut out = Vec::new();
    if image.format == ImageFormat::Jpeg {
        JpegEncoder::new_with_quality(&mut out, JPEG_QUALITY)
            .encode_image(&resized.to_rgb8())
            .map_err(|e| format!("cannot encode image: {e}"))?;
    } else {
        resized
            .write_to(&mut Cursor::new(&mut out), image::ImageFormat::Png)
            .map_err(|e| format!("cannot encode image: {e}"))?;
    }
    Image::new(Bytes::new(out)).ok_or_else(|| "re-encoded image is invalid".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{GenericImageView, RgbaImage};

    fn png(w: u32, h: u32) -> Image {
        let mut out = Vec::new();
        DynamicImage::ImageRgba8(RgbaImage::new(w, h))
            .write_to(&mut Cursor::new(&mut out), image::ImageFormat::Png)
            .unwrap();
        Image::new(Bytes::new(out)).unwrap()
    }

    #[test]
    fn shrinks_longest_side() {
        let out = downscale(png(800, 400), 200).unwrap();
        assert_eq!(out.format, ImageFormat::Png);
        let img = image::load_from_memory(&out.bytes).unwrap();
        assert_eq!(img.dimensions(), (200, 100));
    }

    #[test]
    fn small_images_are_unchanged() {
        let input = png(50, 80);
        let out = downscale(input.clone(), 200).unwrap();
        assert_eq!(out.sha256, input.sha256);
    }

    #[test]
    fn jpeg_stays_jpeg() {
        let mut jpg = Vec::new();
        DynamicImage::new_rgb8(300, 600)
            .write_to(&mut Cursor::new(&mut jpg), image::ImageFormat::Jpeg)
            .unwrap();
        let out = downscale(Image::new(Bytes::new(jpg)).unwrap(), 100).unwrap();
        assert_eq!(out.format, ImageFormat::Jpeg);
        let img = image::load_from_memory(&out.bytes).unwrap();
        assert_eq!(img.dimensions(), (50, 100));
    }

    #[test]
    fn corrupt_image_is_an_error() {
        let bad = Image::new(Bytes::new(b"\x89PNG\r\n\x1a\ngarbage".to_vec())).unwrap();
        assert!(downscale(bad, 10).is_err());
    }
}
