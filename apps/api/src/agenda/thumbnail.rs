//! An attachment's thumbnail (#774), generated at upload for a picture:
//! its long side brought to `THUMBNAIL_MAX_SIDE` pixels at most, encoded
//! afresh as a JPEG, stored under its own key (`thumbnail_key`, 0053).
//! A video or a PDF gets none.
//!
//! Best effort: a picture that passed `storage::sniff_and_validate_mime`
//! but cannot be decoded, or whose decode would pass the bounds below, is
//! stored all the same, without a thumbnail. The bounds are those of the
//! avatar's decode (`avatar::DECODE_PEAK_BYTES`), whose permit the
//! thumbnails share: a JPEG reduced in the inverse DCT, a PNG whose frame
//! and metadata are checked on its header and chunks; and, for a WebP,
//! `check_webp_bounds` on its chunk headers, before any decode. A
//! decompression bomb — a small file declaring a huge frame — is refused
//! on what it declares, never decoded.

use std::io::Cursor;

use image::codecs::jpeg::JpegEncoder;
use image::{imageops, DynamicImage, ImageFormat, ImageReader, Limits, Rgb, RgbImage};

use crate::avatar;

/// Bound on the thumbnail's long side, in pixels.
pub const THUMBNAIL_MAX_SIDE: u32 = 400;

/// Every thumbnail is a JPEG.
pub const THUMBNAIL_CONTENT_TYPE: &str = "image/jpeg";

const THUMBNAIL_JPEG_QUALITY: u8 = 80;

/// Bound on a WebP's frame and canvas, in pixels: 4 Mi (2048 × 2048).
/// `image-webp` decodes a frame whole at the size its header declares,
/// copies its chunk, and for a lossless frame without alpha decodes into a
/// second buffer of 4 bytes a pixel; the peak at this bound is counted by
/// `tests/thumbnail_memory.rs`, under `avatar::DECODE_PEAK_BYTES`.
pub const MAX_WEBP_PIXELS: u64 = 4 * 1024 * 1024;

/// The thumbnail's object key: the attachment's, suffixed. Under the same
/// `{group}/{event}/` prefix, so the reconciliation pass (#58) lists both
/// together.
pub fn thumbnail_key(storage_key: &str) -> String {
    format!("{storage_key}-thumbnail")
}

/// The thumbnail's dimensions for a `width` × `height` picture: the long
/// side brought to `THUMBNAIL_MAX_SIDE`, the ratio kept, rounded to the
/// nearest pixel and never under one; a picture already within the bound
/// keeps its size, never enlarged.
pub fn thumbnail_size(width: u32, height: u32) -> (u32, u32) {
    let long = width.max(height);
    if long <= THUMBNAIL_MAX_SIDE {
        return (width, height);
    }
    // In u64: a u32 side times the bound overflows nothing.
    let scale = |side: u32| {
        let scaled = (u64::from(side) * u64::from(THUMBNAIL_MAX_SIDE) + u64::from(long) / 2)
            / u64::from(long);
        (scaled as u32).max(1)
    };
    (scale(width), scale(height))
}

/// Whether a WebP's decode stays within `MAX_WEBP_PIXELS`, on its chunk
/// headers alone: the canvas of a VP8X header and the frame of every VP8
/// and VP8L chunk, each bounded — `image-webp` decodes an extended file's
/// frame at the frame's own size before comparing it to the canvas. An
/// animation is refused (its frames are composited on a canvas of their
/// own), as is a file cut short or with no frame. Allocates nothing.
pub fn check_webp_bounds(bytes: &[u8]) -> bool {
    let within = |width: u64, height: u64| width * height <= MAX_WEBP_PIXELS;
    if bytes.len() < 12 || &bytes[..4] != b"RIFF" || &bytes[8..12] != b"WEBP" {
        return false;
    }
    let mut frames = 0;
    let mut pos = 12;
    while pos < bytes.len() {
        let Some(header) = bytes.get(pos..pos + 8) else {
            return false;
        };
        let size = u32::from_le_bytes([header[4], header[5], header[6], header[7]]) as usize;
        let Some(data) = bytes.get(pos + 8..pos + 8 + size) else {
            return false;
        };
        match &header[..4] {
            b"VP8X" => {
                let Some(&[flags, _, _, _, w0, w1, w2, h0, h1, h2]) = data.get(..10) else {
                    return false;
                };
                let width = u64::from(u32::from_le_bytes([w0, w1, w2, 0])) + 1;
                let height = u64::from(u32::from_le_bytes([h0, h1, h2, 0])) + 1;
                // Bit 1: an animation.
                if flags & 0x02 != 0 || !within(width, height) {
                    return false;
                }
            }
            b"ANIM" | b"ANMF" => return false,
            b"VP8 " => {
                // The frame tag (3 bytes), the start code, two 14-bit sides.
                let Some(&[_, _, _, 0x9D, 0x01, 0x2A, w0, w1, h0, h1]) = data.get(..10) else {
                    return false;
                };
                let width = u64::from(u16::from_le_bytes([w0, w1]) & 0x3FFF);
                let height = u64::from(u16::from_le_bytes([h0, h1]) & 0x3FFF);
                if !within(width, height) {
                    return false;
                }
                frames += 1;
            }
            b"VP8L" => {
                let Some(&[0x2F, s0, s1, s2, s3]) = data.get(..5) else {
                    return false;
                };
                let sides = u32::from_le_bytes([s0, s1, s2, s3]);
                let width = u64::from(sides & 0x3FFF) + 1;
                let height = u64::from((sides >> 14) & 0x3FFF) + 1;
                if !within(width, height) {
                    return false;
                }
                frames += 1;
            }
            _ => {}
        }
        // Chunks are padded to an even size.
        pos += 8 + size + (size & 1);
    }
    frames > 0
}

/// The thumbnail of an upload of type `mime_type` (as sniffed), or `None`:
/// not a picture, unreadable, or past a bound. CPU-bound: run off the
/// async workers, under `avatar::DECODES`.
pub fn make_thumbnail(bytes: &[u8], mime_type: &str) -> Option<Vec<u8>> {
    let (picture, orientation) = match mime_type {
        "image/jpeg" => avatar::decode_jpeg_reduced(bytes, jpeg_scale_request).ok()?,
        "image/png" => avatar::decode_png(bytes).ok()?,
        "image/webp" => (decode_webp(bytes)?, None),
        _ => return None,
    };

    let (width, height) = thumbnail_size(picture.width(), picture.height());
    // Integer blocks averaged straight from the frame: no buffer of the
    // source's size besides the frame itself.
    let small = imageops::thumbnail(&picture, width, height);
    drop(picture);

    // Transparency laid on white: JPEG has no alpha (as in `avatar`).
    let flat = RgbImage::from_fn(width, height, |x, y| {
        let [r, g, b, a] = small.get_pixel(x, y).0;
        let alpha = u16::from(a);
        let lay = |c: u8| ((u16::from(c) * alpha + 255 * (255 - alpha)) / 255) as u8;
        Rgb([lay(r), lay(g), lay(b)])
    });
    let mut out = DynamicImage::ImageRgb8(flat);
    if let Some(orientation) = orientation {
        out.apply_orientation(orientation);
    }

    let mut jpeg = Vec::new();
    JpegEncoder::new_with_quality(&mut jpeg, THUMBNAIL_JPEG_QUALITY)
        .encode_image(&out)
        .ok()?;
    Some(jpeg)
}

/// The size asked of `jpeg_decoder::Decoder::scale`, which picks the
/// smallest reduction reaching the request on either axis: the long side
/// comes out at `THUMBNAIL_MAX_SIDE` at least whenever it is that long.
fn jpeg_scale_request(_width: u16, _height: u16) -> (u16, u16) {
    let side = THUMBNAIL_MAX_SIDE as u16;
    (side, side)
}

/// A still WebP within `check_webp_bounds`, decoded whole. Its EXIF is
/// not read: `image-webp` copies that chunk whatever its size, and a WebP
/// rarely carries an orientation.
fn decode_webp(bytes: &[u8]) -> Option<DynamicImage> {
    if !check_webp_bounds(bytes) {
        return None;
    }
    // A second guard, on the canvas `image` reads from the header.
    let mut limits = Limits::default();
    limits.max_alloc = Some(MAX_WEBP_PIXELS * 4);
    let mut reader = ImageReader::with_format(Cursor::new(bytes), ImageFormat::WebP);
    reader.limits(limits);
    reader.decode().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::codecs::png::PngEncoder;
    use image::codecs::webp::WebPEncoder;
    use image::{GenericImageView, ImageEncoder, Rgba, RgbaImage};

    fn picture(width: u32, height: u32) -> RgbImage {
        RgbImage::from_fn(width, height, |x, y| Rgb([x as u8, y as u8, 90]))
    }

    fn png(image: &RgbImage) -> Vec<u8> {
        let mut out = Vec::new();
        PngEncoder::new(&mut out)
            .write_image(
                image.as_raw(),
                image.width(),
                image.height(),
                image::ExtendedColorType::Rgb8,
            )
            .unwrap();
        out
    }

    fn jpeg(image: &RgbImage) -> Vec<u8> {
        let mut out = Vec::new();
        JpegEncoder::new_with_quality(&mut out, 90)
            .encode_image(image)
            .unwrap();
        out
    }

    fn webp(image: &RgbaImage) -> Vec<u8> {
        let mut out = Vec::new();
        WebPEncoder::new_lossless(&mut out)
            .write_image(
                image.as_raw(),
                image.width(),
                image.height(),
                image::ExtendedColorType::Rgba8,
            )
            .unwrap();
        out
    }

    /// The dimensions of a stored thumbnail, read back as the JPEG it is.
    fn dimensions(thumbnail: &[u8]) -> (u32, u32) {
        assert_eq!(image::guess_format(thumbnail).unwrap(), ImageFormat::Jpeg);
        image::load_from_memory_with_format(thumbnail, ImageFormat::Jpeg)
            .unwrap()
            .dimensions()
    }

    /// A RIFF/WEBP file of the given chunks, each `(fourcc, data)`.
    fn riff(chunks: &[(&[u8; 4], Vec<u8>)]) -> Vec<u8> {
        let mut body = b"WEBP".to_vec();
        for (fourcc, data) in chunks {
            body.extend_from_slice(*fourcc);
            body.extend_from_slice(&(data.len() as u32).to_le_bytes());
            body.extend_from_slice(data);
            if data.len() % 2 == 1 {
                body.push(0);
            }
        }
        let mut out = b"RIFF".to_vec();
        out.extend_from_slice(&(body.len() as u32).to_le_bytes());
        out.extend_from_slice(&body);
        out
    }

    /// A lossy key frame's header declaring `width` × `height`, then
    /// filler.
    fn vp8(width: u16, height: u16) -> Vec<u8> {
        let mut data = vec![0x50, 0x00, 0x00, 0x9D, 0x01, 0x2A];
        data.extend_from_slice(&width.to_le_bytes());
        data.extend_from_slice(&height.to_le_bytes());
        data.resize(64, 0);
        data
    }

    /// A lossless header declaring `width` × `height`, then filler.
    fn vp8l(width: u32, height: u32) -> Vec<u8> {
        let header = (width - 1) | ((height - 1) << 14);
        let mut data = vec![0x2F];
        data.extend_from_slice(&header.to_le_bytes());
        data.resize(64, 0);
        data
    }

    /// A VP8X header: its flags, then the canvas.
    fn vp8x(flags: u8, width: u32, height: u32) -> Vec<u8> {
        let mut data = vec![flags, 0, 0, 0];
        data.extend_from_slice(&(width - 1).to_le_bytes()[..3]);
        data.extend_from_slice(&(height - 1).to_le_bytes()[..3]);
        data
    }

    /// `png` with its IHDR declaring `width` × `height`, its CRC redone:
    /// what a decompression bomb declares.
    fn png_declaring(png: &[u8], width: u32, height: u32) -> Vec<u8> {
        let mut out = png.to_vec();
        out[16..20].copy_from_slice(&width.to_be_bytes());
        out[20..24].copy_from_slice(&height.to_be_bytes());
        let mut crc = flate2::Crc::new();
        crc.update(&out[12..29]);
        out[29..33].copy_from_slice(&crc.sum().to_be_bytes());
        out
    }

    /// An EXIF block holding an orientation and nothing else.
    fn exif(orientation: u16) -> Vec<u8> {
        let mut tiff = b"II*\0".to_vec();
        tiff.extend_from_slice(&8u32.to_le_bytes());
        tiff.extend_from_slice(&1u16.to_le_bytes());
        tiff.extend_from_slice(&0x0112u16.to_le_bytes());
        tiff.extend_from_slice(&3u16.to_le_bytes());
        tiff.extend_from_slice(&1u32.to_le_bytes());
        tiff.extend_from_slice(&orientation.to_le_bytes());
        tiff.extend_from_slice(&[0, 0]);
        tiff.extend_from_slice(&0u32.to_le_bytes());
        tiff
    }

    /// `jpeg` with an APP1 EXIF segment right after its SOI marker.
    fn with_exif(jpeg: &[u8], orientation: u16) -> Vec<u8> {
        let mut payload = b"Exif\0\0".to_vec();
        payload.extend_from_slice(&exif(orientation));
        let mut out = jpeg[..2].to_vec();
        out.extend_from_slice(&[0xFF, 0xE1]);
        out.extend_from_slice(&((payload.len() + 2) as u16).to_be_bytes());
        out.extend_from_slice(&payload);
        out.extend_from_slice(&jpeg[2..]);
        out
    }

    // -- thumbnail_size --------------------------------------------------

    #[test]
    fn the_long_side_is_brought_to_the_bound_and_the_ratio_kept() {
        assert_eq!(thumbnail_size(4000, 3000), (400, 300));
        assert_eq!(thumbnail_size(3000, 4000), (300, 400));
        assert_eq!(thumbnail_size(1920, 1080), (400, 225));
        // 1000 × 333 is 400 × 133.2: rounded to the nearest pixel.
        assert_eq!(thumbnail_size(1000, 333), (400, 133));
        assert_eq!(thumbnail_size(999, 1000), (400, 400));
    }

    #[test]
    fn a_picture_within_the_bound_is_never_enlarged() {
        assert_eq!(thumbnail_size(400, 400), (400, 400));
        assert_eq!(thumbnail_size(120, 80), (120, 80));
        assert_eq!(thumbnail_size(1, 1), (1, 1));
        assert_eq!(thumbnail_size(400, 10), (400, 10));
    }

    /// A strip: its short side would round to nothing, kept at one pixel.
    #[test]
    fn a_short_side_never_falls_under_one_pixel() {
        assert_eq!(thumbnail_size(16000, 10), (400, 1));
        assert_eq!(thumbnail_size(1, 65535), (1, 400));
    }

    #[test]
    fn the_thumbnail_key_sits_beside_the_attachment() {
        assert_eq!(thumbnail_key("g/e/k"), "g/e/k-thumbnail");
    }

    // -- check_webp_bounds -----------------------------------------------

    #[test]
    fn a_still_webp_within_the_bound_passes() {
        assert!(check_webp_bounds(&riff(&[(b"VP8 ", vp8(2048, 2048))])));
        assert!(check_webp_bounds(&riff(&[(b"VP8L", vp8l(640, 480))])));
        assert!(check_webp_bounds(&riff(&[
            (b"VP8X", vp8x(0x10, 800, 600)),
            (b"ALPH", vec![0; 16]),
            (b"VP8 ", vp8(800, 600)),
        ])));
        assert!(check_webp_bounds(&webp(&RgbaImage::from_pixel(
            32,
            32,
            Rgba([1, 2, 3, 4])
        ))));
    }

    #[test]
    fn a_frame_past_the_bound_is_refused() {
        assert!(!check_webp_bounds(&riff(&[(b"VP8 ", vp8(2049, 2048))])));
        assert!(!check_webp_bounds(&riff(&[(b"VP8L", vp8l(16384, 16384))])));
    }

    /// The canvas says 64 × 64, its frame 16383 × 16383: the frame is
    /// what `image-webp` would decode first.
    #[test]
    fn an_extended_file_is_bounded_on_its_frame_as_well_as_its_canvas() {
        assert!(!check_webp_bounds(&riff(&[
            (b"VP8X", vp8x(0, 64, 64)),
            (b"VP8 ", vp8(16383, 16383)),
        ])));
        assert!(!check_webp_bounds(&riff(&[
            (b"VP8X", vp8x(0, 10000, 10000)),
            (b"VP8 ", vp8(64, 64)),
        ])));
    }

    #[test]
    fn an_animation_is_refused() {
        assert!(!check_webp_bounds(&riff(&[
            (b"VP8X", vp8x(0x02, 64, 64)),
            (b"ANIM", vec![0; 6]),
            (b"ANMF", vec![0; 32]),
        ])));
        // Without the flag, an ANMF chunk all the same.
        assert!(!check_webp_bounds(&riff(&[
            (b"VP8X", vp8x(0, 64, 64)),
            (b"ANMF", vec![0; 32]),
        ])));
    }

    #[test]
    fn a_broken_or_frameless_file_is_refused() {
        assert!(!check_webp_bounds(b""));
        assert!(!check_webp_bounds(b"RIFF\x04\x00\x00\x00WEBP"));
        assert!(!check_webp_bounds(&riff(&[(b"VP8X", vp8x(0, 64, 64))])));
        // A chunk claiming more than the file holds.
        let mut cut = riff(&[(b"VP8L", vp8l(64, 64))]);
        cut.truncate(30);
        assert!(!check_webp_bounds(&cut));
        // A VP8 chunk without its start code.
        let mut bad = vp8(64, 64);
        bad[3] = 0;
        assert!(!check_webp_bounds(&riff(&[(b"VP8 ", bad)])));
    }

    // -- make_thumbnail --------------------------------------------------

    #[test]
    fn a_png_gets_a_jpeg_thumbnail_its_long_side_at_the_bound() {
        let thumbnail = make_thumbnail(&png(&picture(800, 600)), "image/png").unwrap();
        assert_eq!(dimensions(&thumbnail), (400, 300));
    }

    #[test]
    fn a_jpeg_gets_a_thumbnail_reduced_from_its_long_side() {
        let thumbnail = make_thumbnail(&jpeg(&picture(1000, 2000)), "image/jpeg").unwrap();
        assert_eq!(dimensions(&thumbnail), (200, 400));
    }

    #[test]
    fn a_webp_gets_a_thumbnail() {
        let source = RgbaImage::from_pixel(600, 300, Rgba([10, 200, 30, 255]));
        let thumbnail = make_thumbnail(&webp(&source), "image/webp").unwrap();
        assert_eq!(dimensions(&thumbnail), (400, 200));
    }

    #[test]
    fn a_small_picture_keeps_its_size() {
        let thumbnail = make_thumbnail(&png(&picture(120, 80)), "image/png").unwrap();
        assert_eq!(dimensions(&thumbnail), (120, 80));
    }

    /// Orientation 6: the camera was turned a quarter clockwise, the
    /// picture is shown portrait.
    #[test]
    fn the_exif_orientation_is_applied() {
        let source = with_exif(&jpeg(&picture(800, 400)), 6);
        let thumbnail = make_thumbnail(&source, "image/jpeg").unwrap();
        assert_eq!(dimensions(&thumbnail), (200, 400));
    }

    /// Transparency is laid on white, not left to whatever colour the
    /// transparent pixels hold.
    #[test]
    fn transparency_is_laid_on_white() {
        let clear = RgbaImage::from_pixel(64, 64, Rgba([0, 0, 0, 0]));
        let thumbnail = make_thumbnail(&webp(&clear), "image/webp").unwrap();
        let back = image::load_from_memory_with_format(&thumbnail, ImageFormat::Jpeg)
            .unwrap()
            .to_rgb8();
        let Rgb([r, g, b]) = *back.get_pixel(32, 32);
        assert!(r > 245 && g > 245 && b > 245, "{r} {g} {b}");
    }

    #[test]
    fn a_video_or_a_pdf_gets_none() {
        let png = png(&picture(64, 64));
        assert_eq!(make_thumbnail(&png, "video/mp4"), None);
        assert_eq!(make_thumbnail(&png, "video/webm"), None);
        assert_eq!(make_thumbnail(b"%PDF-1.7\n", "application/pdf"), None);
    }

    #[test]
    fn an_unreadable_picture_gets_none() {
        assert_eq!(
            make_thumbnail(b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR", "image/png"),
            None
        );
        assert_eq!(
            make_thumbnail(b"\xFF\xD8\xFF\xE0garbage", "image/jpeg"),
            None
        );
        assert_eq!(make_thumbnail(b"RIFF\0\0\0\0WEBPVP8 ", "image/webp"), None);
    }

    /// A small file declaring a huge frame is refused on what it declares:
    /// a PNG of 30000 × 30000 (2.7 GB in RGB), a JPEG past `MAX_JPEG_SIDE`,
    /// a WebP past `MAX_WEBP_PIXELS`.
    #[test]
    fn a_decompression_bomb_gets_none() {
        let bomb = png_declaring(&png(&picture(64, 64)), 30000, 30000);
        assert_eq!(make_thumbnail(&bomb, "image/png"), None);

        let mut jpeg = jpeg(&picture(64, 64));
        let sof = jpeg.windows(2).position(|w| w == [0xFF, 0xC0]).unwrap();
        jpeg[sof + 5..sof + 7].copy_from_slice(&20000u16.to_be_bytes());
        jpeg[sof + 7..sof + 9].copy_from_slice(&20000u16.to_be_bytes());
        assert_eq!(make_thumbnail(&jpeg, "image/jpeg"), None);

        let webp = riff(&[(b"VP8L", vp8l(16384, 16384))]);
        assert_eq!(make_thumbnail(&webp, "image/webp"), None);
    }
}
