//! Minimal mock `Channel` for in-crate observer tests.

use std::sync::Mutex;

use async_trait::async_trait;
use zeroclaw_api::channel::{
    Channel, ChannelMessage, SendMessage, StatusUpdate,
};

pub(crate) struct MockChannel {
    pub recorded: Mutex<Vec<StatusUpdate>>,
}

impl MockChannel {
    pub fn new() -> Self {
        Self { recorded: Mutex::new(Vec::new()) }
    }

    pub fn count(&self) -> usize {
        self.recorded.lock().unwrap().len()
    }

    pub fn last(&self) -> Option<StatusUpdate> {
        self.recorded.lock().unwrap().last().cloned()
    }
}

#[async_trait]
impl Channel for MockChannel {
    fn name(&self) -> &str { "mock" }

    async fn send(&self, _: &SendMessage) -> anyhow::Result<()> { Ok(()) }

    async fn listen(
        &self,
        _: tokio::sync::mpsc::Sender<ChannelMessage>,
    ) -> anyhow::Result<()> {
        Ok(())
    }

    async fn send_status_update(
        &self,
        _recipient: &str,
        _thread_ts: Option<&str>,
        update: StatusUpdate,
    ) -> anyhow::Result<()> {
        self.recorded.lock().unwrap().push(update);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zeroclaw_api::channel::StatusPhase;

    #[tokio::test]
    async fn records_send_status_update() {
        let ch = MockChannel::new();
        let update = StatusUpdate {
            execution_id: "e".into(),
            phase: StatusPhase::AgentStart,
            name: "agent".into(),
            desc: "x".into(),
        };
        ch.send_status_update("r", None, update.clone()).await.unwrap();
        assert_eq!(ch.count(), 1);
        assert_eq!(ch.last().unwrap().desc, "x");
    }
}
