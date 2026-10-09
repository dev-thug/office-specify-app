//! imports module job handlers (spec §6 — owned by the module worker).
//!
//! `imports.validate` / `imports.commit` / `imports.export` — the CSV
//! pipeline stages (B.3.13). Handlers only extract the entity id from the
//! payload (unknown fields are ignored for forward compat); all business
//! logic lives in `application::imports`. No external effects — the only
//! I/O beyond the job tx is the encrypted local object store.

use async_trait::async_trait;
use contracts::AppError;
use infrastructure::db::Tx;
use infrastructure::jobs::{JobCtx, JobHandler};
use uuid::Uuid;

use crate::JobRegistry;

/// Register this module's JobHandlers on the worker registry.
pub fn register(registry: &mut JobRegistry) {
    registry.register(ImportsValidate);
    registry.register(ImportsCommit);
    registry.register(ImportsExport);
}

fn payload_id(payload: &serde_json::Value, key: &str) -> Result<Uuid, AppError> {
    payload
        .get(key)
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse::<Uuid>().ok())
        .ok_or_else(|| AppError::internal(format!("job payload missing {key}")))
}

struct ImportsValidate;

#[async_trait]
impl JobHandler for ImportsValidate {
    fn kind(&self) -> &'static str {
        "imports.validate"
    }

    async fn handle(
        &self,
        ctx: &JobCtx,
        tx: &mut Tx<'_>,
        payload: serde_json::Value,
    ) -> Result<(), AppError> {
        let id = payload_id(&payload, "import_job_id")?;
        let store = application::imports::object_store_from_env()?;
        application::imports::validate::run(tx, &store, id, Some(ctx.job_id)).await
    }
}

struct ImportsCommit;

#[async_trait]
impl JobHandler for ImportsCommit {
    fn kind(&self) -> &'static str {
        "imports.commit"
    }

    async fn handle(
        &self,
        ctx: &JobCtx,
        tx: &mut Tx<'_>,
        payload: serde_json::Value,
    ) -> Result<(), AppError> {
        let id = payload_id(&payload, "import_job_id")?;
        application::imports::commit_job::run(tx, id, Some(ctx.job_id)).await
    }
}

struct ImportsExport;

#[async_trait]
impl JobHandler for ImportsExport {
    fn kind(&self) -> &'static str {
        "imports.export"
    }

    async fn handle(
        &self,
        ctx: &JobCtx,
        tx: &mut Tx<'_>,
        payload: serde_json::Value,
    ) -> Result<(), AppError> {
        let id = payload_id(&payload, "export_job_id")?;
        let store = application::imports::object_store_from_env()?;
        application::imports::export_job::run(tx, &store, id, Some(ctx.job_id)).await
    }
}
