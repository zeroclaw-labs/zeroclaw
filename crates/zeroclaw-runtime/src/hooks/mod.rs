pub mod builtin;
pub mod registry;
mod runner;
mod traits;

pub use runner::HookRunner;
// HookHandler and HookResult are part of the crate's public hook API surface.
// They may appear unused internally but are intentionally re-exported for
// external integrations and future plugin authors.
#[allow(unused_imports)]
pub use traits::{HookHandler, HookResult};

use std::sync::Arc;
use zeroclaw_config::schema::Config;

/// Assemble the per-run [`HookRunner`] from config: the enabled built-in
/// hooks plus any extras supplied by binary-local factories (e.g. the X0
/// conscience gate and continuity persistence). Returns `None` when hooks
/// are disabled; otherwise `Some` — possibly an empty runner, which the
/// loop treats as a no-op.
///
/// Single source of truth so every agent entry point (the Agent builder
/// and `process_message`) wires the same hook chain.
pub fn build_runner(config: &Config) -> Option<Arc<HookRunner>> {
    if !config.hooks.enabled {
        return None;
    }
    let mut runner = HookRunner::new();
    if config.hooks.builtin.command_logger {
        runner.register(Box::new(builtin::CommandLoggerHook::new()));
    }
    if config.hooks.builtin.webhook_audit.enabled {
        runner.register(Box::new(builtin::WebhookAuditHook::new(
            config.hooks.builtin.webhook_audit.clone(),
        )));
    }
    for extra in registry::build_extras(config) {
        runner.register(extra);
    }
    Some(Arc::new(runner))
}

#[cfg(test)]
mod build_runner_tests {
    use super::*;

    #[test]
    fn disabled_hooks_yield_no_runner() {
        let mut cfg = Config::default();
        cfg.hooks.enabled = false;
        assert!(
            build_runner(&cfg).is_none(),
            "hooks.enabled = false must wire no runner"
        );
    }

    #[test]
    fn enabled_default_yields_empty_runner() {
        let mut cfg = Config::default();
        cfg.hooks.enabled = true;
        cfg.hooks.builtin.command_logger = false;
        cfg.hooks.builtin.webhook_audit.enabled = false;
        let runner = build_runner(&cfg).expect("enabled hooks build a runner");
        // Empty runner ⇒ dispatch is a no-op, so this is behaviour-neutral
        // for callers that previously passed `None`.
        assert!(runner.is_empty());
    }

    #[test]
    fn command_logger_is_registered_when_enabled() {
        let mut cfg = Config::default();
        cfg.hooks.enabled = true;
        cfg.hooks.builtin.command_logger = true;
        cfg.hooks.builtin.webhook_audit.enabled = false;
        let runner = build_runner(&cfg).expect("enabled hooks build a runner");
        assert_eq!(
            runner.len(),
            1,
            "the command-logger hook must be registered"
        );
    }
}
