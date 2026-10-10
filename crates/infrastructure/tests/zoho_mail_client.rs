use axum::{
    Router,
    body::{Body, to_bytes},
    extract::State,
    http::{HeaderMap, Method, Request, Response, StatusCode, Uri},
    routing::any,
};
use infrastructure::providers::{
    SecretValue, SendOutcome,
    zoho_mail_client::{
        ZohoMailClient, ZohoMailClientError, ZohoMailCompose, ZohoMailListOptions, ZohoMailSearch,
    },
};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    net::SocketAddr,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::net::TcpListener;

#[derive(Clone)]
struct MockResponse {
    status: StatusCode,
    headers: HeaderMap,
    body: Vec<u8>,
    delay: Duration,
}

impl MockResponse {
    fn json(body: Value) -> Self {
        Self {
            status: StatusCode::OK,
            headers: HeaderMap::new(),
            body: serde_json::to_vec(&body).unwrap(),
            delay: Duration::ZERO,
        }
    }

    fn status(status: StatusCode, body: impl Into<Vec<u8>>) -> Self {
        Self {
            status,
            headers: HeaderMap::new(),
            body: body.into(),
            delay: Duration::ZERO,
        }
    }
}

#[derive(Clone, Debug)]
struct CapturedRequest {
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Vec<u8>,
}

#[derive(Default)]
struct MockState {
    responses: Mutex<HashMap<String, MockResponse>>,
    requests: Mutex<Vec<CapturedRequest>>,
}

async fn serve_mock(State(state): State<Arc<MockState>>, request: Request<Body>) -> Response<Body> {
    let (parts, body) = request.into_parts();
    let body = to_bytes(body, 4 * 1024 * 1024).await.unwrap().to_vec();
    let key = format!("{} {}", parts.method, parts.uri.path());
    let response = state
        .responses
        .lock()
        .unwrap()
        .get(&key)
        .cloned()
        .unwrap_or_else(|| MockResponse::status(StatusCode::NOT_FOUND, "missing mock route"));
    state.requests.lock().unwrap().push(CapturedRequest {
        method: parts.method,
        uri: parts.uri,
        headers: parts.headers,
        body,
    });
    if !response.delay.is_zero() {
        tokio::time::sleep(response.delay).await;
    }
    let mut builder = Response::builder().status(response.status);
    for (key, value) in response.headers.iter() {
        builder = builder.header(key, value);
    }
    builder.body(Body::from(response.body)).unwrap()
}

async fn mock_client(
    responses: impl IntoIterator<Item = (String, MockResponse)>,
) -> (ZohoMailClient, Arc<MockState>, SocketAddr) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let state = Arc::new(MockState::default());
    state.responses.lock().unwrap().extend(responses);
    let app = Router::new()
        .fallback(any(serve_mock))
        .with_state(state.clone());
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let client = ZohoMailClient::with_base_url_for_testing(
        SecretValue::new("unit-test-access-token"),
        &format!("http://{address}/"),
    )
    .unwrap();
    (client, state, address)
}

fn ok(data: Value) -> Value {
    json!({ "status": { "code": 200, "description": "success" }, "data": data })
}

fn expect_error<T>(result: Result<T, ZohoMailClientError>) -> ZohoMailClientError {
    match result {
        Err(error) => error,
        Ok(_) => panic!("expected a Zoho Mail client error"),
    }
}

fn compose() -> ZohoMailCompose {
    ZohoMailCompose {
        from_address: "sender@example.test".into(),
        to_address: "recipient@example.test".into(),
        cc_address: None,
        bcc_address: None,
        subject: "Hello".into(),
        content: "Body".into(),
        mail_format: Default::default(),
        ask_receipt: None,
    }
}

#[tokio::test]
async fn discovers_accounts_and_normalizes_zoho_status_data_envelope() {
    let (client, state, _) = mock_client([(
        "GET /api/accounts".to_string(),
        MockResponse::json(ok(json!([{
            "accountId": "2560636000000008002",
            "accountDisplayName": "Team Inbox",
            "mailboxAddress": "team@example.test",
            "primaryEmailAddress": "team@example.test",
            "enabled": true
        }]))),
    )])
    .await;

    let accounts = client.list_accounts().await.unwrap();
    assert_eq!(accounts.len(), 1);
    assert_eq!(accounts[0].account_id, "2560636000000008002");
    assert_eq!(
        accounts[0].mailbox_address.as_deref(),
        Some("team@example.test")
    );
    assert_eq!(
        accounts[0].account_display_name.as_deref(),
        Some("Team Inbox")
    );
    assert!(!format!("{client:?}").contains("unit-test-access-token"));

    let requests = state.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, Method::GET);
    assert_eq!(requests[0].uri.path(), "/api/accounts");
    assert_eq!(
        requests[0].headers.get("authorization").unwrap(),
        "Zoho-oauthtoken unit-test-access-token"
    );
}

#[tokio::test]
async fn lists_folders_and_messages_with_numeric_or_string_ids() {
    let (client, _, _) = mock_client([
        (
            "GET /api/accounts/123/folders".to_string(),
            MockResponse::json(ok(json!([{
                "folderId": 9000000002014u64,
                "folderName": "Inbox",
                "folderType": "Inbox",
                "path": "/Inbox",
                "imapAccess": true
            }]))),
        ),
        (
            "GET /api/accounts/123/messages/view".to_string(),
            MockResponse::json(ok(json!([{
                "messageId": 1709887058769100001u64,
                "folderId": "9000000002014",
                "threadId": 1709883095364100001u64,
                "fromAddress": "sender@example.test",
                "toAddress": "recipient@example.test",
                "subject": "Question",
                "receivedTime": 1709887053409u64
            }]))),
        ),
    ])
    .await;

    let folders = client.list_folders("123").await.unwrap();
    assert_eq!(folders[0].folder_id, "9000000002014");
    assert_eq!(folders[0].folder_name, "Inbox");

    let messages = client
        .list_messages("123", "9000000002014", &ZohoMailListOptions::default())
        .await
        .unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].message_id, "1709887058769100001");
    assert_eq!(
        messages[0].thread_id.as_deref(),
        Some("1709883095364100001")
    );
    assert_eq!(messages[0].received_time.as_deref(), Some("1709887053409"));
}

#[tokio::test]
async fn search_query_is_encoded_and_provider_message_details_and_content_are_normalized() {
    let search_response = ok(json!([{
        "messageId": "9000000019029",
        "folderId": "9000000000905",
        "subject": "Project update",
        "fromAddress": "sender@example.test"
    }]));
    let (client, state, _) = mock_client([
        (
            "GET /api/accounts/123/messages/search".to_string(),
            MockResponse::json(search_response),
        ),
        (
            "GET /api/accounts/123/folders/9000000000905/messages/9000000019029/details"
                .to_string(),
            MockResponse::json(ok(json!({
                "messageId": "9000000019029",
                "folderId": "9000000000905",
                "subject": "Project update",
                "fromAddress": "sender@example.test",
                "summary": "Latest status"
            }))),
        ),
        (
            "GET /api/accounts/123/folders/9000000000905/messages/9000000019029/content"
                .to_string(),
            MockResponse::json(ok(json!({
                "messageId": "9000000019029",
                "content": "<p>Latest status</p>"
            }))),
        ),
    ])
    .await;

    let messages = client
        .search_messages(
            "123",
            "from:sender@example.test",
            &ZohoMailSearch::default(),
        )
        .await
        .unwrap();
    assert_eq!(messages[0].message_id, "9000000019029");

    let details = client
        .message_details("123", "9000000000905", "9000000019029")
        .await
        .unwrap();
    assert_eq!(details.summary.as_deref(), Some("Latest status"));

    let content = client
        .message_content("123", "9000000000905", "9000000019029")
        .await
        .unwrap();
    assert_eq!(content.content, "<p>Latest status</p>");

    let requests = state.requests.lock().unwrap();
    let query = requests[0].uri.query().unwrap();
    assert!(query.contains("searchKey=from%3Asender%40example.test"));
}

#[tokio::test]
async fn message_headers_uses_documented_raw_header_endpoint() {
    let (client, state, _) = mock_client([(
        "GET /api/accounts/123/folders/9000000000905/messages/9000000019029/header"
            .to_string(),
        MockResponse::json(ok(json!({
            "messageId": "9000000019029",
            "headerContent": "Message-ID: <reply@sender.example>\r\nReferences: <one@office.example> <two@office.example>\r\n"
        }))),
    )])
    .await;

    let headers = client
        .message_headers("123", "9000000000905", "9000000019029")
        .await
        .unwrap();

    assert!(
        headers
            .header_content
            .contains("Message-ID: <reply@sender.example>")
    );
    let requests = state.requests.lock().unwrap();
    assert_eq!(
        requests[0].uri.path(),
        "/api/accounts/123/folders/9000000000905/messages/9000000019029/header"
    );
    assert!(requests[0].uri.query().unwrap().contains("raw=true"));
}

#[tokio::test]
async fn send_uses_official_payload_and_returns_provider_message_id() {
    let (client, state, _) = mock_client([(
        "POST /api/accounts/123/messages".to_string(),
        MockResponse::json(ok(
            json!({ "messageId": "9000000099", "mailId": "<opaque@zoho>" }),
        )),
    )])
    .await;

    let outcome = client.send_message("123", &compose()).await.unwrap();
    assert!(matches!(
        outcome,
        SendOutcome::Accepted { ref provider_message_id }
            if provider_message_id == "9000000099"
    ));

    let requests = state.requests.lock().unwrap();
    let body: Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(body["fromAddress"], "sender@example.test");
    assert_eq!(body["toAddress"], "recipient@example.test");
    assert_eq!(body["subject"], "Hello");
    assert_eq!(body["content"], "Body");
    assert_eq!(body["mailFormat"], "html");
    assert_eq!(
        requests[0].headers.get("authorization").unwrap(),
        "Zoho-oauthtoken unit-test-access-token"
    );
}

#[tokio::test]
async fn send_success_without_numeric_provider_message_id_is_uncertain() {
    let (client, _, _) = mock_client([(
        "POST /api/accounts/123/messages".to_string(),
        MockResponse::json(ok(json!({ "messageId": "not-a-provider-id" }))),
    )])
    .await;

    let outcome = client.send_message("123", &compose()).await.unwrap();
    assert!(matches!(outcome, SendOutcome::Unknown { .. }));
}

#[tokio::test]
async fn reply_uses_documented_provider_message_id_route_and_action() {
    let (client, state, _) = mock_client([(
        "POST /api/accounts/123/messages/9000000019029".to_string(),
        MockResponse::json(ok(json!({ "messageId": "9000000100" }))),
    )])
    .await;

    let result = client
        .reply_to_message("123", "9000000019029", &compose())
        .await
        .unwrap();
    assert!(matches!(result, SendOutcome::Accepted { .. }));

    let requests = state.requests.lock().unwrap();
    assert_eq!(
        requests[0].uri.path(),
        "/api/accounts/123/messages/9000000019029"
    );
    let body: Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(body["action"], "reply");
    assert_eq!(body["toAddress"], "recipient@example.test");
}

#[tokio::test]
async fn saving_draft_and_template_uses_mode_and_reply_headers_only_when_supplied() {
    let response = MockResponse::json(ok(json!({ "messageId": "777" })));
    let (client, state, _) = mock_client([(
        "POST /api/accounts/123/messages".to_string(),
        response.clone(),
    )])
    .await;

    client
        .save_draft("123", &compose(), None, None)
        .await
        .unwrap();
    client.save_template("123", &compose()).await.unwrap();

    let requests = state.requests.lock().unwrap();
    let draft: Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(draft["mode"], "draft");
    assert!(draft.get("inReplyTo").is_none());
    let template: Value = serde_json::from_slice(&requests[1].body).unwrap();
    assert_eq!(template["mode"], "template");
}

#[tokio::test]
async fn business_auth_and_rate_limit_errors_are_classified_without_body_leaks() {
    let private_error = "body contains address@example.test and private details";
    let (client, _, _) = mock_client([
        (
            "GET /api/accounts".to_string(),
            MockResponse::json(json!({
                "status": { "code": 400, "description": "Invalid Input" },
                "data": { "moreInfo": private_error }
            })),
        ),
        (
            "GET /api/accounts/401".to_string(),
            MockResponse::json(json!({
                "status": { "code": 401, "description": "Unauthorized" },
                "data": { "moreInfo": private_error }
            })),
        ),
        (
            "GET /api/accounts/429".to_string(),
            MockResponse::status(StatusCode::TOO_MANY_REQUESTS, private_error),
        ),
    ])
    .await;

    let business = expect_error(client.list_accounts().await);
    assert!(matches!(
        business,
        ZohoMailClientError::Business { code: Some(400) }
    ));
    assert!(!format!("{business:?}").contains(private_error));

    let auth = expect_error(client.get_account("401").await);
    assert!(matches!(auth, ZohoMailClientError::ReauthRequired));

    let rate_limit = expect_error(client.get_account("429").await);
    assert!(matches!(
        rate_limit,
        ZohoMailClientError::RateLimited { .. }
    ));
}

#[tokio::test]
async fn ambiguous_send_timeout_or_server_failure_returns_unknown_without_retry() {
    let mut delayed = MockResponse::json(ok(json!({ "messageId": "possibly-accepted" })));
    delayed.delay = Duration::from_millis(250);
    let (client, state, _) =
        mock_client([("POST /api/accounts/123/messages".to_string(), delayed)]).await;

    let outcome = client.send_message("123", &compose()).await.unwrap();
    assert!(matches!(outcome, SendOutcome::Unknown { .. }));
    assert_eq!(state.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn server_error_send_is_unknown_and_never_retried() {
    let (client, state, _) = mock_client([(
        "POST /api/accounts/123/messages".to_string(),
        MockResponse::status(StatusCode::INTERNAL_SERVER_ERROR, "private provider body"),
    )])
    .await;

    let outcome = client.send_message("123", &compose()).await.unwrap();
    assert!(matches!(outcome, SendOutcome::Unknown { .. }));
    assert_eq!(state.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn request_timeout_response_is_uncertain_and_never_retried() {
    let (client, state, _) = mock_client([(
        "POST /api/accounts/123/messages".to_string(),
        MockResponse::status(StatusCode::REQUEST_TIMEOUT, "provider timed out"),
    )])
    .await;

    let outcome = client.send_message("123", &compose()).await.unwrap();
    assert!(matches!(outcome, SendOutcome::Unknown { .. }));
    assert_eq!(state.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn rejects_path_injection_and_non_loopback_test_origins() {
    let (client, state, _) = mock_client([]).await;
    let error = expect_error(client.list_folders("123/../../accounts").await);
    assert!(matches!(error, ZohoMailClientError::InvalidId { .. }));
    assert!(state.requests.lock().unwrap().is_empty());

    let external =
        ZohoMailClient::with_base_url_for_testing(SecretValue::new("token"), "http://example.com/");
    assert!(matches!(
        external,
        Err(ZohoMailClientError::UnsafeTestOrigin)
    ));
}

#[tokio::test]
async fn refuses_redirects_and_oversized_json_responses() {
    let mut redirect = MockResponse::status(StatusCode::FOUND, "");
    redirect
        .headers
        .insert("location", "/api/accounts/redirected".parse().unwrap());
    let oversized = " ".repeat(1024 * 1024 + 1);
    let (client, state, _) = mock_client([
        ("GET /api/accounts".to_string(), redirect),
        (
            "GET /api/accounts/123".to_string(),
            MockResponse::status(StatusCode::OK, oversized),
        ),
    ])
    .await;

    let redirect_error = expect_error(client.list_accounts().await);
    assert!(matches!(
        redirect_error,
        ZohoMailClientError::HttpStatus { status: 302 }
    ));
    assert_eq!(state.requests.lock().unwrap().len(), 1);

    let oversized_error = expect_error(client.get_account("123").await);
    assert!(matches!(
        oversized_error,
        ZohoMailClientError::ResponseTooLarge
    ));
}

#[tokio::test]
async fn reads_documented_attachment_content_route_and_keeps_binary_bytes() {
    let payload = vec![0, 1, 2, 255];
    let (client, state, _) = mock_client([(
        "GET /api/accounts/123/folders/456/messages/789/attachments/42".to_string(),
        MockResponse::status(StatusCode::OK, payload.clone()),
    )])
    .await;

    let bytes = client
        .attachment_content("123", "456", "789", "42")
        .await
        .unwrap();
    assert_eq!(bytes, payload);
    assert_eq!(
        state.requests.lock().unwrap()[0].uri.path(),
        "/api/accounts/123/folders/456/messages/789/attachments/42"
    );
    assert_eq!(
        state.requests.lock().unwrap()[0]
            .headers
            .get_all("accept")
            .iter()
            .map(|value| value.to_str().unwrap())
            .collect::<Vec<_>>(),
        ["application/octet-stream"]
    );
}

#[tokio::test]
async fn attachment_content_type_keeps_json_files_as_binary_data() {
    let payload = br#"{"file":"keep these bytes"}"#.to_vec();
    let mut response = MockResponse::status(StatusCode::OK, payload.clone());
    response
        .headers
        .insert("content-type", "application/octet-stream".parse().unwrap());
    let (client, _, _) = mock_client([(
        "GET /api/accounts/123/folders/456/messages/789/attachments/42".to_string(),
        response,
    )])
    .await;

    let bytes = client
        .attachment_content("123", "456", "789", "42")
        .await
        .unwrap();
    assert_eq!(bytes, payload);
}

#[tokio::test]
async fn attachment_metadata_normalizes_ids_sizes_and_include_inline_query() {
    let (client, state, _) = mock_client([(
        "GET /api/accounts/123/folders/456/messages/789/attachmentinfo".to_string(),
        MockResponse::json(ok(json!({
            "messageId": 789,
            "attachments": [{
                "attachmentId": 42,
                "attachmentName": "report.pdf",
                "attachmentSize": 2048
            }],
            "inline": [{
                "attachmentId": "43",
                "attachmentName": "logo.png",
                "attachmentSize": "512",
                "cid": "logo@example.test"
            }]
        }))),
    )])
    .await;

    let info = client
        .attachment_info("123", "456", "789", true)
        .await
        .unwrap();
    assert_eq!(info.message_id.as_deref(), Some("789"));
    assert_eq!(info.attachments[0].attachment_id, "42");
    assert_eq!(info.attachments[0].attachment_size.as_deref(), Some("2048"));
    assert_eq!(info.inline[0].attachment_id, "43");
    assert_eq!(info.inline[0].cid.as_deref(), Some("logo@example.test"));
    assert!(
        state.requests.lock().unwrap()[0]
            .uri
            .query()
            .unwrap()
            .contains("includeInline=true")
    );
}
