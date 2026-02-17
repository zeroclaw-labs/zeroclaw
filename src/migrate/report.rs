use std::fmt;

/// Tracks secrets read from source files for scrubbing from error messages.
pub struct SecretCollector {
    secrets: Vec<String>,
}

impl SecretCollector {
    pub fn new() -> Self {
        Self {
            secrets: Vec::new(),
        }
    }

    /// Register a secret value. Values shorter than 8 chars are ignored
    /// (too short to be meaningful secrets, high false-positive rate).
    pub fn add(&mut self, s: &str) {
        if s.len() >= 8 {
            self.secrets.push(s.to_string());
        }
    }

    /// Replace all registered secret values with [REDACTED].
    pub fn scrub(&self, msg: &str) -> String {
        let mut r = msg.to_string();
        for s in &self.secrets {
            r = r.replace(s.as_str(), "[REDACTED]");
        }
        r
    }
}

/// A single instance created (or to be created) during migration.
#[derive(Debug, Clone)]
pub struct CreatedEntry {
    pub agent_id: String,
    pub instance_id: String,
    pub instance_name: String,
    pub port: u16,
    pub workspace_path: String,
    pub channels: Vec<String>,
}

/// Full migration report returned to the caller.
pub struct MigrationReport {
    pub dry_run: bool,
    pub source_path: String,
    pub created: Vec<CreatedEntry>,
    pub warnings: Vec<String>,
    pub errors: Vec<String>,
}

impl fmt::Display for MigrationReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.dry_run {
            writeln!(f, "=== Migration Dry Run ===")?;
        } else {
            writeln!(f, "=== Migration Report ===")?;
        }
        writeln!(f, "Source: {}", self.source_path)?;
        writeln!(f)?;

        if !self.errors.is_empty() {
            writeln!(f, "ERRORS ({}):", self.errors.len())?;
            for e in &self.errors {
                writeln!(f, "  - {e}")?;
            }
            writeln!(f)?;
        }

        if !self.warnings.is_empty() {
            writeln!(f, "Warnings ({}):", self.warnings.len())?;
            for w in &self.warnings {
                writeln!(f, "  - {w}")?;
            }
            writeln!(f)?;
        }

        if !self.created.is_empty() {
            writeln!(
                f,
                "Instances {} ({}):",
                if self.dry_run {
                    "to create"
                } else {
                    "created"
                },
                self.created.len()
            )?;
            for c in &self.created {
                writeln!(f, "  {} (id: {})", c.instance_name, c.instance_id)?;
                writeln!(f, "    Port:      {}", c.port)?;
                writeln!(f, "    Workspace: {}", c.workspace_path)?;
                if !c.channels.is_empty() {
                    writeln!(f, "    Channels:  {}", c.channels.join(", "))?;
                }
            }
        }

        if !self.dry_run && !self.created.is_empty() && self.errors.is_empty() {
            writeln!(f)?;
            writeln!(
                f,
                "Note: Ports are reserved but not bound. If a port conflict exists, \
                 the instance will fail at start time with a clear bind error."
            )?;
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_collector_scrubs_exact_values() {
        let mut sc = SecretCollector::new();
        sc.add("sk-ant-very-secret-key-12345");
        sc.add("short"); // too short, should be ignored

        let scrubbed = sc.scrub("Error: API key sk-ant-very-secret-key-12345 is invalid");
        assert!(scrubbed.contains("[REDACTED]"));
        assert!(!scrubbed.contains("sk-ant-very-secret-key-12345"));
    }

    #[test]
    fn secret_collector_ignores_short_values() {
        let mut sc = SecretCollector::new();
        sc.add("short");
        assert_eq!(sc.scrub("short"), "short");
    }

    #[test]
    fn report_display_dry_run() {
        let report = MigrationReport {
            dry_run: true,
            source_path: "/home/user/.openclaw/openclaw.json".into(),
            created: vec![CreatedEntry {
                agent_id: "main".into(),
                instance_id: "uuid-1".into(),
                instance_name: "main".into(),
                port: 18801,
                workspace_path: "/home/user/projects".into(),
                channels: vec!["telegram".into()],
            }],
            warnings: vec!["Unsupported field 'tools'".into()],
            errors: vec![],
        };
        let output = format!("{report}");
        assert!(output.contains("Dry Run"));
        assert!(output.contains("to create"));
        assert!(output.contains("telegram"));
    }
}
