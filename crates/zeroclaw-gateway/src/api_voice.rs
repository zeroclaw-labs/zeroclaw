//! Voice REST endpoints: `POST /api/voice/transcribe`.
//!
//! Registered on a dedicated sub-router (see `lib.rs`) with a larger
//! request-body limit than the 64 KiB gateway default (audio payloads)
//! and the long-running timeout (local Whisper on a long clip can exceed
//! the 30s gateway-wide default).

use super::AppState;
use axum::{
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Json, Response},
};
use serde::Deserialize;
use zeroclaw_channels::transcription::TranscriptionManager;

/// Maximum decoded audio size accepted (20 MB).
pub const MAX_TRANSCRIBE_AUDIO_BYTES: usize = 20 * 1024 * 1024;

/// Request-body limit for the voice sub-router: base64 inflates the audio
/// by 4/3, plus JSON envelope overhead.
pub const MAX_TRANSCRIBE_BODY_BYTES: usize = 28 * 1024 * 1024;

#[derive(Deserialize)]
pub struct TranscribeBody {
    pub audio_b64: String,
    /// Audio container of the decoded bytes (e.g. `"wav"`). Used only for
    /// the upload file name most STT providers sniff. Defaults to `wav`.
    #[serde(default)]
    pub format: Option<String>,
}

#[derive(Deserialize)]
pub struct TranscribeQuery {
    /// Configured agent alias whose `transcription_provider` should be
    /// used. Omit for the runtime-default agent binding.
    #[serde(default)]
    pub agent: Option<String>,
}

fn error_response(status: StatusCode, message: String) -> Response {
    (status, Json(serde_json::json!({ "error": message }))).into_response()
}

/// Router-level auth gate for the voice sub-router: rejects unauthenticated
/// requests from the headers alone, BEFORE the (up to 28 MB) body is read
/// and parsed — the in-handler `require_auth` call would otherwise only run
/// after the `Json` extractor has buffered the full payload.
pub async fn require_auth_middleware(
    State(state): State<AppState>,
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    if let Err(e) = super::api::require_auth(&state, request.headers()) {
        return e.into_response();
    }
    next.run(request).await
}

/// POST /api/voice/transcribe — body `{"audio_b64","format"}` → `{"text"}`.
pub async fn handle_voice_transcribe(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<TranscribeQuery>,
    Json(body): Json<TranscribeBody>,
) -> Response {
    if let Err(e) = super::api::require_auth(&state, &headers) {
        return e.into_response();
    }

    // Reject obviously oversized payloads before decoding (4 base64 chars
    // per 3 decoded bytes).
    if body.audio_b64.len() > MAX_TRANSCRIBE_AUDIO_BYTES / 3 * 4 + 4 {
        return error_response(
            StatusCode::PAYLOAD_TOO_LARGE,
            format!(
                "audio exceeds the {} MB limit",
                MAX_TRANSCRIBE_AUDIO_BYTES / (1024 * 1024)
            ),
        );
    }

    use base64::Engine as _;
    let audio = match base64::engine::general_purpose::STANDARD.decode(body.audio_b64.trim()) {
        Ok(bytes) => bytes,
        Err(e) => {
            return error_response(
                StatusCode::BAD_REQUEST,
                format!("audio_b64 is not valid base64: {e}"),
            );
        }
    };
    if audio.is_empty() {
        return error_response(StatusCode::BAD_REQUEST, "audio_b64 is empty".to_string());
    }
    if audio.len() > MAX_TRANSCRIBE_AUDIO_BYTES {
        return error_response(
            StatusCode::PAYLOAD_TOO_LARGE,
            format!(
                "audio exceeds the {} MB limit",
                MAX_TRANSCRIBE_AUDIO_BYTES / (1024 * 1024)
            ),
        );
    }

    // File-name extension for provider MIME sniffing; keep it to a safe
    // alphanumeric token.
    let format = body
        .format
        .as_deref()
        .map(str::trim)
        .filter(|f| !f.is_empty() && f.chars().all(|c| c.is_ascii_alphanumeric()))
        .unwrap_or("wav")
        .to_ascii_lowercase();
    let file_name = format!("audio.{format}");

    let config = state.config.read().clone();
    let manager = match TranscriptionManager::from_config_for_agent(&config, query.agent.as_deref())
    {
        Ok(manager) => manager,
        Err(e) => {
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("transcription manager unavailable: {e}"),
            );
        }
    };

    // Provider resolution: the agent's `transcription_provider` binding
    // when set, else the deterministically-first configured provider as
    // an install-wide fallback.
    let result = if manager.agent_provider_alias().is_empty() {
        let mut available: Vec<String> = manager
            .available_providers()
            .into_iter()
            .map(str::to_string)
            .collect();
        available.sort();
        let Some(provider) = available.first() else {
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "no transcription provider configured — add a \
                 [providers.transcription.<type>.<alias>] entry or a [transcription] block"
                    .to_string(),
            );
        };
        manager
            .transcribe_with_provider(&audio, &file_name, provider)
            .await
    } else {
        manager.transcribe(&audio, &file_name).await
    };

    match result {
        Ok(text) => Json(serde_json::json!({ "text": text })).into_response(),
        Err(e) => {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"error": format!("{e:#}")})),
                "voice transcription failed"
            );
            error_response(
                StatusCode::BAD_GATEWAY,
                zeroclaw_providers::sanitize_api_error(&e.to_string()),
            )
        }
    }
}
