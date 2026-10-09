use axum::{
    Router,
    body::{Body, Bytes},
    extract::{OriginalUri, State},
    http::{HeaderMap, Method, StatusCode},
    response::Response,
    routing::any,
};
use infrastructure::providers::{
    SendOutcome, SendRequest,
    zoho_campaigns::SecretValue,
    zoho_email_api::{
        SuppressionAddRequest, TemplateContentType, TemplateWriteRequest, ZohoEmailApi,
        ZohoEmailApiError,
    },
};
use serde_json::Value;
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
    time::Duration,
};

#[derive(Clone)]
struct MockReply {
    status: StatusCode,
    body: Vec<u8>,
    location: Option<&'static str>,
    delay: Option<Duration>,
}

impl MockReply {
    fn json(status: StatusCode, body: &str) -> Self {
        Self {
            status,
            body: body.as_bytes().to_vec(),
            location: None,
            delay: None,
        }
    }
}

#[derive(Clone, Debug)]
struct CapturedRequest {
    method: Method,
    path_and_query: String,
    authorization: Option<String>,
    content_type: Option<String>,
    body: Vec<u8>,
}

#[derive(Clone)]
struct MockState {
    requests: Arc<Mutex<Vec<CapturedRequest>>>,
    replies: Arc<Mutex<VecDeque<MockReply>>>,
}

async fn mock_response(
    State(state): State<MockState>,
    OriginalUri(uri): OriginalUri,
    method: Method,
    headers: HeaderMap,
    body: Bytes,
) -> Response<Body> {
    state.requests.lock().unwrap().push(CapturedRequest {
        method,
        path_and_query: uri
            .path_and_query()
            .map(|value| value.as_str().to_owned())
            .unwrap_or_else(|| uri.path().to_owned()),
        authorization: headers
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned),
        content_type: headers
            .get("content-type")
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned),
        body: body.to_vec(),
    });
    let reply = state
        .replies
        .lock()
        .unwrap()
        .pop_front()
        .unwrap_or_else(|| MockReply::json(StatusCode::NOT_FOUND, "{}"));
    if let Some(delay) = reply.delay {
        tokio::time::sleep(delay).await;
    }
    let mut builder = Response::builder()
        .status(reply.status)
        .header("content-type", "application/json");
    if let Some(location) = reply.location {
        builder = builder.header("location", location);
    }
    builder.body(Body::from(reply.body)).unwrap()
}

async fn start_client(
    replies: Vec<MockReply>,
    timeout: Duration,
) -> (
    ZohoEmailApi,
    Arc<Mutex<Vec<CapturedRequest>>>,
    tokio::task::JoinHandle<()>,
) {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let state = MockState {
        requests: requests.clone(),
        replies: Arc::new(Mutex::new(replies.into())),
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let app = Router::new().fallback(any(mock_response)).with_state(state);
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let client = ZohoEmailApi::for_test_with_timeout(
        Some(SecretValue::new("unit-api-key")),
        &base_url,
        timeout,
    )
    .unwrap();
    (client, requests, server)
}

fn json_body(request: &CapturedRequest) -> Value {
    serde_json::from_slice(&request.body).unwrap()
}

fn send_request() -> SendRequest {
    SendRequest {
        correlation_id: "booking-42".into(),
        from: "bookings@office.example".into(),
        to: "guest@example.test".into(),
        subject: "Your booking is confirmed".into(),
        body_text: Some("Manage your booking: https://office.example/manage/abc".into()),
        body_html: Some(
            concat!(
                "<html><body><p>Confirmed.</p>",
                "<a href=\"https://office.example/manage/abc?x=1&amp;y=2\">Manage booking</a>",
                "<a href=\"https://office.example/unsubscribe/token\">Unsubscribe</a>",
                "</body></html>"
            )
            .into(),
        ),
        in_reply_to: None,
        headers: Vec::new(),
    }
}

#[tokio::test]
async fn missing_key_and_capabilities_are_explicit_and_conservative() {
    let client = ZohoEmailApi::new(None).unwrap();
    assert!(matches!(
        client.list_templates(1, 20).await,
        Err(ZohoEmailApiError::NotConfigured)
    ));
    assert!(!client.capabilities().supports_send);
    let secret_client = ZohoEmailApi::new(Some(SecretValue::new("do-not-print-this-key"))).unwrap();
    assert!(!format!("{secret_client:?}").contains("do-not-print-this-key"));
}

#[tokio::test]
async fn template_crud_preserves_content_and_uses_documented_success_codes() {
    let content = "<html><body>Office template — keep this intact.</body></html>";
    let replies = vec![
        MockReply::json(
            StatusCode::OK,
            r#"{"response":{"code":200701,"message":"ignored"},"templates":[{"name":"Invoice","id":"tpl-1","zuid":123}],"count":1,"range":{"start_index":1,"end_index":50}}"#,
        ),
        MockReply::json(
            StatusCode::OK,
            r#"{"response":{"code":200702},"template_details":{"content_type":"html","name":"Invoice","id":"tpl/one? c","content":"<html><body>Office template — keep this intact.</body></html>"}}"#,
        ),
        MockReply::json(
            StatusCode::OK,
            r#"{"response":{"code":200703},"template_id":"new-template-2"}"#,
        ),
        MockReply::json(StatusCode::OK, r#"{"response":{"code":200704}}"#),
        MockReply::json(StatusCode::OK, r#"{"response":{"code":200705}}"#),
    ];
    let (client, captured, server) = start_client(replies, Duration::from_secs(2)).await;

    let templates = client.list_templates(1, 50).await.unwrap();
    assert_eq!(templates[0].id, "tpl-1");
    assert_eq!(templates[0].name, "Invoice");
    let detail = client.get_template("tpl/one? c").await.unwrap();
    assert_eq!(detail.content, content);
    assert_eq!(detail.content_type, TemplateContentType::Html);
    let created = client
        .create_template(TemplateWriteRequest {
            name: "New invoice".into(),
            content: content.into(),
            content_type: TemplateContentType::Html,
        })
        .await
        .unwrap();
    assert_eq!(created, "new-template-2");
    client
        .update_template(
            "tpl-1",
            TemplateWriteRequest {
                name: "Invoice v2".into(),
                content: content.into(),
                content_type: TemplateContentType::Html,
            },
        )
        .await
        .unwrap();
    client.delete_template("tpl-1").await.unwrap();

    let requests = captured.lock().unwrap();
    assert_eq!(requests.len(), 5);
    assert_eq!(requests[0].method, Method::GET);
    assert!(requests[0].path_and_query.contains("start_index=1"));
    assert!(requests[0].path_and_query.contains("end_index=50"));
    assert_eq!(
        requests[1].path_and_query,
        "/emailapi/v2/templates/tpl%2Fone%3F%20c"
    );
    assert_eq!(requests[2].method, Method::POST);
    assert_eq!(
        requests[2].content_type.as_deref(),
        Some("application/json")
    );
    assert_eq!(json_body(&requests[2])["template_name"], "New invoice");
    assert_eq!(json_body(&requests[2])["content"], content);
    assert_eq!(json_body(&requests[2])["content_type"], "html");
    assert_eq!(requests[3].method, Method::PUT);
    assert_eq!(requests[4].method, Method::DELETE);
    for request in requests.iter() {
        assert_eq!(
            request.authorization.as_deref(),
            Some("Zoho-zapikey unit-api-key")
        );
    }
    server.abort();
}

#[tokio::test]
async fn inline_send_keeps_office_html_and_links_and_reports_sibling_transmission_id() {
    let replies = vec![MockReply::json(
        StatusCode::OK,
        r#"{"accepted_recipients":1,"rejected_recipients":0,"response":{"code":200001,"message":"success"},"transmission_id":"tx-908"}"#,
    )];
    let (client, captured, server) = start_client(replies, Duration::from_secs(2)).await;
    let request = send_request();
    let html = request.body_html.clone().unwrap();
    let result = client
        .send_inline(request, Some("Office Team"), Some("support@office.example"))
        .await
        .unwrap();
    assert!(
        matches!(result, SendOutcome::Accepted { provider_message_id } if provider_message_id == "tx-908")
    );

    let requests = captured.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, Method::POST);
    assert_eq!(requests[0].path_and_query, "/emailapi/v2/transmission");
    assert_eq!(
        requests[0].authorization.as_deref(),
        Some("Zoho-zapikey unit-api-key")
    );
    let body = json_body(&requests[0]);
    assert_eq!(body["transmission_name"], "booking-42");
    assert_eq!(body["recipients"].as_array().unwrap().len(), 1);
    assert_eq!(body["recipients"][0]["address"], "guest@example.test");
    assert_eq!(
        body["content"]["from"]["address"],
        "bookings@office.example"
    );
    assert_eq!(body["content"]["from"]["name"], "Office Team");
    assert_eq!(body["content"]["reply_to"], "support@office.example");
    assert_eq!(body["content"]["html"], html);
    assert_eq!(
        body["content"]["text"],
        "Manage your booking: https://office.example/manage/abc"
    );
    assert_eq!(body["options"]["open_tracking"], "disabled");
    assert_eq!(body["options"]["click_tracking"], "disabled");
    assert!(body.get("schedule_at").is_none());
    assert!(body["content"].get("template_id").is_none());
    server.abort();
}

#[tokio::test]
async fn send_maps_documented_recipient_counts_and_api_rejection_codes() {
    let (client, _captured, server) = start_client(
        vec![
            MockReply::json(
                StatusCode::OK,
                r#"{"accepted_recipients":0,"rejected_recipients":1,"response":{"code":200001},"transmission_id":"tx-rejected"}"#,
            ),
            MockReply::json(StatusCode::OK, r#"{"errors":[{"code":400009,"message":"sensitive provider detail"}]}"#),
            MockReply::json(StatusCode::OK, r#"{"accepted_recipients":2,"rejected_recipients":0,"response":{"code":200001},"transmission_id":"tx-ambiguous"}"#),
        ],
        Duration::from_secs(2),
    )
    .await;
    let rejected = client
        .send_inline(send_request(), None, None)
        .await
        .unwrap();
    assert!(matches!(
        rejected,
        SendOutcome::Rejected {
            permanent: false,
            ..
        }
    ));
    let api_rejection = client
        .send_inline(send_request(), None, None)
        .await
        .unwrap();
    assert!(
        matches!(api_rejection, SendOutcome::Rejected { permanent: true, ref code } if code == "api_400009")
    );
    assert!(!format!("{api_rejection:?}").contains("sensitive provider detail"));
    let ambiguous_counts = client
        .send_inline(send_request(), None, None)
        .await
        .unwrap();
    assert!(matches!(ambiguous_counts, SendOutcome::Unknown { .. }));
    server.abort();
}

#[tokio::test]
async fn transport_status_timeout_redirect_and_malformed_responses_never_claim_acceptance() {
    let mut redirect = MockReply::json(StatusCode::FOUND, "{}");
    redirect.location = Some("https://attacker.example/capture");
    let mut slow = MockReply::json(
        StatusCode::OK,
        r#"{"accepted_recipients":1,"rejected_recipients":0,"response":{"code":200001},"transmission_id":"late"}"#,
    );
    slow.delay = Some(Duration::from_millis(100));
    let mut oversized = MockReply::json(StatusCode::OK, "{}");
    oversized.body = vec![b'x'; 1_048_577];
    let replies = vec![
        MockReply::json(StatusCode::INTERNAL_SERVER_ERROR, "sensitive payload"),
        MockReply::json(StatusCode::SERVICE_UNAVAILABLE, "{}"),
        MockReply::json(
            StatusCode::UNAUTHORIZED,
            "private address in provider payload",
        ),
        MockReply::json(StatusCode::FORBIDDEN, "private address in provider payload"),
        MockReply::json(
            StatusCode::TOO_MANY_REQUESTS,
            "private address in provider payload",
        ),
        redirect,
        MockReply::json(StatusCode::OK, "not-json"),
        oversized,
        slow,
    ];
    let (client, captured, server) = start_client(replies, Duration::from_millis(25)).await;
    for _ in 0..2 {
        let outcome = client
            .send_inline(send_request(), None, None)
            .await
            .unwrap();
        assert!(matches!(outcome, SendOutcome::Unknown { .. }));
    }
    for expected in ["http_401", "http_403", "rate_limited"] {
        let outcome = client
            .send_inline(send_request(), None, None)
            .await
            .unwrap();
        assert!(matches!(outcome, SendOutcome::Rejected { ref code, .. } if code == expected));
        assert!(!format!("{outcome:?}").contains("private address"));
    }
    for _ in 0..4 {
        let outcome = client
            .send_inline(send_request(), None, None)
            .await
            .unwrap();
        assert!(matches!(outcome, SendOutcome::Unknown { .. }));
    }
    assert_eq!(
        captured.lock().unwrap().len(),
        9,
        "redirect must not be followed"
    );
    server.abort();
    let disconnected = client
        .send_inline(send_request(), None, None)
        .await
        .unwrap();
    assert!(matches!(disconnected, SendOutcome::Unknown { .. }));
}

#[tokio::test]
async fn requests_reject_unsupported_headers_and_reply_threading_before_network_io() {
    let (client, captured, server) = start_client(vec![], Duration::from_secs(2)).await;
    let mut request = send_request();
    request.headers.push(("X-Trace".into(), "test".into()));
    assert!(matches!(
        client.send_inline(request, None, None).await,
        Err(ZohoEmailApiError::UnsupportedFeature)
    ));
    let mut request = send_request();
    request.in_reply_to = Some("<parent@example.test>".into());
    assert!(matches!(
        client.send_inline(request, None, None).await,
        Err(ZohoEmailApiError::UnsupportedFeature)
    ));
    assert!(captured.lock().unwrap().is_empty());
    let capabilities = client.capabilities();
    assert!(capabilities.supports_send);
    assert!(!capabilities.one_click_unsubscribe);
    assert!(!capabilities.custom_headers);
    assert!(!capabilities.idempotency);
    server.abort();
}

#[tokio::test]
async fn oversized_template_content_is_rejected_before_transmission() {
    let (client, captured, server) = start_client(vec![], Duration::from_secs(2)).await;
    let result = client
        .create_template(TemplateWriteRequest {
            name: "too large".into(),
            content: "x".repeat(600 * 1024),
            content_type: TemplateContentType::Html,
        })
        .await;
    assert!(matches!(result, Err(ZohoEmailApiError::RequestTooLarge)));
    assert!(captured.lock().unwrap().is_empty());
    server.abort();
}

#[tokio::test]
async fn template_reads_bound_response_size_and_reject_malformed_json() {
    let mut oversized = MockReply::json(StatusCode::OK, "{}");
    oversized.body = vec![b'x'; 1_048_577];
    let (client, _captured, server) = start_client(
        vec![oversized, MockReply::json(StatusCode::OK, "not-json")],
        Duration::from_secs(2),
    )
    .await;
    assert!(matches!(
        client.list_templates(1, 10).await,
        Err(ZohoEmailApiError::ResponseTooLarge)
    ));
    assert!(matches!(
        client.list_templates(1, 10).await,
        Err(ZohoEmailApiError::InvalidResponse)
    ));
    server.abort();
}

#[tokio::test]
async fn transmission_lookup_uses_documented_detail_endpoint_and_success_code() {
    let (client, captured, server) = start_client(
        vec![MockReply::json(
            StatusCode::OK,
            r#"{"transmission_details":{"subject":"Booking confirmed","from":"bookings@office.example","reply_to":"support@office.example","transmission_name":"booking-42","status":"Sent","sent_time":1728472166000},"response":{"code":200004,"message":"ok"}}"#,
        )],
        Duration::from_secs(2),
    )
    .await;
    let detail = client.get_transmission("tx/42").await.unwrap();
    assert_eq!(detail.subject, "Booking confirmed");
    assert_eq!(detail.status, "Sent");
    assert_eq!(detail.transmission_name, "booking-42");
    assert_eq!(detail.sent_time_ms, Some(1_728_472_166_000));
    assert_eq!(
        captured.lock().unwrap()[0].path_and_query,
        "/emailapi/v2/transmission/tx%2F42"
    );
    server.abort();
}

#[tokio::test]
async fn get_suppressions_returns_a_bounded_typed_page() {
    let (client, captured, server) = start_client(
        vec![MockReply::json(
            StatusCode::OK,
            r#"{"response":{"code":200401,"message":"ok"},"suppression":[{"address":"opted-out@example.test","additional_data":{"source":"office"},"reason":{"reason":"unsubscribe"},"added_time":1728472166000,"modified_time":1728472167000,"suppression_type":"MANUAL","zuid":-1}],"range":{"start_index":1,"end_index":500}}"#,
        )],
        Duration::from_secs(2),
    )
    .await;
    let page = client.get_suppressions(1, 500).await.unwrap();
    assert_eq!(page.entries.len(), 1);
    assert_eq!(page.entries[0].address, "opted-out@example.test");
    assert_eq!(
        page.entries[0].additional_data.as_ref().unwrap()["source"],
        "office"
    );
    assert_eq!(
        page.entries[0].reason.as_ref().unwrap()["reason"],
        "unsubscribe"
    );
    assert_eq!(page.entries[0].added_time_ms, Some(1_728_472_166_000));
    assert_eq!(page.entries[0].modified_time_ms, Some(1_728_472_167_000));
    assert_eq!(page.entries[0].suppression_type.as_deref(), Some("MANUAL"));
    assert_eq!(page.entries[0].zuid, Some(-1));
    assert_eq!(page.range.start_index, 1);
    assert_eq!(page.range.end_index, 500);

    let requests = captured.lock().unwrap();
    assert_eq!(requests[0].method, Method::GET);
    assert_eq!(
        requests[0].path_and_query,
        "/emailapi/v2/recipients/suppression?start_index=1&end_index=500"
    );
    assert_eq!(
        requests[0].authorization.as_deref(),
        Some("Zoho-zapikey unit-api-key")
    );
    server.abort();
}

#[tokio::test]
async fn add_suppressions_posts_only_the_documented_add_shape() {
    let (client, captured, server) = start_client(
        vec![MockReply::json(
            StatusCode::OK,
            r#"{"suppressions_added":["new@example.test"],"response":{"code":200402,"message":"ok"},"suppressions_already_existing":["already@example.test"]}"#,
        )],
        Duration::from_secs(2),
    )
    .await;
    let mut additional_data = serde_json::Map::new();
    additional_data.insert("source".into(), Value::String("office".into()));
    let mut reason = serde_json::Map::new();
    reason.insert("reason".into(), Value::String("unsubscribe".into()));
    let result = client
        .add_suppressions(&[SuppressionAddRequest {
            address: "new@example.test".into(),
            additional_data: Some(additional_data),
            reason: Some(reason),
        }])
        .await
        .unwrap();
    assert_eq!(result.added, vec!["new@example.test"]);
    assert_eq!(result.already_existing, vec!["already@example.test"]);

    let requests = captured.lock().unwrap();
    assert_eq!(requests[0].method, Method::POST);
    assert_eq!(
        requests[0].path_and_query,
        "/emailapi/v2/recipients/suppression"
    );
    assert_eq!(
        requests[0].authorization.as_deref(),
        Some("Zoho-zapikey unit-api-key")
    );
    assert_eq!(
        requests[0].content_type.as_deref(),
        Some("application/json")
    );
    let body = json_body(&requests[0]);
    assert_eq!(body["recipients"].as_array().unwrap().len(), 1);
    assert_eq!(body["recipients"][0]["address"], "new@example.test");
    assert_eq!(body["recipients"][0]["additional_data"]["source"], "office");
    assert_eq!(body["recipients"][0]["reason"]["reason"], "unsubscribe");
    server.abort();
}

#[tokio::test]
async fn suppression_pagination_and_response_size_are_bounded() {
    let mut oversized = MockReply::json(StatusCode::OK, "{}");
    oversized.body = vec![b'x'; 1_048_577];
    let (client, captured, server) = start_client(
        vec![
            oversized,
            MockReply::json(
                StatusCode::OK,
                r#"{"response":{"code":200401},"suppression":[],"range":{"start_index":501,"end_index":1000}}"#,
            ),
        ],
        Duration::from_secs(2),
    )
    .await;
    assert!(matches!(
        client.get_suppressions(0, 500).await,
        Err(ZohoEmailApiError::InvalidRequest)
    ));
    assert!(matches!(
        client.get_suppressions(1, 501).await,
        Err(ZohoEmailApiError::InvalidRequest)
    ));
    assert!(captured.lock().unwrap().is_empty());
    assert!(matches!(
        client.get_suppressions(1, 500).await,
        Err(ZohoEmailApiError::ResponseTooLarge)
    ));
    let page = client.get_suppressions(501, 1000).await.unwrap();
    assert!(page.entries.is_empty());
    assert_eq!(page.range.start_index, 501);
    assert_eq!(page.range.end_index, 1000);
    assert_eq!(captured.lock().unwrap().len(), 2);
    server.abort();
}

#[tokio::test]
async fn suppression_api_errors_are_safe_and_add_requires_recipients() {
    let (client, captured, server) = start_client(
        vec![MockReply::json(
            StatusCode::OK,
            r#"{"errors":[{"code":500401,"message":"private@example.test should not escape"}]}"#,
        )],
        Duration::from_secs(2),
    )
    .await;
    assert!(matches!(
        client.add_suppressions(&[]).await,
        Err(ZohoEmailApiError::InvalidRequest)
    ));
    assert!(captured.lock().unwrap().is_empty());
    let error = client
        .add_suppressions(&[SuppressionAddRequest {
            address: "private@example.test".into(),
            additional_data: None,
            reason: None,
        }])
        .await
        .unwrap_err();
    assert_eq!(error, ZohoEmailApiError::Rejected { code: Some(500401) });
    assert!(!format!("{error:?}").contains("private@example.test"));
    assert!(!error.to_string().contains("private@example.test"));
    server.abort();
}
