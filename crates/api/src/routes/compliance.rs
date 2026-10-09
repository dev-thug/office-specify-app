//! compliance module routes (spec §6 — owned by the module worker).

use utoipa_axum::router::OpenApiRouter;

use crate::state::AppState;

/// Internal /api/v1 routes.
pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
}

/// Public /public/v1 token-capability routes.
pub fn public_router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
}
