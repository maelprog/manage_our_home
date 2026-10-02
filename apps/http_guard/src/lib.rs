//! Request guards shared by apps/api and apps/web: bounds on request
//! bodies (#219) — how long a client may take to send one (`limits`,
//! `body`), and how many uploads a process holds in memory at once
//! (`gate`) — and the refusal of cross-origin requests (#223, `origin`).

pub mod body;
pub mod gate;
pub mod limits;
pub mod origin;

pub use body::{
    guard_request_body, request_timeout, service_unavailable, BodyGuard, BodyReadTimeout,
};
pub use gate::{Busy, UploadGate, UploadPermit};
pub use limits::{BodyReadLimits, Breach, MAX_UPLOAD_BODY_BYTES};
pub use origin::{guard_cross_origin, origin_of, CrossOrigin, OriginGuard};
