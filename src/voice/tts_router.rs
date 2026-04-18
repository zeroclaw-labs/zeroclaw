//! 4-tier voice router (plan §11.5).
//!
//! Picks the right TTS path for each request based on:
//!
//! 1. **Network state** (online vs offline)
//! 2. **Tier S availability** (Gemini Live API key + reachable)
//! 3. **Tier A availability** (Typecast key + reachable)
//! 4. **User preference** ("내 목소리로 통역" — own-voice clone requested)
//! 5. **Tier B availability** (CosyVoice 2 sidecar reachable + user has a
//!    registered voice reference)
//! 6. **Hardware tier** from [`crate::host_probe`] (T1/T2 vs T3/T4 — used to
//!    bias the offline tier when the user has no explicit preference)
//!
//! ## Routing rules
//!
//! ```text
//! interpretation_mode + own_voice + online + Tier A live → A (Typecast clone)
//! interpretation_mode + own_voice + offline + Tier B live → B (CosyVoice 2)
//! online + Tier S live (and not own_voice override)        → S (Gemini Live)
//! online + Tier A live (premium voice picker)              → A
//! offline + T3/T4 + Tier B live                            → B
//! everything else                                          → C (Kokoro)
//! ```
//!
//! Tier S is special: it is the end-to-end S2S path (`SimulSession`),
//! NOT a `TtsEngine`. The router returns `Tier::S` and the caller
//! invokes the existing `SimulSession`/Live API code path. For tiers
//! A/B/C the router additionally exposes the engine handle so callers
//! can synthesize through the trait.

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use super::cosyvoice2::CosyVoice2Engine;
use super::kokoro_tts::KokoroEngine;
use super::tts_engine::{EmotionHint, SynthesisResult, TtsEngine, VoiceCard};

/// One of the four documented tiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Tier {
    /// Gemini 3.1 Flash Live — end-to-end S2S, served by `SimulSession`.
    /// `synthesize()` returns an error for this tier; callers must route
    /// to the Live API path.
    S,
    /// Typecast online — premium 100+ persona voices + cloud cloning.
    A,
    /// CosyVoice 2 offline — zero-shot local cloning.
    B,
    /// Kokoro offline — always-shipped baseline.
    C,
}

impl Tier {
    pub fn label(&self) -> &'static str {
        match self {
            Tier::S => "tier_s_gemini_live",
            Tier::A => "tier_a_typecast",
            Tier::B => "tier_b_cosyvoice2",
            Tier::C => "tier_c_kokoro",
        }
    }
}

/// Snapshot of runtime state used to decide a tier. All fields are
/// independent — caller fills what it knows; unknown booleans default
/// to `false`.
// Each boolean is an independent runtime dimension (network, three engine
// readiness flags, two user-preference toggles, hardware tier, strict mode).
// Folding them into a bitset / typed state machine would obscure the call
// site; keep the explicit struct.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Default)]
pub struct RoutingContext {
    /// Network reachability (typically from `local_llm::shared_health()`).
    pub online: bool,
    /// Tier S endpoint considered usable: API key valid + recent ping ok.
    pub gemini_live_ready: bool,
    /// Tier A endpoint considered usable: Typecast key + sidecar healthy.
    pub typecast_ready: bool,
    /// Tier B engine reachable AND the user has at least one registered
    /// voice reference.
    pub cosyvoice_ready: bool,
    /// Tier C engine reachable.
    pub kokoro_ready: bool,
    /// User explicitly asked for their own cloned voice in the output
    /// (e.g. interpretation mode "내 목소리로 통역" toggle).
    pub own_voice_requested: bool,
    /// Whether this is an interpretation request (vs general voice chat).
    pub interpretation_mode: bool,
    /// Hardware tier hint: true for T3/T4 (16+ GB VRAM-equivalent),
    /// false for T1/T2. Biases offline routing toward CosyVoice 2 when
    /// the device can comfortably run it.
    pub hw_high_perf: bool,
    /// Privacy-strict override: never leave the device.
    pub strict_local: bool,
}

/// Pure routing decision. No I/O — caller assembles `ctx` from cached
/// state (network probe, engine health, config) before calling this.
pub fn decide(ctx: &RoutingContext) -> Tier {
    // Privacy-strict mode wins absolutely: choose the best local path.
    if ctx.strict_local {
        if ctx.cosyvoice_ready && (ctx.own_voice_requested || ctx.hw_high_perf) {
            return Tier::B;
        }
        return Tier::C;
    }

    // Own-voice cloning bypasses Tier S (Live API doesn't clone arbitrary
    // speakers). Online → Typecast preferred; offline → CosyVoice 2.
    if ctx.own_voice_requested && ctx.interpretation_mode {
        if ctx.online && ctx.typecast_ready {
            return Tier::A;
        }
        if ctx.cosyvoice_ready {
            return Tier::B;
        }
        // No clone path available — degrade to baseline so the user still
        // gets audio output. Caller can show a toast explaining the swap.
        return Tier::C;
    }

    // Default online path: Tier S beats Tier A on latency / emotion when
    // own-voice isn't required. Premium picker still wins when user has
    // explicitly chosen a Typecast persona at the session level — the
    // caller sets `own_voice_requested=false` but `typecast_ready=true`
    // and we route here. Tier A in that case requires the caller to also
    // set a flag we don't model yet; default to S.
    if ctx.online {
        if ctx.gemini_live_ready {
            return Tier::S;
        }
        if ctx.typecast_ready {
            return Tier::A;
        }
    }

    // Offline routing.
    if ctx.cosyvoice_ready && ctx.hw_high_perf {
        return Tier::B;
    }

    Tier::C
}

/// Bundle of TTS engines with the routing decision. Cheap to clone via
/// `Arc`. Construct once at startup and share across handlers.
#[derive(Clone)]
pub struct TtsRouter {
    kokoro: Option<Arc<KokoroEngine>>,
    cosyvoice: Option<Arc<CosyVoice2Engine>>,
}

impl TtsRouter {
    /// Empty router — `decide()` will only ever return `Tier::C` if
    /// `kokoro_ready = true`, otherwise the picker silently falls
    /// through. Use [`with_kokoro`] / [`with_cosyvoice`] to populate.
    pub fn new() -> Self {
        Self {
            kokoro: None,
            cosyvoice: None,
        }
    }

    pub fn with_kokoro(mut self, k: Arc<KokoroEngine>) -> Self {
        self.kokoro = Some(k);
        self
    }

    pub fn with_cosyvoice(mut self, c: Arc<CosyVoice2Engine>) -> Self {
        self.cosyvoice = Some(c);
        self
    }

    pub fn kokoro(&self) -> Option<&Arc<KokoroEngine>> {
        self.kokoro.as_ref()
    }

    pub fn cosyvoice(&self) -> Option<&Arc<CosyVoice2Engine>> {
        self.cosyvoice.as_ref()
    }

    /// Aggregate the voice cards the picker should show given the current
    /// tier preference. Tier S/A cards come from elsewhere (Live API
    /// catalog / Typecast user list); the router contributes B + C cards.
    pub fn picker_voice_cards(&self) -> Vec<VoiceCard> {
        let mut out = Vec::new();
        if let Some(c) = &self.cosyvoice {
            out.extend(c.list_voices());
        }
        if let Some(k) = &self.kokoro {
            out.extend(k.list_voices());
        }
        out
    }

    /// Probe registered engines and update `ctx` with the latest health
    /// + readiness flags. Tier S/A readiness must be supplied by the
    ///   caller (those engines aren't owned by the router).
    pub async fn refresh_engine_health(&self, ctx: &mut RoutingContext) {
        if let Some(k) = &self.kokoro {
            ctx.kokoro_ready = k.health_ok().await;
        } else {
            ctx.kokoro_ready = false;
        }
        if let Some(c) = &self.cosyvoice {
            ctx.cosyvoice_ready = c.health_ok().await && !c.list_references().is_empty();
        } else {
            ctx.cosyvoice_ready = false;
        }
    }

    /// Synthesize via the chosen tier. Returns `Tier::S` as an error so
    /// the caller routes to the Live API path explicitly (Live API is
    /// not a `TtsEngine` — it's a bidirectional session).
    pub async fn synthesize(
        &self,
        tier: Tier,
        text: &str,
        voice_id: &str,
        language: &str,
        emotion: &EmotionHint,
    ) -> anyhow::Result<SynthesisResult> {
        match tier {
            Tier::S => {
                anyhow::bail!(
                    "Tier S (Gemini Live) is end-to-end S2S; route via SimulSession instead"
                );
            }
            Tier::A => {
                anyhow::bail!(
                    "Tier A (Typecast) is owned by voice::typecast_interp; \
                     router does not hold a Typecast handle in this PR"
                );
            }
            Tier::B => {
                let c = self.cosyvoice.as_ref().ok_or_else(|| {
                    anyhow::anyhow!("Tier B requested but CosyVoice 2 engine not registered")
                })?;
                c.synthesize(text, voice_id, language, emotion).await
            }
            Tier::C => {
                let k = self.kokoro.as_ref().ok_or_else(|| {
                    anyhow::anyhow!("Tier C requested but Kokoro engine not registered")
                })?;
                k.synthesize(text, voice_id, language, emotion).await
            }
        }
    }
}

impl Default for TtsRouter {
    fn default() -> Self {
        Self::new()
    }
}

// ── Tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx_default_online() -> RoutingContext {
        RoutingContext {
            online: true,
            gemini_live_ready: true,
            typecast_ready: true,
            cosyvoice_ready: false,
            kokoro_ready: true,
            own_voice_requested: false,
            interpretation_mode: false,
            hw_high_perf: false,
            strict_local: false,
        }
    }

    #[test]
    fn default_online_picks_tier_s() {
        let ctx = ctx_default_online();
        assert_eq!(decide(&ctx), Tier::S);
    }

    #[test]
    fn online_without_live_falls_back_to_typecast() {
        let mut ctx = ctx_default_online();
        ctx.gemini_live_ready = false;
        assert_eq!(decide(&ctx), Tier::A);
    }

    #[test]
    fn online_without_live_or_typecast_uses_kokoro() {
        let mut ctx = ctx_default_online();
        ctx.gemini_live_ready = false;
        ctx.typecast_ready = false;
        assert_eq!(decide(&ctx), Tier::C);
    }

    #[test]
    fn own_voice_interpretation_online_picks_typecast() {
        let mut ctx = ctx_default_online();
        ctx.own_voice_requested = true;
        ctx.interpretation_mode = true;
        // Even though Live API is "ready", it doesn't clone arbitrary speakers.
        assert_eq!(decide(&ctx), Tier::A);
    }

    #[test]
    fn own_voice_interpretation_offline_picks_cosyvoice_when_ready() {
        let mut ctx = RoutingContext::default();
        ctx.online = false;
        ctx.cosyvoice_ready = true;
        ctx.kokoro_ready = true;
        ctx.own_voice_requested = true;
        ctx.interpretation_mode = true;
        assert_eq!(decide(&ctx), Tier::B);
    }

    #[test]
    fn own_voice_offline_no_clone_engine_degrades_to_kokoro() {
        let mut ctx = RoutingContext::default();
        ctx.kokoro_ready = true;
        ctx.own_voice_requested = true;
        ctx.interpretation_mode = true;
        // Neither typecast nor cosyvoice is ready → no clone path; pick C.
        assert_eq!(decide(&ctx), Tier::C);
    }

    #[test]
    fn offline_high_perf_with_cosyvoice_picks_tier_b() {
        let mut ctx = RoutingContext::default();
        ctx.cosyvoice_ready = true;
        ctx.kokoro_ready = true;
        ctx.hw_high_perf = true;
        assert_eq!(decide(&ctx), Tier::B);
    }

    #[test]
    fn offline_low_perf_picks_kokoro_even_with_cosyvoice() {
        // T1/T2 user — CosyVoice 2 will be sluggish; bias to Kokoro.
        let mut ctx = RoutingContext::default();
        ctx.cosyvoice_ready = true;
        ctx.kokoro_ready = true;
        ctx.hw_high_perf = false;
        assert_eq!(decide(&ctx), Tier::C);
    }

    #[test]
    fn strict_local_overrides_online_state() {
        let mut ctx = ctx_default_online();
        ctx.strict_local = true;
        ctx.cosyvoice_ready = true;
        ctx.hw_high_perf = true;
        assert_eq!(decide(&ctx), Tier::B);
    }

    #[test]
    fn strict_local_falls_back_to_kokoro_when_no_clone() {
        let mut ctx = ctx_default_online();
        ctx.strict_local = true;
        ctx.cosyvoice_ready = false;
        assert_eq!(decide(&ctx), Tier::C);
    }

    #[test]
    fn tier_label_strings() {
        assert_eq!(Tier::S.label(), "tier_s_gemini_live");
        assert_eq!(Tier::A.label(), "tier_a_typecast");
        assert_eq!(Tier::B.label(), "tier_b_cosyvoice2");
        assert_eq!(Tier::C.label(), "tier_c_kokoro");
    }

    #[tokio::test]
    async fn synthesize_tier_s_returns_error() {
        let router = TtsRouter::new();
        let res = router
            .synthesize(Tier::S, "hi", "v", "en", &EmotionHint::default())
            .await;
        assert!(res.is_err());
        assert!(res.unwrap_err().to_string().contains("SimulSession"));
    }

    #[tokio::test]
    async fn synthesize_tier_a_returns_error() {
        let router = TtsRouter::new();
        let res = router
            .synthesize(Tier::A, "hi", "v", "en", &EmotionHint::default())
            .await;
        assert!(res.is_err());
    }

    #[tokio::test]
    async fn synthesize_unregistered_tier_errors_clearly() {
        let router = TtsRouter::new();
        let res = router
            .synthesize(Tier::C, "hi", "af_heart", "en", &EmotionHint::default())
            .await;
        assert!(res.is_err());
        let msg = res.unwrap_err().to_string();
        assert!(
            msg.contains("Kokoro"),
            "expected Kokoro mention, got: {msg}"
        );
    }

    #[tokio::test]
    async fn picker_returns_kokoro_voices_when_registered() {
        let router = TtsRouter::new().with_kokoro(Arc::new(KokoroEngine::with_defaults()));
        let cards = router.picker_voice_cards();
        // Kokoro ships 10 stock cards (5 ko + 5 en).
        assert_eq!(cards.len(), 10);
        assert!(cards.iter().all(|c| c.engine == "kokoro"));
    }

    #[tokio::test]
    async fn refresh_health_marks_unreachable_engines_not_ready() {
        // Both engines pointed at unreachable port — readiness should fall to false.
        let kokoro = Arc::new(KokoroEngine::new("http://127.0.0.1:1", "af_heart"));
        let cosy =
            Arc::new(CosyVoice2Engine::new("http://127.0.0.1:1", None).expect("engine constructs"));
        let router = TtsRouter::new().with_kokoro(kokoro).with_cosyvoice(cosy);
        let mut ctx = RoutingContext::default();
        ctx.kokoro_ready = true;
        ctx.cosyvoice_ready = true;
        router.refresh_engine_health(&mut ctx).await;
        assert!(!ctx.kokoro_ready);
        assert!(!ctx.cosyvoice_ready);
    }
}
