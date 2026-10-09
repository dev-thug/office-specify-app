//! inbox module routes (spec §6 — owned by the module worker).

use utoipa_axum::router::OpenApiRouter;

use crate::state::AppState;

/// Internal /api/v1 routes.
pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
}

/// /webhooks/v1 provider ingress routes.
pub fn webhook_router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
}
