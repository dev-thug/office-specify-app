//! agents module DTOs (spec §6 — owned by the module worker).
//!
//! B.2 agent-runs rows: `GET/POST /agent-runs`, `GET /agent-runs/{id}`,
//! `POST /agent-runs/{id}/cancel`. Request DTOs deny unknown fields and
//! never accept system-owned fields (`requested_by`, `status`, `usage`) —
//! B.1.1.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

/// `POST /agent-runs` (B.2 "목적·budget·대상 범위"). 생성은 system/admin만.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateAgentRunRequest {
    /// run을 실행할 에이전트 principal id (kind='agent', active여야 한다).
    pub agent_principal_id: Uuid,
    /// 실행 목적 — §13.5 "run에는 목적…을 저장한다".
    pub objective: String,
    /// Target-data scope for the run (not an authorization grant).
    #[serde(default)]
    pub scope: Option<AgentRunTargetScope>,
    /// 비용·호출 상한 (§13.5). 검증된 키만 저장된다.
    #[serde(default)]
    pub budget: Option<AgentRunBudget>,
}

/// §13.5 비용 제한 필드 — 도구 호출 수·후보 수·외부 검색 수·입출력 토큰·비용
/// 예산. 실제 공급자 과금이 없으므로 비용은 추정치다. B.2 target scope는
/// 별도 `scope` 필드로 요청받고 `budget.target_scope` 안에 저장된다.
#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct AgentRunBudget {
    pub max_tool_calls: Option<u32>,
    pub max_proposals: Option<u32>,
    pub max_external_searches: Option<u32>,
    pub max_input_tokens: Option<u64>,
    pub max_output_tokens: Option<u64>,
    /// 추정 비용 상한 — 과금 자료가 없으므로 estimate로 해석한다 (§13.5).
    pub max_cost_usd: Option<f64>,
}

/// `POST /agent-runs` target-data scope. It filters work; it grants no access.
#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct AgentRunTargetScope {
    pub industry: Option<String>,
    pub country_code: Option<String>,
}

/// `agent_runs.status` values, kept in sync with its DDL CHECK constraint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AgentRunStatus {
    Queued,
    Running,
    AwaitingApproval,
    Completed,
    Failed,
    Canceled,
}

impl AgentRunStatus {
    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "queued" => Self::Queued,
            "running" => Self::Running,
            "awaiting_approval" => Self::AwaitingApproval,
            "completed" => Self::Completed,
            "failed" => Self::Failed,
            "canceled" => Self::Canceled,
            _ => return None,
        })
    }

    pub fn can_cancel(self) -> bool {
        matches!(self, Self::Queued | Self::Running | Self::AwaitingApproval)
    }

    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Canceled)
    }
}

/// `GET /agent-runs` filters. Pagination is parsed separately through the
/// shared API pagination extractor.
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct AgentRunFilters {
    pub status: Option<String>,
    pub agent_principal_id: Option<Uuid>,
}

/// `POST /agent-runs/{id}/cancel` body. reason은 선택 — 감사에 기록.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CancelAgentRunRequest {
    /// 취소 사유 — 감사 details에 기록된다.
    pub reason: Option<String>,
}

/// External agent-runner outcome. Clients cannot set any other run status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AgentRunFinishOutcome {
    Completed,
    Failed,
}

impl AgentRunFinishOutcome {
    pub fn status(self) -> AgentRunStatus {
        match self {
            Self::Completed => AgentRunStatus::Completed,
            Self::Failed => AgentRunStatus::Failed,
        }
    }
}

/// `POST /agent-runs/{id}/finish`. System-owned run fields remain server-only.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct FinishAgentRunRequest {
    pub outcome: AgentRunFinishOutcome,
}

/// run 응답. `status`는 DDL CHECK와 동일:
/// queued|running|awaiting_approval|completed|failed|canceled.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AgentRunResponse {
    pub id: Uuid,
    /// run을 수행하는 에이전트 principal.
    pub principal_id: Uuid,
    pub requested_by: Uuid,
    pub source_credential_id: Option<Uuid>,
    pub objective: String,
    pub status: AgentRunStatus,
    /// 상한 스냅샷(요청 시 검증된 키만 저장).
    pub budget: serde_json::Value,
    /// 사용량 누적(도구 호출 수 등 — record_action이 갱신).
    pub usage: serde_json::Value,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
    pub cancel_requested_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    /// 화면 힌트 (B.1.1) — 서버는 항상 재검사한다.
    pub allowed_actions: Vec<String>,
}

/// `GET /agent-runs/{id}` — run + 도구 호출 기록 (§13.5).
/// `agent_actions`는 hash·redacted 인자·결과 코드만 저장한다 — 원문·토큰·
/// 모델 추론은 절대 저장하지 않는다.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct AgentRunDetailResponse {
    pub id: Uuid,
    pub principal_id: Uuid,
    pub requested_by: Uuid,
    pub source_credential_id: Option<Uuid>,
    pub objective: String,
    pub status: AgentRunStatus,
    pub budget: serde_json::Value,
    pub usage: serde_json::Value,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
    pub cancel_requested_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub allowed_actions: Vec<String>,
    pub actions: Vec<AgentActionResponse>,
}

/// Cancellation makes the run stop accepting new actions; an external call
/// that already started is reported separately and may still finish.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AgentRunCancelResponse {
    pub id: Uuid,
    pub principal_id: Uuid,
    pub requested_by: Uuid,
    pub source_credential_id: Option<Uuid>,
    pub objective: String,
    pub status: AgentRunStatus,
    pub budget: serde_json::Value,
    pub usage: serde_json::Value,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
    pub cancel_requested_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub allowed_actions: Vec<String>,
    pub in_flight_external_actions: i64,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct AgentActionResponse {
    pub id: Uuid,
    pub tool_name: String,
    /// SHA-256 hex 64자 (B.1.1 Hash 응답 형식; DB 저장은 BYTEA).
    pub arguments_hash: String,
    /// redacted 인자만 — 원문 아님.
    pub redacted_arguments: serde_json::Value,
    pub result_code: Option<String>,
    pub result_ref: Option<String>,
    pub approval_request_id: Option<Uuid>,
    pub has_external_effect: bool,
    pub trace_id: Option<String>,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}
