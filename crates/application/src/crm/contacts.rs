//! Contact use cases (B.2 `/contacts`, B.3.4, §6.3 email-address identity).
//!
//! Addresses are normalized through `domain::email_address::normalize` under
//! the workspace's local-part policy and deduped workspace-wide on
//! `normalized_key` — the same address may link to N contacts (role
//! addresses, T31 groundwork). Creating a well-formed address never writes
//! `verified`/`consented` state (B.3.4).

use chrono::Utc;
use contracts::crm::{
    AddContactEmailRequest, AddContactEmailResponse, ContactResponse, CreateContactRequest,
    PatchContactRequest, SetPrimaryEmailRequest,
};
use contracts::{AppError, Ctx, ErrorCode, Page, Role};
use domain::crm::{self as dcrm, activity_kind};
use domain::email_address;
use infrastructure::audit::{self, AuditEntry};
use infrastructure::db::{Db, Tx};
use infrastructure::idempotency::Stored;
use infrastructure::repo::crm::{self, ContactFilter, ContactInsert, ContactRow};
use uuid::Uuid;

use super::util;
use super::{require_crm_read, require_crm_write};
use crate::authz;

// ------------------------------------------------------------------- query

#[derive(Debug, Default)]
pub struct ContactListQuery {
    pub q: Option<String>,
    pub company_id: Option<Uuid>,
    pub status: Option<String>,
    pub owner_principal_id: Option<Uuid>,
    pub archived: bool,
    pub limit: u32,
    pub cursor: Option<String>,
}

fn filters_json(q: &ContactListQuery) -> serde_json::Value {
    serde_json::json!({
        "endpoint": "GET /api/v1/contacts",
        "q": q.q,
        "company_id": q.company_id,
        "status": q.status,
        "owner_principal_id": q.owner_principal_id,
        "archived": q.archived,
        "limit": q.limit,
    })
}

/// `GET /contacts` — filters + signed cursor page.
pub async fn list(
    db: &Db,
    ctx: &Ctx,
    hmac_key: &[u8],
    q: &ContactListQuery,
) -> Result<(Vec<ContactResponse>, Page), AppError> {
    require_crm_read(ctx)?;
    let filters = filters_json(q);
    let after = util::decode_cursor(q.cursor.as_deref(), hmac_key, ctx, &filters)?;
    let filter = ContactFilter {
        q: q.q.as_deref().map(dcrm::escape_like),
        company_id: q.company_id,
        status: q.status.clone(),
        owner_principal_id: q.owner_principal_id,
        archived: q.archived,
    };
    let mut tx = db.begin(ctx.workspace_id).await?;
    let mut rows = crm::list_contacts(&mut tx, &filter, after, q.limit + 1).await?;
    let page = util::page_for(&mut rows, q.limit, hmac_key, ctx, &filters, |r| {
        (r.created_at, r.id)
    });
    // Hydrate emails for the page in one query + one eligibility query.
    let ids: Vec<Uuid> = rows.iter().map(|r| r.id).collect();
    let links = crm::contact_emails_batch(&mut tx, &ids).await?;
    let address_ids: Vec<Uuid> = links.iter().map(|l| l.email_address_id).collect();
    let eligibility = crm::marketing_eligibility(&mut tx, &address_ids).await?;
    tx.commit().await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in &rows {
        // List rows keep the response light: `linked_contact_ids` is
        // resolved on the detail call (GET {id}); eligibility is batch-loaded.
        let emails = links
            .iter()
            .filter(|l| l.contact_id == row.id)
            .map(|l| {
                let codes = eligibility
                    .get(&l.email_address_id)
                    .cloned()
                    .unwrap_or_default();
                contracts::crm::ContactEmailItem {
                    email_address_id: l.email_address_id,
                    address: l.address_original.clone(),
                    label: l.label.clone(),
                    is_primary: l.is_primary,
                    verification_status: util::verification_status(&l.verification_status),
                    linked_contact_ids: vec![],
                    marketing_eligibility: contracts::crm::MarketingEligibility {
                        eligible: codes.is_empty(),
                        reason_codes: codes,
                    },
                }
            })
            .collect();
        out.push(util::contact_dto(row, emails));
    }
    Ok((out, page))
}

/// `GET /contacts/{id}` — detail with full email context (shared links +
/// marketing eligibility per B.3.4).
pub async fn get(db: &Db, ctx: &Ctx, id: Uuid) -> Result<ContactResponse, AppError> {
    require_crm_read(ctx)?;
    let mut tx = db.begin(ctx.workspace_id).await?;
    let Some(row) = crm::get_contact(&mut tx, id).await? else {
        return Err(AppError::not_found("contact not found"));
    };
    let links = crm::contact_emails(&mut tx, id).await?;
    let emails = util::contact_email_items(&mut tx, &links).await?;
    tx.commit().await?;
    Ok(util::contact_dto(&row, emails))
}

// ----------------------------------------------------------------- writers

fn validate_contact_insert(
    req: &CreateContactRequest,
    owner: Option<Uuid>,
) -> Result<ContactInsert, AppError> {
    util::require_name(&req.name, "name")?;
    Ok(ContactInsert {
        name: req.name.trim().to_string(),
        company_id: req.company_id,
        job_title: req.job_title.clone(),
        department: req.department.clone(),
        owner_principal_id: owner,
        status: req
            .status
            .unwrap_or(contracts::crm::ContactStatus::New)
            .as_str()
            .to_string(),
    })
}

async fn check_company_ref(tx: &mut Tx<'_>, company_id: Option<Uuid>) -> Result<(), AppError> {
    if let Some(id) = company_id
        && crm::get_company(tx, id).await?.is_none()
    {
        return Err(util::bad_request(
            "company_id does not reference a company",
            vec![util::violation("company_id", "NOT_FOUND")],
        ));
    }
    Ok(())
}

/// `POST /contacts` — optional `Idempotency-Key` (synchronous create).
/// Agent direct creation is rejected (B.3.3 — /lead-proposals).
pub async fn create(
    db: &Db,
    ctx: &Ctx,
    req: &CreateContactRequest,
    idem_key: Option<String>,
) -> Result<Stored<ContactResponse>, AppError> {
    require_crm_write(ctx)?;
    authz::require_not_agent_for(ctx, "create contacts directly")?;
    authz::require_human(ctx)?;
    if let Some(owner) = req.owner_principal_id
        && owner != ctx.principal_id
        && !matches!(ctx.role, Role::Admin | Role::Manager)
    {
        return Err(AppError::forbidden(
            "assigning another owner requires manager or admin role",
        ));
    }
    let insert = validate_contact_insert(req, req.owner_principal_id)?;
    let body = serde_json::to_value(req)
        .map_err(|e| AppError::internal(format!("request serialize: {e}")))?;
    let ctx_owned = ctx.clone();
    util::run_maybe_idempotent(
        db,
        ctx,
        "POST /api/v1/contacts",
        idem_key,
        &body,
        201,
        move |mut tx| async move {
            let r = create_in_tx(&mut tx, &ctx_owned, insert).await?;
            Ok((tx, r))
        },
    )
    .await
}

async fn create_in_tx(
    tx: &mut Tx<'_>,
    ctx: &Ctx,
    mut insert: ContactInsert,
) -> Result<ContactResponse, AppError> {
    check_company_ref(tx, insert.company_id).await?;
    insert.owner_principal_id = util::resolve_owner(tx, insert.owner_principal_id).await?;
    let row = crm::insert_contact(tx, &insert).await?;
    audit::record(
        tx,
        AuditEntry::new(
            ctx.principal_id,
            "contact.create",
            "contact",
            ctx.request_id.clone(),
        )
        .with_target(row.id),
    )
    .await?;
    util::write_activity(
        tx,
        crm::ActivityInsert {
            company_id: row.company_id,
            contact_id: Some(row.id),
            actor_principal_id: Some(ctx.principal_id),
            kind: activity_kind::CONTACT_CREATED,
            source_event_id: row.id,
            occurred_at: row.created_at,
            summary: serde_json::json!({"name": row.name}),
            ..Default::default()
        },
    )
    .await?;
    util::emit_change(tx, "contact.created", "contact", row.id, row.version).await?;
    Ok(util::contact_dto(&row, vec![]))
}

fn apply_contact_patch(
    row: &ContactRow,
    req: &PatchContactRequest,
) -> Result<ContactInsert, AppError> {
    if let Some(name) = &req.name {
        util::require_name(name, "name")?;
    }
    Ok(ContactInsert {
        name: req
            .name
            .clone()
            .map(|n| n.trim().to_string())
            .unwrap_or_else(|| row.name.clone()),
        company_id: match &req.company_id {
            Some(v) => *v,
            None => row.company_id,
        },
        job_title: match &req.job_title {
            Some(v) => v.clone(),
            None => row.job_title.clone(),
        },
        department: match &req.department {
            Some(v) => v.clone(),
            None => row.department.clone(),
        },
        owner_principal_id: row.owner_principal_id,
        status: req
            .status
            .map(|s| s.as_str().to_string())
            .unwrap_or_else(|| row.status.clone()),
    })
}

/// `PATCH /contacts/{id}` — A.6.1 conditional update + If-Match.
pub async fn patch(
    db: &Db,
    ctx: &Ctx,
    id: Uuid,
    expected_version: i64,
    req: &PatchContactRequest,
) -> Result<ContactResponse, AppError> {
    require_crm_write(ctx)?;
    authz::require_not_agent_for(ctx, "modify contacts")?;
    let mut tx = db.begin(ctx.workspace_id).await?;
    let row = crm::get_contact(&mut tx, id)
        .await?
        .ok_or_else(|| AppError::not_found("contact not found"))?;
    authz::crm_can_edit(ctx, row.owner_principal_id)?;
    let merged = apply_contact_patch(&row, req)?;
    check_company_ref(&mut tx, merged.company_id).await?;
    let Some(updated) =
        crm::update_contact_versioned(&mut tx, id, expected_version, &merged).await?
    else {
        let _ = tx.rollback().await;
        return Err(AppError::new(
            ErrorCode::VersionMismatch,
            "contact was modified by another request",
        ));
    };
    audit::record(
        &mut tx,
        AuditEntry::new(
            ctx.principal_id,
            "contact.patch",
            "contact",
            ctx.request_id.clone(),
        )
        .with_target(id),
    )
    .await?;
    util::emit_change(&mut tx, "contact.updated", "contact", id, updated.version).await?;
    let links = crm::contact_emails(&mut tx, id).await?;
    let emails = util::contact_email_items(&mut tx, &links).await?;
    tx.commit().await?;
    Ok(util::contact_dto(&updated, emails))
}

// -------------------------------------------------------- email addressing

/// `POST /contacts/{id}/emails` — B.3.4. Normalizes the address, upserts the
/// workspace `email_addresses` row, links it to the contact and reports the
/// shared links + marketing eligibility. Re-adding the same link is a 200
/// replay (`created = false` → still 201 shape, link already existed).
pub async fn add_email(
    db: &Db,
    ctx: &Ctx,
    contact_id: Uuid,
    req: &AddContactEmailRequest,
    idem_key: Option<String>,
) -> Result<Stored<AddContactEmailResponse>, AppError> {
    require_crm_write(ctx)?;
    let body = serde_json::to_value(req)
        .map_err(|e| AppError::internal(format!("request serialize: {e}")))?;
    let ctx_owned = ctx.clone();
    util::run_maybe_idempotent(
        db,
        ctx,
        "POST /api/v1/contacts/{id}/emails",
        idem_key,
        &body,
        201,
        move |mut tx| async move {
            let r = add_email_in_tx(&mut tx, &ctx_owned, contact_id, req).await?;
            Ok((tx, r))
        },
    )
    .await
}

async fn add_email_in_tx(
    tx: &mut Tx<'_>,
    ctx: &Ctx,
    contact_id: Uuid,
    req: &AddContactEmailRequest,
) -> Result<AddContactEmailResponse, AppError> {
    let contact = crm::get_contact(tx, contact_id)
        .await?
        .ok_or_else(|| AppError::not_found("contact not found"))?;
    authz::crm_can_edit(ctx, contact.owner_principal_id)?;
    // §6.3 workspace-fixed local-part policy.
    let policy = dcrm::local_part_policy(crm::workspace_settings(tx).await?.as_ref());
    let normalized = email_address::normalize(&req.address, policy).map_err(|e| {
        util::bad_request(
            format!("invalid email address: {e}"),
            vec![util::violation("address", "FORMAT")],
        )
    })?;
    let kind = req.kind.unwrap_or(contracts::crm::AddressKind::Unknown);
    let addr = crm::upsert_email_address(
        tx,
        &normalized.original,
        &normalized.normalized_key,
        normalized.normalization_version,
        kind.as_str(),
    )
    .await?;
    let is_primary = req.is_primary.unwrap_or(false);
    let (_link_id, _created) =
        crm::link_contact_email(tx, contact_id, addr.id, req.label.as_deref(), is_primary).await?;
    crm::touch_contact(tx, contact_id).await?;
    // Re-read the link so an existing link's actual primary flag survives.
    let actual_primary = crm::contact_emails(tx, contact_id)
        .await?
        .iter()
        .find(|l| l.email_address_id == addr.id)
        .map(|l| l.is_primary)
        .unwrap_or(is_primary);
    let row = crm::ContactEmailRow {
        email_address_id: addr.id,
        contact_id,
        address_original: addr.address_original,
        label: req.label.clone(),
        is_primary: actual_primary,
        verification_status: addr.verification_status,
    };
    let linked: Vec<Uuid> = crm::contacts_for_address(tx, addr.id)
        .await?
        .into_iter()
        .filter(|c| *c != contact_id)
        .collect();
    let codes = crm::marketing_eligibility(tx, &[addr.id])
        .await?
        .remove(&addr.id)
        .unwrap_or_default();
    audit::record(
        tx,
        AuditEntry::new(
            ctx.principal_id,
            "contact.email_add",
            "contact",
            ctx.request_id.clone(),
        )
        .with_target(contact_id),
    )
    .await?;
    util::write_activity(
        tx,
        crm::ActivityInsert {
            company_id: contact.company_id,
            contact_id: Some(contact_id),
            actor_principal_id: Some(ctx.principal_id),
            kind: activity_kind::CONTACT_EMAIL_ADDED,
            source_event_id: addr.id,
            occurred_at: Utc::now(),
            // Never log/emit the raw address beyond ids — the row itself is
            // reachable through the contact detail for authorized readers.
            summary: serde_json::json!({"email_address_id": addr.id}),
            ..Default::default()
        },
    )
    .await?;
    let updated_contact = crm::get_contact(tx, contact_id)
        .await?
        .ok_or_else(|| AppError::not_found("contact not found"))?;
    util::emit_change(
        tx,
        "contact.email_linked",
        "contact",
        contact_id,
        updated_contact.version,
    )
    .await?;
    Ok(util::add_email_dto(&row, linked, codes))
}

/// `DELETE /contacts/{id}/emails/{email_address_id}` — removes the link (the
/// `email_addresses` row stays: identity is workspace-wide, §6.3). Removing
/// the primary link promotes the oldest remaining link to keep the
/// ≤1-primary invariant meaningful. If-Match guards the contact version.
pub async fn remove_email(
    db: &Db,
    ctx: &Ctx,
    contact_id: Uuid,
    email_address_id: Uuid,
    expected_version: i64,
) -> Result<ContactResponse, AppError> {
    require_crm_write(ctx)?;
    let mut tx = db.begin(ctx.workspace_id).await?;
    let contact = crm::get_contact(&mut tx, contact_id)
        .await?
        .ok_or_else(|| AppError::not_found("contact not found"))?;
    authz::crm_can_edit(ctx, contact.owner_principal_id)?;
    let links = crm::contact_emails(&mut tx, contact_id).await?;
    let Some(link) = links
        .iter()
        .find(|l| l.email_address_id == email_address_id)
    else {
        return Err(AppError::not_found("email link not found"));
    };
    let was_primary = link.is_primary;
    let removed = crm::unlink_contact_email(&mut tx, contact_id, email_address_id).await?;
    if !removed {
        return Err(AppError::not_found("email link not found"));
    }
    if was_primary
        && let Some(next) = links
            .iter()
            .find(|l| l.email_address_id != email_address_id)
    {
        crm::set_primary_email(&mut tx, contact_id, next.email_address_id).await?;
    }
    if !crm::touch_contact_versioned(&mut tx, contact_id, expected_version).await? {
        let _ = tx.rollback().await;
        return Err(AppError::new(
            ErrorCode::VersionMismatch,
            "contact was modified by another request",
        ));
    }
    audit::record(
        &mut tx,
        AuditEntry::new(
            ctx.principal_id,
            "contact.email_remove",
            "contact",
            ctx.request_id.clone(),
        )
        .with_target(contact_id),
    )
    .await?;
    // Re-read so the response carries the post-bump version (ETag
    // consumers chain If-Match off this value).
    let contact = crm::get_contact(&mut tx, contact_id)
        .await?
        .ok_or_else(|| AppError::not_found("contact not found"))?;
    let links = crm::contact_emails(&mut tx, contact_id).await?;
    let emails = util::contact_email_items(&mut tx, &links).await?;
    util::emit_change(
        &mut tx,
        "contact.email_unlinked",
        "contact",
        contact_id,
        contact.version,
    )
    .await?;
    tx.commit().await?;
    Ok(util::contact_dto(&contact, emails))
}

/// `PUT /contacts/{id}/primary-email` — switches the primary link.
/// If-Match is required (mutation on a versioned resource).
pub async fn set_primary_email(
    db: &Db,
    ctx: &Ctx,
    contact_id: Uuid,
    expected_version: i64,
    req: &SetPrimaryEmailRequest,
) -> Result<ContactResponse, AppError> {
    require_crm_write(ctx)?;
    let mut tx = db.begin(ctx.workspace_id).await?;
    let contact = crm::get_contact(&mut tx, contact_id)
        .await?
        .ok_or_else(|| AppError::not_found("contact not found"))?;
    authz::crm_can_edit(ctx, contact.owner_principal_id)?;
    if contact.version != expected_version {
        return Err(AppError::new(
            ErrorCode::VersionMismatch,
            "contact was modified by another request",
        ));
    }
    let ok = crm::set_primary_email(&mut tx, contact_id, req.email_address_id).await?;
    if !ok {
        return Err(util::bad_request(
            "email_address_id is not linked to this contact",
            vec![util::violation("email_address_id", "NOT_LINKED")],
        ));
    }
    if !crm::touch_contact_versioned(&mut tx, contact_id, expected_version).await? {
        let _ = tx.rollback().await;
        return Err(AppError::new(
            ErrorCode::VersionMismatch,
            "contact was modified by another request",
        ));
    }
    audit::record(
        &mut tx,
        AuditEntry::new(
            ctx.principal_id,
            "contact.primary_email",
            "contact",
            ctx.request_id.clone(),
        )
        .with_target(contact_id),
    )
    .await?;
    // Re-read so the response carries the post-bump version (ETag
    // consumers chain If-Match off this value).
    let contact = crm::get_contact(&mut tx, contact_id)
        .await?
        .ok_or_else(|| AppError::not_found("contact not found"))?;
    let links = crm::contact_emails(&mut tx, contact_id).await?;
    let emails = util::contact_email_items(&mut tx, &links).await?;
    util::emit_change(
        &mut tx,
        "contact.primary_email_changed",
        "contact",
        contact_id,
        contact.version,
    )
    .await?;
    tx.commit().await?;
    Ok(util::contact_dto(&contact, emails))
}
