//! Voice Activity Detection trait and event types.

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn noop_vad_always_silence() {
        let mut vad = NoopVad;
        assert_eq!(vad.process(&[0.0; 160]), VadEvent::Silence);
        assert_eq!(vad.process(&[0.5; 160]), VadEvent::Silence);
    }
}
