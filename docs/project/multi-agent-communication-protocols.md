# Multi-Agent Communication Protocols

**Status:** Design Proposal
**Author:** rust-dev
**Date:** 2026-02-26
**Related:**
- [File-Based Multi-Agent Architecture](multi-agent-file-based-architecture.md)
- [Multi-Agent Phase 1 Design](multi-agent-phase1.md)

---

## Executive Summary

This document defines comprehensive communication protocols for ZeroClaw's multi-agent system. The design supports both the existing file-based agent architecture and future in-process coordination patterns.

### Design Goals

1. **Message Format Standardization**: Consistent envelope-based messaging
2. **Flexible Routing**: Direct, broadcast, and pub/sub patterns
3. **Synchronous + Asynchronous**: Support both blocking and non-blocking communication
4. **Event-Driven Coordination**: Agents can react to system events
5. **State Sharing**: Safe shared memory and distributed state patterns
6. **Backward Compatible**: Extends existing `DelegateTool` and `AgentRegistry`

---

## 1. Message Format Specification

### 1.1 Base Message Envelope

```rust
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use chrono::{DateTime, Utc};
use uuid::Uuid;

/// Base message envelope for all agent communication
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentMessage {
    /// Unique message identifier
    pub id: Uuid,

    /// Message type (determines payload interpretation)
    pub r#type: MessageType,

    /// Source agent ID
    pub from: AgentId,

    /// Destination agent ID (None for broadcast)
    pub to: Option<AgentId>,

    /// Message timestamp
    pub timestamp: DateTime<Utc>,

    /// Correlation ID for request-response patterns
    #[serde(skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<Uuid>,

    /// Message payload
    pub payload: Payload,

    /// Message metadata (routing hints, priority, etc.)
    #[serde(default)]
    pub metadata: Metadata,

    /// Security context (signature, encryption info)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub security: Option<SecurityContext>,
}

/// Message type identifier
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum MessageType {
    // Request patterns
    TaskRequest,
    QueryRequest,
    StreamRequest,

    // Response patterns
    TaskResponse,
    QueryResponse,
    StreamChunk,
    StreamEnd,

    // Event patterns
    EventPublished,
    EventNotification,

    // Coordination patterns
    Heartbeat,
    StatusUpdate,
    ErrorReport,
    Handshake,

    // Custom extension
    #[serde(untagged)]
    Custom(String),
}

/// Agent identifier (can be a process ID, thread ID, or logical name)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum AgentId {
    /// Logical agent name (e.g., "researcher")
    Logical(String),
    /// Process ID
    Process(u32),
    /// Thread ID
    Thread(Uuid),
    /// Remote agent (host:port)
    Remote(String),
}

/// Message payload (variant based on message type)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Payload {
    // Task execution
    TaskRequest(TaskRequestPayload),
    TaskResponse(TaskResponsePayload),

    // Query/Response
    QueryRequest(QueryRequestPayload),
    QueryResponse(QueryResponsePayload),

    // Streaming
    StreamRequest(StreamRequestPayload),
    StreamChunk(StreamChunkPayload),

    // Events
    Event(EventPayload),

    // Coordination
    Heartbeat(HeartbeatPayload),
    StatusUpdate(StatusUpdatePayload),
    ErrorReport(ErrorReportPayload),

    // Raw JSON for extensibility
    Raw(serde_json::Value),
}

/// Message metadata for routing and processing
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Metadata {
    /// Message priority (0-255, higher = more important)
    #[serde(default)]
    pub priority: u8,

    /// Time-to-live in seconds (None = no expiry)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ttl_seconds: Option<u64>,

    /// Required delivery level
    #[serde(default)]
    pub delivery: DeliveryLevel,

    /// Routing hints for broker/mediator
    #[serde(default)]
    pub routing: RoutingHints,

    /// User-defined key-value pairs
    #[serde(default)]
    pub labels: HashMap<String, String>,

    /// Request timeout in milliseconds
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
}

/// Delivery reliability guarantees
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryLevel {
    /// Fire-and-forget
    #[default]
    AtMostOnce,
    /// Guaranteed delivery but may duplicate
    AtLeastOnce,
    /// Exactly once delivery
    ExactlyOnce,
}

/// Routing hints for message brokers
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RoutingHints {
    /// Request specific routing key
    #[serde(skip_serializing_if = "Option::is_none")]
    pub routing_key: Option<String>,

    /// Request specific queue/topic
    #[serde(skip_serializing_if = "Option::is_none")]
    pub topic: Option<String>,

    /// Request persistent storage
    #[serde(default)]
    pub persistent: bool,
}
```

### 1.2 Task Request Payload

```rust
/// Payload for task delegation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskRequestPayload {
    /// Human-readable task description
    pub prompt: String,

    /// Structured input data
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input: Option<serde_json::Value>,

    /// Execution context (variables, previous results)
    #[serde(default)]
    pub context: HashMap<String, serde_json::Value>,

    /// Execution constraints
    #[serde(default)]
    pub constraints: ExecutionConstraints,

    /// Callback information (for async responses)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub callback: Option<CallbackInfo>,
}

/// Execution constraints for a task
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ExecutionConstraints {
    /// Maximum execution time
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_seconds: Option<u64>,

    /// Maximum iterations (for agentic mode)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_iterations: Option<usize>,

    /// Allowed tools (whitelist)
    #[serde(default)]
    pub allowed_tools: Vec<String>,

    /// Denied tools (blacklist)
    #[serde(default)]
    pub denied_tools: Vec<String>,

    /// Resource limits
    #[serde(default)]
    pub resources: ResourceLimits,
}

/// Resource consumption limits
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ResourceLimits {
    /// Maximum memory in MB
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_memory_mb: Option<u64>,

    /// Maximum CPU cores (0 = no limit)
    #[serde(default)]
    pub max_cpu_cores: u32,

    /// Maximum network bandwidth in Mbps
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_bandwidth_mbps: Option<u64>,
}

/// Callback information for async responses
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CallbackInfo {
    /// Callback URL
    pub url: String,

    /// Callback method
    #[serde(default = "default_http_method")]
    pub method: HttpMethod,

    /// Authentication header
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auth_header: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum HttpMethod {
    Get,
    Post,
    Put,
    Patch,
}

fn default_http_method() -> HttpMethod {
    HttpMethod::Post
}
```

### 1.3 Task Response Payload

```rust
/// Payload for task completion response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskResponsePayload {
    /// Task status
    pub status: TaskStatus,

    /// Human-readable output
    pub output: String,

    /// Structured result data
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,

    /// Error details if failed
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorDetails>,

    /// Execution metrics
    #[serde(default)]
    pub metrics: ExecutionMetrics,

    /// Artifacts produced
    #[serde(default)]
    pub artifacts: Vec<Artifact>,
}

/// Task completion status
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    /// Task completed successfully
    Success,
    /// Task failed with error
    Failed,
    /// Task timed out
    Timeout,
    /// Task was cancelled
    Cancelled,
    /// Task still in progress (for async updates)
    InProgress,
}

/// Error details for failed tasks
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorDetails {
    /// Error code (machine-readable)
    pub code: String,

    /// Human-readable message
    pub message: String,

    /// Stack trace (if available)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stack_trace: Option<String>,

    /// Error context
    #[serde(default)]
    pub context: HashMap<String, String>,
}

/// Execution metrics
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ExecutionMetrics {
    /// Execution duration in milliseconds
    #[serde(default)]
    pub duration_ms: u64,

    /// LLM API calls made
    #[serde(default)]
    pub llm_calls: u32,

    /// Tool invocations
    #[serde(default)]
    pub tool_calls: u32,

    /// Input tokens consumed
    #[serde(default)]
    pub tokens_input: u32,

    /// Output tokens consumed
    #[serde(default)]
    pub tokens_output: u32,

    /// Memory consumed in bytes
    #[serde(default)]
    pub memory_bytes: u64,
}

/// Artifact produced by agent execution
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Artifact {
    /// Artifact kind
    pub kind: ArtifactKind,

    /// Artifact reference (file path, URL, etc.)
    pub reference: String,

    /// Human-readable description
    #[serde(default)]
    pub description: String,

    /// Size in bytes
    #[serde(default)]
    pub size: u64,

    /// Artifact checksum for verification
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checksum: Option<String>,
}

/// Artifact type
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactKind {
    File,
    Directory,
    Url,
    Data,
    Model,
    Dataset,
}
```

### 1.4 Event Payload

```rust
/// Event publication payload
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventPayload {
    /// Event type (e.g., "file.changed", "task.completed")
    pub event_type: String,

    /// Event source (agent or system component)
    pub source: String,

    /// Event data
    pub data: serde_json::Value,

    /// Event timestamp
    #[serde(default = "default_now")]
    pub timestamp: DateTime<Utc>,

    /// Event version (for schema evolution)
    #[serde(default)]
    pub version: u32,
}

fn default_now() -> DateTime<Utc> {
    Utc::now()
}
```

---

## 2. Communication Patterns

### 2.1 Pattern Overview

```
┌─────────────────────────────────────────────────────────────────┐
│                    Communication Patterns                       │
├─────────────────────────────────────────────────────────────────┤
│                                                                  │
│  1. Direct (1:1)        ┌──────┐         ┌──────┐             │
│                        │Agent A│────────▶│Agent B│             │
│                        └──────┘         └──────┘             │
│                                                                  │
│  2. Broadcast (1:N)   ┌──────┐                          │
│                 ┌─────▶│Agent A│                            │
│                 │      └──────┘                            │
│             ┌───┴───┴────┐                                │
│             ▼       ▼     ▼                                │
│           ┌───┐   ┌───┐  ┌───┐                            │
│           │B  │   │ C │  │ D │                            │
│           └───┘   └───┘  └───┘                            │
│                                                                  │
│  3. Request-Response ┌──────┐         ┌──────┐              │
│                      │Agent A│◀───────▶│Agent B│              │
│                      └──────┘  Sync   └──────┘              │
│                                                                  │
│  4. Pub/Sub            ┌──────┐   ┌─────────┐                │
│                  ┌─────▶│Agent A│──▶│  Event  │               │
│                  │      └──────┘   │  Broker  │               │
│              ┌───┴───┴────┐        └─────────┘                │
│              ▼       ▼     ▼             │                     │
│            ┌───┐   ┌───┐  ┌───┐          ▼                    │
│            │ B │   │ C │  │ D │     ┌─────────┐               │
│            └───┘   └───┘  └───┘     │Subscribers│             │
│                                    └─────────┘               │
│                                                                  │
│  5. Streaming         ┌──────┐  ~~~~~~~~  ┌──────┐           │
│                      │Agent A│━━━━━━━━▶│Agent B│           │
│                      └──────┘  Chunks   └──────┘           │
│                                                                  │
└─────────────────────────────────────────────────────────────────┘
```

### 2.2 Direct Communication (1:1)

```rust
/// Direct messaging between two agents
pub trait DirectMessaging {
    /// Send a message directly to another agent
    async fn send(&self, message: AgentMessage) -> Result<MessageId>;

    /// Receive next message (blocking)
    async fn receive(&mut self) -> Result<AgentMessage>;

    /// Receive with timeout
    async fn receive_timeout(&mut self, duration: Duration) -> Result<Option<AgentMessage>>;

    /// Send and wait for response (request-response pattern)
    async fn call(&self, message: AgentMessage) -> Result<AgentMessage>;
}

/// In-memory direct message channel (for same-process agents)
pub struct MemoryChannel {
    tx: tokio::sync::mpsc::Sender<AgentMessage>,
    rx: tokio::sync::mpsc::Receiver<AgentMessage>,
    agent_id: AgentId,
}

impl MemoryChannel {
    pub fn new(agent_id: AgentId, buffer_size: usize) -> Self {
        let (tx, rx) = tokio::sync::mpsc::channel(buffer_size);
        Self { tx, rx, agent_id }
    }

    pub fn sender(&self) -> ChannelSender {
        ChannelSender {
            tx: self.tx.clone(),
            from: self.agent_id.clone(),
        }
    }

    pub fn receiver(&mut self) -> ChannelReceiver {
        ChannelReceiver {
            rx: self.rx.swap(tokio::sync::mpsc::channel(1).1),
            agent_id: self.agent_id.clone(),
        }
    }
}

pub struct ChannelSender {
    tx: tokio::sync::mpsc::Sender<AgentMessage>,
    from: AgentId,
}

pub struct ChannelReceiver {
    rx: tokio::sync::mpsc::Receiver<AgentMessage>,
    agent_id: AgentId,
}

#[async_trait]
impl DirectMessaging for ChannelSender {
    async fn send(&self, mut message: AgentMessage) -> Result<MessageId> {
        message.from = self.from.clone();
        self.tx.send(message).await?;
        Ok(MessageId::new())
    }

    async fn receive(&mut self) -> Result<AgentMessage> {
        Err(anyhow::anyhow!("Cannot receive from sender"))
    }

    async fn receive_timeout(&mut self, _duration: Duration) -> Result<Option<AgentMessage>> {
        Err(anyhow::anyhow!("Cannot receive from sender"))
    }

    async fn call(&self, message: AgentMessage) -> Result<AgentMessage> {
        // Implement request-response using correlation ID
        let correlation_id = Uuid::new_v4();
        let mut req = message;
        req.correlation_id = Some(correlation_id);
        self.send(req).await?;

        // Wait for response with matching correlation ID
        // (Implementation requires response tracking)
        todo!("Request-response pattern")
    }
}
```

### 2.3 Broadcast Communication (1:N)

```rust
/// Broadcast messaging (one sender, many receivers)
pub trait BroadcastMessaging {
    /// Subscribe to broadcast messages
    async fn subscribe(&self, agent_id: AgentId) -> Result<BroadcastReceiver>;

    /// Publish to all subscribers
    async fn publish(&self, message: AgentMessage) -> Result<()>;

    /// Unsubscribe from broadcasts
    async fn unsubscribe(&self, agent_id: AgentId) -> Result<()>;
}

/// In-memory broadcast channel
pub struct MemoryBroadcast {
    tx: tokio::sync::broadcast::Sender<AgentMessage>,
    subscribers: Arc<RwLock<HashMap<AgentId, bool>>>,
}

impl MemoryBroadcast {
    pub fn new(capacity: usize) -> Self {
        let (tx, _) = tokio::sync::broadcast::channel(capacity);
        Self {
            tx,
            subscribers: Arc::new(RwLock::new(HashMap::new())),
        }
    }
}

#[async_trait]
impl BroadcastMessaging for MemoryBroadcast {
    async fn subscribe(&self, agent_id: AgentId) -> Result<BroadcastReceiver> {
        let rx = self.tx.subscribe();
        self.subscribers.write().await.insert(agent_id.clone(), true);
        Ok(BroadcastReceiver { rx, agent_id })
    }

    async fn publish(&self, message: AgentMessage) -> Result<()> {
        let count = self.tx.send(message.clone())?;
        if count == 0 {
            tracing::warn!("Broadcast message had no receivers");
        }
        Ok(())
    }

    async fn unsubscribe(&self, agent_id: AgentId) -> Result<()> {
        self.subscribers.write().await.remove(&agent_id);
        Ok(())
    }
}

pub struct BroadcastReceiver {
    rx: tokio::sync::broadcast::Receiver<AgentMessage>,
    agent_id: AgentId,
}

impl BroadcastReceiver {
    pub async fn receive(&mut self) -> Result<AgentMessage> {
        Ok(self.rx.recv().await?)
    }
}
```

### 2.4 Request-Response Pattern

```rust
/// Synchronous request-response with timeout support
pub struct RequestResponse<R> {
    pending: Arc<RwLock<HashMap<Uuid, PendingRequest>>>,
    timeout: Duration,
}

struct PendingRequest {
    response_tx: tokio::sync::oneshot::Sender<AgentMessage>,
    deadline: DateTime<Utc>,
}

impl<R: DirectMessaging + Send + Sync> RequestResponse<R> {
    pub fn new(transport: R, timeout: Duration) -> Self {
        Self {
            pending: Arc::new(RwLock::new(HashMap::new())),
            timeout,
        }
    }

    /// Send request and wait for response
    pub async fn call(&self, mut request: AgentMessage) -> Result<AgentMessage> {
        let correlation_id = Uuid::new_v4();
        request.correlation_id = Some(correlation_id);

        // Create response channel
        let (tx, rx) = tokio::sync::oneshot::channel();
        let deadline = Utc::now() + chrono::Duration::milliseconds(self.timeout.as_millis() as i64);

        // Register pending request
        {
            let mut pending = self.pending.write().await;
            pending.insert(correlation_id, PendingRequest {
                response_tx: tx,
                deadline,
            });
        }

        // Spawn cleanup task
        let pending_clone = self.pending.clone();
        tokio::spawn(async move {
            tokio::time::sleep(self.timeout).await;
            let mut pending = pending_clone.write().await;
            pending.remove(&correlation_id);
        });

        // Send request
        // transport.send(request).await?;

        // Wait for response
        let response = tokio::time::timeout(self.timeout, rx)
            .await
            .map_err(|_| anyhow::anyhow!("Request timed out"))??;

        // Clean up
        self.pending.write().await.remove(&correlation_id);

        Ok(response)
    }

    /// Handle an incoming response (for receivers)
    pub async fn handle_response(&self, response: AgentMessage) -> Result<bool> {
        let correlation_id = response.correlation_id
            .ok_or_else(|| anyhow::anyhow!("Response missing correlation ID"))?;

        let tx = {
            let mut pending = self.pending.write().await;
            pending.remove(&correlation_id)
                .ok_or_else(|| anyhow::anyhow!("Unknown correlation ID"))?
                .response_tx
        };

        tx.send(response)
            .map_err(|_| anyhow::anyhow!("Requester already dropped"))?;

        Ok(true)
    }
}
```

### 2.5 Streaming Pattern

```rust
/// Streaming communication for large responses or continuous data
pub trait StreamingMessaging {
    /// Create a new stream
    async fn create_stream(&self, request: StreamRequestPayload) -> Result<StreamHandle>;

    /// Send a chunk to a stream
    async fn send_chunk(&self, stream_id: Uuid, chunk: StreamChunkPayload) -> Result<()>;

    /// End a stream
    async fn end_stream(&self, stream_id: Uuid, final_result: Option<TaskResponsePayload>) -> Result<()>;

    /// Subscribe to a stream
    async fn subscribe_stream(&self, stream_id: Uuid) -> Result<StreamReceiver>;
}

/// Stream handle for the producer
#[derive(Debug, Clone)]
pub struct StreamHandle {
    pub id: Uuid,
    pub created_at: DateTime<Utc>,
}

/// Stream receiver for the consumer
pub struct StreamReceiver {
    id: Uuid,
    rx: tokio::sync::mpsc::Receiver<StreamMessage>,
}

#[derive(Debug, Clone)]
pub enum StreamMessage {
    Chunk(StreamChunkPayload),
    End(Option<TaskResponsePayload>),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamRequestPayload {
    /// Stream topic/purpose
    pub topic: String,

    /// Initial request data
    #[serde(default)]
    pub initial_data: HashMap<String, serde_json::Value>,

    /// Stream configuration
    #[serde(default)]
    pub config: StreamConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct StreamConfig {
    /// Chunk size in bytes (for streaming large data)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chunk_size_bytes: Option<usize>,

    /// Compression algorithm
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compression: Option<String>,

    /// Include intermediate results
    #[serde(default)]
    pub include_intermediate: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamChunkPayload {
    /// Chunk sequence number
    pub sequence: u64,

    /// Chunk data
    pub data: Vec<u8>,

    /// Is this the final chunk?
    #[serde(default)]
    pub is_final: bool,

    /// Progress (0.0 to 1.0)
    #[serde(default)]
    pub progress: f32,
}

/// In-memory stream implementation
pub struct MemoryStreamBroker {
    streams: Arc<RwLock<HashMap<Uuid, StreamState>>>,
}

struct StreamState {
    tx: tokio::sync::mpsc::Sender<StreamMessage>,
    created_at: DateTime<Utc>,
    closed_at: Option<DateTime<Utc>>,
}

impl MemoryStreamBroker {
    pub fn new() -> Self {
        Self {
            streams: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    async fn create_stream(&self, request: StreamRequestPayload) -> Result<StreamHandle> {
        let id = Uuid::new_v4();
        let (tx, _rx) = tokio::sync::mpsc::channel(100);

        let state = StreamState {
            tx,
            created_at: Utc::now(),
            closed_at: None,
        };

        self.streams.write().await.insert(id, state);

        Ok(StreamHandle {
            id,
            created_at: state.created_at,
        })
    }

    async fn send_chunk(&self, stream_id: Uuid, chunk: StreamChunkPayload) -> Result<()> {
        let streams = self.streams.read().await;
        let state = streams.get(&stream_id)
            .ok_or_else(|| anyhow::anyhow!("Stream not found"))?;

        state.tx.send(StreamMessage::Chunk(chunk))
            .await
            .map_err(|_| anyhow::anyhow!("Stream receiver closed"))?;

        Ok(())
    }

    async fn subscribe_stream(&self, stream_id: Uuid) -> Result<StreamReceiver> {
        let streams = self.streams.read().await;
        let state = streams.get(&stream_id)
            .ok_or_else(|| anyhow::anyhow!("Stream not found"))?;

        Ok(StreamReceiver {
            id: stream_id,
            rx: state.tx.subscribe(),
        })
    }
}
```

---

## 3. Routing Patterns

### 3.1 Message Router

```rust
/// Message routing interface
pub trait MessageRouter: Send + Sync {
    /// Route a message to its destination(s)
    async fn route(&self, message: AgentMessage) -> Result<RouteResult>;

    /// Register an agent as available for routing
    async fn register(&self, agent_id: AgentId, endpoint: Endpoint) -> Result<()>;

    /// Unregister an agent
    async fn unregister(&self, agent_id: AgentId) -> Result<()>;

    /// Get registered agents
    async fn agents(&self) -> Result<Vec<AgentId>>;
}

/// Result of routing a message
#[derive(Debug)]
pub enum RouteResult {
    /// Message routed to single recipient
    Direct(Uuid),

    /// Message broadcast to multiple recipients
    Broadcast(Vec<Uuid>),

    /// Message queued for later delivery
    Queued(Uuid),

    /// Routing failed
    Failed(String),
}

/// Endpoint where an agent can be reached
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Endpoint {
    /// In-memory channel
    Memory {
        channel_id: Uuid,
    },

    /// Unix domain socket
    UnixSocket {
        path: String,
    },

    /// Network endpoint
    Network {
        host: String,
        port: u16,
    },

    /// Process (subprocess)
    Process {
        pid: u32,
        stdin: bool,
    },
}

/// In-memory message router
pub struct MemoryRouter {
    endpoints: Arc<RwLock<HashMap<AgentId, Endpoint>>>,
    channels: Arc<RwLock<HashMap<Uuid, tokio::sync::mpsc::Sender<AgentMessage>>>>,
}

impl MemoryRouter {
    pub fn new() -> Self {
        Self {
            endpoints: Arc::new(RwLock::new(HashMap::new())),
            channels: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Create a channel for an agent
    pub async fn create_channel(&self, agent_id: AgentId) -> Result<ChannelSender> {
        let channel_id = Uuid::new_v4();
        let (tx, rx) = tokio::sync::mpsc::channel(100);

        self.channels.write().await.insert(channel_id, tx);
        self.endpoints.write().await.insert(agent_id.clone(), Endpoint::Memory { channel_id });

        Ok(ChannelSender {
            tx,
            from: agent_id,
        })
    }
}

#[async_trait]
impl MessageRouter for MemoryRouter {
    async fn route(&self, message: AgentMessage) -> Result<RouteResult> {
        let to = message.to.clone()
            .ok_or_else(|| anyhow::anyhow!("Message missing destination"))?;

        let endpoint = {
            let endpoints = self.endpoints.read().await;
            endpoints.get(&to).cloned()
                .ok_or_else(|| anyhow::anyhow!("Agent not registered: {:?}", to))?
        };

        match endpoint {
            Endpoint::Memory { channel_id } => {
                let channels = self.channels.read().await;
                let tx = channels.get(&channel_id)
                    .ok_or_else(|| anyhow::anyhow!("Channel not found"))?;

                tx.send(message).await
                    .map_err(|_| anyhow::anyhow!("Send failed"))?;

                Ok(RouteResult::Direct(channel_id))
            }

            Endpoint::UnixSocket { path } => {
                // Implement Unix socket sending
                todo!("Unix socket routing")
            }

            Endpoint::Network { host, port } => {
                // Implement network sending
                todo!("Network routing")
            }

            Endpoint::Process { pid, stdin } => {
                // Implement process communication
                todo!("Process routing")
            }
        }
    }

    async fn register(&self, agent_id: AgentId, endpoint: Endpoint) -> Result<()> {
        self.endpoints.write().await.insert(agent_id, endpoint);
        Ok(())
    }

    async fn unregister(&self, agent_id: AgentId) -> Result<()> {
        self.endpoints.write().await.remove(&agent_id);
        Ok(())
    }

    async fn agents(&self) -> Result<Vec<AgentId>> {
        Ok(self.endpoints.read().await.keys().cloned().collect())
    }
}
```

### 3.2 Topic-Based Pub/Sub

```rust
/// Topic-based pub/sub messaging
pub trait TopicMessaging: Send + Sync {
    /// Subscribe to a topic
    async fn subscribe(&self, agent_id: AgentId, topic: TopicFilter) -> Result<TopicSubscriber>;

    /// Publish to a topic
    async fn publish(&self, topic: String, message: AgentMessage) -> Result<PublishResult>;

    /// Unsubscribe from a topic
    async fn unsubscribe(&self, agent_id: AgentId, topic: String) -> Result<()>;

    /// List active topics
    async fn topics(&self) -> Result<Vec<String>>;
}

/// Topic filter for subscription
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TopicFilter {
    /// Exact topic match
    Exact(String),

    /// Wildcard match (e.g., "agent.*.status")
    Wildcard(String),

    /// Multiple topics
    Multiple(Vec<String>),
}

/// Result of publishing
#[derive(Debug)]
pub struct PublishResult {
    /// Number of subscribers who received the message
    pub delivered: usize,

    /// IDs of subscribers
    pub subscriber_ids: Vec<AgentId>,
}

/// Topic subscriber
pub struct TopicSubscriber {
    agent_id: AgentId,
    topic: TopicFilter,
    rx: tokio::sync::broadcast::Receiver<AgentMessage>,
}

impl TopicSubscriber {
    pub async fn receive(&mut self) -> Result<AgentMessage> {
        Ok(self.rx.recv().await?)
    }

    pub fn try_receive(&mut self) -> Result<Option<AgentMessage>> {
        Ok(self.rx.try_recv()?)
    }
}

/// In-memory topic broker
pub struct MemoryTopicBroker {
    /// Map of topic -> (broadcast sender, subscribers)
    topics: Arc<RwLock<HashMap<String, TopicState>>>,
}

struct TopicState {
    tx: tokio::sync::broadcast::Sender<AgentMessage>,
    subscribers: HashSet<AgentId>,
}

impl MemoryTopicBroker {
    pub fn new() -> Self {
        Self {
            topics: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    async fn get_or_create_topic(&self, topic: &str) -> TopicState {
        let mut topics = self.topics.write().await;

        if !topics.contains_key(topic) {
            let (tx, _) = tokio::sync::broadcast::channel(100);
            topics.insert(topic.to_string(), TopicState {
                tx,
                subscribers: HashSet::new(),
            });
        }

        topics.get(topic).unwrap().clone()
    }
}

#[async_trait]
impl TopicMessaging for MemoryTopicBroker {
    async fn subscribe(&self, agent_id: AgentId, filter: TopicFilter) -> Result<TopicSubscriber> {
        let topics_to_match = match &filter {
            TopicFilter::Exact(t) => vec![t.clone()],
            TopicFilter::Wildcard(pattern) => {
                // Find matching topics
                let topics = self.topics.read().await;
                topics.keys()
                    .filter(|k| wildcard_match(k, pattern))
                    .cloned()
                    .collect()
            }
            TopicFilter::Multiple(list) => list.clone(),
        };

        // Subscribe to first topic (for simplicity)
        let topic = topics_to_match.first()
            .ok_or_else(|| anyhow::anyhow!("No topics matched filter"))?;

        let state = self.get_or_create_topic(topic).await;
        let rx = state.tx.subscribe();

        {
            let mut topics = self.topics.write().await;
            if let Some(s) = topics.get_mut(topic) {
                s.subscribers.insert(agent_id.clone());
            }
        }

        Ok(TopicSubscriber {
            agent_id,
            topic: filter,
            rx,
        })
    }

    async fn publish(&self, topic: String, message: AgentMessage) -> Result<PublishResult> {
        let state = {
            let topics = self.topics.read().await;
            topics.get(&topic).cloned()
        };

        let state = match state {
            Some(s) => s,
            None => {
                // Create topic on first publish
                self.get_or_create_topic(&topic).await
            }
        };

        let delivered = state.tx.send(message.clone())?;
        let subscriber_ids = state.subscribers.iter().cloned().collect();

        Ok(PublishResult {
            delivered,
            subscriber_ids,
        })
    }

    async fn unsubscribe(&self, agent_id: AgentId, topic: String) -> Result<()> {
        let mut topics = self.topics.write().await;
        if let Some(state) = topics.get_mut(&topic) {
            state.subscribers.remove(&agent_id);
        }
        Ok(())
    }

    async fn topics(&self) -> Result<Vec<String>> {
        Ok(self.topics.read().await.keys().cloned().collect())
    }
}

/// Wildcard pattern matching (supports "*" wildcard)
fn wildcard_match(text: &str, pattern: &str) -> bool {
    let parts: Vec<&str> = pattern.split('*').collect();
    if parts.is_empty() {
        return true;
    }

    let mut idx = 0;
    for (i, part) in parts.iter().enumerate() {
        if part.is_empty() {
            continue;
        }

        if i == 0 && !pattern.starts_with('*') {
            if !text.starts_with(part) {
                return false;
            }
            idx = part.len();
            continue;
        }

        if i == parts.len() - 1 && !pattern.ends_with('*') {
            if !text.ends_with(part) {
                return false;
            }
            continue;
        }

        match text[idx..].find(part) {
            Some(pos) => idx += pos + part.len(),
            None => return false,
        }
    }

    true
}
```

---

## 4. Coordination Mechanisms

### 4.1 Heartbeat Protocol

```rust
/// Heartbeat payload for liveness detection
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeartbeatPayload {
    /// Agent sending heartbeat
    pub agent_id: AgentId,

    /// Heartbeat sequence number
    pub sequence: u64,

    /// Agent status
    pub status: AgentStatusInfo,

    /// Load information
    #[serde(default)]
    pub load: LoadInfo,
}

/// Agent status information
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentStatusInfo {
    /// Agent is idle
    Idle,

    /// Agent is processing a task
    Busy { task_id: Uuid },

    /// Agent is draining (not accepting new tasks)
    Draining,

    /// Agent has encountered an error
    Error { message: String },
}

/// Load information for resource-based routing
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct LoadInfo {
    /// Current memory usage in MB
    #[serde(default)]
    pub memory_mb: u64,

    /// CPU usage percentage (0-100)
    #[serde(default)]
    pub cpu_percent: f32,

    /// Number of active tasks
    #[serde(default)]
    pub active_tasks: u32,

    /// Available tool slots
    #[serde(default)]
    pub available_tools: u32,
}

/// Heartbeat monitor
pub struct HeartbeatMonitor {
    interval: Duration,
    timeout: Duration,
    last_heartbeat: Arc<RwLock<HashMap<AgentId, DateTime<Utc>>>>,
    status: Arc<RwLock<HashMap<AgentId, AgentStatusInfo>>>,
}

impl HeartbeatMonitor {
    pub fn new(interval: Duration, timeout: Duration) -> Self {
        Self {
            interval,
            timeout,
            last_heartbeat: Arc::new(RwLock::new(HashMap::new())),
            status: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Process incoming heartbeat
    pub async fn process_heartbeat(&self, heartbeat: HeartbeatPayload) -> Result<bool> {
        let now = Utc::now();
        let agent_id = heartbeat.agent_id.clone();

        // Update last heartbeat time
        self.last_heartbeat.write().await.insert(agent_id.clone(), now);

        // Update status
        self.status.write().await.insert(agent_id.clone(), heartbeat.status.clone());

        Ok(true)
    }

    /// Check for timed-out agents
    pub async fn check_timeouts(&self) -> Vec<AgentId> {
        let now = Utc::now();
        let last = self.last_heartbeat.read().await;

        last.iter()
            .filter(|(_, last_time)| {
                now.signed_duration_since(**last_time).num_milliseconds() > self.timeout.as_millis() as i64
            })
            .map(|(id, _)| id.clone())
            .collect()
    }

    /// Get agent status
    pub async fn status(&self, agent_id: &AgentId) -> Option<AgentStatusInfo> {
        self.status.read().await.get(agent_id).cloned()
    }

    /// Start heartbeat sender task
    pub fn start_sender_task(
        &self,
        agent_id: AgentId,
        mut tx: impl DirectMessaging,
    ) -> tokio::task::JoinHandle<()> {
        let interval = self.interval;
        let agent = agent_id.clone();

        tokio::spawn(async move {
            let mut sequence = 0u64;
            loop {
                tokio::time::sleep(interval).await;

                let heartbeat = AgentMessage {
                    id: Uuid::new_v4(),
                    r#type: MessageType::Heartbeat,
                    from: agent.clone(),
                    to: None, // Broadcast
                    timestamp: Utc::now(),
                    correlation_id: None,
                    payload: Payload::Heartbeat(HeartbeatPayload {
                        agent_id: agent.clone(),
                        sequence,
                        status: AgentStatusInfo::Idle,
                        load: LoadInfo::default(),
                    }),
                    metadata: Metadata::default(),
                    security: None,
                };

                let _ = tx.send(heartbeat).await;
                sequence += 1;
            }
        })
    }
}
```

### 4.2 Distributed Lock

```rust
/// Distributed lock for coordinating access to shared resources
pub trait DistributedLock: Send + Sync {
    /// Acquire a lock (blocking until acquired or timeout)
    async fn acquire(&self, lock_name: &str, holder: AgentId, timeout: Duration) -> Result<LockGuard>;

    /// Try to acquire a lock (non-blocking)
    async fn try_acquire(&self, lock_name: &str, holder: AgentId) -> Result<Option<LockGuard>>;

    /// Release a lock
    async fn release(&self, lock_name: &str, holder: AgentId) -> Result<bool>;

    /// Extend a lock (renew before expiry)
    async fn extend(&self, lock_name: &str, holder: AgentId, duration: Duration) -> Result<bool>;
}

/// Lock guard (released on drop)
pub struct LockGuard {
    lock_name: String,
    holder: AgentId,
    lock: Arc<dyn DistributedLock>,
    released: Arc<AtomicBool>,
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        let lock_name = self.lock_name.clone();
        let holder = self.holder.clone();
        let lock = self.lock.clone();

        tokio::spawn(async move {
            let _ = lock.release(&lock_name, holder).await;
        });
    }
}

/// In-memory distributed lock (single-process only)
pub struct MemoryLock {
    locks: Arc<RwLock<HashMap<String, LockState>>>,
}

struct LockState {
    holder: AgentId,
    expires_at: DateTime<Utc>,
}

impl MemoryLock {
    pub fn new() -> Self {
        Self {
            locks: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    async fn cleanup_expired(&self) {
        let now = Utc::now();
        let mut locks = self.locks.write().await;

        locks.retain(|_, state| state.expires_at > now);
    }
}

#[async_trait]
impl DistributedLock for MemoryLock {
    async fn acquire(&self, lock_name: &str, holder: AgentId, timeout: Duration) -> Result<LockGuard> {
        let deadline = Utc::now() + chrono::Duration::milliseconds(timeout.as_millis() as i64);

        loop {
            if let Some(guard) = self.try_acquire(lock_name, holder.clone()).await? {
                return Ok(guard);
            }

            if Utc::now() > deadline {
                return Err(anyhow::anyhow!("Lock acquisition timed out"));
            }

            tokio::time::sleep(Duration::from_millis(50)).await;
            self.cleanup_expired().await;
        }
    }

    async fn try_acquire(&self, lock_name: &str, holder: AgentId) -> Result<Option<LockGuard>> {
        self.cleanup_expired().await;

        let mut locks = self.locks.write().await;

        if locks.contains_key(lock_name) {
            return Ok(None);
        }

        let state = LockState {
            holder: holder.clone(),
            expires_at: Utc::now() + chrono::Duration::seconds(30), // Default 30s TTL
        };

        locks.insert(lock_name.to_string(), state);

        Ok(Some(LockGuard {
            lock_name: lock_name.to_string(),
            holder,
            lock: Arc::new(self.clone()),
            released: Arc::new(AtomicBool::new(false)),
        }))
    }

    async fn release(&self, lock_name: &str, holder: AgentId) -> Result<bool> {
        let mut locks = self.locks.write().await;

        match locks.get(lock_name) {
            Some(state) if state.holder == holder => {
                locks.remove(lock_name);
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    async fn extend(&self, lock_name: &str, holder: AgentId, duration: Duration) -> Result<bool> {
        let mut locks = self.locks.write().await;

        match locks.get_mut(lock_name) {
            Some(state) if state.holder == holder => {
                state.expires_at = Utc::now() + chrono::Duration::milliseconds(duration.as_millis() as i64);
                Ok(true)
            }
            _ => Ok(false),
        }
    }
}
```

### 4.3 Leader Election

```rust
/// Leader election for coordinator selection
pub trait LeaderElection: Send + Sync {
    /// Participate in election for a given election ID
    async fn campaign(&self, election_id: &str, candidate: AgentId) -> Result<bool>;

    /// Check if this agent is the leader
    async fn is_leader(&self, election_id: &str, candidate: &AgentId) -> bool;

    /// Resign from leadership
    async fn resign(&self, election_id: &str, candidate: &AgentId) -> Result<bool>;

    /// Get current leader
    async fn get_leader(&self, election_id: &str) -> Option<AgentId>;

    /// Watch for leader changes
    async fn watch_leader(&self, election_id: &str) -> tokio::sync::mpsc::Receiver<LeaderChange>;
}

/// Leader change event
#[derive(Debug, Clone)]
pub struct LeaderChange {
    pub election_id: String,
    pub old_leader: Option<AgentId>,
    pub new_leader: AgentId,
}

/// In-memory leader election (single-process)
pub struct MemoryLeaderElection {
    elections: Arc<RwLock<HashMap<String, ElectionState>>>,
    watchers: Arc<Mutex<HashMap<String, Vec<tokio::sync::mpsc::Sender<LeaderChange>>>>>,
}

struct ElectionState {
    leader: AgentId,
    term: u64,
    expires_at: DateTime<Utc>,
}

impl MemoryLeaderElection {
    pub fn new() -> Self {
        Self {
            elections: Arc::new(RwLock::new(HashMap::new())),
            watchers: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    async fn notify_leader_change(&self, election_id: &str, old_leader: Option<AgentId>, new_leader: AgentId) {
        let watchers = self.watchers.lock().unwrap();
        if let Some(senders) = watchers.get(election_id) {
            let change = LeaderChange {
                election_id: election_id.to_string(),
                old_leader,
                new_leader,
            };

            for sender in senders {
                let _ = sender.try_send(change.clone());
            }
        }
    }
}

#[async_trait]
impl LeaderElection for MemoryLeaderElection {
    async fn campaign(&self, election_id: &str, candidate: AgentId) -> Result<bool> {
        let mut elections = self.elections.write().await;

        let state = elections.entry(election_id.to_string()).or_insert(ElectionState {
            leader: AgentId::Logical("none".to_string()),
            term: 0,
            expires_at: Utc::now(),
        });

        // Check if current leadership is expired
        if state.expires_at > Utc::now() && state.leader != candidate {
            return Ok(false);
        }

        // Become leader
        let old_leader = Some(state.leader.clone());
        state.leader = candidate.clone();
        state.term += 1;
        state.expires_at = Utc::now() + chrono::Duration::seconds(10); // 10s TTL

        drop(elections);
        self.notify_leader_change(election_id, old_leader, candidate).await;

        Ok(true)
    }

    async fn is_leader(&self, election_id: &str, candidate: &AgentId) -> bool {
        let elections = self.elections.read().await;

        elections
            .get(election_id)
            .map(|state| state.leader == *candidate && state.expires_at > Utc::now())
            .unwrap_or(false)
    }

    async fn resign(&self, election_id: &str, candidate: &AgentId) -> Result<bool> {
        let mut elections = self.elections.write().await;

        let state = elections.get_mut(election_id);

        match state {
            Some(s) if s.leader == *candidate => {
                s.expires_at = Utc::now(); // Expire immediately
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    async fn get_leader(&self, election_id: &str) -> Option<AgentId> {
        let elections = self.elections.read().await;

        elections
            .get(election_id)
            .filter(|s| s.expires_at > Utc::now())
            .map(|s| s.leader.clone())
    }

    async fn watch_leader(&self, election_id: &str) -> tokio::sync::mpsc::Receiver<LeaderChange> {
        let (tx, rx) = tokio::sync::mpsc::channel(10);

        let mut watchers = self.watchers.lock().unwrap();
        watchers
            .entry(election_id.to_string())
            .or_insert_with(Vec::new)
            .push(tx);

        rx
    }
}
```

---

## 5. State Sharing

### 5.1 Shared Memory Backend

```rust
/// Shared state store for agents
pub trait StateStore: Send + Sync {
    /// Get a value
    async fn get(&self, key: &str) -> Result<Option<serde_json::Value>>;

    /// Set a value
    async fn set(&self, key: &str, value: serde_json::Value, ttl: Option<Duration>) -> Result<()>;

    /// Delete a value
    async fn delete(&self, key: &str) -> Result<bool>;

    /// Compare and swap
    async fn cas(&self, key: &str, old: Option<serde_json::Value>, new: serde_json::Value) -> Result<bool>;

    /// Watch for changes
    async fn watch(&self, key: &str) -> tokio::sync::mpsc::Receiver<StateChange>;

    /// Transaction support
    async fn transaction(&self, ops: Vec<StateOperation>) -> Result<TransactionResult>;
}

/// State operation for transactions
#[derive(Debug, Clone)]
pub enum StateOperation {
    Get { key: String },
    Set { key: String, value: serde_json::Value, ttl: Option<Duration> },
    Delete { key: String },
}

/// Transaction result
#[derive(Debug, Clone)]
pub struct TransactionResult {
    pub succeeded: bool,
    pub results: Vec<Option<serde_json::Value>>,
}

/// State change event
#[derive(Debug, Clone)]
pub struct StateChange {
    pub key: String,
    pub old_value: Option<serde_json::Value>,
    pub new_value: Option<serde_json::Value>,
    pub timestamp: DateTime<Utc>,
}

/// In-memory state store
pub struct MemoryStateStore {
    data: Arc<RwLock<HashMap<String, StateEntry>>>,
    watchers: Arc<Mutex<HashMap<String, Vec<tokio::sync::mpsc::Sender<StateChange>>>>>,
}

struct StateEntry {
    value: serde_json::Value,
    expires_at: Option<DateTime<Utc>>,
}

impl MemoryStateStore {
    pub fn new() -> Self {
        Self {
            data: Arc::new(RwLock::new(HashMap::new())),
            watchers: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    async fn cleanup_expired(&self) {
        let now = Utc::now();
        let mut data = self.data.write().await;

        data.retain(|_, entry| {
            entry
                .expires_at
                .map(|exp| exp > now)
                .unwrap_or(true)
        });
    }

    async fn notify_watchers(&self, key: &str, old: Option<serde_json::Value>, new: Option<serde_json::Value>) {
        let watchers = self.watchers.lock().unwrap();
        if let Some(senders) = watchers.get(key) {
            let change = StateChange {
                key: key.to_string(),
                old_value: old,
                new_value: new,
                timestamp: Utc::now(),
            };

            for sender in senders {
                let _ = sender.try_send(change.clone());
            }
        }
    }
}

#[async_trait]
impl StateStore for MemoryStateStore {
    async fn get(&self, key: &str) -> Result<Option<serde_json::Value>> {
        self.cleanup_expired().await;

        let data = self.data.read().await;
        Ok(data.get(key).map(|entry| entry.value.clone()))
    }

    async fn set(&self, key: &str, value: serde_json::Value, ttl: Option<Duration>) -> Result<()> {
        let old_value = self.get(key).await?;

        let expires_at = ttl.map(|d| Utc::now() + chrono::Duration::milliseconds(d.as_millis() as i64));

        {
            let mut data = self.data.write().await;
            data.insert(key.to_string(), StateEntry {
                value,
                expires_at,
            });
        }

        self.notify_watchers(key, old_value, Some(value)).await;
        Ok(())
    }

    async fn delete(&self, key: &str) -> Result<bool> {
        let old_value = self.get(key).await?;

        let mut data = self.data.write().await;
        let existed = data.remove(key).is_some();

        if existed {
            self.notify_watchers(key, old_value, None).await;
        }

        Ok(existed)
    }

    async fn cas(&self, key: &str, old: Option<serde_json::Value>, new: serde_json::Value) -> Result<bool> {
        let mut data = self.data.write().await;

        let current = data.get(key).map(|e| &e.value);

        match (current, old) {
            (Some(current), Some(old)) if current == &old => {
                data.insert(key.to_string(), StateEntry {
                    value: new,
                    expires_at: None,
                });
                return Ok(true);
            }
            (None, None) => {
                data.insert(key.to_string(), StateEntry {
                    value: new,
                    expires_at: None,
                });
                return Ok(true);
            }
            _ => return Ok(false),
        }
    }

    async fn watch(&self, key: &str) -> tokio::sync::mpsc::Receiver<StateChange> {
        let (tx, rx) = tokio::sync::mpsc::channel(10);

        let mut watchers = self.watchers.lock().unwrap();
        watchers
            .entry(key.to_string())
            .or_insert_with(Vec::new)
            .push(tx);

        rx
    }

    async fn transaction(&self, ops: Vec<StateOperation>) -> Result<TransactionResult> {
        // Simple implementation: execute all ops, rollback on error
        let mut results = Vec::new();
        let mut succeeded = false;

        for op in ops {
            match op {
                StateOperation::Get { key } => {
                    results.push(self.get(&key).await?);
                }
                StateOperation::Set { key, value, ttl } => {
                    self.set(&key, value, ttl).await?;
                    results.push(None);
                }
                StateOperation::Delete { key } => {
                    let deleted = self.delete(&key).await?;
                    results.push(None);
                    succeeded = succeeded || deleted;
                }
            }
        }

        succeeded = true;

        Ok(TransactionResult {
            succeeded,
            results,
        })
    }
}
```

### 5.2 Distributed State (SQLite-based)

```rust
/// SQLite-backed distributed state store
pub struct SQLiteStateStore {
    db: Arc<Mutex<rusqlite::Connection>>,
}

impl SQLiteStateStore {
    pub fn new(path: &Path) -> Result<Self> {
        let conn = rusqlite::Connection::open(path)?;

        conn.execute(
            "CREATE TABLE IF NOT EXISTS state (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL,
                expires_at INTEGER
            )",
            [],
        )?;

        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_state_expires ON state(expires_at)",
            [],
        )?;

        Ok(Self {
            db: Arc::new(Mutex::new(conn)),
        })
    }

    fn cleanup_expired(&self) -> Result<()> {
        let db = self.db.lock().unwrap();
        let now = Utc::now().timestamp();

        db.execute(
            "DELETE FROM state WHERE expires_at IS NOT NULL AND expires_at <= ?",
            [now],
        )?;

        Ok(())
    }
}

#[async_trait]
impl StateStore for SQLiteStateStore {
    async fn get(&self, key: &str) -> Result<Option<serde_json::Value>> {
        self.cleanup_expired()?;

        let db = self.db.lock().unwrap();

        let mut stmt = db.prepare("SELECT value FROM state WHERE key = ?")?;

        let result: Result<Option<String>> = stmt
            .query_row([key], |row| row.get(0))
            .optional();

        match result {
            Ok(Some(json_str)) => {
                let value = serde_json::from_str(&json_str)?;
                Ok(Some(value))
            }
            Ok(None) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    async fn set(&self, key: &str, value: serde_json::Value, ttl: Option<Duration>) -> Result<()> {
        let db = self.db.lock().unwrap();

        let json_str = serde_json::to_string(&value)?;
        let expires_at = ttl
            .map(|d| Utc::now() + chrono::Duration::milliseconds(d.as_millis() as i64))
            .map(|dt| dt.timestamp());

        db.execute(
            "INSERT OR REPLACE INTO state (key, value, expires_at) VALUES (?, ?, ?)",
            [key, &json_str, expires_at],
        )?;

        Ok(())
    }

    async fn delete(&self, key: &str) -> Result<bool> {
        let db = self.db.lock().unwrap();

        let rows = db.execute("DELETE FROM state WHERE key = ?", [key])?;

        Ok(rows > 0)
    }

    async fn cas(&self, key: &str, old: Option<serde_json::Value>, new: serde_json::Value) -> Result<bool> {
        let db = self.db.lock().unwrap();

        let new_json = serde_json::to_string(&new)?;

        match old {
            Some(old_val) => {
                let old_json = serde_json::to_string(&old_val)?;
                let rows = db.execute(
                    "UPDATE state SET value = ? WHERE key = ? AND value = ?",
                    [&new_json, key, &old_json],
                )?;
                Ok(rows > 0)
            }
            None => {
                match db.execute(
                    "INSERT INTO state (key, value, expires_at) VALUES (?, ?, NULL)",
                    [key, &new_json],
                ) {
                    Ok(_) => Ok(true),
                    Err(rusqlite::Error::SqliteFailure(_, _)) => Ok(false), // PK violation
                    Err(e) => Err(e.into()),
                }
            }
        }
    }

    async fn watch(&self, key: &str) -> tokio::sync::mpsc::Receiver<StateChange> {
        // For SQLite, implement polling-based watch
        let (tx, rx) = tokio::sync::mpsc::channel(10);

        let key = key.to_string();
        let db = self.db.clone();

        tokio::spawn(async move {
            let mut last_value = loop {
                let db = db.lock().unwrap();
                if let Ok(Some(v)) = Self::get_sync(&db, &key) {
                    break v;
                }
                tokio::time::sleep(Duration::from_secs(1)).await;
            };

            loop {
                tokio::time::sleep(Duration::from_secs(1)).await;

                let db = db.lock().unwrap();
                let current_value = Self::get_sync(&db, &key).unwrap_or(None);

                if current_value != last_value {
                    let change = StateChange {
                        key: key.clone(),
                        old_value: last_value,
                        new_value: current_value.clone(),
                        timestamp: Utc::now(),
                    };

                    if tx.send(change).await.is_err() {
                        break;
                    }

                    last_value = current_value;
                }
            }
        });

        rx
    }

    async fn transaction(&self, ops: Vec<StateOperation>) -> Result<TransactionResult> {
        let db = self.db.lock().unwrap();
        let tx = db.unchecked_transaction()?;

        let mut results = Vec::new();

        for op in ops {
            match op {
                StateOperation::Get { key } => {
                    let mut stmt = tx.prepare("SELECT value FROM state WHERE key = ?")?;
                    let value = stmt
                        .query_row([key], |row| row.get::<_, String>(0))
                        .optional()
                        .and_then(|s| s.map(|json| serde_json::from_str::<serde_json::Value>(&json).ok()).flatten());

                    results.push(value);
                }
                StateOperation::Set { key, value, ttl } => {
                    let json_str = serde_json::to_string(&value)?;
                    let expires_at = ttl
                        .map(|d| Utc::now() + chrono::Duration::milliseconds(d.as_millis() as i64))
                        .map(|dt| dt.timestamp());

                    tx.execute(
                        "INSERT OR REPLACE INTO state (key, value, expires_at) VALUES (?, ?, ?)",
                        [key, &json_str, expires_at],
                    )?;
                    results.push(None);
                }
                StateOperation::Delete { key } => {
                    tx.execute("DELETE FROM state WHERE key = ?", [key])?;
                    results.push(None);
                }
            }
        }

        tx.commit()?;

        Ok(TransactionResult {
            succeeded: true,
            results,
        })
    }
}

impl SQLiteStateStore {
    fn get_sync(conn: &rusqlite::Connection, key: &str) -> Result<Option<serde_json::Value>> {
        let mut stmt = conn.prepare("SELECT value FROM state WHERE key = ?")?;

        let result: Option<String> = stmt
            .query_row([key], |row| row.get(0))
            .optional()?;

        match result {
            Some(json_str) => {
                let value = serde_json::from_str(&json_str)?;
                Ok(Some(value))
            }
            None => Ok(None),
        }
    }
}
```

---

## 6. Protocol Buffers Definition

For cross-language compatibility and efficiency, define protocol buffers:

```protobuf
// messages.proto
syntax = "proto3";
package zeroclaw.agent;

import "google/protobuf/timestamp.proto";
import "google/protobuf/duration.proto";

// Base message envelope
message AgentMessage {
  string id = 1;
  MessageType type = 2;
  AgentId from = 3;
  AgentId to = 4;
  google.protobuf.Timestamp timestamp = 5;
  string correlation_id = 6;
  Payload payload = 7;
  Metadata metadata = 8;
}

message AgentId {
  oneof id {
    string logical = 1;
    uint32 process = 2;
    string thread = 3;  // UUID
    string remote = 4;  // "host:port"
  }
}

enum MessageType {
  MESSAGE_TYPE_UNKNOWN = 0;

  // Requests
  MESSAGE_TYPE_TASK_REQUEST = 1;
  MESSAGE_TYPE_QUERY_REQUEST = 2;
  MESSAGE_TYPE_STREAM_REQUEST = 3;

  // Responses
  MESSAGE_TYPE_TASK_RESPONSE = 10;
  MESSAGE_TYPE_QUERY_RESPONSE = 11;
  MESSAGE_TYPE_STREAM_CHUNK = 12;
  MESSAGE_TYPE_STREAM_END = 13;

  // Events
  MESSAGE_TYPE_EVENT_PUBLISHED = 20;
  MESSAGE_TYPE_EVENT_NOTIFICATION = 21;

  // Coordination
  MESSAGE_TYPE_HEARTBEAT = 30;
  MESSAGE_TYPE_STATUS_UPDATE = 31;
  MESSAGE_TYPE_ERROR_REPORT = 32;
  MESSAGE_TYPE_HANDSHAKE = 33;
}

message Payload {
  oneof payload {
    TaskRequestPayload task_request = 1;
    TaskResponsePayload task_response = 2;
    QueryRequestPayload query_request = 3;
    QueryResponsePayload query_response = 4;
    StreamRequestPayload stream_request = 5;
    StreamChunkPayload stream_chunk = 6;
    EventPayload event = 7;
    HeartbeatPayload heartbeat = 8;
    StatusUpdatePayload status_update = 9;
    ErrorReportPayload error_report = 10;
    google.protobuf.Struct raw = 99;
  }
}

message TaskRequestPayload {
  string prompt = 1;
  google.protobuf.Struct input = 2;
  map<string, google.protobuf.Value> context = 3;
  ExecutionConstraints constraints = 4;
  CallbackInfo callback = 5;
}

message TaskResponsePayload {
  TaskStatus status = 1;
  string output = 2;
  google.protobuf.Struct data = 3;
  ErrorDetails error = 4;
  ExecutionMetrics metrics = 5;
  repeated Artifact artifacts = 6;
}

enum TaskStatus {
  TASK_STATUS_UNKNOWN = 0;
  TASK_STATUS_SUCCESS = 1;
  TASK_STATUS_FAILED = 2;
  TASK_STATUS_TIMEOUT = 3;
  TASK_STATUS_CANCELLED = 4;
  TASK_STATUS_IN_PROGRESS = 5;
}

message ExecutionMetrics {
  uint64 duration_ms = 1;
  uint32 llm_calls = 2;
  uint32 tool_calls = 3;
  uint32 tokens_input = 4;
  uint32 tokens_output = 5;
  uint64 memory_bytes = 6;
}

message Artifact {
  ArtifactKind kind = 1;
  string reference = 2;
  string description = 3;
  uint64 size = 4;
  string checksum = 5;
}

enum ArtifactKind {
  ARTIFACT_KIND_UNKNOWN = 0;
  ARTIFACT_KIND_FILE = 1;
  ARTIFACT_KIND_DIRECTORY = 2;
  ARTIFACT_KIND_URL = 3;
  ARTIFACT_KIND_DATA = 4;
  ARTIFACT_KIND_MODEL = 5;
  ARTIFACT_KIND_DATASET = 6;
}

message Metadata {
  uint32 priority = 1;
  uint64 ttl_seconds = 2;
  DeliveryLevel delivery = 3;
  RoutingHints routing = 4;
  map<string, string> labels = 5;
  uint64 timeout_ms = 6;
}

enum DeliveryLevel {
  DELIVERY_LEVEL_UNKNOWN = 0;
  DELIVERY_LEVEL_AT_MOST_ONCE = 1;
  DELIVERY_LEVEL_AT_LEAST_ONCE = 2;
  DELIVERY_LEVEL_EXACTLY_ONCE = 3;
}

message RoutingHints {
  string routing_key = 1;
  string topic = 2;
  bool persistent = 3;
}

// Event messages
message EventPayload {
  string event_type = 1;
  string source = 2;
  google.protobuf.Struct data = 3;
  google.protobuf.Timestamp timestamp = 4;
  uint32 version = 5;
}

// Coordination messages
message HeartbeatPayload {
  AgentId agent_id = 1;
  uint64 sequence = 2;
  AgentStatusInfo status = 3;
  LoadInfo load = 4;
}

message AgentStatusInfo {
  oneof status {
    google.protobuf.Empty idle = 1;
    Busy busy = 2;
    google.protobuf.Empty draining = 3;
    Error error = 4;
  }

  message Busy {
    string task_id = 1;  // UUID
  }

  message Error {
    string message = 1;
  }
}

message LoadInfo {
  uint64 memory_mb = 1;
  float cpu_percent = 2;
  uint32 active_tasks = 3;
  uint32 available_tools = 4;
}
```

---

## 7. Security Considerations

### 7.1 Message Authentication

```rust
/// Security context for message authentication and encryption
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecurityContext {
    /// Signature (HMAC-SHA256)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,

    /// Signing key ID
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key_id: Option<String>,

    /// Encryption algorithm
    #[serde(skip_serializing_if = "Option::is_none")]
    pub encryption: Option<EncryptionInfo>,

    /// Permissions context
    #[serde(default)]
    pub permissions: Permissions,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EncryptionInfo {
    pub algorithm: String,
    pub key_id: String,
    pub nonce: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Permissions {
    /// Required permission level
    #[serde(default)]
    pub level: PermissionLevel,

    /// Required capabilities
    #[serde(default)]
    pub capabilities: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PermissionLevel {
    Read,
    Write,
    Admin,
}

/// Message signer for authentication
pub trait MessageSigner: Send + Sync {
    /// Sign a message
    fn sign(&self, message: &AgentMessage) -> Result<String>;

    /// Verify a message signature
    fn verify(&self, message: &AgentMessage, signature: &str) -> Result<bool>;
}

/// HMAC-SHA256 message signer
pub struct HmacSigner {
    key: Vec<u8>,
    key_id: String,
}

impl HmacSigner {
    pub fn new(key: Vec<u8>, key_id: String) -> Self {
        Self { key, key_id }
    }

    fn serialize_for_signing(message: &AgentMessage) -> Result<Vec<u8>> {
        // Create canonical JSON for signing
        let mut msg = message.clone();
        msg.security = None; // Don't include signature in signed data

        let json = serde_json::to_vec(&msg)?;
        Ok(json)
    }
}

impl MessageSigner for HmacSigner {
    fn sign(&self, message: &AgentMessage) -> Result<String> {
        let data = Self::serialize_for_signing(message)?;

        let mut mac = Hmac::<sha2::Sha256>::new_from_slice(&self.key)
            .map_err(|e| anyhow::anyhow!("HMAC error: {}", e))?;

        mac.update(&data);
        let signature = mac.finalize();

        Ok(hex::encode(signature.as_bytes()))
    }

    fn verify(&self, message: &AgentMessage, signature: &str) -> Result<bool> {
        let data = Self::serialize_for_signing(message)?;

        let mut mac = Hmac::<sha2::Sha256>::new_from_slice(&self.key)
            .map_err(|e| anyhow::anyhow!("HMAC error: {}", e))?;

        mac.update(&data);
        let expected = mac.finalize();
        let expected_hex = hex::encode(expected.as_bytes());

        Ok(hmac::compare(&expected_hex, signature))
    }
}
```

### 7.2 Authorization

```rust
/// Authorization checker for agent operations
pub trait Authorizer: Send + Sync {
    /// Check if an agent is allowed to perform an operation
    async fn authorize(&self, agent: &AgentId, operation: &Operation, resource: &str) -> Result<bool>;

    /// Get allowed operations for an agent
    async fn allowed_operations(&self, agent: &AgentId) -> Result<Vec<Operation>>;
}

/// Operation that requires authorization
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum Operation {
    /// Send a message
    SendMessage,

    /// Invoke/delegate to another agent
    DelegateAgent,

    /// Access a resource
    ReadResource { resource: String },
    WriteResource { resource: String },

    /// Execute a tool
    ExecuteTool { tool: String },

    /// Publish to a topic
    PublishTopic { topic: String },

    /// Modify shared state
    WriteState { key: String },
}

/// Policy-based authorizer
pub struct PolicyAuthorizer {
    policies: Arc<RwLock<Vec<Policy>>>,
}

#[derive(Debug, Clone)]
struct Policy {
    subject: AgentSelector,
    operations: Vec<Operation>,
    resources: Vec<ResourceSelector>,
}

#[derive(Debug, Clone)]
enum AgentSelector {
    All,
    Specific(AgentId),
    Role(String),
}

#[derive(Debug, Clone)]
enum ResourceSelector {
    All,
    Prefix(String),
    Pattern(String),
}

impl PolicyAuthorizer {
    pub fn new() -> Self {
        Self {
            policies: Arc::new(RwLock::new(Vec::new())),
        }
    }

    pub async fn add_policy(&self, policy: Policy) {
        self.policies.write().await.push(policy);
    }
}

#[async_trait]
impl Authorizer for PolicyAuthorizer {
    async fn authorize(&self, agent: &AgentId, operation: &Operation, resource: &str) -> Result<bool> {
        let policies = self.policies.read().await;

        for policy in policies.iter() {
            if !self.matches_subject(&policy.subject, agent) {
                continue;
            }

            if !self.matches_operation(&policy.operations, operation) {
                continue;
            }

            if !self.matches_resource(&policy.resources, resource) {
                continue;
            }

            return Ok(true);
        }

        Ok(false)
    }

    async fn allowed_operations(&self, agent: &AgentId) -> Result<Vec<Operation>> {
        // Return all operations allowed for this agent
        let policies = self.policies.read().await;
        let mut ops = Vec::new();

        for policy in policies.iter() {
            if self.matches_subject(&policy.subject, agent) {
                ops.extend(policy.operations.clone());
            }
        }

        Ok(ops)
    }
}

impl PolicyAuthorizer {
    fn matches_subject(&self, selector: &AgentSelector, agent: &AgentId) -> bool {
        match selector {
            AgentSelector::All => true,
            AgentSelector::Specific(id) => id == agent,
            AgentSelector::Role(_) => true, // TODO: Implement role matching
        }
    }

    fn matches_operation(&self, allowed: &[Operation], operation: &Operation) -> bool {
        allowed.iter().any(|op| self.op_matches(op, operation))
    }

    fn op_matches(&self, allowed: &Operation, requested: &Operation) -> bool {
        match (allowed, requested) {
            (Operation::SendMessage, Operation::SendMessage) => true,
            (Operation::DelegateAgent, Operation::DelegateAgent) => true,
            (Operation::ReadResource { resource: a }, Operation::ReadResource { resource: b }) => {
                self.resource_matches(a, b)
            }
            (Operation::WriteResource { resource: a }, Operation::WriteResource { resource: b }) => {
                self.resource_matches(a, b)
            }
            (Operation::ExecuteTool { tool: a }, Operation::ExecuteTool { tool: b }) => {
                a == b || a == "*"
            }
            (Operation::PublishTopic { topic: a }, Operation::PublishTopic { topic: b }) => {
                self.topic_matches(a, b)
            }
            (Operation::WriteState { key: a }, Operation::WriteState { key: b }) => {
                self.key_matches(a, b)
            }
            _ => false,
        }
    }

    fn resource_matches(&self, selector: &str, resource: &str) -> bool {
        if selector == "*" {
            return true;
        }
        if selector.ends_with('*') {
            let prefix = &selector[..selector.len() - 1];
            return resource.starts_with(prefix);
        }
        selector == resource
    }

    fn topic_matches(&self, selector: &str, topic: &str) -> bool {
        wildcard_match(topic, selector)
    }

    fn key_matches(&self, selector: &str, key: &str) -> bool {
        if selector == "*" {
            return true;
        }
        if selector.ends_with('*') {
            let prefix = &selector[..selector.len() - 1];
            return key.starts_with(prefix);
        }
        selector == key
    }

    fn matches_resource(&self, selectors: &[ResourceSelector], resource: &str) -> bool {
        selectors.iter().any(|selector| match selector {
            ResourceSelector::All => true,
            ResourceSelector::Prefix(prefix) => resource.starts_with(prefix),
            ResourceSelector::Pattern(pattern) => {
                // TODO: Implement regex matching
                resource.contains(pattern)
            }
        })
    }
}
```

---

## 8. Usage Examples

### 8.1 Complete Task Delegation Flow

```rust
use crate::agent::communication::*;

async fn delegate_task_example() -> Result<()> {
    // 1. Create communication infrastructure
    let router = Arc::new(MemoryRouter::new());
    let state_store = Arc::new(SQLiteStateStore::new(Path::new("/tmp/agent_state.db"))?) as Arc<dyn StateStore>;

    // 2. Register main agent
    let main_id = AgentId::Logical("main".to_string());
    let main_channel = router.create_channel(main_id.clone()).await?;

    // 3. Register worker agent
    let worker_id = AgentId::Logical("researcher".to_string());
    let worker_channel = router.create_channel(worker_id.clone()).await?;

    // 4. Create task request
    let task = AgentMessage {
        id: Uuid::new_v4(),
        r#type: MessageType::TaskRequest,
        from: main_id.clone(),
        to: Some(worker_id.clone()),
        timestamp: Utc::now(),
        correlation_id: Some(Uuid::new_v4()),
        payload: Payload::TaskRequest(TaskRequestPayload {
            prompt: "Research Rust async runtime best practices".to_string(),
            input: None,
            context: HashMap::new(),
            constraints: ExecutionConstraints {
                timeout_seconds: Some(300),
                max_iterations: Some(10),
                allowed_tools: vec!["web_search".to_string(), "memory_read".to_string()],
                denied_tools: vec![],
                resources: ResourceLimits::default(),
            },
            callback: None,
        }),
        metadata: Metadata {
            priority: 100,
            delivery: DeliveryLevel::AtLeastOnce,
            ..Default::default()
        },
        security: None,
    };

    // 5. Send task
    let message_id = main_channel.send(task).await?;
    println!("Task sent: {}", message_id);

    // 6. Worker receives and processes
    let mut worker_receiver = worker_channel.receiver();
    let incoming = worker_receiver.receive_timeout(Duration::from_secs(5)).await?;

    if let Payload::TaskRequest(task_req) = incoming.payload {
        // Process task...
        let response = AgentMessage {
            id: Uuid::new_v4(),
            r#type: MessageType::TaskResponse,
            from: worker_id.clone(),
            to: Some(main_id),
            timestamp: Utc::now(),
            correlation_id: incoming.correlation_id,
            payload: Payload::TaskResponse(TaskResponsePayload {
                status: TaskStatus::Success,
                output: "Research completed successfully".to_string(),
                data: Some(serde_json::json!({
                    "findings": [
                        "Use Tokio for async runtime",
                        "Prefer async/await over manual futures"
                    ]
                })),
                error: None,
                metrics: ExecutionMetrics {
                    duration_ms: 2500,
                    llm_calls: 3,
                    tool_calls: 2,
                    ..Default::default()
                },
                artifacts: vec![],
            }),
            metadata: Metadata::default(),
            security: None,
        };

        // Send response back
        let _ = worker_channel.send(response).await;
    }

    Ok(())
}
```

### 8.2 Event-Driven Coordination

```rust
async fn event_driven_example() -> Result<()> {
    // 1. Create topic broker
    let broker = Arc::new(MemoryTopicBroker::new());

    // 2. Create agents
    let producer = AgentId::Logical("producer".to_string());
    let consumer_a = AgentId::Logical("consumer_a".to_string());
    let consumer_b = AgentId::Logical("consumer_b".to_string());

    // 3. Subscribe to events
    let mut sub_a = broker.subscribe(
        consumer_a.clone(),
        TopicFilter::Wildcard("events.*".to_string()),
    ).await?;

    let mut sub_b = broker.subscribe(
        consumer_b.clone(),
        TopicFilter::Exact("events.file.changed".to_string()),
    ).await?;

    // 4. Spawn consumer tasks
    tokio::spawn(async move {
        while let Ok(msg) = sub_a.receive().await {
            println!("Consumer A received: {:?}", msg.r#type);
        }
    });

    tokio::spawn(async move {
        while let Ok(msg) = sub_b.receive().await {
            println!("Consumer B received: {:?}", msg.r#type);
        }
    });

    // 5. Publish event
    let event = AgentMessage {
        id: Uuid::new_v4(),
        r#type: MessageType::EventPublished,
        from: producer.clone(),
        to: None,
        timestamp: Utc::now(),
        correlation_id: None,
        payload: Payload::Event(EventPayload {
            event_type: "file.changed".to_string(),
            source: "producer".to_string(),
            data: serde_json::json!({
                "file": "/path/to/file.txt",
                "change": "modified"
            }),
            timestamp: Utc::now(),
            version: 1,
        }),
        metadata: Metadata::default(),
        security: None,
    };

    let result = broker.publish("events.file.changed".to_string(), event).await?;
    println!("Event delivered to {} subscribers", result.delivered);

    Ok(())
}
```

### 8.3 Streaming Large Response

```rust
async fn streaming_example() -> Result<()> {
    let stream_broker = Arc::new(MemoryStreamBroker::new());

    // 1. Create stream
    let request = StreamRequestPayload {
        topic: "large_file_generation".to_string(),
        initial_data: HashMap::new(),
        config: StreamConfig {
            chunk_size_bytes: Some(8192),
            compression: None,
            include_intermediate: true,
        },
    };

    let stream_handle = stream_broker.create_stream(request).await?;

    // 2. Subscribe as consumer
    let mut subscriber = stream_broker.subscribe_stream(stream_handle.id).await?;

    // 3. Spawn consumer
    tokio::spawn(async move {
        let mut full_data = Vec::new();

        loop {
            match subscriber.rx.recv().await {
                Ok(StreamMessage::Chunk(chunk)) => {
                    println!("Received chunk {}: {} bytes", chunk.sequence, chunk.data.len());
                    full_data.extend(chunk.data.clone());

                    if chunk.progress > 0.0 {
                        println!("Progress: {:.1}%", chunk.progress * 100.0);
                    }
                }
                Ok(StreamMessage::End(final_result)) => {
                    println!("Stream complete. Total: {} bytes", full_data.len());
                    if let Some(result) = final_result {
                        println!("Final result: {:?}", result);
                    }
                    break;
                }
                Err(e) => {
                    println!("Stream error: {:?}", e);
                    break;
                }
            }
        }
    });

    // 4. Producer sends chunks
    for i in 0..5 {
        let chunk = StreamChunkPayload {
            sequence: i,
            data: format!("Chunk {} data", i).into_bytes(),
            is_final: false,
            progress: (i + 1) as f32 / 5.0,
        };

        stream_broker.send_chunk(stream_handle.id, chunk).await?;
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    // 5. End stream
    stream_broker.end_stream(stream_handle.id, Some(TaskResponsePayload {
        status: TaskStatus::Success,
        output: "File generation complete".to_string(),
        data: None,
        error: None,
        metrics: ExecutionMetrics::default(),
        artifacts: vec![],
    })).await?;

    Ok(())
}
```

---

## 9. Integration with Existing Components

### 9.1 Extending DelegateTool

```rust
use crate::tools::delegate::DelegateTool;

/// Communication-enabled delegate tool
pub struct CommunicatingDelegateTool {
    base: DelegateTool,
    router: Arc<dyn MessageRouter>,
    state_store: Arc<dyn StateStore>,
}

impl CommunicatingDelegateTool {
    pub fn new(
        base: DelegateTool,
        router: Arc<dyn MessageRouter>,
        state_store: Arc<dyn StateStore>,
    ) -> Self {
        Self {
            base,
            router,
            state_store,
        }
    }

    /// Delegate with full messaging protocol
    async fn delegate_with_messaging(
        &self,
        agent_name: &str,
        prompt: &str,
        context: &str,
    ) -> anyhow::Result<String> {
        // 1. Check if agent is available
        let agents = self.router.agents().await?;
        let target_id = AgentId::Logical(agent_name.to_string());

        if !agents.contains(&target_id) {
            return Err(anyhow::anyhow!("Agent not available: {}", agent_name));
        }

        // 2. Create task message
        let correlation_id = Uuid::new_v4();
        let message = AgentMessage {
            id: Uuid::new_v4(),
            r#type: MessageType::TaskRequest,
            from: AgentId::Logical("main".to_string()),
            to: Some(target_id.clone()),
            timestamp: Utc::now(),
            correlation_id: Some(correlation_id),
            payload: Payload::TaskRequest(TaskRequestPayload {
                prompt: prompt.to_string(),
                input: None,
                context: {
                    let mut map = HashMap::new();
                    if !context.is_empty() {
                        map.insert("context".to_string(), serde_json::json!(context));
                    }
                    map
                },
                constraints: ExecutionConstraints::default(),
                callback: None,
            }),
            metadata: Metadata::default(),
            security: None,
        };

        // 3. Route message
        let result = self.router.route(message).await?;
        println!("Routing result: {:?}", result);

        // 4. Wait for response with matching correlation ID
        let state_key = format!("response:{}", correlation_id);

        // Poll for response (simplified)
        for _ in 0..30 {
            tokio::time::sleep(Duration::from_millis(100)).await;

            if let Some(value) = self.state_store.get(&state_key).await? {
                let response: TaskResponsePayload = serde_json::from_value(value)?;
                return Ok(response.output);
            }
        }

        Err(anyhow::anyhow!("Delegation timed out"))
    }
}
```

---

## 10. Performance Considerations

### 10.1 Message Size Limits

| Channel Type | Max Message Size | Notes |
|---------------|------------------|-------|
| Memory (mpsc) | ~2GB | Limited by RAM |
| Unix Socket | ~8KB (SO_SNDBUF) | Requires streaming for larger |
| Network (HTTP) | Configurable | Use chunking for large data |
| Shared Memory | System limit | Best for >1MB data |

### 10.2 Batching Strategy

```rust
/// Message batching for efficiency
pub trait BatchingPolicy: Send + Sync {
    /// Determine if message should be batched
    fn should_batch(&self, message: &AgentMessage) -> bool;

    /// Get batch size
    fn batch_size(&self) -> usize;

    /// Get batch timeout
    fn batch_timeout(&self) -> Duration;
}

/// Size-based batching
pub struct SizeBasedBatching {
    max_batch_size: usize,
}

impl BatchingPolicy for SizeBasedBatching {
    fn should_batch(&self, message: &AgentMessage) -> bool {
        // Batch if message is small
        let size = serde_json::to_vec(message).unwrap_or_default().len();
        size < 1024 // 1KB threshold
    }

    fn batch_size(&self) -> usize {
        self.max_batch_size
    }

    fn batch_timeout(&self) -> Duration {
        Duration::from_millis(100)
    }
}
```

---

## 11. Error Handling

### 11.1 Error Codes

```rust
/// Error codes for communication failures
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CommErrorCode {
    // General errors
    Unknown = 0,
    Timeout = 1,
    Canceled = 2,

    // Routing errors
    AgentNotFound = 10,
    AgentUnavailable = 11,
    RoutingFailed = 12,

    // Message errors
    MessageTooLarge = 20,
    InvalidFormat = 21,
    MissingField = 22,
    SignatureInvalid = 23,

    // Authorization errors
    Unauthorized = 30,
    Forbidden = 31,
    RateLimited = 32,

    // State errors
    StateConflict = 40,
    StateNotFound = 41,

    // Stream errors
    StreamNotFound = 50,
    StreamClosed = 51,
    ChunkError = 52,
}

impl std::fmt::Display for CommErrorCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CommErrorCode::Unknown => write!(f, "UNKNOWN"),
            CommErrorCode::Timeout => write!(f, "TIMEOUT"),
            CommErrorCode::Canceled => write!(f, "CANCELED"),
            CommErrorCode::AgentNotFound => write!(f, "AGENT_NOT_FOUND"),
            CommErrorCode::AgentUnavailable => write!(f, "AGENT_UNAVAILABLE"),
            CommErrorCode::RoutingFailed => write!(f, "ROUTING_FAILED"),
            CommErrorCode::MessageTooLarge => write!(f, "MESSAGE_TOO_LARGE"),
            CommErrorCode::InvalidFormat => write!(f, "INVALID_FORMAT"),
            CommErrorCode::MissingField => write!(f, "MISSING_FIELD"),
            CommErrorCode::SignatureInvalid => write!(f, "SIGNATURE_INVALID"),
            CommErrorCode::Unauthorized => write!(f, "UNAUTHORIZED"),
            CommErrorCode::Forbidden => write!(f, "FORBIDDEN"),
            CommErrorCode::RateLimited => write!(f, "RATE_LIMITED"),
            CommErrorCode::StateConflict => write!(f, "STATE_CONFLICT"),
            CommErrorCode::StateNotFound => write!(f, "STATE_NOT_FOUND"),
            CommErrorCode::StreamNotFound => write!(f, "STREAM_NOT_FOUND"),
            CommErrorCode::StreamClosed => write!(f, "STREAM_CLOSED"),
            CommErrorCode::ChunkError => write!(f, "CHUNK_ERROR"),
        }
    }
}
```

---

## 12. Testing Support

### 12.1 Test Double Implementations

```rust
#[cfg(test)]
mod tests {
    use super::*;

    /// In-memory test transport
    pub struct TestTransport {
        tx: tokio::sync::mpsc::Sender<AgentMessage>,
        rx: Arc<Mutex<tokio::sync::mpsc::Receiver<AgentMessage>>>,
    }

    impl TestTransport {
        pub fn new() -> (Self, Self) {
            let (tx1, rx1) = tokio::sync::mpsc::channel(10);
            let (tx2, rx2) = tokio::sync::mpsc::channel(10);

            let client = Self {
                tx: tx1,
                rx: Arc::new(Mutex::new(rx2)),
            };

            let server = Self {
                tx: tx2,
                rx: Arc::new(Mutex::new(rx1)),
            };

            (client, server)
        }

        pub async fn send(&mut self, msg: AgentMessage) -> Result<()> {
            self.tx.send(msg).await?;
            Ok(())
        }

        pub async fn recv(&mut self) -> Result<AgentMessage> {
            let rx = self.rx.lock().unwrap();
            Ok(rx.recv().await?)
        }
    }

    #[tokio::test]
    async fn test_request_response() {
        let (mut client, mut server) = TestTransport::new();

        // Client sends request
        let request = AgentMessage {
            id: Uuid::new_v4(),
            r#type: MessageType::TaskRequest,
            from: AgentId::Logical("client".to_string()),
            to: Some(AgentId::Logical("server".to_string())),
            timestamp: Utc::now(),
            correlation_id: Some(Uuid::new_v4()),
            payload: Payload::TaskRequest(TaskRequestPayload {
                prompt: "Test task".to_string(),
                input: None,
                context: HashMap::new(),
                constraints: ExecutionConstraints::default(),
                callback: None,
            }),
            metadata: Metadata::default(),
            security: None,
        };

        client.send(request.clone()).await.unwrap();

        // Server receives
        let received = server.recv().await.unwrap();
        assert_eq!(received.from, AgentId::Logical("client".to_string()));

        // Server sends response
        let response = AgentMessage {
            id: Uuid::new_v4(),
            r#type: MessageType::TaskResponse,
            from: AgentId::Logical("server".to_string()),
            to: Some(AgentId::Logical("client".to_string())),
            timestamp: Utc::now(),
            correlation_id: received.correlation_id,
            payload: Payload::TaskResponse(TaskResponsePayload {
                status: TaskStatus::Success,
                output: "Test result".to_string(),
                data: None,
                error: None,
                metrics: ExecutionMetrics::default(),
                artifacts: vec![],
            }),
            metadata: Metadata::default(),
            security: None,
        };

        server.send(response).await.unwrap();

        // Client receives response
        let received_response = client.recv().await.unwrap();
        assert_eq!(received_response.r#type, MessageType::TaskResponse);
    }
}
```

---

## 13. Summary

This specification defines comprehensive communication protocols for ZeroClaw's multi-agent system:

1. **Message Format**: Standardized envelope with type-safe payloads
2. **Routing Patterns**: Direct, broadcast, pub/sub support
3. **Coordination**: Heartbeat, locks, leader election
4. **State Sharing**: Memory and SQLite-backed stores
5. **Security**: Authentication, authorization, encryption support
6. **Streaming**: Efficient large data transfer
7. **Testing**: Test doubles for unit testing

The design is backward compatible with existing `DelegateTool` and `AgentRegistry` implementations.
