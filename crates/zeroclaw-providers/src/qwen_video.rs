//! Qwen Chat Completions video-file input. Files remain files until dispatch.
use anyhow::{Context, bail, ensure};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use std::sync::Arc;
use zeroclaw_config::policy::SecurityPolicy;

tokio::task_local! {
    static VIDEO_READ_POLICY: Option<Arc<SecurityPolicy>>;
}

/// Apply the current turn's filesystem policy to local video reads.
/// Unscoped calls accept inline bytes but cannot open local paths.
pub async fn with_video_read_policy<F: std::future::Future>(
    policy: Option<Arc<SecurityPolicy>>,
    future: F,
) -> F::Output {
    VIDEO_READ_POLICY.scope(policy, future).await
}

pub(crate) fn current_read_policy() -> Option<Arc<SecurityPolicy>> {
    VIDEO_READ_POLICY.try_with(Clone::clone).ok().flatten()
}
use zeroclaw_api::media::MarkerKind;
use zeroclaw_api::model_provider::ChatMessage;
use zeroclaw_api::tool_carrier::{classify, is_tool_result_carrier, rebuild_carrier};

// Qwen's compatible API documents raw Base64 video files strictly below 7 MB.
const MAX_VIDEO_BYTES: usize = 7_000_000 - 1;
const PREFIX: &str = "[VIDEO:";
const DATA_PREFIX: &str = "data:video/mp4;base64,";

pub(crate) fn supports_model(model: &str) -> bool {
    // Keep the transport scoped to documented video-capable model families.
    [
        "qwen4-max",
        "qwen3.8-flash",
        "qwen3.8-max",
        "qwen3.7-plus",
        "qwen3-vl-plus",
        "qwen3-vl-flash",
    ]
    .iter()
    .any(|name| {
        model == *name
            || model.strip_prefix(name).is_some_and(|suffix| {
                suffix.strip_prefix('-').is_some_and(|date| {
                    matches!(date.len(), 4 | 8) && date.bytes().all(|byte| byte.is_ascii_digit())
                })
            })
    })
}

/// A dispatch-local result. Only successfully loaded references become videos;
/// retained text is never scanned again by the wire serializer.
pub(crate) struct PreparedVideoMessage {
    pub text: String,
    pub videos: Vec<String>,
}

pub(crate) async fn prepare_messages(
    messages: &[ChatMessage],
    model: &str,
) -> Vec<PreparedVideoMessage> {
    let mut remaining_bytes = MAX_VIDEO_BYTES;
    let current_tools = crate::multimodal::current_turn_tool_result_indices(messages);
    let mut prepared = Vec::with_capacity(messages.len());
    for (index, message) in messages.iter().enumerate().rev() {
        let mut result = PreparedVideoMessage {
            text: message.content.clone(),
            videos: Vec::new(),
        };
        if let Some(mut carrier) = classify(&message.role, &message.content) {
            if !carrier.declared
                || !carrier
                    .attachments
                    .iter()
                    .any(|a| a.kind == MarkerKind::Video)
            {
                prepared.push(result);
                continue;
            }
            let mut attachments = Vec::with_capacity(carrier.attachments.len());
            for mut attachment in std::mem::take(&mut carrier.attachments) {
                if attachment.kind != MarkerKind::Video {
                    attachments.push(attachment);
                    continue;
                }
                let unavailable = if !current_tools.contains(&index) {
                    Some("video omitted from an earlier user turn".to_string())
                } else if !supports_model(model) {
                    Some("Qwen model does not support video-file input".to_string())
                } else {
                    match load_video(&attachment.target, remaining_bytes).await {
                        Ok(bytes) => {
                            remaining_bytes -= bytes.len();
                            attachment.target = format!("{DATA_PREFIX}{}", STANDARD.encode(bytes));
                            attachments.push(attachment);
                            None
                        }
                        Err(error) => Some(format!("video unavailable: {error:#}")),
                    }
                };
                if let Some(notice) = unavailable {
                    carrier.text.push_str(&format!("\n[tool {notice}]"));
                }
            }
            result.text = rebuild_carrier(&message.role, &message.content, &carrier, &attachments);
            prepared.push(result);
            continue;
        }
        if message.role == "user"
            && !is_tool_result_carrier(&message.role, &message.content)
            && supports_model(model)
        {
            let mut remaining = message.content.as_str();
            let mut text = String::new();
            while let Some(start) = remaining.find(PREFIX) {
                let Some(end) = remaining[start + PREFIX.len()..].find(']') else {
                    break;
                };
                let end = start + PREFIX.len() + end + 1;
                text.push_str(&remaining[..start]);
                let marker = &remaining[start..end];
                let source = marker[PREFIX.len()..marker.len() - 1].trim();
                match load_video(source, remaining_bytes).await {
                    Ok(bytes) => {
                        remaining_bytes -= bytes.len();
                        result
                            .videos
                            .push(format!("{DATA_PREFIX}{}", STANDARD.encode(bytes)));
                    }
                    Err(_) => text.push_str(marker),
                }
                remaining = &remaining[end..];
            }
            text.push_str(remaining);
            result.text = text;
        }
        prepared.push(result);
    }
    prepared.reverse();
    prepared
}

async fn load_video(source: &str, limit: usize) -> anyhow::Result<Vec<u8>> {
    let bytes = if let Some(encoded) = source.strip_prefix(DATA_PREFIX) {
        ensure!(
            encoded.len() <= limit.div_ceil(3) * 4,
            "Qwen video input exceeds byte budget"
        );
        STANDARD
            .decode(encoded)
            .context("Invalid Qwen video Base64")?
    } else {
        ensure!(
            !source.contains("://") && !source.starts_with("data:"),
            "Qwen video input requires a local MP4 path or MP4 Data URL"
        );
        let policy = current_read_policy()
            .context("Local Qwen video input requires an agent filesystem policy")?;
        let source = source.to_string();
        tokio::task::spawn_blocking(move || read_local_video(&source, limit, &policy))
            .await
            .context("Qwen video reader task failed")??
    };
    ensure!(bytes.len() <= limit, "Qwen video input exceeds byte budget");
    // This checks the MP4 container signature, not codec decodability.
    if !zeroclaw_api::media::is_mp4_container(&bytes) {
        bail!("Qwen video input is not an MP4 container");
    }
    Ok(bytes)
}

fn read_local_video(
    source: &str,
    limit: usize,
    policy: &SecurityPolicy,
) -> anyhow::Result<Vec<u8>> {
    use std::io::Read;

    ensure!(
        policy.is_path_allowed(source),
        "Qwen video path is denied by agent policy"
    );
    let resolved = policy
        .resolve_policy_target(&policy.resolve_tool_path(source))
        .context("Cannot resolve Qwen video file")?;
    ensure!(
        policy.is_resolved_path_readable(&resolved),
        "Qwen video path is denied by agent policy"
    );
    let file = open_video_file_nofollow(&resolved)?;
    let metadata = file.metadata().context("Cannot inspect Qwen video file")?;
    ensure!(
        metadata.is_file() && metadata.len() <= limit as u64,
        "Qwen video input exceeds byte budget or is not a regular file"
    );
    let mut bytes = Vec::new();
    file.take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .context("Cannot read Qwen video file")?;
    Ok(bytes)
}

fn open_video_file_nofollow(resolved: &std::path::Path) -> anyhow::Result<cap_std::fs::File> {
    #[cfg(unix)]
    use cap_fs_ext::OpenOptionsSyncExt;
    use cap_fs_ext::{DirExt, FollowSymlinks, OpenOptionsFollowExt};
    use cap_std::fs::{Dir, OpenOptions};
    use std::path::Component;

    let parent = resolved.parent().context("Qwen video file has no parent")?;
    let anchor = parent
        .ancestors()
        .last()
        .context("Qwen video path has no anchor")?;
    ensure!(anchor.is_absolute(), "Qwen video path must be resolved");
    let mut directory = Dir::open_ambient_dir(anchor, cap_std::ambient_authority())
        .context("Cannot open Qwen video filesystem anchor")?;
    // Walk every component without following replacements, including the
    // workspace root. A last-component-only check would still permit a
    // directory swap to redirect the validated read into a forbidden subtree.
    for component in parent.strip_prefix(anchor)?.components() {
        let Component::Normal(name) = component else {
            bail!("Qwen video path must be canonical");
        };
        directory = directory
            .open_dir_nofollow(name)
            .context("Qwen video directory changed or is unavailable")?;
    }
    let filename = resolved
        .file_name()
        .context("Qwen video path has no filename")?;
    let mut options = OpenOptions::new();
    options.read(true).follow(FollowSymlinks::No);
    #[cfg(unix)]
    options.nonblock(true);
    directory
        .open_with(filename, &options)
        .context("Cannot open Qwen video file")
}

#[cfg(test)]
mod tests {
    use super::*;

    const MP4: &[u8] = b"\0\0\0\x18ftypisom\0\0\0\0isommp42";

    #[tokio::test]
    async fn video_input_validation_is_bounded_and_checks_container() {
        let directory = tempfile::tempdir().unwrap();
        let policy = SecurityPolicy {
            workspace_dir: directory.path().to_path_buf(),
            forbidden_paths: Vec::new(),
            ..Default::default()
        };
        with_video_read_policy(Some(Arc::new(policy)), async {
            let path = directory.path().join("clip.mp4");
            std::fs::write(&path, MP4).unwrap();
            assert_eq!(
                load_video(path.to_str().unwrap(), MP4.len()).await.unwrap(),
                MP4
            );
            assert!(
                load_video(path.to_str().unwrap(), MP4.len() - 1)
                    .await
                    .is_err()
            );
            assert!(
                load_video("https://example.com/a.mp4", MAX_VIDEO_BYTES)
                    .await
                    .is_err()
            );
            assert!(
                load_video("data:video/mp4;base64,%%%", MAX_VIDEO_BYTES)
                    .await
                    .is_err()
            );
            let invalid = format!("{DATA_PREFIX}{}", STANDARD.encode(b"not a video"));
            assert!(load_video(&invalid, MAX_VIDEO_BYTES).await.is_err());
            let inline = format!("{DATA_PREFIX}{}", STANDARD.encode(MP4));
            assert_eq!(load_video(&inline, MAX_VIDEO_BYTES).await.unwrap(), MP4);
            assert!(
                load_video(directory.path().to_str().unwrap(), MAX_VIDEO_BYTES)
                    .await
                    .is_err()
            );
        })
        .await;
    }

    #[tokio::test]
    async fn video_input_preserves_unsupported_and_over_budget_markers() {
        let inline = format!("{DATA_PREFIX}{}", STANDARD.encode(MP4));
        let input = ChatMessage::user(format!("[VIDEO:{inline}]"));
        let expected = input.content.clone();
        let prepared = prepare_messages(&[input], "qwen-turbo").await;
        assert_eq!(prepared[0].text, expected);
        assert!(prepared[0].videos.is_empty());
        assert!(supports_model("qwen3.8-flash-0902"));
        assert!(!supports_model("qwen3.8-flash-invented"));
        let bytes = [MP4, &vec![0; MAX_VIDEO_BYTES / 2]].concat();
        let inline = format!("{DATA_PREFIX}{}", STANDARD.encode(bytes));
        let input = ChatMessage::user(format!("[VIDEO:{inline}] [VIDEO:{inline}]"));
        let prepared = prepare_messages(&[input], "qwen3.8-flash").await;
        assert_eq!(prepared[0].videos.len(), 1);
        assert!(prepared[0].text.contains(&format!("[VIDEO:{inline}]")));
    }
    #[tokio::test]
    async fn video_paths_obey_current_agent_policy() {
        let own = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        let outside = other.path().join("private.mp4");
        std::fs::write(&outside, MP4).unwrap();
        assert!(
            load_video(outside.to_str().unwrap(), MAX_VIDEO_BYTES)
                .await
                .is_err()
        );
        let policy = SecurityPolicy {
            workspace_dir: own.path().to_path_buf(),
            forbidden_paths: Vec::new(),
            ..Default::default()
        };
        with_video_read_policy(Some(Arc::new(policy)), async {
            assert!(
                load_video(outside.to_str().unwrap(), MAX_VIDEO_BYTES)
                    .await
                    .is_err()
            );
            assert!(load_video("../private.mp4", MAX_VIDEO_BYTES).await.is_err());
            #[cfg(unix)]
            {
                let link = own.path().join("outside.mp4");
                std::os::unix::fs::symlink(&outside, &link).unwrap();
                assert!(
                    load_video(link.to_str().unwrap(), MAX_VIDEO_BYTES)
                        .await
                        .is_err()
                );
                let fifo = own.path().join("fifo.mp4");
                assert!(
                    std::process::Command::new("mkfifo")
                        .arg(&fifo)
                        .status()
                        .unwrap()
                        .success()
                );
                let result = tokio::time::timeout(
                    std::time::Duration::from_secs(2),
                    load_video(fifo.to_str().unwrap(), MAX_VIDEO_BYTES),
                )
                .await;
                assert!(result.expect("FIFO must not block").is_err());
            }
        })
        .await;
    }
    #[cfg(unix)]
    #[test]
    fn video_open_rejects_intermediate_directory_replacement() {
        let directory = tempfile::tempdir().unwrap();
        let visible = directory.path().join("visible");
        let private = directory.path().join("private");
        std::fs::create_dir(&visible).unwrap();
        std::fs::create_dir(&private).unwrap();
        std::fs::write(visible.join("clip.mp4"), MP4).unwrap();
        std::fs::write(private.join("clip.mp4"), b"private content").unwrap();
        let resolved = std::fs::canonicalize(visible.join("clip.mp4")).unwrap();
        std::fs::rename(&visible, directory.path().join("original")).unwrap();
        std::os::unix::fs::symlink("private", visible).unwrap();
        assert!(open_video_file_nofollow(&resolved).is_err());
    }
}
