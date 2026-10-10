//! Template MCP tools (B.6.2). Mutations use the same application use case
//! as REST and bind the explicit MCP idempotency key to its request body.

use crate::{ToolDef, arguments, result, tool_with_db};
use contracts::templates::{
    CreateEmailTemplateRequest, CreateTemplateVersionRequest, CreateTemplateVersionResponse,
    EmailTemplateResponse,
};
use schemars::JsonSchema;
use serde::Deserialize;
use uuid::Uuid;

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct CreateTemplateInput {
    /// Stable ASCII key of 16 to 128 characters for replaying this operation.
    idempotency_key: String,
    /// Human-readable name for this reusable template container.
    name: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct CreateDraftVersionInput {
    template_id: Uuid,
    /// Stable ASCII key of 16 to 128 characters for replaying this operation.
    idempotency_key: String,
    subject: String,
    body_text: String,
    body_html: Option<String>,
    variable_schema: serde_json::Value,
}

fn validate_idempotency_key(key: &str) -> Result<(), contracts::AppError> {
    if !key.is_ascii() || !(16..=128).contains(&key.len()) {
        return Err(contracts::AppError::validation(
            "idempotency_key must be 16..=128 ASCII characters",
            vec![contracts::Violation {
                field: "idempotency_key".into(),
                code: "LENGTH".into(),
            }],
        ));
    }
    Ok(())
}

pub fn tools() -> Vec<ToolDef> {
    let create = tool_with_db(
        "templates_create",
        "Create a reusable email-template container. Requires templates.write. Use the returned template_id to add an immutable content version with templates_create_draft_version. This does not create a message, authorize approval, or send email.",
        serde_json::to_value(schemars::schema_for!(CreateTemplateInput))
            .expect("schema serializes"),
        serde_json::to_value(schemars::schema_for!(EmailTemplateResponse))
            .expect("schema serializes"),
        false,
        |db, ctx, value| async move {
            let input = arguments::<CreateTemplateInput>(value)?;
            validate_idempotency_key(&input.idempotency_key)?;
            let request = CreateEmailTemplateRequest { name: input.name };
            let request_hash = infrastructure::crypto::sha256_bytes(
                &serde_json::to_vec(&serde_json::json!({"name": &request.name}))
                    .unwrap_or_default(),
            );
            let scope = infrastructure::idempotency::Scope {
                workspace_id: ctx.workspace_id,
                principal_or_capability: ctx.principal_id.to_string(),
                endpoint: "MCP tools/call templates_create".into(),
                key: input.idempotency_key,
            };
            let outcome = infrastructure::idempotency::run(&db, &scope, &request_hash, |mut tx| {
                let ctx = ctx.clone();
                let request = request;
                async move {
                    let body =
                        application::templates::create_in_tx(&mut tx, &ctx, &request).await?;
                    let body = serde_json::to_value(body).map_err(|e| {
                        contracts::AppError::internal(format!("template response: {e}"))
                    })?;
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
    );

    let create_version = tool_with_db(
        "templates_create_draft_version",
        "Create a new immutable email-template version. Requires templates.write. Provide subject, plain text, optional validated HTML and a closed variable_schema; use templates_render_version to preview. System-owned URLs cannot have defaults or caller-supplied values. Saving content does not create, approve, or send a message.",
        serde_json::to_value(schemars::schema_for!(CreateDraftVersionInput))
            .expect("schema serializes"),
        serde_json::to_value(schemars::schema_for!(CreateTemplateVersionResponse))
            .expect("schema serializes"),
        false,
        |db, ctx, value| async move {
            let input = arguments::<CreateDraftVersionInput>(value)?;
            validate_idempotency_key(&input.idempotency_key)?;
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
    );

    vec![create, create_version]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exposes_template_authoring_tools_with_explicit_keys() {
        let tools = tools();
        assert_eq!(tools.len(), 2);
        assert_eq!(tools[0].name, "templates_create");
        assert_eq!(tools[1].name, "templates_create_draft_version");
        for tool in &tools {
            assert_eq!(tool.input_schema["additionalProperties"], false);
            assert!(
                tool.input_schema["required"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|v| v == "idempotency_key")
            );
        }
    }

    #[test]
    fn idempotency_key_requires_16_to_128_ascii_characters() {
        assert!(validate_idempotency_key(&"a".repeat(15)).is_err());
        assert!(validate_idempotency_key(&"a".repeat(16)).is_ok());
        assert!(validate_idempotency_key(&"a".repeat(128)).is_ok());
        assert!(validate_idempotency_key(&"a".repeat(129)).is_err());
        assert!(validate_idempotency_key("é").is_err());
        assert!(validate_idempotency_key(&"é".repeat(128)).is_err());
    }
}
