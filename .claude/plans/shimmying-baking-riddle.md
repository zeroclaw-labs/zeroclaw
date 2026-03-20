# Mem0 Integration: Dual-Scope Recall + Per-Turn Memory

## Context

Mem0 auto-save works but the integration is missing key features from mem0 best practices: per-turn recall, multi-level scoping, and proper context injection. This causes the bot to "forget" on follow-up turns and not differentiate users.

## What's Missing (vs mem0 docs)

1. **Per-turn recall** — only first turn gets memory context, follow-ups get nothing
2. **Dual-scope** — no sender vs group distinction. All memories use single hardcoded `user_id`
3. **System prompt injection** — memory prepended to user message (pollutes session history)
4. **`agent_id` scoping** — mem0 supports agent-level patterns, not used

## Changes

### 1. `src/memory/mem0.rs` — Use session_id for multi-level scoping

Map zeroclaw's `session_id` param to mem0's `user_id`. This enables per-user and per-group memory namespaces without changing the `Memory` trait.

```rust
// Add helper:
fn effective_user_id(&self, session_id: Option<&str>) -> &str {
    session_id.filter(|s| !s.is_empty()).unwrap_or(&self.user_id)
}

// In store(): use effective_user_id(session_id) as mem0 user_id
// In recall(): use effective_user_id(session_id) as mem0 user_id
// In list(): use effective_user_id(session_id) as mem0 user_id
```

### 2. `src/channels/mod.rs` ~line 2229 — Per-turn dual-scope recall

Remove `if !had_prior_history` gate. Always recall from both sender scope and group scope (for group chats).

```rust
// Detect group chat
let is_group = msg.reply_target.contains("@g.us")
    || msg.reply_target.starts_with("group:");

// Sender-scope recall (always)
let sender_context = build_memory_context(
    ctx.memory.as_ref(), &msg.content, ctx.min_relevance_score,
    Some(&msg.sender),
).await;

// Group-scope recall (groups only)
let group_context = if is_group {
    build_memory_context(
        ctx.memory.as_ref(), &msg.content, ctx.min_relevance_score,
        Some(&history_key),
    ).await
} else {
    String::new()
};

// Merge (deduplicate by checking substring overlap)
let memory_context = merge_memory_contexts(&sender_context, &group_context);
```

### 3. `src/channels/mod.rs` ~line 2244 — Inject into system prompt

Move memory context from user message to system prompt. Re-fetched each turn, doesn't pollute session.

```rust
let mut system_prompt = build_channel_system_prompt(...);
if !memory_context.is_empty() {
    system_prompt.push_str(&format!("\n\n{memory_context}"));
}
let mut history = vec![ChatMessage::system(system_prompt)];
```

### 4. `src/channels/mod.rs` — Dual-scope auto-save

Find existing auto-save call. For group messages, store twice:
- `store(key, content, category, Some(&msg.sender))` — personal facts
- `store(key, content, category, Some(&history_key))` — group context

Both async, non-blocking. DMs only store to sender scope.

### 5. `src/memory/mem0.rs` — Add `agent_id` support (optional)

Pass `self.app_name` as `agent_id` param to mem0 API for agent behavior tracking.

## Files to Modify

1. `src/memory/mem0.rs` — session_id → user_id mapping
2. `src/channels/mod.rs` — per-turn recall, dual-scope, system prompt injection, dual-scope save

## Verification

1. `cargo check --features whatsapp-web,memory-mem0`
2. `cargo test --features whatsapp-web,memory-mem0`
3. Deploy to Synology
4. Test DM: "我鍾意食壽司" → next turn "我鍾意食咩" → should recall
5. Test group: Joe says "我鍾意食壽司" → someone else asks "Joe 鍾意食咩" → should recall from group scope
6. Check mem0 server logs: GET with `user_id=sender` AND `user_id=group_key`
7. Check mem0 server logs: POST with both user_ids for group messages
