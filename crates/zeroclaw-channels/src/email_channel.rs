#![allow(clippy::uninlined_format_args)]
#![allow(clippy::map_unwrap_or)]
#![allow(clippy::redundant_closure_for_method_calls)]
#![allow(clippy::cast_lossless)]
#![allow(clippy::trim_split_whitespace)]
#![allow(clippy::doc_link_with_quotes)]
#![allow(clippy::doc_markdown)]
#![allow(clippy::too_many_lines)]
#![allow(clippy::unnecessary_map_or)]

use anyhow::{Context, Result};
use async_imap::extensions::idle::IdleResponse;
use async_imap::types::Fetch;
use async_trait::async_trait;
use futures_util::TryStreamExt;
use lettre::message::header::ContentType;
use lettre::message::{Attachment, MultiPart, SinglePart};
use lettre::transport::smtp::authentication::Credentials;
use lettre::{Message, SmtpTransport, Transport};
use mail_parser::{MessageParser, MimeHeaders};
use pulldown_cmark::{Options, Parser, html};
use rustls::{ClientConfig, RootCertStore};
use rustls_pki_types::DnsName;
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::net::TcpStream;
use tokio::sync::{Mutex, mpsc};
use tokio::time::{sleep, timeout};
use tokio_rustls::TlsConnector;

use zeroclaw_api::channel::{Channel, ChannelMessage, SendMessage};
use zeroclaw_tools::email_imap::{ImapSession, TlsStreamTolerant};

pub use zeroclaw_config::scattered_types::EmailConfig;

// `TlsStreamTolerant` (the rustls wrapper that turns Exchange's missing
// `close_notify` into a graceful EOF) and the `ImapSession` alias live in
// `zeroclaw_tools::email_imap`, the canonical IMAP utility shared by the
// read-only email tools. Imported here so there is a single definition.

/// Email channel — IMAP IDLE for instant push notifications, SMTP for outbound.
///
/// Inbound sender authorization lives in `peer_groups` in V3; this channel
/// resolves the authorized senders at message-time via [`Self::peer_resolver`]
/// rather than reading a per-channel `allowed_senders` field (it no longer
/// exists on `EmailConfig`).
pub struct EmailChannel {
    pub config: EmailConfig,
    /// The alias key under `[channels.email.<alias>]` this handle is
    /// bound to. Used to scope peer-group writes and resolver lookups.
    pub alias: String,
    /// Resolves inbound external peers from canonical state at message-time.
    /// No cache (see AGENTS.md "ABSOLUTE RULE — SINGLE SOURCE OF TRUTH").
    pub peer_resolver: Arc<dyn Fn() -> Vec<String> + Send + Sync>,
    seen_messages: Arc<Mutex<HashSet<String>>>,
    auth_service: Option<Arc<zeroclaw_providers::auth::AuthService>>,
}

impl EmailChannel {
    pub fn new(
        config: EmailConfig,
        alias: impl Into<String>,
        peer_resolver: Arc<dyn Fn() -> Vec<String> + Send + Sync>,
    ) -> Self {
        Self {
            config,
            alias: alias.into(),
            peer_resolver,
            seen_messages: Arc::new(Mutex::new(HashSet::new())),
            auth_service: None,
        }
    }

    /// Wire in the auth service so XOAUTH2 token refresh works for
    /// channels configured with `[channels.email.<alias>.oauth2]`.
    pub fn with_auth_service(
        mut self,
        auth_service: Arc<zeroclaw_providers::auth::AuthService>,
    ) -> Self {
        self.auth_service = Some(auth_service);
        self
    }

    /// Check if a sender email is in the allowlist (peer group).
    ///
    /// Email allowlist entries support three syntaxes — preserved from
    /// the legacy `EmailConfig::allowed_senders` semantics:
    /// - `*`                wildcard, allow anyone.
    /// - `user@host`        full address, case-insensitive.
    /// - `@host` / `host`   domain match, case-insensitive.
    pub fn is_sender_allowed(&self, email: &str) -> bool {
        let peers = (self.peer_resolver)();
        Self::is_email_sender_allowed(&peers, email)
    }

    /// Pure, testable predicate that applies the email-allowlist match
    /// semantics against an already-resolved peer list.
    ///
    /// Domain-class email matching (`@host` / bare `host` admit a whole
    /// domain; `user@host` is a full case-insensitive address) can't be
    /// expressed by the `crate::allowlist::Match` modes, so the per-entry
    /// comparison runs through `crate::allowlist::is_user_allowed_by`. `peers`
    /// is the caller's freshly-resolved list; no allowlist state is cached.
    fn is_email_sender_allowed(peers: &[String], email: &str) -> bool {
        crate::allowlist::is_user_allowed_by(peers, email, |allowed, email| {
            let email_lower = email.to_lowercase();
            if allowed.starts_with('@') {
                // Domain match with @ prefix: "@example.com"
                email_lower.ends_with(&allowed.to_lowercase())
            } else if allowed.contains('@') {
                // Full email address match
                allowed.eq_ignore_ascii_case(email)
            } else {
                // Domain match without @ prefix: "example.com"
                email_lower.ends_with(&format!("@{}", allowed.to_lowercase()))
            }
        })
    }

    /// Strip HTML tags from content (basic)
    pub fn strip_html(html: &str) -> String {
        let mut result = String::new();
        let mut in_tag = false;
        for ch in html.chars() {
            match ch {
                '<' => in_tag = true,
                '>' => in_tag = false,
                _ if !in_tag => result.push(ch),
                _ => {}
            }
        }
        let mut normalized = String::with_capacity(result.len());
        for word in result.split_whitespace() {
            if !normalized.is_empty() {
                normalized.push(' ');
            }
            normalized.push_str(word);
        }
        normalized
    }

    /// Extract the sender address from a parsed email
    fn extract_sender(parsed: &mail_parser::Message) -> String {
        parsed
            .from()
            .and_then(|addr| addr.first())
            .and_then(|a| a.address())
            .map(|s| s.to_string())
            .unwrap_or_else(|| "unknown".into())
    }

    /// Extract readable text from a parsed email
    fn extract_text(parsed: &mail_parser::Message) -> String {
        if let Some(text) = parsed.body_text(0) {
            return text.to_string();
        }
        if let Some(html) = parsed.body_html(0) {
            return Self::strip_html(html.as_ref());
        }
        for part in parsed.attachments() {
            let part: &mail_parser::MessagePart = part;
            if let Some(ct) = MimeHeaders::content_type(part)
                && ct.ctype() == "text"
                && let Ok(text) = std::str::from_utf8(part.contents())
            {
                let name = MimeHeaders::attachment_name(part).unwrap_or("file");
                return format!("[Attachment: {}]\n{}", name, text);
            }
        }
        "(no readable content)".to_string()
    }

    /// Extract binary attachments from a parsed email as MediaAttachment entries.
    fn extract_attachments(
        &self,
        parsed: &mail_parser::Message,
    ) -> Vec<zeroclaw_api::media::MediaAttachment> {
        let mut attachments = Vec::new();
        let mut total_size = 0;

        for part in parsed.attachments() {
            let part: &mail_parser::MessagePart = part;
            let ct = MimeHeaders::content_type(part);
            let mime_str =
                ct.map(|c| format!("{}/{}", c.ctype(), c.subtype().unwrap_or("octet-stream")));

            // Skip text parts — already handled by extract_text()
            if let Some(ref m) = mime_str
                && m.starts_with("text/")
            {
                continue;
            }

            let data = part.contents().to_vec();
            if data.is_empty() {
                continue;
            }

            // Check size limit
            total_size += data.len();
            if total_size > self.config.max_attachment_bytes {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                    &format!(
                        "Attachment size limit exceeded ({} bytes), dropping remaining attachments",
                        self.config.max_attachment_bytes
                    )
                );
                break;
            }

            let file_name = MimeHeaders::attachment_name(part)
                .unwrap_or("attachment")
                .to_string();

            attachments.push(zeroclaw_api::media::MediaAttachment {
                file_name,
                data,
                mime_type: mime_str,
            });
        }
        attachments
    }

    /// Attempt to obtain a bearer token via the auth service for XOAUTH2.
    /// Returns `Ok(None)` when no oauth2 config is set on this channel.
    async fn get_oauth2_token(&self) -> Result<Option<String>> {
        let Some(ref oauth2) = self.config.oauth2 else {
            return Ok(None);
        };
        let Some(ref auth_service) = self.auth_service else {
            anyhow::bail!(
                "email channel '{}' has oauth2 configured but no auth service was wired in",
                self.alias
            );
        };
        let channel_key = format!("email.{}", self.alias);
        auth_service
            .get_valid_email_oauth2_token(
                &channel_key,
                None,
                &oauth2.token_url,
                &oauth2.client_id,
                &oauth2.scopes,
            )
            .await
    }

    /// Connect to IMAP server with TLS and authenticate
    async fn connect_imap(&self) -> Result<ImapSession> {
        let addr = format!("{}:{}", self.config.imap_host, self.config.imap_port);
        ::zeroclaw_log::record!(
            DEBUG,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
            &format!("Connecting to IMAP server at {}", addr)
        );

        // Connect TCP
        let tcp = TcpStream::connect(&addr).await?;

        // Establish TLS using rustls.
        let certs = RootCertStore {
            roots: webpki_roots::TLS_SERVER_ROOTS.into(),
        };
        let config = ClientConfig::builder()
            .with_root_certificates(certs)
            .with_no_client_auth();
        let tls_connector: TlsConnector = Arc::new(config).into();
        let sni: DnsName = self.config.imap_host.clone().try_into()?;
        let raw_stream = tls_connector.connect(sni.into(), tcp).await?;
        let stream = TlsStreamTolerant(raw_stream);

        // Create IMAP client and consume the server greeting.
        // async-imap requires the caller to read the greeting before issuing
        // any commands (see async-imap docs). login() tolerates a missing
        // explicit read because check_done_ok_from() loops past untagged
        // responses — but do_auth_handshake() used by authenticate() does not,
        // so without this the XOAUTH2 exchange deadlocks on the greeting line.
        let mut client = async_imap::Client::new(stream);
        client
            .read_response()
            .await
            .context("IMAP server did not send a greeting")?;

        // Authenticate: XOAUTH2 when oauth2 is configured, plain LOGIN otherwise.
        let session = if let Some(token) = self.get_oauth2_token().await? {
            struct XOAuth2 {
                user: String,
                token: String,
            }
            impl async_imap::Authenticator for XOAuth2 {
                type Response = String;
                fn process(&mut self, _challenge: &[u8]) -> String {
                    xoauth2_sasl_response(&self.user, &self.token)
                }
            }
            client
                .authenticate(
                    "XOAUTH2",
                    XOAuth2 {
                        user: self.config.username.clone(),
                        token,
                    },
                )
                .await
                .map_err(|(e, _)| {
                    ::zeroclaw_log::record!(
                        ERROR,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                            .with_attrs(::serde_json::json!({
                                "phase": "imap_xoauth2",
                                "error": format!("{}", e),
                            })),
                        "email: IMAP XOAUTH2 authentication failed"
                    );
                    anyhow::Error::msg(format!("IMAP XOAUTH2 auth failed: {}", e))
                })?
        } else {
            client
                .login(&self.config.username, &self.config.password)
                .await
                .map_err(|(e, _)| {
                    ::zeroclaw_log::record!(
                        ERROR,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                            .with_attrs(::serde_json::json!({
                                "phase": "imap_login",
                                "error": format!("{}", e),
                            })),
                        "email: IMAP login failed"
                    );
                    anyhow::Error::msg(format!("IMAP login failed: {}", e))
                })?
        };

        ::zeroclaw_log::record!(
            DEBUG,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
            "IMAP login successful"
        );
        Ok(session)
    }

    /// Maximum number of messages fetched per IMAP round-trip.
    /// Bounds peak memory when the mailbox has a large unseen backlog.
    const MAX_FETCH_BATCH: usize = 10;

    fn build_parsed_email(
        &self,
        parsed: &mail_parser::Message,
        uid: u32,
        uid_validity: Option<u32>,
    ) -> ParsedEmail {
        let sender = Self::extract_sender(parsed);
        let subject = parsed.subject().unwrap_or("(no subject)").to_string();
        let body_text = Self::extract_text(parsed);
        let content = format!("Subject: {}\n\n{}", subject, body_text);
        let msg_id = parsed
            .message_id()
            .map(|s| s.to_string())
            .unwrap_or_else(|| {
                self.stable_missing_message_id(parsed, uid, uid_validity, &sender, &body_text)
            });
        #[allow(clippy::cast_sign_loss)]
        let timestamp = parsed
            .date()
            .map(|d| {
                chrono::NaiveDate::from_ymd_opt(d.year as i32, u32::from(d.month), u32::from(d.day))
                    .and_then(|date| {
                        date.and_hms_opt(
                            u32::from(d.hour),
                            u32::from(d.minute),
                            u32::from(d.second),
                        )
                    })
                    .map_or(0, |n| n.and_utc().timestamp() as u64)
            })
            .unwrap_or_else(|| {
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0)
            });
        let attachments = self.extract_attachments(parsed);
        ParsedEmail {
            msg_id,
            sender,
            subject,
            content,
            timestamp,
            attachments,
        }
    }

    fn stable_missing_message_id(
        &self,
        parsed: &mail_parser::Message,
        uid: u32,
        uid_validity: Option<u32>,
        sender: &str,
        body_text: &str,
    ) -> String {
        let mut hasher = Sha256::new();
        hasher.update(self.config.imap_host.as_bytes());
        hasher.update(b"\0");
        hasher.update(self.config.username.as_bytes());
        hasher.update(b"\0");
        hasher.update(self.config.imap_folder.as_bytes());

        if let Some(uid_validity) = uid_validity.filter(|_| uid != 0) {
            hasher.update(b"\0uidvalidity\0");
            hasher.update(uid_validity.to_be_bytes());
            hasher.update(b"\0uid\0");
            hasher.update(uid.to_be_bytes());
            let digest = hasher.finalize();
            return format!("email-imap-{}-{uid}", hex::encode(&digest[..16]));
        }

        hasher.update(b"\0content\0");
        hasher.update(sender.as_bytes());
        hasher.update(b"\0");
        if let Some(subject) = parsed.subject() {
            hasher.update(subject.as_bytes());
        }
        hasher.update(b"\0");
        if let Some(date) = parsed.date() {
            let date_key = format!(
                "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}",
                date.year, date.month, date.day, date.hour, date.minute, date.second
            );
            hasher.update(date_key.as_bytes());
        }
        hasher.update(b"\0");
        hasher.update(body_text.as_bytes());

        let digest = hasher.finalize();
        format!("email-fallback-{}", hex::encode(&digest[..16]))
    }

    /// Active-mode startup drain: fetch all UNSEEN messages using RFC822.
    /// RFC822 implicitly sets `\Seen` on every fetched message per RFC 3501.
    /// Only called when `observer_mode = false`.
    async fn fetch_unseen_active(
        &self,
        session: &mut ImapSession,
        uid_validity: Option<u32>,
    ) -> Result<Vec<ParsedEmail>> {
        let uids = session.uid_search("UNSEEN").await?;
        let mut uid_list: Vec<u32> = uids.into_iter().collect();
        if uid_list.is_empty() {
            return Ok(Vec::new());
        }
        uid_list.sort_unstable();

        let mut results = Vec::new();
        for chunk in uid_list.chunks(Self::MAX_FETCH_BATCH) {
            let uid_set: String = chunk
                .iter()
                .map(|u| u.to_string())
                .collect::<Vec<_>>()
                .join(",");
            // RFC822 implicitly sets \Seen — intentional in active (non-observer) mode.
            let messages = session.uid_fetch(&uid_set, "RFC822").await?;
            let messages: Vec<Fetch> = messages.try_collect().await?;
            for msg in messages {
                if let Some(body) = msg.body()
                    && let Some(parsed) = MessageParser::default().parse(body)
                {
                    results.push(self.build_parsed_email(
                        &parsed,
                        msg.uid.unwrap_or(0),
                        uid_validity,
                    ));
                }
            }
        }
        Ok(results)
    }

    /// Fetch messages with UID >= uid_threshold. Never modifies any flag.
    /// Returns parsed messages and the new threshold (max fetched UID + 1).
    async fn fetch_new(
        &self,
        session: &mut ImapSession,
        uid_threshold: u32,
        uid_validity: Option<u32>,
    ) -> Result<(Vec<ParsedEmail>, u32)> {
        let search = format!("UID {}:*", uid_threshold);
        let uids = session.uid_search(&search).await?;

        // uid_search("UID X:*") can return UIDs below X on some servers if no
        // message exists at X — filter to be safe.
        let mut uid_list: Vec<u32> = uids.into_iter().filter(|&u| u >= uid_threshold).collect();
        if uid_list.is_empty() {
            return Ok((Vec::new(), uid_threshold));
        }
        uid_list.sort_unstable();
        let new_threshold = uid_list.last().copied().unwrap_or(uid_threshold) + 1;

        ::zeroclaw_log::record!(
            DEBUG,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
            &format!("New message(s) arrived: {} uid(s)", uid_list.len())
        );

        let mut results = Vec::new();

        for chunk in uid_list.chunks(Self::MAX_FETCH_BATCH) {
            let uid_set: String = chunk
                .iter()
                .map(|u| u.to_string())
                .collect::<Vec<_>>()
                .join(",");

            // BODY.PEEK[] — no implicit \Seen. We do not touch flags at all.
            let messages = session.uid_fetch(&uid_set, "BODY.PEEK[]").await?;
            let messages: Vec<Fetch> = messages.try_collect().await?;

            for msg in messages {
                if let Some(body) = msg.body()
                    && let Some(parsed) = MessageParser::default().parse(body)
                {
                    results.push(self.build_parsed_email(
                        &parsed,
                        msg.uid.unwrap_or(0),
                        uid_validity,
                    ));
                }
            }
        }

        Ok((results, new_threshold))
    }

    /// Run the IDLE loop, returning when a new message arrives or timeout
    /// Note: IDLE consumes the session and returns it via done()
    async fn wait_for_changes(
        &self,
        session: ImapSession,
    ) -> Result<(IdleWaitResult, ImapSession)> {
        let idle_timeout = Duration::from_secs(self.config.idle_timeout_secs);

        // Start IDLE mode - this consumes the session
        let mut idle = session.idle();
        idle.init().await?;

        ::zeroclaw_log::record!(
            DEBUG,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
            "Entering IMAP IDLE mode"
        );

        // wait() returns (future, stop_source) - we only need the future
        let (wait_future, _stop_source) = idle.wait();

        // Wait for server notification or timeout
        let result = timeout(idle_timeout, wait_future).await;

        match result {
            Ok(Ok(response)) => {
                ::zeroclaw_log::record!(
                    DEBUG,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                    &format!("IDLE response: {:?}", response)
                );
                // Done with IDLE, return session to normal mode
                let session = idle.done().await?;
                let wait_result = match response {
                    IdleResponse::NewData(_) => IdleWaitResult::NewMail,
                    IdleResponse::Timeout => IdleWaitResult::Timeout,
                    IdleResponse::ManualInterrupt => IdleWaitResult::Interrupted,
                };
                Ok((wait_result, session))
            }
            Ok(Err(e)) => {
                // Try to clean up IDLE state
                let _ = idle.done().await;
                ::zeroclaw_log::record!(
                    ERROR,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({
                            "phase": "idle_wait",
                            "error": format!("{}", e),
                        })),
                    "email: IDLE error"
                );
                Err(anyhow::Error::msg(format!("IDLE error: {}", e)))
            }
            Err(_) => {
                // Timeout - RFC 2177 recommends restarting IDLE every 29 minutes
                ::zeroclaw_log::record!(
                    DEBUG,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                    "IDLE timeout reached, will re-establish"
                );
                let session = idle.done().await?;
                Ok((IdleWaitResult::Timeout, session))
            }
        }
    }

    /// Main listen loop with automatic reconnection.
    ///
    /// Probes the server's CAPABILITY list after login and picks between:
    /// - IMAP IDLE (RFC 2177) for instant push when the server advertises it.
    /// - Periodic polling when the server does not support IDLE (e.g. seznam.cz).
    async fn listen_with_reconnect(&self, tx: mpsc::Sender<ChannelMessage>) -> Result<()> {
        let mut backoff = Duration::from_secs(1);
        let max_backoff = Duration::from_secs(60);

        loop {
            match self.run_session(&tx).await {
                Ok(()) => {
                    // Clean exit (channel closed)
                    return Ok(());
                }
                Err(e) => {
                    ::zeroclaw_log::record!(
                        ERROR,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure),
                        &format!(
                            "IMAP session error: {}. Reconnecting in {:?}...",
                            e, backoff
                        )
                    );
                    sleep(backoff).await;
                    // Exponential backoff with cap
                    backoff = std::cmp::min(backoff * 2, max_backoff);
                }
            }
        }
    }

    /// Run a single IMAP session. Probes server capabilities and dispatches
    /// to the IDLE or polling inner loop.
    async fn run_session(&self, tx: &mpsc::Sender<ChannelMessage>) -> Result<()> {
        let mut session = self.connect_imap().await?;
        let mailbox = session.select(&self.config.imap_folder).await?;
        let uid_validity = mailbox.uid_validity;

        // In observer mode: capture uid_next so we only ever process emails that
        // arrive AFTER this session starts. No startup drain, no flag changes.
        //
        // In active mode: drain UNSEEN messages on startup (RFC822 implicitly
        // sets \Seen), then track via uid_next for subsequent messages.
        let uid_threshold = if self.config.observer_mode {
            let threshold = mailbox.uid_next.unwrap_or(1);
            ::zeroclaw_log::record!(
                INFO,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                &format!(
                    "Email channel observer mode: will only process messages with UID >= {} (no flag changes ever)",
                    threshold
                )
            );
            threshold
        } else {
            // Active mode: drain UNSEEN now, then watch for new arrivals.
            let unseen = self.fetch_unseen_active(&mut session, uid_validity).await?;
            let next_uid = mailbox.uid_next.unwrap_or(1);
            for email in unseen {
                if !self.dispatch_email(email, tx).await? {
                    return Ok(()); // channel closed before we even started listening
                }
            }
            next_uid
        };

        let has_idle = {
            let caps = session.capabilities().await?;
            caps.has_str("IDLE")
        };

        if has_idle {
            ::zeroclaw_log::record!(
                INFO,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                &format!(
                    "Email channel listening on {} (IMAP IDLE, instant push, uid_threshold={})",
                    self.config.imap_folder, uid_threshold
                )
            );
            self.run_idle_inner(session, tx, uid_threshold, uid_validity)
                .await
        } else {
            let poll_interval = Duration::from_secs(self.config.poll_interval_secs);
            ::zeroclaw_log::record!(
                INFO,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                &format!(
                    "Email channel listening on {} (IMAP polling, interval: {:?}, uid_threshold={})",
                    self.config.imap_folder, poll_interval, uid_threshold
                )
            );
            self.run_poll_inner(session, tx, poll_interval, uid_threshold, uid_validity)
                .await
        }
    }

    /// IDLE-based wait loop. Consumes and returns the session across IDLE round trips.
    async fn run_idle_inner(
        &self,
        mut session: ImapSession,
        tx: &mpsc::Sender<ChannelMessage>,
        mut uid_threshold: u32,
        uid_validity: Option<u32>,
    ) -> Result<()> {
        loop {
            match self.wait_for_changes(session).await {
                Ok((IdleWaitResult::NewMail, returned_session)) => {
                    ::zeroclaw_log::record!(
                        DEBUG,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                        "New mail notification received"
                    );
                    session = returned_session;
                    uid_threshold = self
                        .process_new(&mut session, tx, uid_threshold, uid_validity)
                        .await?;
                }
                Ok((IdleWaitResult::Timeout, returned_session)) => {
                    session = returned_session;
                    uid_threshold = self
                        .process_new(&mut session, tx, uid_threshold, uid_validity)
                        .await?;
                }
                Ok((IdleWaitResult::Interrupted, _)) => {
                    ::zeroclaw_log::record!(
                        INFO,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                        "IDLE interrupted, exiting"
                    );
                    return Ok(());
                }
                Err(e) => return Err(e),
            }
        }
    }

    /// Polling-based wait loop. Used when the server does not advertise IDLE.
    async fn run_poll_inner(
        &self,
        mut session: ImapSession,
        tx: &mpsc::Sender<ChannelMessage>,
        poll_interval: Duration,
        mut uid_threshold: u32,
        uid_validity: Option<u32>,
    ) -> Result<()> {
        loop {
            sleep(poll_interval).await;
            session.noop().await?;
            uid_threshold = self
                .process_new(&mut session, tx, uid_threshold, uid_validity)
                .await?;
        }
    }

    /// Send one parsed email to the runtime channel if sender is allowed and not already seen.
    /// Returns false if the channel is closed (caller should stop).
    async fn dispatch_email(
        &self,
        email: ParsedEmail,
        tx: &mpsc::Sender<ChannelMessage>,
    ) -> Result<bool> {
        if !self.is_sender_allowed(&email.sender) {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                &format!("Blocked email from {}", email.sender)
            );
            return Ok(true);
        }
        let is_new = {
            let mut seen = self.seen_messages.lock().await;
            seen.insert(email.msg_id.clone())
        };
        if !is_new {
            return Ok(true);
        }
        let msg = ChannelMessage {
            id: email.msg_id,
            reply_target: email.sender.clone(),
            sender: email.sender,
            content: email.content,
            channel: "email".to_string(),
            channel_alias: Some(self.alias.clone()),
            timestamp: email.timestamp,
            thread_ts: None,
            interruption_scope_id: None,
            attachments: email.attachments,
            subject: Some(email.subject),
        };
        Ok(tx.send(msg).await.is_ok())
    }

    /// Process newly arrived messages (UID >= uid_threshold). Returns updated threshold.
    async fn process_new(
        &self,
        session: &mut ImapSession,
        tx: &mpsc::Sender<ChannelMessage>,
        uid_threshold: u32,
        uid_validity: Option<u32>,
    ) -> Result<u32> {
        let (messages, new_threshold) =
            self.fetch_new(session, uid_threshold, uid_validity).await?;

        for email in messages {
            if !self.dispatch_email(email, tx).await? {
                return Ok(new_threshold); // channel closed
            }
        }

        Ok(new_threshold)
    }

    fn smtp_credentials(&self) -> Credentials {
        let user = smtp_credential_override(self.config.smtp_username.as_deref())
            .unwrap_or(&self.config.username)
            .to_owned();
        let pass = smtp_credential_override(self.config.smtp_password.as_deref())
            .unwrap_or(&self.config.password)
            .to_owned();
        Credentials::new(user, pass)
    }

    fn create_smtp_transport(&self) -> Result<SmtpTransport> {
        let creds = self.smtp_credentials();
        let transport = if self.config.smtp_tls {
            SmtpTransport::relay(&self.config.smtp_host)?
                .port(self.config.smtp_port)
                .credentials(creds)
                .build()
        } else {
            SmtpTransport::builder_dangerous(&self.config.smtp_host)
                .port(self.config.smtp_port)
                .credentials(creds)
                .build()
        };
        Ok(transport)
    }
}

/// Internal struct for parsed email data
struct ParsedEmail {
    msg_id: String,
    sender: String,
    subject: String,
    content: String,
    timestamp: u64,
    attachments: Vec<zeroclaw_api::media::MediaAttachment>,
}

/// Result from waiting on IDLE
enum IdleWaitResult {
    NewMail,
    Timeout,
    Interrupted,
}

impl ::zeroclaw_api::attribution::Attributable for EmailChannel {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        ::zeroclaw_api::attribution::Role::Channel(::zeroclaw_api::attribution::ChannelKind::Email)
    }
    fn alias(&self) -> &str {
        &self.alias
    }
}

fn markdown_to_html(md: &str) -> String {
    let mut options = Options::empty();
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_STRIKETHROUGH);
    let parser = Parser::new_ext(md, options);
    let mut html_output = String::new();
    html::push_html(&mut html_output, parser);
    html_output
}

fn smtp_credential_override(value: Option<&str>) -> Option<&str> {
    value.filter(|value| !value.trim().is_empty())
}

fn is_synthetic_email_message_id(value: &str) -> bool {
    value.starts_with("email-imap-") || value.starts_with("email-fallback-")
}

#[async_trait]

impl Channel for EmailChannel {
    fn name(&self) -> &str {
        "email"
    }

    async fn send(&self, message: &SendMessage) -> Result<()> {
        // Use explicit subject if provided, otherwise fall back to legacy parsing or default
        let default_subject = self.config.default_subject.as_str();
        let (subject, body) = if let Some(ref subj) = message.subject {
            (subj.as_str(), message.content.as_str())
        } else if message.content.starts_with("Subject: ") {
            if let Some(pos) = message.content.find('\n') {
                (&message.content[9..pos], message.content[pos + 1..].trim())
            } else {
                (default_subject, message.content.as_str())
            }
        } else {
            (default_subject, message.content.as_str())
        };

        let mut builder = Message::builder()
            .from(self.config.from_address.parse()?)
            .to(message.recipient.parse()?)
            .subject(subject);
        if let Some(ref reply_id) = message.in_reply_to
            && !is_synthetic_email_message_id(reply_id)
        {
            builder = builder.in_reply_to(reply_id.clone());
        }
        let mut att_parts: Vec<(String, Vec<u8>, ContentType)> = Vec::new();
        for att in &message.attachments {
            let content_type = att
                .mime_type
                .as_deref()
                .and_then(|m| ContentType::parse(m).ok())
                .unwrap_or_else(|| {
                    ContentType::parse("application/octet-stream").expect("hardcoded MIME type")
                });
            let att_data = resolve_attachment_data(&att.file_name, &att.data)?;
            let att_name = std::path::Path::new(&att.file_name)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or(&att.file_name)
                .to_string();
            att_parts.push((att_name, att_data, content_type));
        }

        let email = if self.config.html_body {
            let alt = MultiPart::alternative()
                .singlepart(SinglePart::plain(body.to_string()))
                .singlepart(SinglePart::html(markdown_to_html(body)));
            if att_parts.is_empty() {
                builder.multipart(alt)?
            } else {
                let mut mixed = MultiPart::mixed().multipart(alt);
                for (name, data, ct) in att_parts {
                    mixed = mixed.singlepart(Attachment::new(name).body(data, ct));
                }
                builder.multipart(mixed)?
            }
        } else {
            let plain = SinglePart::plain(body.to_string());
            if att_parts.is_empty() {
                builder.singlepart(plain)?
            } else {
                let mut mixed = MultiPart::mixed().singlepart(plain);
                for (name, data, ct) in att_parts {
                    mixed = mixed.singlepart(Attachment::new(name).body(data, ct));
                }
                builder.multipart(mixed)?
            }
        };

        let transport = self.create_smtp_transport()?;
        transport.send(&email)?;
        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
            &format!(
                "Email sent to {} ({} attachments)",
                message.recipient,
                message.attachments.len()
            )
        );
        Ok(())
    }

    async fn listen(&self, tx: mpsc::Sender<ChannelMessage>) -> Result<()> {
        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
            &format!(
                "Starting email channel on {} (IDLE preferred, polling fallback)",
                self.config.imap_folder
            )
        );
        self.listen_with_reconnect(tx).await
    }

    async fn health_check(&self) -> bool {
        // Fully async health check - attempt IMAP connection
        match timeout(Duration::from_secs(10), self.connect_imap()).await {
            Ok(Ok(mut session)) => {
                // Try to logout cleanly
                let _ = session.logout().await;
                true
            }
            Ok(Err(e)) => {
                ::zeroclaw_log::record!(
                    DEBUG,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                    &format!("Health check failed: {}", e)
                );
                false
            }
            Err(_) => {
                ::zeroclaw_log::record!(
                    DEBUG,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                    "Health check timed out"
                );
                false
            }
        }
    }
}

/// Resolve the byte content of an attachment for sending.
///
/// # Trust boundary
///
/// `file_name` is treated as a file-system path **only** when `data` is empty.
/// This fallback exists exclusively for internally constructed
/// [`MediaAttachment`](zeroclaw_api::media::MediaAttachment) values whose
/// bytes were intentionally omitted (e.g. created via
/// [`MediaAttachment::from_file`](zeroclaw_api::media::MediaAttachment::from_file)
/// after a round-trip through serialization).  Callers that build attachments
/// from untrusted input — user messages, HTTP request bodies, or any external
/// data source — **must** validate or constrain `file_name` before reaching
/// this function; no additional path sanitization is applied here.
///
/// Read errors are propagated rather than silently suppressed.
fn resolve_attachment_data(file_name: &str, data: &[u8]) -> anyhow::Result<Vec<u8>> {
    if data.is_empty() && std::path::Path::new(file_name).exists() {
        std::fs::read(file_name).map_err(|e| {
            anyhow::Error::msg(format!("failed to read attachment '{}': {}", file_name, e))
        })
    } else {
        Ok(data.to_vec())
    }
}

/// Build the SASL XOAUTH2 initial client response for IMAP `AUTHENTICATE`.
///
/// Format per the XOAUTH2 spec: `user=<user>^Aauth=Bearer <token>^A^A`,
/// where `^A` is the `0x01` control byte. The transport base64-encodes this.
fn xoauth2_sasl_response(user: &str, token: &str) -> String {
    format!("user={user}\x01auth=Bearer {token}\x01\x01")
}

#[cfg(test)]
mod tests {
    use super::xoauth2_sasl_response;

    #[test]
    fn xoauth2_sasl_response_matches_spec() {
        let got = xoauth2_sasl_response("alice@example.com", "ya29.TOKEN");
        assert_eq!(
            got,
            "user=alice@example.com\x01auth=Bearer ya29.TOKEN\x01\x01"
        );
        // Exactly three 0x01 separators, none trailing beyond the spec.
        assert_eq!(got.matches('\x01').count(), 3);
        assert!(got.starts_with("user="));
        assert!(got.ends_with("\x01\x01"));
    }

    #[test]
    fn observer_mode_defaults_off() {
        // observer_mode is opt-in: default false keeps the normal flag-changing
        // read path; only when explicitly enabled does the channel switch to
        // the uid-threshold, BODY.PEEK, zero-flag-change behavior.
        assert!(!super::EmailConfig::default().observer_mode);
    }

    fn default_imap_port() -> u16 {
        993
    }
    fn default_smtp_port() -> u16 {
        465
    }
    fn default_imap_folder() -> String {
        "INBOX".into()
    }
    fn default_idle_timeout() -> u64 {
        1740
    }
    fn default_true() -> bool {
        true
    }
    fn default_max_attachment_bytes() -> usize {
        25 * 1024 * 1024
    }
    use super::*;

    // -- resolve_attachment_data tests --

    #[test]
    fn resolve_attachment_data_returns_provided_bytes_when_non_empty() {
        let data = b"hello attachment".to_vec();
        let result = resolve_attachment_data("ignored.bin", &data).unwrap();
        assert_eq!(result, data);
    }

    #[test]
    fn resolve_attachment_data_falls_back_to_file_when_data_empty_and_file_exists() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("att.txt");
        std::fs::write(&path, b"file contents").unwrap();
        let result = resolve_attachment_data(path.to_str().unwrap(), &[]).unwrap();
        assert_eq!(result, b"file contents");
    }

    #[test]
    fn resolve_attachment_data_returns_empty_when_data_empty_and_file_absent() {
        // file_name does not exist on disk — should return empty vec, not error.
        // Use a temp dir to guarantee the path does not exist, rather than a
        // hard-coded /tmp path, for portability.
        let dir = tempfile::tempdir().unwrap();
        let absent = dir.path().join("does-not-exist.bin");
        let result = resolve_attachment_data(absent.to_str().unwrap(), &[]).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn resolve_attachment_data_propagates_read_error_on_unreadable_file() {
        // Create a file, then make it unreadable (Unix only).
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("locked.bin");
            std::fs::write(&path, b"secret").unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
            // Permission enforcement is not guaranteed when running as root;
            // skip rather than produce a false failure.  Reading from
            // /proc/self/status is Linux-specific but that is where this test
            // is most likely to run.  On other Unix systems the check falls
            // back to the USER env var, which is a best-effort heuristic only.
            #[cfg(target_os = "linux")]
            let is_root = std::fs::read_to_string("/proc/self/status")
                .ok()
                .and_then(|s| {
                    s.lines()
                        .find(|l| l.starts_with("Uid:"))
                        .and_then(|l| l.split_whitespace().nth(1))
                        .and_then(|uid| uid.parse::<u32>().ok())
                })
                .map(|uid| uid == 0)
                .unwrap_or(false);
            #[cfg(not(target_os = "linux"))]
            let is_root = std::env::var("USER").map(|u| u == "root").unwrap_or(false);
            if is_root {
                return;
            }
            let result = resolve_attachment_data(path.to_str().unwrap(), &[]);
            assert!(result.is_err());
        }
    }

    #[test]
    fn default_smtp_port_uses_tls_port() {
        assert_eq!(default_smtp_port(), 465);
    }

    #[test]
    fn email_config_default_uses_tls_smtp_defaults() {
        let config = EmailConfig::default();
        assert_eq!(config.smtp_port, 465);
        assert!(config.smtp_tls);
    }

    #[test]
    fn default_idle_timeout_is_29_minutes() {
        assert_eq!(default_idle_timeout(), 1740);
    }

    #[test]
    fn max_fetch_batch_bounds_chunk_size() {
        let cap = EmailChannel::MAX_FETCH_BATCH;
        assert_eq!(cap, 10);

        // Under cap: single chunk
        let uids: Vec<u32> = (1..=3).collect();
        let chunks: Vec<&[u32]> = uids.chunks(cap).collect();
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].len(), 3);

        // Exactly at cap: single chunk
        let uids: Vec<u32> = (1..=10).collect();
        let chunks: Vec<&[u32]> = uids.chunks(cap).collect();
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].len(), 10);

        // Over cap: two chunks
        let uids: Vec<u32> = (1..=15).collect();
        let chunks: Vec<&[u32]> = uids.chunks(cap).collect();
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].len(), 10);
        assert_eq!(chunks[1].len(), 5);
    }

    #[tokio::test]
    async fn seen_messages_starts_empty() {
        let channel =
            EmailChannel::new(EmailConfig::default(), "email_test_alias", empty_resolver());
        let seen = channel.seen_messages.lock().await;
        assert!(seen.is_empty());
    }

    #[tokio::test]
    async fn seen_messages_tracks_unique_ids() {
        let channel =
            EmailChannel::new(EmailConfig::default(), "email_test_alias", empty_resolver());
        let mut seen = channel.seen_messages.lock().await;

        assert!(seen.insert("first-id".to_string()));
        assert!(!seen.insert("first-id".to_string()));
        assert!(seen.insert("second-id".to_string()));
        assert_eq!(seen.len(), 2);
    }

    // EmailConfig tests

    #[test]
    fn email_config_default() {
        let config = EmailConfig::default();
        assert_eq!(config.imap_host, "");
        assert_eq!(config.imap_port, 993);
        assert_eq!(config.imap_folder, "INBOX");
        assert_eq!(config.smtp_host, "");
        assert_eq!(config.smtp_port, 465);
        assert!(config.smtp_tls);
        assert_eq!(config.username, "");
        assert_eq!(config.password, "");
        assert_eq!(config.from_address, "");
        assert_eq!(config.idle_timeout_secs, 1740);
    }

    // EmailChannel tests
    //
    // Inbound peer authorization lives in `peer_groups` in V3; the
    // channel resolves the authorized senders via a peer_resolver
    // closure provided at construction.

    fn empty_resolver() -> Arc<dyn Fn() -> Vec<String> + Send + Sync> {
        Arc::new(Vec::new)
    }

    fn resolver_from(peers: Vec<String>) -> Arc<dyn Fn() -> Vec<String> + Send + Sync> {
        Arc::new(move || peers.clone())
    }

    #[test]
    fn email_config_custom() {
        let config = EmailConfig {
            enabled: true,
            imap_host: "imap.example.com".to_string(),
            imap_port: 993,
            imap_folder: "Archive".to_string(),
            smtp_host: "smtp.example.com".to_string(),
            smtp_port: 465,
            smtp_tls: true,
            username: "user@example.com".to_string(),
            password: "pass123".to_string(),
            smtp_username: None,
            smtp_password: None,
            from_address: "bot@example.com".to_string(),
            idle_timeout_secs: 1200,
            poll_interval_secs: 60,
            default_subject: "Custom Subject".to_string(),
            max_attachment_bytes: default_max_attachment_bytes(),
            html_body: true,
            excluded_tools: vec![],
            oauth2: None,
            observer_mode: false,
        };
        assert_eq!(config.imap_host, "imap.example.com");
        assert_eq!(config.imap_folder, "Archive");
        assert_eq!(config.idle_timeout_secs, 1200);
        assert_eq!(config.default_subject, "Custom Subject");
    }

    #[test]
    fn email_config_clone() {
        let config = EmailConfig {
            enabled: true,
            imap_host: "imap.test.com".to_string(),
            imap_port: 993,
            imap_folder: "INBOX".to_string(),
            smtp_host: "smtp.test.com".to_string(),
            smtp_port: 587,
            smtp_tls: true,
            username: "user@test.com".to_string(),
            password: "secret".to_string(),
            smtp_username: None,
            smtp_password: None,
            from_address: "bot@test.com".to_string(),
            idle_timeout_secs: 1740,
            poll_interval_secs: 60,
            default_subject: "Test Subject".to_string(),
            max_attachment_bytes: default_max_attachment_bytes(),
            html_body: true,
            excluded_tools: vec![],
            oauth2: None,
            observer_mode: false,
        };
        let cloned = config.clone();
        assert_eq!(cloned.imap_host, config.imap_host);
        assert_eq!(cloned.smtp_port, config.smtp_port);
        assert_eq!(cloned.default_subject, config.default_subject);
    }

    fn mailbox_identity_config() -> EmailConfig {
        EmailConfig {
            enabled: true,
            imap_host: "imap.private.example.invalid".to_string(),
            imap_port: 993,
            imap_folder: "Sensitive Folder".to_string(),
            smtp_host: "smtp.example.invalid".to_string(),
            smtp_port: 465,
            smtp_tls: true,
            username: "private-user@example.invalid".to_string(),
            password: "secret".to_string(),
            smtp_username: None,
            smtp_password: None,
            from_address: "bot@example.invalid".to_string(),
            idle_timeout_secs: 1740,
            poll_interval_secs: 60,
            default_subject: "Test Subject".to_string(),
            max_attachment_bytes: default_max_attachment_bytes(),
            html_body: true,
            excluded_tools: vec![],
            oauth2: None,
            observer_mode: false,
        }
    }

    fn parse_test_email(raw: &'static [u8]) -> mail_parser::Message<'static> {
        MessageParser::default().parse(raw).unwrap()
    }

    #[test]
    fn build_parsed_email_keeps_existing_message_id() {
        let channel = EmailChannel::new(
            mailbox_identity_config(),
            "email_test_alias",
            empty_resolver(),
        );
        let parsed = parse_test_email(
            b"From: Sender <sender@example.invalid>\r\n\
              Subject: Has Message ID\r\n\
              Message-ID: <stable-message@example.invalid>\r\n\
              \r\n\
              hello",
        );

        let email = channel.build_parsed_email(&parsed, 42, Some(1234));

        assert_eq!(email.msg_id, parsed.message_id().unwrap());
    }

    #[test]
    fn build_parsed_email_uses_stable_uid_fallback_without_message_id() {
        let channel = EmailChannel::new(
            mailbox_identity_config(),
            "email_test_alias",
            empty_resolver(),
        );
        let parsed = parse_test_email(
            b"From: Sender <sender@example.invalid>\r\n\
              Subject: Missing Message ID\r\n\
              Date: Tue, 16 Jun 2026 00:00:00 +0000\r\n\
              \r\n\
              hello",
        );

        let first = channel.build_parsed_email(&parsed, 42, Some(1234)).msg_id;
        let second = channel.build_parsed_email(&parsed, 42, Some(1234)).msg_id;
        let other_uid = channel.build_parsed_email(&parsed, 43, Some(1234)).msg_id;
        let other_uid_validity = channel.build_parsed_email(&parsed, 42, Some(5678)).msg_id;

        assert_eq!(first, second);
        assert_ne!(first, other_uid);
        assert_ne!(first, other_uid_validity);
        assert!(first.starts_with("email-imap-"));
        assert!(first.ends_with("-42"));
        assert!(!first.contains("imap.private.example.invalid"));
        assert!(!first.contains("private-user@example.invalid"));
        assert!(!first.contains("Sensitive Folder"));
    }

    #[test]
    fn build_parsed_email_missing_message_id_fallback_is_scoped_to_mailbox() {
        let parsed = parse_test_email(
            b"From: Sender <sender@example.invalid>\r\n\
              Subject: Missing Message ID\r\n\
              \r\n\
              hello",
        );
        let first_channel = EmailChannel::new(
            mailbox_identity_config(),
            "email_test_alias",
            empty_resolver(),
        );
        let mut other_config = mailbox_identity_config();
        other_config.username = "other-user@example.invalid".to_string();
        let other_channel = EmailChannel::new(other_config, "email_test_alias", empty_resolver());

        let first_id = first_channel
            .build_parsed_email(&parsed, 42, Some(1234))
            .msg_id;
        let other_mailbox_id = other_channel
            .build_parsed_email(&parsed, 42, Some(1234))
            .msg_id;

        assert_ne!(first_id, other_mailbox_id);
    }

    #[test]
    fn build_parsed_email_missing_uid_validity_uses_content_fallback() {
        let channel = EmailChannel::new(
            mailbox_identity_config(),
            "email_test_alias",
            empty_resolver(),
        );
        let parsed = parse_test_email(
            b"From: Sender <sender@example.invalid>\r\n\
              Subject: Missing UIDVALIDITY\r\n\
              Date: Tue, 16 Jun 2026 00:00:00 +0000\r\n\
              \r\n\
              stable body",
        );

        let first = channel.build_parsed_email(&parsed, 42, None).msg_id;
        let second = channel.build_parsed_email(&parsed, 43, None).msg_id;

        assert_eq!(first, second);
        assert!(first.starts_with("email-fallback-"));
    }

    #[test]
    fn build_parsed_email_missing_uid_fallback_is_stable_and_private() {
        let channel = EmailChannel::new(
            mailbox_identity_config(),
            "email_test_alias",
            empty_resolver(),
        );
        let parsed = parse_test_email(
            b"From: Sender <sender@example.invalid>\r\n\
              Subject: Missing UID\r\n\
              Date: Tue, 16 Jun 2026 00:00:00 +0000\r\n\
              \r\n\
              stable body",
        );
        let other_parsed = parse_test_email(
            b"From: Sender <sender@example.invalid>\r\n\
              Subject: Missing UID\r\n\
              Date: Tue, 16 Jun 2026 00:00:00 +0000\r\n\
              \r\n\
              different body",
        );

        let first = channel.build_parsed_email(&parsed, 0, Some(1234)).msg_id;
        let second = channel.build_parsed_email(&parsed, 0, Some(5678)).msg_id;
        let other_content = channel
            .build_parsed_email(&other_parsed, 0, Some(1234))
            .msg_id;

        assert_eq!(first, second);
        assert_ne!(first, other_content);
        assert!(first.starts_with("email-fallback-"));
        assert!(!first.contains("imap.private.example.invalid"));
        assert!(!first.contains("private-user@example.invalid"));
        assert!(!first.contains("Sensitive Folder"));
    }

    #[test]
    fn synthetic_email_message_ids_are_not_reply_header_ids() {
        assert!(is_synthetic_email_message_id(
            "email-imap-57c2da8dd15cdb2f2f3d118a6d636f86-42"
        ));
        assert!(is_synthetic_email_message_id(
            "email-fallback-57c2da8dd15cdb2f2f3d118a6d636f86"
        ));
        assert!(!is_synthetic_email_message_id(
            "<real-message-id@example.invalid>"
        ));
    }

    #[tokio::test]
    async fn email_channel_new() {
        let config = EmailConfig::default();
        let channel = EmailChannel::new(config.clone(), "email_test_alias", empty_resolver());
        assert_eq!(channel.config.imap_host, config.imap_host);

        let seen_guard = channel.seen_messages.lock().await;
        assert_eq!(seen_guard.len(), 0);
    }

    #[test]
    fn email_channel_name() {
        let channel =
            EmailChannel::new(EmailConfig::default(), "email_test_alias", empty_resolver());
        assert_eq!(channel.name(), "email");
    }

    // is_sender_allowed tests

    #[test]
    fn is_sender_allowed_empty_list_denies_all() {
        let channel =
            EmailChannel::new(EmailConfig::default(), "email_test_alias", empty_resolver());
        assert!(!channel.is_sender_allowed("anyone@example.com"));
        assert!(!channel.is_sender_allowed("user@test.com"));
    }

    #[test]
    fn is_sender_allowed_wildcard_allows_all() {
        let channel = EmailChannel::new(
            EmailConfig::default(),
            "email_test_alias",
            resolver_from(vec!["*".to_string()]),
        );
        assert!(channel.is_sender_allowed("anyone@example.com"));
        assert!(channel.is_sender_allowed("user@test.com"));
        assert!(channel.is_sender_allowed("random@domain.org"));
    }

    #[test]
    fn is_sender_allowed_specific_email() {
        let channel = EmailChannel::new(
            EmailConfig::default(),
            "email_test_alias",
            resolver_from(vec!["allowed@example.com".to_string()]),
        );
        assert!(channel.is_sender_allowed("allowed@example.com"));
        assert!(!channel.is_sender_allowed("other@example.com"));
        assert!(!channel.is_sender_allowed("allowed@other.com"));
    }

    #[test]
    fn is_sender_allowed_domain_with_at_prefix() {
        let channel = EmailChannel::new(
            EmailConfig::default(),
            "email_test_alias",
            resolver_from(vec!["@example.com".to_string()]),
        );
        assert!(channel.is_sender_allowed("user@example.com"));
        assert!(channel.is_sender_allowed("admin@example.com"));
        assert!(!channel.is_sender_allowed("user@other.com"));
    }

    #[test]
    fn is_sender_allowed_domain_without_at_prefix() {
        let channel = EmailChannel::new(
            EmailConfig::default(),
            "email_test_alias",
            resolver_from(vec!["example.com".to_string()]),
        );
        assert!(channel.is_sender_allowed("user@example.com"));
        assert!(channel.is_sender_allowed("admin@example.com"));
        assert!(!channel.is_sender_allowed("user@other.com"));
    }

    #[test]
    fn is_sender_allowed_case_insensitive() {
        let channel = EmailChannel::new(
            EmailConfig::default(),
            "email_test_alias",
            resolver_from(vec!["Allowed@Example.COM".to_string()]),
        );
        assert!(channel.is_sender_allowed("allowed@example.com"));
        assert!(channel.is_sender_allowed("ALLOWED@EXAMPLE.COM"));
        assert!(channel.is_sender_allowed("AlLoWeD@eXaMpLe.cOm"));
    }

    #[test]
    fn is_sender_allowed_multiple_senders() {
        let channel = EmailChannel::new(
            EmailConfig::default(),
            "email_test_alias",
            resolver_from(vec![
                "user1@example.com".to_string(),
                "user2@test.com".to_string(),
                "@allowed.com".to_string(),
            ]),
        );
        assert!(channel.is_sender_allowed("user1@example.com"));
        assert!(channel.is_sender_allowed("user2@test.com"));
        assert!(channel.is_sender_allowed("anyone@allowed.com"));
        assert!(!channel.is_sender_allowed("user3@example.com"));
    }

    #[test]
    fn is_sender_allowed_wildcard_with_specific() {
        let channel = EmailChannel::new(
            EmailConfig::default(),
            "email_test_alias",
            resolver_from(vec!["*".to_string(), "specific@example.com".to_string()]),
        );
        assert!(channel.is_sender_allowed("anyone@example.com"));
        assert!(channel.is_sender_allowed("specific@example.com"));
    }

    #[test]
    fn is_sender_allowed_empty_sender() {
        let channel = EmailChannel::new(
            EmailConfig::default(),
            "email_test_alias",
            resolver_from(vec!["@example.com".to_string()]),
        );
        assert!(!channel.is_sender_allowed(""));
        // "@example.com" ends with "@example.com" so it's allowed
        assert!(channel.is_sender_allowed("@example.com"));
    }

    // strip_html tests

    #[test]
    fn strip_html_basic() {
        assert_eq!(EmailChannel::strip_html("<p>Hello</p>"), "Hello");
        assert_eq!(EmailChannel::strip_html("<div>World</div>"), "World");
    }

    #[test]
    fn strip_html_nested_tags() {
        assert_eq!(
            EmailChannel::strip_html("<div><p>Hello <strong>World</strong></p></div>"),
            "Hello World"
        );
    }

    #[test]
    fn strip_html_multiple_lines() {
        let html = "<div>\n  <p>Line 1</p>\n  <p>Line 2</p>\n</div>";
        assert_eq!(EmailChannel::strip_html(html), "Line 1 Line 2");
    }

    #[test]
    fn strip_html_preserves_text() {
        assert_eq!(EmailChannel::strip_html("No tags here"), "No tags here");
        assert_eq!(EmailChannel::strip_html(""), "");
    }

    #[test]
    fn strip_html_handles_malformed() {
        assert_eq!(EmailChannel::strip_html("<p>Unclosed"), "Unclosed");
        // The function removes everything between < and >, so "Text>with>brackets" becomes "Textwithbrackets"
        assert_eq!(
            EmailChannel::strip_html("Text>with>brackets"),
            "Textwithbrackets"
        );
    }

    #[test]
    fn strip_html_self_closing_tags() {
        // Self-closing tags are removed but don't add spaces
        assert_eq!(EmailChannel::strip_html("Hello<br/>World"), "HelloWorld");
        assert_eq!(EmailChannel::strip_html("Text<hr/>More"), "TextMore");
    }

    #[test]
    fn strip_html_attributes_preserved() {
        assert_eq!(
            EmailChannel::strip_html("<a href=\"http://example.com\">Link</a>"),
            "Link"
        );
    }

    #[test]
    fn strip_html_multiple_spaces_collapsed() {
        assert_eq!(
            EmailChannel::strip_html("<p>Word</p>  <p>Word</p>"),
            "Word Word"
        );
    }

    #[test]
    fn strip_html_special_characters() {
        assert_eq!(
            EmailChannel::strip_html("<span>&lt;tag&gt;</span>"),
            "&lt;tag&gt;"
        );
    }

    // Default function tests

    #[test]
    fn default_imap_port_returns_993() {
        assert_eq!(default_imap_port(), 993);
    }

    #[test]
    fn default_smtp_port_returns_465() {
        assert_eq!(default_smtp_port(), 465);
    }

    #[test]
    fn default_imap_folder_returns_inbox() {
        assert_eq!(default_imap_folder(), "INBOX");
    }

    #[test]
    fn default_true_returns_true() {
        assert!(default_true());
    }

    // EmailConfig serialization tests

    #[test]
    fn email_config_serialize_deserialize() {
        let config = EmailConfig {
            enabled: true,
            imap_host: "imap.example.com".to_string(),
            imap_port: 993,
            imap_folder: "INBOX".to_string(),
            smtp_host: "smtp.example.com".to_string(),
            smtp_port: 587,
            smtp_tls: true,
            username: "user@example.com".to_string(),
            password: "password123".to_string(),
            smtp_username: None,
            smtp_password: None,
            from_address: "bot@example.com".to_string(),
            idle_timeout_secs: 1740,
            poll_interval_secs: 60,
            default_subject: "Serialization Test".to_string(),
            max_attachment_bytes: default_max_attachment_bytes(),
            excluded_tools: vec![],
            html_body: true,
            oauth2: None,
            observer_mode: false,
        };

        let json = serde_json::to_string(&config).unwrap();
        let deserialized: EmailConfig = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.imap_host, config.imap_host);
        assert_eq!(deserialized.smtp_port, config.smtp_port);
        assert_eq!(deserialized.default_subject, config.default_subject);
    }

    #[test]
    fn email_config_deserialize_with_defaults() {
        let json = r#"{
            "imap_host": "imap.test.com",
            "smtp_host": "smtp.test.com",
            "username": "user",
            "password": "pass",
            "from_address": "bot@test.com"
        }"#;

        let config: EmailConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.imap_port, 993); // default
        assert_eq!(config.smtp_port, 465); // default
        assert!(config.smtp_tls); // default
        assert_eq!(config.idle_timeout_secs, 1740); // default
        assert_eq!(config.default_subject, "Re: Message"); // default
    }

    #[test]
    fn idle_timeout_deserializes_explicit_value() {
        let json = r#"{
            "imap_host": "imap.test.com",
            "smtp_host": "smtp.test.com",
            "username": "user",
            "password": "pass",
            "from_address": "bot@test.com",
            "idle_timeout_secs": 900
        }"#;
        let config: EmailConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.idle_timeout_secs, 900);
    }

    #[test]
    fn poll_interval_deserializes_as_independent_field() {
        // poll_interval_secs is a separate field from idle_timeout_secs —
        // used when the IMAP server does not advertise the IDLE capability.
        // Previously (pre-polling-fallback) it was a misleading serde alias
        // for idle_timeout_secs; that coupling has been removed.
        let json = r#"{
            "imap_host": "imap.test.com",
            "smtp_host": "smtp.test.com",
            "username": "user",
            "password": "pass",
            "from_address": "bot@test.com",
            "poll_interval_secs": 120
        }"#;
        let config: EmailConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.poll_interval_secs, 120);
        assert_eq!(config.idle_timeout_secs, 1740); // unchanged default
    }

    #[test]
    fn poll_interval_has_default_when_unset() {
        let json = r#"{
            "imap_host": "imap.test.com",
            "smtp_host": "smtp.test.com",
            "username": "user",
            "password": "pass",
            "from_address": "bot@test.com"
        }"#;
        let config: EmailConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.poll_interval_secs, 60);
    }

    #[test]
    fn idle_timeout_propagates_to_channel() {
        let config = EmailConfig {
            enabled: true,
            idle_timeout_secs: 600,
            ..Default::default()
        };
        let channel = EmailChannel::new(config, "email_test_alias", empty_resolver());
        assert_eq!(channel.config.idle_timeout_secs, 600);
    }

    #[test]
    fn email_config_debug_output() {
        let config = EmailConfig {
            enabled: true,
            imap_host: "imap.debug.com".to_string(),
            ..Default::default()
        };
        let debug_str = format!("{:?}", config);
        assert!(debug_str.contains("imap.debug.com"));
    }

    #[test]
    fn email_config_smtp_credentials_default_to_none() {
        let config = EmailConfig::default();
        assert!(config.smtp_username.is_none());
        assert!(config.smtp_password.is_none());
    }

    #[test]
    fn smtp_credentials_fallback_to_shared() {
        let config = EmailConfig {
            username: "shared@example.com".to_string(),
            password: "shared_pass".to_string(),
            smtp_username: None,
            smtp_password: None,
            ..Default::default()
        };
        let channel = EmailChannel::new(config, "email_test_alias", empty_resolver());
        let creds = channel.smtp_credentials();
        // Credentials doesn't expose fields directly, so round-trip via a
        // fresh construction for comparison
        let expected =
            Credentials::new("shared@example.com".to_string(), "shared_pass".to_string());
        assert_eq!(creds, expected);
    }

    #[test]
    fn smtp_credentials_uses_dedicated_fields() {
        let config = EmailConfig {
            username: "shared@example.com".to_string(),
            password: "shared_pass".to_string(),
            smtp_username: Some("smtp@example.com".to_string()),
            smtp_password: Some("smtp_pass".to_string()),
            ..Default::default()
        };
        let channel = EmailChannel::new(config, "email_test_alias", empty_resolver());
        let creds = channel.smtp_credentials();
        let expected = Credentials::new("smtp@example.com".to_string(), "smtp_pass".to_string());
        assert_eq!(creds, expected);
    }

    #[test]
    fn smtp_credentials_ignore_blank_dedicated_fields() {
        let config = EmailConfig {
            username: "shared@example.com".to_string(),
            password: "shared_pass".to_string(),
            smtp_username: Some("   ".to_string()),
            smtp_password: Some("".to_string()),
            ..Default::default()
        };
        let channel = EmailChannel::new(config, "email_test_alias", empty_resolver());
        let creds = channel.smtp_credentials();
        let expected =
            Credentials::new("shared@example.com".to_string(), "shared_pass".to_string());
        assert_eq!(creds, expected);
    }

    #[test]
    fn smtp_credentials_preserve_nonblank_dedicated_fields() {
        let config = EmailConfig {
            username: "shared@example.com".to_string(),
            password: "shared_pass".to_string(),
            smtp_username: Some("  smtp@example.com  ".to_string()),
            smtp_password: Some("  smtp_pass  ".to_string()),
            ..Default::default()
        };
        let channel = EmailChannel::new(config, "email_test_alias", empty_resolver());
        let creds = channel.smtp_credentials();
        let expected = Credentials::new(
            "  smtp@example.com  ".to_string(),
            "  smtp_pass  ".to_string(),
        );
        assert_eq!(creds, expected);
    }
}
