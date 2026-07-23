# Tool execution lifecycle

ZeroClaw tools are capabilities the model can invoke during a turn. The tool
catalog says what can be called; the execution lifecycle says how a call becomes
safe, observable, cancellable, and provider-visible.

Use this page when a change touches built-in tools, MCP tool activation, the
agent loop, approval policy, tool-call streaming events, receipts, observer
events, tool-result history, cancellation, or the boundary between channel
ingress and agent-side action.

## Execution path

| Step | Owner | Review contract |
| --- | --- | --- |
| Tool definition | `zeroclaw-api::tool::Tool` | A tool has a stable name, description, JSON schema, async `execute`, and attribution. |
| Tool assembly | Runtime tool factory and scoped registry | The agent receives only the tools admitted by bundles, MCP config, risk profile, and per-run narrowing. |
| Turn context resolution | `ResolvedAgentExecution` | The turn starts with one resolved bundle: model access, registry, approval manager, observer, runtime knobs, MCP activation handle, and receipt generator. |
| Provider request | `agent::turn::tool_specs` and provider call | Native-tool providers receive structured specs; text-protocol providers receive prompt instructions unless strict parsing hides them. |
| Tool-call parsing | `agent::turn::parse_response` and parser helpers | Native and text tool calls are normalized into parsed calls with provider ids when available. |
| Preparation | `agent::turn::call_prep` | Hooks, delivery defaults, approval, prompt-required duplicate guards, and ordinary duplicate-call guards run before dispatch. |
| Execution | `agent::tool_execution` | Calls run sequentially or in parallel according to policy, cancellation, and activation constraints. |
| Result recording | `post_exec`, `results_collect`, and `history_append` | Results are ordered, logged, observed, optionally receipted, bounded, and appended back to provider history. |
| Loop control | `run_tool_call_loop` | The model sees tool results and may continue until it returns final text, hits cancellation, or reaches the iteration cap. |

The runtime separates these steps so a review can ask which boundary changed.
Adding a tool is not the same as widening approval policy, changing provider
tool specs, altering observer payloads, or persisting a result.

## Tool definitions and registration

Every tool implements the `Tool` trait:

```rust
#[async_trait]
pub trait Tool: Send + Sync + Attributable {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn parameters_schema(&self) -> serde_json::Value;
    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult>;
}
```

`ToolResult` is small: `success`, `output`, and `error`. Tool implementations
should not each invent their own logging or approval path. The dispatcher owns
the common start/result events, receipts, observer records, progress messages,
and history conversion.

Tool specs are rebuilt for provider requests. The current `ToolSpec` shares
large schemas through `Arc` so the wire format stays the same while avoiding
deep clones on every iteration.

## Resolved execution context

Entry points should not assemble a turn by re-deriving policy inline. The turn
engine receives a `ResolvedAgentExecution` bundle for stable per-agent
dependencies: model binding, effective tool registry, observer and approval
handles, resolved runtime knobs, the deferred-MCP activation set, model-switch
callback, and optional receipt generator.

Per-message state stays outside that bundle: history, streaming sinks, event
channels, steering messages, cancellation token, memory injection state, and the
ingress envelope.

When a PR adds a new execution input, prefer threading it through this resolved
context or the explicit per-turn `ToolLoop` state. Avoid hidden globals or
re-looking-up config inside one tool path.

## Availability and MCP activation

The model can only call tools that are effective for the current turn:

- static tools come from the scoped registry;
- `excluded_tools` removes names before prompt/spec exposure and before
  execution;
- native-tool providers receive structured specs for effective tools;
- text-protocol providers receive tool instructions only when text tool calling
  is allowed;
- strict parsing can hide the text tool protocol entirely;
- `tool_filter_groups` decide which MCP tool schemas are visible for the
  current turn. `mode = "always"` groups can pre-activate eligible deferred MCP
  wrappers, while `dynamic` groups expose tools only when the current user
  message matches their keywords;
- deferred MCP can expose a `tool_search` stub instead of every MCP wrapper.

Deferred MCP activation is stateful within the turn. `tool_search` resolves
matching MCP stubs into the shared `ActivatedToolSet`; later calls can execute
those activated wrappers. Filter groups do not grant capability by themselves:
the scoped registry, MCP policy, and denylist still decide which wrappers can
exist.

Do not run `tool_search` in parallel with the tools it activates. The dispatcher
forces any batch containing `tool_search` to run sequentially so lookup cannot
race activation. Delegate/subagent paths must thread the activated set they were
granted; otherwise a delegated turn can advertise or attempt a tool that its
executor cannot resolve.

## Approval and preparation

Preparation happens before the executor runs a tool:

1. `before_tool_call` hooks may cancel or rewrite the name/arguments.
2. Channel delivery defaults may be injected for channel-aware tools.
3. The runtime clears any "approved" marker in arguments.
4. The approval gate evaluates the tool against the `ApprovalManager`.
5. Approved calls get the runtime-approved marker restored.
6. Duplicate-call guards remove repeated identical calls unless the tool is
   exempt.

Approval has different front doors:

- CLI managers prompt the operator and support `yes`, `no`, and `always`.
- Non-interactive channel managers auto-deny prompt-required tools unless the
  channel provides an inline approval backchannel.
- ACP/web backchannels can carry the approval request to a real operator even
  though the turn itself is non-interactive.
- `DenyWithEdit` / replacement responses are sanitized and become synthetic
  tool results; the original tool does not execute.

Approval is a pre-execution control. It is not a receipt, and it is not proof
that a tool ran. Audit entries record the decision and the deciding channel or
backchannel.

Prompt-required shell calls have an extra loop guard: if the agent repeats the
same prompt-required shell call before approval, the loop aborts instead of
prompting over and over.

## Dispatch, cancellation, and ordering

The executor emits a pending `TurnEvent::ToolCall` immediately before running
the tool so streaming clients can show a live running card. When the tool
finishes, it emits the matching `TurnEvent::ToolResult` using the same
correlation id.

Parallel execution is allowed only when:

- the runtime knob enables parallel tools;
- the batch has more than one executable call;
- no call in the batch requires approval;
- the batch does not contain `tool_search`.

Otherwise calls run sequentially. Sequential dispatch checks cancellation
before each call and stops dispatching the tail when cancelled. Parallel
dispatch can finish some siblings while others are interrupted; completed calls
keep their real terminal result, and only unfinished calls get an interrupted
result.

The ordered result vector keeps one slot per original model call. Preparation
fills slots for cancelled, denied, replaced, or deduplicated calls; execution
fills the remaining slots. This preserves provider history ordering even when
some calls never execute or when parallel calls finish out of order.

## Results, receipts, and history

Successful tool executions normalize empty output to `(no output)`. When
`[agent.tool_receipts] enabled = true`, successful executions can receive a
receipt from the active receipt scope before the result is appended to history.
Channel-runtime paths and direct-turn paths have different scope lifetimes; the
[Tool receipts](../security/tool-receipts.md) page owns the exact HMAC format
and key-lifetime details.

Receipts are result evidence. They are not approval decisions, not durable audit
records, not a chain, and not generated for denied, replaced, blocked, failed,
or interrupted calls.

After execution:

- observer `ToolCallStart` events carry the tool name, provider tool-call id
  when available, arguments, channel, agent alias, and turn id;
- terminal observer `ToolCall` events add duration, success flag, and scrubbed
  result while repeating the correlation fields needed by span-oriented
  backends;
- progress streams show start/completion lines with scrubbed failure text;
- `after_tool_call` hooks run for executed calls;
- results are bounded by `max_tool_result_chars` before they are appended to
  model-visible history;
- loop-detection uses result content except for configured ignored tools;
- the next provider request sees the assistant tool-call turn plus the ordered
  tool results.

Tool results are not long-term memory unless a memory write occurs. They may be
current-turn context, persisted session history, a streamed UI event, an
observer/log record, or a receipt-bearing result. Name the surface precisely in
PRs and reviews.

## What this page does not own

Channel adapters and gateways own inbound transport, auth, pairing, webhook
decoding, and reply delivery. Tool execution starts after a turn has reached the
agent loop and a model has emitted a tool call.

Config lifecycle owns how tool-related settings are loaded, saved, overridden,
and reloaded. This page only covers the resolved values after they enter the
turn.

Security and autonomy docs own the policy vocabulary. This page shows where
that policy is applied to a concrete tool call.

Memory and payload lifecycle owns durability and privacy boundaries for
history, files, media, and memory. This page covers the tool-result path that
feeds those surfaces.

[Background work lifecycle](./background-work-lifecycle.md) owns the longer-lived contract when a tool starts delegated or subagent work. A tool returning a task ID does not make its execution restart-resumable.

## Reviewer checklist

For tool execution changes, answer these before reviewer sign-off:

- Which boundary changed: tool definition, registry assembly, approval,
  execution, receipts, observer events, history, or UI streaming?
- Does the tool remain attributable and registered through the normal factory
  path?
- Does the model only see tools admitted for this agent/run/iteration?
- Do `excluded_tools`, per-run narrowing, `tool_filter_groups`, and deferred
  MCP activation still agree?
- Does a prompt-required call run sequentially and ask the correct approval
  surface?
- Does non-interactive execution deny or use a real backchannel rather than
  silently approving?
- Are duplicate-call and repeated-prompt guards preserved?
- Does cancellation close only unfinished tool cards/results?
- Are observer/log/progress surfaces scrubbed and bounded where user or secret
  payloads can appear?
- Are receipts described as successful-execution evidence, not approval,
  persistence, or zero-knowledge proof?
- Does the PR include boundary-level validation for the user-visible surface it
  changes: CLI, channel, ACP/WS, gateway, cron, or delegate/subagent?

## Source pointers

Canonical docs:

- [Tools overview](../tools/overview.md)
- [Built-In Tool Inventory](../developing/tool-inventory.md)
- [MCP](../tools/mcp.md)
- [Autonomy levels](../security/autonomy.md)
- [Tool receipts](../security/tool-receipts.md)
- [Request lifecycle](./request-lifecycle.md)
- [Memory and payload lifecycle](./memory-payload-lifecycle.md)
- [Config lifecycle](./config-lifecycle.md)
- [ADR-002: Trait-driven extensibility](./decisions/ADR-002-trait-driven-extensibility.md)
- [ADR-004: Tool shared state ownership](./decisions/ADR-004-tool-shared-state-ownership.md)

Key code entry points:

- Tool trait and result shape: `crates/zeroclaw-api/src/tool.rs`
- Observer tool events: `crates/zeroclaw-api/src/observability_traits.rs`
- Turn execution context: `crates/zeroclaw-runtime/src/agent/turn/execution.rs`
- Turn engine run sheet and loop: `crates/zeroclaw-runtime/src/agent/turn/mod.rs`
- Tool-call preparation and approval: `crates/zeroclaw-runtime/src/agent/turn/call_prep.rs`
  and `crates/zeroclaw-runtime/src/agent/turn/approval_gate.rs`
- Tool dispatch: `crates/zeroclaw-runtime/src/agent/tool_execution.rs`
- Tool receipts: `crates/zeroclaw-runtime/src/agent/tool_receipts.rs`
- Result collection/history append: `crates/zeroclaw-runtime/src/agent/turn/results_collect.rs`
  and `crates/zeroclaw-runtime/src/agent/turn/history_append.rs`
- Approval manager: `crates/zeroclaw-runtime/src/approval/mod.rs`
- Scoped tool assembly and deferred MCP activation:
  `crates/zeroclaw-runtime/src/tools/scoped.rs`
