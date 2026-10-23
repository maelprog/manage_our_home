//! What one avatar decode holds at its peak (#588), counted by a global
//! allocator of this test binary's own — hence a file of its own, with a
//! single test, so that nothing else allocates meanwhile. The figures are
//! printed (`-- --nocapture`) and bounded: they are the ones
//! `avatar::DECODE_PEAK_BYTES` documents for sizing the api's memory.

use std::alloc::{GlobalAlloc, Layout, System};
use std::io::Cursor;
use std::sync::atomic::{AtomicUsize, Ordering};

use image::codecs::webp::WebPEncoder;
use image::{DynamicImage, ImageEncoder, ImageFormat, Rgb, RgbImage};
use jpeg_encoder::{ImageBuffer, JpegColorType, SamplingFactor};
use manage_our_home::avatar::{
    reencode_avatar, AvatarError, DECODE_PEAK_BYTES, MAX_AVATAR_BYTES, MAX_JPEG_SIDE,
    MAX_METADATA_BYTES,
};

struct Counting;

static CURRENT: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

fn grew(by: usize) {
    let now = CURRENT.fetch_add(by, Ordering::SeqCst) + by;
    PEAK.fetch_max(now, Ordering::SeqCst);
}

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let p = unsafe { System.alloc(layout) };
        if !p.is_null() {
            grew(layout.size());
        }
        p
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let p = unsafe { System.alloc_zeroed(layout) };
        if !p.is_null() {
            grew(layout.size());
        }
        p
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) };
        CURRENT.fetch_sub(layout.size(), Ordering::SeqCst);
    }

    /// Counted as the move it may be: the old and the new block both live
    /// for a moment.
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let p = unsafe { System.realloc(ptr, layout, new_size) };
        if !p.is_null() {
            grew(new_size);
            CURRENT.fetch_sub(layout.size(), Ordering::SeqCst);
        }
        p
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// The bytes `reencode_avatar` holds at its peak on top of what was
/// allocated before it — the upload's own bytes among them.
fn peak_of(upload: &[u8]) -> usize {
    assert!(upload.len() <= MAX_AVATAR_BYTES, "{}", upload.len());
    let before = CURRENT.load(Ordering::SeqCst);
    PEAK.store(before, Ordering::SeqCst);
    reencode_avatar(upload).expect("the upload is accepted");
    PEAK.load(Ordering::SeqCst) - before
}

/// As `peak_of`, for an upload refused with `refusal`.
fn refusal_peak_of(upload: &[u8], refusal: AvatarError) -> usize {
    assert!(upload.len() <= MAX_AVATAR_BYTES, "{}", upload.len());
    let before = CURRENT.load(Ordering::SeqCst);
    PEAK.store(before, Ordering::SeqCst);
    assert_eq!(reencode_avatar(upload), Err(refusal));
    PEAK.load(Ordering::SeqCst) - before
}

/// CRC-32 of the PNG chunks (ISO 3309), bit by bit.
fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &byte in bytes {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = if crc & 1 == 1 {
                crc >> 1 ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

/// `png` with an iCCP chunk right after its IHDR, holding `profile`.
fn png_with_iccp(png: &[u8], profile: &[u8]) -> Vec<u8> {
    use std::io::Write;
    let mut typed = b"iCCPicc\0\0".to_vec();
    let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::best());
    z.write_all(profile).unwrap();
    typed.extend_from_slice(&z.finish().unwrap());
    let mut chunk = ((typed.len() - 4) as u32).to_be_bytes().to_vec();
    chunk.extend_from_slice(&typed);
    chunk.extend_from_slice(&crc32(&typed).to_be_bytes());
    let after_ihdr = 8 + 4 + 4 + 13 + 4;
    let mut out = png[..after_ihdr].to_vec();
    out.extend_from_slice(&chunk);
    out.extend_from_slice(&png[after_ihdr..]);
    out
}

/// A VP8X `webp` with an EXIF chunk of `size` bytes appended, its flag set.
fn webp_with_exif(webp: &[u8], size: usize) -> Vec<u8> {
    assert_eq!(&webp[12..16], b"VP8X");
    let mut out = webp.to_vec();
    out[20] |= 0x08;
    out.extend_from_slice(b"EXIF");
    out.extend_from_slice(&(size as u32).to_le_bytes());
    out.extend(std::iter::repeat_n(0, size + size % 2));
    let riff = (out.len() - 8) as u32;
    out[4..8].copy_from_slice(&riff.to_le_bytes());
    out
}

/// A WebP whose VP8X canvas is 100 × 100 and whose `fourcc` frame chunk,
/// header only, declares 16383 × 16383.
fn webp_frame_past_canvas(fourcc: &[u8; 4]) -> Vec<u8> {
    let frame: Vec<u8> = if fourcc == b"VP8 " {
        let mut data = vec![0x50, 0x00, 0x00, 0x9D, 0x01, 0x2A];
        data.extend_from_slice(&16383u16.to_le_bytes());
        data.extend_from_slice(&16383u16.to_le_bytes());
        data
    } else {
        let mut data = vec![0x2F];
        data.extend_from_slice(&(16382u32 | 16382 << 14).to_le_bytes());
        data
    };
    let mut body = b"WEBPVP8X".to_vec();
    body.extend_from_slice(&10u32.to_le_bytes());
    body.extend_from_slice(&[0, 0, 0, 0, 99, 0, 0, 99, 0, 0]);
    body.extend_from_slice(fourcc);
    body.extend_from_slice(&(frame.len() as u32).to_le_bytes());
    body.extend_from_slice(&frame);
    if frame.len() % 2 == 1 {
        body.push(0);
    }
    let mut out = b"RIFF".to_vec();
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend_from_slice(&body);
    out
}

fn picture(width: u32, height: u32) -> RgbImage {
    RgbImage::from_fn(width, height, |x, y| {
        Rgb([(x / 16) as u8, (y / 16) as u8, ((x + y) / 32) as u8])
    })
}

/// A picture `jpeg-encoder` draws row by row, so that a fixture of any
/// size costs no frame of its own: a 16384 × 16384 RGB source would be
/// 805 MB.
struct Pattern {
    width: u16,
    height: u16,
    color: JpegColorType,
}

impl ImageBuffer for Pattern {
    fn get_jpeg_color_type(&self) -> JpegColorType {
        self.color
    }

    fn width(&self) -> u16 {
        self.width
    }

    fn height(&self) -> u16 {
        self.height
    }

    fn fill_buffers(&self, y: u16, buffers: &mut [Vec<u8>; 4]) {
        let components = match self.color {
            JpegColorType::Luma => 1,
            JpegColorType::Ycbcr => 3,
            JpegColorType::Cmyk | JpegColorType::Ycck => 4,
        };
        for x in 0..self.width {
            let values = [(x / 16) as u8, (y / 16) as u8, ((x + y) / 32) as u8, 40];
            for (buffer, value) in buffers.iter_mut().zip(values).take(components) {
                buffer.push(value);
            }
        }
    }
}

fn pattern_jpeg(
    width: u16,
    height: u16,
    color: JpegColorType,
    progressive: bool,
    sampling: SamplingFactor,
) -> Vec<u8> {
    let mut out = Vec::new();
    let mut encoder = jpeg_encoder::Encoder::new(&mut out, 90);
    encoder.set_progressive(progressive);
    encoder.set_sampling_factor(sampling);
    encoder
        .encode_image(Pattern {
            width,
            height,
            color,
        })
        .unwrap();
    out
}

/// `jpeg` with APP2 `ICC_PROFILE` segments of about `total` bytes in all,
/// right after its SOI marker. Past 255 segments the numbering wraps: the
/// profile is then invalid, but the decoder holds every segment all the
/// same, which is what is measured.
fn with_icc(jpeg: &[u8], total: usize) -> Vec<u8> {
    const CHUNK: usize = 65_000;
    let count = total.div_ceil(CHUNK);
    let mut out = jpeg[..2].to_vec();
    for i in 0..count {
        let mut payload = b"ICC_PROFILE\0".to_vec();
        payload.push((i % 255 + 1) as u8);
        payload.push(count.min(255) as u8);
        payload.extend(std::iter::repeat_n(0x5A, CHUNK));
        out.extend_from_slice(&[0xFF, 0xE2]);
        out.extend_from_slice(&((payload.len() + 2) as u16).to_be_bytes());
        out.extend_from_slice(&payload);
    }
    out.extend_from_slice(&jpeg[2..]);
    out
}

const MIB: f64 = 1024.0 * 1024.0;

#[test]
fn a_decode_holds_at_most_the_documented_peak() {
    use JpegColorType::{Cmyk, Luma, Ycbcr};
    use SamplingFactor::{F_1_1, F_2_2};

    let mut worst = 0;
    let mut measure = |label: &str, upload: Vec<u8>| {
        let peak = peak_of(&upload);
        eprintln!("avatar decode peak: {label}: {:.1} MiB", peak as f64 / MIB);
        worst = worst.max(peak);
    };

    // Sequential, reduced at decode: the photo of #588 in 4:2:0, 4:4:4 and
    // CMYK, then at `MAX_JPEG_SIDE` — the most rows queued for the
    // decoder's threads.
    let side = MAX_JPEG_SIDE as u16;
    measure(
        "sequential JPEG 8000x6000 4:2:0",
        pattern_jpeg(8000, 6000, Ycbcr, false, F_2_2),
    );
    measure(
        "sequential JPEG 8000x6000 4:4:4",
        pattern_jpeg(8000, 6000, Ycbcr, false, F_1_1),
    );
    measure(
        "sequential JPEG 8000x6000 CMYK",
        pattern_jpeg(8000, 6000, Cmyk, false, F_1_1),
    );
    measure(
        "sequential JPEG at MAX_JPEG_SIDE square 4:2:0",
        pattern_jpeg(side, side, Ycbcr, false, F_2_2),
    );

    // Progressive, each just under `MAX_PROGRESSIVE_COEFFICIENT_BYTES`
    // (23 068 672 bytes): 4:2:0 3200 × 2400 (23 040 000), 4:4:4
    // 2240 × 1712 (23 009 280), greyscale 3900 × 2940 (22 986 752), CMYK
    // 1952 × 1468 (22 986 752) — then the 4:2:0 one with ICC segments
    // filling the file up to `MAX_AVATAR_BYTES`, which never reach the
    // decoder.
    let progressive = pattern_jpeg(3200, 2400, Ycbcr, true, F_2_2);
    let icc = with_icc(&progressive, MAX_AVATAR_BYTES - progressive.len() - 400_000);
    measure("progressive JPEG 3200x2400 4:2:0", progressive);
    measure("progressive JPEG 3200x2400 4:2:0 + ICC", icc);
    measure(
        "progressive JPEG 2240x1712 4:4:4",
        pattern_jpeg(2240, 1712, Ycbcr, true, F_1_1),
    );
    measure(
        "progressive JPEG 3900x2940 grey",
        pattern_jpeg(3900, 2940, Luma, true, F_1_1),
    );
    measure(
        "progressive JPEG 1952x1468 CMYK",
        pattern_jpeg(1952, 1468, Cmyk, true, F_1_1),
    );

    // The whole decodes `image` keeps, at their bounds: a PNG whose RGB
    // frame is just under `MAX_PNG_FRAME_BYTES` (3739 × 3739 × 3 =
    // 41 940 363 bytes), and WebPs of 2097 × 2097, just under
    // `MAX_WEBP_PIXELS` — lossless in RGB and RGBA, lossy in RGB and with
    // an alpha channel (cwebp 1.2.4, `-q 50`, in tests/fixtures/avatar:
    // no lossy encoder here).
    let mut png = Vec::new();
    DynamicImage::ImageRgb8(picture(3739, 3739))
        .write_to(&mut Cursor::new(&mut png), ImageFormat::Png)
        .unwrap();
    // The same with an iCCP profile of `MAX_METADATA_BYTES` once inflated,
    // which `png` inflates and keeps.
    let png_iccp = png_with_iccp(&png, &vec![0; MAX_METADATA_BYTES]);
    measure("PNG 3739x3739 RGB", png);
    measure("PNG 3739x3739 RGB + iCCP at MAX_METADATA_BYTES", png_iccp);
    let source = picture(2097, 2097);
    let mut webp = Vec::new();
    WebPEncoder::new_lossless(&mut webp)
        .write_image(source.as_raw(), 2097, 2097, image::ExtendedColorType::Rgb8)
        .unwrap();
    measure("WebP 2097x2097 RGB lossless", webp);
    let rgba = DynamicImage::ImageRgb8(source).to_rgba8();
    let mut webp = Vec::new();
    WebPEncoder::new_lossless(&mut webp)
        .write_image(rgba.as_raw(), 2097, 2097, image::ExtendedColorType::Rgba8)
        .unwrap();
    drop(rgba);
    measure("WebP 2097x2097 RGBA lossless", webp);
    measure(
        "WebP 2097x2097 RGB lossy",
        include_bytes!("fixtures/avatar/avatar-lossy-rgb-2097.webp").to_vec(),
    );
    let lossy_alpha = include_bytes!("fixtures/avatar/avatar-lossy-alpha-2097.webp");
    measure("WebP 2097x2097 lossy + alpha", lossy_alpha.to_vec());
    measure(
        "WebP 2097x2097 lossy + alpha + EXIF at MAX_METADATA_BYTES",
        webp_with_exif(lossy_alpha, MAX_METADATA_BYTES),
    );

    // Refused before any decode: ICC segments past the first scan of the
    // progressive JPEG at the bound, filling the file up to the cap. Its
    // walk allocates nothing of the file's size.
    let progressive = pattern_jpeg(3200, 2400, Ycbcr, true, F_2_2);
    let first_scan = progressive
        .windows(2)
        .position(|w| w == [0xFF, 0xDA])
        .unwrap();
    let second_scan = first_scan
        + 2
        + progressive[first_scan + 2..]
            .windows(2)
            .position(|w| w == [0xFF, 0xDA])
            .unwrap();
    let segments = with_icc(
        &[0xFF, 0xD8],
        MAX_AVATAR_BYTES - progressive.len() - 400_000,
    );
    let mut late_icc = progressive[..second_scan].to_vec();
    late_icc.extend_from_slice(&segments[2..]);
    late_icc.extend_from_slice(&progressive[second_scan..]);
    drop(progressive);
    let mut refused = |label: &str, upload: &[u8], refusal: AvatarError| {
        let peak = refusal_peak_of(upload, refusal);
        eprintln!(
            "avatar decode peak: {label} (refused): {:.1} MiB",
            peak as f64 / MIB
        );
        worst = worst.max(peak);
    };
    refused(
        "progressive JPEG 3200x2400 + ICC past the first scan",
        &late_icc,
        AvatarError::ImageTooLarge,
    );
    // A WebP frame other than its canvas, which `image-webp` would decode
    // at the frame's size; a PNG whose profile inflates past the bound.
    refused(
        "WebP canvas 100x100, VP8 frame 16383x16383",
        &webp_frame_past_canvas(b"VP8 "),
        AvatarError::Unreadable,
    );
    refused(
        "WebP canvas 100x100, VP8L frame 16383x16383",
        &webp_frame_past_canvas(b"VP8L"),
        AvatarError::Unreadable,
    );
    let mut small = Vec::new();
    DynamicImage::ImageRgb8(picture(64, 64))
        .write_to(&mut Cursor::new(&mut small), ImageFormat::Png)
        .unwrap();
    refused(
        "PNG 64x64 + iCCP of 100 MiB inflated",
        &png_with_iccp(&small, &vec![0; 100 * 1024 * 1024]),
        AvatarError::ImageTooLarge,
    );

    assert!(worst <= DECODE_PEAK_BYTES, "{worst} > {DECODE_PEAK_BYTES}");
}
