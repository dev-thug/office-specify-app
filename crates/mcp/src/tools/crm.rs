//! CRM tools (B.6.2). Reads and mutations delegate to the same application
//! use cases as REST; authenticated identity comes only from the MCP context.

use std::collections::BTreeMap;

use crate::{ToolDef, arguments, result, tool_with_db};
use chrono::{DateTime, Utc};
use contracts::crm::{CompanyResponse, ContactResponse};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

const DEFAULT_LIMIT: u32 = 50;
const MAX_LIMIT: u32 = 200;
const IDEMPOTENCY_KEY_MIN_LEN: usize = 16;

/// Catalog-only helper. Runtime registration must use [`tools_with_key`] so
/// signed company-search cursors use the server's secret HMAC key.
pub fn tools() -> Vec<ToolDef> {
    tools_with_key(&[])
}

/// CRM tools bound to the server's cursor-signing key.
pub fn tools_with_key(hmac_key: &[u8]) -> Vec<ToolDef> {
    let search_key = hmac_key.to_vec();
    let company_key = search_key.clone();

    vec![
        tool_with_db(
            "crm_search_companies",
            "Search CRM companies visible to the authenticated principal. Results are summaries and the signed cursor is bound to this principal and filter set.",
            schema::<SearchCompaniesInput>(),
            schema::<SearchCompaniesOutput>(),
            false,
            move |db, ctx, args| {
                let hmac_key = search_key.clone();
                async move {
                    let input = arguments::<SearchCompaniesInput>(args)?;
                    let limit = checked_limit(input.limit)?;
                    let filters = input.filters.unwrap_or_default();
                    let query = application::crm::companies::CompanyListQuery {
                        q: input.query,
                        lifecycle_stage: filters.lifecycle_stage,
                        owner_principal_id: filters.owner_principal_id,
                        archived: filters.archived.unwrap_or(false),
                        sort: None,
                        limit,
                        cursor: input.cursor,
                    };
                    let (companies, page) =
                        application::crm::companies::list(&db, &ctx, &hmac_key, &query).await?;
                    let output = SearchCompaniesOutput {
                        companies: companies.into_iter().map(company_summary).collect(),
                        next_cursor: page.next_cursor,
                        has_more: page.has_more,
                    };
                    result(output)
                }
            },
        ),
        tool_with_db(
            "crm_get_company",
            "Read a CRM company and only the requested related sections. Activity and note sections apply their source mailbox ACLs.",
            schema::<GetCompanyInput>(),
            schema::<GetCompanyOutput>(),
            false,
            move |db, ctx, args| {
                let hmac_key = company_key.clone();
                async move {
                    let input = arguments::<GetCompanyInput>(args)?;
                    let company =
                        application::crm::companies::get(&db, &ctx, input.company_id).await?;
                    let mut sections = BTreeMap::new();
                    for section in input.requested_sections {
                        match section {
                            CompanySection::Overview => {}
                            CompanySection::Contacts | CompanySection::Emails => {
                                let (items, page) = application::crm::contacts::list(
                                    &db,
                                    &ctx,
                                    &hmac_key,
                                    &application::crm::contacts::ContactListQuery {
                                        company_id: Some(input.company_id),
                                        limit: DEFAULT_LIMIT,
                                        ..Default::default()
                                    },
                                )
                                .await?;
                                if section == CompanySection::Emails {
                                    let items: Vec<_> = items
                                        .into_iter()
                                        .map(contact_summary)
                                        .map(|contact| {
                                            serde_json::json!({
                                                "contact_id": contact.id,
                                                "contact_name": contact.name,
                                                "emails": contact.emails,
                                            })
                                        })
                                        .collect();
                                    sections.insert(
                                        "emails".to_string(),
                                        serde_json::json!({
                                            "items": items,
                                            "next_cursor": page.next_cursor,
                                            "has_more": page.has_more,
                                        }),
                                    );
                                } else {
                                    sections.insert(
                                        "contacts".to_string(),
                                        serde_json::json!({
                                            "items": items.into_iter().map(contact_summary).collect::<Vec<_>>(),
                                            "next_cursor": page.next_cursor,
                                            "has_more": page.has_more,
                                        }),
                                    );
                                }
                            }
                            CompanySection::Activity | CompanySection::Activities => {
                                let (items, page) = application::crm::activities::list_company(
                                    &db,
                                    &ctx,
                                    &hmac_key,
                                    input.company_id,
                                    &application::crm::activities::ActivityListQuery {
                                        limit: DEFAULT_LIMIT,
                                        cursor: None,
                                    },
                                )
                                .await?;
                                sections.insert(
                                    "activities".to_string(),
                                    serde_json::json!({
                                        "items": items,
                                        "next_cursor": page.next_cursor,
                                        "has_more": page.has_more,
                                    }),
                                );
                            }
                            CompanySection::Notes => {
                                let (items, page) = application::crm::notes::list(
                                    &db,
                                    &ctx,
                                    &hmac_key,
                                    &application::crm::notes::NoteListQuery {
                                        company_id: Some(input.company_id),
                                        limit: DEFAULT_LIMIT,
                                        ..Default::default()
                                    },
                                )
                                .await?;
                                sections.insert(
                                    "notes".to_string(),
                                    serde_json::json!({
                                        "items": items,
                                        "next_cursor": page.next_cursor,
                                        "has_more": page.has_more,
                                    }),
                                );
                            }
                            CompanySection::Tasks => {
                                let (items, page) = application::crm::tasks::list(
                                    &db,
                                    &ctx,
                                    &hmac_key,
                                    &application::crm::tasks::TaskListQuery {
                                        company_id: Some(input.company_id),
                                        limit: DEFAULT_LIMIT,
                                        ..Default::default()
                                    },
                                )
                                .await?;
                                sections.insert(
                                    "tasks".to_string(),
                                    serde_json::json!({
                                        "items": items,
                                        "next_cursor": page.next_cursor,
                                        "has_more": page.has_more,
                                    }),
                                );
                            }
                            CompanySection::Meetings => {
                                let (contacts, contact_page) = application::crm::contacts::list(
                                    &db,
                                    &ctx,
                                    &hmac_key,
                                    &application::crm::contacts::ContactListQuery {
                                        company_id: Some(input.company_id),
                                        limit: MAX_LIMIT,
                                        ..Default::default()
                                    },
                                )
                                .await?;
                                let address_ids: std::collections::BTreeSet<_> = contacts
                                    .iter()
                                    .flat_map(|contact| {
                                        contact.emails.iter().map(|email| email.email_address_id)
                                    })
                                    .collect();
                                let bookings =
                                    application::booking::list_bookings(&db, &ctx, None, None)
                                        .await?;
                                let meetings: Vec<_> = bookings
                                    .into_iter()
                                    .filter(|booking| {
                                        address_ids.contains(&booking.guest_email_address_id)
                                    })
                                    .map(|booking| MeetingToolSummary {
                                        id: booking.id,
                                        host_principal_id: booking.host_principal_id,
                                        starts_at: booking.starts_at,
                                        ends_at: booking.ends_at,
                                        status: booking.status,
                                    })
                                    .collect();
                                sections.insert(
                                    "meetings".to_string(),
                                    serde_json::json!({
                                        "items": meetings,
                                        "has_more_contacts": contact_page.has_more,
                                    }),
                                );
                            }
                            CompanySection::Research => {
                                let (items, next) = application::research::list_evidence(
                                    &db,
                                    &ctx,
                                    &application::research::EvidenceFilters {
                                        company_id: Some(input.company_id),
                                        ..Default::default()
                                    },
                                    None,
                                    i64::from(DEFAULT_LIMIT),
                                )
                                .await?;
                                sections.insert(
                                    "research".to_string(),
                                    serde_json::json!({
                                        "items": items,
                                        "has_more": next.is_some(),
                                    }),
                                );
                            }
                        }
                    }
                    result(GetCompanyOutput {
                        company: company_summary(company),
                        sections,
                    })
                }
            },
        ),
        tool_with_db(
            "crm_get_contact",
            "Read one CRM contact, including linked address verification and send-eligibility status. CRM access does not grant mailbox access.",
            schema::<GetContactInput>(),
            schema::<ContactToolOutput>(),
            false,
            |db, ctx, args| async move {
                let input = arguments::<GetContactInput>(args)?;
                let contact = application::crm::contacts::get(&db, &ctx, input.contact_id).await?;
                result(contact_summary(contact))
            },
        ),
        tool_with_db(
            "crm_propose_update",
            "Create a review-pending proposal to change allowlisted company or contact fields. The CRM record is not changed until a human accepts the proposal.",
            schema::<ProposeUpdateInput>(),
            schema::<ProposeUpdateOutput>(),
            false,
            |db, ctx, args| async move {
                let input = arguments::<ProposeUpdateInput>(args)?;
                validate_idempotency_key(&input.idempotency_key)?;
                let request = contracts::research::ProposeUpdateRequest {
                    idempotency_key: input.idempotency_key,
                    agent_run_id: input.agent_run_id,
                    target: contracts::research::ResearchUpdateTarget {
                        kind: match input.target.kind {
                            UpdateTargetKind::Company => {
                                contracts::research::ResearchTargetKind::Company
                            }
                            UpdateTargetKind::Contact => {
                                contracts::research::ResearchTargetKind::Contact
                            }
                        },
                        id: input.target.id,
                    },
                    expected_version: input.expected_version,
                    field_changes: input.field_changes,
                    evidence: input
                        .evidence
                        .into_iter()
                        .map(EvidenceToolInput::into_contract)
                        .collect(),
                };
                let proposal = application::research::propose_update(&db, &ctx, &request).await?;
                result(ProposeUpdateOutput {
                    proposal_id: proposal.id,
                    status: proposal.status.as_str().to_string(),
                    proposed_data: proposal.proposed_data,
                    version: proposal.version,
                })
            },
        ),
        tool_with_db(
            "crm_create_note",
            "Create an internal CRM note on exactly one company, contact, or mailbox-authorized thread. Requires an explicit idempotency key.",
            schema::<CreateNoteInput>(),
            schema::<CreateNoteOutput>(),
            false,
            |db, ctx, args| async move {
                let input = arguments::<CreateNoteInput>(args)?;
                validate_idempotency_key(&input.idempotency_key)?;
                let request = contracts::crm::CreateNoteRequest {
                    company_id: input.target.company_id,
                    contact_id: input.target.contact_id,
                    thread_id: input.target.thread_id,
                    body: input.text,
                };
                let note = application::crm::notes::create(
                    &db,
                    &ctx,
                    &request,
                    Some(input.idempotency_key),
                )
                .await?;
                result(CreateNoteOutput {
                    note_id: note.body.id,
                })
            },
        ),
    ]
}

fn schema<T: JsonSchema>() -> serde_json::Value {
    serde_json::to_value(schemars::schema_for!(T)).expect("JSON Schema serializes")
}

fn checked_limit(limit: Option<u32>) -> Result<u32, contracts::AppError> {
    match limit.unwrap_or(DEFAULT_LIMIT) {
        0 => Err(contracts::AppError::validation(
            "limit must be between 1 and 200",
            vec![contracts::Violation {
                field: "limit".into(),
                code: "RANGE".into(),
            }],
        )),
        n => Ok(n.min(MAX_LIMIT)),
    }
}

fn validate_idempotency_key(key: &str) -> Result<(), contracts::AppError> {
    if (IDEMPOTENCY_KEY_MIN_LEN..=infrastructure::idempotency::KEY_MAX_LEN).contains(&key.len()) {
        Ok(())
    } else {
        Err(contracts::AppError::validation(
            "idempotency_key must be 16..=128 characters",
            vec![contracts::Violation {
                field: "idempotency_key".into(),
                code: "LENGTH".into(),
            }],
        ))
    }
}

fn company_summary(company: CompanyResponse) -> CompanySummary {
    CompanySummary {
        id: company.id,
        name: company.name,
        domain: company.domain,
        website_url: company.website_url,
        description: company.description,
        industry: company.industry,
        country_code: company.country_code,
        lifecycle_stage: company.lifecycle_stage.as_str().to_string(),
        owner_principal_id: company.owner_principal_id,
        version: company.version,
    }
}

pub(super) fn contact_summary(contact: ContactResponse) -> ContactToolOutput {
    ContactToolOutput {
        id: contact.id,
        company_id: contact.company_id,
        name: contact.name,
        job_title: contact.job_title,
        department: contact.department,
        status: contact.status.as_str().to_string(),
        version: contact.version,
        emails: contact
            .emails
            .into_iter()
            .map(|email| ContactEmailToolOutput {
                email_address_id: email.email_address_id,
                address: email.address,
                is_primary: email.is_primary,
                verification_status: verification_status(email.verification_status),
                linked_contact_ids: email.linked_contact_ids,
                marketing_eligibility: MarketingEligibilityOutput {
                    eligible: email.marketing_eligibility.eligible,
                    reason_codes: email.marketing_eligibility.reason_codes,
                },
            })
            .collect(),
    }
}

fn verification_status(status: contracts::crm::VerificationStatus) -> String {
    match status {
        contracts::crm::VerificationStatus::Unknown => "unknown",
        contracts::crm::VerificationStatus::Verified => "verified",
        contracts::crm::VerificationStatus::Invalid => "invalid",
        contracts::crm::VerificationStatus::Risky => "risky",
    }
    .to_string()
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct SearchCompaniesInput {
    #[serde(default)]
    query: Option<String>,
    #[serde(default)]
    filters: Option<CompanyFilters>,
    #[serde(default)]
    cursor: Option<String>,
    #[serde(default)]
    limit: Option<u32>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct CompanyFilters {
    #[serde(default)]
    lifecycle_stage: Option<String>,
    #[serde(default)]
    owner_principal_id: Option<Uuid>,
    #[serde(default)]
    archived: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct GetCompanyInput {
    company_id: Uuid,
    #[serde(default)]
    requested_sections: Vec<CompanySection>,
}

#[derive(Debug, Clone, Copy, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum CompanySection {
    Overview,
    Contacts,
    Activity,
    Activities,
    Emails,
    Meetings,
    Notes,
    Tasks,
    Research,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct GetContactInput {
    contact_id: Uuid,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ProposeUpdateInput {
    agent_run_id: Uuid,
    target: UpdateTargetInput,
    expected_version: i64,
    field_changes: BTreeMap<String, serde_json::Value>,
    evidence: Vec<EvidenceToolInput>,
    #[schemars(length(min = 16, max = 128))]
    idempotency_key: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct UpdateTargetInput {
    kind: UpdateTargetKind,
    id: Uuid,
}

#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum UpdateTargetKind {
    Company,
    Contact,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct EvidenceToolInput {
    field_path: String,
    source_url: String,
    observed_at: DateTime<Utc>,
    value: serde_json::Value,
    evidence_type: EvidenceTypeInput,
    #[serde(default)]
    excerpt: Option<String>,
    #[serde(default)]
    confidence: Option<f64>,
}

impl EvidenceToolInput {
    fn into_contract(self) -> contracts::research::EvidenceInput {
        contracts::research::EvidenceInput {
            field_path: self.field_path,
            source_url: self.source_url,
            observed_at: self.observed_at,
            value: self.value,
            evidence_type: match self.evidence_type {
                EvidenceTypeInput::Observed => contracts::research::EvidenceType::Observed,
                EvidenceTypeInput::Inferred => contracts::research::EvidenceType::Inferred,
            },
            excerpt: self.excerpt,
            confidence: self.confidence,
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum EvidenceTypeInput {
    Observed,
    Inferred,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct CreateNoteInput {
    target: NoteTargetInput,
    text: String,
    #[schemars(length(min = 16, max = 128))]
    idempotency_key: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct NoteTargetInput {
    #[serde(default)]
    company_id: Option<Uuid>,
    #[serde(default)]
    contact_id: Option<Uuid>,
    #[serde(default)]
    thread_id: Option<Uuid>,
}

#[derive(Debug, Serialize, JsonSchema)]
struct SearchCompaniesOutput {
    companies: Vec<CompanySummary>,
    next_cursor: Option<String>,
    has_more: bool,
}

#[derive(Debug, Serialize, JsonSchema)]
struct GetCompanyOutput {
    company: CompanySummary,
    sections: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Serialize, JsonSchema)]
struct CompanySummary {
    id: Uuid,
    name: String,
    domain: Option<String>,
    website_url: Option<String>,
    description: Option<String>,
    industry: Option<String>,
    country_code: Option<String>,
    lifecycle_stage: String,
    owner_principal_id: Option<Uuid>,
    version: i64,
}

#[derive(Debug, Serialize, JsonSchema)]
pub(super) struct ContactToolOutput {
    id: Uuid,
    company_id: Option<Uuid>,
    name: String,
    job_title: Option<String>,
    department: Option<String>,
    status: String,
    version: i64,
    emails: Vec<ContactEmailToolOutput>,
}

#[derive(Debug, Serialize, JsonSchema)]
struct ContactEmailToolOutput {
    email_address_id: Uuid,
    address: String,
    is_primary: bool,
    verification_status: String,
    linked_contact_ids: Vec<Uuid>,
    marketing_eligibility: MarketingEligibilityOutput,
}

#[derive(Debug, Serialize, JsonSchema)]
struct MeetingToolSummary {
    id: Uuid,
    host_principal_id: Uuid,
    starts_at: DateTime<Utc>,
    ends_at: DateTime<Utc>,
    status: String,
}

#[derive(Debug, Serialize, JsonSchema)]
struct MarketingEligibilityOutput {
    eligible: bool,
    reason_codes: Vec<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
struct ProposeUpdateOutput {
    proposal_id: Uuid,
    status: String,
    proposed_data: serde_json::Value,
    version: i64,
}

#[derive(Debug, Serialize, JsonSchema)]
struct CreateNoteOutput {
    note_id: Uuid,
}

#[cfg(test)]
mod tests {
    use super::{CreateNoteInput, ToolDef, tools, tools_with_key, validate_idempotency_key};
    use crate::arguments;
    use contracts::{Ctx, Scope, ScopeSet};
    use infrastructure::db::{Db, ROLE_API};
    use infrastructure::testing::{TestWorld, ctx_for, ctx_for_credential, seed_workspace};
    use serde_json::{Value, json};
    use std::sync::Arc;
    use uuid::Uuid;

    async fn test_world() -> (Db, TestWorld) {
        let url = std::env::var("DATABASE_URL").expect("DATABASE_URL is required for MCP DB tests");
        let owner = Db::connect(&url, None).await.unwrap();
        owner.migrate().await.unwrap();
        let db = Db::connect(&url, Some(ROLE_API)).await.unwrap();
        let world = seed_workspace(&db).await.unwrap();
        (db, world)
    }

    async fn call_tool(
        tools: &[ToolDef],
        db: &Db,
        ctx: &Ctx,
        name: &str,
        args: Value,
    ) -> Result<Value, contracts::AppError> {
        let tool = tools.iter().find(|tool| tool.name == name).unwrap();
        let calendar = Arc::new(infrastructure::providers::fake::FakeMailProvider::new());
        (tool.handler)(
            crate::ToolServices {
                db: db.clone(),
                calendar,
            },
            ctx.clone(),
            args,
        )
        .await
    }

    #[test]
    fn crm_catalog_lists_all_five_tools() {
        let names: Vec<_> = tools().iter().map(|tool| tool.name).collect();
        assert_eq!(
            names,
            [
                "crm_search_companies",
                "crm_get_company",
                "crm_get_contact",
                "crm_propose_update",
                "crm_create_note",
            ]
        );
    }

    #[test]
    fn mutation_schemas_require_idempotency_and_reject_identity_overrides() {
        let tools = tools();
        for name in ["crm_propose_update", "crm_create_note"] {
            let tool = tools.iter().find(|tool| tool.name == name).unwrap();
            let properties = tool.input_schema["properties"].as_object().unwrap();
            assert!(properties.contains_key("idempotency_key"), "{name}");
            assert_eq!(properties["idempotency_key"]["minLength"], 16);
            assert_eq!(properties["idempotency_key"]["maxLength"], 128);
            assert!(
                tool.input_schema["required"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|v| v == "idempotency_key"),
                "{name}"
            );
            assert!(!properties.contains_key("workspace_id"), "{name}");
            assert!(!properties.contains_key("principal_id"), "{name}");
        }
    }

    #[test]
    fn note_tool_arguments_reject_a_caller_selected_principal() {
        let decoded = arguments::<CreateNoteInput>(serde_json::json!({
            "target": {"company_id": Uuid::nil()},
            "text": "Review the source",
            "idempotency_key": "note-1",
            "principal_id": Uuid::nil(),
        }));
        assert!(decoded.is_err());
    }

    #[test]
    fn mcp_mutation_keys_follow_the_schema_minimum() {
        assert!(validate_idempotency_key("123456789012345").is_err());
        assert!(validate_idempotency_key("1234567890123456").is_ok());
        assert!(validate_idempotency_key(&"k".repeat(129)).is_err());
    }

    #[tokio::test]
    async fn crm_tools_use_scoped_application_usecases() {
        let (db, world) = test_world().await;
        let admin = ctx_for(&world, &world.admin, "mcp-crm-admin");
        let agent = ctx_for_credential(
            &world,
            &world.agent,
            Uuid::new_v4(),
            ScopeSet::of([
                Scope::CrmRead,
                Scope::CrmWrite,
                Scope::ResearchPropose,
                Scope::BookingRead,
            ]),
            "mcp-crm-agent",
        );
        let tools = tools_with_key(&[42; 32]);

        let company = application::crm::companies::create(
            &db,
            &admin,
            &contracts::crm::CreateCompanyRequest {
                name: "MCP Tool Target".into(),
                domain: Some("mcp-tools.example".into()),
                website_url: None,
                industry: None,
                country_code: None,
                lifecycle_stage: None,
                owner_principal_id: None,
                description: None,
                custom_fields: None,
            },
            "mcp-create-company-0001".into(),
        )
        .await
        .unwrap()
        .body;

        let search = call_tool(
            &tools,
            &db,
            &agent,
            "crm_search_companies",
            json!({"query": "MCP Tool", "limit": 10}),
        )
        .await
        .unwrap();
        assert!(
            search["companies"]
                .as_array()
                .unwrap()
                .iter()
                .any(|row| row["id"] == json!(company.id))
        );

        let contact = application::crm::contacts::create(
            &db,
            &admin,
            &contracts::crm::CreateContactRequest {
                name: "MCP Contact".into(),
                phone: None,
                email: None,
                company_id: Some(company.id),
                job_title: Some("Director".into()),
                department: None,
                status: None,
                owner_principal_id: None,
            },
            Some("mcp-create-contact-0001".into()),
        )
        .await
        .unwrap()
        .body;
        application::crm::contacts::add_email(
            &db,
            &admin,
            contact.id,
            &contracts::crm::AddContactEmailRequest {
                address: "mcp-contact@example.com".into(),
                label: Some("work".into()),
                is_primary: Some(true),
                kind: None,
            },
            Some("mcp-link-email-0001".into()),
        )
        .await
        .unwrap();

        let company_detail = call_tool(
            &tools,
            &db,
            &agent,
            "crm_get_company",
            json!({
                "company_id": company.id,
                "requested_sections": ["overview", "contacts", "emails", "activity", "meetings", "notes", "tasks", "research"]
            }),
        )
        .await
        .unwrap();
        assert_eq!(company_detail["company"]["id"], json!(company.id));
        assert_eq!(
            company_detail["sections"]["contacts"]["items"][0]["id"],
            json!(contact.id)
        );
        assert_eq!(
            company_detail["sections"]["emails"]["items"][0]["emails"][0]["address"],
            "mcp-contact@example.com"
        );
        assert!(
            company_detail["sections"]["meetings"]["items"]
                .as_array()
                .unwrap()
                .is_empty()
        );

        let contact_detail = call_tool(
            &tools,
            &db,
            &agent,
            "crm_get_contact",
            json!({"contact_id": contact.id}),
        )
        .await
        .unwrap();
        assert_eq!(
            contact_detail["emails"][0]["address"],
            "mcp-contact@example.com"
        );

        let note_args = json!({
            "target": {"company_id": company.id},
            "text": "Review the expansion source.",
            "idempotency_key": "mcp-create-note-0001"
        });
        let first_note = call_tool(&tools, &db, &agent, "crm_create_note", note_args.clone())
            .await
            .unwrap();
        let replay_note = call_tool(&tools, &db, &agent, "crm_create_note", note_args)
            .await
            .unwrap();
        assert_eq!(first_note["note_id"], replay_note["note_id"]);

        let run = application::agents::create_run(
            &db,
            &admin,
            &contracts::agents::CreateAgentRunRequest {
                agent_principal_id: world.agent.principal_id,
                objective: "Review a company field".into(),
                scope: None,
                budget: None,
            },
        )
        .await
        .unwrap();
        let proposal_args = json!({
            "agent_run_id": run.id,
            "target": {"kind": "company", "id": company.id},
            "expected_version": company.version,
            "field_changes": {"industry": "software"},
            "evidence": [{
                "field_path": "industry",
                "source_url": "https://acme.example/about",
                "observed_at": chrono::Utc::now().to_rfc3339(),
                "value": "software",
                "evidence_type": "observed"
            }],
            "idempotency_key": "mcp-propose-update-0001"
        });
        let proposal = call_tool(
            &tools,
            &db,
            &agent,
            "crm_propose_update",
            proposal_args.clone(),
        )
        .await
        .unwrap();
        let proposal_replay = call_tool(&tools, &db, &agent, "crm_propose_update", proposal_args)
            .await
            .unwrap();
        assert_eq!(proposal["proposal_id"], proposal_replay["proposal_id"]);
        assert_eq!(proposal["status"], "pending");

        let unchanged = application::crm::companies::get(&db, &admin, company.id)
            .await
            .unwrap();
        assert_eq!(
            unchanged.industry, None,
            "proposal must not mutate CRM fields"
        );
    }
}
