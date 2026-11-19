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
use zeroclaw_api::model_provider::ChatMessage;
use zeroclaw_api::tool_carrier::is_tool_result_carrier;

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

pub(crate) fn parse_markers(content: &str) -> (String, Vec<String>) {
    let mut text = String::new();
    let mut videos = Vec::new();
    let mut remaining = content;
    while let Some(start) = remaining.find(PREFIX) {
        let Some(end) = remaining[start + PREFIX.len()..].find(']') else {
            break;
        };
        let end = start + PREFIX.len() + end;
        text.push_str(&remaining[..start]);
        videos.push(remaining[start + PREFIX.len()..end].trim().to_owned());
        remaining = &remaining[end + 1..];
    }
    text.push_str(remaining);
    (text, videos)
}

pub(crate) async fn normalize_messages(
    messages: &[ChatMessage],
    model: &str,
) -> anyhow::Result<Vec<ChatMessage>> {
    let mut normalized = messages.to_vec();
    // A request-wide bound also limits repeated videos in retained history.
    let mut remaining_bytes = MAX_VIDEO_BYTES;
    for message in &mut normalized {
        if message.role != "user" || is_tool_result_carrier(&message.role, &message.content) {
            continue;
        }
        let (text, videos) = parse_markers(&message.content);
        if videos.is_empty() {
            continue;
        }
        ensure!(
            supports_model(model),
            "Qwen model does not support video-file input"
        );
        let mut content = text;
        for source in videos {
            let bytes = load_video(&source, remaining_bytes).await?;
            remaining_bytes -= bytes.len();
            content.push_str("\n[VIDEO:");
            content.push_str(DATA_PREFIX);
            content.push_str(&STANDARD.encode(bytes));
            content.push(']');
        }
        message.content = content;
    }
    Ok(normalized)
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
    if bytes.len() < 12 || &bytes[4..8] != b"ftyp" {
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
    async fn video_input_rejects_unsupported_models_and_aggregate_overflow() {
        let inline = format!("{DATA_PREFIX}{}", STANDARD.encode(MP4));
        let input = ChatMessage::user(format!("[VIDEO:{inline}]"));
        assert!(normalize_messages(&[input], "qwen-turbo").await.is_err());
        assert!(supports_model("qwen3.8-flash-0902"));
        assert!(!supports_model("qwen3.8-flash-invented"));
        let bytes = [MP4, &vec![0; MAX_VIDEO_BYTES / 2]].concat();
        let inline = format!("{DATA_PREFIX}{}", STANDARD.encode(bytes));
        let input = ChatMessage::user(format!("[VIDEO:{inline}] [VIDEO:{inline}]"));
        assert!(normalize_messages(&[input], "qwen3.8-flash").await.is_err());
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
        std::fs::write(private.join("clip.mp4"), MP4).unwrap();
        let resolved = std::fs::canonicalize(visible.join("clip.mp4")).unwrap();
        std::fs::rename(&visible, directory.path().join("original")).unwrap();
        std::os::unix::fs::symlink(private, visible).unwrap();
        assert!(open_video_file_nofollow(&resolved).is_err());
    }
}
