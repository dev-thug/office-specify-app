//! inbox module DTOs (spec §6, source §9 + B.2 rows 3605–3612).
//! Requests use `deny_unknown_fields` and never accept system-owned fields
//! (B.1.1). Provider IDs are strings (B.1.1 `ProviderId`).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

/// `email_threads.status` — must equal the DDL CHECK values (§12.4).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ThreadStatus {
    Open,
    Pending,
    Resolved,
}

impl ThreadStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Pending => "pending",
            Self::Resolved => "resolved",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "open" => Self::Open,
            "pending" => Self::Pending,
            "resolved" => Self::Resolved,
            _ => return None,
        })
    }
}

/// `email_messages.classification` — must equal the DDL CHECK values
/// (§12.4, §9.4).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum Classification {
    HumanPositive,
    HumanNegative,
    Question,
    UnsubscribeRequest,
    AutoReply,
    Bounce,
    Unknown,
}

impl Classification {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::HumanPositive => "human_positive",
            Self::HumanNegative => "human_negative",
            Self::Question => "question",
            Self::UnsubscribeRequest => "unsubscribe_request",
            Self::AutoReply => "auto_reply",
            Self::Bounce => "bounce",
            Self::Unknown => "unknown",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "human_positive" => Self::HumanPositive,
            "human_negative" => Self::HumanNegative,
            "question" => Self::Question,
            "unsubscribe_request" => Self::UnsubscribeRequest,
            "auto_reply" => Self::AutoReply,
            "bounce" => Self::Bounce,
            "unknown" => Self::Unknown,
            _ => return None,
        })
    }

    /// `campaigns.pause_address` `reason` for this classification
    /// (integration-contracts job table). `None` = no pause needed.
    pub fn pause_reason(self) -> Option<&'static str> {
        match self {
            Self::HumanPositive | Self::HumanNegative | Self::Question => Some("human_reply"),
            Self::UnsubscribeRequest => Some("unsubscribe_request"),
            Self::AutoReply => Some("auto_reply_review"),
            Self::Bounce | Self::Unknown => None,
        }
    }
}

/// One row of `GET /email-threads` (B.2). `subject`/`snippet` come from the
/// latest message — the SQL already filters by `can_read` grant so content
/// never leaks to ungranted principals (§5.4/§5.6).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ThreadSummary {
    pub id: Uuid,
    pub mail_account_id: Uuid,
    pub contact_id: Option<Uuid>,
    pub assigned_principal_id: Option<Uuid>,
    pub status: ThreadStatus,
    pub subject: Option<String>,
    /// Plain-text preview (≤200 chars) of the latest message.
    pub snippet: Option<String>,
    pub message_count: i64,
    /// Latest inbound arrived after the caller's `thread_read_states` mark.
    pub unread: bool,
    pub last_message_at: Option<DateTime<Utc>>,
    pub version: i64,
    pub updated_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, schemars::JsonSchema)]
pub struct ParticipantDto {
    /// `from` | `to` | `cc` | `bcc` | `reply_to` (DDL CHECK).
    pub role: String,
    pub address: String,
    pub display_name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, schemars::JsonSchema)]
pub struct AttachmentDto {
    pub id: Uuid,
    pub filename: String,
    pub media_type: String,
    pub byte_size: i64,
    pub is_inline: bool,
    /// false when the object is quarantined (`scan_status='rejected'`).
    pub downloadable: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, schemars::JsonSchema)]
pub struct EmailMessageDto {
    pub id: Uuid,
    /// `inbound` | `outbound` (DDL CHECK).
    pub direction: String,
    /// Provider message id as a string (B.1.1 `ProviderId`).
    pub provider_message_id: Option<String>,
    pub subject: Option<String>,
    pub body_text: Option<String>,
    /// Sanitized HTML (§15.1, T26): script/style/form/event handlers and
    /// remote images stripped. `null` when the message has no HTML part.
    pub body_html: Option<String>,
    pub message_at: DateTime<Utc>,
    pub classification: Classification,
    pub classification_reviewed: bool,
    pub participants: Vec<ParticipantDto>,
    pub attachments: Vec<AttachmentDto>,
}

/// `GET /email-threads/{id}` (B.2: 메시지·CRM·thread version).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, schemars::JsonSchema)]
pub struct ThreadDetail {
    pub id: Uuid,
    pub mail_account_id: Uuid,
    pub contact_id: Option<Uuid>,
    pub assigned_principal_id: Option<Uuid>,
    pub status: ThreadStatus,
    pub version: i64,
    /// Authoritative last inbound pointer used to fence reply drafts, including timestamp ties.
    pub last_inbound_message_id: Option<Uuid>,
    /// Caller's own read mark (`thread_read_states`, §9.3) — never another
    /// principal's.
    pub my_last_read_at: Option<DateTime<Utc>>,
    pub updated_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub messages: Vec<EmailMessageDto>,
}

/// `PUT /email-threads/{id}/assignment` (B.2). `null` unassigns.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct AssignmentRequest {
    pub assigned_principal_id: Option<Uuid>,
}

/// `PUT /email-threads/{id}/read-state` (B.2 — 본인의 읽음 위치만).
/// Monotonic per B.1.4: the server stores `max(existing, requested)` so an
/// old timestamp can never erase a newer read mark.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ReadStateRequest {
    /// Mark-read position; defaults to `now()` when omitted.
    pub last_read_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ReadStateResponse {
    pub thread_id: Uuid,
    pub principal_id: Uuid,
    pub last_read_at: DateTime<Utc>,
}

/// `PATCH /email-threads/{id}` (B.2 — status only, If-Match required).
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PatchThreadRequest {
    pub status: ThreadStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ThreadMutationResponse {
    pub id: Uuid,
    pub status: ThreadStatus,
    pub assigned_principal_id: Option<Uuid>,
    pub version: i64,
    pub updated_at: DateTime<Utc>,
}

/// `POST /email-threads/{id}/reply-drafts` (B.2, B.3.10).
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ReplyDraftRequest {
    /// What the client believes is the latest inbound message; a mismatch is
    /// `409 REPLY_CONTEXT_CHANGED` (B.5, T16).
    pub expected_last_inbound_id: Option<Uuid>,
    pub body_text: String,
    pub body_html: Option<String>,
    /// `private` | `mailbox` (message_drafts CHECK); default `private`.
    pub visibility: Option<String>,
}

/// `message_drafts` row created by reply-drafts (B.3.10 result).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct MessageDraftDto {
    pub id: Uuid,
    pub mail_account_id: Uuid,
    pub owner_principal_id: Uuid,
    pub visibility: String,
    pub thread_id: Option<Uuid>,
    pub reply_to_message_id: Option<Uuid>,
    pub expected_last_inbound_id: Option<Uuid>,
    pub to_email_address_id: Uuid,
    pub purpose: String,
    pub subject: String,
    pub body_text: String,
    pub body_html: Option<String>,
    pub status: String,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// `POST /email-messages/{id}/classification-reviews` (B.2 — 분류·이유 →
/// 검토·안전 중단). Human principals only.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ClassificationReviewRequest {
    pub classification: Classification,
    /// Human reviewer's reason — stored in audit metadata only (no column).
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ClassificationReviewResponse {
    pub id: Uuid,
    pub classification: Classification,
    pub classification_reviewed_by: Uuid,
}

/// `POST /message-attachments/{id}/download-requests` (B.2 — 검증 후 짧은
/// 다운로드 URL, §15.2).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct DownloadRequestResponse {
    pub attachment_id: Uuid,
    /// Short-lived capability URL on this API (`GET
    /// /message-attachments/{id}/download?token=…`) — the token expires and
    /// the download re-checks the mailbox grant.
    pub download_url: String,
    pub expires_at: DateTime<Utc>,
}
