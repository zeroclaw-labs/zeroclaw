pub mod health_checker;
pub mod resource_collector;
pub mod usage_collector;

use tokio::sync::broadcast;

/// Coordinator for background tasks.
/// Holds a shutdown broadcast sender.
pub struct BackgroundCoordinator {
    shutdown_tx: broadcast::Sender<()>,
}

impl BackgroundCoordinator {
    pub fn new() -> Self {
        let (shutdown_tx, _) = broadcast::channel(1);
        Self { shutdown_tx }
    }

    /// Get a shutdown receiver for a background task.
    pub fn subscribe_shutdown(&self) -> broadcast::Receiver<()> {
        self.shutdown_tx.subscribe()
    }

    /// Signal all background tasks to stop.
    pub fn shutdown(&self) {
        let _ = self.shutdown_tx.send(());
    }
}

impl Default for BackgroundCoordinator {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_coordinator_shutdown_signal() {
        let coord = BackgroundCoordinator::new();
        let mut rx = coord.subscribe_shutdown();
        coord.shutdown();
        // Receiver should get the signal
        assert!(rx.try_recv().is_ok());
    }

    #[test]
    fn test_coordinator_multiple_subscribers() {
        let coord = BackgroundCoordinator::new();
        let _rx1 = coord.subscribe_shutdown();
        let _rx2 = coord.subscribe_shutdown();
        // Should not panic
        coord.shutdown();
    }
}
