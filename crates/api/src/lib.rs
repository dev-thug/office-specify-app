//! Axum app assembly: extractors, problem+json errors, per-module
//! `OpenApiRouter`s aggregated into `/api/v1`, `/public/v1`, `/webhooks/v1`
//! (spec §4.4). OpenAPI paths aggregate from module routers — there is no
//! central handwritten path list.

pub mod cookies;
pub mod extractors;
/// Frozen path alias: module workers import `api::extract::PageParams`
/// (and friends) — the implementation lives in `extractors`.
pub use extractors as extract;
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
pub fn openapi_document() -> serde_json::Value {
    let mut document = serde_json::to_value(build(false).1).expect("OpenAPI serializes");
    normalize_wire_schemas(&mut document);
    document
}

/// Route annotations describe domain DTOs. The public wire contract wraps
/// successful JSON DTOs in the shared envelope and serves failures as RFC
/// problem+json; generated clients must see that exact representation.
fn normalize_wire_schemas(document: &mut serde_json::Value) {
    let Some(paths) = document
        .get_mut("paths")
        .and_then(serde_json::Value::as_object_mut)
    else {
        return;
    };
    for path in paths.values_mut() {
        let Some(operations) = path.as_object_mut() else {
            continue;
        };
        for operation in operations.values_mut() {
            let Some(responses) = operation
                .get_mut("responses")
                .and_then(serde_json::Value::as_object_mut)
            else {
                continue;
            };
            for (status, response) in responses {
                let Some(content) = response
                    .get_mut("content")
                    .and_then(serde_json::Value::as_object_mut)
                else {
                    continue;
                };
                if status.starts_with('2') && status != "204" {
                    if let Some(media) = content.get_mut("application/json")
                        && let Some(schema) = media.get_mut("schema")
                    {
                        let data = schema.take();
                        *schema = serde_json::json!({
                            "type":"object", "required":["data","meta"],
                            "properties": {
                                "data": data,
                                "meta": {"type":"object", "required":["request_id"], "properties":{"request_id":{"type":"string"}}},
                                "page": {"type":"object", "required":["next_cursor","has_more"], "properties":{
                                    "next_cursor":{"type":["string","null"]}, "has_more":{"type":"boolean"}
                                }}
                            }
                        });
                    }
                } else if status.parse::<u16>().is_ok_and(|code| code >= 400)
                    && let Some(problem) = content.remove("application/json")
                {
                    content.insert("application/problem+json".into(), problem);
                }
            }
        }
    }
}

#[cfg(test)]
mod wire_schema_tests {
    #[test]
    fn exported_openapi_matches_envelope_and_problem_media_types() {
        let document = serde_json::to_value(super::openapi_document()).unwrap();
        let success = &document["paths"]["/api/v1/me"]["get"]["responses"]["200"]["content"]["application/json"]
            ["schema"];
        assert_eq!(success["required"], serde_json::json!(["data", "meta"]));
        assert_eq!(
            success["properties"]["data"]["$ref"],
            "#/components/schemas/MeResponse"
        );
        assert!(
            document["paths"]["/api/v1/me"]["get"]["responses"]["401"]["content"]
                .get("application/problem+json")
                .is_some()
        );
    }
}

/// Router bound to `state`, with middleware + 1MB body limit (spec §4.4).
pub fn build_app(state: AppState) -> axum::Router {
    let (router, _openapi) = build(state.config.dev_endpoints);
    router
        .layer(DefaultBodyLimit::max(1024 * 1024))
        .layer(axum::middleware::from_fn(middleware::protective_headers))
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
