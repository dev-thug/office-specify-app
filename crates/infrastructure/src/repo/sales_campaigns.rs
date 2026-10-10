//! Repository queries for personalized sales campaigns and their targets.

use chrono::{DateTime, Utc};
use contracts::AppError;
use uuid::Uuid;

use crate::db::Tx;
use crate::error::map_sqlx;

const PRIVACY_LOCK_SEED: i64 = 4_200_042;

/// Serialize campaign CRM bindings with contact erasure. Sales writes and
/// binding reads take the shared transaction lock before row locks; erasure
/// takes the matching exclusive lock before locking contact/email rows.
pub async fn lock_workspace_privacy_shared(tx: &mut Tx<'_>) -> Result<(), AppError> {
    lock_workspace_privacy(tx, true).await
}

pub async fn lock_workspace_privacy_exclusive(tx: &mut Tx<'_>) -> Result<(), AppError> {
    lock_workspace_privacy(tx, false).await
}

async fn lock_workspace_privacy(tx: &mut Tx<'_>, shared: bool) -> Result<(), AppError> {
    let workspace_id = tx
        .workspace_id()
        .ok_or_else(|| {
            AppError::internal("workspace privacy lock requires a workspace transaction")
        })?
        .to_string();
    let sql = if shared {
        "SELECT pg_advisory_xact_lock_shared(hashtextextended($1, $2))"
    } else {
        "SELECT pg_advisory_xact_lock(hashtextextended($1, $2))"
    };
    sqlx::query(sql)
        .bind(workspace_id)
        .bind(PRIVACY_LOCK_SEED)
        .execute(tx.conn())
        .await
        .map_err(map_sqlx)?;
    Ok(())
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct DraftBinding {
    pub sales_campaign_id: Uuid,
    pub target_id: Uuid,
    pub draft_id: Uuid,
    pub current_draft_id: Option<Uuid>,
    pub is_current: bool,
    pub mail_account_id: Uuid,
    pub delivery_channel_id: Uuid,
    pub sender_identity_id: Uuid,
    pub smtp_profile_id: Uuid,
    pub meeting_type_id: Option<Uuid>,
    pub company_id: Option<Uuid>,
    pub contact_id: Option<Uuid>,
    pub email_address_id: Option<Uuid>,
    pub recipient_available: bool,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct OutboundBinding {
    pub sales_campaign_id: Uuid,
    pub target_id: Uuid,
    pub outbound_message_id: Uuid,
    pub source_draft_id: Uuid,
    pub source_credential_id: Option<Uuid>,
    pub current_draft_id: Option<Uuid>,
    pub is_current: bool,
    pub status: String,
    pub not_before: Option<DateTime<Utc>>,
    pub outbound_mail_account_id: Uuid,
    pub outbound_delivery_channel_id: Uuid,
    pub outbound_sender_identity_id: Uuid,
    pub outbound_email_address_id: Uuid,
    pub mail_account_id: Uuid,
    pub delivery_channel_id: Uuid,
    pub sender_identity_id: Uuid,
    pub smtp_profile_id: Uuid,
    pub meeting_type_id: Option<Uuid>,
    pub company_id: Option<Uuid>,
    pub contact_id: Option<Uuid>,
    pub email_address_id: Option<Uuid>,
    pub recipient_available: bool,
}

async fn lock_campaign_target(
    tx: &mut Tx<'_>,
    campaign_id: Uuid,
    target_id: Uuid,
) -> Result<bool, AppError> {
    let campaign: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM sales_campaigns
          WHERE workspace_id = $1 AND id = $2 FOR UPDATE",
    )
    .bind(tx.workspace_id())
    .bind(campaign_id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)?;
    if campaign.is_none() {
        return Ok(false);
    }
    let target: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM sales_campaign_targets
          WHERE workspace_id = $1 AND sales_campaign_id = $2 AND id = $3 FOR UPDATE",
    )
    .bind(tx.workspace_id())
    .bind(campaign_id)
    .bind(target_id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)?;
    Ok(target.is_some())
}

async fn lock_target_recipient_email(
    tx: &mut Tx<'_>,
    email_address_id: Option<Uuid>,
) -> Result<(), AppError> {
    if let Some(email_address_id) = email_address_id {
        // Linked send operations lock campaign → target → outbound first,
        // then the address before evaluating consent/suppression policy.
        let _ = crate::repo::compliance_core::lock_email_address(tx, email_address_id).await?;
    }
    Ok(())
}

pub async fn draft_binding(
    tx: &mut Tx<'_>,
    draft_id: Uuid,
) -> Result<Option<DraftBinding>, AppError> {
    lock_workspace_privacy_shared(tx).await?;
    let parent: Option<(Uuid, Uuid)> = sqlx::query_as(
        "SELECT td.sales_campaign_id, td.target_id
           FROM sales_campaign_target_drafts td
           JOIN sales_campaign_targets t
             ON t.workspace_id = td.workspace_id AND t.sales_campaign_id = td.sales_campaign_id
            AND t.id = td.target_id
          WHERE td.workspace_id = $1 AND td.draft_id = $2",
    )
    .bind(tx.workspace_id())
    .bind(draft_id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)?;
    let Some((campaign_id, target_id)) = parent else {
        return Ok(None);
    };
    if !lock_campaign_target(tx, campaign_id, target_id).await? {
        return Ok(None);
    }
    sqlx::query_as::<_, DraftBinding>(
        "SELECT c.id AS sales_campaign_id, t.id AS target_id, td.draft_id,
                t.current_draft_id, t.current_draft_id = td.draft_id AS is_current,
                c.mail_account_id, c.delivery_channel_id, c.sender_identity_id,
                c.smtp_profile_id, c.meeting_type_id, t.company_id, t.contact_id,
                t.email_address_id,
                COALESCE(t.current_draft_id = td.draft_id
                 AND t.email_address_id IS NOT NULL
                 AND t.company_id IS NOT NULL AND t.contact_id IS NOT NULL
                 AND co.archived_at IS NULL AND ct.archived_at IS NULL
                 AND ct.status IN ('new','active'), false) AS recipient_available
           FROM sales_campaign_target_drafts td
           JOIN sales_campaigns c
             ON c.workspace_id = td.workspace_id AND c.id = td.sales_campaign_id
           JOIN sales_campaign_targets t
             ON t.workspace_id = td.workspace_id AND t.sales_campaign_id = td.sales_campaign_id
            AND t.id = td.target_id
           LEFT JOIN contacts ct
             ON ct.workspace_id = t.workspace_id AND ct.id = t.contact_id
           LEFT JOIN companies co
             ON co.workspace_id = t.workspace_id AND co.id = t.company_id
          WHERE td.workspace_id = $1 AND td.draft_id = $2",
    )
    .bind(tx.workspace_id())
    .bind(draft_id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

pub async fn outbound_binding(
    tx: &mut Tx<'_>,
    outbound_id: Uuid,
) -> Result<Option<OutboundBinding>, AppError> {
    lock_workspace_privacy_shared(tx).await?;
    let parent: Option<(Uuid, Uuid)> = sqlx::query_as(
        "SELECT td.sales_campaign_id, td.target_id
           FROM outbound_messages o
           JOIN sales_campaign_target_drafts td
             ON td.workspace_id = o.workspace_id AND td.draft_id = o.source_draft_id
            AND td.mail_account_id = o.mail_account_id
           JOIN sales_campaign_targets t
             ON t.workspace_id = td.workspace_id AND t.sales_campaign_id = td.sales_campaign_id
            AND t.id = td.target_id
          WHERE o.workspace_id = $1 AND o.id = $2",
    )
    .bind(tx.workspace_id())
    .bind(outbound_id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)?;
    let Some((campaign_id, target_id)) = parent else {
        return Ok(None);
    };
    if !lock_campaign_target(tx, campaign_id, target_id).await? {
        return Ok(None);
    }
    let locked_outbound: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM outbound_messages
          WHERE workspace_id = $1 AND id = $2 FOR UPDATE",
    )
    .bind(tx.workspace_id())
    .bind(outbound_id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)?;
    if locked_outbound.is_none() {
        return Ok(None);
    }
    let email_address_id: Option<Uuid> = sqlx::query_scalar(
        "SELECT email_address_id FROM sales_campaign_targets
          WHERE workspace_id = $1 AND sales_campaign_id = $2 AND id = $3",
    )
    .bind(tx.workspace_id())
    .bind(campaign_id)
    .bind(target_id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)?
    .flatten();
    lock_target_recipient_email(tx, email_address_id).await?;
    sqlx::query_as::<_, OutboundBinding>(
        "SELECT c.id AS sales_campaign_id, t.id AS target_id, o.id AS outbound_message_id,
                o.source_draft_id, o.source_credential_id, t.current_draft_id,
                t.current_draft_id = o.source_draft_id AS is_current,
                o.status, o.not_before,
                o.mail_account_id AS outbound_mail_account_id,
                o.delivery_channel_id AS outbound_delivery_channel_id,
                o.sender_identity_id AS outbound_sender_identity_id,
                o.to_email_address_id AS outbound_email_address_id,
                c.mail_account_id, c.delivery_channel_id, c.sender_identity_id,
                c.smtp_profile_id, c.meeting_type_id, t.company_id, t.contact_id,
                t.email_address_id,
                COALESCE(t.current_draft_id = o.source_draft_id
                 AND t.email_address_id IS NOT NULL
                 AND t.company_id IS NOT NULL AND t.contact_id IS NOT NULL
                 AND co.archived_at IS NULL AND ct.archived_at IS NULL
                 AND ct.status IN ('new','active'), false) AS recipient_available
           FROM outbound_messages o
           JOIN sales_campaign_target_drafts td
             ON td.workspace_id = o.workspace_id AND td.draft_id = o.source_draft_id
            AND td.mail_account_id = o.mail_account_id
           JOIN sales_campaigns c
             ON c.workspace_id = td.workspace_id AND c.id = td.sales_campaign_id
           JOIN sales_campaign_targets t
             ON t.workspace_id = td.workspace_id AND t.sales_campaign_id = td.sales_campaign_id
            AND t.id = td.target_id
           LEFT JOIN contacts ct
             ON ct.workspace_id = t.workspace_id AND ct.id = t.contact_id
           LEFT JOIN companies co
             ON co.workspace_id = t.workspace_id AND co.id = t.company_id
          WHERE o.workspace_id = $1 AND o.id = $2",
    )
    .bind(tx.workspace_id())
    .bind(outbound_id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct SenderProfileRow {
    pub smtp_profile_id: Uuid,
    pub profile_name: String,
    pub mail_account_id: Uuid,
    pub mailbox_name: String,
    pub delivery_channel_id: Uuid,
    pub sender_identity_id: Uuid,
    pub from_address: String,
    pub from_name: String,
    pub reply_to: Option<String>,
    pub profile_status: String,
    pub integration_status: String,
    pub mailbox_status: String,
    pub channel_provider: String,
    pub channel_purpose: String,
    pub channel_enabled: bool,
    pub channel_policy_status: String,
    pub sender_status: String,
    pub has_password: bool,
}

const SENDER_PROFILE_SELECT: &str = "SELECT sc.id AS smtp_profile_id, sc.name AS profile_name,
       sc.mail_account_id, ma.name AS mailbox_name, sc.channel_id AS delivery_channel_id,
       sc.sender_identity_id, sc.from_address, sc.from_name, sc.reply_to,
       sc.status AS profile_status, ic.status AS integration_status,
       ma.status AS mailbox_status, dc.provider AS channel_provider,
       dc.purpose AS channel_purpose, dc.enabled AS channel_enabled,
       dc.policy_status AS channel_policy_status, si.status AS sender_status, sc.has_password
  FROM smtp_connections sc
  JOIN integration_connections ic
    ON ic.workspace_id = sc.workspace_id AND ic.id = sc.id
  JOIN mail_accounts ma
    ON ma.workspace_id = sc.workspace_id AND ma.id = sc.mail_account_id
  JOIN delivery_channels dc
    ON dc.workspace_id = sc.workspace_id AND dc.id = sc.channel_id
   AND dc.connection_id = sc.id AND dc.mail_account_id = sc.mail_account_id
   AND dc.sender_identity_id = sc.sender_identity_id
  JOIN sender_identities si
    ON si.workspace_id = sc.workspace_id AND si.mail_account_id = sc.mail_account_id
   AND si.id = sc.sender_identity_id";

pub async fn get_sender_profile(
    tx: &mut Tx<'_>,
    smtp_profile_id: Uuid,
) -> Result<Option<SenderProfileRow>, AppError> {
    sqlx::query_as::<_, SenderProfileRow>(&format!(
        "{SENDER_PROFILE_SELECT} WHERE sc.workspace_id = $1 AND sc.id = $2"
    ))
    .bind(tx.workspace_id())
    .bind(smtp_profile_id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// Workspace-wide eligible candidates. Deliberately does not filter by the
/// caller's grants: ambiguous senders must be resolved in Settings first.
pub async fn list_eligible_sender_profiles(
    tx: &mut Tx<'_>,
) -> Result<Vec<SenderProfileRow>, AppError> {
    sqlx::query_as::<_, SenderProfileRow>(&format!(
        "{SENDER_PROFILE_SELECT}
          WHERE sc.workspace_id = $1 AND sc.status = 'active' AND sc.has_password
            AND ic.status = 'active' AND ma.status = 'active'
            AND dc.provider = 'delivery_provider' AND dc.purpose = 'marketing'
            AND dc.enabled AND dc.policy_status = 'allowed'
            AND si.status = 'verified'
          ORDER BY sc.created_at, sc.id"
    ))
    .bind(tx.workspace_id())
    .fetch_all(tx.conn())
    .await
    .map_err(map_sqlx)
}

pub async fn get_default_sender_profile_id(tx: &mut Tx<'_>) -> Result<Option<Uuid>, AppError> {
    sqlx::query_scalar(
        "SELECT smtp_profile_id FROM sales_campaign_sender_defaults WHERE workspace_id = $1",
    )
    .bind(tx.workspace_id())
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

pub async fn set_default_sender_profile(
    tx: &mut Tx<'_>,
    smtp_profile_id: Uuid,
    updated_by: Uuid,
) -> Result<(), AppError> {
    sqlx::query(
        "INSERT INTO sales_campaign_sender_defaults
            (workspace_id, smtp_profile_id, updated_by)
         VALUES ($1, $2, $3)
         ON CONFLICT (workspace_id) DO UPDATE
            SET smtp_profile_id = EXCLUDED.smtp_profile_id,
                updated_by = EXCLUDED.updated_by,
                version = sales_campaign_sender_defaults.version + 1,
                updated_at = now()",
    )
    .bind(tx.workspace_id())
    .bind(smtp_profile_id)
    .bind(updated_by)
    .execute(tx.conn())
    .await
    .map_err(map_sqlx)?;
    Ok(())
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct CampaignRow {
    pub id: Uuid,
    pub name: String,
    pub objective: Option<String>,
    pub mail_account_id: Uuid,
    pub delivery_channel_id: Uuid,
    pub sender_identity_id: Uuid,
    pub smtp_profile_id: Uuid,
    pub meeting_type_id: Option<Uuid>,
    pub from_address: String,
    pub from_name: String,
    pub reply_to: Option<String>,
    pub sender_status: String,
    pub sender_ready: bool,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

const CAMPAIGN_SELECT: &str = "SELECT c.id, c.name, c.objective, c.mail_account_id,
       c.delivery_channel_id, c.sender_identity_id, c.smtp_profile_id, c.meeting_type_id,
       sc.from_address, sc.from_name, sc.reply_to, sc.status AS sender_status,
       (sc.status = 'active' AND sc.has_password AND ic.status = 'active'
        AND ma.status = 'active' AND dc.purpose = 'marketing' AND dc.provider = 'delivery_provider'
        AND dc.enabled AND dc.policy_status = 'allowed' AND si.status = 'verified') AS sender_ready,
       c.version, c.created_at, c.updated_at
  FROM sales_campaigns c
  JOIN smtp_connections sc
    ON sc.workspace_id = c.workspace_id AND sc.id = c.smtp_profile_id
   AND sc.mail_account_id = c.mail_account_id AND sc.sender_identity_id = c.sender_identity_id
   AND sc.channel_id = c.delivery_channel_id
  JOIN integration_connections ic ON ic.workspace_id = sc.workspace_id AND ic.id = sc.id
  JOIN mail_accounts ma ON ma.workspace_id = c.workspace_id AND ma.id = c.mail_account_id
  JOIN delivery_channels dc ON dc.workspace_id = c.workspace_id AND dc.id = c.delivery_channel_id
  JOIN sender_identities si ON si.workspace_id = c.workspace_id
   AND si.mail_account_id = c.mail_account_id AND si.id = c.sender_identity_id";

pub async fn list_campaigns_for_principal(
    tx: &mut Tx<'_>,
    principal_id: Uuid,
    after: Option<(DateTime<Utc>, Uuid)>,
    limit: i64,
) -> Result<Vec<CampaignRow>, AppError> {
    sqlx::query_as::<_, CampaignRow>(&format!(
        "{CAMPAIGN_SELECT}
          WHERE c.workspace_id = $1
            AND ($3::timestamptz IS NULL OR c.created_at < $3
                 OR (c.created_at = $3 AND c.id < $4))
            AND EXISTS (
                SELECT 1 FROM mail_account_grants g
                 WHERE g.workspace_id = c.workspace_id
                   AND g.mail_account_id = c.mail_account_id
                   AND g.principal_id = $2 AND g.can_read)
          ORDER BY c.created_at DESC, c.id DESC LIMIT $5"
    ))
    .bind(tx.workspace_id())
    .bind(principal_id)
    .bind(after.map(|cursor| cursor.0))
    .bind(after.map(|cursor| cursor.1).unwrap_or(Uuid::nil()))
    .bind(limit)
    .fetch_all(tx.conn())
    .await
    .map_err(map_sqlx)
}

pub async fn get_campaign(
    tx: &mut Tx<'_>,
    campaign_id: Uuid,
) -> Result<Option<CampaignRow>, AppError> {
    sqlx::query_as::<_, CampaignRow>(&format!(
        "{CAMPAIGN_SELECT} WHERE c.workspace_id = $1 AND c.id = $2"
    ))
    .bind(tx.workspace_id())
    .bind(campaign_id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

pub async fn lock_campaign(
    tx: &mut Tx<'_>,
    campaign_id: Uuid,
) -> Result<Option<CampaignRow>, AppError> {
    let locked: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM sales_campaigns
          WHERE workspace_id = $1 AND id = $2 FOR UPDATE",
    )
    .bind(tx.workspace_id())
    .bind(campaign_id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)?;
    match locked {
        Some(_) => get_campaign(tx, campaign_id).await,
        None => Ok(None),
    }
}

pub async fn insert_campaign(
    tx: &mut Tx<'_>,
    name: &str,
    objective: Option<&str>,
    profile: &SenderProfileRow,
    meeting_type_id: Option<Uuid>,
    owner_principal_id: Uuid,
) -> Result<Uuid, AppError> {
    sqlx::query_scalar(
        "INSERT INTO sales_campaigns
            (workspace_id, name, objective, mail_account_id, delivery_channel_id,
             sender_identity_id, smtp_profile_id, meeting_type_id, owner_principal_id)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9) RETURNING id",
    )
    .bind(tx.workspace_id())
    .bind(name)
    .bind(objective)
    .bind(profile.mail_account_id)
    .bind(profile.delivery_channel_id)
    .bind(profile.sender_identity_id)
    .bind(profile.smtp_profile_id)
    .bind(meeting_type_id)
    .bind(owner_principal_id)
    .fetch_one(tx.conn())
    .await
    .map_err(map_sqlx)
}

pub async fn count_targets(tx: &mut Tx<'_>, campaign_id: Uuid) -> Result<i64, AppError> {
    sqlx::query_scalar(
        "SELECT count(*) FROM sales_campaign_targets
          WHERE workspace_id = $1 AND sales_campaign_id = $2",
    )
    .bind(tx.workspace_id())
    .bind(campaign_id)
    .fetch_one(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// Insert only if company → contact → email all point at one live CRM chain.
pub async fn insert_target(
    tx: &mut Tx<'_>,
    campaign_id: Uuid,
    company_id: Uuid,
    contact_id: Uuid,
    email_address_id: Uuid,
) -> Result<Option<Uuid>, AppError> {
    sqlx::query_scalar(
        "INSERT INTO sales_campaign_targets
            (workspace_id, sales_campaign_id, company_id, contact_id,
             contact_email_id, email_address_id)
         SELECT $1, $2, co.id, ct.id, ce.id, ea.id
           FROM companies co
           JOIN contacts ct ON ct.workspace_id = co.workspace_id
            AND ct.company_id = co.id
           JOIN contact_emails ce ON ce.workspace_id = ct.workspace_id
            AND ce.contact_id = ct.id
           JOIN email_addresses ea ON ea.workspace_id = ce.workspace_id
            AND ea.id = ce.email_address_id
          WHERE co.workspace_id = $1 AND co.id = $3 AND ct.id = $4 AND ea.id = $5
            AND co.archived_at IS NULL AND ct.archived_at IS NULL
            AND ct.status IN ('new','active')
         RETURNING id",
    )
    .bind(tx.workspace_id())
    .bind(campaign_id)
    .bind(company_id)
    .bind(contact_id)
    .bind(email_address_id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

pub async fn crm_target_binding_exists(
    tx: &mut Tx<'_>,
    company_id: Uuid,
    contact_id: Uuid,
    email_address_id: Uuid,
) -> Result<bool, AppError> {
    sqlx::query_scalar(
        "SELECT EXISTS (
            SELECT 1 FROM companies co
            JOIN contacts ct ON ct.workspace_id = co.workspace_id AND ct.company_id = co.id
            JOIN contact_emails ce ON ce.workspace_id = ct.workspace_id AND ce.contact_id = ct.id
            JOIN email_addresses ea ON ea.workspace_id = ce.workspace_id AND ea.id = ce.email_address_id
           WHERE co.workspace_id = $1 AND co.id = $2 AND ct.id = $3 AND ea.id = $4
             AND co.archived_at IS NULL AND ct.archived_at IS NULL
             AND ct.status IN ('new','active'))",
    )
    .bind(tx.workspace_id())
    .bind(company_id)
    .bind(contact_id)
    .bind(email_address_id)
    .fetch_one(tx.conn())
    .await
    .map_err(map_sqlx)
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct TargetRow {
    pub id: Uuid,
    pub sales_campaign_id: Uuid,
    pub company_id: Option<Uuid>,
    pub contact_id: Option<Uuid>,
    pub contact_email_id: Option<Uuid>,
    pub email_address_id: Option<Uuid>,
    pub workflow_stage: String,
    pub current_draft_id: Option<Uuid>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

const TARGET_COLS: &str = "id, sales_campaign_id, company_id, contact_id,
    contact_email_id, email_address_id, workflow_stage, current_draft_id,
    version, created_at, updated_at";

pub async fn get_target(
    tx: &mut Tx<'_>,
    campaign_id: Uuid,
    target_id: Uuid,
) -> Result<Option<TargetRow>, AppError> {
    sqlx::query_as::<_, TargetRow>(&format!(
        "SELECT {TARGET_COLS} FROM sales_campaign_targets
          WHERE workspace_id = $1 AND sales_campaign_id = $2 AND id = $3"
    ))
    .bind(tx.workspace_id())
    .bind(campaign_id)
    .bind(target_id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

pub async fn get_target_email_address_id(
    tx: &mut Tx<'_>,
    campaign_id: Uuid,
    target_id: Uuid,
) -> Result<Option<Uuid>, AppError> {
    sqlx::query_scalar(
        "SELECT t.email_address_id
           FROM sales_campaign_targets t
          WHERE t.workspace_id = $1 AND t.sales_campaign_id = $2 AND t.id = $3",
    )
    .bind(tx.workspace_id())
    .bind(campaign_id)
    .bind(target_id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

pub async fn lock_target(
    tx: &mut Tx<'_>,
    campaign_id: Uuid,
    target_id: Uuid,
) -> Result<Option<TargetRow>, AppError> {
    let locked: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM sales_campaign_targets
          WHERE workspace_id = $1 AND sales_campaign_id = $2 AND id = $3 FOR UPDATE",
    )
    .bind(tx.workspace_id())
    .bind(campaign_id)
    .bind(target_id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)?;
    match locked {
        Some(_) => get_target(tx, campaign_id, target_id).await,
        None => Ok(None),
    }
}

pub async fn insert_target_draft(
    tx: &mut Tx<'_>,
    campaign_id: Uuid,
    target_id: Uuid,
    mail_account_id: Uuid,
    draft_id: Uuid,
) -> Result<(), AppError> {
    sqlx::query(
        "INSERT INTO sales_campaign_target_drafts
            (workspace_id, sales_campaign_id, target_id, mail_account_id, draft_id)
         VALUES ($1,$2,$3,$4,$5)",
    )
    .bind(tx.workspace_id())
    .bind(campaign_id)
    .bind(target_id)
    .bind(mail_account_id)
    .bind(draft_id)
    .execute(tx.conn())
    .await
    .map_err(map_sqlx)?;
    Ok(())
}

pub async fn set_current_target_draft(
    tx: &mut Tx<'_>,
    campaign_id: Uuid,
    target_id: Uuid,
    draft_id: Uuid,
) -> Result<(), AppError> {
    sqlx::query(
        "UPDATE sales_campaign_targets
            SET current_draft_id = $4, workflow_stage = 'draft',
                version = version + 1, updated_at = now()
          WHERE workspace_id = $1 AND sales_campaign_id = $2 AND id = $3",
    )
    .bind(tx.workspace_id())
    .bind(campaign_id)
    .bind(target_id)
    .bind(draft_id)
    .execute(tx.conn())
    .await
    .map_err(map_sqlx)?;
    Ok(())
}

pub async fn update_target_stage(
    tx: &mut Tx<'_>,
    campaign_id: Uuid,
    target_id: Uuid,
    expected_version: i64,
    stage: &str,
) -> Result<bool, AppError> {
    let result = sqlx::query(
        "UPDATE sales_campaign_targets
            SET workflow_stage = $5, version = version + 1, updated_at = now()
          WHERE workspace_id = $1 AND sales_campaign_id = $2 AND id = $3 AND version = $4",
    )
    .bind(tx.workspace_id())
    .bind(campaign_id)
    .bind(target_id)
    .bind(expected_version)
    .bind(stage)
    .execute(tx.conn())
    .await
    .map_err(map_sqlx)?;
    Ok(result.rows_affected() == 1)
}

pub async fn touch_target(
    tx: &mut Tx<'_>,
    campaign_id: Uuid,
    target_id: Uuid,
) -> Result<(), AppError> {
    sqlx::query(
        "UPDATE sales_campaign_targets SET version = version + 1, updated_at = now()
          WHERE workspace_id = $1 AND sales_campaign_id = $2 AND id = $3",
    )
    .bind(tx.workspace_id())
    .bind(campaign_id)
    .bind(target_id)
    .execute(tx.conn())
    .await
    .map_err(map_sqlx)?;
    Ok(())
}

pub async fn touch_campaign(tx: &mut Tx<'_>, campaign_id: Uuid) -> Result<(), AppError> {
    sqlx::query(
        "UPDATE sales_campaigns SET version = version + 1, updated_at = now()
          WHERE workspace_id = $1 AND id = $2",
    )
    .bind(tx.workspace_id())
    .bind(campaign_id)
    .execute(tx.conn())
    .await
    .map_err(map_sqlx)?;
    Ok(())
}

pub async fn current_draft_outbound_status(
    tx: &mut Tx<'_>,
    target: &TargetRow,
) -> Result<Option<String>, AppError> {
    sqlx::query_scalar(
        "SELECT o.status FROM outbound_messages o
          WHERE o.workspace_id = $1 AND o.source_draft_id = $2
          ORDER BY o.created_at DESC, o.id DESC LIMIT 1",
    )
    .bind(tx.workspace_id())
    .bind(target.current_draft_id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct TargetBoardRow {
    pub id: Uuid,
    pub company_id: Option<Uuid>,
    pub contact_id: Option<Uuid>,
    pub email_address_id: Option<Uuid>,
    pub company_name: Option<String>,
    pub contact_name: Option<String>,
    pub email_address: Option<String>,
    pub workflow_stage: String,
    pub version: i64,
    pub draft_id: Option<Uuid>,
    pub draft_subject: Option<String>,
    pub draft_status: Option<String>,
    pub draft_version: Option<i64>,
    pub draft_outbound_message_id: Option<Uuid>,
    pub outbound_status: Option<String>,
    pub outbound_payload_hash: Option<String>,
    pub outbound_not_before: Option<DateTime<Utc>>,
    pub approval_status: Option<String>,
    pub approval_expires_at: Option<DateTime<Utc>>,
    pub approved_at: Option<DateTime<Utc>>,
    pub accepted_at: Option<DateTime<Utc>>,
    pub failure_code: Option<String>,
    pub open_count: i64,
    pub click_count: i64,
    pub known_reply_count: i64,
    pub last_reply_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

const TARGET_BOARD_SELECT: &str = "SELECT t.id, t.company_id, t.contact_id, t.email_address_id,
       co.name AS company_name, ct.name AS contact_name, ea.address_original AS email_address,
       t.workflow_stage, t.version, d.id AS draft_id, d.subject AS draft_subject,
       d.status AS draft_status, d.version AS draft_version, o.id AS draft_outbound_message_id,
       o.status AS outbound_status, encode(o.payload_hash, 'hex') AS outbound_payload_hash,
       o.not_before AS outbound_not_before,
       a.status AS approval_status, a.expires_at AS approval_expires_at,
       a.approved_at, o.accepted_at, o.failure_code,
       COALESCE(e.open_count, 0)::bigint AS open_count,
       COALESCE(e.click_count, 0)::bigint AS click_count,
       COALESCE(r.reply_count, 0)::bigint AS known_reply_count,
       r.last_reply_at, t.created_at, t.updated_at
  FROM sales_campaign_targets t
  LEFT JOIN companies co
    ON co.workspace_id = t.workspace_id AND co.id = t.company_id
  LEFT JOIN contacts ct
    ON ct.workspace_id = t.workspace_id AND ct.id = t.contact_id
  LEFT JOIN email_addresses ea
    ON ea.workspace_id = t.workspace_id AND ea.id = t.email_address_id
  LEFT JOIN message_drafts d
    ON d.workspace_id = t.workspace_id AND d.id = t.current_draft_id
  LEFT JOIN LATERAL (
       SELECT om.id, om.status, om.payload_hash, om.not_before, om.accepted_at,
              om.failure_code, om.thread_id
         FROM outbound_messages om
        WHERE om.workspace_id = t.workspace_id AND om.source_draft_id = t.current_draft_id
        ORDER BY om.created_at DESC, om.id DESC LIMIT 1
  ) o ON true
  LEFT JOIN LATERAL (
       SELECT ar.status, ar.expires_at,
              CASE WHEN ar.status = 'approved' AND ad.decision = 'approved'
                   THEN ad.decided_at ELSE NULL END AS approved_at
         FROM approval_requests ar
         LEFT JOIN approval_decisions ad
           ON ad.workspace_id = ar.workspace_id AND ad.approval_request_id = ar.id
        WHERE ar.workspace_id = t.workspace_id AND ar.outbound_message_id = o.id
          AND ar.action_type = 'message_submit'
        ORDER BY ar.created_at DESC, ar.id DESC LIMIT 1
  ) a ON true
  LEFT JOIN LATERAL (
       SELECT COALESCE(SUM(b.request_count) FILTER (WHERE c.capability_type = 'open'), 0)::bigint
                  AS open_count,
              COALESCE(SUM(b.request_count) FILTER (WHERE c.capability_type = 'click'), 0)::bigint
                  AS click_count
         FROM outbound_engagement_capabilities c
         LEFT JOIN outbound_engagement_request_buckets b
           ON b.workspace_id = c.workspace_id AND b.capability_id = c.id
        WHERE c.workspace_id = t.workspace_id AND c.outbound_message_id = o.id
  ) e ON true
  LEFT JOIN LATERAL (
       SELECT COUNT(*)::bigint AS reply_count, MAX(em.message_at) AS last_reply_at
         FROM email_messages em
        WHERE em.workspace_id = t.workspace_id AND em.thread_id = o.thread_id
          AND em.direction = 'inbound'
  ) r ON true";

pub async fn list_target_board_rows(
    tx: &mut Tx<'_>,
    campaign_id: Uuid,
) -> Result<Vec<TargetBoardRow>, AppError> {
    sqlx::query_as::<_, TargetBoardRow>(&format!(
        "{TARGET_BOARD_SELECT}
          WHERE t.workspace_id = $1 AND t.sales_campaign_id = $2
          ORDER BY t.created_at, t.id"
    ))
    .bind(tx.workspace_id())
    .bind(campaign_id)
    .fetch_all(tx.conn())
    .await
    .map_err(map_sqlx)
}

pub async fn get_target_board_row(
    tx: &mut Tx<'_>,
    campaign_id: Uuid,
    target_id: Uuid,
) -> Result<Option<TargetBoardRow>, AppError> {
    sqlx::query_as::<_, TargetBoardRow>(&format!(
        "{TARGET_BOARD_SELECT}
          WHERE t.workspace_id = $1 AND t.sales_campaign_id = $2 AND t.id = $3"
    ))
    .bind(tx.workspace_id())
    .bind(campaign_id)
    .bind(target_id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}
