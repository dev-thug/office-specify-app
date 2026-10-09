//! Request extractors (spec §4.4): `AuthCtx`, `IfMatch`, `IdempotencyKey`,
//! `Json<T>` (deny_unknown_fields DTOs, 1MB limit set at router level).
//!
//! `AuthCtx` resolves `outreach_session` cookie or `Authorization: Bearer
//! osk_…` service credentials — never client-supplied workspace/principal.
//! For session-authenticated non-GET requests it enforces the CSRF
//! double-submit (`X-CSRF-Token` == `outreach_csrf` cookie); bearer requests
//! are exempt.

use axum::http::request::Parts;
use chrono::Utc;
use contracts::{AppError, AuthMethod, Ctx, ErrorCode, PrincipalKind, Scope, ScopeSet};
use infrastructure::repo::auth as auth_repo;
use serde::de::DeserializeOwned;

use crate::middleware::RequestId;
use crate::problem::ProblemError;
use crate::state::AppState;

pub const SESSION_COOKIE: &str = "outreach_session";
pub const CSRF_COOKIE: &str = "outreach_csrf";
pub const CSRF_HEADER: &str = "x-csrf-token";
pub const IDEMPOTENCY_HEADER: &str = "idempotency-key";

fn cookie_value(parts: &Parts, name: &str) -> Option<String> {
    for h in parts.headers.get_all(axum::http::header::COOKIE) {
        let Ok(s) = h.to_str() else { continue };
        for pair in s.split(';') {
            let mut it = pair.trim().splitn(2, '=');
            if let (Some(k), Some(v)) = (it.next(), it.next())
                && k == name
            {
                return Some(v.to_string());
            }
        }
    }
    None
}

fn is_safe_method(method: &axum::http::Method) -> bool {
    matches!(
        *method,
        axum::http::Method::GET | axum::http::Method::HEAD | axum::http::Method::OPTIONS
    )
}

/// Verified request context (spec §4.1).
#[derive(Debug, Clone)]
pub struct AuthCtx(pub Ctx);

impl AuthCtx {
    pub fn ctx(&self) -> &Ctx {
        &self.0
    }
    pub fn into_inner(self) -> Ctx {
        self.0
    }
}

impl axum::extract::FromRequestParts<AppState> for AuthCtx {
    type Rejection = ProblemError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let request_id = parts
            .extensions
            .get::<RequestId>()
            .map(|r| r.0.clone())
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

        // Bearer service credential path (osk_…) — exempt from CSRF.
        if let Some(token) = bearer_token(parts) {
            if !token.starts_with(auth_repo::CREDENTIAL_PREFIX) {
                return Err(ProblemError(AppError::new(
                    ErrorCode::InvalidCredentials,
                    "unsupported bearer credential",
                )));
            }
            let auth = auth_repo::lookup_service_credential(&state.db, &token)
                .await?
                .ok_or_else(|| {
                    AppError::new(
                        ErrorCode::AuthenticationRequired,
                        "invalid or expired credential",
                    )
                })?;
            if auth.principal.status != "active" {
                return Err(ProblemError(AppError::new(
                    ErrorCode::PrincipalDisabled,
                    "principal is disabled",
                )));
            }
            let mut scopes = ScopeSet::from_strings(auth.scopes.iter());
            // Agents may never hold forbidden scopes even if issued (§5.4).
            if auth.principal.principal_kind() == PrincipalKind::Agent {
                for s in Scope::AGENT_FORBIDDEN {
                    scopes = ScopeSet::of(scopes.iter().filter(|x| x != s));
                }
            }
            return Ok(AuthCtx(Ctx {
                request_id,
                workspace_id: auth.principal.workspace_id,
                principal_id: auth.principal.id,
                principal_kind: auth.principal.principal_kind(),
                role: auth.principal.role(),
                scopes,
                permission_version: auth.principal.permission_version,
                auth: AuthMethod::ServiceCredential {
                    credential_id: auth.credential_id,
                },
                agent_run_id: None,
            }));
        }

        let Some(token) = cookie_value(parts, SESSION_COOKIE) else {
            return Err(ProblemError(AppError::new(
                ErrorCode::AuthenticationRequired,
                "authentication required",
            )));
        };
        let auth = auth_repo::lookup_session(&state.db, &token)
            .await?
            .ok_or_else(|| {
                AppError::new(
                    ErrorCode::AuthenticationRequired,
                    "invalid or expired session",
                )
            })?;
        if auth.session.revoked_at.is_some() || auth.session.expires_at <= Utc::now() {
            return Err(ProblemError(AppError::new(
                ErrorCode::AuthenticationRequired,
                "session expired",
            )));
        }
        if auth.principal.status != "active" {
            return Err(ProblemError(AppError::new(
                ErrorCode::PrincipalDisabled,
                "principal is disabled",
            )));
        }

        // CSRF double-submit for session-authenticated mutations.
        if !is_safe_method(&parts.method) {
            let header_tok = parts
                .headers
                .get(CSRF_HEADER)
                .and_then(|v| v.to_str().ok())
                .map(str::to_string);
            let cookie_tok = cookie_value(parts, CSRF_COOKIE);
            match (header_tok, cookie_tok) {
                (Some(h), Some(c)) if h == c => {}
                _ => {
                    return Err(ProblemError(AppError::new(
                        ErrorCode::CsrfTokenInvalid,
                        "missing or invalid CSRF token",
                    )));
                }
            }
        }

        Ok(AuthCtx(Ctx {
            request_id,
            workspace_id: auth.principal.workspace_id,
            principal_id: auth.principal.id,
            principal_kind: auth.principal.principal_kind(),
            role: auth.principal.role(),
            scopes: auth.principal.role().implied_scopes(),
            permission_version: auth.principal.permission_version,
            auth: AuthMethod::Session {
                session_id: auth.session.id,
            },
            agent_run_id: None,
        }))
    }
}

fn bearer_token(parts: &Parts) -> Option<String> {
    let v = parts
        .headers
        .get(axum::http::header::AUTHORIZATION)?
        .to_str()
        .ok()?;
    v.strip_prefix("Bearer ").map(str::to_string)
}

/// Request id from the request-id middleware (for public routes with no
/// AuthCtx).
#[derive(Debug, Clone)]
pub struct RequestIdHeader(pub String);

impl axum::extract::FromRequestParts<AppState> for RequestIdHeader {
    type Rejection = ProblemError;

    async fn from_request_parts(
        parts: &mut Parts,
        _state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        Ok(Self(
            parts
                .extensions
                .get::<RequestId>()
                .map(|r| r.0.clone())
                .unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
        ))
    }
}

/// `If-Match: "<n>"` → expected entity version. Missing → 428
/// VERSION_PRECONDITION_REQUIRED; malformed → 400.
#[derive(Debug, Clone, Copy)]
pub struct IfMatch(pub i64);

impl axum::extract::FromRequestParts<AppState> for IfMatch {
    type Rejection = ProblemError;

    async fn from_request_parts(
        parts: &mut Parts,
        _state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let Some(raw) = parts.headers.get("if-match").and_then(|v| v.to_str().ok()) else {
            return Err(ProblemError(AppError::new(
                ErrorCode::VersionPreconditionRequired,
                "If-Match header is required",
            )));
        };
        let cleaned = raw.trim().trim_start_matches("W/").trim_matches('"');
        cleaned
            .parse::<i64>()
            .map(IfMatch)
            .map_err(|_| ProblemError(AppError::validation("malformed If-Match header", vec![])))
    }
}

/// `Idempotency-Key` required extractor (spec §4.4, ≤128 chars).
#[derive(Debug, Clone)]
pub struct IdempotencyKey(pub String);

impl axum::extract::FromRequestParts<AppState> for IdempotencyKey {
    type Rejection = ProblemError;

    async fn from_request_parts(
        parts: &mut Parts,
        _state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let Some(raw) = parts
            .headers
            .get(IDEMPOTENCY_HEADER)
            .and_then(|v| v.to_str().ok())
        else {
            return Err(ProblemError(AppError::validation(
                "Idempotency-Key header is required",
                vec![],
            )));
        };
        if raw.is_empty() || raw.len() > infrastructure::idempotency::KEY_MAX_LEN {
            return Err(ProblemError(AppError::validation(
                "Idempotency-Key must be 1..=128 characters",
                vec![],
            )));
        }
        Ok(Self(raw.to_string()))
    }
}

/// Optional variant for endpoints where the header is not mandatory.
#[derive(Debug, Clone, Default)]
pub struct OptionalIdempotencyKey(pub Option<String>);

impl axum::extract::FromRequestParts<AppState> for OptionalIdempotencyKey {
    type Rejection = ProblemError;

    async fn from_request_parts(
        parts: &mut Parts,
        _state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        Ok(Self(
            parts
                .headers
                .get(IDEMPOTENCY_HEADER)
                .and_then(|v| v.to_str().ok())
                .map(str::to_string)
                .map(|k| {
                    if k.is_empty() || k.len() > infrastructure::idempotency::KEY_MAX_LEN {
                        Err(ProblemError(AppError::validation(
                            "Idempotency-Key must be 1..=128 characters",
                            vec![],
                        )))
                    } else {
                        Ok(k)
                    }
                })
                .transpose()?,
        ))
    }
}

/// JSON body extractor mapping axum rejections to problem+json 400s.
#[derive(Debug)]
pub struct Json<T>(pub T);

impl<T> axum::extract::FromRequest<AppState> for Json<T>
where
    T: DeserializeOwned,
{
    type Rejection = ProblemError;

    async fn from_request(
        req: axum::extract::Request,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        match axum::Json::<T>::from_request(req, state).await {
            Ok(axum::Json(v)) => Ok(Json(v)),
            Err(rej) => Err(ProblemError(AppError::validation(
                format!("invalid request body: {}", rej.body_text()),
                vec![],
            ))),
        }
    }
}
