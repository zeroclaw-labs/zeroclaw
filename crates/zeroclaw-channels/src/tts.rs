//! Multi-provider Text-to-Speech (TTS) subsystem.

use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};

use zeroclaw_config::schema::{Config, TtsProviderConfig};

/// Maximum text length before synthesis is rejected (default: 4096 chars).
const DEFAULT_MAX_TEXT_LENGTH: usize = 4096;

/// Default HTTP request timeout for TTS API calls.
const TTS_HTTP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

/// Maximum time allowed for a local ffmpeg transcode.
const FFMPEG_TRANSCODE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

// ── TtsProvider trait ────────────────────────────────────────────

/// Trait for pluggable TTS backends.
#[async_trait::async_trait]
pub trait TtsProvider: Send + Sync + ::zeroclaw_api::attribution::Attributable {
    /// ModelProvider identifier (e.g. `"openai"`, `"elevenlabs"`).
    fn name(&self) -> &str;

    /// Synthesize `text` using the given `voice`, returning raw audio bytes.
    async fn synthesize(&self, text: &str, voice: &str) -> Result<Vec<u8>>;

    /// The audio container/format of the bytes returned by
    /// [`synthesize`](Self::synthesize) (e.g. `"opus"`, `"wav"`, `"mp3"`).
    /// Channels use this to pick the correct upload MIME type and Telegram
    /// send method — only `opus`/`ogg` is a true voice note.
    fn output_format(&self) -> &str;

    /// Voices supported by this model_provider.
    fn supported_voices(&self) -> Vec<String>;

    /// Audio output formats supported by this model_provider.
    fn supported_formats(&self) -> Vec<String>;
}

// ── OpenAI TTS ───────────────────────────────────────────────────

/// OpenAI TTS model_provider (`POST /v1/audio/speech`).
pub struct OpenAiTtsProvider {
    alias: String,
    api_key: String,
    model: String,
    speed: f64,
    /// Full endpoint URL. Defaults to the OpenAI production endpoint; can be
    /// overridden via `[providers.tts.openai.<alias>].uri` to point at any
    /// OpenAI-compatible TTS backend (Groq, Azure, self-hosted proxies).
    base_url: String,
    /// Audio response format. Defaults to `"opus"`; override to `"wav"` for
    /// Orpheus-class models or `"mp3"` for broader compatibility.
    response_format: String,
    client: reqwest::Client,
}

impl OpenAiTtsProvider {
    /// Create a new OpenAI TTS model_provider from config. Reads
    /// `[tts_providers.openai.<alias>].api_key` (or via the schema-mirror
    /// env grammar). Legacy `OPENAI_API_KEY` env-var fallback eradicated
    /// in V0.8.0.
    pub fn new(alias: &str, config: &TtsProviderConfig) -> Result<Self> {
        let api_key = config
            .api_key
            .as_deref()
            .map(str::trim)
            .filter(|k| !k.is_empty())
            .map(ToOwned::to_owned)
            .context(
                "Missing OpenAI TTS API key: set `[tts_providers.openai.<alias>].api_key` (or via \
                 `ZEROCLAW_providers__tts__openai__<alias>__api_key=...`).",
            )?;

        Ok(Self {
            alias: alias.to_string(),
            api_key,
            model: config
                .model
                .clone()
                .filter(|m| !m.trim().is_empty())
                .unwrap_or_else(|| "tts-1".to_string()),
            speed: config.speed.unwrap_or(1.0),
            base_url: config
                .uri
                .clone()
                .filter(|u| !u.trim().is_empty())
                .unwrap_or_else(|| "https://api.openai.com/v1/audio/speech".to_string()),
            response_format: config
                .response_format
                .clone()
                .filter(|f| !f.trim().is_empty())
                .unwrap_or_else(|| "opus".to_string()),
            client: reqwest::Client::builder()
                .timeout(TTS_HTTP_TIMEOUT)
                .build()
                .context("Failed to build HTTP client for OpenAI TTS")?,
        })
    }
}

#[async_trait::async_trait]
impl TtsProvider for OpenAiTtsProvider {
    fn name(&self) -> &str {
        "openai"
    }

    fn output_format(&self) -> &str {
        &self.response_format
    }

    async fn synthesize(&self, text: &str, voice: &str) -> Result<Vec<u8>> {
        let body = serde_json::json!({
            "model": self.model,
            "input": text,
            "voice": voice,
            "speed": self.speed,
            "response_format": self.response_format,
        });

        let resp = self
            .client
            .post(&self.base_url)
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .await
            .context("Failed to send OpenAI TTS request")?;

        let status = resp.status();
        if !status.is_success() {
            let error_body: serde_json::Value = resp
                .json()
                .await
                .unwrap_or_else(|_| serde_json::json!({"error": "unknown"}));
            let msg = error_body["error"]["message"]
                .as_str()
                .unwrap_or("unknown error");
            bail!("OpenAI TTS API error ({}): {}", status, msg);
        }

        let bytes = resp
            .bytes()
            .await
            .context("Failed to read OpenAI TTS response body")?;
        Ok(bytes.to_vec())
    }

    fn supported_voices(&self) -> Vec<String> {
        ["alloy", "echo", "fable", "onyx", "nova", "shimmer"]
            .iter()
            .map(|s| (*s).to_string())
            .collect()
    }

    fn supported_formats(&self) -> Vec<String> {
        ["mp3", "opus", "aac", "flac", "wav", "pcm"]
            .iter()
            .map(|s| (*s).to_string())
            .collect()
    }
}

// ── ElevenLabs TTS ───────────────────────────────────────────────

/// ElevenLabs TTS model_provider (`POST /v1/text-to-speech/{voice_id}`).
pub struct ElevenLabsTtsProvider {
    alias: String,
    api_key: String,
    model_id: String,
    stability: f64,
    similarity_boost: f64,
    client: reqwest::Client,
}

impl ElevenLabsTtsProvider {
    /// Create a new ElevenLabs TTS model_provider from config. Reads
    /// `[tts_providers.elevenlabs.<alias>].api_key`. Legacy
    /// `ELEVENLABS_API_KEY` env-var fallback eradicated in V0.8.0.
    pub fn new(alias: &str, config: &TtsProviderConfig) -> Result<Self> {
        let api_key = config
            .api_key
            .as_deref()
            .map(str::trim)
            .filter(|k| !k.is_empty())
            .map(ToOwned::to_owned)
            .context(
                "Missing ElevenLabs API key: set `[tts_providers.elevenlabs.<alias>].api_key` (or \
                 via `ZEROCLAW_providers__tts__elevenlabs__<alias>__api_key=...`).",
            )?;

        Ok(Self {
            alias: alias.to_string(),
            api_key,
            model_id: config
                .model
                .clone()
                .filter(|m| !m.trim().is_empty())
                .unwrap_or_else(|| "eleven_monolingual_v1".to_string()),
            stability: config.stability.unwrap_or(0.5),
            similarity_boost: config.similarity_boost.unwrap_or(0.5),
            client: reqwest::Client::builder()
                .timeout(TTS_HTTP_TIMEOUT)
                .build()
                .context("Failed to build HTTP client for ElevenLabs TTS")?,
        })
    }
}

#[async_trait::async_trait]
impl TtsProvider for ElevenLabsTtsProvider {
    fn name(&self) -> &str {
        "elevenlabs"
    }

    fn output_format(&self) -> &str {
        // ElevenLabs default output is MP3 (mp3_44100_128).
        "mp3"
    }

    async fn synthesize(&self, text: &str, voice: &str) -> Result<Vec<u8>> {
        if !voice
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            bail!("ElevenLabs voice ID contains invalid characters: {voice}");
        }
        let url = format!("https://api.elevenlabs.io/v1/text-to-speech/{voice}");
        let body = serde_json::json!({
            "text": text,
            "model_id": self.model_id,
            "voice_settings": {
                "stability": self.stability,
                "similarity_boost": self.similarity_boost,
            },
        });

        let resp = self
            .client
            .post(&url)
            .header("xi-api-key", &self.api_key)
            .json(&body)
            .send()
            .await
            .context("Failed to send ElevenLabs TTS request")?;

        let status = resp.status();
        if !status.is_success() {
            let error_body: serde_json::Value = resp
                .json()
                .await
                .unwrap_or_else(|_| serde_json::json!({"error": "unknown"}));
            let msg = error_body["detail"]["message"]
                .as_str()
                .or_else(|| error_body["detail"].as_str())
                .unwrap_or("unknown error");
            bail!("ElevenLabs TTS API error ({}): {}", status, msg);
        }

        let bytes = resp
            .bytes()
            .await
            .context("Failed to read ElevenLabs TTS response body")?;
        Ok(bytes.to_vec())
    }

    fn supported_voices(&self) -> Vec<String> {
        // ElevenLabs voices are user-specific; return empty (dynamic lookup).
        Vec::new()
    }

    fn supported_formats(&self) -> Vec<String> {
        ["mp3", "pcm", "ulaw"]
            .iter()
            .map(|s| (*s).to_string())
            .collect()
    }
}

// ── Google Cloud TTS ─────────────────────────────────────────────

/// Google Cloud TTS model_provider (`POST /v1/text:synthesize`).
pub struct GoogleTtsProvider {
    alias: String,
    api_key: String,
    language_code: String,
    client: reqwest::Client,
}

impl GoogleTtsProvider {
    /// Create a new Google Cloud TTS model_provider from config, resolving the API key
    /// from `[tts_providers.google.<alias>].api_key`. Legacy
    /// `GOOGLE_TTS_API_KEY` env-var fallback eradicated in V0.8.0.
    pub fn new(alias: &str, config: &TtsProviderConfig) -> Result<Self> {
        let api_key = config
            .api_key
            .as_deref()
            .map(str::trim)
            .filter(|k| !k.is_empty())
            .map(ToOwned::to_owned)
            .context(
                "Missing Google TTS API key: set `[tts_providers.google.<alias>].api_key` (or via \
                 `ZEROCLAW_providers__tts__google__<alias>__api_key=...`).",
            )?;

        Ok(Self {
            alias: alias.to_string(),
            api_key,
            language_code: config
                .language_code
                .clone()
                .filter(|c| !c.trim().is_empty())
                .unwrap_or_else(|| "en-US".to_string()),
            client: reqwest::Client::builder()
                .timeout(TTS_HTTP_TIMEOUT)
                .build()
                .context("Failed to build HTTP client for Google TTS")?,
        })
    }
}

#[async_trait::async_trait]
impl TtsProvider for GoogleTtsProvider {
    fn name(&self) -> &str {
        "google"
    }

    fn output_format(&self) -> &str {
        // audioConfig.audioEncoding is hard-coded to MP3 below.
        "mp3"
    }

    async fn synthesize(&self, text: &str, voice: &str) -> Result<Vec<u8>> {
        let url = "https://texttospeech.googleapis.com/v1/text:synthesize";
        let body = serde_json::json!({
            "input": { "text": text },
            "voice": {
                "languageCode": self.language_code,
                "name": voice,
            },
            "audioConfig": {
                "audioEncoding": "MP3",
            },
        });

        let resp = self
            .client
            .post(url)
            .header("x-goog-api-key", &self.api_key)
            .json(&body)
            .send()
            .await
            .context("Failed to send Google TTS request")?;

        let status = resp.status();
        let resp_body: serde_json::Value = resp
            .json()
            .await
            .context("Failed to parse Google TTS response")?;

        if !status.is_success() {
            let msg = resp_body["error"]["message"]
                .as_str()
                .unwrap_or("unknown error");
            bail!("Google TTS API error ({}): {}", status, msg);
        }

        let audio_b64 = resp_body["audioContent"]
            .as_str()
            .context("Google TTS response missing 'audioContent' field")?;

        use base64::Engine;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(audio_b64)
            .context("Failed to decode Google TTS base64 audio")?;
        Ok(bytes)
    }

    fn supported_voices(&self) -> Vec<String> {
        // Google voices vary by language; return common English defaults.
        [
            "en-US-Standard-A",
            "en-US-Standard-B",
            "en-US-Standard-C",
            "en-US-Standard-D",
        ]
        .iter()
        .map(|s| (*s).to_string())
        .collect()
    }

    fn supported_formats(&self) -> Vec<String> {
        ["mp3", "wav", "ogg"]
            .iter()
            .map(|s| (*s).to_string())
            .collect()
    }
}

// ── Edge TTS (subprocess) ────────────────────────────────────────

/// Edge TTS model_provider — free, uses the `edge-tts` CLI subprocess.
pub struct EdgeTtsProvider {
    alias: String,
    binary_path: String,
    #[cfg(test)]
    binary_args: Vec<String>,
    timeout: std::time::Duration,
}

/// How long the reaper waits for the child to exit after a graceful kill
/// before escalating to a hard kill, and before the temp file is removed.
const EDGE_TTS_REAP_GRACE: std::time::Duration = std::time::Duration::from_secs(5);

/// Send a graceful termination request to the child. On Unix this is
/// `SIGTERM`, which a cooperative child (or a test fixture) can handle or
/// ignore; Windows has no signal model, so fall back to the hard kill.
#[cfg(unix)]
fn graceful_kill(child: &mut tokio::process::Child) {
    if let Some(pid) = child.id() {
        let _ = unsafe { libc::kill(pid as i32, libc::SIGTERM) };
    }
}

#[cfg(not(unix))]
fn graceful_kill(child: &mut tokio::process::Child) {
    let _ = child.start_kill();
}

/// RAII cleanup for the temporary Edge TTS output file, the spawned child, and
/// its stderr drain. On every path out of [`EdgeTtsProvider::synthesize`] —
/// success, subprocess failure, timeout, output-read failure, and cancellation
/// (future drop) — the child is killed and reaped and the stderr reader aborted
/// before the artifact is removed, so deletion cannot race a still-terminating
/// process. The child reap and stderr drain never block an executor worker:
/// `Drop` only requests a graceful `SIGTERM`, checks the child once without
/// blocking, and if it has not exited hands it to a detached reaper thread that
/// reaps it (bounded) off any worker thread, escalating to a hard kill after
/// [`EDGE_TTS_REAP_GRACE`]. The reaper runs on a `std::thread`, so it is
/// independent of the Tokio runtime's lifetime — it is not cancelled when a
/// runtime shuts down the way a `tokio::spawn`ed task would be, and it never
/// panics for lack of an entered runtime. Cleanup failure is swallowed so it
/// never masks the primary synthesis error. The already-exited and no-child
/// branches delete the temp file with a single synchronous `remove_file` (fast
/// local-unlink); the pending-reap branch removes it inside the reaper thread.
/// If the OS refuses to create the reaper thread (resource pressure), `Drop`
/// recovers the still-owned child and reaps inline so the artifact is still
/// removed rather than leaked. That rare fallback is synchronous and bounded:
/// it performs the same bounded wait and hard-kill escalation on the calling
/// thread, so under thread exhaustion a cancelled synthesis can occupy the
/// dropping task for up to [`EDGE_TTS_REAP_GRACE`] instead of parking the reap
/// off-worker. This is the accepted tradeoff for the exceptional no-thread
/// case; the normal spawn-success path never blocks a worker. On every reaper
/// path the artifact is removed only after child exit is confirmed; if the
/// hard kill fails, status observation errors, or the child never confirms an
/// exit within the bound, the reaper gives up without unlinking rather than
/// racing the delete against a possibly-live child.
struct EdgeTtsTempArtifact {
    path: PathBuf,
    child: Option<tokio::process::Child>,
    /// The stderr-drain task, owned here so cancellation aborts it instead of
    /// detaching it (a descendant holding the pipe can otherwise keep the task
    /// alive after the direct child is gone).
    stderr_reader: Option<tokio::task::JoinHandle<String>>,
}

impl Drop for EdgeTtsTempArtifact {
    fn drop(&mut self) {
        // Abort the stderr drain first: closing the pipe read end is what lets
        // a descendant that inherited the pipe stop keeping the task alive.
        if let Some(reader) = self.stderr_reader.take() {
            reader.abort();
        }

        match self.child.take() {
            // No child: only the temp file needs removing.
            None => {
                let _ = std::fs::remove_file(&self.path);
            }
            Some(mut child) => {
                // Graceful first: SIGTERM on Unix. A cooperative child exits
                // promptly; an uncooperative one keeps running and stays
                // pending so the reaper below genuinely owns the bounded wait.
                graceful_kill(&mut child);
                // Already reaped (e.g. the success path): remove the file
                // inline — no reaper needed.
                if let Ok(Some(_)) = child.try_wait() {
                    let _ = std::fs::remove_file(&self.path);
                    return;
                }
                // Child still terminating: hand it to a detached reaper thread
                // rather than a `tokio::spawn`ed task. The reap-and-remove must
                // not depend on the Tokio runtime staying alive: a spawned task
                // is cancelled when its runtime shuts down (leaving the artifact
                // behind) and panics when dropped with no runtime entered. A
                // thread owns the bounded wait, escalates to a hard kill if the
                // grace window expires, and only then removes the temp file,
                // preserving the reap-before-delete ordering.
                //
                // The child travels in a shared cell so the closure can be
                // dropped without losing it: if the OS refuses the new thread
                // under resource pressure, `spawn` returns `Err` and the
                // closure (and its `Arc` clone) is dropped. The cell then still
                // holds the child, so this `Drop` recovers it and reaps inline
                // as a fallback, rather than leaving the temp file behind with
                // only `kill_on_drop` to stop the process.
                let child_cell = std::sync::Arc::new(std::sync::Mutex::new(Some(child)));
                let reaper_cell = std::sync::Arc::clone(&child_cell);
                let path = self.path.clone();
                let spawned = std::thread::Builder::new()
                    .name("edge-tts-reaper".to_string())
                    .spawn(move || {
                        if let Some(child) = reaper_cell
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .take()
                        {
                            reap_and_remove(child, path, EDGE_TTS_REAP_GRACE);
                        }
                    });
                if spawned.is_err() {
                    // Rare resource-exhaustion path: no thread was created. The
                    // child is still in the cell; reap and remove on this thread
                    // so the artifact is cleaned up despite the failed spawn.
                    if let Some(child) = child_cell
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .take()
                    {
                        reap_and_remove(child, self.path.clone(), EDGE_TTS_REAP_GRACE);
                    }
                }
            }
        }
    }
}

/// Reap the child until it exits or `grace` elapses, escalating to a hard kill
/// on timeout, then remove the temp file only after child exit is confirmed
/// following the hard kill. Runs on a detached [`std::thread`] so it never
/// depends on a Tokio runtime's lifetime and never blocks an executor worker.
/// Synchronous-only ([`tokio::process::Child::try_wait`] /
/// [`tokio::process::Child::start_kill`]) so it needs no runtime context.
///
/// The reap-before-delete contract is not relaxed on any path: `remove_file`
/// runs only once `try_wait` reports the child as exited. A still-terminating
/// child can retain the output handle on Windows and make the unlink fail,
/// leaving exactly the artifact this cleanup is meant to remove, so the reaper
/// stays responsible until the exit is confirmed rather than deleting on a
/// fixed timer.
///
/// The whole sequence is bounded by one absolute deadline (`grace` from the
/// call), so the inline thread-spawn-failure fallback in `Drop` blocks the
/// dropping thread for at most `grace` total. A small confirmation budget is
/// reserved at the tail of that window for the hard kill to be observed. If
/// the kill request fails, `try_wait` keeps erroring, or the child never
/// confirms an exit before the deadline, the reaper gives up WITHOUT removing
/// the file: a status-observation error does not establish that the child
/// exited, so unlinking on it would reintroduce the delete-before-exit race.
fn reap_and_remove(mut child: tokio::process::Child, path: PathBuf, grace: std::time::Duration) {
    reap_and_remove_with(&path, grace, |op| match op {
        ReapOp::Observe => child.try_wait(),
        ReapOp::Kill => child
            .start_kill()
            .map(|()| None::<std::process::ExitStatus>),
    });
}

/// What the reaper is asking the seam to do on the child.
enum ReapOp {
    /// Report child status (`Ok(Some)` = exited).
    Observe,
    /// Request a hard kill.
    Kill,
}

/// Bounded reap-and-remove body with a deterministic failure seam.
///
/// `op` performs the requested [`ReapOp`] and returns child status; the
/// production wrapper closes over the [`tokio::process::Child`], while tests
/// inject a closure that forces kill or wait failures without a real child.
///
/// Only an `Ok(Some)` observation removes the file. A kill request failure, a
/// `try_wait` error, or a child that never confirms an exit before the
/// `grace` deadline leaves the artifact in place (fail-closed) and returns.
fn reap_and_remove_with(
    path: &std::path::Path,
    grace: std::time::Duration,
    mut op: impl FnMut(ReapOp) -> std::io::Result<Option<std::process::ExitStatus>>,
) {
    // One absolute deadline bounds the graceful window AND the post-hard-kill
    // confirmation, so neither the detached reaper thread nor the inline
    // `Drop` fallback can live forever.
    let deadline = std::time::Instant::now() + grace;
    // Reserve a small confirmation budget at the tail of the window so a hard
    // kill has time to be observed before the deadline expires. When `grace`
    // is smaller than the budget (tests pass 150 ms), the budget shrinks to
    // fit and the hard kill is requested immediately.
    let confirm_budget = std::time::Duration::from_millis(250).min(grace);
    let mut hard_killed = false;
    loop {
        match op(ReapOp::Observe) {
            Ok(Some(_)) => {
                // Exit confirmed: safe to unlink.
                let _ = std::fs::remove_file(path);
                return;
            }
            Ok(None) | Err(_) => {
                // `Err` gets the same escalation as an unconfirmed running
                // child: a status-observation error does not establish exit,
                // so a kill is still attempted, but the file is never removed
                // without an `Ok(Some)` confirmation.
                let remaining = deadline.saturating_duration_since(std::time::Instant::now());
                if !hard_killed && remaining <= confirm_budget {
                    let _ = op(ReapOp::Kill);
                    hard_killed = true;
                }
                if remaining.is_zero() {
                    // Bounded: exit was never confirmed before the deadline
                    // (kill failed, wait kept erroring, or a child that
                    // refuses to die). Drop the child — `kill_on_drop` still
                    // requests a final kill — and leave the file in place
                    // rather than racing the unlink against a live child.
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
        }
    }
}

impl EdgeTtsProvider {
    /// Allowed basenames for the Edge TTS binary.
    const ALLOWED_BINARIES: &[&str] = &["edge-tts", "edge-playback"];

    pub fn new(alias: &str, config: &TtsProviderConfig) -> Result<Self> {
        let raw_path = config
            .binary_path
            .clone()
            .filter(|p| !p.trim().is_empty())
            .unwrap_or_else(|| "edge-tts".to_string());
        if raw_path.contains('/') || raw_path.contains('\\') {
            bail!(
                "Edge TTS binary_path must be a bare command name without path separators, got: {raw_path}"
            );
        }
        if !Self::ALLOWED_BINARIES.contains(&raw_path.as_str()) {
            bail!(
                "Edge TTS binary_path must be one of {:?}, got: {raw_path}",
                Self::ALLOWED_BINARIES,
            );
        }
        Ok(Self {
            alias: alias.to_string(),
            binary_path: raw_path,
            #[cfg(test)]
            binary_args: Vec::new(),
            timeout: TTS_HTTP_TIMEOUT,
        })
    }

    /// Test-only constructor that accepts a script path and timeout so tests
    /// can drive the `edge-tts` subprocess. The production [`new`](Self::new)
    /// allowlist stays a security boundary; this exists only in Unix test builds.
    #[cfg(all(test, unix))]
    fn new_with_binary(alias: &str, binary_path: &str, timeout: std::time::Duration) -> Self {
        Self::new_with_command(alias, binary_path, &[], timeout)
    }

    #[cfg(all(test, unix))]
    fn new_with_command(
        alias: &str,
        binary_path: &str,
        binary_args: &[&str],
        timeout: std::time::Duration,
    ) -> Self {
        Self {
            alias: alias.to_string(),
            binary_path: binary_path.to_string(),
            binary_args: binary_args.iter().map(|arg| (*arg).to_string()).collect(),
            timeout,
        }
    }
}

#[async_trait::async_trait]
impl TtsProvider for EdgeTtsProvider {
    fn name(&self) -> &str {
        "edge"
    }

    fn output_format(&self) -> &str {
        // edge-tts writes an MP3 temp file (see `--write-media …mp3`).
        "mp3"
    }
    async fn synthesize(&self, text: &str, voice: &str) -> Result<Vec<u8>> {
        let temp_dir = std::env::temp_dir();
        let output_file = temp_dir.join(format!("zeroclaw_tts_{}.mp3", uuid::Uuid::new_v4()));
        let output_path = output_file
            .to_str()
            .context("Failed to build temp file path for Edge TTS")?;

        // Spawn explicitly and move the child into the artifact guard, which
        // owns the child through timeout handling AND cancellation: on any path
        // out of synthesize it kills and reaps the child before removing the
        // artifact (see EdgeTtsTempArtifact::drop).
        let mut command = tokio::process::Command::new(&self.binary_path);
        #[cfg(test)]
        command.args(&self.binary_args);
        let child = command
            .arg("--text")
            .arg(text)
            .arg("--voice")
            .arg(voice)
            .arg("--write-media")
            .arg(output_path)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .context("Failed to spawn edge-tts subprocess")?;
        let mut artifact = EdgeTtsTempArtifact {
            path: output_file.clone(),
            child: Some(child),
            stderr_reader: None,
        };

        // Drain stderr concurrently so a verbose child cannot deadlock on a
        // full pipe while we wait for it to exit. Bytes are decoded lossily so
        // non-UTF-8 subprocess output still reaches the failure diagnostic. The
        // reader task stays owned by the artifact so cancellation aborts it.
        {
            use tokio::io::AsyncReadExt;
            let pipe = artifact
                .child
                .as_mut()
                .expect("child present after spawn")
                .stderr
                .take()
                .expect("stderr piped");
            artifact.stderr_reader = Some(zeroclaw_spawn::spawn!(async move {
                let mut buf = Vec::new();
                let mut pipe = pipe;
                let _ = pipe.read_to_end(&mut buf).await;
                String::from_utf8_lossy(&buf).into_owned()
            }));
        }

        // One absolute deadline shared by the process wait and the post-exit
        // pipe drain, so the drain bound cannot double the provider timeout.
        let deadline = tokio::time::Instant::now() + self.timeout;
        let (status, stderr) = {
            let child = artifact.child.as_mut().expect("child present after spawn");
            let reader = artifact
                .stderr_reader
                .as_mut()
                .expect("stderr reader set above");
            match tokio::time::timeout_at(deadline, child.wait()).await {
                Ok(Ok(status)) => {
                    // Drain stderr; if it never EOFs (a descendant held the
                    // pipe open) the shared deadline caps the join.
                    match tokio::time::timeout_at(deadline, &mut *reader).await {
                        Ok(Ok(stderr)) => (status, stderr),
                        Ok(Err(_)) => (status, String::new()),
                        Err(_elapsed) => {
                            reader.abort();
                            let _ = reader.await;
                            (status, String::new())
                        }
                    }
                }
                Ok(Err(err)) => {
                    reader.abort();
                    let _ = reader.await;
                    // Bound the kill-and-wait by the same absolute provider
                    // deadline used above, so a child that ignores the kill
                    // cannot hold `synthesize` past the provider timeout; the
                    // artifact guard's `Drop` still owns the final reap.
                    let _ = tokio::time::timeout_at(deadline, child.kill()).await;
                    let _ = tokio::time::timeout_at(deadline, child.wait()).await;
                    return Err(err).context("Failed to wait for edge-tts subprocess");
                }
                Err(_elapsed) => {
                    reader.abort();
                    let _ = reader.await;
                    let _ = tokio::time::timeout_at(deadline, child.kill()).await;
                    let _ = tokio::time::timeout_at(deadline, child.wait()).await;
                    bail!("Edge TTS subprocess timed out");
                }
            }
        };

        if !status.success() {
            bail!("edge-tts failed (exit {}): {}", status, stderr);
        }

        let bytes = tokio::fs::read(&output_file)
            .await
            .context("Failed to read edge-tts output file")?;

        Ok(bytes)
    }

    fn supported_voices(&self) -> Vec<String> {
        // Edge TTS has many voices; return common defaults.
        [
            "en-US-AriaNeural",
            "en-US-GuyNeural",
            "en-US-JennyNeural",
            "en-GB-SoniaNeural",
        ]
        .iter()
        .map(|s| (*s).to_string())
        .collect()
    }

    fn supported_formats(&self) -> Vec<String> {
        vec!["mp3".to_string()]
    }
}

// ── Piper TTS (local, OpenAI-compatible) ─────────────────────────

/// Piper TTS model_provider — local GPU-accelerated server with an OpenAI-compatible endpoint.
pub struct PiperTtsProvider {
    alias: String,
    client: reqwest::Client,
    api_url: String,
}

impl PiperTtsProvider {
    /// Create a new Piper TTS model_provider from config. Falls back to
    /// `http://127.0.0.1:5000/v1/audio/speech` when no `api_url` is supplied.
    pub fn new(alias: &str, config: &TtsProviderConfig) -> Self {
        let api_url = config
            .uri
            .clone()
            .filter(|u| !u.trim().is_empty())
            .unwrap_or_else(|| "http://127.0.0.1:5000/v1/audio/speech".to_string());
        Self {
            alias: alias.to_string(),
            client: reqwest::Client::builder()
                .timeout(TTS_HTTP_TIMEOUT)
                .build()
                .expect("Failed to build HTTP client for Piper TTS"),
            api_url,
        }
    }
}

#[async_trait::async_trait]
impl TtsProvider for PiperTtsProvider {
    fn name(&self) -> &str {
        "piper"
    }

    fn output_format(&self) -> &str {
        // Piper's OpenAI-compatible server returns WAV when no response_format
        // is requested (the body below omits it).
        "wav"
    }

    async fn synthesize(&self, text: &str, voice: &str) -> Result<Vec<u8>> {
        let body = serde_json::json!({
            "model": "tts-1",
            "input": text,
            "voice": voice,
        });

        let resp = self
            .client
            .post(&self.api_url)
            .json(&body)
            .send()
            .await
            .context("Failed to send Piper TTS request")?;

        let status = resp.status();
        if !status.is_success() {
            let error_body: serde_json::Value = resp
                .json()
                .await
                .unwrap_or_else(|_| serde_json::json!({"error": "unknown"}));
            let msg = error_body["error"]["message"]
                .as_str()
                .unwrap_or("unknown error");
            bail!("Piper TTS API error ({}): {}", status, msg);
        }

        let bytes = resp
            .bytes()
            .await
            .context("Failed to read Piper TTS response body")?;
        Ok(bytes.to_vec())
    }

    fn supported_voices(&self) -> Vec<String> {
        // Piper voices depend on installed models; return empty (dynamic).
        Vec::new()
    }

    fn supported_formats(&self) -> Vec<String> {
        ["mp3", "wav", "opus"]
            .iter()
            .map(|s| (*s).to_string())
            .collect()
    }
}

// ── TtsManager ───────────────────────────────────────────────────

async fn write_audio_and_wait_with_output(
    mut child: tokio::process::Child,
    audio: Vec<u8>,
    timeout: std::time::Duration,
) -> Result<std::process::Output> {
    use tokio::io::AsyncWriteExt;

    let mut stdin = child.stdin.take().context("ffmpeg stdin was not piped")?;

    tokio::time::timeout(timeout, async move {
        // Drive stdin and wait concurrently: if the child fills its stdout pipe
        // before stdin is complete, sequential operation would deadlock.
        let (write_result, output) = tokio::join!(
            async move {
                stdin.write_all(&audio).await?;
                stdin.shutdown().await
            },
            child.wait_with_output()
        );

        write_result.context("failed to write audio to ffmpeg stdin")?;
        output.context("ffmpeg process error")
    })
    .await
    .with_context(|| format!("ffmpeg transcode timed out after {timeout:?}"))?
}

async fn transcode_to_opus(audio: Vec<u8>) -> Result<Vec<u8>> {
    use std::process::Stdio;

    let child = tokio::process::Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-i",
            "pipe:0",
            "-f",
            "ogg",
            "-acodec",
            "libopus",
            "-b:a",
            "32k",
            "-vbr",
            "on",
            "pipe:1",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .context(
            "failed to spawn ffmpeg — ensure ffmpeg with libopus support is installed \
             (e.g. `sudo dnf install ffmpeg` / `sudo apt install ffmpeg`)",
        )?;

    let output = write_audio_and_wait_with_output(child, audio, FFMPEG_TRANSCODE_TIMEOUT).await?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("ffmpeg transcode to opus failed: {stderr}");
    }

    anyhow::ensure!(
        !output.stdout.is_empty(),
        "ffmpeg produced empty output — check that libopus is available"
    );

    Ok(output.stdout)
}

pub struct TtsManager {
    tts_providers: HashMap<String, Box<dyn TtsProvider>>,
    voice_by_alias: HashMap<String, String>,
    /// Resolved alias for the agent that owns this manager. Empty when
    /// the agent has no TTS preference (opt-out).
    agent_tts_provider: String,
    default_voice: String,
    max_text_length: usize,
}

impl TtsManager {
    pub fn from_config(config: &Config) -> Result<Self> {
        Self::from_config_for_agent(config, None)
    }

    pub fn from_config_for_agent(config: &Config, agent_alias: Option<&str>) -> Result<Self> {
        let mut tts_providers: HashMap<String, Box<dyn TtsProvider>> = HashMap::new();
        let mut voice_by_alias: HashMap<String, String> = HashMap::new();

        // Typed dispatch over the TtsProviders container's named slots. The
        // unknown-type warn-and-skip arm is gone — the typed container can't
        // hold an unrecognized family.
        for (family, alias, instance) in config.providers.tts.iter_entries() {
            let dotted = format!("{family}.{alias}");
            let result: Result<Box<dyn TtsProvider>> = match family {
                "openai" => OpenAiTtsProvider::new(alias, instance).map(|p| Box::new(p) as _),
                "elevenlabs" => {
                    ElevenLabsTtsProvider::new(alias, instance).map(|p| Box::new(p) as _)
                }
                "google" => GoogleTtsProvider::new(alias, instance).map(|p| Box::new(p) as _),
                "edge" => EdgeTtsProvider::new(alias, instance).map(|p| Box::new(p) as _),
                "piper" => Ok(Box::new(PiperTtsProvider::new(alias, instance)) as _),
                _ => unreachable!("TtsProviders typed slots cover all 5 families"),
            };
            match result {
                Ok(p) => {
                    tts_providers.insert(dotted.clone(), p);
                    if let Some(voice) = instance
                        .voice
                        .as_deref()
                        .map(str::trim)
                        .filter(|v| !v.is_empty())
                    {
                        voice_by_alias.insert(dotted, voice.to_string());
                    }
                }
                Err(e) => {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                            .with_attrs(
                                ::serde_json::json!({"error": format!("{}", e), "dotted": dotted})
                            ),
                        "Skipping TTS provider"
                    );
                }
            }
        }

        let max_text_length = if config.tts.max_text_length == 0 {
            DEFAULT_MAX_TEXT_LENGTH
        } else {
            config.tts.max_text_length
        };

        // Per-agent join: bind to the channel-owning agent's `tts_provider`
        // when known, else the runtime-active agent. Empty (or no resolved
        // agent) = no TTS; `synthesize` fails loud rather than silently
        // pick a provider.
        let agent_tts_provider = agent_alias
            .or_else(|| config.resolved_runtime_agent_alias())
            .and_then(|alias| config.agents.get(alias))
            .map(|a| a.tts_provider.as_str().to_string())
            .unwrap_or_default();

        Ok(Self {
            tts_providers,
            voice_by_alias,
            agent_tts_provider,
            default_voice: config.tts.default_voice.clone(),
            max_text_length,
        })
    }

    pub async fn synthesize_opus(&self, text: &str) -> Result<Vec<u8>> {
        let audio = self.synthesize(text).await?;
        let provider_alias = self.agent_tts_provider.as_str();
        let format = self
            .tts_providers
            .get(provider_alias)
            .map(|p| p.output_format())
            .unwrap_or("unknown");
        if format == "opus" {
            return Ok(audio);
        }
        transcode_to_opus(audio).await
    }

    pub async fn synthesize(&self, text: &str) -> Result<Vec<u8>> {
        let provider_alias = self.agent_tts_provider.as_str();
        if provider_alias.is_empty() {
            bail!(
                "Agent has no tts_provider configured. Set \
                 `agent.<alias>.tts_provider = \"<type>.<alias>\"` referencing a \
                 [providers.tts.<type>.<alias>] entry."
            );
        }
        let voice = self
            .voice_by_alias
            .get(provider_alias)
            .map_or(self.default_voice.as_str(), String::as_str);
        self.synthesize_with_provider(text, provider_alias, voice)
            .await
    }

    /// Synthesize text using the runtime-active agent's resolved
    /// `tts_provider` reference and an explicit voice.
    pub async fn synthesize_with_voice(&self, text: &str, voice: &str) -> Result<Vec<u8>> {
        let provider_alias = self.agent_tts_provider.as_str();
        if provider_alias.is_empty() {
            bail!(
                "Agent has no tts_provider configured. Set \
                 `agent.<alias>.tts_provider = \"<type>.<alias>\"` referencing a \
                 [providers.tts.<type>.<alias>] entry."
            );
        }
        self.synthesize_with_provider(text, provider_alias, voice)
            .await
    }

    /// Synthesize text using a specific dotted-alias model_provider and voice.
    pub async fn synthesize_with_provider(
        &self,
        text: &str,
        provider_alias: &str,
        voice: &str,
    ) -> Result<Vec<u8>> {
        if text.is_empty() {
            bail!("TTS text must not be empty");
        }
        let char_count = text.chars().count();
        if char_count > self.max_text_length {
            bail!(
                "TTS text too long ({} chars, max {})",
                char_count,
                self.max_text_length
            );
        }

        let tts = self.tts_providers.get(provider_alias).ok_or_else(|| {
            let available = self.available_providers().join(", ");
            ::zeroclaw_log::record!(
                ERROR,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "tts_provider": provider_alias,
                        "available": available,
                    })),
                "tts: provider not configured"
            );
            anyhow::Error::msg(format!(
                "TTS model_provider '{}' not configured (available: {})",
                provider_alias, available
            ))
        })?;

        use ::zeroclaw_log::Instrument;
        let span = ::zeroclaw_log::attribution_span!(tts.as_ref());
        ::zeroclaw_log::scope!(voice: voice, => tts.synthesize(text, voice))
            .instrument(span)
            .await
    }

    /// List dotted aliases of all initialized tts_providers.
    pub fn available_providers(&self) -> Vec<String> {
        let mut names: Vec<_> = self.tts_providers.keys().cloned().collect();
        names.sort();
        names
    }

    pub fn agent_output_format(&self) -> Option<&str> {
        let alias = self.agent_tts_provider.as_str();
        if alias.is_empty() {
            return None;
        }
        self.tts_providers.get(alias).map(|p| p.output_format())
    }
}

// ── Tests ────────────────────────────────────────────────────────

impl ::zeroclaw_api::attribution::Attributable for OpenAiTtsProvider {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        ::zeroclaw_api::attribution::Role::Provider(::zeroclaw_api::attribution::ProviderKind::Tts(
            ::zeroclaw_api::attribution::TtsProviderKind::OpenAi,
        ))
    }
    fn alias(&self) -> &str {
        &self.alias
    }
}

impl ::zeroclaw_api::attribution::Attributable for ElevenLabsTtsProvider {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        ::zeroclaw_api::attribution::Role::Provider(::zeroclaw_api::attribution::ProviderKind::Tts(
            ::zeroclaw_api::attribution::TtsProviderKind::ElevenLabs,
        ))
    }
    fn alias(&self) -> &str {
        &self.alias
    }
}

impl ::zeroclaw_api::attribution::Attributable for GoogleTtsProvider {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        ::zeroclaw_api::attribution::Role::Provider(::zeroclaw_api::attribution::ProviderKind::Tts(
            ::zeroclaw_api::attribution::TtsProviderKind::Google,
        ))
    }
    fn alias(&self) -> &str {
        &self.alias
    }
}

impl ::zeroclaw_api::attribution::Attributable for EdgeTtsProvider {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        ::zeroclaw_api::attribution::Role::Provider(::zeroclaw_api::attribution::ProviderKind::Tts(
            ::zeroclaw_api::attribution::TtsProviderKind::Edge,
        ))
    }
    fn alias(&self) -> &str {
        &self.alias
    }
}

impl ::zeroclaw_api::attribution::Attributable for PiperTtsProvider {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        ::zeroclaw_api::attribution::Role::Provider(::zeroclaw_api::attribution::ProviderKind::Tts(
            ::zeroclaw_api::attribution::TtsProviderKind::Piper,
        ))
    }
    fn alias(&self) -> &str {
        &self.alias
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    fn piped_shell_child(script: &str) -> tokio::process::Child {
        use std::process::Stdio;

        tokio::process::Command::new("sh")
            .args(["-c", script])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .expect("spawn test child")
    }

    #[cfg(unix)]
    async fn process_exists(pid: u32) -> bool {
        use std::process::Stdio;

        tokio::process::Command::new("kill")
            .arg("-0")
            .arg(pid.to_string())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await
            .is_ok_and(|status| status.success())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn transcode_process_times_out_stalled_child() {
        let child = piped_shell_child("exec sleep 60");
        let pid = child.id().expect("spawned child has a process ID");
        let started = std::time::Instant::now();
        let error = write_audio_and_wait_with_output(
            child,
            b"audio".to_vec(),
            std::time::Duration::from_millis(20),
        )
        .await
        .expect_err("stalled child must time out");

        assert!(
            error.to_string().contains("timed out"),
            "expected timeout error, got: {error:#}"
        );
        assert!(
            started.elapsed() < std::time::Duration::from_secs(2),
            "stalled child must return promptly"
        );

        let cleanup_deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while process_exists(pid).await && std::time::Instant::now() < cleanup_deadline {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert!(!process_exists(pid).await, "timed-out child must be killed");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn transcode_process_preserves_healthy_pipe_io() {
        let input = vec![b'a'; 1024 * 1024];
        let output = write_audio_and_wait_with_output(
            piped_shell_child("exec cat"),
            input.clone(),
            std::time::Duration::from_secs(5),
        )
        .await
        .expect("healthy child completes");

        assert!(output.status.success());
        assert_eq!(output.stdout, input);
    }

    fn config_with_edge_alias() -> Config {
        let mut cfg = Config::default();
        cfg.agents.insert(
            "default".into(),
            zeroclaw_config::schema::AliasedAgentConfig {
                tts_provider: "edge.default".into(),
                ..Default::default()
            },
        );
        cfg.providers.tts.edge.insert(
            "default".to_string(),
            zeroclaw_config::schema::EdgeTtsProviderConfig {
                base: TtsProviderConfig {
                    binary_path: Some("edge-tts".to_string()),
                    ..TtsProviderConfig::default()
                },
            },
        );
        cfg
    }

    fn config_with_piper_alias() -> Config {
        let mut cfg = Config::default();
        cfg.agents.insert(
            "default".into(),
            zeroclaw_config::schema::AliasedAgentConfig {
                tts_provider: "piper.default".into(),
                ..Default::default()
            },
        );
        cfg.providers.tts.piper.insert(
            "default".to_string(),
            zeroclaw_config::schema::PiperTtsProviderConfig {
                base: TtsProviderConfig {
                    uri: Some("http://127.0.0.1:5000/v1/audio/speech".to_string()),
                    ..TtsProviderConfig::default()
                },
            },
        );
        cfg
    }

    #[test]
    fn tts_manager_creation_with_defaults() {
        let config = Config::default();
        let manager = TtsManager::from_config(&config).unwrap();
        assert!(manager.available_providers().is_empty());
    }

    #[test]
    fn tts_manager_registers_alias_keyed_provider() {
        let cfg = config_with_edge_alias();
        let manager = TtsManager::from_config(&cfg).unwrap();
        assert_eq!(manager.available_providers(), vec!["edge.default"]);
    }

    #[test]
    fn tts_manager_binds_owning_agent_provider() {
        // Reuse the edge.default provider registration, but install two agents:
        // `primary` (the channel owner, has the provider) and a
        // lexicographically-earlier `background` agent with no `tts_provider`.
        let mut cfg = config_with_edge_alias();
        cfg.agents.clear();
        cfg.agents.insert(
            "primary".into(),
            zeroclaw_config::schema::AliasedAgentConfig {
                tts_provider: "edge.default".into(),
                ..Default::default()
            },
        );
        cfg.agents.insert(
            "background".into(),
            zeroclaw_config::schema::AliasedAgentConfig {
                ..Default::default()
            },
        );

        // Owner-bound resolution picks primary's provider...
        let owner_bound = TtsManager::from_config_for_agent(&cfg, Some("primary")).unwrap();
        assert_eq!(
            owner_bound.agent_tts_provider, "edge.default",
            "owner-bound manager must resolve the channel owner's tts_provider"
        );

        // ...while binding to the provider-less first-sorting agent stays empty,
        // proving the binding is per-agent and not a global/first-sorting pick.
        let background_bound = TtsManager::from_config_for_agent(&cfg, Some("background")).unwrap();
        assert!(
            background_bound.agent_tts_provider.is_empty(),
            "an agent with no tts_provider must not inherit another agent's provider"
        );
    }

    #[tokio::test]
    async fn tts_rejects_empty_text() {
        let cfg = config_with_edge_alias();
        let manager = TtsManager::from_config(&cfg).unwrap();
        let err = manager
            .synthesize_with_provider("", "edge.default", "en-US-AriaNeural")
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("must not be empty"),
            "expected empty-text error, got: {err}"
        );
    }

    #[tokio::test]
    async fn tts_rejects_text_exceeding_max_length() {
        let mut cfg = config_with_edge_alias();
        cfg.tts.max_text_length = 10;
        let manager = TtsManager::from_config(&cfg).unwrap();
        let long_text = "a".repeat(11);
        let err = manager
            .synthesize_with_provider(&long_text, "edge.default", "en-US-AriaNeural")
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("too long"),
            "expected too-long error, got: {err}"
        );
    }

    #[tokio::test]
    async fn tts_rejects_unknown_provider() {
        let cfg = Config::default();
        let manager = TtsManager::from_config(&cfg).unwrap();
        let err = manager
            .synthesize_with_provider("hello", "nonexistent.alias", "voice")
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("not configured"),
            "expected not-configured error, got: {err}"
        );
    }

    #[test]
    fn piper_provider_creation_uses_default_url_when_unset() {
        let model_provider = PiperTtsProvider::new("test", &TtsProviderConfig::default());
        assert_eq!(model_provider.name(), "piper");
        assert_eq!(
            model_provider.api_url,
            "http://127.0.0.1:5000/v1/audio/speech"
        );
        assert_eq!(
            model_provider.supported_formats(),
            vec!["mp3", "wav", "opus"]
        );
        assert!(model_provider.supported_voices().is_empty());
    }

    #[test]
    fn tts_manager_with_piper_alias() {
        let cfg = config_with_piper_alias();
        let manager = TtsManager::from_config(&cfg).unwrap();
        assert_eq!(manager.available_providers(), vec!["piper.default"]);
    }

    #[tokio::test]
    async fn tts_rejects_empty_text_for_piper() {
        let cfg = config_with_piper_alias();
        let manager = TtsManager::from_config(&cfg).unwrap();
        let err = manager
            .synthesize_with_provider("", "piper.default", "default")
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("must not be empty"),
            "expected empty-text error, got: {err}"
        );
    }

    #[test]
    fn tts_config_defaults() {
        let config = zeroclaw_config::schema::TtsConfig::default();
        assert!(!config.enabled);
        // TtsConfig has no global default-provider field; per-agent
        // `tts_provider` is the only selector.
        assert_eq!(config.default_voice, "alloy");
        assert_eq!(config.default_format, "mp3");
        assert_eq!(config.max_text_length, DEFAULT_MAX_TEXT_LENGTH);
    }

    fn config_with_openai_wav_alias() -> Config {
        let mut cfg = Config::default();
        cfg.agents.insert(
            "default".into(),
            zeroclaw_config::schema::AliasedAgentConfig {
                tts_provider: "openai.default".into(),
                ..Default::default()
            },
        );
        cfg.providers.tts.openai.insert(
            "default".to_string(),
            zeroclaw_config::schema::OpenAITtsProviderConfig {
                base: TtsProviderConfig {
                    api_key: Some("sk-test".to_string()),
                    response_format: Some("wav".to_string()),
                    ..TtsProviderConfig::default()
                },
            },
        );
        cfg
    }

    #[test]
    fn openai_provider_reports_configured_output_format() {
        let cfg = TtsProviderConfig {
            api_key: Some("sk-test".to_string()),
            response_format: Some("wav".to_string()),
            ..TtsProviderConfig::default()
        };
        let provider = OpenAiTtsProvider::new("default", &cfg).unwrap();
        assert_eq!(provider.output_format(), "wav");
    }

    #[test]
    fn openai_provider_defaults_output_format_to_opus() {
        let cfg = TtsProviderConfig {
            api_key: Some("sk-test".to_string()),
            ..TtsProviderConfig::default()
        };
        let provider = OpenAiTtsProvider::new("default", &cfg).unwrap();
        assert_eq!(provider.output_format(), "opus");
    }

    #[test]
    fn piper_provider_reports_wav_output_format() {
        let provider = PiperTtsProvider::new("default", &TtsProviderConfig::default());
        assert_eq!(provider.output_format(), "wav");
    }

    #[test]
    fn agent_output_format_resolves_active_provider() {
        let cfg = config_with_openai_wav_alias();
        let manager = TtsManager::from_config(&cfg).unwrap();
        assert_eq!(manager.agent_output_format(), Some("wav"));
    }

    #[test]
    fn agent_output_format_none_when_no_provider() {
        let manager = TtsManager::from_config(&Config::default()).unwrap();
        assert_eq!(manager.agent_output_format(), None);
    }

    #[test]
    fn tts_manager_max_text_length_zero_uses_default() {
        let mut cfg = Config::default();
        cfg.tts.max_text_length = 0;
        let manager = TtsManager::from_config(&cfg).unwrap();
        assert_eq!(manager.max_text_length, DEFAULT_MAX_TEXT_LENGTH);
    }

    #[tokio::test]
    async fn synthesize_posts_to_configured_uri_with_response_format() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/audio/speech"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"FAKE_WAV".to_vec()))
            .mount(&server)
            .await;

        let cfg = TtsProviderConfig {
            api_key: Some("sk-test".to_string()),
            uri: Some(format!("{}/v1/audio/speech", server.uri())),
            response_format: Some("wav".to_string()),
            ..TtsProviderConfig::default()
        };
        let provider = OpenAiTtsProvider::new("test", &cfg).unwrap();

        let audio = provider.synthesize("hello world", "hannah").await.unwrap();
        assert_eq!(
            audio, b"FAKE_WAV",
            "synthesize should return the bytes served by the configured endpoint"
        );

        let reqs = server.received_requests().await.unwrap();
        assert_eq!(
            reqs.len(),
            1,
            "exactly one POST should reach the configured uri"
        );
        let body: serde_json::Value = serde_json::from_slice(&reqs[0].body).unwrap();
        assert_eq!(
            body["response_format"], "wav",
            "configured response_format must reach the outgoing request body"
        );
        assert_eq!(body["input"], "hello world");
        assert_eq!(body["voice"], "hannah");
        assert_eq!(body["model"], "tts-1");
    }

    #[tokio::test]
    async fn synthesize_defaults_response_format_to_opus_when_unset() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/audio/speech"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"AUDIO".to_vec()))
            .mount(&server)
            .await;

        // uri points at the mock so we can inspect the body; response_format left unset.
        let cfg = TtsProviderConfig {
            api_key: Some("sk-test".to_string()),
            uri: Some(format!("{}/v1/audio/speech", server.uri())),
            ..TtsProviderConfig::default()
        };
        let provider = OpenAiTtsProvider::new("test", &cfg).unwrap();
        provider.synthesize("hi", "alloy").await.unwrap();

        let reqs = server.received_requests().await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&reqs[0].body).unwrap();
        assert_eq!(
            body["response_format"], "opus",
            "unset response_format must default to opus in the outgoing request"
        );
    }

    #[test]
    fn openai_defaults_to_production_endpoint_when_uri_unset() {
        let cfg = TtsProviderConfig {
            api_key: Some("sk-test".to_string()),
            ..TtsProviderConfig::default()
        };
        let provider = OpenAiTtsProvider::new("test", &cfg).unwrap();
        assert_eq!(provider.base_url, "https://api.openai.com/v1/audio/speech");
        assert_eq!(provider.response_format, "opus");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn edge_tts_removes_temp_output_when_read_fails() {
        use std::os::unix::fs::PermissionsExt;

        // Fake `edge-tts`: records the `--write-media` output path, writes an
        // unreadable artifact there, and exits successfully, forcing the
        // output-read failure path.
        let temp_dir = std::env::temp_dir();
        let script_path =
            temp_dir.join(format!("zeroclaw_edgetts_test_{}.sh", uuid::Uuid::new_v4()));
        let out_path_file = temp_dir.join(format!(
            "zeroclaw_edgetts_path_{}.txt",
            uuid::Uuid::new_v4()
        ));
        let script = script_path.to_str().unwrap();
        let sidecar = out_path_file.to_str().unwrap();
        std::fs::write(
            &script_path,
            format!(
                "#!/bin/sh\n\
                 out=\n\
                 prev=\n\
                 for a in \"$@\"; do\n\
                   if [ \"$prev\" = \"--write-media\" ]; then out=\"$a\"; fi\n\
                   prev=\"$a\"\n\
                 done\n\
                 printf '%s' \"$out\" > \"{sidecar}\"\n\
                 : > \"$out\"\n\
                 chmod 000 \"$out\"\n\
                 exit 0\n"
            ),
        )
        .unwrap();
        std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755)).unwrap();

        let provider =
            EdgeTtsProvider::new_with_binary("test", script, std::time::Duration::from_secs(5));
        let err = provider
            .synthesize("hello", "en-US-AriaNeural")
            .await
            .unwrap_err();
        assert!(
            err.to_string()
                .contains("Failed to read edge-tts output file"),
            "expected output-read failure, got: {err}"
        );

        let artifact =
            std::fs::read_to_string(&out_path_file).expect("script must record output path");
        assert!(
            !std::path::Path::new(&artifact).exists(),
            "Edge TTS temp output must be cleaned up after an output-read failure: {artifact}"
        );

        let _ = std::fs::remove_file(&script_path);
        let _ = std::fs::remove_file(&out_path_file);
    }

    #[cfg(unix)]
    fn read_edge_tts_fixture_state(sidecar: &std::path::Path) -> Option<(PathBuf, u32)> {
        let contents = std::fs::read_to_string(sidecar).ok()?;
        let contents = contents.strip_suffix('\n')?;
        let (artifact, pid) = contents.split_once('\n')?;
        if artifact.is_empty() || pid.is_empty() || pid.contains('\n') {
            return None;
        }
        Some((PathBuf::from(artifact), pid.parse().ok()?))
    }

    #[cfg(unix)]
    fn edge_tts_fixture_process_exists(pid: u32) -> std::io::Result<bool> {
        if unsafe { libc::kill(pid as i32, 0) } == 0 {
            return Ok(true);
        }

        let error = std::io::Error::last_os_error();
        match error.raw_os_error() {
            Some(libc::ESRCH) => Ok(false),
            Some(libc::EPERM) => Ok(true),
            _ => Err(error),
        }
    }

    #[cfg(unix)]
    async fn wait_for_edge_tts_cleanup(path: &std::path::Path, pid: u32, failure: &str) {
        let deadline =
            std::time::Instant::now() + EDGE_TTS_REAP_GRACE + std::time::Duration::from_secs(1);
        loop {
            let child_exists = edge_tts_fixture_process_exists(pid)
                .unwrap_or_else(|error| panic!("failed to inspect child {pid}: {error}"));
            let artifact_exists = path
                .try_exists()
                .unwrap_or_else(|error| panic!("failed to inspect {}: {error}", path.display()));
            if !artifact_exists && !child_exists {
                return;
            }
            if std::time::Instant::now() >= deadline {
                panic!(
                    "{failure}: {}; child {pid} alive: {child_exists}",
                    path.display()
                );
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn edge_tts_timeout_kills_child_and_removes_temp_output() {
        // Fake `edge-tts`: records the `--write-media` output path, writes an
        // artifact there, then keeps rewriting it while hanging, so the short
        // test timeout fires while a partial artifact exists. A live child
        // would recreate the artifact after our cleanup; kill_on_drop must
        // stop it so the path stays gone.
        let temp_dir = std::env::temp_dir();
        let script_path =
            temp_dir.join(format!("zeroclaw_edgetts_test_{}.sh", uuid::Uuid::new_v4()));
        let out_path_file = temp_dir.join(format!(
            "zeroclaw_edgetts_path_{}.txt",
            uuid::Uuid::new_v4()
        ));
        let script = script_path.to_str().unwrap();
        let sidecar = out_path_file.to_str().unwrap();
        std::fs::write(
            &script_path,
            format!(
                "out=\n\
                 prev=\n\
                 for a in \"$@\"; do\n\
                   if [ \"$prev\" = \"--write-media\" ]; then out=\"$a\"; fi\n\
                   prev=\"$a\"\n\
                 done\n\
                 [ -n \"$out\" ] || exit 64\n\
                 : > \"$out\"\n\
                 printf '%s\\n%s\\n' \"$out\" \"$$\" > \"{sidecar}\"\n\
                 while :; do : > \"$out\"; sleep 0.05; done\n"
            ),
        )
        .unwrap();

        // Short timeout so the hanging fake binary trips the timeout path fast.
        let provider = EdgeTtsProvider::new_with_command(
            "test",
            "/bin/sh",
            &[script],
            std::time::Duration::from_millis(250),
        );
        let err = provider
            .synthesize("hello", "en-US-AriaNeural")
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("timed out"),
            "expected a timeout error, got: {err}"
        );

        let (artifact, pid) = read_edge_tts_fixture_state(&out_path_file)
            .expect("script must record output path and process ID");
        wait_for_edge_tts_cleanup(
            &artifact,
            pid,
            "Edge TTS temp output must be removed after a timeout and the child killed",
        )
        .await;

        let _ = std::fs::remove_file(&script_path);
        let _ = std::fs::remove_file(&out_path_file);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn edge_tts_cancellation_reaps_child_and_removes_temp_output() {
        // Fake `edge-tts` that writes an artifact then hangs, like the timeout
        // test. The caller aborts synthesis before the provider timeout so the
        // future is dropped while `child.wait()` is pending; the artifact guard
        // must still kill and reap the child before removing the artifact.
        let temp_dir = std::env::temp_dir();
        let script_path =
            temp_dir.join(format!("zeroclaw_edgetts_test_{}.sh", uuid::Uuid::new_v4()));
        let out_path_file = temp_dir.join(format!(
            "zeroclaw_edgetts_path_{}.txt",
            uuid::Uuid::new_v4()
        ));
        let script = script_path.to_str().unwrap();
        let sidecar = out_path_file.to_str().unwrap();
        std::fs::write(
            &script_path,
            format!(
                "out=\n\
                 prev=\n\
                 for a in \"$@\"; do\n\
                   if [ \"$prev\" = \"--write-media\" ]; then out=\"$a\"; fi\n\
                   prev=\"$a\"\n\
                 done\n\
                 [ -n \"$out\" ] || exit 64\n\
                 : > \"$out\"\n\
                 printf '%s\\n%s\\n' \"$out\" \"$$\" > \"{sidecar}\"\n\
                 while :; do : > \"$out\"; sleep 0.05; done\n"
            ),
        )
        .unwrap();

        // Generous provider timeout: the abort (not the timeout) must drop the
        // waiting future, and the child needs time to start under test load.
        let provider = EdgeTtsProvider::new_with_command(
            "test",
            "/bin/sh",
            &[script],
            std::time::Duration::from_secs(10),
        );
        let mut handle = zeroclaw_spawn::spawn!(async move {
            provider.synthesize("hello", "en-US-AriaNeural").await
        });
        // Wait until the child has created the artifact and recorded its path
        // so the abort deterministically drops the future while `child.wait()`
        // is pending. If startup fails, report the provider result instead of
        // reducing the failure to a missing marker.
        let startup = tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                if let Some((path, pid)) = read_edge_tts_fixture_state(&out_path_file)
                    && path.try_exists().unwrap_or_else(|error| {
                        panic!("failed to inspect {}: {error}", path.display())
                    })
                {
                    break (path, pid);
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        });
        tokio::pin!(startup);
        let artifact = tokio::select! {
            result = &mut handle => {
                Err(format!("fake child exited before creating its output artifact: {result:?}"))
            }
            result = &mut startup => {
                result.map_err(|error| {
                    format!("fake child must create and record its output artifact before abort: {error}")
                })
            }
        };
        let (artifact, pid) = match artifact {
            Ok(state) => state,
            Err(message) => {
                if !handle.is_finished() {
                    handle.abort();
                    let _ = handle.await;
                }
                let _ = std::fs::remove_file(&script_path);
                let _ = std::fs::remove_file(&out_path_file);
                panic!("{message}");
            }
        };
        handle.abort();
        let cancellation = handle.await;
        assert!(
            matches!(cancellation, Err(ref error) if error.is_cancelled()),
            "provider task must end through cancellation: {cancellation:?}"
        );

        wait_for_edge_tts_cleanup(
            &artifact,
            pid,
            "Edge TTS temp output must be removed after cancellation",
        )
        .await;

        let _ = std::fs::remove_file(&script_path);
        let _ = std::fs::remove_file(&out_path_file);
    }

    #[cfg(unix)]
    #[test]
    fn edge_tts_cancellation_cleanup_does_not_block_current_thread_runtime() {
        use std::os::unix::fs::PermissionsExt;

        // A child that ignores SIGTERM for a few seconds then exits on its
        // own, so the artifact's bounded reap is genuinely pending while we
        // probe the runtime. The old `Drop` polled `std::thread::sleep` on the
        // worker and froze the probe; reaping must keep the worker responsive.
        let temp_dir = std::env::temp_dir();
        let script_path =
            temp_dir.join(format!("zeroclaw_edgetts_test_{}.sh", uuid::Uuid::new_v4()));
        let artifact_path =
            temp_dir.join(format!("zeroclaw_edgetts_out_{}.mp3", uuid::Uuid::new_v4()));
        let script = script_path.to_str().unwrap();
        let out = artifact_path.to_str().unwrap();
        std::fs::write(
            &script_path,
            format!(
                "#!/bin/sh\n\
                 : > \"{out}\"\n\
                 trap '' TERM\n\
                 sleep 3\n\
                 exit 0\n"
            ),
        )
        .unwrap();
        std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755)).unwrap();

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("current-thread runtime");
        rt.block_on(async {
            let child = tokio::process::Command::new(script)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .kill_on_drop(true)
                .spawn()
                .expect("spawn fake child");
            let artifact = EdgeTtsTempArtifact {
                path: artifact_path.clone(),
                child: Some(child),
                stderr_reader: None,
            };
            // Make sure the child is running before the artifact is dropped.
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;

            // A probe task on the same worker must keep advancing while cleanup
            // is pending. With a blocking Drop it cannot run until the reap
            // bound ends (~3 s); with the async reaper it fires on schedule.
            let probe = zeroclaw_spawn::spawn!(async {
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                true
            });

            drop(artifact);

            let started = std::time::Instant::now();
            let probe_result = tokio::time::timeout(std::time::Duration::from_secs(1), async {
                probe.await.expect("probe task")
            })
            .await
            .unwrap_or_else(|_| panic!("runtime stalled while Edge TTS cleanup was pending"));
            assert!(probe_result, "probe task must complete");
            assert!(
                started.elapsed() < std::time::Duration::from_secs(1),
                "cleanup must not stall the current-thread runtime"
            );
        });

        // The reaper runs on its own std thread and finishes independently of
        // the runtime's lifetime. Wait for the artifact to be removed so the
        // test proves the promised cleanup rather than leaking its own fixture
        // (the child exits on its own after ~3 s, well inside the reap grace).
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            if !artifact_path.exists() {
                break;
            }
            if std::time::Instant::now() >= deadline {
                panic!("cancellation reaper never removed the temp artifact");
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }

        let _ = std::fs::remove_file(&script_path);
    }

    #[cfg(unix)]
    #[test]
    fn edge_tts_cleanup_completes_after_runtime_shutdown() {
        use std::os::unix::fs::PermissionsExt;

        // A child that ignores SIGTERM for a few seconds then exits on its own,
        // so the artifact's bounded reap is genuinely pending when the runtime
        // is torn down. The reaper must finish (reap + remove the temp file)
        // independently of the Tokio runtime's lifetime: it must not be
        // cancelled when the runtime that owned the artifact shuts down (a
        // `tokio::spawn`ed cleanup task would be), nor panic from dropping off
        // the runtime.
        let temp_dir = std::env::temp_dir();
        let script_path =
            temp_dir.join(format!("zeroclaw_edgetts_test_{}.sh", uuid::Uuid::new_v4()));
        let artifact_path =
            temp_dir.join(format!("zeroclaw_edgetts_out_{}.mp3", uuid::Uuid::new_v4()));
        let script = script_path.to_str().unwrap();
        let out = artifact_path.to_str().unwrap();
        std::fs::write(
            &script_path,
            format!(
                "#!/bin/sh\n\
                 : > \"{out}\"\n\
                 trap '' TERM\n\
                 sleep 2\n\
                 exit 0\n"
            ),
        )
        .unwrap();
        std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755)).unwrap();

        {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("current-thread runtime");
            rt.block_on(async {
                let child = tokio::process::Command::new(script)
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .kill_on_drop(true)
                    .spawn()
                    .expect("spawn fake child");
                let artifact = EdgeTtsTempArtifact {
                    path: artifact_path.clone(),
                    child: Some(child),
                    stderr_reader: None,
                };
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;

                // Drop hands the still-terminating child to the reaper, then the
                // runtime is torn down at the end of this block.
                drop(artifact);
            });
            // Runtime is gone; the reaper must still be running on its own
            // thread, reaping the child and removing the temp file. A
            // `tokio::spawn`ed reaper would have been cancelled right here (its
            // task is aborted at runtime shutdown) before removing the file.
        }

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            if !std::path::Path::new(&artifact_path).exists() {
                break;
            }
            if std::time::Instant::now() >= deadline {
                panic!("Edge TTS temp file was never removed after runtime shutdown");
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }

        let _ = std::fs::remove_file(&script_path);
    }

    #[cfg(unix)]
    #[test]
    fn edge_tts_reaper_confirms_hard_kill_exit_before_removing_artifact() {
        use std::os::unix::fs::PermissionsExt;

        // Force the hard-escalation path of `reap_and_remove`: a child that
        // ignores SIGTERM and would otherwise outlive the default five-second
        // grace. A test-kit `grace` well under the child's lifetime makes the
        // escalation fire quickly. The reaper must remain responsible for the
        // child until `try_wait` confirms it exited after the hard kill, and
        // only then remove the artifact: it must not delete on a fixed timer
        // while the child is still terminating (a race Windows exposes because
        // a live child can retain the output handle and make `remove_file`
        // fail, leaving exactly the artifact this cleanup is meant to remove).
        let temp_dir = std::env::temp_dir();
        let script_path =
            temp_dir.join(format!("zeroclaw_edgetts_test_{}.sh", uuid::Uuid::new_v4()));
        let artifact_path =
            temp_dir.join(format!("zeroclaw_edgetts_out_{}.mp3", uuid::Uuid::new_v4()));
        let script = script_path.to_str().unwrap();
        let out = artifact_path.to_str().unwrap();
        // Hold the artifact open and ignore the graceful TERM, so the graceful
        // window passes and only the hard kill can end the child. A busy loop
        // keeps the tracked shell itself alive (no orphaned `sleep` to linger
        // after the SIGKILL); the reaper's hard kill is the sole way out.
        std::fs::write(
            &script_path,
            format!(
                "#!/bin/sh\n\
                 : > \"{out}\"\n\
                 trap '' TERM\n\
                 while :; do :; done\n"
            ),
        )
        .unwrap();
        std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755)).unwrap();

        // Spawn the child inside a current-thread runtime (as synthesis does),
        // then hand it to the detached reaper thread exactly as `Drop` does.
        // The reaper runs its own short grace (well under the child's 30 s
        // lifetime), so it is guaranteed to reach the hard-kill branch while
        // the child is still ignoring TERM.
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("current-thread runtime");
        let (child, child_pid) = rt.block_on(async {
            let child = tokio::process::Command::new(script)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .expect("spawn fake child");
            let pid = child.id().expect("child pid");
            (child, pid)
        });
        drop(rt);

        reap_and_remove(
            child,
            artifact_path.clone(),
            std::time::Duration::from_millis(150),
        );

        // The reaper removes the artifact only after `try_wait` confirms the
        // child exited, so poll until the artifact disappears and, at that
        // exact moment, assert the child is no longer alive: the reap
        // necessarily preceded the removal. If the old fixed-timer branch had
        // deleted while the child was still terminating, this ordering check
        // catches it.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            if !artifact_path.exists() {
                break;
            }
            if std::time::Instant::now() >= deadline {
                panic!("Edge TTS artifact was never removed by the hard-kill reaper");
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        let still_alive = unsafe { libc::kill(child_pid as i32, 0) } == 0;
        assert!(
            !still_alive,
            "the child must be reaped before (or by the moment) the artifact disappears"
        );

        let _ = std::fs::remove_file(&script_path);
    }

    #[test]
    fn edge_tts_reaper_is_bounded_and_preserves_artifact_when_kill_fails() {
        // The hard kill fails (start_kill errors) and the child never exits,
        // so try_wait keeps returning Ok(None). The reaper must stay bounded
        // by the grace deadline instead of looping forever, and must NOT
        // remove the artifact: exit was never confirmed, so unlinking on that
        // branch would reintroduce the delete-before-exit race.
        let temp_dir = std::env::temp_dir();
        let artifact_path =
            temp_dir.join(format!("zeroclaw_edgetts_out_{}.mp3", uuid::Uuid::new_v4()));
        std::fs::write(&artifact_path, b"stub").unwrap();

        let grace = std::time::Duration::from_millis(200);
        let started = std::time::Instant::now();
        reap_and_remove_with(&artifact_path, grace, |op| match op {
            ReapOp::Observe => Ok(None),
            ReapOp::Kill => Err(std::io::Error::other("hard kill failed")),
        });
        assert!(
            started.elapsed() < grace * 2,
            "the reaper must be bounded even when the hard kill fails"
        );
        assert!(
            artifact_path.exists(),
            "a failed hard kill must not remove the artifact without a confirmed exit"
        );

        let _ = std::fs::remove_file(&artifact_path);
    }

    #[test]
    fn edge_tts_reaper_is_bounded_and_preserves_artifact_when_wait_errors() {
        // try_wait keeps erroring, which does not establish that the child
        // exited. The reaper must remain bounded and must not treat the error
        // as permission to remove the artifact.
        let temp_dir = std::env::temp_dir();
        let artifact_path =
            temp_dir.join(format!("zeroclaw_edgetts_out_{}.mp3", uuid::Uuid::new_v4()));
        std::fs::write(&artifact_path, b"stub").unwrap();

        let grace = std::time::Duration::from_millis(200);
        let started = std::time::Instant::now();
        reap_and_remove_with(&artifact_path, grace, |op| match op {
            ReapOp::Observe => Err(std::io::Error::other("status observation failed")),
            ReapOp::Kill => Ok(None),
        });
        assert!(
            started.elapsed() < grace * 2,
            "the reaper must be bounded even when status observation errors"
        );
        assert!(
            artifact_path.exists(),
            "a wait error must not remove the artifact without a confirmed exit"
        );

        let _ = std::fs::remove_file(&artifact_path);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn edge_tts_descendant_holding_stderr_is_bounded_and_cleaned() {
        use std::os::unix::fs::PermissionsExt;

        // The direct `edge-tts` child exits successfully, but a background
        // descendant keeps the stderr pipe open, so EOF never arrives. The
        // reader join must be bounded (not hang synthesis) and the artifact
        // must still be removed.
        let temp_dir = std::env::temp_dir();
        let script_path =
            temp_dir.join(format!("zeroclaw_edgetts_test_{}.sh", uuid::Uuid::new_v4()));
        let out_path_file = temp_dir.join(format!(
            "zeroclaw_edgetts_path_{}.txt",
            uuid::Uuid::new_v4()
        ));
        let pid_file = temp_dir.join(format!("zeroclaw_edgetts_pid_{}.txt", uuid::Uuid::new_v4()));
        let script = script_path.to_str().unwrap();
        let sidecar = out_path_file.to_str().unwrap();
        let pidfile = pid_file.to_str().unwrap();
        std::fs::write(
            &script_path,
            format!(
                "#!/bin/sh\n\
                 out=\n\
                 prev=\n\
                 for a in \"$@\"; do\n\
                   if [ \"$prev\" = \"--write-media\" ]; then out=\"$a\"; fi\n\
                   prev=\"$a\"\n\
                 done\n\
                 printf '%s' \"$out\" > \"{sidecar}\"\n\
                 : > \"$out\"\n\
                 sleep 100 &\n\
                 echo $! > \"{pidfile}\"\n\
                 exit 0\n"
            ),
        )
        .unwrap();
        std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755)).unwrap();

        // The direct child exits immediately; the provider timeout bounds only
        // the post-exit stderr drain that never EOFs. A few seconds leaves room
        // for the child to start under load while keeping the drain bound.
        let provider =
            EdgeTtsProvider::new_with_binary("test", script, std::time::Duration::from_secs(2));
        let bounded = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            provider.synthesize("hello", "en-US-AriaNeural"),
        )
        .await;
        let _ = bounded
            .unwrap_or_else(|_| panic!("synthesis must not hang on a stderr pipe that never EOFs"));

        // Clean up the descendant that held the pipe open.
        if let Some(pid) = std::fs::read_to_string(&pid_file)
            .ok()
            .and_then(|pid| pid.trim().parse::<i32>().ok())
        {
            let _ = std::process::Command::new("kill")
                .args(["-9", &pid.to_string()])
                .status();
        }

        let artifact =
            std::fs::read_to_string(&out_path_file).expect("script must record output path");
        assert!(
            !std::path::Path::new(&artifact).exists(),
            "artifact must be removed even when stderr never reaches EOF: {artifact}"
        );

        let _ = std::fs::remove_file(&script_path);
        let _ = std::fs::remove_file(&out_path_file);
        let _ = std::fs::remove_file(&pid_file);
    }
}
