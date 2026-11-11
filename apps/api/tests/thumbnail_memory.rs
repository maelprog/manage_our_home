//! What one thumbnail decode holds at its peak (#774), counted by a global
//! allocator of this test binary's own — hence a file of its own, with a
//! single test, as `avatar_memory.rs`. The thumbnails share the avatar's
//! decode permit (`avatar::DECODES`), so their peak is held to the same
//! bound, `avatar::DECODE_PEAK_BYTES`. The figures are printed
//! (`-- --nocapture`).

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

use image::{ImageEncoder, Rgb, RgbImage, Rgba, RgbaImage};
use jpeg_encoder::{ImageBuffer, JpegColorType, SamplingFactor};
use manage_our_home::agenda::thumbnail::{make_thumbnail, MAX_WEBP_PIXELS};
use manage_our_home::avatar::{DECODE_PEAK_BYTES, MAX_JPEG_SIDE};
use manage_our_home::storage::MAX_ATTACHMENT_SIZE_BYTES;

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

/// The bytes `make_thumbnail` holds at its peak on top of what was
/// allocated before it — the upload's own bytes among them — and whether
/// it made a thumbnail.
fn peak_of(upload: &[u8], mime_type: &str) -> (usize, bool) {
    assert!(
        upload.len() <= MAX_ATTACHMENT_SIZE_BYTES,
        "{}",
        upload.len()
    );
    let before = CURRENT.load(Ordering::SeqCst);
    PEAK.store(before, Ordering::SeqCst);
    let made = make_thumbnail(upload, mime_type).is_some();
    (PEAK.load(Ordering::SeqCst) - before, made)
}

/// A picture `jpeg-encoder` draws row by row (as in `avatar_memory.rs`).
struct Pattern {
    width: u16,
    height: u16,
}

impl ImageBuffer for Pattern {
    fn get_jpeg_color_type(&self) -> JpegColorType {
        JpegColorType::Ycbcr
    }

    fn width(&self) -> u16 {
        self.width
    }

    fn height(&self) -> u16 {
        self.height
    }

    fn fill_buffers(&self, y: u16, buffers: &mut [Vec<u8>; 4]) {
        for x in 0..self.width {
            let values = [(x / 16) as u8, (y / 16) as u8, ((x + y) / 32) as u8];
            for (buffer, value) in buffers.iter_mut().zip(values) {
                buffer.push(value);
            }
        }
    }
}

fn pattern_jpeg(width: u16, height: u16, progressive: bool) -> Vec<u8> {
    let mut out = Vec::new();
    let mut encoder = jpeg_encoder::Encoder::new(&mut out, 90);
    encoder.set_progressive(progressive);
    encoder.set_sampling_factor(SamplingFactor::F_2_2);
    encoder.encode_image(Pattern { width, height }).unwrap();
    out
}

fn lossless_webp(width: u32, height: u32, alpha: bool) -> Vec<u8> {
    let mut out = Vec::new();
    let encoder = image::codecs::webp::WebPEncoder::new_lossless(&mut out);
    if alpha {
        let picture = RgbaImage::from_fn(width, height, |x, y| {
            Rgba([(x / 8) as u8, (y / 8) as u8, (x ^ y) as u8, (x + y) as u8])
        });
        encoder
            .write_image(
                picture.as_raw(),
                width,
                height,
                image::ExtendedColorType::Rgba8,
            )
            .unwrap();
    } else {
        let picture = RgbImage::from_fn(width, height, |x, y| {
            Rgb([(x / 8) as u8, (y / 8) as u8, (x ^ y) as u8])
        });
        encoder
            .write_image(
                picture.as_raw(),
                width,
                height,
                image::ExtendedColorType::Rgb8,
            )
            .unwrap();
    }
    out
}

/// A WebP whose lossy key frame declares `width` × `height` and whose
/// chunk is stuffed up to `size` bytes: the frame buffers are allocated on
/// the header, the chunk read whole.
fn stuffed_vp8(width: u16, height: u16, size: usize) -> Vec<u8> {
    let mut frame = vec![0x50, 0x00, 0x00, 0x9D, 0x01, 0x2A];
    frame.extend_from_slice(&width.to_le_bytes());
    frame.extend_from_slice(&height.to_le_bytes());
    frame.resize(size, 0x5A);
    let mut body = b"WEBPVP8 ".to_vec();
    body.extend_from_slice(&(frame.len() as u32).to_le_bytes());
    body.extend_from_slice(&frame);
    let mut out = b"RIFF".to_vec();
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend_from_slice(&body);
    out
}

const MIB: f64 = 1024.0 * 1024.0;

#[test]
fn a_thumbnail_decode_holds_at_most_the_avatar_decode_peak() {
    let mut worst = 0;
    let mut measure = |label: &str, upload: Vec<u8>, mime_type: &str, made: Option<bool>| {
        let (peak, thumbnail) = peak_of(&upload, mime_type);
        eprintln!(
            "thumbnail decode peak: {label}: {:.1} MiB ({})",
            peak as f64 / MIB,
            if thumbnail { "made" } else { "none" }
        );
        if let Some(made) = made {
            assert_eq!(thumbnail, made, "{label}");
        }
        worst = worst.max(peak);
    };

    // JPEG, reduced at decode: a phone photo, the side bound, and a
    // progressive one at the coefficient bound (`avatar`'s bounds).
    measure(
        "sequential JPEG 4032x3024",
        pattern_jpeg(4032, 3024, false),
        "image/jpeg",
        Some(true),
    );
    let side = MAX_JPEG_SIDE as u16;
    measure(
        "sequential JPEG at MAX_JPEG_SIDE square",
        pattern_jpeg(side, side, false),
        "image/jpeg",
        Some(true),
    );
    measure(
        "progressive JPEG 3200x2400",
        pattern_jpeg(3200, 2400, true),
        "image/jpeg",
        Some(true),
    );

    // PNG decoded whole, just under `avatar::MAX_PNG_FRAME_BYTES`.
    let mut png = Vec::new();
    let picture = RgbImage::from_fn(3739, 3739, |x, y| {
        Rgb([(x / 16) as u8, (y / 16) as u8, ((x + y) / 32) as u8])
    });
    image::codecs::png::PngEncoder::new(&mut png)
        .write_image(picture.as_raw(), 3739, 3739, image::ExtendedColorType::Rgb8)
        .unwrap();
    drop(picture);
    measure("PNG 3739x3739 RGB", png, "image/png", Some(true));

    // WebP at `MAX_WEBP_PIXELS` (2048 × 2048): lossless without alpha —
    // decoded into an RGBA buffer, then copied to RGB — and with it.
    assert_eq!(2048 * 2048, MAX_WEBP_PIXELS);
    measure(
        "lossless WebP 2048x2048 RGB",
        lossless_webp(2048, 2048, false),
        "image/webp",
        Some(true),
    );
    measure(
        "lossless WebP 2048x2048 RGBA",
        lossless_webp(2048, 2048, true),
        "image/webp",
        Some(true),
    );
    // Lossy at the bound, its chunk stuffed to the attachment cap: what
    // the decoder allocates on the header and the chunk it reads, before
    // the data fails to decode.
    measure(
        "lossy WebP 2048x2048 stuffed to the cap",
        stuffed_vp8(2048, 2048, MAX_ATTACHMENT_SIZE_BYTES - 64),
        "image/webp",
        None,
    );
    // Past the bound: refused on the header, nothing of its size held.
    measure(
        "lossy WebP 16383x16383 stuffed to the cap",
        stuffed_vp8(16383, 16383, MAX_ATTACHMENT_SIZE_BYTES - 64),
        "image/webp",
        Some(false),
    );

    assert!(worst <= DECODE_PEAK_BYTES, "{worst} > {DECODE_PEAK_BYTES}");
}
