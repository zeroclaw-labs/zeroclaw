# Delegation & SubAgents

A SubAgent is an **ephemeral child run** spawned by a parent agent that inherits the parent's identity by default: same agent alias, same `SecurityPolicy`, same memory allowlist, same configured model provider, same tool registry. Auditable as a child via a tracing span `agent.<alias>.subagent.<run_id>`.

SubAgents are not a separate configuration concept. There is no `[subagents.*]` block in the schema. Every SubAgent's identity is whichever parent's agent loop spawned it.

## When to use `spawn_subagent` vs `delegate`

Two tools sit nearby. They are not interchangeable.

- **`spawn_subagent`**: runs the SAME agent again under its own identity for a focused subtask. The child sees the parent's full permissions envelope minus any narrowing. Use when the parent wants to scope an internal subtask out of its main conversation history without changing identity.
- **`delegate`**: hands the request off to a DIFFERENT configured agent (named by alias). The target agent runs under its own identity and model provider, but delegation is gated: the caller's risk profile must set `delegation_policy mode = "allow"` (default is `"forbidden"`), and the target must be reachable as either a same-profile peer or an explicit `delegates` entry. Explicit entries choose `mode = "bounded"` or `mode = "independent"`, which determines whether the caller's tool ceiling still applies. Use when another configured specialist should own the work. See [Delegation gating](#delegation-gating) below.

This page documents `spawn_subagent` end to end. `delegate` lives at `crates/zeroclaw-runtime/src/tools/delegate.rs` and is a separate surface.

## How a SubAgent is instantiated

Two spawn sites converge on `SubAgentSpawn` (`crates/zeroclaw-runtime/src/subagent/mod.rs:97`):

1. **From an agent loop**: the model calls the `spawn_subagent` tool with a `prompt` string. The tool is registered like any other in the registry (`crates/zeroclaw-runtime/src/tools/mod.rs`, `SpawnSubagentTool::new`).
2. **From cron**: `JobType::Agent` jobs run through `run_agent_job` (`crates/zeroclaw-runtime/src/cron/scheduler.rs`) which builds the same `SubAgentContext` but flags the child as a top-level run (not a SubAgent) so it can itself spawn one level of subagent.

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
   - **Risk-profile tool gate.** If the parent's `[risk_profiles.<alias>].allowed_tools` is non-empty and does not list `spawn_subagent`, or `excluded_tools` lists it, refuse with a message naming the parent alias.
3. The tool calls `SubAgentSpawn::for_agent` + `build`. Failures (unknown parent alias, escalating override) surface as `ToolResult { success: false, error: "subagent spawn failed: ..." }`.
4. The tool constructs `AgentRunOverrides { security, memory: None, is_subagent: true, suppress_memory_inject: true }` (the child's `SubTurn` origin already skips engine memory injection; the flag makes the opt-out explicit) and awaits `crate::agent::run` (`crates/zeroclaw-runtime/src/agent/loop_.rs`, `pub async fn run`) inside a tracing scope keyed `subagent-<uuid>`. The parent's `tool` execution **blocks** until the child returns.
5. The child agent loop runs to completion. Its tool registry is built fresh, with `is_subagent_caller: true` flowing into its own `SpawnSubagentTool` so any attempt to recurse is rejected at the same depth-1 gate.
6. The child returns `Result<String>`. The parent's `spawn_subagent` tool wraps it:
   - Success: `ToolResult { success: true, output: <child's final response>, error: None }`. Empty output is replaced with the literal `"subagent completed without output"`.
   - Failure: `ToolResult { success: false, error: Some("subagent run failed: ...") }`.
7. The parent's tool loop continues with that `ToolResult` in its conversation context. The child's intermediate turns and tool calls are NOT replayed into the parent's history; only the final response surfaces.

## What gets delivered back upstream

One thing: the child's **final assistant message**, as a string, wrapped in `ToolResult.output`.

- The child's tool calls, intermediate reasoning turns, and any memory writes the child performed are observable in the structured logs under the child's tracing span but do not enter the parent's conversation history.
- The child's session lives under the path `subagent-<uuid>` (or `cron-<uuid>` for cron-spawned runs). This is the conversation-history key, not a filesystem location, it isolates the child's history from the parent's.
- Memory writes performed by the child are written to the parent's identity (same agent UUID at the SQL/Postgres backends; same workspace dir for Markdown). Cron-spawned runs disable `memory.auto_save` so opt-in writes still work but routine recall doesn't accumulate.

There is no streaming or partial-progress channel back to the parent. Long-running SubAgents stall the parent's tool execution for their full duration; there is no per-call timeout knob.

### Multiple calls in one turn

The agent loop applies a per-turn duplicate-call guard: a tool called twice with identical arguments in the same turn normally has the second call skipped. `spawn_subagent` and `delegate` are **exempt** from that guard. Launching several with the same prompt (redundancy, sampling, fan-out) is an intentional pattern, not an accidental repeat, so each identical call runs and each result is returned. Without the exemption only the first identical call would execute and only its output would reach the model.

When parallel tool execution is enabled (`parallel_tools = true` in the runtime profile), multiple `spawn_subagent` calls in one turn run concurrently and every child's final response is returned to the parent, keyed to its own tool call. `delegate` has its own explicit fan-out via the `parallel: [...]` argument (see the output-strings section); that path spawns each target on its own task and aggregates all results.

## Permission inheritance

A SubAgent inherits the parent's permissions verbatim unless the spawn site supplies a narrowing `SubAgentOverrides`. Today both in-tree spawn sites pass `SubAgentOverrides::default()` (inherit everything). The override surface is shipped and validated; a future caller-supplied narrowing path drops in without runtime changes.

Inheritance axis by axis:

1. **`SecurityPolicy`**: inherited by `Arc<SecurityPolicy>` cloning. Override path (`SubAgentOverrides::policy = Some(policy)`) runs `SecurityPolicy::ensure_no_escalation_beyond` (`crates/zeroclaw-config/src/policy.rs`) and rejects any field that adds privilege the parent doesn't have. Validated axes include autonomy level, allowed_roots (rw + ro + write-only), allowed_commands, workspace_only, forbidden_paths in the parent ⊆ child direction, shell_env_passthrough, `max_actions_per_hour`, `max_cost_per_day_cents`, `shell_timeout_secs`, `block_high_risk_commands`, and `require_approval_for_medium_risk`. Rejections chain a precise `EscalationViolation` so diagnostics name the offending field.
2. **Action / cost budgets**: `PerSenderTracker` is shared between parent and child by `Arc` clone. Inherit-verbatim path: the child holds the same `Arc<SecurityPolicy>` so writes to `record_action()` / `record_cost()` hit the same bucket. Override path: `SubAgentSpawn::build` copies the parent's `tracker` field into the narrowed child policy explicitly. **A SubAgent cannot bypass `max_actions_per_hour` or `max_cost_per_day_cents` by spawning**, the limit is shared.
3. **Tool registry**: the child's registry is built fresh by `tools::all_tools_with_runtime` under the inherited policy. The registry then passes through `apply_policy_tool_filter` (`crates/zeroclaw-runtime/src/agent/loop_.rs`), which drops any tool whose name fails either gate:
   - The policy's `allowed_tools` / `excluded_tools` (sourced from the parent's `risk_profile`).
   - The caller-supplied `allowed_tools` argument to `agent::run`.
   `spawn_subagent` is in the registry but its `is_subagent_caller` flag is set to `true` for the child, so the depth-1 refusal fires before any spawn work. The same `is_subagent_caller` flag drops `model_switch` from the child's registry entirely: a SubAgent inherits the parent's model verbatim (see axis 5) and must not be able to switch the active model out from under the parent, so the tool is simply not offered to it.
4. **Memory allowlist**: a `HashSet<String>` of sibling agent **aliases** (the `[agents.<alias>]` config keys). Inherited from the parent's `workspace.read_memory_from` plus the parent's own alias. Override path (`SubAgentOverrides::allowed_agent_aliases`) is validated as a subset; any alias not on the parent's list is rejected by name. The parent's own alias is always re-added so a SubAgent always sees its parent's rows.
5. **Model provider**: inherited from the parent's `[agents.<alias>] model_provider` resolution. Temperature comes from the parent's provider entry (`config.model_provider_for_agent(parent_alias).and_then(|e| e.temperature)`). This inheritance is enforced, not merely a default: `model_switch` is excluded from the SubAgent's tool registry (see axis 3), so a SubAgent cannot switch its own model. To run a subtask on a different model, use `delegate` to a sibling agent whose `model_provider` names that model.
6. **Identity at the data layer**: same UUID in the `agents` table (SQL backends), same workspace dir for Markdown, same secret store. The parent-vs-child distinction is purely observability: a separate tracing span and a separate conversation-history session key.

## How a user makes one fire

You don't call these tools yourself; the bot does, from inside its turn. As a user, you influence the bot's choice with how you phrase the request. There is no special command, no slash-syntax, and no JSON the user types. Whether the model picks `spawn_subagent` or `delegate` depends on its system prompt, the tool's `description` text (visible to the model), and the user's wording. **Phrasing influences; it does not force.**

What CAN be made deterministic is **availability**: tools that aren't in the parent agent's registry can't be picked. The risk-profile gate lives in `[risk_profiles.<alias>].allowed_tools` and `[risk_profiles.<alias>].excluded_tools`. A non-empty `allowed_tools` list must include `spawn_subagent` or `delegate` for the model to see that tool; an empty `allowed_tools` list leaves tool availability unrestricted unless `excluded_tools` names the tool. Restart the daemon after editing the config.

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
3. Parent's risk-profile tool gate excludes `spawn_subagent`: `spawn_subagent: refused — agent '<parent_alias>' risk_profile does not list spawn_subagent in allowed_tools`
4. Unknown parent alias / spawn build error: `subagent spawn failed: <wrapped error>`
5. Child run returned an error: `subagent run failed: <wrapped error>`

On success, the tool's output IS the child's final response text. If the child returned an empty string, the output is the literal placeholder: `subagent completed without output`. There is no fixed prefix to grep for in the success case.

### `spawn_subagent`: how to verify it actually fired

Tail your log. The tool-spawned child runs inside a `scope!` that emits a tracing span named `zeroclaw_scope` (with target `zeroclaw_log_internal_scope`) carrying `agent_alias=<parent>` and `session_key=<uuid>`. Every log line emitted during the child run carries those fields. The parent's own turn has its own `session_key`; a NEW `session_key` value appearing mid-turn for the same `agent_alias` is the signal that a SubAgent ran. The child's conversation-history session path is `subagent-<uuid>` (filesystem-ish identifier, distinct from the tracing field).

Cron-launched agent jobs use a different, more explicit span name: `subagent` (literal) with fields `category="cron"`, `agent_alias=<owning agent>`, `cron_job_id=<id>`, `run_id=<uuid>`, `spawn_site="cron"`. Cron paths are trivially greppable: `grep 'spawn_site="cron"' zeroclaw.log`. Note that cron-launched runs are top-level (`is_subagent=false`); they may themselves call `spawn_subagent` once.

This is a thin signal for the agent-loop spawn path. A dedicated "subagent started / completed" record routed through `attribution_span!(tool)` is tracked as a code-side follow-up, once the agent loop wraps tool execution in an attribution span, every `record!` inside the tool will carry `tool=spawn_subagent` automatically and the question becomes a trivial grep.

### Delegation gating

`delegate` enforces two gates in `crates/zeroclaw-runtime/src/tools/delegate.rs` before a target agent runs, in this order:

1. **`delegation_policy.mode`**: the caller's risk profile must permit delegation. `[risk_profiles.<alias>].delegation_policy` is `{ mode = "forbidden" }` by default; set `mode = "allow"` to permit delegation at all. When forbidden, the refusal is:
   ```text
   delegation is forbidden for caller "<caller>" by risk profile "<caller_profile>" delegation_policy; set [risk_profiles.<caller_profile>].delegation_policy mode = "allow"
   ```
   This is editable in the gateway dashboard and zerocode at **Config → Risk profiles → `<profile>` → `delegation_policy.mode`** (a forbidden/allow select).

2. **Reachability**: the target agent must be in the caller's reachable set, resolved by `Config::reachable_delegate_target_configs`. The reachable set is the union of two per-agent sources on `[agents.<caller>]`, minus the caller itself:

   - **same-profile peers**: every other agent sharing the caller's risk profile, included while `delegate_same_risk_profile = true` (the default). Set it `false` to opt the caller out of auto-allowing peers.
   - **explicit roster**: `delegates`, a possibly-empty list of targets the caller may delegate to even across risk profiles. String entries are convenient for manual editing and stand for bounded targets. Object entries make the mode explicit:
     ```toml
     delegates = [
       "reviewer",
       { agent = "sysadmin", mode = "independent" },
     ]
     ```
     When config is saved, every entry is written in object form with `mode = "bounded"` or `mode = "independent"`.
     Do not roll this config shape out before the daemon and UI binaries have been upgraded to a build that supports delegate modes. Older binaries expect `delegates` to contain only strings; an object entry makes the `agents` section invalid for that binary and the resilient loader drops the section so the repair surfaces can still start.

   When the target is outside that set, the refusal names the cause. For example:
   ```text
   delegate target "<target>" is not reachable from "<caller>": different risk profile (caller uses "<caller_profile>", target uses "<target_profile>"). delegate_same_risk_profile only reaches agents with the same risk profile; add an explicit [agents.<caller>].delegates entry with the intended mode, or change one agent's risk_profile.
   ```
   ```text
   delegate target "<target>" is not reachable from "<caller>": delegate_same_risk_profile is disabled and the target is not listed in [agents.<caller>].delegates
   ```
   ```text
   delegate target "<target>" is not reachable from "<caller>": the target agent is disabled
   ```

   A bounded target inherits the caller's action/cost tracker. When the bounded target shares the caller's risk profile, it also inherits the caller's session workspace boundary. A bounded **cross-profile** target is allowed when it is reachable through the caller's delegate roster and `delegation_policy`; it runs under the target's resolved policy, while agentic tool availability is capped by the caller's tool registry.

   An independent target is available only when explicitly listed with `mode = "independent"`. It still requires `delegation_policy.mode = "allow"` and reachability through `delegates`, but once selected it resolves the target agent's own policy without the caller's non-escalation ceiling, session workspace override, or action/cost tracker.

The advertised roster is included in the `agent` parameter description in the tool schema. It lists exactly this reachable set, and only when `delegation_policy.mode = "allow"`. Disabled agents (`enabled = false`) are never reachable, whether as same-profile peers or explicit `delegates` entries.

In bounded agentic delegation the sub-agent's tools are drawn from the caller's already-policy-filtered registry, intersected with the target's own `allowed_tools`. An **empty** `allowed_tools` on the target means "inherit": the sub-agent runs with the caller's full delegatable registry rather than being rejected. A non-empty list intersects with that registry. Either way the caller's registry is the ceiling: a bounded cross-profile target whose risk profile names a tool the caller was never granted does not receive it. Bounded delegation is therefore tool-bounded, not a full `SecurityPolicy::ensure_no_escalation_beyond` check. If that intersection is empty, the target still receives a normal agentic model turn with no tools.

In independent agentic delegation the sub-agent's tools are built from the target agent's own configured policy and runtime registry, like opening a fresh chat with that target. The parent registry is not used as the ceiling. The `delegate` tool is still removed from the child registry so agentic delegation cannot recurse through another `delegate` call.

Depth is capped per the parent's `runtime_profile.max_delegation_depth`. Set it to `1` to allow the top agent a single delegation hop with no further sub-delegation.

#### Agentic target tool policy

If the target agent's `[runtime_profiles.<target>].agentic = true`, `delegate` builds the target sub-loop's tool registry from either the parent's available tools (`mode = "bounded"`) or the target's own runtime registry (`mode = "independent"`). The target risk profile then filters that registry:

1. A configured empty `[risk_profiles.<target_profile>].allowed_tools` list leaves the selected registry unrestricted.
2. A non-empty `allowed_tools` list keeps only exact matching tool names.
3. `[risk_profiles.<target_profile>].excluded_tools` always subtracts from the result.
4. `delegate` is always removed from the child registry so agentic delegation cannot recurse through another `delegate` call.

This policy lives on the target, not the caller. Same-profile peers use the shared risk profile. Explicit cross-profile delegates use the target's risk profile after the reachability and delegation-policy gates. Bounded agentic delegates receive only the caller-capped tool registry intersected with the target's tool policy; independent agentic delegates receive the target-owned tool registry. A missing target risk profile refuses before the sub-loop starts. A configured profile that leaves zero executable child tools still permits a normal model turn with no tools.

### `delegate`: output strings the model sees

Exact, sourced from `crates/zeroclaw-runtime/src/tools/delegate.rs`.

1. Synchronous success: output begins with `[Agent '<target>' (<provider_type>/<model>)]\n` followed by the target agent's response. If the target returned an empty string, the body is the literal `[Empty response]`.
2. Synchronous failure: error field begins with `Agent '<target>' failed: <wrapped error>`.
3. Synchronous timeout (when the target's runtime profile sets `delegation_timeout_secs`): error field is `Agent '<target>' timed out after <N>s`.
4. Background spawn success: output is the three-line literal
   ```text
   Background task started for agent '<target>'.
   task_id: <uuid>
   Use action='check_result' with task_id='<uuid>' to retrieve the result.
   ```
   The result file lives at `<workspace>/delegate_results/<uuid>.json`. While running, the file's `status` field is `running`; terminal states are `completed`, `failed`, or `cancelled`.
5. `action="check_result"` with an unknown task id: error is `No result found for task_id '<uuid>'`.
6. `action="await_sessions"` with `task_ids: [<uuid>, ...]` waits for multiple background result files at once. The output is a JSON object with `status` (`complete` or `timeout`), `completed`, `pending`, `missing`, `failed`, and `results`. `timeout_ms` defaults to 30000 and is capped at 120000; on timeout the tool returns partial results and an error saying one or more tasks are still pending or missing. Duplicate task IDs are rejected.
7. Parallel fan-out output: begins with `[Parallel delegation: <N> agents]\n\n`, followed by per-agent blocks separated by `\n\n`, each block beginning with `--- <target> (success=<bool>) ---\n`. On per-agent failure the inner block is `--- <target> (success=false) ---\nError: <wrapped error>`.
8. Unknown target agent: error is `Unknown agent '<target>'. Available agents: <comma-separated list>`.
9. Depth exceeded (controlled by the parent's `runtime_profile.max_delegation_depth`, default 3): error is `Delegation depth limit reached (<depth>/<max>).`
10. Unknown action: error is `Unknown action '<value>'. Use delegate/check_result/list_results/cancel_task/await_sessions.`
11. Independent target whose risk profile has `always_ask` entries: error is `delegate target "<target>" cannot run in independent mode from "<caller>": risk profile "<profile>" has always_ask entries (<list>). See ZeroClaw docs, "Delegation & SubAgents" > "What's not supported".`
12. Agentic target with a missing target risk profile: error is `Agent '<target>' is agentic but risk_profile '<target_profile>' is not configured`.
13. Agentic target with zero executable child tools: no error is emitted for the empty tool set itself; the target receives a normal model turn without tools.

### `delegate`: how to verify it actually fired

`delegate` does not emit a dedicated tracing span today. The signal is the **target** agent's loop appearing in the log, which inherits whatever scope the parent's tool-call dispatch was inside. Background-mode spawns are easier to verify out-of-band: the result file `<workspace>/delegate_results/<uuid>.json` exists on disk and carries the target agent's `status` + `output` fields; `cat` or `jq` works without touching the log at all.

(Cron-launched agent jobs are a separate spawn site and use the explicit `subagent` span described above; `delegate` and cron are not the same path.)

### What's not in this page (intentionally)

1. Example conversation transcripts. Anything I wrote here describing "what the bot will say" would be model-dependent. The bot's reply is downstream of the tool's output, model, system prompt, and current conversation state, none of which this page controls. The verifiable layer is what the tool returns (above) and what the log captures.
2. A dedicated "subagent fired" / "delegate fired" log marker. Tracked as a code-side follow-up. Today, operators verify via the scope shape described above (which is the existing structural signal) and via the background-mode result file.

## Choosing between `spawn_subagent` and `delegate`

| | `spawn_subagent` | `delegate` |
|---|---|---|
| **Identity** | Same as parent (same UUID, same risk profile) | Target agent's identity (different alias; same-profile peer or an explicit cross-profile delegate) |
| **Permission model** | Parent's policy verbatim (or narrowed subset) | Bounded targets run under the target policy with the caller's agentic tool registry as ceiling; independent targets run under the target policy and target-owned registry |
| **Model provider** | Parent's | Target agent's configured provider |
| **Spawn depth** | Hard cap at 1 | Up to `runtime_profile.max_delegation_depth` (default 3) |
| **Background mode** | Not supported | `background: true` returns a `task_id` |
| **Parallel fan-out** | No built-in argument; multiple calls in one turn run concurrently when `parallel_tools = true` | `parallel: [...]` runs multiple targets concurrently |
| **Gating** | Non-empty `risk_profile.allowed_tools` must list `spawn_subagent`; `excluded_tools` must not list it | The caller's non-empty `risk_profile.allowed_tools` must list `delegate`; `excluded_tools` must not list it; caller's `delegation_policy mode = "allow"`; and the target is in the caller's reachable set (same-profile peer or explicit `delegates` entry) |
| **Use when** | Internal subtask that should stay within the same identity | Want a different configured specialist (different model, different alias) to own the task under bounded or independent delegation |

## What's not supported

1. **Recursion beyond depth 1.** A SubAgent cannot spawn its own SubAgent. The cap is a hard refusal at the tool, not a budget. Cron-launched runs start at depth 0 and may spawn one level; agent-loop-launched SubAgents are at depth 1 and refuse further spawning.
2. **A separate identity for the child.** SubAgents share the parent's agent UUID. To run under a different identity, use `delegate` to hand off to a configured sibling agent.
3. **Per-spawn time budget.** There is no `timeout_secs` argument. The parent blocks for the full duration of the child run; cancellation has to flow through the broader interruption scope.
4. **Streaming progress back to the parent.** The parent sees the child's final response as a single string after completion.
5. **A `[agents.<alias>].subagent_*` config block.** The validator and override type ship today; the operator-facing config surface that plumbs caller-defined narrowing is not in this release. Both spawn sites pass `SubAgentOverrides::default()` until that surface lands.
6. **Independent `delegate` targets with `always_ask`.** Independent delegation is blocked when the target agent's risk profile has non-empty `always_ask` entries. The runtime refuses before starting the target, including background and parallel delegation. This blocker remains until approval forwarding for independent child agents is supported by a future ZeroClaw version.
