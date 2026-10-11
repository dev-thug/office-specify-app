//! Real HTTP MCP template authoring against an isolated PostgreSQL database.
//! No SMTP or external provider is used.
use chrono::Duration;
use infrastructure::{
    db::{Db, ROLE_API},
    repo::auth::create_service_credential,
    testing::{TestWorld, seed_workspace},
};
use mcp::server::{McpState, build_router};
use reqwest::StatusCode;
use serde_json::{Value, json};
use std::sync::Arc;
use uuid::Uuid;

struct Rig {
    owner: Db,
    world: TestWorld,
    url: String,
    write_token: String,
    read_only_token: String,
    server: tokio::task::JoinHandle<()>,
}

impl Drop for Rig {
    fn drop(&mut self) {
        self.server.abort();
    }
}

impl Rig {
    async fn new() -> Self {
        let database_url = std::env::var("MCP_TEST_DATABASE_URL")
            .expect("MCP_TEST_DATABASE_URL must point at the isolated template test database");
        assert!(
            database_url.ends_with("/outreach_mcp_template_verify"),
            "use only the isolated outreach_mcp_template_verify database"
        );
        let owner = Db::connect(&database_url, None).await.unwrap();
        owner.migrate().await.unwrap();
        let world = seed_workspace(&owner).await.unwrap();
        let db = Db::connect(&database_url, Some(ROLE_API))
            .await
            .unwrap()
            .with_default_workspace(Some(world.workspace_id));
        let mut tx = owner.begin(world.workspace_id).await.unwrap();
        let write_scopes = ["templates.read", "templates.write", "mail.compose"]
            .into_iter()
            .map(str::to_string)
            .collect::<Vec<_>>();
        let (_, write_token) = create_service_credential(
            &mut tx,
            world.agent.principal_id,
            "isolated template authoring",
            &write_scopes,
            Duration::hours(1),
        )
        .await
        .unwrap();
        let read_only_scopes = ["templates.read"]
            .into_iter()
            .map(str::to_string)
            .collect::<Vec<_>>();
        let (_, read_only_token) = create_service_credential(
            &mut tx,
            world.agent.principal_id,
            "isolated template reader",
            &read_only_scopes,
            Duration::hours(1),
        )
        .await
        .unwrap();
        sqlx::query("INSERT INTO mail_account_grants (workspace_id,mail_account_id,principal_id,can_read,can_compose,granted_by) VALUES ($1,$2,$3,true,true,$4)")
            .bind(world.workspace_id)
            .bind(world.mail_account_id)
            .bind(world.agent.principal_id)
            .bind(world.admin.principal_id)
            .execute(tx.conn())
            .await
            .unwrap();
        tx.commit().await.unwrap();

        let app = build_router(McpState {
            db: db.clone(),
            calendar: Arc::new(infrastructure::providers::fake::FakeMailProvider::new()),
            config: Arc::new(infrastructure::config::Config::test()),
            rate_limiter: None,
            allowed_origins: Arc::new(vec![]),
            tools: Arc::new(mcp::all_tools_with_key(
                b"isolated-mcp-template-authoring-key",
            )),
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/mcp", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let rig = Self {
            owner,
            world,
            url,
            write_token,
            read_only_token,
            server,
        };
        let initialized = rig
            .rpc(
                &rig.write_token,
                "initialize",
                json!({
                    "protocolVersion":"2025-11-25",
                    "capabilities":{},
                    "clientInfo":{"name":"office-template-authoring-test","version":"1"}
                }),
            )
            .await;
        assert_eq!(initialized["result"]["protocolVersion"], "2025-11-25");
        rig
    }

    async fn rpc(&self, token: &str, method: &str, params: Value) -> Value {
        let response = reqwest::Client::new()
            .post(&self.url)
            .bearer_auth(token)
            .header("MCP-Protocol-Version", "2025-11-25")
            .json(&json!({
                "jsonrpc":"2.0",
                "id":Uuid::new_v4().to_string(),
                "method":method,
                "params":params
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        response.json().await.unwrap()
    }

    async fn call(&self, token: &str, name: &str, args: Value) -> Value {
        let response = self
            .rpc(token, "tools/call", json!({"name":name,"arguments":args}))
            .await;
        assert!(response.get("error").is_none(), "{name}: {response}");
        assert_eq!(response["result"]["isError"], false, "{name}: {response}");
        assert_eq!(
            response["result"]["structuredContent"]["ok"], true,
            "{name}: {response}"
        );
        response["result"]["structuredContent"]["data"].clone()
    }

    async fn tool_error(&self, token: &str, name: &str, args: Value) -> Value {
        let response = self
            .rpc(token, "tools/call", json!({"name":name,"arguments":args}))
            .await;
        assert_eq!(response["result"]["isError"], true, "{name}: {response}");
        response["result"]["structuredContent"]["error"].clone()
    }

    fn version_args(template_id: Uuid, key: &str, html: &str) -> Value {
        json!({
            "template_id":template_id,
            "idempotency_key":key,
            "subject":"Hello {{ first_name }} from {{ company_name }}",
            "body_text":"Hi {{ first_name }}, meet with us: {{ meeting.booking_url }}. Unsubscribe: {{ compliance.unsubscribe_url }}.",
            "body_html":html,
            "variable_schema":{
                "type":"object",
                "properties":{
                    "first_name":{"type":"string","maxLength":80},
                    "company_name":{"type":"string","maxLength":120},
                    "meeting.booking_url":{"type":"string","format":"uri"},
                    "compliance.unsubscribe_url":{"type":"string","format":"uri"}
                },
                "required":["first_name","company_name","meeting.booking_url","compliance.unsubscribe_url"],
                "additionalProperties":false
            }
        })
    }

    async fn outbound_counts(&self) -> (i64, i64) {
        let mut tx = self.owner.begin(self.world.workspace_id).await.unwrap();
        let counts: (i64, i64) = sqlx::query_as(
            "SELECT
                (SELECT count(*) FROM outbound_messages WHERE workspace_id=$1),
                (SELECT count(*) FROM message_drafts WHERE workspace_id=$1)",
        )
        .bind(self.world.workspace_id)
        .fetch_one(tx.conn())
        .await
        .unwrap();
        tx.commit().await.unwrap();
        counts
    }
}

#[tokio::test]
async fn mcp_agents_can_create_immutable_templates_and_preview_without_queueing_mail() {
    let rig = Rig::new().await;
    let listed = rig.rpc(&rig.write_token, "tools/list", json!({})).await;
    let names: Vec<_> = listed["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect();
    assert!(names.contains(&"templates_create"));
    assert!(names.contains(&"templates_create_draft_version"));
    assert!(names.contains(&"templates_render_version"));

    let before = rig.outbound_counts().await;
    assert_eq!(before, (0, 0));
    let container_args = json!({
        "idempotency_key":"template-container-create-0001",
        "name":"Sales follow-up"
    });
    let created = rig
        .call(&rig.write_token, "templates_create", container_args.clone())
        .await;
    let replay = rig
        .call(&rig.write_token, "templates_create", container_args)
        .await;
    assert_eq!(created["template_id"], replay["template_id"]);
    assert_eq!(created["name"], "Sales follow-up");
    let template_id = Uuid::parse_str(created["template_id"].as_str().unwrap()).unwrap();

    let html = r##"<table style="width:100%;max-width:600px;margin:0 auto;border-collapse:collapse"><tr><td style="background-color:#f3f4f6;padding:24px"><p>Hello {{ first_name }} at {{ company_name }}.</p><a href="{{ meeting.booking_url }}" style="display:inline-block;background-color:#171717;color:#ffffff;font-size:16px;font-weight:600;font-family:Arial,sans-serif;text-align:center;line-height:1.5;padding:12px 24px;border:1px solid #171717;border-radius:4px;text-decoration:none">Book time</a> · <a href="{{ compliance.unsubscribe_url }}">Unsubscribe</a></td></tr></table>"##;
    let version_args = Rig::version_args(template_id, "template-version-create-0001", html);
    let version = rig
        .call(
            &rig.write_token,
            "templates_create_draft_version",
            version_args.clone(),
        )
        .await;
    let version_replay = rig
        .call(
            &rig.write_token,
            "templates_create_draft_version",
            version_args,
        )
        .await;
    assert_eq!(
        version["template_version_id"],
        version_replay["template_version_id"]
    );
    assert_eq!(version["version_no"], 1);
    let version_id = Uuid::parse_str(version["template_version_id"].as_str().unwrap()).unwrap();

    let rendered = rig
        .call(
            &rig.write_token,
            "templates_render_version",
            json!({
                "template_version_id":version_id,
                "variables":{"first_name":"Ari","company_name":"Northwind"}
            }),
        )
        .await;
    assert_eq!(rendered["subject"], "Hello Ari from Northwind");
    assert!(rendered["body_text"].as_str().unwrap().contains("Ari"));
    assert!(
        rendered["body_text"]
            .as_str()
            .unwrap()
            .contains("https://preview.invalid/public/v1/booking/preview")
    );
    assert!(
        rendered["body_text"]
            .as_str()
            .unwrap()
            .contains("https://preview.invalid/public/v1/unsubscribe/preview")
    );
    assert!(
        rendered["body_html"]
            .as_str()
            .unwrap()
            .contains("Northwind")
    );
    assert!(
        rendered["body_html"]
            .as_str()
            .unwrap()
            .contains("max-width:600px")
    );
    assert!(
        rendered["body_html"]
            .as_str()
            .unwrap()
            .contains("background-color:#171717")
    );
    assert!(
        rendered["body_html"]
            .as_str()
            .unwrap()
            .contains("display:inline-block")
    );
    assert!(
        rendered["body_html"]
            .as_str()
            .unwrap()
            .contains("text-decoration:none")
    );
    assert_eq!(rig.outbound_counts().await, (0, 0));
}

#[tokio::test]
async fn mcp_template_authoring_requires_write_scope_and_rejects_unsafe_html_and_missing_values() {
    let rig = Rig::new().await;
    let unicode_key_error = rig
        .tool_error(
            &rig.write_token,
            "templates_create",
            json!({
                "idempotency_key":"é".repeat(128),
                "name":"Unicode key must be rejected"
            }),
        )
        .await;
    assert_eq!(unicode_key_error["code"], "VALIDATION_FAILED");
    assert_eq!(
        unicode_key_error["message"],
        "idempotency_key must be 16..=128 ASCII characters"
    );

    let missing_scope = rig
        .tool_error(
            &rig.read_only_token,
            "templates_create",
            json!({
                "idempotency_key":"template-create-no-write-scope",
                "name":"Blocked template"
            }),
        )
        .await;
    assert_eq!(missing_scope["code"], "SCOPE_REQUIRED");

    let created = rig
        .call(
            &rig.write_token,
            "templates_create",
            json!({
                "idempotency_key":"template-create-for-validation",
                "name":"Validation cases"
            }),
        )
        .await;
    let template_id = Uuid::parse_str(created["template_id"].as_str().unwrap()).unwrap();
    let unsafe_args = Rig::version_args(
        template_id,
        "template-version-unsafe-html-0001",
        "<p>Hi</p><img src=x onerror=alert(1)>",
    );
    let unsafe_html = rig
        .tool_error(
            &rig.write_token,
            "templates_create_draft_version",
            unsafe_args,
        )
        .await;
    assert_eq!(unsafe_html["code"], "VALIDATION_FAILED");

    let valid_args = Rig::version_args(
        template_id,
        "template-version-required-vars-0001",
        "<p>Hello {{ first_name }}</p>",
    );
    let version = rig
        .call(
            &rig.write_token,
            "templates_create_draft_version",
            valid_args,
        )
        .await;
    let missing_compose_scope = rig
        .tool_error(
            &rig.read_only_token,
            "templates_render_version",
            json!({
                "template_version_id":version["template_version_id"],
                "variables":{"first_name":"Ari","company_name":"Northwind"}
            }),
        )
        .await;
    assert_eq!(missing_compose_scope["code"], "SCOPE_REQUIRED");
    let missing_value = rig
        .tool_error(
            &rig.write_token,
            "templates_render_version",
            json!({
                "template_version_id":version["template_version_id"],
                "variables":{}
            }),
        )
        .await;
    assert_eq!(missing_value["code"], "TEMPLATE_VARIABLE_MISSING");
    let listed = rig
        .call(
            &rig.write_token,
            "templates_list_versions",
            json!({"template_id":template_id}),
        )
        .await;
    assert_eq!(listed["versions"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn mcp_template_authoring_does_not_resolve_foreign_workspace_template_ids() {
    let rig = Rig::new().await;
    let foreign_world = seed_workspace(&rig.owner).await.unwrap();
    let mut tx = rig.owner.begin(foreign_world.workspace_id).await.unwrap();
    let foreign_template = infrastructure::repo::templates::create_template(
        &mut tx,
        "Foreign workspace template",
        foreign_world.admin.principal_id,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let foreign_attempt = rig
        .tool_error(
            &rig.write_token,
            "templates_create_draft_version",
            Rig::version_args(
                foreign_template.id,
                "template-version-foreign-workspace-0001",
                "<p>Hello {{ first_name }}</p>",
            ),
        )
        .await;
    assert_eq!(foreign_attempt["code"], "NOT_FOUND");
}
