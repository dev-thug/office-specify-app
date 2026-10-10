//! Real MCP research/draft workflow plus human REST review on an isolated DB.
//! Research URLs are inert fixtures; no browser fetch or provider dispatch is performed.
use api::{AppState, testing::TestApp};
use infrastructure::{
    config::Config,
    db::{Db, ROLE_API},
    testing::{TEST_PASSWORD, TestWorld, seed_workspace},
};
use mcp::server::{McpState, build_router};
use reqwest::StatusCode;
use serde_json::{Value, json};
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
            db_url.ends_with("/outreach_mcp_sales_verify"),
            "use only the isolated outreach_mcp_sales_verify database"
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
            "operations.read",
        ]
        .into_iter()
        .map(String::from)
        .collect();
        let (_, token) = infrastructure::repo::auth::create_service_credential(
            &mut tx,
            world.agent.principal_id,
            "isolated MCP sales workflow",
            &scopes,
            chrono::Duration::hours(1),
        )
        .await
        .unwrap();
        // can_send is deliberately granted: the application must still reject
        // an agent's direct submit, independently of mailbox capabilities.
        sqlx::query(
            "INSERT INTO mail_account_grants
                (workspace_id, mail_account_id, principal_id,
                 can_read, can_compose, can_send, can_approve, can_assign, granted_by)
             VALUES ($1, $2, $3, true, true, true, false, false, $4)",
        )
        .bind(world.workspace_id)
        .bind(world.mail_account_id)
        .bind(world.agent.principal_id)
        .bind(world.admin.principal_id)
        .execute(tx.conn())
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
            client: reqwest::Client::new(),
            run_id: Uuid::nil(),
            server,
        };

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

async fn review_lead(rig: &Rig, proposal_id: Uuid) -> Value {
    human_data(
        rig.human_post(
            &format!("/api/v1/lead-proposals/{proposal_id}/decisions"),
            json!({"decision":"accept","reason":"Human reviewed sourced company proposal"}),
            Some(1),
            &format!("sales-review-lead-{proposal_id}"),
        )
        .await,
        StatusCode::OK,
    )
    .await
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
    let rig = Rig::new().await;
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
    assert!(tool_names.contains(&"research_propose_leads"));
    assert!(tool_names.contains(&"crm_get_company"));
    assert!(tool_names.contains(&"mail_create_draft"));
    assert!(!tool_names.contains(&"mail_submit"));
    assert!(!tool_names.contains(&"approvals_decide"));
    assert!(!tool_names.contains(&"mail_get_outbound"));
    assert!(tool_names.contains(&"mail_get_thread"));

    let proposal_args = json!({
        "agent_run_id":rig.run_id,
        "idempotency_key":format!("sales-lead-batch-{suffix}"),
        "proposals":[
            {
                "company_name":aurora_name,
                "domain":format!("aurora-{suffix}.example"),
                "proposed_data":{
                    "industry":"Energy storage",
                    "country_code":"KR",
                    "website_url":format!("https://aurora-{suffix}.example"),
                    "contacts":[{
                        "name":"Ara Kim",
                        "job_title":"Sales Director",
                        "department":"Sales",
                        "email":format!("ara-{suffix}@aurora.example.test")
                    }]
                },
                "evidence":[{
                    "field_path":"industry",
                    "source_url":format!("https://public.example/aurora/{suffix}/company"),
                    "observed_at":chrono::Utc::now().to_rfc3339(),
                    "value":"Energy storage",
                    "evidence_type":"observed",
                    "excerpt":"Company profile identifies grid-scale storage as its sector"
                }]
            },
            {
                "company_name":forge_name,
                "domain":format!("forge-{suffix}.example"),
                "proposed_data":{
                    "industry":"Industrial automation",
                    "country_code":"KR",
                    "website_url":format!("https://forge-{suffix}.example"),
                    "contacts":[{
                        "name":"Min Lee",
                        "job_title":"Commercial Lead",
                        "department":"Partnerships",
                        "email":format!("min-{suffix}@forge.example.test")
                    }]
                },
                "evidence":[{
                    "field_path":"industry",
                    "source_url":format!("https://public.example/forge/{suffix}/company"),
                    "observed_at":chrono::Utc::now().to_rfc3339(),
                    "value":"Industrial automation",
                    "evidence_type":"observed",
                    "excerpt":"Company profile identifies warehouse robotics as its sector"
                }]
            }
        ]
    });
    let proposed = rig
        .call("research_propose_leads", proposal_args.clone())
        .await;
    let replay = rig.call("research_propose_leads", proposal_args).await;
    assert_eq!(proposed["proposal_ids"], replay["proposal_ids"]);
    assert_eq!(proposed["status"], "pending");
    let proposal_ids = proposed["proposal_ids"].as_array().unwrap();
    assert_eq!(proposal_ids.len(), 2);

    assert!(
        rig.call("crm_search_companies", json!({"query":aurora_name}))
            .await["companies"]
            .as_array()
            .unwrap()
            .is_empty(),
        "a sourced proposal must not create a CRM company before human review"
    );
    let accept_attempt = rig
        .tool_error(
            "research_accept_proposal",
            json!({"lead_proposal_id":proposal_ids[0]}),
        )
        .await;
    assert!(accept_attempt.get("error").is_some());

    let first_review = review_lead(
        &rig,
        Uuid::parse_str(proposal_ids[0].as_str().unwrap()).unwrap(),
    )
    .await;
    let second_review = review_lead(
        &rig,
        Uuid::parse_str(proposal_ids[1].as_str().unwrap()).unwrap(),
    )
    .await;
    let aurora_company =
        Uuid::parse_str(first_review["accepted_company_id"].as_str().unwrap()).unwrap();
    let forge_company =
        Uuid::parse_str(second_review["accepted_company_id"].as_str().unwrap()).unwrap();
    assert_ne!(aurora_company, forge_company);

    for (proposal_id, expected_company_id) in [
        (proposal_ids[0].clone(), aurora_company),
        (proposal_ids[1].clone(), forge_company),
    ] {
        let accepted = rig
            .call(
                "research_get_proposal",
                json!({"lead_proposal_id":proposal_id}),
            )
            .await;
        assert_eq!(accepted["status"], "accepted");
        assert_eq!(
            accepted["accepted_company_id"],
            expected_company_id.to_string()
        );
    }

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
    let agent_preapproval = problem(agent_preapproval, StatusCode::FORBIDDEN).await;
    assert_eq!(agent_preapproval["code"], "AGENT_DIRECT_SEND_FORBIDDEN");

    let mcp_submit = rig
        .tool_error(
            "mail_submit",
            json!({"outbound_message_id":outbound_id,"expected_payload_hash":payload_hash}),
        )
        .await;
    assert!(mcp_submit.get("error").is_some());
    let mcp_decide = rig
        .tool_error(
            "approvals_decide",
            json!({"approval_request_id":approval_id,"decision":"approved"}),
        )
        .await;
    assert!(mcp_decide.get("error").is_some());

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
    assert_eq!(decision["execution_status"], "queued");
    assert_eq!(decision["provider_accepted_count"], 0);
    let job_id = decision["job_id"].as_str().unwrap().to_string();

    let replay = human_data(
        rig.manager_post(&decision_path, decision_body, Some(1), decision_key)
            .await,
        StatusCode::OK,
    )
    .await;
    assert_eq!(replay["decision_id"], decision["decision_id"]);
    assert_eq!(replay["job_id"], job_id);
    assert_eq!(replay["provider_accepted_count"], 0);

    let queued = human_data(
        rig.api
            .get(&format!("/api/v1/outbound-messages/{outbound_id}"))
            .await,
        StatusCode::OK,
    )
    .await;
    assert_eq!(queued["status"], "queued");
    assert_eq!(queued["provider_accepted"], Value::Null);
    assert_eq!(queued["delivery_status"], "unknown");
    let remaining_draft = human_data(
        rig.api
            .get(&format!("/api/v1/message-drafts/{forge_draft_id}"))
            .await,
        StatusCode::OK,
    )
    .await;
    assert_eq!(remaining_draft["status"], "draft");
    assert!(remaining_draft["outbound_message_id"].is_null());

    // The job endpoint is catalogued for polling, but approval-created
    // dispatch jobs have no principal owner, so this agent cannot read one.
    let job_poll = rig
        .tool_error("operations_get_job", json!({"job_id":job_id}))
        .await;
    assert_eq!(job_poll["result"]["isError"], true);
    assert_eq!(
        job_poll["result"]["structuredContent"]["error"]["code"],
        "NOT_FOUND"
    );
}
