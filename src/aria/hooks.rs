//! Cross-module event propagation via module-level singletons.
//!
//! Allows the Aria registry layer to notify other subsystems (feed scheduler,
//! cron bridge) when entities are uploaded or deleted, without introducing
//! circular dependencies.

use std::sync::Mutex;

// ── Type aliases ────────────────────────────────────────────────

/// Callback invoked when a feed is uploaded (created or updated).
pub type FeedUploadedFn = Box<dyn Fn(&str) + Send + Sync>;
/// Callback invoked when a feed is deleted.
pub type FeedDeletedFn = Box<dyn Fn(&str) + Send + Sync>;
/// Callback invoked when a cron function is uploaded (created or updated).
pub type CronUploadedFn = Box<dyn Fn(&str) + Send + Sync>;
/// Callback invoked when a cron function is deleted.
pub type CronDeletedFn = Box<dyn Fn(&str) + Send + Sync>;

// ── Hook containers ─────────────────────────────────────────────

/// Holds the feed lifecycle callbacks.
pub struct FeedHooks {
    pub on_feed_uploaded: Option<FeedUploadedFn>,
    pub on_feed_deleted: Option<FeedDeletedFn>,
}

/// Holds the cron function lifecycle callbacks.
pub struct CronHooks {
    pub on_cron_uploaded: Option<CronUploadedFn>,
    pub on_cron_deleted: Option<CronDeletedFn>,
}

// ── Global singletons ───────────────────────────────────────────

static FEED_HOOKS: Mutex<Option<FeedHooks>> = Mutex::new(None);
static CRON_HOOKS: Mutex<Option<CronHooks>> = Mutex::new(None);

// ── Feed hook API ───────────────────────────────────────────────

/// Register feed lifecycle hooks. Replaces any previously set hooks.
pub fn set_feed_hooks(hooks: FeedHooks) {
    let mut guard = FEED_HOOKS.lock().unwrap();
    *guard = Some(hooks);
}

/// Clear feed hooks (useful for teardown/testing).
pub fn clear_feed_hooks() {
    let mut guard = FEED_HOOKS.lock().unwrap();
    *guard = None;
}

/// Notify that a feed was uploaded (created or updated).
/// If no hooks are registered, this is a no-op.
pub fn notify_feed_uploaded(feed_id: &str) {
    let guard = FEED_HOOKS.lock().unwrap();
    if let Some(ref hooks) = *guard {
        if let Some(ref cb) = hooks.on_feed_uploaded {
            cb(feed_id);
        }
    }
}

/// Notify that a feed was deleted.
/// If no hooks are registered, this is a no-op.
pub fn notify_feed_deleted(feed_id: &str) {
    let guard = FEED_HOOKS.lock().unwrap();
    if let Some(ref hooks) = *guard {
        if let Some(ref cb) = hooks.on_feed_deleted {
            cb(feed_id);
        }
    }
}

// ── Cron hook API ───────────────────────────────────────────────

/// Register cron function lifecycle hooks. Replaces any previously set hooks.
pub fn set_cron_hooks(hooks: CronHooks) {
    let mut guard = CRON_HOOKS.lock().unwrap();
    *guard = Some(hooks);
}

/// Clear cron hooks (useful for teardown/testing).
pub fn clear_cron_hooks() {
    let mut guard = CRON_HOOKS.lock().unwrap();
    *guard = None;
}

/// Notify that a cron function was uploaded (created or updated).
/// If no hooks are registered, this is a no-op.
pub fn notify_cron_uploaded(cron_func_id: &str) {
    let guard = CRON_HOOKS.lock().unwrap();
    if let Some(ref hooks) = *guard {
        if let Some(ref cb) = hooks.on_cron_uploaded {
            cb(cron_func_id);
        }
    }
}

/// Notify that a cron function was deleted.
/// If no hooks are registered, this is a no-op.
pub fn notify_cron_deleted(cron_func_id: &str) {
    let guard = CRON_HOOKS.lock().unwrap();
    if let Some(ref hooks) = *guard {
        if let Some(ref cb) = hooks.on_cron_deleted {
            cb(cron_func_id);
        }
    }
}

// ── Tests ───────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;

    // Use a serial lock to prevent test interference with global state
    static TEST_LOCK: Mutex<()> = Mutex::new(());

    fn with_clean_hooks<F: FnOnce()>(f: F) {
        let _guard = TEST_LOCK.lock().unwrap();
        clear_feed_hooks();
        clear_cron_hooks();
        f();
        clear_feed_hooks();
        clear_cron_hooks();
    }

    #[test]
    fn feed_upload_notification_fires() {
        with_clean_hooks(|| {
            let counter = Arc::new(AtomicU32::new(0));
            let counter_clone = counter.clone();
            let last_id = Arc::new(Mutex::new(String::new()));
            let last_id_clone = last_id.clone();

            set_feed_hooks(FeedHooks {
                on_feed_uploaded: Some(Box::new(move |id| {
                    counter_clone.fetch_add(1, Ordering::SeqCst);
                    *last_id_clone.lock().unwrap() = id.to_string();
                })),
                on_feed_deleted: None,
            });

            notify_feed_uploaded("feed-123");
            assert_eq!(counter.load(Ordering::SeqCst), 1);
            assert_eq!(*last_id.lock().unwrap(), "feed-123");

            notify_feed_uploaded("feed-456");
            assert_eq!(counter.load(Ordering::SeqCst), 2);
            assert_eq!(*last_id.lock().unwrap(), "feed-456");
        });
    }

    #[test]
    fn feed_delete_notification_fires() {
        with_clean_hooks(|| {
            let counter = Arc::new(AtomicU32::new(0));
            let counter_clone = counter.clone();

            set_feed_hooks(FeedHooks {
                on_feed_uploaded: None,
                on_feed_deleted: Some(Box::new(move |_id| {
                    counter_clone.fetch_add(1, Ordering::SeqCst);
                })),
            });

            notify_feed_deleted("feed-789");
            assert_eq!(counter.load(Ordering::SeqCst), 1);
        });
    }

    #[test]
    fn feed_notification_noop_without_hooks() {
        with_clean_hooks(|| {
            // Should not panic even with no hooks registered
            notify_feed_uploaded("no-hook");
            notify_feed_deleted("no-hook");
        });
    }

    #[test]
    fn feed_notification_noop_with_none_callbacks() {
        with_clean_hooks(|| {
            set_feed_hooks(FeedHooks {
                on_feed_uploaded: None,
                on_feed_deleted: None,
            });

            // Should not panic
            notify_feed_uploaded("no-cb");
            notify_feed_deleted("no-cb");
        });
    }

    #[test]
    fn cron_upload_notification_fires() {
        with_clean_hooks(|| {
            let counter = Arc::new(AtomicU32::new(0));
            let counter_clone = counter.clone();
            let last_id = Arc::new(Mutex::new(String::new()));
            let last_id_clone = last_id.clone();

            set_cron_hooks(CronHooks {
                on_cron_uploaded: Some(Box::new(move |id| {
                    counter_clone.fetch_add(1, Ordering::SeqCst);
                    *last_id_clone.lock().unwrap() = id.to_string();
                })),
                on_cron_deleted: None,
            });

            notify_cron_uploaded("cron-abc");
            assert_eq!(counter.load(Ordering::SeqCst), 1);
            assert_eq!(*last_id.lock().unwrap(), "cron-abc");
        });
    }

    #[test]
    fn cron_delete_notification_fires() {
        with_clean_hooks(|| {
            let counter = Arc::new(AtomicU32::new(0));
            let counter_clone = counter.clone();

            set_cron_hooks(CronHooks {
                on_cron_uploaded: None,
                on_cron_deleted: Some(Box::new(move |_id| {
                    counter_clone.fetch_add(1, Ordering::SeqCst);
                })),
            });

            notify_cron_deleted("cron-xyz");
            assert_eq!(counter.load(Ordering::SeqCst), 1);
        });
    }

    #[test]
    fn cron_notification_noop_without_hooks() {
        with_clean_hooks(|| {
            notify_cron_uploaded("no-hook");
            notify_cron_deleted("no-hook");
        });
    }

    #[test]
    fn set_hooks_replaces_previous() {
        with_clean_hooks(|| {
            let counter1 = Arc::new(AtomicU32::new(0));
            let counter1_clone = counter1.clone();
            let counter2 = Arc::new(AtomicU32::new(0));
            let counter2_clone = counter2.clone();

            set_feed_hooks(FeedHooks {
                on_feed_uploaded: Some(Box::new(move |_| {
                    counter1_clone.fetch_add(1, Ordering::SeqCst);
                })),
                on_feed_deleted: None,
            });

            notify_feed_uploaded("test");
            assert_eq!(counter1.load(Ordering::SeqCst), 1);

            // Replace hooks
            set_feed_hooks(FeedHooks {
                on_feed_uploaded: Some(Box::new(move |_| {
                    counter2_clone.fetch_add(1, Ordering::SeqCst);
                })),
                on_feed_deleted: None,
            });

            notify_feed_uploaded("test");
            // Old counter should not increment
            assert_eq!(counter1.load(Ordering::SeqCst), 1);
            // New counter should increment
            assert_eq!(counter2.load(Ordering::SeqCst), 1);
        });
    }

    #[test]
    fn clear_hooks_disables_notifications() {
        with_clean_hooks(|| {
            let counter = Arc::new(AtomicU32::new(0));
            let counter_clone = counter.clone();

            set_feed_hooks(FeedHooks {
                on_feed_uploaded: Some(Box::new(move |_| {
                    counter_clone.fetch_add(1, Ordering::SeqCst);
                })),
                on_feed_deleted: None,
            });

            notify_feed_uploaded("test");
            assert_eq!(counter.load(Ordering::SeqCst), 1);

            clear_feed_hooks();

            notify_feed_uploaded("test");
            // Should not increment after clearing
            assert_eq!(counter.load(Ordering::SeqCst), 1);
        });
    }

    #[test]
    fn feed_and_cron_hooks_are_independent() {
        with_clean_hooks(|| {
            let feed_counter = Arc::new(AtomicU32::new(0));
            let feed_counter_clone = feed_counter.clone();
            let cron_counter = Arc::new(AtomicU32::new(0));
            let cron_counter_clone = cron_counter.clone();

            set_feed_hooks(FeedHooks {
                on_feed_uploaded: Some(Box::new(move |_| {
                    feed_counter_clone.fetch_add(1, Ordering::SeqCst);
                })),
                on_feed_deleted: None,
            });

            set_cron_hooks(CronHooks {
                on_cron_uploaded: Some(Box::new(move |_| {
                    cron_counter_clone.fetch_add(1, Ordering::SeqCst);
                })),
                on_cron_deleted: None,
            });

            notify_feed_uploaded("feed-1");
            notify_cron_uploaded("cron-1");

            assert_eq!(feed_counter.load(Ordering::SeqCst), 1);
            assert_eq!(cron_counter.load(Ordering::SeqCst), 1);

            // Clearing feed hooks should not affect cron hooks
            clear_feed_hooks();
            notify_feed_uploaded("feed-2");
            notify_cron_uploaded("cron-2");

            assert_eq!(feed_counter.load(Ordering::SeqCst), 1); // unchanged
            assert_eq!(cron_counter.load(Ordering::SeqCst), 2); // incremented
        });
    }
}
