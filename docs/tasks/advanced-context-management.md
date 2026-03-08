# Feature Plan: Advanced Context Management

This document outlines the architectural changes and implementation tasks to enhance ZeroClaw's context management, bringing it to parity with OpenClaw's capabilities.

## Epic Overview

- **User Value**: Prevents LLM context window overflow errors and "JavaScript heap out of memory" crashes by intelligently summarizing and truncating large conversation histories and tool results before they break the agent loop.
- **Success Metrics**: 
    - 0 OOM crashes during long sessions.
    - 100% recovery from "context window exceeded" API errors.
    - Preserves metadata of truncated tool results so the LLM knows what happened.
- **Scope**: Enhancing `ConversationManager` in `src/agent/conversation.rs` to handle token budget tracking and tool result truncation.
- **Constraints**: Must maintain Rust's high-performance characteristics.

## Architecture Decisions

### ADR 003: Token Budget Tracking
- **Context**: The agent currently only compresses history when the *number of messages* exceeds a hardcoded limit (e.g., 50). This fails if a single message (like a large `cat` output) consumes the entire context window.
- **Decision**: Implement token budget tracking within `ConversationManager`. We will estimate tokens using a fast heuristic (e.g., chars / 4) and trigger compaction when the total budget nears the limit.
- **Rationale**: Proactive compaction prevents API rejection and CLI memory crashes.
- **Patterns Applied**: Defensive Programming, Token-Aware Routing.

### ADR 004: Tool Result Truncation
- **Context**: Sometimes a tool (like `shell` or `file_read`) returns massive output. Summarizing the entire conversation is overkill if the problem is just one giant log dump.
- **Decision**: Implement `truncate_oversized_tool_results` in `ConversationManager`. If a tool result exceeds a threshold, replace it with `[Output truncated due to size: N bytes omitted. Use grep or head to read specific parts.]` while keeping the `tool_call_id` intact.
- **Rationale**: Mirrors OpenClaw's `sessionLikelyHasOversizedToolResults` logic. Keeps the LLM aware that the tool ran successfully without choking its context window.
- **Patterns Applied**: Graceful Degradation.

## Story Breakdown

### Story 1: Advanced Context Management [3 days]
Enhance the existing `ConversationManager` to proactively manage token budgets and massive tool outputs.

#### Acceptance Criteria
- [x] `ConversationManager` tracks estimated token usage.
- [x] `auto_compact` triggers based on token count, not just message count.
- [x] Oversized tool results are automatically truncated before hitting the LLM.

#### Atomic Tasks
- **Task 1.1: Implement Token Budgeting [1h]** ✅ Completed
    - Objective: Add `estimated_tokens()` method to `ChatMessage` and track total in `ConversationManager`.
    - Context Boundary: `src/providers/traits.rs`, `src/agent/conversation.rs`.
    - Validation: Unit tests confirming `chars / 4` estimation logic.
- **Task 1.2: Tool Result Truncation [2h]** ✅ Completed
    - Objective: Implement `truncate_oversized_tool_results()` in `ConversationManager`.
    - Context Boundary: `src/agent/conversation.rs`.
    - Validation: Unit tests showing large `[Tool results]` are replaced with a concise summary.
- **Task 1.3: Token-Triggered Auto-Compaction [1h]** ✅ Completed
    - Objective: Update `auto_compact()` to trigger when `estimated_tokens > max_tokens * 0.8`.
    - Context Boundary: `src/agent/conversation.rs`, `src/agent/loop_.rs`.
    - Validation: Integration tests proving compaction fires before OOM.

## Known Issues

### 🐛 Logic Risk: Over-truncation [SEVERITY: Medium]
- **Description**: Truncating tool results might hide crucial error messages at the end of a long log.
- **Mitigation**: Truncate from the middle, keeping the first N and last M characters.
- **Files Affected**: `src/agent/conversation.rs`.

## Context Preparation Guide

### Task 1.1 & 1.2 & 1.3
- **Files to load**: `src/agent/conversation.rs`, `src/providers/traits.rs`, `src/agent/loop_.rs`.
- **Concepts**: Rust String manipulation, Token counting heuristics.
