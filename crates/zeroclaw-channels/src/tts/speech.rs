//! Draft-owned speculative synthesis and private audio assembly.
use super::{FFMPEG_TRANSCODE_TIMEOUT, TtsManager, transcode_to_opus};
use anyhow::{Context, Result, bail};
use std::io::Write as _;

#[derive(Clone, Default)]
struct SpeechSnapshot {
    text: String,
    finished: bool,
}

/// Owns speculative synthesis for exactly one channel draft. The watch value
/// is the canonical latest text snapshot; no unbounded queue of token deltas
/// or detached synthesis tasks is retained. Dropping the owner cancels work.
pub(crate) struct SpeechJob {
    updates: tokio::sync::watch::Sender<SpeechSnapshot>,
    worker: tokio::task::JoinHandle<Result<Vec<Vec<u8>>>>,
}

impl Drop for SpeechJob {
    fn drop(&mut self) {
        self.worker.abort();
    }
}

impl SpeechJob {
    pub(crate) fn start(manager: std::sync::Arc<TtsManager>) -> Self {
        let (updates, mut rx) = tokio::sync::watch::channel(SpeechSnapshot::default());
        let worker = zeroclaw_spawn::spawn!(async move {
            let cfg = crate::speech_segment::SpeechSegmentConfig::default()
                .with_max_chars(manager.max_text_length);
            let mut completed: Vec<String> = Vec::new();
            let mut audio = Vec::new();
            loop {
                let snapshot = rx.borrow_and_update().clone();
                let segments =
                    crate::speech_segment::ready_segments(&snapshot.text, &cfg, snapshot.finished);
                if !segments.starts_with(&completed) {
                    // A new assistant phase or final policy rewrite invalidates
                    // speculation. Nothing has been uploaded yet.
                    completed.clear();
                    audio.clear();
                }
                if let Some(segment) = segments.get(completed.len()) {
                    let part = manager.synthesize(segment).await?;
                    let latest = rx.borrow().clone();
                    let current =
                        crate::speech_segment::ready_segments(&latest.text, &cfg, latest.finished);
                    if current.starts_with(&completed)
                        && current.get(completed.len()) == Some(segment)
                    {
                        anyhow::ensure!(!part.is_empty(), "TTS returned empty audio");
                        completed.push(segment.clone());
                        audio.push(part);
                    }
                    continue;
                }
                if snapshot.finished {
                    return Ok(audio);
                }
                if rx.changed().await.is_err() {
                    anyhow::bail!("speech draft closed before finalization");
                }
            }
        });
        Self { updates, worker }
    }

    pub(crate) fn update(&self, text: &str) {
        self.updates.send_replace(SpeechSnapshot {
            text: text.to_owned(),
            finished: false,
        });
    }

    pub(crate) async fn finish(mut self, text: &str) -> Result<Vec<Vec<u8>>> {
        self.updates.send_replace(SpeechSnapshot {
            text: text.to_owned(),
            finished: true,
        });
        (&mut self.worker)
            .await
            .context("speech draft worker failed")?
    }
}

/// File extension for a provider output format, used when staging parts for
/// the concat demuxer (ffmpeg probes by extension for some containers).
fn audio_extension(format: &str) -> &'static str {
    match format.to_ascii_lowercase().as_str() {
        "wav" | "pcm" => "wav",
        "mp3" => "mp3",
        "ogg" => "ogg",
        "opus" => "opus",
        "ulaw" => "ulaw",
        _ => "audio",
    }
}

/// Concatenate provider-format audio blobs into one OGG/Opus stream.
///
/// Each part is a complete provider container. The concat demuxer reads its
/// headers and ffmpeg encodes the joined samples into one OGG/Opus container.
/// All parts come from the same provider format; raw headers are never appended.
pub(crate) async fn concat_parts_to_opus(parts: &[Vec<u8>], format: &str) -> Result<Vec<u8>> {
    concat_parts_with_command(parts, format, tokio::process::Command::new("ffmpeg")).await
}

async fn concat_parts_with_command(
    parts: &[Vec<u8>],
    format: &str,
    command: tokio::process::Command,
) -> Result<Vec<u8>> {
    let parts: Vec<&Vec<u8>> = parts.iter().filter(|p| !p.is_empty()).collect();
    anyhow::ensure!(!parts.is_empty(), "no audio parts to concatenate");

    // A declared Opus part can pass through. An OGG container alone does
    // not establish the codec, so every other format is transcoded.
    if parts.len() == 1 {
        let only = parts[0].clone();
        let fmt = format.to_ascii_lowercase();
        if fmt == "opus" {
            return Ok(only);
        }
        return transcode_to_opus(only).await;
    }

    let ext = audio_extension(format);
    // TempDir owns cleanup on success, errors, cancellation and unwinding.
    // Set the creation mode explicitly; tempfile directories otherwise inherit
    // the process umask while its NamedTempFile files default to 0600.
    let mut staging = tempfile::Builder::new();
    staging.prefix("zeroclaw-tts-concat-");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        staging.permissions(std::fs::Permissions::from_mode(0o700));
    }
    let dir = staging
        .tempdir()
        .context("failed to create private concat staging directory")?;
    stage_and_concat(dir.path(), &parts, ext, command).await
}

/// Keep private staged files alive until the owned ffmpeg operation completes.
async fn stage_and_concat(
    dir: &std::path::Path,
    parts: &[&Vec<u8>],
    ext: &str,
    mut command: tokio::process::Command,
) -> Result<Vec<u8>> {
    let mut staged = Vec::with_capacity(parts.len());
    let mut list = String::new();
    for part in parts {
        // NamedTempFile also restricts the audio file itself (0600 on Unix).
        let mut file = tempfile::Builder::new()
            .prefix("part-")
            .suffix(&format!(".{ext}"))
            .tempfile_in(dir)
            .context("failed to create private audio part")?;
        file.write_all(part).context("failed to stage audio part")?;
        let escaped = file.path().to_string_lossy().replace('\'', "'\\''");
        list.push_str(&format!("file '{escaped}'\n"));
        staged.push(file);
    }
    let mut list_file =
        tempfile::NamedTempFile::new_in(dir).context("failed to create private concat list")?;
    list_file
        .write_all(list.as_bytes())
        .context("failed to write concat list")?;
    let list_path = list_file.path();

    use std::process::Stdio;
    let child = command
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "concat",
            "-safe",
            "0",
            "-i",
        ])
        .arg(list_path)
        .args([
            "-f", "ogg", "-acodec", "libopus", "-b:a", "32k", "-vbr", "on", "pipe:1",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .context(
            "failed to spawn ffmpeg for concatenation — ensure ffmpeg with libopus support \
             is installed",
        )?;

    let output = tokio::time::timeout(FFMPEG_TRANSCODE_TIMEOUT, child.wait_with_output())
        .await
        .context("ffmpeg concat timed out")?
        .context("ffmpeg concat failed to run")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("ffmpeg concat to opus failed: {stderr}");
    }
    anyhow::ensure!(
        !output.stdout.is_empty(),
        "ffmpeg produced empty concat output — check that libopus is available"
    );
    Ok(output.stdout)
}

#[cfg(test)]
#[path = "speech_tests.rs"]
pub(crate) mod tests;
