# AGENTS.md — agent/

> Core orchestration loop — observe, decide, act, reflect.

## Overview

The agent subsystem owns the full message lifecycle: prompt construction, LLM dispatch, tool execution, cost tracking, memory injection, history compaction, and cancellation. Entry points are `process_message` (single-shot) and `run` (interactive REPL). The `Agent` struct is built via `AgentBuilder` — all fields except `provider`, `memory`, and `observer` have sensible defaults.

## Key Files

| File | Responsibility |
|---|---|
| `agent.rs` | `Agent` struct, `AgentBuilder`, `turn()` method |
| `loop_.rs` | `process_message`, `run`, tool-call loop, cost tracking, history compaction, credential scrubbing |
| `dispatcher.rs` | `ToolDispatcher` trait — `NativeToolDispatcher` (OpenAI function-calling) vs `XmlToolDispatcher` (XML-wrapped JSON) |
| `prompt.rs` | `SystemPromptBuilder` with composable `PromptSection` trait; sections: Identity, Tools, Safety, Skills, Workspace, DateTime, ChannelMedia |
| `thinking.rs` | `ThinkingLevel` enum (Off..Max), `/think:<level>` directive parsing, temperature/token/prompt adjustments |
| `classifier.rs` | Rule-based query classification — keyword/pattern matching with priority, length constraints; returns hint for model routing |
| `memory_loader.rs` | `MemoryLoader` trait, `DefaultMemoryLoader` — recall with time-decay, relevance threshold (default 0.4), autosave filtering |
| `tests.rs` | 20+ integration tests using `ScriptedProvider` and mock tools/memory |

## Agent Lifecycle

1. `AgentBuilder::new()` -> set provider, tools, memory, observer, config, etc. -> `.build()` returns `Agent`.
2. Dispatcher selection: providers with native function-calling get `NativeToolDispatcher`; others get `XmlToolDispatcher`. Native dispatchers send `ToolSpec` JSON; XML dispatchers embed instructions in the system prompt instead.
3. `Agent.turn(message)` runs one user message through the tool-call loop and returns the final text response.

## Orchestration Loop (loop_.rs)

The tool-call loop in `process_message`/`run` follows this cycle:

1. **Prompt build** — `SystemPromptBuilder::with_defaults().build(ctx)` assembles sections. Workspace files (`AGENTS.md`, `SOUL.md`, `BOOTSTRAP.md`, etc.) are injected from the workspace dir, truncated at 20k chars each.
2. **Memory injection** — `MemoryLoader.load_context()` recalls entries, applies time-decay, filters by relevance score and autosave keys. Prepended as `[Memory context]` block.
3. **Classification** — `classifier::classify()` matches user message against rules (keywords case-insensitive, patterns case-sensitive, priority-sorted). Hint can route to a different model via `route_model_by_hint`.
4. **Thinking** — `/think:<level>` directive parsed and stripped. Level resolves via hierarchy: inline > session > config > Medium. Adjusts temperature, max_tokens, and injects system prompt prefix.
5. **Tool filtering** — `filter_tool_specs_for_turn` applies `tool_filter_groups`: built-in tools always pass; MCP tools need glob-pattern match + keyword match (for `dynamic` mode). `filter_by_allowed_tools` further restricts by allowlist.
6. **LLM call** — `Provider::chat()` with constructed `ChatRequest`. Streaming chunks are batched at `STREAM_CHUNK_MIN_CHARS` (80 chars).
7. **Dispatch** — `ToolDispatcher::parse_response()` extracts text + tool calls. Native dispatcher reads `response.tool_calls`; XML dispatcher parses `<tool_call>JSON</tool_call>` tags (stripping `<think>` blocks first).
8. **Tool execution** — tools run sequentially; results formatted via dispatcher and appended to history.
9. **Budget check** — `check_tool_loop_budget()` reads from `tokio::task_local` cost-tracking context. Model pricing lookup is 3-tier: exact model name, `provider/model`, suffix after last `/`.
10. **Iteration cap** — `DEFAULT_MAX_TOOL_ITERATIONS` (10) prevents runaway loops. Configurable via `max_tool_iterations`.
11. **Credential scrubbing** — `scrub_credentials()` redacts `token|api_key|password|secret|bearer|credential` patterns in tool output, preserving 4-char prefix.
12. **History compaction** — triggers when message count > `DEFAULT_MAX_HISTORY_MESSAGES` (50) OR estimated tokens exceed budget. Compacts older messages (snapped to user-turn boundary) via LLM summarizer, keeping 20 recent messages. Summary capped at 2k chars.
13. **Auto-save** — user messages >= 20 chars and assistant responses are saved to memory with UUID-keyed entries.

## Thinking Model Integration

`ThinkingLevel` has 6 tiers: Off, Minimal, Low, Medium (default), High, Max. Each maps to `ThinkingParams` with temperature adjustment (-0.2 to +0.1), max_tokens adjustment (-1000 to +2000), and optional system prompt prefix. The `/think:` directive must appear at message start (leading whitespace OK). Aliases: `none`=Off, `min`=Minimal, `med`/`default`=Medium, `maximum`=Max.

## Cost Tracking & Budget Enforcement

Cost tracking is scoped via `tokio::task_local!` (`TOOL_LOOP_COST_TRACKING_CONTEXT`), not global state — tests and CLI without cost config get `None`. `record_tool_loop_cost_usage` computes cost from `ModelPricing` entries. `check_tool_loop_budget` returns `BudgetCheck::Allowed` as fallback when no context is scoped.

## Prompt Construction & Memory Injection

`SystemPromptBuilder` chains `PromptSection` impls. Order matters: Identity > ToolHonesty > Tools > Safety > Skills > Workspace > DateTime > ChannelMedia. The Safety section varies by `AutonomyLevel` (Full omits "ask before acting"; Supervised includes it). Security summary is injected under `### Active Security Policy` when present. `IdentitySection` checks for AIEOS identity config first, then always injects workspace files.

## Tool Call Routing

Two dispatcher strategies coexist:
- **NativeToolDispatcher**: sends tool specs as OpenAI function-calling JSON; parses `ChatResponse.tool_calls` directly. `should_send_tool_specs() = true`.
- **XmlToolDispatcher**: injects tool-use protocol in system prompt; parses `<tool_call>{JSON}</tool_call>` from response text. Strips `<think>` tags (Qwen/reasoning models). `should_send_tool_specs() = false`.

`reasoning_content` field on `ConversationMessage::AssistantToolCalls` is preserved by native dispatcher, ignored by XML dispatcher. This prevents thinking-level prefix leakage across turns.

## Model Switching

Runtime model switching uses a global `LazyLock<Arc<Mutex<Option<(String, String)>>>>` (`MODEL_SWITCH_REQUEST`). The `model_switch` tool sets it; the agent loop checks and clears it between iterations. Thread-safe but coarse — only one pending switch at a time.

## Cancellation & Shutdown

`CancellationToken` from `tokio_util` is threaded through the loop. Check it between tool iterations, not mid-LLM-call. Interactive sessions persist history to a JSON state file (`InteractiveSessionState`) for resume.

## Testing Patterns

Tests use `ScriptedProvider` (returns pre-queued `ChatResponse`s, records requests), mock `Memory` and `Tool` impls. Key coverage: simple text, single/multi tool chains, max-iteration bailout, unknown tool recovery, tool failure recovery, parallel dispatch, history trimming, memory auto-save, native vs XML dispatcher, empty responses, mixed text+tools, builder validation, idempotent system prompt insertion. All tests are `#[tokio::test]` async.

## Common Gotchas

- **task_local cost tracking**: forgetting to scope `TOOL_LOOP_COST_TRACKING_CONTEXT` means silent no-op cost tracking, not errors.
- **XmlToolDispatcher strips `<think>` tags**: if a model uses non-standard reasoning tags, tool calls inside them get dropped silently.
- **History compaction snaps to user-turn boundary**: the `compact_end` pointer walks backward until it hits a `role=user` message, which can leave more messages than expected.
- **Credential scrubbing is regex-based**: it catches common patterns but not all secret formats. Values < 8 chars are not redacted.
- **Tool filter groups**: empty `groups` config means all tools pass (backward compat), not "no tools".
- **`BOOTSTRAP_MAX_CHARS` (20k)**: large workspace files are silently truncated in the system prompt.

## Cross-Subsystem Coupling

- **providers/** — `Provider` trait, `ChatRequest`/`ChatResponse`, `ToolCall`, `ConversationMessage`
- **tools/** — `Tool` trait, `ToolSpec`, `ToolResult`; tool execution is synchronous within the async loop
- **memory/** — `Memory` trait for recall/store; `decay` module for time-based scoring; `response_cache` for dedup
- **security/** — `SecurityPolicy` for tool allowlisting; `AutonomyLevel` shapes Safety prompt section
- **config/** — `AgentConfig`, `QueryClassificationConfig`, `ModelPricing`, `ToolFilterGroup`, `IdentityConfig`
- **observability/** — `Observer` trait receives `ObserverEvent`s; `runtime_trace` for structured logging
- **skills/** — `Skill` structs rendered into prompt via `SkillsSection`; callable skill tools are prefixed (`deploy.release_checklist`)
- **i18n/** — `ToolDescriptions` for locale-aware tool descriptions in prompts
