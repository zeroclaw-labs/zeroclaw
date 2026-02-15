use chrono::Utc;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};

type SubscriberMap = HashMap<u64, UnboundedSender<String>>;

static SUBSCRIBERS: OnceLock<Mutex<SubscriberMap>> = OnceLock::new();
static NEXT_SUBSCRIBER_ID: AtomicU64 = AtomicU64::new(1);

fn subscribers() -> &'static Mutex<SubscriberMap> {
    SUBSCRIBERS.get_or_init(|| Mutex::new(HashMap::new()))
}

pub fn subscribe() -> (u64, UnboundedReceiver<String>) {
    let id = NEXT_SUBSCRIBER_ID.fetch_add(1, Ordering::Relaxed);
    let (tx, rx) = unbounded_channel::<String>();
    subscribers().lock().unwrap().insert(id, tx);
    (id, rx)
}

pub fn unsubscribe(id: u64) {
    subscribers().lock().unwrap().remove(&id);
}

pub fn emit(event_type: &str, data: Value) {
    let payload = serde_json::json!({
        "type": event_type,
        "data": data,
        "timestamp": Utc::now().to_rfc3339(),
    })
    .to_string();

    let mut stale: Vec<u64> = Vec::new();
    {
        let guard = subscribers().lock().unwrap();
        for (id, tx) in guard.iter() {
            if tx.send(payload.clone()).is_err() {
                stale.push(*id);
            }
        }
    }
    if !stale.is_empty() {
        let mut guard = subscribers().lock().unwrap();
        for id in stale {
            guard.remove(&id);
        }
    }
}
