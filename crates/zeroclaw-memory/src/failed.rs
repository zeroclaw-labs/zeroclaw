use super::traits::{Memory, MemoryCategory, MemoryEntry, MemoryStats, ProceduralMessage};
use async_trait::async_trait;

/// Memory backend placeholder for a configured backend that failed to build.
///
/// The gateway installs this when `create_memory_from_config` rejects the
/// configured backend (unknown kind, unreachable storage, missing feature).
/// It must be distinguishable from [`super::none::NoneMemory`]: a no-op that
/// answers `Ok` would acknowledge writes that were never persisted, but the
/// gateway still has to boot so the config editor and repair endpoints work.
/// Every data operation therefore fails with the construction error so the
/// operator sees the broken config at the first memory use, not in a diff of
/// data that silently vanished.
#[derive(Debug, Clone)]
pub struct FailedMemory {
    alias: String,
    cause: String,
}

impl FailedMemory {
    pub fn new(alias: &str, cause: &anyhow::Error) -> Self {
        Self {
            alias: alias.to_string(),
            // `{:#}` keeps the whole context chain, so the root cause (why
            // SQLite could not open, which alias was missing) survives.
            cause: format!("{cause:#}"),
        }
    }

    fn err(&self) -> anyhow::Error {
        anyhow::Error::msg(format!(
            "memory backend {:?} failed to construct: {}; fix [memory] and reload",
            self.alias, self.cause
        ))
    }
}

impl ::zeroclaw_api::attribution::Attributable for FailedMemory {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        ::zeroclaw_api::attribution::Role::Memory(::zeroclaw_api::attribution::MemoryKind::None)
    }
    fn alias(&self) -> &str {
        &self.alias
    }
}

#[async_trait]
impl Memory for FailedMemory {
    fn name(&self) -> &str {
        "failed"
    }

    async fn store(
        &self,
        _key: &str,
        _content: &str,
        _category: MemoryCategory,
        _session_id: Option<&str>,
    ) -> anyhow::Result<()> {
        Err(self.err())
    }

    async fn recall(
        &self,
        _query: &str,
        _limit: usize,
        _session_id: Option<&str>,
        _since: Option<&str>,
        _until: Option<&str>,
    ) -> anyhow::Result<Vec<MemoryEntry>> {
        Err(self.err())
    }

    async fn get(&self, _key: &str) -> anyhow::Result<Option<MemoryEntry>> {
        Err(self.err())
    }

    async fn list(
        &self,
        _category: Option<&MemoryCategory>,
        _session_id: Option<&str>,
    ) -> anyhow::Result<Vec<MemoryEntry>> {
        Err(self.err())
    }

    async fn forget(&self, _key: &str) -> anyhow::Result<bool> {
        Err(self.err())
    }

    async fn forget_for_agent(&self, _key: &str, _agent_id: &str) -> anyhow::Result<bool> {
        Err(self.err())
    }

    async fn purge_session_for_agent(
        &self,
        _session_id: &str,
        _agent_id: &str,
    ) -> anyhow::Result<usize> {
        Err(self.err())
    }

    async fn count(&self) -> anyhow::Result<usize> {
        Err(self.err())
    }

    async fn health_check(&self) -> bool {
        false
    }

    async fn store_with_agent(
        &self,
        _key: &str,
        _content: &str,
        _category: MemoryCategory,
        _session_id: Option<&str>,
        _namespace: Option<&str>,
        _importance: Option<f64>,
        _agent_id: Option<&str>,
    ) -> anyhow::Result<()> {
        Err(self.err())
    }

    async fn recall_for_agents(
        &self,
        _allowed_agent_ids: &[&str],
        _query: &str,
        _limit: usize,
        _session_id: Option<&str>,
        _since: Option<&str>,
        _until: Option<&str>,
    ) -> anyhow::Result<Vec<MemoryEntry>> {
        Err(self.err())
    }

    // The trait defaults below answer success (empty results, zero counts,
    // no-op writes) for backends without the capability. For a backend that
    // failed to build that is a false success, so each one fails loudly.

    async fn get_for_agent(
        &self,
        _key: &str,
        _agent_id: &str,
    ) -> anyhow::Result<Option<MemoryEntry>> {
        Err(self.err())
    }

    async fn recall_namespaced(
        &self,
        _namespace: &str,
        _query: &str,
        _limit: usize,
        _session_id: Option<&str>,
        _since: Option<&str>,
        _until: Option<&str>,
    ) -> anyhow::Result<Vec<MemoryEntry>> {
        Err(self.err())
    }

    async fn export_agent(&self, _agent_alias: &str) -> anyhow::Result<Vec<MemoryEntry>> {
        Err(self.err())
    }

    async fn count_agent(&self, _agent_alias: &str) -> anyhow::Result<usize> {
        Err(self.err())
    }

    async fn supersede(&self, _superseded_ids: &[String], _new_id: &str) -> anyhow::Result<()> {
        Err(self.err())
    }

    async fn store_procedural(
        &self,
        _messages: &[ProceduralMessage],
        _session_id: Option<&str>,
    ) -> anyhow::Result<()> {
        Err(self.err())
    }

    async fn count_in_scope(
        &self,
        _namespace: Option<&str>,
        _category: Option<&MemoryCategory>,
    ) -> anyhow::Result<u64> {
        Err(self.err())
    }

    async fn stats(&self) -> anyhow::Result<MemoryStats> {
        Err(self.err())
    }

    async fn reindex(&self) -> anyhow::Result<usize> {
        Err(self.err())
    }

    async fn ensure_agent_uuid(&self, _alias: &str) -> anyhow::Result<String> {
        Err(self.err())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zeroclaw_api::attribution::Attributable as _;

    fn failed() -> FailedMemory {
        FailedMemory::new("sqilte", &anyhow::Error::msg("unknown backend"))
    }

    #[tokio::test]
    async fn failed_memory_fails_all_data_operations() {
        let memory = failed();

        assert!(
            memory
                .store("k", "v", MemoryCategory::Core, None)
                .await
                .is_err()
        );
        assert!(memory.get("k").await.is_err());
        assert!(memory.recall("k", 10, None, None, None).await.is_err());
        assert!(memory.list(None, None).await.is_err());
        assert!(memory.forget("k").await.is_err());
        assert!(memory.forget_for_agent("k", "a").await.is_err());
        assert!(memory.purge_session_for_agent("s", "a").await.is_err());
        assert!(memory.count().await.is_err());
        assert!(
            memory
                .store_with_agent("k", "v", MemoryCategory::Core, None, None, None, None)
                .await
                .is_err()
        );
        assert!(
            memory
                .recall_for_agents(&["a"], "k", 10, None, None, None)
                .await
                .is_err()
        );
        assert!(!memory.health_check().await);

        // Trait defaults that would otherwise report success.
        assert!(memory.get_for_agent("k", "a").await.is_err());
        assert!(
            memory
                .recall_namespaced("ns", "k", 0, None, None, None)
                .await
                .is_err()
        );
        assert!(memory.export_agent("a").await.is_err());
        assert!(memory.count_agent("a").await.is_err());
        assert!(memory.supersede(&["x".into()], "y").await.is_err());
        assert!(memory.store_procedural(&[], None).await.is_err());
        assert!(memory.count_in_scope(None, None).await.is_err());
        assert!(memory.stats().await.is_err());
        assert!(memory.reindex().await.is_err());
        assert!(memory.ensure_agent_uuid("a").await.is_err());
        assert!(
            memory
                .export(&super::super::traits::ExportFilter::default())
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn failed_memory_keeps_the_full_cause_chain() {
        let cause = anyhow::Error::msg("unable to open database file")
            .context("SQLite failed to open database");
        let memory = FailedMemory::new("sqlite", &cause);
        let msg = memory.count().await.unwrap_err().to_string();
        assert!(
            msg.contains("SQLite failed to open database")
                && msg.contains("unable to open database file"),
            "root cause must survive: {msg}"
        );
    }

    #[tokio::test]
    async fn failed_memory_error_names_backend_and_cause() {
        let memory = failed();

        let err = memory.store("k", "v", MemoryCategory::Core, None).await;
        let msg = err.unwrap_err().to_string();
        assert!(
            msg.contains("sqilte") && msg.contains("unknown backend"),
            "error must name the configured backend and the cause: {msg}"
        );
    }

    #[test]
    fn failed_memory_reports_backend_name_and_alias() {
        let memory = failed();
        assert_eq!(memory.name(), "failed");
        assert_eq!(memory.alias(), "sqilte");
    }
}
