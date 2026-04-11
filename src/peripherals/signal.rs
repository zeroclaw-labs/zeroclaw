//! Bridge between hardware peripherals and the generic dispatch router.
//!
//! Peripherals (STM32, RPi GPIO, Arduino, etc.) call [`emit_signal`] to
//! broadcast a hardware event to all registered [`EventHandler`]s without
//! depending on any specific consumer.
//!
//! The handler list is defined by the application at startup — for example
//! a notification handler that sends a KakaoTalk message when a doorbell
//! sensor fires, or an agent-trigger handler that runs an LLM action when
//! a temperature threshold is crossed.
//!
//! ## Why a separate helper instead of extending the `Peripheral` trait?
//!
//! - Most peripherals only expose tools (`Peripheral::tools()`) and never
//!   need to publish events. Forcing every implementation to know about
//!   the event router would be unnecessary churn.
//! - The helper is opt-in: a serial driver can choose to call it when a
//!   line arrives from the firmware, but is not required to.
//! - Keeps the dispatch dependency in one well-defined place (this file).
//!
//! ## Example
//!
//! ```ignore
//! use std::sync::Arc;
//! use zeroclaw::dispatch::{DispatchAuditLogger, EventRouter};
//! use zeroclaw::peripherals::signal::emit_signal;
//!
//! // Built once at startup:
//! let router: Arc<EventRouter> = Arc::new(EventRouter::new());
//! let audit: Arc<DispatchAuditLogger> = ...;
//!
//! // Inside a serial peripheral driver, when a GPIO line changes:
//! let result = emit_signal(
//!     &router, &audit,
//!     "nucleo-f401re", "pin_3", Some("1"),
//! ).await?;
//! tracing::info!("dispatched to {} handlers", result.handler_outcomes.len());
//! ```

use std::sync::Arc;

use anyhow::Result;

use crate::dispatch::{
    DispatchAuditLogger, DispatchEvent, DispatchResult, EventRouter, EventSource,
};

/// Emit a peripheral hardware signal as a dispatch event.
///
/// The topic is constructed as `{board}/{signal}` so handlers can match by
/// either the full path or via wildcard prefix matching in the future.
///
/// Audit-log failures are logged via `tracing::warn!` but never block the
/// dispatch — hardware events must not be lost because of an audit issue.
pub async fn emit_signal(
    router: &Arc<EventRouter>,
    audit: &DispatchAuditLogger,
    board: &str,
    signal: &str,
    payload: Option<&str>,
) -> Result<DispatchResult> {
    let topic = format!("{board}/{signal}");
    let event = DispatchEvent::new(
        EventSource::Peripheral,
        Some(topic),
        payload.map(String::from),
    );

    if let Err(e) = audit.log_event(&event).await {
        tracing::warn!("peripheral signal: audit log_event failed (continuing): {e}");
    }

    let result = router.dispatch(event).await;

    if let Err(e) = audit.log_result(&result).await {
        tracing::warn!("peripheral signal: audit log_result failed: {e}");
    }

    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dispatch::types::HandlerOutcome;
    use crate::dispatch::EventHandler;
    use async_trait::async_trait;

    struct CountingHandler {
        seen: parking_lot::Mutex<Vec<String>>,
    }

    #[async_trait]
    impl EventHandler for CountingHandler {
        fn name(&self) -> &str {
            "counting"
        }
        fn matches(&self, event: &DispatchEvent) -> bool {
            event.source == EventSource::Peripheral
        }
        async fn handle(&self, event: &DispatchEvent) -> anyhow::Result<HandlerOutcome> {
            self.seen
                .lock()
                .push(event.topic.clone().unwrap_or_default());
            Ok(HandlerOutcome::Handled {
                summary: "counted".into(),
            })
        }
    }

    async fn make_audit() -> DispatchAuditLogger {
        let mem_cfg = crate::config::MemoryConfig {
            backend: "sqlite".into(),
            ..crate::config::MemoryConfig::default()
        };
        let tmp = tempfile::tempdir().unwrap();
        let memory: std::sync::Arc<dyn crate::memory::traits::Memory> =
            std::sync::Arc::from(crate::memory::create_memory(&mem_cfg, tmp.path(), None).unwrap());
        DispatchAuditLogger::new(memory)
    }

    #[tokio::test]
    async fn emit_signal_routes_to_handler_and_audits() {
        let router = Arc::new(EventRouter::new());
        let handler = Arc::new(CountingHandler {
            seen: parking_lot::Mutex::new(Vec::new()),
        });
        router.register(handler.clone());
        let audit = make_audit().await;

        let result = emit_signal(&router, &audit, "nucleo-f401re", "pin_3", Some("1"))
            .await
            .unwrap();

        assert_eq!(result.handled_count(), 1);
        assert_eq!(handler.seen.lock().len(), 1);
        assert_eq!(handler.seen.lock()[0], "nucleo-f401re/pin_3");

        // Audit log should contain at least one dispatch event entry.
        let events = audit.list_events().await.unwrap();
        assert_eq!(events.len(), 1);
    }

    #[tokio::test]
    async fn emit_signal_with_no_handlers_still_audits() {
        let router = Arc::new(EventRouter::new());
        let audit = make_audit().await;

        let result = emit_signal(&router, &audit, "rpi-gpio", "gpio_17", None)
            .await
            .unwrap();

        assert_eq!(result.handled_count(), 0);
        assert!(result.matched_handlers.is_empty());
        let events = audit.list_events().await.unwrap();
        assert_eq!(events.len(), 1);
    }
}
