//! approvals module routes (spec §6 — owned by the module worker).

use utoipa_axum::router::OpenApiRouter;

use crate::state::AppState;

/// Internal /api/v1 routes for this module.
pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
}
