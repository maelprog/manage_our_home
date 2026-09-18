//! Bounds on request bodies shared by apps/api and apps/web (#219): how
//! long a client may take to send one (`limits`, `body`), and how many
//! uploads a process holds in memory at once (`gate`).

pub mod body;
pub mod gate;
pub mod limits;

pub use body::{
    guard_request_body, request_timeout, service_unavailable, BodyGuard, BodyReadTimeout,
};
pub use gate::{Busy, UploadGate, UploadPermit};
pub use limits::{BodyReadLimits, Breach};
