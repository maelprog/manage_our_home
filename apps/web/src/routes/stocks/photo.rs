//! The barcode photo of `/stocks/new` (#402): a picture taken through
//! `<input type="file" accept="image/*" capture="environment">`, read here,
//! in memory, and turned into the string a barcode carries. No script, no
//! WebAssembly: the form works as a plain multipart POST.
//!
//! The photo is never written anywhere and never sent on — not to apps/api,
//! not to Open Food Facts. It lives in this process's memory for the length
//! of the request, and only the decoded digits go further, through the same
//! `GET /stocks/new?scan=…` the "Code-barres" field uses. A photo can show
//! the inside of a home: that is why it goes no further than this module
//! (`docs/registre-traitements.md`, Stocks).
//!
//! Bounds, because one decode costs far more memory than the file it
//! reads: `MAX_PHOTO_BYTES` on the file; decoder limits on the declared
//! dimensions (`MAX_DIMENSION`) and on what decoding allocates
//! (`MAX_ALLOC`), since a small file can claim a huge image; the picture is
//! turned grey at once and, past `MAX_SIDE`, reduced by an integer box
//! average written here — `image`'s own resize goes through an `Rgba32F`
//! copy, 16 bytes a pixel, which no decoder limit sees; and at most
//! `DECODE_PERMITS` decodes run at once in the process, whatever the upload
//! gate admits.

use std::io::Cursor;
use std::sync::LazyLock;

use image::{DynamicImage, GrayImage, ImageReader, Limits};
use rxing::{BarcodeFormat, DecodeHints};
use tokio::sync::Semaphore;

/// The largest photo accepted, in bytes. A phone's full-resolution JPEG is a
/// few megabytes; 12 MiB leaves room for the larger sensors.
pub const MAX_PHOTO_BYTES: usize = 12 * 1024 * 1024;

/// The route's body limit: the photo plus the multipart framing, as
/// `MAX_UPLOAD_BODY_BYTES` does for the attachments.
pub const MAX_PHOTO_BODY_BYTES: usize = MAX_PHOTO_BYTES + 64 * 1024;

/// The longest side, in pixels, the barcode is looked for on. 4096 keeps a
/// 12 Mpx phone frame (4032 × 3024) at full resolution, where a barcode
/// across a third of the width has bars of 3 px or more; a larger frame is
/// reduced by a whole factor (`downscale_factor`).
pub const MAX_SIDE: u32 = 4096;

/// Decoder bound: no dimension past this, whatever the file declares.
const MAX_DIMENSION: u32 = 8192;

/// Decoder bound on what decoding may allocate: a 24 Mpx frame in RGB
/// (6000 × 4000 × 3 = 72 MB) fits; a 48 Mpx one does not, and is refused
/// as unreadable. Phones hand a file input their default resolution, 12 or
/// 24 Mpx.
const MAX_ALLOC: u64 = 80 * 1024 * 1024;

/// Decodes running at once in this process. The photo route also holds an
/// upload permit (`manage_our_home_http_guard::UploadGate`), which bounds the
/// bodies held, up to eight; this bounds the far larger working memory of
/// decoding them. Heap high-water mark of one decode, measured by
/// `one_decode_stays_within_its_memory_bound` (2026-10-08): a 12 Mpx colour
/// JPEG, 46.5 MiB; a 24 Mpx colour JPEG, the largest `MAX_ALLOC` admits,
/// 91.6 MiB; an 8192 × 8192 grey PNG, 80.6 MiB; an 8000 × 8000 colour JPEG,
/// refused at its header, 1.7 MiB. So under 185 MiB for two decodes at
/// once, on top of the bodies the upload gate holds.
pub const DECODE_PERMITS: usize = 2;

/// The process-wide pool of decode permits.
pub static DECODES: LazyLock<Semaphore> = LazyLock::new(|| Semaphore::new(DECODE_PERMITS));

/// Why a photo gave no code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PhotoError {
    /// Over `MAX_PHOTO_BYTES`.
    TooLarge,
    /// Not a JPEG, PNG or WebP image this build can read, or past the
    /// decoder bounds.
    NotAnImage,
    /// An image, but no EAN-13, EAN-8 or UPC-A could be read in it.
    NoBarcode,
}

impl PhotoError {
    /// The code the page carries in `?photo=` after a failed scan.
    pub fn code(self) -> &'static str {
        match self {
            PhotoError::TooLarge => "too_large",
            PhotoError::NotAnImage => "not_an_image",
            PhotoError::NoBarcode => "unreadable",
        }
    }
}

/// The whole factor a `width` × `height` picture is divided by so that its
/// longest side is at most `max`: 1 when it already fits.
pub fn downscale_factor(width: u32, height: u32, max: u32) -> u32 {
    width.max(height).div_ceil(max.max(1)).max(1)
}

/// `image` reduced by `factor`, each output pixel the average of a
/// `factor` × `factor` block (the blocks of the last row and column may be
/// cut short). Grey in, grey out, one byte a pixel: no intermediate copy.
pub fn box_downscale(image: &GrayImage, factor: u32) -> GrayImage {
    let factor = factor.max(1);
    let (width, height) = image.dimensions();
    let (out_w, out_h) = (width.div_ceil(factor), height.div_ceil(factor));
    let pixels = image.as_raw();
    // One running sum per output column, refilled for each row of blocks.
    let mut sums = vec![0u32; out_w as usize];
    let mut out = Vec::with_capacity(out_w as usize * out_h as usize);
    for block_y in 0..out_h {
        sums.iter_mut().for_each(|s| *s = 0);
        let rows = (block_y * factor)..((block_y + 1) * factor).min(height);
        let row_count = rows.len() as u32;
        for y in rows {
            let row = &pixels[(y * width) as usize..((y + 1) * width) as usize];
            for (x, &value) in row.iter().enumerate() {
                sums[x / factor as usize] += u32::from(value);
            }
        }
        for (block_x, &sum) in sums.iter().enumerate() {
            let start = block_x as u32 * factor;
            let cols = (start + factor).min(width) - start;
            let n = cols * row_count;
            out.push(((sum + n / 2) / n) as u8);
        }
    }
    GrayImage::from_raw(out_w, out_h, out).expect("out_w × out_h bytes")
}

/// `bytes` decoded under the bounds: the dimensions a header declares are
/// checked before anything is allocated for them.
fn read_image(bytes: &[u8]) -> image::ImageResult<DynamicImage> {
    let mut reader = ImageReader::new(Cursor::new(bytes)).with_guessed_format()?;
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_DIMENSION);
    limits.max_image_height = Some(MAX_DIMENSION);
    limits.max_alloc = Some(MAX_ALLOC);
    reader.limits(limits);
    reader.decode()
}

/// The text of the first EAN-13, EAN-8 or UPC-A found in `bytes`, decoded
/// in memory. Blocking and CPU-bound: call it off the async runtime.
pub fn decode_photo(bytes: &[u8]) -> Result<String, PhotoError> {
    if bytes.len() > MAX_PHOTO_BYTES {
        return Err(PhotoError::TooLarge);
    }
    // Grey at once: the colour buffer goes as soon as the grey one exists.
    let luma = read_image(bytes)
        .map_err(|_| PhotoError::NotAnImage)?
        .into_luma8();
    let factor = downscale_factor(luma.width(), luma.height(), MAX_SIDE);
    let luma = if factor > 1 {
        box_downscale(&luma, factor)
    } else {
        luma
    };
    let (width, height) = luma.dimensions();

    let mut hints = DecodeHints {
        PossibleFormats: Some(
            [
                BarcodeFormat::EAN_13,
                BarcodeFormat::EAN_8,
                BarcodeFormat::UPC_A,
            ]
            .into(),
        ),
        TryHarder: Some(true),
        ..Default::default()
    };
    rxing::helpers::detect_in_luma_with_hints(luma.into_raw(), width, height, None, &mut hints)
        .map(|result| result.getText().to_string())
        .map_err(|_| PhotoError::NoBarcode)
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{GrayImage, ImageFormat, Luma};

    /// An EAN-13 drawn by hand: start guard, six digits in the L/G parity
    /// the first digit sets, centre guard, six digits in R, end guard, with
    /// quiet zones. `module` pixels per bar.
    fn ean13(digits: &str, module: u32) -> GrayImage {
        const L: [&str; 10] = [
            "0001101", "0011001", "0010011", "0111101", "0100011", "0110001", "0101111", "0111011",
            "0110111", "0001011",
        ];
        const G: [&str; 10] = [
            "0100111", "0110011", "0011011", "0100001", "0011101", "0111001", "0000101", "0010001",
            "0001001", "0010111",
        ];
        const R: [&str; 10] = [
            "1110010", "1100110", "1101100", "1000010", "1011100", "1001110", "1010000", "1000100",
            "1001000", "1110100",
        ];
        const PARITY: [&str; 10] = [
            "LLLLLL", "LLGLGG", "LLGGLG", "LLGGGL", "LGLLGG", "LGGLLG", "LGGGLL", "LGLGLG",
            "LGLGGL", "LGGLGL",
        ];
        let d: Vec<usize> = digits.bytes().map(|b| (b - b'0') as usize).collect();
        let mut bits = String::from("101");
        for i in 1..=6 {
            let set = if PARITY[d[0]].as_bytes()[i - 1] == b'L' {
                L
            } else {
                G
            };
            bits.push_str(set[d[i]]);
        }
        bits.push_str("01010");
        for &digit in &d[7..=12] {
            bits.push_str(R[digit]);
        }
        bits.push_str("101");
        let quiet = 12 * module;
        let width = bits.len() as u32 * module + 2 * quiet;
        let height = 60 * module;
        let mut img = GrayImage::from_pixel(width, height, Luma([255]));
        for (i, bit) in bits.bytes().enumerate() {
            if bit == b'1' {
                for x in 0..module {
                    for y in 5 * module..55 * module {
                        img.put_pixel(quiet + i as u32 * module + x, y, Luma([0]));
                    }
                }
            }
        }
        img
    }

    fn encode(img: &GrayImage, format: ImageFormat) -> Vec<u8> {
        let mut out = Cursor::new(Vec::new());
        DynamicImage::ImageLuma8(img.clone())
            .write_to(&mut out, format)
            .unwrap();
        out.into_inner()
    }

    /// `img` in the middle of a larger white frame, as a photo of an
    /// article shows its label.
    fn framed(img: &GrayImage, width: u32, height: u32) -> GrayImage {
        let mut frame = GrayImage::from_pixel(width, height, Luma([255]));
        image::imageops::overlay(
            &mut frame,
            img,
            i64::from((width - img.width()) / 2),
            i64::from((height - img.height()) / 2),
        );
        frame
    }

    #[test]
    fn a_photo_that_fits_is_not_reduced() {
        assert_eq!(downscale_factor(4032, 3024, 4096), 1);
        assert_eq!(downscale_factor(4096, 4096, 4096), 1);
        assert_eq!(downscale_factor(1, 1, 4096), 1);
    }

    #[test]
    fn a_larger_photo_is_divided_by_the_smallest_whole_factor_that_fits() {
        assert_eq!(downscale_factor(6000, 4000, 4096), 2);
        assert_eq!(downscale_factor(4000, 6000, 4096), 2);
        assert_eq!(downscale_factor(4097, 10, 4096), 2);
        assert_eq!(downscale_factor(8192, 8192, 4096), 2);
        assert_eq!(downscale_factor(8193, 1, 4096), 3);
    }

    #[test]
    fn a_box_downscale_averages_each_block() {
        // 4 × 2, factor 2: two blocks, of 0/100/200/250 and of four 50s.
        let image = GrayImage::from_raw(4, 2, vec![0, 100, 50, 50, 200, 250, 50, 50]).unwrap();
        let small = box_downscale(&image, 2);
        assert_eq!(small.dimensions(), (2, 1));
        assert_eq!(small.into_raw(), vec![138, 50]);
    }

    #[test]
    fn the_last_blocks_of_an_uneven_picture_are_cut_short() {
        // 5 × 3, factor 2: 3 × 2 out; the last column and row average what
        // they have.
        let image = GrayImage::from_fn(5, 3, |x, y| Luma([(10 * x + 100 * y) as u8]));
        let small = box_downscale(&image, 2);
        assert_eq!(small.dimensions(), (3, 2));
        assert_eq!(small.get_pixel(0, 0).0, [55]); // 0, 10, 100, 110
        assert_eq!(small.get_pixel(2, 0).0, [90]); // 40, 140
        assert_eq!(small.get_pixel(0, 1).0, [205]); // 200, 210
        assert_eq!(small.get_pixel(2, 1).0, [240]); // 240 alone
    }

    #[test]
    fn a_factor_of_one_is_the_same_picture() {
        let image = GrayImage::from_fn(3, 2, |x, y| Luma([(x * 7 + y * 13) as u8]));
        assert_eq!(box_downscale(&image, 1), image);
    }

    /// `img` in an RGB frame, as a phone camera hands it over: colour, JPEG.
    fn colour_jpeg(img: &GrayImage, width: u32, height: u32) -> Vec<u8> {
        let frame = DynamicImage::ImageLuma8(framed(img, width, height)).into_rgb8();
        let mut out = Cursor::new(Vec::new());
        DynamicImage::ImageRgb8(frame)
            .write_to(&mut out, ImageFormat::Jpeg)
            .unwrap();
        out.into_inner()
    }

    #[test]
    fn a_12_mpx_colour_frame_with_3_px_bars_is_read() {
        // The case a reduction to 2048 px lost: 3 px a bar in 4032 × 3024.
        let jpeg = colour_jpeg(&ean13("3017620422003", 3), 4032, 3024);
        assert_eq!(decode_photo(&jpeg).as_deref(), Ok("3017620422003"));
    }

    #[test]
    fn a_24_mpx_colour_frame_is_reduced_and_read() {
        // 6000 × 4000, divided by 2: 6 px bars become 3.
        let jpeg = colour_jpeg(&ean13("3017620422003", 6), 6000, 4000);
        assert_eq!(decode_photo(&jpeg).as_deref(), Ok("3017620422003"));
    }

    #[test]
    fn a_48_mpx_colour_frame_is_refused_by_the_allocation_bound() {
        // 8000 × 6000 × 3 bytes = 144 MB, past `MAX_ALLOC`, though within
        // `MAX_DIMENSION`: refused before its pixels are allocated.
        let jpeg = colour_jpeg(&GrayImage::from_pixel(10, 10, Luma([0])), 8000, 6000);
        assert!(
            matches!(read_image(&jpeg), Err(image::ImageError::Limits(_))),
            "{:?}",
            read_image(&jpeg).err()
        );
    }

    /// The heap high-water mark of one `decode_photo(bytes)`, counted on a
    /// thread of its own, over what was live when it began. `bytes` is
    /// built before counting starts: the file is the body's cost, which
    /// the upload gate bounds, not the decode's.
    fn decode_peak(bytes: Vec<u8>) -> (Result<String, PhotoError>, usize) {
        let _measuring = crate::heap_count::exclusive();
        std::thread::spawn(move || {
            crate::heap_count::count_this_thread();
            let baseline = crate::heap_count::start();
            let outcome = decode_photo(&bytes);
            let peak = usize::try_from(crate::heap_count::peak() - baseline).unwrap();
            println!(
                "decode of {} bytes: heap peak {:.1} MiB",
                bytes.len(),
                peak as f64 / MIB as f64
            );
            (outcome, peak)
        })
        .join()
        .unwrap()
    }

    const MIB: usize = 1024 * 1024;

    /// What one decode costs at most, measured on the worst inputs the
    /// bounds admit (heap high-water marks on 2026-10-08: 91.6, 80.6 and
    /// 46.5 MiB, in the order below). The colour buffer the decoder fills
    /// is capped by `MAX_ALLOC`; the grey copy made from it lives beside it
    /// for a moment, a third of its size; the reduction and the barcode
    /// search work on the grey picture, a quarter of it once reduced.
    #[test]
    fn one_decode_stays_within_its_memory_bound() {
        let bars = ean13("3017620422003", 6);
        let bound = (MAX_ALLOC as usize) * 4 / 3 + 4 * MIB;
        // The largest colour frame `MAX_ALLOC` admits: 24 Mpx, 72 MB in
        // RGB, plus its 24 MB grey copy.
        let (outcome, peak) = decode_peak(colour_jpeg(&bars, 6000, 4000));
        assert_eq!(outcome.as_deref(), Ok("3017620422003"));
        assert!(peak <= bound, "24 Mpx colour: {} MiB", peak / MIB);
        // The largest picture `MAX_DIMENSION` admits, 8192 × 8192 grey:
        // 64 MB, then a quarter of it.
        let (outcome, peak) = decode_peak(encode(&framed(&bars, 8192, 8192), ImageFormat::Png));
        assert_eq!(outcome.as_deref(), Ok("3017620422003"));
        assert!(peak <= bound, "8192² grey: {} MiB", peak / MIB);
        // The 12 Mpx phone frame, read at full resolution.
        let (outcome, peak) = decode_peak(colour_jpeg(&bars, 4032, 3024));
        assert_eq!(outcome.as_deref(), Ok("3017620422003"));
        assert!(peak <= 64 * MIB, "12 Mpx colour: {} MiB", peak / MIB);
    }

    /// The file that made the case (#402): an 8000 × 8000 colour JPEG of
    /// under 2 MB, which `image`'s `resize_exact`, through its `Rgba32F`
    /// copy, turned into hundreds of MiB. Its 192 MB of RGB is past
    /// `MAX_ALLOC`: refused at its header, before its pixels are allocated
    /// (1.7 MiB measured on 2026-10-08).
    #[test]
    fn a_photo_past_the_allocation_bound_costs_next_to_nothing() {
        let bars = ean13("3017620422003", 6);
        let (outcome, peak) = decode_peak(colour_jpeg(&bars, 8000, 8000));
        assert_eq!(outcome, Err(PhotoError::NotAnImage));
        assert!(peak <= 4 * MIB, "8000² colour, refused: {} MiB", peak / MIB);
    }

    #[test]
    fn an_ean_13_is_read_from_a_png() {
        let png = encode(&ean13("3017620422003", 3), ImageFormat::Png);
        assert_eq!(decode_photo(&png).as_deref(), Ok("3017620422003"));
    }

    #[test]
    fn an_ean_13_is_read_from_a_jpeg() {
        let jpeg = encode(
            &framed(&ean13("3017620422003", 4), 1200, 900),
            ImageFormat::Jpeg,
        );
        assert_eq!(decode_photo(&jpeg).as_deref(), Ok("3017620422003"));
    }

    #[test]
    fn a_full_camera_frame_is_reduced_and_still_read() {
        // 4032 × 3024, the frame of a 12 Mpx phone camera; the bars cover
        // about a third of its width.
        let frame = framed(&ean13("3017620422003", 12), 4032, 3024);
        let png = encode(&frame, ImageFormat::Png);
        assert_eq!(decode_photo(&png).as_deref(), Ok("3017620422003"));
    }

    #[test]
    fn a_barcode_held_sideways_is_read() {
        let turned = image::imageops::rotate90(&framed(&ean13("3017620422003", 4), 1000, 700));
        let png = encode(&turned, ImageFormat::Png);
        assert_eq!(decode_photo(&png).as_deref(), Ok("3017620422003"));
    }

    #[test]
    fn a_photo_without_a_barcode_is_unreadable() {
        let blank = GrayImage::from_pixel(800, 600, Luma([255]));
        assert_eq!(
            decode_photo(&encode(&blank, ImageFormat::Png)),
            Err(PhotoError::NoBarcode)
        );
    }

    #[test]
    fn bytes_that_are_no_image_are_refused() {
        assert_eq!(
            decode_photo(b"not an image at all"),
            Err(PhotoError::NotAnImage)
        );
        assert_eq!(decode_photo(b""), Err(PhotoError::NotAnImage));
    }

    #[test]
    fn a_photo_over_the_limit_is_refused_before_decoding() {
        let big = vec![0u8; MAX_PHOTO_BYTES + 1];
        assert_eq!(decode_photo(&big), Err(PhotoError::TooLarge));
    }

    #[test]
    fn an_image_declaring_huge_dimensions_is_refused() {
        // A PNG header claiming 20 000 × 20 000 pixels: past the decoder
        // bounds, refused without allocating them.
        fn crc32(bytes: &[u8]) -> u32 {
            let mut crc = 0xffff_ffffu32;
            for &b in bytes {
                crc ^= u32::from(b);
                for _ in 0..8 {
                    crc = if crc & 1 == 1 {
                        (crc >> 1) ^ 0xedb8_8320
                    } else {
                        crc >> 1
                    };
                }
            }
            !crc
        }
        let mut ihdr = b"IHDR".to_vec();
        ihdr.extend_from_slice(&20_000u32.to_be_bytes());
        ihdr.extend_from_slice(&20_000u32.to_be_bytes());
        ihdr.extend_from_slice(&[8, 0, 0, 0, 0]); // 8-bit greyscale
        let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
        png.extend_from_slice(&13u32.to_be_bytes());
        png.extend_from_slice(&ihdr);
        png.extend_from_slice(&crc32(&ihdr).to_be_bytes());
        // Refused for its dimensions, at the header — not for the missing
        // pixel data that would follow.
        assert!(
            matches!(read_image(&png), Err(image::ImageError::Limits(_))),
            "{:?}",
            read_image(&png).err()
        );
        assert_eq!(decode_photo(&png), Err(PhotoError::NotAnImage));
    }

    #[test]
    fn the_error_codes_are_stable() {
        assert_eq!(PhotoError::TooLarge.code(), "too_large");
        assert_eq!(PhotoError::NotAnImage.code(), "not_an_image");
        assert_eq!(PhotoError::NoBarcode.code(), "unreadable");
    }
}
