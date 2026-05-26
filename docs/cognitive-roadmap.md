---
name: ZeroClaw Cognitive System Roadmap
updated: 2026-03-24
status: All planned features COMPLETE
---

# ZeroClaw — Cognitive System Roadmap

## Architecture

10,615 LOC across three subsystems:
- `src/consciousness/` (4,469 LOC) — 7-agent debate loop with orchestrator
- `src/cognitive/` (2,666 LOC) — hebbian learning, memory, planning, skills
- `src/conscience/` (~3,480 LOC) — ethical gate with absolute veto
- `src/quantum/` — quantum consciousness agent

CosmicBrain struct in `src/agent/loop_.rs` wires all subsystems together.

## Consciousness Agents (ALL IMPLEMENTED)

| Agent | File | Role |
|-------|------|------|
| Chairman | `agents/chairman.rs` | GlobalWorkspace + AgentPool focus |
| Memory | `agents/memory.rs` | CosmicMemoryGraph + Consolidation |
| Research | `agents/research.rs` | CosmicMemoryGraph + WorldModel |
| Strategy | `agents/strategy.rs` | Counterfactual + Policy + FreeEnergy + dream insights |
| Execution | `agents/execution.rs` | CausalGraph + EmotionalModulator |
| Conscience | `agents/conscience.rs` | NormativeEngine + absolute veto |
| Reflection | `agents/reflection.rs` | SelfModel + DriftDetector |
| Metacognitive | `agents/metacognitive.rs` | Parameter adjustment |
| Quantum | `quantum/brain.rs` | Superposition + decoherence + annealing |

## Feature Status

### Core Consciousness Loop (COMPLETE)

- [x] 7-agent debate orchestration (default 3 rounds, 0.85 approval threshold)
- [x] SharedBus (VecDeque) for sync message passing
- [x] Coherence tracking via EMA
- [x] Tick-based synchronous execution

### Neuromodulation (COMPLETE)

- [x] 4 neurotransmitters: dopamine, serotonin, norepinephrine, cortisol
- [x] NCN signals: precision, gain, ffn_gate
- [x] Neuromodulation decay per tick

### Dream Consolidation (COMPLETE)

- [x] DreamConsolidator — records tick outcomes, extracts recurring patterns
- [x] Pattern recurrence tracking

### Conscience (COMPLETE)

- [x] Pre-action gate with GateVerdict (Block/Ask/Revise/Allow)
- [x] Integrity ledger with asymmetric repair costs
- [x] Cosmic bridge integration

### Persistence (COMPLETE — pre-existing)

- [x] `save_consciousness()` / `load_consciousness()` in `orchestrator.rs:844-929`
- [x] Wired into agent loop: startup (line 1665), periodic checkpoint every 100 ticks (line 2150), shutdown (line 2630)
- [x] Default path: `~/.zeroclaw/consciousness-state.json`

### Config Validation (COMPLETE — 2026-03-24)

- [x] `ConsciousnessConfig::validate()` in `config/schema.rs:2972-2985`
- [x] Validates debate_rounds (1-10), approval_threshold (0.5-1.0)
- [x] Called during `load_or_init()` when consciousness enabled

### Dream → Strategy Feedback (COMPLETE — 2026-03-24)

- [x] `dream_insights` field added to StrategyAgent
- [x] `perceive()` reads `dream_pattern` bus messages
- [x] Tracks recurrence count per pattern
- [x] Generates `dream_guided:` proposals when recurrence >= 3

### Cross-Agent Reconciliation (COMPLETE — 2026-03-24)

- [x] Evidence-weighted scoring replaces simple priority*confidence
- [x] `agent_prediction_accuracy()` weights by calibration data
- [x] Structured rebuttal round when scores within 0.1
- [x] Decisions logged with `tracing::debug`

### Quantum Agent (COMPLETE — pre-existing)

- [x] `QuantumConsciousnessAgent` in `quantum/brain.rs`
- [x] Superposition, decoherence, quantum annealing, entanglement tracking
- [x] Registered in `consciousness/agents/mod.rs:66`

### Scaling Optimization (COMPLETE — 2026-03-24)

- [x] Phase timing instrumentation (perceive, deliberate, act, reflect) via `tracing::debug`
- [x] Low-confidence proposal pruning (confidence < 0.2 before deliberation)
- [x] Consensus caching — skips re-evaluation when proposals unchanged
- [x] `max_discourse_depth` config field in `config/schema.rs:2958` (default: 5)

## Key Files

| File | Purpose |
|------|---------|
| `src/consciousness/orchestrator.rs` | Main consciousness loop + persistence + reconciliation + scaling |
| `src/consciousness/traits.rs` | ConsciousnessAgent trait + PhenomenalState + types |
| `src/consciousness/neuromodulation.rs` | 4-signal neuromodulation engine |
| `src/consciousness/dream.rs` | DreamConsolidator + pattern extraction |
| `src/consciousness/wisdom.rs` | WisdomAccumulator |
| `src/consciousness/prediction_market.rs` | Agent prediction accuracy tracking |
| `src/consciousness/metacognition.rs` | MetacognitiveEngine |
| `src/consciousness/narrative.rs` | NarrativeEngine |
| `src/consciousness/agents/strategy.rs` | Strategy + dream-guided proposals |
| `src/quantum/brain.rs` | QuantumConsciousnessAgent |
| `src/config/schema.rs` | ConsciousnessConfig + validation |
| `src/agent/loop_.rs` | CosmicBrain struct + consciousness wiring |
| `src/conscience/gate.rs` | Ethical evaluation gate |
| `tests/consciousness_integration.rs` | 10+ integration test scenarios |

## Test Coverage

- 4,600+ cosmic tests passing (last verified: 2026-03-24)
- Integration tests cover: orchestrator init, all 7 agents, bus routing, contradiction detection, conscience veto, metacognitive adjustment, coherence tracking, dream consolidation, neuromodulation decay

## Engineering Review Fixes (COMPLETE — 2026-03-24)

- [x] Expanded config validation: 5 float fields (coherence_ema_alpha, coherence_decay_on_empty, collective_coupling, min_edge, calibration_drift_threshold) now validated in OrchestratorConfig
- [x] Profiler memory_snapshots capped at 1000 entries (was unbounded trap)

## Next Steps

All planned features and review fixes implemented. Future work would be:
1. Performance profiling with actual multi-agent workloads (infrastructure ready, needs real data)
2. Quantum agent amplitude-based consensus (currently uses standard vote weighting)
