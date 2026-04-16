//! TTS engine abstraction shared by Kokoro / CosyVoice 2 / Typecast.
//!
//! The 4-tier voice router (PR #9) selects an engine at runtime based on
//! network state, user subscription, hardware tier, and the user's
//! "use my voice for interpretation" preference. Engines plug in by
//! implementing the [`TtsEngine`] trait, so the router decision logic
//! stays free of provider-specific shape.
//!
//! ## Engine families (plan §11.2)
//!
//! - **Tier S — Gemini 3.1 Flash Live API**: end-to-end audio-in/audio-out;
//!   served by the existing `SimulSession` and bypasses this trait.
//! - **Tier A — Typecast (Premium Online)**: 100+ persona voices + user
//!   voice cloning. Implementation lives in `voice::typecast_interp`.
//! - **Tier B — CosyVoice 2 (Offline Pro)**: zero-shot cloning via
//!   FunAudioLLM/CosyVoice. PR #8.
//! - **Tier C — Kokoro (Offline Basic)**: lightweight 82M-parameter
//!   default; ships with every install. PR #7.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// Output of a TTS synthesis call.
///
/// `pcm` is little-endian PCM16 samples at `sample_rate` Hz, mono. The
/// router downsamples / repacketizes for the gateway as needed.
#[derive(Debug, Clone)]
pub struct SynthesisResult {
    pub pcm: Vec<u8>,
    pub sample_rate: u32,
}

/// One synthesized voice slot exposed to the UI ("AI 비서" picker).
///
/// Engines describe their built-in voices via this metadata so the
/// frontend can render persona cards consistently across engines.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VoiceCard {
    /// Engine-specific voice identifier passed back to `synthesize`.
    pub id: String,
    /// Human-friendly display name (e.g. "박 변호사 비서", "Emily").
    pub display_name: String,
    /// BCP-47 language code the voice speaks natively.
    pub language: String,
    /// Optional gender hint ("female" / "male" / "neutral").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gender: Option<String>,
    /// Optional age band hint ("child" / "youth" / "adult" / "senior").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub age_band: Option<String>,
    /// Short persona blurb shown under the card.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub persona_blurb: Option<String>,
    /// Engine name (e.g. "kokoro", "typecast", "cosyvoice2"). Kept on the
    /// card so the UI can stamp a tier badge without asking the engine.
    pub engine: String,
}

/// Optional emotion / pacing controls. Engines that ignore unset fields
/// should treat them as "neutral" defaults. Plan §11.6 requires Gemma 4
/// audio-side emotion tags to bridge through the TTS layer; this struct
/// is the carrier.
#[derive(Debug, Clone, Default)]
pub struct EmotionHint {
    /// Symbolic emotion (e.g. "concerned", "neutral", "warm").
    pub emotion: Option<String>,
    /// Intensity in [0.0, 1.0].
    pub intensity: Option<f32>,
    /// Register: "formal" / "neutral" / "casual".
    pub register: Option<String>,
    /// Speaking rate multiplier (1.0 = engine default).
    pub speed: Option<f32>,
}

/// TTS engine trait — engines (Kokoro, CosyVoice 2, Typecast) implement
/// this and the 4-tier router (PR #9) dispatches calls without caring
/// about provider specifics.
#[async_trait]
pub trait TtsEngine: Send + Sync {
    /// Engine identifier for logging / observability (`"kokoro"`, etc.).
    fn name(&self) -> &str;

    /// List the voices this engine offers as UI cards. Kokoro/CosyVoice 2
    /// return their bundled voices; Typecast may return the user's
    /// personalised list.
    fn list_voices(&self) -> Vec<VoiceCard>;

    /// Synthesize `text` using `voice_id`, optionally constrained by
    /// `language` (BCP-47) and `emotion` controls. Returns PCM16 samples
    /// + the engine's native sample rate.
    async fn synthesize(
        &self,
        text: &str,
        voice_id: &str,
        language: &str,
        emotion: &EmotionHint,
    ) -> anyhow::Result<SynthesisResult>;

    /// Whether this engine supports user voice cloning. Tier A (Typecast)
    /// and Tier B (CosyVoice 2) return true; Tier C (Kokoro) returns false.
    fn supports_cloning(&self) -> bool {
        false
    }

    /// Fast health check — does the engine appear reachable / loaded?
    /// Used by the router to drop a tier when the engine misbehaves
    /// without waiting for a full synthesis call to time out.
    async fn health_ok(&self) -> bool;
}
