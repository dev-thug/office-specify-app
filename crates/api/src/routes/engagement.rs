//! Public request-based engagement endpoints for frozen outbound messages.

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use contracts::ErrorCode;
use std::{
    future::Future,
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::extractors::RequestIdHeader;
use crate::state::AppState;

const OPEN_REQUESTS_PER_CAPABILITY_PER_MINUTE: u32 = 120;
const CLICK_REQUESTS_PER_CAPABILITY_PER_MINUTE: u32 = 60;
const LOCAL_REQUEST_BURST: usize = 240;
const LOCAL_REQUEST_REFILL_PER_SECOND: f64 = 24.0;

const TRANSPARENT_PIXEL: &[u8] = &[
    71, 73, 70, 56, 57, 97, 1, 0, 1, 0, 128, 0, 0, 0, 0, 0, 255, 255, 255, 33, 249, 4, 1, 0, 0, 0,
    0, 44, 0, 0, 0, 0, 1, 0, 1, 0, 0, 2, 2, 68, 1, 0, 59,
];

#[derive(Debug, Clone, Copy)]
enum RequestKind {
    Open,
    Click,
}

impl RequestKind {
    fn redis_label(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Click => "click",
        }
    }

    fn redis_limit(self) -> u32 {
        match self {
            Self::Open => OPEN_REQUESTS_PER_CAPABILITY_PER_MINUTE,
            Self::Click => CLICK_REQUESTS_PER_CAPABILITY_PER_MINUTE,
        }
    }
}

struct TokenBucket {
    tokens: f64,
    last_refill: Instant,
}

impl TokenBucket {
    fn new(now: Instant) -> Self {
        Self {
            tokens: LOCAL_REQUEST_BURST as f64,
            last_refill: now,
        }
    }

    fn try_take(&mut self, now: Instant) -> Result<(), Duration> {
        let elapsed = now
            .saturating_duration_since(self.last_refill)
            .as_secs_f64();
        if elapsed > 0.0 {
            self.tokens = (self.tokens + elapsed * LOCAL_REQUEST_REFILL_PER_SECOND)
                .min(LOCAL_REQUEST_BURST as f64);
            self.last_refill = now.max(self.last_refill);
        }
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            Ok(())
        } else {
            let seconds = ((1.0 - self.tokens) / LOCAL_REQUEST_REFILL_PER_SECOND).max(0.001);
            Err(Duration::from_secs_f64(seconds))
        }
    }
}

struct LocalRequestLimiter {
    open: TokenBucket,
    click: TokenBucket,
}

impl LocalRequestLimiter {
    fn new(now: Instant) -> Self {
        Self {
            open: TokenBucket::new(now),
            click: TokenBucket::new(now),
        }
    }

    fn try_take(&mut self, kind: RequestKind, now: Instant) -> Result<(), Duration> {
        match kind {
            RequestKind::Open => self.open.try_take(now),
            RequestKind::Click => self.click.try_take(now),
        }
    }
}

static PROCESS_LOCAL_LIMITER: OnceLock<Mutex<LocalRequestLimiter>> = OnceLock::new();

/// Process-local pre-lookup guard. It has exactly two buckets regardless of
/// token volume and does not depend on Redis or request metadata.
fn process_local_request_budget(kind: RequestKind) -> Result<(), Duration> {
    let now = Instant::now();
    let limiter = PROCESS_LOCAL_LIMITER.get_or_init(|| Mutex::new(LocalRequestLimiter::new(now)));
    limiter
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .try_take(kind, now)
}

/// Keep resolver futures unpolled until the process-local budget admits the
/// request. This seam also makes the no-lookup guarantee deterministic in
/// tests without a database or sleeps.
async fn with_request_budget<T>(
    budget: Result<(), Duration>,
    lookup: impl Future<Output = T>,
) -> Result<T, Duration> {
    budget?;
    Ok(lookup.await)
}

fn retry_after_seconds(wait: Duration) -> u32 {
    wait.as_secs_f64().ceil().clamp(1.0, u32::MAX as f64) as u32
}

fn no_cache(mut response: Response) -> Response {
    let headers = response.headers_mut();
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("no-store, no-cache, must-revalidate, max-age=0"),
    );
    headers.insert(header::PRAGMA, HeaderValue::from_static("no-cache"));
    headers.insert(
        header::EXPIRES,
        HeaderValue::from_static("Thu, 01 Jan 1970 00:00:00 GMT"),
    );
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    headers.insert(
        "x-robots-tag",
        HeaderValue::from_static("noindex, nofollow"),
    );
    response
}

fn pixel_response() -> Response {
    let mut response = Response::new(Body::from(TRANSPARENT_PIXEL));
    *response.status_mut() = StatusCode::OK;
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static("image/gif"));
    response.headers_mut().insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    no_cache(response)
}

fn click_rate_limited_response(request_id: &str, wait: Duration) -> Response {
    let retry_after = retry_after_seconds(wait);
    let problem = contracts::AppError::new(
        ErrorCode::RateLimited,
        "public engagement request rate limit exceeded",
    )
    .with_retry_after(retry_after)
    .to_problem(request_id, "/public/v1/engagement/click");
    let mut response = (StatusCode::TOO_MANY_REQUESTS, axum::Json(problem)).into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(crate::problem::PROBLEM_CONTENT_TYPE),
    );
    response.headers_mut().insert(
        header::RETRY_AFTER,
        HeaderValue::from_str(&retry_after.to_string())
            .unwrap_or_else(|_| HeaderValue::from_static("1")),
    );
    response.headers_mut().insert(
        "x-request-id",
        HeaderValue::from_str(request_id).unwrap_or_else(|_| HeaderValue::from_static("")),
    );
    no_cache(response)
}

/// Apply an optional Redis limit only after resolving a real capability. The
/// fixed-window key is a hash of the internal capability ID, never a raw URL
/// token, IP address, or user-agent. Redis outages fail open for public GETs;
/// the database still caps stored counts per UTC day.
async fn public_request_limit(
    state: &AppState,
    capability_id: uuid::Uuid,
    request_kind: RequestKind,
) -> Result<(), Duration> {
    let Some(rate_limiter) = &state.rate_limiter else {
        return Ok(());
    };
    let key_hash = infrastructure::crypto::sha256_hex(capability_id.as_bytes());
    match rate_limiter
        .require(
            &format!("engagement:{}:{key_hash}", request_kind.redis_label()),
            request_kind.redis_limit(),
            Duration::from_secs(60),
            infrastructure::ratelimit::FailMode::Open,
        )
        .await
    {
        Ok(_) => Ok(()),
        Err(error) if error.code == ErrorCode::RateLimited => Err(Duration::from_secs(u64::from(
            error.retry_after.unwrap_or(1).max(1),
        ))),
        // Redis errors fail open in this public read path. The local bucket
        // and bounded daily DB counters remain active.
        Err(_) => Ok(()),
    }
}

/// Public requests are intentionally indistinguishable for active and
/// unknown capabilities. Counts describe proxy/scanner-sensitive requests,
/// never proof that a person read or received a message.
#[utoipa::path(
    get,
    path = "/engagement/open/{token}",
    params(("token" = String, Path, description = "Opaque outbound-bound tracking capability")),
    responses(
        (status = 200, description = "No-cache one-pixel image", content_type = "image/gif")
    )
)]
async fn open_pixel(State(state): State<AppState>, Path(token): Path<String>) -> Response {
    let location = match with_request_budget(
        process_local_request_budget(RequestKind::Open),
        application::engagement::resolve_capability(&state.db, &token),
    )
    .await
    {
        Err(_) => return pixel_response(),
        Ok(Ok(location)) => location,
        Ok(Err(_)) => return no_cache(StatusCode::SERVICE_UNAVAILABLE.into_response()),
    };
    if let Some(location) = location {
        if public_request_limit(&state, location.capability_id, RequestKind::Open)
            .await
            .is_err()
        {
            return pixel_response();
        }
        if application::engagement::record_open_request_at(&state.db, location)
            .await
            .is_err()
        {
            return no_cache(StatusCode::SERVICE_UNAVAILABLE.into_response());
        }
    }

    pixel_response()
}

/// The route accepts no redirect destination. Its only Location value comes
/// from the immutable target stored with this token at freeze time.
#[utoipa::path(
    get,
    path = "/engagement/click/{token}",
    params(("token" = String, Path, description = "Opaque outbound-bound tracking capability")),
    responses(
        (status = 302, description = "Redirect to the frozen HTTP(S) destination"),
        (status = 404, description = "Unknown or invalid capability"),
        (status = 429, description = "Public request rate limit", body = contracts::Problem),
    )
)]
async fn click_redirect(
    State(state): State<AppState>,
    Path(token): Path<String>,
    RequestIdHeader(request_id): RequestIdHeader,
) -> Response {
    let location = match with_request_budget(
        process_local_request_budget(RequestKind::Click),
        application::engagement::resolve_capability(&state.db, &token),
    )
    .await
    {
        Err(wait) => return click_rate_limited_response(&request_id, wait),
        Ok(Ok(Some(location))) => location,
        Ok(Ok(None)) => return no_cache(StatusCode::NOT_FOUND.into_response()),
        Ok(Err(_)) => return no_cache(StatusCode::SERVICE_UNAVAILABLE.into_response()),
    };
    if let Err(wait) =
        public_request_limit(&state, location.capability_id, RequestKind::Click).await
    {
        return click_rate_limited_response(&request_id, wait);
    }
    let target = match application::engagement::record_click_request_at(&state.db, location).await {
        Ok(Some(target)) => target,
        Ok(None) => return no_cache(StatusCode::NOT_FOUND.into_response()),
        Err(_) => return no_cache(StatusCode::SERVICE_UNAVAILABLE.into_response()),
    };
    let Ok(location) = HeaderValue::from_str(&target) else {
        return no_cache(StatusCode::NOT_FOUND.into_response());
    };
    let mut response = Response::new(Body::empty());
    *response.status_mut() = StatusCode::FOUND;
    response.headers_mut().insert(header::LOCATION, location);
    no_cache(response)
}

/// `/public/v1` capability URLs are deliberately unauthenticated and never
/// accept a caller-controlled redirect target.
pub fn public_router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(open_pixel))
        .routes(routes!(click_redirect))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;

    #[test]
    fn local_request_buckets_are_bounded_per_route_and_refill_deterministically() {
        let start = Instant::now();
        let mut limiter = LocalRequestLimiter::new(start);
        for _ in 0..LOCAL_REQUEST_BURST {
            assert!(limiter.try_take(RequestKind::Open, start).is_ok());
        }
        let retry_after = limiter.try_take(RequestKind::Open, start).unwrap_err();
        assert!(retry_after > Duration::ZERO);

        assert!(
            limiter.try_take(RequestKind::Click, start).is_ok(),
            "open traffic must not consume the click bucket"
        );
        assert!(
            limiter
                .try_take(RequestKind::Open, start + Duration::from_millis(42),)
                .is_ok()
        );
    }

    #[tokio::test]
    async fn random_capability_is_rejected_before_the_lookup_future_is_polled() {
        let start = Instant::now();
        let mut limiter = LocalRequestLimiter::new(start);
        for _ in 0..LOCAL_REQUEST_BURST {
            assert!(limiter.try_take(RequestKind::Click, start).is_ok());
        }
        let random_token = "A".repeat(43);
        let mut lookup_called = false;

        let result = with_request_budget(limiter.try_take(RequestKind::Click, start), async {
            lookup_called = true;
            random_token
        })
        .await;

        let response = match result {
            Err(wait) => click_rate_limited_response("request-456", wait),
            Ok(_) => panic!("a depleted request bucket must reject before lookup"),
        };
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(
            response
                .headers()
                .get(header::RETRY_AFTER)
                .and_then(|value| value.to_str().ok()),
            Some("1")
        );
        assert!(!lookup_called, "rate exhaustion must skip token lookup");
    }

    #[tokio::test]
    async fn open_bucket_exhaustion_skips_lookup_and_returns_the_gif() {
        let start = Instant::now();
        let mut limiter = LocalRequestLimiter::new(start);
        for _ in 0..LOCAL_REQUEST_BURST {
            assert!(limiter.try_take(RequestKind::Open, start).is_ok());
        }
        let random_token = "B".repeat(43);
        let mut lookup_called = false;

        let result = with_request_budget(limiter.try_take(RequestKind::Open, start), async {
            lookup_called = true;
            random_token
        })
        .await;
        let response = match result {
            Err(_) => pixel_response(),
            Ok(_) => panic!("a depleted request bucket must skip open lookup"),
        };

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
            Some("image/gif")
        );
        assert!(
            response
                .headers()
                .get(header::CACHE_CONTROL)
                .and_then(|value| value.to_str().ok())
                .is_some_and(|value| value.contains("no-store"))
        );
        assert!(!lookup_called);
    }

    #[tokio::test]
    async fn click_rate_limit_response_has_problem_and_retry_after_headers() {
        let response = click_rate_limited_response("request-123", Duration::from_millis(42));

        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(
            response
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
            Some(crate::problem::PROBLEM_CONTENT_TYPE)
        );
        assert_eq!(
            response
                .headers()
                .get(header::RETRY_AFTER)
                .and_then(|value| value.to_str().ok()),
            Some("1")
        );
        assert_eq!(
            response
                .headers()
                .get("x-request-id")
                .and_then(|value| value.to_str().ok()),
            Some("request-123")
        );
        assert!(
            response
                .headers()
                .get(header::CACHE_CONTROL)
                .and_then(|value| value.to_str().ok())
                .is_some_and(|value| value.contains("no-store"))
        );
        assert_eq!(
            response
                .headers()
                .get(header::REFERRER_POLICY)
                .and_then(|value| value.to_str().ok()),
            Some("no-referrer")
        );
        assert_eq!(
            response
                .headers()
                .get("x-robots-tag")
                .and_then(|value| value.to_str().ok()),
            Some("noindex, nofollow")
        );
        let body = to_bytes(response.into_body(), 2048).await.unwrap();
        let problem: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(problem["status"], 429);
        assert_eq!(problem["code"], "RATE_LIMITED");
        assert_eq!(problem["request_id"], "request-123");
        assert_eq!(problem["instance"], "/public/v1/engagement/click");
    }
}
