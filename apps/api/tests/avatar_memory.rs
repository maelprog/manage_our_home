//! What one avatar decode holds at its peak (#588), counted by a global
//! allocator of this test binary's own — hence a file of its own, with a
//! single test, so that nothing else allocates meanwhile. The figures are
//! printed (`-- --nocapture`) and bounded: they are the ones
//! `avatar::DECODE_PEAK_BYTES` documents for sizing the api's memory.

use std::alloc::{GlobalAlloc, Layout, System};
use std::io::Cursor;
use std::sync::atomic::{AtomicUsize, Ordering};

use image::codecs::jpeg::JpegEncoder;
use image::codecs::webp::WebPEncoder;
use image::{DynamicImage, ImageEncoder, ImageFormat, Rgb, RgbImage};
use jpeg_encoder::SamplingFactor;
use manage_our_home::avatar::{reencode_avatar, DECODE_PEAK_BYTES, MAX_AVATAR_BYTES};

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

fn jpeg_encoder_jpeg(image: &RgbImage, progressive: bool, sampling: SamplingFactor) -> Vec<u8> {
    let mut out = Vec::new();
    let mut encoder = jpeg_encoder::Encoder::new(&mut out, 90);
    encoder.set_progressive(progressive);
    encoder.set_sampling_factor(sampling);
    encoder
        .encode(
            image.as_raw(),
            image.width() as u16,
            image.height() as u16,
            jpeg_encoder::ColorType::Rgb,
        )
        .unwrap();
    out
}

/// `jpeg` with APP2 `ICC_PROFILE` segments of about `total` bytes in all,
/// right after its SOI marker.
fn with_icc(jpeg: &[u8], total: usize) -> Vec<u8> {
    const CHUNK: usize = 65_000;
    let count = total.div_ceil(CHUNK);
    let mut out = jpeg[..2].to_vec();
    for i in 0..count {
        let mut payload = b"ICC_PROFILE\0".to_vec();
        payload.push((i + 1) as u8);
        payload.push(count as u8);
        payload.extend(std::iter::repeat_n(0x5A, CHUNK));
        out.extend_from_slice(&[0xFF, 0xE2]);
        out.extend_from_slice(&((payload.len() + 2) as u16).to_be_bytes());
        out.extend_from_slice(&payload);
    }
    out.extend_from_slice(&jpeg[2..]);
    out
}

const MB: f64 = 1_000_000.0;

#[test]
fn a_decode_holds_at_most_the_documented_peak() {
    let mut worst = 0;
    let mut measure = |label: &str, upload: Vec<u8>| {
        let peak = peak_of(&upload);
        eprintln!("avatar decode peak: {label}: {:.1} MB", peak as f64 / MB);
        worst = worst.max(peak);
        peak
    };

    // The photo of #588, sequential as a phone's camera writes it, in
    // 4:2:0 (`image`'s encoder) and in 4:4:4: decoded at 1/8, its peak
    // does not follow its 48 Mpx.
    let photo = picture(8000, 6000);
    let mut sequential = Vec::new();
    JpegEncoder::new_with_quality(&mut sequential, 90)
        .encode_image(&photo)
        .unwrap();
    let sequential_420 = measure("sequential JPEG 8000x6000 4:2:0", sequential);
    let sequential_444 = measure(
        "sequential JPEG 8000x6000 4:4:4",
        jpeg_encoder_jpeg(&photo, false, SamplingFactor::F_1_1),
    );
    drop(photo);
    for peak in [sequential_420, sequential_444] {
        assert!(peak < 16_000_000, "{peak}");
    }

    // A sequential strip at `MAX_JPEG_SIDE`: the widest rows the reduced
    // decode hands its threads.
    measure(
        "sequential JPEG 16384x511 4:4:4",
        jpeg_encoder_jpeg(&picture(16384, 511), false, SamplingFactor::F_1_1),
    );

    // The progressive JPEG nearest `MAX_PROGRESSIVE_COEFFICIENT_BYTES`,
    // in 4:4:4: 6031 × 4631 × 3 × 2 = 167.6 MB reckoned, under the
    // 167.8 MB bound — then the same with an ICC profile filling the file
    // up to `MAX_AVATAR_BYTES`, which the decoder keeps.
    let progressive = jpeg_encoder_jpeg(&picture(6000, 4600), true, SamplingFactor::F_1_1);
    let icc = with_icc(&progressive, MAX_AVATAR_BYTES - progressive.len() - 400_000);
    measure("progressive JPEG 6000x4600 4:4:4", progressive);
    measure("progressive JPEG 6000x4600 4:4:4 + ICC", icc);

    // The whole decodes `image` keeps, at a frame just under `MAX_ALLOC`:
    // 5280 × 5280 in RGB, 83.6 MB, as a PNG and as a lossless WebP.
    let source = picture(5280, 5280);
    let mut png = Vec::new();
    DynamicImage::ImageRgb8(source.clone())
        .write_to(&mut Cursor::new(&mut png), ImageFormat::Png)
        .unwrap();
    let mut webp = Vec::new();
    WebPEncoder::new_lossless(&mut webp)
        .write_image(source.as_raw(), 5280, 5280, image::ExtendedColorType::Rgb8)
        .unwrap();
    drop(source);
    measure("PNG 5280x5280 RGB", png);
    measure("WebP 5280x5280 RGB lossless", webp);

    assert!(worst <= DECODE_PEAK_BYTES, "{worst} > {DECODE_PEAK_BYTES}");
}
