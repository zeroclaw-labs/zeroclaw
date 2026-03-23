//! Software-only path validation sandbox.

use crate::security::traits::Sandbox;
use std::path::{Path, PathBuf};
use std::process::Command;

fn home_dir() -> PathBuf {
    std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/root"))
}

#[derive(Debug, Clone)]
pub struct PathValidationSandbox {
    deny_paths: Vec<PathBuf>,
    allow_paths: Vec<PathBuf>,
}

impl PathValidationSandbox {
    pub fn new() -> Self {
        let home = home_dir();
        let deny_paths = vec![
            home.join(".ssh"),
            home.join(".gnupg"),
            home.join(".zeroclaw").join("secrets"),
            home.join(".zeroclaw").join("credentials"),
            home.join(".aws").join("credentials"),
            PathBuf::from("/etc/shadow"),
            PathBuf::from("/etc/passwd"),
        ];
        Self {
            deny_paths,
            allow_paths: Vec::new(),
        }
    }

    #[allow(dead_code)]
    pub fn with_deny_paths(mut self, paths: Vec<PathBuf>) -> Self {
        self.deny_paths.extend(paths);
        self
    }

    #[allow(dead_code)]
    pub fn with_allow_paths(mut self, paths: Vec<PathBuf>) -> Self {
        self.allow_paths = paths;
        self
    }

    fn validate_path(&self, path: &Path) -> Result<(), String> {
        let resolved = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        for denied in &self.deny_paths {
            if resolved.starts_with(denied) {
                return Err(format!(
                    "access denied: path '{}' is in protected area '{}'",
                    path.display(),
                    denied.display()
                ));
            }
        }
        if !self.allow_paths.is_empty() {
            let allowed = self.allow_paths.iter().any(|a| resolved.starts_with(a));
            if !allowed {
                return Err(format!(
                    "access denied: path '{}' is outside allowed directories",
                    path.display()
                ));
            }
        }
        Ok(())
    }
}

pub fn extract_paths_from_command(command: &str) -> Vec<PathBuf> {
    let home = home_dir();
    let mut paths = Vec::new();
    let mut in_quote = false;
    let mut current = String::new();
    let delimiters = [' ', '\t', ';', '|', '&', '(', ')', '<', '>'];
    for ch in command.chars() {
        match ch {
            '"' => in_quote = !in_quote,
            c if !in_quote && delimiters.contains(&c) => {
                if !current.is_empty() {
                    maybe_push_path(&current, &home, &mut paths);
                    current.clear();
                }
            }
            c => current.push(c),
        }
    }
    if !current.is_empty() {
        maybe_push_path(&current, &home, &mut paths);
    }
    paths
}

fn maybe_push_path(token: &str, home: &Path, paths: &mut Vec<PathBuf>) {
    if token.starts_with('/') {
        paths.push(PathBuf::from(token));
    } else if token.starts_with("~/") {
        paths.push(home.join(&token[2..]));
    }
}

impl Sandbox for PathValidationSandbox {
    fn wrap_command(&self, cmd: &mut Command) -> std::io::Result<()> {
        let program = cmd.get_program().to_string_lossy().to_string();
        let args: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().to_string())
            .collect();
        let full_command = if args.is_empty() {
            program.clone()
        } else {
            format!("{} {}", program, args.join(" "))
        };
        if program.starts_with('/') || program.starts_with("~/") {
            self.validate_path(Path::new(&program))
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::PermissionDenied, e))?;
        }
        let paths = extract_paths_from_command(&full_command);
        for path in &paths {
            self.validate_path(path)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::PermissionDenied, e))?;
        }
        Ok(())
    }

    fn is_available(&self) -> bool {
        true
    }

    fn name(&self) -> &str {
        "path-validation"
    }

    fn description(&self) -> &str {
        "Software-only path validation (validates file paths against deny/allow lists)"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_paths_finds_absolute() {
        let paths = extract_paths_from_command("cat /etc/hosts");
        assert_eq!(paths, vec![PathBuf::from("/etc/hosts")]);
    }

    #[test]
    fn extract_paths_finds_home() {
        let home = home_dir();
        let paths = extract_paths_from_command("cat ~/docs/file.txt");
        assert_eq!(paths, vec![home.join("docs/file.txt")]);
    }

    #[test]
    fn extract_paths_finds_multiple() {
        let paths = extract_paths_from_command("cp /src/a.txt /dst/b.txt");
        assert_eq!(
            paths,
            vec![PathBuf::from("/src/a.txt"), PathBuf::from("/dst/b.txt")]
        );
    }

    #[test]
    fn extract_paths_handles_pipes() {
        let paths = extract_paths_from_command("cat /etc/hosts | grep localhost");
        assert_eq!(paths, vec![PathBuf::from("/etc/hosts")]);
    }

    #[test]
    fn extract_paths_empty_for_builtins() {
        assert!(extract_paths_from_command("echo hello").is_empty());
    }

    #[test]
    fn validate_denies_protected_path() {
        let s = PathValidationSandbox {
            deny_paths: vec![PathBuf::from("/etc/shadow")],
            allow_paths: vec![],
        };
        assert!(s.validate_path(Path::new("/etc/shadow")).is_err());
    }

    #[test]
    fn validate_allows_non_protected_path() {
        let s = PathValidationSandbox {
            deny_paths: vec![PathBuf::from("/etc/shadow")],
            allow_paths: vec![],
        };
        assert!(s.validate_path(Path::new("/tmp/test.txt")).is_ok());
    }

    #[test]
    fn validate_denies_subdirectory() {
        let s = PathValidationSandbox {
            deny_paths: vec![PathBuf::from("/home/user/.ssh")],
            allow_paths: vec![],
        };
        assert!(s
            .validate_path(Path::new("/home/user/.ssh/id_rsa"))
            .is_err());
    }

    #[test]
    fn validate_allow_list_restricts() {
        let s = PathValidationSandbox {
            deny_paths: vec![],
            allow_paths: vec![PathBuf::from("/workspace")],
        };
        assert!(s.validate_path(Path::new("/workspace/file.rs")).is_ok());
        assert!(s.validate_path(Path::new("/etc/hosts")).is_err());
    }

    #[test]
    fn wrap_command_blocks_denied() {
        let s = PathValidationSandbox {
            deny_paths: vec![PathBuf::from("/etc/shadow")],
            allow_paths: vec![],
        };
        let mut cmd = Command::new("cat");
        cmd.arg("/etc/shadow");
        let r = s.wrap_command(&mut cmd);
        assert!(r.is_err());
        assert_eq!(r.unwrap_err().kind(), std::io::ErrorKind::PermissionDenied);
    }

    #[test]
    fn wrap_command_allows_safe() {
        let s = PathValidationSandbox {
            deny_paths: vec![PathBuf::from("/etc/shadow")],
            allow_paths: vec![],
        };
        let mut cmd = Command::new("cat");
        cmd.arg("/tmp/test.txt");
        assert!(s.wrap_command(&mut cmd).is_ok());
    }

    #[test]
    fn sandbox_name_and_availability() {
        let s = PathValidationSandbox::new();
        assert_eq!(s.name(), "path-validation");
        assert!(s.is_available());
    }
}
