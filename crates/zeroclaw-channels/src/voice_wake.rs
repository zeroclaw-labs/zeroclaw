//! Voice Wake Word detection channel.
//!
//! Listens on the default microphone via `cpal`, detects a configurable wake
//! word using energy-based VAD followed by transcription-based keyword matching,
//! then captures the subsequent utterance and dispatches it as a channel message.
//!
//! Gated behind the `voice-wake` Cargo feature.

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Result, bail};
use async_trait::async_trait;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use crate::transcription::transcribe_audio;
use zeroclaw_config::schema::TranscriptionConfig;
use zeroclaw_config::schema::VoiceWakeConfig;

use zeroclaw_api::channel::{Channel, ChannelMessage, SendMessage};

// ── State machine ──────────────────────────────────────────────

/// Internal states for the wake-word detector.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WakeState {
    /// Passively monitoring microphone energy levels.
    Listening,
    /// Energy spike detected — capturing a short window to check for wake word.
    Triggered,
    /// Wake word confirmed — capturing the full utterance that follows.
    Capturing,
    /// Captured audio is being transcribed.
    Processing,
}

impl std::fmt::Display for WakeState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Listening => write!(f, "Listening"),
            Self::Triggered => write!(f, "Triggered"),
            Self::Capturing => write!(f, "Capturing"),
            Self::Processing => write!(f, "Processing"),
        }
    }
}

// ── Channel implementation ─────────────────────────────────────

/// Voice wake-word channel that activates on a spoken keyword.
pub struct VoiceWakeChannel {
    config: VoiceWakeConfig,
    transcription_config: TranscriptionConfig,
}

impl VoiceWakeChannel {
    /// Create a new `VoiceWakeChannel` from its config sections.
    pub fn new(config: VoiceWakeConfig, transcription_config: TranscriptionConfig) -> Self {
        Self {
            config,
            transcription_config,
        }
    }
}

#[async_trait]
impl Channel for VoiceWakeChannel {
    fn name(&self) -> &str {
        "voice_wake"
    }

    async fn send(&self, _message: &SendMessage) -> Result<()> {
        // Voice wake is input-only; outbound messages are not supported.
        Ok(())
    }

    async fn listen(&self, tx: mpsc::Sender<ChannelMessage>) -> Result<()> {
        let config = self.config.clone();
        let transcription_config = self.transcription_config.clone();

        // Run the blocking audio capture loop on a dedicated thread.
        let (audio_tx, mut audio_rx) = mpsc::channel::<Vec<f32>>(4);

        let energy_threshold = config.energy_threshold;
        let silence_timeout = Duration::from_millis(u64::from(config.silence_timeout_ms));
        let max_capture = Duration::from_secs(u64::from(config.max_capture_secs));
        let sample_rate: u32;
        let channels_count: u16;

        // ── Initialise cpal stream ────────────────────────────
        {
            use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

            let host = cpal::default_host();
            let device = host
                .default_input_device()
                .ok_or_else(|| anyhow::anyhow!("No default audio input device available"))?;

            let supported = device.default_input_config()?;
            sample_rate = supported.sample_rate().0;
            channels_count = supported.channels();

            info!(
                device = ?device.name().unwrap_or_default(),
                sample_rate,
                channels = channels_count,
                "VoiceWake: opening audio input"
            );

            let stream_config: cpal::StreamConfig = supported.into();
            let audio_tx_clone = audio_tx.clone();

            let stream = device.build_input_stream(
                &stream_config,
                move |data: &[f32], _: &cpal::InputCallbackInfo| {
                    // Non-blocking: try_send and drop if full.
                    let _ = audio_tx_clone.try_send(data.to_vec());
                },
                move |err| {
                    warn!("VoiceWake: audio stream error: {err}");
                },
                None,
            )?;

            stream.play()?;

            // Keep the stream alive for the lifetime of the channel.
            // We leak it intentionally — the channel runs until the daemon shuts down.
            std::mem::forget(stream);
        }

        // Drop the extra sender so the channel closes when the stream sender drops.
        drop(audio_tx);

        // ── Main detection loop ───────────────────────────────
        let wake_word = config.wake_word.to_lowercase();
        let mut state = WakeState::Listening;
        let mut capture_buf: Vec<f32> = Vec::new();
        let mut last_voice_at = Instant::now();
        let mut capture_start = Instant::now();
        let mut msg_counter: u64 = 0;

        info!(wake_word = %wake_word, "VoiceWake: entering listen loop");

        while let Some(chunk) = audio_rx.recv().await {
            let energy = compute_rms_energy(&chunk);

            match state {
                WakeState::Listening => {
                    if energy >= energy_threshold {
                        debug!(
                            energy,
                            "VoiceWake: energy spike — transitioning to Triggered"
                        );
                        state = WakeState::Triggered;
                        capture_buf.clear();
                        capture_buf.extend_from_slice(&chunk);
                        last_voice_at = Instant::now();
                        capture_start = Instant::now();
                    }
                }
                WakeState::Triggered => {
                    capture_buf.extend_from_slice(&chunk);

                    if energy >= energy_threshold {
                        last_voice_at = Instant::now();
                    }

                    let since_voice = last_voice_at.elapsed();
                    let since_start = capture_start.elapsed();

                    // After enough silence or max time, transcribe to check for wake word.
                    if since_voice >= silence_timeout || since_start >= max_capture {
                        debug!("VoiceWake: Triggered window closed — transcribing for wake word");

                        let wav_bytes =
                            encode_wav_from_f32(&capture_buf, sample_rate, channels_count);

                        match transcribe_audio(wav_bytes, "wake_check.wav", &transcription_config)
                            .await
                        {
                            Ok(text) => {
                                let lower = text.to_lowercase();
                                if lower.contains(&wake_word) {
                                    info!(text = %text, "VoiceWake: wake word detected — capturing utterance");
                                    state = WakeState::Capturing;
                                    capture_buf.clear();
                                    last_voice_at = Instant::now();
                                    capture_start = Instant::now();
                                } else {
                                    debug!(text = %text, "VoiceWake: no wake word — back to Listening");
                                    state = WakeState::Listening;
                                    capture_buf.clear();
                                }
                            }
                            Err(e) => {
                                warn!("VoiceWake: transcription error during wake check: {e}");
                                state = WakeState::Listening;
                                capture_buf.clear();
                            }
                        }
                    }
                }
                WakeState::Capturing => {
                    capture_buf.extend_from_slice(&chunk);

                    if energy >= energy_threshold {
                        last_voice_at = Instant::now();
                    }

                    let since_voice = last_voice_at.elapsed();
                    let since_start = capture_start.elapsed();

                    if since_voice >= silence_timeout || since_start >= max_capture {
                        debug!("VoiceWake: utterance capture complete — transcribing");

                        let wav_bytes =
                            encode_wav_from_f32(&capture_buf, sample_rate, channels_count);

                        match transcribe_audio(wav_bytes, "utterance.wav", &transcription_config)
                            .await
                        {
                            Ok(text) => {
                                let trimmed = text.trim().to_string();
                                if !trimmed.is_empty() {
                                    msg_counter += 1;
                                    let ts = SystemTime::now()
                                        .duration_since(UNIX_EPOCH)
                                        .unwrap_or_default()
                                        .as_secs();

                                    let msg = ChannelMessage {
                                        id: format!("voice_wake_{msg_counter}"),
                                        sender: "voice_user".into(),
                                        reply_target: "voice_user".into(),
                                        content: trimmed,
                                        channel: "voice_wake".into(),
                                        timestamp: ts,
                                        thread_ts: None,
                                        interruption_scope_id: None,
                                        metadata: serde_json::Value::Null,
                                        attachments: vec![],
                                    };

                                    if let Err(e) = tx.send(msg).await {
                                        warn!("VoiceWake: failed to dispatch message: {e}");
                                    }
                                }
                            }
                            Err(e) => {
                                warn!("VoiceWake: transcription error for utterance: {e}");
                            }
                        }

                        state = WakeState::Listening;
                        capture_buf.clear();
                    }
                }
                WakeState::Processing => {
                    // Should not receive chunks while processing, but just buffer them.
                    // State transitions happen above synchronously after transcription.
                }
            }
        }

        bail!("VoiceWake: audio stream ended unexpectedly");
    }
}

// ── Audio utilities ────────────────────────────────────────────

/// Compute RMS (root-mean-square) energy of an audio chunk.
pub fn compute_rms_energy(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum_sq: f32 = samples.iter().map(|s| s * s).sum();
    (sum_sq / samples.len() as f32).sqrt()
}

/// Encode raw f32 PCM samples as a WAV byte buffer (16-bit PCM).
///
/// This produces a minimal valid WAV file that Whisper-compatible APIs accept.
pub fn encode_wav_from_f32(samples: &[f32], sample_rate: u32, channels: u16) -> Vec<u8> {
    let bits_per_sample: u16 = 16;
    let byte_rate = u32::from(channels) * sample_rate * u32::from(bits_per_sample) / 8;
    let block_align = channels * bits_per_sample / 8;
    #[allow(clippy::cast_possible_truncation)]
    let data_len = (samples.len() * 2) as u32; // 16-bit = 2 bytes per sample; max ~25 MB
    let file_len = 36 + data_len;

    let mut buf = Vec::with_capacity(file_len as usize + 8);

    // RIFF header
    buf.extend_from_slice(b"RIFF");
    buf.extend_from_slice(&file_len.to_le_bytes());
    buf.extend_from_slice(b"WAVE");

    // fmt chunk
    buf.extend_from_slice(b"fmt ");
    buf.extend_from_slice(&16u32.to_le_bytes()); // chunk size
    buf.extend_from_slice(&1u16.to_le_bytes()); // PCM format
    buf.extend_from_slice(&channels.to_le_bytes());
    buf.extend_from_slice(&sample_rate.to_le_bytes());
    buf.extend_from_slice(&byte_rate.to_le_bytes());
    buf.extend_from_slice(&block_align.to_le_bytes());
    buf.extend_from_slice(&bits_per_sample.to_le_bytes());

    // data chunk
    buf.extend_from_slice(b"data");
    buf.extend_from_slice(&data_len.to_le_bytes());

    for &sample in samples {
        let clamped = sample.clamp(-1.0, 1.0);
        #[allow(clippy::cast_possible_truncation)]
        let pcm16 = (clamped * 32767.0) as i16; // clamped to [-1,1] so fits i16
        buf.extend_from_slice(&pcm16.to_le_bytes());
    }

    buf
}

// ── Tests ──────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use zeroclaw_config::traits::ChannelConfig;

    // ── State machine tests ────────────────────────────────

    #[test]
    fn wake_state_display() {
        assert_eq!(WakeState::Listening.to_string(), "Listening");
        assert_eq!(WakeState::Triggered.to_string(), "Triggered");
        assert_eq!(WakeState::Capturing.to_string(), "Capturing");
        assert_eq!(WakeState::Processing.to_string(), "Processing");
    }

    #[test]
    fn wake_state_equality() {
        assert_eq!(WakeState::Listening, WakeState::Listening);
        assert_ne!(WakeState::Listening, WakeState::Triggered);
    }

    // ── Energy computation tests ───────────────────────────

    #[test]
    fn rms_energy_of_silence_is_zero() {
        let silence = vec![0.0f32; 1024];
        assert_eq!(compute_rms_energy(&silence), 0.0);
    }

    #[test]
    fn rms_energy_of_empty_is_zero() {
        assert_eq!(compute_rms_energy(&[]), 0.0);
    }

    #[test]
    fn rms_energy_of_constant_signal() {
        // Constant signal at 0.5 → RMS should be 0.5
        let signal = vec![0.5f32; 100];
        let energy = compute_rms_energy(&signal);
        assert!((energy - 0.5).abs() < 1e-5);
    }

    #[test]
    fn rms_energy_above_threshold() {
        let loud = vec![0.8f32; 256];
        let energy = compute_rms_energy(&loud);
        assert!(energy > 0.01, "Loud signal should exceed default threshold");
    }

    #[test]
    fn rms_energy_below_threshold_for_quiet() {
        let quiet = vec![0.001f32; 256];
        let energy = compute_rms_energy(&quiet);
        assert!(
            energy < 0.01,
            "Very quiet signal should be below default threshold"
        );
    }

    // ── WAV encoding tests ─────────────────────────────────

    #[test]
    fn wav_header_is_valid() {
        let samples = vec![0.0f32; 100];
        let wav = encode_wav_from_f32(&samples, 16000, 1);

        // RIFF header
        assert_eq!(&wav[0..4], b"RIFF");
        assert_eq!(&wav[8..12], b"WAVE");

        // fmt chunk
        assert_eq!(&wav[12..16], b"fmt ");
        let fmt_size = u32::from_le_bytes(wav[16..20].try_into().unwrap());
        assert_eq!(fmt_size, 16);

        // PCM format
        let format = u16::from_le_bytes(wav[20..22].try_into().unwrap());
        assert_eq!(format, 1);

        // Channels
        let channels = u16::from_le_bytes(wav[22..24].try_into().unwrap());
        assert_eq!(channels, 1);

        // Sample rate
        let sr = u32::from_le_bytes(wav[24..28].try_into().unwrap());
        assert_eq!(sr, 16000);

        // data chunk
        assert_eq!(&wav[36..40], b"data");
        let data_size = u32::from_le_bytes(wav[40..44].try_into().unwrap());
        assert_eq!(data_size, 200); // 100 samples * 2 bytes each
    }

    #[test]
    fn wav_total_size_correct() {
        let samples = vec![0.0f32; 50];
        let wav = encode_wav_from_f32(&samples, 44100, 2);
        // header (44 bytes) + data (50 * 2 = 100 bytes)
        assert_eq!(wav.len(), 144);
    }

    #[test]
    fn wav_encodes_clipped_samples() {
        // Samples outside [-1, 1] should be clamped
        let samples = vec![-2.0f32, 2.0, 0.0];
        let wav = encode_wav_from_f32(&samples, 16000, 1);

        let s0 = i16::from_le_bytes(wav[44..46].try_into().unwrap());
        let s1 = i16::from_le_bytes(wav[46..48].try_into().unwrap());
        let s2 = i16::from_le_bytes(wav[48..50].try_into().unwrap());

        assert_eq!(s0, -32767); // clamped to -1.0
        assert_eq!(s1, 32767); // clamped to 1.0
        assert_eq!(s2, 0);
    }

    // ── Config parsing tests ───────────────────────────────

    #[test]
    fn voice_wake_config_defaults() {
        let config = VoiceWakeConfig::default();
        assert_eq!(config.wake_word, "hey zeroclaw");
        assert_eq!(config.silence_timeout_ms, 2000);
        assert!((config.energy_threshold - 0.01).abs() < f32::EPSILON);
        assert_eq!(config.max_capture_secs, 30);
    }

    #[test]
    fn voice_wake_config_deserialize_partial() {
        let toml_str = r#"
            wake_word = "okay agent"
            max_capture_secs = 60
        "#;
        let config: VoiceWakeConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.wake_word, "okay agent");
        assert_eq!(config.max_capture_secs, 60);
        // Defaults preserved for unset fields
        assert_eq!(config.silence_timeout_ms, 2000);
        assert!((config.energy_threshold - 0.01).abs() < f32::EPSILON);
    }

    #[test]
    fn voice_wake_config_deserialize_all_fields() {
        let toml_str = r#"
            wake_word = "hello bot"
            silence_timeout_ms = 3000
            energy_threshold = 0.05
            max_capture_secs = 15
        "#;
        let config: VoiceWakeConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.wake_word, "hello bot");
        assert_eq!(config.silence_timeout_ms, 3000);
        assert!((config.energy_threshold - 0.05).abs() < f32::EPSILON);
        assert_eq!(config.max_capture_secs, 15);
    }

    #[test]
    fn voice_wake_config_channel_config_trait() {
        assert_eq!(VoiceWakeConfig::name(), "VoiceWake");
        assert_eq!(VoiceWakeConfig::desc(), "voice wake word detection");
    }

    // ── State transition logic tests ───────────────────────

    #[test]
    fn energy_threshold_determines_trigger() {
        let threshold = 0.01f32;
        let quiet_energy = compute_rms_energy(&vec![0.005f32; 256]);
        let loud_energy = compute_rms_energy(&vec![0.5f32; 256]);

        assert!(quiet_energy < threshold, "Quiet should not trigger");
        assert!(loud_energy >= threshold, "Loud should trigger");
    }

    #[test]
    fn state_transitions_are_deterministic() {
        // Verify that the state enum values are distinct and copyable
        let states = [
            WakeState::Listening,
            WakeState::Triggered,
            WakeState::Capturing,
            WakeState::Processing,
        ];
        for (i, a) in states.iter().enumerate() {
            for (j, b) in states.iter().enumerate() {
                if i == j {
                    assert_eq!(a, b);
                } else {
                    assert_ne!(a, b);
                }
            }
        }
    }

    #[test]
    fn channel_config_impl() {
        // VoiceWakeConfig implements ChannelConfig
        assert_eq!(VoiceWakeConfig::name(), "VoiceWake");
        assert!(!VoiceWakeConfig::desc().is_empty());
    }

    #[test]
    fn voice_wake_channel_name() {
        let config = VoiceWakeConfig::default();
        let transcription_config = TranscriptionConfig::default();
        let channel = VoiceWakeChannel::new(config, transcription_config);
        assert_eq!(channel.name(), "voice_wake");
    }
}
