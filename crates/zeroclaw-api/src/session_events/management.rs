//! Agent lifecycle access to standard sessions. Application composition supplies
//! an implementation; consumers do not open databases or select a backend.

pub trait AgentSessionPort: Send + Sync {
    fn count_live(&self, agent: &str) -> anyhow::Result<usize>;
    fn list_by_agent(&self, agent: &str) -> anyhow::Result<Vec<super::catalog::Session>>;
    fn close_by_agent(&self, agent: &str) -> anyhow::Result<usize>;
    fn rename_agent(&self, from: &str, to: &str) -> anyhow::Result<usize>;
}
