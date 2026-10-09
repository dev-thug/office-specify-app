//! Request-id + problem post-processing middleware.

use axum::body::Body;
use axum::extract::Request;
use axum::http::HeaderValue;
use axum::middleware::Next;
use axum::response::Response;
use uuid::Uuid;

use crate::problem::PROBLEM_CONTENT_TYPE;

#[derive(Debug, Clone)]
pub struct RequestId(pub String);

/// Assign/propagate `X-Request-Id`, stash it in extensions for extractors,
/// and fill `request_id` + `instance` on problem responses (they cannot see
/// the request inside `IntoResponse`).
pub async fn request_id(mut req: Request, next: Next) -> Response {
    let rid = req
        .headers()
        .get("x-request-id")
        .and_then(|v| v.to_str().ok())
        .filter(|s| !s.is_empty() && s.len() <= 128)
        .map(str::to_string)
        .unwrap_or_else(|| Uuid::new_v4().to_string());
    let instance = req.uri().path().to_string();
    req.extensions_mut().insert(RequestId(rid.clone()));

    let res = next.run(req).await;
    let is_problem = res
        .headers()
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.starts_with(PROBLEM_CONTENT_TYPE))
        .unwrap_or(false);

    let mut res = res;
    res.headers_mut().insert(
        "x-request-id",
        HeaderValue::from_str(&rid).unwrap_or_else(|_| HeaderValue::from_static("")),
    );
    if is_problem {
        let (mut parts, body) = res.into_parts();
        // R23: the real 5xx detail lives only in logs, correlated by
        // request_id — never in the response body.
        if parts.status.is_server_error()
            && let Some(detail) = parts
                .extensions
                .remove::<crate::problem::InternalErrorDetail>()
        {
            tracing::error!(request_id = %rid, detail = %detail.0, "internal error");
        }
        let parts = parts;
        let bytes = axum::body::to_bytes(body, 1 << 20)
            .await
            .unwrap_or_default();
        let body = match serde_json::from_slice::<serde_json::Value>(&bytes) {
            Ok(mut v) => {
                v["request_id"] = serde_json::Value::String(rid.clone());
                v["instance"] = serde_json::Value::String(instance);
                serde_json::to_vec(&v).unwrap_or(bytes.to_vec())
            }
            Err(_) => bytes.to_vec(),
        };
        res = Response::from_parts(parts, Body::from(body));
    }
    res
}

/// Protective response headers (R9, spec §4.4):
/// `Cache-Control: no-store`, `Referrer-Policy: no-referrer`,
/// `X-Robots-Tag: noindex, nofollow` on public-capability and auth/PII
/// routes; `Cache-Control: no-store` on every other API response.
pub async fn protective_headers(req: Request, next: Next) -> Response {
    let path = req.uri().path().to_string();
    let mut res = next.run(req).await;
    let sensitive = path.starts_with("/public/v1/")
        || path.starts_with("/webhooks/v1/")
        || path.starts_with("/api/v1/auth/")
        || path.starts_with("/api/v1/integrations/")
        || path == "/api/v1/me";
    let headers = res.headers_mut();
    headers.insert(
        axum::http::header::CACHE_CONTROL,
        HeaderValue::from_static("no-store"),
    );
    if sensitive {
        headers.insert("referrer-policy", HeaderValue::from_static("no-referrer"));
        headers.insert(
            "x-robots-tag",
            HeaderValue::from_static("noindex, nofollow"),
        );
    }
    res
}
