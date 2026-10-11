//! Real MCP research/draft workflow plus human REST review on an isolated DB.
//! Research URLs are inert fixtures; outbound dispatch uses fake providers only.
use api::{AppState, testing::TestApp};
use infrastructure::{
    config::Config,
    db::{Db, ROLE_API},
    testing::{TEST_PASSWORD, TestWorld, seed_workspace},
};
use mcp::server::{McpState, build_router};
use reqwest::StatusCode;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::sync::Arc;
use uuid::Uuid;

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
    client: reqwest::Client,
    run_id: Uuid,
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
            .expect("MCP_TEST_DATABASE_URL must point to the isolated test database");
        assert!(
            db_url.ends_with("/outreach_approved_send_verify"),
            "use only the isolated outreach_approved_send_verify database"
        );
        let owner = Db::connect(&db_url, None).await.unwrap();
        owner.migrate().await.unwrap();
        let world = seed_workspace(&owner).await.unwrap();
        let db = Db::connect(&db_url, Some(ROLE_API))
            .await
            .unwrap()
            .with_default_workspace(Some(world.workspace_id));
        // Workspaces start in fail-closed emergency stop. This fixture
        // enables the test-only queue path; dispatch itself is not run here.
        let mut safety_tx = db.begin(world.workspace_id).await.unwrap();
        sqlx::query("UPDATE workspaces SET emergency_stop = false WHERE id = $1")
            .bind(world.workspace_id)
            .execute(safety_tx.conn())
            .await
            .unwrap();
        safety_tx.commit().await.unwrap();

        let mut tx = owner.begin(world.workspace_id).await.unwrap();
        let scopes: Vec<String> = [
            "crm.read",
            "crm.write",
            "research.propose",
            "research.write",
            "templates.read",
            "mail.read",
            "mail.compose",
            "mail.submit_approved",
            "operations.read",
        ]
        .into_iter()
        .map(String::from)
        .collect();
        let (credential_id, token) = infrastructure::repo::auth::create_service_credential(
            &mut tx,
            world.agent.principal_id,
            "isolated approved-send verification",
            &scopes,
            chrono::Duration::hours(1),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();

        let api = TestApp::spawn_with_state(AppState::with_fake_providers(
            db.clone(),
            Arc::new(Config::test()),
            None,
        ))
        .await;
        let login = api
            .login(world.admin.email.as_deref().unwrap(), TEST_PASSWORD)
            .await;
        assert_eq!(login.status(), StatusCode::OK);
        let csrf = TestApp::csrf_token_from(&login).unwrap();

        let manager_api = TestApp::spawn_with_state(api.state.clone()).await;
        let manager_login = manager_api
            .login(world.manager.email.as_deref().unwrap(), TEST_PASSWORD)
            .await;
        assert_eq!(manager_login.status(), StatusCode::OK);
        let manager_csrf = TestApp::csrf_token_from(&manager_login).unwrap();

        let app = build_router(McpState {
            db: db.clone(),
            calendar: Arc::new(infrastructure::providers::fake::FakeMailProvider::new()),
            config: Arc::new(infrastructure::config::Config::test()),
            rate_limiter: None,
            allowed_origins: Arc::new(vec![]),
            tools: Arc::new(mcp::all_tools_with_key(
                b"isolated-mcp-sales-workflow-hmac-key-32bytes",
            )),
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/mcp", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
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
            client: reqwest::Client::new(),
            run_id: Uuid::nil(),
            server,
        };

        let initial_grant = human_data(
            rig.admin_put_agent_grant(
                json!({
                    "can_read":true,
                    "can_compose":true,
                    "can_send":false,
                    "can_approve":false,
                    "can_assign":false
                }),
                None,
            )
            .await,
            StatusCode::OK,
        )
        .await;
        assert_eq!(initial_grant["can_send"], false);
        rig.agent_grant_version = initial_grant["version"].as_i64().unwrap();

        let init = rig
            .rpc(
                "initialize",
                json!({
                    "protocolVersion":"2025-11-25",
                    "capabilities":{},
                    "clientInfo":{"name":"office-agent-sales-workflow-test","version":"1"}
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
                    "objective":"Research two companies and prepare reviewed Specify sales drafts",
                    "budget":{"max_tool_calls":60,"max_proposals":5}
                }),
                None,
                "sales-workflow-run-create-001",
            )
            .await,
            StatusCode::CREATED,
        )
        .await;
        rig.run_id = Uuid::parse_str(run["id"].as_str().unwrap()).unwrap();
        rig
    }

    async fn rpc(&self, method: &str, params: Value) -> Value {
        let mut request = self
            .client
            .post(&self.url)
            .bearer_auth(&self.token)
            .header("MCP-Protocol-Version", "2025-11-25")
            .json(&json!({
                "jsonrpc":"2.0",
                "id":Uuid::new_v4().to_string(),
                "method":method,
                "params":params
            }));
        if !self.run_id.is_nil() {
            request = request.header("X-Agent-Run-Id", self.run_id.to_string());
        }
        let response = request.send().await.unwrap();
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

    async fn human_post(
        &self,
        path: &str,
        body: Value,
        version: Option<i64>,
        key: &str,
    ) -> reqwest::Response {
        post_as_human(&self.api, &self.csrf, path, body, version, key).await
    }

    async fn admin_put_agent_grant(&self, body: Value, version: Option<i64>) -> reqwest::Response {
        let path = format!(
            "/api/v1/mail-accounts/{}/grants/{}",
            self.world.mail_account_id, self.world.agent.principal_id
        );
        let mut request = self
            .api
            .client
            .put(self.api.url(&path))
            .header("x-csrf-token", &self.csrf)
            .json(&body);
        request = if let Some(version) = version {
            request.header("if-match", format!("\"{version}\""))
        } else {
            request.header("if-none-match", "*")
        };
        request.send().await.unwrap()
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

    async fn agent_post(
        &self,
        path: &str,
        body: Value,
        version: Option<i64>,
        key: &str,
    ) -> reqwest::Response {
        let mut request = self
            .client
            .post(self.api.url(path))
            .bearer_auth(&self.token)
            .header("idempotency-key", key)
            .json(&body);
        if let Some(version) = version {
            request = request.header("if-match", format!("\"{version}\""));
        }
        request.send().await.unwrap()
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

async fn fake_delivery_channel(rig: &Rig) -> Uuid {
    let mut tx = rig.owner.begin(rig.world.workspace_id).await.unwrap();
    let connection_id: Uuid = sqlx::query_scalar(
        "INSERT INTO integration_connections
            (workspace_id, provider, region, secret_ref, status, connected_by)
         VALUES ($1, 'delivery_provider', 'us', $2, 'active', $3)
         RETURNING id",
    )
    .bind(rig.world.workspace_id)
    .bind(format!("sales-workflow-fake:{}", Uuid::new_v4()))
    .bind(rig.world.admin.principal_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    let sender_id: Uuid = sqlx::query_scalar(
        "INSERT INTO sender_identities
            (workspace_id, mail_account_id, address, display_name, status, verified_at)
         VALUES ($1, $2, $3, 'Specify Sales Test', 'verified', now())
         RETURNING id",
    )
    .bind(rig.world.workspace_id)
    .bind(rig.world.mail_account_id)
    .bind(format!("specify-test-{}@test.dev", Uuid::new_v4()))
    .fetch_one(tx.conn())
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let channel = human_data(
        rig.human_post(
            "/api/v1/delivery-channels",
            json!({
                "connection_id":connection_id,
                "mail_account_id":rig.world.mail_account_id,
                "sender_identity_id":sender_id,
                "purpose":"marketing",
                "name":"Isolated fake SMTP sales channel"
            }),
            None,
            "sales-workflow-channel-create-001",
        )
        .await,
        StatusCode::CREATED,
    )
    .await;
    let channel_id = Uuid::parse_str(channel["id"].as_str().unwrap()).unwrap();
    let review = human_data(
        rig.human_post(
            &format!("/api/v1/delivery-channels/{channel_id}/reviews"),
            json!({
                "result":"allowed",
                "contract_evidence_ref":"isolated-fake-provider-fixture",
                "notes":"No external delivery is performed by this MCP workflow test"
            }),
            None,
            "sales-workflow-channel-review-001",
        )
        .await,
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(review["result"], "allowed");

    let channels = human_data(
        rig.api.get("/api/v1/delivery-channels?limit=20").await,
        StatusCode::OK,
    )
    .await;
    let current = channels
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["id"] == channel_id.to_string())
        .unwrap();
    let version = current["version"].as_i64().unwrap();
    let enabled = rig
        .api
        .client
        .patch(
            rig.api
                .url(&format!("/api/v1/delivery-channels/{channel_id}")),
        )
        .header("x-csrf-token", &rig.csrf)
        .header("if-match", format!("\"{version}\""))
        .json(&json!({"enabled":true}))
        .send()
        .await
        .unwrap();
    let enabled = human_data(enabled, StatusCode::OK).await;
    assert_eq!(enabled["enabled"], true);
    assert_eq!(enabled["policy_status"], "allowed");
    channel_id
}

async fn add_and_verify_evidence(
    rig: &Rig,
    company_id: Uuid,
    slug: &str,
    kind: &str,
    fact: &str,
) -> String {
    let evidence = rig
        .call(
            "research_add_evidence",
            json!({
                "agent_run_id":rig.run_id,
                "company_id":company_id,
                "field_path":"sales_signal",
                "source_url":format!("https://public.example/{slug}/{kind}"),
                "observed_at":chrono::Utc::now().to_rfc3339(),
                "value":fact,
                "evidence_type":"observed",
                "excerpt":fact,
                "idempotency_key":format!("sales-evidence-{slug}-{kind}-{}", Uuid::new_v4())
            }),
        )
        .await;
    assert_eq!(evidence["review_status"], "pending");
    let evidence_id = Uuid::parse_str(evidence["evidence_id"].as_str().unwrap()).unwrap();
    let reviewed = human_data(
        rig.human_post(
            &format!("/api/v1/research-evidence/{evidence_id}/reviews"),
            json!({"decision":"verified","reason":"Source reviewed for this test fixture"}),
            None,
            &format!("sales-evidence-review-{evidence_id}"),
        )
        .await,
        StatusCode::OK,
    )
    .await;
    assert_eq!(reviewed["review_status"], "verified");
    format!("https://public.example/{slug}/{kind}")
}

#[tokio::test]
async fn sourced_two_company_context_becomes_reviewed_immutable_approved_sales_draft() {
    let mut rig = Rig::new().await;
    let suffix = Uuid::new_v4().simple().to_string();
    let aurora_name = format!("Aurora Storage {suffix}");
    let forge_name = format!("Forge Robotics {suffix}");
    let aurora_fact = "Aurora is opening a 20 MW grid-storage pilot in Incheon";
    let forge_fact = "Forge is adding robotic picking to its Busan distribution center";

    let listed = rig.rpc("tools/list", json!({})).await;
    let tool_names: Vec<&str> = listed["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    assert!(!tool_names.contains(&"research_propose_leads"));
    assert!(tool_names.contains(&"crm_create_company"));
    assert!(tool_names.contains(&"crm_create_contact"));
    assert!(tool_names.contains(&"crm_get_company"));
    assert!(tool_names.contains(&"mail_create_draft"));
    assert!(tool_names.contains(&"mail_submit_approved"));
    // Operator-parity tools are listed; credential scopes gate their use.
    assert!(tool_names.contains(&"mail_submit"));
    assert!(tool_names.contains(&"approvals_decide"));
    assert!(tool_names.contains(&"mail_get_outbound"));
    assert!(tool_names.contains(&"mail_get_thread"));

    // Agents create sourced companies and contacts directly in the CRM.
    let mut created_companies = Vec::new();
    for (slug, name, industry, contact_name, job_title, department, local) in [
        (
            "aurora",
            &aurora_name,
            "Energy storage",
            "Ara Kim",
            "Sales Director",
            "Sales",
            "ara",
        ),
        (
            "forge",
            &forge_name,
            "Industrial automation",
            "Min Lee",
            "Commercial Lead",
            "Partnerships",
            "min",
        ),
    ] {
        assert!(
            rig.call("crm_search_companies", json!({"query":name}))
                .await["companies"]
                .as_array()
                .unwrap()
                .is_empty(),
            "search before create finds no duplicate"
        );
        let company_args = json!({
            "idempotency_key":format!("sales-company-{slug}-{suffix}"),
            "name":name,
            "domain":format!("{slug}-{suffix}.example"),
            "industry":industry,
            "country_code":"KR",
            "website_url":format!("https://{slug}-{suffix}.example")
        });
        let company = rig.call("crm_create_company", company_args.clone()).await;
        let replay = rig.call("crm_create_company", company_args).await;
        assert_eq!(company["id"], replay["id"]);
        let company_id = Uuid::parse_str(company["id"].as_str().unwrap()).unwrap();
        rig.call(
            "crm_create_contact",
            json!({
                "idempotency_key":format!("sales-contact-{slug}-{suffix}"),
                "name":contact_name,
                "company_id":company_id,
                "job_title":job_title,
                "department":department,
                "email":format!("{local}-{suffix}@{slug}.example.test")
            }),
        )
        .await;
        created_companies.push(company_id);
    }
    let (aurora_company, forge_company) = (created_companies[0], created_companies[1]);
    assert_ne!(aurora_company, forge_company);

    let aurora_news = add_and_verify_evidence(
        &rig,
        aurora_company,
        &format!("aurora-{suffix}"),
        "news",
        aurora_fact,
    )
    .await;
    let aurora_interview = add_and_verify_evidence(
        &rig,
        aurora_company,
        &format!("aurora-{suffix}"),
        "interview",
        "Aurora's finance lead prioritizes forecasting storage dispatch revenue",
    )
    .await;
    let forge_news = add_and_verify_evidence(
        &rig,
        forge_company,
        &format!("forge-{suffix}"),
        "news",
        forge_fact,
    )
    .await;
    let forge_interview = add_and_verify_evidence(
        &rig,
        forge_company,
        &format!("forge-{suffix}"),
        "interview",
        "Forge's operations lead is focused on reducing pick-and-pack exceptions",
    )
    .await;

    let aurora_context = rig
        .call(
            "crm_get_company",
            json!({"company_id":aurora_company,"requested_sections":["research"]}),
        )
        .await;
    let forge_context = rig
        .call(
            "crm_get_company",
            json!({"company_id":forge_company,"requested_sections":["research"]}),
        )
        .await;
    assert_eq!(aurora_context["company"]["industry"], "Energy storage");
    assert_eq!(
        forge_context["company"]["industry"],
        "Industrial automation"
    );
    let aurora_items = aurora_context["sections"]["research"]["items"]
        .as_array()
        .unwrap();
    let forge_items = forge_context["sections"]["research"]["items"]
        .as_array()
        .unwrap();
    for source in [&aurora_news, &aurora_interview] {
        assert!(
            aurora_items
                .iter()
                .any(|item| item["source_url"] == *source)
        );
    }
    for source in [&forge_news, &forge_interview] {
        assert!(forge_items.iter().any(|item| item["source_url"] == *source));
    }
    assert!(
        aurora_items
            .iter()
            .all(|item| item["review_status"] == "verified")
    );
    assert!(
        forge_items
            .iter()
            .all(|item| item["review_status"] == "verified")
    );
    assert!(
        forge_items
            .iter()
            .all(|item| item["source_url"] != aurora_news && item["source_url"] != aurora_interview)
    );
    assert!(
        aurora_items
            .iter()
            .all(|item| item["source_url"] != forge_news && item["source_url"] != forge_interview)
    );

    let aurora_contact = rig
        .call(
            "crm_search_contacts",
            json!({"company_id":aurora_company,"limit":10}),
        )
        .await;
    let forge_contact = rig
        .call(
            "crm_search_contacts",
            json!({"company_id":forge_company,"limit":10}),
        )
        .await;
    assert_eq!(aurora_contact["contacts"].as_array().unwrap().len(), 1);
    assert_eq!(forge_contact["contacts"].as_array().unwrap().len(), 1);
    let aurora_email_id = aurora_contact["contacts"][0]["emails"][0]["email_address_id"].clone();
    let forge_email_id = forge_contact["contacts"][0]["emails"][0]["email_address_id"].clone();

    let basis = human_data(
        rig.human_post(
            "/api/v1/communication-bases",
            json!({
                "email_address_id":aurora_email_id,
                "purpose":"marketing",
                "jurisdiction":"KR",
                "basis_type":"explicit_consent",
                "evidence_ref":"isolated-test-consent-fixture",
                "collected_at":chrono::Utc::now().to_rfc3339()
            }),
            None,
            "sales-workflow-basis-create-001",
        )
        .await,
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(basis["status"], "pending");
    let basis_id = Uuid::parse_str(basis["id"].as_str().unwrap()).unwrap();
    let reviewed_basis = human_data(
        rig.human_post(
            &format!("/api/v1/communication-bases/{basis_id}/reviews"),
            json!({"decision":"valid"}),
            Some(basis["version"].as_i64().unwrap()),
            "sales-workflow-basis-review-001",
        )
        .await,
        StatusCode::OK,
    )
    .await;
    assert_eq!(reviewed_basis["status"], "valid");

    let channel_id = fake_delivery_channel(&rig).await;
    let aurora_fact = aurora_fact.to_string();
    let forge_fact = forge_fact.to_string();
    let aurora_subject = format!("Specify for {aurora_name}");
    let aurora_body = format!(
        "Hi Ara, I saw that {aurora_fact}. Specify helps sales teams connect this company research with focused follow-up. Would a short conversation be useful?"
    );
    let forge_subject = format!("Specify for {forge_name}");
    let forge_body = format!(
        "Hi Min, I saw that {forge_fact}. Specify helps sales teams connect this company research with focused follow-up. Would a short conversation be useful?"
    );
    let aurora_draft = rig
        .call(
            "mail_create_draft",
            json!({
                "idempotency_key":format!("sales-aurora-draft-{suffix}"),
                "mail_account_id":rig.world.mail_account_id,
                "to_email_address_id":aurora_email_id,
                "purpose":"marketing",
                "subject":aurora_subject,
                "body_text":aurora_body,
                "visibility":"mailbox"
            }),
        )
        .await;
    let aurora_draft_replay = rig
        .call(
            "mail_create_draft",
            json!({
                "idempotency_key":format!("sales-aurora-draft-{suffix}"),
                "mail_account_id":rig.world.mail_account_id,
                "to_email_address_id":aurora_email_id,
                "purpose":"marketing",
                "subject":aurora_subject,
                "body_text":aurora_body,
                "visibility":"mailbox"
            }),
        )
        .await;
    let forge_draft = rig
        .call(
            "mail_create_draft",
            json!({
                "idempotency_key":format!("sales-forge-draft-{suffix}"),
                "mail_account_id":rig.world.mail_account_id,
                "to_email_address_id":forge_email_id,
                "purpose":"marketing",
                "subject":forge_subject,
                "body_text":forge_body,
                "visibility":"mailbox"
            }),
        )
        .await;
    assert_eq!(aurora_draft["approval_required"], true);
    assert_eq!(aurora_draft["status"], "draft");
    assert_eq!(aurora_draft["draft_id"], aurora_draft_replay["draft_id"]);
    assert_ne!(aurora_draft["draft_id"], forge_draft["draft_id"]);
    assert_eq!(forge_draft["approval_required"], true);
    assert_eq!(forge_draft["status"], "draft");

    let aurora_draft_id = Uuid::parse_str(aurora_draft["draft_id"].as_str().unwrap()).unwrap();
    let forge_draft_id = Uuid::parse_str(forge_draft["draft_id"].as_str().unwrap()).unwrap();
    let aurora_review_copy = human_data(
        rig.api
            .get(&format!("/api/v1/message-drafts/{aurora_draft_id}"))
            .await,
        StatusCode::OK,
    )
    .await;
    let forge_review_copy = human_data(
        rig.api
            .get(&format!("/api/v1/message-drafts/{forge_draft_id}"))
            .await,
        StatusCode::OK,
    )
    .await;
    assert_eq!(aurora_review_copy["to_email_address_id"], aurora_email_id);
    assert_eq!(forge_review_copy["to_email_address_id"], forge_email_id);
    assert_eq!(aurora_review_copy["subject"], aurora_subject);
    assert_eq!(forge_review_copy["subject"], forge_subject);
    assert!(
        aurora_review_copy["body_text"]
            .as_str()
            .unwrap()
            .contains(&aurora_fact)
    );
    assert!(
        !aurora_review_copy["body_text"]
            .as_str()
            .unwrap()
            .contains(&forge_fact)
    );
    assert!(
        forge_review_copy["body_text"]
            .as_str()
            .unwrap()
            .contains(&forge_fact)
    );
    assert!(
        !forge_review_copy["body_text"]
            .as_str()
            .unwrap()
            .contains(&aurora_fact)
    );

    let frozen = human_data(
        rig.human_post(
            &format!("/api/v1/message-drafts/{aurora_draft_id}/freeze"),
            json!({"delivery_channel_id":channel_id}),
            Some(aurora_draft["version"].as_i64().unwrap()),
            "sales-workflow-freeze-aurora-001",
        )
        .await,
        StatusCode::CREATED,
    )
    .await;
    let outbound_id = Uuid::parse_str(frozen["outbound_message_id"].as_str().unwrap()).unwrap();
    let payload_hash = frozen["payload_hash"].as_str().unwrap().to_string();

    let mut provenance_tx = rig.owner.begin(rig.world.workspace_id).await.unwrap();
    let (outbound_requester, source_credential, local_thread, internet_message_id): (
        Uuid,
        Option<Uuid>,
        Option<Uuid>,
        Option<String>,
    ) = sqlx::query_as(
        "SELECT requested_by, source_credential_id, thread_id, internet_message_id
           FROM outbound_messages WHERE workspace_id=$1 AND id=$2",
    )
    .bind(rig.world.workspace_id)
    .bind(outbound_id)
    .fetch_one(provenance_tx.conn())
    .await
    .unwrap();
    assert_eq!(outbound_requester, rig.world.agent.principal_id);
    assert_eq!(source_credential, Some(rig.credential_id));
    assert!(local_thread.is_some());
    assert!(
        internet_message_id
            .as_deref()
            .is_some_and(|message_id| message_id.starts_with('<') && message_id.ends_with('>'))
    );
    provenance_tx.rollback().await.unwrap();

    let outbound_before_approval = human_data(
        rig.api
            .get(&format!("/api/v1/outbound-messages/{outbound_id}"))
            .await,
        StatusCode::OK,
    )
    .await;
    assert_eq!(outbound_before_approval["status"], "draft");
    assert_eq!(
        outbound_before_approval["to_address"],
        format!("ara-{suffix}@aurora.example.test")
    );
    assert_eq!(outbound_before_approval["subject"], aurora_subject);
    assert!(
        outbound_before_approval["body_text"]
            .as_str()
            .unwrap()
            .contains(&aurora_fact)
    );
    assert!(
        outbound_before_approval["body_text"]
            .as_str()
            .unwrap()
            .contains("/public/unsubscribe/")
    );
    assert!(
        !outbound_before_approval["body_text"]
            .as_str()
            .unwrap()
            .contains(&forge_fact)
    );

    let approval = human_data(
        rig.human_post(
            &format!("/api/v1/outbound-messages/{outbound_id}/approval-requests"),
            json!({
                "reason":"Review the immutable company-specific sales message",
                "expected_payload_hash":payload_hash
            }),
            None,
            "sales-workflow-approval-request-001",
        )
        .await,
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(approval["status"], "pending");
    let approval_id = Uuid::parse_str(approval["approval_request_id"].as_str().unwrap()).unwrap();
    let review_snapshot = human_data(
        rig.manager_api
            .get(&format!("/api/v1/approvals/{approval_id}"))
            .await,
        StatusCode::OK,
    )
    .await;
    assert_eq!(review_snapshot["target"]["status"], "draft");
    assert_eq!(review_snapshot["manifest_hash"], payload_hash);
    assert_eq!(
        review_snapshot["snapshot"]["messages"][0]["to_address_snapshot"],
        format!("ara-{suffix}@aurora.example.test")
    );
    assert_eq!(
        review_snapshot["snapshot"]["messages"][0]["subject"],
        aurora_subject
    );
    assert!(
        review_snapshot["snapshot"]["messages"][0]["body_text"]
            .as_str()
            .unwrap()
            .contains(&aurora_fact)
    );
    assert!(
        !review_snapshot["snapshot"]["messages"][0]["body_text"]
            .as_str()
            .unwrap()
            .contains(&forge_fact)
    );

    let mutation = rig
        .api
        .client
        .patch(
            rig.api
                .url(&format!("/api/v1/message-drafts/{aurora_draft_id}")),
        )
        .header("x-csrf-token", &rig.csrf)
        .header("if-match", "\"1\"")
        .json(&json!({"subject":"Changed after review"}))
        .send()
        .await
        .unwrap();
    let mutation = problem(mutation, StatusCode::CONFLICT).await;
    assert!(mutation["detail"].as_str().unwrap().contains("frozen"));

    let preapproval = rig
        .human_post(
            &format!("/api/v1/outbound-messages/{outbound_id}/submit"),
            json!({"expected_payload_hash":payload_hash}),
            None,
            "sales-workflow-human-preapproval-submit",
        )
        .await;
    let preapproval = problem(preapproval, StatusCode::CONFLICT).await;
    assert_eq!(preapproval["code"], "CONFLICT");

    let agent_preapproval = rig
        .agent_post(
            &format!("/api/v1/outbound-messages/{outbound_id}/submit"),
            json!({"expected_payload_hash":payload_hash}),
            None,
            "sales-workflow-agent-preapproval-submit",
        )
        .await;
    // Agents may submit directly only with mail.send plus a can_send grant;
    // this credential holds neither.
    let agent_preapproval = problem(agent_preapproval, StatusCode::FORBIDDEN).await;
    assert_eq!(agent_preapproval["code"], "MAILBOX_ACCESS_DENIED");

    let mcp_submit = rig
        .tool_error(
            "mail_submit",
            json!({
                "outbound_message_id":outbound_id,
                "expected_payload_hash":payload_hash,
                "idempotency_key":"sales-workflow-mcp-direct-submit"
            }),
        )
        .await;
    assert_eq!(
        mcp_submit["result"]["structuredContent"]["error"]["code"], "MAILBOX_ACCESS_DENIED",
        "{mcp_submit}"
    );
    let preapproval_send_grant = human_data(
        rig.admin_put_agent_grant(
            json!({
                "can_read":true,
                "can_compose":true,
                "can_send":true,
                "can_approve":false,
                "can_assign":false
            }),
            Some(rig.agent_grant_version),
        )
        .await,
        StatusCode::OK,
    )
    .await;
    rig.agent_grant_version = preapproval_send_grant["version"].as_i64().unwrap();
    let approved_submit_before_approval = rig
        .tool_error(
            "mail_submit_approved",
            json!({
                "outbound_message_id": outbound_id,
                "expected_payload_hash": payload_hash,
                "idempotency_key": "approved-send-before-approval"
            }),
        )
        .await;
    assert_eq!(
        approved_submit_before_approval["result"]["structuredContent"]["error"]["code"],
        "CONFLICT"
    );
    let mcp_decide = rig
        .tool_error(
            "approvals_decide",
            json!({
                "idempotency_key":"sales-workflow-mcp-self-decide",
                "approval_request_id":approval_id,
                "expected_version":1,
                "decision":"approved",
                "expected_snapshot_hash":payload_hash
            }),
        )
        .await;
    assert_eq!(
        mcp_decide["result"]["structuredContent"]["error"]["code"], "SCOPE_REQUIRED",
        "{mcp_decide}"
    );

    let decision_body = json!({
        "decision":"approved",
        "expected_snapshot_hash":payload_hash,
        "reason":"Reviewed the exact frozen sales message"
    });
    let decision_path = format!("/api/v1/approvals/{approval_id}/decisions");
    let decision_key = "sales-workflow-human-approval-decision-001";
    let decision = human_data(
        rig.manager_post(&decision_path, decision_body.clone(), Some(1), decision_key)
            .await,
        StatusCode::OK,
    )
    .await;
    assert_eq!(decision["approval_status"], "approved");
    assert_eq!(decision["execution_status"], "ready");
    assert_eq!(decision["provider_accepted_count"], 0);
    assert!(decision["job_id"].is_null());

    let replay = human_data(
        rig.manager_post(&decision_path, decision_body, Some(1), decision_key)
            .await,
        StatusCode::OK,
    )
    .await;
    assert_eq!(replay["decision_id"], decision["decision_id"]);
    assert!(replay["job_id"].is_null());
    assert_eq!(replay["provider_accepted_count"], 0);

    let revoked_send_grant = human_data(
        rig.admin_put_agent_grant(
            json!({
                "can_read":true,
                "can_compose":true,
                "can_send":false,
                "can_approve":false,
                "can_assign":false
            }),
            Some(rig.agent_grant_version),
        )
        .await,
        StatusCode::OK,
    )
    .await;
    rig.agent_grant_version = revoked_send_grant["version"].as_i64().unwrap();
    let denied_without_current_send_grant = rig
        .tool_error(
            "mail_submit_approved",
            json!({
                "outbound_message_id": outbound_id,
                "expected_payload_hash": payload_hash,
                "idempotency_key": "approved-send-without-grant"
            }),
        )
        .await;
    assert_eq!(
        denied_without_current_send_grant["result"]["structuredContent"]["error"]["code"],
        "MAILBOX_ACCESS_DENIED"
    );
    let send_grant = human_data(
        rig.admin_put_agent_grant(
            json!({
                "can_read":true,
                "can_compose":true,
                "can_send":true,
                "can_approve":false,
                "can_assign":false
            }),
            Some(rig.agent_grant_version),
        )
        .await,
        StatusCode::OK,
    )
    .await;
    assert_eq!(send_grant["can_send"], true);
    assert_eq!(send_grant["can_approve"], false);
    rig.agent_grant_version = send_grant["version"].as_i64().unwrap();

    let effective_mailbox = rig
        .client
        .get(rig.api.url(&format!(
            "/api/v1/mail-accounts/{}",
            rig.world.mail_account_id
        )))
        .bearer_auth(&rig.token)
        .send()
        .await
        .unwrap();
    let effective_mailbox = human_data(effective_mailbox, StatusCode::OK).await;
    let effective_actions = effective_mailbox["allowed_actions"].as_array().unwrap();
    assert!(effective_actions.contains(&json!("mail.read")));
    assert!(effective_actions.contains(&json!("mail.compose")));
    // allowed_actions mirrors the mailbox grant for agents as for humans
    // (can_send=true, can_approve=false); the credential scope is enforced
    // separately when the action is attempted.
    assert!(effective_actions.contains(&json!("mail.send")));
    assert!(!effective_actions.contains(&json!("mail.approve")));

    let agent_me = rig
        .client
        .get(rig.api.url("/api/v1/me"))
        .bearer_auth(&rig.token)
        .send()
        .await
        .unwrap();
    let agent_me = human_data(agent_me, StatusCode::OK).await;
    let me_mailbox = agent_me["mail_accounts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|mailbox| mailbox["id"] == rig.world.mail_account_id.to_string())
        .unwrap();
    let me_actions = me_mailbox["allowed_actions"].as_array().unwrap();
    assert!(me_actions.contains(&json!("mail.send")));
    assert!(!me_actions.contains(&json!("mail.approve")));

    let direct_send_after_grant = rig
        .agent_post(
            &format!("/api/v1/outbound-messages/{outbound_id}/submit"),
            json!({"expected_payload_hash":payload_hash}),
            None,
            "sales-workflow-agent-direct-after-grant",
        )
        .await;
    // The can_send grant alone is not enough: the credential lacks mail.send.
    let direct_send_after_grant = problem(direct_send_after_grant, StatusCode::FORBIDDEN).await;
    assert_eq!(direct_send_after_grant["code"], "MAILBOX_ACCESS_DENIED");

    let approved = human_data(
        rig.api
            .get(&format!("/api/v1/outbound-messages/{outbound_id}"))
            .await,
        StatusCode::OK,
    )
    .await;
    assert_eq!(approved["status"], "draft");
    assert_eq!(approved["approval_status"], "approved");
    assert_eq!(approved["approval_valid"], true);
    assert_eq!(approved["approval_snapshot_matches"], true);
    assert!(approved["thread_id"].is_string());
    assert_eq!(approved["provider_accepted"], Value::Null);
    assert_eq!(approved["delivery_status"], "unknown");

    let submit_args = json!({
        "outbound_message_id": outbound_id,
        "expected_payload_hash": payload_hash,
        "idempotency_key": "approved-send-workflow-001"
    });
    let submitted = rig.call("mail_submit_approved", submit_args.clone()).await;
    assert_eq!(submitted["status"], "queued");
    assert_eq!(submitted["provider_accepted"], false);
    let replayed_submit = rig.call("mail_submit_approved", submit_args).await;
    assert_eq!(replayed_submit["job_id"], submitted["job_id"]);
    assert_eq!(replayed_submit["status"], "queued");

    let status = rig
        .call(
            "mail_get_outbound",
            json!({"outbound_message_id": outbound_id}),
        )
        .await;
    assert_eq!(status["status"], "queued");
    assert_eq!(status["delivery_status"], "unknown");
    assert_eq!(status["approval_status"], "approved");
    assert_eq!(status["approval_valid"], true);
    assert_eq!(status["thread_id"], approved["thread_id"]);
    assert_eq!(status["engagement"]["tracking_enabled"], false);
    assert_eq!(status["engagement"]["open_request_count"], 0);
    assert_eq!(status["engagement"]["click_request_count"], 0);
    assert_eq!(status["known_replies"], json!([]));

    let mut tx = rig.owner.begin(rig.world.workspace_id).await.unwrap();
    let queued_jobs: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM jobs WHERE workspace_id=$1 AND kind='sending.dispatch_outbound' AND payload->>'outbound_message_id'=$2 AND state='queued'",
    )
    .bind(rig.world.workspace_id)
    .bind(outbound_id.to_string())
    .fetch_one(tx.conn())
    .await
    .unwrap();
    assert_eq!(queued_jobs, 1, "idempotent submit queues exactly once");

    let original_scopes: Vec<String> = sqlx::query_scalar(
        "SELECT scopes FROM service_credentials WHERE workspace_id=$1 AND id=$2",
    )
    .bind(rig.world.workspace_id)
    .bind(rig.credential_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    sqlx::query("UPDATE service_credentials SET scopes=array_remove(scopes,'mail.read') WHERE workspace_id=$1 AND id=$2")
        .bind(rig.world.workspace_id)
        .bind(rig.credential_id)
        .execute(tx.conn())
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let hidden_status = rig
        .tool_error(
            "mail_get_outbound",
            json!({"outbound_message_id": outbound_id}),
        )
        .await;
    assert_eq!(
        hidden_status["result"]["structuredContent"]["error"]["code"],
        "MAILBOX_ACCESS_DENIED"
    );

    let mut tx = rig.owner.begin(rig.world.workspace_id).await.unwrap();
    sqlx::query("UPDATE service_credentials SET scopes=$3 WHERE workspace_id=$1 AND id=$2")
        .bind(rig.world.workspace_id)
        .bind(rig.credential_id)
        .bind(original_scopes)
        .execute(tx.conn())
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let remaining_draft = human_data(
        rig.api
            .get(&format!("/api/v1/message-drafts/{forge_draft_id}"))
            .await,
        StatusCode::OK,
    )
    .await;
    assert_eq!(remaining_draft["status"], "draft");
    assert!(remaining_draft["outbound_message_id"].is_null());

    // The job endpoint is catalogued for polling, but approved-submit
    // dispatch jobs have no principal owner, so this agent cannot read one.
    let job_poll = rig
        .tool_error("operations_get_job", json!({"job_id": submitted["job_id"]}))
        .await;
    assert_eq!(job_poll["result"]["isError"], true);
    assert_eq!(
        job_poll["result"]["structuredContent"]["error"]["code"],
        "NOT_FOUND"
    );
}

#[tokio::test]
async fn agent_campaign_approval_then_explicit_mcp_execution_sends_one_tracked_threaded_message() {
    let mut rig = Rig::new().await;
    let db = rig.api.state.db.clone();
    let ws = rig.world.workspace_id;
    let channel_id = fake_delivery_channel(&rig).await;
    let public_base_url = rig.api.url("");

    let mut setup = rig.owner.begin(ws).await.unwrap();
    let mut scopes: Vec<String> = sqlx::query_scalar(
        "SELECT scopes FROM service_credentials WHERE workspace_id=$1 AND id=$2",
    )
    .bind(ws)
    .bind(rig.credential_id)
    .fetch_one(setup.conn())
    .await
    .unwrap();
    for scope in ["campaign.compose", "campaign.submit_approved"] {
        if !scopes.iter().any(|existing| existing == scope) {
            scopes.push(scope.to_string());
        }
    }
    sqlx::query("UPDATE service_credentials SET scopes=$3 WHERE workspace_id=$1 AND id=$2")
        .bind(ws)
        .bind(rig.credential_id)
        .bind(scopes)
        .execute(setup.conn())
        .await
        .unwrap();
    sqlx::query(
        "UPDATE workspaces
            SET settings = settings || jsonb_build_object(
                'tracking_enabled', true,
                'public_base_url', $2::text
            )
          WHERE id=$1",
    )
    .bind(ws)
    .bind(public_base_url)
    .execute(setup.conn())
    .await
    .unwrap();

    let recipient_address = format!("campaign-agent-{}@example.test", Uuid::new_v4());
    let recipient_id: Uuid = sqlx::query_scalar(
        "INSERT INTO email_addresses
            (workspace_id, address_original, normalized_key, kind,
             verification_status, verified_at)
         VALUES ($1, $2, $2, 'person', 'verified', now()) RETURNING id",
    )
    .bind(ws)
    .bind(&recipient_address)
    .fetch_one(setup.conn())
    .await
    .unwrap();
    let contact_id: Uuid = sqlx::query_scalar(
        "INSERT INTO contacts (workspace_id, name)
         VALUES ($1, 'Campaign execution recipient') RETURNING id",
    )
    .bind(ws)
    .fetch_one(setup.conn())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO contact_emails
            (workspace_id, contact_id, email_address_id, is_primary)
         VALUES ($1, $2, $3, true)",
    )
    .bind(ws)
    .bind(contact_id)
    .bind(recipient_id)
    .execute(setup.conn())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO communication_bases
            (workspace_id, email_address_id, purpose, jurisdiction, basis_type,
             status, evidence_ref, collected_at, reviewed_by, reviewed_at)
         VALUES ($1, $2, 'marketing', 'US', 'explicit_consent', 'valid',
                 'isolated-campaign-consent', now(), $3, now())",
    )
    .bind(ws)
    .bind(recipient_id)
    .bind(rig.world.admin.principal_id)
    .execute(setup.conn())
    .await
    .unwrap();

    let template_id: Uuid = sqlx::query_scalar(
        "INSERT INTO email_templates (workspace_id, name, created_by)
         VALUES ($1, $2, $3) RETURNING id",
    )
    .bind(ws)
    .bind(format!("campaign-execute-{}", Uuid::new_v4()))
    .bind(rig.world.admin.principal_id)
    .fetch_one(setup.conn())
    .await
    .unwrap();
    let template_version_id: Uuid = sqlx::query_scalar(
        "INSERT INTO template_versions
            (workspace_id, template_id, version_no, subject, body_text, body_html,
             variable_schema, content_hash, created_by)
         VALUES ($1, $2, 1, 'A tracked campaign offer',
                 'See https://example.test/offer',
                 '<p>See <a href=\"https://example.test/offer\">the offer</a></p>',
                 '{\"type\":\"object\",\"properties\":{},\"required\":[],\"additionalProperties\":false}',
                 decode(md5(random()::text), 'hex'), $3)
         RETURNING id",
    )
    .bind(ws)
    .bind(template_id)
    .bind(rig.world.admin.principal_id)
    .fetch_one(setup.conn())
    .await
    .unwrap();
    setup.commit().await.unwrap();

    let draft = rig
        .call(
            "campaign_create_draft",
            json!({
                "idempotency_key":"agent-campaign-create-001",
                "name":"Explicitly approved campaign test",
                "objective":"Verify separated approval and execution",
                "mail_account_id":rig.world.mail_account_id,
                "delivery_channel_id":channel_id,
                "timezone":"UTC",
                "schedule_rules":{
                    "weekdays":[1,2,3,4,5,6,7],
                    "window_start":"00:00",
                    "window_end":"23:59"
                },
                "steps":[{"step_order":1,"template_version_id":template_version_id,"delay_hours":0}]
            }),
        )
        .await;
    let campaign_id = Uuid::parse_str(draft["campaign_id"].as_str().unwrap()).unwrap();
    let revision_id = Uuid::parse_str(draft["revision_id"].as_str().unwrap()).unwrap();

    let created = rig
        .agent_post(
            &format!("/api/v1/campaigns/{campaign_id}/launches"),
            json!({
                "revision_id":revision_id,
                "email_address_ids":[recipient_id],
                "tracking_requested":true
            }),
            None,
            "agent-campaign-launch-001",
        )
        .await;
    let created = human_data(created, StatusCode::ACCEPTED).await;
    let launch_id = Uuid::parse_str(created["launch_id"].as_str().unwrap()).unwrap();
    let freeze_job_id = Uuid::parse_str(created["job_id"].as_str().unwrap()).unwrap();
    let mut tx = rig.owner.begin(ws).await.unwrap();
    let freeze_payload: Value =
        sqlx::query_scalar("SELECT payload FROM jobs WHERE workspace_id=$1 AND id=$2")
            .bind(ws)
            .bind(freeze_job_id)
            .fetch_one(tx.conn())
            .await
            .unwrap();
    let variables: BTreeMap<String, String> = serde_json::from_value(
        freeze_payload
            .get("variables")
            .cloned()
            .unwrap_or_else(|| json!({})),
    )
    .unwrap();
    application::campaigns::freeze_launch(
        &mut tx,
        rig.world.system.principal_id,
        freeze_job_id,
        launch_id,
        &[recipient_id],
        &variables,
        freeze_payload["tracking_requested"]
            .as_bool()
            .unwrap_or(false),
        &freeze_payload["requester_context"],
    )
    .await
    .unwrap();
    sqlx::query(
        "UPDATE jobs SET state='completed', finished_at=now() WHERE workspace_id=$1 AND id=$2",
    )
    .bind(ws)
    .bind(freeze_job_id)
    .execute(tx.conn())
    .await
    .unwrap();
    let (approval_id, manifest): (Uuid, Vec<u8>) = sqlx::query_as(
        "SELECT ar.id, l.manifest_hash
           FROM approval_requests ar
           JOIN campaign_launches l ON l.workspace_id=ar.workspace_id AND l.id=ar.launch_id
          WHERE ar.workspace_id=$1 AND ar.launch_id=$2",
    )
    .bind(ws)
    .bind(launch_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let manifest_hash: String = manifest.iter().map(|byte| format!("{byte:02x}")).collect();

    let decision = human_data(
        rig.manager_post(
            &format!("/api/v1/approvals/{approval_id}/decisions"),
            json!({
                "decision":"approved",
                "expected_snapshot_hash":manifest_hash,
                "reason":"Human reviewed this exact campaign manifest"
            }),
            Some(1),
            "agent-campaign-human-approval-001",
        )
        .await,
        StatusCode::OK,
    )
    .await;
    assert_eq!(decision["execution_status"], "ready");
    assert!(
        decision["job_id"].is_null(),
        "approval never queues activation"
    );

    let send_grant = human_data(
        rig.admin_put_agent_grant(
            json!({
                "can_read":true,
                "can_compose":true,
                "can_send":true,
                "can_approve":false,
                "can_assign":false
            }),
            Some(rig.agent_grant_version),
        )
        .await,
        StatusCode::OK,
    )
    .await;
    assert_eq!(send_grant["can_send"], true);
    rig.agent_grant_version = send_grant["version"].as_i64().unwrap();

    let launch_detail = human_data(
        rig.api
            .get(&format!("/api/v1/campaign-launches/{launch_id}"))
            .await,
        StatusCode::OK,
    )
    .await;
    assert_eq!(launch_detail["status"], "approved");
    assert_eq!(launch_detail["execution_status"], "ready");
    let submit_args = json!({
        "idempotency_key":"agent-campaign-submit-approved-001",
        "launch_id":launch_id,
        "expected_manifest_hash":manifest_hash,
        "expected_version":launch_detail["version"]
    });
    let submitted = rig
        .call("campaign_submit_approved", submit_args.clone())
        .await;
    assert_eq!(submitted["launch_status"], "approved");
    assert_eq!(submitted["execution_status"], "queued");
    let repeated = rig.call("campaign_submit_approved", submit_args).await;
    assert_eq!(repeated["job_id"], submitted["job_id"]);
    let activation_job_id = Uuid::parse_str(submitted["job_id"].as_str().unwrap()).unwrap();

    let safe_status = rig
        .call("campaign_get_launch", json!({"launch_id":launch_id}))
        .await;
    assert_eq!(safe_status["execution_status"], "queued");
    assert_eq!(
        safe_status["activation_job_id"],
        activation_job_id.to_string()
    );
    assert_eq!(safe_status["enrollment_ids"].as_array().unwrap().len(), 1);
    assert!(safe_status.get("messages").is_none());
    assert!(safe_status.get("to_address_snapshot").is_none());

    let mut tx = rig.owner.begin(ws).await.unwrap();
    let activation_payload: Value =
        sqlx::query_scalar("SELECT payload FROM jobs WHERE workspace_id=$1 AND id=$2")
            .bind(ws)
            .bind(activation_job_id)
            .fetch_one(tx.conn())
            .await
            .unwrap();
    let activation_approval_id =
        Uuid::parse_str(activation_payload["approval_request_id"].as_str().unwrap()).unwrap();
    application::campaigns::activate_launch(
        &mut tx,
        rig.world.system.principal_id,
        activation_job_id,
        launch_id,
        activation_approval_id,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let mut tx = rig.owner.begin(ws).await.unwrap();
    #[allow(clippy::type_complexity)]
    let outbound: (
        Uuid,
        Option<Uuid>,
        Option<String>,
        Option<Uuid>,
        Option<chrono::DateTime<chrono::Utc>>,
        String,
        String,
        String,
        Option<String>,
    ) = sqlx::query_as(
        "SELECT id, thread_id, internet_message_id, source_credential_id,
                explicit_submission_at, from_address_snapshot, to_address_snapshot,
                body_text, body_html
           FROM outbound_messages WHERE workspace_id=$1 AND launch_id=$2",
    )
    .bind(ws)
    .bind(launch_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    assert_eq!(outbound.3, Some(rig.credential_id));
    let thread_id = outbound
        .1
        .expect("campaign message must belong to a local thread");
    let internet_message_id = outbound
        .2
        .expect("campaign message must have a stable RFC ID");
    let explicit_submission_at = outbound.4.expect("explicit campaign execution marker");
    assert!(outbound.7.contains("/public/v1/engagement/click/"));
    assert!(outbound.7.contains("/public/unsubscribe/"));
    let capability_count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM outbound_engagement_capabilities
          WHERE workspace_id=$1 AND outbound_message_id=$2",
    )
    .bind(ws)
    .bind(outbound.0)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    assert_eq!(capability_count, 2);
    let dispatch_payload: Value = sqlx::query_scalar(
        "SELECT payload FROM jobs WHERE workspace_id=$1 AND kind='sending.dispatch_outbound'
           AND payload->>'outbound_message_id'=$2",
    )
    .bind(ws)
    .bind(outbound.0.to_string())
    .fetch_one(tx.conn())
    .await
    .unwrap();
    let dispatch_marker: chrono::DateTime<chrono::Utc> = serde_json::from_value(json!(
        dispatch_payload["explicit_submission_at"].as_str().unwrap()
    ))
    .unwrap();
    assert_eq!(dispatch_marker, explicit_submission_at);

    let jobs = infrastructure::jobs::claim(
        &mut tx,
        "isolated-campaign-fake-worker",
        1,
        &["sending.dispatch_outbound"],
    )
    .await
    .unwrap();
    assert_eq!(jobs.len(), 1, "exactly one dispatch is claimable");
    let job = jobs.into_iter().next().unwrap();
    let lease_token = job.lease_token.unwrap();
    let job_ctx = infrastructure::jobs::JobCtx {
        job_id: job.id,
        workspace_id: ws,
        lease_token,
        attempt_count: job.attempt_count,
        lease_owner: job.lease_owner.clone().unwrap(),
    };
    tx.commit().await.unwrap();

    let fake_delivery = Arc::new(infrastructure::providers::fake::FakeDeliveryProvider::new());
    let providers = application::sending::dispatch::Providers {
        mail: Arc::new(infrastructure::providers::fake::FakeMailProvider::new()),
        delivery: fake_delivery.clone(),
    };
    application::sending::dispatch::run_dispatch(&db, &providers, &job_ctx, outbound.0)
        .await
        .unwrap();
    let mut tx = rig.owner.begin(ws).await.unwrap();
    assert!(
        infrastructure::jobs::complete(&mut tx, job.id, lease_token)
            .await
            .unwrap()
    );
    tx.commit().await.unwrap();
    let sent = fake_delivery.sent();
    assert_eq!(sent.len(), 1, "authorized campaign sends exactly once");
    assert_eq!(
        sent[0].0.internet_message_id.as_deref(),
        Some(internet_message_id.as_str())
    );

    let click_token = sent[0]
        .0
        .body_text
        .as_deref()
        .unwrap()
        .split("/public/v1/engagement/click/")
        .nth(1)
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap()
        .trim_end_matches(['.', ',', ';', '!', ')']);
    let click = rig
        .api
        .client
        .get(
            rig.api
                .url(&format!("/public/v1/engagement/click/{click_token}")),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(click.status(), StatusCode::FOUND);
    assert_eq!(
        click.headers().get("location").unwrap(),
        "https://example.test/offer"
    );
    let html = sent[0].0.body_html.as_deref().unwrap();
    let open_token = html
        .split("/public/v1/engagement/open/")
        .nth(1)
        .unwrap()
        .split('"')
        .next()
        .unwrap();
    let open = rig
        .api
        .client
        .get(
            rig.api
                .url(&format!("/public/v1/engagement/open/{open_token}")),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(open.status(), StatusCode::OK);
    assert_eq!(open.headers().get("content-type").unwrap(), "image/gif");

    let reply = infrastructure::providers::ProviderMessage {
        provider_message_id: format!("fake-campaign-reply-{}", Uuid::new_v4()),
        provider_thread_id: None,
        internet_message_id: Some(format!("<reply-{}@example.test>", Uuid::new_v4())),
        in_reply_to: Some(internet_message_id),
        references: vec![],
        from: outbound.6.clone(),
        to: vec![outbound.5.clone()],
        subject: Some("Re: A tracked campaign offer".into()),
        body_text: Some("Thanks, let's talk.".into()),
        body_html: None,
        message_at: chrono::Utc::now(),
    };
    let mut tx = rig.owner.begin(ws).await.unwrap();
    let ingested = application::inbox::ingest::ingest_provider_message(
        &mut tx,
        None,
        rig.world.mail_account_id,
        &reply,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(
        ingested.thread_id, thread_id,
        "reply matches the frozen local thread"
    );

    let outbound_status = human_data(
        rig.api
            .get(&format!("/api/v1/outbound-messages/{}", outbound.0))
            .await,
        StatusCode::OK,
    )
    .await;
    assert_eq!(outbound_status["status"], "accepted");
    assert_eq!(outbound_status["thread_id"], thread_id.to_string());
    assert_eq!(outbound_status["engagement"]["tracking_enabled"], true);
    assert_eq!(outbound_status["engagement"]["click_request_count"], 1);
    assert_eq!(outbound_status["engagement"]["open_request_count"], 1);
    assert_eq!(
        outbound_status["known_replies"].as_array().unwrap().len(),
        1
    );
}
