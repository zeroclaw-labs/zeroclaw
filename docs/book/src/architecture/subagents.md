# SubAgents

A SubAgent is an **ephemeral child run** spawned by a parent agent that inherits the parent's identity by default: same agent alias, same `SecurityPolicy`, same memory allowlist, same configured model provider, same tool registry. Auditable as a child via a tracing span `agent.<alias>.subagent.<run_id>`.

SubAgents are not a separate configuration concept. There is no `[subagents.*]` block in the schema. Every SubAgent's identity is whichever parent's agent loop spawned it.

## When to use a SubAgent vs `delegate`

Two tools sit nearby. They are not interchangeable.

- **`spawn_subagent`** — runs the SAME agent again under its own identity for a focused subtask. The child sees the parent's full permissions envelope minus any narrowing. Use when the parent wants to scope an internal subtask out of its main conversation history without changing identity.
- **`delegate`** — hands the request off to a DIFFERENT configured agent (named by alias). The target agent runs under its own identity and model provider, but delegation is gated: the caller's risk profile must set `delegation_policy mode = "allow"` (default is `"forbidden"`), AND the target must share the **same** risk profile as the caller. Use when a sibling agent on the same trust tier is the right specialist for the work. See [Delegation gating](#delegation-gating) below.

This page documents `spawn_subagent` end to end. `delegate` lives at `crates/zeroclaw-runtime/src/tools/delegate.rs` and is a separate surface.

## How a SubAgent is instantiated

Two spawn sites converge on `SubAgentSpawn` (`crates/zeroclaw-runtime/src/subagent/mod.rs:97`):

1. **From an agent loop**: the model calls the `spawn_subagent` tool with a `prompt` string. The tool is registered like any other in the registry (`crates/zeroclaw-runtime/src/tools/mod.rs:437`).
2. **From cron**: `JobType::Agent` jobs run through `run_agent_job` (`crates/zeroclaw-runtime/src/cron/scheduler.rs:339`) which builds the same `SubAgentContext` but flags the child as a top-level run (not a SubAgent) so it can itself spawn one level of subagent.

Both paths invoke:

```rust
SubAgentSpawn::for_agent(config, parent_alias)?     // resolve parent identity
    .build(SubAgentOverrides::default())?           // validate any narrowing
```

`for_agent` reads the parent's `risk_profile` and `[agents.<alias>.workspace.read_memory_from]` to build the inherited allowlist; the parent's own alias is always added so a SubAgent always sees its parent's own memory rows. `build` applies optional narrowing (see [Permission inheritance](#permission-inheritance) below) and returns a validated `SubAgentContext`.

## Lifecycle

Synchronous, in-process, single tokio runtime. Nothing crosses the process boundary.

1. Parent's tool loop dispatches `spawn_subagent`. The tool reads its `prompt` argument, refuses if empty.
2. The tool checks two guards in order:
   - **Depth-1 cap.** If the calling run was itself a SubAgent (`AgentRunOverrides.is_subagent == true`), refuse with `"spawn_subagent: a subagent may not spawn its own subagents (depth-1 cap)"`. SubAgents cannot recurse.
   - **`risk_profile.allowed_tools` gate.** If the parent's `[risk_profiles.<alias>].allowed_tools` does not list `spawn_subagent`, or `excluded_tools` lists it, refuse with a message naming the parent alias.
3. The tool calls `SubAgentSpawn::for_agent` + `build`. Failures (unknown parent alias, escalating override) surface as `ToolResult { success: false, error: "subagent spawn failed: ..." }`.
4. The tool constructs `AgentRunOverrides { security, memory: None, is_subagent: true }` and awaits `crate::agent::run` (`crates/zeroclaw-runtime/src/agent/loop_.rs:2295`) inside a tracing scope keyed `subagent-<uuid>`. The parent's `tool` execution **blocks** until the child returns.
5. The child agent loop runs to completion. Its tool registry is built fresh, with `is_subagent_caller: true` flowing into its own `SpawnSubagentTool` so any attempt to recurse is rejected at the same depth-1 gate.
6. The child returns `Result<String>`. The parent's `spawn_subagent` tool wraps it:
   - Success: `ToolResult { success: true, output: <child's final response>, error: None }`. Empty output is replaced with the literal `"subagent completed without output"`.
   - Failure: `ToolResult { success: false, error: Some("subagent run failed: ...") }`.
7. The parent's tool loop continues with that `ToolResult` in its conversation context. The child's intermediate turns and tool calls are NOT replayed into the parent's history; only the final response surfaces.

## What gets delivered back upstream

One thing: the child's **final assistant message**, as a string, wrapped in `ToolResult.output`.

- The child's tool calls, intermediate reasoning turns, and any memory writes the child performed are observable in the structured logs under the child's tracing span but do not enter the parent's conversation history.
- The child's session lives under the path `subagent-<uuid>` (or `cron-<uuid>` for cron-spawned runs). This is the conversation-history key, not a filesystem location — it isolates the child's history from the parent's.
- Memory writes performed by the child are written to the parent's identity (same agent UUID at the SQL/Postgres backends; same workspace dir for Markdown). Cron-spawned runs disable `memory.auto_save` so opt-in writes still work but routine recall doesn't accumulate.

There is no streaming or partial-progress channel back to the parent. Long-running SubAgents stall the parent's tool execution for their full duration; there is no per-call timeout knob.

## Permission inheritance

A SubAgent inherits the parent's permissions verbatim unless the spawn site supplies a narrowing `SubAgentOverrides`. Today both in-tree spawn sites pass `SubAgentOverrides::default()` (inherit everything). The override surface is shipped and validated; a future caller-supplied narrowing path drops in without runtime changes.

Inheritance axis by axis:

1. **`SecurityPolicy`** — inherited by `Arc<SecurityPolicy>` cloning. Override path (`SubAgentOverrides::policy = Some(policy)`) runs `SecurityPolicy::ensure_no_escalation_beyond` (`crates/zeroclaw-config/src/policy.rs:2051`) and rejects any field that adds privilege the parent doesn't have. Validated axes include autonomy level, allowed_roots (rw + ro + write-only), allowed_commands, workspace_only, forbidden_paths in the parent ⊆ child direction, shell_env_passthrough, `max_actions_per_hour`, `max_cost_per_day_cents`, `shell_timeout_secs`, `block_high_risk_commands`, and `require_approval_for_medium_risk`. Rejections chain a precise `EscalationViolation` so diagnostics name the offending field.
2. **Action / cost budgets** — `PerSenderTracker` is shared between parent and child by `Arc` clone. Inherit-verbatim path: the child holds the same `Arc<SecurityPolicy>` so writes to `record_action()` / `record_cost()` hit the same bucket. Override path: `SubAgentSpawn::build` copies the parent's `tracker` field into the narrowed child policy explicitly. **A SubAgent cannot bypass `max_actions_per_hour` or `max_cost_per_day_cents` by spawning** — the limit is shared.
3. **Tool registry** — the child's registry is built fresh by `tools::all_tools_with_runtime` under the inherited policy. The registry then passes through `apply_policy_tool_filter` (`crates/zeroclaw-runtime/src/agent/loop_.rs`), which drops any tool whose name fails either gate:
   - The policy's `allowed_tools` / `excluded_tools` (sourced from the parent's `risk_profile`).
   - The caller-supplied `allowed_tools` argument to `agent::run`.
   `spawn_subagent` is in the registry but its `is_subagent_caller` flag is set to `true` for the child, so the depth-1 refusal fires before any spawn work.
4. **Memory allowlist** — a `HashSet<String>` of sibling agent **aliases** (the `[agents.<alias>]` config keys). Inherited from the parent's `workspace.read_memory_from` plus the parent's own alias. Override path (`SubAgentOverrides::allowed_agent_aliases`) is validated as a subset; any alias not on the parent's list is rejected by name. The parent's own alias is always re-added so a SubAgent always sees its parent's rows.
5. **Model provider** — inherited from the parent's `[agents.<alias>] model_provider` resolution. Temperature comes from the parent's provider entry (`config.model_provider_for_agent(parent_alias).and_then(|e| e.temperature)`).
6. **Identity at the data layer** — same UUID in the `agents` table (SQL backends), same workspace dir for Markdown, same secret store. The parent-vs-child distinction is purely observability: a separate tracing span and a separate conversation-history session key.

## How a user makes one fire

You don't call these tools yourself; the bot does, from inside its turn. As a user, you influence the bot's choice with how you phrase the request. There is no special command, no slash-syntax, and no JSON the user types. Whether the model picks `spawn_subagent` or `delegate` depends on its system prompt, the tool's `description` text (visible to the model), and the user's wording. **Phrasing influences; it does not force.**

What CAN be made deterministic is **availability**: tools that aren't in the parent agent's registry can't be picked. That gate lives in `[risk_profiles.<alias>].allowed_tools`. If the alias listed for the parent agent's `risk_profile` doesn't include `spawn_subagent`, the model never sees it. Same for `delegate`. Restart the daemon after editing the config.

```toml
[risk_profiles.frontline]
allowed_tools = ["shell", "file_read", "memory_recall", "spawn_subagent", "delegate"]
```

What's verifiable end-to-end:

1. The literal output strings the tool returns to the model on each path (success, refusal, failure). Quoted verbatim below, sourced from `tools/spawn_subagent.rs` and `tools/delegate.rs`.
2. The literal config knobs that change behavior (`allowed_tools`, `max_delegation_depth`, etc.).
3. The structured tracing span shape that scopes everything emitted during the child run.

What's NOT verifiable from these docs:

1. Whether your specific bot, on your specific model, on your specific system prompt, will pick the tool when asked "Spawn a subagent to ..." Wording moves the needle; outcomes vary. If the bot doesn't pick the tool, the most reliable lever is to extend the bot's system prompt with explicit instructions ("When asked for a focused subtask, use the `spawn_subagent` tool").
2. The exact text the bot writes to you in its final reply. The bot reads the tool's output and **generates its own** reply on top. The tool's output text may be quoted, paraphrased, or summarized.

### `spawn_subagent`: refusal strings the model sees

These are exact, sourced from `crates/zeroclaw-runtime/src/tools/spawn_subagent.rs`. The model receives them as the tool's error string and reacts. The user-visible bot reply is whatever the model writes next; it commonly references or echoes the refusal.

1. Empty/missing `prompt` argument: `Missing or empty 'prompt' parameter`
2. Caller is itself a SubAgent (depth-1 cap): `spawn_subagent: a subagent may not spawn its own subagents (depth-1 cap)`
3. Parent's `risk_profile.allowed_tools` excludes `spawn_subagent`: `spawn_subagent: refused — agent '<parent_alias>' risk_profile does not list spawn_subagent in allowed_tools`
4. Unknown parent alias / spawn build error: `subagent spawn failed: <wrapped error>`
5. Child run returned an error: `subagent run failed: <wrapped error>`

On success, the tool's output IS the child's final response text. If the child returned an empty string, the output is the literal placeholder: `subagent completed without output`. There is no fixed prefix to grep for in the success case.

### `spawn_subagent`: how to verify it actually fired

Tail your log. The tool-spawned child runs inside a `scope!` that emits a tracing span named `zeroclaw_scope` (with target `zeroclaw_log_internal_scope`) carrying `agent_alias=<parent>` and `session_key=<uuid>`. Every log line emitted during the child run carries those fields. The parent's own turn has its own `session_key`; a NEW `session_key` value appearing mid-turn for the same `agent_alias` is the signal that a SubAgent ran. The child's conversation-history session path is `subagent-<uuid>` (filesystem-ish identifier, distinct from the tracing field).

Cron-launched agent jobs use a different, more explicit span name: `subagent` (literal) with fields `category="cron"`, `agent_alias=<owning agent>`, `cron_job_id=<id>`, `run_id=<uuid>`, `spawn_site="cron"`. Cron paths are trivially greppable: `grep 'spawn_site="cron"' zeroclaw.log`. Note that cron-launched runs are top-level (`is_subagent=false`); they may themselves call `spawn_subagent` once.

This is a thin signal for the agent-loop spawn path. A dedicated "subagent started / completed" record routed through `attribution_span!(tool)` is tracked as a code-side follow-up — once the agent loop wraps tool execution in an attribution span, every `record!` inside the tool will carry `tool=spawn_subagent` automatically and the question becomes a trivial grep.

### Delegation gating

`delegate` enforces two gates in `crates/zeroclaw-runtime/src/tools/delegate.rs` before a target agent runs, in this order:

1. **`delegation_policy.mode`** — the caller's risk profile must permit delegation. `[risk_profiles.<alias>].delegation_policy` is `{ mode = "forbidden" }` by default; set `mode = "allow"` to permit delegation at all. When forbidden, the refusal is:
   ```
   delegation is forbidden by the caller's delegation_policy; set [risk_profiles.<caller_profile>].delegation_policy mode = "allow"
   ```
   This is editable in the gateway dashboard and zerocode at **Config → Risk profiles → `<profile>` → `delegation_policy.mode`** (a forbidden/allow select).

2. **Shared risk profile** — the target agent must use the **same** risk profile as the caller. Delegation does not cross trust tiers: an agent on `hardened` cannot delegate to an agent on `permissive`. When they differ, the refusal is:
   ```
   delegate target "<target>" uses risk profile "<target_profile>", but delegation requires the same risk profile as the caller ("<caller_profile>")
   ```

Because reachability is gated by the shared risk profile, the advertised roster (the `agent` parameter's enum in the tool schema) lists only the configured agents that share the caller's risk profile, minus the caller itself — and only when `delegation_policy.mode = "allow"`. There is no separate per-agent allow-list: the shared profile *is* the allow-list.

### `delegate`: output strings the model sees

Exact, sourced from `crates/zeroclaw-runtime/src/tools/delegate.rs`.

1. Synchronous success: output begins with `[Agent '<target>' (<provider_type>/<model>)]\n` followed by the target agent's response. If the target returned an empty string, the body is the literal `[Empty response]`.
2. Synchronous failure: error field begins with `Agent '<target>' failed: <wrapped error>`.
3. Synchronous timeout (when the target's runtime profile sets `delegation_timeout_secs`): error field is `Agent '<target>' timed out after <N>s`.
4. Background spawn success: output is the three-line literal
   ```
   Background task started for agent '<target>'.
   task_id: <uuid>
   Use action='check_result' with task_id='<uuid>' to retrieve the result.
   ```
   The result file lives at `<workspace>/delegate_results/<uuid>.json`. While running, the file's `status` field is `Running`; terminal states are `Completed`, `Failed`, or `Cancelled`.
5. `action="check_result"` with an unknown task id: error is `No result found for task_id '<uuid>'`.
6. Parallel fan-out output: begins with `[Parallel delegation: <N> agents]\n\n`, followed by per-agent blocks separated by `\n\n`, each block beginning with `--- <target> (success=<bool>) ---\n`. On per-agent failure the inner block is `--- <target> (success=false) ---\nError: <wrapped error>`.
7. Unknown target agent: error is `Unknown agent '<target>'. Available agents: <comma-separated list>`.
8. Depth exceeded (controlled by the parent's `runtime_profile.max_delegation_depth`, default 3): error is `Delegation depth limit reached (<depth>/<max>).`
9. Unknown action: error is `Unknown action '<value>'. Use delegate/check_result/list_results/cancel_task.`

### `delegate`: how to verify it actually fired

`delegate` does not emit a dedicated tracing span today. The signal is the **target** agent's loop appearing in the log, which inherits whatever scope the parent's tool-call dispatch was inside. Background-mode spawns are easier to verify out-of-band: the result file `<workspace>/delegate_results/<uuid>.json` exists on disk and carries the target agent's `status` + `output` fields; `cat` or `jq` works without touching the log at all.

(Cron-launched agent jobs are a separate spawn site and use the explicit `subagent` span described above; `delegate` and cron are not the same path.)

### What's not in this page (intentionally)

1. Example conversation transcripts. Anything I wrote here describing "what the bot will say" would be model-dependent. The bot's reply is downstream of the tool's output, model, system prompt, and current conversation state — none of which this page controls. The verifiable layer is what the tool returns (above) and what the log captures.
2. A dedicated "subagent fired" / "delegate fired" log marker. Tracked as a code-side follow-up. Today, operators verify via the scope shape described above (which is the existing structural signal) and via the background-mode result file.

## Choosing between `spawn_subagent` and `delegate`

| | `spawn_subagent` | `delegate` |
|---|---|---|
| **Identity** | Same as parent (same UUID, same risk profile) | Target agent's identity (different alias, **same** risk profile — delegation requires it) |
| **Permission model** | Parent's policy verbatim (or narrowed subset) | Target agent's own policy (within the shared risk profile) |
| **Model provider** | Parent's | Target agent's configured provider |
| **Spawn depth** | Hard cap at 1 | Up to `runtime_profile.max_delegation_depth` (default 3) |
| **Background mode** | Not supported | `background: true` returns a `task_id` |
| **Parallel fan-out** | Not supported | `parallel: [...]` runs multiple targets concurrently |
| **Gating** | `risk_profile.allowed_tools` must list `spawn_subagent` | `allowed_tools` must list `delegate`, caller's `delegation_policy mode = "allow"`, and target shares the caller's risk profile |
| **Use when** | Internal subtask that should stay within the same identity | Want a different specialist (different model, different alias) on the **same trust tier** to handle the task |

## What's not supported

1. **Recursion beyond depth 1.** A SubAgent cannot spawn its own SubAgent. The cap is a hard refusal at the tool, not a budget. Cron-launched runs start at depth 0 and may spawn one level; agent-loop-launched SubAgents are at depth 1 and refuse further spawning.
2. **A separate identity for the child.** SubAgents share the parent's agent UUID. To run under a different identity, use `delegate` to hand off to a configured sibling agent.
3. **Per-spawn time budget.** There is no `timeout_secs` argument. The parent blocks for the full duration of the child run; cancellation has to flow through the broader interruption scope.
4. **Streaming progress back to the parent.** The parent sees the child's final response as a single string after completion.
5. **A `[agents.<alias>].subagent_*` config block.** The validator and override type ship today; the operator-facing config surface that plumbs caller-defined narrowing is not in this release. Both spawn sites pass `SubAgentOverrides::default()` until that surface lands.
