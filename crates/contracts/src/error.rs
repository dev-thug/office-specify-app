use serde::{Deserialize, Serialize};

/// Stable API error codes. Includes every code in source B.5 plus the generic
/// transport-level codes of spec §4.3. Serialization is the wire string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, utoipa::ToSchema)]
pub enum ErrorCode {
    // ---- generic (spec §4.3) ----
    #[serde(rename = "VALIDATION_FAILED")]
    ValidationFailed,
    #[serde(rename = "NOT_FOUND")]
    NotFound,
    #[serde(rename = "FORBIDDEN")]
    Forbidden,
    #[serde(rename = "CONFLICT")]
    Conflict,
    #[serde(rename = "SCOPE_REQUIRED")]
    ScopeRequired,
    #[serde(rename = "INVALID_CREDENTIALS")]
    InvalidCredentials,
    #[serde(rename = "ACCOUNT_LOCKED")]
    AccountLocked,
    #[serde(rename = "CSRF_TOKEN_INVALID")]
    CsrfTokenInvalid,
    #[serde(rename = "DEPENDENCY_UNAVAILABLE")]
    DependencyUnavailable,
    #[serde(rename = "INTERNAL")]
    Internal,

    // ---- B.5 stable codes ----
    #[serde(rename = "AUTHENTICATION_REQUIRED")]
    AuthenticationRequired,
    #[serde(rename = "PRINCIPAL_DISABLED")]
    PrincipalDisabled,
    #[serde(rename = "MAILBOX_ACCESS_DENIED")]
    MailboxAccessDenied,
    #[serde(rename = "AGENT_DIRECT_SEND_FORBIDDEN")]
    AgentDirectSendForbidden,
    #[serde(rename = "SELF_APPROVAL_FORBIDDEN")]
    SelfApprovalForbidden,
    #[serde(rename = "VERSION_PRECONDITION_REQUIRED")]
    VersionPreconditionRequired,
    #[serde(rename = "VERSION_MISMATCH")]
    VersionMismatch,
    #[serde(rename = "IDEMPOTENCY_KEY_REUSED")]
    IdempotencyKeyReused,
    #[serde(rename = "REQUEST_IN_PROGRESS")]
    RequestInProgress,
    #[serde(rename = "SNAPSHOT_MISMATCH")]
    SnapshotMismatch,
    #[serde(rename = "APPROVAL_EXPIRED")]
    ApprovalExpired,
    #[serde(rename = "RECIPIENT_SUPPRESSED")]
    RecipientSuppressed,
    #[serde(rename = "SEND_BASIS_UNVERIFIED")]
    SendBasisUnverified,
    #[serde(rename = "PROVIDER_POLICY_BLOCKED")]
    ProviderPolicyBlocked,
    #[serde(rename = "PROVIDER_POLICY_UNVERIFIED")]
    ProviderPolicyUnverified,
    #[serde(rename = "TEMPLATE_VARIABLE_MISSING")]
    TemplateVariableMissing,
    #[serde(rename = "REPLY_CONTEXT_CHANGED")]
    ReplyContextChanged,
    #[serde(rename = "REPLY_ALREADY_SUBMITTED")]
    ReplyAlreadySubmitted,
    #[serde(rename = "RECIPIENT_ACTIVE_ELSEWHERE")]
    RecipientActiveElsewhere,
    #[serde(rename = "SLOT_UNAVAILABLE")]
    SlotUnavailable,
    #[serde(rename = "BOOKING_LINK_EXPIRED")]
    BookingLinkExpired,
    #[serde(rename = "BOOKING_RECIPIENT_MISMATCH")]
    BookingRecipientMismatch,
    #[serde(rename = "RATE_LIMITED")]
    RateLimited,
    #[serde(rename = "INTEGRATION_REAUTH_REQUIRED")]
    IntegrationReauthRequired,
    #[serde(rename = "CALENDAR_AVAILABILITY_UNAVAILABLE")]
    CalendarAvailabilityUnavailable,
    /// Mostly surfaces as a business state on `GET outbound`, not an HTTP error.
    #[serde(rename = "PROVIDER_SUBMISSION_OUTCOME_UNKNOWN")]
    ProviderSubmissionOutcomeUnknown,
    #[serde(rename = "WORKSPACE_EMERGENCY_STOP")]
    WorkspaceEmergencyStop,
}

impl ErrorCode {
    /// Wire string, e.g. `IDEMPOTENCY_KEY_REUSED`.
    pub fn as_str(self) -> &'static str {
        // serde rename strings, kebab partner of `slug`.
        use ErrorCode::*;
        match self {
            ValidationFailed => "VALIDATION_FAILED",
            NotFound => "NOT_FOUND",
            Forbidden => "FORBIDDEN",
            Conflict => "CONFLICT",
            ScopeRequired => "SCOPE_REQUIRED",
            InvalidCredentials => "INVALID_CREDENTIALS",
            AccountLocked => "ACCOUNT_LOCKED",
            CsrfTokenInvalid => "CSRF_TOKEN_INVALID",
            DependencyUnavailable => "DEPENDENCY_UNAVAILABLE",
            Internal => "INTERNAL",
            AuthenticationRequired => "AUTHENTICATION_REQUIRED",
            PrincipalDisabled => "PRINCIPAL_DISABLED",
            MailboxAccessDenied => "MAILBOX_ACCESS_DENIED",
            AgentDirectSendForbidden => "AGENT_DIRECT_SEND_FORBIDDEN",
            SelfApprovalForbidden => "SELF_APPROVAL_FORBIDDEN",
            VersionPreconditionRequired => "VERSION_PRECONDITION_REQUIRED",
            VersionMismatch => "VERSION_MISMATCH",
            IdempotencyKeyReused => "IDEMPOTENCY_KEY_REUSED",
            RequestInProgress => "REQUEST_IN_PROGRESS",
            SnapshotMismatch => "SNAPSHOT_MISMATCH",
            ApprovalExpired => "APPROVAL_EXPIRED",
            RecipientSuppressed => "RECIPIENT_SUPPRESSED",
            SendBasisUnverified => "SEND_BASIS_UNVERIFIED",
            ProviderPolicyBlocked => "PROVIDER_POLICY_BLOCKED",
            ProviderPolicyUnverified => "PROVIDER_POLICY_UNVERIFIED",
            TemplateVariableMissing => "TEMPLATE_VARIABLE_MISSING",
            ReplyContextChanged => "REPLY_CONTEXT_CHANGED",
            ReplyAlreadySubmitted => "REPLY_ALREADY_SUBMITTED",
            RecipientActiveElsewhere => "RECIPIENT_ACTIVE_ELSEWHERE",
            SlotUnavailable => "SLOT_UNAVAILABLE",
            BookingLinkExpired => "BOOKING_LINK_EXPIRED",
            BookingRecipientMismatch => "BOOKING_RECIPIENT_MISMATCH",
            RateLimited => "RATE_LIMITED",
            IntegrationReauthRequired => "INTEGRATION_REAUTH_REQUIRED",
            CalendarAvailabilityUnavailable => "CALENDAR_AVAILABILITY_UNAVAILABLE",
            ProviderSubmissionOutcomeUnknown => "PROVIDER_SUBMISSION_OUTCOME_UNKNOWN",
            WorkspaceEmergencyStop => "WORKSPACE_EMERGENCY_STOP",
        }
    }

    /// `urn:outreach:problem:<kebab-slug>` (B.1.2).
    pub fn slug(self) -> &'static str {
        use ErrorCode::*;
        match self {
            ValidationFailed => "validation-failed",
            NotFound => "not-found",
            Forbidden => "forbidden",
            Conflict => "conflict",
            ScopeRequired => "scope-required",
            InvalidCredentials => "invalid-credentials",
            AccountLocked => "account-locked",
            CsrfTokenInvalid => "csrf-token-invalid",
            DependencyUnavailable => "dependency-unavailable",
            Internal => "internal",
            AuthenticationRequired => "authentication-required",
            PrincipalDisabled => "principal-disabled",
            MailboxAccessDenied => "mailbox-access-denied",
            AgentDirectSendForbidden => "agent-direct-send-forbidden",
            SelfApprovalForbidden => "self-approval-forbidden",
            VersionPreconditionRequired => "version-precondition-required",
            VersionMismatch => "version-mismatch",
            IdempotencyKeyReused => "idempotency-key-reused",
            RequestInProgress => "request-in-progress",
            SnapshotMismatch => "snapshot-mismatch",
            ApprovalExpired => "approval-expired",
            RecipientSuppressed => "recipient-suppressed",
            SendBasisUnverified => "send-basis-unverified",
            ProviderPolicyBlocked => "provider-policy-blocked",
            ProviderPolicyUnverified => "provider-policy-unverified",
            TemplateVariableMissing => "template-variable-missing",
            ReplyContextChanged => "reply-context-changed",
            ReplyAlreadySubmitted => "reply-already-submitted",
            RecipientActiveElsewhere => "recipient-active-elsewhere",
            SlotUnavailable => "slot-unavailable",
            BookingLinkExpired => "booking-link-expired",
            BookingRecipientMismatch => "booking-recipient-mismatch",
            RateLimited => "rate-limited",
            IntegrationReauthRequired => "integration-reauth-required",
            CalendarAvailabilityUnavailable => "calendar-availability-unavailable",
            ProviderSubmissionOutcomeUnknown => "provider-submission-outcome-unknown",
            WorkspaceEmergencyStop => "workspace-emergency-stop",
        }
    }

    /// Default HTTP status (spec §4.3, B.5, §12.3). `AppError::with_status`
    /// overrides for codes that legitimately vary (e.g. MAILBOX_ACCESS_DENIED
    /// may be 404 when existence must be hidden; VALIDATION_FAILED may be 422
    /// for semantic validation).
    pub fn status(self) -> u16 {
        use ErrorCode::*;
        match self {
            ValidationFailed => 400,
            NotFound => 404,
            Forbidden => 403,
            Conflict => 409,
            ScopeRequired => 403,
            InvalidCredentials => 401,
            AccountLocked => 429,
            CsrfTokenInvalid => 403,
            DependencyUnavailable => 503,
            Internal => 500,
            AuthenticationRequired => 401,
            PrincipalDisabled => 403,
            MailboxAccessDenied => 403,
            AgentDirectSendForbidden => 403,
            SelfApprovalForbidden => 403,
            VersionPreconditionRequired => 428,
            VersionMismatch => 412,
            IdempotencyKeyReused => 409,
            RequestInProgress => 409,
            SnapshotMismatch => 409,
            ApprovalExpired => 409,
            RecipientSuppressed => 422,
            SendBasisUnverified => 422,
            ProviderPolicyBlocked => 422,
            ProviderPolicyUnverified => 422,
            TemplateVariableMissing => 422,
            ReplyContextChanged => 409,
            ReplyAlreadySubmitted => 409,
            RecipientActiveElsewhere => 409,
            SlotUnavailable => 409,
            BookingLinkExpired => 410,
            BookingRecipientMismatch => 422,
            RateLimited => 429,
            IntegrationReauthRequired => 503,
            CalendarAvailabilityUnavailable => 503,
            ProviderSubmissionOutcomeUnknown => 409,
            WorkspaceEmergencyStop => 409,
        }
    }

    /// Default retryability per B.5 (client guidance, not a server guarantee).
    pub fn retryable(self) -> bool {
        use ErrorCode::*;
        matches!(
            self,
            AuthenticationRequired
                | RateLimited
                | IntegrationReauthRequired
                | CalendarAvailabilityUnavailable
                | DependencyUnavailable
                | Internal
                | VersionPreconditionRequired
                | VersionMismatch
        )
    }

    pub fn problem_type(self) -> String {
        format!("urn:outreach:problem:{}", self.slug())
    }
}

/// Field-level violation inside a problem detail (B.1.2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct Violation {
    pub field: String,
    pub code: String,
}

/// RFC 9457 problem detail with project extensions (B.1.2).
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct Problem {
    #[serde(rename = "type")]
    pub problem_type: String,
    pub title: String,
    pub status: u16,
    pub code: ErrorCode,
    pub detail: String,
    pub instance: String,
    pub request_id: String,
    pub retryable: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub violations: Vec<Violation>,
}

/// Application/service error carried through use cases, infra and adapters.
/// Defined here (not in `application`) so infrastructure can return it without
/// an upward dependency; `application::AppError` re-exports this type.
#[derive(Debug)]
pub struct AppError {
    pub code: ErrorCode,
    pub detail: String,
    pub violations: Vec<Violation>,
    /// Seconds for the Retry-After header on 429/503.
    pub retry_after: Option<u32>,
    status: u16,
}

impl AppError {
    pub fn new(code: ErrorCode, detail: impl Into<String>) -> Self {
        Self {
            code,
            detail: detail.into(),
            violations: Vec::new(),
            retry_after: None,
            status: code.status(),
        }
    }

    pub fn with_status(mut self, status: u16) -> Self {
        self.status = status;
        self
    }

    pub fn with_violations(mut self, violations: Vec<Violation>) -> Self {
        self.violations = violations;
        self
    }

    pub fn with_retry_after(mut self, seconds: u32) -> Self {
        self.retry_after = Some(seconds);
        self
    }

    pub fn status(&self) -> u16 {
        self.status
    }

    pub fn not_found(detail: impl Into<String>) -> Self {
        Self::new(ErrorCode::NotFound, detail)
    }

    pub fn forbidden(detail: impl Into<String>) -> Self {
        Self::new(ErrorCode::Forbidden, detail)
    }

    pub fn conflict(detail: impl Into<String>) -> Self {
        Self::new(ErrorCode::Conflict, detail)
    }

    pub fn internal(detail: impl Into<String>) -> Self {
        Self::new(ErrorCode::Internal, detail)
    }

    pub fn validation(detail: impl Into<String>, violations: Vec<Violation>) -> Self {
        Self::new(ErrorCode::ValidationFailed, detail).with_violations(violations)
    }

    pub fn dependency_unavailable(detail: impl Into<String>) -> Self {
        Self::new(ErrorCode::DependencyUnavailable, detail)
    }

    /// Build the RFC 9457 body. `instance` = request path.
    /// R23: 5xx details are internal — the body carries a generic string;
    /// the real detail is logged at the HTTP boundary with the request id.
    pub fn to_problem(&self, request_id: &str, instance: &str) -> Problem {
        let detail = if self.status >= 500 {
            "an internal error occurred".to_string()
        } else {
            self.detail.clone()
        };
        Problem {
            problem_type: self.code.problem_type(),
            title: default_title(self.code),
            status: self.status,
            code: self.code,
            detail,
            instance: instance.to_string(),
            request_id: request_id.to_string(),
            retryable: self.code.retryable(),
            violations: self.violations.clone(),
        }
    }
}

fn default_title(code: ErrorCode) -> String {
    let slug = code.slug();
    let mut out = String::with_capacity(slug.len());
    for (i, word) in slug.split('-').enumerate() {
        if i > 0 {
            out.push(' ');
        }
        let mut chars = word.chars();
        if let Some(c) = chars.next() {
            out.extend(c.to_uppercase());
            out.extend(chars);
        }
    }
    out
}

impl std::fmt::Display for AppError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code.as_str(), self.detail)
    }
}

impl std::error::Error for AppError {}
