//! F4: `outreach-cli bootstrap` must validate the password source and
//! --password-out destination BEFORE any database write — with neither a
//! password source nor a destination it exits non-zero having created
//! nothing (previously the workspace, user and credential were committed
//! before the bail, leaving an undisclosable generated password on a real
//! account).

use std::process::Command;

/// F4: no OUTREACH_ADMIN_PASSWORD, no --admin-password-stdin, no
/// --password-out → non-zero exit, clear message, zero rows written.
#[tokio::test]
async fn bootstrap_without_password_destination_writes_nothing() {
    let url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set for DB tests");
    let pool = sqlx::PgPool::connect(&url).await.unwrap();
    // Run migrations so the assertion queries hit a real schema.
    infrastructure::db::Db::connect(&url, None)
        .await
        .unwrap()
        .migrate()
        .await
        .unwrap();

    let tag = &uuid::Uuid::new_v4().simple().to_string()[..8];
    let ws_name = format!("F4 {tag}");
    let email = format!("f4-{tag}@test.dev");

    let out = Command::new(env!("CARGO_BIN_EXE_outreach-cli"))
        .args([
            "bootstrap",
            "--workspace-name",
            &ws_name,
            "--admin-email",
            &email,
        ])
        .env("APP_ENV", "test")
        .env("DATABASE_URL", &url)
        .env_remove("OUTREACH_ADMIN_PASSWORD")
        .output()
        .expect("failed to spawn outreach-cli");

    assert!(
        !out.status.success(),
        "bootstrap must fail without a password destination: {out:?}"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("--password-out"),
        "error must name the missing flag: {stderr}"
    );

    // No workspace, user, credential or principal rows may exist. These
    // per-run-unique checks are immune to other tests seeding in
    // parallel — workspaces/users/credentials are global tables.
    let ws_found: i64 = sqlx::query_scalar("SELECT count(*) FROM workspaces WHERE name = $1")
        .bind(&ws_name)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(ws_found, 0, "no workspace may be created");
    let user_id: Option<uuid::Uuid> = sqlx::query_scalar("SELECT id FROM users WHERE email = $1")
        .bind(&email)
        .fetch_optional(&pool)
        .await
        .unwrap();
    assert!(user_id.is_none(), "no admin user may be created");
}

/// F4: a --password-out path that already exists must also fail before
/// any write (write_secret_0600 uses create_new — it can never overwrite).
#[tokio::test]
async fn bootstrap_with_existing_password_out_writes_nothing() {
    let url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set for DB tests");
    let pool = sqlx::PgPool::connect(&url).await.unwrap();
    infrastructure::db::Db::connect(&url, None)
        .await
        .unwrap()
        .migrate()
        .await
        .unwrap();

    let tag = &uuid::Uuid::new_v4().simple().to_string()[..8];
    let email = format!("f4x-{tag}@test.dev");
    let out_path = std::env::temp_dir().join(format!("f4-pw-{tag}.txt"));
    std::fs::write(&out_path, "pre-existing\n").unwrap();

    let out = Command::new(env!("CARGO_BIN_EXE_outreach-cli"))
        .args([
            "bootstrap",
            "--admin-email",
            &email,
            "--password-out",
            out_path.to_str().unwrap(),
        ])
        .env("APP_ENV", "test")
        .env("DATABASE_URL", &url)
        .env_remove("OUTREACH_ADMIN_PASSWORD")
        .output()
        .expect("failed to spawn outreach-cli");

    assert!(
        !out.status.success(),
        "existing --password-out must fail up front: {out:?}"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("already exists"),
        "error must explain the destination: {stderr}"
    );
    let user_id: Option<uuid::Uuid> = sqlx::query_scalar("SELECT id FROM users WHERE email = $1")
        .bind(&email)
        .fetch_optional(&pool)
        .await
        .unwrap();
    assert!(user_id.is_none(), "no admin user may be created");
    // The existing file is untouched.
    assert_eq!(
        std::fs::read_to_string(&out_path).unwrap(),
        "pre-existing\n"
    );
    let _ = std::fs::remove_file(&out_path);
}
