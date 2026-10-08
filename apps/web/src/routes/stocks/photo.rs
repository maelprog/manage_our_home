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
//! Bounds: `MAX_PHOTO_BYTES` on the file, decoder limits on the declared
//! dimensions and allocation (a small file can claim a huge image), then
//! the picture is reduced to `MAX_SIDE` on its longest side before the
//! barcode is looked for — enough for the bars of an article held in front
//! of the lens, and a fraction of a full camera frame's work.

use std::io::Cursor;

use image::imageops::FilterType;
use image::{DynamicImage, ImageReader, Limits};
use rxing::{BarcodeFormat, DecodeHints};

/// The largest photo accepted, in bytes. A phone's full-resolution JPEG is a
/// few megabytes; 12 MiB leaves room for the larger sensors.
pub const MAX_PHOTO_BYTES: usize = 12 * 1024 * 1024;

/// The route's body limit: the photo plus the multipart framing, as
/// `MAX_UPLOAD_BODY_BYTES` does for the attachments.
pub const MAX_PHOTO_BODY_BYTES: usize = MAX_PHOTO_BYTES + 64 * 1024;

/// The longest side, in pixels, the photo is reduced to before decoding.
pub const MAX_SIDE: u32 = 2048;

/// Decoder bounds: no dimension past this, whatever the file declares.
const MAX_DIMENSION: u32 = 12_000;

/// Decoder bound on what decoding may allocate (a 48 Mpx frame in RGB is
/// 144 MiB).
const MAX_ALLOC: u64 = 192 * 1024 * 1024;

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

/// `(width, height)` reduced so that the longest side is at most `max`,
/// keeping the proportions; unchanged when it already fits. Never 0.
pub fn fitted_size(width: u32, height: u32, max: u32) -> (u32, u32) {
    let longest = width.max(height);
    if longest <= max {
        return (width, height);
    }
    let scale = |side: u32| ((u64::from(side) * u64::from(max)) / u64::from(longest)).max(1) as u32;
    (scale(width), scale(height))
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
    let image = read_image(bytes).map_err(|_| PhotoError::NotAnImage)?;

    let (width, height) = fitted_size(image.width(), image.height(), MAX_SIDE);
    let image = if (width, height) == (image.width(), image.height()) {
        image
    } else {
        image.resize_exact(width, height, FilterType::Triangle)
    };
    let luma = DynamicImage::into_luma8(image);
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
    fn a_large_photo_is_reduced_to_the_longest_side() {
        assert_eq!(fitted_size(4032, 3024, 2048), (2048, 1536));
        assert_eq!(fitted_size(3024, 4032, 2048), (1536, 2048));
    }

    #[test]
    fn a_photo_that_fits_is_left_as_it_is() {
        assert_eq!(fitted_size(1600, 1200, 2048), (1600, 1200));
        assert_eq!(fitted_size(2048, 10, 2048), (2048, 10));
    }

    #[test]
    fn a_reduced_side_never_falls_to_zero() {
        assert_eq!(fitted_size(100_000, 1, 2048), (2048, 1));
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
