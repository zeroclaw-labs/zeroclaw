//! Tamper-evident audit logging with hash chaining.
//!
//! Each [`AuditEntry`] includes a SHA-256 hash of the previous entry, forming
//! an append-only chain that detects tampering. The [`TamperEvidentLog`]
//! manages writing, rotation with chain continuity, and integrity verification.

use anyhow::{bail, Result};
use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::OpenOptions;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use uuid::Uuid;

use crate::security::compliance::ComplianceFramework;

/// A single tamper-evident audit log entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEntry {
    /// UTC timestamp of the event.
    pub timestamp: DateTime<Utc>,
    /// Unique entry identifier.
    pub entry_id: String,
    /// Actor who performed the action.
    pub actor: String,
    /// Action performed (tool name or operation).
    pub action: String,
    /// Tool that executed the action, if any.
    pub tool: Option<String>,
    /// Sanitized parameters (secrets redacted).
    pub parameters: Option<serde_json::Value>,
    /// Brief result summary.
    pub result_summary: String,
    /// Regulatory framework tags for this entry.
    pub compliance_tags: Vec<String>,
    /// SHA-256 hash of this entry (computed over prev_hash + entry data).
    pub hash: String,
    /// Hash of the previous entry in the chain (empty string for first entry).
    pub prev_hash: String,
}

impl AuditEntry {
    /// Compute the SHA-256 hash for this entry.
    ///
    /// Hash input: `prev_hash | timestamp | actor | action | tool | result_summary | compliance_tags`
    fn compute_hash(
        prev_hash: &str,
        timestamp: &DateTime<Utc>,
        actor: &str,
        action: &str,
        tool: Option<&str>,
        result_summary: &str,
        compliance_tags: &[String],
    ) -> String {
        let mut hasher = Sha256::new();
        hasher.update(prev_hash.as_bytes());
        hasher.update(timestamp.to_rfc3339().as_bytes());
        hasher.update(actor.as_bytes());
        hasher.update(action.as_bytes());
        hasher.update(tool.unwrap_or("").as_bytes());
        hasher.update(result_summary.as_bytes());
        for tag in compliance_tags {
            hasher.update(tag.as_bytes());
        }
        hex::encode(hasher.finalize())
    }

    /// Create a new entry chained to the given previous hash.
    pub fn new(
        prev_hash: &str,
        actor: String,
        action: String,
        tool: Option<String>,
        parameters: Option<serde_json::Value>,
        result_summary: String,
        compliance_tags: Vec<ComplianceFramework>,
    ) -> Self {
        let tag_labels: Vec<String> = compliance_tags
            .iter()
            .map(|f| f.label().to_string())
            .collect();
        let timestamp = Utc::now();
        let hash = Self::compute_hash(
            prev_hash,
            &timestamp,
            &actor,
            &action,
            tool.as_deref(),
            &result_summary,
            &tag_labels,
        );
        Self {
            timestamp,
            entry_id: Uuid::new_v4().to_string(),
            actor,
            action,
            tool,
            parameters,
            result_summary,
            compliance_tags: tag_labels,
            hash,
            prev_hash: prev_hash.to_string(),
        }
    }

    /// Verify that this entry's hash matches its contents.
    pub fn verify(&self) -> bool {
        let expected = Self::compute_hash(
            &self.prev_hash,
            &self.timestamp,
            &self.actor,
            &self.action,
            self.tool.as_deref(),
            &self.result_summary,
            &self.compliance_tags,
        );
        self.hash == expected
    }
}

/// Export format for audit logs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportFormat {
    Json,
    Csv,
    /// Common Event Format (SIEM-compatible).
    Cef,
}

impl ExportFormat {
    /// Parse from string.
    pub fn from_str_name(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "json" => Some(Self::Json),
            "csv" => Some(Self::Csv),
            "cef" => Some(Self::Cef),
            _ => None,
        }
    }
}

/// Tamper-evident append-only log with hash chaining.
pub struct TamperEvidentLog {
    log_path: PathBuf,
    last_hash: Mutex<String>,
    max_size_bytes: u64,
}

impl TamperEvidentLog {
    /// Create or open a tamper-evident log at the given path.
    ///
    /// If the file already has entries, the last hash is read to continue
    /// the chain. `max_size_mb` controls rotation threshold (0 = no rotation).
    pub fn new(log_path: PathBuf, max_size_mb: u32) -> Result<Self> {
        if let Some(parent) = log_path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }

        // Read last hash from existing file if present
        let last_hash = Self::read_last_hash(&log_path)?;

        Ok(Self {
            log_path,
            last_hash: Mutex::new(last_hash),
            max_size_bytes: u64::from(max_size_mb) * 1024 * 1024,
        })
    }

    /// Read the last entry hash from the log file, or empty string if none.
    fn read_last_hash(path: &Path) -> Result<String> {
        if !path.exists() {
            return Ok(String::new());
        }
        let file = std::fs::File::open(path)?;
        let reader = BufReader::new(file);
        let mut last_hash = String::new();
        for line in reader.lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            if let Ok(entry) = serde_json::from_str::<AuditEntry>(&line) {
                last_hash = entry.hash;
            }
        }
        Ok(last_hash)
    }

    /// Append an entry to the log, chaining it to the previous hash.
    pub fn append(
        &self,
        actor: String,
        action: String,
        tool: Option<String>,
        parameters: Option<serde_json::Value>,
        result_summary: String,
        compliance_tags: Vec<ComplianceFramework>,
    ) -> Result<AuditEntry> {
        self.rotate_if_needed()?;

        let mut last_hash = self.last_hash.lock();
        let entry = AuditEntry::new(
            &last_hash,
            actor,
            action,
            tool,
            parameters,
            result_summary,
            compliance_tags,
        );

        let line = serde_json::to_string(&entry)?;
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.log_path)?;
        writeln!(file, "{}", line)?;
        file.sync_all()?;

        *last_hash = entry.hash.clone();
        Ok(entry)
    }

    /// Verify the entire hash chain in the log file.
    ///
    /// Returns `Ok(entry_count)` if the chain is intact, or an error describing
    /// the first broken link.
    pub fn verify_chain(&self) -> Result<usize> {
        Self::verify_chain_at(&self.log_path)
    }

    /// Verify the hash chain of a specific log file.
    pub fn verify_chain_at(path: &Path) -> Result<usize> {
        if !path.exists() {
            return Ok(0);
        }
        let file = std::fs::File::open(path)?;
        let reader = BufReader::new(file);
        let mut prev_hash = String::new();
        let mut count = 0usize;

        for (line_num, line) in reader.lines().enumerate() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            let entry: AuditEntry = serde_json::from_str(&line)
                .map_err(|e| anyhow::anyhow!("line {}: parse error: {}", line_num + 1, e))?;

            if entry.prev_hash != prev_hash {
                bail!(
                    "chain break at line {}: expected prev_hash '{}', got '{}'",
                    line_num + 1,
                    prev_hash,
                    entry.prev_hash
                );
            }
            if !entry.verify() {
                bail!(
                    "hash mismatch at line {}: entry hash does not match computed value",
                    line_num + 1,
                );
            }
            prev_hash = entry.hash.clone();
            count += 1;
        }
        Ok(count)
    }

    /// Read all entries from the log.
    pub fn read_entries(&self) -> Result<Vec<AuditEntry>> {
        Self::read_entries_at(&self.log_path)
    }

    /// Read all entries from a specific log file.
    pub fn read_entries_at(path: &Path) -> Result<Vec<AuditEntry>> {
        if !path.exists() {
            return Ok(Vec::new());
        }
        let file = std::fs::File::open(path)?;
        let reader = BufReader::new(file);
        let mut entries = Vec::new();
        for line in reader.lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            entries.push(serde_json::from_str(&line)?);
        }
        Ok(entries)
    }

    /// Export entries in the specified format.
    pub fn export(&self, format: ExportFormat) -> Result<String> {
        let entries = self.read_entries()?;
        match format {
            ExportFormat::Json => Ok(serde_json::to_string_pretty(&entries)?),
            ExportFormat::Csv => Self::export_csv(&entries),
            ExportFormat::Cef => Self::export_cef(&entries),
        }
    }

    fn export_csv(entries: &[AuditEntry]) -> Result<String> {
        let mut out = String::from(
            "timestamp,entry_id,actor,action,tool,result_summary,compliance_tags,hash,prev_hash\n",
        );
        for e in entries {
            use std::fmt::Write;
            let _ = writeln!(
                out,
                "{},{},{},{},{},{},{},{},{}",
                e.timestamp.to_rfc3339(),
                e.entry_id,
                csv_escape(&e.actor),
                csv_escape(&e.action),
                csv_escape(e.tool.as_deref().unwrap_or("")),
                csv_escape(&e.result_summary),
                csv_escape(&e.compliance_tags.join(";")),
                e.hash,
                e.prev_hash,
            );
        }
        Ok(out)
    }

    fn export_cef(entries: &[AuditEntry]) -> Result<String> {
        let mut out = String::new();
        for e in entries {
            // CEF:Version|Device Vendor|Device Product|Device Version|Signature ID|Name|Severity|Extension
            use std::fmt::Write;
            let _ = writeln!(
                out,
                "CEF:0|ZeroClaw|AgentRuntime|0.1.0|{}|{}|5|act={} suser={} outcome={} cs1Label=compliance cs1={}",
                e.entry_id,
                cef_escape(&e.action),
                cef_escape(&e.action),
                cef_escape(&e.actor),
                cef_escape(&e.result_summary),
                cef_escape_ext(&e.compliance_tags.join(";")),
            );
        }
        Ok(out)
    }

    /// Rotate the log if it exceeds the max size.
    fn rotate_if_needed(&self) -> Result<()> {
        if self.max_size_bytes == 0 {
            return Ok(());
        }
        match std::fs::metadata(&self.log_path) {
            Ok(metadata) => {
                if metadata.len() >= self.max_size_bytes {
                    self.rotate()?;
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // File doesn't exist yet — nothing to rotate.
            }
            Err(e) => {
                tracing::warn!(
                    "Failed to read audit log metadata at {}: {}",
                    self.log_path.display(),
                    e
                );
            }
        }
        Ok(())
    }

    /// Rotate log files while preserving chain continuity.
    ///
    /// The last hash from the rotated file is kept so the next entry
    /// in the new file continues the chain.
    fn rotate(&self) -> Result<()> {
        for i in (1..10).rev() {
            let old = format!("{}.{}.log", self.log_path.display(), i);
            let new = format!("{}.{}.log", self.log_path.display(), i + 1);
            if let Err(e) = std::fs::rename(&old, &new) {
                if e.kind() != std::io::ErrorKind::NotFound {
                    tracing::warn!("Failed to rotate audit log {} -> {}: {}", old, new, e);
                }
            }
        }
        let rotated = format!("{}.1.log", self.log_path.display());
        std::fs::rename(&self.log_path, &rotated)?;
        // Chain continuity: last_hash is already held in memory
        Ok(())
    }
}

/// Escape a value for CSV output.
fn csv_escape(s: &str) -> String {
    if s.contains(',') || s.contains('"') || s.contains('\n') {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

/// Escape a value for CEF output (pipe and backslash).
fn cef_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('|', "\\|")
}

/// Escape a value for CEF extension fields.
///
/// Extension values use `=` as key-value delimiter and newlines as record
/// separators. Both must be escaped to prevent injection/parsing issues.
fn cef_escape_ext(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('=', "\\=")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn entry_hash_chain_integrity() {
        let e1 = AuditEntry::new(
            "",
            "zeroclaw_agent".to_string(),
            "shell_exec".to_string(),
            Some("shell".to_string()),
            None,
            "ok".to_string(),
            vec![],
        );
        assert!(e1.verify());
        assert_eq!(e1.prev_hash, "");

        let e2 = AuditEntry::new(
            &e1.hash,
            "zeroclaw_agent".to_string(),
            "file_read".to_string(),
            Some("file_read".to_string()),
            None,
            "ok".to_string(),
            vec![ComplianceFramework::Gdpr],
        );
        assert!(e2.verify());
        assert_eq!(e2.prev_hash, e1.hash);
    }

    #[test]
    fn entry_detects_tamper() {
        let mut entry = AuditEntry::new(
            "",
            "zeroclaw_agent".to_string(),
            "shell_exec".to_string(),
            None,
            None,
            "ok".to_string(),
            vec![],
        );
        assert!(entry.verify());

        // Tamper with the action
        entry.action = "malicious_action".to_string();
        assert!(!entry.verify());
    }

    #[test]
    fn tamper_evident_log_append_and_verify() -> Result<()> {
        let tmp = TempDir::new()?;
        let log_path = tmp.path().join("audit.jsonl");
        let log = TamperEvidentLog::new(log_path, 0)?;

        log.append(
            "zeroclaw_agent".into(),
            "action_a".into(),
            None,
            None,
            "ok".into(),
            vec![],
        )?;
        log.append(
            "zeroclaw_agent".into(),
            "action_b".into(),
            Some("shell".into()),
            None,
            "ok".into(),
            vec![ComplianceFramework::Finma],
        )?;

        let count = log.verify_chain()?;
        assert_eq!(count, 2);
        Ok(())
    }

    #[test]
    fn tamper_evident_log_detects_chain_break() -> Result<()> {
        let tmp = TempDir::new()?;
        let log_path = tmp.path().join("audit.jsonl");
        let log = TamperEvidentLog::new(log_path.clone(), 0)?;

        log.append(
            "zeroclaw_agent".into(),
            "action_a".into(),
            None,
            None,
            "ok".into(),
            vec![],
        )?;
        log.append(
            "zeroclaw_agent".into(),
            "action_b".into(),
            None,
            None,
            "ok".into(),
            vec![],
        )?;

        // Tamper: rewrite first entry hash in the file
        let content = std::fs::read_to_string(&log_path)?;
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 2);

        let mut e1: AuditEntry = serde_json::from_str(lines[0])?;
        e1.hash = "tampered_hash".to_string();
        let tampered = format!("{}\n{}\n", serde_json::to_string(&e1)?, lines[1]);
        std::fs::write(&log_path, tampered)?;

        let result = TamperEvidentLog::verify_chain_at(&log_path);
        assert!(result.is_err());
        Ok(())
    }

    #[test]
    fn tamper_evident_log_resumes_chain_after_reopen() -> Result<()> {
        let tmp = TempDir::new()?;
        let log_path = tmp.path().join("audit.jsonl");

        // First session
        {
            let log = TamperEvidentLog::new(log_path.clone(), 0)?;
            log.append(
                "zeroclaw_agent".into(),
                "action_a".into(),
                None,
                None,
                "ok".into(),
                vec![],
            )?;
        }

        // Second session — should resume chain
        {
            let log = TamperEvidentLog::new(log_path.clone(), 0)?;
            log.append(
                "zeroclaw_agent".into(),
                "action_b".into(),
                None,
                None,
                "ok".into(),
                vec![],
            )?;
        }

        let count = TamperEvidentLog::verify_chain_at(&log_path)?;
        assert_eq!(count, 2);
        Ok(())
    }

    #[test]
    fn export_json_format() -> Result<()> {
        let tmp = TempDir::new()?;
        let log_path = tmp.path().join("audit.jsonl");
        let log = TamperEvidentLog::new(log_path, 0)?;
        log.append(
            "zeroclaw_agent".into(),
            "test".into(),
            None,
            None,
            "ok".into(),
            vec![],
        )?;

        let json = log.export(ExportFormat::Json)?;
        let parsed: Vec<AuditEntry> = serde_json::from_str(&json)?;
        assert_eq!(parsed.len(), 1);
        Ok(())
    }

    #[test]
    fn export_csv_format() -> Result<()> {
        let tmp = TempDir::new()?;
        let log_path = tmp.path().join("audit.jsonl");
        let log = TamperEvidentLog::new(log_path, 0)?;
        log.append(
            "zeroclaw_agent".into(),
            "test".into(),
            None,
            None,
            "ok".into(),
            vec![],
        )?;

        let csv = log.export(ExportFormat::Csv)?;
        assert!(csv.starts_with("timestamp,"));
        let lines: Vec<&str> = csv.lines().collect();
        assert_eq!(lines.len(), 2); // header + 1 entry
        Ok(())
    }

    #[test]
    fn export_cef_format() -> Result<()> {
        let tmp = TempDir::new()?;
        let log_path = tmp.path().join("audit.jsonl");
        let log = TamperEvidentLog::new(log_path, 0)?;
        log.append(
            "zeroclaw_agent".into(),
            "test".into(),
            None,
            None,
            "ok".into(),
            vec![],
        )?;

        let cef = log.export(ExportFormat::Cef)?;
        assert!(cef.starts_with("CEF:0|ZeroClaw|"));
        Ok(())
    }
}
