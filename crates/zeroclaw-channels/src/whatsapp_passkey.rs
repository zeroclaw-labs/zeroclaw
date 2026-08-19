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
//! permissions are theirs to set; this module consumes and deletes them as soon
//! as they are read.

#![cfg(feature = "whatsapp-web")]

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use base64::prelude::*;
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
///   within a directory on POSIX, so a reader observes either the previous
///   contents or the complete new ones, never a partial document.
/// * **Owner-only.** The mode is applied at creation rather than after the
///   write, so the file is never briefly readable at the prevailing umask. The
///   temp file is created with `create_new`, so a pre-existing path (a stale
///   temp, or something planted) is an error rather than something to clobber
///   or follow.
async fn publish_private(path: &str, contents: &[u8]) -> std::io::Result<()> {
    use tokio::io::AsyncWriteExt as _;

    let tmp = format!("{path}.tmp");
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

/// File-backed operator acknowledgement for the fresh-link verification code.
pub struct FilePasskeyConfirmation {
    session_path: String,
    wait: Duration,
}

impl FilePasskeyConfirmation {
    #[must_use]
    pub fn new(session_path: impl Into<String>) -> Self {
        Self {
            session_path: session_path.into(),
            wait: DEFAULT_WAIT,
        }
    }

    /// Override the operator wait. Tests use a short deadline.
    #[must_use]
    pub fn with_wait(mut self, wait: Duration) -> Self {
        self.wait = wait;
        self
    }

    /// Publish `code` and wait for an acknowledgement bound to this attempt.
    pub async fn wait_for_acknowledgement(&self, code: &str) -> Result<(), PasskeyError> {
        let prompt_file = confirmation_path(&self.session_path);
        let ack_file = confirmation_ack_path(&self.session_path);
        let attempt_id = uuid::Uuid::new_v4().to_string();
        let prompt = PendingPasskeyConfirmation {
            attempt_id: attempt_id.clone(),
            code: code.to_string(),
        };
        let prompt_bytes = serde_json::to_vec(&prompt).map_err(|e| {
            PasskeyError::Backend(format!("could not serialize passkey confirmation: {e}"))
        })?;

        // Any acknowledgement predating this prompt belongs to an earlier
        // ceremony. Clear both names before publishing the new attempt.
        let _ = tokio::fs::remove_file(&ack_file).await;
        let _ = tokio::fs::remove_file(&prompt_file).await;
        publish_private(&prompt_file, &prompt_bytes)
            .await
            .map_err(|e| PasskeyError::Backend(format!("could not write {prompt_file}: {e}")))?;

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
            match tokio::fs::read(&ack_file).await {
                Ok(bytes)
                    if !bytes.is_empty()
                        && serde_json::from_slice::<PasskeyConfirmationAck>(&bytes)
                            .is_ok_and(|ack| ack.attempt_id == attempt_id) =>
                {
                    self.cleanup_attempt(&prompt_file, &ack_file, &attempt_id)
                        .await;
                    return Ok(());
                }
                _ => {}
            }

            if tokio::time::Instant::now() >= deadline {
                self.cleanup_attempt(&prompt_file, &ack_file, &attempt_id)
                    .await;
                return Err(PasskeyError::Cancelled);
            }

            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }

    /// Remove only the state owned by this attempt. A later ceremony may have
    /// replaced it while an older waiter was winding down.
    async fn cleanup_attempt(&self, prompt_file: &str, ack_file: &str, attempt_id: &str) {
        let owns_prompt = tokio::fs::read(prompt_file)
            .await
            .ok()
            .and_then(|bytes| serde_json::from_slice::<PendingPasskeyConfirmation>(&bytes).ok())
            .is_some_and(|prompt| prompt.attempt_id == attempt_id);
        if owns_prompt {
            let _ = tokio::fs::remove_file(prompt_file).await;
            let _ = tokio::fs::remove_file(ack_file).await;
        }
    }
}

/// A [`PasskeyAuthenticator`] that brokers the ceremony through two files
/// beside the session database.
pub struct FilePasskeyAuthenticator {
    session_path: String,
    wait: Duration,
}

impl FilePasskeyAuthenticator {
    #[must_use]
    pub fn new(session_path: impl Into<String>) -> Self {
        Self {
            session_path: session_path.into(),
            wait: DEFAULT_WAIT,
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
    pub fn into_arc(self) -> Arc<dyn PasskeyAuthenticator> {
        Arc::new(self)
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

#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
impl PasskeyAuthenticator for FilePasskeyAuthenticator {
    async fn get_assertion(&self, request: &AssertionRequest) -> Result<Assertion, PasskeyError> {
        let request_file = request_path(&self.session_path);
        let assertion_file = assertion_path(&self.session_path);

        // Clear any assertion left by a previous attempt before advertising a
        // new challenge. A stale file answers the WRONG challenge, so the
        // server would reject it and burn this attempt for no reason.
        let _ = tokio::fs::remove_file(&assertion_file).await;

        publish_private(&request_file, request.raw_options_json.as_bytes())
            .await
            .map_err(|e| PasskeyError::Backend(format!("could not write {request_file}: {e}")))?;

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
        loop {
            match tokio::fs::read(&assertion_file).await {
                Ok(bytes) if !bytes.is_empty() => {
                    // Consume it either way: on success it is spent, and on a
                    // malformed payload leaving it in place would make the
                    // next attempt re-read the same bad file forever.
                    let _ = tokio::fs::remove_file(&assertion_file).await;
                    let _ = tokio::fs::remove_file(&request_file).await;
                    let assertion = parse_assertion(bytes)?;
                    ::zeroclaw_log::record!(
                        INFO,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                        "WhatsApp passkey assertion accepted; resuming device linking"
                    );
                    return Ok(assertion);
                }
                // Absent, or present but still being written: keep waiting.
                _ => {}
            }

            if tokio::time::Instant::now() >= deadline {
                let _ = tokio::fs::remove_file(&request_file).await;
                return Err(PasskeyError::Cancelled);
            }

            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn credential_json(raw_id: &str, signature: &str) -> Vec<u8> {
        serde_json::json!({
            "id": raw_id,
            "rawId": raw_id,
            "type": "public-key",
            "response": {
                "clientDataJSON": "eyJ0IjoxfQ",
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
    }

    #[tokio::test]
    async fn waiting_publishes_the_request_and_consumes_the_assertion() {
        let tmp = tempfile::tempdir().unwrap();
        let session = tmp.path().join("session.db").to_string_lossy().to_string();

        let raw_id = BASE64_URL_SAFE_NO_PAD.encode(b"cred");
        let assertion_file = assertion_path(&session);
        let expected_request = request_path(&session);

        let auth = FilePasskeyAuthenticator::new(&session).with_wait(Duration::from_secs(10));
        let request = AssertionRequest {
            challenge: vec![1, 2, 3],
            rp_id: Some("web.whatsapp.com".into()),
            allow_credentials: vec![],
            user_verification: whatsapp_rust::passkey::UserVerification::Preferred,
            timeout_ms: Some(60_000),
            raw_options_json: r#"{"challenge":"AQID"}"#.into(),
        };

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
    async fn a_published_request_is_owner_only_and_leaves_no_temp_behind() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir
            .path()
            .join("session.db.passkey-request.json")
            .to_string_lossy()
            .into_owned();

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
