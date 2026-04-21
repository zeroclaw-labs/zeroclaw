//! Voice Activity Detection trait, event types, and energy-based implementation.

use std::time::Instant;

/// Result of processing a chunk of audio samples through a VAD.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VadEvent {
    /// No speech detected; continue listening.
    Silence,
    /// Speech has just started.
    SpeechStart,
    /// Speech has just ended.
    SpeechEnd,
}

/// Pluggable Voice Activity Detector.
///
/// Implementations receive mono f32 samples and emit [`VadEvent`] transitions.
pub trait Vad: Send + Sync {
    /// Process a buffer of mono f32 samples and return the detected event.
    fn process(&mut self, samples: &[f32]) -> VadEvent;
}

/// No-op VAD that always reports silence.
///
/// Used when `gateway-voice-duplex` is enabled but no real VAD implementation
/// is configured. A real implementation (energy-based or webrtcvad) will
/// follow in a separate PR.
#[derive(Debug, Default)]
pub struct NoopVad;

impl Vad for NoopVad {
    fn process(&mut self, _samples: &[f32]) -> VadEvent {
        VadEvent::Silence
    }
}

// ── Energy-based VAD ─────────────────────────────────────────────

/// Internal speaking state for [`EnergyVad`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VoiceState {
    /// No speech detected.
    Silent,
    /// Speech is in progress.
    Speaking,
}

/// Energy-based Voice Activity Detector using RMS amplitude.
///
/// Computes the root-mean-square energy of each audio chunk and compares it
/// against a configurable threshold. Transitions to [`VadEvent::SpeechStart`]
/// when energy exceeds the threshold, and emits [`VadEvent::SpeechEnd`] after
/// sustained silence for `silence_timeout_ms` milliseconds.
///
/// This is the same algorithm used by the `voice_wake` channel but packaged
/// as a pluggable [`Vad`] implementation suitable for real-time WebSocket
/// voice duplex sessions.
#[derive(Debug)]
pub struct EnergyVad {
    /// RMS energy threshold above which audio is considered speech.
    energy_threshold: f32,
    /// Duration of sustained silence (ms) before declaring [`VadEvent::SpeechEnd`].
    silence_timeout_ms: u64,
    /// Current speaking state.
    state: VoiceState,
    /// Instant when speech was last detected above threshold.
    last_voice_at: Instant,
}

impl EnergyVad {
    /// Create a new energy-based VAD.
    ///
    /// # Arguments
    /// * `energy_threshold` — RMS energy level that constitutes speech (default 0.01).
    /// * `silence_timeout_ms` — Milliseconds of quiet before `SpeechEnd` (default 500).
    pub fn new(energy_threshold: f32, silence_timeout_ms: u64) -> Self {
        Self {
            energy_threshold,
            silence_timeout_ms,
            state: VoiceState::Silent,
            last_voice_at: Instant::now(),
        }
    }
}

impl Vad for EnergyVad {
    fn process(&mut self, samples: &[f32]) -> VadEvent {
        let energy = compute_rms_energy(samples);

        match self.state {
            VoiceState::Silent => {
                if energy >= self.energy_threshold {
                    self.state = VoiceState::Speaking;
                    self.last_voice_at = Instant::now();
                    VadEvent::SpeechStart
                } else {
                    VadEvent::Silence
                }
            }
            VoiceState::Speaking => {
                if energy >= self.energy_threshold {
                    // Still hearing voice — reset the silence timer.
                    self.last_voice_at = Instant::now();
                    VadEvent::Silence
                } else {
                    // Quiet — check if silence timeout has elapsed.
                    let elapsed = self.last_voice_at.elapsed();
                    if elapsed.as_millis() >= u128::from(self.silence_timeout_ms) {
                        self.state = VoiceState::Silent;
                        VadEvent::SpeechEnd
                    } else {
                        // Brief pause within speech — don't emit event yet.
                        VadEvent::Silence
                    }
                }
            }
        }
    }
}

/// Compute RMS (root-mean-square) energy of an audio chunk.
///
/// Returns 0.0 for empty input.
pub fn compute_rms_energy(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum_sq: f32 = samples.iter().map(|s| s * s).sum();
    (sum_sq / samples.len() as f32).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── NoopVad tests ──

    #[test]
    fn noop_vad_always_silence() {
        let mut vad = NoopVad;
        assert_eq!(vad.process(&[0.0; 160]), VadEvent::Silence);
        assert_eq!(vad.process(&[0.5; 160]), VadEvent::Silence);
    }

    // ── compute_rms_energy tests ──

    #[test]
    fn rms_energy_of_silence_is_zero() {
        assert_eq!(compute_rms_energy(&[0.0f32; 1024]), 0.0);
    }

    #[test]
    fn rms_energy_of_empty_is_zero() {
        assert_eq!(compute_rms_energy(&[]), 0.0);
    }

    #[test]
    fn rms_energy_of_constant_signal() {
        // Constant signal at 0.5 → RMS should be 0.5
        let energy = compute_rms_energy(&[0.5f32; 100]);
        assert!((energy - 0.5).abs() < 1e-5);
    }

    #[test]
    fn rms_energy_above_threshold() {
        let energy = compute_rms_energy(&[0.8f32; 256]);
        assert!(energy > 0.01, "Loud signal should exceed default threshold");
    }

    #[test]
    fn rms_energy_below_threshold_for_quiet() {
        let energy = compute_rms_energy(&[0.001f32; 256]);
        assert!(
            energy < 0.01,
            "Very quiet signal should be below default threshold"
        );
    }

    // ── EnergyVad state transition tests ──

    #[test]
    fn energy_vad_speech_start_on_loud_input() {
        let mut vad = EnergyVad::new(0.01, 500);
        let loud = vec![0.5f32; 160];
        assert_eq!(vad.process(&loud), VadEvent::SpeechStart);
    }

    #[test]
    fn energy_vad_silence_on_quiet_input() {
        let mut vad = EnergyVad::new(0.01, 500);
        let quiet = vec![0.001f32; 160];
        assert_eq!(vad.process(&quiet), VadEvent::Silence);
    }

    #[test]
    fn energy_vad_speech_end_after_timeout() {
        let mut vad = EnergyVad::new(0.01, 500);
        let loud = vec![0.5f32; 160];
        let quiet = vec![0.001f32; 160];

        // Start speech
        assert_eq!(vad.process(&loud), VadEvent::SpeechStart);

        // Simulate silence timeout by rewinding the internal clock
        vad.last_voice_at = Instant::now() - std::time::Duration::from_millis(600);

        // Now quiet should trigger SpeechEnd
        assert_eq!(vad.process(&quiet), VadEvent::SpeechEnd);
    }

    #[test]
    fn energy_vad_no_speech_end_before_timeout() {
        let mut vad = EnergyVad::new(0.01, 500);
        let loud = vec![0.5f32; 160];
        let quiet = vec![0.001f32; 160];

        // Start speech
        assert_eq!(vad.process(&loud), VadEvent::SpeechStart);

        // Quiet but not yet timed out
        assert_eq!(vad.process(&quiet), VadEvent::Silence);
    }

    #[test]
    fn energy_vad_resets_timeout_on_continuous_speech() {
        let mut vad = EnergyVad::new(0.01, 500);
        let loud = vec![0.5f32; 160];
        let quiet = vec![0.001f32; 160];

        // Start speech
        assert_eq!(vad.process(&loud), VadEvent::SpeechStart);

        // Continuous loud input keeps extending the timeout
        for _ in 0..10 {
            // Simulate time passing but still speaking
            vad.last_voice_at = Instant::now() - std::time::Duration::from_millis(200);
            assert_eq!(vad.process(&loud), VadEvent::Silence);
        }

        // Now go quiet and let timeout expire
        vad.last_voice_at = Instant::now() - std::time::Duration::from_millis(600);
        assert_eq!(vad.process(&quiet), VadEvent::SpeechEnd);
    }

    #[test]
    fn energy_vad_full_cycle_start_silence_end() {
        let mut vad = EnergyVad::new(0.01, 500);
        let loud = vec![0.5f32; 160];
        let quiet = vec![0.001f32; 160];

        // Silent → Speaking
        assert_eq!(vad.process(&loud), VadEvent::SpeechStart);
        // Speaking continues
        assert_eq!(vad.process(&loud), VadEvent::Silence);
        // Brief quiet
        assert_eq!(vad.process(&quiet), VadEvent::Silence);
        // Timeout
        vad.last_voice_at = Instant::now() - std::time::Duration::from_millis(600);
        assert_eq!(vad.process(&quiet), VadEvent::SpeechEnd);
        // Back to silent
        assert_eq!(vad.process(&quiet), VadEvent::Silence);
    }

    #[test]
    fn energy_vad_speech_start_again_after_end() {
        let mut vad = EnergyVad::new(0.01, 500);
        let loud = vec![0.5f32; 160];
        let quiet = vec![0.001f32; 160];

        // First utterance
        assert_eq!(vad.process(&loud), VadEvent::SpeechStart);
        vad.last_voice_at = Instant::now() - std::time::Duration::from_millis(600);
        assert_eq!(vad.process(&quiet), VadEvent::SpeechEnd);

        // Second utterance
        assert_eq!(vad.process(&loud), VadEvent::SpeechStart);
    }

    #[test]
    fn energy_vad_custom_threshold() {
        // High threshold — normal speech shouldn't trigger
        let mut vad = EnergyVad::new(0.5, 500);
        let moderate = vec![0.1f32; 160];
        assert_eq!(vad.process(&moderate), VadEvent::Silence);

        // Very loud should trigger
        let very_loud = vec![0.9f32; 160];
        assert_eq!(vad.process(&very_loud), VadEvent::SpeechStart);
    }

    #[test]
    fn energy_vad_custom_timeout() {
        // Very short timeout — speech ends quickly
        let mut vad = EnergyVad::new(0.01, 50);
        let loud = vec![0.5f32; 160];
        let quiet = vec![0.001f32; 160];

        assert_eq!(vad.process(&loud), VadEvent::SpeechStart);
        vad.last_voice_at = Instant::now() - std::time::Duration::from_millis(60);
        assert_eq!(vad.process(&quiet), VadEvent::SpeechEnd);
    }
}
