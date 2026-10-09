//! DB guardrails for the Email API v2 mutation journal.

use infrastructure::{
    db::{Db, ROLE_API, ROLE_REPORT, ROLE_WORKER},
    testing::seed_workspace,
};
use uuid::Uuid;

#[tokio::test]
async fn zoho_template_journal_is_private_hash_only_and_api_runtime_scoped() {
    let url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set — skipping infrastructure test");
            return;
        }
    };
    let owner = Db::connect(&url, None).await.unwrap();
    owner.migrate().await.unwrap();
    let api_db = Db::connect(&url, Some(ROLE_API)).await.unwrap();
    let world = seed_workspace(&api_db).await.unwrap();
    let workspace_id = world.workspace_id;

    let mut tx = owner.begin(workspace_id).await.unwrap();
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM pg_class WHERE oid=to_regclass('public.zoho_email_template_operations'))",
    )
    .fetch_one(tx.conn())
    .await
    .unwrap();
    assert!(exists, "migration must create the mutation journal");
    let columns: Vec<String> = sqlx::query_scalar(
        "SELECT column_name FROM information_schema.columns WHERE table_schema='public' AND table_name='zoho_email_template_operations' ORDER BY ordinal_position",
    )
    .fetch_all(tx.conn())
    .await
    .unwrap();
    assert!(columns.iter().any(|column| column == "request_hmac"));
    assert!(
        columns
            .iter()
            .any(|column| column == "provider_config_hmac")
    );
    assert!(
        !columns
            .iter()
            .any(|column| column == "content" || column == "api_key")
    );
    let rls: (bool, bool) = sqlx::query_as(
        "SELECT relrowsecurity, relforcerowsecurity FROM pg_class WHERE oid=to_regclass('public.zoho_email_template_operations')",
    )
    .fetch_one(tx.conn())
    .await
    .unwrap();
    assert_eq!(rls, (true, true));
    tx.commit().await.unwrap();

    let operation_id = Uuid::new_v4();
    let mut tx = api_db.begin(workspace_id).await.unwrap();
    sqlx::query("INSERT INTO zoho_email_template_operations (id,workspace_id,actor_principal_id,endpoint,idempotency_key_hmac,request_hmac,provider_config_hmac,state) VALUES ($1,$2,$3,'create',$4,$5,$6,'intent')")
        .bind(operation_id).bind(workspace_id).bind(world.admin.principal_id)
        .bind([1u8; 32].as_slice()).bind([2u8; 32].as_slice()).bind([3u8; 32].as_slice())
        .execute(tx.conn()).await.unwrap();
    tx.commit().await.unwrap();

    let mut isolated = api_db.begin(Uuid::new_v4()).await.unwrap();
    let foreign_rows: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM zoho_email_template_operations WHERE workspace_id=$1",
    )
    .bind(workspace_id)
    .fetch_one(isolated.conn())
    .await
    .unwrap();
    assert_eq!(
        foreign_rows, 0,
        "RLS must hide another workspace's journal row"
    );
    isolated.commit().await.unwrap();

    let mut invalid_actor = api_db.begin(workspace_id).await.unwrap();
    let rejected = sqlx::query("INSERT INTO zoho_email_template_operations (id,workspace_id,actor_principal_id,endpoint,idempotency_key_hmac,request_hmac,provider_config_hmac,state) VALUES ($1,$2,$3,'create',$4,$5,$6,'intent')")
        .bind(Uuid::new_v4()).bind(workspace_id).bind(Uuid::new_v4())
        .bind([4u8; 32].as_slice()).bind([5u8; 32].as_slice()).bind([6u8; 32].as_slice())
        .execute(invalid_actor.conn()).await;
    assert!(
        rejected.is_err(),
        "composite actor/workspace foreign key must reject cross-workspace actors"
    );
    invalid_actor.rollback().await.unwrap();

    for (role, expected) in [(ROLE_API, true), (ROLE_WORKER, false), (ROLE_REPORT, false)] {
        let db = Db::connect(&url, Some(role)).await.unwrap();
        let mut tx = db.begin(workspace_id).await.unwrap();
        let allowed: bool = sqlx::query_scalar(
            "SELECT has_table_privilege(current_user, 'public.zoho_email_template_operations', 'SELECT') AND has_table_privilege(current_user, 'public.zoho_email_template_operations', 'INSERT') AND has_table_privilege(current_user, 'public.zoho_email_template_operations', 'UPDATE') AND has_table_privilege(current_user, 'public.zoho_email_template_operations', 'DELETE')",
        )
        .fetch_one(tx.conn())
        .await
        .unwrap();
        assert_eq!(allowed, expected, "journal grants for {role}");
        tx.commit().await.unwrap();
    }
}
