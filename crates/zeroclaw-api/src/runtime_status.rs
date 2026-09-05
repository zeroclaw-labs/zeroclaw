//! Shared runtime-status wire types.

use crate::runtime_traits::{ShellDialect, ShellProfile};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeConfigKind {
    Default,
    Custom,
    Temporary,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeShellFamily {
    Posix,
    #[serde(rename = "cmd")]
    Cmd,
    #[serde(rename = "powershell")]
    PowerShell,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct RuntimeShellProfile {
    pub name: String,
    pub family: RuntimeShellFamily,
}

impl RuntimeShellProfile {
    #[must_use]
    pub fn from_runtime_profile(profile: ShellProfile) -> Option<Self> {
        let family = match profile.dialect {
            ShellDialect::Posix => RuntimeShellFamily::Posix,
            ShellDialect::WindowsCmd => RuntimeShellFamily::Cmd,
            ShellDialect::PowerShell => RuntimeShellFamily::PowerShell,
            ShellDialect::None => return None,
        };

        Some(Self {
            name: profile.name,
            family,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile(name: &str, dialect: ShellDialect) -> ShellProfile {
        ShellProfile {
            name: name.to_string(),
            dialect,
        }
    }

    #[test]
    fn shell_profile_maps_effective_runtime_dialect_to_status_family() {
        for (dialect, family, encoded) in [
            (ShellDialect::Posix, RuntimeShellFamily::Posix, "posix"),
            (ShellDialect::WindowsCmd, RuntimeShellFamily::Cmd, "cmd"),
            (
                ShellDialect::PowerShell,
                RuntimeShellFamily::PowerShell,
                "powershell",
            ),
        ] {
            let status =
                RuntimeShellProfile::from_runtime_profile(profile("pwsh", dialect)).unwrap();
            assert_eq!(status.family, family);
            assert_eq!(
                serde_json::to_value(status.family).unwrap(),
                serde_json::json!(encoded)
            );
        }
    }

    #[test]
    fn shell_profile_omits_shell_less_runtime() {
        assert_eq!(
            RuntimeShellProfile::from_runtime_profile(profile("none", ShellDialect::None)),
            None
        );
    }
}
