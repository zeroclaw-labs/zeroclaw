//! Tirith pre-exec security scanning.
//!
//! Scans commands for content-level threats (homograph URLs, pipe-to-shell,
//! terminal injection, etc.) by invoking the tirith binary as a subprocess.
//!
//! Exit code is the verdict source of truth:
//!   0 = allow, 1 = block, 2 = warn
//!
//! Already integrated in Hermes Agent (NousResearch/hermes-agent#1256) and
//! EurekaClaw (EurekaClaw/EurekaClaw#1). This is the ZeroClaw adaptation.

use std::sync::OnceLock;
use std::time::Duration;

use serde::Deserialize;
use tokio::process::Command;

/// Shell kind tells the guard which tokenizer tirith should use.
#[derive(Clone, Copy, Debug)]
pub enum ShellKind {
    /// Native runtime: posix on Unix, cmd on Windows (matches native.rs).
    Native,
    /// Always POSIX (cron scheduler uses `sh -c` on all platforms).
    Posix,
}

/// Tirith configuration.
#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
pub struct TirithConfig {
    pub enabled: bool,
    pub bin: String,
    pub timeout_secs: u64,
    pub fail_open: bool,
}

impl Default for TirithConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            bin: "tirith".to_string(),
            timeout_secs: 5,
            fail_open: true,
        }
    }
}

/// Scan a command with tirith before execution.
///
/// Call **after** policy checks (allowlist, forbidden paths, risk classification)
/// which are cheap and deterministic. Tirith adds content-level scanning for
/// threats that regex/allowlist rules cannot catch.
///
/// Returns `Ok(())` if allowed, `Err(message)` if blocked.
pub async fn guard(command: &str, shell_kind: ShellKind, config: &TirithConfig) -> Result<(), String> {
    if !config.enabled {
        return Ok(());
    }

    let shell_flag = match shell_kind {
        ShellKind::Posix => "posix",
        ShellKind::Native => {
            if cfg!(windows) {
                "cmd"
            } else {
                "posix"
            }
        }
    };

    let bin = resolve_bin(&config.bin);

    let result = tokio::time::timeout(
        Duration::from_secs(config.timeout_secs),
        Command::new(bin)
            .args([
                "check",
                "--json",
                "--non-interactive",
                "--shell",
                shell_flag,
                "--",
                command,
            ])
            .output(),
    )
    .await;

    match result {
        Ok(Ok(output)) => match output.status.code() {
            Some(0) => Ok(()),
            Some(1) => {
                let summary = summarize_findings(&output.stdout);
                Err(format!("blocked by tirith security scan: {summary}"))
            }
            Some(2) => {
                let summary = summarize_findings(&output.stdout);
                tracing::warn!("tirith security warning: {summary}");
                Ok(())
            }
            _ if config.fail_open => {
                tracing::debug!("tirith: unexpected exit code (fail-open)");
                Ok(())
            }
            _ => Err("tirith: unexpected exit code (fail-closed)".into()),
        },
        Ok(Err(e)) if config.fail_open => {
            tracing::debug!("tirith unavailable (fail-open): {e}");
            Ok(())
        }
        Ok(Err(e)) => Err(format!("tirith failed (fail-closed): {e}")),
        Err(_) if config.fail_open => {
            tracing::debug!("tirith timed out (fail-open)");
            Ok(())
        }
        Err(_) => Err("tirith timed out (fail-closed)".into()),
    }
}

fn summarize_findings(stdout: &[u8]) -> String {
    #[derive(Deserialize)]
    struct Output {
        #[serde(default)]
        findings: Vec<Finding>,
    }
    #[derive(Deserialize)]
    struct Finding {
        #[serde(default)]
        severity: String,
        #[serde(default)]
        title: String,
    }

    let Ok(data) = serde_json::from_slice::<Output>(stdout) else {
        return "security issue detected (details unavailable)".to_string();
    };

    if data.findings.is_empty() {
        return "security issue detected".to_string();
    }

    data.findings
        .iter()
        .filter(|f| !f.title.is_empty())
        .map(|f| format!("[{}] {}", f.severity, f.title))
        .collect::<Vec<_>>()
        .join("; ")
}

/// Resolve the tirith binary path. Checks PATH first, then
/// ~/.zeroclaw/bin/tirith for a previously auto-installed copy.
fn resolve_bin(configured: &str) -> &str {
    static RESOLVED: OnceLock<String> = OnceLock::new();

    RESOLVED
        .get_or_init(|| {
            // If explicit path, use as-is
            if configured != "tirith" {
                return configured.to_string();
            }

            // Check PATH
            if which::which("tirith").is_ok() {
                return "tirith".to_string();
            }

            // Check ~/.zeroclaw/bin/tirith
            if let Some(home) = std::env::var_os("HOME")
                .or_else(|| std::env::var_os("USERPROFILE"))
                .map(std::path::PathBuf::from)
            {
                let local = home.join(".zeroclaw").join("bin").join("tirith");
                if local.is_file() {
                    return local.to_string_lossy().to_string();
                }
            }

            // Fallback to configured (will fail at spawn, handled by fail-open)
            configured.to_string()
        })
        .as_str()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let cfg = TirithConfig::default();
        assert!(cfg.enabled);
        assert_eq!(cfg.bin, "tirith");
        assert_eq!(cfg.timeout_secs, 5);
        assert!(cfg.fail_open);
    }

    #[test]
    fn test_summarize_empty() {
        let s = summarize_findings(b"{}");
        assert_eq!(s, "security issue detected");
    }

    #[test]
    fn test_summarize_with_findings() {
        let json = br#"{"findings":[{"severity":"HIGH","title":"Pipe to shell"}]}"#;
        let s = summarize_findings(json);
        assert!(s.contains("Pipe to shell"));
        assert!(s.contains("HIGH"));
    }

    #[test]
    fn test_summarize_invalid_json() {
        let s = summarize_findings(b"not json");
        assert!(s.contains("details unavailable"));
    }

    #[tokio::test]
    async fn test_disabled_allows() {
        let cfg = TirithConfig {
            enabled: false,
            ..Default::default()
        };
        assert!(guard("anything", ShellKind::Posix, &cfg).await.is_ok());
    }

    #[tokio::test]
    async fn test_missing_binary_fail_open() {
        let cfg = TirithConfig {
            enabled: true,
            bin: "/nonexistent/tirith".to_string(),
            timeout_secs: 1,
            fail_open: true,
        };
        assert!(guard("echo hello", ShellKind::Posix, &cfg).await.is_ok());
    }

    #[tokio::test]
    async fn test_missing_binary_fail_closed() {
        let cfg = TirithConfig {
            enabled: true,
            bin: "/nonexistent/tirith".to_string(),
            timeout_secs: 1,
            fail_open: false,
        };
        assert!(guard("echo hello", ShellKind::Posix, &cfg).await.is_err());
    }
}
