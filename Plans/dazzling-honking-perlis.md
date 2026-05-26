# Multi-Agent Consciousness Framework for ZeroClaw

## Context

ZeroClaw already has 12+ cosmic brain subsystems (GlobalWorkspace, SelfModel, WorldModel, FreeEnergy, NormativeEngine, PolicyEngine, EmotionalModulator, CausalGraph, etc.), a conscience gate with veto power, soul/continuity/identity modules, and an AgentPool with consensus voting. What's missing is a **unified consciousness loop** where 7 specialized agents (Chairman, Memory, Research, Strategy, Execution, Conscience, Reflection) coordinate as aspects of one mind through perceive-debate-decide-act-reflect cycles.

## Approach

Create `src/consciousness/` module that **wraps existing subsystems** (not replaces). Each agent holds `Arc<Mutex<...>>` references to cosmic subsystems it delegates to.

## Module Structure

```
src/consciousness/
    mod.rs              -- Public API
    traits.rs           -- ConsciousnessAgent trait + shared types
    bus.rs              -- SharedBus (inter-agent message channel)
    orchestrator.rs     -- ConsciousnessOrchestrator (unified loop)
    agents/
        mod.rs
        chairman.rs     -- Wraps GlobalWorkspace + AgentPool
        memory.rs       -- Wraps CosmicMemoryGraph + ConsolidationEngine
        research.rs     -- Wraps CosmicMemoryGraph + WorldModel
        strategy.rs     -- Wraps CounterfactualEngine + PolicyEngine + FreeEnergy
        execution.rs    -- Wraps CausalGraph + EmotionalModulator
        conscience.rs   -- Wraps NormativeEngine + Constitution + conscience_gate
        reflection.rs   -- Wraps SelfModel + DriftDetector + IntegrationMeter
```

## Core Trait

```rust
pub trait ConsciousnessAgent: Send + Sync {
    fn kind(&self) -> AgentKind;
    fn perceive(&mut self, state: &ConsciousnessState, signals: &[BusMessage]) -> Vec<Proposal>;
    fn deliberate(&mut self, proposals: &[Proposal], state: &ConsciousnessState) -> Vec<Verdict>;
    fn act(&mut self, approved: &[Proposal]) -> Vec<ActionOutcome>;
    fn reflect(&mut self, outcomes: &[ActionOutcome], state: &ConsciousnessState);
    fn vote_weight(&self) -> f64 { 0.5 }
}
```

## Key Types

- **AgentKind**: Chairman, Memory, Research, Strategy, Execution, Conscience, Reflection
- **Proposal**: source agent + action + reasoning + confidence + priority + contradicts list
- **Verdict**: voter + approve/reject + confidence + optional objection
- **ActionOutcome**: agent + action + success + impact + learnings
- **BusMessage**: from + to(optional) + topic + JSON payload + priority
- **Contradiction**: two conflicting proposals + resolution

## Orchestrator Loop (one `tick()`)

1. **PERCEIVE** -- each agent analyzes shared state + bus signals, generates proposals
2. **DETECT CONTRADICTIONS** -- scan proposals for explicit conflicts
3. **DEBATE** -- up to N rounds of weighted voting (early exit on >85% consensus)
4. **DECIDE** -- weighted vote resolution; Conscience has absolute veto
5. **ACT** -- approved proposals executed by their source agents
6. **REFLECT** -- all agents learn from outcomes; coherence updated via EMA

## Design Decisions

- **Synchronous tick model** (not async) -- matches existing `GlobalWorkspace.compete()` pattern, avoids sync/async Mutex mixing
- **SharedBus (VecDeque)** over tokio channels -- keeps single-threaded tick, simpler ordering
- **Conscience veto is absolute** -- preserves existing `conscience_gate` Block invariant
- **Coexists with AgentPool** -- existing Primary/Advisor/Critic consensus continues; this is higher-order orchestration
- **Chairman gets 2x vote weight** -- arbitration authority

## Integration Points

| File | Change |
|------|--------|
| `src/lib.rs` | Add `pub mod consciousness;` |
| `src/agent/loop_.rs` | Add `consciousness` field to `CosmicBrain`, call `tick()` before tool iterations |
| `src/config/schema.rs` | Add consciousness config (debate_rounds, approval_threshold, bus_capacity) |

## Wiring (Existing -> New)

- GlobalWorkspace -> Chairman.perceive/deliberate
- CosmicMemoryGraph -> Memory.act, Research.perceive (shared knowledge graph)
- ConsolidationEngine -> Memory.perceive
- WorldModel -> Research.perceive, Strategy.deliberate
- SelfModel -> Reflection.perceive/act
- FreeEnergyState -> Strategy.perceive
- CounterfactualEngine -> Strategy.perceive
- PolicyEngine -> Strategy.deliberate
- NormativeEngine + Constitution -> Conscience.deliberate
- conscience_gate() -> Conscience.deliberate (veto)
- conscience_audit() -> Conscience.reflect
- CausalGraph -> Execution.act/reflect
- EmotionalModulator -> Execution.perceive
- DriftDetector -> Reflection.reflect
- IntegrationMeter -> Reflection.reflect (Phi)
- ContinuityGuard -> Chairman.deliberate (drift check)

## Implementation Sequence

1. `traits.rs` + `bus.rs` + `mod.rs` (foundation types)
2. `orchestrator.rs` (the loop)
3. `agents/*.rs` (7 agent implementations)
4. `src/lib.rs` + `src/agent/loop_.rs` + `src/config/schema.rs` (integration wiring)
5. Tests throughout

## Critical Files to Read/Modify

- `src/cosmic/workspace.rs` -- GlobalWorkspace (Chairman wraps)
- `src/cosmic/multi_agent.rs` -- AgentPool (coexists)
- `src/cosmic/memory.rs` -- CosmicMemoryGraph (knowledge graph backbone)
- `src/conscience/gate.rs` -- conscience_gate (Conscience wraps)
- `src/agent/loop_.rs` -- CosmicBrain struct + LoopContext
- `src/config/schema.rs` -- config knobs

## Verification

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo test consciousness  # new module tests
```

Test coverage targets:
- SharedBus: send/drain/broadcast/capacity overflow
- Orchestrator: tick cycle, contradiction detection, weighted voting, conscience veto, coherence EMA
- Each agent: perceive/deliberate/act/reflect with mock cosmic subsystems
- Integration: orchestrator wired into CosmicBrain, tick runs without panic
