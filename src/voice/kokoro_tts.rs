//! Kokoro TTS via Kokoro-FastAPI HTTP server (Tier C — Offline Basic).
//!
//! Kokoro is an 82M-parameter Apache 2.0 TTS model with multilingual
//! support and ~24 kHz mono output. Plan §11.2 specifies it as the
//! always-shipped baseline so the offline voice path works on every
//! install without a paid subscription.
//!
//! ## Deployment shape
//!
//! This client speaks the Kokoro-FastAPI protocol (OpenAI-compatible
//! `/v1/audio/speech` endpoint) so MoA can either:
//!
//! 1. Bundle the FastAPI server as a sidecar process, or
//! 2. Connect to a user-managed Kokoro instance (Docker, dedicated host).
//!
//! The trait surface ([`super::tts_engine::TtsEngine`]) is identical
//! either way, so the router (PR #9) is deployment-agnostic.
//!
//! Direct in-process ONNX Runtime integration (no FastAPI hop) is a
//! follow-up — pulling in `ort` adds ~30 MB of native code and ~3 min to
//! cold builds, so it lives behind a feature flag in a separate PR.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::time::Duration;

use super::tts_engine::{EmotionHint, SynthesisResult, TtsEngine, VoiceCard};

/// Default Kokoro-FastAPI base URL (matches the upstream sidecar default).
pub const DEFAULT_BASE_URL: &str = "http://127.0.0.1:8880";
/// Kokoro outputs 24 kHz mono PCM16.
pub const KOKORO_SAMPLE_RATE: u32 = 24_000;

/// Kokoro TTS HTTP client.
pub struct KokoroEngine {
    base_url: String,
    client: reqwest::Client,
    voices: Vec<VoiceCard>,
    default_voice: String,
}

impl KokoroEngine {
    /// Construct an engine pointing at `base_url` (no trailing slash).
    /// `default_voice` is used when synthesise gets an unknown voice id.
    pub fn new(base_url: impl Into<String>, default_voice: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(60))
                .build()
                .expect("reqwest client builds"),
            voices: bundled_voice_cards(),
            default_voice: default_voice.into(),
        }
    }

    /// Convenient constructor with the documented sidecar default endpoint
    /// and `"af_heart"` as the fallback voice (Kokoro's stock American-female
    /// "warm" voice).
    pub fn with_defaults() -> Self {
        Self::new(DEFAULT_BASE_URL, "af_heart")
    }
}

#[async_trait]
impl TtsEngine for KokoroEngine {
    fn name(&self) -> &str {
        "kokoro"
    }

    fn list_voices(&self) -> Vec<VoiceCard> {
        self.voices.clone()
    }

    async fn synthesize(
        &self,
        text: &str,
        voice_id: &str,
        _language: &str,
        emotion: &EmotionHint,
    ) -> anyhow::Result<SynthesisResult> {
        // Resolve unknown voice ids to the engine default.
        let voice = if self.voices.iter().any(|v| v.id == voice_id) {
            voice_id.to_string()
        } else {
            self.default_voice.clone()
        };

        // OpenAI-compatible body. Kokoro-FastAPI honors `speed`; emotion is
        // not directly exposed, but we hint via prompt prefix when present.
        let prompt_prefix = match emotion.emotion.as_deref() {
            Some(e) if !e.is_empty() => format!("[tone: {e}] "),
            _ => String::new(),
        };
        let speed = emotion.speed.unwrap_or(1.0);

        let body = KokoroSpeechRequest {
            input: format!("{prompt_prefix}{text}"),
            voice,
            // Ask for raw 24 kHz PCM16 little-endian; saves us a WAV header strip.
            response_format: "pcm".to_string(),
            speed,
        };

        let url = format!("{}/v1/audio/speech", self.base_url);
        let resp = self.client.post(&url).json(&body).send().await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let err = resp.text().await.unwrap_or_default();
            anyhow::bail!("Kokoro TTS error {status}: {err}");
        }
        let pcm = resp.bytes().await?.to_vec();
        Ok(SynthesisResult {
            pcm,
            sample_rate: KOKORO_SAMPLE_RATE,
        })
    }

    async fn health_ok(&self) -> bool {
        let url = format!("{}/v1/audio/voices", self.base_url);
        match self
            .client
            .get(&url)
            .timeout(Duration::from_secs(2))
            .send()
            .await
        {
            Ok(r) => r.status().is_success(),
            Err(_) => false,
        }
    }
}

// ── Wire format ─────────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
struct KokoroSpeechRequest {
    input: String,
    voice: String,
    response_format: String,
    speed: f32,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct KokoroVoicesResponse {
    voices: Vec<String>,
}

// ── Bundled voice cards ─────────────────────────────────────────────────

/// Default offline persona pack (plan §11.3): hand-curated subset of
/// Kokoro voices given Korean nicknames so the offline picker still
/// looks like the Typecast-grade picker.
///
/// Kokoro voice id convention: `<lang_initial><gender_initial>_<name>`.
fn bundled_voice_cards() -> Vec<VoiceCard> {
    vec![
        // Korean (5)
        VoiceCard {
            id: "kf_lina".into(),
            display_name: "박 변호사 비서 (한국어 여)".into(),
            language: "ko".into(),
            gender: Some("female".into()),
            age_band: Some("adult".into()),
            persona_blurb: Some("차분하고 정확한 법률 문서 낭독 톤".into()),
            engine: "kokoro".into(),
        },
        VoiceCard {
            id: "km_jihun".into(),
            display_name: "김 팀장 조수 (한국어 남)".into(),
            language: "ko".into(),
            gender: Some("male".into()),
            age_band: Some("adult".into()),
            persona_blurb: Some("실무 회의 보고서 톤, 또렷한 발음".into()),
            engine: "kokoro".into(),
        },
        VoiceCard {
            id: "kf_yuna".into(),
            display_name: "이 신입 비서 (한국어 여, 명랑)".into(),
            language: "ko".into(),
            gender: Some("female".into()),
            age_band: Some("youth".into()),
            persona_blurb: Some("밝고 친근한 일상 대화 톤".into()),
            engine: "kokoro".into(),
        },
        VoiceCard {
            id: "km_seojun".into(),
            display_name: "박 부장 (한국어 남, 진중)".into(),
            language: "ko".into(),
            gender: Some("male".into()),
            age_band: Some("senior".into()),
            persona_blurb: Some("무게감 있는 임원 보고 톤".into()),
            engine: "kokoro".into(),
        },
        VoiceCard {
            id: "kf_arin".into(),
            display_name: "정 인턴 (한국어 여, 차분)".into(),
            language: "ko".into(),
            gender: Some("female".into()),
            age_band: Some("youth".into()),
            persona_blurb: Some("조곤조곤한 메모 낭독 톤".into()),
            engine: "kokoro".into(),
        },
        // English (5) — for interpretation output
        VoiceCard {
            id: "af_heart".into(),
            display_name: "Emily (English F, warm)".into(),
            language: "en".into(),
            gender: Some("female".into()),
            age_band: Some("adult".into()),
            persona_blurb: Some("Warm, conversational; great for client emails".into()),
            engine: "kokoro".into(),
        },
        VoiceCard {
            id: "am_adam".into(),
            display_name: "Adam (English M, neutral)".into(),
            language: "en".into(),
            gender: Some("male".into()),
            age_band: Some("adult".into()),
            persona_blurb: Some("Neutral business voice".into()),
            engine: "kokoro".into(),
        },
        VoiceCard {
            id: "bf_emma".into(),
            display_name: "Emma (British F)".into(),
            language: "en".into(),
            gender: Some("female".into()),
            age_band: Some("adult".into()),
            persona_blurb: Some("British accent, formal".into()),
            engine: "kokoro".into(),
        },
        VoiceCard {
            id: "bm_george".into(),
            display_name: "George (British M, senior)".into(),
            language: "en".into(),
            gender: Some("male".into()),
            age_band: Some("senior".into()),
            persona_blurb: Some("Authoritative; pairs well with legal output".into()),
            engine: "kokoro".into(),
        },
        VoiceCard {
            id: "af_bella".into(),
            display_name: "Bella (English F, youthful)".into(),
            language: "en".into(),
            gender: Some("female".into()),
            age_band: Some("youth".into()),
            persona_blurb: Some("Bright, friendly; informal updates".into()),
            engine: "kokoro".into(),
        },
    ]
}

// ── Tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn engine_name_is_kokoro() {
        let e = KokoroEngine::with_defaults();
        assert_eq!(e.name(), "kokoro");
        assert!(!e.supports_cloning());
    }

    #[test]
    fn bundled_voices_have_required_fields() {
        let cards = bundled_voice_cards();
        assert!(cards.len() >= 10, "need at least 10 stock voices");
        for c in &cards {
            assert!(!c.id.is_empty());
            assert!(!c.display_name.is_empty());
            assert!(matches!(c.language.as_str(), "ko" | "en"));
            assert_eq!(c.engine, "kokoro");
        }
    }

    #[test]
    fn bundled_voices_split_korean_and_english_evenly() {
        let cards = bundled_voice_cards();
        let ko = cards.iter().filter(|v| v.language == "ko").count();
        let en = cards.iter().filter(|v| v.language == "en").count();
        assert_eq!(ko, 5, "expected 5 Korean voices");
        assert_eq!(en, 5, "expected 5 English voices");
    }

    #[test]
    fn list_voices_returns_bundled_set() {
        let e = KokoroEngine::with_defaults();
        assert_eq!(e.list_voices().len(), bundled_voice_cards().len());
    }

    #[test]
    fn request_serializes_with_speed_and_format() {
        let req = KokoroSpeechRequest {
            input: "hello".into(),
            voice: "af_heart".into(),
            response_format: "pcm".into(),
            speed: 1.2,
        };
        let json = serde_json::to_value(&req).unwrap();
        assert_eq!(json["input"], "hello");
        assert_eq!(json["voice"], "af_heart");
        assert_eq!(json["response_format"], "pcm");
        // f32 → JSON drift makes exact compare flaky; check within epsilon.
        let speed = json["speed"].as_f64().expect("speed is a number");
        assert!((speed - 1.2).abs() < 1e-3, "speed {speed} not ~1.2");
    }

    #[tokio::test]
    async fn health_ok_returns_false_for_unreachable_endpoint() {
        let e = KokoroEngine::new("http://127.0.0.1:1", "af_heart");
        assert!(!e.health_ok().await);
    }

    /// Live test against a Kokoro-FastAPI sidecar at the default port.
    /// Run with:
    ///     cargo test --lib voice::kokoro_tts::tests::live_synthesize -- --ignored --nocapture
    #[tokio::test]
    #[ignore]
    async fn live_synthesize() {
        let engine = KokoroEngine::with_defaults();
        if !engine.health_ok().await {
            eprintln!(
                "skipping: no Kokoro-FastAPI server reachable at {}",
                DEFAULT_BASE_URL
            );
            return;
        }
        let result = engine
            .synthesize(
                "안녕하세요. 이것은 코코로 TTS 테스트입니다.",
                "kf_lina",
                "ko",
                &EmotionHint::default(),
            )
            .await
            .expect("synthesize should succeed");
        println!("\nGot {} bytes of {} Hz PCM", result.pcm.len(), result.sample_rate);
        assert!(!result.pcm.is_empty());
        assert_eq!(result.sample_rate, KOKORO_SAMPLE_RATE);
    }
}
