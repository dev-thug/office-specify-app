//! Upload capability → object bytes → `stored_objects` row (§15.2, B.2).
//!
//! `POST /imports/upload-requests` mints an HMAC capability (binds
//! workspace, principal, key, media type, max size, expiry — no DB row, no
//! reusable URL). `PUT /imports/uploads/{key}` verifies it, stores the
//! encrypted bytes via `ObjectStore` and only then inserts the
//! `stored_objects` metadata (its `content_hash` column is NOT NULL — the
//! hash can only be computed once bytes exist).

use chrono::{Duration, Utc};
use contracts::imports::{CreateUploadRequest, StoredObjectResponse, UploadRequestResponse};
use contracts::{AppError, Ctx, Scope, Violation};
use infrastructure::audit::{self, AuditEntry};
use infrastructure::config::Config;
use infrastructure::crypto;
use infrastructure::db::Db;
use infrastructure::repo::imports as repo;
use infrastructure::storage::ObjectStore;
use uuid::Uuid;

use crate::authz;

/// Upload capability lifetime — short; a fresh request is cheap.
const UPLOAD_TOKEN_TTL: Duration = Duration::minutes(15);
/// Max filename length (metadata only).
const MAX_FILENAME: usize = 500;

pub fn request_upload(
    ctx: &Ctx,
    cfg: &Config,
    req: &CreateUploadRequest,
) -> Result<UploadRequestResponse, AppError> {
    authz::require_scope(ctx, Scope::CrmImport)?;
    let mut violations = Vec::new();
    let filename = req.filename.trim();
    if filename.is_empty() || filename.chars().count() > MAX_FILENAME {
        violations.push(Violation {
            field: "filename".into(),
            code: "invalid".into(),
        });
    }
    if !super::IMPORT_MEDIA_TYPES.contains(&req.media_type.as_str()) {
        violations.push(Violation {
            field: "media_type".into(),
            code: "unsupported_media_type".into(),
        });
    }
    if req.byte_size < 1 || req.byte_size as u64 > domain::imports::MAX_IMPORT_BYTES {
        violations.push(Violation {
            field: "byte_size".into(),
            code: "out_of_range".into(),
        });
    }
    if !violations.is_empty() {
        return Err(AppError::validation("invalid upload request", violations).with_status(422));
    }

    let storage_key = Uuid::new_v4().simple().to_string();
    let grant = repo::UploadGrant {
        storage_key: storage_key.clone(),
        workspace_id: ctx.workspace_id,
        principal_id: ctx.principal_id,
        media_type: req.media_type.clone(),
        max_bytes: req.byte_size,
        expires_at: Utc::now() + UPLOAD_TOKEN_TTL,
    };
    Ok(UploadRequestResponse {
        storage_key: storage_key.clone(),
        upload_url: format!("/api/v1/imports/uploads/{storage_key}"),
        upload_token: repo::sign_upload_token(&cfg.hmac_key, &grant),
        expires_at: grant.expires_at,
        max_bytes: grant.max_bytes,
    })
}

/// Execute the upload: verify capability, store bytes, insert the object
/// row — all inside one tx except the object-store write (§15.2's object
/// store sits outside the DB; the stored_objects row references it after
/// `put` succeeds).
pub async fn complete_upload(
    db: &Db,
    ctx: &Ctx,
    cfg: &Config,
    storage_key: &str,
    token: &str,
    body: &[u8],
) -> Result<StoredObjectResponse, AppError> {
    authz::require_scope(ctx, Scope::CrmImport)?;
    let grant = repo::verify_upload_token(
        &cfg.hmac_key,
        ctx.workspace_id,
        ctx.principal_id,
        storage_key,
        token,
    )
    .ok_or_else(|| AppError::forbidden("invalid or expired upload token"))?;

    if body.is_empty() || body.len() as i64 > grant.max_bytes {
        return Err(AppError::validation(
            "uploaded bytes exceed the declared size or are empty",
            vec![Violation {
                field: "body".into(),
                code: "out_of_range".into(),
            }],
        )
        .with_status(422));
    }
    // Belt + suspenders: the capability can't exceed the global limit, but
    // enforce it again so a forged grant can't.
    if body.len() as u64 > domain::imports::MAX_IMPORT_BYTES {
        return Err(
            AppError::validation("upload exceeds the 20MB CSV limit", vec![]).with_status(422),
        );
    }

    let store = super::object_store(cfg)?;
    store.put(storage_key, body).await?;

    let hash = crypto::sha256_bytes(body);
    let mut tx = db.begin(ctx.workspace_id).await?;
    let object_id = repo::insert_stored_object(
        &mut tx,
        storage_key,
        &grant.media_type,
        body.len() as i64,
        &hash,
        // No scanner is wired in v1 — 'pending' is the honest state; the
        // file is only ever consumed by the validate job, never served
        // to a browser.
        "pending",
        Some(ctx.principal_id),
    )
    .await?;
    audit::record(
        &mut tx,
        AuditEntry::new(
            ctx.principal_id,
            "import.object_uploaded",
            "stored_object",
            ctx.request_id.clone(),
        )
        .with_target(object_id),
    )
    .await?;
    tx.commit().await?;

    Ok(StoredObjectResponse {
        object_id,
        storage_key: storage_key.to_string(),
        media_type: grant.media_type,
        byte_size: body.len() as i64,
        content_hash: crypto::sha256_hex(body),
    })
}
