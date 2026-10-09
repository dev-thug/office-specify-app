use axum::{
    Router,
    body::{Body, Bytes},
    extract::{OriginalUri, State},
    http::{HeaderMap, Method, StatusCode},
    response::Response,
    routing::any,
};
use infrastructure::providers::zoho_campaigns::{
    SecretValue,
    operations::{
        CampaignCloneRequest, CampaignContactStatus, CampaignCreateRequest,
        CampaignCustomFieldType, CampaignMutationOutcome, CampaignRecipientAction,
        CampaignSignupForm, CampaignsOperations, CampaignsOperationsError, CampaignsSortOrder,
        ContactInput, CreateCampaignCustomFieldRequest, CreateMailingListRequest,
        SubscriptionStatus,
    },
};
use serde_json::{Value, json};
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
    let mut response = Response::builder()
        .status(reply.status)
        .header("content-type", "application/json");
    if let Some(location) = reply.location {
        response = response.header("location", location);
    }
    response.body(Body::from(reply.body)).unwrap()
}

async fn start_client(
    replies: Vec<MockReply>,
    timeout: Duration,
) -> (
    CampaignsOperations,
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
    let client = CampaignsOperations::for_test_with_timeout(&base_url, timeout).unwrap();
    (client, requests, server)
}

fn form_fields(body: &[u8]) -> std::collections::HashMap<String, String> {
    let url = reqwest::Url::parse(&format!(
        "http://example.test/?{}",
        String::from_utf8_lossy(body)
    ))
    .unwrap();
    url.query_pairs().into_owned().collect()
}

fn sample_contact() -> ContactInput {
    ContactInput {
        email: "member@example.test".into(),
        fields: [
            ("First Name".to_owned(), "민지".to_owned()),
            ("Last Name".to_owned(), "김".to_owned()),
        ]
        .into_iter()
        .collect(),
    }
}

#[tokio::test]
async fn list_and_field_reads_use_official_paths_pagination_and_success_envelopes() {
    let (client, captured, server) = start_client(
        vec![
            MockReply::json(
                StatusCode::OK,
                r#"{"requestdetails":{"fromindex":1,"range":50},"list_of_details":[{"listkey":"list-1","listname":"News","noofcontacts":"8"}],"code":"0"}"#,
            ),
            MockReply::json(
                StatusCode::OK,
                r#"{"requestdetails":{"fromindex":1,"range":10},"list_of_details":[{"contact_email":"member@example.test","firstname":"Minji","zuid":"55"}],"code":0}"#,
            ),
            MockReply::json(
                StatusCode::OK,
                r#"{"response":{"message":"success","code":"0","fieldname":[{"DISPLAY_NAME":"First Name","FIELD_NAME":"firstname","FIELD_ID":"4001"}]}}"#,
            ),
            MockReply::json(
                StatusCode::OK,
                r#"{"topicDetails":[{"topicId":"topic-1","primaryList":"list-1","topicName":"Newsletter"}],"code":"200","message":"Success"}"#,
            ),
        ],
        Duration::from_secs(2),
    )
    .await;
    let access = SecretValue::new("opaque-access-token");

    let lists = client
        .get_mailing_lists(&access, 1, 50, CampaignsSortOrder::Desc)
        .await
        .unwrap();
    assert_eq!(lists.entries[0].list_key, "list-1");
    assert_eq!(lists.entries[0].name, "News");
    assert_eq!(lists.entries[0].contact_count, Some(8));
    let contacts = client
        .get_list_contacts(
            &access,
            "list-1",
            1,
            10,
            Some(CampaignContactStatus::Active),
        )
        .await
        .unwrap();
    assert_eq!(contacts.entries[0].email, "member@example.test");
    assert_eq!(contacts.entries[0].fields["firstname"], "Minji");
    let fields = client.get_contact_fields(&access).await.unwrap();
    assert_eq!(fields[0].display_name, "First Name");
    assert_eq!(fields[0].field_name, "firstname");
    assert_eq!(fields[0].field_id, "4001");
    let topics = client.get_topics(&access, 0, 10).await.unwrap();
    assert_eq!(topics[0].id, "topic-1");
    assert_eq!(topics[0].name, "Newsletter");
    assert_eq!(topics[0].primary_list.as_deref(), Some("list-1"));

    let requests = captured.lock().unwrap();
    assert_eq!(requests.len(), 4);
    assert_eq!(requests[0].method, Method::GET);
    assert!(
        requests[0]
            .path_and_query
            .contains("/api/v1.1/getmailinglists?")
    );
    assert!(requests[0].path_and_query.contains("resfmt=JSON"));
    assert!(requests[0].path_and_query.contains("fromindex=1"));
    assert!(requests[0].path_and_query.contains("range=50"));
    assert!(requests[1].path_and_query.contains("/getlistsubscribers?"));
    assert!(requests[1].path_and_query.contains("status=active"));
    assert!(
        requests[2]
            .path_and_query
            .contains("/contact/allfields?type=json")
    );
    assert!(requests[3].path_and_query.contains("/topics?details="));
    let topics_url = reqwest::Url::parse(&format!(
        "http://example.test{}",
        requests[3].path_and_query
    ))
    .unwrap();
    assert_eq!(
        topics_url
            .query_pairs()
            .find(|(key, _)| key == "details")
            .unwrap()
            .1,
        "{from_index:0,range:10}"
    );
    for request in requests.iter() {
        assert_eq!(
            request.authorization.as_deref(),
            Some("Zoho-oauthtoken opaque-access-token")
        );
    }
    server.abort();
}

#[tokio::test]
async fn subscribe_unsubscribe_and_do_not_mail_encode_json_contactinfo_as_forms() {
    let (client, captured, server) = start_client(
        vec![
            MockReply::json(
                StatusCode::OK,
                r#"{"message":"A confirmation email is sent to the user. User needs to confirm to successfully subscribe.","status":"success","code":"0"}"#,
            ),
            MockReply::json(
                StatusCode::OK,
                r#"{"message":"success","status":"success","code":"0"}"#,
            ),
            MockReply::json(
                StatusCode::OK,
                r#"{"message":"User successfully unsubscribed.","status":"success","code":0}"#,
            ),
            MockReply::json(
                StatusCode::OK,
                r#"{"message":"User successfully moved to Do-Not-Mail.","status":"success","code":"0"}"#,
            ),
        ],
        Duration::from_secs(2),
    )
    .await;
    let access = SecretValue::new("opaque-access-token");

    let subscribed = client
        .subscribe_to_list(&access, "list-1", sample_contact(), Some("topic-1"))
        .await
        .unwrap();
    assert!(matches!(
        subscribed,
        CampaignMutationOutcome::Accepted(SubscriptionStatus::AcceptedUnverified)
    ));
    assert!(matches!(
        client
            .subscribe_to_list(&access, "list-1", sample_contact(), None)
            .await
            .unwrap(),
        CampaignMutationOutcome::Accepted(SubscriptionStatus::AcceptedUnverified)
    ));
    assert!(matches!(
        client
            .unsubscribe_from_list(&access, "list-1", "member@example.test", Some("topic-1"))
            .await
            .unwrap(),
        CampaignMutationOutcome::Accepted(())
    ));
    assert!(matches!(
        client
            .move_to_do_not_mail(&access, "member@example.test")
            .await
            .unwrap(),
        CampaignMutationOutcome::Accepted(())
    ));

    let requests = captured.lock().unwrap();
    assert_eq!(requests.len(), 4);
    assert_eq!(requests[0].method, Method::POST);
    assert_eq!(requests[0].path_and_query, "/api/v1.1/json/listsubscribe");
    assert_eq!(
        requests[0].content_type.as_deref(),
        Some("application/x-www-form-urlencoded")
    );
    let subscribe_form = form_fields(&requests[0].body);
    let contact: Value = serde_json::from_str(&subscribe_form["contactinfo"]).unwrap();
    assert_eq!(subscribe_form["resfmt"], "JSON");
    assert_eq!(subscribe_form["listkey"], "list-1");
    assert_eq!(subscribe_form["topic_id"], "topic-1");
    assert_eq!(contact["Contact Email"], "member@example.test");
    assert_eq!(contact["First Name"], "민지");
    let subscribe_without_topic = form_fields(&requests[1].body);
    assert!(!subscribe_without_topic.contains_key("topic_id"));
    assert_eq!(requests[1].path_and_query, "/api/v1.1/json/listsubscribe");
    assert_eq!(form_fields(&requests[2].body)["topic_id"], "topic-1");
    assert_eq!(requests[2].path_and_query, "/api/v1.1/json/listunsubscribe");
    assert_eq!(
        requests[3].path_and_query,
        "/api/v1.1/json/contactdonotmail"
    );
    assert_eq!(
        serde_json::from_str::<Value>(&form_fields(&requests[3].body)["contactinfo"]).unwrap()["Contact Email"],
        "member@example.test"
    );
    server.abort();
}

#[tokio::test]
async fn campaign_create_clone_and_send_use_documented_forms_and_codes() {
    let (client, captured, server) = start_client(
        vec![
            MockReply::json(
                StatusCode::OK,
                r#"{"message":"Campaign created successfully","campaignKey":"campaign-1","code":"200"}"#,
            ),
            MockReply::json(
                StatusCode::OK,
                r#"{"message":"Campaign created successfully","campaignKey":"campaign-2","code":200}"#,
            ),
            MockReply::json(
                StatusCode::OK,
                r#"{"status":"success","code":0,"campaign_status":"Draft"}"#,
            ),
            MockReply::json(
                StatusCode::OK,
                r#"{"response":{"uri":"/api/v1.1/sendcampaign","code":"200","campaign_status":"inprogress"}}"#,
            ),
        ],
        Duration::from_secs(2),
    )
    .await;
    let access = SecretValue::new("opaque-access-token");
    let created = client
        .create_campaign(
            &access,
            CampaignCreateRequest {
                name: "Newsletter 10월".into(),
                subject: "이번 달 소식".into(),
                from_email: "news@example.test".into(),
                from_name: "Office".into(),
                list_key: "list-1".into(),
                topic_id: Some("topic-1".into()),
                content_url: "https://assets.example.test/campaigns/v42.html".into(),
            },
        )
        .await
        .unwrap();
    assert!(matches!(
        created,
        CampaignMutationOutcome::Accepted(ref value) if value.campaign_key == "campaign-1"
    ));
    let created_without_topic = client
        .create_campaign(
            &access,
            CampaignCreateRequest {
                name: "Newsletter legacy".into(),
                subject: "This month".into(),
                from_email: "news@example.test".into(),
                from_name: "Office".into(),
                list_key: "list-1".into(),
                topic_id: None,
                content_url: "https://assets.example.test/campaigns/v42.html".into(),
            },
        )
        .await
        .unwrap();
    assert!(matches!(
        created_without_topic,
        CampaignMutationOutcome::Accepted(ref value) if value.campaign_key == "campaign-2"
    ));
    let cloned = client
        .clone_campaign(
            &access,
            CampaignCloneRequest {
                old_campaign_key: "campaign-1".into(),
                name: "Newsletter copy".into(),
                subject: "Copy".into(),
                from_email: "news@example.test".into(),
                from_name: "Office".into(),
                reply_to: Some("support@example.test".into()),
            },
        )
        .await
        .unwrap();
    assert!(matches!(
        cloned,
        CampaignMutationOutcome::Accepted(ref value) if value.status.as_deref() == Some("Draft")
    ));
    let sent = client.send_campaign(&access, "campaign-1").await.unwrap();
    assert!(matches!(
        sent,
        CampaignMutationOutcome::Accepted(ref value) if value.status == "inprogress"
    ));

    let requests = captured.lock().unwrap();
    assert_eq!(requests.len(), 4);
    assert_eq!(requests[0].method, Method::POST);
    assert_eq!(requests[0].path_and_query, "/api/v1.1/createCampaign");
    let create_form = form_fields(&requests[0].body);
    assert_eq!(create_form["resfmt"], "JSON");
    assert_eq!(create_form["campaignname"], "Newsletter 10월");
    assert_eq!(create_form["subject"], "이번 달 소식");
    assert_eq!(create_form["topicId"], "topic-1");
    assert_eq!(
        serde_json::from_str::<Value>(&create_form["list_details"]).unwrap(),
        json!({"list-1": []})
    );
    let create_without_topic = form_fields(&requests[1].body);
    assert!(!create_without_topic.contains_key("topicId"));
    assert_eq!(requests[2].path_and_query, "/api/v1.1/json/clonecampaign");
    let clone_form = form_fields(&requests[2].body);
    let campaign_info: Value = serde_json::from_str(&clone_form["campaigninfo"]).unwrap();
    assert_eq!(campaign_info["oldcampaignkey"], "campaign-1");
    assert_eq!(campaign_info["campaignname"], "Newsletter copy");
    assert_eq!(campaign_info["encode_type"], "UTF-8");
    assert_eq!(requests[3].path_and_query, "/api/v1.1/sendcampaign");
    assert_eq!(form_fields(&requests[3].body)["campaignkey"], "campaign-1");
    server.abort();
}

#[tokio::test]
async fn aggregate_and_recipient_reports_remain_distinct_and_report_actions_are_closed() {
    let (client, captured, server) = start_client(
        vec![
            MockReply::json(
                StatusCode::OK,
                r#"{"status":"success","code":"0","campaign-reports":[{"emails_sent_count":"12","opens_count":"7"}],"campaign-details":[{"campaign_key":"campaign-1","campaign_name":"Monthly"}],"campaign-reach":[{"total":"12","emails":"12"}],"campaign-by-loaction":"{us=12}"}"#,
            ),
            MockReply::json(
                StatusCode::OK,
                r#"{"requestdetails":{"range":20,"fromindex":1,"action":"openedcontacts"},"status":"success","list_of_details":[{"contactemailaddress":"member@example.test","contactid":"contact-1","sent_time":"1728472166000"}],"code":0}"#,
            ),
        ],
        Duration::from_secs(2),
    )
    .await;
    let access = SecretValue::new("opaque-access-token");
    let report = client
        .get_campaign_report(&access, "campaign-1")
        .await
        .unwrap();
    assert_eq!(report.metrics["emails_sent_count"], "12");
    assert_eq!(report.details["campaign_key"], "campaign-1");
    assert_eq!(report.reach["total"], "12");
    assert_eq!(
        report.by_location.as_ref().and_then(Value::as_str),
        Some("{us=12}")
    );
    let recipients = client
        .get_campaign_recipients_data(
            &access,
            "campaign-1",
            CampaignRecipientAction::OpenedContacts,
        )
        .await
        .unwrap();
    assert_eq!(recipients.action, CampaignRecipientAction::OpenedContacts);
    assert_eq!(recipients.from_index, 1);
    assert_eq!(recipients.range, 20);
    assert_eq!(recipients.contacts[0].email, "member@example.test");
    assert_eq!(
        recipients.contacts[0].contact_id.as_deref(),
        Some("contact-1")
    );
    assert_eq!(recipients.contacts[0].sent_time_ms, Some(1_728_472_166_000));

    let requests = captured.lock().unwrap();
    assert_eq!(requests[0].method, Method::GET);
    assert_eq!(
        requests[0].path_and_query,
        "/api/v1.1/campaignreports?resfmt=JSON&campaignkey=campaign-1"
    );
    assert_eq!(requests[1].method, Method::POST);
    assert_eq!(
        requests[1].path_and_query,
        "/api/v1.1/getcampaignrecipientsdata"
    );
    let recipient_form = form_fields(&requests[1].body);
    assert_eq!(recipient_form["resfmt"], "JSON");
    assert_eq!(recipient_form["campaignkey"], "campaign-1");
    assert_eq!(recipient_form["action"], "openedcontacts");
    assert!(!recipient_form.contains_key("fromindex"));
    server.abort();
}

#[tokio::test]
async fn ambiguous_mutation_responses_are_unknown_and_provider_errors_are_scrubbed() {
    let mut redirect = MockReply::json(StatusCode::FOUND, "{}");
    redirect.location = Some("https://attacker.example/redirect");
    let mut delayed = MockReply::json(
        StatusCode::OK,
        r#"{"campaignKey":"late","code":"200","message":"success"}"#,
    );
    delayed.delay = Some(Duration::from_millis(100));
    let (client, captured, server) = start_client(
        vec![
            MockReply::json(
                StatusCode::INTERNAL_SERVER_ERROR,
                "private@example.test raw detail",
            ),
            redirect,
            MockReply::json(StatusCode::OK, "not-json"),
            delayed,
            MockReply::json(
                StatusCode::OK,
                r#"{"code":"6611","message":"private@example.test was rejected"}"#,
            ),
            MockReply::json(StatusCode::UNAUTHORIZED, "{}"),
            MockReply::json(StatusCode::TOO_MANY_REQUESTS, "{}"),
        ],
        Duration::from_millis(25),
    )
    .await;
    let access = SecretValue::new("opaque-access-token");
    for _ in 0..4 {
        assert!(matches!(
            client.send_campaign(&access, "campaign-1").await.unwrap(),
            CampaignMutationOutcome::Unknown
        ));
    }
    let rejected = client.send_campaign(&access, "campaign-1").await.unwrap();
    assert!(matches!(
        rejected,
        CampaignMutationOutcome::Rejected { ref code } if code == "6611"
    ));
    assert!(!format!("{rejected:?}").contains("private@example.test"));
    assert!(matches!(
        client.send_campaign(&access, "campaign-1").await,
        Err(CampaignsOperationsError::ReauthRequired)
    ));
    assert!(matches!(
        client.send_campaign(&access, "campaign-1").await,
        Err(CampaignsOperationsError::RateLimited { .. })
    ));
    assert_eq!(captured.lock().unwrap().len(), 7);
    server.abort();
}

#[tokio::test]
async fn validation_blocks_unbounded_pages_invalid_content_origins_and_unknown_actions() {
    let (client, captured, server) = start_client(vec![], Duration::from_secs(2)).await;
    let access = SecretValue::new("opaque-access-token");
    assert!(matches!(
        client
            .get_mailing_lists(&access, 0, 50, CampaignsSortOrder::Asc)
            .await,
        Err(CampaignsOperationsError::InvalidRequest)
    ));
    assert!(matches!(
        client
            .get_list_contacts(&access, "list-1", 1, 101, None)
            .await,
        Err(CampaignsOperationsError::InvalidRequest)
    ));
    let invalid = client
        .create_campaign(
            &access,
            CampaignCreateRequest {
                name: "News".into(),
                subject: "Subject".into(),
                from_email: "news@example.test".into(),
                from_name: "Office".into(),
                list_key: "list-1".into(),
                topic_id: Some("topic-1".into()),
                content_url: "http://assets.example.test/campaign.html".into(),
            },
        )
        .await;
    assert!(matches!(
        invalid,
        Err(CampaignsOperationsError::InvalidRequest)
    ));
    assert!(captured.lock().unwrap().is_empty());
    server.abort();
}

#[tokio::test]
async fn form_requests_and_json_responses_are_capped_at_one_mib() {
    let mut oversized = MockReply::json(StatusCode::OK, "{}");
    oversized.body = vec![b'x'; 1_048_577];
    let (client, captured, server) = start_client(
        vec![oversized, MockReply::json(StatusCode::OK, "not-json")],
        Duration::from_secs(2),
    )
    .await;
    let access = SecretValue::new("opaque-access-token");
    assert!(matches!(
        client
            .get_mailing_lists(&access, 1, 10, CampaignsSortOrder::Asc)
            .await,
        Err(CampaignsOperationsError::ResponseTooLarge)
    ));
    assert!(matches!(
        client
            .get_mailing_lists(&access, 1, 10, CampaignsSortOrder::Asc)
            .await,
        Err(CampaignsOperationsError::InvalidResponse)
    ));

    let fields = (0..140)
        .map(|index| (format!("custom_{index}"), "x".repeat(10_000)))
        .collect();
    let oversized_contact = ContactInput {
        email: "member@example.test".into(),
        fields,
    };
    assert!(matches!(
        client
            .subscribe_to_list(&access, "list-1", oversized_contact, None)
            .await,
        Err(CampaignsOperationsError::RequestTooLarge)
    ));
    assert_eq!(captured.lock().unwrap().len(), 2);
    server.abort();
}

#[tokio::test]
async fn create_list_with_contacts_enforces_the_documented_ten_email_limit() {
    let (client, captured, server) = start_client(
        vec![MockReply::json(
            StatusCode::OK,
            r#"{"status":"success","listkey":"new-list-1","listname":"Native Newsletter","code":"0"}"#,
        )],
        Duration::from_secs(2),
    )
    .await;
    let access = SecretValue::new("opaque-access-token");
    let request = CreateMailingListRequest {
        list_name: "Native Newsletter".into(),
        email_addresses: vec!["one@example.test".into(), "two@example.test".into()],
        signup_form: CampaignSignupForm::Private,
        description: Some("Office consented audience".into()),
    };
    let created = client
        .create_list_with_contacts(&access, request)
        .await
        .unwrap();
    assert!(matches!(
        created,
        CampaignMutationOutcome::Accepted(ref list)
            if list.list_key == "new-list-1" && list.list_name == "Native Newsletter"
    ));
    let too_many = CreateMailingListRequest {
        list_name: "Too many".into(),
        email_addresses: (0..11)
            .map(|index| format!("member{index}@example.test"))
            .collect(),
        signup_form: CampaignSignupForm::Private,
        description: None,
    };
    assert!(matches!(
        client.create_list_with_contacts(&access, too_many).await,
        Err(CampaignsOperationsError::InvalidRequest)
    ));

    let requests = captured.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, Method::POST);
    assert_eq!(requests[0].path_and_query, "/api/v1.1/addlistandcontacts");
    let form = form_fields(&requests[0].body);
    assert_eq!(form["resfmt"], "JSON");
    assert_eq!(form["mode"], "newlist");
    assert_eq!(form["signupform"], "private");
    assert_eq!(form["listname"], "Native Newsletter");
    assert_eq!(form["listdescription"], "Office consented audience");
    assert_eq!(form["emailids"], "one@example.test,two@example.test");
    server.abort();
}

#[tokio::test]
async fn bulk_add_to_existing_list_uses_official_route_and_ten_email_limit() {
    let (client, captured, server) = start_client(
        vec![MockReply::json(
            StatusCode::OK,
            r#"{"status":"success","listkey":"list-1","code":0}"#,
        )],
        Duration::from_secs(2),
    )
    .await;
    let access = SecretValue::new("opaque-access-token");
    let email_addresses = (0..10)
        .map(|index| format!("member{index}@example.test"))
        .collect::<Vec<_>>();
    let result = client
        .add_contacts_to_existing_list(&access, "list-1", &email_addresses)
        .await
        .unwrap();
    assert!(matches!(result, CampaignMutationOutcome::Accepted(())));
    let too_many = (0..11)
        .map(|index| format!("member{index}@example.test"))
        .collect::<Vec<_>>();
    assert!(matches!(
        client
            .add_contacts_to_existing_list(&access, "list-1", &too_many)
            .await,
        Err(CampaignsOperationsError::InvalidRequest)
    ));
    let requests = captured.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0].path_and_query,
        "/api/v1.1/addlistsubscribersinbulk"
    );
    let form = form_fields(&requests[0].body);
    assert_eq!(form["listkey"], "list-1");
    assert_eq!(form["resfmt"], "JSON");
    assert_eq!(form["emailids"], email_addresses.join(","));
    server.abort();
}

#[tokio::test]
async fn full_list_contact_reads_page_through_active_unsubscribed_and_bounced() {
    let first_page = json!({
        "status": "success",
        "code": "0",
        "requestdetails": {"fromindex": 1, "range": 100},
        "list_of_details": (0..100)
            .map(|index| json!({
                "contact_email": format!("member{index}@example.test"),
                "contactstatus": "active",
                "firstname": format!("Member {index}")
            }))
            .collect::<Vec<_>>()
    });
    let second_page = json!({
        "status": "success",
        "code": 0,
        "requestdetails": {"fromindex": 101, "range": 100},
        "list_of_details": [{
            "contact_email": "member100@example.test",
            "contactstatus": "active"
        }]
    });
    let empty_page = |status: &str| {
        MockReply::json(
            StatusCode::OK,
            &json!({
                "status": "success",
                "code": 0,
                "requestdetails": {"fromindex": 1, "range": 100, "status": status},
                "list_of_details": []
            })
            .to_string(),
        )
    };
    let (client, captured, server) = start_client(
        vec![
            MockReply::json(StatusCode::OK, &first_page.to_string()),
            MockReply::json(StatusCode::OK, &second_page.to_string()),
            empty_page("unsub"),
            empty_page("bounce"),
        ],
        Duration::from_secs(2),
    )
    .await;
    let access = SecretValue::new("opaque-access-token");
    let active = client
        .get_all_list_contacts(&access, "list-1", CampaignContactStatus::Active)
        .await
        .unwrap();
    assert_eq!(active.len(), 101);
    assert_eq!(active[100].email, "member100@example.test");
    let unsubscribed = client
        .get_all_list_contacts(&access, "list-1", CampaignContactStatus::Unsubscribed)
        .await
        .unwrap();
    let bounced = client
        .get_all_list_contacts(&access, "list-1", CampaignContactStatus::Bounced)
        .await
        .unwrap();
    assert!(unsubscribed.is_empty());
    assert!(bounced.is_empty());

    let requests = captured.lock().unwrap();
    assert_eq!(requests.len(), 4);
    assert!(requests[0].path_and_query.contains("fromindex=1"));
    assert!(requests[0].path_and_query.contains("status=active"));
    assert!(requests[1].path_and_query.contains("fromindex=101"));
    assert!(requests[1].path_and_query.contains("status=active"));
    assert!(requests[2].path_and_query.contains("status=unsub"));
    assert!(requests[3].path_and_query.contains("status=bounce"));
    server.abort();
}

#[tokio::test]
async fn full_list_contact_reads_fail_on_repeated_pages_instead_of_returning_partial_data() {
    let rows = (0..100)
        .map(|index| {
            json!({
                "contact_email": format!("member{index}@example.test"),
                "contactstatus": "active"
            })
        })
        .collect::<Vec<_>>();
    let repeated_page = |from_index| {
        MockReply::json(
            StatusCode::OK,
            &json!({
                "status": "success",
                "code": "0",
                "requestdetails": {"fromindex": from_index, "range": 100},
                "list_of_details": rows.clone()
            })
            .to_string(),
        )
    };
    let (client, captured, server) = start_client(
        vec![repeated_page(1), repeated_page(101)],
        Duration::from_secs(2),
    )
    .await;
    let error = client
        .get_all_list_contacts(
            &SecretValue::new("opaque-access-token"),
            "list-1",
            CampaignContactStatus::Active,
        )
        .await
        .unwrap_err();
    assert_eq!(error, CampaignsOperationsError::PaginationLimitExceeded);
    assert_eq!(captured.lock().unwrap().len(), 2);
    server.abort();
}

#[tokio::test]
async fn documented_custom_field_create_uses_contact_create_contract() {
    let (client, captured, server) = start_client(
        vec![MockReply::json(
            StatusCode::OK,
            r#"{"response":{"message":"Success","code":"200","fieldname":"CustomerStage","fieldtype":"Picklist","fieldlength":"30"}}"#,
        )],
        Duration::from_secs(2),
    )
    .await;
    let access = SecretValue::new("opaque-access-token");
    let field = client
        .create_custom_field(
            &access,
            CreateCampaignCustomFieldRequest {
                field_name: "CustomerStage".into(),
                field_type: CampaignCustomFieldType::Picklist,
                field_length: Some(30),
            },
        )
        .await
        .unwrap();
    assert!(matches!(
        field,
        CampaignMutationOutcome::Accepted(ref value)
            if value.field_name == "CustomerStage"
                && value.field_type == CampaignCustomFieldType::Picklist
                && value.field_length == Some(30)
    ));
    let requests = captured.lock().unwrap();
    assert_eq!(requests[0].method, Method::POST);
    assert_eq!(requests[0].path_and_query, "/api/v1.1/custom/add");
    let form = form_fields(&requests[0].body);
    assert_eq!(form["type"], "json");
    assert_eq!(form["fieldname"], "CustomerStage");
    assert_eq!(form["fieldtype"], "Picklist");
    assert_eq!(form["fieldlength"], "30");
    server.abort();
}
