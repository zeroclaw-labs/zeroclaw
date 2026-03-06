//! Integration tests for multi-agent communication patterns.
//!
//! This test suite validates the Phase 1 multi-agent coordination features
//! including inter-agent messaging, shared state, and coordination patterns.

use zeroclaw::coordination::channel::{AgentMessageChannel, MemoryMessageChannel};
use zeroclaw::coordination::message::{AgentId, AgentMessage, MessagePayload};
use zeroclaw::coordination::state::{MemorySharedState, SharedAgentState, SharedValue};
use std::sync::Arc;
use std::time::Duration;

/// Test: Two agents exchange direct messages (notify mode).
#[tokio::test]
async fn two_agents_exchange_messages() {
    let channel = Arc::new(MemoryMessageChannel::new());
    let agent_a = AgentId::new("agent_a".to_string());
    let agent_b = AgentId::new("agent_b".to_string());

    // Register both agents
    channel.register(&agent_a).await.unwrap();
    channel.register(&agent_b).await.unwrap();

    // Agent A sends message to Agent B
    let message = AgentMessage::notification(
        agent_a.clone(),
        agent_b.clone(),
        MessagePayload::text("Hello from Agent A!"),
    );
    channel.send(message).await.unwrap();

    // Agent B receives the message
    let received = channel
        .receive(&agent_b, Duration::from_millis(100))
        .await
        .unwrap();

    assert_eq!(received.from(), &agent_a);
    assert_eq!(received.to(), Some(&agent_b));
    assert_eq!(received.payload().as_text(), Some("Hello from Agent A!"));
}

/// Test: Shared state coordination pattern for task claiming.
#[tokio::test]
async fn shared_state_coordination_pattern() {
    let state = Arc::new(MemorySharedState::new());
    let agent_a = AgentId::new("agent_a".to_string());
    let agent_b = AgentId::new("agent_b".to_string());

    // Agent A claims a task using CAS
    let claimed = state
        .cas(
            "task_123".to_string(),
            None,
            SharedValue::new(
                agent_a.clone(),
                serde_json::json!({"status": "claimed", "agent": "agent_a"}),
            ),
        )
        .await
        .unwrap();
    assert!(claimed);

    // Agent B tries to claim the same task (should fail)
    let claimed = state
        .cas(
            "task_123".to_string(),
            None,
            SharedValue::new(
                agent_b.clone(),
                serde_json::json!({"status": "claimed", "agent": "agent_b"}),
            ),
        )
        .await
        .unwrap();
    assert!(!claimed);

    // Verify agent_a owns the task
    let value = state.get("task_123").await.unwrap().unwrap();
    assert_eq!(value.created_by, agent_a);
}

/// Test: Broadcast message reaches all agents.
#[tokio::test]
async fn broadcast_messages() {
    let channel = Arc::new(MemoryMessageChannel::new());
    let agents: Vec<AgentId> = (0..5)
        .map(|i| AgentId::new(format!("agent_{}", i)))
        .collect();

    // Register all agents
    for agent in &agents {
        channel.register(agent).await.unwrap();
    }

    // Agent 0 broadcasts a message
    let message = AgentMessage::broadcast(
        agents[0].clone(),
        MessagePayload::text("Broadcast to all!"),
    );
    channel.send(message).await.unwrap();

    // All other agents should receive the message
    for agent in agents.iter().skip(1) {
        let received = channel
            .receive(agent, Duration::from_millis(100))
            .await
            .unwrap();
        assert_eq!(received.from(), &agents[0]);
        assert_eq!(received.payload().as_text(), Some("Broadcast to all!"));
    }

    // Sender should NOT receive their own broadcast
    assert!(channel
        .receive(&agents[0], Duration::from_millis(50))
        .await
        .is_err());
}

/// Test: Request-response timeout.
#[tokio::test]
async fn request_response_timeout() {
    let channel = Arc::new(MemoryMessageChannel::new());
    let agent_a = AgentId::new("agent_a".to_string());
    let agent_b = AgentId::new("agent_b".to_string());

    channel.register(&agent_a).await.unwrap();
    channel.register(&agent_b).await.unwrap();

    // Agent A sends a request with short timeout
    // Agent B doesn't respond (no handler running)
    let result = channel
        .request(
            agent_b.clone(),
            MessagePayload::text("Are you there?"),
            Duration::from_millis(100),
        )
        .await;

    assert!(result.is_err());
}

/// Test: Task claiming with CAS prevents race conditions.
#[tokio::test]
async fn task_claiming_with_cas() {
    let state = Arc::new(MemorySharedState::new());

    // Simulate two agents trying to claim the same task
    let task_key = "shared_task_001";
    let agent_a = AgentId::new("worker_a".to_string());
    let agent_b = AgentId::new("worker_b".to_string());

    // Agent A attempts to claim
    let claim_a = state
        .cas(
            task_key.to_string(),
            None,
            SharedValue::new(
                agent_a.clone(),
                serde_json::json!({"status": "claimed", "worker": "worker_a"}),
            ),
        )
        .await
        .unwrap();

    // Agent B attempts to claim (simultaneously)
    let claim_b = state
        .cas(
            task_key.to_string(),
            None,
            SharedValue::new(
                agent_b.clone(),
                serde_json::json!({"status": "claimed", "worker": "worker_b"}),
            ),
        )
        .await
        .unwrap();

    // Exactly one should succeed
    assert!(claim_a ^ claim_b); // XOR - only one true

    // Verify the task has exactly one owner
    let value = state.get(task_key).await.unwrap().unwrap();
    let owner = value.data["status"].as_str().unwrap();
    assert!(owner == "claimed");
}

/// Test: Message ordering is preserved per sender.
#[tokio::test]
async fn message_ordering_preserved() {
    let channel = Arc::new(MemoryMessageChannel::new());
    let agent_a = AgentId::new("agent_a".to_string());
    let agent_b = AgentId::new("agent_b".to_string());

    channel.register(&agent_a).await.unwrap();
    channel.register(&agent_b).await.unwrap();

    // Send multiple messages in order
    for i in 0..10 {
        let msg = AgentMessage::notification(
            agent_a.clone(),
            agent_b.clone(),
            MessagePayload::text(format!("message {}", i)),
        );
        channel.send(msg).await.unwrap();
    }

    // Receive and verify order
    for i in 0..10 {
        let msg = channel
            .receive(&agent_b, Duration::from_millis(100))
            .await
            .unwrap();
        assert_eq!(
            msg.payload().as_text(),
            Some(format!("message {}", i).as_str())
        );
    }
}

/// Test: List keys with prefix filtering.
#[tokio::test]
async fn list_keys_with_prefix() {
    let state = Arc::new(MemorySharedState::new());

    // Set keys with different prefixes
    for i in 1..=3 {
        let value = SharedValue::new(
            AgentId::new("system".to_string()),
            serde_json::json!(i),
        );
        state.set(format!("task:{}", i), value).await.unwrap();
    }
    for i in 1..=2 {
        let value = SharedValue::new(
            AgentId::new("system".to_string()),
            serde_json::json!(i),
        );
        state.set(format!("user:{}", i), value).await.unwrap();
    }

    // List task: prefix
    let task_keys = state.list(Some("task:")).await.unwrap();
    assert_eq!(task_keys.len(), 3);
    assert!(task_keys.contains(&"task:1".to_string()));
    assert!(task_keys.contains(&"task:2".to_string()));
    assert!(task_keys.contains(&"task:3".to_string()));

    // List user: prefix
    let user_keys = state.list(Some("user:")).await.unwrap();
    assert_eq!(user_keys.len(), 2);
}

/// Test: State version increments on updates.
#[tokio::test]
async fn state_version_increments() {
    let state = Arc::new(MemorySharedState::new());
    let agent_id = AgentId::new("test_agent".to_string());

    // Initial set
    let value1 = SharedValue::new(agent_id.clone(), serde_json::json!(1));
    state.set("counter".to_string(), value1).await.unwrap();

    let v1 = state.get("counter").await.unwrap().unwrap();
    assert_eq!(v1.version, 1);

    // Update should increment version
    let value2 = SharedValue::new(agent_id.clone(), serde_json::json!(2));
    state.set("counter".to_string(), value2).await.unwrap();

    let v2 = state.get("counter").await.unwrap().unwrap();
    assert_eq!(v2.version, 2);
    assert_ne!(v1.updated_at, v2.updated_at);
}

/// Test: Delete operation removes key.
#[tokio::test]
async fn delete_removes_key() {
    let state = Arc::new(MemorySharedState::new());
    let agent_id = AgentId::new("test_agent".to_string());

    let value = SharedValue::new(agent_id, serde_json::json!("test"));
    state.set("temp_key".to_string(), value).await.unwrap();

    // Verify exists
    assert!(state.get("temp_key").await.unwrap().is_some());

    // Delete
    assert!(state.delete("temp_key").await.unwrap());

    // Verify gone
    assert!(state.get("temp_key").await.unwrap().is_none());
}

/// Test: CAS operation with matching expected value succeeds.
#[tokio::test]
async fn cas_with_matching_expected() {
    let state = Arc::new(MemorySharedState::new());
    let agent_id = AgentId::new("test_agent".to_string());

    // Set initial value
    let value1 = SharedValue::new(agent_id.clone(), serde_json::json!("v1"));
    state.set("key".to_string(), value1).await.unwrap();

    // Get current value
    let current = state.get("key").await.unwrap().unwrap();

    // Update with matching expected
    let value2 = SharedValue::new(agent_id.clone(), serde_json::json!("v2"));
    let result = state
        .cas("key".to_string(), Some(current), value2)
        .await
        .unwrap();

    assert!(result);

    // Verify value changed
    let updated = state.get("key").await.unwrap().unwrap();
    assert_eq!(updated.data, serde_json::json!("v2"));
    assert_eq!(updated.version, 2);
}

/// Test: Peek returns pending message count.
#[tokio::test]
async fn peek_returns_pending_count() {
    let channel = Arc::new(MemoryMessageChannel::new());
    let agent_a = AgentId::new("agent_a".to_string());
    let agent_b = AgentId::new("agent_b".to_string());

    channel.register(&agent_a).await.unwrap();
    channel.register(&agent_b).await.unwrap();

    // Send messages
    for _ in 0..3 {
        let msg = AgentMessage::notification(
            agent_a.clone(),
            agent_b.clone(),
            MessagePayload::text("test"),
        );
        channel.send(msg).await.unwrap();
    }

    // Peek should return 3
    let count = channel.peek(&agent_b).await.unwrap();
    assert_eq!(count, 3);
}

/// Test: Clear removes all pending messages.
#[tokio::test]
async fn clear_removes_pending_messages() {
    let channel = Arc::new(MemoryMessageChannel::new());
    let agent_a = AgentId::new("agent_a".to_string());
    let agent_b = AgentId::new("agent_b".to_string());

    channel.register(&agent_a).await.unwrap();
    channel.register(&agent_b).await.unwrap();

    // Send messages
    for _ in 0..3 {
        let msg = AgentMessage::notification(
            agent_a.clone(),
            agent_b.clone(),
            MessagePayload::text("test"),
        );
        channel.send(msg).await.unwrap();
    }

    assert_eq!(channel.peek(&agent_b).await.unwrap(), 3);

    // Clear
    channel.clear(&agent_b).await.unwrap();

    assert_eq!(channel.peek(&agent_b).await.unwrap(), 0);
}

/// Test: Unregister removes agent from channel.
#[tokio::test]
async fn unregister_removes_agent() {
    let channel = Arc::new(MemoryMessageChannel::new());
    let agent_a = AgentId::new("agent_a".to_string());
    let agent_b = AgentId::new("agent_b".to_string());

    channel.register(&agent_a).await.unwrap();
    channel.register(&agent_b).await.unwrap();

    assert_eq!(channel.agent_count().await, 2);

    // Unregister agent_b
    let removed = channel.unregister(&agent_b).await.unwrap();
    assert!(removed);
    assert_eq!(channel.agent_count().await, 1);

    // Cannot unregister again
    let removed_again = channel.unregister(&agent_b).await.unwrap();
    assert!(!removed_again);
}
