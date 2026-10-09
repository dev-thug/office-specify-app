//! Research MCP tools (B.6): candidate proposals, evidence, and run status.
//! These tools only write pending review data; they do not accept a lead,
//! create consent, fetch source URLs, or send messages.

use std::future::Future;

use chrono::{DateTime, Utc};
use contracts::agents::AgentRunDetailResponse;
use contracts::research::{
    CreateEvidenceRequest, CreateLeadProposalsRequest, EvidenceType, LeadProposalInput,
};
use contracts::{AppError, Ctx};
use infrastructure::db::{Db, Tx};
use infrastructure::idempotency::{self, Scope, Stored};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;
use uuid::Uuid;

use crate::{ToolDef, tool_with_db};

const PROPOSE_LEADS_ENDPOINT: &str = "MCP research_propose_leads";
const ADD_EVIDENCE_ENDPOINT: &str = "MCP research_add_evidence";

pub fn tools() -> Vec<ToolDef> {
    vec![
        tool_with_db(
            "research_propose_leads",
            "Store up to 100 sourced lead candidates for human review. This does not create CRM records, establish communication consent, or send messages. Never guess email addresses; submit only addresses present in evidence.",
            idempotent_schema::<ResearchProposeLeadsInput>(),
            envelope_schema::<ResearchProposeLeadsOutput>(),
            false,
            |db, ctx, args| async move {
                let input = crate::arguments::<ResearchProposeLeadsInput>(args)?;
                let body = serde_json::to_value(&input)
                    .map_err(|e| AppError::internal(format!("research arguments: {e}")))?;
                let request = CreateLeadProposalsRequest {
                    agent_run_id: Some(input.agent_run_id),
                    proposals: input.proposals,
                };
                let key = input.idempotency_key;
                let run_ctx = ctx.clone();
                let response = run_idempotent(
                    &db,
                    &ctx,
                    PROPOSE_LEADS_ENDPOINT,
                    &key,
                    &body,
                    201,
                    move |mut tx| {
                        let ctx = run_ctx;
                        async move {
                            let created =
                                application::research::create_proposals_tx(&mut tx, &ctx, &request)
                                    .await?;
                            let output = ResearchProposeLeadsOutput {
                                proposal_ids: created.proposals.iter().map(|p| p.id).collect(),
                                status: "pending".into(),
                                duplicate_candidates: created
                                    .proposals
                                    .into_iter()
                                    .filter(|p| !p.duplicate_company_ids.is_empty())
                                    .map(|p| DuplicateCandidate {
                                        proposal_id: p.id,
                                        company_ids: p.duplicate_company_ids,
                                    })
                                    .collect(),
                            };
                            Ok((tx, output))
                        }
                    },
                )
                .await?;
                crate::result(response)
            },
        ),
        tool_with_db(
            "research_add_evidence",
            "Attach one public-source observation to a company, contact, or lead proposal in your research run. Evidence remains pending human review; source URLs are validated and stored but never fetched by this tool.",
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
            "Read the status and redacted action history for a research run you are allowed to access.",
            schema::<ResearchGetRunInput>(),
            envelope_schema::<ResearchGetRunOutput>(),
            false,
            |db, ctx, args| async move {
                let input = crate::arguments::<ResearchGetRunInput>(args)?;
                let detail = application::agents::get_run_detail(&db, &ctx, input.run_id).await?;
                crate::result(ResearchGetRunOutput::from(detail))
            },
        ),
    ]
}

/// Transaction-preserving MCP idempotency. The payload and the stored
/// response commit together, so a retry replays the same result without
/// creating another proposal/evidence row.
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
struct ResearchProposeLeadsInput {
    agent_run_id: Uuid,
    idempotency_key: String,
    proposals: Vec<LeadProposalInput>,
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

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct ResearchProposeLeadsOutput {
    proposal_ids: Vec<Uuid>,
    status: String,
    duplicate_candidates: Vec<DuplicateCandidate>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct DuplicateCandidate {
    proposal_id: Uuid,
    company_ids: Vec<Uuid>,
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
    fn exposes_the_three_read_and_propose_research_tools() {
        let tools = tools();
        let names: Vec<_> = tools.iter().map(|t| t.name).collect();
        assert_eq!(
            names,
            [
                "research_propose_leads",
                "research_add_evidence",
                "research_get_run"
            ]
        );
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
            tools[1].input_schema["properties"]["idempotency_key"]["maxLength"],
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
