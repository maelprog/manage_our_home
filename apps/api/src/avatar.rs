//! Profile photo (#467): `PUT /me/avatar`, `DELETE /me/avatar`,
//! `GET /me/avatar` and `GET /groups/:id/members/:user_id/avatar`.
//!
//! The upload is never stored as sent. Its type is sniffed from its bytes
//! (JPEG, PNG or WebP, whatever its name says), it is decoded under bounds —
//! a JPEG reduced in the decode itself, so that a 50 Mpx phone photo is
//! accepted without ever being held at full size (#588) —, the orientation its EXIF records is applied, its centred square is
//! reduced to `AVATAR_SIDE` pixels, and that square is encoded afresh as a
//! JPEG. The new file carries no metadata at all: EXIF, GPS position, camera
//! model and the like go with the original, which is never written anywhere.
//!
//! The stored object sits under `avatars/{user_id}/{uuid}`, named by
//! `users.avatar_key` (0032). A new upload gets a new key; the old object is
//! deleted once the new key is committed. Whatever escapes that — a failed
//! delete, a dropped request — is found by the daily reconciliation pass
//! (`attachment_reconcile`), which knows the avatar keys as well as the
//! attachment ones.
//!
//! Reading is proxied: the bytes come through this API, not through a
//! presigned MinIO link, because the pages' CSP (`infra/Caddyfile`) admits
//! images from their own origin only (`default-src 'self'`, no `img-src`).

use std::io::Cursor;
use std::sync::LazyLock;

use axum::extract::{Multipart, Path, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::IntoResponse;
use image::codecs::jpeg::JpegEncoder;
use image::metadata::Orientation;
use image::{
    imageops, DynamicImage, GenericImageView, GrayImage, ImageDecoder, ImageError, ImageFormat,
    ImageReader, Limits, Rgb, RgbImage,
};
use jpeg_decoder::PixelFormat;
use tokio::sync::Semaphore;
use uuid::Uuid;

use crate::auth::session::{scoped_tx, AuthUser};
use crate::error::{AppError, AppResult};
use crate::storage::UPLOAD_FRAMING_MARGIN_BYTES;
use crate::AppState;

/// The largest photo accepted, in bytes. A phone's JPEG in its 48/50 Mpx
/// mode weighs about 15 to 30 MB (a Galaxy S25 Ultra's 50 Mpx JPEG, 21 MB);
/// 32 MiB admits them (arbitrage of 2026-10-23, #588). The services that
/// cap an avatar at 2 to 10 MB shrink the photo in the client before
/// sending it, which apps/web does not.
pub const MAX_AVATAR_BYTES: usize = 32 * 1024 * 1024;

/// The route's body limit: the photo plus the multipart framing, as
/// `storage::MAX_UPLOAD_BODY_BYTES` does for the attachments. Above the cap
/// on purpose, so that a file past it is answered `file_too_large` by the
/// handler rather than cut by the limit.
pub const MAX_AVATAR_BODY_BYTES: usize = MAX_AVATAR_BYTES + UPLOAD_FRAMING_MARGIN_BYTES;

/// Side, in pixels, of the stored square.
pub const AVATAR_SIDE: u32 = 256;

/// The stored object's type: every avatar is re-encoded to it.
pub const AVATAR_CONTENT_TYPE: &str = "image/jpeg";

const AVATAR_JPEG_QUALITY: u8 = 85;

/// Bound of `image`'s whole decode (PNG, WebP): no dimension past this,
/// whatever the file declares.
const MAX_DIMENSION: u32 = 8192;

/// Photos decoded at once in this process: one (arbitrage of 2026-10-23,
/// #588). The permit is taken before the body is read and held to the end
/// of the decode, so that one photo body at most is in memory, with its
/// decode. Another photo upload waits for it, its body unread.
///
/// Worst case of the api's memory, against the pod's limit of 256 MiB on
/// k3s (`apps/manage-our-home/base/api.yaml` on the `k3s` branch), in MiB
/// (1 MiB = 1 048 576 bytes):
///
/// - the process at rest: 7 to 8 MiB (`kubectl top`, PR of #431);
/// - 7 attachment bodies, the rest of the upload gate's 8 permits
///   (`GLOBAL_UPLOADS`, shared with the photos): 7 × (20.06 MiB of
///   `storage::MAX_UPLOAD_BODY_BYTES` + about 0.6 MiB of connection buffer,
///   measured in #249) = 144.6 MiB;
/// - the one photo body: 32.06 MiB of `MAX_AVATAR_BODY_BYTES` + 0.6 MiB =
///   32.7 MiB;
/// - its decode: `DECODE_PEAK_BYTES`, 50 MiB.
///
/// 8 + 144.6 + 32.7 + 50 = 235.3 MiB, 20.7 MiB under the limit for the
/// rest of the process. A photo upload waiting for the permit holds a gate
/// permit and its connection buffer only, less than an attachment body.
const DECODE_PERMITS: usize = 1;

/// What one decode holds at its peak, in bytes, at most: the upload's
/// bytes aside, the decoders' buffers and the frame — 50 MiB (arbitrage
/// of 2026-10-23, #588). Each path has its bound, checked on the file's
/// header and chunks before any decode — the frame's dimensions, the
/// metadata a decoder would inflate or copy — and refused past it. Peaks
/// counted by `tests/avatar_memory.rs`, which fails past this bound, at
/// each bound, in debug and release builds on 7 cores and on a single one
/// (`--cpuset-cpus 0`); the ranges are over three such series of four
/// runs:
///
/// - sequential JPEG, reduced at decode (`MAX_JPEG_SIDE`): 8000 × 6000,
///   3.2 to 8.1 MiB in 4:2:0, 4.3 to 16.2 MiB in 4:4:4, 5.7 to 16.4 MiB
///   in CMYK; 16384 × 16384 in 4:2:0, 18.0 to 25.6 MiB;
/// - progressive JPEG (`MAX_PROGRESSIVE_COEFFICIENT_BYTES`), at the bound:
///   4:2:0 3200 × 2400, 22.5 to 29.2 MiB, 22.5 to 27.9 MiB with ICC
///   segments before the first scan filling the file; 4:4:4 2240 × 1712,
///   23.3 to 30.0 MiB; greyscale 3900 × 2940, 22.6 to 34.3 MiB (37.0 MiB
///   on one core in the review of #589); CMYK 1952 × 1468, 23.3 to
///   28.1 MiB. With ICC segments past the first scan the file is refused
///   (`scan_jpeg_header`), its walk allocating nothing: 0.0 MiB;
/// - PNG (`MAX_PNG_FRAME_BYTES`), RGB 3739 × 3739: 40.4 MiB, 41.4 MiB with
///   an iCCP profile of `MAX_METADATA_BYTES` once inflated; a profile
///   inflating to 100 MiB is refused at 0.1 MiB;
/// - WebP (`MAX_WEBP_PIXELS`), 2097 × 2097: lossy with an alpha channel
///   44.1 MiB, the largest, also with an EXIF chunk of
///   `MAX_METADATA_BYTES`; lossless RGB 29.4 MiB, lossless RGBA 17.0 MiB,
///   lossy RGB 20.4 MiB. An animated WebP is refused, and so is a frame
///   other than its canvas (`webp_frame`): 0.0 MiB for a VP8 or VP8L
///   frame of 16383 × 16383 under a canvas of 100 × 100.
///
/// The JPEG figures vary with scheduling: past a reduced frame 128 pixels
/// wide, `jpeg_decoder` hands its rows to a thread per component through a
/// queue it does not bound, and on a single core rows pile up — the high
/// ends above. For a progressive JPEG that queue is bounded by the
/// coefficient buffer it copies, hence a bound of half the budget; for a
/// sequential one it is measured only. Before #588, the comment here
/// counted about 235 MB per decode and about 470 MB for the two permits
/// (review of #467).
pub const DECODE_PEAK_BYTES: usize = 50 * 1024 * 1024;

static DECODES: LazyLock<Semaphore> = LazyLock::new(|| Semaphore::new(DECODE_PERMITS));

/// Why an upload is refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AvatarError {
    /// Over `MAX_AVATAR_BYTES`.
    TooLarge,
    /// Not a JPEG, PNG or WebP by its bytes.
    UnsupportedType,
    /// Of an accepted type, but not decodable.
    Unreadable,
    /// Decodable, but past the bounds a decode is allowed (#588).
    ImageTooLarge,
}

impl AvatarError {
    /// The code of the 422 answer. The first two are the attachment
    /// upload's.
    pub fn code(self) -> &'static str {
        match self {
            AvatarError::TooLarge => "file_too_large",
            AvatarError::UnsupportedType => "unsupported_file_type",
            AvatarError::Unreadable => "unreadable_image",
            AvatarError::ImageTooLarge => "image_too_large",
        }
    }
}

impl From<AvatarError> for AppError {
    fn from(e: AvatarError) -> Self {
        AppError::Unprocessable(e.code().into())
    }
}

/// The format of an accepted upload: the size first, then the type sniffed
/// from the bytes. The filename and the declared type are never looked at.
pub fn sniff_avatar_format(bytes: &[u8]) -> Result<ImageFormat, AvatarError> {
    if bytes.len() > MAX_AVATAR_BYTES {
        return Err(AvatarError::TooLarge);
    }
    match infer::get(bytes).map(|kind| kind.mime_type()) {
        Some("image/jpeg") => Ok(ImageFormat::Jpeg),
        Some("image/png") => Ok(ImageFormat::Png),
        Some("image/webp") => Ok(ImageFormat::WebP),
        _ => Err(AvatarError::UnsupportedType),
    }
}

/// The centred square of a `width` × `height` picture, as `(x, y, side)`.
/// An odd margin rounds down, the extra pixel left on the far side.
pub fn center_square(width: u32, height: u32) -> (u32, u32, u32) {
    let side = width.min(height);
    ((width - side) / 2, (height - side) / 2, side)
}

/// Bound on either side of a JPEG (#588). Its decode is reduced in the
/// inverse DCT, so its frame follows the reduced size, not the photo's:
/// 16384 × 16384 comes out at 2048 × 2048. What this bound keeps short is
/// the decode's time, and the rows `jpeg_decoder` may queue for its
/// threads (see `DECODE_PEAK_BYTES`).
pub const MAX_JPEG_SIDE: u32 = 16384;

/// Bound on a progressive JPEG's coefficient buffer, in bytes, as
/// `progressive_coefficient_bytes` reckons it from the header. Every
/// coefficient of a progressive picture is held until its last scan, at
/// full size whatever the reduction, and `jpeg_decoder` copies them row by
/// row to its threads at the end: up to twice the buffer. 22 MiB admits
/// 7.7 Mpx in 4:2:0 (3200 × 2400, 23.0 MB), 3.8 Mpx in 4:4:4, 11.5 Mpx in
/// greyscale, 2.9 Mpx in CMYK. A phone's camera writes sequential JPEGs,
/// which this bound does not touch.
pub const MAX_PROGRESSIVE_COEFFICIENT_BYTES: u64 = 22 * 1024 * 1024;

/// Bound on a WebP's pixels. `image` decodes it whole, and its decoder
/// holds about 10.5 bytes a pixel at its peak for a lossy picture with an
/// alpha channel, the costliest still WebP (70.2 MiB at 2645 × 2645 in
/// the review of #589): 4.4 Mpx, a square of 2097, keeps it within the
/// decode's budget (`DECODE_PEAK_BYTES`). An animated WebP is refused
/// (`webp_is_animated`).
pub const MAX_WEBP_PIXELS: u64 = 4_400_000;

/// Bound on a PNG's decoded frame, in bytes, as its header declares it
/// (`ImageDecoder::total_bytes`): `image` decodes it whole, the frame,
/// its metadata (`MAX_METADATA_BYTES`) and little else. 40 MiB is
/// 3739 × 3739 in 8-bit RGB, 3238 × 3238 in RGBA.
/// Also handed to `image` as `Limits::max_alloc`, for what its decoders
/// allocate besides the frame.
pub const MAX_PNG_FRAME_BYTES: u64 = 40 * 1024 * 1024;

/// How a JPEG is coded, as its SOF marker says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JpegCoding {
    /// Baseline or extended sequential: decoded row by row, reduced.
    Sequential,
    /// Progressive: every coefficient held until the last scan.
    Progressive,
    /// Lossless: refused, `jpeg_decoder` cannot reduce it.
    Lossless,
}

/// A JPEG's frame, from its SOF segment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JpegFrame {
    pub width: u16,
    pub height: u16,
    pub coding: JpegCoding,
    /// Horizontal and vertical sampling factors of each component.
    pub sampling: Vec<(u8, u8)>,
}

/// What `scan_jpeg_header` finds before the first scan.
pub struct JpegHeader<'a> {
    /// The file with the APP2 segments of its header left out, as slices
    /// to read in a row: `jpeg_decoder` keeps every ICC segment it reads,
    /// and a file filled with them would add its whole size to the decode.
    /// The avatar applies no colour profile, so nothing is lost. An APP2
    /// past the first scan refuses the file (`icc_past`).
    pub kept: Vec<&'a [u8]>,
    pub frame: JpegFrame,
}

/// Walks a JPEG's markers up to its first scan: the frame its SOF
/// declares, and the file without its ICC profile.
pub fn scan_jpeg_header(bytes: &[u8]) -> Result<JpegHeader<'_>, AvatarError> {
    const UNREADABLE: AvatarError = AvatarError::Unreadable;
    if !bytes.starts_with(&[0xFF, 0xD8]) {
        return Err(UNREADABLE);
    }
    let mut kept = Vec::new();
    let mut kept_from = 0;
    let mut frame = None;
    let mut pos = 2;
    loop {
        let marker = loop {
            match bytes.get(pos..pos + 2) {
                // Fill bytes before a marker.
                Some(&[0xFF, 0xFF]) => pos += 1,
                Some(&[0xFF, marker]) => break marker,
                _ => return Err(UNREADABLE),
            }
        };
        match marker {
            // Standalone markers: no length.
            0x01 | 0xD0..=0xD7 => {
                pos += 2;
                continue;
            }
            // An end with no scan at all.
            0xD9 => break,
            // Start of scan: the header is over, but `jpeg_decoder` reads
            // the segments between scans as well.
            0xDA => {
                if icc_past(bytes, pos) {
                    return Err(AvatarError::ImageTooLarge);
                }
                break;
            }
            _ => {}
        }
        let length = match bytes.get(pos + 2..pos + 4) {
            Some(&[high, low]) => usize::from(u16::from_be_bytes([high, low])),
            _ => return Err(UNREADABLE),
        };
        let end = pos + 2 + length;
        if length < 2 || end > bytes.len() {
            return Err(UNREADABLE);
        }
        match marker {
            0xE2 => {
                kept.push(&bytes[kept_from..pos]);
                kept_from = end;
            }
            // SOF0 to SOF15, but DHT (C4), JPG (C8) and DAC (CC).
            0xC0..=0xCF if !matches!(marker, 0xC4 | 0xC8 | 0xCC) => {
                frame = Some(parse_sof(marker, &bytes[pos + 4..end])?);
            }
            _ => {}
        }
        pos = end;
    }
    kept.push(&bytes[kept_from..]);
    Ok(JpegHeader {
        kept,
        frame: frame.ok_or(UNREADABLE)?,
    })
}

/// Whether an APP2 segment comes after `pos`, the first SOS marker:
/// `jpeg_decoder` keeps every ICC segment it reads, between scans and
/// before the end marker too, where `scan_jpeg_header` cannot leave them
/// out without walking the scans' data for the decoder. So the file is
/// refused instead; a camera writes its profile before the first scan.
/// In the entropy-coded data, a 0xFF is followed by a stuffed 0x00 or a
/// restart marker, never by a segment's marker.
fn icc_past(bytes: &[u8], mut pos: usize) -> bool {
    while pos + 1 < bytes.len() {
        if bytes[pos] != 0xFF {
            pos += 1;
            continue;
        }
        match bytes[pos + 1] {
            // Stuffed byte, fill byte, standalone markers.
            0x00 | 0xFF | 0x01 | 0xD0..=0xD7 => pos += 1,
            0xD9 => return false,
            0xE2 => return true,
            // A segment with a length — for a SOS, its header; its data
            // follows.
            _ => match bytes.get(pos + 2..pos + 4) {
                Some(&[high, low]) => pos += 2 + usize::from(u16::from_be_bytes([high, low])),
                _ => return false,
            },
        }
    }
    false
}

fn parse_sof(marker: u8, segment: &[u8]) -> Result<JpegFrame, AvatarError> {
    let unreadable = Err(AvatarError::Unreadable);
    let Some(&[_precision, h1, h0, w1, w0, count]) = segment.get(..6) else {
        return unreadable;
    };
    let count = usize::from(count);
    let Some(components) = segment.get(6..6 + 3 * count) else {
        return unreadable;
    };
    let sampling: Vec<(u8, u8)> = components
        .as_chunks::<3>()
        .0
        .iter()
        .map(|c| (c[1] >> 4, c[1] & 0x0F))
        .collect();
    if sampling.is_empty()
        || sampling
            .iter()
            .any(|&(h, v)| !(1..=4).contains(&h) || !(1..=4).contains(&v))
    {
        return unreadable;
    }
    let coding = match marker {
        0xC2 | 0xC6 | 0xCA | 0xCE => JpegCoding::Progressive,
        0xC3 | 0xC7 | 0xCB | 0xCF => JpegCoding::Lossless,
        _ => JpegCoding::Sequential,
    };
    Ok(JpegFrame {
        width: u16::from_be_bytes([w1, w0]),
        height: u16::from_be_bytes([h1, h0]),
        coding,
        sampling,
    })
}

/// The size handed to `jpeg_decoder::Decoder::scale`, which picks the
/// smallest scale (1/8, 1/4, 1/2 or 1) reaching the request on *either*
/// axis. The short side is asked for `AVATAR_SIDE` and the long one for
/// more than any reduced side can reach, so that the short side decides:
/// the centred square is never decoded smaller than the avatar.
pub fn jpeg_scale_request(width: u16, height: u16) -> (u16, u16) {
    let side = AVATAR_SIDE as u16;
    if width <= height {
        (side, u16::MAX)
    } else {
        (u16::MAX, side)
    }
}

/// The coefficient buffer `jpeg_decoder` allocates for a progressive JPEG:
/// 64 coefficients of 2 bytes per 8 × 8 block of each component, every
/// component padded to whole MCUs, as its `block_size` is.
pub fn progressive_coefficient_bytes(frame: &JpegFrame) -> u64 {
    let h_max = frame.sampling.iter().map(|&(h, _)| u64::from(h)).max();
    let v_max = frame.sampling.iter().map(|&(_, v)| u64::from(v)).max();
    let (Some(h_max), Some(v_max)) = (h_max, v_max) else {
        return 0;
    };
    let mcus_x = u64::from(frame.width).div_ceil(8 * h_max);
    let mcus_y = u64::from(frame.height).div_ceil(8 * v_max);
    frame
        .sampling
        .iter()
        .map(|&(h, v)| mcus_x * u64::from(h) * mcus_y * u64::from(v) * 64 * 2)
        .sum()
}

/// The bounds of a JPEG, checked on what its header declares, before
/// anything of its size is allocated.
pub fn check_jpeg_bounds(frame: &JpegFrame) -> Result<(), AvatarError> {
    if frame.coding == JpegCoding::Lossless {
        return Err(AvatarError::Unreadable);
    }
    if u32::from(frame.width) > MAX_JPEG_SIDE || u32::from(frame.height) > MAX_JPEG_SIDE {
        return Err(AvatarError::ImageTooLarge);
    }
    if frame.coding == JpegCoding::Progressive
        && progressive_coefficient_bytes(frame) > MAX_PROGRESSIVE_COEFFICIENT_BYTES
    {
        return Err(AvatarError::ImageTooLarge);
    }
    Ok(())
}

/// The bounds of `image`'s whole decode, on what the header declares.
pub fn check_whole_bounds(
    format: ImageFormat,
    width: u32,
    height: u32,
    total_bytes: u64,
) -> Result<(), AvatarError> {
    let past = match format {
        ImageFormat::WebP => u64::from(width) * u64::from(height) > MAX_WEBP_PIXELS,
        _ => total_bytes > MAX_PNG_FRAME_BYTES,
    };
    if past {
        Err(AvatarError::ImageTooLarge)
    } else {
        Ok(())
    }
}

/// The JPEG's colours from `jpeg_decoder`'s CMYK, which it hands over as
/// ink: no ink is 0, full ink 255. Each RGB channel is what its ink and
/// the black leave of the white.
fn cmyk_to_rgb(cmyk: &[u8]) -> Vec<u8> {
    cmyk.as_chunks::<4>()
        .0
        .iter()
        .flat_map(|p| {
            let k = 255 - u16::from(p[3]);
            let leave = |c: u8| ((255 - u16::from(c)) * k / 255) as u8;
            [leave(p[0]), leave(p[1]), leave(p[2])]
        })
        .collect()
}

/// The slices of `JpegHeader::kept`, read in a row.
struct Pieces<'a> {
    pieces: std::vec::IntoIter<&'a [u8]>,
    current: &'a [u8],
}

impl std::io::Read for Pieces<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        while self.current.is_empty() {
            match self.pieces.next() {
                Some(next) => self.current = next,
                None => return Ok(0),
            }
        }
        self.current.read(buf)
    }
}

/// A decoded picture and the orientation its EXIF records.
type Decoded = (DynamicImage, Option<Orientation>);

/// A JPEG decoded reduced (#588): `jpeg_decoder` scales it in the inverse
/// DCT to the smallest frame whose short side still holds `AVATAR_SIDE`,
/// so an 8000 × 6000 photo is decoded straight to 1000 × 750 and never
/// exists at full size. Greyscale, colour and CMYK alike; the bounds of
/// `check_jpeg_bounds` first, on the header.
pub(crate) fn decode_jpeg(bytes: &[u8]) -> Result<Decoded, AvatarError> {
    let header = scan_jpeg_header(bytes)?;
    check_jpeg_bounds(&header.frame)?;
    let mut pieces = header.kept.into_iter();
    let current = pieces.next().unwrap_or_default();
    let mut decoder = jpeg_decoder::Decoder::new(Pieces { pieces, current });
    decoder.read_info().map_err(|_| AvatarError::Unreadable)?;
    let info = decoder.info().ok_or(AvatarError::Unreadable)?;

    let (request_width, request_height) = jpeg_scale_request(info.width, info.height);
    let (width, height) = decoder
        .scale(request_width, request_height)
        .map_err(|_| AvatarError::Unreadable)?;
    // Unreachable once reduced under `MAX_JPEG_SIDE`: a second guard.
    decoder.set_max_decoding_buffer_size(MAX_PNG_FRAME_BYTES as usize);
    let pixels = decoder.decode().map_err(|_| AvatarError::Unreadable)?;
    let orientation = decoder.exif_data().and_then(Orientation::from_exif_chunk);

    let (width, height) = (u32::from(width), u32::from(height));
    let picture = match info.pixel_format {
        PixelFormat::L8 => GrayImage::from_raw(width, height, pixels).map(DynamicImage::ImageLuma8),
        PixelFormat::RGB24 => {
            RgbImage::from_raw(width, height, pixels).map(DynamicImage::ImageRgb8)
        }
        PixelFormat::CMYK32 => {
            RgbImage::from_raw(width, height, cmyk_to_rgb(&pixels)).map(DynamicImage::ImageRgb8)
        }
        // More than 8 bits: only lossless, refused above.
        PixelFormat::L16 => None,
    };
    Ok((picture.ok_or(AvatarError::Unreadable)?, orientation))
}

/// Bound on the metadata a whole decode holds besides the picture, in
/// bytes (review of #589): a PNG's iCCP profile once inflated — `png`
/// inflates it whatever the caller asks — and its eXIf chunk, which it
/// copies; a WebP's EXIF chunk, which `image` reads whole for the
/// orientation. A real profile is a few kilobytes (Display P3, about half
/// a kilobyte), a camera's EXIF some tens. Past it: `image_too_large`.
pub const MAX_METADATA_BYTES: usize = 1024 * 1024;

/// A WebP's frame, as its VP8 or VP8L chunk declares it, and the bound on
/// its EXIF chunk. `image-webp` decodes a frame at its own dimensions and
/// only then compares them with a VP8X canvas: a frame other than its
/// canvas is refused as unreadable here, before any decode, and the pixel
/// bound applies to the frame.
pub fn webp_frame(bytes: &[u8]) -> Result<(u32, u32), AvatarError> {
    const UNREADABLE: AvatarError = AvatarError::Unreadable;
    if bytes.get(..4) != Some(b"RIFF") || bytes.get(8..12) != Some(b"WEBP") {
        return Err(UNREADABLE);
    }
    let le24 = |b: &[u8]| u32::from(b[0]) | u32::from(b[1]) << 8 | u32::from(b[2]) << 16;
    let mut canvas = None;
    let mut frame = None;
    let mut exif = 0;
    let mut pos = 12;
    while let Some(header) = bytes.get(pos..pos + 8) {
        let size = u32::from_le_bytes([header[4], header[5], header[6], header[7]]) as usize;
        let data = bytes.get(pos + 8..pos + 8 + size).ok_or(UNREADABLE)?;
        match &header[..4] {
            b"VP8X" => {
                let canvas_bytes = data.get(4..10).ok_or(UNREADABLE)?;
                canvas = Some((le24(&canvas_bytes[..3]) + 1, le24(&canvas_bytes[3..]) + 1));
            }
            b"VP8 " if frame.is_none() => {
                // Frame tag (3 bytes), start code, then 14 bits a side.
                let Some(&[0x9D, 0x01, 0x2A, w0, w1, h0, h1]) = data.get(3..10) else {
                    return Err(UNREADABLE);
                };
                let side = |low: u8, high: u8| u32::from(u16::from_le_bytes([low, high]) & 0x3FFF);
                frame = Some((side(w0, w1), side(h0, h1)));
            }
            b"VP8L" if frame.is_none() => {
                // Signature, then 14 bits a side, minus one.
                let Some(&[0x2F, b0, b1, b2, b3]) = data.get(..5) else {
                    return Err(UNREADABLE);
                };
                let bits = u32::from_le_bytes([b0, b1, b2, b3]);
                frame = Some(((bits & 0x3FFF) + 1, (bits >> 14 & 0x3FFF) + 1));
            }
            b"EXIF" => exif += size,
            _ => {}
        }
        pos += 8 + size + size % 2;
    }
    if exif > MAX_METADATA_BYTES {
        return Err(AvatarError::ImageTooLarge);
    }
    let frame = frame.ok_or(UNREADABLE)?;
    match canvas {
        Some(canvas) if canvas != frame => Err(UNREADABLE),
        _ => Ok(frame),
    }
}

/// The bound on a PNG's metadata (`MAX_METADATA_BYTES`), on its chunks:
/// the eXIf chunk's size, the iCCP profile's once inflated — counted in
/// a sink, never held. A chunk cut short or a broken profile is left to
/// `png`, which refuses the one and ignores the other.
fn check_png_metadata(bytes: &[u8]) -> Result<(), AvatarError> {
    use std::io::Read;
    let mut pos = 8;
    while let Some(header) = bytes.get(pos..pos + 8) {
        let length = u32::from_be_bytes([header[0], header[1], header[2], header[3]]) as usize;
        let Some(data) = bytes.get(pos + 8..pos + 8 + length) else {
            return Ok(());
        };
        match &header[4..] {
            b"eXIf" if length > MAX_METADATA_BYTES => return Err(AvatarError::ImageTooLarge),
            b"iCCP" => {
                // A name of 1 to 79 bytes, its NUL, the compression method.
                if let Some(nul) = data.iter().take(81).position(|&b| b == 0) {
                    let deflated = data.get(nul + 2..).unwrap_or_default();
                    let mut inflated = flate2::read::ZlibDecoder::new(deflated)
                        .take(MAX_METADATA_BYTES as u64 + 1);
                    let size = std::io::copy(&mut inflated, &mut std::io::sink()).unwrap_or(0);
                    if size > MAX_METADATA_BYTES as u64 {
                        return Err(AvatarError::ImageTooLarge);
                    }
                }
            }
            b"IEND" => break,
            _ => {}
        }
        pos += 12 + length;
    }
    Ok(())
}

/// Whether a WebP is animated: the animation flag of its VP8X chunk, the
/// first one of the file when there is one. A simple WebP (a VP8 or VP8L
/// chunk alone) is a still image.
pub fn webp_is_animated(bytes: &[u8]) -> bool {
    bytes.get(12..16) == Some(b"VP8X") && bytes.get(20).is_some_and(|flags| flags & 0x02 != 0)
}

/// A PNG or a WebP, decoded whole by `image` under `MAX_DIMENSION`,
/// `check_whole_bounds` and `MAX_METADATA_BYTES`.
fn decode_whole(bytes: &[u8], format: ImageFormat) -> Result<Decoded, AvatarError> {
    // A bound passed is `ImageError::Limits`; anything else is unreadable.
    let refusal = |e: ImageError| match e {
        ImageError::Limits(_) => AvatarError::ImageTooLarge,
        _ => AvatarError::Unreadable,
    };
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_DIMENSION);
    limits.max_image_height = Some(MAX_DIMENSION);
    limits.max_alloc = Some(MAX_PNG_FRAME_BYTES);
    // The bounds a decoder would only check once it has allocated: on
    // the chunks first.
    match format {
        ImageFormat::WebP => {
            // Not a photo, and its decoder holds frames on top of frames.
            if webp_is_animated(bytes) {
                return Err(AvatarError::UnsupportedType);
            }
            let (width, height) = webp_frame(bytes)?;
            if width > MAX_DIMENSION || height > MAX_DIMENSION {
                return Err(AvatarError::ImageTooLarge);
            }
            check_whole_bounds(format, width, height, 0)?;
        }
        _ => check_png_metadata(bytes)?,
    }
    let mut reader = ImageReader::with_format(Cursor::new(bytes), format);
    reader.limits(limits);
    let mut decoder = reader.into_decoder().map_err(refusal)?;
    let (width, height) = decoder.dimensions();
    check_whole_bounds(format, width, height, decoder.total_bytes())?;
    // An unreadable orientation is no reason to refuse the photo: it is
    // then stored as its pixels lie.
    let orientation = decoder.orientation().ok();
    let picture = DynamicImage::from_decoder(decoder).map_err(refusal)?;
    Ok((picture, orientation))
}

/// The JPEG stored for an upload: see the module comment. CPU-bound, run
/// off the async workers by the handler.
pub fn reencode_avatar(bytes: &[u8]) -> Result<Vec<u8>, AvatarError> {
    let format = sniff_avatar_format(bytes)?;
    let (picture, orientation) = match format {
        ImageFormat::Jpeg => decode_jpeg(bytes)?,
        _ => decode_whole(bytes, format)?,
    };

    let (x, y, side) = center_square(picture.width(), picture.height());
    // `thumbnail` averages integer blocks straight from the view: no copy
    // of the crop, and no floating-point buffer of the source size.
    let square = imageops::thumbnail(&*picture.view(x, y, side, side), AVATAR_SIDE, AVATAR_SIDE);
    drop(picture);

    // Transparency laid on white: JPEG has no alpha, and dropping the
    // channel would leave whatever colour the transparent pixels hold,
    // black as often as not.
    let flat = RgbImage::from_fn(AVATAR_SIDE, AVATAR_SIDE, |px, py| {
        let [r, g, b, a] = square.get_pixel(px, py).0;
        let alpha = u16::from(a);
        let lay = |c: u8| ((u16::from(c) * alpha + 255 * (255 - alpha)) / 255) as u8;
        Rgb([lay(r), lay(g), lay(b)])
    });

    // Applied to the square rather than to the picture: the centred square
    // of a turned picture is the turned centred square, and turning 256 px
    // costs nothing next to turning the whole frame.
    let mut out = DynamicImage::ImageRgb8(flat);
    if let Some(orientation) = orientation {
        out.apply_orientation(orientation);
    }

    let mut jpeg = Vec::new();
    JpegEncoder::new_with_quality(&mut jpeg, AVATAR_JPEG_QUALITY)
        .encode_image(&out)
        .map_err(|_| AvatarError::Unreadable)?;
    Ok(jpeg)
}

/// `PUT /me/avatar` — sets or replaces the caller's photo: one `file` field
/// of `multipart/form-data`, answered 204. Refusals: 422 `file_too_large`,
/// `unsupported_file_type`, `unreadable_image` or `image_too_large`
/// (`AvatarError`), 400 for a body that is not one `file` field, 503 when
/// the upload gate is full.
///
/// The body is read with no transaction open, as `upload_attachment` does
/// (#216), and reading stops at the first byte past `MAX_AVATAR_BYTES`.
/// Nothing is written before the photo has been re-encoded: a refused
/// upload stores nothing.
///
/// Then the `users` row is locked (401 if the account was purged since the
/// session was checked), the new key written, the object put and the
/// transaction committed, in that order: a storage failure rolls the
/// key back, and the previous photo stays. The previous object is deleted
/// after the commit; if that fails, it is an orphan the reconciliation pass
/// removes.
pub async fn put_avatar(
    State(state): State<AppState>,
    auth: AuthUser,
    headers: HeaderMap,
    mut multipart: Multipart,
) -> AppResult<impl IntoResponse> {
    let _upload_permit = state
        .upload_gate
        .try_acquire(auth.user_id)
        .map_err(|busy| {
            tracing::info!(?busy, "avatar upload turned away");
            AppError::UploadsBusy
        })?;
    // Before the body is read: see `DECODE_PERMITS`.
    let _decode = DECODES
        .acquire()
        .await
        .map_err(|e| AppError::Internal(e.into()))?;

    let mut bytes = None;
    while let Some(mut field) = multipart.next_field().await.map_err(multipart_error)? {
        if field.name() == Some("file") {
            if bytes.is_some() {
                return Err(AppError::BadRequest("multiple_files".into()));
            }
            let mut file = Vec::with_capacity(declared_length(&headers).min(MAX_AVATAR_BYTES));
            while let Some(chunk) = field.chunk().await.map_err(multipart_error)? {
                if file.len() + chunk.len() > MAX_AVATAR_BYTES {
                    return Err(AvatarError::TooLarge.into());
                }
                file.extend_from_slice(&chunk);
            }
            bytes = Some(file);
        }
    }
    drop(multipart);
    let bytes = bytes.ok_or(AppError::BadRequest("missing_file".into()))?;

    let jpeg = tokio::task::spawn_blocking(move || reencode_avatar(&bytes))
        .await
        .map_err(|e| AppError::Internal(e.into()))??;
    drop(_decode);

    let key = format!("avatars/{}/{}", auth.user_id, Uuid::new_v4());
    let mut tx = crate::db::begin(&state.db).await?;
    // `deleted_at IS NULL`: a request authenticated before the account's
    // purge, and reaching this lock after it, must not write a key on the
    // anonymised row — the photo would outlive the purge. It is answered
    // 401, as the session it came with now would be.
    let previous = sqlx::query_scalar!(
        "SELECT avatar_key FROM users WHERE id = $1 AND deleted_at IS NULL FOR UPDATE",
        auth.user_id
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(AppError::Unauthorized)?;
    sqlx::query!(
        "UPDATE users SET avatar_key = $2 WHERE id = $1 AND deleted_at IS NULL",
        auth.user_id,
        key
    )
    .execute(&mut *tx)
    .await?;
    state
        .storage
        .put_object(&key, jpeg.into(), AVATAR_CONTENT_TYPE)
        .await
        .map_err(AppError::Internal)?;
    tx.commit().await?;

    if let Some(previous) = previous {
        if let Err(error) = state.storage.delete_objects(&[previous]).await {
            tracing::warn!(
                ?error,
                "replaced avatar object not deleted, left to reconcile"
            );
        }
    }

    Ok(StatusCode::NO_CONTENT)
}

/// `DELETE /me/avatar` — removes the caller's photo, 204, also when there
/// was none. The object goes first, with the row locked, then the key: a
/// storage failure answers 500 and leaves the photo in place for a retry,
/// rather than a key cleared over an object nothing points at any more
/// (the order `agenda::attachments::delete_objects` follows, #54).
pub async fn delete_avatar(
    State(state): State<AppState>,
    auth: AuthUser,
) -> AppResult<impl IntoResponse> {
    let mut tx = crate::db::begin(&state.db).await?;
    let key = sqlx::query_scalar!(
        "SELECT avatar_key FROM users WHERE id = $1 FOR UPDATE",
        auth.user_id
    )
    .fetch_one(&mut *tx)
    .await?;
    if let Some(key) = key {
        state
            .storage
            .delete_objects(&[key])
            .await
            .map_err(AppError::Internal)?;
        sqlx::query!(
            "UPDATE users SET avatar_key = NULL WHERE id = $1",
            auth.user_id
        )
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `GET /me/avatar` — the caller's own photo, 404 when there is none.
pub async fn get_own_avatar(
    State(state): State<AppState>,
    auth: AuthUser,
) -> AppResult<impl IntoResponse> {
    let key = sqlx::query_scalar!("SELECT avatar_key FROM users WHERE id = $1", auth.user_id)
        .fetch_one(&state.db)
        .await?
        .ok_or(AppError::NotFound)?;
    serve(&state, &key).await
}

/// `GET /groups/:id/members/:user_id/avatar` — a member's photo, under the
/// rule of `groups::get_member` (#466): the caller and the target must both
/// be members of `:id`, both checks written in the query. Every refusal is
/// 404, like a member with no photo, so the answer never tells who belongs
/// to which group.
pub async fn get_member_avatar(
    State(state): State<AppState>,
    auth: AuthUser,
    Path((group_id, user_id)): Path<(Uuid, Uuid)>,
) -> AppResult<impl IntoResponse> {
    let mut tx = scoped_tx(&state.db, group_id, auth.user_id).await?;
    let key = sqlx::query_scalar!(
        r#"
        SELECT u.avatar_key
        FROM group_members me
        JOIN group_members gm ON gm.group_id = me.group_id AND gm.user_id = $3
        JOIN users u ON u.id = gm.user_id
        WHERE me.group_id = $1 AND me.user_id = $2
        "#,
        group_id,
        auth.user_id,
        user_id
    )
    .fetch_optional(&mut *tx)
    .await?
    .flatten()
    .ok_or(AppError::NotFound)?;
    tx.commit().await?;
    serve(&state, &key).await
}

/// The photo's bytes. `private`: a shared cache must not keep a response
/// only some members may read; `no-cache`: a replaced photo shows at once.
async fn serve(state: &AppState, key: &str) -> AppResult<impl IntoResponse> {
    let bytes = state
        .storage
        .get_object(key)
        .await
        .map_err(AppError::Internal)?;
    Ok((
        [
            (header::CONTENT_TYPE, AVATAR_CONTENT_TYPE),
            (header::CACHE_CONTROL, "private, no-cache"),
        ],
        bytes,
    ))
}

/// A body cut by the route's limit is a file too large, not a malformed
/// body: the limit sits above the cap, so only a file past the cap reaches
/// it — in one go when the body arrives as a single frame, before the
/// handler's own count of the bytes could answer.
fn multipart_error(e: axum::extract::multipart::MultipartError) -> AppError {
    if e.status() == StatusCode::PAYLOAD_TOO_LARGE {
        AvatarError::TooLarge.into()
    } else {
        AppError::BadRequest("invalid_multipart".into())
    }
}

/// The body length the client declares, to size the file buffer — capped
/// by the caller, so a client declaring a gigabyte gets no more than the
/// cap. Without a usable `Content-Length`, the cap.
fn declared_length(headers: &HeaderMap) -> usize {
    headers
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(MAX_AVATAR_BYTES)
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::codecs::jpeg::JpegEncoder;
    use image::codecs::png::PngEncoder;
    use image::codecs::webp::WebPEncoder;
    use image::{ImageEncoder, Rgb, RgbImage, Rgba, RgbaImage};
    use jpeg_encoder::SamplingFactor;

    const RED: Rgb<u8> = Rgb([220, 20, 20]);
    const BLUE: Rgb<u8> = Rgb([20, 20, 220]);

    /// `width` × `height`, red on the left half, blue on the right.
    fn halves(width: u32, height: u32) -> RgbImage {
        RgbImage::from_fn(width, height, |x, _| if x < width / 2 { RED } else { BLUE })
    }

    fn jpeg(image: &RgbImage) -> Vec<u8> {
        let mut out = Vec::new();
        JpegEncoder::new_with_quality(&mut out, 95)
            .encode_image(image)
            .unwrap();
        out
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

    /// An EXIF block as a phone writes one: an orientation, and a GPS IFD
    /// holding a latitude reference and a marker string.
    fn exif(orientation: u16) -> Vec<u8> {
        let mut tiff = Vec::new();
        tiff.extend_from_slice(b"II*\0");
        tiff.extend_from_slice(&8u32.to_le_bytes());
        // IFD0: two entries, then the next-IFD offset.
        tiff.extend_from_slice(&2u16.to_le_bytes());
        tiff.extend_from_slice(&0x0112u16.to_le_bytes());
        tiff.extend_from_slice(&3u16.to_le_bytes());
        tiff.extend_from_slice(&1u32.to_le_bytes());
        tiff.extend_from_slice(&orientation.to_le_bytes());
        tiff.extend_from_slice(&[0, 0]);
        let gps_offset: u32 = 8 + 2 + 2 * 12 + 4;
        tiff.extend_from_slice(&0x8825u16.to_le_bytes());
        tiff.extend_from_slice(&4u16.to_le_bytes());
        tiff.extend_from_slice(&1u32.to_le_bytes());
        tiff.extend_from_slice(&gps_offset.to_le_bytes());
        tiff.extend_from_slice(&0u32.to_le_bytes());
        // GPS IFD: GPSLatitudeRef "N", GPSMapDatum pointing at a marker.
        let marker = b"GPS-MARKER-48.8566N\0";
        let marker_offset: u32 = gps_offset + 2 + 2 * 12 + 4;
        tiff.extend_from_slice(&2u16.to_le_bytes());
        tiff.extend_from_slice(&0x0001u16.to_le_bytes());
        tiff.extend_from_slice(&2u16.to_le_bytes());
        tiff.extend_from_slice(&2u32.to_le_bytes());
        tiff.extend_from_slice(b"N\0\0\0");
        tiff.extend_from_slice(&0x0012u16.to_le_bytes());
        tiff.extend_from_slice(&2u16.to_le_bytes());
        tiff.extend_from_slice(&(marker.len() as u32).to_le_bytes());
        tiff.extend_from_slice(&marker_offset.to_le_bytes());
        tiff.extend_from_slice(&0u32.to_le_bytes());
        tiff.extend_from_slice(marker);
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

    fn contains(haystack: &[u8], needle: &[u8]) -> bool {
        haystack.windows(needle.len()).any(|w| w == needle)
    }

    fn decoded(bytes: &[u8]) -> RgbImage {
        image::load_from_memory_with_format(bytes, ImageFormat::Jpeg)
            .expect("the stored avatar is a JPEG")
            .to_rgb8()
    }

    fn is_red(p: Rgb<u8>) -> bool {
        p[0] > 150 && p[2] < 100
    }

    fn is_blue(p: Rgb<u8>) -> bool {
        p[2] > 150 && p[0] < 100
    }

    // --- validation ---

    #[test]
    fn jpeg_png_and_webp_are_accepted() {
        let image = halves(8, 8);
        assert_eq!(sniff_avatar_format(&jpeg(&image)), Ok(ImageFormat::Jpeg));
        assert_eq!(sniff_avatar_format(&png(&image)), Ok(ImageFormat::Png));
        let mut webp = Vec::new();
        WebPEncoder::new_lossless(&mut webp)
            .write_image(image.as_raw(), 8, 8, image::ExtendedColorType::Rgb8)
            .unwrap();
        assert_eq!(sniff_avatar_format(&webp), Ok(ImageFormat::WebP));
    }

    /// Criterion of #467: a PDF renamed `.jpg` is refused. The name never
    /// reaches this function; the bytes are what is sniffed.
    #[test]
    fn a_pdf_is_refused_whatever_its_name() {
        let pdf = b"%PDF-1.7\n1 0 obj << /Type /Catalog >> endobj\ntrailer\n%%EOF\n";
        assert_eq!(sniff_avatar_format(pdf), Err(AvatarError::UnsupportedType));
        assert_eq!(reencode_avatar(pdf), Err(AvatarError::UnsupportedType));
    }

    #[test]
    fn a_gif_or_unknown_bytes_are_refused() {
        assert_eq!(
            sniff_avatar_format(b"GIF89a\x01\x00\x01\x00\x00\x00\x00;"),
            Err(AvatarError::UnsupportedType)
        );
        assert_eq!(
            sniff_avatar_format(b"just some text"),
            Err(AvatarError::UnsupportedType)
        );
        assert_eq!(sniff_avatar_format(b""), Err(AvatarError::UnsupportedType));
    }

    /// The size is checked before the content: a file past the cap is
    /// refused as too large, whatever it holds.
    #[test]
    fn a_file_past_the_cap_is_too_large() {
        let mut big = jpeg(&halves(8, 8));
        big.resize(MAX_AVATAR_BYTES + 1, 0);
        assert_eq!(sniff_avatar_format(&big), Err(AvatarError::TooLarge));
        assert_eq!(reencode_avatar(&big), Err(AvatarError::TooLarge));

        let mut at_cap = jpeg(&halves(8, 8));
        at_cap.resize(MAX_AVATAR_BYTES, 0);
        assert_eq!(sniff_avatar_format(&at_cap), Ok(ImageFormat::Jpeg));
    }

    /// Arbitrage of 2026-10-23 (#588): a 48/50 Mpx phone JPEG, 15 to
    /// 30 MB, fits under the cap.
    #[test]
    fn the_cap_is_32_mib() {
        assert_eq!(MAX_AVATAR_BYTES, 32 * 1024 * 1024);
    }

    /// A valid JPEG at exactly the cap — padded past its end marker — is
    /// re-encoded; one byte more is refused as too large.
    #[test]
    fn a_photo_at_the_cap_is_accepted() {
        let mut at_cap = jpeg(&halves(64, 64));
        at_cap.resize(MAX_AVATAR_BYTES, 0);
        assert!(reencode_avatar(&at_cap).is_ok());
        at_cap.push(0);
        assert_eq!(reencode_avatar(&at_cap), Err(AvatarError::TooLarge));
    }

    #[test]
    fn a_truncated_image_is_unreadable() {
        let whole = png(&halves(64, 64));
        assert_eq!(
            reencode_avatar(&whole[..whole.len() / 2]),
            Err(AvatarError::Unreadable)
        );
    }

    /// A small file can declare a huge picture: refused at its header,
    /// before anything of its size is allocated, with a code of its own —
    /// the picture is readable, it is too large (#588).
    #[test]
    fn a_picture_past_the_decoder_bounds_is_too_large() {
        let wide = RgbImage::new(10_000, 1);
        assert_eq!(
            reencode_avatar(&png(&wide)),
            Err(AvatarError::ImageTooLarge)
        );
    }

    /// Within the dimension bound, but past `MAX_PNG_FRAME_BYTES` once decoded: a
    /// 4096 × 4096 RGBA PNG of 16 bits per channel compresses to little,
    /// and decodes to 4096 × 4096 × 8 = 134 MB. Refused before decoding.
    #[test]
    fn a_picture_past_the_allocation_bound_is_too_large() {
        let deep = DynamicImage::ImageRgba16(image::ImageBuffer::new(4096, 4096));
        let mut bytes = Vec::new();
        deep.write_to(&mut Cursor::new(&mut bytes), ImageFormat::Png)
            .unwrap();
        assert!(bytes.len() < MAX_AVATAR_BYTES, "{}", bytes.len());
        assert_eq!(reencode_avatar(&bytes), Err(AvatarError::ImageTooLarge));
    }

    #[test]
    fn each_refusal_has_its_own_code() {
        assert_eq!(AvatarError::TooLarge.code(), "file_too_large");
        assert_eq!(AvatarError::UnsupportedType.code(), "unsupported_file_type");
        assert_eq!(AvatarError::Unreadable.code(), "unreadable_image");
        assert_eq!(AvatarError::ImageTooLarge.code(), "image_too_large");
    }

    // --- JPEG reduced at decode (#588) ---

    /// `jpeg-encoder`'s JPEG of `image`, for what `image`'s encoder does
    /// not write: progressive scans, chosen chroma sampling.
    fn jpeg_with(image: &RgbImage, progressive: bool, sampling: SamplingFactor) -> Vec<u8> {
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

    /// Criterion of #588: a photo of a 48/50 Mpx phone mode, 8000 × 6000,
    /// is accepted and comes out the usual square, its middle kept.
    #[test]
    fn a_50_mpx_jpeg_is_accepted() {
        let out = decoded(&reencode_avatar(&jpeg(&halves(8000, 6000))).unwrap());
        assert_eq!(out.dimensions(), (AVATAR_SIDE, AVATAR_SIDE));
        assert!(is_red(*out.get_pixel(AVATAR_SIDE / 4, AVATAR_SIDE / 2)));
        assert!(is_blue(
            *out.get_pixel(3 * AVATAR_SIDE / 4, AVATAR_SIDE / 2)
        ));
    }

    /// Criterion of #588: past the bound, the refusal says so — not
    /// `unreadable_image`. A strip past `MAX_JPEG_SIDE` costs little to
    /// encode and is refused at its header.
    #[test]
    fn a_jpeg_past_the_side_bound_is_too_large() {
        let strip = halves(MAX_JPEG_SIDE + 8, 8);
        assert_eq!(
            reencode_avatar(&jpeg(&strip)),
            Err(AvatarError::ImageTooLarge)
        );
        let at_bound = halves(MAX_JPEG_SIDE, 8);
        assert!(reencode_avatar(&jpeg(&at_bound)).is_ok());
    }

    /// The request handed to `jpeg_decoder::Decoder::scale`, which picks
    /// the smallest scale reaching it on either axis: the short side is
    /// asked for the avatar's side, the long one for more than any JPEG
    /// can reach, so that the short side decides.
    #[test]
    fn the_scale_is_decided_by_the_short_side() {
        let side = AVATAR_SIDE as u16;
        assert_eq!(jpeg_scale_request(8000, 6000), (u16::MAX, side));
        assert_eq!(jpeg_scale_request(6000, 8000), (side, u16::MAX));
        assert_eq!(jpeg_scale_request(512, 512), (side, u16::MAX));
    }

    /// The decoded frame is the smallest scale keeping the short side at
    /// `AVATAR_SIDE` or more: 1/8 of 8000 × 6000, 1/2 of a panorama whose
    /// short side is 600, the full size of a picture already small.
    #[test]
    fn a_jpeg_is_decoded_at_the_smallest_scale_that_keeps_the_avatar_side() {
        let dims = |w, h| {
            let (picture, _) = decode_jpeg(&jpeg(&halves(w, h))).unwrap();
            picture.dimensions()
        };
        assert_eq!(dims(8000, 6000), (1000, 750));
        assert_eq!(dims(6000, 600), (3000, 300));
        assert_eq!(dims(300, 200), (300, 200));
    }

    /// `JpegFrame` of `width` × `height` with the given sampling factors.
    fn frame(width: u16, height: u16, coding: JpegCoding, sampling: &[(u8, u8)]) -> JpegFrame {
        JpegFrame {
            width,
            height,
            coding,
            sampling: sampling.to_vec(),
        }
    }

    const S420: &[(u8, u8)] = &[(2, 2), (1, 1), (1, 1)];
    const S444: &[(u8, u8)] = &[(1, 1), (1, 1), (1, 1)];
    const GREY: &[(u8, u8)] = &[(1, 1)];
    const CMYK444: &[(u8, u8)] = &[(1, 1), (1, 1), (1, 1), (1, 1)];

    /// The coefficient buffer `jpeg_decoder` allocates for a progressive
    /// picture: 64 coefficients of 2 bytes per block, each component padded
    /// to whole MCUs.
    #[test]
    fn the_progressive_buffer_follows_the_sampling() {
        let prog = JpegCoding::Progressive;
        // 4:2:0, one MCU: 2 × 2 luma blocks and one block per chroma.
        assert_eq!(
            progressive_coefficient_bytes(&frame(16, 16, prog, S420)),
            6 * 128
        );
        // One pixel past it: two MCUs across.
        assert_eq!(
            progressive_coefficient_bytes(&frame(17, 16, prog, S420)),
            12 * 128
        );
        assert_eq!(
            progressive_coefficient_bytes(&frame(8, 8, prog, S444)),
            3 * 128
        );
        assert_eq!(
            progressive_coefficient_bytes(&frame(9, 8, prog, GREY)),
            2 * 128
        );
        assert_eq!(
            progressive_coefficient_bytes(&frame(8, 8, prog, CMYK444)),
            4 * 128
        );
    }

    #[test]
    fn the_jpeg_bounds() {
        let seq = JpegCoding::Sequential;
        let prog = JpegCoding::Progressive;
        let side = MAX_JPEG_SIDE as u16;
        // Sequential: the side bound alone, in colour, grey or CMYK.
        for sampling in [S420, S444, GREY, CMYK444] {
            assert_eq!(check_jpeg_bounds(&frame(8000, 6000, seq, sampling)), Ok(()));
            assert_eq!(check_jpeg_bounds(&frame(side, side, seq, sampling)), Ok(()));
            assert_eq!(
                check_jpeg_bounds(&frame(side + 1, 8, seq, sampling)),
                Err(AvatarError::ImageTooLarge)
            );
            assert_eq!(
                check_jpeg_bounds(&frame(8, side + 1, seq, sampling)),
                Err(AvatarError::ImageTooLarge)
            );
        }
        // Progressive: the coefficient buffer. 3200 × 2400 in 4:2:0 (7.7 Mpx,
        // 23.0 MB) passes, 4000 × 3000 does not; 4:4:4 doubles the buffer,
        // CMYK more; greyscale holds two thirds of 4:2:0.
        assert_eq!(check_jpeg_bounds(&frame(3200, 2400, prog, S420)), Ok(()));
        assert_eq!(
            check_jpeg_bounds(&frame(4000, 3000, prog, S420)),
            Err(AvatarError::ImageTooLarge)
        );
        assert_eq!(
            check_jpeg_bounds(&frame(3200, 2400, prog, S444)),
            Err(AvatarError::ImageTooLarge)
        );
        assert_eq!(
            check_jpeg_bounds(&frame(3200, 2400, prog, CMYK444)),
            Err(AvatarError::ImageTooLarge)
        );
        assert_eq!(check_jpeg_bounds(&frame(3200, 2400, prog, GREY)), Ok(()));
        // Lossless: not decoded at all.
        assert_eq!(
            check_jpeg_bounds(&frame(64, 64, JpegCoding::Lossless, GREY)),
            Err(AvatarError::Unreadable)
        );
    }

    /// The bounds of the whole decodes: pixels for WebP, whose decoder
    /// holds several frames' worth, the frame's bytes for PNG.
    #[test]
    fn the_whole_decode_bounds() {
        assert_eq!(check_whole_bounds(ImageFormat::WebP, 2097, 2097, 0), Ok(()));
        assert_eq!(
            check_whole_bounds(ImageFormat::WebP, 2098, 2098, 0),
            Err(AvatarError::ImageTooLarge)
        );
        assert_eq!(
            check_whole_bounds(ImageFormat::Png, 1, 1, MAX_PNG_FRAME_BYTES),
            Ok(())
        );
        assert_eq!(
            check_whole_bounds(ImageFormat::Png, 1, 1, MAX_PNG_FRAME_BYTES + 1),
            Err(AvatarError::ImageTooLarge)
        );
    }

    /// The header walk: the frame read from the SOF, the ICC profile left
    /// out of what the decoder is handed, the EXIF kept.
    #[test]
    fn the_header_walk_reads_the_frame_and_leaves_the_icc_out() {
        let base = jpeg_with(&halves(64, 48), true, SamplingFactor::F_2_2);
        let source = with_exif(&with_icc(&base, 3), 6);
        assert!(contains(&source, b"ICC_PROFILE"));

        let header = scan_jpeg_header(&source).unwrap();
        assert_eq!(header.frame, frame(64, 48, JpegCoding::Progressive, S420));
        let kept: Vec<u8> = header.kept.concat();
        assert!(!contains(&kept, b"ICC_PROFILE"));
        assert!(contains(&kept, b"Exif\0\0"));
        assert_eq!(kept.len(), source.len() - 3 * (4 + 14 + 100));

        let sequential = jpeg_with(&halves(17, 9), false, SamplingFactor::F_1_1);
        let header = scan_jpeg_header(&sequential).unwrap();
        assert_eq!(header.frame, frame(17, 9, JpegCoding::Sequential, S444));
        assert_eq!(header.kept.concat(), sequential);
    }

    #[test]
    fn a_broken_header_is_unreadable() {
        let whole = jpeg(&halves(16, 16));
        assert!(scan_jpeg_header(b"not a jpeg").is_err());
        // Cut inside the header: no SOF reached.
        assert_eq!(
            scan_jpeg_header(&whole[..20]).err(),
            Some(AvatarError::Unreadable)
        );
        // A segment length running past the end.
        let mut lying = whole[..4].to_vec();
        lying.extend_from_slice(&[0xFF, 0xFF]);
        assert_eq!(
            scan_jpeg_header(&lying).err(),
            Some(AvatarError::Unreadable)
        );
    }

    /// `jpeg` with `count` APP2 `ICC_PROFILE` segments of 100 bytes each.
    fn with_icc(jpeg: &[u8], count: u8) -> Vec<u8> {
        let mut out = jpeg[..2].to_vec();
        for i in 0..count {
            let mut payload = b"ICC_PROFILE\0".to_vec();
            payload.push(i + 1);
            payload.push(count);
            payload.extend(std::iter::repeat_n(0x5A, 100));
            out.extend_from_slice(&[0xFF, 0xE2]);
            out.extend_from_slice(&((payload.len() + 2) as u16).to_be_bytes());
            out.extend_from_slice(&payload);
        }
        out.extend_from_slice(&jpeg[2..]);
        out
    }

    /// A progressive JPEG goes through the reduced decode as well, in 4:2:0
    /// as in 4:4:4.
    #[test]
    fn a_progressive_jpeg_is_accepted() {
        for sampling in [SamplingFactor::F_2_2, SamplingFactor::F_1_1] {
            let source = jpeg_with(&halves(640, 480), true, sampling);
            let out = decoded(&reencode_avatar(&source).unwrap());
            assert_eq!(out.dimensions(), (AVATAR_SIDE, AVATAR_SIDE));
            assert!(is_red(*out.get_pixel(AVATAR_SIDE / 4, AVATAR_SIDE / 2)));
            assert!(is_blue(
                *out.get_pixel(3 * AVATAR_SIDE / 4, AVATAR_SIDE / 2)
            ));
        }
    }

    #[test]
    fn a_greyscale_jpeg_is_accepted() {
        let grey = image::GrayImage::from_fn(400, 300, |x, _| {
            image::Luma([if x < 200 { 30 } else { 220 }])
        });
        let mut source = Vec::new();
        JpegEncoder::new_with_quality(&mut source, 90)
            .encode_image(&grey)
            .unwrap();
        let out = decoded(&reencode_avatar(&source).unwrap());
        assert_eq!(out.dimensions(), (AVATAR_SIDE, AVATAR_SIDE));
        assert!(out.get_pixel(AVATAR_SIDE / 4, AVATAR_SIDE / 2)[0] < 80);
        assert!(out.get_pixel(3 * AVATAR_SIDE / 4, AVATAR_SIDE / 2)[0] > 170);
    }

    /// `jpeg-encoder`'s CMYK JPEG of a `width` × `height` picture of one
    /// colour.
    fn cmyk_jpeg(width: u16, height: u16, cmyk: [u8; 4], progressive: bool) -> Vec<u8> {
        let pixels: Vec<u8> = (0..usize::from(width) * usize::from(height))
            .flat_map(|_| cmyk)
            .collect();
        let mut out = Vec::new();
        let mut encoder = jpeg_encoder::Encoder::new(&mut out, 95);
        encoder.set_progressive(progressive);
        encoder
            .encode(&pixels, width, height, jpeg_encoder::ColorType::Cmyk)
            .unwrap();
        out
    }

    /// A CMYK JPEG goes through the reduced decode too, sequential or
    /// progressive, and keeps its colours: no ink is white, full magenta
    /// and yellow are red.
    #[test]
    fn a_cmyk_jpeg_is_accepted_with_its_colours() {
        for progressive in [false, true] {
            let white = cmyk_jpeg(640, 480, [0, 0, 0, 0], progressive);
            let white = decoded(&reencode_avatar(&white).unwrap());
            assert_eq!(white.dimensions(), (AVATAR_SIDE, AVATAR_SIDE));
            let p = white.get_pixel(AVATAR_SIDE / 2, AVATAR_SIDE / 2);
            assert!(p.0.iter().all(|&c| c > 240), "{progressive} {p:?}");

            let red = cmyk_jpeg(640, 480, [0, 255, 255, 0], progressive);
            let red = decoded(&reencode_avatar(&red).unwrap());
            let p = *red.get_pixel(AVATAR_SIDE / 2, AVATAR_SIDE / 2);
            assert!(is_red(p), "{progressive} {p:?}");
        }
    }

    /// `jpeg` with one APP2 `ICC_PROFILE` segment of 100 bytes inserted at
    /// `at`, a marker boundary past the first scan.
    fn with_icc_at(jpeg: &[u8], at: usize) -> Vec<u8> {
        let segment = with_icc(&[0xFF, 0xD8], 1);
        let mut out = jpeg[..at].to_vec();
        out.extend_from_slice(&segment[2..]);
        out.extend_from_slice(&jpeg[at..]);
        out
    }

    /// `jpeg_decoder` reads the APP segments between scans too, and keeps
    /// every ICC one: an APP2 past the first scan — between two scans of a
    /// progressive picture, or before the end marker — is refused, as the
    /// memory it could take has no bound.
    #[test]
    fn an_icc_segment_past_the_first_scan_is_too_large() {
        let progressive = jpeg_with(&halves(64, 48), true, SamplingFactor::F_2_2);
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
        let before_eoi = progressive.len() - 2;
        for at in [second_scan, before_eoi] {
            assert_eq!(
                reencode_avatar(&with_icc_at(&progressive, at)),
                Err(AvatarError::ImageTooLarge),
                "APP2 at {at}"
            );
        }
        let sequential = jpeg(&halves(64, 48));
        assert_eq!(
            reencode_avatar(&with_icc_at(&sequential, sequential.len() - 2)),
            Err(AvatarError::ImageTooLarge)
        );
        // The same file, its ICC before the first scan: accepted.
        assert!(reencode_avatar(&with_icc(&progressive, 1)).is_ok());
    }

    /// A RIFF WebP with a VP8X chunk carrying `flags`, then nothing more.
    fn vp8x_webp(flags: u8) -> Vec<u8> {
        let mut chunk = b"VP8X".to_vec();
        chunk.extend_from_slice(&10u32.to_le_bytes());
        chunk.extend_from_slice(&[flags, 0, 0, 0]);
        chunk.extend_from_slice(&[15, 0, 0, 15, 0, 0]);
        let mut out = b"RIFF".to_vec();
        out.extend_from_slice(&((4 + chunk.len()) as u32).to_le_bytes());
        out.extend_from_slice(b"WEBP");
        out.extend_from_slice(&chunk);
        out
    }

    /// The animation flag of the VP8X chunk; a simple WebP (VP8 or VP8L
    /// only) is never animated.
    #[test]
    fn an_animated_webp_is_told_from_its_header() {
        assert!(webp_is_animated(&vp8x_webp(0x02)));
        assert!(webp_is_animated(&vp8x_webp(0x12)));
        assert!(!webp_is_animated(&vp8x_webp(0x10)));
        assert!(!webp_is_animated(&vp8x_webp(0x00)));
        let mut still = Vec::new();
        WebPEncoder::new_lossless(&mut still)
            .write_image(halves(8, 8).as_raw(), 8, 8, image::ExtendedColorType::Rgb8)
            .unwrap();
        assert!(!webp_is_animated(&still));
        assert!(!webp_is_animated(b"RIFF"));
    }

    /// An animated WebP is not a photo: refused as of a type not taken,
    /// before anything is decoded — its decoder holds frames on top of
    /// frames.
    #[test]
    fn an_animated_webp_is_refused() {
        assert_eq!(
            reencode_avatar(&vp8x_webp(0x02)),
            Err(AvatarError::UnsupportedType)
        );
    }

    // --- WebP frame and metadata, PNG metadata (#588, review of #589) ---

    /// A RIFF WebP of the given chunks, each `(fourcc, data)`.
    fn riff_webp(chunks: &[(&[u8; 4], Vec<u8>)]) -> Vec<u8> {
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

    /// A VP8X chunk's data: `flags`, then the canvas.
    fn vp8x(flags: u8, width: u32, height: u32) -> Vec<u8> {
        let mut data = vec![flags, 0, 0, 0];
        data.extend_from_slice(&(width - 1).to_le_bytes()[..3]);
        data.extend_from_slice(&(height - 1).to_le_bytes()[..3]);
        data
    }

    /// The header of a VP8 key frame of `width` × `height`, no data.
    fn vp8_header(width: u16, height: u16) -> Vec<u8> {
        let mut data = vec![0x50, 0x00, 0x00, 0x9D, 0x01, 0x2A];
        data.extend_from_slice(&width.to_le_bytes());
        data.extend_from_slice(&height.to_le_bytes());
        data
    }

    /// The header of a VP8L picture of `width` × `height`, no data.
    fn vp8l_header(width: u32, height: u32) -> Vec<u8> {
        let mut data = vec![0x2F];
        data.extend_from_slice(&((width - 1) | (height - 1) << 14).to_le_bytes());
        data
    }

    /// The VP8L chunk data of `image`'s lossless WebP of `width` × `height`.
    fn vp8l_of(width: u32, height: u32) -> Vec<u8> {
        let mut webp = Vec::new();
        WebPEncoder::new_lossless(&mut webp)
            .write_image(
                halves(width, height).as_raw(),
                width,
                height,
                image::ExtendedColorType::Rgb8,
            )
            .unwrap();
        assert_eq!(&webp[12..16], b"VP8L");
        let size = u32::from_le_bytes(webp[16..20].try_into().unwrap()) as usize;
        webp[20..20 + size].to_vec()
    }

    /// The frame's own dimensions are read from its chunk, VP8 or VP8L,
    /// simple or under a VP8X canvas of the same size.
    #[test]
    fn the_webp_frame_is_read_from_its_chunk() {
        assert_eq!(
            webp_frame(&riff_webp(&[(b"VP8L", vp8l_of(8, 6))])),
            Ok((8, 6))
        );
        assert_eq!(
            webp_frame(&riff_webp(&[(b"VP8 ", vp8_header(3000, 2000))])),
            Ok((3000, 2000))
        );
        assert_eq!(
            webp_frame(&riff_webp(&[
                (b"VP8X", vp8x(0, 8, 6)),
                (b"VP8L", vp8l_of(8, 6))
            ])),
            Ok((8, 6))
        );
        assert_eq!(
            webp_frame(&riff_webp(&[(b"VP8X", vp8x(0, 8, 6))])),
            Err(AvatarError::Unreadable)
        );
    }

    /// `image-webp` decodes a frame at its own dimensions before it looks at
    /// the canvas: a frame larger than its canvas is refused before any
    /// decode, in VP8 as in VP8L.
    #[test]
    fn a_webp_frame_other_than_its_canvas_is_unreadable() {
        for frame in [
            (b"VP8 ", vp8_header(16383, 16383)),
            (b"VP8L", vp8l_header(16383, 16383)),
        ] {
            let lying = riff_webp(&[(b"VP8X", vp8x(0, 100, 100)), frame]);
            assert_eq!(webp_frame(&lying), Err(AvatarError::Unreadable));
            assert_eq!(reencode_avatar(&lying), Err(AvatarError::Unreadable));
        }
    }

    /// The pixel bound is checked on the frame, before any decode.
    #[test]
    fn a_webp_frame_past_the_bound_is_too_large() {
        let large = riff_webp(&[(b"VP8 ", vp8_header(3000, 3000))]);
        assert_eq!(reencode_avatar(&large), Err(AvatarError::ImageTooLarge));
    }

    /// The EXIF a decoder reads for the orientation is held whole: past
    /// `MAX_METADATA_BYTES`, refused.
    #[test]
    fn a_webp_exif_past_the_metadata_bound_is_too_large() {
        let with_exif = |size: usize| {
            riff_webp(&[
                (b"VP8X", vp8x(0x08, 8, 6)),
                (b"VP8L", vp8l_of(8, 6)),
                (b"EXIF", vec![0; size]),
            ])
        };
        assert!(reencode_avatar(&with_exif(64)).is_ok());
        assert_eq!(
            reencode_avatar(&with_exif(MAX_METADATA_BYTES + 1)),
            Err(AvatarError::ImageTooLarge)
        );
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

    /// `png` with a chunk of `kind` and `data` right after its IHDR.
    fn png_with_chunk(png: &[u8], kind: &[u8; 4], data: &[u8]) -> Vec<u8> {
        let mut chunk = (data.len() as u32).to_be_bytes().to_vec();
        let mut typed = kind.to_vec();
        typed.extend_from_slice(data);
        chunk.extend_from_slice(&typed);
        chunk.extend_from_slice(&crc32(&typed).to_be_bytes());
        let after_ihdr = 8 + 4 + 4 + 13 + 4;
        let mut out = png[..after_ihdr].to_vec();
        out.extend_from_slice(&chunk);
        out.extend_from_slice(&png[after_ihdr..]);
        out
    }

    /// An iCCP chunk's data: a profile name, the compression method, then
    /// `profile` deflated.
    fn iccp(profile: &[u8]) -> Vec<u8> {
        use std::io::Write;
        let mut data = b"icc\0\0".to_vec();
        let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::best());
        z.write_all(profile).unwrap();
        data.extend_from_slice(&z.finish().unwrap());
        data
    }

    /// `png` decompresses an iCCP profile with no bound of ours: a profile
    /// past `MAX_METADATA_BYTES` once inflated is refused, a real one — a
    /// Display P3 profile is about half a kilobyte — accepted. A large eXIf
    /// is refused the same way: `png` copies it whole.
    #[test]
    fn png_metadata_past_the_bound_is_too_large() {
        let base = png(&halves(64, 64));
        let real: Vec<u8> = (0..548u32).map(|i| (i * 37 % 251) as u8).collect();
        assert!(reencode_avatar(&png_with_chunk(&base, b"iCCP", &iccp(&real))).is_ok());

        let bomb = png_with_chunk(&base, b"iCCP", &iccp(&vec![0; MAX_METADATA_BYTES + 1]));
        assert!(bomb.len() < 10_000, "{}", bomb.len());
        assert_eq!(reencode_avatar(&bomb), Err(AvatarError::ImageTooLarge));
        let at_bound = png_with_chunk(&base, b"iCCP", &iccp(&vec![0; MAX_METADATA_BYTES]));
        assert!(reencode_avatar(&at_bound).is_ok());

        let exif = png_with_chunk(&base, b"eXIf", &vec![0; MAX_METADATA_BYTES + 1]);
        assert_eq!(reencode_avatar(&exif), Err(AvatarError::ImageTooLarge));
    }

    /// A JPEG carrying an ICC profile decodes as one without: the profile
    /// is never handed to the decoder.
    #[test]
    fn a_jpeg_with_an_icc_profile_is_accepted() {
        let source = with_icc(&jpeg(&halves(640, 480)), 4);
        let out = decoded(&reencode_avatar(&source).unwrap());
        assert!(is_red(*out.get_pixel(AVATAR_SIDE / 4, AVATAR_SIDE / 2)));
    }

    // --- dimensioning ---

    #[test]
    fn the_square_is_centred_on_the_longer_side() {
        assert_eq!(center_square(300, 100), (100, 0, 100));
        assert_eq!(center_square(100, 300), (0, 100, 100));
        assert_eq!(center_square(64, 64), (0, 0, 64));
        // An odd margin rounds down: the extra pixel goes to the far side.
        assert_eq!(center_square(7, 4), (1, 0, 4));
        assert_eq!(center_square(1, 1), (0, 0, 1));
    }

    #[test]
    fn every_avatar_comes_out_a_jpeg_square_of_the_fixed_side() {
        for (w, h) in [(300, 100), (100, 300), (512, 512), (10, 10), (1, 1)] {
            let out = reencode_avatar(&png(&halves(w, h))).unwrap();
            assert_eq!(
                image::guess_format(&out).unwrap(),
                ImageFormat::Jpeg,
                "{w}x{h}"
            );
            assert_eq!(
                decoded(&out).dimensions(),
                (AVATAR_SIDE, AVATAR_SIDE),
                "{w}x{h}"
            );
        }
    }

    /// The crop keeps the middle: a 3:1 picture, red on its left half and
    /// blue on its right, keeps both halves of the middle third.
    #[test]
    fn the_crop_keeps_the_middle_of_the_picture() {
        let out = decoded(&reencode_avatar(&png(&halves(300, 100))).unwrap());
        assert!(is_red(*out.get_pixel(AVATAR_SIDE / 4, AVATAR_SIDE / 2)));
        assert!(is_blue(
            *out.get_pixel(3 * AVATAR_SIDE / 4, AVATAR_SIDE / 2)
        ));
    }

    /// Transparency is laid on white, not on the black a dropped alpha
    /// channel leaves.
    #[test]
    fn transparency_is_laid_on_white() {
        let clear = RgbaImage::from_pixel(16, 16, Rgba([0, 0, 0, 0]));
        let mut bytes = Vec::new();
        PngEncoder::new(&mut bytes)
            .write_image(clear.as_raw(), 16, 16, image::ExtendedColorType::Rgba8)
            .unwrap();
        let out = decoded(&reencode_avatar(&bytes).unwrap());
        let p = out.get_pixel(AVATAR_SIDE / 2, AVATAR_SIDE / 2);
        assert!(p.0.iter().all(|&c| c > 240), "{p:?}");
    }

    // --- metadata ---

    /// Criterion of #467: a JPEG carrying GPS coordinates comes out with no
    /// EXIF at all.
    #[test]
    fn the_stored_jpeg_carries_no_exif() {
        let source = with_exif(&jpeg(&halves(64, 64)), 1);
        assert!(contains(&source, b"Exif\0\0"));
        assert!(contains(&source, b"GPS-MARKER"));

        let out = reencode_avatar(&source).unwrap();

        assert!(!contains(&out, b"Exif"), "an EXIF block survived");
        assert!(!contains(&out, b"GPS-MARKER"), "the GPS data survived");
        assert!(!contains(&out, &[0xFF, 0xE1]), "an APP1 segment survived");
    }

    /// The orientation a phone records instead of turning the pixels is
    /// applied before the EXIF goes, so the photo is stored upright.
    /// Orientation 6: the picture is to be turned 90° clockwise, which
    /// brings its left half to the top.
    #[test]
    fn the_exif_orientation_is_applied_before_it_is_dropped() {
        let upright = decoded(&reencode_avatar(&with_exif(&jpeg(&halves(64, 32)), 1)).unwrap());
        assert!(is_red(*upright.get_pixel(AVATAR_SIDE / 4, AVATAR_SIDE / 2)));
        assert!(is_blue(
            *upright.get_pixel(3 * AVATAR_SIDE / 4, AVATAR_SIDE / 2)
        ));

        let turned = decoded(&reencode_avatar(&with_exif(&jpeg(&halves(64, 32)), 6)).unwrap());
        assert!(is_red(*turned.get_pixel(AVATAR_SIDE / 2, AVATAR_SIDE / 4)));
        assert!(is_blue(
            *turned.get_pixel(AVATAR_SIDE / 2, 3 * AVATAR_SIDE / 4)
        ));
    }
}
