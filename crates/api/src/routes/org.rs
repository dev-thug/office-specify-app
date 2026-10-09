//! Org module routes (spec §6): auth (§4.5/X1/X2) + `/me`. The module
//! worker extends this file for principals/credentials/emergency-stop.

use crate::problem::ProblemError;
use axum::extract::ConnectInfo;
use axum::http::{HeaderMap, header};
use axum::response::IntoResponse;
use contracts::org::{
    AcceptInvitationRequest, InvitationPreviewResponse, LoginRequest, MeResponse,
};
use contracts::{AppError, AuthMethod, Envelope, Meta, Problem};
use std::net::SocketAddr;
use std::time::Duration;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::cookies;
use crate::extractors::{AuthCtx, Json};
use crate::state::AppState;

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(login))
        .routes(routes!(logout))
        .routes(routes!(invitation_preview))
        .routes(routes!(invitation_accept))
        .routes(routes!(me))
}

fn envelope<T: serde::Serialize>(data: T, request_id: &str) -> Envelope<T> {
    Envelope {
        data,
        meta: Meta {
            request_id: request_id.to_string(),
        },
        page: None,
    }
}

fn auth_cookies(state: &AppState, session: &str, csrf: &str) -> [axum::http::HeaderValue; 2] {
    let ttl = state.config.session_ttl.as_secs() as i64;
    [
        cookies::session(session, state.config.cookie_secure, ttl),
        cookies::csrf(csrf, state.config.cookie_secure, ttl),
    ]
}

/// Login rate limit: 10 attempts/minute per client IP, fail closed when
/// Redis is configured (spec §16.3); no limiter configured → skipped.
async fn login_rate_limit(state: &AppState, addr: &SocketAddr) -> Result<(), AppError> {
    if let Some(rl) = &state.rate_limiter {
        rl.require(
            &format!("login:{}", addr.ip()),
            10,
            Duration::from_secs(60),
            infrastructure::ratelimit::FailMode::Closed,
        )
        .await?;
    }
    Ok(())
}

#[utoipa::path(
    post, path = "/auth/login",
    request_body = LoginRequest,
    responses(
        (status = 200, description = "Authenticated; sets outreach_session + outreach_csrf cookies", body = MeResponse),
        (status = 401, description = "Invalid credentials", body = Problem),
        (status = 429, description = "Account locked / rate limited", body = Problem),
    )
)]
async fn login(
    axum::extract::State(state): axum::extract::State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    request_id: crate::extractors::RequestIdHeader,
    Json(req): Json<LoginRequest>,
) -> Result<impl IntoResponse, ProblemError> {
    login_rate_limit(&state, &addr).await?;
    let workspace_id = state
        .db
        .default_workspace_id()
        .ok_or_else(|| AppError::internal("DEFAULT_WORKSPACE_ID not configured"))?;
    let out = application::org::auth::login(
        &state.db,
        workspace_id,
        chrono::Duration::from_std(state.config.session_ttl).unwrap(),
        &req.email,
        &req.password,
        &request_id.0,
    )
    .await?;
    let [sc, cc] = auth_cookies(&state, &out.session_token, &out.csrf_token);
    let mut headers = HeaderMap::new();
    headers.append(header::SET_COOKIE, sc);
    headers.append(header::SET_COOKIE, cc);
    Ok((headers, axum::Json(envelope(out.me, &request_id.0))))
}

#[utoipa::path(
    post, path = "/auth/logout",
    responses(
        (status = 204, description = "Session revoked"),
        (status = 401, description = "Unauthenticated", body = Problem),
        (status = 403, description = "CSRF failure", body = Problem),
    )
)]
async fn logout(
    axum::extract::State(state): axum::extract::State<AppState>,
    auth: AuthCtx,
) -> Result<impl IntoResponse, ProblemError> {
    let ctx = auth.into_inner();
    let session_id = match ctx.auth {
        AuthMethod::Session { session_id } => session_id,
        _ => {
            return Err(ProblemError(AppError::validation(
                "logout requires a session",
                vec![],
            )));
        }
    };
    application::org::auth::logout(&state.db, &ctx, session_id).await?;
    let [sc, cc] = cookies::clear(state.config.cookie_secure);
    let mut headers = HeaderMap::new();
    headers.append(header::SET_COOKIE, sc);
    headers.append(header::SET_COOKIE, cc);
    Ok((axum::http::StatusCode::NO_CONTENT, headers))
}

#[utoipa::path(
    get, path = "/auth/invitations/{token}",
    params(("token" = String, Path, description = "One-time invitation token")),
    responses(
        (status = 200, description = "Invitation preview", body = InvitationPreviewResponse),
        (status = 404, description = "Not found or expired", body = Problem),
    )
)]
async fn invitation_preview(
    axum::extract::State(state): axum::extract::State<AppState>,
    request_id: crate::extractors::RequestIdHeader,
    axum::extract::Path(token): axum::extract::Path<String>,
) -> Result<axum::Json<Envelope<InvitationPreviewResponse>>, ProblemError> {
    let res = application::org::auth::invitation_preview(&state.db, &token).await?;
    Ok(axum::Json(envelope(res, &request_id.0)))
}

#[utoipa::path(
    post, path = "/auth/invitations/{token}/accept",
    params(("token" = String, Path)),
    request_body = AcceptInvitationRequest,
    responses(
        (status = 200, description = "Account created; session cookies set", body = MeResponse),
        (status = 404, description = "Invitation not found or expired", body = Problem),
        (status = 400, description = "Validation failed", body = Problem),
    )
)]
async fn invitation_accept(
    axum::extract::State(state): axum::extract::State<AppState>,
    request_id: crate::extractors::RequestIdHeader,
    axum::extract::Path(token): axum::extract::Path<String>,
    Json(req): Json<AcceptInvitationRequest>,
) -> Result<impl IntoResponse, ProblemError> {
    let out = application::org::auth::accept_invitation(
        &state.db,
        chrono::Duration::from_std(state.config.session_ttl).unwrap(),
        &token,
        &req.display_name,
        &req.password,
        &request_id.0,
    )
    .await?;
    let [sc, cc] = auth_cookies(&state, &out.session_token, &out.csrf_token);
    let mut headers = HeaderMap::new();
    headers.append(header::SET_COOKIE, sc);
    headers.append(header::SET_COOKIE, cc);
    Ok((headers, axum::Json(envelope(out.me, &request_id.0))))
}

#[utoipa::path(
    get, path = "/me",
    responses(
        (status = 200, description = "Current principal", body = MeResponse),
        (status = 401, description = "Unauthenticated", body = Problem),
    )
)]
async fn me(
    axum::extract::State(state): axum::extract::State<AppState>,
    auth: AuthCtx,
) -> Result<axum::Json<Envelope<MeResponse>>, ProblemError> {
    let ctx = auth.into_inner();
    let request_id = ctx.request_id.clone();
    let mut tx = state.db.begin(ctx.workspace_id).await?;
    let principal = infrastructure::repo::auth::get_principal(&mut tx, ctx.principal_id)
        .await?
        .ok_or_else(|| AppError::not_found("principal not found"))?;
    tx.commit().await?;
    let res = application::org::me::me_for_principal(&state.db, &principal).await?;
    Ok(axum::Json(envelope(res, &request_id)))
}
