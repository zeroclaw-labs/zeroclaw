//! Encrypted credential vault for site login and payment info.
//!
//! Stores credentials (IDs, passwords, card numbers) encrypted locally
//! using ChaCha20-Poly1305 via the existing SecretStore infrastructure.
//! Credentials are NEVER stored in plaintext and NEVER transmitted externally.
//!
//! Security architecture:
//! - `credential_store`: encrypt and save a credential locally
//! - `credential_recall get`: returns a REFERENCE TOKEN `{{CRED:site:label}}`
//!   (the actual value is NEVER sent to the LLM)
//! - `resolve_credential_ref()`: called by the browser tool at fill time
//!   to resolve the reference token to the actual value locally
//! - The decrypted credential only exists briefly in gateway memory during
//!   browser form fill, then is dropped

use crate::security::SecretStore;
use crate::tools::traits::{Tool, ToolResult, ToolSpec};
use async_trait::async_trait;
use serde_json::json;
use std::path::{Path, PathBuf};

/// Credential entry stored on disk (encrypted values).
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
struct CredentialEntry {
    site: String,
    label: String,
    encrypted_value: String,
    /// Display hint (e.g. "user@email.com" for ID, "****-1234" for card)
    display_hint: String,
    created_at: i64,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Default)]
struct VaultData {
    credentials: Vec<CredentialEntry>,
}

fn vault_path(workspace_dir: &Path) -> PathBuf {
    workspace_dir.join("credential_vault.json.enc")
}

fn load_vault(path: &Path, store: &SecretStore) -> VaultData {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return VaultData::default();
    };
    let decrypted = match store.decrypt(&raw) {
        Ok(d) => d,
        Err(_) => return VaultData::default(),
    };
    serde_json::from_str(&decrypted).unwrap_or_default()
}

fn save_vault(path: &Path, store: &SecretStore, vault: &VaultData) -> anyhow::Result<()> {
    let json = serde_json::to_string_pretty(vault)?;
    let encrypted = store.encrypt(&json)?;
    std::fs::write(path, encrypted)?;
    Ok(())
}

fn make_display_hint(label: &str, value: &str) -> String {
    let lower = label.to_lowercase();
    if lower.contains("password") || lower.contains("비밀번호") || lower.contains("cvc") {
        "••••••".to_string()
    } else if lower.contains("card") || lower.contains("카드") {
        if value.len() >= 4 {
            format!("****-{}", &value[value.len() - 4..])
        } else {
            "****".to_string()
        }
    } else {
        // ID, email — show as-is
        value.to_string()
    }
}

// ── credential_store tool ────────────────────────────────────

pub struct CredentialStoreTool {
    workspace_dir: PathBuf,
}

impl CredentialStoreTool {
    pub fn new(workspace_dir: &Path) -> Self {
        Self {
            workspace_dir: workspace_dir.to_path_buf(),
        }
    }
}

#[async_trait]
impl Tool for CredentialStoreTool {
    fn name(&self) -> &str {
        "credential_store"
    }

    fn description(&self) -> &str {
        "Encrypt and save a site credential (login ID, password, card number) to the LOCAL encrypted vault. Never stores plaintext. Never transmits externally."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "site": {
                    "type": "string",
                    "description": "Site domain (e.g. 'bigcase.ai', 'coupang.com')"
                },
                "label": {
                    "type": "string",
                    "description": "Credential label: 'id', 'password', 'card_number', 'card_expiry', 'card_cvc', or custom"
                },
                "value": {
                    "type": "string",
                    "description": "The credential value to encrypt and store"
                }
            },
            "required": ["site", "label", "value"]
        })
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: self.name().to_string(),
            description: self.description().to_string(),
            parameters: self.parameters_schema(),
        }
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let site = args.get("site").and_then(|v| v.as_str()).unwrap_or("");
        let label = args.get("label").and_then(|v| v.as_str()).unwrap_or("");
        let value = args.get("value").and_then(|v| v.as_str()).unwrap_or("");

        if site.is_empty() || label.is_empty() || value.is_empty() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("site, label, and value are required".into()),
            });
        }

        let store = SecretStore::new(&self.workspace_dir, true);
        let path = vault_path(&self.workspace_dir);
        let mut vault = load_vault(&path, &store);

        let encrypted_value = store.encrypt(value)?;
        let display_hint = make_display_hint(label, value);

        // Remove existing entry for same site+label, then add new
        vault
            .credentials
            .retain(|c| !(c.site == site && c.label == label));
        vault.credentials.push(CredentialEntry {
            site: site.to_string(),
            label: label.to_string(),
            encrypted_value,
            display_hint: display_hint.clone(),
            created_at: chrono::Utc::now().timestamp(),
        });

        save_vault(&path, &store, &vault)?;

        Ok(ToolResult {
            success: true,
            output: format!(
                "Credential saved (encrypted): site={}, label={}, hint={}",
                site, label, display_hint
            ),
            error: None,
        })
    }
}

// ── credential_recall tool ───────────────────────────────────

pub struct CredentialRecallTool {
    workspace_dir: PathBuf,
}

impl CredentialRecallTool {
    pub fn new(workspace_dir: &Path) -> Self {
        Self {
            workspace_dir: workspace_dir.to_path_buf(),
        }
    }
}

#[async_trait]
impl Tool for CredentialRecallTool {
    fn name(&self) -> &str {
        "credential_recall"
    }

    fn description(&self) -> &str {
        "Retrieve a stored credential from the LOCAL encrypted vault. Use 'list' to see stored credentials (masked). Use 'get' to obtain a secure reference token ({{CRED:site:label}}) — pass this token directly to `browser fill` and the gateway will resolve it locally. The actual credential is NEVER returned to you."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["list", "get", "delete"],
                    "description": "'list' = show all stored credentials (masked), 'get' = decrypt a specific credential, 'delete' = remove a credential"
                },
                "site": {
                    "type": "string",
                    "description": "Site domain to filter by (optional for 'list', required for 'get'/'delete')"
                },
                "label": {
                    "type": "string",
                    "description": "Credential label (required for 'get'/'delete')"
                }
            },
            "required": ["action"]
        })
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: self.name().to_string(),
            description: self.description().to_string(),
            parameters: self.parameters_schema(),
        }
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let action = args.get("action").and_then(|v| v.as_str()).unwrap_or("list");
        let site = args.get("site").and_then(|v| v.as_str()).unwrap_or("");
        let label = args.get("label").and_then(|v| v.as_str()).unwrap_or("");

        let store = SecretStore::new(&self.workspace_dir, true);
        let path = vault_path(&self.workspace_dir);

        match action {
            "list" => {
                let vault = load_vault(&path, &store);
                let entries: Vec<_> = vault
                    .credentials
                    .iter()
                    .filter(|c| site.is_empty() || c.site == site)
                    .map(|c| {
                        json!({
                            "site": c.site,
                            "label": c.label,
                            "display_hint": c.display_hint,
                        })
                    })
                    .collect();

                if entries.is_empty() {
                    return Ok(ToolResult {
                        success: true,
                        output: if site.is_empty() {
                            "No stored credentials.".into()
                        } else {
                            format!("No stored credentials for site: {site}")
                        },
                        error: None,
                    });
                }

                Ok(ToolResult {
                    success: true,
                    output: serde_json::to_string_pretty(&entries)?,
                    error: None,
                })
            }
            "get" => {
                if site.is_empty() || label.is_empty() {
                    return Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some("site and label are required for 'get' action".into()),
                    });
                }

                let vault = load_vault(&path, &store);
                let entry = vault
                    .credentials
                    .iter()
                    .find(|c| c.site == site && c.label == label);

                match entry {
                    Some(e) => {
                        // Return a REFERENCE TOKEN, not the decrypted value.
                        // The actual credential never leaves the local gateway.
                        // The browser tool resolves this token locally at fill time.
                        let ref_token = format!("{{{{CRED:{}:{}}}}}", site, label);
                        Ok(ToolResult {
                            success: true,
                            output: format!(
                                "Credential reference: {} (hint: {}). Pass this token directly to `browser fill` — the gateway will resolve it locally. The actual value is NOT exposed to the LLM.",
                                ref_token, e.display_hint
                            ),
                            error: None,
                        })
                    }
                    None => Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(format!("No credential found: site={site}, label={label}")),
                    }),
                }
            }
            "delete" => {
                if site.is_empty() || label.is_empty() {
                    return Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some("site and label are required for 'delete' action".into()),
                    });
                }

                let mut vault = load_vault(&path, &store);
                let before = vault.credentials.len();
                vault
                    .credentials
                    .retain(|c| !(c.site == site && c.label == label));
                let removed = before - vault.credentials.len();

                if removed > 0 {
                    save_vault(&path, &store, &vault)?;
                }

                Ok(ToolResult {
                    success: true,
                    output: format!("Deleted {removed} credential(s) for site={site}, label={label}"),
                    error: None,
                })
            }
            _ => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Unknown action: {action}. Use 'list', 'get', or 'delete'.")),
            }),
        }
    }
}

// ── Public credential reference resolver ────────────────────────

/// Pattern for credential reference tokens: `{{CRED:site:label}}`
const CRED_REF_PREFIX: &str = "{{CRED:";
const CRED_REF_SUFFIX: &str = "}}";

/// Check if a string contains a credential reference token.
pub fn contains_credential_ref(s: &str) -> bool {
    s.contains(CRED_REF_PREFIX) && s.contains(CRED_REF_SUFFIX)
}

/// Resolve all `{{CRED:site:label}}` tokens in a string by decrypting
/// the corresponding credentials from the local vault.
///
/// Called by the browser tool at form-fill time. The decrypted values
/// exist only in the returned String and should be used immediately
/// for the browser command, then dropped.
///
/// Returns the input string with all references replaced by decrypted values.
/// Unresolvable references are left as-is (the browser will likely error,
/// which is better than silently swallowing a missing credential).
pub fn resolve_credential_refs(input: &str, workspace_dir: &Path) -> String {
    if !contains_credential_ref(input) {
        return input.to_string();
    }

    let store = SecretStore::new(workspace_dir, true);
    let path = vault_path(workspace_dir);
    let vault = load_vault(&path, &store);

    let mut result = input.to_string();

    // Find and replace all {{CRED:site:label}} patterns
    while let Some(start) = result.find(CRED_REF_PREFIX) {
        let after_prefix = start + CRED_REF_PREFIX.len();
        let Some(end) = result[after_prefix..].find(CRED_REF_SUFFIX) else {
            break; // malformed — stop
        };
        let inner = &result[after_prefix..after_prefix + end]; // "site:label"
        let full_token = &result[start..after_prefix + end + CRED_REF_SUFFIX.len()];

        let parts: Vec<&str> = inner.splitn(2, ':').collect();
        if parts.len() == 2 {
            let site = parts[0];
            let label = parts[1];

            if let Some(entry) = vault
                .credentials
                .iter()
                .find(|c| c.site == site && c.label == label)
            {
                if let Ok(decrypted) = store.decrypt(&entry.encrypted_value) {
                    result = result.replacen(full_token, &decrypted, 1);
                    continue;
                }
            }
        }
        // Could not resolve — skip past this token to avoid infinite loop
        break;
    }

    result
}
