# Scoping Note: [10] Tool-Ownership Refactor and [4] History-Model Change

**Status:** Scoping only — not authorized for implementation.  
**Date:** 2026-05-30  
**Prerequisite for:** Streaming tool execution, reversible context collapse  

Both items are High-disruption, both require callers to opt in, both change ownership/structure. They share a shape and should be addressed as one workstream.

---

## [10] Tool-Ownership Refactor (Streaming Tool Execution)

**Core tension.** `run_tool_call_loop` takes `tools_registry: &[Box<dyn Tool>]` — a borrowed slice. `StreamingToolExecutor` stores `Arc<Vec<Box<dyn Tool>>>` and clones the Arc into each spawned task. These types are incompatible: you cannot construct an `Arc<Vec<...>>` from a `&[...]` without cloning every tool.

### Caller inventory

| Caller | Holds now | Needs to hold | Breakage |
|---|---|---|---|
| `run()` (loop_.rs) | `mut tools_registry: Vec<Box<dyn Tool>>`, passed as `&tools_registry` | `Arc<Vec<Box<dyn Tool>>>`, passed as `&arc_ref` for loop and `Arc::clone` for streaming executor | Signature change only; `run()` owns the Vec so wrapping in Arc is trivial. `register_skill_tools` and `retain()` mutate the Vec — these must happen before the Arc is constructed. |
| Orchestrator (orchestrator/mod.rs) | `tools_registry: Arc<Vec<Box<dyn Tool>>>` in `ChannelRuntimeContext`, already Arc-wrapped | No change needed | Already correct. Passes as `ctx.tools_registry.as_ref()` which coerces to `&[...]`. |
| `execute_agentic` (delegate.rs) | Builds `sub_tools: Vec<Box<dyn Tool>>` locally from `parent_tools: Arc<RwLock<Vec<Arc<dyn Tool>>>>` | Would need `Arc<Vec<...>>` if the sub-agent loop enables streaming execution | Each agentic delegate call constructs a fresh filtered Vec. Wrapping in Arc is cheap. |
| `partition_tool_calls` (tool_execution.rs) | `tools_registry: &[Box<dyn Tool>]` | **No change.** It only reads concurrency-safety metadata synchronously. `&Arc<Vec<...>>` auto-derefs to `&[...]`. | None. |
| `SwarmTool` (swarm.rs) | Does not use a tool registry directly. | Unaffected. | None. |

### Model-switch interaction

`ModelSwitchRequested` aborts the current loop call. The outer loop catches this, recreates the provider, and re-enters. Tools are not provider-bound, so no tool cancellation is needed. However, if `StreamingToolExecutor` has in-flight spawned tasks when the loop aborts, the executor is dropped; those tasks hold `Arc` clones and will run to completion or be cancelled via the `CancellationToken`.

**Open question:** Should a model switch forcibly cancel in-flight streaming tool tasks, or let them drain? Current design (cancellation token propagation) supports both; needs a policy decision.

### Recommended change surface

Change `run_tool_call_loop`'s parameter from `&[Box<dyn Tool>]` to `&Arc<Vec<Box<dyn Tool>>>`. The function can still deref to `&[...]` for internal use and pass `Arc::clone` when constructing a `StreamingToolExecutor`. All three callers already own or can trivially produce an Arc. Estimated: ~15 lines changed across 4 files, plus test fixtures.

---

## [4] History-Model Change (Context Collapse)

**Current design.** `ContextCollapser` maintains `CollapsedRegion` structs indexed by position in the raw history. `project()` takes `&[ChatMessage]` and returns a new `Vec<ChatMessage>` with collapsed regions replaced by summary messages. It never mutates the input.

### History mutation points in `run_tool_call_loop`

| Mutation | Should target raw or projected? |
|---|---|
| `history.push(ChatMessage::assistant/user(...))` — appending LLM responses, hook errors, tool results | Raw log. New messages always append to canonical history. |
| `fast_trim_tool_results(history, 4)` — truncates old tool output in-place | Raw log. Destructive compaction, separate from reversible collapse. |
| `emergency_history_trim(history, 4)` — drops oldest non-system messages | Raw log. Irreversible; interacts badly with collapse indices. |
| `trim_history(&mut history, ...)` — hard cap on message count | Raw log, but shifts all indices. |

### LLM consumption point

`prepare_messages_for_provider(history, ...)` currently receives the raw mutable history. With collapse, this call would receive `collapser.project(history)` — a cheap read-only operation producing a temporary Vec.

### Context pipeline interaction

`ContextPipeline::run()` takes `&mut Vec<ChatMessage>` and mutates history in-place. These stages operate on the raw log. Collapse is conceptually a higher-level operation that sits between the raw log and the LLM call — it does not belong inside the pipeline. The pipeline's destructive mutations (emergency trim, tool-result budget) would invalidate collapse region indices because they remove messages and shift positions.

### Index stability problem

`CollapsedRegion` stores `start` and `end` as absolute indices. Any mutation that removes or inserts messages before a collapsed region invalidates those indices:
- `emergency_history_trim` removes from the front, shifting all indices down. **Unsafe.**
- `fast_trim_tool_results` replaces content but does not change message count. **Safe.**
- `trim_history` removes from the front. **Unsafe.**

### Open questions requiring decisions

1. **Index remapping vs. message IDs.** Should regions track by index (requires remapping after every destructive trim) or by message UUID/sequence number (requires adding an ID field to `ChatMessage`)? Index remapping is fragile; IDs are cleaner but touch the provider layer.

2. **Pipeline ordering.** Should collapse run before or after the destructive pipeline stages? Before: the pipeline sees the projected (shorter) view and may skip trimming entirely (the desired outcome). After: indices are already broken. Recommendation: collapse replaces destructive stages for the LLM call path, while the raw log retains a separate hard cap.

3. **Reversibility under emergency trim.** If `emergency_history_trim` drops messages inside a collapsed region, the region's summary becomes the only record. Is that acceptable, or should collapse be promoted to permanent at that point?

4. **Session persistence.** `save_interactive_session_history` serializes the raw history. Should collapse state also be persisted so regions survive restarts?

### Estimated disruption

Medium. The `project()` call is a single insertion point (before `prepare_messages_for_provider`). The real complexity is index stability under destructive trims — this drives the decision between index-tracking and ID-based regions. No changes to the provider layer or tool execution are required.
