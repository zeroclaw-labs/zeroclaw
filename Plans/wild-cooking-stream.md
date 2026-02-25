# Cosmic Brain Phase 2 — 7 Remaining Modules + Full Wiring

## Context

Ricardo requested a comprehensive expansion of the cosmic brain subsystem in ZeroClaw. Phase 1 delivered: memory graph, free energy, integration metrics. This session added 4 more: causality, self_model, normative, modulation (92 cosmic tests passing). 7 modules remain + full wiring of all 11 modules into the agent loop and life loop.

## Current State

**`src/cosmic/` modules (7 complete, 7 pending):**
- ✅ `memory.rs` — CosmicMemoryGraph, spreading activation (14 tests)
- ✅ `free_energy.rs` — FreeEnergyState, prediction-correction (13 tests)
- ✅ `integration.rs` — IntegrationMeter, Phi, clustering (9 tests)
- ✅ `causality.rs` — CausalGraph, loop detection, transfer entropy (13 tests)
- ✅ `self_model.rs` — SelfModel + WorldModel, dual beliefs (14 tests)
- ✅ `normative.rs` — NormativeEngine, obligation/prohibition evaluation (14 tests)
- ✅ `modulation.rs` — EmotionalModulator, 8 global variables, BehavioralBias (15 tests)
- 🔲 `persistence.rs` — Persistent state storage for all cosmic modules
- 🔲 `multi_agent.rs` — Cosmic-aware multi-agent coordination
- 🔲 `policy.rs` — Hierarchical policy engine
- 🔲 `counterfactual.rs` — Counterfactual simulation engine
- 🔲 `consolidation.rs` — Memory consolidation process
- 🔲 `drift.rs` — Drift detection across all subsystems
- 🔲 `constitution.rs` — Internal value constitution

**Wiring points:**
- `src/agent/loop_.rs` — Currently wires only graph+free_energy+integration as `CosmicBrain` tuple
- `src/life/mod.rs` — Currently wires only IntegrationMeter
- `src/config/schema.rs` — `CosmicBrainConfig` needs new fields for new modules

## Plan: 7 New Modules

### Module 8: `persistence.rs` — Persistent State Storage
**Purpose:** Save/load all cosmic state to disk across sessions.
**Key types:**
- `CosmicPersistence` — orchestrates save/load for all cosmic modules
- `CosmicSnapshot` — serializable aggregate of all module states
**Methods:** `save_all(path)`, `load_all(path) -> CosmicSnapshot`, `save_module(name, data)`, `load_module(name)`
**Pattern:** Follows `EmotionalState.save()/load_or_default()` from `src/life/emotional.rs`
**Storage:** JSON files in `data/cosmic/` (one per module: `graph.json`, `free_energy.json`, etc.)
**Tests:** 8+ (round-trip, partial load, missing files, corruption recovery)

### Module 9: `multi_agent.rs` — Multi-Agent Architecture
**Purpose:** Coordinate multiple sub-agents sharing cosmic state.
**Key types:**
- `AgentPool` — manages named sub-agents with shared cosmic state
- `AgentRole` — enum (Primary, Advisor, Critic, Explorer)
- `ConsensusResult` — aggregated decision from multiple agents
**Methods:** `register_agent()`, `broadcast_state()`, `request_consensus()`, `merge_beliefs()`
**Pattern:** Extends `src/tools/delegate.rs` DelegateTool concept but with shared beliefs
**Integration:** SelfModel/WorldModel shared across pool; NormativeEngine enforces constraints
**Tests:** 10+ (register, broadcast, consensus, role filtering, empty pool)

### Module 10: `policy.rs` — Hierarchical Policy Engine
**Purpose:** Layered policy evaluation (constitution → domain → context → learned).
**Key types:**
- `PolicyLayer` — enum (Constitutional, Domain, Contextual, Learned)
- `Policy` — id, layer, condition, action, priority, weight
- `PolicyEngine` — evaluates action against all layers, highest layer wins on conflict
**Methods:** `register_policy()`, `evaluate()`, `check_conflict()`, `active_policies()`
**Pattern:** Extends existing `src/security/policy.rs` SecurityPolicy but generalized
**Integration:** NormativeEngine feeds learned policies; constitution.rs provides base layer
**Tests:** 10+ (layer precedence, conflict resolution, domain filtering)

### Module 11: `counterfactual.rs` — Counterfactual Simulators
**Purpose:** "What if" reasoning — simulate action outcomes before execution.
**Key types:**
- `Scenario` — hypothetical action + context
- `SimulationResult` — predicted outcome, confidence, risk assessment
- `CounterfactualEngine` — runs scenarios against current state
**Methods:** `simulate()`, `compare_scenarios()`, `best_action()`, `regret()`
**Pattern:** Uses WorldModel beliefs + FreeEnergyState predictions to estimate outcomes
**Integration:** Feeds CausalGraph (which subsystems would be affected), NormativeEngine (is it allowed)
**Tests:** 10+ (single scenario, comparison, regret calculation, empty state)

### Module 12: `consolidation.rs` — Memory Consolidation Process
**Purpose:** Cross-session memory synthesis — merge, deduplicate, extract patterns.
**Key types:**
- `ConsolidationEngine` — processes raw memories into consolidated knowledge
- `ConsolidationResult` — merged entries, extracted patterns, pruned count
- `MemoryPattern` — recurring theme extracted from multiple memories
**Methods:** `consolidate()`, `extract_patterns()`, `merge_similar()`, `prune_redundant()`
**Pattern:** Fills the gap identified in exploration — no cross-session aggregation exists
**Integration:** Operates on CosmicMemoryGraph nodes; outputs feed WorldModel beliefs
**Tests:** 10+ (merge similar, pattern extraction, dedup, empty input)

### Module 13: `drift.rs` — Drift Detection
**Purpose:** Monitor behavioral/value drift across all cosmic subsystems.
**Key types:**
- `DriftDetector` — monitors all subsystem states for drift
- `DriftAlert` — subsystem, drift_magnitude, direction, timestamp
- `DriftReport` — aggregate drift across all subsystems
**Methods:** `record_sample()`, `detect_drift()`, `drift_report()`, `is_drifting()`
**Pattern:** Extends `src/continuity/guard.rs` DriftLimits concept but for cosmic subsystems
**Integration:** Monitors SelfModel, WorldModel, NormativeEngine, EmotionalModulator, CausalGraph
**Tests:** 10+ (no drift, gradual drift, sudden shift, threshold, empty)

### Module 14: `constitution.rs` — Internal Value Constitution
**Purpose:** Immutable core values with integrity verification.
**Key types:**
- `Value` — id, description, priority, immutable flag
- `Constitution` — set of values with SHA-256 integrity hash
- `IntegrityCheck` — hash comparison result
**Methods:** `register_value()`, `verify_integrity()`, `check_action_alignment()`, `compute_hash()`
**Pattern:** Mirrors `src/soul/constitution.rs` SHA-256 verification but for cosmic values
**Integration:** PolicyEngine uses as top Constitutional layer; NormativeEngine derives norms from values
**Tests:** 10+ (register, integrity pass/fail, alignment check, immutability)

## Wiring Plan

### Config additions (`src/config/schema.rs`):
Add to `CosmicBrainConfig`:
```rust
pub persistence_dir: String,          // "data/cosmic"
pub multi_agent_pool_size: usize,     // 4
pub policy_conflict_resolution: String, // "highest_layer"
pub counterfactual_max_scenarios: usize, // 10
pub consolidation_interval_secs: u32, // 3600
pub drift_window_size: usize,         // 50
pub drift_threshold: f64,             // 0.1
```

### Agent loop (`src/agent/loop_.rs`):
- Expand `CosmicBrain` type alias to include all new modules
- Initialize all modules in `run()` gated by `config.cosmic_brain.enabled`
- Post-turn: record causal events, update self/world models, check drift, persist
- Pre-action: check policy engine, run counterfactual if risk > threshold

### Life loop (`src/life/mod.rs`):
- Periodic consolidation tick
- Drift monitoring on each tick
- Modulation feeds from emotional state
- Constitution integrity check

### Module registration (`src/cosmic/mod.rs`, `src/lib.rs`):
- Add all 7 new modules to mod.rs with pub use re-exports
- No changes needed to lib.rs (already has `pub mod cosmic`)

## Execution Strategy

**Parallel: 4 agents, each building ~2 modules:**
- Agent A: `persistence.rs` + `constitution.rs` (both state-focused)
- Agent B: `multi_agent.rs` + `policy.rs` (both governance-focused)
- Agent C: `counterfactual.rs` + `consolidation.rs` (both reasoning-focused)
- Agent D: `drift.rs` + full wiring (config + loop_.rs + life/mod.rs + mod.rs)

## Verification

```bash
cargo fmt --check
cargo clippy -- -D warnings
cargo test cosmic           # All cosmic tests (~150+ after expansion)
cargo test                  # Full suite (4683+ tests, no regressions)
```

Each module must have 8+ tests. Total new tests: ~70+.
All modules enabled by default when `cosmic_brain.enabled = true`.
