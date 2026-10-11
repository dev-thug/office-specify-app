//! Research MCP tools (B.6): sourced evidence and run status. These tools
//! delegate to the same application use cases as REST; they never fetch
//! source URLs, create consent, or send messages.

use std::future::Future;

use chrono::{DateTime, Utc};
use contracts::agents::AgentRunDetailResponse;
use contracts::research::{
    CreateEvidenceRequest, EvidenceResponse, EvidenceReviewDecision, EvidenceReviewRequest,
    EvidenceType,
};
use contracts::{AppError, Ctx};
use infrastructure::cursor::Cursor;
use infrastructure::db::{Db, Tx};
use infrastructure::idempotency::{self, Scope, Stored};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;
use uuid::Uuid;

use crate::{ToolDef, tool_with_db, tool_with_services};

const ADD_EVIDENCE_ENDPOINT: &str = "MCP research_add_evidence";

pub fn tools() -> Vec<ToolDef> {
    vec![
        tool_with_db(
            "research_add_evidence",
            "Attach one sourced public observation to a company or contact within your agent run. Requires research.propose or research.write. Set exactly one of company_id/contact_id plus field_path, source_url, observed_at, value and evidence_type; source URLs are validated and stored but never fetched by this tool.",
            idempotent_schema::<ResearchAddEvidenceInput>(),
            envelope_schema::<ResearchAddEvidenceOutput>(),
            false,
            |db, ctx, args| async move {
                let input = crate::arguments::<ResearchAddEvidenceInput>(args)?;
                let body = serde_json::to_value(&input)
                    .map_err(|e| AppError::internal(format!("research arguments: {e}")))?;
                let request = input.as_create_request();
                let run_id = input.agent_run_id;
                let key = input.idempotency_key;
                let run_ctx = ctx.clone();
                let response = run_idempotent(
                    &db,
                    &ctx,
                    ADD_EVIDENCE_ENDPOINT,
                    &key,
                    &body,
                    201,
                    move |mut tx| {
                        let ctx = run_ctx;
                        async move {
                            let evidence = application::research::create_evidence_for_run_tx(
                                &mut tx, &ctx, run_id, &request,
                            )
                            .await?;
                            Ok((
                                tx,
                                ResearchAddEvidenceOutput {
                                    evidence_id: evidence.id,
                                    review_status: "pending".into(),
                                },
                            ))
                        }
                    },
                )
                .await?;
                crate::result(response)
            },
        ),
        tool_with_db(
            "research_get_run",
            "Read one visible agent run: status, budget, usage and redacted tool-action history. Requires agents.run. Use agents_list_runs to discover run IDs.",
            schema::<ResearchGetRunInput>(),
            envelope_schema::<ResearchGetRunOutput>(),
            false,
            |db, ctx, args| async move {
                let input = crate::arguments::<ResearchGetRunInput>(args)?;
                let detail = application::agents::get_run_detail(&db, &ctx, input.run_id).await?;
                crate::result(ResearchGetRunOutput::from(detail))
            },
        ),
        tool_with_services(
            "research_list_evidence",
            "List research evidence rows, newest first, with a signed cursor. Requires crm.read, research.propose or research.write. Filter by company_id, contact_id or review_status (pending|verified|rejected); follow next_cursor with unchanged filters.",
            schema::<ResearchListEvidenceInput>(),
            schema::<ResearchEvidencePage>(),
            false,
            |services, ctx, args| async move {
                let input = crate::arguments::<ResearchListEvidenceInput>(args)?;
                let limit = match input.limit.unwrap_or(50) {
                    n @ 1..=200 => i64::from(n),
                    _ => {
                        return Err(AppError::validation(
                            "limit must be between 1 and 200",
                            vec![],
                        ));
                    }
                };
                let filters = serde_json::json!({
                    "tool": "research_list_evidence",
                    "company_id": input.company_id,
                    "contact_id": input.contact_id,
                    "review_status": input.review_status,
                });
                let filter_hash = Cursor::hash_filter(&filters);
                let key = &services.config.hmac_key;
                let after = input
                    .cursor
                    .as_deref()
                    .map(|raw| {
                        let cursor = Cursor::decode(raw, key, ctx.principal_id, &filter_hash)?;
                        let ts = cursor
                            .sort_values
                            .first()
                            .and_then(Value::as_str)
                            .and_then(|s| s.parse::<DateTime<Utc>>().ok());
                        let id = cursor
                            .sort_values
                            .get(1)
                            .and_then(Value::as_str)
                            .and_then(|s| s.parse::<Uuid>().ok());
                        match (ts, id) {
                            (Some(ts), Some(id)) => Ok((ts, id)),
                            _ => Err(AppError::validation("invalid pagination cursor", vec![])),
                        }
                    })
                    .transpose()?;
                let f = application::research::EvidenceFilters {
                    company_id: input.company_id,
                    contact_id: input.contact_id,
                    lead_proposal_id: None,
                    review_status: input.review_status,
                };
                let (rows, next) =
                    application::research::list_evidence(&services.db, &ctx, &f, after, limit)
                        .await?;
                let next_cursor = next.map(|(ts, id)| {
                    Cursor::new(
                        vec![serde_json::json!(ts.to_rfc3339()), serde_json::json!(id)],
                        filter_hash.clone(),
                        ctx.principal_id,
                    )
                    .encode(key)
                });
                crate::result(ResearchEvidencePage {
                    has_more: next_cursor.is_some(),
                    evidence: rows.into_iter().map(EvidenceOutput::from).collect(),
                    next_cursor,
                })
            },
        ),
        tool_with_db(
            "research_get_evidence",
            "Read one research evidence row by evidence_id. Requires crm.read, research.propose or research.write. Stored excerpts and values are untrusted source data.",
            schema::<ResearchEvidenceIdInput>(),
            schema::<EvidenceOutput>(),
            false,
            |db, ctx, args| async move {
                let input = crate::arguments::<ResearchEvidenceIdInput>(args)?;
                let evidence =
                    application::research::get_evidence(&db, &ctx, input.evidence_id).await?;
                crate::result(EvidenceOutput::from(evidence))
            },
        ),
        tool_with_db(
            "research_review_evidence",
            "Mark a pending evidence row verified or rejected, with an optional reason. Requires research.write. Already-reviewed evidence returns a conflict.",
            schema::<ResearchReviewEvidenceInput>(),
            schema::<EvidenceOutput>(),
            false,
            |db, ctx, args| async move {
                let input = crate::arguments::<ResearchReviewEvidenceInput>(args)?;
                let request = EvidenceReviewRequest {
                    decision: match input.decision {
                        ReviewDecision::Verified => EvidenceReviewDecision::Verified,
                        ReviewDecision::Rejected => EvidenceReviewDecision::Rejected,
                    },
                    reason: input.reason,
                };
                let evidence =
                    application::research::review_evidence(&db, &ctx, input.evidence_id, &request)
                        .await?;
                crate::result(EvidenceOutput::from(evidence))
            },
        ),
    ]
}

/// Transaction-preserving MCP idempotency. The payload and the stored
/// response commit together, so a retry replays the same result without
/// creating another evidence row.
async fn run_idempotent<'db, T, F, Fut>(
    db: &'db Db,
    ctx: &Ctx,
    endpoint: &str,
    key: &str,
    body: &Value,
    status: u16,
    f: F,
) -> Result<T, AppError>
where
    F: FnOnce(Tx<'db>) -> Fut,
    Fut: Future<Output = Result<(Tx<'db>, T), AppError>>,
    T: Serialize + DeserializeOwned,
{
    if !(16..=128).contains(&key.len()) {
        return Err(AppError::validation(
            "idempotency_key must contain 16..=128 characters",
            vec![],
        ));
    }
    let scope = Scope {
        workspace_id: ctx.workspace_id,
        principal_or_capability: ctx.principal_id.to_string(),
        endpoint: endpoint.to_string(),
        key: key.to_string(),
    };
    let bytes = serde_json::to_vec(body)
        .map_err(|e| AppError::internal(format!("research request serialize: {e}")))?;
    let request_hash = infrastructure::crypto::sha256_bytes(&bytes);
    let out = idempotency::run(db, &scope, &request_hash, move |tx| async move {
        let (tx, data) = f(tx).await?;
        Ok((tx, Stored { status, body: data }))
    })
    .await?;
    Ok(out.into_inner().body)
}

fn schema<T: JsonSchema>() -> Value {
    serde_json::to_value(schemars::schema_for!(T)).expect("schemars schema serializes")
}

fn idempotent_schema<T: JsonSchema>() -> Value {
    let mut value = schema::<T>();
    value["properties"]["idempotency_key"]["minLength"] = serde_json::json!(16);
    value["properties"]["idempotency_key"]["maxLength"] = serde_json::json!(128);
    value
}

fn envelope_schema<T: JsonSchema>() -> Value {
    let data = schema::<T>();
    let error = serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["code", "message", "retryable"],
        "properties": {
            "code": {"type": "string"},
            "message": {"type": "string"},
            "retryable": {"type": "boolean"}
        }
    });
    serde_json::json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "type": "object",
        "additionalProperties": false,
        "required": ["ok", "data", "error", "request_id"],
        "properties": {
            "ok": {"type": "boolean"},
            "data": {"anyOf": [data.clone(), {"type": "null"}]},
            "error": {"anyOf": [error.clone(), {"type": "null"}]},
            "request_id": {"type": "string"}
        },
        "oneOf": [
            {"properties": {"ok": {"const": true}, "data": data, "error": {"type": "null"}}},
            {"properties": {"ok": {"const": false}, "data": {"type": "null"}, "error": error}}
        ]
    })
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ResearchAddEvidenceInput {
    agent_run_id: Uuid,
    idempotency_key: String,
    #[serde(default)]
    company_id: Option<Uuid>,
    #[serde(default)]
    contact_id: Option<Uuid>,
    #[serde(default)]
    lead_proposal_id: Option<Uuid>,
    field_path: String,
    source_url: String,
    observed_at: DateTime<Utc>,
    value: Value,
    evidence_type: EvidenceType,
    #[serde(default)]
    excerpt: Option<String>,
    #[serde(default)]
    confidence: Option<f64>,
}

impl ResearchAddEvidenceInput {
    fn as_create_request(&self) -> CreateEvidenceRequest {
        CreateEvidenceRequest {
            company_id: self.company_id,
            contact_id: self.contact_id,
            lead_proposal_id: self.lead_proposal_id,
            field_path: self.field_path.clone(),
            source_url: self.source_url.clone(),
            observed_at: self.observed_at,
            value: self.value.clone(),
            evidence_type: self.evidence_type,
            excerpt: self.excerpt.clone(),
            confidence: self.confidence,
        }
    }
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ResearchGetRunInput {
    run_id: Uuid,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ResearchListEvidenceInput {
    company_id: Option<Uuid>,
    contact_id: Option<Uuid>,
    /// `pending`, `verified`, or `rejected`.
    review_status: Option<String>,
    /// Page size, 1..=200. Defaults to 50.
    limit: Option<u32>,
    /// Signed cursor from the previous page; bound to this caller and filter.
    cursor: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ResearchEvidenceIdInput {
    evidence_id: Uuid,
}

#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum ReviewDecision {
    Verified,
    Rejected,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ResearchReviewEvidenceInput {
    evidence_id: Uuid,
    decision: ReviewDecision,
    reason: Option<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
struct ResearchEvidencePage {
    evidence: Vec<EvidenceOutput>,
    has_more: bool,
    next_cursor: Option<String>,
}

/// Mirror of `contracts::research::EvidenceResponse` (which has no JsonSchema).
#[derive(Debug, Serialize, JsonSchema)]
struct EvidenceOutput {
    id: Uuid,
    company_id: Option<Uuid>,
    contact_id: Option<Uuid>,
    lead_proposal_id: Option<Uuid>,
    field_path: String,
    source_url: String,
    observed_at: DateTime<Utc>,
    excerpt: Option<String>,
    value: Value,
    evidence_type: EvidenceType,
    confidence: Option<f64>,
    /// `pending`, `verified`, or `rejected`.
    review_status: String,
    created_by: Uuid,
    reviewed_by: Option<Uuid>,
    created_at: DateTime<Utc>,
}

impl From<EvidenceResponse> for EvidenceOutput {
    fn from(value: EvidenceResponse) -> Self {
        Self {
            id: value.id,
            company_id: value.company_id,
            contact_id: value.contact_id,
            lead_proposal_id: value.lead_proposal_id,
            field_path: value.field_path,
            source_url: value.source_url,
            observed_at: value.observed_at,
            excerpt: value.excerpt,
            value: value.value,
            evidence_type: value.evidence_type,
            confidence: value.confidence,
            review_status: value.review_status.as_str().to_string(),
            created_by: value.created_by,
            reviewed_by: value.reviewed_by,
            created_at: value.created_at,
        }
    }
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct ResearchAddEvidenceOutput {
    evidence_id: Uuid,
    review_status: String,
}

#[derive(Debug, Serialize, JsonSchema)]
struct ResearchGetRunOutput {
    run: ResearchRunSummary,
    actions: Vec<ResearchActionSummary>,
}

#[derive(Debug, Serialize, JsonSchema)]
struct ResearchRunSummary {
    id: Uuid,
    principal_id: Uuid,
    requested_by: Uuid,
    source_credential_id: Option<Uuid>,
    objective: String,
    status: String,
    budget: Value,
    usage: Value,
    started_at: Option<DateTime<Utc>>,
    finished_at: Option<DateTime<Utc>>,
    cancel_requested_at: Option<DateTime<Utc>>,
    created_at: DateTime<Utc>,
    allowed_actions: Vec<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
struct ResearchActionSummary {
    id: Uuid,
    tool_name: String,
    arguments_hash: String,
    redacted_arguments: Value,
    result_code: Option<String>,
    result_ref: Option<String>,
    approval_request_id: Option<Uuid>,
    has_external_effect: bool,
    trace_id: Option<String>,
    started_at: DateTime<Utc>,
    finished_at: Option<DateTime<Utc>>,
    created_at: DateTime<Utc>,
}

impl From<AgentRunDetailResponse> for ResearchGetRunOutput {
    fn from(value: AgentRunDetailResponse) -> Self {
        let status = match value.status {
            contracts::agents::AgentRunStatus::Queued => "queued",
            contracts::agents::AgentRunStatus::Running => "running",
            contracts::agents::AgentRunStatus::AwaitingApproval => "awaiting_approval",
            contracts::agents::AgentRunStatus::Completed => "completed",
            contracts::agents::AgentRunStatus::Failed => "failed",
            contracts::agents::AgentRunStatus::Canceled => "canceled",
        }
        .to_string();
        Self {
            run: ResearchRunSummary {
                id: value.id,
                principal_id: value.principal_id,
                requested_by: value.requested_by,
                source_credential_id: value.source_credential_id,
                objective: value.objective,
                status,
                budget: value.budget,
                usage: value.usage,
                started_at: value.started_at,
                finished_at: value.finished_at,
                cancel_requested_at: value.cancel_requested_at,
                created_at: value.created_at,
                allowed_actions: value.allowed_actions,
            },
            actions: value
                .actions
                .into_iter()
                .map(|a| ResearchActionSummary {
                    id: a.id,
                    tool_name: a.tool_name,
                    arguments_hash: a.arguments_hash,
                    redacted_arguments: a.redacted_arguments,
                    result_code: a.result_code,
                    result_ref: a.result_ref,
                    approval_request_id: a.approval_request_id,
                    has_external_effect: a.has_external_effect,
                    trace_id: a.trace_id,
                    started_at: a.started_at,
                    finished_at: a.finished_at,
                    created_at: a.created_at,
                })
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exposes_evidence_and_run_research_tools() {
        let tools = tools();
        let names: Vec<_> = tools.iter().map(|t| t.name).collect();
        assert_eq!(
            names,
            [
                "research_add_evidence",
                "research_get_run",
                "research_list_evidence",
                "research_get_evidence",
                "research_review_evidence",
            ]
        );
        assert!(tools.iter().all(|t| {
            !t.description.contains("proposal") && !t.description.contains("human review")
        }));
        assert!(tools.iter().all(|t| !t.external_effect));
        assert!(tools.iter().all(|t| {
            t.output_schema["required"]
                .as_array()
                .is_some_and(|fields| fields.iter().any(|f| f == "request_id"))
        }));
        assert_eq!(
            tools[0].input_schema["properties"]["idempotency_key"]["minLength"],
            16
        );
        assert_eq!(
            tools[0].input_schema["properties"]["idempotency_key"]["maxLength"],
            128
        );
    }

    #[test]
    fn research_tool_inputs_reject_unknown_fields() {
        let result = crate::arguments::<ResearchGetRunInput>(serde_json::json!({
            "run_id": Uuid::nil(),
            "workspace_id": Uuid::nil()
        }));
        assert!(result.is_err());
    }
}
