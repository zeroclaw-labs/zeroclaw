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

Each path must hand the engine the same policy for the same agent config. The
parity harness asserts exactly that: a setting enforced on one path is enforced
on every path.

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
`master` and every production path constructs through them; absorbing each surface's
per-field resolution into `resolve()`, and sealing the bundle's fields behind it, are
the work later surface PRs do.

## The harness

The parity harness does not exist on `master` yet: today the runtime has
`agent/safety_net.rs` and `agent/turn/execution.rs`, not `agent/parity.rs`. The
harness will live in `crates/zeroclaw-runtime/src/agent/parity.rs`, a `#[cfg(test)]`
sibling of the `#7415` `safety_net.rs` turn-engine oracle - that is the intended
location if it remains the chosen shape. A future surface PR creates it; thereafter
it grows one surface at a time and asserts only what no other test covers:

- A surface (tools, approval, runtime budgets, context and history, memory,
  skills) is strangled into `resolve` one PR at a time.
- That PR adds the surface's parity test: given one agent config, every
  construction path hands the engine the same resolved value for the setting.
- Behaviors already covered by a primitive's own unit tests, or by the
  `safety_net` engine oracle, are not restated. The harness adds only the
  cross-path parity assertion, which is the property no per-primitive test makes.

Until a surface has a single resolution seam, there is nothing to assert parity
against, so its row stays in the divergence record as documentation rather than as
a premature test.

## Adding a surface (the workflow each future surface PR follows)

With `resolve()` in place (see above), each surface PR follows these steps:

1. Move the surface's resolution and wiring from the construction sites into
   `ResolvedAgentExecution::resolve`; delete the per-site copies.
2. Add a parity test: build a distinctive agent config, drive each construction
   path, and assert the engine receives the identical resolved value.
3. Flip the surface's row to enforced-on-every-path.
4. Keep the strangle behavior-neutral elsewhere: the `safety_net` oracle and the
   primitives' own unit tests stay green.
