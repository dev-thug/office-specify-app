//! Template MCP tools (B.6.2). Mutations use the same application use case
//! as REST and bind the explicit MCP idempotency key to its request body.

use crate::{ToolDef, arguments, result, tool_with_db};
use contracts::templates::{CreateTemplateVersionRequest, CreateTemplateVersionResponse};
use schemars::JsonSchema;
use serde::Deserialize;
use uuid::Uuid;

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct CreateDraftVersionInput {
    template_id: Uuid,
    idempotency_key: String,
    subject: String,
    body_text: String,
    body_html: Option<String>,
    variable_schema: serde_json::Value,
}

pub fn tools() -> Vec<ToolDef> {
    vec![tool_with_db(
        "templates_create_draft_version",
        "Create a new immutable email-template version. This only creates a template version; it does not create or send a message. System-owned URLs cannot be supplied in variable_schema defaults or render values.",
        serde_json::to_value(schemars::schema_for!(CreateDraftVersionInput))
            .expect("schema serializes"),
        serde_json::to_value(schemars::schema_for!(CreateTemplateVersionResponse))
            .expect("schema serializes"),
        false,
        |db, ctx, value| async move {
            let input = arguments::<CreateDraftVersionInput>(value)?;
            let idempotency_key = input.idempotency_key.clone();
            let request = CreateTemplateVersionRequest {
                subject: input.subject,
                body_text: input.body_text,
                body_html: input.body_html,
                variable_schema: input.variable_schema,
            };
            let request_hash = infrastructure::crypto::sha256_bytes(
                &serde_json::to_vec(&serde_json::json!({
                    "template_id": input.template_id,
                    "subject": &request.subject,
                    "body_text": &request.body_text,
                    "body_html": &request.body_html,
                    "variable_schema": &request.variable_schema,
                }))
                .unwrap_or_default(),
            );
            let scope = infrastructure::idempotency::Scope {
                workspace_id: ctx.workspace_id,
                principal_or_capability: ctx.principal_id.to_string(),
                endpoint: "MCP tools/call templates_create_draft_version".into(),
                key: idempotency_key,
            };
            let template_id = input.template_id;
            let outcome = infrastructure::idempotency::run(&db, &scope, &request_hash, |mut tx| {
                let ctx = ctx.clone();
                let request = request;
                async move {
                    let body = application::templates::create_version_in_tx(
                        &mut tx,
                        &ctx,
                        template_id,
                        &request,
                    )
                    .await?;
                    Ok((
                        tx,
                        infrastructure::idempotency::Stored { status: 201, body },
                    ))
                }
            })
            .await?
            .into_inner();
            result(outcome.body)
        },
    )]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exposes_only_template_draft_version_tool_with_explicit_key() {
        let tools = tools();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "templates_create_draft_version");
        assert_eq!(tools[0].input_schema["additionalProperties"], false);
        assert!(
            tools[0].input_schema["required"]
                .as_array()
                .unwrap()
                .iter()
                .any(|v| v == "idempotency_key")
        );
    }
}
