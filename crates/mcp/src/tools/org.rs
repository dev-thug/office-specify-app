//! org module MCP tools: `/me`, principals, member invitations, agents and
//! service credentials, and the workspace emergency stop. Each tool mirrors
//! the REST handler in `crates/api/src/routes/org.rs` and calls the same
//! application use cases (authorization lives there).

use chrono::{DateTime, Utc};
use contracts::org::{
    CreateAgentRequest, CreateCredentialRequest, CreateInvitationRequest, DisableRequestRequest,
    EmergencyStopRequest, PatchPrincipalRequest, PrincipalListItem, PrincipalPickerResponse,
};
use contracts::{AppError, Ctx, Role};
use infrastructure::cursor::Cursor;
use infrastructure::idempotency::{self, Stored};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Map, Value, json};
use uuid::Uuid;

use crate::{ToolDef, ToolServices, arguments, result, tool_with_db, tool_with_services};

const IDEMPOTENCY_KEY_MIN_LEN: usize = 16;
const PAGE_DEFAULT_LIMIT: u32 = 50;
const PAGE_MAX_LIMIT: u32 = 200;

fn schema<T: JsonSchema>() -> Value {
    serde_json::to_value(schemars::schema_for!(T)).expect("schema serializes")
}

fn object_schema() -> Value {
    json!({"type": "object"})
}

fn page_schema() -> Value {
    json!({
        "type": "object",
        "required": ["items", "next_cursor", "has_more"],
        "properties": {
            "items": {"type": "array", "items": {"type": "object"}},
            "next_cursor": {"type": ["string", "null"]},
            "has_more": {"type": "boolean"}
        }
    })
}

fn items_schema() -> Value {
    json!({
        "type": "object",
        "required": ["items"],
        "properties": {"items": {"type": "array", "items": {"type": "object"}}}
    })
}

fn validate_idempotency_key(key: &str) -> Result<(), AppError> {
    if (IDEMPOTENCY_KEY_MIN_LEN..=idempotency::KEY_MAX_LEN).contains(&key.len()) {
        Ok(())
    } else {
        Err(AppError::validation(
            "idempotency_key must be 16..=128 characters",
            vec![contracts::Violation {
                field: "idempotency_key".into(),
                code: "LENGTH".into(),
            }],
        ))
    }
}

/// Same limits as the REST `PageParams` extractor (default 50, max 200, >0).
fn page_limit(limit: Option<u32>) -> Result<i64, AppError> {
    match limit {
        None => Ok(PAGE_DEFAULT_LIMIT as i64),
        Some(0) => Err(AppError::validation(
            "invalid pagination parameters",
            vec![contracts::Violation {
                field: "limit".into(),
                code: "invalid".into(),
            }],
        )),
        Some(n) => Ok(n.min(PAGE_MAX_LIMIT) as i64),
    }
}

/// Decode a signed `(created_at, id)` keyset cursor (same as REST).
fn keyset_after(
    hmac_key: &[u8],
    ctx: &Ctx,
    cursor: Option<&str>,
    filter_hash: &str,
) -> Result<Option<(DateTime<Utc>, Uuid)>, AppError> {
    let Some(raw) = cursor else { return Ok(None) };
    let cur = Cursor::decode(raw, hmac_key, ctx.principal_id, filter_hash)?;
    let mut it = cur.sort_values.iter();
    let (Some(Value::String(ts)), Some(Value::String(id))) = (it.next(), it.next()) else {
        return Err(AppError::validation("invalid pagination cursor", vec![]));
    };
    let ts = DateTime::parse_from_rfc3339(ts)
        .map(|t| t.with_timezone(&Utc))
        .map_err(|_| AppError::validation("invalid pagination cursor", vec![]))?;
    let id = id
        .parse::<Uuid>()
        .map_err(|_| AppError::validation("invalid pagination cursor", vec![]))?;
    Ok(Some((ts, id)))
}

fn next_cursor(
    hmac_key: &[u8],
    ctx: &Ctx,
    seed: Option<(DateTime<Utc>, Uuid)>,
    filter_hash: &str,
) -> Option<String> {
    seed.map(|(ts, id)| {
        Cursor::new(
            vec![json!(ts.to_rfc3339()), json!(id)],
            filter_hash.to_string(),
            ctx.principal_id,
        )
        .encode(hmac_key)
    })
}

/// Same canonical request hash as the REST handlers so a key retried on
/// either surface resolves to the same stored outcome.
fn request_hash<T: serde::Serialize>(tag: &str, body: &T) -> Vec<u8> {
    let mut bytes = tag.as_bytes().to_vec();
    bytes.extend_from_slice(&serde_json::to_vec(body).unwrap_or_else(|_| b"{}".to_vec()));
    infrastructure::crypto::sha256_bytes(&bytes).to_vec()
}

fn idempotency_scope(ctx: &Ctx, endpoint: &str, key: String) -> idempotency::Scope {
    idempotency::Scope {
        workspace_id: ctx.workspace_id,
        principal_or_capability: ctx.principal_id.to_string(),
        endpoint: endpoint.into(),
        key,
    }
}

/// Base URL for the one-time invitation link (mirrors REST `invite_base`).
fn invite_base(services: &ToolServices) -> String {
    std::env::var("OUTREACH_PUBLIC_BASE_URL")
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| format!("http://{}", services.config.api_addr))
}

// --------------------------------------------------------------------------
// inputs
// --------------------------------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct EmptyInput {}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct PageInput {
    /// Page size (default 50, max 200).
    #[serde(default)]
    limit: Option<u32>,
    /// `next_cursor` from the previous page.
    #[serde(default)]
    cursor: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ListPrincipalsInput {
    #[serde(default)]
    limit: Option<u32>,
    #[serde(default)]
    cursor: Option<String>,
    /// `human` | `agent` | `system` (admin only; others always see active humans).
    #[serde(default)]
    kind: Option<String>,
    /// `admin` | `manager` | `member` | `agent` | `system`.
    #[serde(default)]
    role: Option<String>,
    /// `invited` | `active` | `disabled` (admin only).
    #[serde(default)]
    status: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct IdInput {
    id: Uuid,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct UpdatePrincipalInput {
    principal_id: Uuid,
    /// Current principal `version` (the REST ETag / If-Match value).
    expected_version: i64,
    /// Fields to change: `role` (`admin` | `manager` | `member`) and/or
    /// `status` (`active` | `disabled`).
    changes: Map<String, Value>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct DisablePrincipalInput {
    principal_id: Uuid,
    /// Active human principal that receives the disabled principal's records.
    #[serde(default)]
    reassign_to: Option<Uuid>,
    /// Cancel not-yet-dispatched work owned by the principal (default true).
    #[serde(default)]
    cancel_pending: Option<bool>,
    #[schemars(length(min = 16, max = 128))]
    idempotency_key: String,
}

#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum HumanRole {
    Admin,
    Manager,
    Member,
}

impl From<HumanRole> for Role {
    fn from(role: HumanRole) -> Self {
        match role {
            HumanRole::Admin => Role::Admin,
            HumanRole::Manager => Role::Manager,
            HumanRole::Member => Role::Member,
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct CreateInvitationInput {
    email: String,
    role: HumanRole,
    #[schemars(length(min = 16, max = 128))]
    idempotency_key: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct CreateAgentInput {
    name: String,
    /// Intended scopes the agent's credentials may carry.
    #[serde(default)]
    scopes: Option<Vec<String>>,
    #[schemars(length(min = 16, max = 128))]
    idempotency_key: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct CreateCredentialInput {
    agent_id: Uuid,
    name: String,
    scopes: Vec<String>,
    /// Days until expiry; default 365, max 730.
    #[serde(default)]
    expires_in_days: Option<u32>,
    /// Issue without a practical expiry (overrides `expires_in_days`).
    #[serde(default)]
    never_expires: bool,
    #[schemars(length(min = 16, max = 128))]
    idempotency_key: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct SetEmergencyStopInput {
    /// true engages the stop (new submissions halt); false resumes.
    enabled: bool,
    /// Required, audited reason.
    reason: String,
    /// Current `policy_version` (the REST ETag / If-Match value).
    expected_version: i64,
}

// --------------------------------------------------------------------------
// tools
// --------------------------------------------------------------------------

/// Tools this module exposes over MCP.
pub fn tools() -> Vec<ToolDef> {
    vec![
        tool_with_db(
            "org_get_me",
            "Return the calling principal: kind, role, workspace, display name, permission version and mailbox grants with allowed actions.",
            schema::<EmptyInput>(),
            object_schema(),
            false,
            |db, ctx, args| async move {
                let _: EmptyInput = arguments(args)?;
                let mut tx = db.begin(ctx.workspace_id).await?;
                let principal =
                    infrastructure::repo::auth::get_principal(&mut tx, ctx.principal_id)
                        .await?
                        .ok_or_else(|| AppError::not_found("principal not found"))?;
                tx.commit().await?;
                let res = application::org::me::me_for_principal(&db, &principal).await?;
                result(res)
            },
        ),
        tool_with_services(
            "org_list_principals",
            "List workspace principals (admins, and agents with the org.admin scope, get full detail with kind/role/status filters; others get the active-human picker). Paginated with limit/cursor.",
            schema::<ListPrincipalsInput>(),
            page_schema(),
            false,
            |services, ctx, args| async move {
                let input: ListPrincipalsInput = arguments(args)?;
                let limit = page_limit(input.limit)?;
                application::org::admin::check_principal_picker_read(&services.db, &ctx).await?;
                let admin = ctx.role == Role::Admin;
                let filter = if admin {
                    application::org::admin::PrincipalFilter {
                        kind: input.kind,
                        role: input.role,
                        status: input.status,
                    }
                } else {
                    application::org::admin::PrincipalFilter {
                        kind: Some("human".into()),
                        role: input.role,
                        status: Some("active".into()),
                    }
                };
                let filter_hash = Cursor::hash_filter(&json!({
                    "kind": filter.kind,
                    "role": filter.role,
                    "status": filter.status,
                }));
                let key = &services.config.hmac_key;
                let after = keyset_after(key, &ctx, input.cursor.as_deref(), &filter_hash)?;
                let res = application::org::admin::list_principals(
                    &services.db,
                    &ctx,
                    &filter,
                    after,
                    limit,
                )
                .await?;
                let cursor = next_cursor(key, &ctx, res.cursor_seed, &filter_hash);
                let items: Vec<PrincipalListItem> = res
                    .items
                    .into_iter()
                    .map(|principal| {
                        if admin {
                            PrincipalListItem::Detail(principal)
                        } else {
                            PrincipalListItem::Picker(PrincipalPickerResponse {
                                id: principal.id,
                                kind: principal.kind,
                                name: principal.name,
                                role: principal.role,
                                status: principal.status,
                            })
                        }
                    })
                    .collect();
                result(json!({"items": items, "next_cursor": cursor, "has_more": res.has_more}))
            },
        ),
        tool_with_db(
            "org_get_principal",
            "Get one principal's detail including `version` (pass it as expected_version to org_update_principal). Admin only (agents need the org.admin scope).",
            schema::<IdInput>(),
            object_schema(),
            false,
            |db, ctx, args| async move {
                let input: IdInput = arguments(args)?;
                application::org::admin::check_roster_read(
                    &db,
                    &ctx,
                    "principal.read",
                    Some(input.id),
                )
                .await?;
                result(application::org::admin::get_principal(&db, &ctx, input.id).await?)
            },
        ),
        tool_with_db(
            "org_update_principal",
            "Change a principal's role and/or status. `changes` accepts `role` (admin|manager|member) and `status` (active|disabled); requires expected_version. Admin only (agents need the org.admin scope); use org_request_disable_principal for a full disable with reassignment.",
            schema::<UpdatePrincipalInput>(),
            object_schema(),
            false,
            |db, ctx, args| async move {
                let input: UpdatePrincipalInput = arguments(args)?;
                let req: PatchPrincipalRequest =
                    serde_json::from_value(Value::Object(input.changes)).map_err(|e| {
                        AppError::validation(format!("invalid changes: {e}"), vec![])
                    })?;
                let res = application::org::admin::patch_principal(
                    &db,
                    &ctx,
                    input.principal_id,
                    input.expected_version,
                    &req,
                )
                .await?;
                result(res)
            },
        ),
        tool_with_db(
            "org_request_disable_principal",
            "Disable a principal asynchronously: owned records go to `reassign_to`, pending work is cancelled unless cancel_pending=false. Returns the cleanup job_id. Admin only (agents need the org.admin scope); requires idempotency_key.",
            schema::<DisablePrincipalInput>(),
            object_schema(),
            false,
            |db, ctx, args| async move {
                let input: DisablePrincipalInput = arguments(args)?;
                validate_idempotency_key(&input.idempotency_key)?;
                let id = input.principal_id;
                application::org::admin::check_admin(
                    &db,
                    &ctx,
                    "principal.disable",
                    "principal",
                    Some(id),
                )
                .await?;
                let req = DisableRequestRequest {
                    reassign_to: input.reassign_to,
                    cancel_pending: input.cancel_pending,
                };
                let scope = idempotency_scope(
                    &ctx,
                    "POST /api/v1/principals/{id}/disable-requests",
                    input.idempotency_key,
                );
                let hash = request_hash(&id.to_string(), &req);
                let outcome = idempotency::run(&db, &scope, &hash, move |mut tx| async move {
                    let res = application::org::admin::request_disable_tx(&mut tx, &ctx, id, &req)
                        .await?;
                    Ok((
                        tx,
                        Stored {
                            status: 202,
                            body: res,
                        },
                    ))
                })
                .await?;
                result(outcome.into_inner().body)
            },
        ),
        tool_with_services(
            "org_create_invitation",
            "Create a member invitation for an email and human role. The one-time `invite_url` is returned only on the first (non-replayed) call; no email is sent. Admin only (agents need the org.admin scope); requires idempotency_key.",
            schema::<CreateInvitationInput>(),
            object_schema(),
            false,
            |services, ctx, args| async move {
                let input: CreateInvitationInput = arguments(args)?;
                validate_idempotency_key(&input.idempotency_key)?;
                let db = services.db.clone();
                let invite_base = invite_base(&services);
                application::org::admin::check_admin(
                    &db,
                    &ctx,
                    "member_invitation.create",
                    "member_invitation",
                    None,
                )
                .await?;
                let req = CreateInvitationRequest {
                    email: input.email,
                    role: input.role.into(),
                };
                let scope = idempotency_scope(
                    &ctx,
                    "POST /api/v1/member-invitations",
                    input.idempotency_key,
                );
                let hash = request_hash("", &req);
                // The stored body never holds the one-time URL; splice it back
                // only into the fresh response (same as REST).
                let captured = std::sync::Arc::new(std::sync::Mutex::new(None::<String>));
                let outcome = idempotency::run(&db, &scope, &hash, {
                    let captured = captured.clone();
                    move |mut tx| async move {
                        let mut res = application::org::admin::create_invitation_tx(
                            &mut tx,
                            &ctx,
                            &req,
                            &invite_base,
                        )
                        .await?;
                        *captured.lock().unwrap() = res.invite_url.take();
                        Ok((
                            tx,
                            Stored {
                                status: 201,
                                body: res,
                            },
                        ))
                    }
                })
                .await?;
                let is_replay = outcome.is_replay();
                let mut stored = outcome.into_inner();
                if !is_replay {
                    stored.body.invite_url = captured.lock().unwrap().take();
                }
                result(stored.body)
            },
        ),
        tool_with_services(
            "org_list_invitations",
            "List member invitations with accepted/revoked state. Admin only (agents need the org.admin scope). Paginated with limit/cursor.",
            schema::<PageInput>(),
            page_schema(),
            false,
            |services, ctx, args| async move {
                let input: PageInput = arguments(args)?;
                let limit = page_limit(input.limit)?;
                let key = &services.config.hmac_key;
                let filter_hash = Cursor::hash_filter(&json!({"list": "member-invitations"}));
                let after = keyset_after(key, &ctx, input.cursor.as_deref(), &filter_hash)?;
                let res =
                    application::org::admin::list_invitations(&services.db, &ctx, after, limit)
                        .await?;
                let cursor = next_cursor(key, &ctx, res.cursor_seed, &filter_hash);
                result(json!({"items": res.items, "next_cursor": cursor, "has_more": res.has_more}))
            },
        ),
        tool_with_db(
            "org_revoke_invitation",
            "Revoke a pending member invitation by id (idempotent). Admin only (agents need the org.admin scope).",
            schema::<IdInput>(),
            object_schema(),
            false,
            |db, ctx, args| async move {
                let input: IdInput = arguments(args)?;
                result(application::org::admin::revoke_invitation(&db, &ctx, input.id).await?)
            },
        ),
        tool_with_db(
            "org_create_agent",
            "Create an agent principal with a name and optional intended scopes. Admin only (agents need the org.admin scope); requires idempotency_key. Issue its token with org_create_credential.",
            schema::<CreateAgentInput>(),
            object_schema(),
            false,
            |db, ctx, args| async move {
                let input: CreateAgentInput = arguments(args)?;
                validate_idempotency_key(&input.idempotency_key)?;
                application::org::admin::check_admin(&db, &ctx, "agent.create", "principal", None)
                    .await?;
                let req = CreateAgentRequest {
                    name: input.name,
                    scopes: input.scopes,
                };
                let scope = idempotency_scope(&ctx, "POST /api/v1/agents", input.idempotency_key);
                let hash = request_hash("", &req);
                let outcome = idempotency::run(&db, &scope, &hash, move |mut tx| async move {
                    let res = application::org::admin::create_agent_tx(&mut tx, &ctx, &req).await?;
                    Ok((
                        tx,
                        Stored {
                            status: 201,
                            body: res,
                        },
                    ))
                })
                .await?;
                result(outcome.into_inner().body)
            },
        ),
        tool_with_db(
            "org_create_credential",
            "Issue a service credential (bearer token) for an agent with the given scopes and expiry. The `secret` token is returned only once, on the first (non-replayed) call. Admin only (agents need the org.admin scope); requires idempotency_key.",
            schema::<CreateCredentialInput>(),
            object_schema(),
            false,
            |db, ctx, args| async move {
                let input: CreateCredentialInput = arguments(args)?;
                validate_idempotency_key(&input.idempotency_key)?;
                let agent_id = input.agent_id;
                application::org::admin::check_admin(
                    &db,
                    &ctx,
                    "credential.create",
                    "service_credential",
                    None,
                )
                .await?;
                let req = CreateCredentialRequest {
                    name: input.name,
                    scopes: input.scopes,
                    expires_in_days: input.expires_in_days,
                    never_expires: input.never_expires,
                };
                let scope = idempotency_scope(
                    &ctx,
                    "POST /api/v1/agents/{id}/credentials",
                    input.idempotency_key,
                );
                let hash = request_hash(&agent_id.to_string(), &req);
                // `secret` is stripped before persistence and spliced back only
                // on the fresh response (same as REST).
                let captured = std::sync::Arc::new(std::sync::Mutex::new(None::<String>));
                let outcome = idempotency::run(&db, &scope, &hash, {
                    let captured = captured.clone();
                    move |mut tx| async move {
                        let mut res = application::org::admin::create_credential_tx(
                            &mut tx, &ctx, agent_id, &req,
                        )
                        .await?;
                        *captured.lock().unwrap() = res.secret.take();
                        Ok((
                            tx,
                            Stored {
                                status: 201,
                                body: res,
                            },
                        ))
                    }
                })
                .await?;
                let is_replay = outcome.is_replay();
                let mut stored = outcome.into_inner();
                if !is_replay {
                    stored.body.secret = captured.lock().unwrap().take();
                }
                result(stored.body)
            },
        ),
        tool_with_db(
            "org_list_credentials",
            "List all service credentials (prefix, scopes, expiry, last use, revocation); secrets are never returned. Admin only (agents need the org.admin scope).",
            schema::<EmptyInput>(),
            items_schema(),
            false,
            |db, ctx, args| async move {
                let _: EmptyInput = arguments(args)?;
                let items = application::org::admin::list_credentials(&db, &ctx).await?;
                result(json!({"items": items}))
            },
        ),
        tool_with_db(
            "org_revoke_credential",
            "Revoke a service credential by id (idempotent); pending work under it is stopped. Admin only (agents need the org.admin scope).",
            schema::<IdInput>(),
            object_schema(),
            false,
            |db, ctx, args| async move {
                let input: IdInput = arguments(args)?;
                result(application::org::admin::revoke_credential(&db, &ctx, input.id).await?)
            },
        ),
        tool_with_db(
            "org_get_emergency_stop",
            "Read the workspace emergency-stop state and its policy_version (pass it as expected_version to org_set_emergency_stop).",
            schema::<EmptyInput>(),
            object_schema(),
            false,
            |db, ctx, args| async move {
                let _: EmptyInput = arguments(args)?;
                result(application::org::admin::emergency_stop_state(&db, &ctx).await?)
            },
        ),
        tool_with_db(
            "org_set_emergency_stop",
            "Engage (enabled=true, admin/manager) or release (enabled=false, admin) the workspace emergency stop that halts new submissions; agents need the org.admin scope. Requires a reason and expected_version (current policy_version).",
            schema::<SetEmergencyStopInput>(),
            object_schema(),
            false,
            |db, ctx, args| async move {
                let input: SetEmergencyStopInput = arguments(args)?;
                let req = EmergencyStopRequest {
                    enabled: input.enabled,
                    reason: input.reason,
                };
                let res = application::org::admin::set_emergency_stop(
                    &db,
                    &ctx,
                    input.expected_version,
                    &req,
                )
                .await?;
                result(res)
            },
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn org_tools_are_unique_and_keyed_mutations_require_idempotency() {
        let defs = tools();
        let names: std::collections::HashSet<_> = defs.iter().map(|t| t.name).collect();
        assert_eq!(names.len(), defs.len());
        for name in [
            "org_request_disable_principal",
            "org_create_invitation",
            "org_create_agent",
            "org_create_credential",
        ] {
            let def = defs.iter().find(|t| t.name == name).unwrap();
            assert!(
                def.input_schema["required"]
                    .as_array()
                    .is_some_and(|f| f.iter().any(|v| v == "idempotency_key")),
                "{name}"
            );
        }
        for name in ["org_update_principal", "org_set_emergency_stop"] {
            let def = defs.iter().find(|t| t.name == name).unwrap();
            assert!(
                def.input_schema["required"]
                    .as_array()
                    .is_some_and(|f| f.iter().any(|v| v == "expected_version")),
                "{name}"
            );
        }
    }

    #[test]
    fn page_limit_matches_rest() {
        assert_eq!(page_limit(None).unwrap(), 50);
        assert_eq!(page_limit(Some(500)).unwrap(), 200);
        assert!(page_limit(Some(0)).is_err());
    }
}
