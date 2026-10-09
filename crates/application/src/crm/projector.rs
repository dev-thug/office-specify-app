//! Outbox → `activities` projection (integration contracts "Activities
//! projection"). `crm.activity_projector` subscribes to the six CRM-relevant
//! event types; each event becomes exactly one row anchored by
//! `source_event_id = event.id` (UNIQUE ⇒ re-delivery is a no-op).
//!
//! Payloads carry ids only (§16.2); the projector resolves domain linkage
//! from shared tables — never copies subject/body/addresses into `summary`
//! (§5.6). Mail-derived rows set `mail_account_id` so the read path's
//! mailbox-grant filter applies.

use contracts::AppError;
use domain::crm::activity_kind;
use infrastructure::db::Tx;
use infrastructure::outbox::OutboxRow;
use infrastructure::repo::crm::{self, ActivityInsert};
use uuid::Uuid;

pub const EVENT_TYPES: &[&str] = &[
    activity_kind::LEAD_ACCEPTED,
    activity_kind::APPROVAL_DECIDED,
    activity_kind::OUTBOUND_ACCEPTED,
    activity_kind::MESSAGE_RECEIVED,
    activity_kind::SUPPRESSION_CREATED,
    activity_kind::BOOKING_CONFIRMED,
];

fn uuid_field(payload: &serde_json::Value, field: &str) -> Option<Uuid> {
    payload
        .get(field)
        .and_then(|v| v.as_str())
        .and_then(|s| Uuid::parse_str(s).ok())
}

fn str_field<'a>(payload: &'a serde_json::Value, field: &str) -> Option<&'a str> {
    payload.get(field).and_then(|v| v.as_str())
}

/// Consumer entry point — runs inside the processed_events transaction.
/// Unknown event types are ignored (defensive; subscription filters already).
pub async fn handle(tx: &mut Tx<'_>, event: &OutboxRow) -> Result<(), AppError> {
    let insert = match event.event_type.as_str() {
        "lead.accepted" => lead_accepted(tx, event).await?,
        "approval.decided" => approval_decided(tx, event).await?,
        "outbound.accepted" => outbound_accepted(tx, event).await?,
        "message.received" => message_received(tx, event).await?,
        "suppression.created" => suppression_created(tx, event).await?,
        "booking.confirmed" => booking_confirmed(tx, event).await?,
        _ => return Ok(()),
    };
    if let Some(a) = insert {
        crm::insert_activity(tx, &a).await?;
    }
    Ok(())
}

fn base(event: &OutboxRow, kind: &'static str) -> ActivityInsert {
    ActivityInsert {
        kind,
        source_event_id: event.id,
        occurred_at: event.created_at,
        summary: serde_json::json!({}),
        ..Default::default()
    }
}

/// `lead.accepted` `{lead_proposal_id, company_id, contact_id?}` → timeline
/// on the accepted company. Actor = the human reviewer.
async fn lead_accepted(
    tx: &mut Tx<'_>,
    event: &OutboxRow,
) -> Result<Option<ActivityInsert>, AppError> {
    let p = &event.payload;
    let company_id = uuid_field(p, "company_id");
    if company_id.is_none() {
        return Ok(None);
    }
    let proposal_id = uuid_field(p, "lead_proposal_id");
    let mut actor = None;
    let mut company = company_id;
    if let Some(id) = proposal_id
        && let Some((accepted_company, reviewed_by)) = crm::lead_proposal_refs(tx, id).await?
    {
        company = accepted_company.or(company);
        actor = reviewed_by;
    }
    let mut a = base(event, activity_kind::LEAD_ACCEPTED);
    a.company_id = company;
    a.contact_id = uuid_field(p, "contact_id");
    a.actor_principal_id = actor;
    a.summary = serde_json::json!({"lead_proposal_id": proposal_id});
    Ok(Some(a))
}

/// `approval.decided` `{approval_request_id, target_type, target_id,
/// decision}` → timeline anchored to the best-resolvable CRM target. Actor =
/// the deciding human.
async fn approval_decided(
    tx: &mut Tx<'_>,
    event: &OutboxRow,
) -> Result<Option<ActivityInsert>, AppError> {
    let p = &event.payload;
    let request_id = uuid_field(p, "approval_request_id");
    let target_type = str_field(p, "target_type").unwrap_or_default();
    let target_id = uuid_field(p, "target_id");
    let decision = str_field(p, "decision").unwrap_or_default();
    let mut a = base(event, activity_kind::APPROVAL_DECIDED);
    if let Some(rid) = request_id {
        a.actor_principal_id = crm::approval_decided_by(tx, rid).await?;
    }
    match (target_type, target_id) {
        ("message_submit", Some(outbound_id)) => {
            if let Some((mail_account_id, thread_id, to_addr, _)) =
                crm::outbound_refs(tx, outbound_id).await?
            {
                a.mail_account_id = Some(mail_account_id);
                a.thread_id = thread_id;
                if let Some((contact, company)) = crm::contact_refs_for_address(tx, to_addr).await?
                {
                    a.contact_id = Some(contact);
                    a.company_id = company;
                }
            }
        }
        ("booking_change", Some(booking_id)) => {
            a.booking_id = Some(booking_id);
            if let Some(guest) = crm::booking_guest_address(tx, booking_id).await?
                && let Some((contact, company)) = crm::contact_refs_for_address(tx, guest).await?
            {
                a.contact_id = Some(contact);
                a.company_id = company;
            }
        }
        ("suppression_release", Some(suppression_id)) => {
            if let Some((addr, _)) = crm::suppression_target(tx, suppression_id).await?
                && let Some((contact, company)) = crm::contact_refs_for_address(tx, addr).await?
            {
                a.contact_id = Some(contact);
                a.company_id = company;
            }
        }
        // campaign_launch and unknown targets: ids-only summary row.
        _ => {}
    }
    a.summary = serde_json::json!({
        "approval_request_id": request_id,
        "target_type": target_type,
        "target_id": target_id,
        "decision": decision,
    });
    Ok(Some(a))
}

/// `outbound.accepted` `{outbound_message_id, campaign_enrollment_id?,
/// email_address_id, thread_id?}` → mail-derived (mailbox-grant-gated) row.
/// Actor = the message requester.
async fn outbound_accepted(
    tx: &mut Tx<'_>,
    event: &OutboxRow,
) -> Result<Option<ActivityInsert>, AppError> {
    let p = &event.payload;
    let Some(outbound_id) = uuid_field(p, "outbound_message_id") else {
        return Ok(None);
    };
    let mut a = base(event, activity_kind::OUTBOUND_ACCEPTED);
    if let Some((mail_account_id, thread_id, to_addr, requested_by)) =
        crm::outbound_refs(tx, outbound_id).await?
    {
        a.mail_account_id = Some(mail_account_id);
        a.thread_id = thread_id.or_else(|| uuid_field(p, "thread_id"));
        a.actor_principal_id = Some(requested_by);
        let addr = uuid_field(p, "email_address_id").unwrap_or(to_addr);
        if let Some((contact, company)) = crm::contact_refs_for_address(tx, addr).await? {
            a.contact_id = Some(contact);
            a.company_id = company;
        }
    }
    a.summary = serde_json::json!({
        "outbound_message_id": outbound_id,
        "campaign_enrollment_id": uuid_field(p, "campaign_enrollment_id"),
    });
    Ok(Some(a))
}

/// `message.received` `{email_message_id, thread_id, mail_account_id,
/// from_email_address_id?, classification}` → gated mail-derived row.
async fn message_received(
    tx: &mut Tx<'_>,
    event: &OutboxRow,
) -> Result<Option<ActivityInsert>, AppError> {
    let p = &event.payload;
    let mut a = base(event, activity_kind::MESSAGE_RECEIVED);
    a.mail_account_id = uuid_field(p, "mail_account_id");
    a.thread_id = uuid_field(p, "thread_id");
    if let Some(thread) = a.thread_id
        && let Some((account, contact)) = crm::thread_refs(tx, thread).await?
    {
        a.mail_account_id = a.mail_account_id.or(Some(account));
        a.contact_id = contact;
    }
    if a.contact_id.is_none()
        && let Some(addr) = uuid_field(p, "from_email_address_id")
        && let Some((contact, company)) = crm::contact_refs_for_address(tx, addr).await?
    {
        a.contact_id = Some(contact);
        a.company_id = company;
    }
    if a.contact_id.is_some()
        && a.company_id.is_none()
        && let Some(contact) = a.contact_id
        && let Some(row) = crm::get_contact(tx, contact).await?
    {
        a.company_id = row.company_id;
    }
    a.summary = serde_json::json!({
        "email_message_id": uuid_field(p, "email_message_id"),
        "classification": str_field(p, "classification"),
    });
    Ok(Some(a))
}

/// `suppression.created` `{email_address_id, purpose_scope, reason}` → row on
/// the linked contact/company. Not mail-derived (no mailbox) — visible to
/// all CRM readers; reason codes are enum labels.
async fn suppression_created(
    tx: &mut Tx<'_>,
    event: &OutboxRow,
) -> Result<Option<ActivityInsert>, AppError> {
    let p = &event.payload;
    let Some(addr) = uuid_field(p, "email_address_id") else {
        return Ok(None);
    };
    let scope = str_field(p, "purpose_scope").unwrap_or("marketing");
    let mut a = base(event, activity_kind::SUPPRESSION_CREATED);
    if let Some((contact, company)) = crm::contact_refs_for_address(tx, addr).await? {
        a.contact_id = Some(contact);
        a.company_id = company;
    }
    a.actor_principal_id = crm::suppression_actor(tx, addr, scope).await?;
    a.summary = serde_json::json!({
        "email_address_id": addr,
        "purpose_scope": scope,
        "reason": str_field(p, "reason"),
    });
    Ok(Some(a))
}

/// `booking.confirmed` `{booking_id, company_id?, contact_id?}` → row on the
/// guest's contact/company. Actor = the host principal.
async fn booking_confirmed(
    tx: &mut Tx<'_>,
    event: &OutboxRow,
) -> Result<Option<ActivityInsert>, AppError> {
    let p = &event.payload;
    let Some(booking_id) = uuid_field(p, "booking_id") else {
        return Ok(None);
    };
    let mut a = base(event, activity_kind::BOOKING_CONFIRMED);
    a.booking_id = Some(booking_id);
    a.company_id = uuid_field(p, "company_id");
    a.contact_id = uuid_field(p, "contact_id");
    if let Some((host, guest_addr)) = crm::booking_refs(tx, booking_id).await? {
        a.actor_principal_id = Some(host);
        if a.contact_id.is_none()
            && let Some((contact, company)) = crm::contact_refs_for_address(tx, guest_addr).await?
        {
            a.contact_id = Some(contact);
            a.company_id = a.company_id.or(company);
        }
    }
    a.summary = serde_json::json!({"booking_id": booking_id});
    Ok(Some(a))
}
