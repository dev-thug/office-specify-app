//! Provider-message ingest (§9.1/§9.2/§9.4) — shared by `inbox.sync_mailbox`
//! and `inbox.process_webhook`. All writes happen in the caller's
//! transaction; dedupe anchors on `(mail_account_id, provider_message_id)`
//! so webhook/sync/replay overlap never duplicates a message (T14).
//!
//! Thread match order: exact mailbox-local outbound Message-ID plus
//! normalized sender/recipient match → provider thread ID → exact ID reference
//! to a stored inbound message → a new thread linked to an explicit CRM
//! address association. Subject similarity is never used.
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
    // Provider adapters parse RFC headers, but normalize them again at the
    // ingest boundary so malformed or non-canonical IDs never become a
    // correlation key even when a new adapter is added later.
    let mut normalized_msg = msg.clone();
    normalized_msg.internet_message_id = msg
        .internet_message_id
        .as_deref()
        .and_then(infrastructure::providers::mail_headers::parse_single_message_id);
    normalized_msg.in_reply_to = msg
        .in_reply_to
        .as_deref()
        .and_then(infrastructure::providers::mail_headers::parse_single_message_id);
    normalized_msg.references.clear();
    for reference in &msg.references {
        if let Some(reference) =
            infrastructure::providers::mail_headers::parse_single_message_id(reference)
            && !normalized_msg.references.contains(&reference)
        {
            normalized_msg.references.push(reference);
        }
    }

    // Dedupe first — replayed webhook/sync overlaps are free no-ops.
    if let Some(existing) =
        repo::message_by_provider_id(tx, mail_account_id, &normalized_msg.provider_message_id)
            .await?
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
        &normalized_msg.from,
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
    let outbound_target = repo::outbound_reply_target(
        tx,
        mail_account_id,
        normalized_msg.in_reply_to.as_deref(),
        &normalized_msg.references,
    )
    .await?;
    let thread_id = if let Some(target) = outbound_target {
        if same_email_address(&normalized_msg.from, &target.recipient_address) {
            target.thread_id
        } else {
            // A forged or unexpected From must not fall through to a provider
            // thread id or an ancestor in References.
            repo::create_thread(tx, mail_account_id, None, contact_id).await?
        }
    } else if let Some(pt) = normalized_msg.provider_thread_id.as_deref() {
        match repo::thread_by_provider_id(tx, mail_account_id, pt).await? {
            Some(t) => t,
            None => repo::create_thread(tx, mail_account_id, Some(pt), contact_id).await?,
        }
    } else {
        // In-Reply-To is the exact target; absent that, the most recent
        // validated References entry is considered.
        let mut refs = Vec::new();
        if let Some(irt) = &normalized_msg.in_reply_to {
            refs.push(irt.clone());
            refs.extend(normalized_msg.references.iter().cloned());
        } else {
            refs.extend(normalized_msg.references.iter().rev().cloned());
        }
        match repo::thread_by_references(tx, mail_account_id, &refs).await? {
            Some(t) => t,
            None => repo::create_thread(tx, mail_account_id, None, contact_id).await?,
        }
    };

    let classification = domain::inbox::classify_inbound(
        normalized_msg.subject.as_deref(),
        normalized_msg.body_text.as_deref(),
        &normalized_msg.from,
    );

    let inserted = repo::insert_message_if_new(
        tx,
        mail_account_id,
        thread_id,
        "inbound",
        &normalized_msg,
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
    repo::insert_participant(
        tx,
        message_id,
        "from",
        &normalized_msg.from,
        None,
        from_address_id,
    )
    .await?;
    for to in &normalized_msg.to {
        let addr_id = match domain::email_address::normalize(
            to,
            domain::email_address::LocalPartPolicy::Lowercase,
        ) {
            Ok(n) => Some(repo::upsert_email_address(tx, &n.normalized_key, &n.original).await?),
            Err(_) => None,
        };
        repo::insert_participant(tx, message_id, "to", to, None, addr_id).await?;
    }

    repo::advance_last_inbound(tx, thread_id, message_id, normalized_msg.message_at).await?;

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

fn same_email_address(left: &str, right: &str) -> bool {
    let policy = domain::email_address::LocalPartPolicy::Lowercase;
    let Ok(left) = domain::email_address::normalize(left, policy) else {
        return false;
    };
    let Ok(right) = domain::email_address::normalize(right, policy) else {
        return false;
    };
    left.normalized_key == right.normalized_key
}
