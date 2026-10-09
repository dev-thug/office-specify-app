//! operations module DTOs (spec §6 — owned by the module worker).
//!
//! B.2 운영 rows: `GET /jobs/{id}`, `POST /jobs/{id}/retry-requests`,
//! `GET /audit-logs`, `GET /dashboard/summary` plus the module-owned ops
//! views `GET /operations/alerts` and
//! `POST /operations/send-workers-lock/release` (T30 restore lock).
//! Request DTOs deny unknown fields and never accept system-owned fields
//! (B.1.1).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

// ------------------------------------------------------------------ jobs

/// `GET /jobs/{id}` response — 진행·에러·재시도 가능 여부 (B.2).
/// Only safe ID/version references from `payload` are returned.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct JobResponse {
    pub id: Uuid,
    pub kind: String,
    /// DDL CHECK 값과 동일: queued|running|retry_wait|completed|dead|canceled|unknown.
    pub state: JobState,
    pub payload_version: i32,
    pub result_refs: Vec<JobResultRef>,
    pub requested_by: Option<Uuid>,
    pub attempt_count: i32,
    /// §16.4 상한(5회) — 클라이언트 표시용.
    pub max_attempts: i32,
    /// kind가 외부 효과 작업인지 (§16.4; 재시도 시 reason 필수).
    pub external_effect: bool,
    /// state ∈ {dead, retry_wait} 일 때만 true. `unknown`은 절대 재시도 불가(ADR-006).
    pub retryable: bool,
    /// 외부 효과 kind는 재시도 요청 시 reason + 감사가 필요하다.
    pub retry_requires_reason: bool,
    pub available_at: DateTime<Utc>,
    pub lease_owner: Option<String>,
    pub lease_expires_at: Option<DateTime<Utc>>,
    pub last_error_code: Option<String>,
    pub finished_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

/// `GET /jobs` summary row. The collection omits payload/result references;
/// those are available only through the access-checked detail endpoint.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct JobSummaryResponse {
    pub id: Uuid,
    pub kind: String,
    pub state: JobState,
    pub payload_version: i32,
    pub attempt_count: i32,
    pub last_error_code: Option<String>,
    pub available_at: DateTime<Utc>,
    pub lease_owner: Option<String>,
    pub lease_expires_at: Option<DateTime<Utc>>,
    pub requested_by: Option<Uuid>,
    pub finished_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub retryable: bool,
    pub external_effect: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct JobResultRef {
    #[serde(rename = "type")]
    pub reference_type: String,
    pub id: Uuid,
}

/// `jobs.state` values, kept in sync with the DDL CHECK constraint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    Queued,
    Running,
    RetryWait,
    Completed,
    Dead,
    Canceled,
    Unknown,
}

impl JobState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::RetryWait => "retry_wait",
            Self::Completed => "completed",
            Self::Dead => "dead",
            Self::Canceled => "canceled",
            Self::Unknown => "unknown",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "queued" => Self::Queued,
            "running" => Self::Running,
            "retry_wait" => Self::RetryWait,
            "completed" => Self::Completed,
            "dead" => Self::Dead,
            "canceled" => Self::Canceled,
            "unknown" => Self::Unknown,
            _ => return None,
        })
    }
}

/// `POST /jobs/{id}/retry-requests` body. `reason`은 외부 효과 kind에서 필수.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct JobRetryRequest {
    /// 운영자 사유 — 외부 효과 kind는 필수, 감사 details에 기록된다.
    pub reason: Option<String>,
}

/// `GET /audit-logs` supported filters. Pagination lives in the shared
/// `PageParams` extractor; only these fields affect the query.
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct AuditLogFilters {
    pub action: Option<String>,
    pub target_type: Option<String>,
    pub target_id: Option<Uuid>,
    pub actor_principal_id: Option<Uuid>,
    pub from: Option<DateTime<Utc>>,
    pub to: Option<DateTime<Utc>>,
}

/// `GET /dashboard/summary` supported range and scope filters.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct DashboardFilters {
    pub from: Option<DateTime<Utc>>,
    pub to: Option<DateTime<Utc>>,
    /// "team" or "mine". Defaults to team for Admin/Manager and mine for
    /// Member/Agent callers.
    pub scope: Option<String>,
}

// ------------------------------------------------------------ audit logs

/// `GET /audit-logs` row — redacted 표현 (§5.6). `details`는 기록 시점에
/// allowlist로 작성되므로 본문·토큰을 포함하지 않는다.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct AuditLogEntry {
    pub id: Uuid,
    pub occurred_at: DateTime<Utc>,
    pub action: String,
    pub target_type: String,
    pub target_id: Option<Uuid>,
    pub actor_principal_id: Option<Uuid>,
    pub requested_by: Option<Uuid>,
    pub approved_by: Option<Uuid>,
    pub job_id: Option<Uuid>,
    pub request_id: String,
    pub trace_id: Option<String>,
    /// `details->>'outcome'` projection ("ok" | "denied:<code>" | "failed:<code>").
    pub outcome: Option<String>,
    pub details: serde_json::Value,
}

// ---------------------------------------------------------- dashboard

/// `GET /dashboard/summary` (B.2) — §2.4 KPI 정의 + cohort + freshness
/// (UI01: 분모·관측 기간·데이터 갱신 시각 표시, 모르는 값은 null=N/A).
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct DashboardSummaryResponse {
    /// 관측·코호트 기간.
    pub period: DashboardPeriod,
    /// 응답 생성 시각 — 라이브 조회이며 캐시 스냅샷이 아니다.
    pub generated_at: DateTime<Utc>,
    /// 적용된 범위 필터: "team" | "mine".
    pub scope: String,
    /// Count-only work queues visible to this principal and dashboard scope.
    pub queues: Vec<DashboardQueue>,
    pub kpis: DashboardKpis,
    /// §2.4 "운영 안정성"은 각각 별도 모니터링 — 합산 점수로 만들지 않는다.
    pub operational: OperationalCounts,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct DashboardQueue {
    pub key: String,
    pub count: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub oldest_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct DashboardPeriod {
    pub from: DateTime<Utc>,
    pub to: DateTime<Utc>,
}

/// 비율 KPI: 분모가 0이면 `value`는 null(UI01의 N/A 표시). 분자·분모는 항상 표시.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct RatioKpi {
    pub numerator: i64,
    pub denominator: i64,
    pub value: Option<f64>,
}

/// 시간 KPI: 표본이 없으면 `median_seconds`는 null.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct MedianKpi {
    pub sample_size: i64,
    pub median_seconds: Option<f64>,
}

/// §2.4 성공 지표 — 각 KPI의 정확한 SQL 정의는 `application::operations::dashboard`
/// 의 한국어 주석을 따른다.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct DashboardKpis {
    /// 적격 접촉 기업 수 (기간 내 공급자 수락 메일의 고유 기업).
    pub eligible_contacted_companies: i64,
    /// 답장 기업 비율 (사람 답장 관측 기업 / 적격 접촉 기업).
    pub reply_company_ratio: RatioKpi,
    /// 유효 미팅 전환율 (유효 미팅이 열린 기업 / 적격 접촉 기업).
    pub meeting_conversion: RatioKpi,
    /// 최초 답장 처리 시간 — 수신 관측~직원 회신 수락, 중앙값(초).
    pub first_reply: MedianKpi,
    /// 에이전트 후보 채택률 (채택 / 검토 완료).
    pub agent_proposal_adoption: RatioKpi,
}

/// 운영 안정성 카운터 (§2.4 마지막 행 — 각각 별도 모니터링).
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct OperationalCounts {
    /// outbound_messages.state='unknown' — 대조 필요(ADR-006).
    pub unknown_outbound_messages: i64,
    /// jobs.state='unknown'.
    pub unknown_jobs: i64,
    pub dead_jobs: i64,
    /// 미발행 outbox 잔량 (DB backlog).
    pub outbox_backlog: i64,
    /// 재인증 필요 메일 계정 수.
    pub reauth_required_mailboxes: i64,
    pub emergency_stop: bool,
    /// T30 복원 잠금 — workspaces.settings.send_workers_locked.
    pub send_workers_locked: bool,
}

// ---------------------------------------------------------------- alerts

/// `GET /operations/alerts` — §17.2 필수 알림의 REST 표현(라이브 카운트).
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct AlertsResponse {
    pub generated_at: DateTime<Utc>,
    pub emergency_stop: bool,
    /// T30 복원 잠금 — true면 발송 워커가 잠겨 있어 대조 후 해제가 필요하다.
    pub send_workers_locked: bool,
    /// 미발행 outbox backlog (§17.2 "DB backlog").
    pub outbox_backlog: BacklogAlert,
    /// "unknown submission > 0" 알림 — 자동 재발송 금지, 대조만.
    pub unknown_outbound: UnknownOutboundAlert,
    /// "dead jobs" 알림 — 운영자 조치 대상.
    pub dead_jobs: DeadJobsAlert,
    /// "OAuth 실패 반복" 계열 — 재인증 필요 메일함.
    pub reauth_mailboxes: ReauthMailboxAlert,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct BacklogAlert {
    pub count: i64,
    pub oldest_created_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct UnknownOutboundAlert {
    /// outbound_messages.status='unknown' 건수.
    pub outbound_messages: i64,
    /// jobs.state='unknown' 건수.
    pub jobs: i64,
    /// 조사 시작점용 샘플 id(outbound_message).
    pub sample_outbound_ids: Vec<Uuid>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct DeadJobsAlert {
    pub count: i64,
    pub samples: Vec<DeadJobSample>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct DeadJobSample {
    pub id: Uuid,
    pub kind: String,
    pub last_error_code: Option<String>,
    pub finished_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ReauthMailboxAlert {
    pub count: i64,
    pub samples: Vec<ReauthMailboxSample>,
}

/// 메일 계정 메타데이터만 노출 — 본문·토큰 아님 (§5.6).
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ReauthMailboxSample {
    pub id: Uuid,
    pub name: String,
    pub last_synced_at: Option<DateTime<Utc>>,
}

// ------------------------------------------------- send workers lock (T30)

/// `POST /operations/send-workers-lock/release` body (T30).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ReleaseSendWorkersLockRequest {
    /// 대조(reconcile) 근거 — 필수. 감사 details에 기록된다.
    pub reason: String,
}

/// send-workers-lock 상태 응답.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SendWorkersLockResponse {
    pub send_workers_locked: bool,
}
