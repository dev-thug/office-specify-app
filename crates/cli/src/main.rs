//! `outreach-cli` — operator commands (spec §4.5):
//!   bootstrap   — create workspace + admin user/principal + system worker principal (A.7)
//!   migrate     — apply SQLx migrations
//!   openapi     — write contracts/openapi/openapi.json
//!   mcp-schema  — write contracts/mcp/tools.json

use std::io::Read;
use std::path::PathBuf;

use anyhow::{Context as _, bail};
use clap::{Parser, Subcommand};
use infrastructure::config::Config;
use infrastructure::db::{Db, ROLE_API};
use infrastructure::repo::auth as auth_repo;

#[derive(Parser)]
#[command(name = "outreach-cli", about = "Outreach OS operator CLI")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Apply database migrations.
    Migrate,
    /// Create the first workspace + admin login + system worker principal (A.7).
    /// Workspace starts with emergency_stop=true (DDL default).
    Bootstrap {
        #[arg(long, default_value = "Outreach Workspace")]
        workspace_name: String,
        #[arg(long, default_value = "admin@outreach.local")]
        admin_email: String,
        /// Read the admin password from stdin; generates one when unset.
        #[arg(long)]
        admin_password_stdin: bool,
        #[arg(long)]
        admin_password: Option<String>,
    },
    /// Export the OpenAPI document to contracts/openapi/openapi.json.
    Openapi {
        #[arg(long, default_value = "contracts/openapi/openapi.json")]
        out: PathBuf,
    },
    /// Export the MCP tool catalog to contracts/mcp/tools.json.
    McpSchema {
        #[arg(long, default_value = "contracts/mcp/tools.json")]
        out: PathBuf,
    },
    /// Insert synthetic demo data — a workspace with admin/manager/member/
    /// agent/system principals, a fake mail account and grants. Dev only.
    SeedDemo,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "warn".into()),
        )
        .init();
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Migrate => {
            let db = connect()?;
            db.migrate().await?;
            println!("migrations applied");
        }
        Cmd::Bootstrap {
            workspace_name,
            admin_email,
            admin_password_stdin,
            admin_password,
        } => {
            bootstrap(
                &workspace_name,
                &admin_email,
                admin_password_stdin,
                admin_password,
            )
            .await?
        }
        Cmd::Openapi { out } => {
            let doc = api::openapi_document();
            write_json(&out, &serde_json::to_value(&doc)?)?;
            println!("wrote {}", out.display());
        }
        Cmd::McpSchema { out } => {
            write_json(&out, &mcp::server::tools_document())?;
            println!("wrote {}", out.display());
        }
        Cmd::SeedDemo => {
            let db = connect()?;
            let world = infrastructure::testing::seed_workspace(&db).await?;
            println!("seeded workspace {}", world.workspace_id);
            for p in [
                &world.admin,
                &world.manager,
                &world.member_a,
                &world.member_b,
            ] {
                println!(
                    "  {:7} {} (password: {})",
                    format!("{:?}", p.role).to_lowercase(),
                    p.email.as_deref().unwrap_or("-"),
                    infrastructure::testing::TEST_PASSWORD
                );
            }
        }
    }
    Ok(())
}

fn connect() -> anyhow::Result<Db> {
    let config = Config::from_env().map_err(|e| anyhow::anyhow!("{e}"))?;
    let url = config.database_url.clone();
    // connect() is async; bridge via a tiny runtime handle — we're already
    // inside tokio::main.
    Ok(tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(Db::connect(&url, Some(ROLE_API)))
    })?)
}

async fn bootstrap(
    workspace_name: &str,
    admin_email: &str,
    password_stdin: bool,
    password: Option<String>,
) -> anyhow::Result<()> {
    let db = connect()?;
    // Idempotent: reuse the first workspace when one exists.
    let workspace_id = match auth_repo::first_workspace_id(&db).await? {
        Some(id) => {
            eprintln!("workspace already exists ({id}) — adding admin to it");
            id
        }
        None => auth_repo::insert_workspace(&db, workspace_name).await?,
    };

    let password = match (password_stdin, password) {
        (true, _) => {
            let mut buf = String::new();
            std::io::stdin().read_to_string(&mut buf)?;
            buf.trim().to_string()
        }
        (false, Some(p)) => p,
        (false, None) => {
            let p = format!("{}!", &infrastructure::crypto::generate_token()[..24]);
            eprintln!("generated admin password (shown once below)");
            p
        }
    };
    if password.len() < 12 || password.len() > 128 {
        bail!("admin password must be 12–128 chars");
    }

    let email = domain::email_address::normalize(
        admin_email,
        domain::email_address::LocalPartPolicy::Lowercase,
    )
    .context("invalid admin email")?
    .normalized_key;
    let user = match auth_repo::find_user_by_email(&db, &email).await? {
        Some(u) => u,
        None => auth_repo::create_user(&db, &email, "Administrator").await?,
    };
    let hash = infrastructure::crypto::hash_password(&password)?;
    auth_repo::set_password_credential(&db, user.id, &hash).await?;

    let mut tx = db.begin(workspace_id).await?;
    if auth_repo::find_principal_for_user(&db, workspace_id, user.id)
        .await?
        .is_none()
    {
        auth_repo::insert_principal(
            &mut tx,
            "human",
            Some(user.id),
            "Administrator",
            "admin",
            "active",
        )
        .await?;
    }
    // A.7: system worker principal for AuthMethod::System job contexts.
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM principals WHERE workspace_id = $1 AND kind = 'system')",
    )
    .bind(workspace_id)
    .fetch_one(tx.conn())
    .await?;
    if !exists {
        auth_repo::insert_principal(&mut tx, "system", None, "System Worker", "system", "active")
            .await?;
    }
    tx.commit().await?;

    println!("workspace_id = {workspace_id}");
    println!("admin_email  = {email}");
    println!("admin_password = {password}");
    println!("log in via POST /api/v1/auth/login");
    Ok(())
}

fn write_json(path: &PathBuf, value: &serde_json::Value) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, serde_json::to_string_pretty(value)? + "\n")?;
    Ok(())
}
