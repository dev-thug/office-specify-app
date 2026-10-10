//! Background synchronization for Office compliance suppressions and native
//! Zoho Campaigns export polling.

use std::sync::Arc;

use contracts::{AppError, Ctx};
use infrastructure::{
    config::Config,
    db::{Db, Tx},
    jobs::JobCtx,
    providers::zoho_campaigns::{http::CampaignsHttp, operations::CampaignsOperations},
    repo::{campaigns, marketing_suppressions},
};
use uuid::Uuid;

use crate::JobRegistry;

pub const PUSH_SUPPRESSION_KIND: &str = "marketing.push_suppression";
pub const POLL_EXPORT_KIND: &str = "marketing.poll_export";
pub const BACKFILL_SUPPRESSION_KIND: &str = marketing_suppressions::SUPPRESSION_BACKFILL_JOB_KIND;
const BACKFILL_BATCH_SIZE: i64 = 100;

struct Dependencies {
    db: Db,
    config: Config,
    http: Option<CampaignsHttp>,
    operations: Option<CampaignsOperations>,
}

struct PushSuppression {
    dependencies: Arc<Dependencies>,
}

struct BackfillSuppressions;

#[async_trait::async_trait]
impl infrastructure::jobs::JobHandler for PushSuppression {
    fn kind(&self) -> &'static str {
        PUSH_SUPPRESSION_KIND
    }

    fn external_effect(&self) -> bool {
        true
    }

    async fn handle(
        &self,
        ctx: &JobCtx,
        tx: &mut Tx<'_>,
        payload: serde_json::Value,
    ) -> Result<(), AppError> {
        let email_address_id = payload_uuid(&payload, "email_address_id")?;
        let only_connection_id = optional_payload_uuid(&payload, "connection_id")?;
        let system_ctx = system_context(tx, ctx).await?;
        let http = self.dependencies.http.as_ref().ok_or_else(|| {
            AppError::dependency_unavailable("Zoho Campaigns OAuth is not configured")
        })?;
        let operations = self.dependencies.operations.as_ref().ok_or_else(|| {
            AppError::dependency_unavailable("Zoho Campaigns operations are not configured")
        })?;
        application::marketing::suppressions::push_suppression_to_connection(
            &self.dependencies.db,
            &self.dependencies.config,
            http,
            operations,
            &system_ctx,
            email_address_id,
            only_connection_id,
        )
        .await
    }
}

/// DB-only connection bootstrap. It pages through active Office suppressions
/// and enqueues the existing journaled push handler; it never reads addresses
/// or contacts and never makes a provider call.
#[async_trait::async_trait]
impl infrastructure::jobs::JobHandler for BackfillSuppressions {
    fn kind(&self) -> &'static str {
        BACKFILL_SUPPRESSION_KIND
    }

    async fn handle(
        &self,
        _ctx: &JobCtx,
        tx: &mut Tx<'_>,
        payload: serde_json::Value,
    ) -> Result<(), AppError> {
        let connection_id = payload_uuid(&payload, "connection_id")?;
        let backfill_id = payload_uuid(&payload, "backfill_id")?;
        let after_email_address_id = optional_payload_uuid(&payload, "after_email_address_id")?;
        let mut addresses = marketing_suppressions::list_active_suppression_addresses_for_backfill(
            tx,
            connection_id,
            after_email_address_id,
            BACKFILL_BATCH_SIZE + 1,
        )
        .await?;
        let has_more = addresses.len() > BACKFILL_BATCH_SIZE as usize;
        addresses.truncate(BACKFILL_BATCH_SIZE as usize);
        let next_after = addresses.last().copied();

        for email_address_id in addresses {
            let dedupe_key = format!(
                "marketing-suppression-backfill:{backfill_id}:{connection_id}:{email_address_id}"
            );
            infrastructure::jobs::enqueue(
                tx,
                PUSH_SUPPRESSION_KIND,
                1,
                serde_json::json!({
                    "email_address_id": email_address_id,
                    "connection_id": connection_id,
                }),
                chrono::Utc::now(),
                Some(&dedupe_key),
            )
            .await?;
        }

        if has_more {
            let after = next_after
                .ok_or_else(|| AppError::internal("suppression backfill page has no cursor"))?;
            let dedupe_key = format!(
                "marketing-suppression-backfill:{backfill_id}:{connection_id}:after:{after}"
            );
            infrastructure::jobs::enqueue(
                tx,
                BACKFILL_SUPPRESSION_KIND,
                1,
                serde_json::json!({
                    "connection_id": connection_id,
                    "backfill_id": backfill_id,
                    "after_email_address_id": after,
                }),
                chrono::Utc::now(),
                Some(&dedupe_key),
            )
            .await?;
        }
        Ok(())
    }
}

struct PollExport {
    dependencies: Arc<Dependencies>,
}

#[async_trait::async_trait]
impl infrastructure::jobs::JobHandler for PollExport {
    fn kind(&self) -> &'static str {
        POLL_EXPORT_KIND
    }

    async fn handle(
        &self,
        ctx: &JobCtx,
        tx: &mut Tx<'_>,
        payload: serde_json::Value,
    ) -> Result<(), AppError> {
        let export_id = payload_uuid(&payload, "export_id")?;
        let system_ctx = system_context(tx, ctx).await?;
        let http = self.dependencies.http.as_ref().ok_or_else(|| {
            AppError::dependency_unavailable("Zoho Campaigns OAuth is not configured")
        })?;
        let operations = self.dependencies.operations.as_ref().ok_or_else(|| {
            AppError::dependency_unavailable("Zoho Campaigns operations are not configured")
        })?;
        application::marketing::suppressions::poll_export(
            &self.dependencies.db,
            &self.dependencies.config,
            http,
            operations,
            &system_ctx,
            export_id,
        )
        .await
    }
}

/// Register production Campaigns sync handlers. Missing OAuth configuration
/// leaves jobs registered but fail-closed if one is delivered.
pub fn register(registry: &mut JobRegistry, db: &Db, config: &Config) {
    let http = if config.zoho.campaigns.missing_fields().is_empty() {
        CampaignsHttp::new(config.zoho.campaigns.clone()).ok()
    } else {
        None
    };
    let operations = CampaignsOperations::new().ok();
    register_with_dependencies(registry, db.clone(), config.clone(), http, operations);
}

/// Dependency-injected registration for loopback provider tests.
pub fn register_with_dependencies(
    registry: &mut JobRegistry,
    db: Db,
    config: Config,
    http: Option<CampaignsHttp>,
    operations: Option<CampaignsOperations>,
) {
    let dependencies = Arc::new(Dependencies {
        db,
        config,
        http,
        operations,
    });
    registry.register(PushSuppression {
        dependencies: dependencies.clone(),
    });
    registry.register(BackfillSuppressions);
    registry.register(PollExport { dependencies });
}

fn payload_uuid(payload: &serde_json::Value, key: &str) -> Result<Uuid, AppError> {
    payload
        .get(key)
        .and_then(serde_json::Value::as_str)
        .and_then(|value| Uuid::parse_str(value).ok())
        .ok_or_else(|| AppError::validation(format!("payload.{key} is missing or invalid"), vec![]))
}

fn optional_payload_uuid(payload: &serde_json::Value, key: &str) -> Result<Option<Uuid>, AppError> {
    match payload.get(key) {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(value) => value
            .as_str()
            .and_then(|value| Uuid::parse_str(value).ok())
            .map(Some)
            .ok_or_else(|| AppError::validation(format!("payload.{key} is invalid"), vec![])),
    }
}

async fn system_context(tx: &mut Tx<'_>, job: &JobCtx) -> Result<Ctx, AppError> {
    let principal_id = campaigns::system_principal_id(tx)
        .await?
        .ok_or_else(|| AppError::internal("workspace system principal is missing"))?;
    Ok(Ctx::system(
        job.workspace_id,
        principal_id,
        job.job_id,
        format!("worker:marketing:{}", job.job_id),
    ))
}
