//! Datasheet management for industry devices connected via Aardvark.
//!
//! When a user identifies a new device (e.g. "I have an LM75 temperature
//! sensor"), the [`DatasheetTool`] calls [`DatasheetManager`] to:
//!
//! 1. **search** — query the web for the device datasheet PDF URL.
//! 2. **download** — fetch the PDF and save it to
//!    `~/.zeroclaw/hardware/datasheets/<device>.pdf`.
//! 3. **list** — enumerate all locally cached datasheets.
//! 4. **read** — return the local path of a cached datasheet so the LLM can
//!    reference it with the `read_file` tool or a future RAG pipeline.
//!
//! # Note on PDF extraction
//!
//! Full in-process PDF parsing is available when the `rag-pdf` feature is
//! enabled (adds `pdf-extract`).  Without that feature, the tool returns the
//! PDF file path and instructs the LLM to use a future RAG step.

use crate::tools::traits::{Tool, ToolResult};
use async_trait::async_trait;
use std::path::PathBuf;

// ── DatasheetManager ─────────────────────────────────────────────────────────

/// Manages device datasheet files in `~/.zeroclaw/hardware/datasheets/`.
pub struct DatasheetManager {
    /// Root datasheet storage directory.
    datasheet_dir: PathBuf,
}

impl DatasheetManager {
    /// Create a manager rooted at the default ZeroClaw datasheets directory.
    pub fn new() -> Option<Self> {
        let home = directories::BaseDirs::new()?.home_dir().to_path_buf();
        Some(Self {
            datasheet_dir: home.join(".zeroclaw").join("hardware").join("datasheets"),
        })
    }

    /// Check if a datasheet for `device_name` already exists locally.
    ///
    /// Searches for `<device_name_lower>.pdf` (case-insensitive stem match).
    pub fn find_local(&self, device_name: &str) -> Option<PathBuf> {
        let target = format!("{}.pdf", device_name.to_lowercase().replace(' ', "_"));
        let candidate = self.datasheet_dir.join(&target);
        if candidate.exists() {
            return Some(candidate);
        }
        // Broader scan: any filename containing the device name.
        if let Ok(entries) = std::fs::read_dir(&self.datasheet_dir) {
            for entry in entries.filter_map(|e| e.ok()) {
                let name = entry.file_name();
                let name_str = name.to_string_lossy().to_lowercase();
                let key = device_name.to_lowercase().replace(' ', "_");
                if name_str.contains(&key) && name_str.ends_with(".pdf") {
                    return Some(entry.path());
                }
            }
        }
        None
    }

    /// Download a datasheet PDF from `url` and save it locally.
    ///
    /// The file is saved as `~/.zeroclaw/hardware/datasheets/<device_name>.pdf`.
    /// Returns the path to the saved file.
    pub async fn download_datasheet(
        &self,
        url: &str,
        device_name: &str,
    ) -> anyhow::Result<PathBuf> {
        std::fs::create_dir_all(&self.datasheet_dir)?;

        let filename = format!("{}.pdf", device_name.to_lowercase().replace(' ', "_"));
        let dest = self.datasheet_dir.join(&filename);

        let client = reqwest::Client::builder()
            .user_agent("ZeroClaw/0.1 (datasheet downloader)")
            .timeout(std::time::Duration::from_secs(30))
            .build()?;

        let response = client.get(url).send().await?;
        if !response.status().is_success() {
            anyhow::bail!(
                "HTTP {} downloading datasheet from {url}",
                response.status()
            );
        }
        let bytes = response.bytes().await?;
        std::fs::write(&dest, &bytes)?;

        tracing::info!(device = %device_name, path = %dest.display(), "datasheet downloaded");
        Ok(dest)
    }

    /// List all locally cached datasheet filenames.
    pub fn list_datasheets(&self) -> Vec<String> {
        if let Ok(entries) = std::fs::read_dir(&self.datasheet_dir) {
            let mut names: Vec<String> = entries
                .filter_map(|e| e.ok())
                .map(|e| e.file_name().to_string_lossy().to_string())
                .filter(|n| n.ends_with(".pdf"))
                .collect();
            names.sort();
            return names;
        }
        Vec::new()
    }

    /// Build a web search query for a device datasheet.
    ///
    /// Returns a suggested search query string the LLM (or a search tool) can
    /// use to find the datasheet.
    pub fn search_query(device_name: &str) -> String {
        format!(
            "{device_name} datasheet filetype:pdf site:ti.com OR site:nxp.com OR site:st.com OR site:microchip.com OR site:infineon.com OR site:analog.com"
        )
    }
}

impl Default for DatasheetManager {
    fn default() -> Self {
        Self::new().unwrap_or_else(|| Self {
            datasheet_dir: PathBuf::from(".zeroclaw/hardware/datasheets"),
        })
    }
}

// ── DatasheetTool ─────────────────────────────────────────────────────────────

/// Tool: search for, download, and manage device datasheets.
///
/// Invoked by the LLM when a user identifies a new device connected via
/// Aardvark (e.g. "I have an LM75 temperature sensor on the I2C bus").
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
         Use when the user identifies a new device connected via the Aardvark adapter \
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
                    output: String::new(),
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
                            output: String::new(),
                            error: Some(
                                "missing required parameter: device_name for action 'search'"
                                    .to_string(),
                            ),
                        });
                    }
                };

                // Check if we already have a cached copy.
                if let Some(path) = mgr.find_local(&device) {
                    return Ok(ToolResult {
                        success: true,
                        output: format!(
                            "Datasheet for '{device}' already cached at: {}\n\
                             Use action='read' to get the local path.",
                            path.display()
                        ),
                        error: None,
                    });
                }

                let query = DatasheetManager::search_query(&device);
                Ok(ToolResult {
                    success: true,
                    output: format!(
                        "Suggested web search for '{device}' datasheet:\n{query}\n\n\
                         Once you have a direct PDF URL, use:\n\
                         datasheet(action=\"download\", device_name=\"{device}\", url=\"<URL>\")"
                    ),
                    error: None,
                })
            }

            "download" => {
                let device = match args.get("device_name").and_then(|v| v.as_str()) {
                    Some(d) => d.to_string(),
                    None => {
                        return Ok(ToolResult {
                            success: false,
                            output: String::new(),
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
                            output: String::new(),
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
                             ~/.zeroclaw/hardware/devices/aardvark0.md with the key \
                             registers, I2C address, and protocol notes from this datasheet.",
                            path.display()
                        ),
                        error: None,
                    }),
                    Err(e) => Ok(ToolResult {
                        success: false,
                        output: String::new(),
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
                    output,
                    error: None,
                })
            }

            "read" => {
                let device = match args.get("device_name").and_then(|v| v.as_str()) {
                    Some(d) => d.to_string(),
                    None => {
                        return Ok(ToolResult {
                            success: false,
                            output: String::new(),
                            error: Some(
                                "missing required parameter: device_name for action 'read'"
                                    .to_string(),
                            ),
                        });
                    }
                };
                match mgr.find_local(&device) {
                    Some(path) => Ok(ToolResult {
                        success: true,
                        output: format!(
                            "Datasheet for '{device}' is available at: {}",
                            path.display()
                        ),
                        error: None,
                    }),
                    None => Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(format!(
                            "no datasheet found for '{device}'. \
                             Use action='search' to find one."
                        )),
                    }),
                }
            }

            other => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "unknown action '{other}'. Valid: search, download, list, read"
                )),
            }),
        }
    }
}
