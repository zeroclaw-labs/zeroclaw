//! Command validation and argument security for plugin CLI execution.

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Command;
use std::sync::{LazyLock, RwLock};

static COMMAND_PATH_CACHE: LazyLock<RwLock<HashMap<String, PathBuf>>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandNotFoundError {
    pub command: String,
}

impl std::fmt::Display for CommandNotFoundError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "command not found: {}", self.command)
    }
}

impl std::error::Error for CommandNotFoundError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandNotAllowedError {
    pub command: String,
    pub reason: String,
}

impl std::fmt::Display for CommandNotAllowedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "command not allowed: {} ({})", self.command, self.reason)
    }
}

impl std::error::Error for CommandNotAllowedError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArgumentValidationError {
    pub argument: String,
    pub reason: String,
}

impl std::fmt::Display for ArgumentValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "argument validation failed: {} ({})",
            self.argument, self.reason
        )
    }
}

impl std::error::Error for ArgumentValidationError {}

pub const SHELL_METACHARACTERS: &[char] = &[
    ';', '|', '&', '`', '$', '<', '>', '(', ')', '\'', '"', '\\', '\n', '\0',
];

pub fn is_safe_argument(arg: &str) -> bool {
    !arg.contains(SHELL_METACHARACTERS)
}

pub fn resolve_command_path(command: &str) -> Result<PathBuf, CommandNotFoundError> {
    {
        let cache = COMMAND_PATH_CACHE.read().unwrap();
        if let Some(path) = cache.get(command) {
            return Ok(path.clone());
        }
    }

    let output = Command::new("which")
        .arg(command)
        .output()
        .map_err(|_| CommandNotFoundError {
            command: command.to_string(),
        })?;

    if !output.status.success() {
        return Err(CommandNotFoundError {
            command: command.to_string(),
        });
    }

    let path_str = String::from_utf8_lossy(&output.stdout);
    let path = PathBuf::from(path_str.trim());

    if !path.is_absolute() {
        return Err(CommandNotFoundError {
            command: command.to_string(),
        });
    }

    {
        let mut cache = COMMAND_PATH_CACHE.write().unwrap();
        cache.insert(command.to_string(), path.clone());
    }

    Ok(path)
}

#[cfg(test)]
pub fn clear_command_path_cache() {
    let mut cache = COMMAND_PATH_CACHE.write().unwrap();
    cache.clear();
}

pub fn validate_command_allowlist(
    command: &str,
    allowed_commands: &[String],
) -> Result<PathBuf, CommandNotAllowedError> {
    if allowed_commands.iter().any(|c| c.trim() == "*") {
        return Err(CommandNotAllowedError {
            command: command.to_string(),
            reason: "wildcards are not allowed in plugin command allowlists".to_string(),
        });
    }

    let resolved_path = resolve_command_path(command).map_err(|e| CommandNotAllowedError {
        command: command.to_string(),
        reason: e.to_string(),
    })?;

    for allowed in allowed_commands {
        if allowed.starts_with('/') {
            let allowed_path = PathBuf::from(allowed);
            if resolved_path == allowed_path {
                return Ok(resolved_path);
            }
        } else if let Ok(allowed_resolved) = resolve_command_path(allowed)
            && resolved_path == allowed_resolved
        {
            return Ok(resolved_path);
        }
    }

    Err(CommandNotAllowedError {
        command: command.to_string(),
        reason: "command not in allowlist".to_string(),
    })
}

#[cfg(feature = "plugins-wasm")]
pub fn validate_arguments(
    command: &str,
    args: &[&str],
    allowed_patterns: &[zeroclaw_plugins::capabilities::ArgPattern],
) -> Result<(), ArgumentValidationError> {
    for arg in args {
        if !is_safe_argument(arg) {
            return Err(ArgumentValidationError {
                argument: arg.to_string(),
                reason: "contains shell metacharacter".to_string(),
            });
        }
    }

    let pattern = allowed_patterns.iter().find(|p| p.command == command);

    match pattern {
        Some(p) => {
            for arg in args {
                if !p.matches(command, arg) {
                    return Err(ArgumentValidationError {
                        argument: arg.to_string(),
                        reason: format!(
                            "does not match any allowed pattern for command '{}'",
                            command
                        ),
                    });
                }
            }
            Ok(())
        }
        None => {
            if args.is_empty() {
                Ok(())
            } else {
                Err(ArgumentValidationError {
                    argument: args[0].to_string(),
                    reason: format!("no argument patterns defined for command '{}'", command),
                })
            }
        }
    }
}

#[cfg(feature = "plugins-wasm")]
pub fn validate_arguments_strict(
    command: &str,
    args: &[&str],
    allowed_patterns: &[zeroclaw_plugins::capabilities::ArgPattern],
) -> Result<(), ArgumentValidationError> {
    for arg in args {
        if !is_safe_argument(arg) {
            return Err(ArgumentValidationError {
                argument: arg.to_string(),
                reason: "contains shell metacharacter".to_string(),
            });
        }
    }

    let pattern = allowed_patterns.iter().find(|p| p.command == command);

    match pattern {
        Some(p) => {
            if p.has_wildcards() {
                return Err(ArgumentValidationError {
                    argument: String::new(),
                    reason: format!(
                        "Strict security level rejects wildcard patterns for command '{}'; use exact patterns only",
                        command
                    ),
                });
            }
            for arg in args {
                if !p.matches_exact(command, arg) {
                    return Err(ArgumentValidationError {
                        argument: arg.to_string(),
                        reason: format!(
                            "does not exactly match any allowed pattern for command '{}' (Strict mode)",
                            command
                        ),
                    });
                }
            }
            Ok(())
        }
        None => {
            if args.is_empty() {
                Ok(())
            } else {
                Err(ArgumentValidationError {
                    argument: args[0].to_string(),
                    reason: format!("no argument patterns defined for command '{}'", command),
                })
            }
        }
    }
}

pub fn validate_path_traversal(args: &[&str]) -> Result<(), ArgumentValidationError> {
    for arg in args {
        if contains_path_traversal(arg) {
            return Err(ArgumentValidationError {
                argument: arg.to_string(),
                reason: "contains path traversal sequence '..'".to_string(),
            });
        }
    }
    Ok(())
}

fn contains_path_traversal(s: &str) -> bool {
    if s == ".." {
        return true;
    }
    let bytes = s.as_bytes();
    let len = bytes.len();
    for i in 0..len.saturating_sub(1) {
        if bytes[i] == b'.' && bytes[i + 1] == b'.' {
            let before_ok = i == 0 || bytes[i - 1] == b'/' || bytes[i - 1] == b'\\';
            let after_ok = i + 2 >= len || bytes[i + 2] == b'/' || bytes[i + 2] == b'\\';
            if before_ok && after_ok {
                return true;
            }
        }
    }
    false
}

#[cfg(feature = "plugins-wasm")]
pub fn warn_broad_cli_patterns(
    plugin_name: &str,
    command: &str,
    allowed_patterns: &[zeroclaw_plugins::capabilities::ArgPattern],
) {
    let Some(pattern) = allowed_patterns.iter().find(|p| p.command == command) else {
        return;
    };
    let broad = pattern.get_broad_patterns();
    if !broad.is_empty() {
        tracing::warn!(
            plugin = %plugin_name,
            command = %command,
            patterns = ?broad,
            "CLI allowed_args contains broad patterns (ending with '*') which may allow \
             more arguments than intended; consider using exact patterns for tighter security"
        );
    }
}
