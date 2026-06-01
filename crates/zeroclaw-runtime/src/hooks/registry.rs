//! Process-global extension point for adding hooks that live outside the
//! `zeroclaw-runtime` crate.
//!
//! The runtime crate doesn't know about binary-local modules like the X0
//! fork's conscience gate, so `Agent::run` can't construct their hooks
//! directly. Instead the binary registers a [`HookFactory`] at startup;
//! each time an Agent is built, the registry walks the registered
//! factories and asks each to produce the hooks it needs for the current
//! `Config`. The agent's `HookRunner` registers the results alongside the
//! built-in command-logger and webhook-audit hooks.
//!
//! ## Why a registry instead of a builder argument
//!
//! The Agent is constructed inside `Agent::run` (`agent/agent.rs:2153`),
//! which is the entry point the binary calls — there's no place to thread
//! per-call hook lists without rewriting every channel handler. Picking a
//! process-global registry keeps the architectural surface contained: the
//! binary calls [`register_factory`] once at startup, every Agent inside
//! the daemon inherits the registered factories, and the runtime trait
//! itself doesn't change.
//!
//! Pattern note: this mirrors how `tracing` lets crates `set_global_default`
//! without taking the subscriber as a per-call argument.

use std::sync::{LazyLock, Mutex};

use zeroclaw_config::schema::Config;

use super::traits::HookHandler;

/// Factory closure: given the loaded config, return the hooks the X0
/// binary wants to install in the next Agent. Run once per Agent build.
/// Should be cheap — the closure runs on every agent spawn, including
/// per-turn rebuilds in the gateway.
pub type HookFactory = Box<dyn Fn(&Config) -> Vec<Box<dyn HookHandler>> + Send + Sync + 'static>;

static REGISTRY: LazyLock<Mutex<Vec<HookFactory>>> = LazyLock::new(|| Mutex::new(Vec::new()));

/// Append a factory to the global registry. Called from the binary
/// (typically once, in `main`) before any Agent is constructed.
///
/// Registration is **append-only** and has no de-duplication; callers
/// should only register each factory once. Repeated registrations would
/// produce duplicate hook chains on every Agent build.
pub fn register_factory(factory: HookFactory) {
    if let Ok(mut guard) = REGISTRY.lock() {
        guard.push(factory);
    }
}

/// Invoke every registered factory with `config` and concatenate the
/// returned hook handlers. Returns an empty vec when no factories are
/// registered, which is the case for builds without `agent-runtime` or
/// `x0-extended`.
///
/// The hooks come back in factory-registration order; the agent's
/// `HookRunner` then sorts them by their declared `priority()`.
pub fn build_extras(config: &Config) -> Vec<Box<dyn HookHandler>> {
    let Ok(guard) = REGISTRY.lock() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for factory in guard.iter() {
        out.extend(factory(config));
    }
    out
}

/// Drain every registered factory. Used by tests so one test's
/// registrations don't leak into the next. Not part of the production
/// surface — production never unregisters.
#[cfg(test)]
pub fn clear_for_test() {
    if let Ok(mut guard) = REGISTRY.lock() {
        guard.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hooks::traits::HookHandler;
    use async_trait::async_trait;

    struct NopHook(&'static str);

    #[async_trait]
    impl HookHandler for NopHook {
        fn name(&self) -> &str {
            self.0
        }
    }

    // NOTE: registry is process-global. Tests cooperate by calling
    // clear_for_test() at the start of each test and serialise across
    // each other through `cargo test`'s default per-target single-thread
    // assumption only when run with `--test-threads=1`. They use a
    // private flag + mutex to guard against concurrency under normal
    // multi-threaded `cargo test`.
    use std::sync::Mutex as StdMutex;
    static GUARD: LazyLock<StdMutex<()>> = LazyLock::new(|| StdMutex::new(()));

    #[test]
    fn build_extras_is_empty_without_factories() {
        let _g = GUARD.lock().unwrap_or_else(|e| e.into_inner());
        clear_for_test();
        let cfg = Config::default();
        assert!(build_extras(&cfg).is_empty());
    }

    #[test]
    fn factories_run_in_registration_order() {
        let _g = GUARD.lock().unwrap_or_else(|e| e.into_inner());
        clear_for_test();
        register_factory(Box::new(|_| vec![Box::new(NopHook("first"))]));
        register_factory(Box::new(|_| vec![Box::new(NopHook("second"))]));
        let cfg = Config::default();
        let hooks = build_extras(&cfg);
        assert_eq!(hooks.len(), 2);
        assert_eq!(hooks[0].name(), "first");
        assert_eq!(hooks[1].name(), "second");
        clear_for_test();
    }

    #[test]
    fn factory_can_skip_hooks_based_on_config() {
        let _g = GUARD.lock().unwrap_or_else(|e| e.into_inner());
        clear_for_test();
        register_factory(Box::new(|cfg| {
            if cfg.conscience.gate_enabled {
                vec![Box::new(NopHook("gate-on"))]
            } else {
                Vec::new()
            }
        }));
        let mut cfg = Config::default();
        cfg.conscience.gate_enabled = false;
        assert!(build_extras(&cfg).is_empty());
        cfg.conscience.gate_enabled = true;
        let hooks = build_extras(&cfg);
        assert_eq!(hooks.len(), 1);
        assert_eq!(hooks[0].name(), "gate-on");
        clear_for_test();
    }
}
