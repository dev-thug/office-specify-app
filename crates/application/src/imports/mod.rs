//! imports module use cases (spec §6 — owned by the module worker).
//! Upload capability → object store → validate job → commit job, plus
//! exports (B.3.13, §15.1/§15.2).

pub mod commit;
pub mod commit_job;
pub mod export_job;
pub mod query;
pub mod upload;
pub mod validate;

use contracts::AppError;
use infrastructure::config::Config;
use infrastructure::storage::LocalObjectStore;

/// Media types the upload-request allowlist accepts for CSV imports.
pub const IMPORT_MEDIA_TYPES: &[&str] =
    &["text/csv", "application/csv", "application/vnd.ms-excel"];

/// Build the module's object store from process config. The API side gets
/// it from `AppState::config`; the worker handlers build it from env — the
/// two processes must share `OBJECTS_DIR`/`OUTREACH_SECRET_KEY`/
/// `OUTREACH_HMAC_KEY` in production just like they share `DATABASE_URL`
/// (§15.2/§15.3). Local FS + AES-GCM at rest.
pub fn object_store(cfg: &Config) -> Result<LocalObjectStore, AppError> {
    let enc = cfg
        .secret_store_key
        .ok_or_else(|| AppError::dependency_unavailable("object encryption key not configured"))?;
    Ok(LocalObjectStore::new(
        cfg.objects_dir.clone(),
        enc,
        cfg.hmac_key.clone(),
    ))
}

/// Same store from process env — worker-side entry point.
pub fn object_store_from_env() -> Result<LocalObjectStore, AppError> {
    let cfg = Config::from_env()
        .map_err(|e| AppError::dependency_unavailable(format!("worker config: {e}")))?;
    object_store(&cfg)
}

/// Lowercase hex (sha256 digests in API responses) — the `hex` crate is
/// not an application dependency.
pub fn hex_lower(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}
