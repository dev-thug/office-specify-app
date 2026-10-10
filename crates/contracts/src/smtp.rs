//! Office-managed SMTP connection API contracts.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

/// `POST /smtp-connections` — credential is accepted on write only.
///
/// Deliberately does not implement `Debug`: request bodies can contain a
/// password and should not be printed by handlers or diagnostics.
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateSmtpConnectionRequest {
    pub name: String,
    pub mail_account_id: Uuid,
    pub host: String,
    pub port: u16,
    /// `tls` or `starttls`.
    pub security: String,
    pub username: String,
    #[schema(value_type = String, format = Password, write_only = true)]
    pub password: String,
    pub from_address: String,
    pub from_name: String,
    #[serde(default)]
    pub reply_to: Option<String>,
    #[serde(default)]
    pub custom_headers: BTreeMap<String, String>,
    /// Required administrator attestation that this sender address is a
    /// registered sender or authorized alias.
    pub sender_confirmed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SmtpConnectionResponse {
    pub id: Uuid,
    pub name: String,
    pub mail_account_id: Uuid,
    pub host: String,
    pub port: u16,
    /// `tls` or `starttls`.
    pub security: String,
    pub username: String,
    pub from_address: String,
    pub from_name: String,
    pub reply_to: Option<String>,
    pub custom_headers: BTreeMap<String, String>,
    /// `pending` | `active` | `disabled` | `reauth_required`.
    pub status: String,
    pub last_error_code: Option<String>,
    pub channel_id: Uuid,
    pub sender_identity_id: Uuid,
    pub version: i64,
    pub has_password: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_json() -> serde_json::Value {
        serde_json::json!({
            "name": "Office SMTP",
            "mail_account_id": Uuid::new_v4(),
            "host": "smtp.example.test",
            "port": 587,
            "security": "starttls",
            "username": "smtp-user",
            "password": "write-only-secret",
            "from_address": "news@example.test",
            "from_name": "Office News",
            "sender_confirmed": true
        })
    }

    #[test]
    fn sender_confirmation_is_required_in_create_request() {
        let mut body = create_json();
        body.as_object_mut().unwrap().remove("sender_confirmed");
        assert!(serde_json::from_value::<CreateSmtpConnectionRequest>(body).is_err());
    }

    #[test]
    fn response_never_contains_the_password() {
        let secret = "write-only-secret";
        let request: CreateSmtpConnectionRequest =
            serde_json::from_value(create_json()).expect("valid request");
        assert_eq!(request.password, secret);

        let response = SmtpConnectionResponse {
            id: Uuid::new_v4(),
            name: "Office SMTP".into(),
            mail_account_id: request.mail_account_id,
            host: request.host,
            port: request.port,
            security: request.security,
            username: request.username,
            from_address: request.from_address,
            from_name: request.from_name,
            reply_to: request.reply_to,
            custom_headers: request.custom_headers,
            status: "pending".into(),
            last_error_code: None,
            channel_id: Uuid::new_v4(),
            sender_identity_id: Uuid::new_v4(),
            version: 1,
            has_password: true,
        };
        let json = serde_json::to_value(&response).expect("serializable response");
        assert!(!json.to_string().contains(secret));
        assert!(json.get("password").is_none());
        assert!(json.get("secret_ref").is_none());
    }
}
