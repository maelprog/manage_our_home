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
use jpeg_decoder::{CodingProcess, PixelFormat};
use tokio::sync::Semaphore;
use uuid::Uuid;

use crate::auth::session::{scoped_tx, AuthUser};
use crate::error::{AppError, AppResult};
use crate::storage::UPLOAD_FRAMING_MARGIN_BYTES;
use crate::AppState;

/// The largest photo accepted, in bytes. A phone's full-resolution JPEG is
/// a few megabytes; 12 MiB leaves room for the larger sensors, as the
/// barcode photo of apps/web does (`routes/stocks/photo.rs`).
pub const MAX_AVATAR_BYTES: usize = 12 * 1024 * 1024;

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

/// Bound of `image`'s whole decode (PNG, WebP, the JPEGs `decode_jpeg`
/// leaves): no dimension past this, whatever the file declares.
const MAX_DIMENSION: u32 = 8192;

/// Bound on the frame of `image`'s whole decode, in bytes: a 24 Mpx frame
/// in 8-bit RGB (6000 × 4000 × 3 = 72 MB) fits; a larger one — more pixels,
/// or more bytes per pixel, as a 16-bit PNG — is refused as too large from
/// what its header declares (`ImageDecoder::total_bytes`), before anything
/// is decoded. Also handed to `image` as `Limits::max_alloc`, which does not
/// bound that frame on the `from_decoder` path: hence the explicit check.
/// A JPEG reduced at decode is not bound by it, but by `MAX_JPEG_SIDE` and
/// `MAX_PROGRESSIVE_COEFFICIENT_BYTES`.
const MAX_ALLOC: u64 = 80 * 1024 * 1024;

/// Decodes running at once in this process, since the upload gate alone
/// admits more bodies than decodes should multiply. Count
/// `DECODE_PERMITS × DECODE_PEAK_BYTES`, 360 MB, for the decodes when
/// sizing the api's memory (the pod's limit on k3s).
const DECODE_PERMITS: usize = 2;

/// What one decode holds at its peak, in bytes, at most: the upload's
/// bytes aside, the decoders' buffers and the frame. Peaks counted by
/// `tests/avatar_memory.rs` (#588), which fails past this bound:
///
/// - a sequential JPEG of 8000 × 6000, reduced to 1000 × 750: 4.5 to
///   5.6 MB, in 4:2:0 as in 4:4:4 — the 48 Mpx are never held;
/// - a progressive 4:4:4 JPEG of 6000 × 4600, the nearest to
///   `MAX_PROGRESSIVE_COEFFICIENT_BYTES`: 168 to 169 MB, every coefficient
///   held until the last scan;
/// - a PNG of 5280 × 5280 in RGB, near `MAX_ALLOC`: 84.0 MB;
/// - a lossless WebP of 6000 × 4000: 168.0 MB.
///
/// The two JPEG figures move by up to a megabyte between runs, the decoder
/// working on threads of its own. PNG and WebP are decoded whole (`image`
/// has no reduced decode for them). A sequential JPEG at `MAX_JPEG_SIDE`
/// is reduced to 2048 × 2048, a frame of 12.6 MB (reckoned, not counted).
/// Before #588, a progressive 4:4:4 JPEG of 5000 × 5000 held 233 MB
/// (review of #467).
pub const DECODE_PEAK_BYTES: usize = 180_000_000;

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

/// Bound on either side of a JPEG (#588). Its sequential decode is reduced
/// in the inverse DCT, so what it holds follows the reduced frame, not the
/// photo: this bound is what keeps that frame small — 16384 × 16384 comes
/// out at 2048 × 2048 — and the decode's time short. A 200 Mpx phone mode
/// (16320 × 12240) fits.
pub const MAX_JPEG_SIDE: u32 = 16384;

/// Bound on a progressive JPEG's coefficient buffer, in bytes, as
/// `progressive_coefficient_bytes` reckons it from the header. Every
/// coefficient of a progressive picture is held until its last scan, at
/// full size whatever the reduction: that is the one decode whose memory
/// grows with the photo. 160 MiB admits 24 Mpx in colour (6000 × 4000,
/// 146 MB) and refuses 48 Mpx (8000 × 6000, 291 MB); a phone's camera
/// writes sequential JPEGs, which this bound does not touch.
pub const MAX_PROGRESSIVE_COEFFICIENT_BYTES: u64 = 160 * 1024 * 1024;

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

/// An upper bound of a progressive JPEG's coefficient buffer: 64
/// coefficients of 2 bytes per 8 × 8 block of each component. A component
/// sampled below the largest factor has fewer blocks, never more, and the
/// picture is padded to whole MCUs, 32 pixels at most: hence a full plane
/// per component, of the padded size.
pub fn progressive_coefficient_bytes(width: u32, height: u32, components: u32) -> u64 {
    u64::from(components) * (u64::from(width) + 31) * (u64::from(height) + 31) * 2
}

/// The bounds of a JPEG decoded reduced, checked on what its header
/// declares, before anything of its size is allocated.
pub fn check_jpeg_bounds(
    width: u32,
    height: u32,
    components: u32,
    progressive: bool,
) -> Result<(), AvatarError> {
    if width > MAX_JPEG_SIDE || height > MAX_JPEG_SIDE {
        return Err(AvatarError::ImageTooLarge);
    }
    if progressive
        && progressive_coefficient_bytes(width, height, components)
            > MAX_PROGRESSIVE_COEFFICIENT_BYTES
    {
        return Err(AvatarError::ImageTooLarge);
    }
    Ok(())
}

/// A decoded picture and the orientation its EXIF records.
type Decoded = (DynamicImage, Option<Orientation>);

/// A JPEG decoded reduced (#588): `jpeg_decoder` scales it in the inverse
/// DCT to the smallest frame whose short side still holds `AVATAR_SIDE`,
/// so an 8000 × 6000 photo is decoded straight to 1000 × 750 and never
/// exists at full size. `None` for what this path leaves to `image`'s
/// whole decode, under its bounds: CMYK, lossless or more than 8 bits.
pub(crate) fn decode_jpeg(bytes: &[u8]) -> Result<Option<Decoded>, AvatarError> {
    let mut decoder = jpeg_decoder::Decoder::new(bytes);
    decoder.read_info().map_err(|_| AvatarError::Unreadable)?;
    let info = decoder.info().ok_or(AvatarError::Unreadable)?;
    let components = match info.pixel_format {
        PixelFormat::L8 => 1,
        PixelFormat::RGB24 => 3,
        PixelFormat::L16 | PixelFormat::CMYK32 => return Ok(None),
    };
    let progressive = match info.coding_process {
        CodingProcess::DctSequential => false,
        CodingProcess::DctProgressive => true,
        CodingProcess::Lossless => return Ok(None),
    };
    check_jpeg_bounds(
        info.width.into(),
        info.height.into(),
        components,
        progressive,
    )?;

    let (request_width, request_height) = jpeg_scale_request(info.width, info.height);
    let (width, height) = decoder
        .scale(request_width, request_height)
        .map_err(|_| AvatarError::Unreadable)?;
    // Unreachable once reduced under `MAX_JPEG_SIDE`: a second guard.
    decoder.set_max_decoding_buffer_size(MAX_ALLOC as usize);
    let pixels = decoder.decode().map_err(|_| AvatarError::Unreadable)?;
    let orientation = decoder.exif_data().and_then(Orientation::from_exif_chunk);

    let (width, height) = (u32::from(width), u32::from(height));
    let picture = if components == 1 {
        GrayImage::from_raw(width, height, pixels).map(DynamicImage::ImageLuma8)
    } else {
        RgbImage::from_raw(width, height, pixels).map(DynamicImage::ImageRgb8)
    };
    Ok(Some((picture.ok_or(AvatarError::Unreadable)?, orientation)))
}

/// A picture decoded whole by `image`, under `MAX_DIMENSION` and
/// `MAX_ALLOC`: PNG, WebP, and the JPEGs `decode_jpeg` leaves.
fn decode_whole(bytes: &[u8], format: ImageFormat) -> Result<Decoded, AvatarError> {
    // A bound passed is `ImageError::Limits`; anything else is unreadable.
    let refusal = |e: ImageError| match e {
        ImageError::Limits(_) => AvatarError::ImageTooLarge,
        _ => AvatarError::Unreadable,
    };
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_DIMENSION);
    limits.max_image_height = Some(MAX_DIMENSION);
    limits.max_alloc = Some(MAX_ALLOC);
    let mut reader = ImageReader::with_format(Cursor::new(bytes), format);
    reader.limits(limits);
    let mut decoder = reader.into_decoder().map_err(refusal)?;
    if decoder.total_bytes() > MAX_ALLOC {
        return Err(AvatarError::ImageTooLarge);
    }
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
    let reduced = match format {
        ImageFormat::Jpeg => decode_jpeg(bytes)?,
        _ => None,
    };
    let (picture, orientation) = match reduced {
        Some(decoded) => decoded,
        None => decode_whole(bytes, format)?,
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

    let jpeg = {
        let _decode = DECODES
            .acquire()
            .await
            .map_err(|e| AppError::Internal(e.into()))?;
        tokio::task::spawn_blocking(move || reencode_avatar(&bytes))
            .await
            .map_err(|e| AppError::Internal(e.into()))??
    };

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

    /// Within the dimension bound, but past `MAX_ALLOC` once decoded: a
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
            let (picture, _) = decode_jpeg(&jpeg(&halves(w, h))).unwrap().unwrap();
            picture.dimensions()
        };
        assert_eq!(dims(8000, 6000), (1000, 750));
        assert_eq!(dims(6000, 600), (3000, 300));
        assert_eq!(dims(300, 200), (300, 200));
    }

    /// An upper bound of a progressive JPEG's coefficient buffer: 64
    /// coefficients of 2 bytes per 8 × 8 block of every component, whatever
    /// its chroma sampling, padding to the MCU included.
    #[test]
    fn the_progressive_buffer_bound_covers_every_sampling() {
        // 4:4:4, the largest: three full planes of 2 bytes per pixel.
        assert!(progressive_coefficient_bytes(6000, 4000, 3) >= 6000 * 4000 * 3 * 2);
        // MCU padding: at the largest sampling factor (4), a 1 × 1 picture
        // still holds 4 × 4 blocks in each of its components.
        assert!(progressive_coefficient_bytes(1, 1, 3) >= 3 * 16 * 64 * 2);
        assert!(progressive_coefficient_bytes(1, 1, 1) < progressive_coefficient_bytes(1, 1, 3));
    }

    #[test]
    fn the_jpeg_bounds() {
        // Sequential: the side bound alone; the frame decoded is reduced.
        assert_eq!(check_jpeg_bounds(8000, 6000, 3, false), Ok(()));
        assert_eq!(
            check_jpeg_bounds(MAX_JPEG_SIDE, MAX_JPEG_SIDE, 3, false),
            Ok(())
        );
        assert_eq!(
            check_jpeg_bounds(MAX_JPEG_SIDE + 1, 8, 3, false),
            Err(AvatarError::ImageTooLarge)
        );
        assert_eq!(
            check_jpeg_bounds(8, MAX_JPEG_SIDE + 1, 1, false),
            Err(AvatarError::ImageTooLarge)
        );
        // Progressive: every coefficient is held until the last scan, so
        // the buffer bound applies — 24 Mpx in colour passes, 48 Mpx does
        // not; 48 Mpx in greyscale, a third of it, passes.
        assert_eq!(check_jpeg_bounds(6000, 4000, 3, true), Ok(()));
        assert_eq!(
            check_jpeg_bounds(8000, 6000, 3, true),
            Err(AvatarError::ImageTooLarge)
        );
        assert_eq!(check_jpeg_bounds(8000, 6000, 1, true), Ok(()));
        assert_eq!(
            check_jpeg_bounds(MAX_JPEG_SIDE + 1, 8, 1, true),
            Err(AvatarError::ImageTooLarge)
        );
    }

    /// A progressive JPEG goes through the reduced decode as well, in 4:2:0
    /// as in 4:4:4.
    #[test]
    fn a_progressive_jpeg_is_accepted() {
        for sampling in [SamplingFactor::F_2_2, SamplingFactor::F_1_1] {
            let source = jpeg_with(&halves(640, 480), true, sampling);
            assert!(decode_jpeg(&source).unwrap().is_some());
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

    /// A CMYK JPEG, which the reduced decode leaves to `image`, is still
    /// accepted.
    #[test]
    fn a_cmyk_jpeg_is_accepted() {
        let cmyk = vec![0u8; 64 * 64 * 4];
        let mut source = Vec::new();
        jpeg_encoder::Encoder::new(&mut source, 90)
            .encode(&cmyk, 64, 64, jpeg_encoder::ColorType::Cmyk)
            .unwrap();
        assert!(decode_jpeg(&source).unwrap().is_none());
        let out = decoded(&reencode_avatar(&source).unwrap());
        assert_eq!(out.dimensions(), (AVATAR_SIDE, AVATAR_SIDE));
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
