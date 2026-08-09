//! Speak Tool - Text-to-speech via Piper
//! Converts text to speech using Piper TTS (fast, offline, runs on Pi).
//! Plays audio through the speaker.

use crate::config::RobotConfig;
use crate::traits::{Tool, ToolResult};
use anyhow::Result;
use async_trait::async_trait;
use serde_json::{Value, json};
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;
use tokio::process::{Child, Command};
use tokio::time::timeout;

/// Longest text this tool will synthesize. Also the input to the synthesis
/// deadline below, so the bound tracks the workload it has to admit.
const MAX_SPEECH_INPUT_BYTES: u64 = 1000;

/// Conservative floor on how densely text maps to speech. English prose runs
/// nearer 15 bytes per second of audio; assuming 10 over-states the worst-case
/// clip length, which is the safe direction for a deadline. At this rate the
/// input limit is 100 seconds of speech.
const SPEECH_BYTES_PER_SECOND: u64 = 10;

/// Piper's real-time factor for a medium voice on a Raspberry Pi 4 — the slowest
/// hardware this crate claims to support, and the size of the default
/// `en_US-lessac-medium` voice. Published as 0.803; carried as a percentage to
/// keep the derivation in integer const arithmetic, rounded up.
/// Benchmark: <https://github.com/rhasspy/piper/issues/33>
const PIPER_PI4_RTF_PERCENT: u64 = 81;

/// One-time cost before synthesis starts: loading the ONNX voice, from SD card
/// on a cold Pi. Not covered by the real-time factor, which measures synthesis
/// alone.
const PIPER_MODEL_LOAD_SECS: u64 = 30;

/// Doubling of the derived worst case. The deadline exists to catch a wedged
/// process, so it should sit well clear of legitimate slow work — thermal
/// throttling and IO contention on a loaded Pi are normal, not wedged.
const PIPER_SAFETY_FACTOR: u64 = 2;

/// Wall-clock bound on Piper speech synthesis, derived from the largest input
/// the tool accepts on the slowest supported hardware rather than picked flat:
/// worst-case clip length x the Pi 4 real-time factor, plus model load, doubled.
/// Exceeding it means the process is wedged rather than merely slow.
const PIPER_SYNTH_TIMEOUT: Duration = Duration::from_secs(
    (MAX_SPEECH_INPUT_BYTES / SPEECH_BYTES_PER_SECOND * PIPER_PI4_RTF_PERCENT / 100
        + PIPER_MODEL_LOAD_SECS)
        * PIPER_SAFETY_FACTOR,
);

/// Wall-clock bound on audio playback. Playback legitimately lasts as long as
/// the clip — at most `MAX_SPEECH_INPUT_BYTES / SPEECH_BYTES_PER_SECOND`
/// seconds — so this ceiling clears that by a wide margin and only trips on a
/// stuck audio device rather than on real speech.
const AUDIO_PLAYBACK_TIMEOUT: Duration = Duration::from_secs(300);

pub struct SpeakTool {
    config: RobotConfig,
    audio_dir: PathBuf,
}

impl SpeakTool {
    pub fn new(config: RobotConfig) -> Self {
        let audio_dir = directories::UserDirs::new()
            .map(|d| d.home_dir().join(".zeroclaw/tts_cache"))
            .unwrap_or_else(|| PathBuf::from("/tmp/zeroclaw_tts"));

        let _ = std::fs::create_dir_all(&audio_dir);

        Self { config, audio_dir }
    }

    /// Generate speech using Piper and play it
    async fn speak(&self, text: &str, emotion: &str) -> Result<()> {
        let piper_path = &self.config.audio.piper_path;
        let voice = &self.config.audio.piper_voice;
        let speaker_device = &self.config.audio.speaker_device;

        // Model path
        let model_path = directories::UserDirs::new()
            .map(|d| {
                d.home_dir()
                    .join(format!(".zeroclaw/models/piper/{}.onnx", voice))
            })
            .unwrap_or_else(|| PathBuf::from(format!("/usr/local/share/piper/{}.onnx", voice)));

        // Adjust text based on emotion (simple SSML-like modifications)
        let processed_text = match emotion {
            "excited" => format!("{}!", text.trim_end_matches('.')),
            "sad" => text.to_string(), // Piper doesn't support prosody, but we keep the hook
            "whisper" => text.to_string(),
            _ => text.to_string(),
        };

        // Generate WAV file
        let output_path = self.audio_dir.join("speech.wav");

        // Pipe text to piper, output to WAV. Piper writes the audio to
        // `--output_file`, so its own stdout/stderr are detached: they are never
        // surfaced to the caller and must not block on or pollute our stdio.
        let mut piper = Command::new(piper_path)
            .args([
                "--model",
                model_path.to_str().unwrap(),
                "--output_file",
                output_path.to_str().unwrap(),
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()?;

        // Write text to stdin
        if let Some(mut stdin) = piper.stdin.take() {
            use tokio::io::AsyncWriteExt;
            stdin.write_all(processed_text.as_bytes()).await?;
        }

        let status =
            wait_for_child_with_timeout(&mut piper, "Piper TTS", PIPER_SYNTH_TIMEOUT).await?;
        if !status.success() {
            anyhow::bail!("Piper TTS failed");
        }

        // Play audio using aplay
        let mut aplay = Command::new("aplay");
        aplay.args(["-D", speaker_device, output_path.to_str().unwrap()]);
        let play_result =
            run_audio_command_with_timeout(aplay, "aplay", AUDIO_PLAYBACK_TIMEOUT).await?;

        if !play_result.status.success() {
            // Fallback: try paplay (PulseAudio)
            let mut paplay = Command::new("paplay");
            paplay.arg(output_path.to_str().unwrap());
            let fallback =
                run_audio_command_with_timeout(paplay, "paplay", AUDIO_PLAYBACK_TIMEOUT).await?;

            if !fallback.status.success() {
                anyhow::bail!(
                    "Audio playback failed. Tried aplay and paplay.\n{}",
                    String::from_utf8_lossy(&play_result.stderr)
                );
            }
        }

        Ok(())
    }

    /// Play a sound effect
    async fn play_sound(&self, sound: &str) -> Result<()> {
        let sounds_dir = directories::UserDirs::new()
            .map(|d| d.home_dir().join(".zeroclaw/sounds"))
            .unwrap_or_else(|| PathBuf::from("/usr/local/share/zeroclaw/sounds"));

        let sound_file = sounds_dir.join(format!("{}.wav", sound));

        if !sound_file.exists() {
            anyhow::bail!("Sound file not found: {}", sound_file.display());
        }

        let speaker_device = &self.config.audio.speaker_device;
        let mut aplay = Command::new("aplay");
        aplay.args(["-D", speaker_device, sound_file.to_str().unwrap()]);
        let output = run_audio_command_with_timeout(aplay, "aplay", AUDIO_PLAYBACK_TIMEOUT).await?;

        if !output.status.success() {
            anyhow::bail!("Sound playback failed");
        }

        Ok(())
    }
}

/// Wait for an already-spawned child, bounded by `deadline`. A child that
/// outlives the deadline is force-killed and reaped in one bounded step
/// (`Child::kill`), so a wedged process surfaces as an error instead of hanging
/// the caller forever.
async fn wait_for_child_with_timeout(
    child: &mut Child,
    label: &str,
    deadline: Duration,
) -> Result<std::process::ExitStatus> {
    match timeout(deadline, child.wait()).await {
        Ok(status) => Ok(status?),
        Err(_) => {
            if let Err(error) = child.kill().await {
                anyhow::bail!(
                    "{label} timed out after {deadline:?} and could not be killed: {error}"
                );
            }
            anyhow::bail!("{label} timed out after {deadline:?}");
        }
    }
}

/// Run an audio command to completion with detached stdin, `kill_on_drop(true)`,
/// and a wall-clock `deadline`. Audio players do not read stdin, so it is
/// detached to stop them blocking on the parent's; dropping the timed-out
/// `output()` future kills the child.
async fn run_audio_command_with_timeout(
    mut command: Command,
    label: &str,
    deadline: Duration,
) -> Result<std::process::Output> {
    command.stdin(Stdio::null()).kill_on_drop(true);

    match timeout(deadline, command.output()).await {
        Ok(output) => Ok(output?),
        Err(_) => anyhow::bail!("{label} timed out after {deadline:?}"),
    }
}

#[async_trait]
impl Tool for SpeakTool {
    fn name(&self) -> &str {
        "speak"
    }

    fn description(&self) -> &str {
        "Speak text out loud using text-to-speech. The robot will say the given text \
         through its speaker. Can also play sound effects like 'beep', 'chime', 'laugh'."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "text": {
                    "type": "string",
                    "description": "The text to speak out loud"
                },
                "emotion": {
                    "type": "string",
                    "enum": ["neutral", "excited", "sad", "whisper"],
                    "description": "Emotional tone. Default 'neutral'."
                },
                "sound": {
                    "type": "string",
                    "description": "Play a sound effect instead of speaking (e.g., 'beep', 'chime', 'laugh', 'alert')"
                }
            }
        })
    }

    async fn execute(&self, args: Value) -> Result<ToolResult> {
        // Check if playing a sound effect
        if let Some(sound) = args["sound"].as_str() {
            return match self.play_sound(sound).await {
                Ok(()) => Ok(ToolResult {
                    success: true,
                    output: format!("Played sound: {}", sound),
                    error: None,
                }),
                Err(e) => Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("Sound playback failed: {e}")),
                }),
            };
        }

        // Speak text
        let text = args["text"].as_str().ok_or_else(|| {
            anyhow::Error::msg("Missing 'text' parameter (or use 'sound' for effects)")
        })?;

        if text.is_empty() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Cannot speak empty text".to_string()),
            });
        }

        // Limit text length for safety
        if text.len() as u64 > MAX_SPEECH_INPUT_BYTES {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "Text too long (max {MAX_SPEECH_INPUT_BYTES} characters)"
                )),
            });
        }

        let emotion = args["emotion"].as_str().unwrap_or("neutral");

        match self.speak(text, emotion).await {
            Ok(()) => Ok(ToolResult {
                success: true,
                output: format!("Said: \"{}\"", text),
                error: None,
            }),
            Err(e) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Speech failed: {e}")),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn speak_tool_name() {
        let tool = SpeakTool::new(RobotConfig::default());
        assert_eq!(tool.name(), "speak");
    }

    #[test]
    fn speak_tool_schema() {
        let tool = SpeakTool::new(RobotConfig::default());
        let schema = tool.parameters_schema();
        assert!(schema["properties"]["text"].is_object());
        assert!(schema["properties"]["emotion"].is_object());
    }

    // A stalled TTS or audio subprocess must surface a bounded error instead of
    // hanging the caller forever.

    #[cfg(unix)]
    #[tokio::test]
    async fn child_wait_helper_times_out_and_kills_stalled_process() {
        let mut child = Command::new("sleep")
            .arg("60")
            .kill_on_drop(true)
            .spawn()
            .expect("spawn stalled child");

        let started = std::time::Instant::now();
        let error =
            wait_for_child_with_timeout(&mut child, "test synth", Duration::from_millis(20))
                .await
                .unwrap_err()
                .to_string();

        assert!(error.contains("timed out"), "unexpected error: {error}");
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "timeout helper waited too long: {:?}",
            started.elapsed()
        );
        // `Child::id` returns `None` only once the child has been reaped, so this
        // proves the timed-out process was killed and waited, not just abandoned.
        assert!(
            child.id().is_none(),
            "timed-out child was not killed and reaped"
        );
    }

    #[test]
    fn piper_timeout_admits_the_largest_supported_workload() {
        // Independent restatement of the worst case the deadline must admit: the
        // full input limit, spoken at the conservative byte rate, synthesized at
        // the Pi 4 real-time factor, plus a cold model load. Recomputed from
        // floats here so a transcription slip in the integer const arithmetic
        // above shows up as a failure rather than as a silently tighter bound.
        let worst_case_audio_secs = MAX_SPEECH_INPUT_BYTES as f64 / SPEECH_BYTES_PER_SECOND as f64;
        let worst_case_synth_secs = worst_case_audio_secs * 0.803 + PIPER_MODEL_LOAD_SECS as f64;

        assert!(
            PIPER_SYNTH_TIMEOUT.as_secs_f64() >= worst_case_synth_secs,
            "synthesis deadline {PIPER_SYNTH_TIMEOUT:?} rejects the largest supported \
             workload, which needs ~{worst_case_synth_secs:.0}s on a Pi 4"
        );
        // Playback must outlast the longest clip the tool can produce.
        assert!(
            AUDIO_PLAYBACK_TIMEOUT.as_secs_f64() > worst_case_audio_secs,
            "playback deadline {AUDIO_PLAYBACK_TIMEOUT:?} is shorter than the longest \
             clip this tool can generate (~{worst_case_audio_secs:.0}s)"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn child_wait_helper_admits_a_slow_but_healthy_synthesis() {
        // The deadline must distinguish wedged from slow. A Pi 4 synthesizing a
        // near-limit input legitimately runs well past half a minute, so this
        // drives a healthy child under the *production* constant for longer than
        // any flat sub-minute bound would have tolerated, and asserts it is
        // allowed to finish. It costs its own wall-clock time deliberately:
        // asserting the constant's value alone would not catch a deadline that
        // is applied to the wrong wait.
        let slow_but_healthy = Duration::from_secs(35);
        assert!(
            slow_but_healthy > Duration::from_secs(30),
            "this regression is only meaningful if it outlasts a flat 30s bound"
        );

        let mut child = Command::new("sleep")
            .arg(slow_but_healthy.as_secs().to_string())
            .kill_on_drop(true)
            .spawn()
            .expect("spawn slow child");

        let status = wait_for_child_with_timeout(&mut child, "test synth", PIPER_SYNTH_TIMEOUT)
            .await
            .expect("a slow but healthy synthesis must not be killed");

        assert!(status.success());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn child_wait_helper_allows_successful_process() {
        let mut child = Command::new("true").spawn().expect("spawn child");

        let status = wait_for_child_with_timeout(&mut child, "test synth", Duration::from_secs(5))
            .await
            .expect("child should finish within the deadline");

        assert!(status.success());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn audio_command_helper_times_out_stalled_process() {
        let mut command = Command::new("sleep");
        command.arg("60");

        let started = std::time::Instant::now();
        let error =
            run_audio_command_with_timeout(command, "test player", Duration::from_millis(20))
                .await
                .unwrap_err()
                .to_string();

        assert!(error.contains("timed out"), "unexpected error: {error}");
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "timeout helper waited too long: {:?}",
            started.elapsed()
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn audio_command_helper_allows_successful_process() {
        let command = Command::new("true");

        let output = run_audio_command_with_timeout(command, "test player", Duration::from_secs(5))
            .await
            .expect("player should finish within the deadline");

        assert!(output.status.success());
    }
}
