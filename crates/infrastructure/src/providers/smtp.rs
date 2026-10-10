use super::{
    DeliveryCapabilities, DeliveryProvider, ProviderError, SecretValue, SendOutcome, SendRequest,
    SentLookup,
};
use async_trait::async_trait;
use lettre::{
    AsyncSmtpTransport, AsyncTransport, Tokio1Executor,
    message::{
        Mailbox, Message, MultiPart, SinglePart,
        header::{HeaderName, HeaderValue},
    },
    transport::smtp::authentication::Credentials,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    net::IpAddr,
    str::FromStr,
    time::Duration,
};
use uuid::Uuid;

const MAX_HEADER_COUNT: usize = 50;
const MAX_HEADER_VALUE_BYTES: usize = 998;
const MAX_TOTAL_HEADER_BYTES: usize = 16 * 1024;
const SMTP_TIMEOUT: Duration = Duration::from_secs(20);

/// How the SMTP connection is upgraded to an authenticated, encrypted session.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SmtpSecurity {
    /// Implicit TLS from the first byte of the connection (commonly port 465).
    Tls,
    /// Require STARTTLS before sending credentials or message content.
    Starttls,
}

/// Connection credentials and the one sender identity owned by this transport.
///
/// This intentionally has no `Debug` implementation so configuration dumps
/// cannot accidentally expose the SMTP password.
#[derive(Clone)]
pub struct SmtpConfig {
    pub host: String,
    pub port: u16,
    pub security: SmtpSecurity,
    pub username: String,
    pub password: SecretValue,
    pub from_address: String,
    pub from_name: String,
    pub reply_to: Option<String>,
    pub custom_headers: BTreeMap<String, String>,
}

/// SMTP transport boundary for Office-owned campaign delivery.
pub struct SmtpDeliveryProvider {
    config: SmtpConfig,
    transport: AsyncSmtpTransport<Tokio1Executor>,
}

#[cfg(test)]
use std::net::SocketAddr;

impl SmtpDeliveryProvider {
    /// Build a TLS-only SMTP transport bound to the configured sender alias.
    pub fn new(config: SmtpConfig) -> Result<Self, ProviderError> {
        validate_config(&config).map_err(|_| ProviderError::NotConfigured)?;
        let credentials = credentials(&config);
        let mut builder = match config.security {
            SmtpSecurity::Tls => AsyncSmtpTransport::<Tokio1Executor>::relay(&config.host),
            SmtpSecurity::Starttls => {
                AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(&config.host)
            }
        }
        .map_err(|_| ProviderError::NotConfigured)?;
        builder = builder
            .port(config.port)
            .credentials(credentials)
            .timeout(Some(SMTP_TIMEOUT));

        Ok(Self {
            config,
            transport: builder.build(),
        })
    }

    /// Check that the relay accepts an authenticated SMTP session.
    ///
    /// The error deliberately omits lettre's diagnostic text because it can
    /// contain server-provided material or authentication details.
    pub async fn verify_connection(&self) -> Result<(), ProviderError> {
        match self.transport.test_connection().await {
            Ok(true) => Ok(()),
            Ok(false) => Err(ProviderError::unavailable(
                "SMTP connection verification failed",
            )),
            Err(error)
                if error
                    .status()
                    .is_some_and(|status| matches!(u16::from(status), 530 | 534 | 535)) =>
            {
                Err(ProviderError::ReauthRequired)
            }
            Err(error) if error.is_transient() => Err(ProviderError::unavailable(
                "SMTP connection temporarily unavailable",
            )),
            Err(_) => Err(ProviderError::unavailable(
                "SMTP connection verification failed",
            )),
        }
    }

    fn build_message(&self, req: &SendRequest) -> Result<(Message, String, String), &'static str> {
        if req.from != self.config.from_address {
            return Err("sender_mismatch");
        }
        if req.subject.len() > MAX_HEADER_VALUE_BYTES || contains_header_control(&req.subject) {
            return Err("invalid_subject");
        }

        let correlation_id = Uuid::from_str(&req.correlation_id)
            .map_err(|_| "invalid_correlation_id")?
            .to_string();
        let sender_address = parse_address(&self.config.from_address).ok_or("invalid_sender")?;
        let recipient_address = parse_address(&req.to).ok_or("invalid_recipient")?;
        let sender_domain = self
            .config
            .from_address
            .rsplit_once('@')
            .map(|(_, domain)| domain.trim_matches(['[', ']']))
            .filter(|domain| valid_message_id_domain(domain))
            .unwrap_or("localhost");
        let message_id = match req.internet_message_id.as_deref() {
            Some(value) => crate::providers::mail_headers::parse_single_message_id(value)
                .filter(|id| id.len() <= MAX_HEADER_VALUE_BYTES)
                .ok_or("invalid_message_id")?,
            None => format!("<{correlation_id}@{sender_domain}>"),
        };
        let in_reply_to = match req.in_reply_to.as_deref() {
            Some(value) => Some(
                crate::providers::mail_headers::parse_single_message_id(value)
                    .filter(|id| id.len() <= MAX_HEADER_VALUE_BYTES)
                    .ok_or("invalid_in_reply_to")?,
            ),
            None => None,
        };
        let mut references = Vec::new();
        for value in &req.references {
            let reference = crate::providers::mail_headers::parse_single_message_id(value)
                .ok_or("invalid_references")?;
            if !references.contains(&reference) {
                references.push(reference);
            }
        }
        let references = references.join(" ");
        // Include field-name overhead so the generated header line remains
        // within the RFC 5322 hard line limit.
        if references.len() > MAX_HEADER_VALUE_BYTES.saturating_sub(14) {
            return Err("invalid_references");
        }

        let headers =
            effective_headers(&self.config.custom_headers, &req.headers, &correlation_id)?;
        let mut builder = Message::builder()
            .from(Mailbox::new(
                (!self.config.from_name.is_empty()).then(|| self.config.from_name.clone()),
                sender_address,
            ))
            .to(Mailbox::new(None, recipient_address))
            .subject(req.subject.clone())
            .message_id(Some(message_id.clone()));
        if let Some(reply_to) = self.config.reply_to.as_deref() {
            let reply_to = parse_address(reply_to).ok_or("invalid_reply_to")?;
            builder = builder.reply_to(Mailbox::new(None, reply_to));
        }
        if let Some(in_reply_to) = in_reply_to {
            builder = builder.in_reply_to(in_reply_to);
        }
        if !references.is_empty() {
            let header_name =
                HeaderName::new_from_ascii("References".into()).map_err(|_| "invalid_header")?;
            builder = builder.raw_header(HeaderValue::new(header_name, references));
        }

        for (name, value) in headers {
            let header_name = HeaderName::new_from_ascii(name).map_err(|_| "invalid_header")?;
            builder = builder.raw_header(HeaderValue::new(header_name, value));
        }
        let correlation_header = HeaderName::new_from_ascii("X-Outreach-Correlation".into())
            .map_err(|_| "invalid_header")?;
        builder = builder.raw_header(HeaderValue::new(correlation_header, correlation_id.clone()));

        let message = match (&req.body_text, &req.body_html) {
            (Some(text), Some(html)) => builder.multipart(MultiPart::alternative_plain_html(
                text.clone(),
                html.clone(),
            )),
            (Some(text), None) => builder.singlepart(SinglePart::plain(text.clone())),
            (None, Some(html)) => builder.singlepart(SinglePart::html(html.clone())),
            (None, None) => builder.body(String::new()),
        }
        .map_err(|_| "invalid_message")?;

        Ok((message, message_id, correlation_id))
    }

    #[cfg(test)]
    fn for_test(config: SmtpConfig, address: SocketAddr) -> Result<Self, ProviderError> {
        if !address.ip().is_loopback() {
            return Err(ProviderError::NotConfigured);
        }
        validate_config(&config).map_err(|_| ProviderError::NotConfigured)?;
        let transport =
            AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(address.ip().to_string())
                .port(address.port())
                .credentials(credentials(&config))
                .timeout(Some(Duration::from_secs(2)))
                .build();
        Ok(Self { config, transport })
    }
}

#[async_trait]
impl DeliveryProvider for SmtpDeliveryProvider {
    fn capabilities(&self) -> DeliveryCapabilities {
        DeliveryCapabilities {
            supports_send: true,
            one_click_unsubscribe: true,
            custom_headers: true,
            idempotency: false,
        }
    }

    async fn send(&self, req: SendRequest) -> Result<SendOutcome, ProviderError> {
        let (message, message_id, _correlation_id) = match self.build_message(&req) {
            Ok(message) => message,
            Err(code) => {
                return Ok(SendOutcome::Rejected {
                    permanent: true,
                    code: code.into(),
                });
            }
        };

        match self.transport.send(message).await {
            Ok(_) => Ok(SendOutcome::Accepted {
                provider_message_id: message_id,
            }),
            Err(error) => match error.status() {
                Some(status) => Ok(SendOutcome::Rejected {
                    permanent: error.is_permanent(),
                    code: format!("smtp_{status}"),
                }),
                None => Ok(SendOutcome::Unknown {
                    reason: "smtp_transport_ambiguous".into(),
                }),
            },
        }
    }

    async fn lookup_sent(
        &self,
        _correlation_id: &str,
    ) -> Result<Option<SentLookup>, ProviderError> {
        Ok(None)
    }
}

fn credentials(config: &SmtpConfig) -> Credentials {
    Credentials::new(config.username.clone(), config.password.expose().to_owned())
}

fn validate_config(config: &SmtpConfig) -> Result<(), ()> {
    if config.port == 0
        || !valid_host(&config.host)
        || config.username.trim().is_empty()
        || config.username.len() > 255
        || config.username.chars().any(char::is_control)
        || config.password.expose().is_empty()
        || config.password.expose().len() > 4096
        || !valid_display_name(&config.from_name)
        || parse_address(&config.from_address).is_none()
    {
        return Err(());
    }
    if let Some(reply_to) = config.reply_to.as_deref()
        && parse_address(reply_to).is_none()
    {
        return Err(());
    }
    if config.custom_headers.len() > MAX_HEADER_COUNT {
        return Err(());
    }

    let mut seen = BTreeSet::new();
    let mut total_bytes = 0usize;
    for (name, value) in &config.custom_headers {
        validate_header(name, value)?;
        total_bytes = total_bytes.saturating_add(name.len() + value.len());
        let lower_name = name.to_ascii_lowercase();
        if !seen.insert(lower_name.clone()) {
            return Err(());
        }
        if is_generated_header(&lower_name)
            || is_structural_header(&lower_name)
            || !is_allowed_custom_header(&lower_name)
        {
            return Err(());
        }
    }
    if total_bytes > MAX_TOTAL_HEADER_BYTES {
        return Err(());
    }
    Ok(())
}

fn valid_host(host: &str) -> bool {
    if host.is_empty() || host.len() > 253 || host.trim() != host {
        return false;
    }
    if IpAddr::from_str(host).is_ok() {
        return true;
    }
    if host.chars().any(|ch| ch.is_control() || ch.is_whitespace()) {
        return false;
    }
    host.split('.').all(|label| {
        !label.is_empty()
            && label.len() <= 63
            && !label.starts_with('-')
            && !label.ends_with('-')
            && label
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    })
}

fn valid_display_name(value: &str) -> bool {
    value.len() <= 256 && !contains_header_control(value)
}

fn parse_address(value: &str) -> Option<lettre::Address> {
    lettre::Address::from_str(value).ok()
}

fn valid_message_id_domain(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 253
        && !value.contains(':')
        && value.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        })
}

fn contains_header_control(value: &str) -> bool {
    value
        .chars()
        .any(|ch| ch == '\r' || ch == '\n' || (ch.is_control() && ch != '\t'))
}

fn validate_header(name: &str, value: &str) -> Result<(), ()> {
    if name.is_empty()
        || name.len() > 78
        || !name.is_ascii()
        || !name.bytes().all(is_header_name_byte)
        || value.len() > MAX_HEADER_VALUE_BYTES
        || contains_header_control(value)
    {
        return Err(());
    }
    Ok(())
}

fn is_header_name_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric()
        || matches!(
            byte,
            b'!' | b'#'
                | b'$'
                | b'%'
                | b'&'
                | b'\''
                | b'*'
                | b'+'
                | b'-'
                | b'.'
                | b'^'
                | b'_'
                | b'`'
                | b'|'
                | b'~'
        )
}

fn is_generated_header(lower_name: &str) -> bool {
    matches!(
        lower_name,
        "list-unsubscribe" | "list-unsubscribe-post" | "x-outreach-correlation"
    )
}

fn is_allowed_custom_header(lower_name: &str) -> bool {
    matches!(lower_name, "x-auto-response-suppress" | "x-importance")
}

fn is_structural_header(lower_name: &str) -> bool {
    matches!(
        lower_name,
        "from"
            | "to"
            | "cc"
            | "bcc"
            | "sender"
            | "reply-to"
            | "subject"
            | "date"
            | "message-id"
            | "in-reply-to"
            | "references"
            | "return-path"
            | "received"
            | "delivered-to"
            | "dkim-signature"
            | "authentication-results"
            | "resent-from"
            | "resent-to"
            | "resent-sender"
            | "resent-date"
            | "resent-message-id"
    ) || lower_name.starts_with("arc-")
        || lower_name.starts_with("content-")
        || lower_name == "mime-version"
}

fn effective_headers(
    profile_headers: &BTreeMap<String, String>,
    request_headers: &[(String, String)],
    correlation_id: &str,
) -> Result<BTreeMap<String, String>, &'static str> {
    if profile_headers.len() + request_headers.len() + 1 > MAX_HEADER_COUNT {
        return Err("too_many_headers");
    }

    let mut total_bytes = 0usize;
    let mut headers = BTreeMap::new();
    for (name, value) in profile_headers {
        validate_header(name, value).map_err(|_| "invalid_header")?;
        total_bytes = total_bytes.saturating_add(name.len() + value.len());
        let lower_name = name.to_ascii_lowercase();
        if is_generated_header(&lower_name) {
            return Err("protected_header");
        }
        if is_structural_header(&lower_name) || !is_allowed_custom_header(&lower_name) {
            return Err("unsupported_header");
        }
        headers.insert(lower_name, (name.clone(), value.clone()));
    }

    let mut request_seen = BTreeSet::new();
    for (name, value) in request_headers {
        validate_header(name, value).map_err(|_| "invalid_header")?;
        total_bytes = total_bytes.saturating_add(name.len() + value.len());
        let lower_name = name.to_ascii_lowercase();
        if !request_seen.insert(lower_name.clone()) {
            return Err("duplicate_header");
        }
        if lower_name == "x-outreach-correlation" {
            if value != correlation_id {
                return Err("correlation_header_mismatch");
            }
            continue;
        }
        if is_structural_header(&lower_name) {
            return Err("protected_header");
        }
        if !is_generated_header(&lower_name) && !is_allowed_custom_header(&lower_name) {
            return Err("unsupported_header");
        }
        headers.insert(lower_name, (name.clone(), value.clone()));
    }

    if total_bytes > MAX_TOTAL_HEADER_BYTES {
        return Err("headers_too_large");
    }
    Ok(headers.into_values().collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{collections::BTreeMap, sync::Arc, time::Duration};
    use tokio::{
        io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
        net::{TcpListener, TcpStream},
        sync::Mutex,
        task::JoinHandle,
        time::timeout,
    };

    const CORRELATION_ID: &str = "c0ffee00-0000-4000-8000-000000000001";
    const SMTP_PASSWORD: &str = "smtp-password-must-never-appear";

    #[derive(Clone, Copy)]
    enum ServerBehavior {
        Accept,
        RejectAuth { code: u16 },
        RejectTemporaryAuth,
        RejectRecipient,
        DropFinalReply,
    }

    #[derive(Default)]
    struct Captured {
        mail_from: Option<String>,
        rcpt_to: Option<String>,
        message: String,
        auth_attempted: bool,
    }

    struct TestServer {
        addr: std::net::SocketAddr,
        captured: Arc<Mutex<Captured>>,
        task: JoinHandle<()>,
    }

    async fn start_server(behavior: ServerBehavior) -> TestServer {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        let captured = Arc::new(Mutex::new(Captured::default()));
        let server_capture = Arc::clone(&captured);
        let task = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            serve(stream, behavior, server_capture).await;
        });
        TestServer {
            addr,
            captured,
            task,
        }
    }

    async fn serve(stream: TcpStream, behavior: ServerBehavior, captured: Arc<Mutex<Captured>>) {
        let (read, mut write) = stream.into_split();
        let mut read = BufReader::new(read);
        write.write_all(b"220 test relay ready\r\n").await.unwrap();

        loop {
            let mut line = String::new();
            if read.read_line(&mut line).await.unwrap() == 0 {
                return;
            }
            let command = line.trim_end_matches(['\r', '\n']);
            let upper = command.to_ascii_uppercase();
            if upper.starts_with("EHLO ") || upper.starts_with("HELO ") {
                write
                    .write_all(b"250-test relay\r\n250-AUTH PLAIN LOGIN\r\n250 SIZE 5242880\r\n")
                    .await
                    .unwrap();
            } else if upper.starts_with("AUTH ") {
                captured.lock().await.auth_attempted = true;
                if let ServerBehavior::RejectAuth { code } = behavior {
                    write
                        .write_all(format!("{code} 5.7.8 authentication failed\r\n").as_bytes())
                        .await
                        .unwrap();
                    return;
                }
                if matches!(behavior, ServerBehavior::RejectTemporaryAuth) {
                    write
                        .write_all(b"454 4.7.0 temporary authentication failure\r\n")
                        .await
                        .unwrap();
                    return;
                }
                if upper == "AUTH PLAIN" {
                    write.write_all(b"334 \r\n").await.unwrap();
                    let mut auth = String::new();
                    read.read_line(&mut auth).await.unwrap();
                } else if upper == "AUTH LOGIN" {
                    write.write_all(b"334 VXNlcm5hbWU6\r\n").await.unwrap();
                    let mut username = String::new();
                    read.read_line(&mut username).await.unwrap();
                    write.write_all(b"334 UGFzc3dvcmQ6\r\n").await.unwrap();
                    let mut password = String::new();
                    read.read_line(&mut password).await.unwrap();
                }
                write
                    .write_all(b"235 2.7.0 authenticated\r\n")
                    .await
                    .unwrap();
            } else if upper.starts_with("MAIL FROM:") {
                captured.lock().await.mail_from = Some(command.to_owned());
                write
                    .write_all(b"250 2.1.0 sender accepted\r\n")
                    .await
                    .unwrap();
            } else if upper.starts_with("RCPT TO:") {
                captured.lock().await.rcpt_to = Some(command.to_owned());
                if matches!(behavior, ServerBehavior::RejectRecipient) {
                    write
                        .write_all(b"550 5.1.1 recipient rejected\r\n")
                        .await
                        .unwrap();
                    return;
                }
                write
                    .write_all(b"250 2.1.5 recipient accepted\r\n")
                    .await
                    .unwrap();
            } else if upper == "DATA" {
                write.write_all(b"354 send message\r\n").await.unwrap();
                let mut message = String::new();
                loop {
                    let mut data_line = String::new();
                    if read.read_line(&mut data_line).await.unwrap() == 0 {
                        return;
                    }
                    if data_line == ".\r\n" || data_line == ".\n" {
                        break;
                    }
                    if let Some(unstuffed) = data_line.strip_prefix("..") {
                        message.push('.');
                        message.push_str(unstuffed);
                    } else {
                        message.push_str(&data_line);
                    }
                }
                captured.lock().await.message = message;
                if matches!(behavior, ServerBehavior::DropFinalReply) {
                    return;
                }
                write.write_all(b"250 2.0.0 accepted\r\n").await.unwrap();
            } else if upper == "NOOP" {
                write.write_all(b"250 2.0.0 ok\r\n").await.unwrap();
            } else if upper == "QUIT" {
                write.write_all(b"221 2.0.0 bye\r\n").await.unwrap();
                return;
            } else {
                write.write_all(b"250 2.0.0 ok\r\n").await.unwrap();
            }
        }
    }

    fn config(port: u16) -> SmtpConfig {
        let mut custom_headers = BTreeMap::new();
        custom_headers.insert("X-Auto-Response-Suppress".into(), "profile-value".into());
        custom_headers.insert("X-Importance".into(), "1".into());
        SmtpConfig {
            host: "relay.office.test".into(),
            port,
            security: SmtpSecurity::Starttls,
            username: "smtp-user".into(),
            password: super::super::SecretValue::new(SMTP_PASSWORD),
            from_address: "hello@news.specify.app".into(),
            from_name: "ニュース Office".into(),
            reply_to: Some("support@specify.app".into()),
            custom_headers,
        }
    }

    fn request() -> SendRequest {
        SendRequest {
            correlation_id: CORRELATION_ID.into(),
            from: "hello@news.specify.app".into(),
            to: "reader@example.net".into(),
            subject: "안녕하세요 — Office update".into(),
            body_text: Some("A plain-text update".into()),
            body_html: Some("<p><strong>HTML</strong> update</p>".into()),
            internet_message_id: None,
            in_reply_to: None,
            references: vec![],
            headers: vec![
                (
                    "List-Unsubscribe".into(),
                    "<https://office.test/unsubscribe/token>".into(),
                ),
                (
                    "List-Unsubscribe-Post".into(),
                    "List-Unsubscribe=One-Click".into(),
                ),
                ("X-Outreach-Correlation".into(), CORRELATION_ID.into()),
                ("X-Auto-Response-Suppress".into(), "All".into()),
                ("X-Importance".into(), "high".into()),
            ],
        }
    }

    #[tokio::test]
    async fn sends_bound_alias_multipart_headers_and_stable_message_id() {
        let server = start_server(ServerBehavior::Accept).await;
        let provider = SmtpDeliveryProvider::for_test(config(server.addr.port()), server.addr)
            .expect("valid loopback test transport");

        let outcome = provider.send(request()).await.unwrap();
        assert!(matches!(
            outcome,
            SendOutcome::Accepted { provider_message_id }
                if provider_message_id == format!("<{CORRELATION_ID}@news.specify.app>")
        ));
        let captured = server.captured.lock().await;
        assert_eq!(
            captured.mail_from.as_deref(),
            Some("MAIL FROM:<hello@news.specify.app>")
        );
        assert_eq!(
            captured.rcpt_to.as_deref(),
            Some("RCPT TO:<reader@example.net>")
        );
        assert!(captured.message.contains("From:"));
        assert!(captured.message.contains("hello@news.specify.app"));
        assert!(captured.message.to_ascii_lowercase().contains("=?utf-8?"));
        assert!(captured.message.contains("Reply-To:"));
        assert!(captured.message.contains("support@specify.app"));
        assert!(
            captured
                .message
                .contains(&format!("Message-ID: <{CORRELATION_ID}@news.specify.app>"))
        );
        assert!(
            captured
                .message
                .contains("List-Unsubscribe: <https://office.test/unsubscribe/token>")
        );
        assert!(
            captured
                .message
                .contains("List-Unsubscribe-Post: List-Unsubscribe=One-Click")
        );
        assert!(
            captured
                .message
                .contains(&format!("X-Outreach-Correlation: {CORRELATION_ID}"))
        );
        assert!(captured.message.contains("X-Importance: high"));
        assert!(captured.message.contains("X-Auto-Response-Suppress: All"));
        assert!(!captured.message.contains("profile-value"));
        assert!(captured.message.contains("multipart/alternative"));
        assert!(captured.message.contains("text/plain"));
        assert!(captured.message.contains("text/html"));
        assert!(captured.message.contains("A plain-text update"));
        assert!(captured.message.contains("<strong>HTML</strong> update"));
        assert!(provider.capabilities().supports_send);
        assert!(provider.capabilities().custom_headers);
        assert!(provider.capabilities().one_click_unsubscribe);
        assert!(!provider.capabilities().idempotency);
        assert!(
            provider
                .lookup_sent(CORRELATION_ID)
                .await
                .unwrap()
                .is_none()
        );
        drop(captured);
        server.task.await.unwrap();
    }

    #[tokio::test]
    async fn sends_frozen_message_id_and_reply_chain_headers() {
        let server = start_server(ServerBehavior::Accept).await;
        let provider = SmtpDeliveryProvider::for_test(config(server.addr.port()), server.addr)
            .expect("valid loopback test transport");
        let mut req = request();
        req.internet_message_id = Some("<outbound@news.specify.app>".into());
        req.in_reply_to = Some("<inbound@customer.example>".into());
        req.references = vec![
            "<root@customer.example>".into(),
            "<inbound@customer.example>".into(),
        ];

        let outcome = provider.send(req).await.unwrap();

        assert!(
            matches!(outcome, SendOutcome::Accepted { ref provider_message_id }
            if provider_message_id == "<outbound@news.specify.app>")
        );
        let captured = server.captured.lock().await;
        assert!(
            captured
                .message
                .contains("Message-ID: <outbound@news.specify.app>")
        );
        assert!(
            captured
                .message
                .contains("In-Reply-To: <inbound@customer.example>")
        );
        assert!(
            captured
                .message
                .contains("References: <root@customer.example> <inbound@customer.example>")
        );
    }

    #[tokio::test]
    async fn rejects_injected_message_reference_headers_before_network_io() {
        let server = start_server(ServerBehavior::Accept).await;
        let provider = SmtpDeliveryProvider::for_test(config(server.addr.port()), server.addr)
            .expect("valid loopback test transport");
        let mut req = request();
        req.references = vec!["<reply@customer.example>\r\nBcc: victim@example.test".into()];

        let outcome = provider.send(req).await.unwrap();

        assert!(
            matches!(outcome, SendOutcome::Rejected { permanent: true, code }
            if code == "invalid_references")
        );
        assert!(server.captured.lock().await.message.is_empty());
    }

    #[tokio::test]
    async fn rejects_header_injection_and_forged_transport_headers_before_connecting() {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        let provider = SmtpDeliveryProvider::for_test(config(addr.port()), addr).unwrap();

        let mut injected = request();
        injected.headers.push((
            "X-Campaign".into(),
            "safe\r\nBcc: victim@example.net".into(),
        ));
        assert!(matches!(
            provider.send(injected).await.unwrap(),
            SendOutcome::Rejected {
                permanent: true,
                ..
            }
        ));

        let mut forged = request();
        forged
            .headers
            .push(("Authentication-Results".into(), "mx.example; pass".into()));
        assert!(matches!(
            provider.send(forged).await.unwrap(),
            SendOutcome::Rejected {
                permanent: true,
                ..
            }
        ));

        let mut forged_correlation = request();
        if let Some((_, value)) = forged_correlation
            .headers
            .iter_mut()
            .find(|(name, _)| name.eq_ignore_ascii_case("X-Outreach-Correlation"))
        {
            *value = "not-the-request-correlation".into();
        }
        assert!(matches!(
            provider.send(forged_correlation).await.unwrap(),
            SendOutcome::Rejected {
                permanent: true,
                ..
            }
        ));

        assert!(
            timeout(Duration::from_millis(100), listener.accept())
                .await
                .is_err()
        );
    }

    #[test]
    fn rejects_profile_headers_that_are_owned_by_the_adapter_or_campaign() {
        for (name, value) in [
            ("List-Unsubscribe", "<https://profile.invalid/unsubscribe>"),
            ("List-Unsubscribe-Post", "List-Unsubscribe=One-Click"),
            ("X-Outreach-Correlation", "forged-profile-value"),
        ] {
            let mut profile_headers = BTreeMap::new();
            profile_headers.insert(name.to_owned(), value.to_owned());
            assert!(
                validate_config(&SmtpConfig {
                    custom_headers: profile_headers.clone(),
                    ..config(587)
                })
                .is_err()
            );
            assert!(effective_headers(&profile_headers, &[], CORRELATION_ID).is_err());
        }
    }

    #[tokio::test]
    async fn authentication_rejections_are_redacted_and_require_reauthentication() {
        for code in [530, 534, 535] {
            let server = start_server(ServerBehavior::RejectAuth { code }).await;
            let provider = SmtpDeliveryProvider::for_test(config(server.addr.port()), server.addr)
                .expect("valid loopback test transport");

            let result = provider.verify_connection().await;
            assert!(matches!(result, Err(ProviderError::ReauthRequired)));
            let diagnostic = format!("{result:?}");
            assert!(!diagnostic.contains(SMTP_PASSWORD));
            assert!(!diagnostic.contains("authentication failed"));
            assert!(server.captured.lock().await.auth_attempted);
            server.task.await.unwrap();
        }
    }

    #[tokio::test]
    async fn transient_smtp_verification_failures_remain_unavailable_and_redacted() {
        let server = start_server(ServerBehavior::RejectTemporaryAuth).await;
        let provider = SmtpDeliveryProvider::for_test(config(server.addr.port()), server.addr)
            .expect("valid loopback test transport");

        let result = provider.verify_connection().await;
        assert!(matches!(
            result,
            Err(ProviderError::Unavailable(ref reason))
                if reason == "SMTP connection temporarily unavailable"
        ));
        let diagnostic = format!("{result:?}");
        assert!(!diagnostic.contains(SMTP_PASSWORD));
        assert!(!diagnostic.contains("temporary authentication failure"));
        server.task.await.unwrap();
    }

    #[tokio::test]
    async fn explicit_smtp_rejection_returns_rejected_with_status_code() {
        let server = start_server(ServerBehavior::RejectRecipient).await;
        let provider = SmtpDeliveryProvider::for_test(config(server.addr.port()), server.addr)
            .expect("valid loopback test transport");

        assert!(matches!(
            provider.send(request()).await.unwrap(),
            SendOutcome::Rejected {
                permanent: true,
                code,
            } if code == "smtp_550"
        ));
        server.task.await.unwrap();
    }

    #[tokio::test]
    async fn dropped_final_reply_is_unknown_and_must_not_be_retried() {
        let server = start_server(ServerBehavior::DropFinalReply).await;
        let provider = SmtpDeliveryProvider::for_test(config(server.addr.port()), server.addr)
            .expect("valid loopback test transport");

        assert!(matches!(
            provider.send(request()).await.unwrap(),
            SendOutcome::Unknown { .. }
        ));
        assert!(!server.captured.lock().await.message.is_empty());
        server.task.await.unwrap();
    }

    #[test]
    fn security_serializes_as_lowercase_transport_names() {
        assert_eq!(
            serde_json::to_string(&SmtpSecurity::Tls).unwrap(),
            "\"tls\""
        );
        assert_eq!(
            serde_json::to_string(&SmtpSecurity::Starttls).unwrap(),
            "\"starttls\""
        );
    }

    #[test]
    fn rejects_invalid_host_and_zero_port_configuration() {
        let mut invalid_host = config(587);
        invalid_host.host = "relay.example\r\nMAIL FROM:forged@example.net".into();
        assert!(SmtpDeliveryProvider::new(invalid_host).is_err());

        assert!(SmtpDeliveryProvider::new(config(0)).is_err());
    }
}
