//! `android_screenshot` — capture the phone screen through the bridge APK.

use async_trait::async_trait;
use base64::Engine;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, SystemTime};
use zeroclaw_api::tool::{Tool, ToolResult};

use super::client::AndroidBridgeClient;
use super::{tool_msg, tool_msg_with_args};

/// Maximum base64 payload accepted from the bridge. Mirrors
/// `MAX_BASE64_BYTES` in `crate::screenshot` and the cap the protocol
/// requires the server to enforce, so a misbehaving bridge cannot push an
/// unbounded blob through the tool result.
const MAX_BASE64_BYTES: usize = 2_097_152;

/// Lower bound on a useful capture width; also guards against a zero or
/// negative `max_width` reaching the bridge.
const MIN_WIDTH: u32 = 120;
/// Upper bound on requested width. Beyond this the base64 cap dominates anyway.
const MAX_WIDTH: u32 = 4096;

static TOOL_DESCRIPTION: OnceLock<String> = OnceLock::new();
static SCREENSHOT_SWEEPER: OnceLock<()> = OnceLock::new();
const CACHE_DIR_NAME: &str = "zeroclaw-android-screenshots";
const CACHE_MAX_FILES: usize = 8;
const CACHE_MAX_BYTES: u64 = 16 * 1024 * 1024;
const CACHE_MAX_AGE: Duration = Duration::from_secs(30 * 60);

#[derive(Debug, Clone)]
struct CacheEntry {
    path: PathBuf,
    size: u64,
    modified: SystemTime,
}

fn retention_victims(
    mut entries: Vec<CacheEntry>,
    now: SystemTime,
    protected: Option<&Path>,
) -> Vec<PathBuf> {
    entries.sort_by_key(|entry| std::cmp::Reverse(entry.modified));
    let mut kept = 0usize;
    let mut bytes = 0u64;
    let mut victims = Vec::new();
    for entry in entries {
        let is_protected = protected.is_some_and(|path| path == entry.path);
        let expired = now
            .duration_since(entry.modified)
            .is_ok_and(|age| age > CACHE_MAX_AGE);
        let over_budget =
            kept >= CACHE_MAX_FILES || bytes.saturating_add(entry.size) > CACHE_MAX_BYTES;
        if !is_protected && (expired || over_budget) {
            victims.push(entry.path);
        } else {
            kept += 1;
            bytes = bytes.saturating_add(entry.size);
        }
    }
    victims
}

fn cleanup_cache(dir: &Path, protected: Option<&Path>) {
    let entries = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let metadata = entry.metadata().ok()?;
            metadata.is_file().then(|| CacheEntry {
                path: entry.path(),
                size: metadata.len(),
                modified: metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH),
            })
        })
        .collect();
    for victim in retention_victims(entries, SystemTime::now(), protected) {
        let _ = std::fs::remove_file(victim);
    }
}

fn screenshot_cache_dir() -> anyhow::Result<PathBuf> {
    let dir = std::env::temp_dir().join(CACHE_DIR_NAME);
    std::fs::create_dir_all(&dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
    }
    cleanup_cache(&dir, None);
    let sweeper_dir = dir.clone();
    SCREENSHOT_SWEEPER.get_or_init(|| {
        std::thread::Builder::new()
            .name("android-screenshot-sweeper".into())
            .spawn(move || {
                loop {
                    std::thread::sleep(Duration::from_secs(5 * 60));
                    cleanup_cache(&sweeper_dir, None);
                }
            })
            .ok();
    });
    Ok(dir)
}

pub struct AndroidScreenshotTool {
    client: AndroidBridgeClient,
    default_max_width: u32,
}

impl AndroidScreenshotTool {
    #[must_use]
    pub fn new(client: AndroidBridgeClient, default_max_width: u32) -> Self {
        Self {
            client,
            default_max_width: default_max_width.clamp(MIN_WIDTH, MAX_WIDTH),
        }
    }
}

/// Render a successful `screenshot` response into a tool result.
///
/// Split from the socket path so the `[IMAGE:]` contract is testable against
/// a synthetic bridge response, with no phone and no live socket.
pub(crate) fn render_screenshot(data: &Value) -> anyhow::Result<ToolResult> {
    let encoded = data
        .get("png_base64")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::Error::msg(tool_msg("tool-android-screenshot-error-missing-png")))?;

    if encoded.len() > MAX_BASE64_BYTES {
        anyhow::bail!(tool_msg_with_args(
            "tool-android-screenshot-error-too-large",
            &[
                ("bytes", &encoded.len().to_string()),
                ("max", &MAX_BASE64_BYTES.to_string()),
            ],
        ));
    }

    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map_err(|e| {
            anyhow::Error::msg(tool_msg_with_args(
                "tool-android-screenshot-error-decode",
                &[("error", &e.to_string())],
            ))
        })?;

    let width = data.get("width").and_then(Value::as_u64).unwrap_or(0);
    let height = data.get("height").and_then(Value::as_u64).unwrap_or(0);

    // A uniquely named file under the OS temp dir. It must outlive this
    // function: the multimodal pipeline reads the path out of the marker on
    // the next provider request, so an auto-deleting handle would race it.
    let path = screenshot_cache_dir()?.join(format!(
        "zeroclaw_android_screenshot_{}.png",
        uuid::Uuid::new_v4()
    ));
    let write_result = (|| -> std::io::Result<()> {
        use std::io::Write as _;
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        let mut file = options.open(&path)?;
        file.write_all(&bytes)?;
        file.sync_all()
    })();
    write_result.map_err(|e| {
        anyhow::Error::msg(tool_msg_with_args(
            "tool-android-screenshot-error-write",
            &[
                ("path", &path.display().to_string()),
                ("error", &e.to_string()),
            ],
        ))
    })?;
    cleanup_cache(path.parent().unwrap_or_else(|| Path::new(".")), Some(&path));

    let marker_path = path.display().to_string();
    let size_kb = bytes.len() / 1024;
    let output = format!(
        "{}\n[IMAGE:{marker_path}]",
        tool_msg_with_args(
            "tool-android-screenshot-captured",
            &[
                ("width", &width.to_string()),
                ("height", &height.to_string()),
                ("kb", &size_kb.to_string()),
                ("path", &marker_path),
            ],
        )
    );

    Ok(ToolResult {
        success: true,
        output: output.into(),
        error: None,
    })
}

#[async_trait]
impl Tool for AndroidScreenshotTool {
    fn name(&self) -> &str {
        "android_screenshot"
    }

    fn description(&self) -> &str {
        TOOL_DESCRIPTION
            .get_or_init(|| tool_msg("tool-android-screenshot"))
            .as_str()
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "max_width": {
                    "type": "integer",
                    "minimum": MIN_WIDTH,
                    "maximum": MAX_WIDTH,
                    "description": tool_msg("tool-android-screenshot-param-max-width")
                },
                "expect_package": {
                    "type": "string",
                    "description": tool_msg("tool-android-action-param-expect-package")
                }
            },
            "required": []
        })
    }

    async fn execute(&self, args: Value) -> anyhow::Result<ToolResult> {
        let max_width = args
            .get("max_width")
            .and_then(Value::as_u64)
            .and_then(|v| u32::try_from(v).ok())
            .map_or(self.default_max_width, |w| w.clamp(MIN_WIDTH, MAX_WIDTH));

        // Verify the app on screen before capturing, so a picture of the wrong app cannot be
        // described as if it were the right one.
        if let Some(expected) = args.get("expect_package").and_then(Value::as_str) {
            self.client.assert_foreground(expected).await?;
        }

        let data = self
            .client
            .call("screenshot", json!({ "max_width": max_width }))
            .await?;
        render_screenshot(&data)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 1x1 PNG, the smallest thing that survives a base64 round trip.
    const MINIMAL_PNG: &[u8] = &[
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x02, 0x00, 0x00, 0x00, 0x90,
        0x77, 0x53, 0xDE, 0x00, 0x00, 0x00, 0x0C, 0x49, 0x44, 0x41, 0x54, 0x08, 0xD7, 0x63, 0xF8,
        0xCF, 0xC0, 0x00, 0x00, 0x00, 0x02, 0x00, 0x01, 0xE2, 0x21, 0xBC, 0x33, 0x00, 0x00, 0x00,
        0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
    ];

    fn fake_response() -> Value {
        json!({
            "png_base64": base64::engine::general_purpose::STANDARD.encode(MINIMAL_PNG),
            "width": 540,
            "height": 1140,
        })
    }

    /// The whole point of the tool: the capture must reach the vision model
    /// through an absolute-path `[IMAGE:]` marker on its own line, never as a
    /// bare `data:` URI.
    #[test]
    fn emits_absolute_path_image_marker_on_its_own_line() {
        let result = render_screenshot(&fake_response()).expect("render succeeds");
        assert!(result.success);

        let output = result.output.as_str();
        let marker_line = output
            .lines()
            .find(|line| line.starts_with("[IMAGE:"))
            .expect("output carries an [IMAGE:] marker on its own line");

        let path = marker_line
            .trim_start_matches("[IMAGE:")
            .trim_end_matches(']');
        assert!(
            std::path::Path::new(path).is_absolute(),
            "marker path must be absolute, got: {path}"
        );
        assert_eq!(
            marker_line,
            format!("[IMAGE:{path}]"),
            "the marker must be alone on its line"
        );

        // The file the marker points at must actually exist with the bytes.
        let written = std::fs::read(path).expect("marker path is readable");
        assert_eq!(written, MINIMAL_PNG);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn does_not_emit_a_bare_data_uri() {
        let result = render_screenshot(&fake_response()).expect("render succeeds");
        let output = result.output.as_str();
        assert!(
            !output.contains("data:image"),
            "screenshot must not inline a data: URI: {output}"
        );

        if let Some(line) = output.lines().find(|l| l.starts_with("[IMAGE:")) {
            let path = line.trim_start_matches("[IMAGE:").trim_end_matches(']');
            let _ = std::fs::remove_file(path);
        }
    }

    #[test]
    fn rejects_response_without_png() {
        let err = render_screenshot(&json!({ "width": 540 })).expect_err("missing png is an error");
        assert!(!err.to_string().is_empty());
    }

    /// The oversized payload must be rejected by the *size cap*, not
    /// incidentally by base64 decoding. `MAX_BASE64_BYTES` is a multiple of 4,
    /// so padding by 4 keeps the string decodable: an all-`A` run of
    /// multiple-of-4 length is valid base64. If the cap is removed this input
    /// decodes cleanly and the test fails, which is the point.
    #[test]
    fn rejects_oversized_payload() {
        let oversized = "A".repeat(MAX_BASE64_BYTES + 4);
        assert!(
            base64::engine::general_purpose::STANDARD
                .decode(&oversized)
                .is_ok(),
            "test input must be valid base64 so only the size cap can reject it"
        );

        let err = render_screenshot(&json!({ "png_base64": oversized }))
            .expect_err("oversized payload is an error");
        assert!(!err.to_string().is_empty());
    }

    #[test]
    fn rejects_invalid_base64() {
        let err = render_screenshot(&json!({ "png_base64": "!!!not base64!!!" }))
            .expect_err("invalid base64 is an error");
        assert!(!err.to_string().is_empty());
    }

    #[test]
    fn retention_keeps_newest_within_count_and_byte_budgets() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(10_000);
        let entries: Vec<_> = (0..10)
            .map(|index| CacheEntry {
                path: PathBuf::from(format!("shot-{index}.png")),
                size: 1024,
                modified: now - Duration::from_secs(index),
            })
            .collect();
        let victims = retention_victims(entries, now, None);
        assert_eq!(victims.len(), 2);
        assert!(victims.contains(&PathBuf::from("shot-8.png")));
        assert!(victims.contains(&PathBuf::from("shot-9.png")));
    }

    #[test]
    fn retention_expires_old_files_but_never_the_current_capture() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(10_000);
        let current = PathBuf::from("current.png");
        let old = now - CACHE_MAX_AGE - Duration::from_secs(1);
        let victims = retention_victims(
            vec![
                CacheEntry {
                    path: current.clone(),
                    size: 1,
                    modified: old,
                },
                CacheEntry {
                    path: PathBuf::from("old.png"),
                    size: 1,
                    modified: old,
                },
            ],
            now,
            Some(&current),
        );
        assert!(!victims.contains(&current));
        assert!(victims.contains(&PathBuf::from("old.png")));
    }
}
