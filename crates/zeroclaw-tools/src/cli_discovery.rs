//! CLI tool auto-discovery — scans PATH for known CLI tools.
//! Zero external dependencies (uses `std::process::Command` + `std::env`).

use std::path::PathBuf;

/// Category of a discovered CLI tool.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub enum CliCategory {
    VersionControl,
    Language,
    PackageManager,
    Container,
    Build,
    Cloud,
    AiAgent,
    Productivity,
}

impl std::fmt::Display for CliCategory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::VersionControl => write!(f, "Version Control"),
            Self::Language => write!(f, "Language"),
            Self::PackageManager => write!(f, "Package Manager"),
            Self::Container => write!(f, "Container"),
            Self::Build => write!(f, "Build"),
            Self::Cloud => write!(f, "Cloud"),
            Self::AiAgent => write!(f, "AI Agent"),
            Self::Productivity => write!(f, "Productivity"),
        }
    }
}

/// A discovered CLI tool with metadata.
#[derive(Debug, Clone, serde::Serialize)]
pub struct DiscoveredCli {
    pub name: String,
    pub path: PathBuf,
    pub version: Option<String>,
    pub category: CliCategory,
}

/// Known CLI tools to scan for.
struct KnownCli {
    name: &'static str,
    version_args: &'static [&'static str],
    category: CliCategory,
}

const KNOWN_CLIS: &[KnownCli] = &[
    KnownCli {
        name: "git",
        version_args: &["--version"],
        category: CliCategory::VersionControl,
    },
    KnownCli {
        name: "python",
        version_args: &["--version"],
        category: CliCategory::Language,
    },
    KnownCli {
        name: "python3",
        version_args: &["--version"],
        category: CliCategory::Language,
    },
    KnownCli {
        name: "node",
        version_args: &["--version"],
        category: CliCategory::Language,
    },
    KnownCli {
        name: "npm",
        version_args: &["--version"],
        category: CliCategory::PackageManager,
    },
    KnownCli {
        name: "pip",
        version_args: &["--version"],
        category: CliCategory::PackageManager,
    },
    KnownCli {
        name: "pip3",
        version_args: &["--version"],
        category: CliCategory::PackageManager,
    },
    KnownCli {
        name: "docker",
        version_args: &["--version"],
        category: CliCategory::Container,
    },
    KnownCli {
        name: "cargo",
        version_args: &["--version"],
        category: CliCategory::Build,
    },
    KnownCli {
        name: "make",
        version_args: &["--version"],
        category: CliCategory::Build,
    },
    KnownCli {
        name: "kubectl",
        version_args: &["version", "--client", "--short"],
        category: CliCategory::Cloud,
    },
    KnownCli {
        name: "rustc",
        version_args: &["--version"],
        category: CliCategory::Language,
    },
    KnownCli {
        name: "claude",
        version_args: &["--version"],
        category: CliCategory::AiAgent,
    },
    KnownCli {
        name: "gemini",
        version_args: &["--version"],
        category: CliCategory::AiAgent,
    },
    KnownCli {
        name: "kilo",
        version_args: &["--version"],
        category: CliCategory::AiAgent,
    },
    KnownCli {
        name: "gws",
        version_args: &["--version"],
        category: CliCategory::Productivity,
    },
];

/// Discover available CLI tools on the system.
/// Scans PATH for known tools and returns metadata for each found.
pub fn discover_cli_tools(additional: &[String], excluded: &[String]) -> Vec<DiscoveredCli> {
    // Build the probe list first — cheap, no spawns — preserving the
    // KNOWN_CLIS-then-`additional` ordering that callers and tests rely on.
    let mut probes: Vec<(&str, &[&str], CliCategory)> = Vec::new();

    for known in KNOWN_CLIS {
        if excluded.iter().any(|e| e == known.name) {
            continue;
        }
        probes.push((known.name, known.version_args, known.category.clone()));
    }

    // Append additional user-specified tools, skipping duplicates.
    for tool_name in additional {
        if excluded.iter().any(|e| e == tool_name) {
            continue;
        }
        if probes.iter().any(|(n, _, _)| *n == tool_name.as_str()) {
            continue;
        }
        probes.push((tool_name.as_str(), &["--version"], CliCategory::Build));
    }

    // Probe concurrently. Each tool needs up to two short-lived child
    // processes (`where`/`which` + `--version`); running them serially made
    // `/api/cli-tools` visibly slow (wall time ~= sum of every probe). Scoped
    // threads bound the scan by the slowest single tool instead. Output order
    // is preserved because handles are joined in spawn order.
    std::thread::scope(|scope| {
        let handles: Vec<_> = probes
            .into_iter()
            .map(|(name, args, category)| scope.spawn(move || probe_cli(name, args, category)))
            .collect();
        handles
            .into_iter()
            .filter_map(|h| h.join().ok().flatten())
            .collect()
    })
}

/// Suppress the console window that Windows otherwise spawns for each
/// short-lived child process. Without this, hitting `/api/cli-tools` from the
/// web UI flashes a `cmd`/console window for every probe — distracting in GUI
/// and service contexts. No-op on non-Windows platforms. Mirrors the runtime
/// shell-command fix from issue #5562.
#[cfg(target_os = "windows")]
fn hide_console(cmd: &mut std::process::Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    cmd.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(target_os = "windows"))]
fn hide_console(_cmd: &mut std::process::Command) {}

/// Probe a single CLI tool: check if it exists and get its version.
fn probe_cli(name: &str, version_args: &[&str], category: CliCategory) -> Option<DiscoveredCli> {
    // Try to find the tool using `which` (Unix) or `where` (Windows)
    let path = find_executable(name)?;

    // Try to get version
    let version = get_version(name, version_args);

    Some(DiscoveredCli {
        name: name.to_string(),
        path,
        version,
        category,
    })
}

/// Find an executable on PATH.
fn find_executable(name: &str) -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    let which_cmd = "where";
    #[cfg(not(target_os = "windows"))]
    let which_cmd = "which";

    let mut cmd = std::process::Command::new(which_cmd);
    cmd.arg(name)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    hide_console(&mut cmd);
    let output = cmd.output().ok()?;

    if !output.status.success() {
        return None;
    }

    let path_str = String::from_utf8_lossy(&output.stdout);
    let first_line = path_str.lines().next()?.trim();
    if first_line.is_empty() {
        return None;
    }
    Some(PathBuf::from(first_line))
}

/// Get the version string of a CLI tool.
fn get_version(name: &str, args: &[&str]) -> Option<String> {
    let mut cmd = std::process::Command::new(name);
    cmd.args(args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    hide_console(&mut cmd);
    let output = cmd.output().ok()?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    // Some tools print version to stderr (e.g., pip)
    let version_text = if stdout.trim().is_empty() {
        stderr.trim().to_string()
    } else {
        stdout.trim().to_string()
    };

    // Extract first line only
    let first_line = version_text.lines().next()?.trim().to_string();
    if first_line.is_empty() {
        None
    } else {
        Some(first_line)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discover_returns_vec() {
        // Just verify it runs without panic
        let results = discover_cli_tools(&[], &[]);
        // We can't assert specific tools exist in CI, but structure is valid
        for cli in &results {
            assert!(!cli.name.is_empty());
        }
    }

    #[test]
    fn excluded_tools_are_skipped() {
        let results = discover_cli_tools(&[], &["git".to_string()]);
        assert!(!results.iter().any(|r| r.name == "git"));
    }

    #[test]
    fn category_display() {
        assert_eq!(CliCategory::VersionControl.to_string(), "Version Control");
        assert_eq!(CliCategory::Language.to_string(), "Language");
        assert_eq!(CliCategory::PackageManager.to_string(), "Package Manager");
        assert_eq!(CliCategory::Container.to_string(), "Container");
        assert_eq!(CliCategory::Build.to_string(), "Build");
        assert_eq!(CliCategory::Cloud.to_string(), "Cloud");
        assert_eq!(CliCategory::AiAgent.to_string(), "AI Agent");
        assert_eq!(CliCategory::Productivity.to_string(), "Productivity");
    }
}
