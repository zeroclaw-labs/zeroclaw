//! Integration and security tests for context access host functions.
//!
//! Task US-ZCL-26-9: comprehensive integration tests verifying:
//! 1. Plugin reads session context successfully (set + readback)
//! 2. Plugin reads user identity successfully (set + readback)
//! 3. Paranoid mode denies all context access at every security boundary
//! 4. Plugin without context capability cannot access context functions

use std::collections::HashMap;
use std::sync::Arc;
use zeroclaw::config::AuditConfig;
use zeroclaw::memory::none::NoneMemory;
use zeroclaw::plugins::host_functions::{
    AgentConfigResponse, HostFunctionRegistry, SessionContextResponse, UserIdentityResponse,
};
use zeroclaw::plugins::loader::NetworkSecurityLevel;
use zeroclaw::plugins::{ContextCapability, MemoryCapability, PluginCapabilities, PluginManifest};
use zeroclaw::security::audit::AuditLogger;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn make_registry() -> HostFunctionRegistry {
    let tmp = tempfile::TempDir::new().expect("temp dir");
    let memory = Arc::new(NoneMemory::new());
    let audit = Arc::new(
        AuditLogger::new(
            AuditConfig {
                enabled: false,
                ..Default::default()
            },
            tmp.path().to_path_buf(),
        )
        .expect("audit logger"),
    );
    HostFunctionRegistry::new(memory, vec![], audit)
}

fn manifest_with_caps(caps: PluginCapabilities) -> PluginManifest {
    let toml_str = r#"
[plugin]
name = "test-context-integration"
version = "0.1.0"
wasm_path = "plugin.wasm"
capabilities = ["tool"]
"#;
    let mut m = PluginManifest::parse(toml_str).unwrap();
    m.host_capabilities = caps;
    m
}

fn all_context_caps() -> PluginCapabilities {
    PluginCapabilities {
        context: Some(ContextCapability {
            session: true,
            user_identity: true,
            agent_config: true,
        }),
        ..Default::default()
    }
}

// ===========================================================================
// 1. Plugin reads session context successfully
// ===========================================================================

#[test]
fn session_context_set_and_readback() {
    let registry = make_registry();

    registry.set_session_context(SessionContextResponse {
        channel_name: "telegram".to_string(),
        conversation_id: "conv-42".to_string(),
        timestamp: "2026-03-30T12:00:00Z".to_string(),
    });

    let ctx = registry.session_context.lock();
    assert_eq!(ctx.channel_name, "telegram");
    assert_eq!(ctx.conversation_id, "conv-42");
    assert_eq!(ctx.timestamp, "2026-03-30T12:00:00Z");
}

#[test]
fn session_context_updates_reflect_latest_value() {
    let registry = make_registry();

    registry.set_session_context(SessionContextResponse {
        channel_name: "slack".to_string(),
        conversation_id: "old-conv".to_string(),
        timestamp: "2026-03-30T10:00:00Z".to_string(),
    });

    // Update with new values
    registry.set_session_context(SessionContextResponse {
        channel_name: "matrix".to_string(),
        conversation_id: "new-conv".to_string(),
        timestamp: "2026-03-30T11:00:00Z".to_string(),
    });

    let ctx = registry.session_context.lock();
    assert_eq!(ctx.channel_name, "matrix", "should reflect latest set");
    assert_eq!(ctx.conversation_id, "new-conv");
}

#[test]
fn session_context_serializes_to_valid_json() {
    let registry = make_registry();

    registry.set_session_context(SessionContextResponse {
        channel_name: "telegram".to_string(),
        conversation_id: "conv-99".to_string(),
        timestamp: "2026-03-30T15:30:00Z".to_string(),
    });

    let ctx = registry.session_context.lock();
    let json = serde_json::to_value(&*ctx).expect("serialization must succeed");
    assert_eq!(json["channel_name"], "telegram");
    assert_eq!(json["conversation_id"], "conv-99");
    assert_eq!(json["timestamp"], "2026-03-30T15:30:00Z");
}

#[test]
fn session_context_default_has_empty_fields() {
    let registry = make_registry();
    let ctx = registry.session_context.lock();
    assert!(
        ctx.channel_name.is_empty(),
        "default channel_name should be empty"
    );
    assert!(
        ctx.conversation_id.is_empty(),
        "default conversation_id should be empty"
    );
    // timestamp has a default but should be a valid string
    assert!(
        !ctx.timestamp.is_empty(),
        "default timestamp should not be empty"
    );
}

#[test]
fn session_context_function_registered_for_enabled_capability() {
    let registry = make_registry();
    let manifest = manifest_with_caps(PluginCapabilities {
        context: Some(ContextCapability {
            session: true,
            user_identity: false,
            agent_config: false,
        }),
        ..Default::default()
    });

    let fns = registry.build_functions(&manifest);
    let names: Vec<&str> = fns.iter().map(|f| f.name()).collect();
    assert!(
        names.contains(&"context_session"),
        "session function must be registered"
    );
    assert_eq!(fns.len(), 1, "only one function for session-only");
}

// ===========================================================================
// 2. Plugin reads user identity successfully
// ===========================================================================

#[test]
fn user_identity_set_and_readback() {
    let registry = make_registry();

    registry.set_user_identity(UserIdentityResponse {
        username: "jdoe".to_string(),
        display_name: "Jane Doe".to_string(),
        channel_user_id: "U12345".to_string(),
    });

    let id = registry.user_identity.lock();
    assert_eq!(id.username, "jdoe");
    assert_eq!(id.display_name, "Jane Doe");
    assert_eq!(id.channel_user_id, "U12345");
}

#[test]
fn user_identity_updates_reflect_latest_value() {
    let registry = make_registry();

    registry.set_user_identity(UserIdentityResponse {
        username: "alice".to_string(),
        display_name: "Alice".to_string(),
        channel_user_id: "TG-1".to_string(),
    });

    registry.set_user_identity(UserIdentityResponse {
        username: "bob".to_string(),
        display_name: "Bob".to_string(),
        channel_user_id: "TG-2".to_string(),
    });

    let id = registry.user_identity.lock();
    assert_eq!(id.username, "bob", "should reflect latest set");
    assert_eq!(id.display_name, "Bob");
    assert_eq!(id.channel_user_id, "TG-2");
}

#[test]
fn user_identity_serializes_to_valid_json() {
    let registry = make_registry();

    registry.set_user_identity(UserIdentityResponse {
        username: "dev".to_string(),
        display_name: "Developer".to_string(),
        channel_user_id: "SL-789".to_string(),
    });

    let id = registry.user_identity.lock();
    let json = serde_json::to_value(&*id).expect("serialization must succeed");
    assert_eq!(json["username"], "dev");
    assert_eq!(json["display_name"], "Developer");
    assert_eq!(json["channel_user_id"], "SL-789");
}

#[test]
fn user_identity_function_registered_for_enabled_capability() {
    let registry = make_registry();
    let manifest = manifest_with_caps(PluginCapabilities {
        context: Some(ContextCapability {
            session: false,
            user_identity: true,
            agent_config: false,
        }),
        ..Default::default()
    });

    let fns = registry.build_functions(&manifest);
    let names: Vec<&str> = fns.iter().map(|f| f.name()).collect();
    assert!(
        names.contains(&"context_user_identity"),
        "user_identity function must be registered"
    );
    assert_eq!(fns.len(), 1, "only one function for user_identity-only");
}

// ===========================================================================
// 2b. Agent config round-trip (bonus — completes the context triple)
// ===========================================================================

#[test]
fn agent_config_set_and_readback() {
    let registry = make_registry();

    let mut identity = HashMap::new();
    identity.insert("role".to_string(), "assistant".to_string());
    identity.insert("team".to_string(), "engineering".to_string());

    registry.set_agent_config(AgentConfigResponse {
        name: "ZeroClaw".to_string(),
        personality_traits: vec!["friendly".to_string(), "concise".to_string()],
        identity,
    });

    let cfg = registry.agent_config.lock();
    assert_eq!(cfg.name, "ZeroClaw");
    assert_eq!(cfg.personality_traits, vec!["friendly", "concise"]);
    assert_eq!(cfg.identity.get("role").unwrap(), "assistant");
    assert_eq!(cfg.identity.get("team").unwrap(), "engineering");
}

#[test]
fn agent_config_serializes_to_valid_json() {
    let registry = make_registry();

    registry.set_agent_config(AgentConfigResponse {
        name: "Bot".to_string(),
        personality_traits: vec!["technical".to_string()],
        identity: HashMap::new(),
    });

    let cfg = registry.agent_config.lock();
    let json = serde_json::to_value(&*cfg).expect("serialization must succeed");
    assert_eq!(json["name"], "Bot");
    assert_eq!(json["personality_traits"], serde_json::json!(["technical"]));
    assert!(json["identity"].as_object().unwrap().is_empty());
}

// ===========================================================================
// 3. Paranoid mode denies all context access
// ===========================================================================

#[test]
fn paranoid_denies_all_context_even_when_all_enabled() {
    let registry = make_registry();
    let manifest = manifest_with_caps(all_context_caps());

    let fns = registry.build_functions_for_level(&manifest, NetworkSecurityLevel::Paranoid);

    assert!(
        fns.is_empty(),
        "paranoid mode must register zero context functions"
    );
}

#[test]
fn paranoid_denies_context_but_preserves_memory() {
    let registry = make_registry();
    let manifest = manifest_with_caps(PluginCapabilities {
        memory: Some(MemoryCapability {
            read: true,
            write: true,
        }),
        context: Some(ContextCapability {
            session: true,
            user_identity: true,
            agent_config: true,
        }),
        ..Default::default()
    });

    let fns = registry.build_functions_for_level(&manifest, NetworkSecurityLevel::Paranoid);
    let names: Vec<&str> = fns.iter().map(|f| f.name()).collect();

    // Memory should still work
    assert!(names.contains(&"zeroclaw_memory_recall"));
    assert!(names.contains(&"zeroclaw_memory_store"));
    assert!(names.contains(&"zeroclaw_memory_forget"));

    // Context must be denied
    assert!(!names.contains(&"context_session"));
    assert!(!names.contains(&"context_user_identity"));
    assert!(!names.contains(&"context_agent_config"));

    assert_eq!(fns.len(), 3, "only 3 memory functions in paranoid mode");
}

#[test]
fn paranoid_denies_each_context_sub_capability_individually() {
    let registry = make_registry();

    // Test each sub-capability in isolation
    for (label, caps) in [
        (
            "session-only",
            ContextCapability {
                session: true,
                user_identity: false,
                agent_config: false,
            },
        ),
        (
            "user_identity-only",
            ContextCapability {
                session: false,
                user_identity: true,
                agent_config: false,
            },
        ),
        (
            "agent_config-only",
            ContextCapability {
                session: false,
                user_identity: false,
                agent_config: true,
            },
        ),
    ] {
        let manifest = manifest_with_caps(PluginCapabilities {
            context: Some(caps),
            ..Default::default()
        });

        let fns = registry.build_functions_for_level(&manifest, NetworkSecurityLevel::Paranoid);
        assert!(
            fns.is_empty(),
            "paranoid must deny {label}, got {} functions",
            fns.len()
        );
    }
}

#[test]
fn non_paranoid_levels_all_allow_context() {
    let registry = make_registry();
    let manifest = manifest_with_caps(all_context_caps());

    for level in [
        NetworkSecurityLevel::Default,
        NetworkSecurityLevel::Strict,
        NetworkSecurityLevel::Relaxed,
    ] {
        let fns = registry.build_functions_for_level(&manifest, level);
        assert_eq!(
            fns.len(),
            3,
            "{level:?} should allow all 3 context functions"
        );
    }
}

// ===========================================================================
// 4. Plugin without context capability cannot access context functions
// ===========================================================================

#[test]
fn no_context_capability_yields_no_context_functions() {
    let registry = make_registry();
    let manifest = manifest_with_caps(PluginCapabilities::default());

    let fns = registry.build_functions(&manifest);
    assert!(
        fns.is_empty(),
        "default capabilities should yield no functions"
    );
}

#[test]
fn context_none_yields_no_context_functions() {
    let registry = make_registry();
    let manifest = manifest_with_caps(PluginCapabilities {
        context: None,
        ..Default::default()
    });

    let fns = registry.build_functions(&manifest);
    assert!(fns.is_empty(), "context: None should yield no functions");
}

#[test]
fn context_all_false_yields_no_context_functions() {
    let registry = make_registry();
    let manifest = manifest_with_caps(PluginCapabilities {
        context: Some(ContextCapability {
            session: false,
            user_identity: false,
            agent_config: false,
        }),
        ..Default::default()
    });

    let fns = registry.build_functions(&manifest);
    assert!(
        fns.is_empty(),
        "all-false context should yield no functions, got {}",
        fns.len()
    );
}

#[test]
fn memory_only_plugin_gets_no_context_functions() {
    let registry = make_registry();
    let manifest = manifest_with_caps(PluginCapabilities {
        memory: Some(MemoryCapability {
            read: true,
            write: true,
        }),
        context: None,
        ..Default::default()
    });

    let fns = registry.build_functions(&manifest);
    let names: Vec<&str> = fns.iter().map(|f| f.name()).collect();

    // Memory functions present
    assert!(names.contains(&"zeroclaw_memory_recall"));
    assert!(names.contains(&"zeroclaw_memory_store"));

    // No context functions
    assert!(!names.contains(&"context_session"));
    assert!(!names.contains(&"context_user_identity"));
    assert!(!names.contains(&"context_agent_config"));
}

// ===========================================================================
// Security: context data isolation between updates
// ===========================================================================

#[test]
fn context_setters_are_independent() {
    let registry = make_registry();

    // Set session context
    registry.set_session_context(SessionContextResponse {
        channel_name: "telegram".to_string(),
        conversation_id: "c1".to_string(),
        timestamp: "2026-03-30T00:00:00Z".to_string(),
    });

    // Set user identity (should not affect session context)
    registry.set_user_identity(UserIdentityResponse {
        username: "alice".to_string(),
        display_name: "Alice".to_string(),
        channel_user_id: "A1".to_string(),
    });

    // Set agent config (should not affect either)
    registry.set_agent_config(AgentConfigResponse {
        name: "Bot".to_string(),
        personality_traits: vec![],
        identity: HashMap::new(),
    });

    // Verify each is independent
    let ctx = registry.session_context.lock();
    assert_eq!(ctx.channel_name, "telegram");
    drop(ctx);

    let id = registry.user_identity.lock();
    assert_eq!(id.username, "alice");
    drop(id);

    let cfg = registry.agent_config.lock();
    assert_eq!(cfg.name, "Bot");
}
