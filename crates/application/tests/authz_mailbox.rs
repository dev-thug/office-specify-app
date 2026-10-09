//! R7: `require_mailbox` is fully conjunctive — grant row AND §5.4 role
//! clause AND credential scope. A member holding a `can_approve` grant must
//! still be denied; a credential ctx missing the scope is denied even when
//! role and grant line up.
//!
//! The seed fixture already grants member_a/manager every mailbox
//! permission, so the grant conjunct is satisfied — the denials below come
//! from the role clause and the scope clause respectively.

use application::authz::{MailboxPerm, require_mailbox};
use contracts::{ErrorCode, ScopeSet};
use infrastructure::db::{Db, ROLE_API};
use infrastructure::testing::{ctx_for, ctx_for_credential, seed_workspace};
use uuid::Uuid;

async fn test_db() -> Db {
    let url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set for DB tests");
    let owner = Db::connect(&url, None).await.unwrap();
    owner.migrate().await.unwrap();
    Db::connect(&url, Some(ROLE_API)).await.unwrap()
}

#[tokio::test]
async fn member_approve_grant_still_denied() {
    let db = test_db().await;
    let world = seed_workspace(&db).await.unwrap();
    let mut tx = db.begin(world.workspace_id).await.unwrap();

    // Prove the grant conjunct is satisfied — the denial is the §5.4 role
    // clause, not a missing grant.
    let grant = application::authz::mailbox_grant(
        &mut tx,
        &ctx_for(&world, &world.member_a, "r7-check"),
        world.mail_account_id,
    )
    .await
    .unwrap()
    .expect("seed grants member_a full mailbox permissions");
    assert!(grant.can_approve);

    let ctx = ctx_for(&world, &world.member_a, "r7-member");
    let err = require_mailbox(&mut tx, &ctx, world.mail_account_id, MailboxPerm::Approve)
        .await
        .unwrap_err();
    assert_eq!(
        err.code,
        ErrorCode::MailboxAccessDenied,
        "§5.4: members may never approve even with a grant"
    );

    // A manager with the same grant passes the role clause.
    let ctx = ctx_for(&world, &world.manager, "r7-manager");
    require_mailbox(&mut tx, &ctx, world.mail_account_id, MailboxPerm::Approve)
        .await
        .expect("manager + grant + implied scope must pass");
    tx.rollback().await.unwrap();
}

#[tokio::test]
async fn missing_credential_scope_denied() {
    let db = test_db().await;
    let world = seed_workspace(&db).await.unwrap();

    // Credential-scoped ctx without mail:send — grant row and role clause
    // hold but the third conjunct fails.
    let scopes = ScopeSet::of(vec![contracts::Scope::MailRead]);
    let ctx = ctx_for_credential(&world, &world.manager, Uuid::new_v4(), scopes, "r7-scope");
    let mut tx = db.begin(world.workspace_id).await.unwrap();
    let err = require_mailbox(&mut tx, &ctx, world.mail_account_id, MailboxPerm::Send)
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::MailboxAccessDenied);
    tx.rollback().await.unwrap();
}
