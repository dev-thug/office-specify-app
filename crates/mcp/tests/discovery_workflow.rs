//! Real HTTP MCP + cookie/CSRF REST fixtures against an isolated DB.
//! No SMTP, external research fetch, or live-app records are used.
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
    db: Db,
    world: TestWorld,
    api: TestApp,
    csrf: String,
    url: String,
    token: String,
    client: reqwest::Client,
    run_id: Option<Uuid>,
    server: tokio::task::JoinHandle<()>,
}

impl Drop for Rig {
    fn drop(&mut self) {
        self.server.abort();
    }
}

impl Rig {
    async fn new() -> Self {
        let url = std::env::var("MCP_TEST_DATABASE_URL").expect("MCP_TEST_DATABASE_URL required");
        assert!(
            url.ends_with("/outreach_mcp_workflow"),
            "use only the isolated outreach_mcp_workflow database"
        );
        let owner = Db::connect(&url, None).await.unwrap();
        owner.migrate().await.unwrap();
        let world = seed_workspace(&owner).await.unwrap();
        let db = Db::connect(&url, Some(ROLE_API))
            .await
            .unwrap()
            .with_default_workspace(Some(world.workspace_id));
        let mut tx = owner.begin(world.workspace_id).await.unwrap();
        let scopes: Vec<String> = [
            "crm.read",
            "crm.write",
            "research.propose",
            "research.write",
            "templates.read",
            "templates.write",
            "mail.read",
            "mail.compose",
            "booking.read",
            "booking.propose",
        ]
        .into_iter()
        .map(String::from)
        .collect();
        let (_, token) = infrastructure::repo::auth::create_service_credential(
            &mut tx,
            world.agent.principal_id,
            "isolated MCP workflow",
            &scopes,
            chrono::Duration::hours(1),
        )
        .await
        .unwrap();
        sqlx::query("INSERT INTO mail_account_grants (workspace_id,mail_account_id,principal_id,can_read,can_compose,granted_by) VALUES ($1,$2,$3,true,true,$4)")
            .bind(world.workspace_id).bind(world.mail_account_id).bind(world.agent.principal_id).bind(world.admin.principal_id).execute(tx.conn()).await.unwrap();
        tx.commit().await.unwrap();
        let api = TestApp::spawn_with_state(AppState::with_fake_providers(
            db.clone(),
            Arc::new(Config::test()),
            None,
        ))
        .await;
        let login = api
            .login(world.admin.email.as_ref().unwrap(), TEST_PASSWORD)
            .await;
        assert_eq!(login.status(), StatusCode::OK);
        let csrf = TestApp::csrf_token_from(&login).unwrap();
        let app = build_router(McpState {
            db: db.clone(),
            calendar: Arc::new(infrastructure::providers::fake::FakeMailProvider::new()),
            config: Arc::new(infrastructure::config::Config::test()),
            rate_limiter: None,
            allowed_origins: Arc::new(vec![]),
            tools: Arc::new(mcp::all_tools_with_key(
                b"isolated-mcp-workflow-hmac-key-32bytes",
            )),
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/mcp", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let rig = Self {
            owner,
            db,
            world,
            api,
            csrf,
            url,
            token,
            client: reqwest::Client::new(),
            run_id: None,
            server,
        };
        let init=rig.rpc("initialize",json!({"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"office-agent-workflow-test","version":"1"}})).await;
        assert_eq!(init["result"]["protocolVersion"], "2025-11-25");
        assert!(
            init["result"]["instructions"]
                .as_str()
                .unwrap()
                .contains("crm_search_contacts")
        );
        let notify = rig
            .client
            .post(&rig.url)
            .bearer_auth(&rig.token)
            .json(&json!({"jsonrpc":"2.0","method":"notifications/initialized"}))
            .send()
            .await
            .unwrap();
        assert_eq!(notify.status(), StatusCode::ACCEPTED);
        rig
    }

    async fn rpc(&self, method: &str, params: Value) -> Value {
        let mut request=self.client.post(&self.url).bearer_auth(&self.token).header("MCP-Protocol-Version","2025-11-25")
            .json(&json!({"jsonrpc":"2.0","id":Uuid::new_v4().to_string(),"method":method,"params":params}));
        if let Some(run_id) = self.run_id {
            request = request.header("X-Agent-Run-Id", run_id.to_string());
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

    async fn denied(&self, name: &str, args: Value) -> Value {
        let response = self
            .rpc("tools/call", json!({"name":name,"arguments":args}))
            .await;
        assert_eq!(response["result"]["isError"], true, "{name}: {response}");
        response["result"]["structuredContent"]["error"].clone()
    }

    async fn human_post(&self, path: &str, body: Value, version: Option<i64>) -> Value {
        let mut request = self
            .api
            .client
            .post(self.api.url(path))
            .header("x-csrf-token", &self.csrf)
            .header("idempotency-key", Uuid::new_v4().to_string())
            .json(&body);
        if let Some(version) = version {
            request = request.header("if-match", format!("\"{version}\""));
        }
        let response = request.send().await.unwrap();
        let status = response.status();
        let value: Value = response.json().await.unwrap();
        assert!(status.is_success(), "{path}: {status} {value}");
        value["data"].clone()
    }
}

#[tokio::test]
async fn agent_research_writes_crm_directly_and_supports_followup() {
    let mut rig = Rig::new().await;
    let company_name = format!("MCP Connected {}", Uuid::new_v4());
    let initial = rig
        .call("crm_search_companies", json!({"query":company_name}))
        .await;
    assert!(initial["companies"].as_array().unwrap().is_empty());
    let run=rig.human_post("/api/v1/agent-runs",json!({"agent_principal_id":rig.world.agent.principal_id,"objective":"Sourced research written directly to CRM","budget":{"max_tool_calls":50,"max_proposals":5}}),None).await;
    rig.run_id = Some(Uuid::parse_str(run["id"].as_str().unwrap()).unwrap());
    // The review-only lead-proposal tools are gone; agents write CRM records directly.
    for removed in [
        "research_propose_leads",
        "research_get_proposal",
        "research_accept_proposal",
        "crm_propose_update",
    ] {
        let response = rig
            .rpc("tools/call", json!({"name":removed,"arguments":{}}))
            .await;
        assert_eq!(response["error"]["code"], -32601, "{removed}: {response}");
    }
    let args = json!({"name":company_name,"domain":"mcp-connected.example","industry":"Software","country_code":"KR","website_url":"https://mcp-connected.example","idempotency_key":Uuid::new_v4().to_string()});
    let created = rig.call("crm_create_company", args.clone()).await;
    let replay = rig.call("crm_create_company", args).await;
    assert!(created["id"].is_string(), "{created}");
    assert_eq!(created["id"], replay["id"]);
    let company_id = &created["id"];
    let found = rig
        .call("crm_search_companies", json!({"query":company_name}))
        .await;
    assert_eq!(found["companies"].as_array().unwrap().len(), 1);
    assert_eq!(found["companies"][0]["id"], *company_id);
    for (name, extra) in [
        (
            "Agent Test One",
            json!({"job_title":"Sales Manager","department":"Sales","email":"one@mcp-connected.example"}),
        ),
        (
            "Agent Test Two",
            json!({"email":"two@mcp-connected.example"}),
        ),
    ] {
        let mut contact = json!({"name":name,"company_id":company_id,"idempotency_key":Uuid::new_v4().to_string()});
        for (key, value) in extra.as_object().unwrap() {
            contact[key] = value.clone();
        }
        rig.call("crm_create_contact", contact).await;
    }
    let company = rig
        .call(
            "crm_get_company",
            json!({"company_id":company_id,"requested_sections":["contacts","research"]}),
        )
        .await;
    assert_eq!(company["company"]["industry"], "Software");
    assert_eq!(company["company"]["country_code"], "KR");
    assert_eq!(
        company["company"]["website_url"],
        "https://mcp-connected.example"
    );
    let first = rig
        .call(
            "crm_search_contacts",
            json!({"company_id":company_id,"limit":1}),
        )
        .await;
    assert_eq!(first["contacts"].as_array().unwrap().len(), 1);
    assert_eq!(first["has_more"], true);
    let second = rig
        .call(
            "crm_search_contacts",
            json!({"company_id":company_id,"limit":1,"cursor":first["next_cursor"]}),
        )
        .await;
    assert_eq!(second["contacts"].as_array().unwrap().len(), 1);
    assert_ne!(first["contacts"][0]["id"], second["contacts"][0]["id"]);
    for contact in [&first["contacts"][0], &second["contacts"][0]] {
        assert_eq!(contact["emails"][0]["verification_status"], "unknown");
        // Eligibility depends only on suppressions (communication basis is
        // reference-only); these fresh addresses are not suppressed.
        assert_eq!(
            contact["emails"][0]["marketing_eligibility"]["eligible"], true,
            "{contact}"
        );
    }
    let named = rig
        .call(
            "crm_search_contacts",
            json!({"company_id":company_id,"query":"Agent Test One"}),
        )
        .await;
    assert_eq!(named["contacts"].as_array().unwrap().len(), 1);
    assert_eq!(named["contacts"][0]["name"], "Agent Test One");
    rig.denied("crm_search_contacts",json!({"company_id":company_id,"limit":1,"query":"different","cursor":first["next_cursor"]})).await;
    let changes =
        json!({"industry":"AI Software","description":"Agent-updated company description"});
    let stale = rig
        .denied(
            "crm_update_company",
            json!({"company_id":company_id,"expected_version":company["company"]["version"].as_i64().unwrap() + 1,"changes":changes}),
        )
        .await;
    assert_eq!(stale["code"], "VERSION_MISMATCH", "{stale}");
    assert_eq!(
        rig.call("crm_get_company", json!({"company_id":company_id}))
            .await["company"]["industry"],
        "Software"
    );
    let update = rig
        .call(
            "crm_update_company",
            json!({"company_id":company_id,"expected_version":company["company"]["version"],"changes":changes}),
        )
        .await;
    assert_eq!(update["industry"], "AI Software");
    assert!(update["version"].as_i64() > company["company"]["version"].as_i64());
    let updated = rig
        .call("crm_get_company", json!({"company_id":company_id}))
        .await;
    assert_eq!(
        updated["company"]["description"],
        "Agent-updated company description"
    );
    assert_eq!(named["contacts"][0]["job_title"], "Sales Manager");
    assert_eq!(named["contacts"][0]["department"], "Sales");
    let note_args = json!({"target":{"company_id":company_id},"text":"Sourced company research summary from the MCP agent.","idempotency_key":Uuid::new_v4().to_string()});
    let note = rig.call("crm_create_note", note_args.clone()).await;
    assert_eq!(
        note["note_id"],
        rig.call("crm_create_note", note_args).await["note_id"]
    );
    let evidence=rig.call("research_add_evidence",json!({"agent_run_id":run["id"],"company_id":company_id,"field_path":"employee_count","source_url":"https://public.example/team","observed_at":chrono::Utc::now().to_rfc3339(),"value":42,"evidence_type":"observed","idempotency_key":Uuid::new_v4().to_string()})).await;
    assert_eq!(evidence["review_status"], "pending");
    let final_record = rig
        .call(
            "crm_get_company",
            json!({"company_id":company_id,"requested_sections":["notes","research"]}),
        )
        .await;
    assert!(
        final_record["sections"]["notes"]["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["id"] == note["note_id"])
    );
    assert!(
        final_record["sections"]["research"]["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["value"] == 42)
    );
    let report = rig
        .api
        .get(&format!(
            "/api/v1/agent-runs/{}",
            run["id"].as_str().unwrap()
        ))
        .await;
    assert_eq!(report.status(), StatusCode::OK);
    let report: Value = report.json().await.unwrap();
    for name in [
        "crm_search_contacts",
        "crm_create_company",
        "crm_update_company",
        "crm_create_note",
    ] {
        assert!(
            report["data"]["actions"]
                .as_array()
                .unwrap()
                .iter()
                .any(|action| action["tool_name"] == name),
            "run audit missing {name}"
        );
    }
}

#[tokio::test]
async fn discovery_pages_templates_renders_and_gates_host_access_on_org_admin() {
    let rig = Rig::new().await;
    let template = rig
        .human_post(
            "/api/v1/email-templates",
            json!({"name":"Agent discovered email"}),
            None,
        )
        .await;
    rig.human_post(
        "/api/v1/email-templates",
        json!({"name":"Another paginated template"}),
        None,
    )
    .await;
    let body = json!({"subject":"Hello {{contact.name}}","body_text":"Hello {{contact.name}}. Unsubscribe: {{compliance.unsubscribe_url}}","body_html":"<p>Hello {{contact.name}}</p><p><a href=\"{{compliance.unsubscribe_url}}\">Unsubscribe</a></p>","variable_schema":{"type":"object","properties":{"contact.name":{"type":"string","maxLength":300},"compliance.unsubscribe_url":{"type":"string","format":"uri","maxLength":2048}},"required":["contact.name","compliance.unsubscribe_url"],"additionalProperties":false}});
    for _ in 0..2 {
        rig.human_post(
            &format!(
                "/api/v1/email-templates/{}/versions",
                template["template_id"].as_str().unwrap()
            ),
            body.clone(),
            None,
        )
        .await;
    }
    let first = rig.call("templates_list", json!({"limit":1})).await;
    assert_eq!(first["has_more"], true);
    let next = rig
        .call(
            "templates_list",
            json!({"limit":1,"cursor":first["next_cursor"]}),
        )
        .await;
    assert_ne!(
        first["templates"][0]["template_id"],
        next["templates"][0]["template_id"]
    );
    rig.denied(
        "templates_list",
        json!({"limit":1,"include_archived":true,"cursor":first["next_cursor"]}),
    )
    .await;
    rig.denied("templates_list", json!({"limit":0})).await;
    let template_id = [&first["templates"][0], &next["templates"][0]]
        .into_iter()
        .find(|item| item["name"] == "Agent discovered email")
        .unwrap()["template_id"]
        .clone();
    let discovered = rig
        .call("templates_get", json!({"template_id":template_id}))
        .await;
    assert_eq!(discovered["name"], "Agent discovered email");
    let versions = rig
        .call(
            "templates_list_versions",
            json!({"template_id":template_id,"limit":1}),
        )
        .await;
    assert_eq!(versions["versions"][0]["version_no"], 2);
    assert_eq!(versions["has_more"], true);
    let older = rig
        .call(
            "templates_list_versions",
            json!({"template_id":template_id,"limit":1,"cursor":versions["next_cursor"]}),
        )
        .await;
    assert_eq!(older["versions"][0]["version_no"], 1);
    let version_id = &versions["versions"][0]["template_version_id"];
    let content = rig
        .call(
            "templates_get_version",
            json!({"template_version_id":version_id}),
        )
        .await;
    assert_eq!(
        content["variable_schema"]["properties"]["contact.name"]["type"],
        "string"
    );
    let rendered = rig
        .call(
            "templates_render_version",
            json!({"template_version_id":version_id,"variables":{"contact.name":"Agent Preview"}}),
        )
        .await;
    assert_eq!(rendered["subject"], "Hello Agent Preview");
    assert!(
        rendered["body_text"]
            .as_str()
            .unwrap()
            .contains("https://preview.invalid/")
    );
    rig.denied("templates_render_version",json!({"template_version_id":version_id,"variables":{"contact.name":"Agent Preview","compliance.unsubscribe_url":"https://attacker.example"}})).await;
    let calendar = rig
        .human_post("/api/v1/calendar-connections/local", json!({}), None)
        .await;
    let meeting=rig.human_post("/api/v1/meeting-types",json!({"calendar_connection_id":calendar["id"],"name":"Agent readable meeting","duration_minutes":30}),None).await;
    // Without org.admin an agent has operator-level host access only, so
    // another host's meeting type stays closed.
    rig.denied(
        "booking_list_slots",
        json!({"meeting_type_id":meeting["id"],"from":"2026-10-12","to":"2026-10-13","timezone":"Asia/Seoul"}),
    )
    .await;
    let visible = rig.call("booking_list_meeting_types", json!({})).await;
    assert_eq!(
        visible["meeting_types"][0]["name"],
        "Agent readable meeting"
    );
    let details = rig
        .call(
            "booking_get_meeting_type",
            json!({"meeting_type_id":visible["meeting_types"][0]["id"]}),
        )
        .await;
    assert_eq!(details["duration_minutes"], 30);
    assert!(details.get("calendar_connection_id").is_none());
    assert!(details.get("provider_calendar_id").is_none());
    rig.human_post("/api/v1/meeting-types",json!({"calendar_connection_id":calendar["id"],"name":"Second agent readable meeting","duration_minutes":45}),None).await;
    let page = rig
        .call("booking_list_meeting_types", json!({"limit":1}))
        .await;
    assert_eq!(page["has_more"], true);
    let next = rig
        .call(
            "booking_list_meeting_types",
            json!({"limit":1,"cursor":page["next_cursor"]}),
        )
        .await;
    assert_eq!(next["has_more"], false);
    assert_ne!(
        page["meeting_types"][0]["id"],
        next["meeting_types"][0]["id"]
    );
    rig.denied(
        "booking_list_meeting_types",
        json!({"limit":1,"include_disabled":true,"cursor":page["next_cursor"]}),
    )
    .await;
    let propose_args = json!({"meeting_type_id":meeting["id"],"guest_name":"Test Guest","guest_email":"guest@example.com","desired_starts_at":"2026-10-12T01:00:00Z","timezone":"Asia/Seoul","purpose":"Preserve host authorization","idempotency_key":Uuid::new_v4().to_string()});
    rig.denied("booking_propose", propose_args.clone()).await;
    // With org.admin the agent gets admin-equivalent host access.
    let mut tx = rig.owner.begin(rig.world.workspace_id).await.unwrap();
    let (_, admin_token) = infrastructure::repo::auth::create_service_credential(
        &mut tx,
        rig.world.agent.principal_id,
        "org admin booking",
        &[
            "booking.read".into(),
            "booking.propose".into(),
            "org.admin".into(),
        ],
        chrono::Duration::hours(1),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let as_admin = |name: &'static str, args: Value| {
        let request = rig.client.post(&rig.url).bearer_auth(&admin_token).json(
            &json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":name,"arguments":args}}),
        );
        async move { request.send().await.unwrap().json::<Value>().await.unwrap() }
    };
    let slots = as_admin(
        "booking_list_slots",
        json!({"meeting_type_id":meeting["id"],"from":"2026-10-12","to":"2026-10-13","timezone":"Asia/Seoul"}),
    )
    .await;
    assert_eq!(slots["result"]["isError"], false, "{slots}");
    assert_eq!(
        slots["result"]["structuredContent"]["data"]["reservation_guaranteed"],
        false
    );
    let propose = as_admin("booking_propose", propose_args).await;
    // A proposal is still only an approval request, never a confirmed booking.
    let proposal = &propose["result"]["structuredContent"]["data"];
    assert_eq!(propose["result"]["isError"], false, "{propose}");
    assert_eq!(proposal["status"], "proposed");
    assert!(proposal["booking_id"].is_null());
    assert!(proposal["approval_request_id"].is_string());
}

#[tokio::test]
async fn new_reads_deny_missing_scopes_and_foreign_workspace_ids() {
    let rig = Rig::new().await;
    let foreign_rig = Rig::new().await;
    let foreign = &foreign_rig.world;
    let foreign_admin =
        infrastructure::testing::ctx_for(foreign, &foreign.admin, "foreign-template-fixture");
    let foreign_template = application::templates::create(
        &rig.db,
        &foreign_admin,
        &contracts::templates::CreateEmailTemplateRequest {
            name: "Foreign private template".into(),
        },
    )
    .await
    .unwrap();
    rig.denied(
        "templates_get",
        json!({"template_id":foreign_template.template_id}),
    )
    .await;
    rig.denied(
        "templates_list",
        json!({"workspace_id":foreign.workspace_id}),
    )
    .await;
    let foreign_calendar = foreign_rig
        .human_post("/api/v1/calendar-connections/local", json!({}), None)
        .await;
    let foreign_meeting=foreign_rig.human_post("/api/v1/meeting-types",json!({"calendar_connection_id":foreign_calendar["id"],"name":"Foreign meeting","duration_minutes":30}),None).await;
    rig.denied(
        "booking_get_meeting_type",
        json!({"meeting_type_id":foreign_meeting["id"]}),
    )
    .await;
    assert!(
        rig.call("booking_list_meeting_types", json!({})).await["meeting_types"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    for name in ["Cursor template one", "Cursor template two"] {
        rig.human_post("/api/v1/email-templates", json!({"name":name}), None)
            .await;
    }
    let cursor_page = rig.call("templates_list", json!({"limit":1})).await;
    assert_eq!(cursor_page["has_more"], true);
    let mut tx = rig.owner.begin(rig.world.workspace_id).await.unwrap();
    let (_, token) = infrastructure::repo::auth::create_service_credential(
        &mut tx,
        rig.world.agent.principal_id,
        "no discovery scopes",
        &[],
        chrono::Duration::hours(1),
    )
    .await
    .unwrap();
    let (_, other_caller) = infrastructure::repo::auth::create_service_credential(
        &mut tx,
        rig.world.member_a.principal_id,
        "different cursor caller",
        &["templates.read".into()],
        chrono::Duration::hours(1),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    for (arguments, expected_error) in [
        (json!({"limit":1}), false),
        (json!({"limit":1,"cursor":cursor_page["next_cursor"]}), true),
    ] {
        let response:Value=rig.client.post(&rig.url).bearer_auth(&other_caller).json(&json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"templates_list","arguments":arguments}})).send().await.unwrap().json().await.unwrap();
        assert_eq!(
            response["result"]["isError"], expected_error,
            "cursor caller binding: {response}"
        );
    }
    for (name, args) in [
        ("crm_search_contacts", json!({})),
        ("templates_list", json!({})),
        ("booking_list_meeting_types", json!({})),
    ] {
        let response:Value=rig.client.post(&rig.url).bearer_auth(&token).json(&json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":name,"arguments":args}})).send().await.unwrap().json().await.unwrap();
        assert_eq!(response["result"]["isError"], true, "{name}: {response}");
    }
}
