//! Provider-message ingest (§9.1/§9.2/§9.4) — shared by `inbox.sync_mailbox`
//! and `inbox.process_webhook`. All writes happen in the caller's
//! transaction; dedupe anchors on `(mail_account_id, provider_message_id)`
//! so webhook/sync/replay overlap never duplicates a message (T14).
//!
//! Thread match order (§9.2): verified provider thread id →
//! `In-Reply-To`/`References` against stored `internet_message_id`s → a new
//! thread linked to the sender's CRM contact (explicit address link only —
//! never subject similarity).
//!
//! `body_html` is stored sanitized — the raw provider payload is never
//! persisted (§15.1; T26 sanitization also runs at DTO build so stored
//! content can be re-tightened).

use contracts::inbox::Classification;
use contracts::{AppError, ErrorCode};
use uuid::Uuid;

use infrastructure::db::Tx;
use infrastructure::providers::ProviderMessage;
use infrastructure::repo::inbox as repo;
use infrastructure::{audit, audit::AuditEntry, outbox};

pub struct IngestOutcome {
    /// `Some` only for a newly inserted message (dedupe miss).
    pub message_id: Option<Uuid>,
    pub thread_id: Uuid,
    pub classification: Classification,
}

/// Upsert one provider-observed inbound message. `actor`/`job_id` decorate
/// the audit row (worker = system principal + job id).
pub async fn ingest_provider_message(
    tx: &mut Tx<'_>,
    job_id: Option<Uuid>,
    mail_account_id: Uuid,
    msg: &ProviderMessage,
) -> Result<IngestOutcome, AppError> {
    // Dedupe first — replayed webhook/sync overlaps are free no-ops.
    if let Some(existing) =
        repo::message_by_provider_id(tx, mail_account_id, &msg.provider_message_id).await?
    {
        return Ok(IngestOutcome {
            message_id: None,
            thread_id: existing.thread_id,
            classification: Classification::parse(&existing.classification)
                .unwrap_or(Classification::Unknown),
        });
    }

    // Sender address row (§6.3 normalization) — feeds thread contact link,
    // the `from` participant, `message.received` and pause jobs.
    let from_norm = domain::email_address::normalize(
        &msg.from,
        domain::email_address::LocalPartPolicy::Lowercase,
    );
    let from_address_id = match &from_norm {
        Ok(n) => Some(repo::upsert_email_address(tx, &n.normalized_key, &n.original).await?),
        Err(_) => None,
    };
    let contact_id = match from_address_id {
        Some(id) => repo::contact_for_address(tx, id).await?,
        None => None,
    };

    // Thread resolution (§9.2 order).
    let thread_id = if let Some(pt) = msg.provider_thread_id.as_deref() {
        match repo::thread_by_provider_id(tx, mail_account_id, pt).await? {
            Some(t) => t,
            None => repo::create_thread(tx, mail_account_id, Some(pt), contact_id).await?,
        }
    } else {
        let mut refs = msg.references.clone();
        if let Some(irt) = &msg.in_reply_to {
            refs.push(irt.clone());
        }
        match repo::thread_by_references(tx, mail_account_id, &refs).await? {
            Some(t) => t,
            None => repo::create_thread(tx, mail_account_id, None, contact_id).await?,
        }
    };

    let classification = domain::inbox::classify_inbound(
        msg.subject.as_deref(),
        msg.body_text.as_deref(),
        &msg.from,
    );

    let inserted = repo::insert_message_if_new(
        tx,
        mail_account_id,
        thread_id,
        "inbound",
        msg,
        classification.as_str(),
    )
    .await?;
    let Some(message_id) = inserted else {
        // Lost the insert race with a concurrent ingest (unique index
        // decided) — report as duplicate.
        let existing = repo::message_by_provider_id(tx, mail_account_id, &msg.provider_message_id)
            .await?
            .ok_or_else(|| AppError::new(ErrorCode::Conflict, "message ingest conflict"))?;
        return Ok(IngestOutcome {
            message_id: None,
            thread_id: existing.thread_id,
            classification: Classification::parse(&existing.classification)
                .unwrap_or(Classification::Unknown),
        });
    };

    // Participants: from + to (+reply_to when present), each linked to its
    // normalized address row where the address parses.
    repo::insert_participant(tx, message_id, "from", &msg.from, None, from_address_id).await?;
    for to in &msg.to {
        let addr_id = match domain::email_address::normalize(
            to,
            domain::email_address::LocalPartPolicy::Lowercase,
        ) {
            Ok(n) => Some(repo::upsert_email_address(tx, &n.normalized_key, &n.original).await?),
            Err(_) => None,
        };
        repo::insert_participant(tx, message_id, "to", to, None, addr_id).await?;
    }

    repo::advance_last_inbound(tx, thread_id, message_id, msg.message_at).await?;

    outbox::emit(
        tx,
        "message.received",
        "email_message",
        message_id,
        1,
        serde_json::json!({
            "email_message_id": message_id,
            "thread_id": thread_id,
            "mail_account_id": mail_account_id,
            "from_email_address_id": from_address_id,
            "classification": classification.as_str(),
        }),
    )
    .await?;

    // §9.4 safety stop: pause follow-ups on human replies, unsubscribe
    // language and auto-replies. A reply's text never creates a suppression
    // itself — a human confirms scope (or the unsubscribe endpoints run the
    // compliance path).
    if let (Some(reason), Some(addr)) = (classification.pause_reason(), from_address_id) {
        repo::enqueue_coalesced(
            tx,
            "campaigns.pause_address",
            &serde_json::json!({
                "email_address_id": addr,
                "reason": reason,
            }),
            &format!("pause_address:{addr}:{reason}"),
        )
        .await?;
    }

    audit::record(
        tx,
        AuditEntry {
            actor_principal_id: None,
            requested_by: None,
            approved_by: None,
            job_id,
            action: "inbox.message_received".into(),
            target_type: "email_message".into(),
            target_id: Some(message_id),
            outcome: "ok".into(),
            metadata: serde_json::json!({
                "thread_id": thread_id,
                "mail_account_id": mail_account_id,
                "classification": classification.as_str(),
            }),
            request_id: job_id.map(|j| j.to_string()).unwrap_or_default(),
            trace_id: None,
        },
    )
    .await?;

    Ok(IngestOutcome {
        message_id: Some(message_id),
        thread_id,
        classification,
    })
}
