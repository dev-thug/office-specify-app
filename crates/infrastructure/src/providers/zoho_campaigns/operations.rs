//! Typed operations for the regular Zoho Campaigns v1.1 API.
//!
//! This client is separate from the Email API v2 client and is fixed to the
//! US Campaigns host. It only exposes documented operations; mutating calls
//! never retry and return Unknown when delivery cannot be determined.

use super::SecretValue;
use reqwest::{
    Client, Method, Response, Url,
    header::{HeaderValue, RETRY_AFTER},
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    time::Duration,
};
use thiserror::Error;

const API_BASE: &str = "https://campaigns.zoho.com/api/v1.1/";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);
const MAX_JSON_BYTES: usize = 1024 * 1024;
const MAX_PAGE_SIZE: u32 = 100;
const MAX_FULL_LIST_PAGES: u32 = 100;
const MAX_FULL_LIST_CONTACTS: usize = MAX_PAGE_SIZE as usize * MAX_FULL_LIST_PAGES as usize;

#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum CampaignsOperationsError {
    #[error("Zoho Campaigns credentials are not configured")]
    NotConfigured,
    #[error("Zoho Campaigns client configuration is invalid")]
    InvalidConfiguration,
    #[error("invalid Zoho Campaigns operation request")]
    InvalidRequest,
    #[error("Zoho Campaigns authorization must be renewed")]
    ReauthRequired,
    #[error("Zoho Campaigns rate limited the request")]
    RateLimited { retry_after_seconds: Option<u32> },
    #[error("Zoho Campaigns request failed")]
    Transport,
    #[error("Zoho Campaigns returned HTTP {status}")]
    HttpStatus { status: u16 },
    #[error("Zoho Campaigns response is invalid")]
    InvalidResponse,
    #[error("Zoho Campaigns request exceeds the allowed size")]
    RequestTooLarge,
    #[error("Zoho Campaigns response exceeds the allowed size")]
    ResponseTooLarge,
    #[error("Zoho Campaigns contact pagination exceeded its configured limit")]
    PaginationLimitExceeded,
    #[error("Zoho Campaigns rejected the operation with code {code}")]
    Rejected { code: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CampaignMutationOutcome<T> {
    Accepted(T),
    Rejected { code: String },
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CampaignsSortOrder {
    Asc,
    Desc,
}
impl CampaignsSortOrder {
    fn as_str(self) -> &'static str {
        match self {
            Self::Asc => "asc",
            Self::Desc => "desc",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CampaignContactStatus {
    Active,
    Recent,
    MostRecent,
    Unsubscribed,
    Bounced,
}
impl CampaignContactStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Recent => "recent",
            Self::MostRecent => "mostrecent",
            Self::Unsubscribed => "unsub",
            Self::Bounced => "bounce",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CampaignsPage<T> {
    pub entries: Vec<T>,
    pub from_index: u32,
    pub range: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CampaignMailingList {
    pub list_key: String,
    pub name: String,
    pub contact_count: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CampaignContactRecord {
    pub email: String,
    pub fields: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CampaignContactField {
    pub display_name: String,
    pub field_name: String,
    pub field_id: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CampaignSignupForm {
    Public,
    Private,
}
impl CampaignSignupForm {
    fn as_str(self) -> &'static str {
        match self {
            Self::Public => "public",
            Self::Private => "private",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CreateMailingListRequest {
    pub list_name: String,
    pub email_addresses: Vec<String>,
    pub signup_form: CampaignSignupForm,
    pub description: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CreatedMailingList {
    pub list_key: String,
    pub list_name: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CampaignCustomFieldType {
    Text,
    Integer,
    Phone,
    Date,
    Picklist,
    Email,
    Checkbox,
    LongInteger,
    Url,
    Textarea,
    RadioOption,
    Multiselect,
    DateTime,
    Decimal,
    Percent,
}
impl CampaignCustomFieldType {
    fn as_str(self) -> &'static str {
        match self {
            Self::Text => "Text",
            Self::Integer => "Integer",
            Self::Phone => "Phone",
            Self::Date => "Date",
            Self::Picklist => "Picklist",
            Self::Email => "Email",
            Self::Checkbox => "Checkbox",
            Self::LongInteger => "LongInteger",
            Self::Url => "URL",
            Self::Textarea => "textarea",
            Self::RadioOption => "RadioOption",
            Self::Multiselect => "Multiselect",
            Self::DateTime => "DateTime",
            Self::Decimal => "Decimal",
            Self::Percent => "Percent",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CreateCampaignCustomFieldRequest {
    pub field_name: String,
    pub field_type: CampaignCustomFieldType,
    pub field_length: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CreatedCampaignCustomField {
    pub field_name: String,
    pub field_type: CampaignCustomFieldType,
    pub field_length: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CampaignTopic {
    pub id: String,
    pub name: String,
    pub primary_list: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContactInput {
    pub email: String,
    /// Zoho expects contact display labels such as First Name and Last Name.
    pub fields: BTreeMap<String, String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SubscriptionStatus {
    /// The API accepted the synchronization request, but did not provide a
    /// separately verified contact status.
    AcceptedUnverified,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CampaignType {
    Normal,
    AbTesting,
}
impl CampaignType {
    fn as_str(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::AbTesting => "abtesting",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CampaignDetails {
    /// Provider metadata and status only. Zoho's documented endpoint does not
    /// return the campaign HTML body.
    pub status: Option<String>,
    pub details: BTreeMap<String, Value>,
    pub associated_lists: Vec<BTreeMap<String, Value>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CampaignCreateRequest {
    pub name: String,
    pub subject: String,
    pub from_email: String,
    pub from_name: String,
    pub list_key: String,
    /// Required for organizations using the updated topic model.
    pub topic_id: Option<String>,
    pub content_url: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CampaignCloneRequest {
    pub old_campaign_key: String,
    pub name: String,
    pub subject: String,
    pub from_email: String,
    pub from_name: String,
    pub reply_to: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CreatedCampaign {
    pub campaign_key: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClonedCampaign {
    pub status: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SendCampaignAccepted {
    pub status: String,
}

/// Campaign metric fields are retained verbatim. Aggregate counts must not be
/// interpreted as individual recipient delivery or engagement events.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CampaignReport {
    pub metrics: BTreeMap<String, Value>,
    pub details: BTreeMap<String, Value>,
    pub reach: BTreeMap<String, Value>,
    pub by_location: Option<Value>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CampaignRecipientAction {
    SentContacts,
    OpenedContacts,
    OptOutContacts,
    SpamContacts,
    UnopenedContacts,
    ClickedContacts,
    SentHardBounce,
    SentSoftBounce,
    UnsentContacts,
}
impl CampaignRecipientAction {
    fn as_str(self) -> &'static str {
        match self {
            Self::SentContacts => "sentcontacts",
            Self::OpenedContacts => "openedcontacts",
            Self::OptOutContacts => "optoutcontacts",
            Self::SpamContacts => "spamcontacts",
            Self::UnopenedContacts => "unopenedcontacts",
            Self::ClickedContacts => "clickedcontacts",
            Self::SentHardBounce => "senthardbounce",
            Self::SentSoftBounce => "sentsoftbounce",
            Self::UnsentContacts => "unsentcontacts",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CampaignRecipientRecord {
    pub email: String,
    pub contact_id: Option<String>,
    /// Zoho reports the send timestamp, not necessarily the action event time.
    pub sent_time_ms: Option<i64>,
    pub fields: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CampaignRecipientPage {
    pub action: CampaignRecipientAction,
    pub from_index: u32,
    pub range: u32,
    pub contacts: Vec<CampaignRecipientRecord>,
}

#[derive(Clone)]
pub struct CampaignsOperations {
    client: Client,
    api_base: Url,
}

impl std::fmt::Debug for CampaignsOperations {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CampaignsOperations")
            .field("api_base", &self.api_base)
            .finish()
    }
}

impl CampaignsOperations {
    pub fn new() -> Result<Self, CampaignsOperationsError> {
        Self::build(API_BASE, REQUEST_TIMEOUT)
    }

    fn build(base: &str, timeout: Duration) -> Result<Self, CampaignsOperationsError> {
        if timeout.is_zero() {
            return Err(CampaignsOperationsError::InvalidConfiguration);
        }
        let api_base =
            Url::parse(base).map_err(|_| CampaignsOperationsError::InvalidConfiguration)?;
        let client = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .timeout(timeout)
            .connect_timeout(CONNECT_TIMEOUT)
            .build()
            .map_err(|_| CampaignsOperationsError::InvalidConfiguration)?;
        Ok(Self { client, api_base })
    }

    /// Test-only constructor. The test server must bind a loopback address.
    #[cfg(feature = "testing")]
    pub fn for_test(base_url: &str) -> Result<Self, CampaignsOperationsError> {
        Self::for_test_with_timeout(base_url, REQUEST_TIMEOUT)
    }

    /// Test-only constructor with a controllable request deadline.
    #[cfg(feature = "testing")]
    pub fn for_test_with_timeout(
        base_url: &str,
        timeout: Duration,
    ) -> Result<Self, CampaignsOperationsError> {
        let origin =
            Url::parse(base_url).map_err(|_| CampaignsOperationsError::InvalidConfiguration)?;
        if origin.scheme() != "http"
            || !matches!(origin.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"))
            || !origin.username().is_empty()
            || origin.password().is_some()
            || origin.query().is_some()
            || origin.fragment().is_some()
        {
            return Err(CampaignsOperationsError::InvalidConfiguration);
        }
        Self::build(
            &format!("{}/api/v1.1/", base_url.trim_end_matches('/')),
            timeout,
        )
    }

    pub async fn get_mailing_lists(
        &self,
        access: &SecretValue,
        from_index: u32,
        range: u32,
        sort: CampaignsSortOrder,
    ) -> Result<CampaignsPage<CampaignMailingList>, CampaignsOperationsError> {
        validate_page(from_index, range, false)?;
        let response = self
            .read_json(
                Method::GET,
                "getmailinglists",
                &[
                    ("resfmt", "JSON".to_owned()),
                    ("sort", sort.as_str().to_owned()),
                    ("fromindex", from_index.to_string()),
                    ("range", range.to_string()),
                ],
                access,
            )
            .await?;
        if api_code(&response).as_deref() == Some("2401") {
            return Ok(CampaignsPage {
                entries: Vec::new(),
                from_index,
                range: 0,
            });
        }
        require_read_success(&response, &["0"])?;
        let root = data_root(&response);
        let rows = array_field(root, "list_of_details")?;
        if rows.len() > range as usize {
            return Err(CampaignsOperationsError::InvalidResponse);
        }
        let entries = rows
            .iter()
            .map(|row| {
                Ok(CampaignMailingList {
                    list_key: string_field(row, "listkey")?,
                    name: string_field(row, "listname")?,
                    contact_count: optional_u64_field(row, "noofcontacts")?,
                })
            })
            .collect::<Result<Vec<_>, CampaignsOperationsError>>()?;
        let (from_index, range) = returned_page(root, from_index, range)?;
        Ok(CampaignsPage {
            entries,
            from_index,
            range,
        })
    }

    pub async fn get_list_contacts(
        &self,
        access: &SecretValue,
        list_key: &str,
        from_index: u32,
        range: u32,
        status: Option<CampaignContactStatus>,
    ) -> Result<CampaignsPage<CampaignContactRecord>, CampaignsOperationsError> {
        validate_key(list_key)?;
        validate_page(from_index, range, false)?;
        let mut params = vec![
            ("resfmt", "JSON".to_owned()),
            ("listkey", list_key.to_owned()),
            ("fromindex", from_index.to_string()),
            ("range", range.to_string()),
        ];
        if let Some(status) = status {
            params.push(("status", status.as_str().to_owned()));
        }
        let response = self
            .read_json(Method::GET, "getlistsubscribers", &params, access)
            .await?;
        let root = data_root(&response);
        // The live Korean API uses the generic 2502 code for an explicitly
        // empty filtered list. Never treat 2502 alone as complete coverage:
        // the documented generic display failure must still fail closed.
        if api_code(&response).as_deref() == Some("2502")
            && matches!(
                root.get("uri").and_then(Value::as_str),
                Some("/api/v1.1/getlistsubscribers" | "/getlistsubscribers")
            )
            && root.get("status").and_then(Value::as_str) == Some("error")
            && root.get("message").and_then(Value::as_str)
                == Some("아직은 이 목록에 연락처가 없습니다.")
            && root.get("list_of_details").is_none()
        {
            return Ok(CampaignsPage {
                entries: Vec::new(),
                from_index,
                range: 0,
            });
        }
        require_read_success(&response, &["0"])?;
        let rows = array_field(root, "list_of_details")?;
        if rows.len() > range as usize {
            return Err(CampaignsOperationsError::InvalidResponse);
        }
        let entries = rows
            .iter()
            .map(|row| {
                Ok(CampaignContactRecord {
                    email: first_string_field(row, &["contact_email", "contactemailaddress"])?,
                    fields: object_map(row),
                })
            })
            .collect::<Result<Vec<_>, CampaignsOperationsError>>()?;
        let (from_index, range) = returned_page(root, from_index, range)?;
        Ok(CampaignsPage {
            entries,
            from_index,
            range,
        })
    }

    /// Fetch the complete page sequence for one documented list-contact
    /// status, capped at 10,000 rows. A repeated/over-limit page fails rather
    /// than returning a partial audience as though it were complete.
    pub async fn get_all_list_contacts(
        &self,
        access: &SecretValue,
        list_key: &str,
        status: CampaignContactStatus,
    ) -> Result<Vec<CampaignContactRecord>, CampaignsOperationsError> {
        let mut contacts = Vec::new();
        let mut seen_emails = BTreeSet::new();
        for page_number in 0..MAX_FULL_LIST_PAGES {
            let from_index = page_number * MAX_PAGE_SIZE + 1;
            let page = self
                .get_list_contacts(access, list_key, from_index, MAX_PAGE_SIZE, Some(status))
                .await?;
            let page_len = page.entries.len();
            let previous_len = contacts.len();
            for contact in page.entries {
                if seen_emails.insert(contact.email.to_ascii_lowercase()) {
                    contacts.push(contact);
                    if contacts.len() > MAX_FULL_LIST_CONTACTS {
                        return Err(CampaignsOperationsError::PaginationLimitExceeded);
                    }
                }
            }
            if page_len < MAX_PAGE_SIZE as usize {
                return Ok(contacts);
            }
            if contacts.len() == previous_len || page_number + 1 == MAX_FULL_LIST_PAGES {
                return Err(CampaignsOperationsError::PaginationLimitExceeded);
            }
        }
        Err(CampaignsOperationsError::PaginationLimitExceeded)
    }

    /// Create one mailing list with up to ten email-only contacts, as
    /// documented by addlistandcontacts.
    pub async fn create_list_with_contacts(
        &self,
        access: &SecretValue,
        request: CreateMailingListRequest,
    ) -> Result<CampaignMutationOutcome<CreatedMailingList>, CampaignsOperationsError> {
        validate_new_list_request(&request)?;
        let params = vec![
            ("resfmt", "JSON".to_owned()),
            ("emailids", request.email_addresses.join(",")),
            ("listname", request.list_name),
            ("signupform", request.signup_form.as_str().to_owned()),
            ("mode", "newlist".to_owned()),
        ];
        let mut params = params;
        if let Some(description) = request.description {
            params.push(("listdescription", description));
        }
        let outcome = self
            .mutate_json("addlistandcontacts", &params, access, &["0"])
            .await?;
        Ok(try_map_mutation(outcome, |value| {
            let root = data_root(&value);
            Some(CreatedMailingList {
                list_key: string_field(root, "listkey").ok()?,
                list_name: string_field(root, "listname").ok()?,
            })
        }))
    }

    /// Add up to ten email-only contacts to a previously created list.
    pub async fn add_contacts_to_existing_list(
        &self,
        access: &SecretValue,
        list_key: &str,
        email_addresses: &[String],
    ) -> Result<CampaignMutationOutcome<()>, CampaignsOperationsError> {
        validate_key(list_key)?;
        validate_email_batch(email_addresses)?;
        let params = vec![
            ("resfmt", "JSON".to_owned()),
            ("listkey", list_key.to_owned()),
            ("emailids", email_addresses.join(",")),
        ];
        let outcome = self
            .mutate_json("addlistsubscribersinbulk", &params, access, &["0"])
            .await?;
        Ok(map_mutation(outcome, |_| ()))
    }

    /// Reads Zoho's documented contact/allfields schema, used to map the
    /// campaign merge fields exposed for a contact.
    pub async fn get_contact_fields(
        &self,
        access: &SecretValue,
    ) -> Result<Vec<CampaignContactField>, CampaignsOperationsError> {
        let response = self
            .read_json(
                Method::GET,
                "contact/allfields",
                &[("type", "json".to_owned())],
                access,
            )
            .await?;
        require_read_success(&response, &["0"])?;
        let root = data_root(&response);
        let fields = root.get("fieldnames").unwrap_or(root);
        let rows = array_field(fields, "fieldname")?;
        rows.iter()
            .map(|row| {
                Ok(CampaignContactField {
                    display_name: string_field(row, "DISPLAY_NAME")?,
                    field_name: string_field(row, "FIELD_NAME")?,
                    field_id: string_field(row, "FIELD_ID")?,
                })
            })
            .collect()
    }

    /// Create a documented Campaigns custom contact field. No merge-tag API
    /// is inferred from this operation.
    pub async fn create_custom_field(
        &self,
        access: &SecretValue,
        request: CreateCampaignCustomFieldRequest,
    ) -> Result<CampaignMutationOutcome<CreatedCampaignCustomField>, CampaignsOperationsError> {
        validate_custom_field_request(&request)?;
        let mut params = vec![
            ("type", "json".to_owned()),
            ("fieldname", request.field_name.clone()),
            ("fieldtype", request.field_type.as_str().to_owned()),
        ];
        if let Some(length) = request.field_length {
            params.push(("fieldlength", length.to_string()));
        }
        let outcome = self
            .mutate_json("custom/add", &params, access, &["200"])
            .await?;
        Ok(try_map_mutation(outcome, |value| {
            let root = data_root(&value);
            let field_name = string_field(root, "fieldname").ok()?;
            let field_type = parse_custom_field_type(&string_field(root, "fieldtype").ok()?)?;
            let field_length = optional_u32_field(root, "fieldlength")
                .ok()
                .flatten()
                .or(request.field_length);
            Some(CreatedCampaignCustomField {
                field_name,
                field_type,
                field_length,
            })
        }))
    }

    pub async fn get_topics(
        &self,
        access: &SecretValue,
        from_index: u32,
        range: u32,
    ) -> Result<Vec<CampaignTopic>, CampaignsOperationsError> {
        validate_page(from_index, range, true)?;
        // Zoho's topic API sample specifies a details value in its unquoted
        // key/value form rather than the JSON body used elsewhere.
        let details = format!("{{from_index:{from_index},range:{range}}}");
        let response = self
            .read_json(Method::GET, "topics", &[("details", details)], access)
            .await?;
        require_read_success(&response, &["200"])?;
        let rows = array_field(data_root(&response), "topicDetails")?;
        if rows.len() > range as usize {
            return Err(CampaignsOperationsError::InvalidResponse);
        }
        rows.iter()
            .map(|row| {
                Ok(CampaignTopic {
                    id: string_field(row, "topicId")?,
                    name: string_field(row, "topicName")?,
                    primary_list: optional_string_field(row, "primaryList")?,
                })
            })
            .collect()
    }

    pub async fn subscribe_to_list(
        &self,
        access: &SecretValue,
        list_key: &str,
        contact: ContactInput,
        topic_id: Option<&str>,
    ) -> Result<CampaignMutationOutcome<SubscriptionStatus>, CampaignsOperationsError> {
        validate_key(list_key)?;
        validate_contact(&contact)?;
        if topic_id.is_some_and(|topic| topic.trim().is_empty()) {
            return Err(CampaignsOperationsError::InvalidRequest);
        }
        let contact_info = serialize_contact_info(&contact)?;
        let mut params = vec![
            ("resfmt", "JSON".to_owned()),
            ("listkey", list_key.to_owned()),
            ("contactinfo", contact_info),
        ];
        if let Some(topic_id) = topic_id {
            params.push(("topic_id", topic_id.to_owned()));
        }
        let outcome = self
            .mutate_json("json/listsubscribe", &params, access, &["0"])
            .await?;
        Ok(map_mutation(outcome, |_| {
            SubscriptionStatus::AcceptedUnverified
        }))
    }

    pub async fn unsubscribe_from_list(
        &self,
        access: &SecretValue,
        list_key: &str,
        email: &str,
        topic_id: Option<&str>,
    ) -> Result<CampaignMutationOutcome<()>, CampaignsOperationsError> {
        validate_key(list_key)?;
        validate_email(email)?;
        if topic_id.is_some_and(|topic| topic.trim().is_empty()) {
            return Err(CampaignsOperationsError::InvalidRequest);
        }
        let contact_info = email_contact_info(email)?;
        let mut params = vec![
            ("resfmt", "JSON".to_owned()),
            ("listkey", list_key.to_owned()),
            ("contactinfo", contact_info),
        ];
        if let Some(topic_id) = topic_id {
            params.push(("topic_id", topic_id.to_owned()));
        }
        let outcome = self
            .mutate_json("json/listunsubscribe", &params, access, &["0"])
            .await?;
        Ok(map_mutation(outcome, |_| ()))
    }

    pub async fn move_to_do_not_mail(
        &self,
        access: &SecretValue,
        email: &str,
    ) -> Result<CampaignMutationOutcome<()>, CampaignsOperationsError> {
        validate_email(email)?;
        let params = vec![
            ("resfmt", "JSON".to_owned()),
            ("contactinfo", email_contact_info(email)?),
        ];
        let outcome = self
            .mutate_json("json/contactdonotmail", &params, access, &["0"])
            .await?;
        Ok(map_mutation(outcome, |_| ()))
    }

    pub async fn get_campaign_details(
        &self,
        access: &SecretValue,
        campaign_key: &str,
        campaign_type: CampaignType,
    ) -> Result<CampaignDetails, CampaignsOperationsError> {
        validate_key(campaign_key)?;
        let response = self
            .read_json(
                Method::GET,
                "getcampaigndetails",
                &[
                    ("resfmt", "JSON".to_owned()),
                    ("campaignkey", campaign_key.to_owned()),
                    ("campaigntype", campaign_type.as_str().to_owned()),
                ],
                access,
            )
            .await?;
        require_read_success(&response, &["0"])?;
        let root = data_root(&response);
        let details = first_object_field(root, "campaign-details")?;
        let associated_lists = optional_array_field(root, "associated_mailing_lists")?
            .iter()
            .map(object_map)
            .collect();
        Ok(CampaignDetails {
            status: optional_string_field(root, "campaign_status")?,
            details,
            associated_lists,
        })
    }

    pub async fn create_campaign(
        &self,
        access: &SecretValue,
        request: CampaignCreateRequest,
    ) -> Result<CampaignMutationOutcome<CreatedCampaign>, CampaignsOperationsError> {
        validate_campaign_create(&request)?;
        let list_details = json!({request.list_key.clone(): []}).to_string();
        let mut params = vec![
            ("resfmt", "JSON".to_owned()),
            ("campaignname", request.name),
            ("from_email", request.from_email),
            ("from_name", request.from_name),
            ("subject", request.subject),
            ("content_url", request.content_url),
            ("list_details", list_details),
        ];
        if let Some(topic_id) = request.topic_id {
            params.push(("topicId", topic_id));
        }
        let outcome = self
            .mutate_json("createCampaign", &params, access, &["200"])
            .await?;
        Ok(try_map_mutation(outcome, |value| {
            string_field(data_root(&value), "campaignKey")
                .map(|campaign_key| CreatedCampaign { campaign_key })
                .ok()
        }))
    }

    pub async fn clone_campaign(
        &self,
        access: &SecretValue,
        request: CampaignCloneRequest,
    ) -> Result<CampaignMutationOutcome<ClonedCampaign>, CampaignsOperationsError> {
        validate_campaign_clone(&request)?;
        let mut campaign_info = serde_json::Map::new();
        campaign_info.insert("campaignname".into(), Value::String(request.name));
        campaign_info.insert("subject".into(), Value::String(request.subject));
        campaign_info.insert("from_name".into(), Value::String(request.from_name));
        campaign_info.insert("from_add".into(), Value::String(request.from_email));
        campaign_info.insert(
            "oldcampaignkey".into(),
            Value::String(request.old_campaign_key),
        );
        campaign_info.insert("encode_type".into(), Value::String("UTF-8".into()));
        if let Some(reply_to) = request.reply_to {
            campaign_info.insert("reply_to".into(), Value::String(reply_to));
        }
        let params = vec![
            ("resfmt", "JSON".to_owned()),
            ("campaigninfo", Value::Object(campaign_info).to_string()),
        ];
        let outcome = self
            .mutate_json("json/clonecampaign", &params, access, &["0"])
            .await?;
        Ok(map_mutation(outcome, |value| ClonedCampaign {
            status: optional_string_field(data_root(&value), "campaign_status")
                .ok()
                .flatten(),
        }))
    }

    pub async fn send_campaign(
        &self,
        access: &SecretValue,
        campaign_key: &str,
    ) -> Result<CampaignMutationOutcome<SendCampaignAccepted>, CampaignsOperationsError> {
        validate_key(campaign_key)?;
        let params = vec![
            ("resfmt", "JSON".to_owned()),
            ("campaignkey", campaign_key.to_owned()),
        ];
        let outcome = self
            .mutate_json("sendcampaign", &params, access, &["200"])
            .await?;
        Ok(try_map_mutation(outcome, |value| {
            optional_string_field(data_root(&value), "campaign_status")
                .ok()
                .flatten()
                .map(|status| SendCampaignAccepted { status })
        }))
    }

    pub async fn get_campaign_report(
        &self,
        access: &SecretValue,
        campaign_key: &str,
    ) -> Result<CampaignReport, CampaignsOperationsError> {
        validate_key(campaign_key)?;
        let response = self
            .read_json(
                Method::GET,
                "campaignreports",
                &[
                    ("resfmt", "JSON".to_owned()),
                    ("campaignkey", campaign_key.to_owned()),
                ],
                access,
            )
            .await?;
        require_read_success(&response, &["0"])?;
        let root = data_root(&response);
        Ok(CampaignReport {
            metrics: first_object_field(root, "campaign-reports")?,
            details: optional_first_object_field(root, "campaign-details")?,
            reach: optional_first_object_field(root, "campaign-reach")?,
            by_location: root
                .get("campaign-by-loaction")
                .or_else(|| root.get("campaign-by-location"))
                .cloned(),
        })
    }

    pub async fn get_campaign_recipients_data(
        &self,
        access: &SecretValue,
        campaign_key: &str,
        action: CampaignRecipientAction,
    ) -> Result<CampaignRecipientPage, CampaignsOperationsError> {
        validate_key(campaign_key)?;
        let params = vec![
            ("resfmt", "JSON".to_owned()),
            ("campaignkey", campaign_key.to_owned()),
            ("action", action.as_str().to_owned()),
        ];
        let response = self
            .read_form_json("getcampaignrecipientsdata", &params, access)
            .await?;
        require_read_success(&response, &["0"])?;
        let root = data_root(&response);
        let rows = array_field(root, "list_of_details")?;
        if rows.len() > MAX_PAGE_SIZE as usize {
            return Err(CampaignsOperationsError::InvalidResponse);
        }
        let request_details = root.get("requestdetails").unwrap_or(&Value::Null);
        let from_index = optional_u32_field(request_details, "fromindex")?.unwrap_or(1);
        let range = optional_u32_field(request_details, "range")?.unwrap_or(rows.len() as u32);
        let contacts = rows
            .iter()
            .map(|row| {
                Ok(CampaignRecipientRecord {
                    email: first_string_field(
                        row,
                        &[
                            "contactemailaddress",
                            "contact_email",
                            "contact_email_address",
                        ],
                    )?,
                    contact_id: optional_string_field(row, "contactid")?,
                    sent_time_ms: optional_i64_field(row, "sent_time")?,
                    fields: object_map(row),
                })
            })
            .collect::<Result<Vec<_>, CampaignsOperationsError>>()?;
        Ok(CampaignRecipientPage {
            action,
            from_index,
            range,
            contacts,
        })
    }

    async fn read_json(
        &self,
        method: Method,
        endpoint: &str,
        params: &[(&str, String)],
        access: &SecretValue,
    ) -> Result<Value, CampaignsOperationsError> {
        let response = self
            .send_request(method, endpoint, params, None, access)
            .await?;
        if response.status() == reqwest::StatusCode::UNAUTHORIZED
            || response.status() == reqwest::StatusCode::FORBIDDEN
        {
            return Err(CampaignsOperationsError::ReauthRequired);
        }
        if response.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
            return Err(rate_limited(&response));
        }
        if !response.status().is_success() {
            return Err(CampaignsOperationsError::HttpStatus {
                status: response.status().as_u16(),
            });
        }
        read_bounded_json(response).await
    }

    async fn read_form_json(
        &self,
        endpoint: &str,
        params: &[(&str, String)],
        access: &SecretValue,
    ) -> Result<Value, CampaignsOperationsError> {
        let response = self
            .send_request(Method::POST, endpoint, &[], Some(params), access)
            .await?;
        if response.status() == reqwest::StatusCode::UNAUTHORIZED
            || response.status() == reqwest::StatusCode::FORBIDDEN
        {
            return Err(CampaignsOperationsError::ReauthRequired);
        }
        if response.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
            return Err(rate_limited(&response));
        }
        if !response.status().is_success() {
            return Err(CampaignsOperationsError::HttpStatus {
                status: response.status().as_u16(),
            });
        }
        read_bounded_json(response).await
    }

    async fn mutate_json(
        &self,
        endpoint: &str,
        params: &[(&str, String)],
        access: &SecretValue,
        success_codes: &[&str],
    ) -> Result<CampaignMutationOutcome<Value>, CampaignsOperationsError> {
        let response = match self
            .send_request(Method::POST, endpoint, &[], Some(params), access)
            .await
        {
            Ok(response) => response,
            Err(CampaignsOperationsError::Transport) => {
                return Ok(CampaignMutationOutcome::Unknown);
            }
            Err(error) => return Err(error),
        };
        let status = response.status();
        if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
            return Err(CampaignsOperationsError::ReauthRequired);
        }
        if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            return Err(rate_limited(&response));
        }
        if status.is_server_error()
            || status.is_redirection()
            || status == reqwest::StatusCode::REQUEST_TIMEOUT
        {
            return Ok(CampaignMutationOutcome::Unknown);
        }
        if status.is_client_error() {
            return Ok(CampaignMutationOutcome::Rejected {
                code: format!("http_{}", status.as_u16()),
            });
        }
        if !status.is_success() {
            return Ok(CampaignMutationOutcome::Unknown);
        }
        let value = match read_bounded_json(response).await {
            Ok(value) => value,
            Err(_) => return Ok(CampaignMutationOutcome::Unknown),
        };
        match api_code(&value) {
            Some(code) if success_codes.contains(&code.as_str()) => {
                Ok(CampaignMutationOutcome::Accepted(value))
            }
            Some(code) => Ok(CampaignMutationOutcome::Rejected { code }),
            None => Ok(CampaignMutationOutcome::Unknown),
        }
    }

    async fn send_request(
        &self,
        method: Method,
        endpoint: &str,
        query: &[(&str, String)],
        form: Option<&[(&str, String)]>,
        access: &SecretValue,
    ) -> Result<Response, CampaignsOperationsError> {
        if access.expose().trim().is_empty() {
            return Err(CampaignsOperationsError::NotConfigured);
        }
        let mut url = self
            .api_base
            .join(endpoint)
            .map_err(|_| CampaignsOperationsError::InvalidConfiguration)?;
        if !query.is_empty() {
            let mut pairs = url.query_pairs_mut();
            for (key, value) in query {
                pairs.append_pair(key, value);
            }
        }
        let mut authorization =
            HeaderValue::from_str(&format!("Zoho-oauthtoken {}", access.expose()))
                .map_err(|_| CampaignsOperationsError::InvalidRequest)?;
        authorization.set_sensitive(true);
        let mut request = self
            .client
            .request(method, url)
            .header(reqwest::header::AUTHORIZATION, authorization)
            .header(reqwest::header::ACCEPT, "application/json");
        if let Some(form) = form {
            let body = encode_form(form)?;
            if body.len() > MAX_JSON_BYTES {
                return Err(CampaignsOperationsError::RequestTooLarge);
            }
            request = request
                .header(
                    reqwest::header::CONTENT_TYPE,
                    "application/x-www-form-urlencoded",
                )
                .body(body);
        }
        request
            .send()
            .await
            .map_err(|_| CampaignsOperationsError::Transport)
    }
}

fn validate_page(
    from_index: u32,
    range: u32,
    allow_zero_start: bool,
) -> Result<(), CampaignsOperationsError> {
    if (!allow_zero_start && from_index == 0) || range == 0 || range > MAX_PAGE_SIZE {
        return Err(CampaignsOperationsError::InvalidRequest);
    }
    Ok(())
}

fn validate_key(value: &str) -> Result<(), CampaignsOperationsError> {
    if value.trim().is_empty() || value.len() > 512 {
        return Err(CampaignsOperationsError::InvalidRequest);
    }
    Ok(())
}

fn validate_email(value: &str) -> Result<(), CampaignsOperationsError> {
    let Some((local, domain)) = value.split_once('@') else {
        return Err(CampaignsOperationsError::InvalidRequest);
    };
    if value.len() > 254
        || local.is_empty()
        || domain.is_empty()
        || domain.contains('@')
        || value.chars().any(char::is_whitespace)
    {
        return Err(CampaignsOperationsError::InvalidRequest);
    }
    Ok(())
}

fn validate_email_batch(emails: &[String]) -> Result<(), CampaignsOperationsError> {
    if emails.is_empty() || emails.len() > 10 {
        return Err(CampaignsOperationsError::InvalidRequest);
    }
    let mut unique = BTreeSet::new();
    for email in emails {
        validate_email(email)?;
        if !unique.insert(email.to_ascii_lowercase()) {
            return Err(CampaignsOperationsError::InvalidRequest);
        }
    }
    Ok(())
}

fn validate_new_list_request(
    request: &CreateMailingListRequest,
) -> Result<(), CampaignsOperationsError> {
    if request.list_name.trim().is_empty()
        || request
            .description
            .as_deref()
            .is_some_and(|description| description.trim().is_empty())
    {
        return Err(CampaignsOperationsError::InvalidRequest);
    }
    validate_email_batch(&request.email_addresses)
}

fn validate_custom_field_request(
    request: &CreateCampaignCustomFieldRequest,
) -> Result<(), CampaignsOperationsError> {
    if request.field_name.is_empty()
        || !request
            .field_name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric())
        || request.field_length == Some(0)
    {
        return Err(CampaignsOperationsError::InvalidRequest);
    }
    Ok(())
}

fn parse_custom_field_type(value: &str) -> Option<CampaignCustomFieldType> {
    match value {
        "Text" => Some(CampaignCustomFieldType::Text),
        "Integer" => Some(CampaignCustomFieldType::Integer),
        "Phone" => Some(CampaignCustomFieldType::Phone),
        "Date" => Some(CampaignCustomFieldType::Date),
        "Picklist" => Some(CampaignCustomFieldType::Picklist),
        "Email" => Some(CampaignCustomFieldType::Email),
        "Checkbox" => Some(CampaignCustomFieldType::Checkbox),
        "LongInteger" => Some(CampaignCustomFieldType::LongInteger),
        "URL" => Some(CampaignCustomFieldType::Url),
        "textarea" => Some(CampaignCustomFieldType::Textarea),
        "RadioOption" => Some(CampaignCustomFieldType::RadioOption),
        "Multiselect" => Some(CampaignCustomFieldType::Multiselect),
        "DateTime" => Some(CampaignCustomFieldType::DateTime),
        "Decimal" => Some(CampaignCustomFieldType::Decimal),
        "Percent" => Some(CampaignCustomFieldType::Percent),
        _ => None,
    }
}

fn validate_contact(contact: &ContactInput) -> Result<(), CampaignsOperationsError> {
    validate_email(&contact.email)?;
    if contact.fields.iter().any(|(field, value)| {
        field.trim().is_empty() || field.chars().count() > 100 || value.chars().count() > 10_000
    }) {
        return Err(CampaignsOperationsError::InvalidRequest);
    }
    Ok(())
}

fn validate_campaign_create(
    request: &CampaignCreateRequest,
) -> Result<(), CampaignsOperationsError> {
    validate_email(&request.from_email)?;
    validate_key(&request.list_key)?;
    if request
        .topic_id
        .as_deref()
        .is_some_and(|topic| validate_key(topic).is_err())
    {
        return Err(CampaignsOperationsError::InvalidRequest);
    }
    if request.name.trim().is_empty()
        || request.subject.trim().is_empty()
        || request.from_name.trim().is_empty()
    {
        return Err(CampaignsOperationsError::InvalidRequest);
    }
    let content_url =
        Url::parse(&request.content_url).map_err(|_| CampaignsOperationsError::InvalidRequest)?;
    if content_url.scheme() != "https"
        || content_url.host_str().is_none()
        || !content_url.username().is_empty()
        || content_url.password().is_some()
        || content_url.fragment().is_some()
    {
        return Err(CampaignsOperationsError::InvalidRequest);
    }
    Ok(())
}

fn validate_campaign_clone(request: &CampaignCloneRequest) -> Result<(), CampaignsOperationsError> {
    validate_key(&request.old_campaign_key)?;
    validate_email(&request.from_email)?;
    if request.name.trim().is_empty()
        || request.subject.trim().is_empty()
        || request.from_name.trim().is_empty()
        || request
            .reply_to
            .as_deref()
            .is_some_and(|reply| validate_email(reply).is_err())
    {
        return Err(CampaignsOperationsError::InvalidRequest);
    }
    Ok(())
}

fn serialize_contact_info(contact: &ContactInput) -> Result<String, CampaignsOperationsError> {
    // Zoho documents contactinfo as JSON for /json/listsubscribe but its
    // published example is malformed JSON. This SDK uses standard JSON and
    // keeps this contract unverified until Zoho confirms the request shape.
    let mut fields = contact.fields.clone();
    fields.insert("Contact Email".into(), contact.email.clone());
    serde_json::to_string(&fields).map_err(|_| CampaignsOperationsError::InvalidRequest)
}

fn email_contact_info(email: &str) -> Result<String, CampaignsOperationsError> {
    serde_json::to_string(&BTreeMap::from([("Contact Email", email)]))
        .map_err(|_| CampaignsOperationsError::InvalidRequest)
}

fn encode_form(params: &[(&str, String)]) -> Result<Vec<u8>, CampaignsOperationsError> {
    let mut url = Url::parse("https://form.invalid/")
        .map_err(|_| CampaignsOperationsError::InvalidConfiguration)?;
    {
        let mut pairs = url.query_pairs_mut();
        for (key, value) in params {
            pairs.append_pair(key, value);
        }
    }
    url.query()
        .map(str::as_bytes)
        .map(<[u8]>::to_vec)
        .ok_or(CampaignsOperationsError::InvalidRequest)
}

async fn read_bounded_json(mut response: Response) -> Result<Value, CampaignsOperationsError> {
    if response
        .content_length()
        .is_some_and(|length| length > MAX_JSON_BYTES as u64)
    {
        return Err(CampaignsOperationsError::ResponseTooLarge);
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| CampaignsOperationsError::Transport)?
    {
        if bytes.len().saturating_add(chunk.len()) > MAX_JSON_BYTES {
            return Err(CampaignsOperationsError::ResponseTooLarge);
        }
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes).map_err(|_| CampaignsOperationsError::InvalidResponse)
}

fn rate_limited(response: &Response) -> CampaignsOperationsError {
    CampaignsOperationsError::RateLimited {
        retry_after_seconds: response
            .headers()
            .get(RETRY_AFTER)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<u32>().ok()),
    }
}

fn data_root(value: &Value) -> &Value {
    value
        .get("response")
        .filter(|response| response.is_object())
        .unwrap_or(value)
}

fn api_code(value: &Value) -> Option<String> {
    let candidate = data_root(value).get("code").or_else(|| value.get("code"))?;
    let code = candidate
        .as_str()
        .map(str::to_owned)
        .or_else(|| candidate.as_i64().map(|number| number.to_string()))?;
    (!code.is_empty() && code.len() <= 12 && code.bytes().all(|byte| byte.is_ascii_digit()))
        .then_some(code)
}

fn require_read_success(
    value: &Value,
    success_codes: &[&str],
) -> Result<(), CampaignsOperationsError> {
    let Some(code) = api_code(value) else {
        return Err(CampaignsOperationsError::InvalidResponse);
    };
    if success_codes.contains(&code.as_str()) {
        return Ok(());
    }
    if matches!(code.as_str(), "401" | "901" | "902" | "1007") {
        return Err(CampaignsOperationsError::ReauthRequired);
    }
    Err(CampaignsOperationsError::Rejected { code })
}

fn map_mutation<T, U>(
    outcome: CampaignMutationOutcome<T>,
    transform: impl FnOnce(T) -> U,
) -> CampaignMutationOutcome<U> {
    match outcome {
        CampaignMutationOutcome::Accepted(value) => {
            CampaignMutationOutcome::Accepted(transform(value))
        }
        CampaignMutationOutcome::Rejected { code } => CampaignMutationOutcome::Rejected { code },
        CampaignMutationOutcome::Unknown => CampaignMutationOutcome::Unknown,
    }
}

fn try_map_mutation<T, U>(
    outcome: CampaignMutationOutcome<T>,
    transform: impl FnOnce(T) -> Option<U>,
) -> CampaignMutationOutcome<U> {
    match outcome {
        CampaignMutationOutcome::Accepted(value) => transform(value)
            .map(CampaignMutationOutcome::Accepted)
            .unwrap_or(CampaignMutationOutcome::Unknown),
        CampaignMutationOutcome::Rejected { code } => CampaignMutationOutcome::Rejected { code },
        CampaignMutationOutcome::Unknown => CampaignMutationOutcome::Unknown,
    }
}

fn array_field<'a>(
    value: &'a Value,
    field: &str,
) -> Result<&'a Vec<Value>, CampaignsOperationsError> {
    value
        .get(field)
        .and_then(Value::as_array)
        .ok_or(CampaignsOperationsError::InvalidResponse)
}

fn optional_array_field<'a>(
    value: &'a Value,
    field: &str,
) -> Result<&'a [Value], CampaignsOperationsError> {
    match value.get(field) {
        None | Some(Value::Null) => Ok(&[]),
        Some(Value::Array(values)) => Ok(values),
        _ => Err(CampaignsOperationsError::InvalidResponse),
    }
}

fn string_field(value: &Value, field: &str) -> Result<String, CampaignsOperationsError> {
    value
        .get(field)
        .and_then(value_as_string)
        .filter(|value| !value.is_empty())
        .ok_or(CampaignsOperationsError::InvalidResponse)
}

fn first_string_field(value: &Value, fields: &[&str]) -> Result<String, CampaignsOperationsError> {
    fields
        .iter()
        .find_map(|field| value.get(*field).and_then(value_as_string))
        .filter(|value| !value.is_empty())
        .ok_or(CampaignsOperationsError::InvalidResponse)
}

fn optional_string_field(
    value: &Value,
    field: &str,
) -> Result<Option<String>, CampaignsOperationsError> {
    match value.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value_as_string(value)
            .map(Some)
            .ok_or(CampaignsOperationsError::InvalidResponse),
    }
}

fn value_as_string(value: &Value) -> Option<String> {
    value
        .as_str()
        .map(str::to_owned)
        .or_else(|| value.as_i64().map(|number| number.to_string()))
        .or_else(|| value.as_u64().map(|number| number.to_string()))
}

fn optional_u64_field(value: &Value, field: &str) -> Result<Option<u64>, CampaignsOperationsError> {
    match value.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_u64()
            .or_else(|| value.as_str().and_then(|number| number.parse().ok()))
            .map(Some)
            .ok_or(CampaignsOperationsError::InvalidResponse),
    }
}

fn optional_u32_field(value: &Value, field: &str) -> Result<Option<u32>, CampaignsOperationsError> {
    match value.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_u64()
            .and_then(|number| u32::try_from(number).ok())
            .or_else(|| value.as_str().and_then(|number| number.parse().ok()))
            .map(Some)
            .ok_or(CampaignsOperationsError::InvalidResponse),
    }
}

fn optional_i64_field(value: &Value, field: &str) -> Result<Option<i64>, CampaignsOperationsError> {
    match value.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_i64()
            .or_else(|| value.as_str().and_then(|number| number.parse().ok()))
            .map(Some)
            .ok_or(CampaignsOperationsError::InvalidResponse),
    }
}

fn object_map(value: &Value) -> BTreeMap<String, Value> {
    value
        .as_object()
        .map(|values| {
            values
                .iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect()
        })
        .unwrap_or_default()
}

fn first_object_field(
    value: &Value,
    field: &str,
) -> Result<BTreeMap<String, Value>, CampaignsOperationsError> {
    let value = value
        .get(field)
        .ok_or(CampaignsOperationsError::InvalidResponse)?;
    if let Some(object) = value.as_object() {
        return Ok(object
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect());
    }
    let first = value
        .as_array()
        .and_then(|values| values.first())
        .ok_or(CampaignsOperationsError::InvalidResponse)?;
    if !first.is_object() {
        return Err(CampaignsOperationsError::InvalidResponse);
    }
    Ok(object_map(first))
}

fn optional_first_object_field(
    value: &Value,
    field: &str,
) -> Result<BTreeMap<String, Value>, CampaignsOperationsError> {
    match value.get(field) {
        None | Some(Value::Null) => Ok(BTreeMap::new()),
        Some(_) => first_object_field(value, field),
    }
}

fn returned_page(
    root: &Value,
    default_from: u32,
    default_range: u32,
) -> Result<(u32, u32), CampaignsOperationsError> {
    let details = root.get("requestdetails").unwrap_or(root);
    let from_index = optional_u32_field(details, "fromindex")?.unwrap_or(default_from);
    let range = optional_u32_field(details, "range")?.unwrap_or(default_range);
    if from_index == 0 || range > MAX_PAGE_SIZE {
        return Err(CampaignsOperationsError::InvalidResponse);
    }
    Ok((from_index, range))
}
