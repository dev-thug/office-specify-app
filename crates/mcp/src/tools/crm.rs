//! CRM tools (B.6.2). Reads and mutations delegate to the same application
//! use cases as REST; authenticated identity comes only from the MCP context.

use std::collections::BTreeMap;

use crate::{ToolDef, arguments, result, tool_with_db};
use chrono::{DateTime, Utc};
use contracts::crm::{CompanyResponse, ContactResponse};
use schemars::JsonSchema;
use serde::de::DeserializeOwned;
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
    let activities_key = search_key.clone();
    let notes_key = search_key.clone();
    let tasks_key = search_key.clone();

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
        tool_with_db(
            "crm_create_company",
            "Create a CRM company record immediately (scope crm.write; same as POST /companies). Search first with crm_search_companies by name and domain to avoid duplicates; the response lists advisory duplicate_candidates. Returns the created company summary including id and version. Requires an explicit idempotency key.",
            schema::<CreateCompanyInput>(),
            schema::<CreateCompanyOutput>(),
            false,
            |db, ctx, args| async move {
                let input = arguments::<CreateCompanyInput>(args)?;
                validate_idempotency_key(&input.idempotency_key)?;
                let request: contracts::crm::CreateCompanyRequest = to_contract(&input)?;
                let mut company =
                    application::crm::companies::create(&db, &ctx, &request, input.idempotency_key)
                        .await?
                        .body;
                let duplicate_candidates = company
                    .duplicate_candidates
                    .take()
                    .unwrap_or_default()
                    .into_iter()
                    .map(|candidate| DuplicateCandidateOutput {
                        id: candidate.id,
                        name: candidate.name,
                        domain: candidate.domain,
                        matched_on: candidate.matched_on,
                    })
                    .collect();
                result(CreateCompanyOutput {
                    company: company_summary(company),
                    duplicate_candidates,
                })
            },
        ),
        tool_with_db(
            "crm_update_company",
            "Update a CRM company directly (scope crm.write; same as PATCH /companies/{id}). Pass expected_version from the latest read and a changes object with any of: name, domain, website_url, industry, country_code, lifecycle_stage (prospect|engaged|qualified|customer|inactive), description, custom_fields (replaces the whole object). null clears nullable fields. Returns the updated company with its new version.",
            schema::<UpdateCompanyInput>(),
            schema::<CompanySummary>(),
            false,
            |db, ctx, args| async move {
                let input = arguments::<UpdateCompanyInput>(args)?;
                let request: contracts::crm::PatchCompanyRequest = from_changes(input.changes)?;
                let company = application::crm::companies::patch(
                    &db,
                    &ctx,
                    input.company_id,
                    input.expected_version,
                    &request,
                )
                .await?;
                result(company_summary(company))
            },
        ),
        tool_with_db(
            "crm_archive_company",
            "Archive a CRM company (scope crm.write; same as POST /companies/{id}/archive). Requires expected_version from the latest read and accepts an optional reason. Returns the archived company with its new version.",
            schema::<ArchiveCompanyInput>(),
            schema::<CompanySummary>(),
            false,
            |db, ctx, args| async move {
                let input = arguments::<ArchiveCompanyInput>(args)?;
                let company = application::crm::companies::archive(
                    &db,
                    &ctx,
                    input.company_id,
                    input.expected_version,
                    &contracts::crm::ArchiveCompanyRequest {
                        reason: input.reason,
                    },
                )
                .await?;
                result(company_summary(company))
            },
        ),
        tool_with_db(
            "crm_request_company_merge",
            "Queue a merge of source_id into the surviving company_id (scope crm.write plus manager/admin role; same as POST /companies/{id}/merge-requests). field_choices maps name, domain, website_url, industry, country_code, lifecycle_stage, description, custom_fields or owner_principal_id to \"source\" or \"target\". Requires an explicit idempotency key; returns the queued job.",
            schema::<MergeCompanyInput>(),
            schema::<MergeCompanyOutput>(),
            false,
            |db, ctx, args| async move {
                let input = arguments::<MergeCompanyInput>(args)?;
                validate_idempotency_key(&input.idempotency_key)?;
                let merge = application::crm::companies::request_merge(
                    &db,
                    &ctx,
                    input.company_id,
                    &contracts::crm::MergeCompanyRequest {
                        source_id: input.source_id,
                        field_choices: input.field_choices,
                    },
                    input.idempotency_key,
                )
                .await?
                .body;
                result(MergeCompanyOutput {
                    job_id: merge.job_id,
                    source_id: merge.source_id,
                    target_id: merge.target_id,
                    status: merge.status,
                })
            },
        ),
        tool_with_db(
            "crm_list_company_activities",
            "List a company's activity timeline (scope crm.read; same as GET /companies/{id}/activities). Mailbox-sourced rows are filtered by mailbox grants. Supports limit (1-200) and the returned cursor.",
            schema::<ListCompanyActivitiesInput>(),
            schema::<PageOutput<ActivityToolOutput>>(),
            false,
            move |db, ctx, args| {
                let hmac_key = activities_key.clone();
                async move {
                    let input = arguments::<ListCompanyActivitiesInput>(args)?;
                    let limit = checked_limit(input.limit)?;
                    let (items, page) = application::crm::activities::list_company(
                        &db,
                        &ctx,
                        &hmac_key,
                        input.company_id,
                        &application::crm::activities::ActivityListQuery {
                            limit,
                            cursor: input.cursor,
                        },
                    )
                    .await?;
                    result(PageOutput {
                        items: items.into_iter().map(activity_output).collect(),
                        next_cursor: page.next_cursor,
                        has_more: page.has_more,
                    })
                }
            },
        ),
        tool_with_db(
            "crm_create_contact",
            "Create a CRM contact immediately (scope crm.write; same as POST /contacts). Optional email links a primary address atomically; status is new|active|departed|inactive. Search existing contacts first to avoid duplicates. Requires an explicit idempotency key; returns the contact with id and version.",
            schema::<CreateContactInput>(),
            schema::<ContactToolOutput>(),
            false,
            |db, ctx, args| async move {
                let input = arguments::<CreateContactInput>(args)?;
                validate_idempotency_key(&input.idempotency_key)?;
                let request: contracts::crm::CreateContactRequest = to_contract(&input)?;
                let contact = application::crm::contacts::create(
                    &db,
                    &ctx,
                    &request,
                    Some(input.idempotency_key),
                )
                .await?
                .body;
                result(contact_summary(contact))
            },
        ),
        tool_with_db(
            "crm_update_contact",
            "Update a CRM contact directly (scope crm.write; same as PATCH /contacts/{id}). Pass expected_version from the latest read and a changes object with any of: name, phone, company_id, job_title, department, status (new|active|departed|inactive). null clears nullable fields; company_id null unlinks the company. Returns the updated contact with its new version.",
            schema::<UpdateContactInput>(),
            schema::<ContactToolOutput>(),
            false,
            |db, ctx, args| async move {
                let input = arguments::<UpdateContactInput>(args)?;
                let request: contracts::crm::PatchContactRequest = from_changes(input.changes)?;
                let contact = application::crm::contacts::patch(
                    &db,
                    &ctx,
                    input.contact_id,
                    input.expected_version,
                    &request,
                )
                .await?;
                result(contact_summary(contact))
            },
        ),
        tool_with_db(
            "crm_add_contact_email",
            "Link an email address to a contact (scope crm.write; same as POST /contacts/{id}/emails). Optional label, is_primary and kind (person|role|unknown). Requires an explicit idempotency key; returns the linked address with verification and send-eligibility status.",
            schema::<AddContactEmailInput>(),
            schema::<AddContactEmailOutput>(),
            false,
            |db, ctx, args| async move {
                let input = arguments::<AddContactEmailInput>(args)?;
                validate_idempotency_key(&input.idempotency_key)?;
                let request: contracts::crm::AddContactEmailRequest = to_contract(&input)?;
                let email = application::crm::contacts::add_email(
                    &db,
                    &ctx,
                    input.contact_id,
                    &request,
                    Some(input.idempotency_key),
                )
                .await?
                .body;
                result(AddContactEmailOutput {
                    email_address_id: email.email_address_id,
                    address: email.address,
                    label: email.label,
                    is_primary: email.is_primary,
                    verification_status: verification_status(email.verification_status),
                    linked_contact_ids: email.linked_contact_ids,
                    marketing_eligibility: MarketingEligibilityOutput {
                        eligible: email.marketing_eligibility.eligible,
                        reason_codes: email.marketing_eligibility.reason_codes,
                    },
                })
            },
        ),
        tool_with_db(
            "crm_remove_contact_email",
            "Unlink an email address from a contact (scope crm.write; same as DELETE /contacts/{id}/emails/{email_address_id}). Requires the contact's expected_version; returns the updated contact.",
            schema::<RemoveContactEmailInput>(),
            schema::<ContactToolOutput>(),
            false,
            |db, ctx, args| async move {
                let input = arguments::<RemoveContactEmailInput>(args)?;
                let contact = application::crm::contacts::remove_email(
                    &db,
                    &ctx,
                    input.contact_id,
                    input.email_address_id,
                    input.expected_version,
                )
                .await?;
                result(contact_summary(contact))
            },
        ),
        tool_with_db(
            "crm_set_primary_email",
            "Make an already linked email address the contact's primary address (scope crm.write; same as PUT /contacts/{id}/primary-email). Requires the contact's expected_version; returns the updated contact.",
            schema::<SetPrimaryEmailInput>(),
            schema::<ContactToolOutput>(),
            false,
            |db, ctx, args| async move {
                let input = arguments::<SetPrimaryEmailInput>(args)?;
                let contact = application::crm::contacts::set_primary_email(
                    &db,
                    &ctx,
                    input.contact_id,
                    input.expected_version,
                    &contracts::crm::SetPrimaryEmailRequest {
                        email_address_id: input.email_address_id,
                    },
                )
                .await?;
                result(contact_summary(contact))
            },
        ),
        tool_with_db(
            "crm_list_notes",
            "List CRM notes, optionally filtered by company_id, contact_id or thread_id (scope crm.read; same as GET /notes). Thread notes are filtered by mailbox grants. Supports limit (1-200) and the returned cursor.",
            schema::<ListNotesInput>(),
            schema::<PageOutput<NoteToolOutput>>(),
            false,
            move |db, ctx, args| {
                let hmac_key = notes_key.clone();
                async move {
                    let input = arguments::<ListNotesInput>(args)?;
                    let limit = checked_limit(input.limit)?;
                    let (items, page) = application::crm::notes::list(
                        &db,
                        &ctx,
                        &hmac_key,
                        &application::crm::notes::NoteListQuery {
                            company_id: input.company_id,
                            contact_id: input.contact_id,
                            thread_id: input.thread_id,
                            limit,
                            cursor: input.cursor,
                        },
                    )
                    .await?;
                    result(PageOutput {
                        items: items.into_iter().map(note_output).collect(),
                        next_cursor: page.next_cursor,
                        has_more: page.has_more,
                    })
                }
            },
        ),
        tool_with_db(
            "crm_list_tasks",
            "List CRM tasks, optionally filtered by status (open|done|canceled), owner_principal_id, company_id or contact_id (scope crm.read; same as GET /tasks). Supports limit (1-200) and the returned cursor.",
            schema::<ListTasksInput>(),
            schema::<PageOutput<TaskToolOutput>>(),
            false,
            move |db, ctx, args| {
                let hmac_key = tasks_key.clone();
                async move {
                    let input = arguments::<ListTasksInput>(args)?;
                    let limit = checked_limit(input.limit)?;
                    let (items, page) = application::crm::tasks::list(
                        &db,
                        &ctx,
                        &hmac_key,
                        &application::crm::tasks::TaskListQuery {
                            status: input.status,
                            owner_principal_id: input.owner_principal_id,
                            company_id: input.company_id,
                            contact_id: input.contact_id,
                            limit,
                            cursor: input.cursor,
                        },
                    )
                    .await?;
                    result(PageOutput {
                        items: items.into_iter().map(task_output).collect(),
                        next_cursor: page.next_cursor,
                        has_more: page.has_more,
                    })
                }
            },
        ),
        tool_with_db(
            "crm_create_task",
            "Create a CRM task (scope crm.write; same as POST /tasks). Owner defaults to the caller; assigning another owner requires manager/admin. Optional company_id, contact_id, thread_id and due_at (RFC 3339). Requires an explicit idempotency key; returns the task with id and version.",
            schema::<CreateTaskInput>(),
            schema::<TaskToolOutput>(),
            false,
            |db, ctx, args| async move {
                let input = arguments::<CreateTaskInput>(args)?;
                validate_idempotency_key(&input.idempotency_key)?;
                let request: contracts::crm::CreateTaskRequest = to_contract(&input)?;
                let task = application::crm::tasks::create(
                    &db,
                    &ctx,
                    &request,
                    Some(input.idempotency_key),
                )
                .await?
                .body;
                result(task_output(task))
            },
        ),
        tool_with_db(
            "crm_update_task",
            "Update a CRM task (scope crm.write; same as PATCH /tasks/{id}). Pass expected_version and a changes object with any of: title, status (open|done|canceled), due_at (RFC 3339, null clears), owner_principal_id (reassignment requires manager/admin). Returns the updated task with its new version.",
            schema::<UpdateTaskInput>(),
            schema::<TaskToolOutput>(),
            false,
            |db, ctx, args| async move {
                let input = arguments::<UpdateTaskInput>(args)?;
                let request: contracts::crm::PatchTaskRequest = from_changes(input.changes)?;
                let task = application::crm::tasks::patch(
                    &db,
                    &ctx,
                    input.task_id,
                    input.expected_version,
                    &request,
                )
                .await?;
                result(task_output(task))
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

/// Converts a local, schema-documented tool input into the REST contract
/// request so validation (including `deny_unknown_fields`) stays identical.
fn to_contract<T: DeserializeOwned>(input: &impl Serialize) -> Result<T, contracts::AppError> {
    let value = serde_json::to_value(input)
        .map_err(|e| contracts::AppError::internal(format!("tool arguments: {e}")))?;
    serde_json::from_value(value).map_err(|e| {
        contracts::AppError::validation(format!("invalid tool arguments: {e}"), vec![])
    })
}

/// PATCH-style `changes` map keeps the contract's absent-vs-null semantics.
fn from_changes<T: DeserializeOwned>(
    changes: serde_json::Map<String, serde_json::Value>,
) -> Result<T, contracts::AppError> {
    serde_json::from_value(serde_json::Value::Object(changes)).map_err(|e| {
        contracts::AppError::validation(
            format!("invalid changes: {e}"),
            vec![contracts::Violation {
                field: "changes".into(),
                code: "INVALID".into(),
            }],
        )
    })
}

fn activity_output(activity: contracts::crm::ActivityResponse) -> ActivityToolOutput {
    ActivityToolOutput {
        id: activity.id,
        kind: activity.kind,
        occurred_at: activity.occurred_at,
        summary: activity.summary,
        company_id: activity.company_id,
        contact_id: activity.contact_id,
        mail_account_id: activity.mail_account_id,
        thread_id: activity.thread_id,
        booking_id: activity.booking_id,
        actor_principal_id: activity.actor_principal_id,
    }
}

fn note_output(note: contracts::crm::NoteResponse) -> NoteToolOutput {
    NoteToolOutput {
        id: note.id,
        company_id: note.company_id,
        contact_id: note.contact_id,
        thread_id: note.thread_id,
        mail_account_id: note.mail_account_id,
        created_by: note.created_by,
        body: note.body,
        version: note.version,
        created_at: note.created_at,
        updated_at: note.updated_at,
    }
}

fn task_output(task: contracts::crm::TaskResponse) -> TaskToolOutput {
    TaskToolOutput {
        id: task.id,
        title: task.title,
        owner_principal_id: task.owner_principal_id,
        company_id: task.company_id,
        contact_id: task.contact_id,
        thread_id: task.thread_id,
        status: task.status.as_str().to_string(),
        due_at: task.due_at,
        version: task.version,
        created_at: task.created_at,
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

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum LifecycleStageInput {
    Prospect,
    Engaged,
    Qualified,
    Customer,
    Inactive,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum ContactStatusInput {
    New,
    Active,
    Departed,
    Inactive,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum AddressKindInput {
    Person,
    Role,
    Unknown,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct CreateCompanyInput {
    name: String,
    #[serde(default)]
    domain: Option<String>,
    #[serde(default)]
    website_url: Option<String>,
    #[serde(default)]
    industry: Option<String>,
    /// ISO 3166-1 alpha-2 country code.
    #[serde(default)]
    country_code: Option<String>,
    #[serde(default)]
    lifecycle_stage: Option<LifecycleStageInput>,
    #[serde(default)]
    owner_principal_id: Option<Uuid>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    custom_fields: Option<serde_json::Map<String, serde_json::Value>>,
    #[serde(skip_serializing)]
    #[schemars(length(min = 16, max = 128))]
    idempotency_key: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct UpdateCompanyInput {
    company_id: Uuid,
    expected_version: i64,
    /// Fields to change; absent keeps, null clears nullable fields.
    changes: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ArchiveCompanyInput {
    company_id: Uuid,
    expected_version: i64,
    #[serde(default)]
    reason: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct MergeCompanyInput {
    /// Surviving (target) company.
    company_id: Uuid,
    /// Company merged into the target.
    source_id: Uuid,
    /// Per-field winner: "source" or "target".
    #[serde(default)]
    field_choices: BTreeMap<String, String>,
    #[schemars(length(min = 16, max = 128))]
    idempotency_key: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ListCompanyActivitiesInput {
    company_id: Uuid,
    #[serde(default)]
    cursor: Option<String>,
    #[serde(default)]
    limit: Option<u32>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct CreateContactInput {
    name: String,
    #[serde(default)]
    phone: Option<String>,
    /// Optional primary email linked atomically with creation.
    #[serde(default)]
    email: Option<String>,
    #[serde(default)]
    company_id: Option<Uuid>,
    #[serde(default)]
    job_title: Option<String>,
    #[serde(default)]
    department: Option<String>,
    #[serde(default)]
    status: Option<ContactStatusInput>,
    #[serde(default)]
    owner_principal_id: Option<Uuid>,
    #[serde(skip_serializing)]
    #[schemars(length(min = 16, max = 128))]
    idempotency_key: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct UpdateContactInput {
    contact_id: Uuid,
    expected_version: i64,
    /// Fields to change; absent keeps, null clears nullable fields.
    changes: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct AddContactEmailInput {
    #[serde(skip_serializing)]
    contact_id: Uuid,
    address: String,
    #[serde(default)]
    label: Option<String>,
    #[serde(default)]
    is_primary: Option<bool>,
    #[serde(default)]
    kind: Option<AddressKindInput>,
    #[serde(skip_serializing)]
    #[schemars(length(min = 16, max = 128))]
    idempotency_key: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct RemoveContactEmailInput {
    contact_id: Uuid,
    email_address_id: Uuid,
    expected_version: i64,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct SetPrimaryEmailInput {
    contact_id: Uuid,
    email_address_id: Uuid,
    expected_version: i64,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ListNotesInput {
    #[serde(default)]
    company_id: Option<Uuid>,
    #[serde(default)]
    contact_id: Option<Uuid>,
    #[serde(default)]
    thread_id: Option<Uuid>,
    #[serde(default)]
    cursor: Option<String>,
    #[serde(default)]
    limit: Option<u32>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ListTasksInput {
    /// open | done | canceled
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    owner_principal_id: Option<Uuid>,
    #[serde(default)]
    company_id: Option<Uuid>,
    #[serde(default)]
    contact_id: Option<Uuid>,
    #[serde(default)]
    cursor: Option<String>,
    #[serde(default)]
    limit: Option<u32>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct CreateTaskInput {
    title: String,
    /// Defaults to the caller; another owner requires manager/admin.
    #[serde(default)]
    owner_principal_id: Option<Uuid>,
    #[serde(default)]
    company_id: Option<Uuid>,
    #[serde(default)]
    contact_id: Option<Uuid>,
    #[serde(default)]
    thread_id: Option<Uuid>,
    #[serde(default)]
    due_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing)]
    #[schemars(length(min = 16, max = 128))]
    idempotency_key: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct UpdateTaskInput {
    task_id: Uuid,
    expected_version: i64,
    /// Fields to change; absent keeps, due_at null clears the due date.
    changes: serde_json::Map<String, serde_json::Value>,
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
struct CreateCompanyOutput {
    #[serde(flatten)]
    company: CompanySummary,
    /// Advisory same-domain/same-name matches; never auto-merged.
    duplicate_candidates: Vec<DuplicateCandidateOutput>,
}

#[derive(Debug, Serialize, JsonSchema)]
struct DuplicateCandidateOutput {
    id: Uuid,
    name: String,
    domain: Option<String>,
    matched_on: Vec<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
struct MergeCompanyOutput {
    job_id: Uuid,
    source_id: Uuid,
    target_id: Uuid,
    status: String,
}

#[derive(Debug, Serialize, JsonSchema)]
struct PageOutput<T> {
    items: Vec<T>,
    next_cursor: Option<String>,
    has_more: bool,
}

#[derive(Debug, Serialize, JsonSchema)]
struct ActivityToolOutput {
    id: Uuid,
    kind: String,
    occurred_at: DateTime<Utc>,
    summary: serde_json::Value,
    company_id: Option<Uuid>,
    contact_id: Option<Uuid>,
    mail_account_id: Option<Uuid>,
    thread_id: Option<Uuid>,
    booking_id: Option<Uuid>,
    actor_principal_id: Option<Uuid>,
}

#[derive(Debug, Serialize, JsonSchema)]
struct NoteToolOutput {
    id: Uuid,
    company_id: Option<Uuid>,
    contact_id: Option<Uuid>,
    thread_id: Option<Uuid>,
    mail_account_id: Option<Uuid>,
    created_by: Uuid,
    body: String,
    version: i64,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

#[derive(Debug, Serialize, JsonSchema)]
struct TaskToolOutput {
    id: Uuid,
    title: String,
    owner_principal_id: Uuid,
    company_id: Option<Uuid>,
    contact_id: Option<Uuid>,
    thread_id: Option<Uuid>,
    status: String,
    due_at: Option<DateTime<Utc>>,
    version: i64,
    created_at: DateTime<Utc>,
}

#[derive(Debug, Serialize, JsonSchema)]
struct AddContactEmailOutput {
    email_address_id: Uuid,
    address: String,
    label: Option<String>,
    is_primary: bool,
    verification_status: String,
    linked_contact_ids: Vec<Uuid>,
    marketing_eligibility: MarketingEligibilityOutput,
}

#[derive(Debug, Serialize, JsonSchema)]
struct CreateNoteOutput {
    note_id: Uuid,
}

#[cfg(test)]
mod tests {
    use super::{
        CreateCompanyInput, CreateNoteInput, ToolDef, from_changes, to_contract, tools,
        tools_with_key, validate_idempotency_key,
    };
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
                config: Arc::new(infrastructure::config::Config::test()),
                rate_limiter: None,
            },
            ctx.clone(),
            args,
        )
        .await
    }

    #[test]
    fn crm_catalog_lists_every_crm_tool() {
        let names: Vec<_> = tools().iter().map(|tool| tool.name).collect();
        assert_eq!(
            names,
            [
                "crm_search_companies",
                "crm_get_company",
                "crm_get_contact",
                "crm_create_note",
                "crm_create_company",
                "crm_update_company",
                "crm_archive_company",
                "crm_request_company_merge",
                "crm_list_company_activities",
                "crm_create_contact",
                "crm_update_contact",
                "crm_add_contact_email",
                "crm_remove_contact_email",
                "crm_set_primary_email",
                "crm_list_notes",
                "crm_list_tasks",
                "crm_create_task",
                "crm_update_task",
            ]
        );
        assert!(!names.contains(&"crm_propose_update"));
    }

    #[test]
    fn mutation_schemas_require_idempotency_and_reject_identity_overrides() {
        let tools = tools();
        for name in [
            "crm_create_note",
            "crm_create_company",
            "crm_request_company_merge",
            "crm_create_contact",
            "crm_add_contact_email",
            "crm_create_task",
        ] {
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
    fn versioned_update_schemas_require_expected_version() {
        let tools = tools();
        for name in [
            "crm_update_company",
            "crm_archive_company",
            "crm_update_contact",
            "crm_remove_contact_email",
            "crm_set_primary_email",
            "crm_update_task",
        ] {
            let tool = tools.iter().find(|tool| tool.name == name).unwrap();
            assert!(
                tool.input_schema["required"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|v| v == "expected_version"),
                "{name}"
            );
            let properties = tool.input_schema["properties"].as_object().unwrap();
            assert!(!properties.contains_key("workspace_id"), "{name}");
            assert!(!properties.contains_key("principal_id"), "{name}");
        }
    }

    #[test]
    fn create_company_converts_to_the_rest_contract() {
        let input = arguments::<CreateCompanyInput>(json!({
            "name": "Acme",
            "lifecycle_stage": "engaged",
            "idempotency_key": "create-company-0001"
        }))
        .unwrap();
        let request: contracts::crm::CreateCompanyRequest = to_contract(&input).unwrap();
        assert_eq!(request.name, "Acme");
        assert_eq!(
            request.lifecycle_stage,
            Some(contracts::crm::LifecycleStage::Engaged)
        );
        let patch: Result<contracts::crm::PatchCompanyRequest, _> = from_changes(
            json!({"workspace_id": Uuid::nil()})
                .as_object()
                .unwrap()
                .clone(),
        );
        assert!(patch.is_err(), "unknown patch fields are rejected");
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
            ScopeSet::of([Scope::CrmRead, Scope::CrmWrite, Scope::BookingRead]),
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
    }

    #[tokio::test]
    async fn agent_creates_and_updates_a_company_directly() {
        if std::env::var("DATABASE_URL").is_err() {
            eprintln!("skipping: DATABASE_URL not set");
            return;
        }
        let (db, world) = test_world().await;
        let agent = ctx_for_credential(
            &world,
            &world.agent,
            Uuid::new_v4(),
            ScopeSet::of([Scope::CrmRead, Scope::CrmWrite]),
            "mcp-crm-agent-write",
        );
        let tools = tools_with_key(&[42; 32]);

        let created = call_tool(
            &tools,
            &db,
            &agent,
            "crm_create_company",
            json!({
                "name": "Agent Direct Co",
                "domain": "agent-direct.example",
                "idempotency_key": "mcp-agent-create-company-0001"
            }),
        )
        .await
        .unwrap();
        let company_id: Uuid = serde_json::from_value(created["id"].clone()).unwrap();
        let version = created["version"].as_i64().unwrap();
        assert_eq!(created["name"], "Agent Direct Co");
        assert!(created["duplicate_candidates"].is_array());

        let updated = call_tool(
            &tools,
            &db,
            &agent,
            "crm_update_company",
            json!({
                "company_id": company_id,
                "expected_version": version,
                "changes": {"industry": "software", "description": null}
            }),
        )
        .await
        .unwrap();
        assert_eq!(updated["id"], json!(company_id));
        assert_eq!(updated["industry"], "software");
        let new_version = updated["version"].as_i64().unwrap();
        assert!(new_version > version);

        let stale = call_tool(
            &tools,
            &db,
            &agent,
            "crm_update_company",
            json!({
                "company_id": company_id,
                "expected_version": version,
                "changes": {"industry": "hardware"}
            }),
        )
        .await;
        assert!(stale.is_err(), "stale expected_version is rejected");
    }
}
