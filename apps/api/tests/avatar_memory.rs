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
    reencode_avatar, DECODE_PEAK_BYTES, MAX_AVATAR_BYTES, MAX_JPEG_SIDE,
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
    // 41 940 363 bytes), and lossless WebPs of 2645 × 2645, just under
    // `MAX_WEBP_PIXELS`, in RGB and RGBA.
    let mut png = Vec::new();
    DynamicImage::ImageRgb8(picture(3739, 3739))
        .write_to(&mut Cursor::new(&mut png), ImageFormat::Png)
        .unwrap();
    measure("PNG 3739x3739 RGB", png);
    let source = picture(2645, 2645);
    let mut webp = Vec::new();
    WebPEncoder::new_lossless(&mut webp)
        .write_image(source.as_raw(), 2645, 2645, image::ExtendedColorType::Rgb8)
        .unwrap();
    measure("WebP 2645x2645 RGB lossless", webp);
    let rgba = DynamicImage::ImageRgb8(source).to_rgba8();
    let mut webp = Vec::new();
    WebPEncoder::new_lossless(&mut webp)
        .write_image(rgba.as_raw(), 2645, 2645, image::ExtendedColorType::Rgba8)
        .unwrap();
    drop(rgba);
    measure("WebP 2645x2645 RGBA lossless", webp);

    assert!(worst <= DECODE_PEAK_BYTES, "{worst} > {DECODE_PEAK_BYTES}");
}
