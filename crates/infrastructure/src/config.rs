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

#[derive(Clone)]
pub struct Config {
    pub app_env: AppEnv,
    pub zoho: crate::providers::zoho_campaigns::ZohoSettings,
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
    /// Max jobs the worker handles concurrently per poll (R13).
    pub worker_concurrency: usize,
    /// TCP peers allowed to set `X-Forwarded-For` (CIDR list). Defaults to
    /// loopback only — the Next.js server is the expected proxy (§4.10).
    pub trusted_proxies: Vec<ipnetwork::IpNetwork>,
}

// Configuration contains database credentials and cryptographic key material.
// Keep debug output metadata-only even when callers log the whole configuration.
impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Config")
            .field("app_env", &self.app_env)
            .field("api_addr", &self.api_addr)
            .field("mcp_addr", &self.mcp_addr)
            .field("cookie_secure", &self.cookie_secure)
            .field("worker_concurrency", &self.worker_concurrency)
            .finish_non_exhaustive()
    }
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
        // Fail closed (R1): APP_ENV must be an explicit known value.
        // Unset → production (secure-by-default); unknown → startup error.
        let env = match std::env::var("APP_ENV").as_deref() {
            Ok("development") | Ok("dev") => AppEnv::Dev,
            Ok("test") => AppEnv::Test,
            Ok("production") | Ok("prod") => AppEnv::Prod,
            Ok(other) => {
                return Err(ConfigError(format!(
                    "APP_ENV must be development|test|production, got {other:?}"
                )));
            }
            Err(_) => AppEnv::Prod,
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
        // Prod requirements (R1): a real HMAC key ≥32 bytes and Redis —
        // both are startup errors when absent, never silent fallbacks.
        let hmac_key = match get("OUTREACH_HMAC_KEY") {
            Some(k) => {
                let key = base64_decode(&k).unwrap_or_else(|| k.as_bytes().to_vec());
                if key.len() < 32 {
                    return Err(ConfigError(
                        "OUTREACH_HMAC_KEY must be at least 32 bytes".into(),
                    ));
                }
                key
            }
            None if env.is_dev_or_test() => b"outreach-dev-hmac-key".to_vec(),
            None => {
                return Err(ConfigError(
                    "OUTREACH_HMAC_KEY is required in production".into(),
                ));
            }
        };
        let redis_url = get("REDIS_URL");
        if env == AppEnv::Prod && redis_url.is_none() {
            return Err(ConfigError(
                "REDIS_URL is required in production (rate limits fail closed)".into(),
            ));
        }
        let trusted_proxies = get("TRUSTED_PROXIES")
            .map(|s| {
                s.split(',')
                    .map(|p| {
                        p.trim().parse::<ipnetwork::IpNetwork>().map_err(|_| {
                            ConfigError(format!("TRUSTED_PROXIES entry {p:?} is not a CIDR"))
                        })
                    })
                    .collect::<Result<Vec<_>, _>>()
            })
            .transpose()?
            .unwrap_or_else(|| {
                vec![
                    ipnetwork::IpNetwork::V4("127.0.0.1/32".parse().unwrap()),
                    ipnetwork::IpNetwork::V6("::1/128".parse().unwrap()),
                ]
            });
        Ok(Self {
            app_env: env,
            zoho: crate::providers::zoho_campaigns::ZohoSettings::from_env(
                &get("PUBLIC_BASE_URL")
                    .or_else(|| get("OUTREACH_PUBLIC_BASE_URL"))
                    .unwrap_or_else(|| {
                        if env.is_dev_or_test() {
                            "http://localhost:3000".into()
                        } else {
                            String::new()
                        }
                    }),
                env.is_dev_or_test(),
            ),
            database_url,
            redis_url,
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
            hmac_key,
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
            worker_concurrency: get("WORKER_CONCURRENCY")
                .map(|s| {
                    s.parse::<usize>().map_err(|_| {
                        ConfigError("WORKER_CONCURRENCY must be a positive integer".into())
                    })
                })
                .transpose()?
                .unwrap_or(4)
                .max(1),
            trusted_proxies,
        })
    }

    /// Deterministic config for tests.
    pub fn test() -> Self {
        Self {
            app_env: AppEnv::Test,
            zoho: crate::providers::zoho_campaigns::ZohoSettings {
                fake_for_tests: true,
                ..Default::default()
            },
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
            worker_concurrency: 4,
            trusted_proxies: vec![
                ipnetwork::IpNetwork::V4("127.0.0.1/32".parse().unwrap()),
                ipnetwork::IpNetwork::V6("::1/128".parse().unwrap()),
            ],
        }
    }
}

fn base64_decode(s: &str) -> Option<Vec<u8>> {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(s.trim())
        .ok()
        .or_else(|| {
            base64::engine::general_purpose::STANDARD
                .decode(s.trim())
                .ok()
        })
}

/// Validate a public application origin before embedding capability URLs.
pub fn validate_public_origin(raw: &str) -> Result<String, contracts::AppError> {
    let url = reqwest::Url::parse(raw.trim()).map_err(|_| {
        contracts::AppError::dependency_unavailable("public application origin is invalid")
    })?;
    let loopback = matches!(
        url.host_str(),
        Some("localhost" | "127.0.0.1" | "[::1]" | "::1")
    );
    if !(url.scheme() == "https" || (url.scheme() == "http" && loopback))
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || !matches!(url.path(), "" | "/")
    {
        return Err(contracts::AppError::dependency_unavailable(
            "public application origin must be HTTPS (or a local development origin) without credentials or a path",
        ));
    }
    Ok(url.as_str().trim_end_matches('/').to_string())
}

/// Workspace override, then PUBLIC_BASE_URL. Production has no implicit origin.
pub fn public_base_url(workspace_value: Option<String>) -> Result<String, contracts::AppError> {
    let configured = workspace_value
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            std::env::var("PUBLIC_BASE_URL")
                .ok()
                .filter(|value| !value.trim().is_empty())
        });
    match configured {
        Some(value) => validate_public_origin(&value),
        None if matches!(
            std::env::var("APP_ENV").as_deref(),
            Ok("development" | "dev" | "test" | "testing")
        ) =>
        {
            Ok("http://localhost:3000".into())
        }
        None => Err(contracts::AppError::dependency_unavailable(
            "PUBLIC_BASE_URL or workspace public_base_url is required",
        )),
    }
}

#[cfg(test)]
mod public_origin_tests {
    #[test]
    fn debug_output_excludes_credentials_and_key_material() {
        let mut config = super::Config::test();
        config.database_url =
            "postgres://private-user:private-password@localhost/private-db".into();
        config.hmac_key = b"private-hmac-key-material".to_vec();
        config.zoho.mail_client_secret = Some(crate::providers::zoho_campaigns::SecretValue::new(
            "private-client-secret",
        ));
        let debug = format!("{config:?}");
        assert!(!debug.contains("private-"));
        assert!(!debug.contains("database_url"));
        assert!(!debug.contains("hmac_key"));
    }
    #[test]
    fn generated_capabilities_use_only_explicit_safe_origins() {
        for valid in [
            "https://app.example.test/",
            "http://localhost:3000",
            "http://127.0.0.1:13000",
            "http://[::1]:3000",
        ] {
            assert!(super::validate_public_origin(valid).is_ok(), "{valid}");
        }
        for invalid in [
            "http://localhost.evil.test",
            "http://127.0.0.1.evil.test",
            "https://user:pass@app.test",
            "https://app.test/path",
            "https://app.test/?token=secret",
            "javascript:alert(1)",
            "file:///etc/passwd",
        ] {
            assert!(super::validate_public_origin(invalid).is_err(), "{invalid}");
        }
    }
}
