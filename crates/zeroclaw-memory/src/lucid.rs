#[cfg(test)]
use super::enriched::EnrichmentPolicy;
use super::enriched::{
    CleanupSupport, EnrichedMemory, EnricherCapabilities, EnrichmentCleanupRequest,
    EnrichmentRecallRequest, EnrichmentStoreRequest, MemoryEnricher, RecallScope, RecallSupport,
    ResultKind,
};
#[cfg(test)]
use super::sqlite::SqliteMemory;
use super::traits::{MemoryCategory, MemoryEntry};
use async_trait::async_trait;
use chrono::Local;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::process::Command;
use tokio::time::timeout;
use zeroclaw_api::attribution::MemoryKind;
use zeroclaw_config::schema::LucidEnrichmentConfig;

/// External-process connector for the `lucid-memory` command-line tool.
pub struct LucidConnector {
    lucid_cmd: String,
    token_budget: usize,
    workspace_dir: PathBuf,
    recall_timeout: Duration,
    store_timeout: Duration,
}

/// SQLite-authoritative memory enriched by Lucid process recall.
pub type LucidEnrichedMemory = EnrichedMemory<LucidConnector>;

impl LucidConnector {
    const DEFAULT_LUCID_CMD: &'static str = "lucid";
    const DEFAULT_TOKEN_BUDGET: usize = 200;
    const DEFAULT_RECALL_TIMEOUT_MS: u64 = 500;
    const DEFAULT_STORE_TIMEOUT_MS: u64 = 800;

    pub(crate) fn new(workspace_dir: &Path) -> Self {
        Self {
            lucid_cmd: Self::DEFAULT_LUCID_CMD.to_string(),
            token_budget: Self::DEFAULT_TOKEN_BUDGET,
            workspace_dir: workspace_dir.to_path_buf(),
            recall_timeout: Duration::from_millis(Self::DEFAULT_RECALL_TIMEOUT_MS),
            store_timeout: Duration::from_millis(Self::DEFAULT_STORE_TIMEOUT_MS),
        }
    }

    pub(crate) fn from_config(
        workspace_dir: &Path,
        config: &LucidEnrichmentConfig,
    ) -> anyhow::Result<Self> {
        // Defense in depth behind `Config::validate_lucid_enrichment_deadlines`;
        // both layers cite the shared LUCID_DEADLINE_RULE wording.
        fn configured_timeout(
            value: Option<u64>,
            default_ms: u64,
            field: &str,
        ) -> anyhow::Result<Duration> {
            let millis = value.unwrap_or(default_ms);
            if millis == 0 {
                anyhow::bail!(
                    "memory_enrichment.lucid.{field} {}",
                    zeroclaw_config::schema::LUCID_DEADLINE_RULE
                );
            }
            Ok(Duration::from_millis(millis))
        }

        let lucid_cmd = config
            .binary_path
            .as_ref()
            .filter(|path| !path.trim().is_empty())
            .cloned()
            .unwrap_or_else(|| Self::DEFAULT_LUCID_CMD.to_string());
        Ok(Self {
            lucid_cmd,
            token_budget: Self::DEFAULT_TOKEN_BUDGET,
            workspace_dir: workspace_dir.to_path_buf(),
            recall_timeout: configured_timeout(
                config.recall_timeout_ms,
                Self::DEFAULT_RECALL_TIMEOUT_MS,
                "recall_timeout_ms",
            )?,
            store_timeout: configured_timeout(
                config.store_timeout_ms,
                Self::DEFAULT_STORE_TIMEOUT_MS,
                "store_timeout_ms",
            )?,
        })
    }

    #[cfg(test)]
    fn with_options(
        workspace_dir: &Path,
        lucid_cmd: String,
        token_budget: usize,
        recall_timeout: Duration,
        store_timeout: Duration,
    ) -> Self {
        Self {
            lucid_cmd,
            token_budget,
            workspace_dir: workspace_dir.to_path_buf(),
            recall_timeout,
            store_timeout,
        }
    }

    fn to_lucid_type(category: &MemoryCategory) -> &'static str {
        match category {
            MemoryCategory::Core => "decision",
            MemoryCategory::Daily => "context",
            MemoryCategory::Conversation => "conversation",
            MemoryCategory::Custom(_) => "learning",
        }
    }

    fn to_memory_category(label: &str) -> MemoryCategory {
        let normalized = label.to_lowercase();
        if normalized.contains("visual") {
            return MemoryCategory::Custom("visual".to_string());
        }

        match normalized.as_str() {
            "decision" | "learning" | "solution" => MemoryCategory::Core,
            "context" | "conversation" => MemoryCategory::Conversation,
            "bug" => MemoryCategory::Daily,
            other => MemoryCategory::Custom(other.to_string()),
        }
    }

    fn parse_lucid_context(raw: &str) -> Vec<MemoryEntry> {
        let mut in_context_block = false;
        let mut entries = Vec::new();
        let now = Local::now().to_rfc3339();

        for line in raw.lines().map(str::trim) {
            if line == "<lucid-context>" {
                in_context_block = true;
                continue;
            }

            if line == "</lucid-context>" {
                break;
            }

            if !in_context_block || line.is_empty() {
                continue;
            }

            let Some(rest) = line.strip_prefix("- [") else {
                continue;
            };
            let Some((label, content_part)) = rest.split_once(']') else {
                continue;
            };
            let content = content_part.trim();
            if content.is_empty() {
                continue;
            }

            let rank = entries.len();
            entries.push(MemoryEntry {
                id: format!("lucid:{rank}"),
                key: format!("lucid_{rank}"),
                content: content.to_string(),
                category: Self::to_memory_category(label.trim()),
                timestamp: now.clone(),
                session_id: None,
                score: Some((1.0 - rank as f64 * 0.05).max(0.1)),
                namespace: "default".into(),
                importance: None,
                superseded_by: None,
                kind: None,
                pinned: false,
                tenant_id: None,
                agent_alias: None,
                agent_id: None,
            });
        }

        entries
    }

    async fn run_command(
        &self,
        args: &[String],
        timeout_window: Duration,
    ) -> anyhow::Result<String> {
        let mut command = Command::new(&self.lucid_cmd);
        command.args(args).kill_on_drop(true);

        let output = timeout(timeout_window, command.output())
            .await
            .map_err(|_| {
                ::zeroclaw_log::record!(
                    ERROR,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Timeout)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({
                            "command": self.lucid_cmd,
                            "timeout_ms": timeout_window.as_millis() as u64
                        })),
                    "lucid command timed out"
                );
                anyhow::Error::msg(format!(
                    "lucid command timed out after {}ms",
                    timeout_window.as_millis()
                ))
            })??;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("lucid command failed: {stderr}");
        }

        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }

    fn build_store_args(&self, key: &str, content: &str, category: &MemoryCategory) -> Vec<String> {
        vec![
            "store".to_string(),
            format!("{key}: {content}"),
            format!("--type={}", Self::to_lucid_type(category)),
            format!("--project={}", self.workspace_dir.display()),
        ]
    }

    fn build_recall_args(&self, query: &str) -> Vec<String> {
        vec![
            "context".to_string(),
            query.to_string(),
            format!("--budget={}", self.token_budget),
            format!("--project={}", self.workspace_dir.display()),
        ]
    }
}

#[cfg(test)]
impl EnrichedMemory<LucidConnector> {
    #[allow(clippy::too_many_arguments)]
    fn with_options(
        alias: &str,
        workspace_dir: &Path,
        local: SqliteMemory,
        lucid_cmd: String,
        token_budget: usize,
        local_hit_threshold: usize,
        recall_timeout: Duration,
        store_timeout: Duration,
        failure_cooldown: Duration,
    ) -> Self {
        Self::from_parts(
            alias,
            local,
            LucidConnector::with_options(
                workspace_dir,
                lucid_cmd,
                token_budget,
                recall_timeout,
                store_timeout,
            ),
            EnrichmentPolicy::new(local_hit_threshold, failure_cooldown),
        )
    }
}

#[async_trait]
impl MemoryEnricher for LucidConnector {
    fn name(&self) -> &'static str {
        "lucid"
    }

    fn attribution_kind(&self) -> MemoryKind {
        MemoryKind::Lucid
    }

    fn capabilities(&self) -> EnricherCapabilities {
        EnricherCapabilities {
            result_kind: ResultKind::DerivedContext,
            recall_scope: RecallScope::UnscopedOnly,
            recall_support: RecallSupport::SemanticAndRecent,
            cleanup_support: CleanupSupport::None,
        }
    }

    async fn store(&self, request: EnrichmentStoreRequest<'_>) -> anyhow::Result<()> {
        let args = self.build_store_args(request.key, request.content, request.category);
        self.run_command(&args, self.store_timeout).await?;
        Ok(())
    }

    async fn recall(
        &self,
        request: EnrichmentRecallRequest<'_>,
    ) -> anyhow::Result<Vec<MemoryEntry>> {
        let args = self.build_recall_args(request.query);
        let output = self.run_command(&args, self.recall_timeout).await?;
        Ok(Self::parse_lucid_context(&output))
    }

    async fn cleanup(&self, _request: EnrichmentCleanupRequest<'_>) -> anyhow::Result<()> {
        anyhow::bail!("Lucid does not support agent-scoped cleanup")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::traits::Memory;
    use tempfile::TempDir;

    fn test_memory(workspace: &Path, command: String) -> LucidEnrichedMemory {
        let sqlite = SqliteMemory::new("sqlite", workspace).unwrap();
        LucidEnrichedMemory::with_options(
            "test",
            workspace,
            sqlite,
            command,
            200,
            3,
            Duration::from_secs(5),
            Duration::from_secs(5),
            Duration::from_secs(2),
        )
    }

    // ── Platform-independent tests (no fake-lucid shell script) ─────────
    // These run everywhere, including Windows CI. Executing a real script
    // is unix-only; those tests live in `process` below.

    #[test]
    fn refresh_embedder_forwards_to_local_sqlite() {
        let tmp = TempDir::new().unwrap();
        let local = SqliteMemory::new("test", tmp.path()).unwrap();
        let lucid = crate::build_lucid_enriched_memory(tmp.path(), local, None).unwrap();
        assert_eq!(lucid.embedder_dimensions(), 0);

        Memory::refresh_embedder(
            &lucid,
            "openai",
            Some("sk-test"),
            "text-embedding-3-small",
            1536,
        );

        assert_eq!(lucid.embedder_dimensions(), 1536);
    }

    #[test]
    fn connector_consumes_typed_binary_and_deadlines() {
        let tmp = TempDir::new().unwrap();
        let config = LucidEnrichmentConfig {
            binary_path: Some("/opt/lucid/bin/lucid".to_string()),
            recall_timeout_ms: Some(2500),
            store_timeout_ms: Some(3000),
        };

        let connector = LucidConnector::from_config(tmp.path(), &config).unwrap();
        assert_eq!(connector.lucid_cmd, "/opt/lucid/bin/lucid");
        assert_eq!(connector.recall_timeout, Duration::from_millis(2500));
        assert_eq!(connector.store_timeout, Duration::from_millis(3000));
    }

    #[test]
    fn connector_rejects_zero_deadlines() {
        let tmp = TempDir::new().unwrap();
        for config in [
            LucidEnrichmentConfig {
                recall_timeout_ms: Some(0),
                ..LucidEnrichmentConfig::default()
            },
            LucidEnrichmentConfig {
                store_timeout_ms: Some(0),
                ..LucidEnrichmentConfig::default()
            },
        ] {
            let error = LucidConnector::from_config(tmp.path(), &config)
                .err()
                .expect("zero deadline must fail");
            assert!(
                error
                    .to_string()
                    .contains(zeroclaw_config::schema::LUCID_DEADLINE_RULE)
            );
        }
    }

    #[tokio::test]
    async fn lucid_name() {
        let tmp = TempDir::new().unwrap();
        let memory = test_memory(tmp.path(), "nonexistent-lucid-binary".to_string());
        assert_eq!(memory.name(), "lucid");
    }

    #[tokio::test]
    async fn store_succeeds_when_lucid_missing() {
        let tmp = TempDir::new().unwrap();
        let memory = test_memory(tmp.path(), "nonexistent-lucid-binary".to_string());

        memory
            .store("lang", "User prefers Rust", MemoryCategory::Core, None)
            .await
            .unwrap();

        let entry = memory.get("lang").await.unwrap();
        assert_eq!(entry.unwrap().content, "User prefers Rust");
    }

    // ── Tests that execute a real fake-lucid shell script (unix-only) ───

    #[cfg(unix)]
    mod process {
        use super::*;
        use crate::test_support::write_lucid_script;

        const STORE_OK: &str = r#"  echo '{"success":true,"id":"mem_store"}'
  exit 0"#;

        fn write_fake_lucid_script(dir: &Path) -> String {
            write_lucid_script(
                dir,
                "fake-lucid.sh",
                "",
                STORE_OK,
                r#"  cat <<'EOF'
<lucid-context>
Auth context snapshot
- [decision] Use token refresh middleware
- [context] Working in src/auth.rs
</lucid-context>
EOF
  exit 0"#,
            )
        }

        fn write_delayed_lucid_script(dir: &Path) -> String {
            write_lucid_script(
                dir,
                "delayed-lucid.sh",
                "",
                STORE_OK,
                r#"  sleep 0.2
  cat <<'EOF'
<lucid-context>
- [decision] Delayed token refresh guidance
</lucid-context>
EOF
  exit 0"#,
            )
        }

        fn write_probe_lucid_script(dir: &Path, marker_path: &Path) -> String {
            write_lucid_script(
                dir,
                "probe-lucid.sh",
                "",
                STORE_OK,
                &format!(
                    r#"  printf 'context\n' >> "{marker}"
  cat <<'EOF'
<lucid-context>
- [decision] should not be used when local hits are enough
</lucid-context>
EOF
  exit 0"#,
                    marker = marker_path.display()
                ),
            )
        }

        fn write_failing_lucid_script(dir: &Path, marker_path: &Path) -> String {
            write_lucid_script(
                dir,
                "failing-lucid.sh",
                "",
                STORE_OK,
                &format!(
                    r#"  printf 'context\n' >> "{marker}"
  echo "simulated lucid failure" >&2
  exit 1"#,
                    marker = marker_path.display()
                ),
            )
        }

        #[tokio::test]
        async fn recall_merges_lucid_and_local_results() {
            let tmp = TempDir::new().unwrap();
            let memory = test_memory(tmp.path(), write_fake_lucid_script(tmp.path()));

            memory
                .store(
                    "local_note",
                    "Local sqlite auth fallback note",
                    MemoryCategory::Core,
                    None,
                )
                .await
                .unwrap();

            let entries = memory.recall("auth", 5, None, None, None).await.unwrap();
            assert!(
                entries
                    .iter()
                    .any(|entry| entry.content.contains("Local sqlite auth fallback note"))
            );
            assert!(
                entries
                    .iter()
                    .any(|entry| entry.content.contains("token refresh"))
            );
        }

        #[tokio::test]
        async fn session_scoped_recall_keeps_lucid_derived_context() {
            let tmp = TempDir::new().unwrap();
            let memory = test_memory(tmp.path(), write_fake_lucid_script(tmp.path()));

            let entries = memory
                .recall("auth", 5, Some("session-a"), None, None)
                .await
                .unwrap();

            assert!(
                entries
                    .iter()
                    .any(|entry| entry.content.contains("token refresh"))
            );
        }

        #[tokio::test]
        async fn recent_recall_invokes_lucid_when_local_results_are_insufficient() {
            let tmp = TempDir::new().unwrap();
            let memory = test_memory(tmp.path(), write_fake_lucid_script(tmp.path()));

            let entries = memory.recall("*", 5, None, None, None).await.unwrap();

            assert!(
                entries
                    .iter()
                    .any(|entry| entry.content.contains("token refresh"))
            );
        }

        #[tokio::test]
        async fn recall_handles_lucid_cold_start_delay_within_timeout() {
            let tmp = TempDir::new().unwrap();
            let memory = test_memory(tmp.path(), write_delayed_lucid_script(tmp.path()));

            memory
                .store(
                    "local_note",
                    "Local sqlite auth fallback note",
                    MemoryCategory::Core,
                    None,
                )
                .await
                .unwrap();

            let entries = memory.recall("auth", 5, None, None, None).await.unwrap();
            assert!(
                entries
                    .iter()
                    .any(|entry| entry.content.contains("Delayed token refresh guidance"))
            );
        }

        #[tokio::test]
        async fn recall_skips_lucid_when_local_hits_are_enough() {
            let tmp = TempDir::new().unwrap();
            let marker = tmp.path().join("context_calls.log");
            let sqlite = SqliteMemory::new("test", tmp.path()).unwrap();
            let memory = LucidEnrichedMemory::with_options(
                "test",
                tmp.path(),
                sqlite,
                write_probe_lucid_script(tmp.path(), &marker),
                200,
                1,
                Duration::from_secs(5),
                Duration::from_secs(5),
                Duration::from_secs(2),
            );

            memory
                .store(
                    "pref",
                    "Rust should stay local-first",
                    MemoryCategory::Core,
                    None,
                )
                .await
                .unwrap();

            let entries = memory.recall("rust", 5, None, None, None).await.unwrap();
            assert!(
                entries
                    .iter()
                    .any(|entry| entry.content.contains("Rust should stay local-first"))
            );
            let context_calls = tokio::fs::read_to_string(&marker).await.unwrap_or_default();
            assert!(context_calls.trim().is_empty());
        }

        #[tokio::test]
        async fn agent_scoped_recall_does_not_query_unscoped_lucid() {
            let tmp = TempDir::new().unwrap();
            let marker = tmp.path().join("scoped_context_calls.log");
            let sqlite = SqliteMemory::new("test", tmp.path()).unwrap();
            let memory = LucidEnrichedMemory::with_options(
                "test",
                tmp.path(),
                sqlite,
                write_probe_lucid_script(tmp.path(), &marker),
                200,
                99,
                Duration::from_secs(5),
                Duration::from_secs(5),
                Duration::from_secs(2),
            );
            let agent_id = memory.ensure_agent_uuid("agent-a").await.unwrap();

            let entries = memory
                .recall_for_agents(&[agent_id.as_str()], "missing", 5, None, None, None)
                .await
                .unwrap();

            assert!(entries.is_empty());
            let context_calls = tokio::fs::read_to_string(&marker).await.unwrap_or_default();
            assert!(context_calls.trim().is_empty());
        }

        #[tokio::test]
        async fn failure_cooldown_avoids_repeated_lucid_calls() {
            let tmp = TempDir::new().unwrap();
            let marker = tmp.path().join("failing_context_calls.log");
            let sqlite = SqliteMemory::new("test", tmp.path()).unwrap();
            let memory = LucidEnrichedMemory::with_options(
                "test",
                tmp.path(),
                sqlite,
                write_failing_lucid_script(tmp.path(), &marker),
                200,
                99,
                Duration::from_secs(5),
                Duration::from_secs(5),
                Duration::from_secs(5),
            );

            assert!(
                memory
                    .recall("auth", 5, None, None, None)
                    .await
                    .unwrap()
                    .is_empty()
            );
            assert!(
                memory
                    .recall("auth", 5, None, None, None)
                    .await
                    .unwrap()
                    .is_empty()
            );

            let calls = tokio::fs::read_to_string(&marker).await.unwrap_or_default();
            assert_eq!(calls.lines().count(), 1);
        }
    }
}
