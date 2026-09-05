//! Datasheet management for connected industry devices.

use async_trait::async_trait;
use cap_fs_ext::DirExt;
use cap_std::ambient_authority;
use cap_std::fs::{Dir, OpenOptions};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use zeroclaw_api::attribution::ToolKind;
use zeroclaw_api::tool::{Tool, ToolResult};
use zeroclaw_api::tool_attribution;

tool_attribution!(DatasheetTool, ToolKind::Plugin);

const APPROVED_DATASHEET_HOSTS: [&str; 6] = [
    "ti.com",
    "nxp.com",
    "st.com",
    "microchip.com",
    "infineon.com",
    "analog.com",
];
const MAX_DATASHEET_BYTES: u64 = 25 * 1024 * 1024;
const MAX_DEVICE_NAME_BYTES: usize = 80;
const MAX_REDIRECTS: usize = 5;
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(30);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
static DATASHEET_COMMIT_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn datasheet_filename(device_name: &str) -> anyhow::Result<String> {
    let trimmed = device_name.trim();
    if trimmed.is_empty() {
        anyhow::bail!("device_name must not be empty");
    }
    if trimmed.len() > MAX_DEVICE_NAME_BYTES {
        anyhow::bail!("device_name exceeds the {MAX_DEVICE_NAME_BYTES}-byte limit");
    }
    if !trimmed
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b' ' | b'_' | b'-'))
    {
        anyhow::bail!("device_name may contain only ASCII letters, digits, spaces, '_' and '-'");
    }

    Ok(format!(
        "{}.pdf",
        trimmed.to_ascii_lowercase().replace(' ', "_")
    ))
}

fn is_approved_datasheet_host(host: &str) -> bool {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    APPROVED_DATASHEET_HOSTS.iter().any(|approved| {
        host == *approved
            || host
                .strip_suffix(approved)
                .is_some_and(|prefix| prefix.ends_with('.'))
    })
}

fn validate_datasheet_url(url: &reqwest::Url) -> anyhow::Result<()> {
    if url.scheme() != "https" {
        anyhow::bail!("datasheet URL must use HTTPS");
    }
    if !url.username().is_empty() || url.password().is_some() {
        anyhow::bail!("datasheet URL must not contain credentials");
    }
    if url.port_or_known_default() != Some(443) {
        anyhow::bail!("datasheet URL must use HTTPS port 443");
    }

    let host = url
        .host_str()
        .ok_or_else(|| anyhow::Error::msg("datasheet URL must include a host"))?;
    if !is_approved_datasheet_host(host) {
        anyhow::bail!("datasheet URL host '{host}' is not approved");
    }
    Ok(())
}

fn parse_datasheet_url(raw_url: &str) -> anyhow::Result<reqwest::Url> {
    let url = reqwest::Url::parse(raw_url)
        .map_err(|error| anyhow::Error::msg(format!("invalid datasheet URL: {error}")))?;
    validate_datasheet_url(&url)?;
    Ok(url)
}

fn datasheet_redirect_policy() -> reqwest::redirect::Policy {
    reqwest::redirect::Policy::custom(|attempt| {
        if let Err(error) = validate_datasheet_url(attempt.url()) {
            return attempt.error(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                format!("datasheet redirect denied: {error}"),
            ));
        }
        reqwest::redirect::Policy::limited(MAX_REDIRECTS).redirect(attempt)
    })
}

fn datasheet_client() -> anyhow::Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .user_agent("ZeroClaw/0.1 (datasheet downloader)")
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(DOWNLOAD_TIMEOUT)
        .redirect(datasheet_redirect_policy())
        .build()?)
}

fn validate_content_length(content_length: Option<u64>, max_bytes: u64) -> anyhow::Result<()> {
    if let Some(content_length) = content_length
        && content_length > max_bytes
    {
        anyhow::bail!(
            "datasheet response advertises {content_length} bytes, exceeding the {max_bytes}-byte limit"
        );
    }
    Ok(())
}

struct PendingDatasheet {
    directory: Dir,
    file: Option<tokio::fs::File>,
    temp_name: String,
    destination_name: String,
    written: u64,
    max_bytes: u64,
}

impl PendingDatasheet {
    fn new(directory: Dir, destination_name: String, max_bytes: u64) -> anyhow::Result<Self> {
        let temp_name = format!(".datasheet-{}.part", uuid::Uuid::new_v4());
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        let file = directory.open_with(&temp_name, &options)?;
        Ok(Self {
            directory,
            file: Some(tokio::fs::File::from_std(file.into_std())),
            temp_name,
            destination_name,
            written: 0,
            max_bytes,
        })
    }

    async fn write_chunk(&mut self, chunk: &[u8]) -> anyhow::Result<()> {
        let next_size = self
            .written
            .checked_add(chunk.len() as u64)
            .ok_or_else(|| anyhow::Error::msg("datasheet response size overflow"))?;
        if next_size > self.max_bytes {
            anyhow::bail!(
                "datasheet response exceeds the {}-byte limit",
                self.max_bytes
            );
        }
        self.file
            .as_mut()
            .expect("pending datasheet file is open")
            .write_all(chunk)
            .await?;
        self.written = next_size;
        Ok(())
    }

    async fn commit(
        mut self,
        identity: &same_file::Handle,
        display_path: &Path,
    ) -> anyhow::Result<u64> {
        self.file
            .as_mut()
            .expect("pending datasheet file is open")
            .flush()
            .await?;
        drop(self.file.take());
        let _commit_guard = DATASHEET_COMMIT_LOCK
            .lock()
            .map_err(|_| anyhow::Error::msg("datasheet commit lock is poisoned"))?;

        let backup_name = match self.directory.symlink_metadata(&self.destination_name) {
            Ok(metadata) if metadata.is_file() => {
                let backup_name = format!(".datasheet-{}.rollback", uuid::Uuid::new_v4());
                self.directory
                    .hard_link(&self.destination_name, &self.directory, &backup_name)?;
                Some(backup_name)
            }
            Ok(metadata) if metadata.is_symlink() => None,
            Ok(_) => anyhow::bail!("datasheet destination exists but is not a regular file"),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(error.into()),
        };

        if let Err(install_error) =
            self.directory
                .rename(&self.temp_name, &self.directory, &self.destination_name)
        {
            if let Some(backup_name) = &backup_name
                && let Err(cleanup_error) = self.directory.remove_file(backup_name)
            {
                anyhow::bail!(
                    "datasheet install failed: {install_error}; rollback cleanup failed: {cleanup_error}"
                );
            }
            return Err(install_error.into());
        }
        self.temp_name.clear();

        if let Err(verification_error) = verify_display_path(identity, display_path) {
            if let Err(rollback_error) = self.rollback_install(backup_name.as_deref()) {
                anyhow::bail!("{verification_error}; datasheet rollback failed: {rollback_error}");
            }
            return Err(verification_error);
        }

        if let Some(backup_name) = &backup_name
            && let Err(cleanup_error) = self.directory.remove_file(backup_name)
        {
            if let Err(rollback_error) = self.rollback_install(Some(backup_name)) {
                anyhow::bail!(
                    "datasheet rollback cleanup failed: {cleanup_error}; restoration failed: {rollback_error}"
                );
            }
            return Err(cleanup_error.into());
        }
        Ok(self.written)
    }

    fn rollback_install(&self, backup_name: Option<&str>) -> anyhow::Result<()> {
        if let Some(backup_name) = backup_name {
            self.directory
                .rename(backup_name, &self.directory, &self.destination_name)?;
        } else {
            match self.directory.remove_file(&self.destination_name) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
        Ok(())
    }
}

impl Drop for PendingDatasheet {
    fn drop(&mut self) {
        drop(self.file.take());
        if !self.temp_name.is_empty() {
            let _ = self.directory.remove_file(&self.temp_name);
        }
    }
}

// ── DatasheetManager ─────────────────────────────────────────────────────────

/// Manages device datasheet files in `~/.zeroclaw/hardware/datasheets/`.
pub struct DatasheetManager {
    /// Root ZeroClaw directory that grants authority for datasheet storage.
    zeroclaw_root: PathBuf,
    /// Datasheet directory beneath `zeroclaw_root`.
    datasheet_relative: PathBuf,
}

struct BoundDatasheetDir {
    directory: Dir,
    display_path: PathBuf,
    identity: same_file::Handle,
}

fn directory_identity(directory: &Dir) -> anyhow::Result<same_file::Handle> {
    Ok(same_file::Handle::from_file(
        directory.try_clone()?.into_std_file(),
    )?)
}

fn verify_display_path(identity: &same_file::Handle, path: &Path) -> anyhow::Result<()> {
    let visible = same_file::Handle::from_path(path)?;
    if identity != &visible {
        anyhow::bail!(
            "datasheet cache path '{}' no longer identifies the bound directory",
            path.display()
        );
    }
    Ok(())
}

impl DatasheetManager {
    /// Create a manager rooted at the default ZeroClaw datasheets directory.
    pub fn new() -> Option<Self> {
        let home = directories::BaseDirs::new()?.home_dir().to_path_buf();
        Some(Self {
            zeroclaw_root: home.join(".zeroclaw"),
            datasheet_relative: PathBuf::from("hardware/datasheets"),
        })
    }

    fn open_datasheet_dir(&self, create: bool) -> anyhow::Result<Option<BoundDatasheetDir>> {
        if create {
            std::fs::create_dir_all(&self.zeroclaw_root)?;
        } else if !self.zeroclaw_root.exists() {
            return Ok(None);
        }

        let mut directory = Dir::open_ambient_dir(&self.zeroclaw_root, ambient_authority())?;
        let canonical_root = std::fs::canonicalize(&self.zeroclaw_root)?;
        verify_display_path(&directory_identity(&directory)?, &canonical_root)?;
        for component in self.datasheet_relative.components() {
            let std::path::Component::Normal(name) = component else {
                anyhow::bail!("datasheet cache path must stay beneath the ZeroClaw root");
            };
            let relative = Path::new(name);
            directory = match directory.open_dir_nofollow(relative) {
                Ok(child) => child,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound && create => {
                    if let Err(create_error) = directory.create_dir(relative)
                        && create_error.kind() != std::io::ErrorKind::AlreadyExists
                    {
                        return Err(create_error.into());
                    }
                    directory.open_dir_nofollow(relative)?
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                Err(error) => return Err(error.into()),
            };
        }

        let display_path = canonical_root.join(&self.datasheet_relative);
        let identity = directory_identity(&directory)?;
        verify_display_path(&identity, &display_path)?;
        Ok(Some(BoundDatasheetDir {
            directory,
            display_path,
            identity,
        }))
    }

    /// Check if a datasheet for `device_name` already exists locally.
    /// Checks the single normalized cache identity for `device_name`.
    pub fn find_local(&self, device_name: &str) -> anyhow::Result<Option<PathBuf>> {
        let target = datasheet_filename(device_name)?;
        let Some(bound) = self.open_datasheet_dir(false)? else {
            return Ok(None);
        };
        if !bound
            .directory
            .symlink_metadata(&target)
            .is_ok_and(|metadata| metadata.is_file())
        {
            return Ok(None);
        }
        verify_display_path(&bound.identity, &bound.display_path)?;
        Ok(Some(bound.display_path.join(target)))
    }

    /// Download a datasheet PDF from `url` and save it locally.
    /// The file is saved as `~/.zeroclaw/hardware/datasheets/<device_name>.pdf`.
    /// Returns the path to the saved file.
    pub async fn download_datasheet(
        &self,
        url: &str,
        device_name: &str,
    ) -> anyhow::Result<PathBuf> {
        let filename = datasheet_filename(device_name)?;
        let url = parse_datasheet_url(url)?;
        let client = datasheet_client()?;
        let mut response = client.get(url).send().await?;
        if !response.status().is_success() {
            anyhow::bail!("HTTP {} downloading datasheet", response.status());
        }
        validate_datasheet_url(response.url())?;
        validate_content_length(response.content_length(), MAX_DATASHEET_BYTES)?;

        let bound = self
            .open_datasheet_dir(true)?
            .expect("create=true returns a bound datasheet directory");
        let BoundDatasheetDir {
            directory,
            display_path,
            identity,
        } = bound;
        let dest = display_path.join(&filename);
        let mut pending = PendingDatasheet::new(directory, filename, MAX_DATASHEET_BYTES)?;
        while let Some(chunk) = response.chunk().await? {
            pending.write_chunk(&chunk).await?;
        }
        let written = pending.commit(&identity, &display_path).await?;

        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(
                ::serde_json::json!({
                    "device": device_name,
                    "path": dest.display().to_string(),
                    "bytes": written,
                })
            ),
            "datasheet downloaded"
        );
        Ok(dest)
    }

    /// List all locally cached datasheet filenames.
    pub fn list_datasheets(&self) -> Vec<String> {
        let Ok(Some(bound)) = self.open_datasheet_dir(false) else {
            return Vec::new();
        };
        let Ok(entries) = bound.directory.entries() else {
            return Vec::new();
        };
        let mut names: Vec<String> = entries
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_ok_and(|file_type| file_type.is_file()))
            .map(|entry| entry.file_name().to_string_lossy().to_string())
            .filter(|name| name.ends_with(".pdf"))
            .collect();
        names.sort();
        names
    }

    /// Build a web search query for a device datasheet.
    /// Returns a suggested search query string the LLM (or a search tool) can
    /// use to find the datasheet.
    pub fn search_query(device_name: &str) -> String {
        let sites = APPROVED_DATASHEET_HOSTS
            .iter()
            .map(|host| format!("site:{host}"))
            .collect::<Vec<_>>()
            .join(" OR ");
        format!("{device_name} datasheet filetype:pdf {sites}")
    }
}

impl Default for DatasheetManager {
    fn default() -> Self {
        Self::new().unwrap_or_else(|| Self {
            zeroclaw_root: PathBuf::from(".zeroclaw"),
            datasheet_relative: PathBuf::from("hardware/datasheets"),
        })
    }
}

// ── DatasheetTool ─────────────────────────────────────────────────────────────

/// Tool: search for, download, and manage device datasheets.
/// Invoked by the LLM when a user identifies a newly connected device
/// (e.g. "I have an LM75 temperature sensor on the I2C bus").
pub struct DatasheetTool;

impl DatasheetTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for DatasheetTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for DatasheetTool {
    fn name(&self) -> &str {
        "datasheet"
    }

    fn description(&self) -> &str {
        "Search for, download, and manage device datasheets. \
         Use when the user identifies a newly connected device \
         (e.g. 'I have an LM75 sensor'). \
         Actions: 'search' returns a web search query; \
         'download' fetches a PDF from a URL; \
         'list' shows cached datasheets; \
         'read' returns the local path of a cached datasheet."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["search", "download", "list", "read"],
                    "description": "Operation to perform"
                },
                "device_name": {
                    "type": "string",
                    "description": "Device name (e.g. 'LM75', 'PSoC6', 'MPU6050')"
                },
                "url": {
                    "type": "string",
                    "description": "For action='download': direct URL to the datasheet PDF"
                }
            },
            "required": ["action"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let action = match args.get("action").and_then(|v| v.as_str()) {
            Some(a) => a.to_string(),
            None => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new().into(),
                    error: Some("missing required parameter: action".to_string()),
                });
            }
        };

        let mgr = DatasheetManager::default();

        match action.as_str() {
            "search" => {
                let device = match args.get("device_name").and_then(|v| v.as_str()) {
                    Some(d) => d.to_string(),
                    None => {
                        return Ok(ToolResult {
                            success: false,
                            output: String::new().into(),
                            error: Some(
                                "missing required parameter: device_name for action 'search'"
                                    .to_string(),
                            ),
                        });
                    }
                };

                // Check if we already have a cached copy.
                match mgr.find_local(&device) {
                    Ok(Some(path)) => {
                        return Ok(ToolResult {
                            success: true,
                            output: format!(
                                "Datasheet for '{device}' already cached at: {}\n\
                                 Use action='read' to get the local path.",
                                path.display()
                            )
                            .into(),
                            error: None,
                        });
                    }
                    Ok(None) => {}
                    Err(error) => {
                        return Ok(ToolResult {
                            success: false,
                            output: String::new().into(),
                            error: Some(format!("invalid device_name: {error}")),
                        });
                    }
                }

                let query = DatasheetManager::search_query(&device);
                Ok(ToolResult {
                    success: true,
                    output: format!(
                        "Suggested web search for '{device}' datasheet:\n{query}\n\n\
                         Once you have a direct PDF URL, use:\n\
                         datasheet(action=\"download\", device_name=\"{device}\", url=\"<URL>\")"
                    )
                    .into(),
                    error: None,
                })
            }

            "download" => {
                let device = match args.get("device_name").and_then(|v| v.as_str()) {
                    Some(d) => d.to_string(),
                    None => {
                        return Ok(ToolResult {
                            success: false,
                            output: String::new().into(),
                            error: Some(
                                "missing required parameter: device_name for action 'download'"
                                    .to_string(),
                            ),
                        });
                    }
                };
                let url = match args.get("url").and_then(|v| v.as_str()) {
                    Some(u) => u.to_string(),
                    None => {
                        return Ok(ToolResult {
                            success: false,
                            output: String::new().into(),
                            error: Some(
                                "missing required parameter: url for action 'download'".to_string(),
                            ),
                        });
                    }
                };

                match mgr.download_datasheet(&url, &device).await {
                    Ok(path) => Ok(ToolResult {
                        success: true,
                        output: format!(
                            "Datasheet for '{device}' downloaded successfully.\n\
                             Saved to: {}\n\n\
                             Next step: create a device profile at \
                             ~/.zeroclaw/hardware/devices/<device>.md with the key \
                             registers, I2C address, and protocol notes from this datasheet.",
                            path.display()
                        )
                        .into(),
                        error: None,
                    }),
                    Err(e) => Ok(ToolResult {
                        success: false,
                        output: String::new().into(),
                        error: Some(format!("download failed: {e}")),
                    }),
                }
            }

            "list" => {
                let datasheets = mgr.list_datasheets();
                let output = if datasheets.is_empty() {
                    "No datasheets cached yet.\n\
                     Use datasheet(action=\"search\", device_name=\"...\") to find one."
                        .to_string()
                } else {
                    format!(
                        "{} cached datasheet(s) in ~/.zeroclaw/hardware/datasheets/:\n{}",
                        datasheets.len(),
                        datasheets
                            .iter()
                            .map(|n| format!("  - {n}"))
                            .collect::<Vec<_>>()
                            .join("\n")
                    )
                };
                Ok(ToolResult {
                    success: true,
                    output: output.into(),
                    error: None,
                })
            }

            "read" => {
                let device = match args.get("device_name").and_then(|v| v.as_str()) {
                    Some(d) => d.to_string(),
                    None => {
                        return Ok(ToolResult {
                            success: false,
                            output: String::new().into(),
                            error: Some(
                                "missing required parameter: device_name for action 'read'"
                                    .to_string(),
                            ),
                        });
                    }
                };
                match mgr.find_local(&device) {
                    Ok(Some(path)) => Ok(ToolResult {
                        success: true,
                        output: format!(
                            "Datasheet for '{device}' is available at: {}",
                            path.display()
                        )
                        .into(),
                        error: None,
                    }),
                    Ok(None) => Ok(ToolResult {
                        success: false,
                        output: String::new().into(),
                        error: Some(format!(
                            "no datasheet found for '{device}'. \
                             Use action='search' to find one."
                        )),
                    }),
                    Err(error) => Ok(ToolResult {
                        success: false,
                        output: String::new().into(),
                        error: Some(format!("invalid device_name: {error}")),
                    }),
                }
            }

            other => Ok(ToolResult {
                success: false,
                output: String::new().into(),
                error: Some(format!(
                    "unknown action '{other}'. Valid: search, download, list, read"
                )),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open_test_dir(path: &Path) -> Dir {
        Dir::open_ambient_dir(path, ambient_authority()).unwrap()
    }

    fn test_manager(root: &Path) -> DatasheetManager {
        DatasheetManager {
            zeroclaw_root: root.to_path_buf(),
            datasheet_relative: PathBuf::from("datasheets"),
        }
    }

    #[test]
    fn filename_policy_normalizes_safe_device_names() {
        assert_eq!(datasheet_filename(" LM75 ").unwrap(), "lm75.pdf");
        assert_eq!(
            datasheet_filename("STM32F4 Discovery").unwrap(),
            "stm32f4_discovery.pdf"
        );
        assert_eq!(
            datasheet_filename("ESP32-S3_dev").unwrap(),
            "esp32-s3_dev.pdf"
        );
    }

    #[test]
    fn filename_policy_rejects_unsafe_device_names() {
        for unsafe_name in [
            "",
            "   ",
            "../outside",
            "/absolute",
            r"..\outside",
            ".hidden",
            "chip/pdf",
            "chip\nname",
            "µcontroller",
        ] {
            assert!(
                datasheet_filename(unsafe_name).is_err(),
                "unsafe name should be rejected: {unsafe_name:?}"
            );
        }
        assert!(datasheet_filename(&"x".repeat(MAX_DEVICE_NAME_BYTES + 1)).is_err());
    }

    #[test]
    fn url_policy_accepts_only_approved_https_manufacturers() {
        for allowed in [
            "https://ti.com/lit/ds/example.pdf",
            "https://www.ti.com/lit/ds/example.pdf",
            "https://downloads.st.com/example.pdf",
            "https://nxp.com:443/docs/example.pdf",
            "https://www.microchip.com/example.pdf",
            "https://infineon.com/example.pdf",
            "https://analog.com/example.pdf",
        ] {
            assert!(
                parse_datasheet_url(allowed).is_ok(),
                "approved URL should pass: {allowed}"
            );
        }

        for denied in [
            "http://ti.com/example.pdf",
            "https://user@ti.com/example.pdf",
            "https://ti.com:8443/example.pdf",
            "https://evil-ti.com/example.pdf",
            "https://ti.com.evil.example/example.pdf",
            "https://127.0.0.1/example.pdf",
            "https://localhost/example.pdf",
            "not a url",
        ] {
            assert!(
                parse_datasheet_url(denied).is_err(),
                "unapproved URL should fail: {denied}"
            );
        }
    }

    #[test]
    fn content_length_policy_rejects_only_oversized_responses() {
        assert!(validate_content_length(None, 4).is_ok());
        assert!(validate_content_length(Some(4), 4).is_ok());
        assert!(validate_content_length(Some(5), 4).is_err());
    }

    #[test]
    fn search_query_uses_the_enforced_manufacturer_policy() {
        let query = DatasheetManager::search_query("LM75");
        for host in APPROVED_DATASHEET_HOSTS {
            assert!(query.contains(&format!("site:{host}")));
        }
        assert_eq!(
            query.matches("site:").count(),
            APPROVED_DATASHEET_HOSTS.len()
        );
    }

    #[tokio::test]
    async fn pending_download_commits_only_after_complete_bounded_write() {
        let dir = tempfile::tempdir().unwrap();
        let destination = dir.path().join("lm75.pdf");
        std::fs::write(&destination, b"old").unwrap();

        let directory = open_test_dir(dir.path());
        let identity = directory_identity(&directory).unwrap();
        let mut pending = PendingDatasheet::new(directory, "lm75.pdf".into(), 8).unwrap();
        pending.write_chunk(b"new").await.unwrap();
        pending.write_chunk(b" data").await.unwrap();
        assert_eq!(pending.commit(&identity, dir.path()).await.unwrap(), 8);

        assert_eq!(std::fs::read(&destination).unwrap(), b"new data");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[tokio::test]
    async fn oversized_pending_download_preserves_existing_destination() {
        let dir = tempfile::tempdir().unwrap();
        let destination = dir.path().join("lm75.pdf");
        std::fs::write(&destination, b"known-good").unwrap();

        let mut pending =
            PendingDatasheet::new(open_test_dir(dir.path()), "lm75.pdf".into(), 4).unwrap();
        pending.write_chunk(b"1234").await.unwrap();
        assert!(pending.write_chunk(b"5").await.is_err());
        drop(pending);

        assert_eq!(std::fs::read(&destination).unwrap(), b"known-good");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn committed_download_replaces_destination_symlink_without_following_it() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let external_target = outside.path().join("external.pdf");
        let destination = dir.path().join("lm75.pdf");
        std::fs::write(&external_target, b"external").unwrap();
        symlink(&external_target, &destination).unwrap();

        let directory = open_test_dir(dir.path());
        let identity = directory_identity(&directory).unwrap();
        let mut pending = PendingDatasheet::new(directory, "lm75.pdf".into(), 8).unwrap();
        pending.write_chunk(b"new data").await.unwrap();
        pending.commit(&identity, dir.path()).await.unwrap();

        assert_eq!(std::fs::read(&external_target).unwrap(), b"external");
        assert_eq!(std::fs::read(&destination).unwrap(), b"new data");
        assert!(
            std::fs::symlink_metadata(&destination)
                .unwrap()
                .file_type()
                .is_file()
        );
    }

    #[cfg(unix)]
    #[test]
    fn find_local_ignores_symlinked_cache_entries() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let external_target = outside.path().join("external.pdf");
        std::fs::write(&external_target, b"external").unwrap();
        symlink(&external_target, dir.path().join("lm75.pdf")).unwrap();

        let manager = test_manager(dir.path());
        assert!(manager.find_local("LM75").unwrap().is_none());
        assert!(manager.list_datasheets().is_empty());
    }

    #[test]
    fn find_local_uses_only_the_exact_normalized_identity() {
        let root = tempfile::tempdir().unwrap();
        let cache = root.path().join("datasheets");
        std::fs::create_dir(&cache).unwrap();
        std::fs::write(cache.join("lm750.pdf"), b"different device").unwrap();

        assert!(
            test_manager(root.path())
                .find_local("LM75")
                .unwrap()
                .is_none()
        );
    }

    #[cfg(unix)]
    #[test]
    fn cache_directory_symlink_is_rejected() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        symlink(outside.path(), root.path().join("datasheets")).unwrap();

        assert!(test_manager(root.path()).open_datasheet_dir(true).is_err());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn bound_directory_prevents_path_swap_from_redirecting_commit() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let cache = root.path().join("datasheets");
        std::fs::create_dir(&cache).unwrap();
        std::fs::write(cache.join("lm75.pdf"), b"known-good").unwrap();
        let manager = test_manager(root.path());
        let bound = manager.open_datasheet_dir(true).unwrap().unwrap();
        let BoundDatasheetDir {
            directory,
            display_path,
            identity,
        } = bound;
        let mut pending = PendingDatasheet::new(directory, "lm75.pdf".into(), 8).unwrap();

        let moved = root.path().join("datasheets-original");
        std::fs::rename(root.path().join("datasheets"), &moved).unwrap();
        symlink(outside.path(), root.path().join("datasheets")).unwrap();

        pending.write_chunk(b"new data").await.unwrap();
        assert!(pending.commit(&identity, &display_path).await.is_err());

        assert_eq!(
            std::fs::read(moved.join("lm75.pdf")).unwrap(),
            b"known-good"
        );
        assert!(!outside.path().join("lm75.pdf").exists());
        assert_eq!(std::fs::read_dir(&moved).unwrap().count(), 1);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn path_swap_rolls_back_a_new_destination_without_orphans() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let manager = test_manager(root.path());
        let bound = manager.open_datasheet_dir(true).unwrap().unwrap();
        let BoundDatasheetDir {
            directory,
            display_path,
            identity,
        } = bound;
        let mut pending = PendingDatasheet::new(directory, "lm75.pdf".into(), 8).unwrap();

        let moved = root.path().join("datasheets-original");
        std::fs::rename(root.path().join("datasheets"), &moved).unwrap();
        symlink(outside.path(), root.path().join("datasheets")).unwrap();

        pending.write_chunk(b"new data").await.unwrap();
        assert!(pending.commit(&identity, &display_path).await.is_err());

        assert_eq!(std::fs::read_dir(&moved).unwrap().count(), 0);
        assert!(!outside.path().join("lm75.pdf").exists());
    }
}
