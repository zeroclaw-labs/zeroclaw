//! CLI command execution for ZeroClaw plugins.
//!
//! This module provides a typed interface for plugins to execute CLI commands
//! within the constraints defined by their manifest capabilities. All commands
//! are validated against the plugin's security policy before execution.
//!
//! # Manifest Configuration
//!
//! To use CLI capabilities, your plugin manifest must declare the `cli` capability
//! with appropriate permissions. Here's an example `plugin.toml`:
//!
//! ```toml
//! [plugin]
//! name = "my-git-plugin"
//! version = "1.0.0"
//!
//! [capabilities.cli]
//! # Commands the plugin is allowed to execute
//! allowed_commands = ["git", "npm", "cargo"]
//!
//! # Argument patterns for validation (regex)
//! # Each command can have allowed argument patterns
//! [capabilities.cli.allowed_args]
//! git = ["^(status|log|diff|branch|checkout)$", "^--[a-z-]+$"]
//! npm = ["^(install|run|test)$"]
//! cargo = ["^(build|test|check)$", "^--release$"]
//!
//! # Directories the plugin can use as working directory
//! allowed_paths = ["/workspace", "/tmp"]
//!
//! # Environment variables the plugin can set
//! allowed_env = ["PATH", "HOME", "GIT_AUTHOR_NAME"]
//! ```
//!
//! # Usage Examples
//!
//! ## Basic Command Execution
//!
//! ```rust,ignore
//! use zeroclaw_plugin_sdk::cli::{cli_exec, CliError};
//!
//! // Run a simple command
//! let response = cli_exec("git", &["status"], None, None)?;
//! println!("Exit code: {}", response.exit_code);
//! println!("Output: {}", response.stdout);
//! ```
//!
//! ## With Working Directory
//!
//! ```rust,ignore
//! use zeroclaw_plugin_sdk::cli::cli_exec;
//!
//! // Run command in a specific directory
//! let response = cli_exec(
//!     "cargo",
//!     &["build", "--release"],
//!     Some("/workspace/my-project"),
//!     None,
//! )?;
//!
//! if response.exit_code == 0 {
//!     println!("Build succeeded!");
//! } else {
//!     eprintln!("Build failed: {}", response.stderr);
//! }
//! ```
//!
//! ## With Environment Variables
//!
//! ```rust,ignore
//! use zeroclaw_plugin_sdk::cli::cli_exec;
//! use std::collections::HashMap;
//!
//! let mut env = HashMap::new();
//! env.insert("GIT_AUTHOR_NAME".to_string(), "Bot".to_string());
//!
//! let response = cli_exec(
//!     "git",
//!     &["commit", "-m", "Automated commit"],
//!     Some("/workspace/repo"),
//!     Some(env),
//! )?;
//! ```
//!
//! ## Error Handling
//!
//! ```rust,ignore
//! use zeroclaw_plugin_sdk::cli::{cli_exec, CliError, CliResponse};
//! use extism_pdk::Error;
//!
//! fn run_command() -> Result<CliResponse, Error> {
//!     cli_exec("git", &["status"], None, None)
//! }
//!
//! match run_command() {
//!     Ok(response) => {
//!         if response.truncated {
//!             eprintln!("Warning: output was truncated");
//!         }
//!         if response.timed_out {
//!             eprintln!("Warning: command timed out");
//!         }
//!         println!("{}", response.stdout);
//!     }
//!     Err(e) => {
//!         eprintln!("Command failed: {}", e);
//!     }
//! }
//! ```
//!
//! # Security Model
//!
//! All CLI operations are subject to security validation:
//!
//! - **Command allowlist**: Only commands listed in `allowed_commands` can be executed
//! - **Argument validation**: Arguments are validated against `allowed_args` patterns
//! - **Path restrictions**: Working directories must be within `allowed_paths`
//! - **Environment filtering**: Only variables in `allowed_env` are passed through
//! - **Rate limiting**: Commands may be rate-limited based on host configuration
//! - **Timeouts**: Long-running commands are terminated after the configured timeout
//! - **Output limits**: Large outputs are truncated to prevent memory exhaustion

use extism_pdk::*;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ---------------------------------------------------------------------------
// Request / response types (mirror the host-side structs)
// ---------------------------------------------------------------------------

/// Request to execute a CLI command.
#[derive(Debug, Clone, Serialize)]
pub struct CliRequest {
    /// The command to execute (e.g., "git", "npm").
    pub command: String,
    /// Arguments to pass to the command.
    pub args: Vec<String>,
    /// Working directory for command execution.
    /// Must be within the plugin's allowed_paths.
    pub working_dir: Option<String>,
    /// Environment variables to set for the command.
    /// Only variables in the plugin's allowed_env list will be applied.
    pub env: Option<HashMap<String, String>>,
}

/// Response from CLI command execution.
#[derive(Debug, Clone, Deserialize)]
pub struct CliResponse {
    /// Standard output from the command.
    pub stdout: String,
    /// Standard error from the command.
    pub stderr: String,
    /// Exit code returned by the command.
    pub exit_code: i32,
    /// Whether the output was truncated due to size limits.
    pub truncated: bool,
    /// Whether the command was terminated due to timeout.
    pub timed_out: bool,
}

/// Errors that can occur during CLI command execution.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub enum CliError {
    /// The command is not in the plugin's allowed_commands or arguments
    /// failed validation against allowed_args patterns.
    PermissionDenied,
    /// Rate limit exceeded. Retry after the specified duration.
    RateLimited {
        /// Seconds to wait before retrying.
        retry_after_secs: u64,
    },
    /// The specified command was not found on the system.
    CommandNotFound,
    /// Command execution failed with an error.
    ExecutionFailed {
        /// The stderr output from the failed command.
        stderr: String,
    },
    /// Command execution exceeded the time limit.
    Timeout,
    /// Output was truncated due to size limits.
    OutputTruncated,
}

impl std::fmt::Display for CliError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CliError::PermissionDenied => write!(f, "permission denied"),
            CliError::RateLimited { retry_after_secs } => {
                write!(f, "rate limited, retry after {} seconds", retry_after_secs)
            }
            CliError::CommandNotFound => write!(f, "command not found"),
            CliError::ExecutionFailed { stderr } => write!(f, "execution failed: {}", stderr),
            CliError::Timeout => write!(f, "command timed out"),
            CliError::OutputTruncated => write!(f, "output truncated"),
        }
    }
}

impl std::error::Error for CliError {}

// ---------------------------------------------------------------------------
// Host function import
// ---------------------------------------------------------------------------

#[host_fn]
extern "ExtismHost" {
    fn zeroclaw_cli_exec(input: Json<CliRequest>) -> Json<CliResponse>;
}

// ---------------------------------------------------------------------------
// Public wrapper API
// ---------------------------------------------------------------------------

/// Execute a CLI command with the given parameters.
///
/// # Arguments
///
/// * `command` - The command to execute (e.g., "git", "npm")
/// * `args` - Arguments to pass to the command
/// * `working_dir` - Optional working directory (must be in allowed_paths)
/// * `env` - Optional environment variables (filtered by allowed_env)
///
/// # Returns
///
/// Returns a `CliResponse` containing stdout, stderr, exit code, and flags
/// indicating if output was truncated or if the command timed out.
///
/// # Errors
///
/// Returns an error if:
/// - The command is not in the plugin's allowed_commands
/// - Arguments fail validation against allowed_args patterns
/// - The working directory is outside allowed_paths
/// - Rate limit is exceeded
/// - The host function call fails
pub fn cli_exec(
    command: &str,
    args: &[&str],
    working_dir: Option<&str>,
    env: Option<HashMap<String, String>>,
) -> Result<CliResponse, Error> {
    let request = CliRequest {
        command: command.to_string(),
        args: args.iter().map(|s| s.to_string()).collect(),
        working_dir: working_dir.map(|s| s.to_string()),
        env,
    };
    let Json(response) = unsafe { zeroclaw_cli_exec(Json(request))? };
    Ok(response)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // Core acceptance criterion: Error types cover permission denied
    // -----------------------------------------------------------------------

    #[test]
    fn cli_error_has_permission_denied_variant() {
        let err = CliError::PermissionDenied;

        match err {
            CliError::PermissionDenied => {}
            _ => panic!("Expected PermissionDenied variant"),
        }
    }

    #[test]
    fn cli_error_permission_denied_display() {
        let err = CliError::PermissionDenied;
        let msg = format!("{}", err);

        assert!(
            msg.contains("permission") || msg.contains("denied"),
            "PermissionDenied display should mention permission or denied, got: {}",
            msg
        );
    }

    #[test]
    fn cli_error_permission_denied_debug() {
        let err = CliError::PermissionDenied;
        let debug = format!("{:?}", err);

        assert!(
            debug.contains("PermissionDenied"),
            "Debug should contain PermissionDenied, got: {}",
            debug
        );
    }

    #[test]
    fn cli_error_permission_denied_clone() {
        let err = CliError::PermissionDenied;
        let cloned = err.clone();

        assert_eq!(err, cloned);
    }

    #[test]
    fn cli_error_permission_denied_equality() {
        let err1 = CliError::PermissionDenied;
        let err2 = CliError::PermissionDenied;

        assert_eq!(err1, err2);
    }

    // -----------------------------------------------------------------------
    // Core acceptance criterion: Error types cover rate limited
    // -----------------------------------------------------------------------

    #[test]
    fn cli_error_has_rate_limited_variant() {
        let err = CliError::RateLimited {
            retry_after_secs: 30,
        };

        match err {
            CliError::RateLimited { retry_after_secs } => {
                assert_eq!(retry_after_secs, 30);
            }
            _ => panic!("Expected RateLimited variant"),
        }
    }

    #[test]
    fn cli_error_rate_limited_display() {
        let err = CliError::RateLimited {
            retry_after_secs: 45,
        };
        let msg = format!("{}", err);

        assert!(
            msg.contains("rate") || msg.contains("limit"),
            "RateLimited display should mention rate or limit, got: {}",
            msg
        );
        assert!(
            msg.contains("45"),
            "RateLimited display should include retry_after_secs value, got: {}",
            msg
        );
    }

    #[test]
    fn cli_error_rate_limited_debug() {
        let err = CliError::RateLimited {
            retry_after_secs: 60,
        };
        let debug = format!("{:?}", err);

        assert!(
            debug.contains("RateLimited"),
            "Debug should contain RateLimited, got: {}",
            debug
        );
        assert!(
            debug.contains("60"),
            "Debug should contain retry_after_secs value, got: {}",
            debug
        );
    }

    #[test]
    fn cli_error_rate_limited_clone() {
        let err = CliError::RateLimited {
            retry_after_secs: 15,
        };
        let cloned = err.clone();

        assert_eq!(err, cloned);
    }

    #[test]
    fn cli_error_rate_limited_equality() {
        let err1 = CliError::RateLimited {
            retry_after_secs: 30,
        };
        let err2 = CliError::RateLimited {
            retry_after_secs: 30,
        };
        let err3 = CliError::RateLimited {
            retry_after_secs: 60,
        };

        assert_eq!(err1, err2);
        assert_ne!(err1, err3);
    }

    #[test]
    fn cli_error_rate_limited_zero_retry() {
        let err = CliError::RateLimited {
            retry_after_secs: 0,
        };

        match err {
            CliError::RateLimited { retry_after_secs } => {
                assert_eq!(retry_after_secs, 0);
            }
            _ => panic!("Expected RateLimited variant"),
        }
    }

    #[test]
    fn cli_error_rate_limited_large_retry() {
        let err = CliError::RateLimited {
            retry_after_secs: 3600, // 1 hour
        };

        match err {
            CliError::RateLimited { retry_after_secs } => {
                assert_eq!(retry_after_secs, 3600);
            }
            _ => panic!("Expected RateLimited variant"),
        }
    }

    // -----------------------------------------------------------------------
    // CliError implements std::error::Error
    // -----------------------------------------------------------------------

    #[test]
    fn cli_error_implements_std_error() {
        fn assert_error<E: std::error::Error>() {}

        assert_error::<CliError>();
    }

    #[test]
    fn cli_error_as_boxed_error() {
        let err: Box<dyn std::error::Error> = Box::new(CliError::PermissionDenied);

        let _ = format!("{}", err);
        let _ = format!("{:?}", err);
    }

    #[test]
    fn cli_error_with_question_mark() {
        fn returns_permission_denied() -> Result<(), CliError> {
            Err(CliError::PermissionDenied)
        }

        fn returns_rate_limited() -> Result<(), CliError> {
            Err(CliError::RateLimited {
                retry_after_secs: 10,
            })
        }

        assert!(returns_permission_denied().is_err());
        assert!(returns_rate_limited().is_err());
    }

    // -----------------------------------------------------------------------
    // PermissionDenied vs RateLimited distinction
    // -----------------------------------------------------------------------

    #[test]
    fn permission_denied_and_rate_limited_are_distinct() {
        let permission_err = CliError::PermissionDenied;
        let rate_err = CliError::RateLimited {
            retry_after_secs: 30,
        };

        assert_ne!(permission_err, rate_err);
    }

    #[test]
    fn pattern_matching_distinguishes_error_types() {
        let errors = vec![
            CliError::PermissionDenied,
            CliError::RateLimited {
                retry_after_secs: 30,
            },
        ];

        let mut permission_denied_count = 0;
        let mut rate_limited_count = 0;

        for err in errors {
            match err {
                CliError::PermissionDenied => permission_denied_count += 1,
                CliError::RateLimited { .. } => rate_limited_count += 1,
                _ => {}
            }
        }

        assert_eq!(permission_denied_count, 1);
        assert_eq!(rate_limited_count, 1);
    }

    // -----------------------------------------------------------------------
    // Verify all expected CliError variants exist
    // -----------------------------------------------------------------------

    #[test]
    fn cli_error_has_all_expected_variants() {
        let variants: Vec<CliError> = vec![
            CliError::PermissionDenied,
            CliError::RateLimited {
                retry_after_secs: 0,
            },
            CliError::CommandNotFound,
            CliError::ExecutionFailed {
                stderr: String::new(),
            },
            CliError::Timeout,
            CliError::OutputTruncated,
        ];

        assert_eq!(variants.len(), 6, "CliError should have 6 variants");

        for i in 0..variants.len() {
            for j in (i + 1)..variants.len() {
                assert_ne!(
                    variants[i], variants[j],
                    "Variants {} and {} should be distinct",
                    i, j
                );
            }
        }
    }
}
