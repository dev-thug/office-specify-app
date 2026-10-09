//! Company activity timeline (B.2 `GET /companies/{id}/activities`).
//! Mail-derived rows are mailbox-grant filtered inside the repo SQL (§5.6 —
//! existence is hidden, not just contents).

use contracts::crm::ActivityResponse;
use contracts::{AppError, Ctx, Page};
use infrastructure::db::Db;
use infrastructure::repo::crm;
use uuid::Uuid;

use super::require_crm_read;
use super::util;

#[derive(Debug, Default)]
pub struct ActivityListQuery {
    pub limit: u32,
    pub cursor: Option<String>,
}

/// `GET /companies/{id}/activities` — keyset over `occurred_at`.
pub async fn list_company(
    db: &Db,
    ctx: &Ctx,
    hmac_key: &[u8],
    company_id: Uuid,
    q: &ActivityListQuery,
) -> Result<(Vec<ActivityResponse>, Page), AppError> {
    require_crm_read(ctx)?;
    let filters = serde_json::json!({
        "endpoint": "GET /api/v1/companies/{id}/activities",
        "company_id": company_id,
        "limit": q.limit,
    });
    let after = util::decode_cursor(q.cursor.as_deref(), hmac_key, ctx, &filters)?;
    let mut tx = db.begin(ctx.workspace_id).await?;
    crm::get_company(&mut tx, company_id)
        .await?
        .ok_or_else(|| AppError::not_found("company not found"))?;
    let mut rows =
        crm::list_company_activities(&mut tx, company_id, ctx.principal_id, after, q.limit + 1)
            .await?;
    tx.commit().await?;
    let page = util::page_for(&mut rows, q.limit, hmac_key, ctx, &filters, |r| {
        (r.occurred_at, r.id)
    });
    Ok((rows.iter().map(util::activity_dto).collect(), page))
}
