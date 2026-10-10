//! Read-only IMAP access for SMTP-profile inboxes.
//!
//! The adapter is deliberately limited to the configured public host on
//! implicit-TLS port 993 and the INBOX folder. It uses `EXAMINE` and
//! `BODY.PEEK[]`, never mailbox-mutating commands. UID ranges are capped so
//! each worker job has a bounded amount of work; the caller persists the
//! returned cursor before scheduling the next page.

use std::{
    collections::{HashMap, HashSet},
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    str::FromStr,
    sync::Arc,
    time::Duration,
};

use async_imap::types::Fetch;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use futures::TryStreamExt;
use mailparse::{MailAddr, MailHeader, ParsedMail, addrparse_header};
use tokio::{net::TcpStream, time::timeout};
use tokio_rustls::{TlsConnector, rustls};
use uuid::Uuid;

use super::{ProviderError, ProviderMessage, SecretValue};

pub const IMAP_PORT: u16 = 993;
pub const INBOX_FOLDER: &str = "INBOX";
pub const UID_WINDOW_SIZE: u32 = 100;
pub const MAX_MESSAGE_BYTES: usize = 512 * 1024;
pub const MAX_PAGE_BYTES: usize = 1024 * 1024;
const MAX_MESSAGES_PER_BODY_PAGE: usize = MAX_PAGE_BYTES / MAX_MESSAGE_BYTES;

const OPERATION_TIMEOUT: Duration = Duration::from_secs(40);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(8);
const MAX_DNS_ADDRESSES: usize = 16;

/// The cursor is mailbox-owned and valid only for its matching UIDVALIDITY.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImapCursor {
    pub uid_validity: Option<u32>,
    pub last_uid: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UidWindow {
    pub start_uid: u32,
    pub end_uid: u32,
    pub has_more: bool,
}

/// Parsed messages and the exact next durable checkpoint for one bounded
/// history window. Oversized or malformed messages count as scanned so a
/// single bad message cannot stall all future polling.
#[derive(Debug, Clone)]
pub struct ImapInboxPage {
    pub uid_validity: u32,
    pub last_uid: u32,
    pub has_more: bool,
    pub scanned: usize,
    pub skipped: usize,
    pub truncated: usize,
    pub messages: Vec<ProviderMessage>,
}

/// Test seam and provider boundary. There is intentionally no folder
/// parameter: every implementation can only read the account's INBOX.
#[async_trait]
pub trait ImapInboxSource: Send + Sync {
    async fn fetch_inbox_page(
        &self,
        host: &str,
        username: &str,
        password: &SecretValue,
        mail_account_id: Uuid,
        cursor: ImapCursor,
    ) -> Result<ImapInboxPage, ProviderError>;
}

#[derive(Debug, Default, Clone, Copy)]
pub struct TlsImapInboxSource;

#[async_trait]
impl ImapInboxSource for TlsImapInboxSource {
    async fn fetch_inbox_page(
        &self,
        host: &str,
        username: &str,
        password: &SecretValue,
        mail_account_id: Uuid,
        cursor: ImapCursor,
    ) -> Result<ImapInboxPage, ProviderError> {
        if !valid_imap_hostname(host) {
            return Err(ProviderError::NotConfigured);
        }

        timeout(
            OPERATION_TIMEOUT,
            fetch_inbox_page_inner(host, username, password.expose(), mail_account_id, cursor),
        )
        .await
        .map_err(|_| ProviderError::unavailable("IMAP inbox operation timed out"))?
    }
}

async fn fetch_inbox_page_inner(
    host: &str,
    username: &str,
    password: &str,
    mail_account_id: Uuid,
    cursor: ImapCursor,
) -> Result<ImapInboxPage, ProviderError> {
    let addresses = resolve_public_imap_addresses(host).await?;
    let tls = tls_connector();
    let server_name = rustls::pki_types::ServerName::try_from(host.to_owned())
        .map_err(|_| ProviderError::NotConfigured)?;

    let mut session = None;
    for address in addresses {
        let tcp = match timeout(CONNECT_TIMEOUT, TcpStream::connect(address)).await {
            Ok(Ok(stream)) => stream,
            Ok(Err(_)) | Err(_) => continue,
        };
        let _ = tcp.set_nodelay(true);
        let tls_stream = match timeout(CONNECT_TIMEOUT, tls.connect(server_name.clone(), tcp)).await
        {
            Ok(Ok(stream)) => stream,
            Ok(Err(_)) | Err(_) => continue,
        };
        let client = async_imap::Client::new(tls_stream);
        match client.login(username, password).await {
            Ok(authenticated) => {
                session = Some(authenticated);
                break;
            }
            Err((_error, _client)) => {
                // Do not surface server diagnostics: authentication and IMAP
                // errors may include user-controlled response text.
                return Err(ProviderError::ReauthRequired);
            }
        }
    }
    let mut session = session.ok_or_else(|| {
        ProviderError::unavailable("IMAP TLS connection could not be established")
    })?;

    let mailbox = session
        .examine(INBOX_FOLDER)
        .await
        .map_err(|_| ProviderError::unavailable("IMAP INBOX could not be opened read-only"))?;
    let uid_validity = mailbox
        .uid_validity
        .ok_or_else(|| ProviderError::unavailable("IMAP INBOX omitted UIDVALIDITY"))?;
    let uid_next = mailbox
        .uid_next
        .ok_or_else(|| ProviderError::unavailable("IMAP INBOX omitted UIDNEXT"))?;
    let current_last_uid = if cursor.uid_validity == Some(uid_validity) {
        cursor.last_uid
    } else {
        0
    };
    let Some(window) = uid_window(cursor, uid_validity, uid_next) else {
        let _ = session.logout().await;
        return Ok(ImapInboxPage {
            uid_validity,
            last_uid: current_last_uid,
            has_more: false,
            scanned: 0,
            skipped: 0,
            truncated: 0,
            messages: Vec::new(),
        });
    };

    let matching_uids = session
        .uid_search(format!("UID {}:{}", window.start_uid, window.end_uid))
        .await
        .map_err(|_| ProviderError::unavailable("IMAP INBOX search failed"))?;
    let mut matching_uids: Vec<u32> = matching_uids
        .into_iter()
        .filter(|uid| *uid >= window.start_uid && *uid <= window.end_uid)
        .collect();
    matching_uids.sort_unstable();

    let mut skipped = 0usize;
    let mut truncated = 0usize;
    let mut scanned = 0usize;
    let mut last_uid = window.end_uid;
    let mut messages = Vec::new();
    if matching_uids.is_empty() {
        let _ = session.logout().await;
        return Ok(ImapInboxPage {
            uid_validity,
            last_uid,
            has_more: window.has_more,
            scanned,
            skipped,
            truncated,
            messages,
        });
    }

    let uid_set = matching_uids
        .iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(",");
    let metadata_stream = session
        .uid_fetch(&uid_set, "(UID RFC822.SIZE INTERNALDATE)")
        .await
        .map_err(|_| ProviderError::unavailable("IMAP message metadata could not be read"))?;
    let mut metadata: Vec<Fetch> = metadata_stream
        .try_collect()
        .await
        .map_err(|_| ProviderError::unavailable("IMAP message metadata could not be read"))?;
    metadata.sort_by_key(|fetch| fetch.uid.unwrap_or(u32::MAX));

    let requested: HashSet<u32> = matching_uids.iter().copied().collect();
    let mut message_metadata = HashMap::<u32, (Option<usize>, Option<DateTime<Utc>>)>::new();
    for fetch in metadata {
        let Some(uid) = fetch.uid.filter(|uid| requested.contains(uid)) else {
            continue;
        };
        let size = fetch.size.map(|size| size as usize);
        let internal_date = fetch.internal_date().map(|date| date.with_timezone(&Utc));
        message_metadata.insert(uid, (size, internal_date));
    }

    // RFC822.SIZE is informational only. Import at most two messages in one
    // job because each partial BODY.PEEK response is capped at 512 KiB. This
    // keeps the page below 1 MiB even when a server lies about the size.
    let (selected_uids, scanned_through_uid) = select_uid_page(&matching_uids, window.end_uid);
    scanned = selected_uids.len();
    last_uid = scanned_through_uid;

    // Fetch bodies serially so this adapter never buffers a batch of message
    // literals. The IMAP partial-section request limits each response; we
    // also cap the bytes passed to MIME parsing and mark truncated imports.
    let body_query = body_peek_query();
    for uid in selected_uids {
        let provider_id = imap_provider_message_id(mail_account_id, host, uid_validity, uid);
        let message_at = message_metadata
            .get(&uid)
            .and_then(|(_, date)| *date)
            .unwrap_or_else(Utc::now);
        let reported_size = message_metadata.get(&uid).and_then(|(size, _)| *size);
        let body_stream = session
            .uid_fetch(uid.to_string(), &body_query)
            .await
            .map_err(|_| ProviderError::unavailable("IMAP message body could not be read"))?;
        let mut body_fetches = body_stream;
        let mut matching_body = None;
        while let Some(fetch) = body_fetches
            .try_next()
            .await
            .map_err(|_| ProviderError::unavailable("IMAP message body could not be read"))?
        {
            if fetch.uid == Some(uid) {
                matching_body = Some(fetch);
            }
        }
        let Some(fetch) = matching_body else {
            messages.push(truncated_stub(
                provider_id,
                message_at,
                "Office imported this INBOX item, but the server did not return its message body.",
            ));
            truncated += 1;
            continue;
        };
        let raw = fetch.body().unwrap_or_default();
        let capped_raw = &raw[..raw.len().min(MAX_MESSAGE_BYTES)];
        let body_truncated = body_is_truncated(raw.len(), reported_size);
        let parsed = parse_message(capped_raw, provider_id.clone(), message_at);
        let parse_failed = parsed.is_none();
        let fallback_reason = if raw.is_empty() {
            "Office imported this INBOX item, but the server did not return its message body."
        } else {
            "Office imported this INBOX item, but could not parse its RFC822 headers."
        };
        let message =
            parsed.unwrap_or_else(|| truncated_stub(provider_id, message_at, fallback_reason));
        if body_truncated {
            let marker = truncation_marker(reported_size, capped_raw.len());
            messages.push(with_body_marker(message, &marker));
            truncated += 1;
        } else if parse_failed {
            messages.push(message);
            skipped += 1;
        } else {
            messages.push(message);
        }
    }

    messages.sort_by_key(|message| message.message_at);
    let has_more = last_uid < uid_next.saturating_sub(1);
    let _ = session.logout().await;
    Ok(ImapInboxPage {
        uid_validity,
        last_uid,
        has_more,
        scanned,
        skipped,
        truncated,
        messages,
    })
}

fn tls_connector() -> TlsConnector {
    let roots = rustls::RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let config = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .expect("ring provider supports rustls safe protocol versions")
        .with_root_certificates(roots)
        .with_no_client_auth();
    TlsConnector::from(Arc::new(config))
}

fn select_uid_page(matching_uids: &[u32], window_end: u32) -> (Vec<u32>, u32) {
    let selected: Vec<u32> = matching_uids
        .iter()
        .take(MAX_MESSAGES_PER_BODY_PAGE)
        .copied()
        .collect();
    let scanned_through = if matching_uids.len() > selected.len() {
        selected.last().copied().unwrap_or(window_end)
    } else {
        window_end
    };
    (selected, scanned_through)
}

fn body_peek_query() -> String {
    format!("(UID BODY.PEEK[]<0.{MAX_MESSAGE_BYTES}>)")
}

/// Validate syntax separately from DNS so API handlers can report malformed
/// host input as a validation error and retryable resolver issues as a
/// dependency failure.
pub fn valid_imap_hostname(host: &str) -> bool {
    if host.is_empty()
        || host.len() > 253
        || host.trim() != host
        || !host.is_ascii()
        || !host.contains('.')
        || host.parse::<IpAddr>().is_ok()
        || host.ends_with('.')
    {
        return false;
    }
    let lower = host.to_ascii_lowercase();
    if [
        "localhost",
        ".localhost",
        ".local",
        ".internal",
        ".test",
        ".example",
        ".invalid",
        ".onion",
    ]
    .iter()
    .any(|suffix| lower == *suffix || lower.ends_with(suffix))
    {
        return false;
    }
    let Some(tld) = lower.rsplit('.').next() else {
        return false;
    };
    if !tld.bytes().any(|byte| byte.is_ascii_alphabetic()) {
        return false;
    }
    lower.split('.').all(|label| {
        !label.is_empty()
            && label.len() <= 63
            && !label.starts_with('-')
            && !label.ends_with('-')
            && label
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    })
}

/// Resolve and pin only a public IP address. Every DNS answer must be public,
/// avoiding mixed public/private records that permit a caller to steer the
/// connection to a private address. TLS still validates the original host.
pub async fn resolve_public_imap_addresses(host: &str) -> Result<Vec<SocketAddr>, ProviderError> {
    if !valid_imap_hostname(host) {
        return Err(ProviderError::NotConfigured);
    }
    let addresses = timeout(
        Duration::from_secs(4),
        tokio::net::lookup_host((host, IMAP_PORT)),
    )
    .await
    .map_err(|_| ProviderError::unavailable("IMAP host lookup timed out"))?
    .map_err(|_| ProviderError::unavailable("IMAP host lookup failed"))?;
    let mut resolved = Vec::new();
    for address in addresses.take(MAX_DNS_ADDRESSES + 1) {
        if !is_public_ip(address.ip()) {
            return Err(ProviderError::NotConfigured);
        }
        if !resolved.contains(&address) {
            resolved.push(address);
        }
    }
    if resolved.is_empty() || resolved.len() > MAX_DNS_ADDRESSES {
        return Err(ProviderError::NotConfigured);
    }
    Ok(resolved)
}

fn is_public_ip(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(ip) => is_public_ipv4(ip),
        IpAddr::V6(ip) => is_public_ipv6(ip),
    }
}

fn is_public_ipv4(ip: Ipv4Addr) -> bool {
    if ip.is_private()
        || ip.is_loopback()
        || ip.is_link_local()
        || ip.is_broadcast()
        || ip.is_unspecified()
        || ip.is_multicast()
    {
        return false;
    }
    const NON_PUBLIC: [&str; 14] = [
        "0.0.0.0/8",
        "100.64.0.0/10",
        "192.0.0.0/24",
        "192.0.2.0/24",
        "192.88.99.0/24",
        "198.18.0.0/15",
        "198.51.100.0/24",
        "203.0.113.0/24",
        "224.0.0.0/4",
        "240.0.0.0/4",
        "255.255.255.255/32",
        "192.31.196.0/24",
        "192.52.193.0/24",
        "192.175.48.0/24",
    ];
    !NON_PUBLIC.iter().any(|range| {
        ipnetwork::Ipv4Network::from_str(range).is_ok_and(|network| network.contains(ip))
    })
}

fn is_public_ipv6(ip: Ipv6Addr) -> bool {
    if let Some(mapped) = ip.to_ipv4_mapped() {
        return is_public_ipv4(mapped);
    }
    if ip.is_loopback()
        || ip.is_unspecified()
        || ip.is_multicast()
        || ip.is_unique_local()
        || ip.is_unicast_link_local()
    {
        return false;
    }
    // IANA defines currently assignable IPv6 global-unicast space as
    // 2000::/3. Site-local fec0::/10, ULA, link-local, mapped, and the
    // reserved prefixes are outside that allocation. Keep explicit special
    // ranges that sit inside the global-unicast block below.
    let globally_unicast =
        ipnetwork::Ipv6Network::from_str("2000::/3").is_ok_and(|network| network.contains(ip));
    if !globally_unicast {
        return false;
    }
    const NON_PUBLIC: [&str; 4] = ["2001::/23", "2001:db8::/32", "2002::/16", "3fff::/20"];
    !NON_PUBLIC.iter().any(|range| {
        ipnetwork::Ipv6Network::from_str(range).is_ok_and(|network| network.contains(ip))
    })
}

pub fn uid_window(cursor: ImapCursor, uid_validity: u32, uid_next: u32) -> Option<UidWindow> {
    if uid_next <= 1 {
        return None;
    }
    let last_uid = if cursor.uid_validity == Some(uid_validity) {
        cursor.last_uid
    } else {
        0
    };
    if last_uid >= uid_next - 1 {
        return None;
    }
    let start_uid = last_uid.saturating_add(1).max(1);
    let end_uid = start_uid
        .saturating_add(UID_WINDOW_SIZE - 1)
        .min(uid_next - 1);
    Some(UidWindow {
        start_uid,
        end_uid,
        has_more: end_uid < uid_next - 1,
    })
}

pub fn imap_provider_message_id(
    mail_account_id: Uuid,
    host: &str,
    uid_validity: u32,
    uid: u32,
) -> String {
    use sha2::{Digest, Sha256};

    let host_fingerprint = hex::encode(Sha256::digest(host.to_ascii_lowercase().as_bytes()));
    format!("imap:{mail_account_id}:{host_fingerprint}:{uid_validity}:{uid}")
}

fn body_is_truncated(actual_bytes: usize, reported_size: Option<usize>) -> bool {
    actual_bytes == 0
        || actual_bytes >= MAX_MESSAGE_BYTES
        || reported_size.is_some_and(|reported| reported > actual_bytes)
}

pub fn parse_message(
    raw: &[u8],
    provider_message_id: String,
    message_at: DateTime<Utc>,
) -> Option<ProviderMessage> {
    if raw.is_empty() || raw.len() > MAX_MESSAGE_BYTES {
        return None;
    }
    let parsed = mailparse::parse_mail(raw).ok()?;
    let from = first_address(&parsed, "From")?;
    let to = addresses(&parsed, "To")
        .into_iter()
        .chain(addresses(&parsed, "Cc"))
        .take(100)
        .collect::<Vec<_>>();
    let internet_message_id =
        first_header(&parsed, "Message-ID").and_then(|value| single_message_id(&value));
    let in_reply_to = first_header(&parsed, "In-Reply-To")
        .and_then(|value| message_id_tokens(&value).last().cloned());
    let references = first_header(&parsed, "References")
        .map(|value| message_id_tokens(&value))
        .unwrap_or_default();
    let (body_text, body_html) = body_parts(&parsed);
    let subject = first_header(&parsed, "Subject").map(|value| value.trim().to_string());
    Some(ProviderMessage {
        provider_message_id,
        provider_thread_id: None,
        internet_message_id,
        in_reply_to,
        references,
        from,
        to,
        subject: subject.filter(|value| !value.is_empty()),
        body_text,
        body_html,
        message_at,
    })
}

fn truncated_stub(
    provider_message_id: String,
    message_at: DateTime<Utc>,
    body_text: &str,
) -> ProviderMessage {
    ProviderMessage {
        provider_message_id,
        provider_thread_id: None,
        internet_message_id: None,
        in_reply_to: None,
        references: Vec::new(),
        from: "unknown@invalid".into(),
        to: Vec::new(),
        subject: Some("Unparsed inbox message".into()),
        body_text: Some(body_text.into()),
        body_html: None,
        message_at,
    }
}

fn truncation_marker(reported_size: Option<usize>, captured_bytes: usize) -> String {
    let original_size = reported_size
        .map(|size| format!("{size} bytes"))
        .unwrap_or_else(|| "unknown".into());
    format!(
        "[Office imported this inbox item with its first {captured_bytes} message bytes. The message content was truncated; the IMAP server reported an original size of {original_size}.]"
    )
}

fn with_body_marker(mut message: ProviderMessage, marker: &str) -> ProviderMessage {
    match message.body_text.as_mut() {
        Some(body) if !body.is_empty() => {
            body.push_str("\n\n");
            body.push_str(marker);
        }
        _ => message.body_text = Some(marker.into()),
    }
    if let Some(html) = message.body_html.as_mut() {
        html.push_str("\n<p>");
        html.push_str(marker);
        html.push_str("</p>");
    }
    message
}

fn first_address(parsed: &ParsedMail<'_>, name: &str) -> Option<String> {
    addresses(parsed, name).into_iter().next()
}

fn addresses(parsed: &ParsedMail<'_>, name: &str) -> Vec<String> {
    let mut values = Vec::new();
    for header in parsed
        .headers
        .iter()
        .filter(|header| header.get_key_ref().eq_ignore_ascii_case(name))
    {
        if let Ok(list) = addrparse_header(header) {
            for address in list.iter() {
                match address {
                    MailAddr::Single(info) => values.push(info.addr.clone()),
                    MailAddr::Group(group) => {
                        values.extend(group.addrs.iter().map(|info| info.addr.clone()));
                    }
                }
                if values.len() >= 100 {
                    return values;
                }
            }
        }
    }
    values
}

fn first_header(parsed: &ParsedMail<'_>, name: &str) -> Option<String> {
    parsed
        .headers
        .iter()
        .find(|header| header.get_key_ref().eq_ignore_ascii_case(name))
        .map(MailHeader::get_value)
}

fn single_message_id(value: &str) -> Option<String> {
    message_id_tokens(value).into_iter().next()
}

fn message_id_tokens(value: &str) -> Vec<String> {
    let mut ids = Vec::new();
    for token in value.split_ascii_whitespace().take(64) {
        let candidate = token.trim_matches([',', ';']);
        let id = if candidate.starts_with('<') && candidate.ends_with('>') {
            candidate.to_string()
        } else if !candidate.contains(['<', '>'])
            && candidate.contains('@')
            && !candidate.chars().any(char::is_control)
        {
            format!("<{candidate}>")
        } else {
            continue;
        };
        if id.len() > 998 || ids.contains(&id) {
            continue;
        }
        ids.push(id);
        if ids.len() == 32 {
            break;
        }
    }
    ids
}

fn body_parts(parsed: &ParsedMail<'_>) -> (Option<String>, Option<String>) {
    let mut plain = None;
    let mut html = None;
    let mut visited = 0usize;
    collect_body_parts(parsed, 0, &mut visited, &mut plain, &mut html);
    (plain, html)
}

fn collect_body_parts(
    parsed: &ParsedMail<'_>,
    depth: usize,
    visited: &mut usize,
    plain: &mut Option<String>,
    html: &mut Option<String>,
) {
    if depth > 12 || *visited >= 128 {
        return;
    }
    *visited += 1;
    let is_attachment = parsed.headers.iter().any(|header| {
        header
            .get_key_ref()
            .eq_ignore_ascii_case("Content-Disposition")
            && header
                .get_value()
                .to_ascii_lowercase()
                .trim_start()
                .starts_with("attachment")
    });
    if is_attachment {
        return;
    }
    match parsed.ctype.mimetype.to_ascii_lowercase().as_str() {
        "text/plain" if plain.is_none() => {
            *plain = parsed.get_body().ok();
        }
        "text/html" if html.is_none() => {
            *html = parsed.get_body().ok();
        }
        _ => {
            for part in &parsed.subparts {
                if plain.is_some() && html.is_some() {
                    break;
                }
                collect_body_parts(part, depth + 1, visited, plain, html);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ImapCursor, ImapInboxPage, ImapInboxSource, MAX_MESSAGE_BYTES, MAX_MESSAGES_PER_BODY_PAGE,
        SecretValue, body_is_truncated, body_peek_query, imap_provider_message_id, is_public_ip,
        parse_message, select_uid_page, truncated_stub, truncation_marker, uid_window,
        valid_imap_hostname, with_body_marker,
    };
    use chrono::{DateTime, Utc};
    use std::net::IpAddr;
    use std::sync::Mutex;
    use uuid::Uuid;

    #[test]
    fn parses_unrelated_inbox_sender_and_keeps_reply_headers() {
        let raw = concat!(
            "From: Finance Team <billing@vendor.example>\r\n",
            "To: help@example.test\r\n",
            "Subject: invoice question\r\n",
            "Message-ID: <incoming-1@vendor.example>\r\n",
            "In-Reply-To: <sent-1@office.example>\r\n",
            "References: <sent-0@office.example> <sent-1@office.example>\r\n",
            "Date: Sat, 10 Oct 2099 09:30:00 +0000\r\n",
            "Content-Type: text/plain; charset=utf-8\r\n",
            "\r\n",
            "Please resend the invoice.\r\n"
        )
        .as_bytes();

        let internal_date = DateTime::parse_from_rfc3339("2026-10-10T09:30:00+00:00")
            .unwrap()
            .with_timezone(&Utc);
        let message =
            parse_message(raw, "imap:mailbox:8:31".into(), internal_date).expect("valid message");

        assert_eq!(message.from, "billing@vendor.example");
        assert_eq!(message.to, ["help@example.test"]);
        assert_eq!(
            message.internet_message_id.as_deref(),
            Some("<incoming-1@vendor.example>")
        );
        assert_eq!(
            message.in_reply_to.as_deref(),
            Some("<sent-1@office.example>")
        );
        assert_eq!(
            message.references,
            ["<sent-0@office.example>", "<sent-1@office.example>"]
        );
        assert_eq!(
            message.body_text.as_deref(),
            Some("Please resend the invoice.\r\n")
        );
        assert_eq!(message.message_at, internal_date);
    }

    #[test]
    fn uidvalidity_change_restarts_the_bounded_history_backfill() {
        let cursor = ImapCursor {
            uid_validity: Some(12),
            last_uid: 5_000,
        };

        let window = uid_window(cursor, 13, 10_001).expect("non-empty mailbox");

        assert_eq!((window.start_uid, window.end_uid), (1, 100));
        assert!(window.has_more);
    }

    #[test]
    fn provider_ids_are_scoped_to_the_mailbox_and_imap_uid_identity() {
        let mailbox_a = Uuid::from_u128(1);
        let mailbox_b = Uuid::from_u128(2);

        let first = imap_provider_message_id(mailbox_a, "imap.example.com", 99, 7);

        assert_eq!(
            first,
            imap_provider_message_id(mailbox_a, "imap.example.com", 99, 7)
        );
        assert_ne!(
            first,
            imap_provider_message_id(mailbox_a, "imap.example.com", 99, 8)
        );
        assert_ne!(
            first,
            imap_provider_message_id(mailbox_a, "imap.example.com", 100, 7)
        );
        assert_ne!(
            first,
            imap_provider_message_id(mailbox_a, "other.example.net", 99, 7)
        );
        assert_ne!(
            first,
            imap_provider_message_id(mailbox_b, "imap.example.com", 99, 7)
        );
    }

    #[test]
    fn host_validation_rejects_local_names_and_ip_literals() {
        assert!(!valid_imap_hostname("localhost"));
        assert!(!valid_imap_hostname("127.0.0.1"));
        assert!(!valid_imap_hostname("mail.internal"));
        assert!(valid_imap_hostname("imappro.zoho.com"));
    }

    #[test]
    fn public_address_guard_rejects_private_and_shared_ranges() {
        assert!(is_public_ip("1.1.1.1".parse::<IpAddr>().unwrap()));
        assert!(!is_public_ip("10.0.0.4".parse::<IpAddr>().unwrap()));
        assert!(!is_public_ip("100.64.0.1".parse::<IpAddr>().unwrap()));
        assert!(!is_public_ip("192.0.2.10".parse::<IpAddr>().unwrap()));
        assert!(!is_public_ip("::1".parse::<IpAddr>().unwrap()));
        assert!(!is_public_ip("fd00::1".parse::<IpAddr>().unwrap()));
        assert!(!is_public_ip("fec0::1".parse::<IpAddr>().unwrap()));
        assert!(is_public_ip(
            "2606:4700:4700::1111".parse::<IpAddr>().unwrap()
        ));
    }

    #[test]
    fn body_fetch_uses_peek_and_caps_each_literal() {
        let query = body_peek_query();

        assert!(query.contains("BODY.PEEK[]"));
        assert!(!query.contains("BODY[]"));
        assert!(query.contains("<0.524288>"));
    }

    #[test]
    fn uid_page_limit_is_independent_of_untrusted_rfc822_size() {
        let uids: Vec<u32> = (1..=100).collect();
        let reported_sizes = vec![1usize; uids.len()];
        let (selected, scanned_through) = select_uid_page(&uids, 100);

        assert_eq!(reported_sizes.len(), 100);
        assert_eq!(selected.len(), MAX_MESSAGES_PER_BODY_PAGE);
        assert_eq!(selected, [1, 2]);
        assert_eq!(scanned_through, 2);
    }

    #[test]
    fn exact_partial_fetch_limit_is_marked_truncated_even_when_size_matches() {
        assert!(body_is_truncated(
            MAX_MESSAGE_BYTES,
            Some(MAX_MESSAGE_BYTES)
        ));
    }

    #[test]
    fn provider_size_smaller_than_complete_literal_is_not_truncated() {
        assert!(!body_is_truncated(5_615, Some(1_955)));
        assert!(!body_is_truncated(5_211, Some(41)));
        assert!(body_is_truncated(5_615, Some(5_616)));
    }

    #[test]
    fn truncated_stub_keeps_a_visible_content_marker_and_mailbox_message_id() {
        let message = truncated_stub(
            "imap:mailbox:host:99:10".into(),
            Utc::now(),
            "Office could not parse the oversized headers.",
        );
        let marker = truncation_marker(Some(900_000), MAX_MESSAGE_BYTES);
        let message = with_body_marker(message, &marker);

        assert_eq!(message.from, "unknown@invalid");
        assert!(message.provider_message_id.starts_with("imap:mailbox:"));
        assert!(message.body_text.unwrap().contains("content was truncated"));
    }

    struct FakeInboxSource {
        requested_mailbox: Mutex<Option<Uuid>>,
    }

    #[async_trait::async_trait]
    impl ImapInboxSource for FakeInboxSource {
        async fn fetch_inbox_page(
            &self,
            host: &str,
            _username: &str,
            _password: &SecretValue,
            mail_account_id: Uuid,
            _cursor: ImapCursor,
        ) -> Result<ImapInboxPage, super::ProviderError> {
            assert_eq!(host, "imap.example.net");
            *self.requested_mailbox.lock().unwrap() = Some(mail_account_id);
            let message_at = Utc::now();
            let raw =
                b"From: bills@vendor.example\r\nTo: office@example.net\r\n\r\nInvoice attached.";
            let message = parse_message(
                raw,
                imap_provider_message_id(mail_account_id, host, 7, 4),
                message_at,
            )
            .unwrap();
            Ok(ImapInboxPage {
                uid_validity: 7,
                last_uid: 4,
                has_more: false,
                scanned: 1,
                skipped: 0,
                truncated: 0,
                messages: vec![message],
            })
        }
    }

    #[tokio::test]
    async fn fake_imap_source_supplies_whole_inbox_pages_without_network() {
        let mailbox_id = Uuid::from_u128(91);
        let source = FakeInboxSource {
            requested_mailbox: Mutex::new(None),
        };

        let page = source
            .fetch_inbox_page(
                "imap.example.net",
                "office@example.net",
                &SecretValue::new("test-only"),
                mailbox_id,
                ImapCursor {
                    uid_validity: None,
                    last_uid: 0,
                },
            )
            .await
            .unwrap();

        assert_eq!(*source.requested_mailbox.lock().unwrap(), Some(mailbox_id));
        assert_eq!(page.messages[0].from, "bills@vendor.example");
        assert!(
            page.messages[0]
                .provider_message_id
                .starts_with(&format!("imap:{mailbox_id}:"))
        );
    }
}
