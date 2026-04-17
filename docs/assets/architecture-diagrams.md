# ZeroClaw Architecture Diagrams

This document provides visual representations of ZeroClaw's architecture, execution modes, and data flows.

---

## 1. Execution Modes

**Ways ZeroClaw can be run:**

```mermaid
flowchart TD
    Start[zeroclaw CLI] --> Onboard[onboard<br/>Setup wizard]
    Start --> Agent[agent<br/>Interactive CLI]
    Start --> Gateway[gateway<br/>HTTP server]
    Start --> Daemon[daemon<br/>Long-running runtime]
    Start --> Channel[channel<br/>Messaging platforms]
    Start --> Service[service<br/>OS service mgmt]
    Start --> Models[models<br/>Provider catalog]
    Start --> Cron[cron<br/>Scheduled tasks]
    Start --> Hardware[hardware<br/>Peripheral discovery]
    Start --> Peripheral[peripheral<br/>Hardware management]
    Start --> Status[status<br/>System overview]
    Start --> Doctor[doctor<br/>Diagnostics]
    Start --> Migrate[migrate<br/>Data import]
    Start --> Skills[skills<br/>User capabilities]
    Start --> Integrations[integrations<br/>Browse 50+ apps]

    Agent --> AgentSingle[-m message<br/>One-shot]
    Agent --> AgentInteractive[Interactive REPL<br/>stdin/stdout]

    Daemon --> DaemonSupervised[Supervised runtime<br/>Gateway + Channels + Scheduler]
```

---

## 2. System Architecture Overview

**High-level component structure:**

```mermaid
flowchart TB
    subgraph CLI[CLI Entry Point]
        Main[main.rs]
    end

    subgraph Core[Core Subsystems]
        Config[config/<br/>Configuration & Schema]
        Agent[agent/<br/>Orchestration Loop]
        Providers[providers/<br/>LLM Adapters]
        Channels[channels/<br/>Messaging Platforms]
        Tools[tools/<br/>Tool Execution]
        Memory[memory/<br/>Storage Backends]
        Security[security/<br/>Policy & Pairing]
        Runtime[runtime/<br/>Execution Adapters]
        Gateway[gateway/<br/>HTTP/Webhook Server]
        Daemon[daemon/<br/>Supervised Runtime]
        Peripherals[peripherals/<br/>Hardware Control]
        Observability[observability/<br/>Telemetry & Metrics]
        RAG[rag/<br/>Hardware Documentation]
        Cron[cron/<br/>Scheduler]
        Skills[skills/<br/>User Capabilities]
    end

    subgraph Integrations[Integrations]
        Composio[Composio<br/>1000+ Apps]
        Browser[Browser<br/>Brave Integration]
        Tunnel[Tunnel<br/>Cloudflare/boringproxy]
    end

    Main --> Config
    Main --> Agent
    Main --> Gateway
    Main --> Daemon
    Main --> Channels

    Agent --> Providers
    Agent --> Tools
    Agent --> Memory
    Agent --> Security
    Agent --> Runtime
    Agent --> Peripherals
    Agent --> RAG
    Agent --> Skills

    Channels --> Agent
    Gateway --> Agent

    Daemon --> Gateway
    Daemon --> Channels
    Daemon --> Cron
    Daemon --> Observability

    Tools --> Composio
    Tools --> Browser
    Gateway --> Tunnel

    classDef coreComp fill:#4A90E2,stroke:#1E3A5F,color:#fff
    classDef integComp fill:#50C878,stroke:#1E3A5F,color:#fff
    classDef cliComp fill:#F5A623,stroke:#1E3A5F,color:#fff

    class Config,Agent,Providers,Channels,Tools,Memory,Security,Runtime,Gateway,Daemon,Peripherals,Observability,RAG,Cron,Skills coreComp
    class Composio,Browser,Tunnel integComp
    class Main cliComp
```

---

## 3. Message Flow Through The System

**How a user message becomes a response:**

```mermaid
sequenceDiagram
    participant User
    participant Channel as Channel Layer
    participant Dispatcher as Message Dispatcher
    participant Agent as Agent Loop
    participant Provider as LLM Provider
    participant Tools as Tool Registry
    participant Memory as Memory Backend

    User->>Channel: Send message
    Channel->>Dispatcher: ChannelMessage{id, sender, content}
    Dispatcher->>Memory: Recall context
    Memory-->>Dispatcher: Relevant memories
    Dispatcher->>Agent: process_message()

    Note over Agent: Build system prompt<br/>+ memory context

    Agent->>Provider: chat_with_tools(history)
    Provider-->>Agent: LLM response

    alt Tool calls present
        loop For each tool call
            Agent->>Tools: execute(args)
            Tools-->>Agent: ToolResult
        end
        Agent->>Provider: chat_with_tools(+ tool results)
        Provider-->>Agent: Final response
    end

    Agent-->>Dispatcher: Response text
    Dispatcher->>Memory: Store conversation
    Dispatcher-->>Channel: SendMessage{content, recipient}
    Channel-->>User: Reply
```

---

## 4. Agent Loop Execution Flow

**The core agent orchestration loop:**

```mermaid
flowchart TD
    Start[[Start: User Message]] --> BuildContext[Build Context]

    BuildContext --> MemoryRecall[Memory.recall<br/>Retrieve relevant entries]
    BuildContext --> HardwareRAG{Hardware<br/>enabled?}
    HardwareRAG -->|Yes| LoadDatasheets[Load Hardware RAG<br/>Pin aliases + chunks]
    HardwareRAG -->|No| BuildPrompt[Build System Prompt]
    LoadDatasheets --> BuildPrompt

    MemoryRecall --> Enrich[Enrich Message<br/>memory + RAG context]
    Enrich --> BuildPrompt

    BuildPrompt --> InitHistory[Initialize History<br/>system + user message]

    InitHistory --> ToolLoop{Tool Call Loop<br/>max 10 iterations}

    ToolLoop --> LLMRequest[Provider.chat_with_tools<br/>or chat_with_history]
    LLMRequest --> ParseResponse[Parse Response]

    ParseResponse --> HasTools{Tool calls<br/>present?}

    HasTools -->|No| SaveResponse[Push assistant response]
    SaveResponse --> Return[[Return: Final Response]]

    HasTools -->|Yes| Approval{Needs<br/>approval?}
    Approval -->|Yes & Denied| DenyTool[Record denied]
    DenyTool --> NextIteration

    Approval -->|No / Approved| ExecuteTools[Execute Tools<br/>in parallel]

    ExecuteTools --> ScrubResults[Scrub credentials<br/>from output]
    ScrubResults --> AddResults[Add tool results<br/>to history]
    AddResults --> NextIteration

    DenyTool --> NextIteration[Increment iteration]
    NextIteration --> MaxIter{Reached<br/>max 10?}
    MaxIter -->|Yes| Error[[Error: Max iterations]]
    MaxIter -->|No| ToolLoop

    classDef contextStep fill:#E8F4FD,stroke:#4A90E2
    classDef llmStep fill:#FFF4E6,stroke:#F5A623
    classDef toolStep fill:#E8FDF5,stroke:#50C878
    classDef errorStep fill:#FDE8E8,stroke:#D0021B

    class BuildContext,MemoryRecall,HardwareRAG,LoadDatasheets,Enrich,BuildPrompt,InitHistory contextStep
    class LLMRequest,ParseResponse llmStep
    class ExecuteTools,ScrubResults,AddResults toolStep
    class Error errorStep
```

---

## 5. Daemon Supervision Model

**How the daemon keeps components alive:**

```mermaid
flowchart TB
    Start[[zeroclaw daemon]] --> SpawnComponents

    SpawnComponents --> SpawnState[Spawn State Writer<br/>5s flush interval]
    SpawnComponents --> SpawnGateway[Spawn Gateway Supervisor]
    SpawnComponents --> SpawnChannels{Channels<br/>configured?}
    SpawnComponents --> SpawnHeartbeat{Heartbeat<br/>enabled?}
    SpawnComponents --> SpawnScheduler{Cron<br/>enabled?}

    SpawnChannels -->|Yes| SpawnChannelSup[Spawn Channel Supervisor]
    SpawnChannels -->|No| MarkChannelsOK[Mark channels OK<br/>disabled]

    SpawnHeartbeat -->|Yes| SpawnHeartbeatWorker[Spawn Heartbeat Worker]
    SpawnHeartbeat -->|No| MarkHeartbeatOK[Mark heartbeat OK<br/>disabled]

    SpawnScheduler -->|Yes| SpawnSchedulerWorker[Spawn Cron Scheduler]
    SpawnScheduler -->|No| MarkSchedulerOK[Mark scheduler OK<br/>disabled]

    SpawnGateway --> GatewayLoop{Gateway Loop}
    SpawnChannelSup --> ChannelLoop{Channel Loop}
    SpawnHeartbeatWorker --> HeartbeatLoop{Heartbeat Loop}
    SpawnSchedulerWorker --> SchedulerLoop{Scheduler Loop}

    GatewayLoop --> GatewayRun[run_gateway]
    GatewayRun --> GatewayExit{Exit OK?}
    GatewayExit -->|No| GatewayError[Mark error + log]
    GatewayExit -->|Yes| GatewayUnexpected[Mark: unexpected exit]
    GatewayError --> GatewayBackoff[Wait with backoff]
    GatewayUnexpected --> GatewayBackoff
    GatewayBackoff --> GatewayLoop

    ChannelLoop --> ChannelRun[start_channels]
    ChannelRun --> ChannelExit{Exit OK?}
    ChannelExit -->|No| ChannelError[Mark error + log]
    ChannelExit -->|Yes| ChannelUnexpected[Mark: unexpected exit]
    ChannelError --> ChannelBackoff[Wait with backoff]
    ChannelUnexpected --> ChannelBackoff
    ChannelBackoff --> ChannelLoop

    HeartbeatLoop --> HeartbeatRun[Collect tasks + Agent runs]
    HeartbeatRun --> HeartbeatExit{Exit OK?}
    HeartbeatExit -->|No| HeartbeatError[Mark error + log]
    HeartbeatExit -->|Yes| HeartbeatUnexpected[Mark: unexpected exit]
    HeartbeatError --> HeartbeatBackoff[Wait with backoff]
    HeartbeatUnexpected --> HeartbeatBackoff
    HeartbeatBackoff --> HeartbeatLoop

    SchedulerLoop --> SchedulerRun[cron::scheduler::run]
    SchedulerRun --> SchedulerExit{Exit OK?}
    SchedulerExit -->|No| SchedulerError[Mark error + log]
    SchedulerExit -->|Yes| SchedulerUnexpected[Mark: unexpected exit]
    SchedulerError --> SchedulerBackoff[Wait with backoff]
    SchedulerUnexpected --> SchedulerBackoff
    SchedulerBackoff --> SchedulerLoop

    MarkChannelsOK --> Running[Daemon Running<br/>Ctrl+C to stop]
    MarkHeartbeatOK --> Running
    MarkSchedulerOK --> Running
    SpawnState --> Running

    Running --> StopRequest[Ctrl+C received]
    StopRequest --> AbortAll[Abort all tasks]
    AbortAll --> JoinAll[Wait for tasks]
    JoinAll --> Done[[Daemon stopped]]

    classDef supervisor fill:#FDE8E8,stroke:#D0021B
    classDef running fill:#E8FDF5,stroke:#50C878
    classDef component fill:#E8F4FD,stroke:#4A90E2

    class SpawnGateway,SpawnChannelSup,SpawnHeartbeatWorker,SpawnSchedulerWorker,SpawnState supervisor
    class Running running
    class GatewayRun,ChannelRun,HeartbeatRun,SchedulerRun component
```

---

## 6. Gateway HTTP Endpoints

**The gateway's HTTP API structure:**

```mermaid
flowchart TB
    Client[HTTP Client] --> Gateway[ZeroClaw Gateway]

    Gateway --> PairPOST[POST /pair<br/>Exchange one-time code<br/>for bearer token]
    Gateway --> HealthGET[GET /health<br/>Status check]
    Gateway --> WebhookPOST[POST /webhook<br/>Main agent endpoint]
    Gateway --> WAVerify[GET /whatsapp<br/>Meta verification]
    Gateway --> WAMessage[POST /whatsapp<br/>WhatsApp webhook]

    PairPOST --> PairLimiter[Rate Limiter<br/>pair req/min]
    PairLimiter --> PairGuard[PairingGuard<br/>Code validation]
    PairGuard --> PairResponse[{paired, token, persisted}]

    WebhookPOST --> WebhookLimiter[Rate Limiter<br/>webhook req/min]
    WebhookLimiter --> WebhookPairing{Pairing<br/>required?}
    WebhookPairing -->|Yes| BearerAuth[Bearer token check]
    WebhookPairing -->|No| WebhookSecret{Secret<br/>configured?}
    WebhookSecret -->|Yes| SecretCheck[X-Webhook-Secret<br/>HMAC-SHA256 verify]
    WebhookSecret -->|No| Idempotency[Idempotency check<br/>X-Idempotency-Key]
    BearerAuth --> Idempotency
    SecretCheck --> Idempotency

    Idempotency --> MemoryStore[Auto-save to memory]
    MemoryStore --> ProviderCall[Provider.simple_chat]
    ProviderCall --> WebhookResponse[{response, model}]

    WAVerify --> TokenCheck[verify_token check<br/>constant-time compare]
    TokenCheck --> Challenge[Return hub.challenge]

    WAMessage --> SignatureCheck[X-Hub-Signature-256<br/>HMAC-SHA256 verify]
    SignatureCheck --> ParsePayload[Parse messages]
    ParsePayload --> ForEach[For each message]
    ForEach --> WAMemory[Auto-save to memory]
    WAMemory --> WAProvider[Provider.simple_chat]
    WAProvider --> WASend[WhatsAppChannel.send]

    classDef auth fill:#FDE8E8,stroke:#D0021B
    classDef processing fill:#E8F4FD,stroke:#4A90E2
    classDef response fill:#E8FDF5,stroke:#50C878

    class PairLimiter,PairGuard,BearerAuth,SecretCheck auth
    class MemoryStore,ProviderCall,TokenCheck,ParsePayload,ForEach,WAMemory,WAProvider processing
    class PairResponse,WebhookResponse,Challenge,WASend response
```

---

## 7. Channel Message Dispatch

**How channels route messages to the agent:**

```mermaid
flowchart TB
    subgraph Channels[Channel Listeners]
        TG[Telegram]
        DC[Discord]
        SL[Slack]
        IM[iMessage]
        MX[Matrix]
        SIG[Signal]
        WA[WhatsApp]
        Email[Email]
        IRC[IRC]
        Lark[Lark]
        DT[DingTalk]
        QQ[QQ]
    end

    Channels --> MPSC[MPSC Channel<br/>100-buffer queue]

    MPSC --> Semaphore[Semaphore<br/>Max in-flight limit]
    Semaphore --> WorkerPool[Worker Pool<br/>JoinSet]

    WorkerPool --> Process[process_channel_message]

    Process --> LogReceive[Log: 💬 from user]
    LogReceive --> MemoryRecall[build_memory_context]
    MemoryRecall --> AutoSave[Auto-save if enabled]

    AutoSave --> StartTyping[channel.start_typing]
    StartTyping --> Timeout[300s timeout guard]

    Timeout --> AgentCall[run_tool_call_loop<br/>silent mode]
    AgentCall --> StopTyping[channel.stop_typing]

    StopTyping --> Success{Success?}
    Success -->|Yes| LogReply[Log: 🤖 Reply time]
    Success -->|No| LogError[Log: ❌ LLM error]
    Success -->|Timeout| LogTimeout[Log: ❌ Timeout]

    LogReply --> SendReply[channel.send reply]
    LogError --> SendError[channel.send error msg]
    LogTimeout --> SendTimeout[channel.send timeout msg]

    SendReply --> Done[Message complete]
    SendError --> Done
    SendTimeout --> Done

    Done --> NextWorker[Join next worker]
    NextWorker --> WorkerPool

    classDef channel fill:#E8F4FD,stroke:#4A90E2
    classDef queue fill:#FFF4E6,stroke:#F5A623
    classDef process fill:#FDE8E8,stroke:#D0021B
    classDef success fill:#E8FDF5,stroke:#50C878

    class TG,DC,SL,IM,MX,SIG,WA,Email,IRC,Lark,DT,QQ channel
    class MPSC,Semaphore,WorkerPool queue
    class Process,LogReceive,MemoryRecall,AutoSave,StartTyping,Timeout,AgentCall,StopTyping process
    class LogReply,SendReply,Done,NextWorker success
```

---

## 8. Memory System Architecture

**Storage backends and data flow:**

```mermaid
flowchart TB
    subgraph Frontend[Memory Frontends]
        AutoSave[Auto-save hooks<br/>user_msg, assistant_resp]
        StoreTool[memory_store tool]
        RecallTool[memory_recall tool]
        ForgetTool[memory_forget tool]
        GetTool[memory_get tool]
        ListTool[memory_list tool]
        CountTool[memory_count tool]
    end

    subgraph Backends[Memory Backends]
        Sqlite[(sqlite<br/>Default, local file)]
        Markdown[(markdown<br/>Daily .md files)]
        Lucid[(lucid<br/>Cloud sync)]
        None[(none<br/>In-memory only)]
    end

    subgraph Categories[Memory Categories]
        Conv[Conversation<br/>Chat transcripts]
        Daily[Daily<br/>Session summaries]
        Core[Core<br/>Long-term facts]
    end

    AutoSave --> MemoryTrait[Memory trait]
    StoreTool --> MemoryTrait
    RecallTool --> MemoryTrait
    ForgetTool --> MemoryTrait
    GetTool --> MemoryTrait
    ListTool --> MemoryTrait
    CountTool --> MemoryTrait

    MemoryTrait --> Factory[create_memory factory]
    Factory -->|config.memory.backend| BackendSelect{Backend?}

    BackendSelect -->|sqlite| Sqlite
    BackendSelect -->|markdown| Markdown
    BackendSelect -->|lucid| Lucid
    BackendSelect -->|none| None

    Sqlite --> Categories
    Markdown --> Categories
    Lucid --> Categories

    Categories --> Storage[(Persistent Storage)]

    RAG[Hardware RAG] -.->|load_chunks| Markdown

    classDef frontend fill:#E8F4FD,stroke:#4A90E2
    classDef backend fill:#FFF4E6,stroke:#F5A623
    classDef category fill:#E8FDF5,stroke:#50C878
    classDef storage fill:#FDE8E8,stroke:#D0021B

    class AutoSave,StoreTool,RecallTool,ForgetTool,GetTool,ListTool,CountTool frontend
    class Sqlite,Markdown,Lucid,None backend
    class Conv,Daily,Core category
    class Storage storage
```

---

## 9. Provider and Model Routing

**LLM provider abstraction and routing:**

```mermaid
flowchart TB
    subgraph Providers[Supported Providers]
        OR[OpenRouter]
        Anth[Anthropic]
        OAI[OpenAI]
        OpenRouter[openrouter]
        MiniMax[minimax]
        DeepSeek[deepseek]
        Kimi[kimi]
        Custom[custom URL]
    end

    subgraph Routing[Model Routing]
        Routes[model_routes config<br/>Pattern -> Provider]
    end

    subgraph Factory[Provider Factory]
        Resilient[create_resilient_provider<br/>Retry + Timeout]
        Routed[create_routed_provider<br/>Model-based routing]
    end

    subgraph Traits[Provider Trait]
        ChatSystem[chat_with_system<br/>Simple chat]
        ChatHistory[chat_with_history<br/>Multi-turn]
        ChatTools[chat_with_tools<br/>Native function calling]
        Warmup[warmup<br/>Connection pool warmup]
        SupportsNative[supports_native_tools<br/>Capability check]
    end

    Providers --> Factory
    Routes --> Factory

    Factory --> Traits

    ChatSystem --> LLM1[LLM API Call]
    ChatHistory --> LLM2[LLM API Call]
    ChatTools --> LLM3[LLM API Call + Functions]

    LLM1 --> Response[ChatMessage<br/>text + role]
    LLM2 --> Response
    LLM3 --> ToolResponse[ChatMessage + ToolCalls<br/>id, name, arguments]

    classDef provider fill:#E8F4FD,stroke:#4A90E2
    classDef routing fill:#FFF4E6,stroke:#F5A623
    classDef factory fill:#E8FDF5,stroke:#50C878
    classDef trait fill:#FDE8E8,stroke:#D0021B

    class OR,Anth,OAI,OpenRouter,MiniMax,DeepSeek,Kimi,Custom provider
    class Routes routing
    class Resilient,Routed factory
    class ChatSystem,ChatHistory,ChatTools,Warmup,SupportsNative trait
```

---

## 10. Tool Execution Architecture

**Tool registry, execution, and security:**

```mermaid
flowchart TB
    subgraph ToolCategories[Tool Categories]
        Core[Core Tools<br/>shell, file_read, file_write]
        Memory[Memory Tools<br/>store, recall, forget]
        Schedule[Schedule Tools<br/>cron_add, cron_list, etc.]
        Browser[Browser<br/>Brave integration]
        Composio[Composio<br/>1000+ app actions]
        Hardware[Hardware<br/>gpio_read, gpio_write,<br/>arduino_upload, etc.]
        Delegate[Delegate<br/>Sub-agent routing]
        Screenshot[screenshot<br/>Screen capture]
    end

    subgraph Registry[Tool Registry]
        AllTools[all_tools_with_runtime<br/>Factory function]
        DefaultTools[default_tools<br/>Base set]
        PeripheralTools[create_peripheral_tools<br/>Hardware-specific]
    end

    subgraph Security[Security Policy]
        AllowedCmds[allowed_commands<br/>Allowlist]
        WorkspaceOnly[workspace_only<br/>Path restriction]
        MaxActions[max_actions_per_hour<br/>Rate limit]
        MaxCost[max_cost_per_day_cents<br/>Cost cap]
        Approval[approval manager<br/>Supervised tools]
    end

    subgraph Execution[Tool Execution]
        Validate[Input validation<br/>Schema check]
        Approve{Approval<br/>needed?}
        Execute[execute async]
        Scrub[Scrub credentials<br/>from output]
        Result[ToolResult<br/>success, output, error]
    end

    ToolCategories --> Registry
    Registry --> Security
    Security --> Execution

    Validate --> Approve
    Approve -->|Yes| Prompt[Prompt CLI]
    Approve -->|No / Approved| Execute
    Approve -->|Denied| Denied[Return denied]

    Prompt --> UserChoice{User choice?}
    UserChoice -->|Yes| Execute
    UserChoice -->|No| Denied

    Execute --> Scrub
    Scrub --> Result
    Result --> Return[Return to agent loop]

    classDef tools fill:#E8F4FD,stroke:#4A90E2
    classDef registry fill:#FFF4E6,stroke:#F5A623
    classDef security fill:#FDE8E8,stroke:#D0021B
    classDef exec fill:#E8FDF5,stroke:#50C878

    class Core,Memory,Schedule,Browser,Composio,Hardware,Delegate,Screenshot tools
    class AllTools,DefaultTools,PeripheralTools registry
    class AllowedCmds,WorkspaceOnly,MaxActions,MaxCost,Approval security
    class Validate,Approve,Prompt,Execute,Scrub,Result,Return exec
```

---

## 11. Configuration Loading

**How configuration is loaded and merged:**

```mermaid
flowchart TB
    Start[Config::load_or_init] --> Exists{Config file<br/>exists?}

    Exists -->|No| RunWizard[Run onboard wizard]
    RunWizard --> Save[Save config.toml]
    Save --> Load[Load from file]

    Exists -->|Yes| Load

    Load --> Parse[TOML parse]
    Parse --> Defaults[Apply defaults<br/>Config::default]

    Defaults --> EnvOverrides[apply_env_overrides<br/>ZEROCLAW_* env vars]

    EnvOverrides --> Validate[Schema validation]

    Validate --> Valid{Valid?}
    Valid -->|No| Error[[Error: invalid config]]
    Valid -->|Yes| Complete[Complete Config]

    Complete --> Paths[Paths<br/>workspace_dir, config_path]
    Complete --> Providers[default_provider,<br/>api_key, api_url]
    Complete --> Model[default_model,<br/>default_temperature]
    Complete --> Gateway[gateway config<br/>port, host, pairing]
    Complete --> Channels[channels_config<br/>telegram, discord, etc.]
    Complete --> Memory[memory config<br/>backend, auto_save]
    Complete --> Security[autonomy config<br/>level, allowed_commands]
    Complete --> Reliability[reliability config<br/>timeouts, retries]
    Complete --> Observability[observability<br/>backend, metrics]
    Complete --> Runtime[runtime config<br/>kind, exec]
    Complete --> Peripherals[peripherals<br/>boards, datasheet_dir]
    Complete --> Cron[cron config<br/>enabled, db_path]
    Complete --> Composio[composio<br/>enabled, api_key]
    Complete --> Browser[browser<br/>enabled, allowlist]
    Complete --> Tunnel[tunnel<br/>provider, token]

    classDef config fill:#E8F4FD,stroke:#4A90E2
    classDef error fill:#FDE8E8,stroke:#D0021B
    classDef section fill:#FFF4E6,stroke:#F5A623

    class Load,Parse,Defaults,EnvOverrides,Validate,Complete config
    class Error error
    class Paths,Providers,Model,Gateway,Channels,Memory,Security,Reliability,Observability,Runtime,Peripherals,Cron,Composio,Browser,Tunnel section
```

---

## 12. Hardware Peripherals Integration

**Hardware board support and control:**

```mermaid
flowchart TB
    subgraph Boards[Supported Boards]
        Nucleo[Nucleo-F401RE<br/>STM32F401RETx]
        Uno[Arduino Uno<br/>ATmega328P]
        UnoQ[Uno Q<br/>ESP32 WiFi bridge]
        RPi[RPi GPIO<br/>Native Linux]
        ESP32[ESP32<br/>Direct serial]
    end

    subgraph Transport[Transport Layer]
        Serial[Serial port<br/>/dev/ttyACM0, /dev/ttyUSB0]
        USB[USB probe-rs<br/>ST-Link JTAG]
        Native[Native GPIO<br/>Linux sysfs]
    end

    subgraph Peripherals[Peripheral System]
        Create[create_peripheral_tools<br/>Factory function]
        GPIO[gpio_read/write<br/>Digital I/O]
        Upload[arduino_upload<br/>Sketch flash]
        MemMap[hardware_memory_map<br/>Address ranges]
        BoardInfo[hardware_board_info<br/>Chip identification]
        MemRead[hardware_memory_read<br/>Register dump]
        Capabilities[hardware_capabilities<br/>Pin enumeration]
    end

    subgraph RAG[Hardware RAG]
        Datasheets[datasheet_dir<br/>.md documentation]
        Chunks[Chunked embedding<br/>Semantic search]
        PinAliases[Pin alias mapping<br/>red_led → 13]
    end

    Boards --> Transport
    Transport --> Peripherals

    RAG -.->|Context injection| Peripherals

    Create --> ToolRegistry[Tool registry]
    GPIO --> ToolRegistry
    Upload --> ToolRegistry
    MemMap --> ToolRegistry
    BoardInfo --> ToolRegistry
    MemRead --> ToolRegistry
    Capabilities --> ToolRegistry

    ToolRegistry --> Agent[Agent loop integration]

    classDef board fill:#E8F4FD,stroke:#4A90E2
    classDef transport fill:#FFF4E6,stroke:#F5A623
    classDef peripheral fill:#E8FDF5,stroke:#50C878
    classDef rag fill:#FDE8E8,stroke:#D0021B

    class Nucleo,Uno,UnoQ,RPi,ESP32 board
    class Serial,USB,Native transport
    class Create,GPIO,Upload,MemMap,BoardInfo,MemRead,Capabilities,ToolRegistry peripheral
    class Datasheets,Chunks,PinAliases rag
```

---

## 13. Observable Events

**Telemetry and observability flow:**

```mermaid
flowchart TB
    subgraph Observers[Observer Backends]
        Noop[NoopObserver<br/>No-op / testing]
        Console[ConsoleObserver<br/>Stdout logging]
        Metrics[MetricsObserver<br/>Prometheus format]
    end

    subgraph Events[Observable Events]
        AgentStart[AgentStart<br/>provider, model]
        LlmRequest[LlmRequest<br/>provider, model, msg_count]
        LlmResponse[LlmResponse<br/>duration, success, error]
        ToolCallStart[ToolCallStart<br/>tool name]
        ToolCall[ToolCall<br/>tool, duration, success]
        TurnComplete[TurnComplete<br/>end of agent loop]
        AgentEnd[AgentEnd<br/>duration, tokens, cost]
    end

    subgraph Outputs[Outputs]
        Stdout[stdout trace logs]
        MetricsFile[metrics.json<br/>JSON lines]
        Prometheus[Prometheus<br/>Text format]
    end

    Events --> Observers
    Observers --> Outputs

    AgentStart --> Record[record_event]
    LlmRequest --> Record
    LlmResponse --> Record
    ToolCallStart --> Record
    ToolCall --> Record
    TurnComplete --> Record
    AgentEnd --> Record

    Record --> Dispatch[Dispatch to backend]
    Dispatch --> Console
    Dispatch --> Metrics

    Console --> Stdout
    Metrics --> MetricsFile

    classDef observer fill:#E8F4FD,stroke:#4A90E2
    classDef event fill:#FFF4E6,stroke:#F5A623
    classDef output fill:#E8FDF5,stroke:#50C878

    class Noop,Console,Metrics observer
    class AgentStart,LlmRequest,LlmResponse,ToolCallStart,ToolCall,TurnComplete,AgentEnd,Record,Dispatch event
    class Stdout,MetricsFile,Prometheus output
```

---

## Summary Diagram

**Quick reference overview:**

```mermaid
mindmap
    root((ZeroClaw))
        Modes
            Agent CLI
                Interactive
                Single-shot
            Gateway
                HTTP API
                Webhooks
            Daemon
                Supervised
                Multi-component
            Channels
                12+ platforms
        Components
            Agent Loop
                Tool calling
                Memory aware
            Providers
                50+ LLMs
                Model routing
            Channels
                Real-time
                Supervised
            Tools
                30+ tools
                Hardware control
            Memory
                4 backends
                RAG-capable
            Security
                Pairing
                Approval
                Policy
        Integrations
            Composio
                1000+ apps
            Browser
                Brave
            Tunnel
                Cloudflare
                boringproxy
        Hardware
            STM32
            Arduino
            ESP32
            RPi GPIO
```

---

Generated for ZeroClaw v0.6.9 - Architecture Documentation
