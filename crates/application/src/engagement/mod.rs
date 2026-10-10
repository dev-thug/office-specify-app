use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use ammonia::Url;
use contracts::AppError;

const MAX_TRACKED_LINKS_PER_MESSAGE: usize = 100;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CapabilityKind {
    Open,
    Click,
}

struct PreparedCapability {
    token: String,
    kind: CapabilityKind,
    target_url: Option<String>,
}

/// Rewritten immutable content and the capability material the freeze
/// transaction persists after inserting its outbound row. Raw tokens stay in
/// memory only long enough to build the sent URLs.
pub struct PreparedEngagement {
    pub body_text: String,
    pub body_html: Option<String>,
    pub tracking_enabled: bool,
    outbound_message_id: uuid::Uuid,
    recipient_email_address_id: uuid::Uuid,
    capabilities: Vec<PreparedCapability>,
}

struct RewrittenContent {
    body_text: String,
    body_html: Option<String>,
    tracking_enabled: bool,
    capabilities: Vec<PreparedCapability>,
}

#[derive(Default)]
struct RewriteState {
    token_by_target: HashMap<String, String>,
    capabilities: Vec<PreparedCapability>,
}

fn rewrite_content(
    base_url: &str,
    body_text: &str,
    body_html: Option<&str>,
    enabled: bool,
) -> Result<RewrittenContent, AppError> {
    if !enabled {
        return Ok(RewrittenContent {
            body_text: body_text.to_string(),
            body_html: body_html.map(str::to_string),
            tracking_enabled: false,
            capabilities: Vec::new(),
        });
    }

    let base = Url::parse(base_url)
        .map_err(|_| AppError::validation("engagement origin is invalid", vec![]))?;
    if !matches!(base.scheme(), "http" | "https")
        || base.host_str().is_none()
        || !base.username().is_empty()
        || base.password().is_some()
        || base.path() != "/"
        || base.query().is_some()
        || base.fragment().is_some()
    {
        return Err(AppError::validation(
            "engagement origin must be an HTTP(S) origin",
            vec![],
        ));
    }
    let base_url = base.as_str().trim_end_matches('/').to_string();
    let mut state = RewriteState::default();
    let open_token = body_html.map(|_| infrastructure::crypto::generate_token());
    if let Some(token) = &open_token {
        state.capabilities.push(PreparedCapability {
            token: token.clone(),
            kind: CapabilityKind::Open,
            target_url: None,
        });
    }

    let rewritten_text = rewrite_plain_text_links(body_text, &base_url, &mut state);
    let shared_state = Arc::new(Mutex::new(state));
    let rewritten_html = body_html
        .map(|html| {
            let safe_html = domain::templates::sanitize_email_html(html).map_err(|_| {
                AppError::validation("body_html contains disallowed constructs", vec![])
            })?;
            let state_for_filter = Arc::clone(&shared_state);
            let base_for_filter = base_url.clone();
            let mut builder = domain::templates::safe_email_html_builder();
            builder.attribute_filter(move |element, attribute, value| {
                if attribute == "style" && !domain::templates::is_safe_inline_style(value) {
                    return None;
                }
                if element == "a" && attribute == "href" {
                    let mut state = state_for_filter
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    if let Some(target) = eligible_target(value)
                        && let Some(token) = token_for_target(&mut state, target)
                    {
                        return Some(Cow::Owned(click_url(&base_for_filter, &token)));
                    }
                }
                Some(Cow::Borrowed(value))
            });
            let cleaned = builder.clean(&safe_html).to_string();
            Ok::<_, AppError>(match open_token.as_deref() {
                Some(token) => append_open_pixel(&cleaned, &base_url, token),
                None => cleaned,
            })
        })
        .transpose()?;

    let capabilities = std::mem::take(
        &mut shared_state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .capabilities,
    );
    let tracking_enabled = !capabilities.is_empty();
    Ok(RewrittenContent {
        body_text: rewritten_text,
        body_html: rewritten_html,
        tracking_enabled,
        capabilities,
    })
}

fn click_url(base_url: &str, token: &str) -> String {
    format!("{base_url}/public/v1/engagement/click/{token}")
}

fn open_url(base_url: &str, token: &str) -> String {
    format!("{base_url}/public/v1/engagement/open/{token}")
}

fn append_open_pixel(html: &str, base_url: &str, token: &str) -> String {
    format!(
        "{html}<img src=\"{}\" width=\"1\" height=\"1\" alt=\"\" aria-hidden=\"true\">",
        open_url(base_url, token)
    )
}

fn eligible_target(value: &str) -> Option<String> {
    if value.trim() != value {
        return None;
    }
    let url = Url::parse(value).ok()?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return None;
    }

    let path = url.path().to_ascii_lowercase();
    if path_has_untracked_action(&path) {
        return None;
    }
    Some(url.to_string())
}

fn path_has_untracked_action(path: &str) -> bool {
    let segments: Vec<&str> = path
        .split('/')
        .filter(|segment| !segment.is_empty())
        .collect();
    segments.contains(&"unsubscribe")
        || matches!(segments.as_slice(), ["public", "book", ..])
        || matches!(segments.as_slice(), ["public", "bookings", ..])
        || matches!(segments.as_slice(), ["public", "booking", ..])
        || matches!(segments.as_slice(), ["public", "v1", "booking", ..])
        || matches!(segments.as_slice(), ["public", "v1", "bookings", ..])
        || matches!(segments.as_slice(), ["public", "v1", "booking-links", ..])
}

fn token_for_target(state: &mut RewriteState, target: String) -> Option<String> {
    if let Some(token) = state.token_by_target.get(&target) {
        return Some(token.clone());
    }
    if state
        .capabilities
        .iter()
        .filter(|capability| capability.kind == CapabilityKind::Click)
        .count()
        >= MAX_TRACKED_LINKS_PER_MESSAGE
    {
        return None;
    }

    let token = infrastructure::crypto::generate_token();
    state.token_by_target.insert(target.clone(), token.clone());
    state.capabilities.push(PreparedCapability {
        token: token.clone(),
        kind: CapabilityKind::Click,
        target_url: Some(target),
    });
    Some(token)
}

fn rewrite_plain_text_links(body: &str, base_url: &str, state: &mut RewriteState) -> String {
    let mut out = String::with_capacity(body.len());
    let mut cursor = 0;
    while let Some(start) = find_http_link(body, cursor) {
        out.push_str(&body[cursor..start]);
        let end = body[start..]
            .find(|character: char| {
                character.is_whitespace() || matches!(character, '<' | '>' | '"' | '\'')
            })
            .map_or(body.len(), |offset| start + offset);
        let raw = &body[start..end];
        let trim_end = raw.trim_end_matches(['.', ',', ';', ':', '!', '?', ')', ']', '}']);
        let punctuation = &raw[trim_end.len()..];
        if let Some(target) = eligible_target(trim_end) {
            if let Some(token) = token_for_target(state, target) {
                out.push_str(&click_url(base_url, &token));
            } else {
                out.push_str(trim_end);
            }
        } else {
            out.push_str(trim_end);
        }
        out.push_str(punctuation);
        cursor = end;
    }
    out.push_str(&body[cursor..]);
    out
}

fn find_http_link(body: &str, from: usize) -> Option<usize> {
    let remaining = body.get(from..)?;
    let http = remaining.find("http://");
    let https = remaining.find("https://");
    match (http, https) {
        (Some(a), Some(b)) => Some(from + a.min(b)),
        (Some(a), None) | (None, Some(a)) => Some(from + a),
        (None, None) => None,
    }
}

/// Mint rewrites only when the draft opted in and the workspace has not
/// disabled engagement. The caller hashes the returned content before
/// inserting the immutable outbound snapshot.
pub async fn prepare_frozen_outbound_in_tx(
    tx: &mut infrastructure::db::Tx<'_>,
    outbound_message_id: uuid::Uuid,
    recipient_email_address_id: uuid::Uuid,
    body_text: &str,
    body_html: Option<&str>,
    tracking_requested: bool,
) -> Result<PreparedEngagement, AppError> {
    let content =
        if !tracking_requested || !infrastructure::repo::engagement::tracking_enabled(tx).await? {
            rewrite_content("http://localhost", body_text, body_html, false)?
        } else {
            let origin = infrastructure::repo::sending::public_base_url(tx).await?;
            rewrite_content(&origin, body_text, body_html, true)?
        };
    Ok(PreparedEngagement {
        body_text: content.body_text,
        body_html: content.body_html,
        tracking_enabled: content.tracking_enabled,
        outbound_message_id,
        recipient_email_address_id,
        capabilities: content.capabilities,
    })
}

/// Persist capability hashes after the immutable outbound row exists, inside
/// the same transaction as freeze. Raw tokens are hashed in the repository.
pub async fn persist_frozen_outbound_in_tx(
    tx: &mut infrastructure::db::Tx<'_>,
    outbound_message_id: uuid::Uuid,
    recipient_email_address_id: uuid::Uuid,
    prepared: &PreparedEngagement,
) -> Result<(), AppError> {
    if prepared.outbound_message_id != outbound_message_id
        || prepared.recipient_email_address_id != recipient_email_address_id
    {
        return Err(AppError::internal(
            "engagement capabilities do not match the frozen recipient and outbound",
        ));
    }
    if !prepared.tracking_enabled {
        return Ok(());
    }
    for capability in &prepared.capabilities {
        infrastructure::repo::engagement::insert_capability(
            tx,
            outbound_message_id,
            recipient_email_address_id,
            match capability.kind {
                CapabilityKind::Open => "open",
                CapabilityKind::Click => "click",
            },
            &capability.token,
            capability.target_url.as_deref(),
        )
        .await?;
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, serde::Serialize)]
pub struct EngagementCounts {
    pub tracking_enabled: bool,
    pub open_request_count: i64,
    pub click_request_count: i64,
}

#[derive(Debug, Clone, Copy)]
pub struct EngagementCapabilityLocation {
    pub workspace_id: uuid::Uuid,
    pub capability_id: uuid::Uuid,
}

/// Resolve a public capability to a workspace-scoped row. Expired tracking
/// capabilities still resolve so click requests can honor their stored URL;
/// the scoped repository check disables counting after `expires_at`.
pub async fn resolve_capability(
    db: &infrastructure::db::Db,
    token: &str,
) -> Result<Option<EngagementCapabilityLocation>, AppError> {
    Ok(infrastructure::repo::engagement::resolve_token(db, token)
        .await?
        .map(|location| EngagementCapabilityLocation {
            workspace_id: location.workspace_id,
            capability_id: location.capability_id,
        }))
}

/// Mailbox-authorized request counters for one immutable outbound. Request
/// counts reflect proxy/scanner-sensitive HTTP requests, never delivery or
/// proof that a person read the message.
pub async fn get_outbound_counts(
    db: &infrastructure::db::Db,
    ctx: &contracts::Ctx,
    outbound_message_id: uuid::Uuid,
) -> Result<EngagementCounts, AppError> {
    let mut tx = db.begin(ctx.workspace_id).await?;
    let counts = infrastructure::repo::engagement::outbound_counts(
        &mut tx,
        ctx.principal_id,
        outbound_message_id,
    )
    .await?
    .ok_or_else(|| AppError::not_found("outbound message not found"))?;
    tx.commit().await?;
    Ok(EngagementCounts {
        tracking_enabled: counts.tracking_enabled,
        open_request_count: counts.open_request_count,
        click_request_count: counts.click_request_count,
    })
}

/// Record an open request without retaining network metadata. Invalid or
/// revoked capabilities return `false`, allowing public pixels to stay
/// indistinguishable.
pub async fn record_open_request(
    db: &infrastructure::db::Db,
    token: &str,
) -> Result<bool, AppError> {
    let Some(location) = resolve_capability(db, token).await? else {
        return Ok(false);
    };
    record_open_request_at(db, location).await
}

pub async fn record_open_request_at(
    db: &infrastructure::db::Db,
    location: EngagementCapabilityLocation,
) -> Result<bool, AppError> {
    let mut tx = db.begin(location.workspace_id).await?;
    let recorded =
        infrastructure::repo::engagement::record_request(&mut tx, location.capability_id, "open")
            .await?;
    tx.commit().await?;
    Ok(recorded.is_some_and(|capability| capability.tracking_active))
}

/// Record a click request and return only the immutable HTTP(S) target stored
/// when the message froze. A caller-supplied query string is never consulted.
pub async fn record_click_request(
    db: &infrastructure::db::Db,
    token: &str,
) -> Result<Option<String>, AppError> {
    let Some(location) = resolve_capability(db, token).await? else {
        return Ok(None);
    };
    record_click_request_at(db, location).await
}

pub async fn record_click_request_at(
    db: &infrastructure::db::Db,
    location: EngagementCapabilityLocation,
) -> Result<Option<String>, AppError> {
    let mut tx = db.begin(location.workspace_id).await?;
    let row =
        infrastructure::repo::engagement::record_request(&mut tx, location.capability_id, "click")
            .await?;
    tx.commit().await?;
    Ok(row
        .and_then(|row| row.target_url)
        .and_then(|target| eligible_target(&target)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rewrites_http_content_links_but_keeps_booking_unsubscribe_and_non_http_urls_direct() {
        let text = "Read https://example.test/article?id=7. Book https://office.test/public/book/booking-token. Unsubscribe https://office.test/public/unsubscribe/unsubscribe-token. Email mailto:sales@example.test.";
        let html = Some(
            r#"<p><a href="https://example.test/article?id=7">Read</a> <a href="https://office.test/public/book/booking-token">Book</a> <a href="https://office.test/public/unsubscribe/unsubscribe-token">Unsubscribe</a> <a href="mailto:sales@example.test">Email</a> <a href="/contact">Contact</a></p>"#,
        );

        let prepared = rewrite_content("https://office.test", text, html, true).unwrap();

        assert!(prepared.body_text.contains("/engagement/click/"));
        assert!(
            prepared
                .body_text
                .contains("https://office.test/public/book/booking-token")
        );
        assert!(
            prepared
                .body_text
                .contains("https://office.test/public/unsubscribe/unsubscribe-token")
        );
        assert!(prepared.body_text.contains("mailto:sales@example.test"));
        let prepared_html = prepared.body_html.as_deref().unwrap();
        assert!(prepared_html.contains("/engagement/click/"));
        assert!(prepared_html.contains("/engagement/open/"));
        assert!(prepared_html.contains("https://office.test/public/book/booking-token"));
        assert!(prepared_html.contains("https://office.test/public/unsubscribe/unsubscribe-token"));
        assert!(prepared_html.contains("mailto:sales@example.test"));
        assert!(prepared_html.contains("href=\"/contact\""));
        assert_eq!(
            prepared.capabilities.len(),
            2,
            "one click and one open capability"
        );
    }

    #[test]
    fn does_not_rewrite_or_mint_capabilities_when_tracking_is_disabled() {
        let text = "Visit https://example.test/article";
        let html = Some(r#"<a href="https://example.test/article">Visit</a>"#);

        let prepared = rewrite_content("https://office.test", text, html, false).unwrap();

        assert_eq!(prepared.body_text, text);
        assert_eq!(prepared.body_html.as_deref(), html);
        assert!(prepared.capabilities.is_empty());
    }

    #[test]
    fn text_only_tracking_records_clicks_without_claiming_open_tracking() {
        let prepared = rewrite_content(
            "https://office.test",
            "Read https://example.test/article",
            None,
            true,
        )
        .unwrap();

        assert!(prepared.tracking_enabled);
        assert!(prepared.body_text.contains("/engagement/click/"));
        assert!(prepared.body_html.is_none());
        assert_eq!(prepared.capabilities.len(), 1);
        assert_eq!(prepared.capabilities[0].kind, CapabilityKind::Click);
    }

    #[test]
    fn leaves_links_direct_after_the_per_message_capability_limit() {
        let text = (0..=MAX_TRACKED_LINKS_PER_MESSAGE)
            .map(|index| format!("https://example.test/{index}"))
            .collect::<Vec<_>>()
            .join(" ");

        let prepared = rewrite_content("https://office.test", &text, None, true).unwrap();

        assert_eq!(
            prepared
                .capabilities
                .iter()
                .filter(|capability| capability.kind == CapabilityKind::Click)
                .count(),
            MAX_TRACKED_LINKS_PER_MESSAGE
        );
        assert!(prepared.body_text.contains("https://example.test/100"));
    }

    #[test]
    fn booking_and_manage_routes_are_never_rewritten() {
        for url in [
            "https://office.test/public/v1/booking-links/token",
            "https://office.test/public/v1/bookings/manage-token",
            "https://office.test/public/booking/manage-token",
        ] {
            assert!(eligible_target(url).is_none(), "{url} must stay direct");
        }
    }

    #[test]
    fn tracking_preserves_validated_inline_cta_and_table_styles() {
        let html = Some(
            r##"<table style="width:100%;max-width:600px;border-collapse:collapse"><tr><td style="background-color:#f3f4f6;padding:24px"><a href="https://example.test/offer" style="display:inline-block;background-color:#171717;color:#ffffff;font-size:16px;font-weight:600;padding:12px 24px;border-radius:4px;text-decoration:none">Review offer</a></td></tr></table>"##,
        );

        let prepared = rewrite_content("https://office.test", "Review offer", html, true).unwrap();
        let output = prepared.body_html.as_deref().unwrap();

        assert!(output.contains("max-width:600px"), "{output}");
        assert!(output.contains("background-color:#171717"), "{output}");
        assert!(output.contains("color:#ffffff"), "{output}");
        assert!(output.contains("padding:12px 24px"), "{output}");
        assert!(output.contains("border-radius:4px"), "{output}");
        assert!(output.contains("/engagement/click/"), "{output}");
        assert!(output.contains("/engagement/open/"), "{output}");
    }

    #[test]
    fn tracking_rejects_unsafe_inline_css_in_plain_draft_html() {
        let html = Some(r#"<a href="https://example.test" style="position:fixed">Open</a>"#);
        assert!(rewrite_content("https://office.test", "Open", html, true).is_err());
    }
}
