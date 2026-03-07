# Feature Plan: Agent Stability and Persistence

This document outlines the architectural changes and implementation tasks to enhance ZeroClaw's stability, persistence, and long-term guidance capabilities.

## Epic Overview

- **User Value**: Ensures the agent never "forgets" its goals or recent progress due to rate limits, API errors, or system crashes. Provides a reliable, professional experience similar to mature competitors like OpenClaw.
- **Success Metrics**: 
    - 0% "resetting" behavior after recoverable provider errors.
    - Persistent history across sessions (if configured).
    - Guaranteed adherence to "Anchored Guidance" even in long-context scenarios.
- **Scope**: Refactoring `src/agent/loop_.rs` and `src/agent/agent.rs`, introducing a `ConversationManager`, and implementing eager checkpointing.
- **Constraints**: Must maintain Rust's high-performance characteristics (<10ms overhead for persistence) and security standards (scrubbing).

## Architecture Decisions

### ADR 001: Unified Conversation Manager
- **Context**: Current history management is procedural, scattered across `loop_.rs`, and duplicated in `agent.rs`. This makes robust persistence difficult.
- **Decision**: Introduce a `ConversationManager` struct to encapsulate `Vec<ChatMessage>`, `Memory` backend interaction, and history compaction.
- **Rationale**: Centralizing history logic enables turn-by-turn checkpointing and consistent "Guidance Anchoring" across all agent entry points.
- **Consequences**: Improves testability of history logic but requires a significant refactor of the massive `loop_.rs` file.
- **Patterns Applied**: Single Responsibility Principle (SRP), Encapsulation.

### ADR 002: Eager Turn-by-Turn Checkpointing
- **Context**: Agent amnesia occurs because state is only saved at the end of a successful loop. Errors cause the loop to exit and transient history to be lost.
- **Decision**: Implement a `.checkpoint()` mechanism that writes to the `Memory` backend immediately after every provider response and tool execution.
- **Rationale**: Mimics the "Lobster-Tank" robustness of OpenClaw while leveraging ZeroClaw's high-performance SQLite/Markdown backends.
- **Consequences**: Slight increase in I/O operations, mitigated by ZeroClaw's efficient async I/O.
- **Patterns Applied**: Fail-Safe Design, Durability.

## Story Breakdown

### Story 1: Architectural Unification [1 week]
Refactor the dueling agent loops into a single cohesive structure powered by a `ConversationManager`.

#### Acceptance Criteria
- [x] `ConversationManager` created and tested in isolation.
- [x] `src/agent/loop_.rs` reduced in size by offloading history logic.
- [x] `Agent::turn` and `run_tool_call_loop` share the same underlying logic.

#### Atomic Tasks
- **Task 1.1: Create ConversationManager [2h]** ✅ Completed
    - Objective: Implement `src/agent/conversation.rs` with history trimming and compaction.
    - Context Boundary: `src/agent/conversation.rs`, `src/agent/mod.rs`.
    - Validation: Unit tests for `trim_history` and `auto_compact_history` moved to the new struct.
- **Task 1.2: Refactor loop_.rs to use ConversationManager [4h]** ✅ Completed
    - Objective: Replace raw `Vec<ChatMessage>` with `ConversationManager` in `run_tool_call_loop`.
    - Context Boundary: `src/agent/loop_.rs`, `src/agent/conversation.rs`.
    - Validation: Existing integration tests pass with the new abstraction.

### Story 2: Robust Persistence & Recovery [1 week]
Implement the eager checkpointing logic to eliminate agent amnesia.

#### Acceptance Criteria
- [ ] Agent recovers progress after a mock rate limit error.
- [ ] Initial greeting is only shown for truly new conversations.
- [ ] Credentials remain scrubbed in persistent storage.

#### Atomic Tasks
- **Task 2.1: Implement Eager Checkpointing [3h]**
    - Objective: Add `.checkpoint()` calls to `run_tool_call_loop` after each major state change.
    - Context Boundary: `src/agent/loop_.rs`, `src/agent/conversation.rs`.
    - Validation: Verify SQLite entries are created even if the loop is aborted mid-way.
- **Task 2.2: Conditional Greeting Logic [1h]**
    - Objective: Modify startup to check `ConversationManager::is_empty()` before printing the 🦀 intro.
    - Context Boundary: `src/agent/loop_.rs`, `src/agent/agent.rs`.
    - Validation: No intro shown on retry of a failed turn.

### Story 3: Anchored Guidance [3 days]
Ensure long-term goals are never lost in long contexts.

#### Acceptance Criteria
- [ ] Core instructions from `AGENTS.md` are always present in the context.
- [ ] Summary compaction preserves the user's primary objective.

#### Atomic Tasks
- **Task 3.1: Guidance Anchoring in Prompt Builder [2h]**
    - Objective: Update `src/agent/prompt.rs` to ensure "Identity" files are anchored to the end of the context if needed.
    - Context Boundary: `src/agent/prompt.rs`, `src/agent/conversation.rs`.
    - Validation: Verify model adherence in long-context simulation tests.

## Known Issues

### 🐛 Performance Risk: Persistence Latency [SEVERITY: Low]
- **Description**: Frequent SQLite writes might slow down the loop.
- **Mitigation**: Use async task spawning for checkpointing or batch writes for tool-heavy bursts.
- **Files Affected**: `src/agent/conversation.rs`.

### 🐛 Complexity Risk: Circular Dependencies [SEVERITY: Medium]
- **Description**: `ConversationManager` might need `Provider` for compaction, while `Provider` is used by the loop.
- **Mitigation**: Use dependency injection or trait-based decoupling for the summarization engine.
- **Files Affected**: `src/agent/mod.rs`, `src/agent/conversation.rs`.

## Dependency Visualization
```
Task 1.1 (ConversationManager) ──► Task 1.2 (Refactor Loop) ──► Task 2.1 (Checkpointing)
                                                                    │
                                                                    ▼
Task 3.1 (Guidance Anchoring) ◄────────────────────────────── Task 2.2 (Conditional Greeting)
```

## Context Preparation Guide

### Task 1.1
- **Files to load**: `src/agent/loop_.rs` (history logic), `src/memory/traits.rs`.
- **Concepts**: SRP, Rust memory management (Ownership/Borrowing).

## Success Criteria
- ✅ All atomic tasks completed and validated.
- ✅ 0% "amnesia" observed in manual rate-limit testing.
- ✅ `src/agent/loop_.rs` size reduced by at least 20%.
- ✅ Documentation updated in `docs/architecture/persistence.md`.
