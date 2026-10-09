//! Pool + transaction wrapper implementing spec §4.2: every workspace-scoped
//! query runs inside a transaction that sets `app.workspace_id` (transaction-
//! local) and, when configured, `SET LOCAL ROLE <runtime_role>` so RLS is
//! enforced even when the pool connects as a superuser in dev/tests.

use contracts::AppError;
use sqlx::postgres::{PgConnection, PgPool, PgPoolOptions};
use uuid::Uuid;

use crate::error::map_sqlx;

/// DB roles usable through `SET LOCAL ROLE` (migration 0003).
pub const ROLE_API: &str = "api_runtime";
pub const ROLE_WORKER: &str = "worker_runtime";

#[derive(Clone)]
pub struct Db {
    pool: PgPool,
    runtime_role: Option<&'static str>,
    /// Server-fixed workspace for pre-auth transactions (A.5). Sessions,
    /// service credentials and public capability tokens are looked up under
    /// this workspace's RLS context — never client-supplied.
    default_workspace_id: Option<Uuid>,
}

impl Db {
    pub fn new(
        pool: PgPool,
        runtime_role: Option<&'static str>,
        default_workspace_id: Option<Uuid>,
    ) -> Self {
        debug_assert!(matches!(runtime_role, None | Some(ROLE_API) | Some(ROLE_WORKER)));
        Self {
            pool,
            runtime_role,
            default_workspace_id,
        }
    }

    pub async fn connect(url: &str, runtime_role: Option<&'static str>) -> Result<Self, AppError> {
        let pool = PgPoolOptions::new()
            .max_connections(10)
            .connect(url)
            .await
            .map_err(map_sqlx)?;
        Ok(Self::new(pool, runtime_role, None))
    }

    pub fn with_default_workspace(mut self, workspace_id: Option<Uuid>) -> Self {
        self.default_workspace_id = workspace_id;
        self
    }

    pub fn default_workspace_id(&self) -> Option<Uuid> {
        self.default_workspace_id
    }

    /// Pre-auth transaction under the server-fixed default workspace (A.5):
    /// session/service-credential/public-token lookups and invitation
    /// previews. Errors when `default_workspace_id` is unset.
    pub async fn begin_preauth(&self) -> Result<Tx<'_>, AppError> {
        let ws = self.default_workspace_id.ok_or_else(|| {
            AppError::internal("DEFAULT_WORKSPACE_ID is not configured")
        })?;
        self.begin(ws).await
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// Workspace-scoped transaction (A.5). All workspace-table access goes
    /// through this; RLS makes rows from other workspaces invisible.
    pub async fn begin(&self, workspace_id: Uuid) -> Result<Tx<'_>, AppError> {
        let mut inner = self.pool.begin().await.map_err(map_sqlx)?;
        sqlx::query("SELECT set_config('app.workspace_id', $1::text, true)")
            .bind(workspace_id.to_string())
            .execute(&mut *inner)
            .await
            .map_err(map_sqlx)?;
        if let Some(role) = self.runtime_role {
            // Role name is a compile-time whitelisted constant, never user input.
            sqlx::query(&format!("SET LOCAL ROLE {role}"))
                .execute(&mut *inner)
                .await
                .map_err(map_sqlx)?;
        }
        Ok(Tx {
            inner,
            workspace_id: Some(workspace_id),
        })
    }

    /// Pre-auth transaction for global tables (users, credentials, session
    /// rows). The runtime role is still applied but `app.workspace_id` stays
    /// unset, so workspace-scoped tables read as empty (RLS NULL comparison).
    pub async fn begin_global(&self) -> Result<Tx<'_>, AppError> {
        let mut inner = self.pool.begin().await.map_err(map_sqlx)?;
        if let Some(role) = self.runtime_role {
            sqlx::query(&format!("SET LOCAL ROLE {role}"))
                .execute(&mut *inner)
                .await
                .map_err(map_sqlx)?;
        }
        Ok(Tx {
            inner,
            workspace_id: None,
        })
    }

    /// Run the embedded sqlx migrations.
    pub async fn migrate(&self) -> Result<(), AppError> {
        sqlx::migrate!("../../migrations")
            .run(&self.pool)
            .await
            .map_err(|e| AppError::internal(format!("migration failed: {e}")))
    }
}

pub struct Tx<'db> {
    inner: sqlx::Transaction<'db, sqlx::Postgres>,
    workspace_id: Option<Uuid>,
}

impl<'db> Tx<'db> {
    /// Executor for sqlx queries: `query(...).execute(tx.conn())`.
    pub fn conn(&mut self) -> &mut PgConnection {
        &mut self.inner
    }

    pub fn workspace_id(&self) -> Option<Uuid> {
        self.workspace_id
    }

    pub async fn commit(self) -> Result<(), AppError> {
        self.inner.commit().await.map_err(map_sqlx)
    }

    pub async fn rollback(self) -> Result<(), AppError> {
        self.inner.rollback().await.map_err(map_sqlx)
    }
}
