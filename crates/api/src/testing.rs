//! In-process test client (spec §7). `TestApp` serves the real app on an
//! ephemeral port with a reqwest client that keeps cookies — exercising the
//! actual cookie/CSRF path.
//!
//! ```ignore
//! let world = seed_workspace(&db).await?;
//! let app = TestApp::spawn(state).await;
//! let res = app.login("admin@test.dev", TEST_PASSWORD).await;
//! let res = app.get("/api/v1/me").await;
//! ```

use std::net::SocketAddr;
use std::sync::Arc;

use infrastructure::config::Config;
use infrastructure::db::Db;
use reqwest::StatusCode;

use crate::state::AppState;

/// A running in-process API + cookie-aware client.
pub struct TestApp {
    pub addr: SocketAddr,
    /// Cookie-store enabled client — session/csrf cookies persist across
    /// calls like a browser.
    pub client: reqwest::Client,
    pub state: AppState,
    handle: tokio::task::JoinHandle<()>,
}

impl TestApp {
    /// Spawn against a seeded DB. `config` should be `Config::test()`
    /// (dev_endpoints on, no Secure cookies) with database_url filled.
    pub async fn spawn(db: Db, mut config: Config) -> Self {
        config.dev_endpoints = true;
        let state = AppState::with_fake_providers(db, Arc::new(config), None);
        Self::spawn_with_state(state).await
    }

    pub async fn spawn_with_state(state: AppState) -> Self {
        let app = crate::build_app(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = tokio::spawn(async move {
            axum::serve(
                listener,
                app.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await
            .unwrap();
        });
        let client = reqwest::Client::builder()
            .cookie_store(true)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap();
        Self {
            addr,
            client,
            state,
            handle,
        }
    }

    pub fn url(&self, path: &str) -> String {
        format!("http://{}{}", self.addr, path)
    }

    /// Login helper — posts credentials, cookies land in the jar.
    /// Returns the response for status/cookie assertions.
    pub async fn login(&self, email: &str, password: &str) -> reqwest::Response {
        self.client
            .post(self.url("/api/v1/auth/login"))
            .json(&serde_json::json!({"email": email, "password": password}))
            .send()
            .await
            .unwrap()
    }

    /// CSRF token captured from the `Set-Cookie` headers of a response
    /// (e.g. the login response) — reqwest's jar doesn't expose reads.
    pub fn csrf_token_from(res: &reqwest::Response) -> Option<String> {
        res.headers()
            .get_all("set-cookie")
            .iter()
            .filter_map(|v| v.to_str().ok())
            .filter_map(|s| s.split(';').next()?.split_once('='))
            .find(|(k, _)| *k == "outreach_csrf")
            .map(|(_, v)| v.to_string())
    }

    /// POST JSON with an explicit CSRF header token.
    pub async fn post_with_csrf(
        &self,
        path: &str,
        body: serde_json::Value,
        csrf: &str,
    ) -> reqwest::Response {
        self.client
            .post(self.url(path))
            .header("x-csrf-token", csrf)
            .json(&body)
            .send()
            .await
            .unwrap()
    }

    pub async fn get(&self, path: &str) -> reqwest::Response {
        self.client.get(self.url(path)).send().await.unwrap()
    }

    /// POST JSON, attaching `X-CSRF-Token` when `csrf` is Some.
    pub async fn post(
        &self,
        path: &str,
        body: serde_json::Value,
        csrf: Option<&str>,
    ) -> reqwest::Response {
        let mut req = self.client.post(self.url(path)).json(&body);
        if let Some(t) = csrf {
            req = req.header("x-csrf-token", t);
        }
        req.send().await.unwrap()
    }

    /// POST JSON without the CSRF header (for CSRF-failure tests).
    pub async fn post_no_csrf(&self, path: &str, body: serde_json::Value) -> reqwest::Response {
        self.client
            .post(self.url(path))
            .json(&body)
            .send()
            .await
            .unwrap()
    }

    pub async fn logout(&self, csrf: Option<&str>) -> reqwest::Response {
        self.post("/api/v1/auth/logout", serde_json::json!({}), csrf)
            .await
    }

    /// Read the JSON `data` field of a 2xx envelope response.
    pub async fn data(res: reqwest::Response) -> serde_json::Value {
        assert!(res.status().is_success(), "status {}", res.status());
        res.json::<serde_json::Value>().await.unwrap()["data"].take()
    }

    /// Assert a problem+json error and return the parsed problem.
    pub async fn problem(res: reqwest::Response, status: StatusCode) -> serde_json::Value {
        assert_eq!(res.status(), status);
        assert_eq!(
            res.headers()
                .get("content-type")
                .and_then(|v| v.to_str().ok()),
            Some("application/problem+json")
        );
        let p = res.json::<serde_json::Value>().await.unwrap();
        assert!(p["code"].is_string(), "problem missing code: {p}");
        assert!(
            p["request_id"].is_string(),
            "problem missing request_id: {p}"
        );
        p
    }
}

impl Drop for TestApp {
    fn drop(&mut self) {
        self.handle.abort();
    }
}
