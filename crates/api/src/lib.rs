//! Axum app assembly: extractors, problem+json errors, per-module
//! `OpenApiRouter`s aggregated into `/api/v1`, `/public/v1`, `/webhooks/v1`
//! (spec §4.4). OpenAPI paths aggregate from module routers — there is no
//! central handwritten path list.

pub mod cookies;
pub mod extractors;
pub mod middleware;
pub mod problem;
pub mod routes;
pub mod state;
pub mod testing;

use axum::extract::DefaultBodyLimit;
use axum::response::IntoResponse;
use contracts::AppError;
use serde::Deserialize;
use utoipa_axum::router::OpenApiRouter;

pub use state::AppState;

/// Full router + OpenAPI document (before state binding). `dev_endpoints`
/// controls whether the fake-provider injection route is registered
/// (APP_ENV dev/test, spec §4.9).
pub fn build(dev_endpoints: bool) -> (axum::Router<AppState>, utoipa::openapi::OpenApi) {
    let router = OpenApiRouter::<AppState>::new()
        .nest("/api/v1", routes::api_router())
        .nest("/public/v1", routes::public_router())
        .nest("/webhooks/v1", routes::webhook_router());
    let router = if dev_endpoints {
        router.routes(utoipa_axum::routes!(dev_fake_mail_inbound))
    } else {
        router
    };
    router.split_for_parts()
}

/// OpenAPI document only (CLI `openapi` command) — no state required.
/// Excludes dev endpoints.
pub fn openapi_document() -> utoipa::openapi::OpenApi {
    build(false).1
}

/// Router bound to `state`, with middleware + 1MB body limit (spec §4.4).
pub fn build_app(state: AppState) -> axum::Router {
    let (router, _openapi) = build(state.config.dev_endpoints);
    router
        .layer(DefaultBodyLimit::max(1024 * 1024))
        .layer(axum::middleware::from_fn(middleware::request_id))
        .with_state(state)
}

// ---- dev-only fake provider endpoint (spec §4.9) -------------------------

#[derive(Debug, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct FakeInboundRequest {
    pub from: String,
    pub to: Vec<String>,
    pub subject: Option<String>,
    pub body_text: Option<String>,
    pub in_reply_to: Option<String>,
}

/// `POST /api/v1/dev/fake-mail/inbound` — injects a message into the
/// in-memory Fake mail provider. Registered only when `APP_ENV` is dev/test
/// (`config.dev_endpoints`); the handler also double-checks.
#[utoipa::path(
    post, path = "/api/v1/dev/fake-mail/inbound",
    request_body = FakeInboundRequest,
    responses(
        (status = 200, description = "Injected"),
        (status = 404, description = "Not a dev/test environment", body = contracts::Problem),
    )
)]
async fn dev_fake_mail_inbound(
    axum::extract::State(state): axum::extract::State<AppState>,
    axum::Json(req): axum::Json<FakeInboundRequest>,
) -> Result<impl IntoResponse, problem::ProblemError> {
    if !state.config.dev_endpoints {
        return Err(problem::ProblemError(AppError::not_found("not found")));
    }
    let Some(fake) = &state.fake_mail else {
        return Err(problem::ProblemError(AppError::not_found(
            "fake provider not configured",
        )));
    };
    fake.inject_inbound(infrastructure::providers::ProviderMessage {
        provider_message_id: format!("fake-in-{}", uuid::Uuid::new_v4()),
        provider_thread_id: req.in_reply_to.clone(),
        internet_message_id: Some(format!("<{}@fake>", uuid::Uuid::new_v4())),
        in_reply_to: req.in_reply_to,
        references: vec![],
        from: req.from,
        to: req.to,
        subject: req.subject,
        body_text: req.body_text,
        body_html: None,
        message_at: chrono::Utc::now(),
    });
    Ok(axum::Json(serde_json::json!({"injected": true})))
}
