use super::traits::{Memory, MemoryCategory, MemoryEntry};
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
            cause: cause.to_string(),
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
