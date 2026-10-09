use infrastructure::providers::zoho_campaigns::{CampaignsOAuthConfig, SecretValue};

#[test]
fn missing_configuration_never_produces_a_provider_authorization_url() {
    let config = CampaignsOAuthConfig::default();
    assert!(config.authorization_url("state").is_err());
    assert_eq!(config.missing_fields().len(), 3);
}
#[test]
fn authorization_url_has_exact_required_parameters_and_campaign_scopes() {
    let config = CampaignsOAuthConfig {
        allow_local_redirect: true,
        client_id: Some("1000.unit-client".into()),
        client_secret: Some(SecretValue::new("private-secret")),
        redirect_uri: Some(
            "http://127.0.0.1:13000/api/v1/integrations/zoho-campaigns/callback".into(),
        ),
    };
    let url =
        reqwest::Url::parse(&config.authorization_url("state+with&reserved").unwrap()).unwrap();
    let query: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
    assert_eq!(url.host_str(), Some("accounts.zoho.com"));
    assert_eq!(query["client_id"], "1000.unit-client");
    assert_eq!(query["response_type"], "code");
    assert_eq!(query["access_type"], "offline");
    assert_eq!(query["prompt"], "consent");
    assert_eq!(query["state"], "state+with&reserved");
    assert!(query["scope"].contains("ZohoCampaigns.campaign.READ"));
    assert!(!query["scope"].contains("ZohoMail"));
    assert!(!format!("{config:?}").contains("private-secret"));
}
#[test]
fn placeholder_clients_and_unsafe_callback_urls_are_not_configured() {
    for redirect in [
        "https://evil.test/callback?secret=1",
        "javascript:alert(1)",
        "http://office.example/callback",
    ] {
        let config = CampaignsOAuthConfig {
            allow_local_redirect: true,
            client_id: Some("1000.client".into()),
            client_secret: Some(SecretValue::new("secret")),
            redirect_uri: Some(redirect.into()),
        };
        assert!(config.authorization_url("state").is_err());
    }
    let config = CampaignsOAuthConfig {
        allow_local_redirect: true,
        client_id: Some("dev-zoho-client-id".into()),
        client_secret: Some(SecretValue::new("secret")),
        redirect_uri: Some("https://office.example/callback".into()),
    };
    assert!(config.authorization_url("state").is_err());
}

#[tokio::test]
async fn token_exchange_uses_form_body_and_list_probe_uses_campaigns_header() {
    use axum::{
        Json, Router,
        extract::{Form, State},
        http::HeaderMap,
        routing::{get, post},
    };
    use std::sync::{Arc, Mutex};
    type Captured = Arc<Mutex<Vec<serde_json::Value>>>;
    async fn token(
        State(c): State<Captured>,
        Form(form): Form<std::collections::HashMap<String, String>>,
    ) -> Json<serde_json::Value> {
        c.lock().unwrap().push(serde_json::to_value(form).unwrap());
        Json(
            serde_json::json!({"access_token":"access-private", "refresh_token":"refresh-private", "expires_in":3600, "api_domain":"https://api.zoho.com"}),
        )
    }
    async fn lists(headers: HeaderMap) -> Json<serde_json::Value> {
        assert_eq!(headers["authorization"], "Zoho-oauthtoken access-private");
        Json(
            serde_json::json!({"code":"0","status":"success","list_of_details":[{"listkey":"list-1","listname":"Newsletter","noofcontacts":"2"}]}),
        )
    }
    let captured: Captured = Arc::new(Mutex::new(vec![]));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let app = Router::new()
        .route("/oauth/v2/token", post(token))
        .route("/api/v1.1/getmailinglists", get(lists))
        .with_state(captured.clone());
    let handle = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let config = CampaignsOAuthConfig {
        allow_local_redirect: true,
        client_id: Some("1000.unit".into()),
        client_secret: Some(SecretValue::new("private-client-secret")),
        redirect_uri: Some("http://localhost:3000/callback".into()),
    };
    let client =
        infrastructure::providers::zoho_campaigns::http::CampaignsHttp::for_test(config, &base)
            .unwrap();
    let tokens = client.exchange_code("private-code").await.unwrap();
    assert!(!format!("{tokens:?}").contains("access-private"));
    assert_eq!(
        client.list_lists(&tokens.access_token).await.unwrap()[0].name,
        "Newsletter"
    );
    let form = &captured.lock().unwrap()[0];
    assert_eq!(form["client_id"], "1000.unit");
    assert_eq!(form["client_secret"], "private-client-secret");
    assert_eq!(form["code"], "private-code");
    assert_eq!(form["grant_type"], "authorization_code");
    handle.abort();
}

#[test]
fn production_requires_a_public_https_callback() {
    let mut config = CampaignsOAuthConfig {
        client_id: Some("1000.client".into()),
        client_secret: Some(SecretValue::new("secret")),
        redirect_uri: Some("http://127.0.0.1:13000/callback".into()),
        allow_local_redirect: false,
    };
    assert!(config.authorization_url("state").is_err());
    config.redirect_uri = Some("https://localhost/callback".into());
    assert!(config.authorization_url("state").is_err());
    config.redirect_uri = Some("https://office.example/callback".into());
    assert!(config.authorization_url("state").is_ok());
}

#[tokio::test]
async fn mailing_list_verification_reads_all_pages_without_silent_truncation() {
    use axum::{Json, Router, extract::Query, routing::get};
    use serde_json::json;
    async fn lists(
        Query(query): Query<std::collections::HashMap<String, String>>,
    ) -> Json<serde_json::Value> {
        let start = query["fromindex"].parse::<usize>().unwrap();
        let rows = if start == 1 {
            (0..100).map(|i|json!({"listkey":format!("list-{i}"),"listname":format!("List {i}"),"noofcontacts":"0"})).collect::<Vec<_>>()
        } else {
            vec![json!({"listkey":"list-100","listname":"Final list","noofcontacts":"1"})]
        };
        Json(json!({"code":"0","list_of_details":rows}))
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new().route("/api/v1.1/getmailinglists", get(lists)),
        )
        .await
        .unwrap()
    });
    let config = CampaignsOAuthConfig {
        client_id: Some("1000.test".into()),
        client_secret: Some(SecretValue::new("secret")),
        redirect_uri: Some("http://localhost/callback".into()),
        allow_local_redirect: true,
    };
    let client =
        infrastructure::providers::zoho_campaigns::http::CampaignsHttp::for_test(config, &base)
            .unwrap();
    let result = client.list_lists(&SecretValue::new("token")).await.unwrap();
    assert_eq!(result.len(), 101);
    assert_eq!(result[100].name, "Final list");
    task.abort();
}

#[test]
fn campaign_delivery_consent_requests_only_required_read_create_update_scopes() {
    use infrastructure::providers::zoho_campaigns::CAMPAIGNS_SCOPES;
    for scope in [
        "ZohoCampaigns.campaign.READ",
        "ZohoCampaigns.campaign.CREATE",
        "ZohoCampaigns.campaign.UPDATE",
        "ZohoCampaigns.contact.READ",
        "ZohoCampaigns.contact.CREATE",
        "ZohoCampaigns.contact.UPDATE",
    ] {
        assert!(CAMPAIGNS_SCOPES.contains(&scope), "missing {scope}");
    }
    assert!(
        !CAMPAIGNS_SCOPES
            .iter()
            .any(|s| s.ends_with("ALL") || s.ends_with("DELETE"))
    );
}
