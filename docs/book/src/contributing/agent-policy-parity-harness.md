# Agent-policy parity

An agent's policy - which tools it may call, when it must ask for approval, its
runtime budgets, its memory scope, and its skills - must be enforced identically
no matter which code path assembles and runs the turn. ZeroClaw builds a turn
through several distinct construction paths, and historically each applied the
policy itself. When the same policy is re-derived in several places, a setting
honored on one path can be silently skipped on another.

[#8120](https://github.com/zeroclaw-labs/zeroclaw/pull/8120) (MCP tools from one
agent appearing in another agent's session) was one such divergence: the
per-agent tool scoping that the channel path applied was missing on another
construction path. The agent-policy parity harness exists to make that class of
bug visible before it ships, and the trunk it builds on
([#8156](https://github.com/zeroclaw-labs/zeroclaw/pull/8156)) exists to make it
impossible by construction.

## The construction paths

A turn's engine inputs (the tool registry, the approval manager, the resolved
runtime knobs) are assembled at several distinct sites:

| Path | Where it builds the engine input |
|---|---|
| Channel | the channels orchestrator |
| RPC | the `Agent` struct (`from_config` / `turn`) |
| Gateway | the gateway server |
| `loop_::run` | non-interactive runs: cron jobs, the daemon heartbeat, sub-agent spawning |
| Delegate | sub-agent delegation |
| SOP live nested step | `drive_live_sop_actions`: a step delegating to a different agent re-assembles that agent's engine input in-flight |

Each path must hand the engine the same policy for the same agent config. The
parity harness asserts exactly that: a setting enforced on one path is enforced
on every path. The SOP live nested-step path is a sub-turn inside an already-running
turn: when a step names a different agent, its complete execution contract is
re-assembled through the same seam rather than inherited from the parent turn --
gated tools, security policy, MCP scope, provider binding and temperature, resolved
runtime controls, and an approval manager carrying the step agent's risk profile
under the parent surface's interactivity mode. The step runs on an explicit child
transcript (its own system prompt plus the step context; the parent conversation
never reaches the step agent's provider), and its records stamp the step agent as
the acting identity with the delegating agent as parent correlation. A path that
cannot re-assemble fails the cross-agent step closed.

## The parity matrix

For each policy setting and each construction path, the setting is either
enforced, partially enforced, or not applied. The matrix of (setting x path) is
an audited divergence record: where each setting is and is not applied, verified
against the source. A "gap" cell is a setting a path omits.

Under the project's governing principle, a gap is a defect, not a default:
**omission is not a grant.** A construction path that fails to apply a
restriction has widened the agent's authority by accident, which is precisely
the failure #8120 was an instance of.

## The convergence target: one resolution seam

The structural fix is to stop re-deriving the policy per path. #8156 introduced
the `ResolvedAgentExecution` carrier - a behavior-neutral regrouping of the engine's
per-agent inputs into one bundle (`agent/turn/execution.rs`). This change adds its
`ResolvedAgentExecution::resolve` constructor and routes every production turn path
through it (grouping the inputs into `ResolvedIo` + `ResolvedRuntimeKnobs` layers), so
the bundle is produced in one seam rather than assembled inline at each site. Today
`resolve()` spreads already-resolved inputs (behavior-neutral); later surface PRs move
the per-field resolution (tools via a scoped registry, approval, the runtime knobs)
into it and seal the inputs. With that resolution and sealing in place:

- there is exactly one place a setting is applied, so there is nothing to diverge;
- a newtype with a private field (for example a scoped tool registry that only the
  resolver can mint) makes handing the engine an unresolved policy a compile error.

The end state is that the divergence is uncompilable rather than merely tested
against. Current/future boundary: `ResolvedAgentExecution`, its `resolve()`
constructor, and the `ResolvedIo` / `ResolvedRuntimeKnobs` input layers all exist on
`master` and every production path constructs through them; the TOOL surface now has
its gated constructor too (`ScopedToolRegistry::assemble`, below, with the gateway as
its first consumer); absorbing the remaining surfaces' per-field resolution into
`resolve()`, and sealing the bundle's fields behind it, are the work later surface
PRs do.

## The tool-assembly seam (Epic A, the first surface)

The per-agent tool registry is the first surface with a single gated constructor:
`ScopedToolRegistry::assemble` (`crates/zeroclaw-runtime/src/tools/scoped.rs`). The
registry has historically been assembled by hand at six construction sites - the
reason the built-in filter and MCP scoping had to be patched per-site (#7064,
\#6960, #8120). `assemble` applies, in order: the agent's `config.peripherals`
(when connected - see the knob below), the built-in `allowed_tools`/
`excluded_tools` filter, the ACP memory strip, MCP server scoping per `mcp_bundles`
plus per-tool gating (eager or deferred; omission is not a grant) with the MCP
capability tools and pinned-resources prompt section, and skill registration under
the same `SecurityPolicy` (a site with no skills passes an empty slice - the
gateway does, until the Epic F loader unification).

Per-site variation is expressed as data, never as a skipped security step. The
`ScopedAssembly` knobs only narrow or withhold - none can widen what the policy
grants:

- `caller_allowed` - a per-run allowlist (the `run()` path); intersects with, never
  overrides, the policy filter and the MCP tool-access policy.
- `connect_mcp` - `false` on the ACP fast-boot path: MCP servers are neither
  resolved nor connected, so nothing is granted.
- `connect_peripherals` - `false` on listing-only surfaces: loading peripherals
  physically connects hardware (exclusive serial holds), which a registry no turn
  runs against must never do.
- `exclude_memory` - the ACP memory-tool strip.

Cut-over status (the strangle, one site per PR): the **gateway** (#8640),
`loop_::run` (#8700), and `process_message` (#8701) all construct through
`assemble` today. The gateway cut-over - both its registry builders, the
dashboard-agent seed and the per-agent `/api/tools` listings - closed the
gateway's filter gap by construction: its listings previously showed
unfiltered built-ins the agent's policy denies (live gateway chat resolves
through `process_message`, which already filtered), plus a `tool_search` stub
even when policy denied every deferred MCP tool. One scoping note keeps the
listings claim honest: peripherals are excluded from listings by design
(`connect_peripherals: false` - enumerating them without connecting hardware
is a future refinement). The `process_message` cut-over closed a second,
independent divergence: it previously filtered built-ins through
`filter_channel_builtin_tools`, a variant that admitted the canonical
read-only defaults past `allowed_tools` at non-Full autonomy, while every
other path applied the plain `apply_policy_tool_filter`. #8701 retired that
variant, so every path now applies the same plain filter (ledger A4, backed
by an in-file positive parity test rather than a divergence characterization).

The remaining hand-rolled sites - the channels orchestrator (`start_channels`),
`Agent::from_config`, and the delegate independent-target builder
(`independent_agentic_tools_for_target`, added by #8239 while this program was
in flight - the recurrence the seal exists to end) - migrate in follow-up PRs.
Once all sites mint through `assemble`, the engine's tools field
seals to `ScopedToolRegistry` (a private-field newtype only `assemble` constructs),
and handing the engine an unscoped registry - or quietly re-inlining a construction
site, as a cross-merge did to the channel path once already - becomes a compile
error instead of a review catch. Until that seal, cross-site parity for the
not-yet-migrated sites remains by convention; what the seam guarantees today is
that every path routed through it shares one implementation.

## The harness

The parity harness lives at `crates/zeroclaw-runtime/src/agent/parity.rs`, a
`#[cfg(test)]` sibling of the `#7415` `safety_net.rs` turn-engine oracle, reusing
its fixtures. It carries an INDEX of parity rows - each naming its owner epic, a
public tracking reference, and the test (or tracked-divergence record) that backs
it - plus two layers of tests. The index deliberately encodes no per-path verdict
grid: a static grid of hand-written cells would itself be data that goes stale
when another PR changes a path, with no test noticing - the very failure this
program exists to end. So the enforceable claims live only in the tests; a
meta-test enforces the index's bookkeeping (owner, tracking, and evidence
present), nothing more. The human-readable (setting x path) grid lives in this
page. The two test layers:

- **L1 engine locks**: when a setting reaches `run_tool_call_loop`, the engine
  honors it (e.g. an `excluded_tools` entry never executes, even if the model
  calls it).
- **L2 path-parity** is what asserts a setting resolves the same on every
  construction path. Where a surface already resolves through one seam, its L2
  test is a positive parity assertion. Where a confirmed divergence has no single
  seam yet, it ships as an always-running characterization test that pins the
  divergence as it exists (asserting the two paths currently differ) - so when the
  owning epic unifies the semantic, that assertion fails in the same PR and must be
  rewritten into the positive parity assertion. The divergence can change only
  loudly, never silently. There are no `#[ignore]`d specs: a known-failing ignored
  test never runs in CI and protects nothing, so the goal is carried as a live
  assertion of the current state.

It grows one surface at a time and asserts only what no other test covers:

- A surface (tools, approval, runtime budgets, context and history, memory,
  skills) is strangled into `resolve` one PR at a time.
- That PR adds the surface's parity test: given one agent config, every
  construction path hands the engine the same resolved value for the setting.
- Behaviors already covered by a primitive's own unit tests, or by the
  `safety_net` engine oracle, are not restated. The harness adds only the
  cross-path parity assertion, which is the property no per-primitive test makes.

Until a surface has a single resolution seam, there is nothing to assert parity
against, so its row stays in the divergence record as an always-running
divergence characterization rather than as a premature green test - never as an
`#[ignore]`d spec, consistent with the no-ignored-specs rule above.

## Adding a surface (the workflow each future surface PR follows)

With `resolve()` in place (see above), each surface PR follows these steps:

1. Move the surface's resolution and wiring from the construction sites into
   `ResolvedAgentExecution::resolve`; delete the per-site copies.
2. Add a parity test: build a distinctive agent config, drive each construction
   path, and assert the engine receives the identical resolved value.
3. Flip the surface's row to enforced-on-every-path.
4. Keep the strangle behavior-neutral elsewhere: the `safety_net` oracle and the
   primitives' own unit tests stay green.
