//! The barcode photo of `/stocks/new` (#402): a picture taken through
//! `<input type="file" accept="image/*" capture="environment">`, read here,
//! in memory, and turned into the string a barcode carries. No script, no
//! WebAssembly: the form works as a plain multipart POST.
//!
//! The photo is never written anywhere and never sent on — not to apps/api,
//! not to Open Food Facts. It lives in this process's memory for the length
//! of the request, and only the decoded code goes further — its digits, or
//! the text of a GS1 DataMatrix or QR code (#403) — through the same
//! `GET /stocks/new?scan=…` the "Code-barres" field uses. A photo can show
//! the inside of a home: that is why it goes no further than this module
//! (`docs/registre-traitements.md`, Stocks).
//!
//! Bounds, because one decode costs far more memory than the file it
//! reads: `MAX_PHOTO_BYTES` on the file; decoder limits on the declared
//! dimensions (`MAX_DIMENSION`) and on what decoding allocates
//! (`MAX_ALLOC`), since a small file can claim a huge image — `image`
//! weighs only its output buffer, so what a JPEG needs beyond it, the
//! coefficients of a progressive one, is weighed here from its header
//! (`JpegFrame::decode_bytes`), under the same `MAX_ALLOC`; the picture is
//! turned grey at once and, past `MAX_SIDE`, reduced by an integer box
//! average written here — `image`'s own resize goes through an `Rgba32F`
//! copy, 16 bytes a pixel, which no decoder limit sees; and at most
//! `DECODE_PERMITS` decodes run at once in the process, whatever the upload
//! gate admits.

use std::io::Cursor;
use std::sync::LazyLock;

use image::error::{DecodingError, LimitError, LimitErrorKind};
use image::{DynamicImage, GrayImage, ImageError, ImageFormat, ImageReader, Limits};
use rxing::common::HybridBinarizer;
use rxing::{
    BarcodeFormat, BinaryBitmap, DecodeHints, Luma8LuminanceSource, MultiFormatReader,
    RXingResultMetadataType, RXingResultMetadataValue, Reader,
};
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

/// Bound on what decoding may allocate: a 24 Mpx frame in RGB
/// (6000 × 4000 × 3 = 72 MB) fits; a 48 Mpx one does not, and is refused
/// as unreadable. Phones hand a file input their default resolution, 12 or
/// 24 Mpx, in baseline JPEG. A progressive JPEG also holds two bytes per
/// coefficient: a 12 Mpx one fits at the usual 4:2:0 subsampling (73 MB),
/// not at 4:4:4 (110 MB).
const MAX_ALLOC: u64 = 80 * 1024 * 1024;

/// Decodes running at once in this process. The photo route also holds an
/// upload permit (`manage_our_home_http_guard::UploadGate`), which bounds the
/// bodies held, up to eight; this bounds the far larger working memory of
/// decoding them. One decode is held to `MAX_ALLOC` × 3/2 plus 4 MiB, 124
/// MiB, by `one_decode_stays_within_its_memory_bound`; its heap high-water
/// marks there (2026-10-08, DataMatrix and QR readers included), on the
/// worst inputs each bound admits: grey with alpha PNG, 8192 × 5120,
/// 120.0 MiB; grey PNG, 8192², 98.7 MiB; baseline colour JPEG, 24 Mpx,
/// 91.6 MiB; progressive grey JPEG, 5280², 80.6 MiB; progressive colour
/// JPEG at 4:4:4, 9.3 Mpx, 80.5 MiB;
/// progressive CMYK JPEG, 7 Mpx, 73.8 MiB; progressive colour JPEG at
/// 4:2:0, 12 Mpx, 70.8 MiB. Files past the bounds are refused at their
/// header, under 0.1 MiB. So under 248 MiB for two decodes at once, on top
/// of the bodies the upload gate holds.
pub const DECODE_PERMITS: usize = 2;

/// The process-wide pool of decode permits.
pub static DECODES: LazyLock<Semaphore> = LazyLock::new(|| Semaphore::new(DECODE_PERMITS));

/// Why a photo gave no code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PhotoError {
    /// Over `MAX_PHOTO_BYTES`.
    TooLarge,
    /// Not a JPEG or PNG image this build can read, or past the
    /// decoder bounds.
    NotAnImage,
    /// An image, but no EAN-13, EAN-8, UPC-A, DataMatrix or QR code could
    /// be read in it.
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

/// A JPEG's frame header (SOFn): what decoding it will allocate is read
/// from here, before anything is decoded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JpegFrame {
    pub width: u32,
    pub height: u32,
    /// SOF2, SOF6, SOF10 or SOF14: the image is sent in several scans.
    pub progressive: bool,
    /// Each component's sampling factors, horizontal then vertical.
    pub sampling: Vec<(u32, u32)>,
}

/// The frame header of the JPEG in `bytes`: the first SOFn, past the
/// segments before it. `None` when `bytes` is not a JPEG, or when a scan,
/// the end of the image or the end of the bytes comes first.
pub fn jpeg_frame(bytes: &[u8]) -> Option<JpegFrame> {
    let be16 = |at: usize| Some(u16::from_be_bytes([*bytes.get(at)?, *bytes.get(at + 1)?]));
    if bytes.get(..2)? != [0xFF, 0xD8] {
        return None;
    }
    let mut at = 2;
    loop {
        if *bytes.get(at)? != 0xFF {
            return None;
        }
        // A marker may be preceded by any number of fill bytes.
        while *bytes.get(at)? == 0xFF {
            at += 1;
        }
        let marker = bytes[at];
        at += 1;
        match marker {
            // Standalone markers: no length follows.
            0x01 | 0xD0..=0xD7 => continue,
            // Start of image again, end of image, start of scan.
            0xD8..=0xDA => return None,
            _ => {}
        }
        let length = usize::from(be16(at)?);
        let segment = bytes.get(at + 2..at + length.max(2))?;
        // SOF0 to SOF15, less DHT (C4), JPG (C8) and DAC (CC).
        if (0xC0..=0xCF).contains(&marker) && !matches!(marker, 0xC4 | 0xC8 | 0xCC) {
            let count = usize::from(*segment.get(5)?);
            let sampling = segment
                .get(6..6 + 3 * count)?
                .as_chunks::<3>()
                .0
                .iter()
                .map(|spec| (u32::from(spec[1] >> 4), u32::from(spec[1] & 0x0F)))
                .collect();
            return Some(JpegFrame {
                height: u32::from(be16(at + 3)?),
                width: u32::from(be16(at + 5)?),
                progressive: matches!(marker, 0xC2 | 0xC6 | 0xCA | 0xCE),
                sampling,
            });
        }
        at += length;
    }
}

impl JpegFrame {
    /// What decoding allocates, in bytes: the output, counted at one byte
    /// per component and pixel (`image` turns CMYK into RGB, which is
    /// less); and, for a progressive JPEG, every DCT coefficient of the
    /// image at once, two bytes each, which zune-jpeg (the decoder behind
    /// `image`) keeps from the first scan to the last and no `Limits`
    /// sees. Coefficients are counted on whole MCUs, each component at its
    /// own sampling, as zune-jpeg lays them out.
    pub fn decode_bytes(&self) -> u64 {
        let (width, height) = (u64::from(self.width), u64::from(self.height));
        let output = width * height * self.sampling.len() as u64;
        if !self.progressive {
            return output;
        }
        let h_max = self
            .sampling
            .iter()
            .map(|&(h, _)| h)
            .max()
            .unwrap_or(1)
            .max(1);
        let v_max = self
            .sampling
            .iter()
            .map(|&(_, v)| v)
            .max()
            .unwrap_or(1)
            .max(1);
        let mcus_x = width.div_ceil(8 * u64::from(h_max));
        let mcus_y = height.div_ceil(8 * u64::from(v_max));
        let coefficients: u64 = self
            .sampling
            .iter()
            .map(|&(h, v)| mcus_x * 8 * u64::from(h) * mcus_y * 8 * u64::from(v))
            .sum();
        output + 2 * coefficients
    }
}

/// `bytes` decoded under the bounds: the dimensions a header declares are
/// checked before anything is allocated for them.
fn read_image(bytes: &[u8]) -> image::ImageResult<DynamicImage> {
    let mut reader = ImageReader::new(Cursor::new(bytes)).with_guessed_format()?;
    // `max_alloc` only weighs the output buffer: what a JPEG costs beyond
    // it is weighed here, from its header.
    if reader.format() == Some(ImageFormat::Jpeg) {
        let frame = jpeg_frame(bytes).ok_or_else(|| {
            ImageError::Decoding(DecodingError::new(
                ImageFormat::Jpeg.into(),
                "no frame header",
            ))
        })?;
        if frame.decode_bytes() > MAX_ALLOC {
            return Err(ImageError::Limits(LimitError::from_kind(
                LimitErrorKind::InsufficientMemory,
            )));
        }
    }
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_DIMENSION);
    limits.max_image_height = Some(MAX_DIMENSION);
    limits.max_alloc = Some(MAX_ALLOC);
    reader.limits(limits);
    reader.decode()
}

/// Whether a DataMatrix or QR code is a GS1 one, the only 2D codes passed
/// on: FNC1 in first position, by its symbology identifier (`]d2`, `]d5`
/// for a DataMatrix, `]Q3`, `]Q4` for a QR code), or a GS1 Digital Link,
/// an http(s) URL where a `01` path segment is followed by a GTIN: 8, 12,
/// 13 or 14 digits whose GS1 check digit holds. A `01` that is a month or a
/// page (`/2026/01/galette`) is not one. Reading the elements is apps/api's
/// business (`stocks::gs1`).
pub fn is_gs1_2d(text: &str, symbology: Option<&str>) -> bool {
    if matches!(symbology, Some("]d2" | "]d5" | "]Q3" | "]Q4")) {
        return true;
    }
    let Some((scheme, rest)) = text.split_once("://") else {
        return false;
    };
    if !(scheme.eq_ignore_ascii_case("http") || scheme.eq_ignore_ascii_case("https")) {
        return false;
    }
    let path = rest.split(['?', '#']).next().unwrap_or(rest);
    // The first segment is the host; `01` must be followed by the GTIN.
    let segments: Vec<&str> = path.split('/').skip(1).collect();
    segments
        .windows(2)
        .any(|pair| pair[0] == "01" && is_gtin(pair[1]))
}

/// 8, 12, 13 or 14 digits whose GS1 mod-10 check holds: from the right,
/// weights 1, 3, 1, 3…, the check digit included, the sum a multiple of 10.
fn is_gtin(segment: &str) -> bool {
    matches!(segment.len(), 8 | 12 | 13 | 14)
        && segment.bytes().all(|b| b.is_ascii_digit())
        && segment
            .bytes()
            .rev()
            .enumerate()
            .map(|(i, b)| u32::from(b - b'0') * if i % 2 == 0 { 1 } else { 3 })
            .sum::<u32>()
            .is_multiple_of(10)
}

/// The formats looked for, in two passes over the same picture.
fn hints(formats: &[BarcodeFormat]) -> DecodeHints {
    DecodeHints {
        PossibleFormats: Some(formats.iter().copied().collect()),
        TryHarder: Some(true),
        ..Default::default()
    }
}

/// The text of the code found in `bytes`, decoded in memory: a GS1
/// DataMatrix or QR code first (#403), which may carry an expiry date, else
/// an EAN-13, EAN-8 or UPC-A. Any other 2D code — a brand's QR code, a
/// Wi-Fi one in the background — is passed over, and its text never leaves
/// this function. Blocking and CPU-bound: call it off the async runtime.
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

    // One picture, binarized once, read by both passes.
    let source = Luma8LuminanceSource::new(luma.into_raw(), width, height)
        .map_err(|_| PhotoError::NoBarcode)?;
    let mut bitmap = BinaryBitmap::new(HybridBinarizer::new(source));
    let mut reader = MultiFormatReader::default();
    let two_d = reader.decode_with_hints(
        &mut bitmap,
        &hints(&[BarcodeFormat::DATA_MATRIX, BarcodeFormat::QR_CODE]),
    );
    if let Ok(result) = two_d {
        let symbology = match result
            .getRXingResultMetadata()
            .get(&RXingResultMetadataType::SYMBOLOGY_IDENTIFIER)
        {
            Some(RXingResultMetadataValue::SymbologyIdentifier(id)) => Some(id.as_str()),
            _ => None,
        };
        if is_gs1_2d(result.getText(), symbology) {
            return Ok(result.getText().to_string());
        }
    }
    reader
        .decode_with_hints(
            &mut bitmap,
            &hints(&[
                BarcodeFormat::EAN_13,
                BarcodeFormat::EAN_8,
                BarcodeFormat::UPC_A,
            ]),
        )
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

    /// What one decode may cost: the output `MAX_ALLOC` caps (or, for a
    /// progressive JPEG, the output and its coefficients, which
    /// `JpegFrame::decode_bytes` keeps under it), then the grey copy made
    /// from the output, which lives beside it for a moment: at most half
    /// its size, for a grey picture with alpha. The reduction and the
    /// barcode search work on the grey picture, smaller still.
    const DECODE_BOUND: usize = MAX_ALLOC as usize * 3 / 2 + 4 * MIB;

    /// `frame` as `jpeg-encoder` writes it: progressive or not, at
    /// `sampling`, in `colour` (grey, RGB, or CMYK with white as no ink).
    fn jpeg_of(
        frame: &GrayImage,
        colour: jpeg_encoder::ColorType,
        sampling: jpeg_encoder::SamplingFactor,
        progressive: bool,
    ) -> Vec<u8> {
        use jpeg_encoder::ColorType;
        let pixels: Vec<u8> = match colour {
            ColorType::Luma => frame.as_raw().clone(),
            ColorType::Rgb => frame.as_raw().iter().flat_map(|&g| [g, g, g]).collect(),
            ColorType::Cmyk => frame
                .as_raw()
                .iter()
                .flat_map(|&g| [0, 0, 0, 255 - g])
                .collect(),
            other => unreachable!("{other:?}"),
        };
        let mut out = Vec::new();
        let mut encoder = jpeg_encoder::Encoder::new(&mut out, 90);
        encoder.set_sampling_factor(sampling);
        encoder.set_progressive(progressive);
        let (width, height) = frame.dimensions();
        encoder
            .encode(&pixels, width as u16, height as u16, colour)
            .unwrap();
        out
    }

    /// One decode stays within `DECODE_BOUND`, on the worst inputs each
    /// bound admits. Heap high-water marks measured on 2026-10-08 are in
    /// `DECODE_PERMITS`' comment.
    #[test]
    fn one_decode_stays_within_its_memory_bound() {
        use jpeg_encoder::{ColorType, SamplingFactor};
        let bars = ean13("3017620422003", 6);
        let check = |name: &str, bytes: Vec<u8>, read: bool| {
            let (outcome, peak) = decode_peak(bytes);
            if read {
                assert_eq!(outcome.as_deref(), Ok("3017620422003"), "{name}");
            } else {
                assert_ne!(outcome, Err(PhotoError::NotAnImage), "{name}");
            }
            assert!(peak <= DECODE_BOUND, "{name}: {} MiB", peak / MIB);
        };
        // Baseline colour, 24 Mpx: 72 MB of RGB, the most `MAX_ALLOC`
        // admits, then its 24 MB grey copy.
        check("24 Mpx colour", colour_jpeg(&bars, 6000, 4000), true);
        // Progressive colour, 12 Mpx at 4:2:0, the usual subsampling:
        // 37 MB of RGB and 37 MB of coefficients.
        let frame = framed(&bars, 4032, 3024);
        let jpeg = jpeg_of(&frame, ColorType::Rgb, SamplingFactor::R_4_2_0, true);
        check("12 Mpx progressive 4:2:0", jpeg, true);
        // Progressive colour at 4:4:4, 9 bytes a pixel: 9.3 Mpx.
        let frame = framed(&bars, 3520, 2640);
        let jpeg = jpeg_of(&frame, ColorType::Rgb, SamplingFactor::R_4_4_4, true);
        check("9.3 Mpx progressive 4:4:4", jpeg, true);
        // Progressive CMYK, 12 bytes a pixel: 7 Mpx. Whether a barcode is
        // found in CMYK depends on how its ink is read: only the memory is
        // the point here.
        let frame = framed(&bars, 2640, 2640);
        let jpeg = jpeg_of(&frame, ColorType::Cmyk, SamplingFactor::R_4_4_4, true);
        check("7 Mpx progressive CMYK", jpeg, false);
        // Progressive grey, 3 bytes a pixel: 5280 × 5280.
        let frame = framed(&bars, 5280, 5280);
        let jpeg = jpeg_of(&frame, ColorType::Luma, SamplingFactor::R_4_4_4, true);
        check("5280² progressive grey", jpeg, true);
        // The largest picture `MAX_DIMENSION` admits, 8192 × 8192 grey:
        // 64 MB, then a quarter of it.
        let png = encode(&framed(&bars, 8192, 8192), ImageFormat::Png);
        check("8192² grey PNG", png, true);
        // Grey with alpha, 2 bytes a pixel, at exactly `MAX_ALLOC`: its
        // grey copy is half of it, the largest share.
        let frame = DynamicImage::ImageLuma8(framed(&bars, 8192, 5120)).into_luma_alpha8();
        let mut png = Cursor::new(Vec::new());
        DynamicImage::ImageLumaA8(frame)
            .write_to(&mut png, ImageFormat::Png)
            .unwrap();
        check("8192 × 5120 grey and alpha PNG", png.into_inner(), true);
        // The 12 Mpx phone frame, baseline, read at full resolution.
        let (outcome, peak) = decode_peak(colour_jpeg(&bars, 4032, 3024));
        assert_eq!(outcome.as_deref(), Ok("3017620422003"));
        assert!(peak <= 64 * MIB, "12 Mpx colour: {} MiB", peak / MIB);
    }

    /// Past the bounds, a photo is refused at its header, before its
    /// pixels or its coefficients are allocated. The first is the file
    /// that made the case (#402): an 8000 × 8000 colour JPEG of under
    /// 2 MB, 192 MB of RGB. The progressive ones fit `MAX_ALLOC` by their
    /// output alone, not with their coefficients.
    #[test]
    fn a_photo_past_the_bounds_costs_next_to_nothing() {
        use jpeg_encoder::{ColorType, SamplingFactor};
        let bars = ean13("3017620422003", 6);
        let check = |name: &str, bytes: Vec<u8>| {
            let (outcome, peak) = decode_peak(bytes);
            assert_eq!(outcome, Err(PhotoError::NotAnImage), "{name}");
            assert!(peak <= 4 * MIB, "{name}, refused: {} MiB", peak / MIB);
        };
        check("8000² colour", colour_jpeg(&bars, 8000, 8000));
        for (name, width, height, colour) in [
            ("6000 × 4000 progressive 4:4:4", 6000, 4000, ColorType::Rgb),
            ("5280² progressive 4:4:4", 5280, 5280, ColorType::Rgb),
            ("5280² progressive CMYK", 5280, 5280, ColorType::Cmyk),
            ("8192² progressive grey", 8192, 8192, ColorType::Luma),
            ("4032 × 3024 progressive 4:4:4", 4032, 3024, ColorType::Rgb),
        ] {
            let frame = framed(&bars, width, height);
            check(name, jpeg_of(&frame, colour, SamplingFactor::R_4_4_4, true));
        }
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
    fn a_full_camera_frame_is_read_at_full_resolution() {
        // 4032 × 3024, the frame of a 12 Mpx phone camera; the bars cover
        // about a third of its width. It fits `MAX_SIDE`: not reduced.
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

    /// A 2D code from rxing's writer, `module` pixels per module, with a
    /// quiet zone of four modules.
    fn matrix_code(contents: &str, format: BarcodeFormat, gs1: bool, module: u32) -> GrayImage {
        use rxing::Writer;
        let hints = rxing::EncodeHints {
            Gs1Format: Some(gs1),
            DataMatrixCompact: Some(gs1),
            Margin: Some("0".into()),
            ..Default::default()
        };
        let matrix = rxing::MultiFormatWriter
            .encode_with_hints(contents, &format, 0, 0, &hints)
            .unwrap();
        let (width, height) = (matrix.getWidth(), matrix.getHeight());
        let quiet = 4;
        GrayImage::from_fn(
            (width + 2 * quiet) * module,
            (height + 2 * quiet) * module,
            |x, y| {
                let (mx, my) = (x / module, y / module);
                let inside =
                    (quiet..width + quiet).contains(&mx) && (quiet..height + quiet).contains(&my);
                Luma([if inside && matrix.get(mx - quiet, my - quiet) {
                    0
                } else {
                    255
                }])
            },
        )
    }

    /// (01) GTIN-14, (10) a lot ended by FNC1, (17) an expiry date.
    const GS1_STRING: &str = "010301762042200310LOT-42\u{1d}17270131";

    #[test]
    fn a_gs1_datamatrix_is_read_with_its_separator() {
        let code = matrix_code(GS1_STRING, BarcodeFormat::DATA_MATRIX, true, 12);
        let jpeg = encode(&framed(&code, 1200, 900), ImageFormat::Jpeg);
        // The leading FNC1 is dropped and the one ending the lot comes back
        // as ASCII 29: apps/api reads the elements from there (#403).
        assert_eq!(decode_photo(&jpeg).as_deref(), Ok(GS1_STRING));
    }

    #[test]
    fn a_gs1_digital_link_qr_code_is_read() {
        let url = "https://id.gs1.org/01/03017620422003/10/LOT42?17=270131";
        let code = matrix_code(url, BarcodeFormat::QR_CODE, false, 10);
        let jpeg = encode(&framed(&code, 1200, 900), ImageFormat::Jpeg);
        assert_eq!(decode_photo(&jpeg).as_deref(), Ok(url));
    }

    #[test]
    fn a_2d_code_is_gs1_by_its_symbology_or_as_a_digital_link() {
        // FNC1 in first position: a GS1 DataMatrix (]d2, ]d5 with an ECI),
        // a GS1 QR code (]Q3, ]Q4).
        for symbology in ["]d2", "]d5", "]Q3", "]Q4"] {
            assert!(is_gs1_2d(GS1_STRING, Some(symbology)), "{symbology}");
        }
        // A Digital Link is a plain QR code: its URL names the GTIN (01).
        for url in [
            "https://id.gs1.org/01/03017620422003?17=270131",
            "HTTP://example.com/p/01/3017620422003",
            // GTIN-12 and GTIN-8, with their check digits.
            "https://id.gs1.org/01/036000291452",
            "https://id.gs1.org/01/96385074/10/LOT",
        ] {
            assert!(is_gs1_2d(url, Some("]Q1")), "{url}");
        }
        for (text, symbology) in [
            ("WIFI:T:WPA;S:maison;P:secret;;", Some("]Q1")),
            ("https://example.com/promo?ref=01", Some("]Q1")),
            ("https://example.com/01", Some("]Q1")),
            // `01` as a month or a page, not a GTIN.
            ("https://brand.com/2026/01/galette", Some("]Q1")),
            ("https://brand.com/fr/01/promo", Some("]Q1")),
            ("https://brand.com/fr/01/12345678901", Some("]Q1")),
            // A GTIN's length, a wrong check digit.
            ("https://id.gs1.org/01/03017620422004", Some("]Q1")),
            ("https://id.gs1.org/01/96385073", Some("]Q1")),
            (GS1_STRING, Some("]d1")),
            (GS1_STRING, None),
            ("BEGIN:VCARD", Some("]Q2")),
        ] {
            assert!(!is_gs1_2d(text, symbology), "{text:?} {symbology:?}");
        }
    }

    /// `left` and `right` side by side on a white frame.
    fn side_by_side(left: &GrayImage, right: &GrayImage) -> GrayImage {
        let width = left.width() + right.width() + 200;
        let height = left.height().max(right.height()) + 200;
        let mut frame = GrayImage::from_pixel(width, height, Luma([255]));
        image::imageops::overlay(&mut frame, left, 50, 100);
        image::imageops::overlay(&mut frame, right, i64::from(left.width()) + 150, 100);
        frame
    }

    #[test]
    fn a_gs1_code_wins_over_the_ean_printed_beside_it() {
        let photo = side_by_side(
            &ean13("3017620422003", 4),
            &matrix_code(GS1_STRING, BarcodeFormat::DATA_MATRIX, true, 12),
        );
        let png = encode(&photo, ImageFormat::Png);
        assert_eq!(decode_photo(&png).as_deref(), Ok(GS1_STRING));
    }

    #[test]
    fn a_qr_code_that_is_no_gs1_code_is_passed_over() {
        // Beside an EAN, the EAN is read.
        let promo = matrix_code(
            "https://example.com/promo",
            BarcodeFormat::QR_CODE,
            false,
            10,
        );
        let photo = side_by_side(&ean13("3017620422003", 4), &promo);
        let png = encode(&photo, ImageFormat::Png);
        assert_eq!(decode_photo(&png).as_deref(), Ok("3017620422003"));
        // A dated URL has a `01` segment, but no GTIN after it: the EAN
        // still wins.
        let dated = matrix_code(
            "https://brand.com/2026/01/galette",
            BarcodeFormat::QR_CODE,
            false,
            10,
        );
        let photo = side_by_side(&ean13("3017620422003", 4), &dated);
        let png = encode(&photo, ImageFormat::Png);
        assert_eq!(decode_photo(&png).as_deref(), Ok("3017620422003"));
        // Alone, nothing is: its text never leaves the process.
        let wifi = matrix_code(
            "WIFI:T:WPA;S:maison;P:secret;;",
            BarcodeFormat::QR_CODE,
            false,
            10,
        );
        let png = encode(&framed(&wifi, 1200, 900), ImageFormat::Png);
        assert_eq!(decode_photo(&png), Err(PhotoError::NoBarcode));
    }

    #[test]
    fn a_datamatrix_in_a_full_camera_frame_is_read() {
        let code = matrix_code(GS1_STRING, BarcodeFormat::DATA_MATRIX, true, 24);
        let png = encode(&framed(&code, 4032, 3024), ImageFormat::Png);
        assert_eq!(decode_photo(&png).as_deref(), Ok(GS1_STRING));
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

    /// A JPEG cut after its frame header: SOI, an APP1 the parser must
    /// skip, then SOF`marker` with one component per `(h, v)`, then SOS.
    fn jpeg_header(marker: u8, width: u16, height: u16, sampling: &[(u8, u8)]) -> Vec<u8> {
        let mut out = vec![0xFF, 0xD8, 0xFF, 0xE1, 0x00, 0x06, b'E', b'x', b'i', b'f'];
        out.extend_from_slice(&[0xFF, marker]);
        out.extend_from_slice(&(8 + 3 * sampling.len() as u16).to_be_bytes());
        out.push(8);
        out.extend_from_slice(&height.to_be_bytes());
        out.extend_from_slice(&width.to_be_bytes());
        out.push(sampling.len() as u8);
        for (i, (h, v)) in sampling.iter().enumerate() {
            out.extend_from_slice(&[i as u8 + 1, h << 4 | v, 0]);
        }
        out.extend_from_slice(&[0xFF, 0xDA, 0x00, 0x02]);
        out
    }

    const YCC_420: [(u8, u8); 3] = [(2, 2), (1, 1), (1, 1)];
    const YCC_444: [(u8, u8); 3] = [(1, 1), (1, 1), (1, 1)];

    #[test]
    fn a_baseline_frame_header_is_read_past_the_app_segments() {
        let frame = jpeg_frame(&jpeg_header(0xC0, 4032, 3024, &YCC_420)).unwrap();
        assert_eq!(
            frame,
            JpegFrame {
                width: 4032,
                height: 3024,
                progressive: false,
                sampling: vec![(2, 2), (1, 1), (1, 1)],
            }
        );
    }

    #[test]
    fn every_multi_scan_frame_is_progressive() {
        for marker in [0xC2, 0xC6, 0xCA, 0xCE] {
            assert!(
                jpeg_frame(&jpeg_header(marker, 8, 8, &YCC_444))
                    .unwrap()
                    .progressive
            );
        }
        for marker in [0xC0, 0xC1, 0xC3] {
            assert!(
                !jpeg_frame(&jpeg_header(marker, 8, 8, &YCC_444))
                    .unwrap()
                    .progressive
            );
        }
    }

    #[test]
    fn no_frame_header_is_none() {
        assert_eq!(jpeg_frame(b"not a jpeg"), None);
        assert_eq!(jpeg_frame(&[0xFF, 0xD8]), None);
        // Cut inside the frame header.
        assert_eq!(jpeg_frame(&jpeg_header(0xC2, 8, 8, &YCC_444)[..16]), None);
        // The scan starts before any frame header.
        assert_eq!(jpeg_frame(&[0xFF, 0xD8, 0xFF, 0xDA, 0x00, 0x02]), None);
        // A Huffman table (C4) is not a frame header.
        assert_eq!(
            jpeg_frame(&[0xFF, 0xD8, 0xFF, 0xC4, 0x00, 0x02, 0xFF, 0xD9]),
            None
        );
    }

    #[test]
    fn a_baseline_jpeg_costs_its_output_only() {
        let frame = jpeg_frame(&jpeg_header(0xC0, 6000, 4000, &YCC_444)).unwrap();
        assert_eq!(frame.decode_bytes(), 6000 * 4000 * 3);
    }

    #[test]
    fn a_progressive_jpeg_also_costs_every_coefficient_at_once() {
        // 4:4:4, 6000 × 4000: output 72 MB, plus 2 bytes for each sample of
        // each component, 144 MB.
        let frame = jpeg_frame(&jpeg_header(0xC2, 6000, 4000, &YCC_444)).unwrap();
        assert_eq!(frame.decode_bytes(), 72_000_000 + 144_000_000);
        // 4:2:0, 4032 × 3024: 252 × 189 MCUs of 16 × 16; luma covers them
        // whole (12 192 768 samples), each chroma a quarter (3 048 192).
        let frame = jpeg_frame(&jpeg_header(0xC2, 4032, 3024, &YCC_420)).unwrap();
        assert_eq!(
            frame.decode_bytes(),
            36_578_304 + 2 * (12_192_768 + 2 * 3_048_192)
        );
        // Grey, 8192 × 8192: 64 MiB out, 128 MiB of coefficients.
        let frame = jpeg_frame(&jpeg_header(0xC2, 8192, 8192, &[(1, 1)])).unwrap();
        assert_eq!(frame.decode_bytes(), 3 * 8192 * 8192);
    }

    #[test]
    fn coefficients_are_counted_on_whole_mcus() {
        // 17 × 9, 4:2:0: 2 × 1 MCUs of 16 × 16. Luma 32 × 16, chroma 16 × 8.
        let frame = jpeg_frame(&jpeg_header(0xC2, 17, 9, &YCC_420)).unwrap();
        assert_eq!(
            frame.decode_bytes(),
            17 * 9 * 3 + 2 * (32 * 16 + 2 * 16 * 8)
        );
        // CMYK, four components: counted at one byte each on output.
        let frame = jpeg_frame(&jpeg_header(0xC2, 8, 8, &[(1, 1); 4])).unwrap();
        assert_eq!(frame.decode_bytes(), 8 * 8 * 4 + 2 * 4 * 64);
    }

    #[test]
    fn a_jpeg_whose_decode_would_pass_the_bound_is_refused_at_its_header() {
        // 6000 × 4000 progressive 4:4:4: 216 MB, past `MAX_ALLOC`, though
        // its 72 MB of output is within it.
        let header = jpeg_header(0xC2, 6000, 4000, &YCC_444);
        assert!(
            matches!(read_image(&header), Err(image::ImageError::Limits(_))),
            "{:?}",
            read_image(&header).err()
        );
    }

    #[test]
    fn the_error_codes_are_stable() {
        assert_eq!(PhotoError::TooLarge.code(), "too_large");
        assert_eq!(PhotoError::NotAnImage.code(), "not_an_image");
        assert_eq!(PhotoError::NoBarcode.code(), "unreadable");
    }
}
