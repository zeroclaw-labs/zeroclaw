# Feature Plan: Distributed Node Registry

This document outlines the architectural changes and implementation tasks to enable ZeroClaw to manage and dispatch tasks to remote "Nodes" (e.g., Raspberry Pis, remote desktops), achieving parity with OpenClaw's distributed execution model.

## Epic Overview

- **User Value**: Allows ZeroClaw to perform physical actions or resource-intensive tasks on remote hardware (like sensors on a RPi or a browser on a powerful desktop) while maintaining a central control plane.
- **Success Metrics**: 
    - Remote nodes can register via WebSockets.
    - ZeroClaw's gateway maintains a live registry of connected nodes and their capabilities.
    - Agents can dispatch tool calls to specific remote nodes.
- **Scope**: Implementing a `NodeRegistry` in the gateway, a WebSocket endpoint for node registration, and a `RemoteTool` adapter.
- **Constraints**: Must be secure (authenticated) and handle intermittent connectivity.

## Architecture Decisions

### ADR 005: WebSocket-Based Node Registration
- **Context**: We need a real-time, bidirectional connection between the central ZeroClaw gateway and remote nodes.
- **Decision**: Use persistent WebSockets for node registration and command dispatch.
- **Rationale**: Low latency, handles NAT traversal (outbound from node to gateway), and allows streaming results (logs/images) back to the gateway.
- **Patterns Applied**: Proxy Pattern, Observer Pattern.

### ADR 006: Capability-Based Dispatching
- **Context**: Different nodes have different hardware/software (e.g., Node A has a camera, Node B has a high-end GPU).
- **Decision**: Nodes announce a list of `capabilities` (e.g., `["camera", "shell", "browser"]`) upon connection. ZeroClaw's dispatcher selects the appropriate node based on the requested tool.
- **Rationale**: Dynamic and flexible; allows the registry to scale without central configuration of every node.

## Story Breakdown

### Story 1: WebSocket Node Registry [1 week]
Build the foundation for remote nodes to connect and stay registered.

#### Acceptance Criteria
- [ ] Gateway exposes `/ws/register-node`.
- [ ] Nodes can connect with a unique `node_id` and a list of `capabilities`.
- [ ] Gateway maintains a thread-safe map of active `NodeSessions`.

#### Atomic Tasks
- **Task 1.1: Define Node Protocol & Registry Struct [2h]**
    - Objective: Create `src/gateway/node_registry.rs` with `NodeSession` and `NodeRegistry` structs.
    - Context: `src/gateway/mod.rs`, `src/gateway/node_registry.rs`.
- **Task 1.2: Implement Registration WebSocket [3h]**
    - Objective: Add the registration handler to `src/gateway/ws.rs` and route it in `src/gateway/mod.rs`.
    - Context: `src/gateway/ws.rs`.

### Story 2: Remote Tool Dispatching [1 week]
Enable the agent loop to send commands to remote nodes.

#### Acceptance Criteria
- [ ] A `RemoteTool` trait implementation that wraps WebSocket communication.
- [ ] The agent loop can "see" tools provided by remote nodes.
- [ ] Command execution results are successfully returned from the node to the agent.

#### Atomic Tasks
- **Task 2.1: Implement RemoteTool Adapter [4h]**
    - Objective: Create a `Tool` implementation that sends a JSON request over a `NodeSession` and waits for the `ToolResult` message.
    - Context: `src/tools/traits.rs`, `src/gateway/node_registry.rs`.

## Known Issues
- **Intermittent Connectivity**: Remote nodes might disconnect mid-task. Need robust retry/timeout logic in the proxy.

## Context Preparation Guide
- **Files to load**: `src/gateway/ws.rs`, `src/gateway/mod.rs`, `src/tools/traits.rs`.
- **Concepts**: Axum WebSockets, Shared State (Arc/Mutex), Proxy Pattern.
