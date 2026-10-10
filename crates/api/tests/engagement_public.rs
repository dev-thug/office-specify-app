//! Public engagement requests count bounded, proxy-sensitive HTTP requests
//! and always redirect through the server-stored target.

use api::{AppState, testing::TestApp};
use contracts::sending::{CreateMessageDraftRequest, FreezeMessageDraftRequest};
use infrastructure::{
    config::Config,
    db::{Db, ROLE_API},
    repo::{compliance::test_fixtures, engagement},
    testing::{ctx_for, seed_workspace},
};
use reqwest::StatusCode;
use std::sync::Arc;
use uuid::Uuid;

#[tokio::test]
async fn public_requests_use_stored_targets_and_stop_counting_after_expiry() {
    let url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set — skipping engagement API test");
            return;
        }
    };
    let owner = Db::connect(&url, None).await.unwrap();
    owner.migrate().await.unwrap();
    let owner_db = owner.clone();
    let db = Db::connect(&url, Some(ROLE_API)).await.unwrap();
    let world = seed_workspace(&db).await.unwrap();
    let (outbound_id, open_token, click_token, open_capability_id, click_capability_id) = {
        let mut tx = db.begin(world.workspace_id).await.unwrap();
        let recipient_id = test_fixtures::insert_email_address(
            &mut tx,
            &format!("engagement-api+{}@example.test", Uuid::new_v4()),
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
            Some("https://example.test/article?fixed=1"),
        )
        .await
        .unwrap();
        let open_capability_id = capability_id(&mut tx, &open_token).await;
        let click_capability_id = capability_id(&mut tx, &click_token).await;
        tx.commit().await.unwrap();
        (
            outbound_id,
            open_token,
            click_token,
            open_capability_id,
            click_capability_id,
        )
    };

    let app_db = db.clone().with_default_workspace(Some(world.workspace_id));
    let app = TestApp::spawn_with_state(AppState::with_fake_providers(
        app_db,
        Arc::new(Config::test()),
        None,
    ))
    .await;

    let click = app
        .get(&format!(
            "/public/v1/engagement/click/{click_token}?target=https%3A%2F%2Fevil.test%2F"
        ))
        .await;
    assert_eq!(click.status(), StatusCode::FOUND);
    assert_eq!(
        click
            .headers()
            .get("location")
            .and_then(|value| value.to_str().ok()),
        Some("https://example.test/article?fixed=1"),
        "query parameters cannot override the stored redirect target"
    );
    assert!(
        click
            .headers()
            .get("cache-control")
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.contains("no-store"))
    );

    let pixel = app
        .get(&format!("/public/v1/engagement/open/{open_token}"))
        .await;
    assert_eq!(pixel.status(), StatusCode::OK);
    assert_eq!(
        pixel
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok()),
        Some("image/gif")
    );
    assert_eq!(pixel.bytes().await.unwrap().len(), 43);

    let invalid_open = app
        .get(&format!(
            "/public/v1/engagement/open/{}",
            flip_first_token_character(&open_token)
        ))
        .await;
    assert_eq!(invalid_open.status(), StatusCode::OK);
    let invalid_click = app
        .get(&format!(
            "/public/v1/engagement/click/{}?target=https%3A%2F%2Fevil.test",
            flip_first_token_character(&click_token)
        ))
        .await;
    assert_eq!(invalid_click.status(), StatusCode::NOT_FOUND);

    {
        let mut tx = db.begin(world.workspace_id).await.unwrap();
        let counts = engagement::outbound_counts(&mut tx, world.admin.principal_id, outbound_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(counts.open_request_count, 1);
        assert_eq!(counts.click_request_count, 1);
        let status: String = sqlx::query_scalar(
            "SELECT status FROM outbound_messages WHERE workspace_id = $1 AND id = $2",
        )
        .bind(world.workspace_id)
        .bind(outbound_id)
        .fetch_one(tx.conn())
        .await
        .unwrap();
        assert_eq!(status, "draft", "engagement never mutates delivery state");
        tx.commit().await.unwrap();
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

    let expired_click = app
        .get(&format!(
            "/public/v1/engagement/click/{click_token}?target=https%3A%2F%2Fevil.test"
        ))
        .await;
    assert_eq!(expired_click.status(), StatusCode::FOUND);
    assert_eq!(
        expired_click
            .headers()
            .get("location")
            .and_then(|value| value.to_str().ok()),
        Some("https://example.test/article?fixed=1")
    );
    let expired_pixel = app
        .get(&format!("/public/v1/engagement/open/{open_token}"))
        .await;
    assert_eq!(expired_pixel.status(), StatusCode::OK);
    {
        let mut tx = db.begin(world.workspace_id).await.unwrap();
        let counts = engagement::outbound_counts(&mut tx, world.admin.principal_id, outbound_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(counts.open_request_count, 1);
        assert_eq!(counts.click_request_count, 1);
        tx.commit().await.unwrap();
    }
}

#[tokio::test]
async fn freeze_keeps_safe_styled_cta_while_rewriting_text_and_html_links() {
    let url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set — skipping tracked HTML freeze test");
            return;
        }
    };
    let owner = Db::connect(&url, None).await.unwrap();
    owner.migrate().await.unwrap();
    let db = Db::connect(&url, Some(ROLE_API)).await.unwrap();
    let world = seed_workspace(&db).await.unwrap();
    let (recipient_id, sender_identity_id, channel_id) = {
        let mut tx = db.begin(world.workspace_id).await.unwrap();
        let recipient_id = test_fixtures::insert_email_address(
            &mut tx,
            &format!("engagement-styled+{}@example.test", Uuid::new_v4()),
        )
        .await
        .unwrap();
        let sender_identity_id: Uuid = sqlx::query_scalar(
            "INSERT INTO sender_identities
                (workspace_id, mail_account_id, address, display_name, status, verified_at)
             VALUES ($1, $2, 'styled@test.dev', 'Styled Sender', 'verified', now())
             RETURNING id",
        )
        .bind(world.workspace_id)
        .bind(world.mail_account_id)
        .fetch_one(tx.conn())
        .await
        .unwrap();
        let channel_id: Uuid = sqlx::query_scalar(
            "INSERT INTO delivery_channels
                (workspace_id, mail_account_id, sender_identity_id, connection_id,
                 provider, purpose, name)
             VALUES ($1, $2, $3, $4, 'zoho_mail', 'correspondence', 'Styled Outreach')
             RETURNING id",
        )
        .bind(world.workspace_id)
        .bind(world.mail_account_id)
        .bind(sender_identity_id)
        .bind(world.connection_id)
        .fetch_one(tx.conn())
        .await
        .unwrap();
        tx.commit().await.unwrap();
        (recipient_id, sender_identity_id, channel_id)
    };

    let html = r##"<table style="width:100%;max-width:600px;border-collapse:collapse"><tr><td style="background-color:#f3f4f6;padding:24px"><a href="https://example.test/offer" style="display:inline-block;background-color:#171717;color:#ffffff;font-size:16px;font-weight:600;padding:12px 24px;border-radius:4px;text-decoration:none">Review offer</a></td></tr></table>"##;
    let ctx = ctx_for(&world, &world.admin, "engagement-styled-freeze");
    let draft = application::sending::create_draft(
        &db,
        &ctx,
        &CreateMessageDraftRequest {
            mail_account_id: world.mail_account_id,
            to_email_address_id: recipient_id,
            purpose: "correspondence".into(),
            subject: "A styled offer".into(),
            body_text: "Review offer: https://example.test/offer".into(),
            body_html: Some(html.into()),
            template_version_id: None,
            variables: None,
            thread_id: None,
            reply_to_message_id: None,
            expected_last_inbound_id: None,
            visibility: None,
        },
    )
    .await
    .unwrap();
    let frozen = application::sending::freeze_draft(
        &db,
        &ctx,
        draft.draft_id,
        draft.version,
        &FreezeMessageDraftRequest {
            delivery_channel_id: channel_id,
            sender_identity_id: Some(sender_identity_id),
            meeting_type_id: None,
            tracking_requested: true,
        },
    )
    .await
    .unwrap();

    let mut tx = db.begin(world.workspace_id).await.unwrap();
    let (body_text, body_html, payload_hash): (String, String, Vec<u8>) = sqlx::query_as(
        "SELECT body_text, body_html, payload_hash FROM outbound_messages
          WHERE workspace_id = $1 AND id = $2",
    )
    .bind(world.workspace_id)
    .bind(frozen.outbound_message_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    assert_eq!(payload_hash.len(), 32);
    assert!(body_html.contains("max-width:600px"), "{body_html}");
    assert!(
        body_html.contains("background-color:#171717"),
        "{body_html}"
    );
    assert!(body_html.contains("color:#ffffff"), "{body_html}");
    assert!(body_html.contains("padding:12px 24px"), "{body_html}");
    assert!(body_html.contains("border-radius:4px"), "{body_html}");
    assert!(
        body_html.contains("/public/v1/engagement/open/"),
        "{body_html}"
    );
    assert_eq!(
        click_token(&body_text),
        click_token(&body_html),
        "plain-text and styled HTML links share the recipient-bound redirect"
    );
    tx.commit().await.unwrap();
}

fn flip_first_token_character(token: &str) -> String {
    format!(
        "{}{}",
        if token.starts_with('A') { 'B' } else { 'A' },
        &token[1..]
    )
}

fn click_token(body: &str) -> &str {
    const MARKER: &str = "/public/v1/engagement/click/";
    let rest = &body[body.find(MARKER).expect("tracked click URL") + MARKER.len()..];
    rest.split(|character: char| character.is_whitespace() || matches!(character, '"' | '<' | '>'))
        .next()
        .unwrap()
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
