use async_trait::async_trait;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use zeroclaw::hooks::{HookHandler, HookRunner};

/// A test hook that counts how many times each lifecycle event fires.
struct LifecycleCounterHook {
    gateway_stops: Arc<AtomicUsize>,
    session_starts: Arc<AtomicUsize>,
    session_ends: Arc<AtomicUsize>,
    heartbeat_ticks: Arc<AtomicUsize>,
    last_session_id: Arc<parking_lot::Mutex<String>>,
    last_channel: Arc<parking_lot::Mutex<String>>,
}

#[async_trait]
impl HookHandler for LifecycleCounterHook {
    fn name(&self) -> &str {
        "lifecycle-counter"
    }

    async fn on_gateway_stop(&self) {
        self.gateway_stops.fetch_add(1, Ordering::SeqCst);
    }

    async fn on_session_start(&self, session_id: &str, channel: &str) {
        self.session_starts.fetch_add(1, Ordering::SeqCst);
        *self.last_session_id.lock() = session_id.to_string();
        *self.last_channel.lock() = channel.to_string();
    }

    async fn on_session_end(&self, session_id: &str, channel: &str) {
        self.session_ends.fetch_add(1, Ordering::SeqCst);
        *self.last_session_id.lock() = session_id.to_string();
        *self.last_channel.lock() = channel.to_string();
    }

    async fn on_heartbeat_tick(&self) {
        self.heartbeat_ticks.fetch_add(1, Ordering::SeqCst);
    }
}

#[tokio::test]
async fn fire_gateway_stop_dispatches_to_all_handlers() {
    let stops1 = Arc::new(AtomicUsize::new(0));
    let stops2 = Arc::new(AtomicUsize::new(0));

    let mut runner = HookRunner::new();
    runner.register(Box::new(LifecycleCounterHook {
        gateway_stops: stops1.clone(),
        session_starts: Arc::new(AtomicUsize::new(0)),
        session_ends: Arc::new(AtomicUsize::new(0)),
        heartbeat_ticks: Arc::new(AtomicUsize::new(0)),
        last_session_id: Arc::new(parking_lot::Mutex::new(String::new())),
        last_channel: Arc::new(parking_lot::Mutex::new(String::new())),
    }));
    runner.register(Box::new(LifecycleCounterHook {
        gateway_stops: stops2.clone(),
        session_starts: Arc::new(AtomicUsize::new(0)),
        session_ends: Arc::new(AtomicUsize::new(0)),
        heartbeat_ticks: Arc::new(AtomicUsize::new(0)),
        last_session_id: Arc::new(parking_lot::Mutex::new(String::new())),
        last_channel: Arc::new(parking_lot::Mutex::new(String::new())),
    }));

    runner.fire_gateway_stop().await;

    assert_eq!(stops1.load(Ordering::SeqCst), 1);
    assert_eq!(stops2.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn fire_session_start_passes_session_id_and_channel() {
    let session_starts = Arc::new(AtomicUsize::new(0));
    let last_session_id = Arc::new(parking_lot::Mutex::new(String::new()));
    let last_channel = Arc::new(parking_lot::Mutex::new(String::new()));

    let mut runner = HookRunner::new();
    runner.register(Box::new(LifecycleCounterHook {
        gateway_stops: Arc::new(AtomicUsize::new(0)),
        session_starts: session_starts.clone(),
        session_ends: Arc::new(AtomicUsize::new(0)),
        heartbeat_ticks: Arc::new(AtomicUsize::new(0)),
        last_session_id: last_session_id.clone(),
        last_channel: last_channel.clone(),
    }));

    runner.fire_session_start("sess-123", "websocket").await;

    assert_eq!(session_starts.load(Ordering::SeqCst), 1);
    assert_eq!(*last_session_id.lock(), "sess-123");
    assert_eq!(*last_channel.lock(), "websocket");
}

#[tokio::test]
async fn fire_session_end_passes_session_id_and_channel() {
    let session_ends = Arc::new(AtomicUsize::new(0));
    let last_session_id = Arc::new(parking_lot::Mutex::new(String::new()));
    let last_channel = Arc::new(parking_lot::Mutex::new(String::new()));

    let mut runner = HookRunner::new();
    runner.register(Box::new(LifecycleCounterHook {
        gateway_stops: Arc::new(AtomicUsize::new(0)),
        session_starts: Arc::new(AtomicUsize::new(0)),
        session_ends: session_ends.clone(),
        heartbeat_ticks: Arc::new(AtomicUsize::new(0)),
        last_session_id: last_session_id.clone(),
        last_channel: last_channel.clone(),
    }));

    runner.fire_session_end("sess-456", "websocket").await;

    assert_eq!(session_ends.load(Ordering::SeqCst), 1);
    assert_eq!(*last_session_id.lock(), "sess-456");
    assert_eq!(*last_channel.lock(), "websocket");
}

#[tokio::test]
async fn fire_heartbeat_tick_dispatches_to_all_handlers() {
    let ticks1 = Arc::new(AtomicUsize::new(0));
    let ticks2 = Arc::new(AtomicUsize::new(0));

    let mut runner = HookRunner::new();
    runner.register(Box::new(LifecycleCounterHook {
        gateway_stops: Arc::new(AtomicUsize::new(0)),
        session_starts: Arc::new(AtomicUsize::new(0)),
        session_ends: Arc::new(AtomicUsize::new(0)),
        heartbeat_ticks: ticks1.clone(),
        last_session_id: Arc::new(parking_lot::Mutex::new(String::new())),
        last_channel: Arc::new(parking_lot::Mutex::new(String::new())),
    }));
    runner.register(Box::new(LifecycleCounterHook {
        gateway_stops: Arc::new(AtomicUsize::new(0)),
        session_starts: Arc::new(AtomicUsize::new(0)),
        session_ends: Arc::new(AtomicUsize::new(0)),
        heartbeat_ticks: ticks2.clone(),
        last_session_id: Arc::new(parking_lot::Mutex::new(String::new())),
        last_channel: Arc::new(parking_lot::Mutex::new(String::new())),
    }));

    runner.fire_heartbeat_tick().await;

    assert_eq!(ticks1.load(Ordering::SeqCst), 1);
    assert_eq!(ticks2.load(Ordering::SeqCst), 1);
}
