//! End-to-end tests for node registration, persistence, cross-platform
//! capability management, and the device pairing lifecycle.
//!
//! These tests exercise the public `NodeRegistry`, `NodePersistence`, and
//! `DeviceRegistry` APIs directly. WebSocket-level tests (full HTTP stack)
//! live as inline `#[cfg(test)]` tests in `src/gateway/nodes.rs`.

use crate::support::platform_fixtures;
use std::sync::Arc;
use tokio::sync::mpsc;
use zeroclaw::gateway::api_pairing::DeviceRegistry;
use zeroclaw::gateway::nodes::{NodeCapability, NodeInfo, NodePersistence, NodeRegistry};

fn cap(name: &str, desc: &str) -> NodeCapability {
    NodeCapability {
        name: name.into(),
        description: desc.into(),
        parameters: serde_json::json!({"type": "object", "properties": {}}),
    }
}

fn make_info(node_id: &str, device_type: Option<&str>, caps: Vec<NodeCapability>) -> NodeInfo {
    let (tx, _rx) = mpsc::channel(1);
    NodeInfo {
        node_id: node_id.into(),
        device_type: device_type.map(String::from),
        capabilities: caps,
        invoke_tx: tx,
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Test 1: Registration round-trip + offline on disconnect
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn node_register_shows_online_and_offline_on_unregister() {
    let tmp = tempfile::TempDir::new().unwrap();
    let registry = NodeRegistry::new_with_persistence(16, tmp.path());

    let info = make_info(
        "test-phone",
        Some("android"),
        vec![cap("camera.snap", "Photo"), cap("gps", "GPS")],
    );
    assert!(registry.register(info));

    // Should be online.
    let all = registry.list_all_nodes();
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].node_id, "test-phone");
    assert_eq!(all[0].status, "online");
    assert_eq!(all[0].device_type.as_deref(), Some("android"));
    assert_eq!(all[0].capabilities.len(), 2);

    // Unregister (simulate disconnect).
    registry.unregister("test-phone");

    // Should show offline from persistence.
    let all = registry.list_all_nodes();
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].node_id, "test-phone");
    assert_eq!(all[0].status, "offline");
    assert_eq!(all[0].device_type.as_deref(), Some("android"));
}

// ─────────────────────────────────────────────────────────────────────────────
// Test 2: Persistence across registry re-creation (simulates gateway restart)
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn node_metadata_persists_across_registry_restart() {
    let tmp = tempfile::TempDir::new().unwrap();

    // First "session": register a node.
    {
        let registry = NodeRegistry::new_with_persistence(16, tmp.path());
        let info = make_info(
            "persistent-node",
            Some("macos"),
            platform_fixtures::macos_capabilities(),
        );
        assert!(registry.register(info));
        assert_eq!(registry.list_all_nodes().len(), 1);
        assert_eq!(registry.list_all_nodes()[0].status, "online");
        // Node disconnects.
        registry.unregister("persistent-node");
    }

    // Second "session": new registry with same persistence path.
    {
        let registry = NodeRegistry::new_with_persistence(16, tmp.path());

        // Should show offline from persistence (no live nodes).
        let all = registry.list_all_nodes();
        assert_eq!(all.len(), 1, "persisted node should survive restart");
        assert_eq!(all[0].node_id, "persistent-node");
        assert_eq!(all[0].status, "offline");
        assert_eq!(all[0].device_type.as_deref(), Some("macos"));
        assert_eq!(
            all[0].capabilities.len(),
            platform_fixtures::macos_capabilities().len()
        );

        // Reconnect (re-register).
        let info = make_info(
            "persistent-node",
            Some("macos"),
            platform_fixtures::macos_capabilities(),
        );
        assert!(registry.register(info));
        let all = registry.list_all_nodes();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].status, "online");
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Test 3: Device pairing + registry coexistence
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn paired_device_and_node_coexist() {
    let tmp = tempfile::TempDir::new().unwrap();

    // Create device registry and register a device.
    let device_reg = DeviceRegistry::new(tmp.path());
    device_reg.register(
        "hash_abc123".into(),
        zeroclaw::gateway::api_pairing::DeviceInfo {
            id: "device-1".into(),
            name: Some("My Phone".into()),
            device_type: Some("android".into()),
            paired_at: chrono::Utc::now(),
            last_seen: chrono::Utc::now(),
            ip_address: Some("192.168.1.50".into()),
        },
    );

    // Create node registry and register a node.
    let node_reg = NodeRegistry::new_with_persistence(16, tmp.path());
    let info = make_info(
        "phone-1",
        Some("android"),
        platform_fixtures::android_capabilities(),
    );
    assert!(node_reg.register(info));

    // Both should be visible via their respective APIs.
    let devices = device_reg.list();
    assert_eq!(devices.len(), 1);
    assert_eq!(devices[0].name.as_deref(), Some("My Phone"));

    let nodes = node_reg.list_all_nodes();
    assert_eq!(nodes.len(), 1);
    assert_eq!(nodes[0].node_id, "phone-1");
    assert_eq!(nodes[0].status, "online");
}

// ─────────────────────────────────────────────────────────────────────────────
// Test 4: Device revoke removes device from registry
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn device_revoke_removes_device() {
    let tmp = tempfile::TempDir::new().unwrap();
    let device_reg = DeviceRegistry::new(tmp.path());

    device_reg.register(
        "hash_def456".into(),
        zeroclaw::gateway::api_pairing::DeviceInfo {
            id: "revoke-me".into(),
            name: Some("Revoke Test".into()),
            device_type: None,
            paired_at: chrono::Utc::now(),
            last_seen: chrono::Utc::now(),
            ip_address: None,
        },
    );
    assert_eq!(device_reg.list().len(), 1);

    let revoked = device_reg.revoke("revoke-me");
    assert!(revoked, "revoke should succeed");
    assert_eq!(
        device_reg.list().len(),
        0,
        "device list should be empty after revoke"
    );

    // Revoking again should return false.
    assert!(!device_reg.revoke("revoke-me"));
}

// ─────────────────────────────────────────────────────────────────────────────
// Test 5: Cross-platform nodes have different capabilities
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn cross_platform_nodes_have_different_capabilities() {
    let tmp = tempfile::TempDir::new().unwrap();
    let registry = NodeRegistry::new_with_persistence(16, tmp.path());

    registry.register(make_info(
        "mac-1",
        Some("macos"),
        platform_fixtures::macos_capabilities(),
    ));
    registry.register(make_info(
        "android-1",
        Some("android"),
        platform_fixtures::android_capabilities(),
    ));
    registry.register(make_info(
        "win-1",
        Some("windows"),
        platform_fixtures::windows_capabilities(),
    ));
    registry.register(make_info(
        "linux-1",
        Some("linux"),
        platform_fixtures::linux_capabilities(),
    ));

    let all = registry.list_all_nodes();
    assert_eq!(all.len(), 4);

    let mac = all.iter().find(|n| n.node_id == "mac-1").unwrap();
    let android = all.iter().find(|n| n.node_id == "android-1").unwrap();
    let win = all.iter().find(|n| n.node_id == "win-1").unwrap();
    let linux = all.iter().find(|n| n.node_id == "linux-1").unwrap();

    // Verify device types.
    assert_eq!(mac.device_type.as_deref(), Some("macos"));
    assert_eq!(android.device_type.as_deref(), Some("android"));
    assert_eq!(win.device_type.as_deref(), Some("windows"));
    assert_eq!(linux.device_type.as_deref(), Some("linux"));

    // macOS should have applescript.
    assert!(
        mac.capabilities
            .iter()
            .any(|c| c.name == "desktop.applescript")
    );
    // Android should have sensors.
    assert!(
        android
            .capabilities
            .iter()
            .any(|c| c.name == "sensors.accelerometer")
    );
    // Android should NOT have applescript.
    assert!(
        !android
            .capabilities
            .iter()
            .any(|c| c.name == "desktop.applescript")
    );
    // Windows should have clipboard.
    assert!(
        win.capabilities
            .iter()
            .any(|c| c.name == "desktop.clipboard")
    );
    // Linux should have only 2 caps.
    assert_eq!(linux.capabilities.len(), 2);

    // Capability counts differ across platforms.
    let counts: Vec<usize> = all.iter().map(|n| n.capabilities.len()).collect();
    assert!(
        counts.windows(2).all(|w| w[0] != w[1]) || counts.len() > 2,
        "different platforms should have different capability sets"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Test 6: Capability invocation round-trip via channels
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn invoke_node_capability_returns_result() {
    let tmp = tempfile::TempDir::new().unwrap();
    let registry = Arc::new(NodeRegistry::new_with_persistence(16, tmp.path()));
    let (invoke_tx, mut invoke_rx) = mpsc::channel(32);

    registry.register(NodeInfo {
        node_id: "echo-node".into(),
        device_type: None,
        capabilities: vec![cap("echo", "Echo back")],
        invoke_tx,
    });

    // Invoke via the registry's channel.
    let reg_clone = registry.clone();
    let invoke_task = tokio::spawn(async move {
        let tx = reg_clone.invoke_tx("echo-node").unwrap();
        let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
        tx.send(zeroclaw::gateway::nodes::NodeInvocation {
            call_id: "call-1".into(),
            capability: "echo".into(),
            args: serde_json::json!({"msg": "hello"}),
            response_tx: resp_tx,
        })
        .await
        .unwrap();
        resp_rx.await.unwrap()
    });

    // Simulate node processing the invocation.
    let invocation = invoke_rx.recv().await.unwrap();
    assert_eq!(invocation.call_id, "call-1");
    assert_eq!(invocation.capability, "echo");
    invocation
        .response_tx
        .send(zeroclaw::gateway::nodes::NodeInvocationResult {
            success: true,
            output: "echoed: hello".into(),
            error: None,
        })
        .unwrap();

    let result = invoke_task.await.unwrap();
    assert!(result.success);
    assert_eq!(result.output, "echoed: hello");
}

// ─────────────────────────────────────────────────────────────────────────────
// Test 7: Multiple concurrent nodes are independent
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn multiple_concurrent_nodes_independent() {
    let tmp = tempfile::TempDir::new().unwrap();
    let registry = NodeRegistry::new_with_persistence(16, tmp.path());

    for i in 0..4 {
        registry.register(make_info(
            &format!("node-{i}"),
            None,
            vec![cap(&format!("cap-{i}"), &format!("Capability {i}"))],
        ));
    }
    assert_eq!(registry.len(), 4);

    let all = registry.list_all_nodes();
    let online = all.iter().filter(|n| n.status == "online").count();
    assert_eq!(online, 4);

    // Disconnect node-1.
    registry.unregister("node-1");
    assert_eq!(registry.len(), 3);

    let all = registry.list_all_nodes();
    assert_eq!(all.len(), 4, "all 4 should still be listed (1 offline)");
    let online = all.iter().filter(|n| n.status == "online").count();
    let offline = all.iter().filter(|n| n.status == "offline").count();
    assert_eq!(online, 3);
    assert_eq!(offline, 1);

    // The offline one should be node-1.
    let offline_node = all.iter().find(|n| n.status == "offline").unwrap();
    assert_eq!(offline_node.node_id, "node-1");
}

// ─────────────────────────────────────────────────────────────────────────────
// Test 8: Max nodes enforcement
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn max_nodes_enforcement_rejects_overflow() {
    let tmp = tempfile::TempDir::new().unwrap();
    let registry = NodeRegistry::new_with_persistence(2, tmp.path());

    registry.register(make_info("node-0", None, vec![]));
    registry.register(make_info("node-1", None, vec![]));
    assert_eq!(registry.len(), 2);

    // Third node should be rejected.
    let (tx, _rx) = mpsc::channel(1);
    let overflow = NodeInfo {
        node_id: "node-overflow".into(),
        device_type: None,
        capabilities: vec![],
        invoke_tx: tx,
    };
    assert!(!registry.register(overflow), "3rd node should be rejected");
    assert_eq!(registry.len(), 2);

    // But re-registering an existing node should succeed.
    registry.register(make_info(
        "node-0",
        Some("macos"),
        vec![cap("new", "Updated")],
    ));
    assert_eq!(registry.len(), 2);
    let caps = registry.all_capabilities();
    assert!(caps.iter().any(|c| c.2.name == "new"));
}

// ─────────────────────────────────────────────────────────────────────────────
// Test 9: Node persistence layer directly
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn node_persistence_crud() {
    let tmp = tempfile::TempDir::new().unwrap();
    let persistence = NodePersistence::new(tmp.path());

    // Persist a node.
    persistence.persist_node(
        "phone-1",
        Some("android"),
        &platform_fixtures::android_capabilities(),
        None,
    );

    let nodes = persistence.list_persisted_nodes();
    assert_eq!(nodes.len(), 1);
    assert_eq!(nodes[0].node_id, "phone-1");
    assert_eq!(nodes[0].device_type.as_deref(), Some("android"));
    assert_eq!(
        nodes[0].capabilities.len(),
        platform_fixtures::android_capabilities().len()
    );

    // Update (upsert).
    persistence.persist_node(
        "phone-1",
        Some("android"),
        &[cap("updated", "Updated cap")],
        Some("device-123"),
    );
    let nodes = persistence.list_persisted_nodes();
    assert_eq!(nodes.len(), 1, "upsert should not create duplicate");
    assert_eq!(nodes[0].capabilities.len(), 1);
    assert_eq!(nodes[0].linked_device_id.as_deref(), Some("device-123"));

    // Remove.
    assert!(persistence.remove_node("phone-1"));
    assert_eq!(persistence.list_persisted_nodes().len(), 0);
    assert!(
        !persistence.remove_node("phone-1"),
        "second remove returns false"
    );
}
