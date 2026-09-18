//! The request body wrapper and the middleware that installs it (#219).
//!
//! Every route gets it, in apps/api and apps/web alike: a body that breaks
//! a bound in `BodyReadLimits` fails the read, and the middleware answers
//! 408 with `Connection: close` whatever the handler made of that failure.
//! A handler cannot be trusted to: `Multipart` answers 400, `Json` 400 or
//! 413, and apps/web's upload page redirects. And the connection is closed
//! because the rest of the body was never read.
//!
//! No bound starts until the handler first polls the body, and a handler
//! that never does is untouched. That is what spares the Messagerie
//! WebSocket (`/groups/:id/messages/ws`): its upgrade is a GET without a
//! body, and the connection it becomes is not a request body at all. A
//! `TimeoutLayer` over whole requests would have cut it.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, OnceLock};
use std::task::{Context, Poll};

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::middleware::Next;
use axum::response::Response;
use bytes::Bytes;
use http_body::{Frame, SizeHint};
use tokio::time::{Instant, Sleep};

use crate::limits::{BodyReadLimits, Breach, Verdict};

type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// The error a guarded body fails with once a bound is broken.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BodyReadTimeout(pub Breach);

impl std::fmt::Display for BodyReadTimeout {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "request body read timed out ({:?})", self.0)
    }
}

impl std::error::Error for BodyReadTimeout {}

/// A request body held to `BodyReadLimits`. `tripped` is set the first
/// time a bound is broken, so the middleware can tell after the fact.
pub struct GuardedBody<B> {
    inner: B,
    limits: BodyReadLimits,
    clock: Option<Clock>,
    timer: Option<Pin<Box<Sleep>>>,
    tripped: Arc<OnceLock<Breach>>,
}

struct Clock {
    first_read: Instant,
    last_chunk: Instant,
    received: u64,
}

impl<B> GuardedBody<B> {
    pub fn new(inner: B, limits: BodyReadLimits, tripped: Arc<OnceLock<Breach>>) -> Self {
        Self {
            inner,
            limits,
            clock: None,
            timer: None,
            tripped,
        }
    }
}

impl<B> http_body::Body for GuardedBody<B>
where
    B: http_body::Body<Data = Bytes> + Unpin,
    B::Error: Into<BoxError>,
{
    type Data = Bytes;
    type Error = BoxError;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, BoxError>>> {
        let this = &mut *self;
        if let Some(breach) = this.tripped.get() {
            return Poll::Ready(Some(Err(Box::new(BodyReadTimeout(*breach)))));
        }
        let now = Instant::now();
        let clock = this.clock.get_or_insert(Clock {
            first_read: now,
            last_chunk: now,
            received: 0,
        });

        match Pin::new(&mut this.inner).poll_frame(cx) {
            Poll::Ready(Some(Ok(frame))) => {
                if let Some(data) = frame.data_ref() {
                    clock.received += data.len() as u64;
                    clock.last_chunk = now;
                }
                return Poll::Ready(Some(Ok(frame)));
            }
            Poll::Ready(Some(Err(e))) => return Poll::Ready(Some(Err(e.into()))),
            Poll::Ready(None) => return Poll::Ready(None),
            Poll::Pending => {}
        }

        // Nothing to hand over: judge the wait so far, and arm a timer for
        // the next instant the verdict could change without a new chunk.
        // The inner body has registered the waker too, so whichever comes
        // first — a chunk or the deadline — polls this again.
        loop {
            let now = Instant::now();
            match this.limits.judge(
                now - clock.first_read,
                now - clock.last_chunk,
                clock.received,
            ) {
                Verdict::Breached(breach) => {
                    let _ = this.tripped.set(breach);
                    this.timer = None;
                    return Poll::Ready(Some(Err(Box::new(BodyReadTimeout(breach)))));
                }
                Verdict::Within { recheck_at } => {
                    let deadline = clock.first_read + recheck_at;
                    let timer = match &mut this.timer {
                        Some(timer) => {
                            timer.as_mut().reset(deadline);
                            timer
                        }
                        None => this
                            .timer
                            .insert(Box::pin(tokio::time::sleep_until(deadline))),
                    };
                    if timer.as_mut().poll(cx).is_pending() {
                        return Poll::Pending;
                    }
                    // Already due: judge again, which `judge` guarantees
                    // is a breach once its own deadline is reached.
                }
            }
        }
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        self.inner.size_hint()
    }
}

/// What the middleware needs: the bounds, and the body of the 408 in the
/// app's own shape (JSON for apps/api, a page for apps/web). Status and
/// `Connection` are set by the middleware, not by `render`.
#[derive(Clone, Copy)]
pub struct BodyGuard {
    pub limits: BodyReadLimits,
    pub render: fn() -> Response,
}

/// `axum::middleware::from_fn_with_state(guard, guard_request_body)`.
pub async fn guard_request_body(
    State(guard): State<BodyGuard>,
    req: Request,
    next: Next,
) -> Response {
    let tripped = Arc::new(OnceLock::new());
    let req = req.map(|b| Body::new(GuardedBody::new(b, guard.limits, tripped.clone())));
    let resp = next.run(req).await;
    match tripped.get() {
        None => resp,
        Some(breach) => {
            tracing::info!(?breach, "request body read timed out");
            request_timeout((guard.render)())
        }
    }
}

/// 408 with `Connection: close` (RFC 9110 §15.5.9: the server closes the
/// connection, and the unread rest of the body must not be taken for a
/// next request).
pub fn request_timeout(mut resp: Response) -> Response {
    *resp.status_mut() = StatusCode::REQUEST_TIMEOUT;
    resp.headers_mut()
        .insert(header::CONNECTION, HeaderValue::from_static("close"));
    resp
}

/// 503 with `Retry-After`, for a full `UploadGate`.
pub fn service_unavailable(mut resp: Response) -> Response {
    *resp.status_mut() = StatusCode::SERVICE_UNAVAILABLE;
    resp.headers_mut().insert(
        header::RETRY_AFTER,
        HeaderValue::from(crate::gate::RETRY_AFTER_SECS),
    );
    resp
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::routing::{get, post};
    use axum::Router;
    use http_body_util::BodyExt;
    use std::time::Duration;
    use tokio::sync::mpsc;
    use tower::ServiceExt;

    const S: fn(u64) -> Duration = Duration::from_secs;

    /// A body fed chunk by chunk from a channel; ends when the sender drops.
    struct Fed(mpsc::Receiver<Bytes>);

    impl http_body::Body for Fed {
        type Data = Bytes;
        type Error = std::convert::Infallible;

        fn poll_frame(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
        ) -> Poll<Option<Result<Frame<Bytes>, Self::Error>>> {
            self.0.poll_recv(cx).map(|c| c.map(|b| Ok(Frame::data(b))))
        }
    }

    /// Sends `(delay before, chunk size)` in order, then keeps the channel
    /// open for `hold` before ending the body.
    fn feed(plan: Vec<(Duration, usize)>, hold: Duration) -> Fed {
        let (tx, rx) = mpsc::channel(1);
        tokio::spawn(async move {
            for (delay, size) in plan {
                tokio::time::sleep(delay).await;
                if tx.send(Bytes::from(vec![b'x'; size])).await.is_err() {
                    return;
                }
            }
            tokio::time::sleep(hold).await;
        });
        Fed(rx)
    }

    fn guarded(body: Fed) -> (GuardedBody<Fed>, Arc<OnceLock<Breach>>) {
        let tripped = Arc::new(OnceLock::new());
        (
            GuardedBody::new(body, BodyReadLimits::PRODUCTION, tripped.clone()),
            tripped,
        )
    }

    /// Reads the body to its end or its first error: bytes read, the
    /// error's breach if any, and how long it took.
    async fn drain(mut body: GuardedBody<Fed>) -> (u64, Option<Breach>, Duration) {
        let start = Instant::now();
        let mut read = 0;
        loop {
            match body.frame().await {
                None => return (read, None, start.elapsed()),
                Some(Ok(f)) => read += f.into_data().map(|d| d.len() as u64).unwrap_or(0),
                Some(Err(e)) => {
                    let e = e.downcast::<BodyReadTimeout>().expect("a BodyReadTimeout");
                    return (read, Some(e.0), start.elapsed());
                }
            }
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_body_sent_at_a_steady_pace_is_read_whole() {
        // 20 chunks of 5 kB, one a second: 5 kB/s, well above the rate.
        let (body, tripped) = guarded(feed(vec![(S(1), 5_000); 20], Duration::ZERO));
        let (read, breach, _) = drain(body).await;
        assert_eq!((read, breach), (100_000, None));
        assert!(tripped.get().is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn silence_after_a_generous_start_is_cut_at_the_idle_bound() {
        // 1 MB at once covers the rate for 1 000 s; then nothing.
        let (body, tripped) = guarded(feed(vec![(Duration::ZERO, 1_000_000)], S(3_600)));
        let (read, breach, took) = drain(body).await;
        assert_eq!((read, breach), (1_000_000, Some(Breach::Idle)));
        assert_eq!(took, S(30));
        assert_eq!(tripped.get(), Some(&Breach::Idle));
    }

    #[tokio::test(start_paused = true)]
    async fn a_drip_under_the_idle_bound_is_cut_when_grace_ends() {
        // One byte every 29 s: never 30 s of silence, and far under 1 kB/s.
        let (body, _) = guarded(feed(vec![(S(29), 1); 40], Duration::ZERO));
        let (read, breach, took) = drain(body).await;
        assert_eq!((read, breach), (0, Some(Breach::TooSlow)));
        assert_eq!(took, S(10));
    }

    #[tokio::test(start_paused = true)]
    async fn a_body_just_under_the_rate_is_cut_the_moment_it_falls_behind() {
        // 10 kB up front, then 900 bytes each second. After the k-th chunk
        // (at t = k s) the average holds until t = 10 + 0.9·k s, which
        // first comes before the next chunk for k = 91: cut just after
        // t = 91.9 s, 91 chunks in.
        let mut plan = vec![(Duration::ZERO, 10_000)];
        plan.extend(vec![(S(1), 900); 1_000]);
        let (body, _) = guarded(feed(plan, Duration::ZERO));
        let (read, breach, took) = drain(body).await;
        assert_eq!(breach, Some(Breach::TooSlow));
        assert_eq!(read, 10_000 + 91 * 900);
        assert!(
            took > Duration::from_millis(91_900) && took < S(92),
            "cut at {took:?}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_body_meeting_every_other_bound_is_cut_at_the_total() {
        // 2 kB every second, forever: fast enough, never silent. Chunks land
        // on the half second so none of them ties with the 900 s bound.
        let mut plan = vec![(Duration::from_millis(500), 2_000)];
        plan.extend(vec![(S(1), 2_000); 2_000]);
        let (body, _) = guarded(feed(plan, Duration::ZERO));
        let (read, breach, took) = drain(body).await;
        assert_eq!(breach, Some(Breach::Total));
        assert_eq!(took, S(900));
        assert_eq!(read, 900 * 2_000);
    }

    #[tokio::test(start_paused = true)]
    async fn the_clock_starts_at_the_first_read_not_at_wrapping() {
        let (body, _) = guarded(feed(vec![(S(1), 5_000); 3], Duration::ZERO));
        // An hour between wrapping and the handler's first read.
        tokio::time::sleep(S(3_600)).await;
        let (read, breach, _) = drain(body).await;
        assert_eq!((read, breach), (15_000, None));
    }

    #[tokio::test(start_paused = true)]
    async fn an_error_is_repeated_on_every_later_read() {
        let (mut body, _) = guarded(feed(vec![], S(3_600)));
        let first = body.frame().await.unwrap().unwrap_err();
        assert!(first.is::<BodyReadTimeout>());
        let again = body.frame().await.unwrap().unwrap_err();
        assert!(again.is::<BodyReadTimeout>());
    }

    // -- the middleware -----------------------------------------------------

    fn render() -> Response {
        Response::new(Body::from("too slow"))
    }

    fn app() -> Router {
        Router::new()
            // A handler that turns any body failure into its own 400, as
            // `Multipart`'s and `Json`'s rejections do.
            .route(
                "/echo",
                post(|body: Body| async move {
                    match body.collect().await {
                        Ok(b) => (StatusCode::OK, b.to_bytes()).into_response(),
                        Err(_) => (StatusCode::BAD_REQUEST, "bad body").into_response(),
                    }
                }),
            )
            // Never touches its body.
            .route("/idle", get(|| async { "never read" }))
            .layer(axum::middleware::from_fn_with_state(
                BodyGuard {
                    limits: BodyReadLimits::PRODUCTION,
                    render,
                },
                guard_request_body,
            ))
    }

    use axum::response::IntoResponse;

    fn request(method: &str, uri: &str, body: Fed) -> Request {
        Request::builder()
            .method(method)
            .uri(uri)
            .body(Body::new(body))
            .unwrap()
    }

    #[tokio::test(start_paused = true)]
    async fn a_breach_is_answered_408_with_connection_close_whatever_the_handler_said() {
        let resp = app()
            .oneshot(request(
                "POST",
                "/echo",
                feed(vec![(S(29), 1); 40], Duration::ZERO),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::REQUEST_TIMEOUT);
        assert_eq!(resp.headers()[header::CONNECTION], "close");
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(&body[..], b"too slow");
    }

    #[tokio::test(start_paused = true)]
    async fn a_body_within_bounds_reaches_the_handler_untouched() {
        let resp = app()
            .oneshot(request(
                "POST",
                "/echo",
                feed(vec![(S(1), 3_000); 4], Duration::ZERO),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert!(resp.headers().get(header::CONNECTION).is_none());
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(body.len(), 12_000);
    }

    #[tokio::test(start_paused = true)]
    async fn a_request_whose_body_is_never_read_is_never_timed() {
        // The WebSocket case: the handler answers without polling the body,
        // however long the client would take to send one.
        let resp = app()
            .oneshot(request("GET", "/idle", feed(vec![], S(3_600))))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[test]
    fn service_unavailable_carries_retry_after() {
        let resp = service_unavailable(Response::new(Body::empty()));
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(resp.headers()[header::RETRY_AFTER], "30");
    }

    #[test]
    fn request_timeout_carries_connection_close() {
        let resp = request_timeout(Response::new(Body::empty()));
        assert_eq!(resp.status(), StatusCode::REQUEST_TIMEOUT);
        assert_eq!(resp.headers()[header::CONNECTION], "close");
    }
}
