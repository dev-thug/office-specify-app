//! Shared crm use-case helpers: validation, row→DTO mapping, optional
//! idempotency, signed-cursor pagination plumbing.

use std::future::Future;

use chrono::{DateTime, Utc};
use contracts::crm::{
    ActivityResponse, AddContactEmailResponse, CompanyDuplicateCandidate, CompanyResponse,
    ContactEmailItem, ContactResponse, ContactStatus, LifecycleStage, MarketingEligibility,
    NoteResponse, TaskResponse, TaskStatus, VerificationStatus,
};
use contracts::{AppError, Ctx, Page, Violation};
use infrastructure::cursor::Cursor;
use infrastructure::db::{Db, Tx};
use infrastructure::error::map_sqlx;
use infrastructure::idempotency::{self, Stored};
use infrastructure::repo::crm::{
    self, ActivityInsert, ActivityRow, CompanyRow, ContactEmailRow, ContactRow, NoteRow, TaskRow,
};
use serde::{Serialize, de::DeserializeOwned};
use uuid::Uuid;

// ------------------------------------------------------------ validation

pub(crate) fn bad_request(detail: impl Into<String>, violations: Vec<Violation>) -> AppError {
    AppError::validation(detail, violations)
}

pub(crate) fn violation(field: &str, code: &str) -> Violation {
    Violation {
        field: field.to_string(),
        code: code.to_string(),
    }
}

/// `1..=300` per the DDL CHECK; empty names are rejected before the DB.
pub(crate) fn require_name(name: &str, field: &str) -> Result<(), AppError> {
    let len = name.trim().len();
    if (1..=300).contains(&len) {
        Ok(())
    } else {
        Err(bad_request(
            format!("{field} must be 1-300 characters"),
            vec![violation(field, "LENGTH")],
        ))
    }
}

/// `country_code ~ '^[A-Z]{2}$'` (DDL CHECK).
pub(crate) fn require_country_code(code: Option<&str>) -> Result<(), AppError> {
    if let Some(c) = code
        && !(c.len() == 2 && c.bytes().all(|b| b.is_ascii_uppercase()))
    {
        return Err(bad_request(
            "country_code must be an uppercase ISO-3166 alpha-2 code",
            vec![violation("country_code", "FORMAT")],
        ));
    }
    Ok(())
}

/// Validate an optional owner override: the target must exist, be human and
/// active (§5.1). Returns the resolved owner.
pub(crate) async fn resolve_owner(
    tx: &mut Tx<'_>,
    owner: Option<Uuid>,
) -> Result<Option<Uuid>, AppError> {
    if let Some(id) = owner
        && !crm::principal_is_active_human(tx, id).await?
    {
        return Err(bad_request(
            "owner_principal_id must reference an active human principal",
            vec![violation("owner_principal_id", "INVALID_PRINCIPAL")],
        ));
    }
    Ok(owner)
}

// ------------------------------------------------------------- idempotency

/// Run `f` once for a POST; when `key` is present route it through
/// `idempotency::run` so retries replay the stored response (B.1.3).
/// `endpoint` is the canonical "METHOD /route/pattern" scope string.
///
/// `f` receives the transaction by value and returns it with the response, so
/// the business writes, audit, outbox and idempotency response commit atomically.
pub(crate) async fn run_maybe_idempotent<'db, T, F, Fut>(
    db: &'db Db,
    ctx: &Ctx,
    endpoint: &str,
    key: Option<String>,
    request_body: &serde_json::Value,
    status: u16,
    f: F,
) -> Result<Stored<T>, AppError>
where
    F: FnOnce(Tx<'db>) -> Fut,
    Fut: Future<Output = Result<(Tx<'db>, T), AppError>>,
    T: Serialize + DeserializeOwned,
{
    let Some(key) = key else {
        let tx = db.begin(ctx.workspace_id).await?;
        let (tx, body) = f(tx).await?;
        tx.commit().await?;
        return Ok(Stored { status, body });
    };
    let scope = idempotency::Scope {
        workspace_id: ctx.workspace_id,
        principal_or_capability: ctx.principal_id.to_string(),
        endpoint: endpoint.to_string(),
        key,
    };
    let request_hash = infrastructure::crypto::sha256_bytes(request_body.to_string().as_bytes());
    let out = idempotency::run(db, &scope, &request_hash, move |tx| {
        let future = f(tx);
        async move {
            let (tx, body) = future.await?;
            Ok((tx, Stored { status, body }))
        }
    })
    .await?;
    Ok(out.into_inner())
}

// -------------------------------------------------------------- pagination

/// Keyset position of the last row of a page: `(sort_ts, id)`.
pub(crate) type Keyset = (DateTime<Utc>, Uuid);

/// Decode the cursor for this list, binding it to the principal and the
/// filter projection (B.1.5). `filters` must be the canonical JSON of the
/// request's filter fields.
pub(crate) fn decode_cursor(
    encoded: Option<&str>,
    hmac_key: &[u8],
    ctx: &Ctx,
    filters: &serde_json::Value,
) -> Result<Option<Keyset>, AppError> {
    let Some(enc) = encoded else { return Ok(None) };
    let expected = Cursor::hash_filter(filters);
    let cur = Cursor::decode(enc, hmac_key, ctx.principal_id, &expected)?;
    let [ts_v, id_v] = <[serde_json::Value; 2]>::try_from(cur.sort_values)
        .map_err(|_| AppError::validation("invalid pagination cursor", vec![]))?;
    let ts = DateTime::parse_from_rfc3339(ts_v.as_str().unwrap_or_default())
        .map_err(|_| AppError::validation("invalid pagination cursor", vec![]))?
        .with_timezone(&Utc);
    let id = Uuid::parse_str(id_v.as_str().unwrap_or_default())
        .map_err(|_| AppError::validation("invalid pagination cursor", vec![]))?;
    Ok(Some((ts, id)))
}

/// Build `Page` for a fetched page: `rows` were fetched with `limit + 1`
/// lookahead, so `has_more` is true when the fetch overflowed. `keyset_of`
/// extracts the keyset position from a row.
pub(crate) fn page_for<R>(
    rows: &mut Vec<R>,
    limit: u32,
    hmac_key: &[u8],
    ctx: &Ctx,
    filters: &serde_json::Value,
    keyset_of: impl Fn(&R) -> Keyset,
) -> Page {
    let has_more = rows.len() as u32 > limit;
    if has_more {
        rows.truncate(limit as usize);
    }
    let next_cursor = if has_more && let Some(last) = rows.last() {
        let (ts, id) = keyset_of(last);
        Some(
            Cursor::new(
                vec![
                    serde_json::Value::String(ts.to_rfc3339()),
                    serde_json::Value::String(id.to_string()),
                ],
                Cursor::hash_filter(filters),
                ctx.principal_id,
            )
            .encode(hmac_key),
        )
    } else {
        None
    };
    Page {
        next_cursor,
        has_more,
    }
}

// ------------------------------------------------------------ row → DTO

pub(crate) fn company_dto(r: &CompanyRow) -> CompanyResponse {
    CompanyResponse {
        id: r.id,
        name: r.name.clone(),
        domain: r.domain.clone(),
        website_url: r.website_url.clone(),
        industry: r.industry.clone(),
        country_code: r.country_code.clone(),
        lifecycle_stage: LifecycleStage::parse(&r.lifecycle_stage)
            .unwrap_or(LifecycleStage::Prospect),
        owner_principal_id: r.owner_principal_id,
        description: r.description.clone(),
        custom_fields: r.custom_fields.clone(),
        archived_at: r.archived_at,
        version: r.version,
        created_at: r.created_at,
        updated_at: r.updated_at,
        duplicate_candidates: None,
    }
}

pub(crate) fn duplicate_dto(r: &CompanyRow, matched_on: Vec<String>) -> CompanyDuplicateCandidate {
    CompanyDuplicateCandidate {
        id: r.id,
        name: r.name.clone(),
        domain: r.domain.clone(),
        matched_on,
    }
}

pub(crate) fn contact_status(s: &str) -> ContactStatus {
    ContactStatus::parse(s).unwrap_or(ContactStatus::New)
}

pub(crate) fn task_status(s: &str) -> TaskStatus {
    TaskStatus::parse(s).unwrap_or(TaskStatus::Open)
}

pub(crate) fn verification_status(s: &str) -> VerificationStatus {
    match s {
        "verified" => VerificationStatus::Verified,
        "invalid" => VerificationStatus::Invalid,
        "risky" => VerificationStatus::Risky,
        _ => VerificationStatus::Unknown,
    }
}

pub(crate) fn contact_dto(r: &ContactRow, emails: Vec<ContactEmailItem>) -> ContactResponse {
    ContactResponse {
        id: r.id,
        company_id: r.company_id,
        name: r.name.clone(),
        job_title: r.job_title.clone(),
        department: r.department.clone(),
        owner_principal_id: r.owner_principal_id,
        status: contact_status(&r.status),
        archived_at: r.archived_at,
        emails,
        version: r.version,
        created_at: r.created_at,
        updated_at: r.updated_at,
    }
}

/// Build `ContactEmailItem`s for a set of link rows, resolving the shared
/// `linked_contact_ids` and `marketing_eligibility` once per address.
pub(crate) async fn contact_email_items(
    tx: &mut Tx<'_>,
    rows: &[ContactEmailRow],
) -> Result<Vec<ContactEmailItem>, AppError> {
    let ids: Vec<Uuid> = rows.iter().map(|r| r.email_address_id).collect();
    let eligibility = crm::marketing_eligibility(tx, &ids).await?;
    let mut items = Vec::with_capacity(rows.len());
    for r in rows {
        let linked: Vec<Uuid> = crm::contacts_for_address(tx, r.email_address_id)
            .await?
            .into_iter()
            .filter(|c| *c != r.contact_id)
            .collect();
        let codes = eligibility
            .get(&r.email_address_id)
            .cloned()
            .unwrap_or_default();
        items.push(ContactEmailItem {
            email_address_id: r.email_address_id,
            address: r.address_original.clone(),
            label: r.label.clone(),
            is_primary: r.is_primary,
            verification_status: verification_status(&r.verification_status),
            linked_contact_ids: linked,
            marketing_eligibility: MarketingEligibility {
                eligible: codes.is_empty(),
                reason_codes: codes,
            },
        });
    }
    Ok(items)
}

pub(crate) fn add_email_dto(
    row: &ContactEmailRow,
    linked: Vec<Uuid>,
    reason_codes: Vec<String>,
) -> AddContactEmailResponse {
    AddContactEmailResponse {
        email_address_id: row.email_address_id,
        address: row.address_original.clone(),
        label: row.label.clone(),
        is_primary: row.is_primary,
        verification_status: verification_status(&row.verification_status),
        linked_contact_ids: linked,
        marketing_eligibility: MarketingEligibility {
            eligible: reason_codes.is_empty(),
            reason_codes,
        },
    }
}

pub(crate) fn note_dto(r: &NoteRow) -> NoteResponse {
    NoteResponse {
        id: r.id,
        company_id: r.company_id,
        contact_id: r.contact_id,
        thread_id: r.thread_id,
        mail_account_id: r.mail_account_id,
        created_by: r.created_by,
        body: r.body.clone(),
        version: r.version,
        created_at: r.created_at,
        updated_at: r.updated_at,
    }
}

pub(crate) fn task_dto(r: &TaskRow) -> TaskResponse {
    TaskResponse {
        id: r.id,
        title: r.title.clone(),
        owner_principal_id: r.owner_principal_id,
        company_id: r.company_id,
        contact_id: r.contact_id,
        thread_id: r.thread_id,
        status: task_status(&r.status),
        due_at: r.due_at,
        version: r.version,
        created_at: r.created_at,
    }
}

pub(crate) fn activity_dto(r: &ActivityRow) -> ActivityResponse {
    ActivityResponse {
        id: r.id,
        kind: r.kind.clone(),
        occurred_at: r.occurred_at,
        summary: r.summary.clone(),
        company_id: r.company_id,
        contact_id: r.contact_id,
        mail_account_id: r.mail_account_id,
        thread_id: r.thread_id,
        booking_id: r.booking_id,
        actor_principal_id: r.actor_principal_id,
    }
}

// ------------------------------------------------------------ own timeline

/// Write a crm-owned timeline row inside the business tx (integration
/// contracts: "plus crm's own notes/tasks"). `source_event_id` is the entity
/// or job id — the `(workspace, source_event_id, kind)` UNIQUE makes retries
/// and replays idempotent.
pub(crate) async fn write_activity(tx: &mut Tx<'_>, a: ActivityInsert) -> Result<(), AppError> {
    crm::insert_activity(tx, &a).await.map(|_| ())
}

/// Record an ID/version-only CRM change event in the transaction that writes
/// the business row and audit entry. These events contain no note text,
/// addresses, message bodies, or other user content.
pub(crate) async fn emit_change(
    tx: &mut Tx<'_>,
    event_type: &str,
    aggregate_type: &str,
    aggregate_id: Uuid,
    aggregate_version: i64,
) -> Result<(), AppError> {
    infrastructure::outbox::emit(
        tx,
        event_type,
        aggregate_type,
        aggregate_id,
        aggregate_version,
        serde_json::json!({"id": aggregate_id, "version": aggregate_version}),
    )
    .await?;
    Ok(())
}

/// Emit a transition with a small allowlisted payload, such as the IDs in a
/// queued company merge request.
pub(crate) async fn emit_event(
    tx: &mut Tx<'_>,
    event_type: &str,
    aggregate_type: &str,
    aggregate_id: Uuid,
    aggregate_version: i64,
    payload: serde_json::Value,
) -> Result<(), AppError> {
    infrastructure::outbox::emit(
        tx,
        event_type,
        aggregate_type,
        aggregate_id,
        aggregate_version,
        payload,
    )
    .await?;
    Ok(())
}

/// The workspace's `system` principal — audit `actor` for worker-executed
/// writes (§5.1: actor ≠ requested_by).
pub(crate) async fn system_principal(tx: &mut Tx<'_>) -> Result<Option<Uuid>, AppError> {
    sqlx::query_scalar(
        "SELECT id FROM principals
          WHERE workspace_id = $1 AND kind = 'system' AND status = 'active'
          ORDER BY created_at, id LIMIT 1",
    )
    .bind(tx.workspace_id())
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}
