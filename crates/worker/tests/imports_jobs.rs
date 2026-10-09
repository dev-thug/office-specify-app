//! Task 5 (imports) worker tests — durable job handlers + T32.
//!
//! - `imports.validate`/`imports.commit`/`imports.export` registered and
//!   driven through the real `JobHandler::handle` path.
//! - `t32_*`: a suppressed address re-imported under `duplicate_strategy=
//!   link` keeps its suppression while a fresh contact is recreated.
//! - Job re-entry is idempotent (status-guarded no-op).

mod common;

use contracts::Scope;
use contracts::ScopeSet;
use contracts::imports::CommitImportRequest;
use infrastructure::db::Tx;
use infrastructure::jobs::JobCtx;
use infrastructure::repo::imports as repo;
use infrastructure::storage::{LocalObjectStore, ObjectStore};
use infrastructure::testing::{ctx_for_credential, seed_workspace};
use serde_json::json;
use uuid::Uuid;
use worker::registry;

/// The handlers build their object store from `Config::from_env` — point
/// `OBJECTS_DIR` at a per-process temp dir so tests don't litter the repo.
fn init_env() {
    static INIT: std::sync::Once = std::sync::Once::new();
    INIT.call_once(|| {
        // SAFETY: runs once before any test body in this binary reads the
        // env; other binaries have their own process env. No other thread
        // in this test binary mutates APP_ENV or OBJECTS_DIR.
        unsafe {
            std::env::set_var("APP_ENV", "test");
            std::env::set_var(
                "OBJECTS_DIR",
                std::env::temp_dir()
                    .join("outreach-t5-worker-objects")
                    .to_str()
                    .unwrap(),
            );
        }
    });
}

/// Env-configured store — same construction the handlers use.
fn env_store() -> LocalObjectStore {
    init_env();
    application::imports::object_store_from_env().unwrap()
}

/// Seed `stored_objects` + `import_jobs` ('validating') around CSV bytes
/// already in the object store — the exact state `imports.validate` takes
/// over from.
async fn seed_import(
    tx: &mut Tx<'_>,
    store: &impl ObjectStore,
    csv: &str,
    requested_by: Uuid,
    strategy: &str,
) -> Uuid {
    let key = Uuid::new_v4().simple().to_string();
    store.put(&key, csv.as_bytes()).await.unwrap();
    let hash = infrastructure::crypto::sha256_bytes(csv.as_bytes());
    let object_id = repo::insert_stored_object(
        tx,
        &key,
        "text/csv",
        csv.len() as i64,
        &hash,
        "clean",
        Some(requested_by),
    )
    .await
    .unwrap();
    repo::insert_import_job(
        tx,
        object_id,
        requested_by,
        &json!({
            "job_kind": "import",
            "columns": {"Email": "contact.email", "Name": "contact.name"},
            "has_header": true,
            "duplicate_strategy": strategy
        }),
        "validating",
    )
    .await
    .unwrap()
}

/// Enqueue a real `jobs` row (the audit trail FKs `job_id` → `jobs`) then
/// hand it to the registered handler — the runner's claim→handle shape.
async fn run_handler(kind: &str, tx: &mut Tx<'_>, payload: serde_json::Value) {
    init_env();
    let handler = registry().get(kind).expect("handler registered");
    let job_id =
        infrastructure::jobs::enqueue(tx, kind, 1, payload.clone(), chrono::Utc::now(), None)
            .await
            .unwrap();
    let ctx = JobCtx {
        job_id,
        workspace_id: tx.workspace_id().unwrap(),
        lease_token: Uuid::new_v4(),
        attempt_count: 1,
        lease_owner: "test".into(),
    };
    handler.handle(&ctx, tx, payload).await.unwrap();
}

#[tokio::test]
async fn import_job_handlers_registered() {
    let reg = registry();
    for kind in ["imports.validate", "imports.commit", "imports.export"] {
        assert!(reg.get(kind).is_some(), "no handler for {kind}");
    }
}

#[tokio::test]
async fn import_cannot_read_another_principals_uploaded_object() {
    let Some(db) = common::test_db().await else {
        return;
    };
    let seed_db = common::seed_db().await;
    let world = seed_workspace(&seed_db).await.unwrap();
    let object_id = {
        let mut tx = db.begin(world.workspace_id).await.unwrap();
        let object_id = repo::insert_stored_object(
            &mut tx,
            &Uuid::new_v4().simple().to_string(),
            "text/csv",
            1,
            &infrastructure::crypto::sha256_bytes(b"x"),
            "clean",
            Some(world.admin.principal_id),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        object_id
    };
    let manager = ctx_for_credential(
        &world,
        &world.manager,
        Uuid::new_v4(),
        ScopeSet::of([Scope::CrmImport]),
        "cross-principal-import",
    );
    let req = contracts::imports::CreateImportRequest {
        stored_object_id: object_id,
        mapping: contracts::imports::ImportMapping {
            columns: [("Name".into(), "contact.name".into())].into(),
            has_header: true,
            duplicate_strategy: contracts::imports::DuplicateStrategy::Review,
        },
    };
    let mut tx = db.begin(world.workspace_id).await.unwrap();
    let err = application::imports::query::create_import(&mut tx, &manager, &req)
        .await
        .unwrap_err();
    let _ = tx.rollback().await;
    assert_eq!(err.code, contracts::ErrorCode::Forbidden);
}

#[tokio::test]
async fn t32_reimported_suppressed_address_stays_suppressed_contact_recreated() {
    let Some(db) = common::test_db().await else {
        return;
    };
    let seed_db = common::seed_db().await;
    let world = seed_workspace(&seed_db).await.unwrap();
    let ws = world.workspace_id;
    let store = env_store();
    let importer = ctx_for_credential(
        &world,
        &world.admin,
        Uuid::new_v4(),
        ScopeSet::of([Scope::CrmImport]),
        "t32",
    );

    // Seed: suppressed address + its old contact; then the contact is
    // deleted — the T32 "re-import after removal" shape.
    let (addr_id, suppression_id) = {
        let mut tx = db.begin(ws).await.unwrap();
        let addr_id: Uuid = sqlx::query_scalar(
            "INSERT INTO email_addresses (workspace_id, address_original, normalized_key, normalization_version)
             VALUES ($1, 'Supp@X.Test', 'supp@x.test', 1) RETURNING id",
        )
        .bind(ws)
        .fetch_one(tx.conn())
        .await
        .unwrap();
        let old_contact: Uuid = sqlx::query_scalar(
            "INSERT INTO contacts (workspace_id, name) VALUES ($1, 'Old Contact') RETURNING id",
        )
        .bind(ws)
        .fetch_one(tx.conn())
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO contact_emails (workspace_id, contact_id, email_address_id, is_primary)
             VALUES ($1, $2, $3, true)",
        )
        .bind(ws)
        .bind(old_contact)
        .bind(addr_id)
        .execute(tx.conn())
        .await
        .unwrap();
        let suppression_id: Uuid = sqlx::query_scalar(
            "INSERT INTO suppressions (workspace_id, email_address_id, purpose_scope, reason)
             VALUES ($1, $2, 'marketing', 'unsubscribe') RETURNING id",
        )
        .bind(ws)
        .bind(addr_id)
        .fetch_one(tx.conn())
        .await
        .unwrap();
        sqlx::query("DELETE FROM contact_emails WHERE workspace_id = $1 AND contact_id = $2")
            .bind(ws)
            .bind(old_contact)
            .execute(tx.conn())
            .await
            .unwrap();
        sqlx::query("DELETE FROM contacts WHERE workspace_id = $1 AND id = $2")
            .bind(ws)
            .bind(old_contact)
            .execute(tx.conn())
            .await
            .unwrap();
        tx.commit().await.unwrap();
        (addr_id, suppression_id)
    };

    // Import CSV carrying the same address (as 'supp@x.test' — the
    // normalization canonicalizes to the existing key).
    let import_id = {
        let mut tx = db.begin(ws).await.unwrap();
        let id = seed_import(
            &mut tx,
            &store,
            "Email,Name\nsupp@x.test,New Person\n",
            world.admin.principal_id,
            "link",
        )
        .await;
        tx.commit().await.unwrap();
        id
    };

    // imports.validate via the registered handler.
    {
        let mut tx = db.begin(ws).await.unwrap();
        run_handler(
            "imports.validate",
            &mut tx,
            json!({"import_job_id": import_id}),
        )
        .await;
        tx.commit().await.unwrap();
    }

    // Row came out 'duplicate' — address already exists.
    let mut tx = db.begin(ws).await.unwrap();
    let (status, job_status, hash): (String, String, Vec<u8>) = sqlx::query_as(
        "SELECT r.status, j.status, j.validation_hash
           FROM import_rows r JOIN import_jobs j ON j.id = r.import_job_id
          WHERE r.workspace_id = $1 AND j.id = $2",
    )
    .bind(ws)
    .bind(import_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    assert_eq!(status, "duplicate");
    assert_eq!(job_status, "ready");
    let hash_hex = application::imports::hex_lower(&hash);
    tx.rollback().await.unwrap();

    // Idempotent re-entry: running validate again is a no-op.
    {
        let mut tx = db.begin(ws).await.unwrap();
        run_handler(
            "imports.validate",
            &mut tx,
            json!({"import_job_id": import_id}),
        )
        .await;
        tx.commit().await.unwrap();
    }
    let mut tx = db.begin(ws).await.unwrap();
    let rows: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM import_rows WHERE workspace_id = $1 AND import_job_id = $2",
    )
    .bind(ws)
    .bind(import_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    assert_eq!(rows, 1, "validate re-run must not duplicate rows");
    tx.rollback().await.unwrap();

    // Request commit through the real API path (hash pins the snapshot).
    {
        let mut tx = db.begin(ws).await.unwrap();
        application::imports::commit::request_commit(
            &mut tx,
            &importer,
            import_id,
            &CommitImportRequest {
                validation_hash: hash_hex,
                invalid_row_policy: contracts::imports::InvalidRowPolicy::Skip,
            },
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
    }
    {
        let mut tx = db.begin(ws).await.unwrap();
        run_handler(
            "imports.commit",
            &mut tx,
            json!({"import_job_id": import_id}),
        )
        .await;
        tx.commit().await.unwrap();
    }

    // ---- T32 assertions ----
    let mut tx = db.begin(ws).await.unwrap();
    // 1. Suppression untouched: still keyed to the same address, unreleased.
    let (sup_addr, released): (Uuid, Option<chrono::DateTime<chrono::Utc>>) = sqlx::query_as(
        "SELECT email_address_id, released_at FROM suppressions
              WHERE workspace_id = $1 AND id = $2",
    )
    .bind(ws)
    .bind(suppression_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    assert_eq!(sup_addr, addr_id);
    assert!(
        released.is_none(),
        "re-import must never release an existing suppression"
    );

    // 2. Contact recreated: NEW contact linked to the SAME address id.
    let new_contact: Uuid = sqlx::query_scalar(
        "SELECT contact_id FROM contact_emails
          WHERE workspace_id = $1 AND email_address_id = $2
          ORDER BY created_at DESC LIMIT 1",
    )
    .bind(ws)
    .bind(addr_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    let (name,): (String,) =
        sqlx::query_as("SELECT name FROM contacts WHERE workspace_id = $1 AND id = $2")
            .bind(ws)
            .bind(new_contact)
            .fetch_one(tx.conn())
            .await
            .unwrap();
    assert_eq!(name, "New Person");

    // 3. Still one email_addresses row for the address.
    let n: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM email_addresses
          WHERE workspace_id = $1 AND normalized_key = 'supp@x.test'",
    )
    .bind(ws)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    assert_eq!(n, 1);

    // 4. Job completed; the row references the recreated contact.
    let (job_status, imported_contact): (String, Option<Uuid>) = sqlx::query_as(
        "SELECT j.status, r.contact_id
           FROM import_jobs j JOIN import_rows r ON r.import_job_id = j.id
          WHERE j.workspace_id = $1 AND j.id = $2",
    )
    .bind(ws)
    .bind(import_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    assert_eq!(job_status, "completed");
    assert_eq!(imported_contact, Some(new_contact));
    tx.rollback().await.unwrap();
}

#[tokio::test]
async fn import_validate_per_row_errors_and_commit_skip() {
    let Some(db) = common::test_db().await else {
        return;
    };
    let seed_db = common::seed_db().await;
    let world = seed_workspace(&seed_db).await.unwrap();
    let ws = world.workspace_id;
    let store = env_store();
    let importer = ctx_for_credential(
        &world,
        &world.admin,
        Uuid::new_v4(),
        ScopeSet::of([Scope::CrmImport]),
        "rows",
    );

    let import_id = {
        let mut tx = db.begin(ws).await.unwrap();
        let id = seed_import(
            &mut tx,
            &store,
            "Email,Name\nok@x.test,Ok Row\nbad-email,Bad Row\n",
            world.admin.principal_id,
            "review",
        )
        .await;
        tx.commit().await.unwrap();
        id
    };
    {
        let mut tx = db.begin(ws).await.unwrap();
        run_handler(
            "imports.validate",
            &mut tx,
            json!({"import_job_id": import_id}),
        )
        .await;
        tx.commit().await.unwrap();
    }

    let mut tx = db.begin(ws).await.unwrap();
    let rows: Vec<(i32, String, serde_json::Value)> = sqlx::query_as(
        "SELECT row_no, status, errors FROM import_rows
          WHERE workspace_id = $1 AND import_job_id = $2 ORDER BY row_no",
    )
    .bind(ws)
    .bind(import_id)
    .fetch_all(tx.conn())
    .await
    .unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].1, "valid");
    assert_eq!(rows[1].1, "invalid");
    assert_eq!(rows[1].2[0]["code"], "invalid_email");
    let hash: Vec<u8> = sqlx::query_scalar(
        "SELECT validation_hash FROM import_jobs WHERE workspace_id = $1 AND id = $2",
    )
    .bind(ws)
    .bind(import_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    let hash_hex = application::imports::hex_lower(&hash);
    tx.rollback().await.unwrap();

    // Commit with `reject` policy + invalid rows → 422 at request time.
    {
        let mut tx = db.begin(ws).await.unwrap();
        let err = application::imports::commit::request_commit(
            &mut tx,
            &importer,
            import_id,
            &CommitImportRequest {
                validation_hash: hash_hex.clone(),
                invalid_row_policy: contracts::imports::InvalidRowPolicy::Reject,
            },
        )
        .await
        .unwrap_err();
        assert_eq!(err.status(), 422);
        tx.rollback().await.unwrap();
    }
    // skip → committing → commit job → only the valid row imported.
    {
        let mut tx = db.begin(ws).await.unwrap();
        application::imports::commit::request_commit(
            &mut tx,
            &importer,
            import_id,
            &CommitImportRequest {
                validation_hash: hash_hex,
                invalid_row_policy: contracts::imports::InvalidRowPolicy::Skip,
            },
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
    }
    {
        let mut tx = db.begin(ws).await.unwrap();
        run_handler(
            "imports.commit",
            &mut tx,
            json!({"import_job_id": import_id}),
        )
        .await;
        tx.commit().await.unwrap();
    }
    let mut tx = db.begin(ws).await.unwrap();
    let (imported, still_invalid): (i64, i64) = sqlx::query_as(
        "SELECT count(*) FILTER (WHERE status = 'imported'),
                count(*) FILTER (WHERE status = 'invalid')
           FROM import_rows WHERE workspace_id = $1 AND import_job_id = $2",
    )
    .bind(ws)
    .bind(import_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    assert_eq!((imported, still_invalid), (1, 1));
    // Imported address exists; NO communication basis was granted — a CSV
    // can never assert consent (B.3.13).
    let bases: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM communication_bases cb
           JOIN email_addresses e ON e.id = cb.email_address_id AND e.workspace_id = cb.workspace_id
          WHERE cb.workspace_id = $1 AND e.normalized_key = 'ok@x.test'",
    )
    .bind(ws)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    assert_eq!(bases, 0);
    tx.rollback().await.unwrap();
}
