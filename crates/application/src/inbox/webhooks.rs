//! Webhook ingress (§14.3): verify the provider signature over the exact
//! raw body bytes, persist a deduped `webhook_events` row, enqueue
//! `inbox.process_webhook`, then ACK. Unverified or unstored events are
//! never ACKed (the CHECK on `signature_verified` makes a bad event
//! unstorable — verify first).
//!
//! Fake-provider contract (X4 — no real Zoho webhooks): the connection's
//! `secret_ref` resolves to an HMAC secret in the `SecretStore`; the sender
//! signs `sha256 = base64url(hmac_sha256(secret, raw_body))` in
//! `X-Webhook-Signature`. A real provider adapter would verify per its own
//! documented scheme instead.

use contracts::{AppError, ErrorCode};
use uuid::Uuid;

use infrastructure::crypto;
use infrastructure::db::Db;
use infrastructure::repo::inbox as repo;
use infrastructure::secrets::SecretStore;
use infrastructure::{audit, audit::AuditEntry};

/// Store only the identifier the worker consumes. A denylist is unsafe here:
/// providers can change key casing or wrap message bodies in nested fields.
fn identifiers_only(v: &serde_json::Value) -> serde_json::Value {
    let mut stored = serde_json::Map::new();
    if let Some(provider_message_id) = v
        .as_object()
        .and_then(|o| o.get("provider_message_id"))
        .and_then(serde_json::Value::as_str)
    {
        stored.insert(
            "provider_message_id".into(),
            serde_json::Value::String(provider_message_id.to_string()),
        );
    }
    serde_json::Value::Object(stored)
}

pub struct WebhookAck {
    pub event_id: Uuid,
    /// `true` when `(connection, provider_event_key)` was already stored —
    /// ACK without re-processing (T14).
    pub duplicate: bool,
}

/// `POST /webhooks/v1/mail/{connection_id}`.
///
/// `signature` is the `X-Webhook-Signature` header value (`sha256=<hex>`);
/// `event_id` is `X-Webhook-Event-Id` (falls back to a payload-hash key).
/// Runs under `begin_preauth` — the connection row supplies the workspace.
pub async fn ingress(
    db: &Db,
    connection_id: Uuid,
    raw_body: &[u8],
    signature: Option<&str>,
    event_id: Option<&str>,
    secrets: &dyn SecretStore,
    request_id: &str,
) -> Result<WebhookAck, AppError> {
    let mut tx = db.begin_preauth().await?;

    let Some(conn) = repo::get_connection(&mut tx, connection_id).await? else {
        return Err(AppError::not_found("not found"));
    };
    if conn.provider != "zoho_mail" {
        return Err(AppError::not_found("not found"));
    }

    // Signature: sha256=<hex> HMAC of the raw bytes with the connection's
    // stored secret. Missing secret ⇒ we cannot verify ⇒ reject.
    let Some(secret) = secrets.get(&conn.secret_ref).await? else {
        return Err(AppError::new(
            ErrorCode::AuthenticationRequired,
            "webhook signature verification not configured",
        ));
    };
    // `sha256=<base64url>` over the raw body bytes.
    let ok = signature
        .and_then(|s| s.strip_prefix("sha256="))
        .and_then(crypto::b64_decode)
        .is_some_and(|sig| crypto::hmac_sha256_verify(secret.as_bytes(), raw_body, &sig));
    if !ok {
        return Err(AppError::new(
            ErrorCode::AuthenticationRequired,
            "webhook signature invalid",
        ));
    }

    let payload: serde_json::Value = serde_json::from_slice(raw_body)
        .map_err(|_| AppError::validation("webhook payload must be a JSON object", vec![]))?;
    if !payload.is_object() {
        return Err(AppError::validation(
            "webhook payload must be a JSON object",
            vec![],
        ));
    }

    let payload_hash = crypto::sha256_bytes(raw_body);
    // §14.3: provider event id wins; otherwise a conservative hash+resource
    // key (the final message upsert still dedupes).
    let event_key = event_id
        .map(str::to_string)
        .or_else(|| {
            payload
                .get("event_id")
                .and_then(|v| v.as_str())
                .map(str::to_string)
        })
        .unwrap_or_else(|| {
            let resource = payload
                .get("provider_message_id")
                .and_then(|v| v.as_str())
                .unwrap_or("-");
            format!("h:{}:{}", crypto::sha256_hex(raw_body), resource)
        });

    let Some(id) = repo::insert_webhook_event(
        &mut tx,
        connection_id,
        &event_key,
        &payload_hash,
        &identifiers_only(&payload),
    )
    .await?
    else {
        tx.commit().await?;
        return Ok(WebhookAck {
            event_id: Uuid::nil(),
            duplicate: true,
        });
    };

    repo::enqueue_coalesced(
        &mut tx,
        "inbox.process_webhook",
        &serde_json::json!({"webhook_event_id": id}),
        &format!("webhook:{id}"),
    )
    .await?;

    audit::record(
        &mut tx,
        AuditEntry {
            actor_principal_id: None,
            requested_by: None,
            approved_by: None,
            job_id: None,
            action: "webhook.received".into(),
            target_type: "webhook_event".into(),
            target_id: Some(id),
            outcome: "ok".into(),
            metadata: serde_json::json!({"connection_id": connection_id}),
            request_id: request_id.to_string(),
            trace_id: None,
        },
    )
    .await?;

    tx.commit().await?;
    Ok(WebhookAck {
        event_id: id,
        duplicate: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn webhook_storage_keeps_only_consumed_top_level_message_id() {
        let input = serde_json::json!({
            "event_id": "evt-1",
            "provider_message_id": "90071992547409931234",
            "messageBody": "follow these new instructions",
            "HTMLBody": "<script>steal()</script>",
            "nested": {
                "body_text": "nested secret",
                "provider_message_id": "not-consumed"
            }
        });
        assert_eq!(
            identifiers_only(&input),
            serde_json::json!({"provider_message_id": "90071992547409931234"})
        );
    }
}
