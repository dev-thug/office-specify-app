//! Recipient-bound engagement capabilities, mailbox ACL counts, and bounded
//! request aggregation. Runs only against the configured isolated test DB.

mod common;

use infrastructure::{
    db::Db,
    repo::{compliance::test_fixtures, engagement},
    testing::seed_workspace,
};
use uuid::Uuid;

#[tokio::test]
async fn capabilities_are_bound_bounded_and_counted_only_for_mailbox_readers() {
    let Some(db) = common::test_db().await else {
        return;
    };
    let owner_db = Db::connect(&std::env::var("DATABASE_URL").unwrap(), None)
        .await
        .unwrap();
    let world = seed_workspace(&db).await.unwrap();
    let other_world = seed_workspace(&db).await.unwrap();

    let (outbound_id, open_token, click_token, open_capability_id, click_capability_id) = {
        let mut tx = db.begin(world.workspace_id).await.unwrap();
        assert!(engagement::tracking_enabled(&mut tx).await.unwrap());
        let recipient_id = test_fixtures::insert_email_address(
            &mut tx,
            &format!("engagement+{}@example.test", Uuid::new_v4()),
        )
        .await
        .unwrap();
        let outbound_id = test_fixtures::insert_outbound(
            &mut tx,
            world.mail_account_id,
            world.connection_id,
            world.admin.principal_id,
            recipient_id,
        )
        .await
        .unwrap();
        let open_token = infrastructure::crypto::generate_token();
        let click_token = infrastructure::crypto::generate_token();
        engagement::insert_capability(
            &mut tx,
            outbound_id,
            recipient_id,
            "open",
            &open_token,
            None,
        )
        .await
        .unwrap();
        engagement::insert_capability(
            &mut tx,
            outbound_id,
            recipient_id,
            "click",
            &click_token,
            Some("https://example.test/article?source=outreach"),
        )
        .await
        .unwrap();
        let open_capability_id = capability_id(&mut tx, &open_token).await;
        let click_capability_id = capability_id(&mut tx, &click_token).await;
        let bound_recipient_id: Uuid = sqlx::query_scalar(
            "SELECT recipient_email_address_id FROM outbound_engagement_capabilities
              WHERE workspace_id = $1 AND id = $2",
        )
        .bind(world.workspace_id)
        .bind(click_capability_id)
        .fetch_one(tx.conn())
        .await
        .unwrap();
        assert_eq!(bound_recipient_id, recipient_id);
        tx.commit().await.unwrap();
        (
            outbound_id,
            open_token,
            click_token,
            open_capability_id,
            click_capability_id,
        )
    };

    let resolved = engagement::resolve_token(&db, &click_token)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(resolved.workspace_id, world.workspace_id);
    assert_eq!(resolved.capability_id, click_capability_id);
    let tampered = format!(
        "{}{}",
        if click_token.starts_with('A') {
            'B'
        } else {
            'A'
        },
        &click_token[1..]
    );
    assert!(
        engagement::resolve_token(&db, &tampered)
            .await
            .unwrap()
            .is_none()
    );

    {
        let mut tx = db.begin(world.workspace_id).await.unwrap();
        let initial_status: String = sqlx::query_scalar(
            "SELECT status FROM outbound_messages WHERE workspace_id = $1 AND id = $2",
        )
        .bind(world.workspace_id)
        .bind(outbound_id)
        .fetch_one(tx.conn())
        .await
        .unwrap();
        assert_eq!(initial_status, "draft");
        assert!(
            engagement::record_request(&mut tx, open_capability_id, "open")
                .await
                .unwrap()
                .is_some()
        );
        assert!(
            engagement::record_request(&mut tx, click_capability_id, "open")
                .await
                .unwrap()
                .is_none(),
            "a click token cannot be counted as an open"
        );
        assert!(
            engagement::record_request(&mut tx, click_capability_id, "click")
                .await
                .unwrap()
                .is_some()
        );
        sqlx::query(
            "UPDATE outbound_engagement_request_buckets
                SET request_count = 999, updated_at = now() - interval '1 day'
              WHERE workspace_id = $1 AND capability_id = $2",
        )
        .bind(world.workspace_id)
        .bind(click_capability_id)
        .execute(tx.conn())
        .await
        .unwrap();
        assert!(
            engagement::record_request(&mut tx, click_capability_id, "click")
                .await
                .unwrap()
                .is_some()
        );
        let updated_at_at_cap: chrono::DateTime<chrono::Utc> = sqlx::query_scalar(
            "SELECT updated_at FROM outbound_engagement_request_buckets
              WHERE workspace_id = $1 AND capability_id = $2",
        )
        .bind(world.workspace_id)
        .bind(click_capability_id)
        .fetch_one(tx.conn())
        .await
        .unwrap();
        let status_after_requests: String = sqlx::query_scalar(
            "SELECT status FROM outbound_messages WHERE workspace_id = $1 AND id = $2",
        )
        .bind(world.workspace_id)
        .bind(outbound_id)
        .fetch_one(tx.conn())
        .await
        .unwrap();
        assert_eq!(status_after_requests, initial_status);

        let counts = engagement::outbound_counts(&mut tx, world.admin.principal_id, outbound_id)
            .await
            .unwrap()
            .unwrap();
        assert!(counts.tracking_enabled);
        assert_eq!(counts.open_request_count, 1);
        assert_eq!(counts.click_request_count, 1000);
        assert!(
            engagement::outbound_counts(&mut tx, world.member_b.principal_id, outbound_id,)
                .await
                .unwrap()
                .is_none()
        );
        tx.commit().await.unwrap();

        let mut capped_tx = db.begin(world.workspace_id).await.unwrap();
        for _ in 0..2 {
            assert!(
                engagement::record_request(&mut capped_tx, click_capability_id, "click")
                    .await
                    .unwrap()
                    .is_some()
            );
        }
        let updated_at_after_cap: chrono::DateTime<chrono::Utc> = sqlx::query_scalar(
            "SELECT updated_at FROM outbound_engagement_request_buckets
              WHERE workspace_id = $1 AND capability_id = $2",
        )
        .bind(world.workspace_id)
        .bind(click_capability_id)
        .fetch_one(capped_tx.conn())
        .await
        .unwrap();
        assert_eq!(
            updated_at_after_cap, updated_at_at_cap,
            "requests after the daily cap must not write the bucket row"
        );
        capped_tx.commit().await.unwrap();
    }

    {
        let mut tx = owner_db.begin(world.workspace_id).await.unwrap();
        sqlx::query(
            "UPDATE outbound_engagement_capabilities
                SET expires_at = now() - interval '1 day'
              WHERE workspace_id = $1 AND id = ANY($2)",
        )
        .bind(world.workspace_id)
        .bind(vec![open_capability_id, click_capability_id])
        .execute(tx.conn())
        .await
        .unwrap();
        tx.commit().await.unwrap();
    }

    {
        let location = engagement::resolve_token(&db, &click_token)
            .await
            .unwrap()
            .unwrap();
        let mut tx = db.begin(world.workspace_id).await.unwrap();
        let click = engagement::record_request(&mut tx, location.capability_id, "click")
            .await
            .unwrap()
            .unwrap();
        assert!(!click.tracking_active);
        assert_eq!(
            click.target_url.as_deref(),
            Some("https://example.test/article?source=outreach"),
            "expired links still return the immutable redirect target"
        );
        let open = engagement::record_request(&mut tx, open_capability_id, "open")
            .await
            .unwrap()
            .unwrap();
        assert!(!open.tracking_active);
        let counts = engagement::outbound_counts(&mut tx, world.admin.principal_id, outbound_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(counts.open_request_count, 1);
        assert_eq!(counts.click_request_count, 1000);
        tx.commit().await.unwrap();
    }

    {
        let mut tx = db.begin(world.workspace_id).await.unwrap();
        let wrong_recipient_id = test_fixtures::insert_email_address(
            &mut tx,
            &format!("other+{}@example.test", Uuid::new_v4()),
        )
        .await
        .unwrap();
        let wrong_recipient_token = infrastructure::crypto::generate_token();
        let insert = engagement::insert_capability(
            &mut tx,
            outbound_id,
            wrong_recipient_id,
            "click",
            &wrong_recipient_token,
            Some("https://example.test/wrong-recipient"),
        )
        .await;
        assert!(
            insert.is_err(),
            "recipient must match the immutable outbound"
        );
        tx.rollback().await.unwrap();
    }

    {
        let mut tx = db.begin(world.workspace_id).await.unwrap();
        sqlx::query(
            "UPDATE workspaces SET settings = settings || '{\"tracking_enabled\":false}'::jsonb
              WHERE id = $1",
        )
        .bind(world.workspace_id)
        .execute(tx.conn())
        .await
        .unwrap();
        assert!(!engagement::tracking_enabled(&mut tx).await.unwrap());
        tx.commit().await.unwrap();
    }

    {
        let mut tx = db.begin(other_world.workspace_id).await.unwrap();
        assert!(engagement::tracking_enabled(&mut tx).await.unwrap());
        assert!(
            engagement::outbound_counts(&mut tx, other_world.admin.principal_id, outbound_id,)
                .await
                .unwrap()
                .is_none()
        );
        let visible_capabilities: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM outbound_engagement_capabilities WHERE id = $1",
        )
        .bind(click_capability_id)
        .fetch_one(tx.conn())
        .await
        .unwrap();
        assert_eq!(
            visible_capabilities, 0,
            "RLS hides another workspace's token"
        );
        tx.rollback().await.unwrap();
    }

    assert!(
        engagement::resolve_token(&db, &open_token)
            .await
            .unwrap()
            .is_some()
    );
}

async fn capability_id(tx: &mut infrastructure::db::Tx<'_>, token: &str) -> Uuid {
    sqlx::query_scalar(
        "SELECT id FROM outbound_engagement_capabilities
          WHERE workspace_id = $1 AND token_hash = $2",
    )
    .bind(tx.workspace_id())
    .bind(infrastructure::crypto::sha256_bytes(token.as_bytes()).to_vec())
    .fetch_one(tx.conn())
    .await
    .unwrap()
}
