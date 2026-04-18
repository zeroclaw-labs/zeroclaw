//! Gemma 4 E4B speech-to-text via Ollama, drop-in alternative to Deepgram.
//!
//! Replaces the cloud Deepgram WebSocket path with an on-device transcription
//! pipeline:
//!
//! 1. Client streams 16 kHz mono PCM16 into [`GemmaAsrSession::send_audio`].
//! 2. A simple RMS-based VAD groups samples into utterances and detects the
//!    end-of-utterance after `silence_ms` of quiet.
//! 3. The completed utterance is wrapped in a minimal WAV header, base64-
//!    encoded, and POSTed to Ollama `/api/chat` under the `images` field
//!    (Ollama's quirky multimodal channel — see plan §11.8 verification).
//! 4. The model's text response is emitted as a single
//!    [`SttEvent::Final`] event reusing Deepgram's event shape so downstream
//!    consumers (segmentation engine, gateway) need no changes.
//!
//! ## Latency profile
//!
//! Unlike Deepgram (streaming partials within ~200 ms), Gemma 4 is a
//! request/response model so first text appears only after end-of-utterance
//! plus inference time (typically 1.5–3 s for a 5 s utterance on Apple
//! Silicon). No `Partial` events are emitted; UI should show a "listening
//! → transcribing" spinner during the gap. Mid-utterance partials would
//! require overlapping windows and are deferred to a follow-up PR.
//!
//! ## Why `images` field instead of `audio`?
//!
//! Ollama 0.20.x delivers multimodal binary input via the `images` field
//! regardless of modality. Live verification on the project author's
//! MacBook Air M4 confirmed `audio` is silently dropped while `images`
//! reaches the model and produces an accurate transcription. See
//! `docs/plans/2026-04-16-moa-gemma4-ollama-v1.1.md` §11 verification log.

// Audio DSP code does many narrow numeric casts (sample-rate × ms → bytes,
// PCM i16 → f64 for RMS). All operands are bounded by audio-domain constants
// well below the truncation thresholds.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_lossless,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]

use anyhow::Result;
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, Mutex};

use super::deepgram_stt::SttEvent;

// ── Constants ───────────────────────────────────────────────────────────

/// Audio input format expected from the client (matches Deepgram path).
pub const INPUT_SAMPLE_RATE: u32 = 16_000;
/// Mono.
pub const INPUT_CHANNELS: u16 = 1;
/// 16-bit signed little-endian.
pub const INPUT_BITS_PER_SAMPLE: u16 = 16;

/// Default Ollama HTTP endpoint.
pub const DEFAULT_OLLAMA_URL: &str = "http://127.0.0.1:11434";
/// Default Gemma 4 audio-capable tag (E4B effective 4B).
pub const DEFAULT_MODEL: &str = "gemma4:e4b";

// ── Configuration ───────────────────────────────────────────────────────

/// Session configuration for [`GemmaAsrSession`].
#[derive(Debug, Clone)]
pub struct GemmaAsrConfig {
    /// Session identifier (passed through to logs / events).
    pub session_id: String,
    /// Ollama base URL (no trailing slash).
    pub base_url: String,
    /// Ollama tag, e.g. `"gemma4:e4b"`. Must be an audio-capable tier
    /// (E2B or E4B) — 26B/31B are text+image only.
    pub model: String,
    /// Optional language hint (BCP-47, e.g. `"ko"`, `"en"`). Sent in the
    /// system prompt; `None` lets the model auto-detect.
    pub language_hint: Option<String>,
    /// Milliseconds of below-threshold audio that ends an utterance.
    pub silence_ms: u64,
    /// Hard ceiling on a single utterance length (forced flush).
    pub max_utterance_ms: u64,
    /// RMS threshold below which a frame is considered silence.
    /// Range 0.0–1.0 over normalized PCM. Default 0.012 (~−38 dBFS).
    pub rms_threshold: f32,
    /// VAD frame size in milliseconds (typical: 20–30).
    pub frame_ms: u64,
}

impl Default for GemmaAsrConfig {
    fn default() -> Self {
        Self {
            session_id: String::new(),
            base_url: DEFAULT_OLLAMA_URL.to_string(),
            model: DEFAULT_MODEL.to_string(),
            language_hint: None,
            silence_ms: 1_000,
            max_utterance_ms: 30_000,
            rms_threshold: 0.012,
            frame_ms: 30,
        }
    }
}

// ── Session ─────────────────────────────────────────────────────────────

/// Drop-in counterpart of `DeepgramSttSession` backed by Ollama+Gemma 4.
///
/// Same call surface: `send_audio` to push PCM, `event_rx` to read
/// transcription events, `close` to flush and stop.
pub struct GemmaAsrSession {
    audio_tx: mpsc::Sender<Vec<u8>>,
    pub event_rx: Arc<Mutex<mpsc::Receiver<SttEvent>>>,
    session_id: String,
    stop_tx: mpsc::Sender<()>,
}

impl GemmaAsrSession {
    /// Spawn the audio buffering + VAD + transcription task and return a
    /// session handle. The task lives until `close()` or the audio channel
    /// is closed by the caller.
    pub async fn start(config: GemmaAsrConfig) -> Result<Self> {
        let session_id = config.session_id.clone();
        let (audio_tx, audio_rx) = mpsc::channel::<Vec<u8>>(512);
        let (event_tx, event_rx) = mpsc::channel::<SttEvent>(64);
        let (stop_tx, stop_rx) = mpsc::channel::<()>(1);

        // Initial Ready event so the gateway can transition out of "connecting".
        let _ = event_tx
            .send(SttEvent::Ready {
                request_id: format!("gemma-{session_id}"),
            })
            .await;

        tokio::spawn(transcription_loop(config, audio_rx, event_tx, stop_rx));

        Ok(Self {
            audio_tx,
            event_rx: Arc::new(Mutex::new(event_rx)),
            session_id,
            stop_tx,
        })
    }

    /// Push a chunk of 16 kHz mono PCM16 into the session.
    pub async fn send_audio(&self, pcm: Vec<u8>) -> Result<()> {
        self.audio_tx
            .send(pcm)
            .await
            .map_err(|_| anyhow::anyhow!("gemma asr session is closed"))
    }

    /// Stop the session, flushing any pending audio first.
    pub async fn close(&self) {
        let _ = self.stop_tx.send(()).await;
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }
}

// ── Background loop ─────────────────────────────────────────────────────

async fn transcription_loop(
    cfg: GemmaAsrConfig,
    mut audio_rx: mpsc::Receiver<Vec<u8>>,
    event_tx: mpsc::Sender<SttEvent>,
    mut stop_rx: mpsc::Receiver<()>,
) {
    let frame_bytes = pcm_bytes_for_ms(cfg.frame_ms);
    let silence_frames_needed = (cfg.silence_ms / cfg.frame_ms).max(1) as usize;
    let max_utterance_bytes = pcm_bytes_for_ms(cfg.max_utterance_ms);

    let mut utterance: Vec<u8> = Vec::with_capacity(pcm_bytes_for_ms(5_000));
    let mut consecutive_silent = 0usize;
    let mut speech_seen = false;
    let mut frame_buffer: Vec<u8> = Vec::with_capacity(frame_bytes);

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(120))
        .build()
        .expect("reqwest client builds");

    loop {
        tokio::select! {
            _ = stop_rx.recv() => {
                if !utterance.is_empty() {
                    flush_and_transcribe(&client, &cfg, &event_tx, &utterance).await;
                }
                break;
            }
            chunk = audio_rx.recv() => {
                let Some(chunk) = chunk else { break };
                frame_buffer.extend_from_slice(&chunk);

                while frame_buffer.len() >= frame_bytes {
                    let frame: Vec<u8> = frame_buffer.drain(..frame_bytes).collect();
                    let rms = pcm16_rms(&frame);

                    if rms >= cfg.rms_threshold {
                        if !speech_seen {
                            speech_seen = true;
                            let _ = event_tx
                                .send(SttEvent::SpeechStarted {
                                    timestamp: 0.0,
                                })
                                .await;
                        }
                        consecutive_silent = 0;
                        utterance.extend_from_slice(&frame);
                    } else if speech_seen {
                        // In-utterance silence: keep recording but count down.
                        utterance.extend_from_slice(&frame);
                        consecutive_silent += 1;
                        if consecutive_silent >= silence_frames_needed {
                            // End of utterance.
                            flush_and_transcribe(&client, &cfg, &event_tx, &utterance).await;
                            utterance.clear();
                            consecutive_silent = 0;
                            speech_seen = false;
                        }
                    }
                    // Else: pre-speech silence, drop the frame entirely.

                    if utterance.len() >= max_utterance_bytes {
                        flush_and_transcribe(&client, &cfg, &event_tx, &utterance).await;
                        utterance.clear();
                        consecutive_silent = 0;
                        speech_seen = false;
                    }
                }
            }
        }
    }
}

/// Wrap PCM in WAV, send to Ollama, emit Final event with the response text.
/// Errors are logged but do not propagate out of the session loop.
async fn flush_and_transcribe(
    client: &reqwest::Client,
    cfg: &GemmaAsrConfig,
    event_tx: &mpsc::Sender<SttEvent>,
    pcm: &[u8],
) {
    if pcm.len() < pcm_bytes_for_ms(200) {
        // Too short to be meaningful speech.
        return;
    }
    let wav = wrap_pcm16_in_wav(pcm, INPUT_SAMPLE_RATE, INPUT_CHANNELS);
    let b64 = BASE64.encode(&wav);

    let prompt = match cfg.language_hint.as_deref() {
        Some(lang) => format!(
            "Transcribe this {lang} audio. Output ONLY the literal transcript text \
             with no additional commentary, no translation, no headers."
        ),
        None => "Transcribe this audio. Output ONLY the literal transcript text \
                 in the spoken language with no additional commentary."
            .to_string(),
    };

    let req = OllamaChatRequest {
        model: cfg.model.clone(),
        stream: false,
        messages: vec![OllamaMessage {
            role: "user".to_string(),
            content: prompt,
            images: vec![b64],
        }],
    };

    let resp = match client
        .post(format!("{}/api/chat", cfg.base_url))
        .json(&req)
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(error = ?e, "gemma asr POST failed");
            return;
        }
    };

    let parsed: OllamaChatResponse = match resp.json().await {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(error = ?e, "gemma asr response parse failed");
            return;
        }
    };

    let text = parsed.message.content.trim().to_string();
    if text.is_empty() {
        return;
    }

    let _ = event_tx
        .send(SttEvent::Final {
            text,
            confidence: 1.0,
            speech_final: true,
            language: cfg.language_hint.clone(),
            words: Vec::new(),
        })
        .await;
}

// ── PCM helpers ─────────────────────────────────────────────────────────

#[inline]
fn pcm_bytes_for_ms(ms: u64) -> usize {
    let samples = (INPUT_SAMPLE_RATE as u64 * ms) / 1_000;
    (samples as usize) * (INPUT_CHANNELS as usize) * (INPUT_BITS_PER_SAMPLE as usize / 8)
}

/// RMS over a PCM16LE little-endian buffer, normalized to [0.0, 1.0].
fn pcm16_rms(pcm: &[u8]) -> f32 {
    if pcm.is_empty() {
        return 0.0;
    }
    let mut sum_sq: f64 = 0.0;
    let mut n: usize = 0;
    for chunk in pcm.chunks_exact(2) {
        let sample = i16::from_le_bytes([chunk[0], chunk[1]]) as f64;
        let normalized = sample / (i16::MAX as f64);
        sum_sq += normalized * normalized;
        n += 1;
    }
    if n == 0 {
        return 0.0;
    }
    (sum_sq / n as f64).sqrt() as f32
}

/// Wrap raw PCM16LE in a minimal RIFF/WAVE header so Ollama recognizes the
/// format without needing a separate container.
pub fn wrap_pcm16_in_wav(pcm: &[u8], sample_rate: u32, channels: u16) -> Vec<u8> {
    let bits_per_sample: u16 = 16;
    let byte_rate = sample_rate * channels as u32 * bits_per_sample as u32 / 8;
    let block_align: u16 = channels * bits_per_sample / 8;
    let data_len = pcm.len() as u32;
    let chunk_size = 36 + data_len;

    let mut wav = Vec::with_capacity(44 + pcm.len());
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&chunk_size.to_le_bytes());
    wav.extend_from_slice(b"WAVE");
    wav.extend_from_slice(b"fmt ");
    wav.extend_from_slice(&16u32.to_le_bytes()); // PCM fmt chunk size
    wav.extend_from_slice(&1u16.to_le_bytes()); // PCM format
    wav.extend_from_slice(&channels.to_le_bytes());
    wav.extend_from_slice(&sample_rate.to_le_bytes());
    wav.extend_from_slice(&byte_rate.to_le_bytes());
    wav.extend_from_slice(&block_align.to_le_bytes());
    wav.extend_from_slice(&bits_per_sample.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_len.to_le_bytes());
    wav.extend_from_slice(pcm);
    wav
}

// ── Ollama wire format ──────────────────────────────────────────────────

#[derive(Serialize)]
struct OllamaChatRequest {
    model: String,
    stream: bool,
    messages: Vec<OllamaMessage>,
}

#[derive(Serialize)]
struct OllamaMessage {
    role: String,
    content: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    images: Vec<String>,
}

#[derive(Deserialize)]
struct OllamaChatResponse {
    message: OllamaResponseMessage,
}

#[derive(Deserialize)]
struct OllamaResponseMessage {
    #[serde(default)]
    content: String,
}

// ── Tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wav_header_has_correct_riff_chunk() {
        let pcm = vec![0u8; 1024];
        let wav = wrap_pcm16_in_wav(&pcm, 16_000, 1);
        assert_eq!(&wav[0..4], b"RIFF");
        assert_eq!(&wav[8..12], b"WAVE");
        assert_eq!(&wav[12..16], b"fmt ");
        assert_eq!(&wav[36..40], b"data");
        // Total = 44 (header) + pcm length.
        assert_eq!(wav.len(), 44 + pcm.len());
        // chunk_size at offset 4 = 36 + data_len
        let chunk_size = u32::from_le_bytes([wav[4], wav[5], wav[6], wav[7]]);
        assert_eq!(chunk_size, 36 + pcm.len() as u32);
    }

    #[test]
    fn wav_header_encodes_sample_rate() {
        let wav = wrap_pcm16_in_wav(&[0; 4], 16_000, 1);
        let sr = u32::from_le_bytes([wav[24], wav[25], wav[26], wav[27]]);
        assert_eq!(sr, 16_000);
        let ch = u16::from_le_bytes([wav[22], wav[23]]);
        assert_eq!(ch, 1);
    }

    #[test]
    fn pcm_bytes_for_ms_round_trip() {
        // 1 s of 16 kHz mono 16-bit = 32 000 bytes.
        assert_eq!(pcm_bytes_for_ms(1_000), 32_000);
        assert_eq!(pcm_bytes_for_ms(30), 960);
    }

    #[test]
    fn rms_silence_is_zero() {
        let silent = vec![0u8; 320]; // 10 ms of zeros
        assert_eq!(pcm16_rms(&silent), 0.0);
    }

    #[test]
    fn rms_full_scale_is_one() {
        // Construct a buffer of i16::MAX samples little-endian.
        let mut full = Vec::new();
        for _ in 0..160 {
            full.extend_from_slice(&i16::MAX.to_le_bytes());
        }
        let rms = pcm16_rms(&full);
        assert!((rms - 1.0).abs() < 1e-3, "expected ~1.0, got {rms}");
    }

    #[test]
    fn rms_handles_odd_byte_lengths() {
        // Odd byte length: trailing byte should be ignored, no panic.
        let buf = vec![0u8, 0, 0, 0, 0xFF];
        let _ = pcm16_rms(&buf);
    }

    #[tokio::test]
    async fn session_emits_ready_immediately() {
        let cfg = GemmaAsrConfig {
            session_id: "test".to_string(),
            // unreachable URL is fine — the Ready event is emitted before
            // any network call.
            base_url: "http://127.0.0.1:1".to_string(),
            ..Default::default()
        };
        let session = GemmaAsrSession::start(cfg).await.unwrap();
        let mut rx = session.event_rx.lock().await;
        let evt = tokio::time::timeout(Duration::from_millis(500), rx.recv())
            .await
            .expect("ready event must arrive")
            .expect("channel must yield event");
        assert!(matches!(evt, SttEvent::Ready { .. }));
    }

    /// Live test against the user's Ollama daemon. Sends 2 s of synthesized
    /// 440 Hz tone (which Gemma should describe as a tone or note no speech
    /// content), then 2 s of silence to trigger flush. Run with:
    ///     cargo test --lib voice::gemma_asr::tests::live_transcribe -- --ignored --nocapture
    #[tokio::test]
    #[ignore]
    async fn live_transcribe() {
        let cfg = GemmaAsrConfig {
            session_id: "live-test".to_string(),
            silence_ms: 600,
            ..Default::default()
        };
        let session = GemmaAsrSession::start(cfg).await.unwrap();

        // Synthesize a louder waveform so VAD triggers (square wave >> tone).
        let mut pcm = Vec::new();
        for i in 0..(INPUT_SAMPLE_RATE as usize * 2) {
            let v: i16 = if (i / 16) % 2 == 0 { 20_000 } else { -20_000 };
            pcm.extend_from_slice(&v.to_le_bytes());
        }
        // Trailing silence to flush.
        pcm.extend(vec![0u8; pcm_bytes_for_ms(1_000)]);

        // Send in 100 ms chunks so the loop processes incrementally.
        let chunk = pcm_bytes_for_ms(100);
        for slice in pcm.chunks(chunk) {
            session.send_audio(slice.to_vec()).await.unwrap();
        }

        // Wait up to 30 s for either Final or end of test.
        let mut rx = session.event_rx.lock().await;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
        while tokio::time::Instant::now() < deadline {
            if let Ok(Some(evt)) = tokio::time::timeout(Duration::from_millis(500), rx.recv()).await
            {
                println!("event: {evt:?}");
                if let SttEvent::Final { text, .. } = evt {
                    println!("\nfinal transcript: {text:?}");
                    return;
                }
            }
        }
        println!("(no final event before deadline — that may be normal for non-speech audio)");
    }
}
