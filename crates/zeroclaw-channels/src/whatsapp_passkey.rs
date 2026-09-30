//! File-brokered operator handoff for WhatsApp Web's SHORTCAKE passkey
//! companion-linking gate.
//!
//! WhatsApp began requiring a WebAuthn assertion during device linking in a
//! server-side rollout from 2026-06-30. The assertion must be signed by a
//! passkey already registered to the account, and its private key normally
//! lives non-extractable in a platform authenticator, so no process running
//! on the host can produce one on its own. `whatsapp-rust` abstracts exactly
//! that one step behind [`PasskeyAuthenticator`] and ships no default.
//!
//! This module supplies that authenticator and the fresh-link verification
//! handoff. Both use files next to the session database the channel owns:
//!
//! 1. When the server demands an assertion, the request options are written
//!    to `<session>.passkey-request.json`.
//! 2. The operator performs the ceremony where a real authenticator lives —
//!    the practical route is a logged-in `web.whatsapp.com` tab, because a
//!    browser only signs for an rpId matching the page origin — and saves the
//!    resulting credential JSON to `<session>.passkey-assertion.json`.
//! 3. This authenticator picks it up and hands it back to the library, which
//!    resumes the protocol.
//! 4. A fresh link publishes WhatsApp's verification code and a one-shot
//!    attempt id to `<session>.passkey-confirmation.json`.
//! 5. After matching the code on the primary phone, the operator writes that
//!    attempt id to `<session>.passkey-confirmed.json`; only then does ZeroClaw
//!    send the final confirmation. Re-links prove continuity and upstream
//!    auto-confirms them without this second handoff.
//!
//! The file is deliberately the interface rather than an HTTP handler. The
//! ceremony is rare, operator-driven and one-shot, and the live client is
//! reachable only from inside the channel's own run loop — the gateway's
//! `login_relink` hook is disk-only by design and never touches a running
//! channel. Keeping the seam on disk avoids inventing cross-crate plumbing
//! for a rare event, and leaves an HTTP endpoint a trivial future addition:
//! it would write the same file.
//!
//! The assertion is a one-time signature over the server's challenge and
//! carries no private key. The acknowledgement carries only a random attempt
//! id bound to the current code prompt. Neither file is reusable against a
//! different ceremony.
//!
//! The request and confirmation prompt are published atomically (temp file plus
//! rename) and, on Unix, created owner-only. Both matter because the reader is
//! a poller rather than a process that knows when the write finished: without
//! the rename it could read a truncated document, and without the mode the
//! challenge would sit at the prevailing umask. The assertion and
//! acknowledgement are written by whoever runs the ceremony, so their
//! permissions are theirs to set. Assertions are consumed only after they parse
//! completely and match the current challenge; an in-progress or superseded
//! response remains available to be completed or replaced until the deadline.

#![cfg(feature = "whatsapp-web")]

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use base64::prelude::*;
use tokio_util::sync::CancellationToken;
use whatsapp_rust::passkey::{Assertion, AssertionRequest, PasskeyAuthenticator, PasskeyError};

/// How long to wait for each operator-mediated ceremony step.
///
/// Generous on purpose: a human has to run a browser ceremony, and the
/// server re-issues its request if this attempt lapses. The server's own
/// `timeout_ms` is deliberately NOT used as the ceiling — it is typically
/// around a minute, which is shorter than the manual step realistically
/// takes, and giving up early would just burn the attempt.
const DEFAULT_WAIT: Duration = Duration::from_secs(300);

/// How often to look for the assertion file while waiting.
const POLL_INTERVAL: Duration = Duration::from_secs(1);

/// Path the request options are written to, derived from the session path.
#[must_use]
pub fn request_path(session_path: &str) -> String {
    format!("{session_path}.passkey-request.json")
}

/// Path the operator drops the signed assertion at.
#[must_use]
pub fn assertion_path(session_path: &str) -> String {
    format!("{session_path}.passkey-assertion.json")
}

/// Path carrying the verification code for a fresh link.
#[must_use]
pub fn confirmation_path(session_path: &str) -> String {
    format!("{session_path}.passkey-confirmation.json")
}

/// Path where the operator acknowledges the current verification code.
#[must_use]
pub fn confirmation_ack_path(session_path: &str) -> String {
    format!("{session_path}.passkey-confirmed.json")
}

/// All passkey artifacts, including interrupted atomic publications.
/// Session invalidation derives its cleanup list from the broker's path helpers.
pub(crate) fn artifact_paths(session_path: &str) -> [String; 6] {
    [
        request_path(session_path),
        assertion_path(session_path),
        confirmation_path(session_path),
        confirmation_ack_path(session_path),
        publication_path(&request_path(session_path)),
        publication_path(&confirmation_path(session_path)),
    ]
}

fn publication_path(path: &str) -> String {
    format!("{path}.tmp")
}

async fn remove_if_present(path: &str) -> std::io::Result<()> {
    match tokio::fs::remove_file(path).await {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

/// The fresh-link verification state published for the operator.
///
/// `attempt_id` binds the acknowledgement to this exact ceremony. The display
/// code is short and can repeat, so it is not sufficient as a replay guard.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct PendingPasskeyConfirmation {
    pub attempt_id: String,
    pub code: String,
}

#[derive(serde::Deserialize)]
struct PasskeyConfirmationAck {
    attempt_id: String,
}

/// Publish `contents` at `path` atomically, and owner-only where the platform
/// supports it.
///
/// Both properties matter because the file is picked up by a poller — the
/// operator, or the gateway hook in the follow-up — rather than read once by a
/// process that knows when the write finished:
///
/// * **Atomic.** A plain write leaves a window where a reader sees a truncated
///   JSON document. Writing to a sibling temp file and renaming is atomic
///   within a directory, so a reader observes either the previous contents or
///   the complete new ones, never a partial document. Tokio delegates to
///   `std::fs::rename`, which replaces an existing file on Windows too (using
///   `MoveFileExW` with `MOVEFILE_REPLACE_EXISTING`). Do not unlink the request
///   first: that would introduce a gap in which no request is visible.
/// * **Owner-only.** The mode is applied at creation rather than after the
///   write, so the file is never briefly readable at the prevailing umask. The
///   temp file is created with `create_new`, so a pre-existing path (a stale
///   temp, or something planted) is an error rather than something to clobber
///   or follow.
async fn publish_private(path: &str, contents: &[u8]) -> std::io::Result<()> {
    use tokio::io::AsyncWriteExt as _;

    let tmp = publication_path(path);
    // A previous run that died between create and rename would otherwise make
    // `create_new` fail forever.
    let _ = tokio::fs::remove_file(&tmp).await;

    let mut options = tokio::fs::OpenOptions::new();
    options.write(true).create_new(true);
    // tokio's OpenOptions exposes `mode` inherently on Unix, so no extension
    // trait is needed. Set at creation, not after: a later `set_permissions`
    // would leave a window where the challenge is readable at the umask.
    #[cfg(unix)]
    options.mode(0o600);

    let mut file = options.open(&tmp).await?;
    file.write_all(contents).await?;
    // Flush before the rename so the published name never points at a file
    // whose contents have not reached disk.
    file.sync_all().await?;
    drop(file);

    tokio::fs::rename(&tmp, path).await
}

/// Read the pending verification-code prompt for `session_path`, if any.
///
/// The prompt is published atomically, so a parse error is a real corrupt or
/// incompatible file rather than an in-progress write.
pub fn pending_confirmation(
    session_path: &str,
) -> std::io::Result<Option<PendingPasskeyConfirmation>> {
    match std::fs::read(confirmation_path(session_path)) {
        Ok(bytes) => serde_json::from_slice(&bytes).map(Some).map_err(|e| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("invalid passkey confirmation state: {e}"),
            )
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// File-backed operator acknowledgement shared by every event in one client session.
pub struct FilePasskeyConfirmation {
    session_path: String,
    wait: Duration,
    closed: CancellationToken,
    // This is the authority for the current attempt, not a copy of client state.
    // The mutex covers only publication, acceptance, and cleanup; never a human
    // wait or the network confirmation.
    current: tokio::sync::Mutex<Option<Arc<ConfirmationAttempt>>>,
}

struct ConfirmationAttempt {
    id: String,
    cancelled: CancellationToken,
    // Invalidation first cancels, then drains this operation. A replacement
    // cannot reach upstream while an old confirmation future is still polling.
    in_flight: tokio::sync::Mutex<()>,
}

impl ConfirmationAttempt {
    async fn cancel_and_drain(&self) {
        self.cancelled.cancel();
        let _drained = self.in_flight.lock().await;
    }
}

/// One-shot authority to confirm the acknowledged ceremony. Replacement and
/// session shutdown revoke this even after the acknowledgement was consumed.
pub struct PasskeyConfirmationPermit {
    attempt: Arc<ConfirmationAttempt>,
}

impl PasskeyConfirmationPermit {
    pub async fn confirm<F, Fut>(self, confirm: F) -> Result<(), PasskeyError>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<(), PasskeyError>>,
    {
        let _in_flight = self.attempt.in_flight.lock().await;
        tokio::select! {
            biased;
            _ = self.attempt.cancelled.cancelled() => Err(PasskeyError::Cancelled),
            // Keep construction inside the selected future: a revoked permit
            // must not invoke the callback, even if it has synchronous effects.
            result = async { confirm().await } => result,
        }
    }
}

impl FilePasskeyConfirmation {
    #[must_use]
    pub fn new(session_path: impl Into<String>) -> Self {
        Self {
            session_path: session_path.into(),
            wait: DEFAULT_WAIT,
            closed: CancellationToken::new(),
            current: tokio::sync::Mutex::new(None),
        }
    }

    #[must_use]
    pub fn with_cancellation(mut self, closed: CancellationToken) -> Self {
        self.closed = closed;
        self
    }

    /// Override the operator wait. Tests use a short deadline.
    #[must_use]
    pub fn with_wait(mut self, wait: Duration) -> Self {
        self.wait = wait;
        self
    }

    /// Revoke outstanding permits when the protocol advances. Late callbacks
    /// from a closed client must not touch a replacement client's files.
    pub async fn invalidate(&self) -> std::io::Result<()> {
        let mut current = self.current.lock().await;
        if self.closed.is_cancelled() {
            return Ok(());
        }
        self.clear_current(&mut current).await
    }

    pub async fn shutdown(&self) -> std::io::Result<()> {
        self.closed.cancel();
        let mut current = self.current.lock().await;
        self.clear_current(&mut current).await
    }

    async fn clear_current(
        &self,
        current: &mut Option<Arc<ConfirmationAttempt>>,
    ) -> std::io::Result<()> {
        if let Some(attempt) = current.take() {
            attempt.cancel_and_drain().await;
        }
        self.remove_artifacts().await
    }

    async fn remove_artifacts(&self) -> std::io::Result<()> {
        let prompt = confirmation_path(&self.session_path);
        for path in [
            confirmation_ack_path(&self.session_path),
            publication_path(&prompt),
            prompt,
        ] {
            remove_if_present(&path).await?;
        }
        Ok(())
    }

    /// Publish `code`, accepting only an acknowledgement of the current prompt.
    pub async fn wait_for_acknowledgement(
        &self,
        code: &str,
    ) -> Result<PasskeyConfirmationPermit, PasskeyError> {
        let prompt_file = confirmation_path(&self.session_path);
        let ack_file = confirmation_ack_path(&self.session_path);
        let attempt_id = uuid::Uuid::new_v4().to_string();
        let prompt = PendingPasskeyConfirmation {
            attempt_id: attempt_id.clone(),
            code: code.to_string(),
        };
        let prompt_bytes =
            serde_json::to_vec(&prompt).map_err(|e| PasskeyError::Backend(e.to_string()))?;
        let attempt = Arc::new(ConfirmationAttempt {
            id: attempt_id.clone(),
            cancelled: self.closed.child_token(),
            in_flight: tokio::sync::Mutex::new(()),
        });
        let cancelled = &attempt.cancelled;

        {
            let mut current = self.current.lock().await;
            if self.closed.is_cancelled() {
                return Err(PasskeyError::Cancelled);
            }
            if let Some(previous) = current.take() {
                previous.cancel_and_drain().await;
            }
            self.remove_artifacts()
                .await
                .map_err(|e| PasskeyError::Backend(e.to_string()))?;
            *current = Some(Arc::clone(&attempt));
            if let Err(error) = publish_private(&prompt_file, &prompt_bytes).await {
                cancelled.cancel();
                return Err(PasskeyError::Backend(format!(
                    "could not write {prompt_file}: {error}"
                )));
            }
        }

        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
            &format!(
                "WhatsApp requires confirmation of the passkey verification code. \
                 Prompt written to {prompt_file}. After matching the code on the \
                 primary phone, write JSON containing its attempt_id to {ack_file} \
                 within {}s.",
                self.wait.as_secs()
            )
        );

        let deadline = tokio::time::Instant::now() + self.wait;
        loop {
            {
                let current = self.current.lock().await;
                if cancelled.is_cancelled()
                    || current
                        .as_ref()
                        .is_none_or(|attempt| attempt.id != attempt_id)
                {
                    return Err(PasskeyError::Cancelled);
                }
                let owns_prompt = pending_confirmation(&self.session_path)
                    .map_err(|e| PasskeyError::Backend(e.to_string()))?
                    .is_some_and(|prompt| prompt.attempt_id == attempt_id);
                if !owns_prompt {
                    // A disk-only relink can remove the prompt while this client
                    // still runs. Its acknowledgement must then fail closed.
                    cancelled.cancel();
                    return Err(PasskeyError::Cancelled);
                }
                let acknowledged = match tokio::fs::read(&ack_file).await {
                    Ok(bytes) => serde_json::from_slice::<PasskeyConfirmationAck>(&bytes)
                        .is_ok_and(|ack| ack.attempt_id == attempt_id),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
                    Err(error) => return Err(PasskeyError::Backend(error.to_string())),
                };
                if acknowledged {
                    self.remove_artifacts()
                        .await
                        .map_err(|e| PasskeyError::Backend(e.to_string()))?;
                    return Ok(PasskeyConfirmationPermit { attempt });
                }
                if tokio::time::Instant::now() >= deadline {
                    cancelled.cancel();
                    self.remove_artifacts()
                        .await
                        .map_err(|e| PasskeyError::Backend(e.to_string()))?;
                    return Err(PasskeyError::Cancelled);
                }
            }
            tokio::select! {
                biased;
                _ = cancelled.cancelled() => return Err(PasskeyError::Cancelled),
                _ = tokio::time::sleep_until(std::cmp::min(deadline, tokio::time::Instant::now() + POLL_INTERVAL)) => {},
            }
        }
    }
}

/// A [`PasskeyAuthenticator`] that brokers the ceremony through two files
/// beside the session database.
pub struct FilePasskeyAuthenticator {
    session_path: String,
    wait: Duration,
    generation: AtomicU64,
    closed: CancellationToken,
    file_ops: tokio::sync::Mutex<()>,
    confirmation: Option<Arc<FilePasskeyConfirmation>>,
}

impl FilePasskeyAuthenticator {
    #[must_use]
    pub fn new(session_path: impl Into<String>) -> Self {
        Self {
            session_path: session_path.into(),
            wait: DEFAULT_WAIT,
            generation: AtomicU64::new(0),
            closed: CancellationToken::new(),
            file_ops: tokio::sync::Mutex::new(()),
            confirmation: None,
        }
    }

    /// Override how long to wait for the operator. Used by tests to keep the
    /// timeout path fast.
    #[must_use]
    pub fn with_wait(mut self, wait: Duration) -> Self {
        self.wait = wait;
        self
    }

    #[must_use]
    pub fn with_cancellation(mut self, closed: CancellationToken) -> Self {
        self.closed = closed;
        self
    }

    /// Tie new assertions to the session's confirmation authority. Upstream can
    /// open a replacement protocol session only after this authenticator returns,
    /// so revoking here fences an old confirmation before that replacement exists.
    #[must_use]
    pub fn with_confirmation(mut self, confirmation: Arc<FilePasskeyConfirmation>) -> Self {
        self.confirmation = Some(confirmation);
        self
    }

    /// Drain pending publication/consumption before the next client starts.
    pub async fn shutdown(&self) -> std::io::Result<()> {
        self.closed.cancel();
        let _file_guard = self.file_ops.lock().await;
        self.begin_attempt();
        for path in [
            request_path(&self.session_path),
            assertion_path(&self.session_path),
            publication_path(&request_path(&self.session_path)),
        ] {
            remove_if_present(&path).await?;
        }
        Ok(())
    }

    fn begin_attempt(&self) -> u64 {
        self.generation
            .fetch_add(1, Ordering::AcqRel)
            .wrapping_add(1)
    }

    fn owns_attempt(&self, generation: u64) -> bool {
        !self.closed.is_cancelled() && self.generation.load(Ordering::Acquire) == generation
    }

    async fn cleanup_attempt(&self, generation: u64, request_file: &str, assertion_file: &str) {
        let _file_guard = self.file_ops.lock().await;
        if self.owns_attempt(generation) {
            let _ = tokio::fs::remove_file(assertion_file).await;
            let _ = tokio::fs::remove_file(request_file).await;
        }
    }
}

/// Build an [`Assertion`] from the credential JSON a WebAuthn `get()` returns.
///
/// The operator saves exactly what the browser produced, so the credential id
/// is recovered from the JSON itself rather than asked for separately — one
/// less field to transcribe wrongly. `rawId` is preferred over `id` because
/// both carry the same base64url value and `rawId` is the canonical binary
/// form; either is accepted since hand-assembled payloads often omit one.
pub fn parse_assertion(bytes: Vec<u8>) -> Result<Assertion, PasskeyError> {
    let value: serde_json::Value = serde_json::from_slice(&bytes)
        .map_err(|e| PasskeyError::InvalidOptions(format!("assertion is not valid JSON: {e}")))?;

    let raw_id = value
        .get("rawId")
        .or_else(|| value.get("id"))
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            PasskeyError::InvalidOptions(
                "assertion is missing a string `rawId` (or `id`) credential identifier".into(),
            )
        })?;

    let credential_id = BASE64_URL_SAFE_NO_PAD
        .decode(raw_id.trim_end_matches('='))
        .map_err(|e| PasskeyError::InvalidOptions(format!("rawId is not base64url: {e}")))?;

    if credential_id.is_empty() {
        return Err(PasskeyError::InvalidOptions(
            "assertion rawId decoded to zero bytes, which is never a credential id".into(),
        ));
    }

    // A response block without a signature cannot satisfy the server, and
    // failing here names the problem instead of surfacing an opaque rejection
    // from WhatsApp minutes later.
    if value
        .get("response")
        .and_then(|r| r.get("signature"))
        .and_then(|s| s.as_str())
        .is_none_or(str::is_empty)
    {
        return Err(PasskeyError::InvalidOptions(
            "assertion is missing `response.signature`; save the full credential JSON returned by navigator.credentials.get()".into(),
        ));
    }

    Ok(Assertion {
        assertion_json: bytes,
        credential_id,
    })
}

fn parse_assertion_for_request(
    bytes: Vec<u8>,
    request: &AssertionRequest,
) -> Result<Assertion, PasskeyError> {
    let assertion = parse_assertion(bytes)?;
    let value: serde_json::Value = serde_json::from_slice(&assertion.assertion_json)
        .map_err(|e| PasskeyError::InvalidOptions(format!("assertion is not valid JSON: {e}")))?;
    let client_data = value
        .get("response")
        .and_then(|response| response.get("clientDataJSON"))
        .and_then(|client_data| client_data.as_str())
        .ok_or_else(|| {
            PasskeyError::InvalidOptions(
                "assertion is missing `response.clientDataJSON`; save the full credential JSON returned by navigator.credentials.get()".into(),
            )
        })?;
    let client_data = BASE64_URL_SAFE_NO_PAD
        .decode(client_data.trim_end_matches('='))
        .map_err(|e| {
            PasskeyError::InvalidOptions(format!("clientDataJSON is not base64url: {e}"))
        })?;
    let client_data: serde_json::Value = serde_json::from_slice(&client_data).map_err(|e| {
        PasskeyError::InvalidOptions(format!("clientDataJSON is not valid JSON: {e}"))
    })?;
    let challenge = client_data
        .get("challenge")
        .and_then(|challenge| challenge.as_str())
        .ok_or_else(|| {
            PasskeyError::InvalidOptions("clientDataJSON is missing the request challenge".into())
        })?;
    let challenge = BASE64_URL_SAFE_NO_PAD
        .decode(challenge.trim_end_matches('='))
        .map_err(|e| {
            PasskeyError::InvalidOptions(format!("clientDataJSON challenge is not base64url: {e}"))
        })?;
    if challenge != request.challenge {
        return Err(PasskeyError::InvalidOptions(
            "assertion challenge does not match the current passkey request".into(),
        ));
    }
    Ok(assertion)
}

#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
impl PasskeyAuthenticator for FilePasskeyAuthenticator {
    async fn get_assertion(&self, request: &AssertionRequest) -> Result<Assertion, PasskeyError> {
        if let Some(confirmation) = &self.confirmation {
            confirmation
                .invalidate()
                .await
                .map_err(|e| PasskeyError::Backend(e.to_string()))?;
        }
        let request_file = request_path(&self.session_path);
        let assertion_file = assertion_path(&self.session_path);
        let generation;

        {
            // The lock covers only short file mutations. It never spans the
            // operator wait, so a reissued request supersedes the old one
            // instead of queuing behind its five-minute deadline.
            let _file_guard = self.file_ops.lock().await;
            if self.closed.is_cancelled() {
                return Err(PasskeyError::Cancelled);
            }
            generation = self.begin_attempt();

            // Clear any assertion left by a previous attempt before
            // advertising a new challenge. A stale file answers the wrong
            // challenge, so the server would reject it and burn this attempt.
            let _ = tokio::fs::remove_file(&assertion_file).await;
            if let Err(error) =
                publish_private(&request_file, request.raw_options_json.as_bytes()).await
            {
                let _ = tokio::fs::remove_file(&request_file).await;
                let _ = tokio::fs::remove_file(format!("{request_file}.tmp")).await;
                return Err(PasskeyError::Backend(format!(
                    "could not write {request_file}: {error}"
                )));
            }
        }

        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
            &format!(
                "WhatsApp requires a passkey assertion to link this device. \
                 Request options written to {request_file}. Run the WebAuthn \
                 ceremony in a logged-in web.whatsapp.com tab and save the \
                 resulting credential JSON to {assertion_file} within {}s.",
                self.wait.as_secs()
            )
        );

        let deadline = tokio::time::Instant::now() + self.wait;
        let mut last_invalid = None;
        loop {
            {
                let _file_guard = self.file_ops.lock().await;
                if !self.owns_attempt(generation) {
                    return Err(PasskeyError::Cancelled);
                }

                match tokio::fs::read(&assertion_file).await {
                    Ok(bytes) if !bytes.is_empty() => {
                        match parse_assertion_for_request(bytes, request) {
                            Ok(assertion) => {
                                let _ = tokio::fs::remove_file(&assertion_file).await;
                                let _ = tokio::fs::remove_file(&request_file).await;
                                ::zeroclaw_log::record!(
                                    INFO,
                                    ::zeroclaw_log::Event::new(
                                        module_path!(),
                                        ::zeroclaw_log::Action::Note
                                    ),
                                    "WhatsApp passkey assertion accepted; resuming device linking"
                                );
                                return Ok(assertion);
                            }
                            Err(error) => {
                                // A manual writer may still be appending JSON,
                                // or this may be a late response to the request
                                // this generation replaced. Leave it available
                                // to be completed or overwritten until the
                                // deadline rather than destroying the response.
                                last_invalid = Some(error);
                            }
                        }
                    }
                    // Absent or empty: keep waiting.
                    _ => {}
                }
            }

            if tokio::time::Instant::now() >= deadline {
                self.cleanup_attempt(generation, &request_file, &assertion_file)
                    .await;
                return Err(last_invalid.unwrap_or(PasskeyError::Cancelled));
            }

            tokio::select! {
                biased;
                _ = self.closed.cancelled() => return Err(PasskeyError::Cancelled),
                _ = tokio::time::sleep(POLL_INTERVAL) => {},
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assertion_request(challenge: &[u8]) -> AssertionRequest {
        AssertionRequest {
            challenge: challenge.to_vec(),
            rp_id: Some("web.whatsapp.com".into()),
            allow_credentials: vec![],
            user_verification: whatsapp_rust::passkey::UserVerification::Preferred,
            timeout_ms: Some(60_000),
            raw_options_json: serde_json::json!({
                "challenge": BASE64_URL_SAFE_NO_PAD.encode(challenge),
            })
            .to_string(),
        }
    }

    async fn wait_for_contents(path: &str, expected: &str) {
        for _ in 0..100 {
            if tokio::fs::read_to_string(path)
                .await
                .is_ok_and(|contents| contents == expected)
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("{path} never contained the expected passkey request");
    }

    fn credential_json(raw_id: &str, signature: &str) -> Vec<u8> {
        credential_json_for_challenge(raw_id, signature, &[1, 2, 3])
    }

    fn credential_json_for_challenge(raw_id: &str, signature: &str, challenge: &[u8]) -> Vec<u8> {
        let client_data = serde_json::json!({
            "type": "webauthn.get",
            "challenge": BASE64_URL_SAFE_NO_PAD.encode(challenge),
            "origin": "https://web.whatsapp.com",
        });
        let client_data = BASE64_URL_SAFE_NO_PAD.encode(serde_json::to_vec(&client_data).unwrap());
        serde_json::json!({
            "id": raw_id,
            "rawId": raw_id,
            "type": "public-key",
            "response": {
                "clientDataJSON": client_data,
                "authenticatorData": "YXV0aA",
                "signature": signature,
                "userHandle": null,
            }
        })
        .to_string()
        .into_bytes()
    }

    #[test]
    fn paths_sit_beside_the_session_database() {
        assert_eq!(
            request_path("/data/wa.db"),
            "/data/wa.db.passkey-request.json"
        );
        assert_eq!(
            assertion_path("/data/wa.db"),
            "/data/wa.db.passkey-assertion.json"
        );
        assert_eq!(
            confirmation_path("/data/wa.db"),
            "/data/wa.db.passkey-confirmation.json"
        );
        assert_eq!(
            confirmation_ack_path("/data/wa.db"),
            "/data/wa.db.passkey-confirmed.json"
        );
    }

    #[test]
    fn parses_a_browser_credential_and_recovers_the_id() {
        let raw_id = BASE64_URL_SAFE_NO_PAD.encode(b"credential-1");
        let bytes = credential_json(&raw_id, "c2ln");

        let assertion = parse_assertion(bytes.clone()).unwrap();
        assert_eq!(assertion.credential_id, b"credential-1".to_vec());
        assert_eq!(
            assertion.assertion_json, bytes,
            "the credential JSON must reach the server byte-for-byte"
        );
    }

    #[test]
    fn rejects_payloads_that_cannot_satisfy_the_server() {
        // Not JSON at all.
        assert!(parse_assertion(b"not json".to_vec()).is_err());

        // No credential identifier.
        let no_id = serde_json::json!({ "response": { "signature": "c2ln" } })
            .to_string()
            .into_bytes();
        assert!(parse_assertion(no_id).is_err());

        // Identifier present but not base64url.
        let bad_b64 = credential_json("not valid base64!!", "c2ln");
        assert!(parse_assertion(bad_b64).is_err());

        // An empty rawId decodes to zero bytes, which is never a credential.
        let empty_id = credential_json("", "c2ln");
        assert!(parse_assertion(empty_id).is_err());

        // Well-formed envelope with nothing to verify.
        let no_signature = credential_json(&BASE64_URL_SAFE_NO_PAD.encode(b"cred"), "");
        assert!(parse_assertion(no_signature).is_err());

        // A structurally valid response to an older challenge cannot answer
        // the request currently published for the operator.
        let request = assertion_request(&[1, 2, 3]);
        let stale =
            credential_json_for_challenge(&BASE64_URL_SAFE_NO_PAD.encode(b"cred"), "c2ln", &[9]);
        assert!(parse_assertion_for_request(stale, &request).is_err());
    }

    #[tokio::test]
    async fn waiting_publishes_the_request_and_consumes_the_assertion() {
        let tmp = tempfile::tempdir().unwrap();
        let session = tmp.path().join("session.db").to_string_lossy().to_string();

        let raw_id = BASE64_URL_SAFE_NO_PAD.encode(b"cred");
        let assertion_file = assertion_path(&session);
        let expected_request = request_path(&session);

        let auth = FilePasskeyAuthenticator::new(&session).with_wait(Duration::from_secs(10));
        let request = assertion_request(&[1, 2, 3]);

        // Drive the wait and the operator concurrently without spawning: the
        // authenticator only resolves once the second future drops the file,
        // which is exactly the interleaving being tested.
        let operator = async {
            for _ in 0..50 {
                if tokio::fs::try_exists(&expected_request)
                    .await
                    .unwrap_or(false)
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            let published = tokio::fs::read_to_string(&expected_request).await.unwrap();
            assert!(
                published.contains("challenge"),
                "the server's request options must be published verbatim"
            );
            tokio::fs::write(&assertion_file, credential_json(&raw_id, "c2ln"))
                .await
                .unwrap();
        };

        let (assertion, ()) = tokio::join!(auth.get_assertion(&request), operator);
        let assertion = assertion.unwrap();

        assert_eq!(assertion.credential_id, b"cred".to_vec());
        assert!(
            !tokio::fs::try_exists(&assertion_file).await.unwrap(),
            "a spent assertion must be consumed, not left to be replayed"
        );
        assert!(
            !tokio::fs::try_exists(&expected_request).await.unwrap(),
            "the request file must be cleaned up once answered"
        );
    }

    #[tokio::test]
    async fn reissued_request_supersedes_old_waiter_without_touching_new_state() {
        let tmp = tempfile::tempdir().unwrap();
        let session = tmp.path().join("session.db").to_string_lossy().to_string();
        let auth =
            Arc::new(FilePasskeyAuthenticator::new(&session).with_wait(Duration::from_secs(5)));
        let request_file = request_path(&session);
        let assertion_file = assertion_path(&session);
        let first_request = assertion_request(&[1]);
        let first_options = first_request.raw_options_json.clone();

        let first = {
            let auth = Arc::clone(&auth);
            zeroclaw_spawn::spawn!(async move { auth.get_assertion(&first_request).await })
        };
        wait_for_contents(&request_file, &first_options).await;

        let second_request = assertion_request(&[2]);
        let second_options = second_request.raw_options_json.clone();
        let second = {
            let auth = Arc::clone(&auth);
            zeroclaw_spawn::spawn!(async move { auth.get_assertion(&second_request).await })
        };
        wait_for_contents(&request_file, &second_options).await;

        let first_result = tokio::time::timeout(Duration::from_secs(2), first)
            .await
            .expect("superseded waiter should exit promptly")
            .unwrap();
        assert!(matches!(first_result, Err(PasskeyError::Cancelled)));
        assert_eq!(
            tokio::fs::read_to_string(&request_file).await.unwrap(),
            second_options,
            "the old waiter must not remove the replacement request"
        );
        assert!(
            !tokio::fs::try_exists(format!("{request_file}.tmp"))
                .await
                .unwrap()
        );

        // A late response to the superseded challenge remains available to be
        // replaced; it cannot be consumed as the current attempt's response.
        let raw_id = BASE64_URL_SAFE_NO_PAD.encode(b"current-credential");
        tokio::fs::write(
            &assertion_file,
            credential_json_for_challenge(&raw_id, "c2ln", &[1]),
        )
        .await
        .unwrap();
        tokio::time::sleep(POLL_INTERVAL + Duration::from_millis(200)).await;
        assert!(!second.is_finished());
        assert!(tokio::fs::try_exists(&assertion_file).await.unwrap());

        tokio::fs::write(
            &assertion_file,
            credential_json_for_challenge(&raw_id, "c2ln", &[2]),
        )
        .await
        .unwrap();
        let assertion = tokio::time::timeout(Duration::from_secs(2), second)
            .await
            .expect("current waiter should consume its matching assertion")
            .unwrap()
            .unwrap();
        assert_eq!(assertion.credential_id, b"current-credential".to_vec());
        assert!(!tokio::fs::try_exists(&request_file).await.unwrap());
        assert!(!tokio::fs::try_exists(&assertion_file).await.unwrap());
    }

    #[tokio::test]
    async fn interrupted_waiter_request_is_replaced_on_restart() {
        let tmp = tempfile::tempdir().unwrap();
        let session = tmp.path().join("session.db").to_string_lossy().into_owned();
        let request_file = request_path(&session);
        let assertion_file = assertion_path(&session);
        let auth = Arc::new(FilePasskeyAuthenticator::new(&session));
        let first_request = assertion_request(&[1]);
        let first_options = first_request.raw_options_json.clone();
        let first = {
            let auth = Arc::clone(&auth);
            zeroclaw_spawn::spawn!(async move { auth.get_assertion(&first_request).await })
        };
        wait_for_contents(&request_file, &first_options).await;
        first.abort();
        assert!(first.await.unwrap_err().is_cancelled());
        drop(auth);
        assert_eq!(
            tokio::fs::read_to_string(&request_file).await.unwrap(),
            first_options
        );

        // A new process can encounter both the last published request and an
        // incomplete publication. Neither may prevent the next handoff.
        tokio::fs::write(publication_path(&request_file), b"partial request")
            .await
            .unwrap();
        let request = assertion_request(&[2]);
        let options = request.raw_options_json.clone();
        let replacement = zeroclaw_spawn::spawn!({
            let session = session.clone();
            async move {
                FilePasskeyAuthenticator::new(session)
                    .with_wait(Duration::from_secs(5))
                    .get_assertion(&request)
                    .await
            }
        });
        wait_for_contents(&request_file, &options).await;
        assert!(
            !tokio::fs::try_exists(publication_path(&request_file))
                .await
                .unwrap()
        );

        let raw_id = BASE64_URL_SAFE_NO_PAD.encode(b"replacement-credential");
        tokio::fs::write(
            &assertion_file,
            credential_json_for_challenge(&raw_id, "c2ln", &[2]),
        )
        .await
        .unwrap();
        let assertion = tokio::time::timeout(Duration::from_secs(2), replacement)
            .await
            .expect("the restarted broker must accept its assertion")
            .unwrap()
            .unwrap();
        assert_eq!(assertion.credential_id, b"replacement-credential");
        assert!(!tokio::fs::try_exists(&request_file).await.unwrap());
        assert!(!tokio::fs::try_exists(&assertion_file).await.unwrap());
    }

    #[tokio::test]
    async fn partial_manual_assertion_is_not_consumed_before_write_finishes() {
        use tokio::io::AsyncWriteExt as _;

        let tmp = tempfile::tempdir().unwrap();
        let session = tmp.path().join("session.db").to_string_lossy().to_string();
        let auth =
            Arc::new(FilePasskeyAuthenticator::new(&session).with_wait(Duration::from_secs(5)));
        let request = assertion_request(&[1, 2, 3]);
        let expected_options = request.raw_options_json.clone();
        let request_file = request_path(&session);
        let assertion_file = assertion_path(&session);
        let waiter = {
            let auth = Arc::clone(&auth);
            zeroclaw_spawn::spawn!(async move { auth.get_assertion(&request).await })
        };
        wait_for_contents(&request_file, &expected_options).await;

        let raw_id = BASE64_URL_SAFE_NO_PAD.encode(b"credential");
        let assertion = credential_json(&raw_id, "c2ln");
        let split = assertion.len() / 2;
        let mut writer = tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&assertion_file)
            .await
            .unwrap();
        writer.write_all(&assertion[..split]).await.unwrap();
        writer.flush().await.unwrap();

        tokio::time::sleep(POLL_INTERVAL + Duration::from_millis(200)).await;
        assert!(!waiter.is_finished());
        assert!(tokio::fs::try_exists(&assertion_file).await.unwrap());

        writer.write_all(&assertion[split..]).await.unwrap();
        writer.sync_all().await.unwrap();
        drop(writer);

        let assertion = tokio::time::timeout(Duration::from_secs(2), waiter)
            .await
            .expect("waiter should accept the completed assertion")
            .unwrap()
            .unwrap();
        assert_eq!(assertion.credential_id, b"credential".to_vec());
        assert!(!tokio::fs::try_exists(&request_file).await.unwrap());
        assert!(!tokio::fs::try_exists(&assertion_file).await.unwrap());
    }

    #[tokio::test]
    async fn publishing_replaces_existing_request_with_private_complete_file() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir
            .path()
            .join("session.db.passkey-request.json")
            .to_string_lossy()
            .into_owned();

        tokio::fs::write(&target, br#"{"challenge":"old"}"#)
            .await
            .unwrap();

        publish_private(&target, br#"{"challenge":"AQID"}"#)
            .await
            .unwrap();

        assert_eq!(
            tokio::fs::read(&target).await.unwrap(),
            br#"{"challenge":"AQID"}"#.to_vec(),
            "the published file must carry the exact bytes"
        );
        assert!(
            !tokio::fs::try_exists(format!("{target}.tmp"))
                .await
                .unwrap(),
            "the rename must consume the temp file, not leave it beside the real one"
        );

        // The request is published for a poller to read, so it must never be
        // observable at the prevailing umask.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = tokio::fs::metadata(&target)
                .await
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600, "request file must be owner-only");
        }
    }

    #[tokio::test]
    async fn publishing_over_a_stale_temp_file_still_succeeds() {
        // A run that died between create and rename leaves a temp behind.
        // Without clearing it, `create_new` would fail on every later attempt
        // and the channel could never publish another challenge.
        let dir = tempfile::tempdir().unwrap();
        let target = dir
            .path()
            .join("session.db.passkey-request.json")
            .to_string_lossy()
            .into_owned();
        tokio::fs::write(format!("{target}.tmp"), b"leftover")
            .await
            .unwrap();

        publish_private(&target, b"fresh").await.unwrap();

        assert_eq!(tokio::fs::read(&target).await.unwrap(), b"fresh".to_vec());
    }

    async fn wait_for_confirmation(session: &str, code: &str) -> PendingPasskeyConfirmation {
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if let Some(prompt) = pending_confirmation(session).unwrap()
                    && prompt.code == code
                {
                    return prompt;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("confirmation prompt must be published")
    }

    async fn acknowledge(session: &str, prompt: &PendingPasskeyConfirmation) {
        tokio::fs::write(
            confirmation_ack_path(session),
            serde_json::json!({"attempt_id": prompt.attempt_id}).to_string(),
        )
        .await
        .unwrap();
    }

    fn confirmation_waiter(
        broker: &Arc<FilePasskeyConfirmation>,
        code: &'static str,
    ) -> tokio::task::JoinHandle<Result<PasskeyConfirmationPermit, PasskeyError>> {
        let broker = Arc::clone(broker);
        zeroclaw_spawn::spawn!(async move { broker.wait_for_acknowledgement(code).await })
    }

    #[tokio::test]
    async fn overlapping_confirmation_cannot_accept_old_ack_or_remove_replacement() {
        let tmp = tempfile::tempdir().unwrap();
        let session = tmp.path().join("session.db").to_string_lossy().into_owned();
        let broker =
            Arc::new(FilePasskeyConfirmation::new(&session).with_wait(Duration::from_secs(5)));
        let old = confirmation_waiter(&broker, "OLD-CODE");
        let old_prompt = wait_for_confirmation(&session, "OLD-CODE").await;
        let replacement = confirmation_waiter(&broker, "NEW-CODE");
        let new_prompt = wait_for_confirmation(&session, "NEW-CODE").await;
        acknowledge(&session, &old_prompt).await;
        let result = tokio::time::timeout(Duration::from_secs(2), old)
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(result, Err(PasskeyError::Cancelled)));
        assert_eq!(
            pending_confirmation(&session).unwrap(),
            Some(new_prompt.clone())
        );
        assert!(!replacement.is_finished());
        acknowledge(&session, &new_prompt).await;
        let permit = replacement.await.unwrap().unwrap();
        let mut confirmed = false;
        permit
            .confirm(|| {
                confirmed = true;
                async { Ok(()) }
            })
            .await
            .unwrap();
        assert!(confirmed);
        assert!(pending_confirmation(&session).unwrap().is_none());
    }

    #[tokio::test]
    async fn supersession_after_acknowledgement_revokes_final_confirmation_call() {
        let tmp = tempfile::tempdir().unwrap();
        let session = tmp.path().join("session.db").to_string_lossy().into_owned();
        let broker =
            Arc::new(FilePasskeyConfirmation::new(&session).with_wait(Duration::from_secs(5)));
        let old = confirmation_waiter(&broker, "OLD-CODE");
        let prompt = wait_for_confirmation(&session, "OLD-CODE").await;
        acknowledge(&session, &prompt).await;
        let old_permit = old.await.unwrap().unwrap();
        let replacement = confirmation_waiter(&broker, "NEW-CODE");
        let new_prompt = wait_for_confirmation(&session, "NEW-CODE").await;
        let called = std::cell::Cell::new(false);
        let result = old_permit
            .confirm(|| {
                called.set(true);
                async { Ok(()) }
            })
            .await;
        assert!(!called.get(), "a revoked permit must not call the client");
        assert!(matches!(result, Err(PasskeyError::Cancelled)));
        assert_eq!(pending_confirmation(&session).unwrap(), Some(new_prompt));
        broker.shutdown().await.unwrap();
        assert!(matches!(
            replacement.await.unwrap(),
            Err(PasskeyError::Cancelled)
        ));
    }

    #[tokio::test]
    async fn supersession_cancels_inflight_confirmation_without_blocking_new_prompt() {
        let tmp = tempfile::tempdir().unwrap();
        let session = tmp.path().join("session.db").to_string_lossy().into_owned();
        let broker =
            Arc::new(FilePasskeyConfirmation::new(&session).with_wait(Duration::from_secs(5)));
        let old = confirmation_waiter(&broker, "OLD-CODE");
        let prompt = wait_for_confirmation(&session, "OLD-CODE").await;
        acknowledge(&session, &prompt).await;
        let permit = old.await.unwrap().unwrap();
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (finish_tx, finish_rx) = tokio::sync::oneshot::channel::<()>();
        let confirming = zeroclaw_spawn::spawn!(async move {
            permit
                .confirm(|| async {
                    started_tx.send(()).unwrap();
                    finish_rx.await.unwrap();
                    panic!("supersession must drop the pending client operation");
                })
                .await
        });
        started_rx.await.unwrap();
        let replacement = confirmation_waiter(&broker, "NEW-CODE");
        wait_for_confirmation(&session, "NEW-CODE").await;
        assert!(matches!(
            confirming.await.unwrap(),
            Err(PasskeyError::Cancelled)
        ));
        assert!(finish_tx.send(()).is_err());
        broker.shutdown().await.unwrap();
        assert!(matches!(
            replacement.await.unwrap(),
            Err(PasskeyError::Cancelled)
        ));
    }

    #[tokio::test]
    async fn new_assertion_revokes_confirmation_before_upstream_can_replace_session() {
        let tmp = tempfile::tempdir().unwrap();
        let session = tmp.path().join("session.db").to_string_lossy().into_owned();
        let confirmation =
            Arc::new(FilePasskeyConfirmation::new(&session).with_wait(Duration::from_secs(5)));
        let old = confirmation_waiter(&confirmation, "OLD-CODE");
        let prompt = wait_for_confirmation(&session, "OLD-CODE").await;
        acknowledge(&session, &prompt).await;
        let old_permit = old.await.unwrap().unwrap();
        let auth = Arc::new(
            FilePasskeyAuthenticator::new(&session).with_confirmation(Arc::clone(&confirmation)),
        );
        let request = assertion_request(&[9]);
        let waiter = {
            let auth = Arc::clone(&auth);
            let request = request.clone();
            zeroclaw_spawn::spawn!(async move { auth.get_assertion(&request).await })
        };
        wait_for_contents(&request_path(&session), &request.raw_options_json).await;
        assert!(matches!(
            old_permit
                .confirm(|| async {
                    panic!("new assertion must revoke the old confirmation before upstream sees it")
                })
                .await,
            Err(PasskeyError::Cancelled)
        ));
        assert!(!waiter.is_finished());
        tokio::fs::write(
            assertion_path(&session),
            credential_json_for_challenge(&BASE64_URL_SAFE_NO_PAD.encode(b"current"), "c2ln", &[9]),
        )
        .await
        .unwrap();
        assert_eq!(waiter.await.unwrap().unwrap().credential_id, b"current");
    }

    #[tokio::test]
    async fn reconnect_fences_old_waiters_permits_and_late_callbacks() {
        let tmp = tempfile::tempdir().unwrap();
        let session = tmp.path().join("session.db").to_string_lossy().into_owned();
        let closed = CancellationToken::new();
        let guard = closed.clone().drop_guard();
        let old_broker = Arc::new(FilePasskeyConfirmation::new(&session).with_cancellation(closed));
        let old = confirmation_waiter(&old_broker, "OLD-CODE");
        let old_prompt = wait_for_confirmation(&session, "OLD-CODE").await;
        acknowledge(&session, &old_prompt).await;
        let permit = old.await.unwrap().unwrap();
        // Mirrors listener cancellation, including a dropped listen future.
        drop(guard);
        old_broker.shutdown().await.unwrap();
        let new_broker =
            Arc::new(FilePasskeyConfirmation::new(&session).with_wait(Duration::from_secs(5)));
        let replacement = confirmation_waiter(&new_broker, "NEW-CODE");
        let new_prompt = wait_for_confirmation(&session, "NEW-CODE").await;
        acknowledge(&session, &old_prompt).await;
        assert!(matches!(
            permit
                .confirm(|| async { panic!("closed client must never confirm") })
                .await,
            Err(PasskeyError::Cancelled)
        ));
        assert!(matches!(
            old_broker.wait_for_acknowledgement("LATE-CODE").await,
            Err(PasskeyError::Cancelled)
        ));
        old_broker.invalidate().await.unwrap();
        assert_eq!(
            pending_confirmation(&session).unwrap(),
            Some(new_prompt.clone())
        );
        assert!(!replacement.is_finished());
        acknowledge(&session, &new_prompt).await;
        replacement
            .await
            .unwrap()
            .unwrap()
            .confirm(|| async { Ok(()) })
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn shutdown_cancels_waiters_before_reconnecting_on_the_same_files() {
        let tmp = tempfile::tempdir().unwrap();
        let session = tmp.path().join("session.db").to_string_lossy().into_owned();
        let closed = CancellationToken::new();
        let auth =
            Arc::new(FilePasskeyAuthenticator::new(&session).with_cancellation(closed.clone()));
        let confirmation =
            Arc::new(FilePasskeyConfirmation::new(&session).with_cancellation(closed.clone()));
        let request = assertion_request(&[1]);
        let auth_waiter = {
            let auth = Arc::clone(&auth);
            let request = request.clone();
            zeroclaw_spawn::spawn!(async move { auth.get_assertion(&request).await })
        };
        wait_for_contents(&request_path(&session), &request.raw_options_json).await;
        let confirmation_waiter = confirmation_waiter(&confirmation, "OLD-CODE");
        wait_for_confirmation(&session, "OLD-CODE").await;
        closed.cancel();
        confirmation.shutdown().await.unwrap();
        auth.shutdown().await.unwrap();
        assert!(matches!(
            auth_waiter.await.unwrap(),
            Err(PasskeyError::Cancelled)
        ));
        assert!(matches!(
            confirmation_waiter.await.unwrap(),
            Err(PasskeyError::Cancelled)
        ));
        for path in artifact_paths(&session) {
            assert!(
                !std::path::Path::new(&path).exists(),
                "{path} survived shutdown"
            );
        }
        tokio::fs::write(request_path(&session), b"replacement request")
            .await
            .unwrap();
        assert!(matches!(
            auth.get_assertion(&request).await,
            Err(PasskeyError::Cancelled)
        ));
        assert_eq!(
            tokio::fs::read(request_path(&session)).await.unwrap(),
            b"replacement request"
        );
    }

    #[tokio::test]
    async fn removed_confirmation_prompt_cannot_be_acknowledged() {
        let tmp = tempfile::tempdir().unwrap();
        let session = tmp.path().join("session.db").to_string_lossy().into_owned();
        let broker =
            Arc::new(FilePasskeyConfirmation::new(&session).with_wait(Duration::from_secs(5)));
        let waiter = confirmation_waiter(&broker, "OLD-CODE");
        let prompt = wait_for_confirmation(&session, "OLD-CODE").await;
        tokio::fs::remove_file(confirmation_path(&session))
            .await
            .unwrap();
        acknowledge(&session, &prompt).await;
        assert!(matches!(
            waiter.await.unwrap(),
            Err(PasskeyError::Cancelled)
        ));
    }

    #[tokio::test]
    async fn fresh_confirmation_waits_for_a_matching_attempt_acknowledgement() {
        let tmp = tempfile::tempdir().unwrap();
        let session = tmp.path().join("session.db").to_string_lossy().to_string();
        let broker = FilePasskeyConfirmation::new(&session).with_wait(Duration::from_secs(3));
        let prompt_file = confirmation_path(&session);
        let ack_file = confirmation_ack_path(&session);

        let operator = async {
            for _ in 0..100 {
                if tokio::fs::try_exists(&prompt_file).await.unwrap_or(false) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            let prompt = pending_confirmation(&session)
                .unwrap()
                .expect("the confirmation prompt must be published");
            assert_eq!(prompt.code, "ABCD-EFGH");
            tokio::fs::write(
                &ack_file,
                serde_json::json!({ "attempt_id": prompt.attempt_id })
                    .to_string()
                    .as_bytes(),
            )
            .await
            .unwrap();
        };

        let (result, ()) = tokio::join!(broker.wait_for_acknowledgement("ABCD-EFGH"), operator);
        result.unwrap();
        assert!(!tokio::fs::try_exists(&prompt_file).await.unwrap());
        assert!(!tokio::fs::try_exists(&ack_file).await.unwrap());
    }

    #[tokio::test]
    async fn stale_acknowledgement_cannot_confirm_a_later_attempt() {
        let tmp = tempfile::tempdir().unwrap();
        let session = tmp.path().join("session.db").to_string_lossy().to_string();
        let broker = FilePasskeyConfirmation::new(&session).with_wait(Duration::from_millis(200));
        let prompt_file = confirmation_path(&session);
        let ack_file = confirmation_ack_path(&session);

        let operator = async {
            for _ in 0..50 {
                if tokio::fs::try_exists(&prompt_file).await.unwrap_or(false) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            tokio::fs::write(&ack_file, br#"{"attempt_id":"from-an-older-attempt"}"#)
                .await
                .unwrap();
        };

        let (result, ()) = tokio::join!(broker.wait_for_acknowledgement("ABCD-EFGH"), operator);
        assert!(matches!(result, Err(PasskeyError::Cancelled)));
        assert!(!tokio::fs::try_exists(&prompt_file).await.unwrap());
        assert!(!tokio::fs::try_exists(&ack_file).await.unwrap());
    }

    #[tokio::test]
    async fn a_stale_assertion_is_discarded_before_publishing_a_new_challenge() {
        let tmp = tempfile::tempdir().unwrap();
        let session = tmp.path().join("session.db").to_string_lossy().to_string();

        // Left over from a previous attempt: it answers a challenge that is
        // no longer live, so it must never be handed to the server.
        tokio::fs::write(
            assertion_path(&session),
            credential_json(&BASE64_URL_SAFE_NO_PAD.encode(b"stale"), "c2ln"),
        )
        .await
        .unwrap();

        let auth = FilePasskeyAuthenticator::new(&session).with_wait(Duration::from_millis(200));
        let request = AssertionRequest {
            challenge: vec![9],
            rp_id: None,
            allow_credentials: vec![],
            user_verification: whatsapp_rust::passkey::UserVerification::Preferred,
            timeout_ms: None,
            raw_options_json: r#"{"challenge":"CQ"}"#.into(),
        };

        let result = auth.get_assertion(&request).await;
        assert!(
            matches!(result, Err(PasskeyError::Cancelled)),
            "the stale file must be cleared and the wait must time out instead"
        );
        assert!(
            !tokio::fs::try_exists(&request_path(&session))
                .await
                .unwrap(),
            "a lapsed attempt must not leave its request file behind"
        );
    }
}
