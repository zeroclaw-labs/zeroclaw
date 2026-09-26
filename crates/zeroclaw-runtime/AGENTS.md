# zeroclaw-runtime — Transitional Holding Crate

This crate is a **temporary holding area**, not a permanent home. It contains 126K LOC of subsystems extracted from the original monolith that have not yet been decomposed into their final crate structure.

Do not add new functionality here, unless the Core Team has granted a recorded exception (see below). The RFC's Phase 2-4 roadmap defines the decomposition plan: agent loop, gateway, channels orchestrator, daemon, cron, security, observability, hardware, TUI, skills, and doctor will each be extracted into dedicated crates or converted to WASM plugins.

## Exceptions

Extraction is the default. The Core Team may grant a bounded exception when immediate extraction would require a disproportionate refactor, or would establish a crate boundary the roadmap does not intend. See [ADR-016](../../docs/book/src/architecture/decisions/ADR-016-holding-crate-exceptions.md).

An exception must name its permitted scope, intended destination, approving authority, and either an expiry or a review condition, and must be recorded before the feature it covers merges. An expiry ends the permission, so further additions need Core Team renewal; it does not require removing code that already landed. A review condition only obliges reconsideration.

An exception is granted by adding its row to the table below through normal review, approved by the Core Team. The row is the record: a decision that lives only in a review thread has not been made. A feature pull request cannot grant itself one: the contract it would be waiving is the one constraining it.

An exception requires a concrete supported use case. Code with no receiving caller does not qualify; retirement or an explicit ownership decision is the right answer there.

An exception permits continued work on a subsystem already held here. It never permits introducing a new subsystem, and it does not generalise from one subsystem to another.

### Active exceptions

| Scope | Destination | Approved by | Expires or reviewed |
| --- | --- | --- | --- |
| Provider-image recovery in `src/agent/agent.rs`, `src/agent/loop_.rs`, `src/agent/turn/{mod,provider_call,stream_consume}.rs`, and `src/tools/send_message_to_peer.rs`, limited to the bounded recovery and live-state wiring proposed in #10480 | Agent-loop owner in the planned `zeroclaw-kernel` extraction | @JordanTheJet, [approved in #10949 on 2026-09-18](https://github.com/zeroclaw-labs/zeroclaw/pull/10949#pullrequestreview-5253299366) | Review at the agent-loop extraction design review, or before expanding recovery eligibility or retained state |
| Peer-inbox lifecycle in `src/tools/send_message_to_peer.rs` and `src/control_plane/{task_registry.rs,task_store_sqlite.rs}`, limited to the #9597 `TaskRecord` registration, terminal settlement, and focused lifecycle tests around existing authorized agent-peer dispatch | Agent-loop and task-lifecycle owners in the planned `zeroclaw-kernel` extraction | @JordanTheJet, [approved in #11030 on 2026-09-21](https://github.com/zeroclaw-labs/zeroclaw/pull/11030#pullrequestreview-5265344105) | Review when agent-loop or control-plane extraction begins, or before expanding peer lifecycle scope |
| Pre-turn tool-elicitation hints in `src/agent/agent.rs`, `src/agent/loop_.rs`, `src/agent/tool_execution.rs`, and `src/agent/turn/{elicitation,mod,tool_specs}.rs`, limited to the default-off, admitted-channel turn flow proposed in #10325; no new tool execution or routing authority | Agent-loop owner in the planned `zeroclaw-kernel` extraction | @IftekharUddin, [approved in #11047 on 2026-09-24](https://github.com/zeroclaw-labs/zeroclaw/pull/11047#pullrequestreview-5295402310) | Review at the agent-loop extraction design review, or before widening trigger sources, retained hint state, or tool execution authority |
| Prefix-fingerprint instrumentation in `src/agent/turn/mod.rs` and `src/agent/turn/provider_call.rs`, limited to #10990: compute `system_chars`, `system_sha256`, `tools_count`, and `tools_sha256` from the finalized logical-request inputs and attach them to the existing `llm_request` event, with focused regressions | Agent-loop request-event producer in the planned `zeroclaw-kernel` extraction | @IftekharUddin, [approved in #11047 on 2026-09-24](https://github.com/zeroclaw-labs/zeroclaw/pull/11047#pullrequestreview-5295402310) | Review at the agent-loop extraction design review, or before expanding fingerprint inputs, retained state, trace access, or emission policy |

**Stability tier:** Experimental — no stability guarantee. Decomposition begins at v0.8.0.
