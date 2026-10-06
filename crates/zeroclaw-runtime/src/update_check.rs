//! The daemon's release check, behind `system/version-check` and the
//! dashboard's version badge.
//!
//! The check runs this process's own executable as
//! `zeroclaw update --check --json`, keeping a single source of truth for
//! update logic. The daemon runs it rather than a gateway: a gateway in its
//! own process would run its own executable, not the daemon's. A successful
//! check of the latest release is reused for an hour to stay well under
//! GitHub's unauthenticated rate limit; a forced check, or one for a
//! specific release, always runs.

use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use anyhow::Context;
use serde::Deserialize;
use zeroclaw_rpc_proto::types::VersionCheckResponse;

/// How long a successful latest-release check is reused before re-querying.
const CHECK_CACHE_TTL: Duration = Duration::from_secs(3600);
/// Upper bound on the `zeroclaw update --check` subprocess.
const CHECK_TIMEOUT: Duration = Duration::from_secs(15);

/// Parsed output of `zeroclaw update --check --json`. Field names must match
/// the JSON emitted in `src/main.rs`.
#[derive(Debug, Clone, Deserialize)]
struct CliCheck {
    current_version: String,
    latest_version: String,
    is_newer: bool,
    release_url: Option<String>,
    release_notes: Option<String>,
    published_at: Option<String>,
}

impl CliCheck {
    fn response(&self) -> VersionCheckResponse {
        VersionCheckResponse {
            current_version: self.current_version.clone(),
            latest_version: Some(self.latest_version.clone()),
            is_newer: self.is_newer,
            release_url: self.release_url.clone(),
            release_notes: self.release_notes.clone(),
            published_at: self.published_at.clone(),
            error: None,
        }
    }
}

/// The last successful latest-release check. A check for a specific
/// release is never cached.
static CHECK_CACHE: Mutex<Option<(Instant, VersionCheckResponse)>> = Mutex::new(None);

fn lock_recover<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Whether a newer release than this daemon's exists: the latest release,
/// or the release tagged `version`. `force` skips the cache.
///
/// Never fails: a check that cannot complete answers with `error` set and
/// no `latest_version`, so a caller can always show the running version.
pub async fn check(force: bool, version: Option<&str>) -> VersionCheckResponse {
    let use_cache = !force && version.is_none();
    if use_cache {
        let cached = lock_recover(&CHECK_CACHE)
            .as_ref()
            .filter(|(checked_at, _)| checked_at.elapsed() < CHECK_CACHE_TTL)
            .map(|(_, cached)| cached.clone());
        if let Some(cached) = cached {
            return cached;
        }
    }

    match run_cli_check(version).await {
        Ok(info) => {
            let response = info.response();
            if use_cache {
                *lock_recover(&CHECK_CACHE) = Some((Instant::now(), response.clone()));
            }
            response
        }
        Err(error) => VersionCheckResponse {
            current_version: env!("CARGO_PKG_VERSION").to_string(),
            latest_version: None,
            is_newer: false,
            release_url: None,
            release_notes: None,
            published_at: None,
            error: Some(error.to_string()),
        },
    }
}

async fn run_cli_check(version: Option<&str>) -> anyhow::Result<CliCheck> {
    let exe = std::env::current_exe().context("cannot determine current executable path")?;
    let mut cmd = tokio::process::Command::new(exe);
    cmd.arg("update").arg("--check").arg("--json");
    if let Some(v) = version {
        cmd.arg("--version").arg(v);
    }
    // A check that times out is not left running behind the daemon.
    cmd.stdin(std::process::Stdio::null()).kill_on_drop(true);

    let output = tokio::time::timeout(CHECK_TIMEOUT, cmd.output())
        .await
        .context("version check timed out")?
        .context("failed to spawn `zeroclaw update --check`")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("update --check failed: {}", stderr.trim());
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    serde_json::from_str::<CliCheck>(stdout.trim())
        .context("failed to parse `update --check --json` output")
}

/// Record `response` as a fresh latest-release check, as a successful
/// unforced [`check`] would, so tests can read the cache without running
/// the subprocess.
#[cfg(any(test, feature = "test-util"))]
pub fn remember_latest_check(response: VersionCheckResponse) {
    *lock_recover(&CHECK_CACHE) = Some((Instant::now(), response));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_check_json_roundtrips() {
        let json = r#"{
            "current_version": "0.7.3",
            "latest_version": "0.7.4",
            "is_newer": true,
            "release_url": "https://example.com/r",
            "release_notes": "- fix things",
            "published_at": "2026-06-20T00:00:00Z"
        }"#;
        let parsed: CliCheck = serde_json::from_str(json).unwrap();
        assert!(parsed.is_newer);
        assert_eq!(parsed.latest_version, "0.7.4");
        let resp = parsed.response();
        assert_eq!(resp.latest_version.as_deref(), Some("0.7.4"));
        assert!(resp.is_newer);
        let out = serde_json::to_value(&resp).unwrap();
        assert_eq!(out["latest_version"], "0.7.4");
        assert_eq!(out["is_newer"], true);
        // `error` is skipped on the success path, never serialized as null.
        assert!(out.get("error").is_none());
    }

    #[test]
    fn a_failed_check_reports_the_running_version_and_the_error() {
        // A specific release is never served from the cache, so this runs
        // the subprocess: here the test binary, which refuses the
        // arguments.
        let checked = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(check(false, Some("v0.0.0-unreleased")));
        assert_eq!(checked.current_version, env!("CARGO_PKG_VERSION"));
        assert_eq!(checked.latest_version, None);
        assert!(!checked.is_newer);
        assert!(checked.error.is_some(), "{checked:?}");
        let out = serde_json::to_value(&checked).unwrap();
        assert!(out["latest_version"].is_null(), "{out}");
        assert!(out.get("release_url").is_none(), "{out}");
    }
}
