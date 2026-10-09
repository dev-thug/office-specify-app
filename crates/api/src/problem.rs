//! `AppError` → `application/problem+json` responses (B.1.2).
//!
//! Orphan rules prevent implementing `IntoResponse` for `AppError` (both
//! foreign to this crate), so the HTTP boundary wraps it in `ProblemError`.
//! `From<AppError>` keeps handler code natural: `Result<T, ProblemError>`
//! with `?` on any `AppError`-producing call. `request_id`/`instance` are
//! filled by the request-id middleware, which post-processes problem
//! responses (extractor rejections can't see the request in
//! `IntoResponse`).

use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use contracts::AppError;

pub const PROBLEM_CONTENT_TYPE: &str = "application/problem+json";

/// Real error detail stripped from a 5xx problem body (R23) — carried in
/// response extensions so the request-id middleware can log it with the
/// request id.
#[derive(Debug, Clone)]
pub struct InternalErrorDetail(pub String);

/// HTTP-boundary error type — wraps `contracts::AppError` for axum.
#[derive(Debug)]
pub struct ProblemError(pub AppError);

impl From<AppError> for ProblemError {
    fn from(e: AppError) -> Self {
        Self(e)
    }
}

impl From<ProblemError> for AppError {
    fn from(e: ProblemError) -> Self {
        e.0
    }
}

impl std::ops::Deref for ProblemError {
    type Target = AppError;
    fn deref(&self) -> &AppError {
        &self.0
    }
}

impl IntoResponse for ProblemError {
    fn into_response(self) -> Response {
        let err = self.0;
        let status =
            StatusCode::from_u16(err.status()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        let problem = err.to_problem("", "");
        let mut res = (status, axum::Json(problem)).into_response();
        if err.status() >= 500 {
            res.extensions_mut()
                .insert(InternalErrorDetail(err.detail.clone()));
        }
        res.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static(PROBLEM_CONTENT_TYPE),
        );
        if let Some(secs) = err.retry_after
            && let Ok(v) = HeaderValue::from_str(&secs.to_string())
        {
            res.headers_mut().insert(header::RETRY_AFTER, v);
        }
        res
    }
}
