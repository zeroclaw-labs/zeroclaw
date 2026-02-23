use anyhow::{Context, Result};
use parking_lot::Mutex;
use rusqlite::{params, Connection};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

const DOMAIN_APPROVAL_DB_PATH: &str = "state/domain_policy.db";
pub const DOMAIN_APPROVAL_REQUIRED_PREFIX: &str = "DOMAIN_APPROVAL_REQUIRED:";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DomainListKind {
    Allow,
    Deny,
}

impl DomainListKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Allow => "allow",
            Self::Deny => "deny",
        }
    }
}

#[derive(Debug)]
pub struct DomainPolicyStore {
    conn: Mutex<Connection>,
}

static DOMAIN_POLICY_STORE: OnceLock<Arc<DomainPolicyStore>> = OnceLock::new();

impl DomainPolicyStore {
    pub fn open(workspace_dir: &Path) -> Result<Arc<Self>> {
        let db_path = workspace_dir.join(DOMAIN_APPROVAL_DB_PATH);
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create {}", parent.display()))?;
        }

        let conn = Connection::open(&db_path)
            .with_context(|| format!("Failed to open {}", db_path.display()))?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA synchronous=NORMAL;
             CREATE TABLE IF NOT EXISTS domain_policy (
               domain TEXT NOT NULL,
               list_type TEXT NOT NULL CHECK(list_type IN ('allow','deny')),
               source TEXT NOT NULL,
               updated_at INTEGER NOT NULL DEFAULT (strftime('%s','now')),
               PRIMARY KEY (domain, list_type)
             );",
        )?;

        Ok(Arc::new(Self {
            conn: Mutex::new(conn),
        }))
    }

    pub fn seed_allowlist(&self, domains: &[String], source: &str) -> Result<()> {
        for domain in domains {
            if let Some(normalized) = normalize_domain_pattern(domain) {
                self.insert(&normalized, DomainListKind::Allow, source)?;
            }
        }
        Ok(())
    }

    pub fn insert(&self, domain: &str, kind: DomainListKind, source: &str) -> Result<()> {
        let normalized = normalize_domain_pattern(domain)
            .ok_or_else(|| anyhow::anyhow!("Invalid domain pattern: {domain}"))?;

        let conn = self.conn.lock();
        conn.execute(
            "INSERT INTO domain_policy(domain, list_type, source, updated_at)
             VALUES(?1, ?2, ?3, strftime('%s','now'))
             ON CONFLICT(domain, list_type)
             DO UPDATE SET source=excluded.source, updated_at=excluded.updated_at",
            params![normalized, kind.as_str(), source],
        )?;
        Ok(())
    }

    pub fn is_denied(&self, host: &str) -> bool {
        self.matches_list(host, DomainListKind::Deny)
    }

    pub fn is_allowed(&self, host: &str) -> bool {
        self.matches_list(host, DomainListKind::Allow)
    }

    fn matches_list(&self, host: &str, kind: DomainListKind) -> bool {
        let Some(host) = normalize_host(host) else {
            return false;
        };

        let conn = self.conn.lock();
        let mut stmt = match conn.prepare("SELECT domain FROM domain_policy WHERE list_type = ?1") {
            Ok(stmt) => stmt,
            Err(_) => return false,
        };

        let rows = match stmt.query_map(params![kind.as_str()], |row| row.get::<_, String>(0)) {
            Ok(rows) => rows,
            Err(_) => return false,
        };

        for pattern in rows.flatten() {
            if host_matches_pattern(&host, &pattern) {
                return true;
            }
        }

        false
    }

    pub fn db_path(workspace_dir: &Path) -> PathBuf {
        workspace_dir.join(DOMAIN_APPROVAL_DB_PATH)
    }
}

pub fn init_runtime_domain_policy(
    workspace_dir: &Path,
    seed_allowlist: &[String],
) -> Result<Arc<DomainPolicyStore>> {
    let store = DomainPolicyStore::open(workspace_dir)?;
    store.seed_allowlist(seed_allowlist, "config")?;
    let _ = DOMAIN_POLICY_STORE.set(Arc::clone(&store));
    Ok(store)
}

pub fn runtime_domain_policy() -> Option<Arc<DomainPolicyStore>> {
    DOMAIN_POLICY_STORE.get().cloned()
}

pub fn extract_domain_approval_host(message: &str) -> Option<String> {
    message
        .strip_prefix(DOMAIN_APPROVAL_REQUIRED_PREFIX)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToString::to_string)
}

pub fn normalize_domain_pattern(raw: &str) -> Option<String> {
    let mut value = raw.trim().to_ascii_lowercase();
    if value.is_empty() {
        return None;
    }

    if value == "*" {
        return Some(value);
    }

    if let Some(rest) = value.strip_prefix("https://") {
        value = rest.to_string();
    } else if let Some(rest) = value.strip_prefix("http://") {
        value = rest.to_string();
    }

    value = value
        .split(['/', '?', '#'])
        .next()
        .unwrap_or_default()
        .to_string();

    if let Some((_, host)) = value.rsplit_once('@') {
        value = host.to_string();
    }

    if let Some((host, _)) = value.split_once(':') {
        value = host.to_string();
    }

    value = value
        .trim_start_matches('.')
        .trim_end_matches('.')
        .to_string();

    if value.is_empty() || value.chars().any(char::is_whitespace) {
        return None;
    }

    Some(value)
}

fn normalize_host(raw: &str) -> Option<String> {
    normalize_domain_pattern(raw).filter(|v| !v.contains('*'))
}

fn host_matches_pattern(host: &str, pattern: &str) -> bool {
    if pattern == "*" {
        return true;
    }

    if !pattern.contains('*') {
        return host == pattern || host.ends_with(&format!(".{pattern}"));
    }

    wildcard_match(pattern.as_bytes(), host.as_bytes())
}

fn wildcard_match(pattern: &[u8], value: &[u8]) -> bool {
    let mut p = 0usize;
    let mut v = 0usize;
    let mut star_idx: Option<usize> = None;
    let mut match_idx = 0usize;

    while v < value.len() {
        if p < pattern.len() && pattern[p] == value[v] {
            p += 1;
            v += 1;
            continue;
        }

        if p < pattern.len() && pattern[p] == b'*' {
            star_idx = Some(p);
            p += 1;
            match_idx = v;
            continue;
        }

        if let Some(star) = star_idx {
            p = star + 1;
            match_idx += 1;
            v = match_idx;
            continue;
        }

        return false;
    }

    while p < pattern.len() && pattern[p] == b'*' {
        p += 1;
    }

    p == pattern.len()
}
