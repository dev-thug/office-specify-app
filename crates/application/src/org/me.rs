//! `GET /me` (B.3.1): principal identity + mailbox list with per-mailbox
//! allowed_actions (grants only — no implicit mailbox access, A.7).

use contracts::AppError;
use contracts::org::{MeMailAccount, MeResponse};
use infrastructure::db::Db;
use infrastructure::error::map_sqlx;
use infrastructure::repo::auth::PrincipalRow;

pub async fn me_for_principal(db: &Db, principal: &PrincipalRow) -> Result<MeResponse, AppError> {
    let mut tx = db.begin(principal.workspace_id).await?;
    let rows: Vec<(uuid::Uuid, String, bool, bool, bool, bool, bool)> = sqlx::query_as(
        "SELECT ma.id, ma.primary_address, g.can_read, g.can_compose, g.can_send,
                g.can_approve, g.can_assign
           FROM mail_account_grants g
           JOIN mail_accounts ma
             ON ma.workspace_id = g.workspace_id AND ma.id = g.mail_account_id
          WHERE g.workspace_id = $1 AND g.principal_id = $2 AND ma.status = 'active'
          ORDER BY ma.primary_address",
    )
    .bind(principal.workspace_id)
    .bind(principal.id)
    .fetch_all(tx.conn())
    .await
    .map_err(map_sqlx)?;
    tx.commit().await?;

    let mail_accounts = rows
        .into_iter()
        .map(|(id, address, r, c, s, a, asg)| MeMailAccount {
            id,
            address,
            // B.3.1 wire strings are the `mail.*` action names.
            allowed_actions: [
                (r, "mail.read"),
                (c, "mail.compose"),
                (s, "mail.send"),
                (a, "mail.approve"),
                (asg, "mail.assign"),
            ]
            .into_iter()
            .filter(|(ok, _)| *ok)
            .map(|(_, name)| name.to_string())
            .collect(),
        })
        .collect();

    Ok(MeResponse {
        principal_id: principal.id,
        kind: principal.principal_kind(),
        role: principal.role(),
        workspace_id: principal.workspace_id,
        display_name: principal.name.clone(),
        permission_version: principal.permission_version,
        mail_accounts,
    })
}
