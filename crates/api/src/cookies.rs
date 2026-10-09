//! Session + CSRF cookie helpers (spec §4.4). Session: HttpOnly, Secure in
//! prod, SameSite=Lax, Path=/. CSRF cookie is not HttpOnly (double-submit).

use axum::http::HeaderValue;

use crate::extractors::{CSRF_COOKIE, SESSION_COOKIE};

fn build(name: &str, value: &str, http_only: bool, secure: bool, max_age_secs: i64) -> HeaderValue {
    let mut s = format!("{name}={value}; Path=/; SameSite=Lax; Max-Age={max_age_secs}");
    if http_only {
        s.push_str("; HttpOnly");
    }
    if secure {
        s.push_str("; Secure");
    }
    HeaderValue::from_str(&s).unwrap_or_else(|_| HeaderValue::from_static(""))
}

/// `outreach_session` — HttpOnly.
pub fn session(session_token: &str, secure: bool, max_age_secs: i64) -> HeaderValue {
    build(SESSION_COOKIE, session_token, true, secure, max_age_secs)
}

/// `outreach_csrf` — readable by JS (double-submit).
pub fn csrf(csrf_token: &str, secure: bool, max_age_secs: i64) -> HeaderValue {
    build(CSRF_COOKIE, csrf_token, false, secure, max_age_secs)
}

/// Expire both cookies (logout).
pub fn clear(secure: bool) -> [HeaderValue; 2] {
    [
        HeaderValue::from_str(&format!(
            "{SESSION_COOKIE}=; Path=/; SameSite=Lax; Max-Age=0; HttpOnly{}",
            if secure { "; Secure" } else { "" }
        ))
        .unwrap(),
        HeaderValue::from_str(&format!(
            "{CSRF_COOKIE}=; Path=/; SameSite=Lax; Max-Age=0{}",
            if secure { "; Secure" } else { "" }
        ))
        .unwrap(),
    ]
}
