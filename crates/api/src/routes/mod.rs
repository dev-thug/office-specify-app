//! Route aggregation (spec §4.4, §6). Each module exposes
//! `pub fn router() -> OpenApiRouter<AppState>` — module workers extend only
//! their own file; this registry is pre-wired.

pub mod agents;
pub mod approvals;
pub mod booking;
pub mod campaigns;
pub mod compliance;
pub mod crm;
pub mod imports;
pub mod inbox;
pub mod mailboxes;
pub mod marketing;
pub mod operations;
pub mod org;
pub mod research;
pub mod sending;
pub mod templates;
pub mod zoho_templates;

use utoipa_axum::router::OpenApiRouter;

use crate::state::AppState;

/// All `/api/v1` routes (internal, authenticated).
pub fn api_router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .merge(org::router())
        .merge(mailboxes::router())
        .merge(marketing::router())
        .merge(zoho_templates::router())
        .merge(crm::router())
        .merge(research::router())
        .merge(imports::router())
        .merge(templates::router())
        .merge(campaigns::router())
        .merge(approvals::router())
        .merge(sending::router())
        .merge(inbox::router())
        .merge(booking::router())
        .merge(compliance::router())
        .merge(agents::router())
        .merge(operations::router())
}

/// `/public/v1` — token-capability endpoints (unsubscribe, booking).
/// Module workers merge their public routes here.
pub fn public_router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .merge(booking::public_router())
        .merge(compliance::public_router())
}

/// `/webhooks/v1` — provider webhook ingress (inbox/sending workers fill).
pub fn webhook_router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new().merge(inbox::webhook_router())
}
