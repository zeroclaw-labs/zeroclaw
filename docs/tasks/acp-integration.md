# Feature Plan: Agent Client Protocol (ACP) Integration

This document outlines the architectural changes and implementation tasks to provide ZeroClaw with a standard Agent Client Protocol (ACP) interface, achieving parity with professional tools like Kimi Code CLI and allowing ZeroClaw to be used natively in editors like Zed and JetBrains.

## Epic Overview

- **User Value**: Enables ZeroClaw to be used as a primary coding assistant directly within compatible IDEs (Zed, IntelliJ, Neovim) without custom plugins.
- **Success Metrics**: 
    - ZeroClaw successfully responds to the ACP `initialize` request.
    - ZeroClaw can handle a full conversation loop via the `session/prompt` ACP method.
    - Standard JSON-RPC 2.0 transport over both WebSockets and `stdio`.
- **Scope**: Implementing a new `src/gateway/acp/` module for protocol handling and integrating it into the `gateway` and `main` entry points.
- **Constraints**: Must follow the official [ACP Specification](https://agentclientprotocol.com).

## Architecture Decisions

### ADR 011: JSON-RPC 2.0 over Multiple Transports
- **Context**: Local editors (like Zed) often prefer `stdio` for process-based agents, while remote setups prefer WebSockets.
- **Decision**: Implement a transport-agnostic ACP handler that can be driven by either a WebSocket stream or a `stdio` reader/writer.
- **Rationale**: Maximizes flexibility for both local and cloud-hosted ZeroClaw instances.
- **Patterns Applied**: Strategy Pattern, Codec.

### ADR 012: Map ACP Sessions to ZeroClaw ConversationManager
- **Context**: ACP defines its own session lifecycle (`session/new`, `session/prompt`).
- **Decision**: Directly map ACP `session_id` to ZeroClaw's `ConversationManager` session management.
- **Rationale**: Reuses the robust persistence and auto-compaction logic we just built.

## Story Breakdown

### Story 1: Protocol Foundation [1 week]
Define the message types and basic JSON-RPC server.

#### Acceptance Criteria
- [ ] Rust structs for all major ACP messages (Request, Response, Notification).
- [ ] A JSON-RPC 2.0 codec that parses messages from a stream.

#### Atomic Tasks
- **Task 1.1: Define ACP Message Types [3h]**
    - Objective: Create `src/gateway/acp/types.rs` with Pydantic-like (Serde) structs for `Initialize`, `SessionNew`, `SessionPrompt`.
    - Context: `src/gateway/acp/mod.rs`.
- **Task 1.2: JSON-RPC Dispatcher [4h]**
    - Objective: Implement a router that matches incoming method strings to Rust handler functions.
    - Context: `src/gateway/acp/server.rs`.

### Story 2: Gateway & Stdio Integration [1 week]
Expose the ACP interface to the outside world.

#### Acceptance Criteria
- [ ] `zeroclaw acp` command launches the agent in `stdio` protocol mode.
- [ ] `/ws/acp` endpoint is available in the gateway.

#### Atomic Tasks
- **Task 2.1: WebSocket ACP Handler [3h]**
    - Objective: Add a new Axum WebSocket handler that pipes messages into the ACP dispatcher.
    - Context: `src/gateway/ws.rs`.
- **Task 2.2: Stdio CLI Command [2h]**
    - Objective: Add an `acp` subcommand to `main.rs` that reads from stdin and writes to stdout using the JSON-RPC codec.
    - Context: `src/main.rs`.

### Story 3: Agent Loop Mapping [3 days]
Connect the protocol to the "Brain."

#### Acceptance Criteria
- [ ] `session/prompt` triggers a `run_tool_call_loop` turn and streams partial results back via ACP notifications.

#### Atomic Tasks
- **Task 3.1: ACP Agent Bridge [4h]**
    - Objective: Map `SessionPrompt` arguments to ZeroClaw's `agent_turn` and handle streaming via ACP `session/append` notifications.
    - Context: `src/gateway/acp/bridge.rs`.

## Known Issues
- **Concurrency**: ACP expects a stateful session. Need to ensure `ConversationManager` is correctly locked during multi-turn bursts.

## Context Preparation Guide
- **Files to load**: `src/gateway/mod.rs`, `src/gateway/ws.rs`, `src/agent/loop_.rs`.
- **Concepts**: JSON-RPC 2.0, Async Streams, Token-based Authentication.
