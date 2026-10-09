//! Test fixtures (spec §7). `seed_workspace` creates a full world:
//! workspace + admin/manager/member users with password credentials, an
//! agent principal, the system principal, and a mail account chain
//! (integration_connection → mail_account → grants). `ctx_for` builds the
//! `Ctx` an authenticated request would carry.

use contracts::{AppError, AuthMethod, Ctx, PrincipalKind, Role, ScopeSet};
use uuid::Uuid;

use crate::db::Db;
use crate::error::map_sqlx;
use crate::repo::auth as auth_repo;

/// Deterministic test password (meets the ≥12-char policy).
pub const TEST_PASSWORD: &str = "test-password-1234";

#[derive(Debug, Clone)]
pub struct TestPrincipal {
    pub principal_id: Uuid,
    /// None for agent/system principals.
    pub user_id: Option<Uuid>,
    pub email: Option<String>,
    pub role: Role,
    pub kind: PrincipalKind,
}

#[derive(Debug)]
pub struct TestWorld {
    pub workspace_id: Uuid,
    pub admin: TestPrincipal,
    pub manager: TestPrincipal,
    pub member_a: TestPrincipal,
    pub member_b: TestPrincipal,
    pub agent: TestPrincipal,
    pub system: TestPrincipal,
    pub mail_account_id: Uuid,
    pub connection_id: Uuid,
}

/// Build the `Ctx` a session-authenticated request by `p` would carry.
pub fn ctx_for(world: &TestWorld, p: &TestPrincipal, request_id: &str) -> Ctx {
    Ctx {
        request_id: request_id.to_string(),
        workspace_id: world.workspace_id,
        principal_id: p.principal_id,
        principal_kind: p.kind,
        role: p.role,
        scopes: p.role.implied_scopes(),
        permission_version: 1,
        auth: AuthMethod::Session {
            session_id: Uuid::nil(),
        },
        agent_run_id: None,
    }
}

/// Build a service-credential `Ctx` (e.g. agent/MCP requests).
pub fn ctx_for_credential(
    world: &TestWorld,
    p: &TestPrincipal,
    credential_id: Uuid,
    scopes: ScopeSet,
    request_id: &str,
) -> Ctx {
    Ctx {
        request_id: request_id.to_string(),
        workspace_id: world.workspace_id,
        principal_id: p.principal_id,
        principal_kind: p.kind,
        role: p.role,
        scopes,
        permission_version: 1,
        auth: AuthMethod::ServiceCredential { credential_id },
        agent_run_id: None,
    }
}

/// Seed a complete workspace. Uses `Db::begin`/`begin_global` so the fixture
/// exercises the same RLS path as production code. The pool identity must
/// own the schema (dev/test superuser) — `SET LOCAL ROLE` only applies when
/// the `Db` was built with a runtime role.
pub async fn seed_workspace(db: &Db) -> Result<TestWorld, AppError> {
    let workspace_id = auth_repo::insert_workspace(db, "Test Workspace").await?;
    // users.email is globally UNIQUE and the test DB persists across runs —
    // namespace every seeded address.
    let suffix = &workspace_id.simple().to_string()[..8];
    let email = |local: &str| format!("{local}+{suffix}@test.dev");

    async fn human(
        db: &Db,
        ws: Uuid,
        email: &str,
        name: &str,
        role: &str,
    ) -> Result<TestPrincipal, AppError> {
        let user = auth_repo::create_user(db, email, name).await?;
        let hash = crate::crypto::hash_password(TEST_PASSWORD)?;
        auth_repo::set_password_credential(db, user.id, &hash).await?;
        let mut tx = db.begin(ws).await?;
        let pid =
            auth_repo::insert_principal(&mut tx, "human", Some(user.id), name, role, "active")
                .await?;
        tx.commit().await?;
        Ok(TestPrincipal {
            principal_id: pid,
            user_id: Some(user.id),
            email: Some(email.to_string()),
            role: match role {
                "admin" => Role::Admin,
                "manager" => Role::Manager,
                _ => Role::Member,
            },
            kind: PrincipalKind::Human,
        })
    }

    let admin = human(db, workspace_id, &email("admin"), "Admin", "admin").await?;
    let manager = human(db, workspace_id, &email("manager"), "Manager", "manager").await?;
    let member_a = human(db, workspace_id, &email("member.a"), "Member A", "member").await?;
    let member_b = human(db, workspace_id, &email("member.b"), "Member B", "member").await?;

    let mut tx = db.begin(workspace_id).await?;
    let agent_id =
        auth_repo::insert_principal(&mut tx, "agent", None, "Test Agent", "agent", "active")
            .await?;
    let system_id =
        auth_repo::insert_principal(&mut tx, "system", None, "System", "system", "active").await?;
    let connection_id: Uuid = sqlx::query_scalar(
        "INSERT INTO integration_connections
            (workspace_id, provider, region, secret_ref, status, connected_by)
         VALUES ($1, 'zoho_mail', 'us', 'test-secret-ref', 'active', $2)
         RETURNING id",
    )
    .bind(workspace_id)
    .bind(admin.principal_id)
    .fetch_one(tx.conn())
    .await
    .map_err(map_sqlx)?;
    let mail_account_id: Uuid = sqlx::query_scalar(
        "INSERT INTO mail_accounts
            (workspace_id, connection_id, provider_account_id, primary_address, name, status)
         VALUES ($1, $2, 'fake-acct-1', 'shared@test.dev', 'Shared Inbox', 'active')
         RETURNING id",
    )
    .bind(workspace_id)
    .bind(connection_id)
    .fetch_one(tx.conn())
    .await
    .map_err(map_sqlx)?;
    for pid in [
        admin.principal_id,
        manager.principal_id,
        member_a.principal_id,
    ] {
        sqlx::query(
            "INSERT INTO mail_account_grants
                (workspace_id, mail_account_id, principal_id,
                 can_read, can_compose, can_send, can_approve, can_assign, granted_by)
             VALUES ($1, $2, $3, true, true, true, true, true, $4)",
        )
        .bind(workspace_id)
        .bind(mail_account_id)
        .bind(pid)
        .bind(admin.principal_id)
        .execute(tx.conn())
        .await
        .map_err(map_sqlx)?;
    }
    tx.commit().await?;

    Ok(TestWorld {
        workspace_id,
        admin,
        manager,
        member_a,
        member_b,
        agent: TestPrincipal {
            principal_id: agent_id,
            user_id: None,
            email: None,
            role: Role::Agent,
            kind: PrincipalKind::Agent,
        },
        system: TestPrincipal {
            principal_id: system_id,
            user_id: None,
            email: None,
            role: Role::System,
            kind: PrincipalKind::System,
        },
        mail_account_id,
        connection_id,
    })
}
