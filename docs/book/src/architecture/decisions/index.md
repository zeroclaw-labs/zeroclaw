---
type: architecture
status: accepted
last-reviewed: 2026-07-19
relates-to:
  - docs/book/src/foundations/fnd-002-documentation-standards.md
---

# Architecture Decision Records

Architecture Decision Records (ADRs) are the durable record of architecture choices that constrain future ZeroClaw work. They are shorter than RFCs: each one records the context, decision, consequences, and the sources that justify the decision.

Accepted ADRs are immutable. If the architecture changes, write a new ADR and mark the old one as superseded rather than rewriting history.

## Current ADRs

| ADR | Status | Decision |
| --- | --- | --- |
| [ADR-001](./ADR-001-rust-first.md) | accepted | ZeroClaw's runtime and first-party crates are implemented in Rust. |
| [ADR-002](./ADR-002-trait-driven-extensibility.md) | accepted | First-party extension surfaces use explicit trait contracts. |
| [ADR-003](./ADR-003-wasm-plugin-model.md) | superseded by [ADR-009](./ADR-009-wit-wasmtime-plugin-execution.md) | WASM plugins initially used Extism as the execution bridge. |
| [ADR-004](./ADR-004-tool-shared-state-ownership.md) | accepted | Tool-held shared state follows daemon-owned identity, handle ownership, isolation, and reload rules. |
| [ADR-005](./ADR-005-pluggable-memory-backends.md) | accepted | Memory storage uses a backend-neutral contract with SQLite as the default. |
| [ADR-006](./ADR-006-runtime-channel-plugins.md) | superseded by [ADR-013](./ADR-013-channels-as-gateway-clients.md) | Runtime plugins are the target for optional channels, with explicit capability-based native exceptions during migration. |
| [ADR-007](./ADR-007-gateway-extraction.md) | superseded by [ADR-013](./ADR-013-channels-as-gateway-clients.md) | The gateway becomes a separate optional process over a supported local IPC contract. |
| [ADR-008](./ADR-008-goal-mode-control-plane-and-usage-accounting.md) | superseded | Goal mode was to use the durable task control plane; that plane was deleted (control-plane migration wall) and durable task truth lives in Tachi through the task-intent bridge. |
| [ADR-009](./ADR-009-wit-wasmtime-plugin-execution.md) | retired | WIT components and direct `wasmtime` replaced the Extism plugin bridge; the WASM plugin host and `wit/` contracts were later removed in favour of MCP. |
| [ADR-010](./ADR-010-memory-authority-boundaries.md) | proposed | Session history, curated memory, and enrichment have separate authority boundaries. |
| [ADR-011](./ADR-011-multi-agent-runtime-boundaries.md) | accepted | Configured agents have explicit runtime boundaries under one daemon. |
| [ADR-012](./ADR-012-generation-scoped-live-config-apply.md) | proposed | Live config application uses canonical generations and target-specific results. |
| [ADR-013](./ADR-013-channels-as-gateway-clients.md) | accepted (Node deferral amended by [ADR-017](./ADR-017-personal-agent-body-edges-and-delegation.md)) | Channels are out-of-process gateway clients (bridges); the gateway is the only external door and the runtime owns the turn lifecycle. |
| [ADR-014](./ADR-014-reviewed-agentsoul-projection.md) | accepted (Voice precedence amended by [ADR-015](./ADR-015-one-governed-soul.md); Voice renamed Tone by [ADR-018](./ADR-018-soul-character-model.md)) | Reviewed AgentSoul uses a closed presentation-only review and projection contract. |
| [ADR-015](./ADR-015-one-governed-soul.md) | accepted (§1 and §3 amended by [ADR-016](./ADR-016-growth-proposed-by-agent-approved-by-owner.md); layers renamed by [ADR-018](./ADR-018-soul-character-model.md)) | Soul has three owner-governed layers (identity, principles, voice); the model can only propose changes, and legacy personality files become seeds. |
| [ADR-016](./ADR-016-growth-proposed-by-agent-approved-by-owner.md) | accepted (Growth and Voice layers renamed Story and Tone, `challenge` floor tightened, by [ADR-018](./ADR-018-soul-character-model.md)) | The agent grows through a bounded Growth layer and Voice changes that it proposes and the owner approves; it reflects once a week from the owner's own messages only. |
| [ADR-017](./ADR-017-personal-agent-body-edges-and-delegation.md) | accepted | One always-on body on the owner's computer; speakers, phones, and other devices are gateway Clients and Nodes; the body answers directly or reasons itself, and hands work to CLI agents only through Tachi. |
| [ADR-018](./ADR-018-soul-character-model.md) | accepted | Each Soul layer answers one question; the Voice and Growth layers are renamed Tone and Story, and Growth names the proposal-and-approval process; Tone gains owner-made exemplars; proposals cannot lower `challenge` and each dial cools down for 28 days; drift is reported against an owner-set anchor; a Soul check compares behavior across models; prompt-injected files are governed by role. |
| [ADR-019](./ADR-019-one-body-many-hands.md) | proposed | One body retains Soul, approval and safety authority; edges split only for a named reason. Clients use scoped chat/intake contracts, channel outbox uses its separate contract, Nodes use v2, and body-initiated MCP stays distinct. Shared host code is extracted with its second consumer; new platforms and robot capabilities remain subject to #374 dispatch and their existing acceptance gates. |
| [ADR-020](./ADR-020-durable-turns-replay-and-attention.md) | proposed | Pi Durable supplies design references for scoped request identities and conservative replay. Approval evidence preserves boot-bound grants; cancellation does not imply external rollback. Existing owners provide background work and policy-gated delivery. Observations preserve provenance and compaction preserves retained history subject to privacy deletion. Clear authorized work is acknowledged without redundant intent confirmation. |
| [ADR-021](./ADR-021-full-duplex-voice-at-the-edge.md) | proposed | Full-duplex voice keeps audio at the edge and personal authority in the body; a versioned stateful spoken-text projection preserves security checks, while scoped playback evidence never rewrites canonical history or proves human hearing; listening is bounded and its privacy indicator overrides attention; an optional front harness has one model tool, approved presentation phrases without raw Soul export, isolated conversation context, and measured fallback behavior. |
| [ADR-022](./ADR-022-one-small-node-many-devices.md) | proposed | Thin devices keep only bounded local audio processing, with separate scoped credentials for Node, voice, and legacy links; one voice adapter isolates sources and conversations; stock ESPHome support requires a pinned trial; shared v2 host code is extracted from #514 and a real Yonbo adapter while N3B1 stays on the robot, with physical-device modules extracted later from two consumers; wake arbitration grants no identity or route authority, proactive delivery uses authorized #63 destinations, and shared-device voice never approves. #374 remains the execution index. |
| [ADR-023](./ADR-023-realtime-voice-carrier-with-body-delegation.md) | proposed | A realtime speech-to-speech service is a carrier: it answers small talk and public facts itself and delegates everything personal or consequential to the body over the voice surface; the kernel pushes a voice persona projection without raw Soul or unconsented User Model data, carrier-handled exchanges are untrusted presentation events, and the voice credential is a bridge credential without operator authority. On acceptance it supersedes the ADR-021 front-harness restrictions for carrier mode (§1, §9 table, §10, §12, Acceptance 8) and amends ADR-017 §3/§4 and the ADR-019 Yonbo Client rule. |

ADR-006 and ADR-007 were implementation-gated roadmap decisions from [FND-002](../../foundations/fnd-002-documentation-standards.md). Neither acceptance boundary shipped; both are superseded by ADR-013 for this fork.

ADR-010 remains proposed until the acceptance gates in the record are met.

ADR-012 remains proposed until canonical config publication, generation-scoped results, and the bounded security and channel live-apply consumers meet the acceptance gates in the record.

ADR-014 records the accepted owner-selected minimum Slice A contract after
independent documentation review and grants no #189/#190 activation.

ADR-019 remains proposed until the owner accepts it.

ADR-020 remains proposed until the owner accepts it.

ADR-021 remains proposed until the owner accepts it.

ADR-022 remains proposed until the owner accepts it.

ADR-023 remains proposed until the owner accepts it.
