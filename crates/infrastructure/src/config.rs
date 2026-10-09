//! Environment configuration. Secrets come from env; nothing sensitive is
//! logged (spec §15.3).

use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppEnv {
    Dev,
    Test,
    Prod,
}

impl AppEnv {
    pub fn is_dev_or_test(self) -> bool {
        matches!(self, AppEnv::Dev | AppEnv::Test)
    }
}

#[derive(Debug, Clone)]
pub struct Config {
    pub app_env: AppEnv,
    pub database_url: String,
    pub redis_url: Option<String>,
    pub api_addr: SocketAddr,
    pub mcp_addr: SocketAddr,
    /// Server-fixed workspace for pre-auth transactions (A.5).
    pub default_workspace_id: Option<Uuid>,
    /// `Secure` on session cookies; false for dev/test over http.
    pub cookie_secure: bool,
    /// HMAC key for cursors, CSRF tokens, signed object URLs (base64 or raw).
    pub hmac_key: Vec<u8>,
    /// AES-256 key for the file SecretStore (64 hex chars).
    pub secret_store_key: Option<[u8; 32]>,
    pub objects_dir: PathBuf,
    pub secrets_file: PathBuf,
    pub session_ttl: Duration,
    /// Origin allowlist for the MCP endpoint (§13.3). Empty = deny all.
    pub mcp_allowed_origins: Vec<String>,
    /// JSON script file driving Fake provider outcomes (spec §4.9).
    pub fake_provider_script: Option<PathBuf>,
    /// Dev/test-only fake inbound-mail endpoint (spec §4.9).
    pub dev_endpoints: bool,
}

#[derive(Debug)]
pub struct ConfigError(pub String);

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ConfigError {}

impl Config {
    pub fn from_env() -> Result<Self, ConfigError> {
        let env = match std::env::var("APP_ENV").as_deref() {
            Ok("prod") | Ok("production") => AppEnv::Prod,
            Ok("test") => AppEnv::Test,
            _ => AppEnv::Dev,
        };
        let get = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
        let database_url = get("DATABASE_URL")
            .unwrap_or_else(|| "postgres://hyunjoong@localhost:5432/outreach_dev".into());
        let secret_store_key = match get("OUTREACH_SECRET_KEY") {
            Some(hex) => {
                let bytes = hex::decode(hex.trim())
                    .map_err(|_| ConfigError("OUTREACH_SECRET_KEY must be 64 hex chars".into()))?;
                let arr: [u8; 32] = bytes
                    .try_into()
                    .map_err(|_| ConfigError("OUTREACH_SECRET_KEY must be 32 bytes".into()))?;
                Some(arr)
            }
            None if env.is_dev_or_test() => Some([7u8; 32]),
            None => None,
        };
        Ok(Self {
            app_env: env,
            database_url,
            redis_url: get("REDIS_URL"),
            api_addr: get("API_ADDR")
                .unwrap_or_else(|| "127.0.0.1:8080".into())
                .parse()
                .map_err(|_| ConfigError("API_ADDR is not a socket address".into()))?,
            mcp_addr: get("MCP_ADDR")
                .unwrap_or_else(|| "127.0.0.1:8081".into())
                .parse()
                .map_err(|_| ConfigError("MCP_ADDR is not a socket address".into()))?,
            default_workspace_id: get("DEFAULT_WORKSPACE_ID")
                .map(|s| {
                    s.parse::<Uuid>()
                        .map_err(|_| ConfigError("DEFAULT_WORKSPACE_ID is not a uuid".into()))
                })
                .transpose()?,
            cookie_secure: env == AppEnv::Prod,
            hmac_key: get("OUTREACH_HMAC_KEY")
                .map(|k| {
                    base64_decode(&k).unwrap_or_else(|| k.as_bytes().to_vec())
                })
                .unwrap_or_else(|| b"outreach-dev-hmac-key".to_vec()),
            secret_store_key,
            objects_dir: get("OBJECTS_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("var/objects")),
            secrets_file: get("SECRETS_FILE")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("var/secrets.json")),
            session_ttl: Duration::from_secs(
                get("SESSION_TTL_SECONDS")
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(12 * 3600),
            ),
            mcp_allowed_origins: get("MCP_ALLOWED_ORIGINS")
                .map(|s| s.split(',').map(|o| o.trim().to_string()).collect())
                .unwrap_or_default(),
            fake_provider_script: get("FAKE_PROVIDER_SCRIPT").map(PathBuf::from),
            dev_endpoints: env.is_dev_or_test(),
        })
    }

    /// Deterministic config for tests.
    pub fn test() -> Self {
        Self {
            app_env: AppEnv::Test,
            database_url: String::new(),
            redis_url: None,
            api_addr: "127.0.0.1:0".parse().unwrap(),
            mcp_addr: "127.0.0.1:0".parse().unwrap(),
            default_workspace_id: None,
            cookie_secure: false,
            hmac_key: b"test-hmac-key".to_vec(),
            secret_store_key: Some([3u8; 32]),
            objects_dir: PathBuf::from("/tmp/outreach-test-objects"),
            secrets_file: PathBuf::from("/tmp/outreach-test-secrets.json"),
            session_ttl: Duration::from_secs(3600),
            mcp_allowed_origins: vec!["https://agent.example".into()],
            fake_provider_script: None,
            dev_endpoints: true,
        }
    }
}

fn base64_decode(s: &str) -> Option<Vec<u8>> {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(s.trim())
        .ok()
        .or_else(|| base64::engine::general_purpose::STANDARD.decode(s.trim()).ok())
}
