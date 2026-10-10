//! Isolated MCP test for company-specific campaign drafts and approved batch
//! scheduling. The API uses fake providers and no dispatch worker is started.
use api::{AppState, testing::TestApp};
use chrono::Utc;
use contracts::{
    sales_campaigns::SetSalesCampaignSenderRequest,
    templates::{CreateEmailTemplateRequest, CreateTemplateVersionRequest},
};
use infrastructure::{
    config::Config,
    db::{Db, ROLE_API},
    testing::{TEST_PASSWORD, TestWorld, ctx_for, seed_workspace},
};
use mcp::server::{McpState, build_router};
use reqwest::StatusCode;
use serde_json::{Value, json};
use std::sync::Arc;
use uuid::Uuid;

struct TargetIds {
    company_id: Uuid,
    contact_id: Uuid,
    email_address_id: Uuid,
}

struct Rig {
    owner: Db,
    world: TestWorld,
    api: TestApp,
    manager_api: TestApp,
    csrf: String,
    manager_csrf: String,
    url: String,
    token: String,
    credential_id: Uuid,
    agent_grant_version: i64,
    run_id: Uuid,
    smtp_profile_id: Uuid,
    delivery_channel_id: Uuid,
    wrong_delivery_channel_id: Uuid,
    meeting_type_id: Uuid,
    client: reqwest::Client,
    server: tokio::task::JoinHandle<()>,
}

impl Drop for Rig {
    fn drop(&mut self) {
        self.server.abort();
    }
}

impl Rig {
    async fn new() -> Self {
        let db_url = std::env::var("MCP_TEST_DATABASE_URL")
            .expect("MCP_TEST_DATABASE_URL must use the isolated sales campaign database");
        assert_eq!(
            db_url, "postgres://hyunjoong@localhost:5432/outreach_sales_kanban_mcp",
            "tests must only use the dedicated outreach_sales_kanban_mcp database"
        );
        let owner = Db::connect(&db_url, None).await.unwrap();
        owner.migrate().await.unwrap();
        let world = seed_workspace(&owner).await.unwrap();
        let db = Db::connect(&db_url, Some(ROLE_API))
            .await
            .unwrap()
            .with_default_workspace(Some(world.workspace_id));

        let mut safety_tx = db.begin(world.workspace_id).await.unwrap();
        sqlx::query("UPDATE workspaces SET emergency_stop = false WHERE id = $1")
            .bind(world.workspace_id)
            .execute(safety_tx.conn())
            .await
            .unwrap();
        safety_tx.commit().await.unwrap();

        let (smtp_profile_id, delivery_channel_id, wrong_delivery_channel_id) =
            seed_smtp_profile(&owner, &world).await;
        let meeting_type_id = seed_meeting_type(&owner, &world).await;
        let (credential_id, token) = {
            let mut tx = owner.begin(world.workspace_id).await.unwrap();
            let scopes: Vec<String> = [
                "campaign.compose",
                "crm.read",
                "mail.read",
                "mail.compose",
                "mail.submit_approved",
                "templates.read",
            ]
            .into_iter()
            .map(String::from)
            .collect();
            let value = infrastructure::repo::auth::create_service_credential(
                &mut tx,
                world.agent.principal_id,
                "isolated personalized sales campaign MCP test",
                &scopes,
                chrono::Duration::hours(1),
            )
            .await
            .unwrap();
            tx.commit().await.unwrap();
            value
        };

        let api = TestApp::spawn_with_state(AppState::with_fake_providers(
            db.clone(),
            Arc::new(Config::test()),
            None,
        ))
        .await;
        let admin_login = api
            .login(world.admin.email.as_deref().unwrap(), TEST_PASSWORD)
            .await;
        assert_eq!(admin_login.status(), StatusCode::OK);
        let csrf = TestApp::csrf_token_from(&admin_login).unwrap();
        let manager_api = TestApp::spawn_with_state(api.state.clone()).await;
        let manager_login = manager_api
            .login(world.manager.email.as_deref().unwrap(), TEST_PASSWORD)
            .await;
        assert_eq!(manager_login.status(), StatusCode::OK);
        let manager_csrf = TestApp::csrf_token_from(&manager_login).unwrap();

        let admin_ctx = ctx_for(&world, &world.admin, "sales-campaign-sender-default");
        let mut tx = owner.begin(world.workspace_id).await.unwrap();
        application::sales_campaigns::set_sales_campaign_sender(
            &mut tx,
            &admin_ctx,
            &SetSalesCampaignSenderRequest { smtp_profile_id },
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();

        let router = build_router(McpState {
            db: db.clone(),
            calendar: Arc::new(infrastructure::providers::fake::FakeMailProvider::new()),
            allowed_origins: Arc::new(vec![]),
            tools: Arc::new(mcp::all_tools_with_key(
                b"isolated-sales-campaign-mcp-hmac-key-2026",
            )),
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/mcp", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let client = reqwest::Client::new();
        let mut rig = Self {
            owner,
            world,
            api,
            manager_api,
            csrf,
            manager_csrf,
            url,
            token,
            credential_id,
            agent_grant_version: 0,
            run_id: Uuid::nil(),
            smtp_profile_id,
            delivery_channel_id,
            wrong_delivery_channel_id,
            meeting_type_id,
            client,
            server,
        };

        let grant = human_data(
            rig.admin_put_agent_grant(json!({
                "can_read": true,
                "can_compose": true,
                "can_send": true,
                "can_approve": false,
                "can_assign": false
            }))
            .await,
            StatusCode::OK,
        )
        .await;
        assert_eq!(grant["can_send"], true);
        assert_eq!(grant["can_approve"], false);
        rig.agent_grant_version = grant["version"].as_i64().unwrap();

        let init = rig
            .rpc(
                "initialize",
                json!({
                    "protocolVersion":"2025-11-25",
                    "capabilities":{},
                    "clientInfo":{"name":"sales-campaign-test","version":"1"}
                }),
            )
            .await;
        assert_eq!(init["result"]["protocolVersion"], "2025-11-25");
        let notify = rig
            .client
            .post(&rig.url)
            .bearer_auth(&rig.token)
            .json(&json!({"jsonrpc":"2.0","method":"notifications/initialized"}))
            .send()
            .await
            .unwrap();
        assert_eq!(notify.status(), StatusCode::ACCEPTED);

        let run = human_data(
            post_as_human(
                &rig.api,
                &rig.csrf,
                "/api/v1/agent-runs",
                json!({
                    "agent_principal_id":rig.world.agent.principal_id,
                    "objective":"Prepare two researched, company-specific sales drafts for human review",
                    "budget":{"max_tool_calls":40,"max_proposals":3}
                }),
                None,
                "sales-campaign-test-agent-run-001",
            )
            .await,
            StatusCode::CREATED,
        )
        .await;
        rig.run_id = Uuid::parse_str(run["id"].as_str().unwrap()).unwrap();
        rig
    }

    async fn rpc(&self, method: &str, params: Value) -> Value {
        self.rpc_with_token(&self.token, method, params).await
    }

    async fn rpc_with_token(&self, token: &str, method: &str, params: Value) -> Value {
        let response = self
            .client
            .post(&self.url)
            .bearer_auth(token)
            .header("MCP-Protocol-Version", "2025-11-25")
            .header("X-Agent-Run-Id", self.run_id.to_string())
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

    async fn call(&self, name: &str, args: Value) -> Value {
        let response = self
            .rpc("tools/call", json!({"name":name,"arguments":args}))
            .await;
        assert!(response.get("error").is_none(), "{name}: {response}");
        assert_eq!(response["result"]["isError"], false, "{name}: {response}");
        assert_eq!(
            response["result"]["structuredContent"]["ok"], true,
            "{name}: {response}"
        );
        response["result"]["structuredContent"]["data"].clone()
    }

    async fn tool_error(&self, name: &str, args: Value) -> Value {
        let response = self
            .rpc("tools/call", json!({"name":name,"arguments":args}))
            .await;
        assert!(
            response.get("error").is_some() || response["result"]["isError"] == true,
            "{name} unexpectedly executed: {response}"
        );
        response
    }

    async fn tool_error_with_token(&self, token: &str, name: &str, args: Value) -> Value {
        let response = self
            .rpc_with_token(token, "tools/call", json!({"name":name,"arguments":args}))
            .await;
        assert!(
            response.get("error").is_some() || response["result"]["isError"] == true,
            "{name} unexpectedly executed: {response}"
        );
        response
    }

    async fn create_service_credential(&self, scopes: &[&str]) -> (Uuid, String) {
        let mut tx = self.owner.begin(self.world.workspace_id).await.unwrap();
        let scopes: Vec<String> = scopes.iter().map(|scope| (*scope).to_string()).collect();
        let credential = infrastructure::repo::auth::create_service_credential(
            &mut tx,
            self.world.agent.principal_id,
            "same-agent narrow replay regression",
            &scopes,
            chrono::Duration::hours(1),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        credential
    }

    async fn human_post(
        &self,
        path: &str,
        body: Value,
        version: Option<i64>,
        key: &str,
    ) -> reqwest::Response {
        post_as_human(&self.api, &self.csrf, path, body, version, key).await
    }

    async fn manager_post(
        &self,
        path: &str,
        body: Value,
        version: Option<i64>,
        key: &str,
    ) -> reqwest::Response {
        post_as_human(
            &self.manager_api,
            &self.manager_csrf,
            path,
            body,
            version,
            key,
        )
        .await
    }

    async fn admin_put_agent_grant(&self, body: Value) -> reqwest::Response {
        self.api
            .client
            .put(self.api.url(&format!(
                "/api/v1/mail-accounts/{}/grants/{}",
                self.world.mail_account_id, self.world.agent.principal_id
            )))
            .header("x-csrf-token", &self.csrf)
            .header("if-none-match", "*")
            .json(&body)
            .send()
            .await
            .unwrap()
    }

    async fn admin_update_agent_grant(&self, body: Value, version: i64) -> reqwest::Response {
        self.api
            .client
            .put(self.api.url(&format!(
                "/api/v1/mail-accounts/{}/grants/{}",
                self.world.mail_account_id, self.world.agent.principal_id
            )))
            .header("x-csrf-token", &self.csrf)
            .header("if-match", format!("\"{version}\""))
            .json(&body)
            .send()
            .await
            .unwrap()
    }
}

async fn post_as_human(
    app: &TestApp,
    csrf: &str,
    path: &str,
    body: Value,
    version: Option<i64>,
    key: &str,
) -> reqwest::Response {
    let mut request = app
        .client
        .post(app.url(path))
        .header("x-csrf-token", csrf)
        .header("idempotency-key", key)
        .json(&body);
    if let Some(version) = version {
        request = request.header("if-match", format!("\"{version}\""));
    }
    request.send().await.unwrap()
}

async fn human_data(response: reqwest::Response, expected: StatusCode) -> Value {
    let status = response.status();
    let value: Value = response.json().await.unwrap();
    assert_eq!(status, expected, "unexpected API response: {value}");
    value["data"].clone()
}

async fn problem(response: reqwest::Response, expected: StatusCode) -> Value {
    let status = response.status();
    let value: Value = response.json().await.unwrap();
    assert_eq!(status, expected, "unexpected API response: {value}");
    value
}

async fn seed_smtp_profile(owner: &Db, world: &TestWorld) -> (Uuid, Uuid, Uuid) {
    let mut tx = owner.begin(world.workspace_id).await.unwrap();
    let profile_id: Uuid = sqlx::query_scalar(
        "INSERT INTO integration_connections
            (workspace_id, provider, region, secret_ref, status, connected_by)
         VALUES ($1, 'delivery_provider', 'us', $2, 'active', $3)
         RETURNING id",
    )
    .bind(world.workspace_id)
    .bind(format!("sales-campaign-fake-smtp:{}", Uuid::new_v4()))
    .bind(world.admin.principal_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    let from_address = format!("sales-{}@test.dev", Uuid::new_v4().simple());
    let sender_identity_id: Uuid = sqlx::query_scalar(
        "INSERT INTO sender_identities
            (workspace_id, mail_account_id, address, display_name, status, verified_at)
         VALUES ($1, $2, $3, 'Specify Sales Test', 'verified', now())
         RETURNING id",
    )
    .bind(world.workspace_id)
    .bind(world.mail_account_id)
    .bind(&from_address)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    let delivery_channel_id: Uuid = sqlx::query_scalar(
        "INSERT INTO delivery_channels
            (workspace_id, mail_account_id, sender_identity_id, connection_id,
             provider, purpose, name, policy_status, enabled)
         VALUES ($1, $2, $3, $4, 'delivery_provider', 'marketing',
                 'isolated sales campaign SMTP', 'unverified', false)
         RETURNING id",
    )
    .bind(world.workspace_id)
    .bind(world.mail_account_id)
    .bind(sender_identity_id)
    .bind(profile_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO channel_reviews
            (workspace_id, delivery_channel_id, reviewed_by, result, purpose,
             contract_evidence_ref, notes)
         VALUES ($1, $2, $3, 'allowed', 'marketing',
                 'isolated-fake-smtp-policy-fixture', 'No email is sent by this test')",
    )
    .bind(world.workspace_id)
    .bind(delivery_channel_id)
    .bind(world.admin.principal_id)
    .execute(tx.conn())
    .await
    .unwrap();

    let wrong_connection_id: Uuid = sqlx::query_scalar(
        "INSERT INTO integration_connections
            (workspace_id, provider, region, secret_ref, status, connected_by)
         VALUES ($1, 'delivery_provider', 'us', $2, 'active', $3)
         RETURNING id",
    )
    .bind(world.workspace_id)
    .bind(format!("sales-campaign-wrong-channel:{}", Uuid::new_v4()))
    .bind(world.admin.principal_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    let wrong_sender_id: Uuid = sqlx::query_scalar(
        "INSERT INTO sender_identities
            (workspace_id, mail_account_id, address, display_name, status, verified_at)
         VALUES ($1, $2, $3, 'Wrong Sender Test', 'verified', now()) RETURNING id",
    )
    .bind(world.workspace_id)
    .bind(world.mail_account_id)
    .bind(format!("wrong-{}@test.dev", Uuid::new_v4().simple()))
    .fetch_one(tx.conn())
    .await
    .unwrap();
    let wrong_channel_id: Uuid = sqlx::query_scalar(
        "INSERT INTO delivery_channels
            (workspace_id, mail_account_id, sender_identity_id, connection_id,
             provider, purpose, name, policy_status, enabled)
         VALUES ($1, $2, $3, $4, 'delivery_provider', 'marketing',
                 'other valid SMTP channel', 'allowed', true) RETURNING id",
    )
    .bind(world.workspace_id)
    .bind(world.mail_account_id)
    .bind(wrong_sender_id)
    .bind(wrong_connection_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    sqlx::query(
        "UPDATE delivery_channels SET policy_status = 'allowed', enabled = true
          WHERE workspace_id = $1 AND id = $2",
    )
    .bind(world.workspace_id)
    .bind(delivery_channel_id)
    .execute(tx.conn())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO smtp_connections
            (id, workspace_id, mail_account_id, sender_identity_id, channel_id,
             name, host, port, security, username, from_address, from_name,
             status, has_password)
         VALUES ($1, $2, $3, $4, $5, 'isolated sales SMTP', 'smtp.invalid', 587,
                 'starttls', 'test-user', $6, 'Specify Sales Test',
                 'active', true)",
    )
    .bind(profile_id)
    .bind(world.workspace_id)
    .bind(world.mail_account_id)
    .bind(sender_identity_id)
    .bind(delivery_channel_id)
    .bind(&from_address)
    .execute(tx.conn())
    .await
    .unwrap();
    tx.commit().await.unwrap();
    (profile_id, delivery_channel_id, wrong_channel_id)
}

async fn seed_meeting_type(owner: &Db, world: &TestWorld) -> Uuid {
    let mut tx = owner.begin(world.workspace_id).await.unwrap();
    let calendar_connection: Uuid = sqlx::query_scalar(
        "INSERT INTO integration_connections
            (workspace_id, provider, region, secret_ref, status, connected_by)
         VALUES ($1, 'zoho_calendar', 'us', $2, 'active', $3) RETURNING id",
    )
    .bind(world.workspace_id)
    .bind(format!("sales-campaign-calendar:{}", Uuid::new_v4()))
    .bind(world.admin.principal_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    let calendar_id: Uuid = sqlx::query_scalar(
        "INSERT INTO calendar_connections
            (workspace_id, host_principal_id, connection_id, provider_calendar_id, status)
         VALUES ($1, $2, $3, $4, 'active') RETURNING id",
    )
    .bind(world.workspace_id)
    .bind(world.admin.principal_id)
    .bind(calendar_connection)
    .bind(format!("sales-campaign-calendar-{}", Uuid::new_v4()))
    .fetch_one(tx.conn())
    .await
    .unwrap();
    let meeting_type_id: Uuid = sqlx::query_scalar(
        "INSERT INTO meeting_types
            (workspace_id, host_principal_id, calendar_connection_id, name,
             duration_minutes, minimum_notice_minutes, booking_horizon_days)
         VALUES ($1, $2, $3, 'Sales follow-up', 30, 0, 30) RETURNING id",
    )
    .bind(world.workspace_id)
    .bind(world.admin.principal_id)
    .bind(calendar_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    tx.commit().await.unwrap();
    meeting_type_id
}

async fn create_sales_test_template(owner: &Db, world: &TestWorld) -> Uuid {
    let ctx = ctx_for(world, &world.admin, "sales-campaign-template-fixture");
    let template = application::templates::create(
        owner,
        &ctx,
        &CreateEmailTemplateRequest {
            name: format!("Company-specific sales starter {}", Uuid::new_v4().simple()),
        },
    )
    .await
    .unwrap();
    let version = application::templates::create_version(
        owner,
        &ctx,
        template.template_id,
        &CreateTemplateVersionRequest {
            subject: "Starter subject for {{contact.first_name}}".into(),
            body_text: "Starter body for {{contact.first_name}}".into(),
            body_html: Some("<p>Starter HTML for {{contact.first_name}}</p>".into()),
            variable_schema: json!({
                "type":"object",
                "properties":{"contact.first_name":{"type":"string"}},
                "required":["contact.first_name"],
                "additionalProperties":false
            }),
        },
    )
    .await
    .unwrap();
    version.template_version_id
}

async fn seed_crm_target(owner: &Db, world: &TestWorld, label: &str) -> TargetIds {
    let mut tx = owner.begin(world.workspace_id).await.unwrap();
    let company_id: Uuid = sqlx::query_scalar(
        "INSERT INTO companies
            (workspace_id, name, domain, industry, country_code, owner_principal_id)
         VALUES ($1, $2, $3, 'Software', 'KR', $4) RETURNING id",
    )
    .bind(world.workspace_id)
    .bind(format!(
        "{label} {suffix}",
        suffix = Uuid::new_v4().simple()
    ))
    .bind(format!("{label}-{}.example.test", Uuid::new_v4().simple()))
    .bind(world.admin.principal_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    let contact_id: Uuid = sqlx::query_scalar(
        "INSERT INTO contacts (workspace_id, company_id, name, status, owner_principal_id)
         VALUES ($1, $2, $3, 'active', $4) RETURNING id",
    )
    .bind(world.workspace_id)
    .bind(company_id)
    .bind(format!("{label} Contact"))
    .bind(world.admin.principal_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    let email = format!(
        "{}-{}@example.test",
        label.to_ascii_lowercase(),
        Uuid::new_v4()
    );
    let email_address_id: Uuid = sqlx::query_scalar(
        "INSERT INTO email_addresses
            (workspace_id, address_original, normalized_key, kind,
             verification_status, verified_at)
         VALUES ($1, $2, $3, 'person', 'verified', now()) RETURNING id",
    )
    .bind(world.workspace_id)
    .bind(&email)
    .bind(email.to_ascii_lowercase())
    .fetch_one(tx.conn())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO contact_emails (workspace_id, contact_id, email_address_id, is_primary)
         VALUES ($1, $2, $3, true)",
    )
    .bind(world.workspace_id)
    .bind(contact_id)
    .bind(email_address_id)
    .execute(tx.conn())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO communication_bases
            (workspace_id, email_address_id, purpose, jurisdiction, basis_type,
             status, evidence_ref, collected_at, reviewed_by, reviewed_at)
         VALUES ($1, $2, 'marketing', 'KR', 'explicit_consent', 'valid',
                 'isolated-sales-campaign-test-consent', now(), $3, now())",
    )
    .bind(world.workspace_id)
    .bind(email_address_id)
    .bind(world.admin.principal_id)
    .execute(tx.conn())
    .await
    .unwrap();
    tx.commit().await.unwrap();
    TargetIds {
        company_id,
        contact_id,
        email_address_id,
    }
}

fn new_key(prefix: &str) -> String {
    format!("{prefix}-{}", Uuid::new_v4().simple())
}

#[tokio::test]
async fn personalized_campaign_keeps_target_copy_and_queues_only_approved_current_message() {
    let rig = Rig::new().await;
    let aurora = seed_crm_target(&rig.owner, &rig.world, "aurora").await;
    let forge = seed_crm_target(&rig.owner, &rig.world, "forge").await;
    let template_version_id = create_sales_test_template(&rig.owner, &rig.world).await;

    let listed = rig.rpc("tools/list", json!({})).await;
    let tool_names: Vec<_> = listed["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect();
    for name in [
        "sales_campaigns_list",
        "sales_campaigns_create",
        "sales_campaigns_get",
        "sales_campaigns_add_target",
        "sales_campaigns_create_message",
        "sales_campaigns_move_stage",
        "sales_campaigns_queue",
    ] {
        assert!(tool_names.contains(&name), "missing MCP tool {name}");
    }
    assert!(!tool_names.contains(&"approvals_decide"));
    assert!(!tool_names.contains(&"mail_submit"));

    let campaign = rig
        .call(
            "sales_campaigns_create",
            json!({
                "idempotency_key":new_key("sales-campaign-create"),
                "name":"Two-company campaign",
                "objective":"Start conversations with relevant operators",
                "meeting_type_id":rig.meeting_type_id
            }),
        )
        .await;
    let campaign_id = Uuid::parse_str(campaign["id"].as_str().unwrap()).unwrap();
    assert_eq!(campaign["smtp_profile_id"], rig.smtp_profile_id.to_string());
    assert_eq!(
        campaign["delivery_channel_id"],
        rig.delivery_channel_id.to_string()
    );
    let campaigns = rig.call("sales_campaigns_list", json!({"limit":10})).await;
    assert!(
        campaigns["campaigns"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["id"] == campaign_id.to_string())
    );

    let aurora_target = rig
        .call(
            "sales_campaigns_add_target",
            json!({
                "idempotency_key":new_key("sales-campaign-aurora-target"),
                "campaign_id":campaign_id,
                "company_id":aurora.company_id,
                "contact_id":aurora.contact_id,
                "email_address_id":aurora.email_address_id
            }),
        )
        .await;
    let forge_target = rig
        .call(
            "sales_campaigns_add_target",
            json!({
                "idempotency_key":new_key("sales-campaign-forge-target"),
                "campaign_id":campaign_id,
                "company_id":forge.company_id,
                "contact_id":forge.contact_id,
                "email_address_id":forge.email_address_id
            }),
        )
        .await;
    let aurora_target_id = Uuid::parse_str(aurora_target["id"].as_str().unwrap()).unwrap();
    let forge_target_id = Uuid::parse_str(forge_target["id"].as_str().unwrap()).unwrap();
    assert_ne!(aurora_target_id, forge_target_id);

    let aurora_message = rig
        .call(
            "sales_campaigns_create_message",
            json!({
                "idempotency_key":new_key("sales-campaign-aurora-message"),
                "campaign_id":campaign_id,
                "target_id":aurora_target_id,
                "subject":"A company-specific storage question",
                "body_text":"Hi {{contact.first_name}}, I saw Aurora's storage pilot and have a question tailored to your team.",
                "body_html":"<p>Hi {{contact.first_name}}, I saw Aurora's storage pilot.</p>",
                "template_version_id":template_version_id,
                "variables":{"contact.first_name":"Ara"}
            }),
        )
        .await;
    let forge_message = rig
        .call(
            "sales_campaigns_create_message",
            json!({
                "idempotency_key":new_key("sales-campaign-forge-message"),
                "campaign_id":campaign_id,
                "target_id":forge_target_id,
                "subject":"A company-specific robotics question",
                "body_text":"Hi {{contact.first_name}}, I saw Forge's picking automation work and have a question for your team.",
                "body_html":"<p>Hi {{contact.first_name}}, I saw Forge's robotics work.</p>",
                "template_version_id":template_version_id,
                "variables":{"contact.first_name":"Min"}
            }),
        )
        .await;
    assert_eq!(aurora_message["approval_required"], true);
    assert_eq!(forge_message["approval_required"], true);
    assert_ne!(aurora_message["draft_id"], forge_message["draft_id"]);
    assert!(
        aurora_message["body_text"]
            .as_str()
            .unwrap()
            .contains("Aurora")
    );
    assert!(
        forge_message["body_text"]
            .as_str()
            .unwrap()
            .contains("Forge")
    );
    assert!(
        aurora_message["body_html"]
            .as_str()
            .unwrap()
            .contains("Aurora")
    );
    assert!(
        forge_message["body_html"]
            .as_str()
            .unwrap()
            .contains("Forge")
    );
    assert_eq!(aurora_message["sales_campaign_id"], campaign_id.to_string());
    assert_eq!(
        forge_message["sales_target_id"],
        forge_target_id.to_string()
    );

    let aurora_draft_id = Uuid::parse_str(aurora_message["draft_id"].as_str().unwrap()).unwrap();
    let original_aurora_version = aurora_message["version"].as_i64().unwrap();
    let wrong_channel = problem(
        rig.human_post(
            &format!("/api/v1/message-drafts/{aurora_draft_id}/freeze"),
            json!({"delivery_channel_id":rig.wrong_delivery_channel_id}),
            Some(original_aurora_version),
            &new_key("sales-campaign-wrong-channel-freeze"),
        )
        .await,
        StatusCode::BAD_REQUEST,
    )
    .await;
    assert_eq!(wrong_channel["code"], "VALIDATION_FAILED");

    let recipient_tamper = rig
        .api
        .client
        .patch(
            rig.api
                .url(&format!("/api/v1/message-drafts/{aurora_draft_id}")),
        )
        .header("x-csrf-token", &rig.csrf)
        .header("if-match", format!("\"{original_aurora_version}\""))
        .json(&json!({"to_email_address_id":forge.email_address_id}))
        .send()
        .await
        .unwrap();
    let recipient_tamper = problem(recipient_tamper, StatusCode::BAD_REQUEST).await;
    assert_eq!(recipient_tamper["code"], "VALIDATION_FAILED");
    let private_visibility = rig
        .api
        .client
        .patch(
            rig.api
                .url(&format!("/api/v1/message-drafts/{aurora_draft_id}")),
        )
        .header("x-csrf-token", &rig.csrf)
        .header("if-match", format!("\"{original_aurora_version}\""))
        .json(&json!({"visibility":"private"}))
        .send()
        .await
        .unwrap();
    let private_visibility = problem(private_visibility, StatusCode::BAD_REQUEST).await;
    assert_eq!(private_visibility["code"], "VALIDATION_FAILED");

    let before_review = rig
        .call("sales_campaigns_get", json!({"campaign_id":campaign_id}))
        .await;
    let aurora_version = before_review["targets"]
        .as_array()
        .unwrap()
        .iter()
        .find(|target| target["id"] == aurora_target_id.to_string())
        .unwrap()["version"]
        .as_i64()
        .unwrap();
    let forge_version = before_review["targets"]
        .as_array()
        .unwrap()
        .iter()
        .find(|target| target["id"] == forge_target_id.to_string())
        .unwrap()["version"]
        .as_i64()
        .unwrap();
    let aurora_review_stage = rig
        .call(
            "sales_campaigns_move_stage",
            json!({
                "idempotency_key":new_key("sales-campaign-aurora-review-stage"),
                "campaign_id":campaign_id,
                "target_id":aurora_target_id,
                "stage":"review",
                "expected_target_version":aurora_version
            }),
        )
        .await;
    let forge_review_stage = rig
        .call(
            "sales_campaigns_move_stage",
            json!({
                "idempotency_key":new_key("sales-campaign-forge-review-stage"),
                "campaign_id":campaign_id,
                "target_id":forge_target_id,
                "stage":"review",
                "expected_target_version":forge_version
            }),
        )
        .await;
    assert_eq!(aurora_review_stage["workflow_stage"], "review");
    assert_eq!(forge_review_stage["workflow_stage"], "review");

    let forge_draft_id = Uuid::parse_str(forge_message["draft_id"].as_str().unwrap()).unwrap();
    let aurora_frozen = freeze_and_request_review(
        &rig,
        aurora_draft_id,
        original_aurora_version,
        campaign_id,
        aurora_target_id,
        None,
        "sales-campaign-aurora",
    )
    .await;
    let forge_frozen = freeze_and_request_review(
        &rig,
        forge_draft_id,
        forge_message["version"].as_i64().unwrap(),
        campaign_id,
        forge_target_id,
        None,
        "sales-campaign-forge",
    )
    .await;
    let aurora_outbound = human_data(
        rig.api
            .get(&format!(
                "/api/v1/outbound-messages/{}",
                aurora_frozen["outbound_message_id"].as_str().unwrap()
            ))
            .await,
        StatusCode::OK,
    )
    .await;
    let forge_outbound = human_data(
        rig.api
            .get(&format!(
                "/api/v1/outbound-messages/{}",
                forge_frozen["outbound_message_id"].as_str().unwrap()
            ))
            .await,
        StatusCode::OK,
    )
    .await;
    assert_eq!(
        aurora_outbound["subject"],
        "A company-specific storage question"
    );
    assert_eq!(
        forge_outbound["subject"],
        "A company-specific robotics question"
    );
    assert!(
        aurora_outbound["body_text"]
            .as_str()
            .unwrap()
            .contains("Aurora's storage pilot")
    );
    assert!(
        forge_outbound["body_text"]
            .as_str()
            .unwrap()
            .contains("Forge's picking automation")
    );
    assert!(
        aurora_outbound["body_text"]
            .as_str()
            .unwrap()
            .contains("Hi Ara")
    );
    assert!(
        forge_outbound["body_text"]
            .as_str()
            .unwrap()
            .contains("Hi Min")
    );
    assert!(
        !aurora_outbound["body_text"]
            .as_str()
            .unwrap()
            .contains("Starter body")
    );
    assert!(
        !forge_outbound["body_text"]
            .as_str()
            .unwrap()
            .contains("Starter body")
    );
    assert!(
        aurora_outbound["body_html"]
            .as_str()
            .unwrap()
            .contains("Aurora's storage pilot")
    );
    assert!(
        forge_outbound["body_html"]
            .as_str()
            .unwrap()
            .contains("Forge's robotics work")
    );
    assert!(
        !aurora_outbound["body_html"]
            .as_str()
            .unwrap()
            .contains("Starter HTML")
    );
    assert!(
        !forge_outbound["body_html"]
            .as_str()
            .unwrap()
            .contains("Starter HTML")
    );

    // A frozen first draft is stale as soon as a replacement becomes current.
    // The outbound was never approved; the binding guard must reject it before
    // any attempt to schedule a send.
    let replacement = rig
        .call(
            "sales_campaigns_create_message",
            json!({
                "idempotency_key":new_key("sales-campaign-aurora-replacement"),
                "campaign_id":campaign_id,
                "target_id":aurora_target_id,
                "subject":"Updated Aurora subject",
                "body_text":"Updated copy remains specific to Aurora with {{meeting.booking_url}}.",
                "body_html":"<p>Updated Aurora copy: <a href=\"{{meeting.booking_url}}\">Schedule a follow-up</a></p>"
            }),
        )
        .await;
    let replacement_id = Uuid::parse_str(replacement["draft_id"].as_str().unwrap()).unwrap();
    let conflicting_meeting_type = problem(
        rig.human_post(
            &format!("/api/v1/message-drafts/{replacement_id}/freeze"),
            json!({
                "delivery_channel_id":rig.delivery_channel_id,
                "meeting_type_id":Uuid::new_v4()
            }),
            Some(replacement["version"].as_i64().unwrap()),
            &new_key("sales-campaign-conflicting-meeting-type"),
        )
        .await,
        StatusCode::BAD_REQUEST,
    )
    .await;
    assert_eq!(conflicting_meeting_type["code"], "VALIDATION_FAILED");
    let replacement_frozen = human_data(
        rig.human_post(
            &format!("/api/v1/message-drafts/{replacement_id}/freeze"),
            json!({"delivery_channel_id":rig.delivery_channel_id}),
            Some(replacement["version"].as_i64().unwrap()),
            &new_key("sales-campaign-raw-booking-freeze"),
        )
        .await,
        StatusCode::CREATED,
    )
    .await;
    let replacement_outbound = human_data(
        rig.api
            .get(&format!(
                "/api/v1/outbound-messages/{}",
                replacement_frozen["outbound_message_id"].as_str().unwrap()
            ))
            .await,
        StatusCode::OK,
    )
    .await;
    assert!(
        replacement_outbound["body_text"]
            .as_str()
            .unwrap()
            .contains("http://localhost:3000/public/book/")
    );
    assert!(
        !replacement_outbound["body_text"]
            .as_str()
            .unwrap()
            .contains("{{meeting.booking_url}}")
    );
    let superseded_candidate = rig
        .call(
            "sales_campaigns_create_message",
            json!({
                "idempotency_key":new_key("sales-campaign-aurora-superseded-candidate"),
                "campaign_id":campaign_id,
                "target_id":aurora_target_id,
                "subject":"Superseded before freeze",
                "body_text":"This draft must not freeze after a newer target draft exists."
            }),
        )
        .await;
    let latest_aurora = rig
        .call(
            "sales_campaigns_create_message",
            json!({
                "idempotency_key":new_key("sales-campaign-aurora-latest"),
                "campaign_id":campaign_id,
                "target_id":aurora_target_id,
                "subject":"Latest Aurora copy",
                "body_text":"This latest Aurora draft remains editable."
            }),
        )
        .await;
    let superseded_draft_id =
        Uuid::parse_str(superseded_candidate["draft_id"].as_str().unwrap()).unwrap();
    let superseded_freeze = problem(
        rig.human_post(
            &format!("/api/v1/message-drafts/{superseded_draft_id}/freeze"),
            json!({"delivery_channel_id":rig.delivery_channel_id}),
            Some(superseded_candidate["version"].as_i64().unwrap()),
            &new_key("sales-campaign-superseded-freeze"),
        )
        .await,
        StatusCode::CONFLICT,
    )
    .await;
    assert_eq!(superseded_freeze["code"], "CONFLICT");
    assert!(latest_aurora["draft_id"].is_string());
    let stale_version = rig
        .call("sales_campaigns_get", json!({"campaign_id":campaign_id}))
        .await["targets"]
        .as_array()
        .unwrap()
        .iter()
        .find(|target| target["id"] == aurora_target_id.to_string())
        .unwrap()["version"]
        .as_i64()
        .unwrap();
    let stale = rig
        .tool_error(
            "sales_campaigns_queue",
            json!({
                "idempotency_key":new_key("sales-campaign-stale"),
                "campaign_id":campaign_id,
                "target_id":aurora_target_id,
                "outbound_message_id":aurora_frozen["outbound_message_id"],
                "expected_payload_hash":aurora_frozen["payload_hash"],
                "expected_target_version":stale_version
            }),
        )
        .await;
    assert!(stale["result"]["isError"] == true || stale.get("error").is_some());
    assert!(replacement["draft_id"].is_string());

    // An outbound from another campaign target cannot be attached to Aurora.
    let current_aurora = rig
        .call("sales_campaigns_get", json!({"campaign_id":campaign_id}))
        .await["targets"]
        .as_array()
        .unwrap()
        .iter()
        .find(|target| target["id"] == aurora_target_id.to_string())
        .unwrap()["version"]
        .as_i64()
        .unwrap();
    let foreign = rig
        .tool_error(
            "sales_campaigns_queue",
            json!({
                "idempotency_key":new_key("sales-campaign-foreign"),
                "campaign_id":campaign_id,
                "target_id":aurora_target_id,
                "outbound_message_id":forge_frozen["outbound_message_id"],
                "expected_payload_hash":forge_frozen["payload_hash"],
                "expected_target_version":current_aurora
            }),
        )
        .await;
    assert!(foreign["result"]["isError"] == true || foreign.get("error").is_some());

    let unapproved = rig
        .tool_error(
            "sales_campaigns_queue",
            json!({
                "idempotency_key":new_key("sales-campaign-unapproved"),
                "campaign_id":campaign_id,
                "target_id":forge_target_id,
                "outbound_message_id":forge_frozen["outbound_message_id"],
                "expected_payload_hash":forge_frozen["payload_hash"],
                "expected_target_version":forge_frozen["target_version"]
            }),
        )
        .await;
    assert!(unapproved["result"]["isError"] == true || unapproved.get("error").is_some());

    let approved = human_data(
        rig.manager_post(
            &format!(
                "/api/v1/approvals/{}/decisions",
                forge_frozen["approval_request_id"].as_str().unwrap()
            ),
            json!({
                "decision":"approved",
                "expected_snapshot_hash":forge_frozen["payload_hash"],
                "reason":"Reviewed the exact company-specific Forge snapshot"
            }),
            Some(1),
            &new_key("sales-campaign-human-approval"),
        )
        .await,
        StatusCode::OK,
    )
    .await;
    assert_eq!(approved["approval_status"], "approved");
    assert_eq!(approved["execution_status"], "ready");

    let before_queue = rig
        .call("sales_campaigns_get", json!({"campaign_id":campaign_id}))
        .await;
    let forge_current_version = before_queue["targets"]
        .as_array()
        .unwrap()
        .iter()
        .find(|target| target["id"] == forge_target_id.to_string())
        .unwrap()["version"]
        .as_i64()
        .unwrap();
    let queue_args = json!({
        "idempotency_key":"sales-campaign-approved-queue-0001",
        "campaign_id":campaign_id,
        "target_id":forge_target_id,
        "outbound_message_id":forge_frozen["outbound_message_id"],
        "expected_payload_hash":forge_frozen["payload_hash"],
        "expected_target_version":forge_current_version
    });
    let queued = rig.call("sales_campaigns_queue", queue_args.clone()).await;
    let replayed = rig.call("sales_campaigns_queue", queue_args).await;
    assert_eq!(queued["submission"]["status"], "queued");
    assert_eq!(queued["submission"]["provider_accepted"], false);
    assert_eq!(
        queued["submission"]["job_id"],
        replayed["submission"]["job_id"]
    );
    assert_eq!(queued["batch_at"], replayed["batch_at"]);

    let batch_at = chrono::DateTime::parse_from_rfc3339(queued["batch_at"].as_str().unwrap())
        .unwrap()
        .with_timezone(&Utc);
    assert_eq!(batch_at.timestamp().rem_euclid(600), 0);
    assert!(batch_at >= Utc::now());
    let outbound_id =
        Uuid::parse_str(forge_frozen["outbound_message_id"].as_str().unwrap()).unwrap();
    let mut tx = rig.owner.begin(rig.world.workspace_id).await.unwrap();
    let (source_credential_id, not_before): (Option<Uuid>, Option<chrono::DateTime<Utc>>) =
        sqlx::query_as(
            "SELECT source_credential_id, not_before FROM outbound_messages
              WHERE workspace_id=$1 AND id=$2",
        )
        .bind(rig.world.workspace_id)
        .bind(outbound_id)
        .fetch_one(tx.conn())
        .await
        .unwrap();
    assert_eq!(source_credential_id, Some(rig.credential_id));
    assert_eq!(not_before, Some(batch_at));
    let queued_jobs: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM jobs WHERE workspace_id=$1
           AND kind='sending.dispatch_outbound'
           AND payload->>'outbound_message_id'=$2 AND state='queued'",
    )
    .bind(rig.world.workspace_id)
    .bind(outbound_id.to_string())
    .fetch_one(tx.conn())
    .await
    .unwrap();
    assert_eq!(queued_jobs, 1, "same-key replay stores one durable job");
    let attempts: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM send_attempts WHERE workspace_id=$1 AND outbound_message_id=$2",
    )
    .bind(rig.world.workspace_id)
    .bind(outbound_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    assert_eq!(attempts, 0, "this test only schedules and never sends mail");
    sqlx::query(
        "UPDATE outbound_messages SET status='unknown', version=version+1
          WHERE workspace_id=$1 AND id=$2",
    )
    .bind(rig.world.workspace_id)
    .bind(outbound_id)
    .execute(tx.conn())
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let unknown_target_version = rig
        .call("sales_campaigns_get", json!({"campaign_id":campaign_id}))
        .await["targets"]
        .as_array()
        .unwrap()
        .iter()
        .find(|target| target["id"] == forge_target_id.to_string())
        .unwrap()["version"]
        .as_i64()
        .unwrap();
    let unknown_retry = rig
        .tool_error(
            "sales_campaigns_queue",
            json!({
                "idempotency_key":new_key("sales-campaign-unknown-retry"),
                "campaign_id":campaign_id,
                "target_id":forge_target_id,
                "outbound_message_id":outbound_id,
                "expected_payload_hash":forge_frozen["payload_hash"],
                "expected_target_version":unknown_target_version
            }),
        )
        .await;
    assert!(unknown_retry["result"]["isError"] == true || unknown_retry.get("error").is_some());
    let mut tx = rig.owner.begin(rig.world.workspace_id).await.unwrap();
    let queued_jobs_after_unknown: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM jobs WHERE workspace_id=$1
           AND kind='sending.dispatch_outbound'
           AND payload->>'outbound_message_id'=$2 AND state='queued'",
    )
    .bind(rig.world.workspace_id)
    .bind(outbound_id.to_string())
    .fetch_one(tx.conn())
    .await
    .unwrap();
    assert_eq!(
        queued_jobs_after_unknown, 1,
        "unknown state must not retry or duplicate"
    );
    tx.rollback().await.unwrap();
}

#[tokio::test]
async fn legacy_and_human_approved_submit_paths_keep_linked_campaigns_batched() {
    let rig = Rig::new().await;
    let target = seed_crm_target(&rig.owner, &rig.world, "legacy-submit").await;
    let campaign = rig
        .call(
            "sales_campaigns_create",
            json!({
                "idempotency_key":new_key("legacy-submit-campaign"),
                "name":"Legacy submit guard",
                "objective":"Verify the old approved-submit entrypoint keeps campaign batching"
            }),
        )
        .await;
    let campaign_id = Uuid::parse_str(campaign["id"].as_str().unwrap()).unwrap();
    let added = rig
        .call(
            "sales_campaigns_add_target",
            json!({
                "idempotency_key":new_key("legacy-submit-target"),
                "campaign_id":campaign_id,
                "company_id":target.company_id,
                "contact_id":target.contact_id,
                "email_address_id":target.email_address_id
            }),
        )
        .await;
    let target_id = Uuid::parse_str(added["id"].as_str().unwrap()).unwrap();
    let draft = rig
        .call(
            "sales_campaigns_create_message",
            json!({
                "idempotency_key":new_key("legacy-submit-draft"),
                "campaign_id":campaign_id,
                "target_id":target_id,
                "subject":"Legacy submit remains batched",
                "body_text":"This approved campaign draft still waits for the next UTC batch."
            }),
        )
        .await;
    let draft_id = Uuid::parse_str(draft["draft_id"].as_str().unwrap()).unwrap();
    let reviewed = freeze_and_request_review(
        &rig,
        draft_id,
        draft["version"].as_i64().unwrap(),
        campaign_id,
        target_id,
        None,
        "legacy-submit",
    )
    .await;
    let approval = human_data(
        rig.manager_post(
            &format!(
                "/api/v1/approvals/{}/decisions",
                reviewed["approval_request_id"].as_str().unwrap()
            ),
            json!({
                "decision":"approved",
                "expected_snapshot_hash":reviewed["payload_hash"],
                "reason":"Reviewed the exact frozen campaign snapshot"
            }),
            Some(1),
            &new_key("legacy-submit-human-approval"),
        )
        .await,
        StatusCode::OK,
    )
    .await;
    assert_eq!(approval["approval_status"], "approved");

    let outbound_id = Uuid::parse_str(reviewed["outbound_message_id"].as_str().unwrap()).unwrap();
    let submitted = rig
        .call(
            "mail_submit_approved",
            json!({
                "idempotency_key":new_key("legacy-approved-submit"),
                "outbound_message_id":outbound_id,
                "expected_payload_hash":reviewed["payload_hash"]
            }),
        )
        .await;
    assert_eq!(submitted["status"], "queued");

    let mut tx = rig.owner.begin(rig.world.workspace_id).await.unwrap();
    let (not_before, source_credential_id): (Option<chrono::DateTime<Utc>>, Option<Uuid>) =
        sqlx::query_as(
            "SELECT not_before, source_credential_id FROM outbound_messages
          WHERE workspace_id=$1 AND id=$2",
        )
        .bind(rig.world.workspace_id)
        .bind(outbound_id)
        .fetch_one(tx.conn())
        .await
        .unwrap();
    let not_before = not_before.expect("linked campaign outbound must always be scheduled");
    assert_eq!(not_before.timestamp().rem_euclid(600), 0);
    assert!(not_before >= Utc::now());
    assert_eq!(source_credential_id, Some(rig.credential_id));
    let queued_jobs: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM jobs WHERE workspace_id=$1
           AND kind='sending.dispatch_outbound'
           AND payload->>'outbound_message_id'=$2 AND state='queued'",
    )
    .bind(rig.world.workspace_id)
    .bind(outbound_id.to_string())
    .fetch_one(tx.conn())
    .await
    .unwrap();
    assert_eq!(queued_jobs, 1);
    tx.rollback().await.unwrap();

    // The REST campaign queue is also used by session-authenticated humans.
    // It must use the existing send-grant mode while still checking the same
    // independent approval and the same ten-minute schedule.
    let human_target = seed_crm_target(&rig.owner, &rig.world, "human-batch").await;
    let human_target_response = rig
        .call(
            "sales_campaigns_add_target",
            json!({
                "idempotency_key":new_key("human-batch-target"),
                "campaign_id":campaign_id,
                "company_id":human_target.company_id,
                "contact_id":human_target.contact_id,
                "email_address_id":human_target.email_address_id
            }),
        )
        .await;
    let human_target_id = Uuid::parse_str(human_target_response["id"].as_str().unwrap()).unwrap();
    let human_draft = rig
        .call(
            "sales_campaigns_create_message",
            json!({
                "idempotency_key":new_key("human-batch-draft"),
                "campaign_id":campaign_id,
                "target_id":human_target_id,
                "subject":"Human queue still uses a batch",
                "body_text":"The session human queues this independently approved draft: {{meeting.booking_url}}",
                "body_html":"<p><a href=\"{{meeting.booking_url}}\">Schedule the follow-up</a></p>"
            }),
        )
        .await;
    let human_draft_id = Uuid::parse_str(human_draft["draft_id"].as_str().unwrap()).unwrap();
    let human_reviewed = freeze_and_request_review(
        &rig,
        human_draft_id,
        human_draft["version"].as_i64().unwrap(),
        campaign_id,
        human_target_id,
        Some(rig.meeting_type_id),
        "human-batch",
    )
    .await;
    let human_outbound = human_data(
        rig.api
            .get(&format!(
                "/api/v1/outbound-messages/{}",
                human_reviewed["outbound_message_id"].as_str().unwrap()
            ))
            .await,
        StatusCode::OK,
    )
    .await;
    assert!(
        human_outbound["body_text"]
            .as_str()
            .unwrap()
            .contains("http://localhost:3000/public/book/")
    );
    assert!(
        !human_outbound["body_text"]
            .as_str()
            .unwrap()
            .contains("{{meeting.booking_url}}")
    );
    let human_approval = human_data(
        rig.manager_post(
            &format!(
                "/api/v1/approvals/{}/decisions",
                human_reviewed["approval_request_id"].as_str().unwrap()
            ),
            json!({
                "decision":"approved",
                "expected_snapshot_hash":human_reviewed["payload_hash"],
                "reason":"Reviewed this human-queued campaign message"
            }),
            Some(1),
            &new_key("human-batch-approval"),
        )
        .await,
        StatusCode::OK,
    )
    .await;
    assert_eq!(human_approval["approval_status"], "approved");
    let campaign_detail = rig
        .call("sales_campaigns_get", json!({"campaign_id":campaign_id}))
        .await;
    let human_target_version = campaign_detail["targets"]
        .as_array()
        .unwrap()
        .iter()
        .find(|target| target["id"] == human_target_id.to_string())
        .unwrap()["version"]
        .as_i64()
        .unwrap();
    let human_queue = human_data(
        rig.human_post(
            &format!("/api/v1/sales-campaigns/{campaign_id}/targets/{human_target_id}/queue"),
            json!({
                "outbound_message_id":human_reviewed["outbound_message_id"],
                "expected_payload_hash":human_reviewed["payload_hash"],
                "expected_target_version":human_target_version
            }),
            None,
            &new_key("human-batch-queue"),
        )
        .await,
        StatusCode::ACCEPTED,
    )
    .await;
    assert_eq!(human_queue["submission"]["status"], "queued");
    let human_outbound_id =
        Uuid::parse_str(human_reviewed["outbound_message_id"].as_str().unwrap()).unwrap();
    let mut tx = rig.owner.begin(rig.world.workspace_id).await.unwrap();
    let human_not_before: Option<chrono::DateTime<Utc>> = sqlx::query_scalar(
        "SELECT not_before FROM outbound_messages WHERE workspace_id=$1 AND id=$2",
    )
    .bind(rig.world.workspace_id)
    .bind(human_outbound_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    let human_not_before = human_not_before.expect("human-queued campaign outbound is scheduled");
    assert_eq!(human_not_before.timestamp().rem_euclid(600), 0);
    assert!(human_not_before >= Utc::now());
    tx.rollback().await.unwrap();
}

#[tokio::test]
async fn completed_stage_replay_rechecks_same_agent_scope_and_current_mailbox_grant() {
    let rig = Rig::new().await;
    let target = seed_crm_target(&rig.owner, &rig.world, "replay-scope").await;
    let create_args = json!({
        "idempotency_key":"replay-scope-campaign-create-0001",
        "name":"Replay authorization guard",
        "objective":"Check current authorization before a cached mutation response"
    });
    let campaign = rig
        .call("sales_campaigns_create", create_args.clone())
        .await;
    let campaign_id = Uuid::parse_str(campaign["id"].as_str().unwrap()).unwrap();
    let mut tx = rig.owner.begin(rig.world.workspace_id).await.unwrap();
    sqlx::query("UPDATE smtp_connections SET status='disabled' WHERE workspace_id=$1 AND id=$2")
        .bind(rig.world.workspace_id)
        .bind(rig.smtp_profile_id)
        .execute(tx.conn())
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let create_replay = rig.call("sales_campaigns_create", create_args).await;
    assert_eq!(create_replay["id"], campaign_id.to_string());
    let live_campaign = rig
        .call("sales_campaigns_get", json!({"campaign_id":campaign_id}))
        .await;
    assert_eq!(live_campaign["campaign"]["sender_ready"], false);
    let added = rig
        .call(
            "sales_campaigns_add_target",
            json!({
                "idempotency_key":new_key("replay-scope-target"),
                "campaign_id":campaign_id,
                "company_id":target.company_id,
                "contact_id":target.contact_id,
                "email_address_id":target.email_address_id
            }),
        )
        .await;
    let target_id = Uuid::parse_str(added["id"].as_str().unwrap()).unwrap();
    let draft = rig
        .call(
            "sales_campaigns_create_message",
            json!({
                "idempotency_key":new_key("replay-scope-draft"),
                "campaign_id":campaign_id,
                "target_id":target_id,
                "subject":"Replay guard",
                "body_text":"A company-specific draft for authorization replay coverage."
            }),
        )
        .await;
    assert!(draft["draft_id"].is_string());
    let campaign_detail = rig
        .call("sales_campaigns_get", json!({"campaign_id":campaign_id}))
        .await;
    let target_version = campaign_detail["targets"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["id"] == target_id.to_string())
        .unwrap()["version"]
        .as_i64()
        .unwrap();
    let stage_args = json!({
        "idempotency_key":"same-agent-stage-replay-0001",
        "campaign_id":campaign_id,
        "target_id":target_id,
        "stage":"review",
        "expected_target_version":target_version
    });
    let staged = rig
        .call("sales_campaigns_move_stage", stage_args.clone())
        .await;
    assert_eq!(staged["workflow_stage"], "review");
    let broad_replay = rig
        .call("sales_campaigns_move_stage", stage_args.clone())
        .await;
    assert_eq!(broad_replay["id"], staged["id"]);
    assert_eq!(broad_replay["version"], staged["version"]);

    // A distinct, narrower credential authenticating as the same agent has
    // the same principal-scoped idempotency key. It must not receive the
    // broad credential's cached response without current campaign scope.
    let (_, narrow_token) = rig
        .create_service_credential(&["crm.read", "mail.read", "mail.compose"])
        .await;
    let narrow_replay = rig
        .tool_error_with_token(
            &narrow_token,
            "sales_campaigns_move_stage",
            stage_args.clone(),
        )
        .await;
    assert!(narrow_replay["result"]["isError"] == true);

    let (_, no_read_token) = rig
        .create_service_credential(&["campaign.compose", "crm.read", "mail.compose"])
        .await;
    let no_read_mutation = rig
        .tool_error_with_token(
            &no_read_token,
            "sales_campaigns_move_stage",
            json!({
                "idempotency_key":new_key("stage-without-mail-read"),
                "campaign_id":campaign_id,
                "target_id":target_id,
                "stage":"draft",
                "expected_target_version":staged["version"]
            }),
        )
        .await;
    assert_eq!(
        no_read_mutation["result"]["structuredContent"]["error"]["code"],
        "SCOPE_REQUIRED"
    );

    let revoked = human_data(
        rig.admin_update_agent_grant(
            json!({
                "can_read":true,
                "can_compose":false,
                "can_send":false,
                "can_approve":false,
                "can_assign":false
            }),
            rig.agent_grant_version,
        )
        .await,
        StatusCode::OK,
    )
    .await;
    assert_eq!(revoked["can_compose"], false);
    let revoked_grant_replay = rig
        .tool_error("sales_campaigns_move_stage", stage_args)
        .await;
    assert_eq!(
        revoked_grant_replay["result"]["structuredContent"]["error"]["code"],
        "MAILBOX_ACCESS_DENIED"
    );
}

async fn freeze_and_request_review(
    rig: &Rig,
    draft_id: Uuid,
    draft_version: i64,
    campaign_id: Uuid,
    target_id: Uuid,
    meeting_type_id: Option<Uuid>,
    key_prefix: &str,
) -> Value {
    let mut freeze_request = json!({"delivery_channel_id":rig.delivery_channel_id});
    if let Some(meeting_type_id) = meeting_type_id {
        freeze_request["meeting_type_id"] = json!(meeting_type_id);
    }
    let frozen = human_data(
        rig.human_post(
            &format!("/api/v1/message-drafts/{draft_id}/freeze"),
            freeze_request,
            Some(draft_version),
            &new_key(&format!("{key_prefix}-freeze")),
        )
        .await,
        StatusCode::CREATED,
    )
    .await;
    let outbound_id = Uuid::parse_str(frozen["outbound_message_id"].as_str().unwrap()).unwrap();
    let approval = human_data(
        rig.human_post(
            &format!("/api/v1/outbound-messages/{outbound_id}/approval-requests"),
            json!({
                "reason":"Hand this company-specific message to an independent human reviewer",
                "expected_payload_hash":frozen["payload_hash"]
            }),
            None,
            &new_key(&format!("{key_prefix}-approval-request")),
        )
        .await,
        StatusCode::CREATED,
    )
    .await;
    let campaign = rig
        .call("sales_campaigns_get", json!({"campaign_id":campaign_id}))
        .await;
    let target_version = campaign["targets"]
        .as_array()
        .unwrap()
        .iter()
        .find(|target| target["id"] == target_id.to_string())
        .unwrap()["version"]
        .as_i64()
        .unwrap();
    json!({
        "campaign_id":campaign_id,
        "target_id":target_id,
        "target_version":target_version,
        "outbound_message_id":outbound_id,
        "payload_hash":frozen["payload_hash"],
        "approval_request_id":approval["approval_request_id"]
    })
}
