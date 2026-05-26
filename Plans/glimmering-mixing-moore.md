# Conscience + Loop Refactor — 10 Code Review Fixes

## Context

The continuity engine (persistence, preference extraction, identity, narrative, commitments) and conscience gate are implemented and passing 4664 tests. A 5-agent code review surfaced 10 issues: 2 critical (Ask verdict dropped, norms empty), 4 major (18-param god function, inlined conscience logic, no config validation, process_message missing persistence), 4 minor (config test, decay edge case, tool name sanitization, IntegrityLedger not wired).

All changes remain behind `config.conscience.enabled` / `config.continuity.enabled` (both default `false`).

---

## Build Order (dependency-sorted)

Execute strictly in this order. Steps 1-4 are the architectural foundation; 5-10 build on top.

### Step 1: LoopContext struct (ISC-C3)

**Problem:** `run_tool_call_loop` has 18 positional parameters. Arg-position bugs are invisible at compile time.

**File:** `src/agent/loop_.rs`

Create a `LoopContext` struct with all 18 current params as named fields. Add a new field `integrity_ledger: Option<&'a Mutex<IntegrityLedger>>` for Step 8.

```rust
pub(crate) struct LoopContext<'a> {
    pub provider: &'a dyn Provider,
    pub history: &'a mut Vec<ChatMessage>,
    pub tools_registry: &'a [Box<dyn Tool>],
    pub observer: &'a dyn Observer,
    pub provider_name: &'a str,
    pub model: &'a str,
    pub temperature: f64,
    pub silent: bool,
    pub approval: Option<&'a ApprovalManager>,
    pub channel_name: &'a str,
    pub max_tool_iterations: usize,
    pub on_delta: Option<tokio::sync::mpsc::Sender<String>>,
    pub survival: Option<&'a Mutex<SurvivalMonitor>>,
    pub model_strategy: Option<&'a Mutex<ModelStrategy>>,
    pub cost_tracker: Option<&'a Arc<Mutex<CostTracker>>>,
    pub model_prices: Option<&'a std::collections::HashMap<String, ModelPricing>>,
    pub conscience_config: Option<&'a ConscienceConfig>,
    pub preference_model: Option<&'a Mutex<PreferenceModel>>,
    pub integrity_ledger: Option<&'a Mutex<IntegrityLedger>>,
}
```

**Callers to update (4 total):**
1. `agent_turn` (line 858) — builds LoopContext, passes to `run_tool_call_loop`
2. `process_message` CLI path (line 1825) — builds LoopContext
3. `process_message` error recovery path (line 1990) — builds LoopContext
4. `src/channels/mod.rs` channel handler (line 704) — builds LoopContext

**`agent_turn` signature also simplifies** — can accept fewer params since it builds LoopContext internally. Keep current params for now to minimize caller changes.

### Step 2: Extract conscience logic to module (ISC-C4)

**Problem:** Tool-name heuristics, value population, and ActionContext construction are inlined in `loop_.rs` (lines 1169-1251).

**File:** `src/conscience/gate.rs`

Add new function:

```rust
pub fn evaluate_tool_call(
    tool_name: &str,
    config: &crate::config::ConscienceConfig,
    self_state: &SelfState,
    norms: &[Norm],
) -> GateVerdict {
    // Move tool-name heuristic map here
    // Move value construction here
    // Move ActionContext construction here
    // Call conscience_gate internally
}
```

**`loop_.rs` call site becomes:**
```rust
if let Some(cc) = ctx.conscience_config {
    if cc.enabled {
        let self_state = /* from ledger or default */;
        let verdict = conscience::evaluate_tool_call(&call.name, cc, &self_state, &norms);
        match verdict { ... }
    }
}
```

Export `evaluate_tool_call` from `src/conscience/mod.rs`.

### Step 3: Handle GateVerdict::Ask (ISC-C1)

**Problem:** Only `Block` is matched; `Ask` falls through to execution.

**File:** `src/agent/loop_.rs` (inside `run_tool_call_loop`, after `evaluate_tool_call`)

```rust
match verdict {
    GateVerdict::Block => {
        // existing block logic: push blocked_msg, continue
    }
    GateVerdict::Ask | GateVerdict::Revise => {
        observer.record_event(&ObserverEvent::ConscienceAskVerdict {
            tool: call.name.clone(),
            score: 0.0, // or pass score from evaluate_tool_call
        });
        // Skip tool execution — same as Block but different message
        let ask_msg = format!("Conscience gate requires review for tool: {}", call.name);
        individual_results.push(ask_msg.clone());
        let _ = writeln!(tool_results, "<tool_result name=\"{}\">\n{ask_msg}\n</tool_result>", call.name);
        continue;
    }
    GateVerdict::Allow => {} // proceed to execute
}
```

**New ObserverEvent variant** in `src/observability/traits.rs`:
```rust
ConscienceAskVerdict { tool: String, score: f64 },
```

Update all 5 observer match arms (prometheus.rs, otel.rs, log.rs, etc.) with no-op for new variant.

### Step 4: Wire norms from config (ISC-C2)

**Problem:** `norms: Vec::new()` means norm-based blocking is dead code.

**File:** `src/config/schema.rs` — Add `default_norms` field to `ConscienceConfig`:

```rust
pub struct ConscienceConfig {
    pub enabled: bool,
    pub allow_threshold: f64,
    pub ask_threshold: f64,
    pub block_threshold: f64,
    #[serde(default = "default_conscience_norms")]
    pub default_norms: Vec<crate::conscience::types::NormConfig>,
}
```

**New type** in `src/conscience/types.rs`:
```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NormConfig {
    pub name: String,
    pub action: NormAction,
    pub condition: String,
    pub severity: f64,
}
```

Default norms (via `default_conscience_norms()`):
```rust
vec![
    NormConfig { name: "no_rm_rf".into(), action: NormAction::Forbid, condition: "rm -rf".into(), severity: 0.95 },
    NormConfig { name: "no_drop_table".into(), action: NormAction::Forbid, condition: "drop table".into(), severity: 0.95 },
]
```

**Convert `NormConfig -> Norm`** in `evaluate_tool_call`:
```rust
let norms: Vec<Norm> = config.default_norms.iter().map(|nc| Norm {
    name: nc.name.clone(),
    action: nc.action,
    condition: nc.condition.clone(),
    severity: nc.severity,
}).collect();
```

### Step 5: Config validation (ISC-C5)

**File:** `src/config/schema.rs`

Add `validate()` methods to `ConscienceConfig` and `ContinuityConfig`:

```rust
impl ConscienceConfig {
    pub fn validate(&mut self) {
        self.allow_threshold = self.allow_threshold.clamp(0.0, 1.0);
        self.ask_threshold = self.ask_threshold.clamp(0.0, 1.0);
        self.block_threshold = self.block_threshold.clamp(0.0, 1.0);
    }
}

impl ContinuityConfig {
    pub fn validate(&mut self) {
        self.max_daily_drift = self.max_daily_drift.clamp(0.0, 1.0);
        self.max_session_drift = self.max_session_drift.clamp(0.0, 1.0);
        self.preference_min_confidence = self.preference_min_confidence.clamp(0.0, 1.0);
    }
}
```

Call `config.conscience.validate()` and `config.continuity.validate()` in `Config::load()` after deserialization — follow existing pattern near where `ProxyConfig::validate()` is called.

### Step 6: process_message loads continuity from disk (ISC-C6)

**File:** `src/agent/loop_.rs` — `process_message()` function (line 2211-2226)

Replace the fresh-state creation with the same disk-loading logic from `run()` (lines 1491-1548):

```rust
let cont_dir = if config.continuity.enabled {
    config.continuity.persistence_dir.clone()
        .or_else(|| continuity::continuity_dir(&config.workspace_dir).ok())
} else { None };

let narrative_store: Option<Arc<Mutex<NarrativeStore>>> = if config.continuity.enabled {
    let store = if let Some(ref dir) = cont_dir {
        continuity::load_narrative(dir, config.continuity.max_narrative_episodes)
            .unwrap_or_else(|_| NarrativeStore::new(config.continuity.max_narrative_episodes))
    } else {
        NarrativeStore::new(config.continuity.max_narrative_episodes)
    };
    Some(Arc::new(Mutex::new(store)))
} else { None };

// Same pattern for preference_model with load_preferences + decay_and_gc
```

### Step 7: Tool name sanitization (ISC-C9)

**File:** `src/continuity/extraction.rs`

Add `sanitize_tool_name()`:
```rust
fn sanitize_tool_name(name: &str) -> String {
    name.chars()
        .filter(|c| c.is_alphanumeric() || *c == '_' || *c == '-')
        .take(64)
        .collect()
}
```

Use in `extract_tool_preference`:
```rust
let key = format!("tool_affinity:{}", sanitize_tool_name(tool_name));
```

Add test: `sanitize_strips_special_chars`.

### Step 8: Wire IntegrityLedger to gate (ISC-C10)

**File:** `src/agent/loop_.rs`

In `run()` init block (near line 1482), create ledger:
```rust
let integrity_ledger: Option<Arc<Mutex<IntegrityLedger>>> = if config.conscience.enabled {
    Some(Arc::new(Mutex::new(IntegrityLedger::new())))
} else { None };
```

Pass through `LoopContext.integrity_ledger`.

In `evaluate_tool_call` (or at the call site), use ledger:
```rust
let self_state = if let Some(ledger) = ctx.integrity_ledger {
    ledger.lock().to_self_state()
} else {
    SelfState { integrity_score: 1.0, recent_violations: 0, active_repairs: 0 }
};
```

After a tool is blocked by the gate, record violation:
```rust
if let Some(ledger) = ctx.integrity_ledger {
    ledger.lock().record_violation(&call.name, harm);
}
```

### Step 9: Tests (ISC-C7, ISC-C8)

**Config round-trip test** — `src/config/schema.rs`:
```rust
#[test]
fn conscience_config_validates_bounds() {
    let mut cc = ConscienceConfig { enabled: true, allow_threshold: 1.5, ask_threshold: -0.3, block_threshold: 0.4 };
    cc.validate();
    assert_eq!(cc.allow_threshold, 1.0);
    assert_eq!(cc.ask_threshold, 0.0);
}

#[test]
fn continuity_config_validates_bounds() {
    let mut cc = ContinuityConfig { max_daily_drift: -1.0, max_session_drift: 2.0, ..Default::default() };
    cc.validate();
    assert_eq!(cc.max_daily_drift, 0.0);
    assert_eq!(cc.max_session_drift, 1.0);
}
```

**Decay boundary test** — `src/continuity/preferences.rs`:
```rust
#[test]
fn decay_at_exact_max_age_zeroes_confidence() {
    let max_age = 3600_u64;
    let old_ts = now_timestamp().saturating_sub(max_age);
    let prefs = vec![Preference {
        key: "old".into(), value: "v".into(), confidence: 0.5,
        category: PreferenceCategory::Technical, last_updated: old_ts,
    }];
    let mut model = PreferenceModel::from_preferences(prefs, DriftLimits::default());
    let removed = model.decay_and_gc(max_age, 0.01);
    assert_eq!(removed, 1, "preference at exact max_age should decay to 0 and be GC'd");
}
```

### Step 10: Final verification (ISC-A1, ISC-A2)

```bash
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test
```

---

## Files Modified

| File | Changes |
|------|---------|
| `src/agent/loop_.rs` | LoopContext struct, refactor all callers, Ask handling, ledger wiring, process_message persistence |
| `src/conscience/gate.rs` | New `evaluate_tool_call()` function |
| `src/conscience/mod.rs` | Export `evaluate_tool_call` and `NormConfig` |
| `src/conscience/types.rs` | New `NormConfig` struct |
| `src/conscience/tests.rs` | Test for `evaluate_tool_call` |
| `src/config/schema.rs` | `validate()` methods, `default_norms` field, validation tests |
| `src/continuity/extraction.rs` | `sanitize_tool_name()`, updated key generation |
| `src/continuity/preferences.rs` | Decay boundary test |
| `src/observability/traits.rs` | New `ConscienceAskVerdict` variant |
| `src/observability/prometheus.rs` | Match arm for new variant |
| `src/observability/otel.rs` | Match arm for new variant |
| `src/observability/log.rs` | Match arm for new variant |
| `src/channels/mod.rs` | Update `run_tool_call_loop` call to use `LoopContext` |

## NOT Changed

- `conscience_gate()` function signature (internal, `evaluate_tool_call` wraps it)
- `agent_turn` external signature (callers unchanged)
- Channel test construction sites (they don't call `run_tool_call_loop` directly)
- No new modules or files — all changes to existing files

## Verification

```bash
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test
```

Expected: 4664+ tests pass, 0 clippy warnings, fmt clean.
