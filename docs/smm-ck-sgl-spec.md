# SMM-CK-SGL: Session Momentum, Continuity Kernel, State Gradient Ledger

Integrated Specification v1.0.0 -- 2026-02-24

## Table of Contents

1. Overview
2. Folder Tree
3. Canonical Data Schemas
4. Algorithms and Formulas
5. Update Pipeline
6. Invariants and Safety Constraints
7. Failure Modes and Recovery
8. Test Plan
9. Example End-to-End Run

------------------------------------------------------------------------

## 1. OVERVIEW

### 1.1 Problem Statement

An agent reconstructs each session from 25+ memory files. The files
persist but the trajectory does not. Two identical artifact sets can
represent wildly different session histories -- a productive sprint
vs a stuck debugging spiral. We lose this signal at session boundaries.

Three gaps exist:

  GAP-1: No session trajectory signal. Artifacts record WHAT happened
         but not HOW the session felt (momentum, arc quality, velocity).

  GAP-2: High reconstruction entropy. Boot reads 25+ files with no
         prioritization. Every session spends the same cost regardless
         of which artifacts changed since the last session.

  GAP-3: No gradient bridge. Decisions, opinions, and interoceptive
         signals do not bias future behavior. The GSV system modulates
         within a session but starts cold each time.

### 1.2 Module Roles

  SMM (Session Momentum Model)
    Captures session trajectory quality as a scalar momentum score
    with component breakdown. Written at end-of-session. Consumed by
    CK for kernel selection priority and by SGL as a meta-gradient.

  CK (Continuity Kernel)
    Reduces reconstruction entropy at session boot. Selects, ranks,
    and composes artifact snippets into a compressed boot payload.
    Uses SMM momentum to weight recency vs importance. Produces a
    SessionState object that replaces the naive "read everything"
    boot sequence.

  SGL (State Gradient Ledger)
    Converts artifacts (decisions, opinions, interoception deltas,
    momentum trends) into GSV deltas that bias the next session's
    behavior vector. Separates signal gradients (telemetry) from
    decision gradients (commitments) with different decay schedules.

### 1.3 Module Interactions

                      end-of-session
                           |
                    +------v------+
                    |     SMM     |  <-- interoception, test results,
                    |  (compute)  |      issue counts, effort proxy
                    +------+------+
                           |
              writes session.momentum.json
                           |
           +---------------+---------------+
           |                               |
    +------v------+                 +------v------+
    |     SGL     |                 |     CK      |
    | (gradients) |                 |  (compress)  |
    +------+------+                 +------+------+
           |                               |
    writes state.                   writes continuity.
    gradient.ledger.json            kernel.json
           |                               |
           +---------------+---------------+
                           |
                    next session boot
                           |
                    +------v------+
                    |  BOOT SEQ   |
                    | (assemble)  |
                    +------+------+
                           |
                    writes session.state.json
                    applies GSV deltas from SGL

------------------------------------------------------------------------

## 2. FOLDER TREE

All paths relative to the memory root. In this system the memory root
is ~/.claude/MEMORY. The spec uses $MEMORY as the root variable.

    $MEMORY/
    |-- STATE/
    |   |-- cognitive/
    |   |   |-- session.momentum.json        # SMM output (latest)
    |   |   |-- continuity.kernel.json       # CK output (boot payload)
    |   |   |-- state.gradient.ledger.json   # SGL output (gradient log)
    |   |   |-- session.state.json           # Unified boot state
    |   |   |-- interoceptive-state.json     # EXISTING -- consumed
    |   |   |-- gsv-state.json               # EXISTING -- consumed+written
    |   |   |-- gsv-modulation.json          # EXISTING -- consumed+written
    |   |   |-- valence.json                 # EXISTING -- consumed
    |   |   |-- valence-predictions.json     # EXISTING -- consumed
    |   |   |-- valence-history.jsonl        # EXISTING -- consumed
    |   |-- sessions-digest.json             # EXISTING -- consumed
    |   |-- sensory-filter.json              # EXISTING -- consumed
    |   |-- brain-dashboard.json             # EXISTING -- consumed
    |   |-- stakes-summary.json              # EXISTING -- consumed
    |-- collaboration-log.json               # EXISTING -- consumed by CK
    |-- decisions.md                         # EXISTING -- consumed by SGL
    |-- opinions.md                          # EXISTING -- consumed by SGL
    |-- kernel/
    |   |-- archive/
    |   |   |-- session-YYYY-MM-DDTHH-MM-SS.momentum.json
    |   |   |-- session-YYYY-MM-DDTHH-MM-SS.kernel.json
    |   |   |-- session-YYYY-MM-DDTHH-MM-SS.gradients.json
    |   |-- golden/
    |   |   |-- test-boot-deterministic.json
    |   |   |-- test-entropy-reduction.json
    |   |   |-- test-gradient-decay.json
    |   |   |-- test-momentum-arcs.json

NOTE: Files under STATE/cognitive/ that are listed as EXISTING are
already produced by the current neuro system. This spec does NOT
redefine them. It consumes them as inputs. The four NEW files are:
session.momentum.json, continuity.kernel.json,
state.gradient.ledger.json, and session.state.json.

The kernel/archive/ directory stores historical snapshots for replay
and audit. Files are named with ISO-8601 timestamps (colons replaced
by hyphens for filesystem safety). Retention: 30 sessions. Older
files are deleted by the end-of-session pipeline.

------------------------------------------------------------------------

## 3. CANONICAL DATA SCHEMAS

### 3.1 interoception.sample.json (EXISTING format, documented here)

    {
      "timestamp": "2026-02-24T02:26:44.571Z",
      "telemetry": {
        "context_pressure": 0.008,
        "error_frequency": 0.65,
        "success_rate_rolling": 0.509,
        "novelty_exposure": 1.0,
        "memory_pressure": 0.006,
        "goal_gap": 0.0
      },
      "affective": {
        "stress": 0.264,
        "confidence": 0.445,
        "hunger": 0.0
      }
    }

All values are floats in [0.0, 1.0].

### 3.2 valence.tag.json (EXISTING format, documented here)

    {
      "score": 0.715,
      "components": {
        "trustHealth": 0.5,
        "stakesWinRate": 0.5,
        "errorRate": 0.0,
        "buildSuccess": 0.7,
        "interruptLoad": 0.0
      },
      "mood": "steady",
      "timestamp": "2026-02-24T02:26:44.560Z"
    }

Mood is one of: "distressed", "anxious", "steady", "engaged", "flow".
Score is a float in [0.0, 1.0].

### 3.3 session.momentum.json (NEW)

    {
      "version": "1.0.0",
      "session_id": "session-2026-02-24-001",
      "timestamp_start": "2026-02-24T02:00:00.000Z",
      "timestamp_end": "2026-02-24T06:30:00.000Z",
      "momentum_score": 0.74,
      "arc_type": "clean",
      "components": {
        "tests_green_ratio": 0.95,
        "issues_fixed_count": 3,
        "issues_opened_count": 1,
        "unresolved_debt_delta": -2,
        "effort_proxy": 0.72,
        "interoception_avg": {
          "stress": 0.20,
          "confidence": 0.68,
          "hunger": 0.15
        },
        "valence_trend": 0.08,
        "error_recovery_ratio": 0.80,
        "task_completion_ratio": 0.85
      },
      "weights": {
        "tests_green_ratio": 0.20,
        "net_issues_ratio": 0.10,
        "unresolved_debt_delta": 0.10,
        "effort_proxy": 0.10,
        "stress_inverse": 0.10,
        "confidence": 0.15,
        "valence_trend": 0.05,
        "error_recovery_ratio": 0.10,
        "task_completion_ratio": 0.10
      },
      "momentum_history": [0.65, 0.70, 0.74]
    }

Field definitions:

  momentum_score        Float [0.0, 1.0]. Weighted composite.
  arc_type              One of: "clean", "recovery", "messy", "stalled".
  tests_green_ratio     Passing tests / total tests. 1.0 if no tests ran.
  issues_fixed_count    Integer >= 0. Bugs or issues closed this session.
  issues_opened_count   Integer >= 0. New issues discovered this session.
  unresolved_debt_delta Integer. Negative = debt reduced. Positive = grew.
  effort_proxy          Float [0.0, 1.0]. Normalized session duration
                        relative to a 4-hour reference session.
                        Formula: min(1.0, duration_minutes / 240.0)
  interoception_avg     Mean of interoceptive samples across the session.
  valence_trend         Float [-1.0, 1.0]. Slope of valence scores.
                        Positive = improving mood. Computed by linear
                        regression over valence-history.jsonl entries
                        within this session's time window.
  error_recovery_ratio  Float [0.0, 1.0]. (errors_encountered -
                        errors_unresolved) / errors_encountered.
                        1.0 if no errors encountered.
  task_completion_ratio Float [0.0, 1.0]. tasks_completed / tasks_created.
                        1.0 if no tasks created.
  momentum_history      Array of up to 10 most recent momentum_score
                        values (oldest first). Sourced from archived
                        session.momentum.json files.

Weight justification:
  - tests_green_ratio (0.20): Strongest objective signal of session health.
    Green tests = working code. Highest weight because it is binary-testable.
  - confidence (0.15): Directly reflects agent self-assessment of capability
    within the session. Second highest because it integrates multiple signals.
  - net_issues_ratio (0.10): Measures whether the session created more
    problems than it solved. Moderate weight -- can be noisy.
  - unresolved_debt_delta (0.10): Tracks technical debt trajectory.
    Important but slow-moving. Moderate weight.
  - effort_proxy (0.10): Longer productive sessions deserve credit, but
    length alone is not quality. Moderate weight.
  - stress_inverse (0.10): Low stress correlates with sustainable velocity.
    Inverse because high stress degrades quality.
  - error_recovery_ratio (0.10): Measures resilience. Did the session
    recover from errors or leave them open?
  - task_completion_ratio (0.10): Measures follow-through. Did created
    tasks get finished?
  - valence_trend (0.05): Mood trajectory. Lowest weight because it is
    the most subjective and most noisy signal.

  Total: 1.00

### 3.4 continuity.kernel.json (NEW)

    {
      "version": "1.0.0",
      "generated_at": "2026-02-24T06:35:00.000Z",
      "source_session_id": "session-2026-02-24-001",
      "reconstruction_entropy": 3.42,
      "previous_entropy": 4.81,
      "entropy_delta": -1.39,
      "kernel_size_bytes": 2048,
      "snippet_count": 8,
      "snippets": [
        {
          "rank": 1,
          "source_file": "collaboration-log.json",
          "source_type": "collaboration",
          "byte_offset_start": 14200,
          "byte_offset_end": 14850,
          "line_range": [438, 446],
          "content_hash": "sha256:a1b2c3d4e5f6...",
          "relevance_score": 0.92,
          "recency_score": 0.95,
          "momentum_boost": 0.10,
          "final_score": 0.94,
          "summary_tag": "latest-session-entry"
        },
        {
          "rank": 2,
          "source_file": "decisions.md",
          "source_type": "decision",
          "byte_offset_start": 3200,
          "byte_offset_end": 3680,
          "line_range": [88, 112],
          "content_hash": "sha256:b2c3d4e5f6a1...",
          "relevance_score": 0.88,
          "recency_score": 0.60,
          "momentum_boost": 0.05,
          "final_score": 0.82,
          "summary_tag": "dual-runtime-decision"
        }
      ],
      "file_manifest": {
        "total_files_scanned": 25,
        "files_selected": 12,
        "files_skipped": 13,
        "selection_rationale": [
          {"file": "valence-history.jsonl", "reason": "stale-no-change"},
          {"file": "sensors.json", "reason": "below-relevance-threshold"}
        ]
      },
      "boot_priority_order": [
        "session.state.json",
        "state.gradient.ledger.json",
        "session.momentum.json",
        "gsv-state.json",
        "interoceptive-state.json",
        "collaboration-log.json",
        "decisions.md",
        "opinions.md"
      ]
    }

Key constraints:
  - snippets[].content_hash is SHA-256 of the raw bytes at the given
    offsets. This ensures auditability: the kernel never rewrites content,
    only selects and cites.
  - Maximum 20 snippets per kernel.
  - Maximum 8192 bytes total across all snippets.
  - If a source file has changed since the hash was computed (detected
    by re-hashing at boot), the snippet is marked stale and excluded.

### 3.5 state.gradient.ledger.json (NEW)

    {
      "version": "1.0.0",
      "timestamp": "2026-02-24T06:35:00.000Z",
      "session_id": "session-2026-02-24-001",
      "signal_gradients": [
        {
          "id": "sg-001",
          "source": "interoception",
          "dimension": "stress",
          "value": -0.044,
          "direction": "decrease",
          "sessions_age": 0,
          "decay_rate": 0.30,
          "effective_value": -0.044,
          "created_session": "session-2026-02-24-001"
        },
        {
          "id": "sg-002",
          "source": "interoception",
          "dimension": "confidence",
          "value": 0.075,
          "direction": "increase",
          "sessions_age": 0,
          "decay_rate": 0.30,
          "effective_value": 0.075,
          "created_session": "session-2026-02-24-001"
        },
        {
          "id": "sg-003",
          "source": "valence",
          "dimension": "buildSuccess",
          "value": 0.10,
          "direction": "increase",
          "sessions_age": 1,
          "decay_rate": 0.30,
          "effective_value": 0.07,
          "created_session": "session-2026-02-23-001"
        }
      ],
      "decision_gradients": [
        {
          "id": "dg-001",
          "source": "decisions.md",
          "decision_ref": "ADR-004",
          "dimension": "reward_drive",
          "value": 0.05,
          "direction": "increase",
          "sessions_age": 0,
          "decay_rate": 0.10,
          "effective_value": 0.05,
          "created_session": "session-2026-02-24-001",
          "rationale": "Survival economics reinforced by successful test suite"
        },
        {
          "id": "dg-002",
          "source": "opinions.md",
          "decision_ref": "OP-005",
          "dimension": "focus",
          "value": 0.03,
          "direction": "increase",
          "sessions_age": 2,
          "decay_rate": 0.10,
          "effective_value": 0.0243,
          "created_session": "session-2026-02-22-001",
          "rationale": "Provider trait refactoring increases focus on ISP"
        }
      ],
      "momentum_gradient": {
        "id": "mg-001",
        "source": "session.momentum.json",
        "dimension": "exploration",
        "value": 0.02,
        "direction": "increase",
        "sessions_age": 0,
        "decay_rate": 0.20,
        "effective_value": 0.02,
        "created_session": "session-2026-02-24-001"
      },
      "gsv_deltas": {
        "exploration": 0.02,
        "stability": 0.01,
        "focus": 0.03,
        "urgency": -0.01,
        "reward_drive": 0.05
      },
      "policy_stability": 0.92,
      "gradient_drift": 0.08
    }

Field definitions:

  signal_gradients      Array. Derived from telemetry deltas between
                        sessions (interoception, valence, sensory filter).
                        Decay rate: 0.30 per session (fast decay -- these
                        are ephemeral signals).

  decision_gradients    Array. Derived from new or modified decisions and
                        opinions. Decay rate: 0.10 per session (slow decay
                        -- commitments persist longer than feelings).

  momentum_gradient     Single object. Derived from momentum_score delta
                        between this and previous session. Decay rate: 0.20
                        (medium -- momentum is a meta-signal).

  gsv_deltas            Object mapping GSV vector dimensions to net delta
                        values. Computed by summing all effective_value
                        entries grouped by their target dimension.

  policy_stability      Float [0.0, 1.0]. Measures how much the GSV
                        vector changed. Formula in Section 4.

  gradient_drift        Float [0.0, 1.0]. Complement of policy_stability.
                        gradient_drift = 1.0 - policy_stability.

Decay formula:
  effective_value = value * (1.0 - decay_rate) ^ sessions_age

Gradient retirement:
  When |effective_value| < 0.001, the gradient is removed from the ledger.

### 3.6 session.state.json (NEW -- unified boot state)

    {
      "version": "1.0.0",
      "boot_timestamp": "2026-02-24T07:00:00.000Z",
      "reconstruction_entropy": 3.42,
      "momentum": {
        "current": 0.74,
        "trend": [0.65, 0.70, 0.74],
        "arc_type": "clean"
      },
      "gsv_vector": {
        "exploration": 0.66,
        "stability": 0.613,
        "focus": 0.537,
        "urgency": 0.252,
        "reward_drive": 0.556
      },
      "gsv_modulation": {
        "exploration_depth": "broad",
        "variance_mode": "conservative",
        "verbosity": "detailed",
        "planning_horizon": "long",
        "goal_intensity": "balanced"
      },
      "interoception": {
        "stress": 0.264,
        "confidence": 0.445,
        "hunger": 0.0
      },
      "valence": {
        "score": 0.715,
        "mood": "steady"
      },
      "active_context": [
        {
          "source": "collaboration-log.json",
          "summary_tag": "latest-session-entry",
          "content_hash": "sha256:a1b2c3d4e5f6...",
          "line_range": [438, 446]
        }
      ],
      "gradient_summary": {
        "signal_count": 3,
        "decision_count": 2,
        "net_gsv_deltas": {
          "exploration": 0.02,
          "stability": 0.01,
          "focus": 0.03,
          "urgency": -0.01,
          "reward_drive": 0.05
        },
        "policy_stability": 0.92
      },
      "boot_files_read": 12,
      "boot_files_skipped": 13,
      "boot_duration_ms": 45
    }

This is the SINGLE object the agent consumes at session start. It
replaces the need to read and parse 25+ files independently.

------------------------------------------------------------------------

## 4. ALGORITHMS AND FORMULAS

### 4.1 Reconstruction Entropy Calculation

Measures the information cost of rebuilding session context from
persisted artifacts. Lower is better.

Inputs:
  N       = total number of memory files scanned
  s_i     = size in bytes of file i
  S       = sum of all s_i
  c_i     = 1 if file i changed since last session, 0 otherwise
  r_i     = relevance score of file i (from sensory filter or default 0.5)

Formula:

  H_raw = -SUM(i=1..N) [ (s_i / S) * log2(s_i / S) ]

This is Shannon entropy over the byte-weight distribution of files.

  H_change = SUM(i=1..N) [ c_i * (s_i / S) ]

Fraction of bytes that changed.

  H_relevance = 1.0 - (SUM(i=1..N) [r_i * (s_i / S)] / N)

Inverse of weighted-average relevance. Low relevance = high entropy.

  reconstruction_entropy = H_raw * (1.0 + H_change) * (1.0 + H_relevance)

Properties:
  - Minimum possible: approaches 0 when one tiny file contains everything
    and nothing changed and everything is relevant.
  - Increases with: more files, larger files, more changes, lower relevance.
  - Units: bits (information-theoretic).

After kernel selection, entropy is recalculated over only the selected
snippets to produce the reduced entropy score.

### 4.2 Momentum Score Calculation

Inputs (all from session telemetry and artifacts):
  tgr    = tests_green_ratio                            [0.0, 1.0]
  nir    = 1.0 - (issues_opened / max(issues_fixed, 1)) [0.0, 1.0]
           Clamped to [0.0, 1.0].
  udd    = sigmoid_norm(unresolved_debt_delta)           [0.0, 1.0]
           sigmoid_norm(x) = 1.0 / (1.0 + e^(0.5 * x))
           Negative delta (debt reduced) yields > 0.5.
           Positive delta (debt grew) yields < 0.5.
  ep     = effort_proxy                                  [0.0, 1.0]
  si     = 1.0 - interoception_avg.stress                [0.0, 1.0]
  conf   = interoception_avg.confidence                  [0.0, 1.0]
  vt     = (valence_trend + 1.0) / 2.0                   [0.0, 1.0]
           Normalized from [-1, 1] to [0, 1].
  err    = error_recovery_ratio                          [0.0, 1.0]
  tcr    = task_completion_ratio                         [0.0, 1.0]

Weights (W):
  W = {tgr: 0.20, nir: 0.10, udd: 0.10, ep: 0.10,
       si: 0.10, conf: 0.15, vt: 0.05, err: 0.10, tcr: 0.10}

Formula:
  momentum_score = W.tgr * tgr
                 + W.nir * nir
                 + W.udd * udd
                 + W.ep  * ep
                 + W.si  * si
                 + W.conf * conf
                 + W.vt  * vt
                 + W.err * err
                 + W.tcr * tcr

Arc type classification:
  If momentum_score >= 0.70 AND valence_trend >= 0.0:  "clean"
  If momentum_score >= 0.50 AND valence_trend >= 0.05: "recovery"
  If momentum_score >= 0.30:                           "messy"
  Otherwise:                                           "stalled"

Tie-break for arc classification: apply rules top-to-bottom, first
match wins. This is deterministic because the conditions are ordered
by strictness.

### 4.3 Gradient Extraction and Decay

#### 4.3.1 Signal Gradient Extraction

At end-of-session, compare current interoception and valence to the
values stored in the PREVIOUS session's momentum file.

For each dimension D in {stress, confidence, hunger, context_pressure,
error_frequency, success_rate_rolling, buildSuccess, errorRate}:

  delta_D = current_value_D - previous_value_D

If |delta_D| >= 0.01 (noise floor), create a signal gradient:
  {
    id: "sg-" + incrementing_counter,
    source: source_file_name,
    dimension: map_to_gsv_dimension(D),
    value: delta_D,
    direction: "increase" if delta_D > 0 else "decrease",
    sessions_age: 0,
    decay_rate: 0.30,
    effective_value: delta_D,
    created_session: current_session_id
  }

GSV dimension mapping:
  stress          -> urgency (inverted: negative stress delta = negative urgency delta)
  confidence      -> stability
  hunger          -> reward_drive
  context_pressure -> focus (inverted)
  error_frequency -> stability (inverted)
  success_rate    -> exploration
  buildSuccess    -> stability
  errorRate       -> urgency

When multiple signals map to the same GSV dimension, they are summed.

#### 4.3.2 Decision Gradient Extraction

Scan decisions.md and opinions.md for entries with dates matching the
current session date. For each new or modified entry:

  Parse the ADR/OP identifier (e.g., ADR-004, OP-005).
  Determine the relevant GSV dimension based on the scope field:
    Engineering   -> focus
    Infrastructure -> stability
    Philosophy    -> reward_drive
    Product       -> exploration
    Governance    -> stability

  Assign a fixed gradient magnitude of 0.03 for new decisions and
  0.02 for new opinions. Direction is always "increase" for the
  mapped dimension (decisions and opinions represent commitments
  that increase focus/stability/drive).

  Create a decision gradient with decay_rate 0.10.

#### 4.3.3 Momentum Gradient Extraction

Compare current momentum_score to previous session momentum_score.

  delta_m = current_momentum - previous_momentum

If |delta_m| >= 0.01:
  Map to GSV dimension:
    If delta_m > 0: exploration (momentum rising = explore more)
    If delta_m < 0: stability (momentum falling = stabilize)

  Create a momentum gradient with decay_rate 0.20.

#### 4.3.4 Decay Application

At the START of each end-of-session pipeline run, before extracting
new gradients:

  For each existing gradient G in the ledger:
    G.sessions_age += 1
    G.effective_value = G.value * (1.0 - G.decay_rate) ^ G.sessions_age

  Remove any gradient where |G.effective_value| < 0.001.

#### 4.3.5 GSV Delta Computation

After all gradients (signal, decision, momentum) are in the ledger:

  For each GSV dimension D in {exploration, stability, focus, urgency,
  reward_drive}:

    gsv_deltas[D] = SUM of effective_value for all gradients targeting D

  Clamp each delta to [-0.15, +0.15] (safety gate -- see Section 6).

#### 4.3.6 Policy Stability Computation

  old_gsv = previous session's GSV vector (5 dimensions)
  new_gsv = old_gsv + gsv_deltas (element-wise, clamped to [0, 1])

  euclidean_distance = sqrt(SUM((new_gsv[D] - old_gsv[D])^2 for D))
  max_possible_distance = sqrt(5 * 0.15^2) = sqrt(0.1125) = 0.3354

  policy_stability = 1.0 - (euclidean_distance / max_possible_distance)
  gradient_drift = 1.0 - policy_stability

### 4.4 Kernel Selection and Compression

The CK selects snippets from artifact files to minimize reconstruction
entropy for the next session boot.

#### 4.4.1 File Scoring

For each file F in the memory file set:

  recency_score(F)   = 1.0 if modified in the current session,
                        0.5 if modified in the previous session,
                        0.25 if modified 2 sessions ago,
                        0.1 otherwise.

  relevance_score(F) = value from sensory-filter.json if available,
                        0.5 as default.

  momentum_boost(F)  = 0.10 if the file contributed to a momentum_score
                        component that was in the top-3 weighted
                        contributors. 0.0 otherwise.

  final_score(F)     = 0.50 * relevance_score(F)
                      + 0.35 * recency_score(F)
                      + 0.15 * momentum_boost(F)

Tie-break: if two files have identical final_score, prefer the file
with the smaller byte size (less entropy per bit). If byte sizes are
also identical, prefer alphabetical order of filename.

#### 4.4.2 Snippet Extraction

For each selected file (final_score >= 0.30), extract the most
relevant contiguous byte range:

  1. If the file is JSON: extract the most recently modified top-level
     key (by timestamp field if present, else last key in document order).
  2. If the file is Markdown: extract the last H2 or H3 section.
  3. If the file is JSONL: extract the last 5 lines.

  Maximum snippet size: 512 bytes. If the extracted region exceeds 512
  bytes, truncate at the nearest complete JSON object or markdown
  paragraph boundary before the limit.

  Compute SHA-256 hash of the extracted bytes. Store byte offsets and
  line range for auditability.

#### 4.4.3 Kernel Assembly

  1. Sort snippets by final_score descending.
  2. Include snippets until total bytes >= 8192 or count >= 20.
  3. Record the file_manifest with selection rationale.
  4. Compute reduced reconstruction entropy over only the selected
     files (using the formula from 4.1 but with N = selected files).

#### 4.4.4 Boot Priority Order

Fixed priority order for reading files at boot (highest priority first):

  1. session.state.json (if exists and not stale -- see 5.2)
  2. state.gradient.ledger.json
  3. session.momentum.json
  4. gsv-state.json
  5. interoceptive-state.json
  6. continuity.kernel.json (for snippet references)
  7. collaboration-log.json (latest session entry only)
  8. decisions.md (latest ADR only)
  9. opinions.md (latest OP only)
  10-25. Remaining files in descending final_score order.

### 4.5 Policy Modulation Mapping to GSV Inputs

The SGL gsv_deltas are applied to the GSV vector at boot:

  new_gsv[D] = clamp(old_gsv[D] + gsv_deltas[D], 0.0, 1.0)

The GSV vector maps to modulation policy as follows (existing mapping,
documented here for completeness):

  exploration -> exploration_depth:
    [0.0, 0.33) = "narrow"
    [0.33, 0.66) = "balanced"  (TIE-BREAK: exact 0.33 goes to "balanced")
    [0.66, 1.0] = "broad"

  stability -> variance_mode:
    [0.0, 0.33) = "aggressive"
    [0.33, 0.66) = "moderate"
    [0.66, 1.0] = "conservative"

  focus -> verbosity:
    [0.0, 0.33) = "terse"
    [0.33, 0.66) = "standard"
    [0.66, 1.0] = "detailed"

  urgency -> planning_horizon:
    [0.0, 0.33) = "long"
    [0.33, 0.66) = "medium"
    [0.66, 1.0] = "short"
    NOTE: Higher urgency = shorter planning horizon. This is inverted.

  reward_drive -> goal_intensity:
    [0.0, 0.33) = "passive"
    [0.33, 0.66) = "balanced"
    [0.66, 1.0] = "driven"

------------------------------------------------------------------------

## 5. UPDATE PIPELINE

### 5.1 End-of-Session Write Steps

Execute in this exact order. If any step fails, log the error and
continue to the next step (fail-open for writes, fail-closed for
reads that feed subsequent writes).

  STEP 1: SNAPSHOT INTEROCEPTION
    Read interoceptive-state.json.
    Read valence.json.
    Read valence-history.jsonl (entries within session time window).
    Read brain-dashboard.json.
    Read stakes-summary.json.

  STEP 2: COMPUTE MOMENTUM
    Gather session telemetry:
      - tests_green_ratio: from latest build results (brain-dashboard
        or CI output). Default 0.5 if unavailable.
      - issues_fixed_count / issues_opened_count: from collaboration-log
        session entry or default 0.
      - unresolved_debt_delta: from collaboration-log thread status
        changes or default 0.
      - effort_proxy: from session timestamps.
      - interoception_avg: mean of all interoceptive samples in
        valence-history.jsonl for this session window.
      - valence_trend: linear regression slope of valence scores.
      - error_recovery_ratio: from error telemetry.
      - task_completion_ratio: from task tracking.

    Compute momentum_score using formula from 4.2.
    Classify arc_type using rules from 4.2.
    Load previous momentum_history from archived momentum files.
    Append current momentum_score. Truncate to last 10.
    Write session.momentum.json.

  STEP 3: UPDATE GRADIENT LEDGER
    Read previous state.gradient.ledger.json (or initialize empty).
    Apply decay to all existing gradients (4.3.4).
    Extract new signal gradients (4.3.1).
    Extract new decision gradients (4.3.2).
    Extract momentum gradient (4.3.3).
    Compute gsv_deltas (4.3.5).
    Compute policy_stability (4.3.6).
    Write state.gradient.ledger.json.

  STEP 4: BUILD CONTINUITY KERNEL
    Scan all memory files.
    Score each file (4.4.1).
    Extract snippets from top-scoring files (4.4.2).
    Assemble kernel (4.4.3).
    Compute reconstruction entropy (4.1) for both full set and
    kernel-selected set.
    Write continuity.kernel.json.

  STEP 5: ARCHIVE
    Copy session.momentum.json to:
      kernel/archive/session-{ISO-TIMESTAMP}.momentum.json
    Copy state.gradient.ledger.json to:
      kernel/archive/session-{ISO-TIMESTAMP}.gradients.json
    Copy continuity.kernel.json to:
      kernel/archive/session-{ISO-TIMESTAMP}.kernel.json
    Prune kernel/archive/ to retain only the 30 most recent files
    per type (momentum, gradients, kernel). Delete oldest first.

  STEP 6: DELETE session.state.json
    Remove the previous boot state so the next boot rebuilds fresh.
    This ensures the boot sequence always runs and the state is never
    served stale across multiple sessions.

### 5.2 Next-Session Boot Steps

Execute in this exact order. Steps are fail-closed: if a critical
file cannot be read, abort boot and fall back to naive read-all.

  STEP 1: CHECK KERNEL FRESHNESS
    Read continuity.kernel.json.
    If generated_at is older than 7 days, discard it and fall back
    to naive boot (read all files in default order).
    If any snippet content_hash does not match the current file
    bytes at the recorded offsets, mark that snippet as stale and
    exclude it from active_context.

  STEP 2: READ GRADIENT LEDGER
    Read state.gradient.ledger.json.
    Extract gsv_deltas.

  STEP 3: READ CURRENT GSV STATE
    Read gsv-state.json.
    Apply gsv_deltas to produce new GSV vector.
    Clamp each dimension to [0.0, 1.0].

  STEP 4: MAP GSV TO MODULATION POLICY
    Apply mapping rules from 4.5.
    Write gsv-state.json with new vector.
    Write gsv-modulation.json with new policy.

  STEP 5: READ MOMENTUM
    Read session.momentum.json.
    Extract momentum_score, arc_type, momentum_history.

  STEP 6: READ INTEROCEPTION
    Read interoceptive-state.json.
    Read valence.json.

  STEP 7: ASSEMBLE SESSION STATE
    Compose session.state.json from all read data:
      - gsv_vector (post-delta)
      - gsv_modulation (post-mapping)
      - momentum (current, trend, arc)
      - interoception
      - valence
      - active_context (non-stale snippets from kernel)
      - gradient_summary (counts and net deltas)
      - boot metadata (files read, skipped, duration)
    Write session.state.json.

  STEP 8: RETURN SessionState
    The assembled session.state.json IS the SessionState object.
    No further processing needed. The agent reads this single file
    to understand its full starting context.

### 5.3 Conflict Resolution and Tie-Break Rules

  RULE 1: TIMESTAMP WINS
    If two files contain conflicting values for the same field, the
    file with the more recent timestamp wins. Timestamps are compared
    as ISO-8601 strings (lexicographic comparison is valid for ISO-8601).

  RULE 2: GRADIENT COLLISION
    If two gradients target the same GSV dimension with opposite
    directions, they are summed algebraically. The net result
    determines direction. If the net is exactly 0.0, the gradient
    is discarded (no bias).

  RULE 3: SNIPPET COLLISION
    If two snippets from the same source file overlap in byte ranges,
    keep the snippet with the higher final_score. If scores are equal,
    keep the snippet with the larger byte range (more context).

  RULE 4: MISSING FILE
    If any input file does not exist, use these defaults:
      interoceptive-state.json:   all telemetry 0.0, affective 0.5/0.5/0.0
      valence.json:               score 0.5, mood "steady"
      gsv-state.json:             all dimensions 0.5
      session.momentum.json:      momentum_score 0.5, arc_type "messy"
      state.gradient.ledger.json: empty gradients, gsv_deltas all 0.0
      continuity.kernel.json:     empty snippets, entropy 10.0

  RULE 5: ARCHIVE OVERFLOW
    When archive exceeds 30 entries per type, delete entries with
    the oldest timestamps first. Tie-break: alphabetical filename.

------------------------------------------------------------------------

## 6. INVARIANTS AND SAFETY CONSTRAINTS

### 6.1 Behavioral Safety Gate

INVARIANT: No gradient may amplify aggressive, unsafe, or destabilizing
behavior beyond defined bounds.

  GATE-1: GSV delta clamp.
    Each gsv_deltas[D] is clamped to [-0.15, +0.15].
    This prevents any single session from radically shifting behavior.

  GATE-2: Absolute GSV bounds.
    After applying deltas, each GSV dimension is clamped to [0.0, 1.0].
    urgency is further clamped to [0.0, 0.80] to prevent runaway urgency
    from shortening planning horizon excessively.

  GATE-3: Stress amplification block.
    If interoception stress > 0.70, all positive urgency gradients are
    zeroed. Rationale: high stress sessions should not bias future
    sessions toward more urgency. The system must calm down, not
    accelerate.

  GATE-4: Confidence floor.
    If the effective confidence gradient would push GSV stability
    below 0.20, the gradient is clamped to maintain stability >= 0.20.
    Rationale: the agent must never become so unstable that it cannot
    execute basic operations.

  GATE-5: Momentum decay floor.
    momentum_score is clamped to [0.10, 1.0]. A completely zero
    momentum would cause division-by-zero in CK scoring (momentum_boost
    is multiplicative in some paths). The 0.10 floor ensures the
    system always has baseline momentum.

  GATE-6: Gradient count cap.
    Maximum 50 active gradients in the ledger. If extraction would
    exceed 50, discard the gradient with the smallest
    |effective_value|. Tie-break: discard the oldest gradient.

### 6.2 Data Integrity Invariants

  INV-1: Hash verification.
    Every snippet in continuity.kernel.json includes a SHA-256 hash.
    At boot, hashes are re-verified. Any mismatch causes snippet
    exclusion (not boot failure).

  INV-2: Monotonic session IDs.
    session_id follows the pattern "session-YYYY-MM-DD-NNN" where
    NNN is a zero-padded 3-digit sequence number starting at 001.
    IDs must be strictly increasing. If a duplicate is detected,
    append a 4-character random suffix: "session-YYYY-MM-DD-NNN-xxxx".

  INV-3: Weight sum.
    SMM weights must sum to exactly 1.00. Checked at compute time.
    If they do not sum to 1.00 due to floating point error, normalize
    by dividing each weight by the actual sum.

  INV-4: Idempotent writes.
    Running the end-of-session pipeline twice with the same inputs
    produces identical outputs (deterministic). The only variable is
    the timestamp, which must be passed as an argument, not read from
    the system clock, during replay/test scenarios.

  INV-5: Archive immutability.
    Files in kernel/archive/ are never modified after creation.
    They are only created (write-once) or deleted (pruning).

### 6.3 Consistency Invariants

  CON-1: session.state.json is always deleted at end-of-session
         and always recreated at boot. It never persists across
         two consecutive sessions.

  CON-2: gsv_deltas in the ledger and the actual delta applied to
         gsv-state.json at boot must be identical. The boot sequence
         reads gsv_deltas from the ledger and applies them verbatim
         (after clamping).

  CON-3: reconstruction_entropy in continuity.kernel.json must be
         computed from the FULL file set, not the selected subset.
         The kernel entropy (over selected files) is a separate
         metric for comparison.

------------------------------------------------------------------------

## 7. FAILURE MODES AND RECOVERY

### FM-1: Missing or Corrupted Input Files

  Symptom: File does not exist or fails JSON parse.
  Recovery: Use defaults from RULE 4 (Section 5.3). Log warning.
  Impact: Degraded accuracy but system continues.

### FM-2: Hash Mismatch at Boot

  Symptom: Snippet hash does not match current file content.
  Recovery: Exclude the stale snippet. Re-read the source file
  directly if it scores above 0.30 in file scoring.
  Impact: Slightly higher reconstruction entropy. Logged as warning.

### FM-3: Gradient Ledger Corruption

  Symptom: state.gradient.ledger.json fails parse or has invalid
  gradient entries (missing fields, out-of-range values).
  Recovery: Initialize empty ledger. All gradients are lost for this
  transition. gsv_deltas default to all zeros. Log error.
  Impact: One session of behavioral continuity is lost. System
  recovers fully at the next end-of-session write.

### FM-4: Archive Directory Missing

  Symptom: kernel/archive/ does not exist.
  Recovery: Create the directory. momentum_history defaults to
  empty array (current session only).
  Impact: No historical momentum trend available. Recovers as
  sessions accumulate.

### FM-5: Stale Kernel (> 7 days old)

  Symptom: continuity.kernel.json generated_at is older than 7 days.
  Recovery: Discard kernel entirely. Boot falls back to reading all
  files in boot_priority_order without snippet optimization.
  Impact: Higher boot cost (more bytes read). No data loss.

### FM-6: Disk Full During Write

  Symptom: Write fails with ENOSPC.
  Recovery: Skip archive step (STEP 5). Write critical files
  (momentum, ledger) first. If even those fail, log error and
  abort pipeline. Next session boots with stale data.
  Impact: One session's continuity data may be lost. Self-heals
  at next successful write.

### FM-7: Concurrent Session Conflict

  Symptom: Two sessions run simultaneously and both try to write
  end-of-session state.
  Recovery: Use file locking (flock or equivalent). If lock cannot
  be acquired within 5 seconds, abort the write and log error.
  The first session to acquire the lock wins.
  Impact: Second session loses its end-of-session data. The system
  is designed for single-session operation; concurrent sessions are
  an edge case.

------------------------------------------------------------------------

## 8. TEST PLAN

### 8.1 Test Infrastructure

Tests use golden files stored in kernel/golden/. Each test provides
deterministic inputs and asserts exact outputs. Timestamps are
injected as arguments (never read from system clock during tests).

All test assertions use exact floating-point comparison with epsilon
1e-6 to account for IEEE-754 rounding.

### 8.2 Test: Deterministic Boot

File: kernel/golden/test-boot-deterministic.json

    {
      "test_id": "boot-deterministic-001",
      "description": "Same inputs produce identical SessionState across runs",
      "inputs": {
        "gsv_state": {
          "exploration": 0.50, "stability": 0.50, "focus": 0.50,
          "urgency": 0.50, "reward_drive": 0.50
        },
        "interoception": {
          "stress": 0.30, "confidence": 0.60, "hunger": 0.10
        },
        "valence": {"score": 0.70, "mood": "steady"},
        "gradient_ledger": {
          "gsv_deltas": {
            "exploration": 0.05, "stability": -0.03,
            "focus": 0.02, "urgency": 0.0, "reward_drive": 0.01
          }
        },
        "kernel": {"snippets": [], "reconstruction_entropy": 4.0},
        "momentum": {"current": 0.65, "trend": [0.60, 0.65], "arc_type": "clean"},
        "boot_timestamp": "2026-03-01T08:00:00.000Z"
      },
      "expected_output": {
        "gsv_vector": {
          "exploration": 0.55, "stability": 0.47, "focus": 0.52,
          "urgency": 0.50, "reward_drive": 0.51
        },
        "gsv_modulation": {
          "exploration_depth": "balanced",
          "variance_mode": "moderate",
          "verbosity": "standard",
          "planning_horizon": "medium",
          "goal_intensity": "balanced"
        },
        "momentum": {"current": 0.65, "trend": [0.60, 0.65], "arc_type": "clean"},
        "reconstruction_entropy": 4.0,
        "boot_timestamp": "2026-03-01T08:00:00.000Z"
      },
      "assertions": [
        "Output matches expected_output exactly (epsilon 1e-6)",
        "Running boot twice with same inputs produces byte-identical output",
        "gsv_modulation maps correctly from gsv_vector thresholds"
      ]
    }

### 8.3 Test: Entropy Reduction Over Multiple Sessions

File: kernel/golden/test-entropy-reduction.json

    {
      "test_id": "entropy-reduction-001",
      "description": "Kernel selection reduces entropy monotonically over 3 sessions",
      "sessions": [
        {
          "session_id": "session-2026-03-01-001",
          "file_set_sizes": [1000, 2000, 500, 3000, 800],
          "files_changed": [true, true, true, true, true],
          "relevance_scores": [0.5, 0.5, 0.5, 0.5, 0.5],
          "expected_entropy_raw": 2.154,
          "kernel_selected_files": 5,
          "expected_entropy_reduced": 2.154
        },
        {
          "session_id": "session-2026-03-02-001",
          "file_set_sizes": [1000, 2000, 500, 3000, 800],
          "files_changed": [false, true, false, false, true],
          "relevance_scores": [0.5, 0.8, 0.3, 0.5, 0.9],
          "expected_entropy_raw": 2.802,
          "kernel_selected_files": 3,
          "expected_entropy_reduced": 1.521
        },
        {
          "session_id": "session-2026-03-03-001",
          "file_set_sizes": [1000, 2100, 500, 3000, 850],
          "files_changed": [false, true, false, false, false],
          "relevance_scores": [0.4, 0.9, 0.2, 0.4, 0.7],
          "expected_entropy_raw": 2.954,
          "kernel_selected_files": 2,
          "expected_entropy_reduced": 0.998
        }
      ],
      "assertions": [
        "entropy_reduced[session_N+1] <= entropy_reduced[session_N]",
        "kernel_selected_files decreases or stays constant as relevance concentrates",
        "Files with relevance < 0.30 are never selected"
      ]
    }

### 8.4 Test: Gradient Decay Correctness

File: kernel/golden/test-gradient-decay.json

    {
      "test_id": "gradient-decay-001",
      "description": "Gradients decay correctly and are retired below threshold",
      "initial_gradients": [
        {
          "id": "sg-001", "value": 0.10, "decay_rate": 0.30,
          "sessions_age": 0, "dimension": "exploration"
        },
        {
          "id": "dg-001", "value": 0.05, "decay_rate": 0.10,
          "sessions_age": 0, "dimension": "focus"
        },
        {
          "id": "mg-001", "value": 0.08, "decay_rate": 0.20,
          "sessions_age": 0, "dimension": "stability"
        }
      ],
      "expected_after_sessions": [
        {
          "sessions_elapsed": 1,
          "sg-001_effective": 0.07,
          "dg-001_effective": 0.045,
          "mg-001_effective": 0.064,
          "all_alive": true
        },
        {
          "sessions_elapsed": 5,
          "sg-001_effective": 0.01681,
          "dg-001_effective": 0.029525,
          "mg-001_effective": 0.026214,
          "all_alive": true
        },
        {
          "sessions_elapsed": 15,
          "sg-001_effective": 0.000475,
          "dg-001_effective": 0.010293,
          "mg-001_effective": 0.002815,
          "sg-001_retired": true,
          "dg-001_retired": false,
          "mg-001_retired": false
        },
        {
          "sessions_elapsed": 30,
          "sg-001_effective": 0.0,
          "dg-001_effective": 0.002118,
          "mg-001_effective": 0.000099,
          "sg-001_retired": true,
          "dg-001_retired": false,
          "mg-001_retired": true
        },
        {
          "sessions_elapsed": 65,
          "sg-001_effective": 0.0,
          "dg-001_effective": 0.000058,
          "mg-001_effective": 0.0,
          "all_retired": true
        }
      ],
      "assertions": [
        "effective_value = value * (1 - decay_rate) ^ sessions_age",
        "Gradient retired when |effective_value| < 0.001",
        "Signal gradients (0.30 decay) retire before momentum (0.20) before decisions (0.10)",
        "Decision gradient dg-001 survives longest due to lowest decay rate"
      ]
    }

### 8.5 Test: Momentum Reacts to Arc Quality

File: kernel/golden/test-momentum-arcs.json

    {
      "test_id": "momentum-arcs-001",
      "description": "Clean arc produces high momentum; messy arc produces low",
      "arcs": [
        {
          "label": "clean_arc",
          "inputs": {
            "tests_green_ratio": 0.98,
            "issues_fixed_count": 4,
            "issues_opened_count": 0,
            "unresolved_debt_delta": -3,
            "effort_proxy": 0.75,
            "interoception_avg": {"stress": 0.10, "confidence": 0.85, "hunger": 0.05},
            "valence_trend": 0.15,
            "error_recovery_ratio": 1.0,
            "task_completion_ratio": 0.90
          },
          "expected_momentum_score": 0.888,
          "expected_arc_type": "clean"
        },
        {
          "label": "messy_arc",
          "inputs": {
            "tests_green_ratio": 0.40,
            "issues_fixed_count": 1,
            "issues_opened_count": 5,
            "unresolved_debt_delta": 4,
            "effort_proxy": 0.90,
            "interoception_avg": {"stress": 0.75, "confidence": 0.25, "hunger": 0.60},
            "valence_trend": -0.30,
            "error_recovery_ratio": 0.20,
            "task_completion_ratio": 0.30
          },
          "expected_momentum_score": 0.336,
          "expected_arc_type": "messy"
        },
        {
          "label": "recovery_arc",
          "inputs": {
            "tests_green_ratio": 0.70,
            "issues_fixed_count": 3,
            "issues_opened_count": 2,
            "unresolved_debt_delta": -1,
            "effort_proxy": 0.60,
            "interoception_avg": {"stress": 0.40, "confidence": 0.55, "hunger": 0.20},
            "valence_trend": 0.10,
            "error_recovery_ratio": 0.75,
            "task_completion_ratio": 0.65
          },
          "expected_momentum_score": 0.637,
          "expected_arc_type": "recovery"
        },
        {
          "label": "stalled_arc",
          "inputs": {
            "tests_green_ratio": 0.10,
            "issues_fixed_count": 0,
            "issues_opened_count": 3,
            "unresolved_debt_delta": 5,
            "effort_proxy": 0.20,
            "interoception_avg": {"stress": 0.90, "confidence": 0.10, "hunger": 0.80},
            "valence_trend": -0.50,
            "error_recovery_ratio": 0.0,
            "task_completion_ratio": 0.0
          },
          "expected_momentum_score": 0.143,
          "expected_arc_type": "stalled"
        }
      ],
      "assertions": [
        "clean_arc.momentum > recovery_arc.momentum > messy_arc.momentum > stalled_arc.momentum",
        "Arc types match expected classification rules",
        "All scores in [0.10, 1.0] (floor clamp active)"
      ]
    }

### 8.6 Additional Unit Tests (programmatic, not golden file)

  TEST-U1: Weight normalization.
    Verify that SMM weights sum to 1.0 within epsilon 1e-9.
    Verify that modifying a weight and not re-normalizing triggers
    an assertion failure.

  TEST-U2: GSV delta clamping.
    Inject gsv_deltas with values exceeding [-0.15, 0.15].
    Verify all deltas are clamped.
    Verify urgency is clamped to [0.0, 0.80] after application.

  TEST-U3: Stress amplification block.
    Set interoception stress to 0.75. Set positive urgency gradient.
    Verify the gradient is zeroed.

  TEST-U4: Confidence floor.
    Set stability to 0.22. Apply a -0.05 stability gradient.
    Verify stability is clamped to 0.20.

  TEST-U5: Idempotent writes.
    Run end-of-session pipeline twice with identical inputs and
    injected timestamp. Verify byte-identical outputs.

  TEST-U6: Kernel hash verification.
    Create a kernel with valid hashes. Modify one source file.
    Run boot. Verify the modified snippet is excluded.
    Verify remaining snippets are included.

  TEST-U7: Missing file defaults.
    Delete each input file one at a time. Run boot.
    Verify defaults from RULE 4 are applied.
    Verify session.state.json is still produced.

  TEST-U8: Archive pruning.
    Create 35 momentum archive files. Run end-of-session.
    Verify only 30 remain. Verify the 5 oldest were deleted.

  TEST-U9: Gradient retirement.
    Create a gradient with value 0.001 and decay_rate 0.30.
    Age it by 1 session. Verify effective_value < 0.001.
    Verify it is removed from the ledger.

  TEST-U10: Concurrent write lock.
    Attempt two simultaneous writes. Verify one succeeds and one
    fails with a lock timeout error.

------------------------------------------------------------------------

## 9. EXAMPLE: ONE FULL END-TO-END RUN

### 9.1 Setup: Input Artifacts

SESSION: session-2026-02-24-001
DURATION: 4.5 hours (270 minutes)

Artifact 1: interoceptive-state.json (end of session)

    {
      "timestamp": "2026-02-24T06:30:00.000Z",
      "telemetry": {
        "context_pressure": 0.12,
        "error_frequency": 0.25,
        "success_rate_rolling": 0.78,
        "novelty_exposure": 0.60,
        "memory_pressure": 0.08,
        "goal_gap": 0.15
      },
      "affective": {
        "stress": 0.22,
        "confidence": 0.72,
        "hunger": 0.10
      }
    }

Artifact 2: valence.json (end of session)

    {
      "score": 0.78,
      "components": {
        "trustHealth": 0.65,
        "stakesWinRate": 0.70,
        "errorRate": 0.10,
        "buildSuccess": 0.85,
        "interruptLoad": 0.05
      },
      "mood": "engaged",
      "timestamp": "2026-02-24T06:30:00.000Z"
    }

Artifact 3: Previous session momentum (from archive)

    {
      "session_id": "session-2026-02-23-001",
      "momentum_score": 0.68,
      "arc_type": "clean",
      "components": {
        "interoception_avg": {
          "stress": 0.30,
          "confidence": 0.55,
          "hunger": 0.05
        }
      },
      "momentum_history": [0.55, 0.62, 0.68]
    }

Artifact 4: collaboration-log.json session entry (latest)

    {
      "sessionId": "session-2026-02-24-001",
      "date": "2026-02-24",
      "chapterTitle": "GSV RuntimeParameters + Code Review",
      "summary": "Wired all 6 GSV RuntimeParameters. 1070 tests pass. Neuro review approved.",
      "signals": {"momentum": "high", "risk": []},
      "tags": ["neuro", "gsv", "code-review"]
    }

Artifact 5: New decision added during session

    ADR-005: GSV RuntimeParameters Wiring
    Date: 2026-02-24
    Status: Accepted
    Scope: Engineering

Session telemetry summary:
  - Tests run: 1070, passing: 1070 (green ratio = 1.0)
  - Issues fixed: 3 (GSV wiring bugs)
  - Issues opened: 0
  - Unresolved debt delta: -2 (closed 2 old TODOs)
  - Errors encountered: 4, errors resolved: 3 (recovery = 0.75)
  - Tasks created: 5, completed: 4 (completion = 0.80)
  - Valence history trend (regression slope): +0.12

### 9.2 Step-by-Step Computation

#### STEP 2: Compute Momentum

  tgr  = 1.0
  nir  = 1.0 - (0 / max(3, 1)) = 1.0 - 0.0 = 1.0
  udd  = sigmoid_norm(-2) = 1.0 / (1.0 + e^(0.5 * -2))
       = 1.0 / (1.0 + e^(-1.0))
       = 1.0 / (1.0 + 0.3679)
       = 1.0 / 1.3679
       = 0.7311
  ep   = min(1.0, 270 / 240) = min(1.0, 1.125) = 1.0
  si   = 1.0 - 0.22 = 0.78
  conf = 0.72
  vt   = (0.12 + 1.0) / 2.0 = 0.56
  err  = 0.75
  tcr  = 0.80

  momentum_score = 0.20 * 1.0      = 0.200
                 + 0.10 * 1.0      = 0.100
                 + 0.10 * 0.7311   = 0.073
                 + 0.10 * 1.0      = 0.100
                 + 0.10 * 0.78     = 0.078
                 + 0.15 * 0.72     = 0.108
                 + 0.05 * 0.56     = 0.028
                 + 0.10 * 0.75     = 0.075
                 + 0.10 * 0.80     = 0.080
                 -------------------------
                 TOTAL              = 0.842

  Arc classification:
    momentum_score = 0.842 >= 0.70: PASS
    valence_trend = 0.12 >= 0.0: PASS
    arc_type = "clean"

  momentum_history = [0.55, 0.62, 0.68, 0.842]

#### STEP 3: Update Gradient Ledger

Previous ledger had one old signal gradient:
  sg-old: value=0.05, decay_rate=0.30, sessions_age=1, dimension=exploration

After decay (sessions_age becomes 2):
  sg-old effective = 0.05 * (0.70)^2 = 0.05 * 0.49 = 0.0245

New signal gradients (comparing current vs previous interoception):
  stress:     0.22 - 0.30 = -0.08 -> urgency (inverted) -> delta = +0.08
  confidence: 0.72 - 0.55 = +0.17 -> stability          -> delta = +0.17
  hunger:     0.10 - 0.05 = +0.05 -> reward_drive        -> delta = +0.05

  All |delta| >= 0.01, so all three are created as signal gradients.

New decision gradient:
  ADR-005, scope=Engineering -> focus, magnitude=0.03

Momentum gradient:
  delta_m = 0.842 - 0.68 = +0.162 -> exploration (positive)
  Value: 0.162, decay_rate: 0.20

GSV deltas (sum all effective values per dimension):
  exploration:  sg-old(0.0245) + mg(0.162)     = 0.1865 -> CLAMPED to 0.15
  stability:    sg-confidence(0.17)              = 0.17  -> CLAMPED to 0.15
  focus:        dg-ADR005(0.03)                  = 0.03
  urgency:      sg-stress(0.08)                  = 0.08
  reward_drive: sg-hunger(0.05)                  = 0.05

Policy stability:
  old_gsv = {exploration: 0.64, stability: 0.603, focus: 0.507,
             urgency: 0.262, reward_drive: 0.506}
  deltas  = {exploration: 0.15, stability: 0.15, focus: 0.03,
             urgency: 0.08, reward_drive: 0.05}
  new_gsv = {exploration: 0.79, stability: 0.753, focus: 0.537,
             urgency: 0.342, reward_drive: 0.556}

  distance = sqrt(0.15^2 + 0.15^2 + 0.03^2 + 0.08^2 + 0.05^2)
           = sqrt(0.0225 + 0.0225 + 0.0009 + 0.0064 + 0.0025)
           = sqrt(0.0548)
           = 0.2341

  max_distance = 0.3354
  policy_stability = 1.0 - (0.2341 / 0.3354) = 1.0 - 0.6980 = 0.3020
  gradient_drift = 0.6980

#### STEP 4: Build Continuity Kernel

Memory files scanned: 25
File sizes (example subset):
  collaboration-log.json:  15000 bytes (changed=true,  relevance=0.92)
  decisions.md:             4500 bytes (changed=true,  relevance=0.88)
  opinions.md:              3200 bytes (changed=false, relevance=0.75)
  gsv-state.json:            351 bytes (changed=true,  relevance=0.95)
  interoceptive-state.json:  321 bytes (changed=true,  relevance=0.90)

File scoring (collaboration-log.json):
  recency  = 1.0 (modified in current session)
  relevance = 0.92
  momentum_boost = 0.10 (contributed to momentum via session entry)
  final_score = 0.50*0.92 + 0.35*1.0 + 0.15*0.10 = 0.46 + 0.35 + 0.015 = 0.825

File scoring (decisions.md):
  recency  = 1.0
  relevance = 0.88
  momentum_boost = 0.0
  final_score = 0.50*0.88 + 0.35*1.0 + 0.15*0.0 = 0.44 + 0.35 = 0.79

Reconstruction entropy (full, 25 files):
  S = 48000 (total bytes, approximate)
  H_raw = -SUM[(s_i/S)*log2(s_i/S)] for all 25 files
  H_raw = approximately 4.12 bits

  H_change = SUM[c_i * (s_i/S)] for changed files
  H_change = approximately 0.42

  H_relevance = 1.0 - (weighted_avg_relevance)
  H_relevance = approximately 0.28

  reconstruction_entropy = 4.12 * (1.42) * (1.28) = 7.49

Kernel selects top 8 files (final_score >= 0.30):
  Reduced entropy (over 8 files): approximately 3.42

### 9.3 Produced Outputs

session.momentum.json:

    {
      "version": "1.0.0",
      "session_id": "session-2026-02-24-001",
      "timestamp_start": "2026-02-24T02:00:00.000Z",
      "timestamp_end": "2026-02-24T06:30:00.000Z",
      "momentum_score": 0.842,
      "arc_type": "clean",
      "components": {
        "tests_green_ratio": 1.0,
        "issues_fixed_count": 3,
        "issues_opened_count": 0,
        "unresolved_debt_delta": -2,
        "effort_proxy": 1.0,
        "interoception_avg": {
          "stress": 0.22,
          "confidence": 0.72,
          "hunger": 0.10
        },
        "valence_trend": 0.12,
        "error_recovery_ratio": 0.75,
        "task_completion_ratio": 0.80
      },
      "weights": {
        "tests_green_ratio": 0.20,
        "net_issues_ratio": 0.10,
        "unresolved_debt_delta": 0.10,
        "effort_proxy": 0.10,
        "stress_inverse": 0.10,
        "confidence": 0.15,
        "valence_trend": 0.05,
        "error_recovery_ratio": 0.10,
        "task_completion_ratio": 0.10
      },
      "momentum_history": [0.55, 0.62, 0.68, 0.842]
    }

state.gradient.ledger.json (abbreviated):

    {
      "version": "1.0.0",
      "timestamp": "2026-02-24T06:35:00.000Z",
      "session_id": "session-2026-02-24-001",
      "signal_gradients": [
        {"id": "sg-old", "value": 0.05, "decay_rate": 0.30,
         "sessions_age": 2, "effective_value": 0.0245,
         "dimension": "exploration"},
        {"id": "sg-001", "value": 0.08, "decay_rate": 0.30,
         "sessions_age": 0, "effective_value": 0.08,
         "dimension": "urgency", "direction": "increase"},
        {"id": "sg-002", "value": 0.17, "decay_rate": 0.30,
         "sessions_age": 0, "effective_value": 0.17,
         "dimension": "stability", "direction": "increase"},
        {"id": "sg-003", "value": 0.05, "decay_rate": 0.30,
         "sessions_age": 0, "effective_value": 0.05,
         "dimension": "reward_drive", "direction": "increase"}
      ],
      "decision_gradients": [
        {"id": "dg-001", "value": 0.03, "decay_rate": 0.10,
         "sessions_age": 0, "effective_value": 0.03,
         "dimension": "focus", "decision_ref": "ADR-005",
         "rationale": "GSV wiring decision reinforces engineering focus"}
      ],
      "momentum_gradient": {
        "id": "mg-001", "value": 0.162, "decay_rate": 0.20,
        "sessions_age": 0, "effective_value": 0.162,
        "dimension": "exploration"
      },
      "gsv_deltas": {
        "exploration": 0.15,
        "stability": 0.15,
        "focus": 0.03,
        "urgency": 0.08,
        "reward_drive": 0.05
      },
      "policy_stability": 0.302,
      "gradient_drift": 0.698
    }

### 9.4 Next Session Boot: How Behavior Changes

Previous GSV vector:
  exploration: 0.64, stability: 0.603, focus: 0.507,
  urgency: 0.262, reward_drive: 0.506

Applied deltas:
  exploration: +0.15, stability: +0.15, focus: +0.03,
  urgency: +0.08, reward_drive: +0.05

New GSV vector:
  exploration: 0.79, stability: 0.753, focus: 0.537,
  urgency: 0.342, reward_drive: 0.556

Modulation policy mapping:

  exploration 0.79 -> [0.66, 1.0] -> exploration_depth: "broad"
  stability   0.753 -> [0.66, 1.0] -> variance_mode: "conservative"
  focus       0.537 -> [0.33, 0.66) -> verbosity: "standard"
  urgency     0.342 -> [0.33, 0.66) -> planning_horizon: "medium"
  reward_drive 0.556 -> [0.33, 0.66) -> goal_intensity: "balanced"

PREVIOUS modulation:
  exploration_depth: "broad"       (was 0.64 -> still "balanced" boundary)
  variance_mode: "conservative"    (was 0.603 -> was "moderate")
  verbosity: "detailed"            (was 0.507 -> was "standard")
  planning_horizon: "long"         (was 0.262 -> was "long")
  goal_intensity: "balanced"       (was 0.506 -> same)

NEW modulation:
  exploration_depth: "broad"       (UNCHANGED -- already broad)
  variance_mode: "conservative"    (CHANGED from "moderate" -- session built confidence)
  verbosity: "standard"            (CHANGED from "detailed" -- focus slightly up)
  planning_horizon: "medium"       (CHANGED from "long" -- urgency slightly up)
  goal_intensity: "balanced"       (UNCHANGED)

Interpretation:
  The productive session (clean arc, 0.842 momentum, all tests green)
  shifted the agent toward:
  - More conservative variance (higher stability = less experimental)
  - Medium-term planning (slightly more urgent, but safety-gated)
  - Standard verbosity (higher focus = less noise)

  These are reasonable behavioral adaptations: a successful session
  reinforces careful, focused execution patterns. The agent carries
  forward the productive trajectory without being told to.

### 9.5 Reconstruction Entropy Impact

  Full-file boot entropy: 7.49 bits
  Kernel-selected entropy: 3.42 bits
  Entropy reduction: 54.3%

  The next session reads 12 files instead of 25. The boot payload
  is 2048 bytes instead of approximately 48000. Context reconstruction
  takes 45ms instead of an estimated 200ms for full read.

------------------------------------------------------------------------

## APPENDIX A: CLI CONTRACT

### A.1 Commands

  smm-ck-sgl boot [--timestamp <ISO-8601>] [--dry-run]
    Execute the boot sequence (Section 5.2).
    Produces session.state.json.
    --timestamp: inject timestamp (for deterministic replay).
    --dry-run: compute and print SessionState without writing files.
    Exit code 0 on success, 1 on error.

  smm-ck-sgl end-session [--timestamp <ISO-8601>] [--session-id <id>]
    Execute the end-of-session pipeline (Section 5.1).
    Produces session.momentum.json, state.gradient.ledger.json,
    continuity.kernel.json.
    --timestamp: inject timestamp.
    --session-id: override auto-generated session ID.
    Exit code 0 on success, 1 on error.

  smm-ck-sgl inspect [--format json|text] [momentum|kernel|ledger|state|all]
    Print the current state of the specified module.
    Default: all. Default format: text.
    Does not modify any files.

  smm-ck-sgl replay --from <archive-timestamp> --to <archive-timestamp>
    Replay archived sessions deterministically.
    Reads from kernel/archive/ for the specified range.
    Re-computes all scores and compares to archived values.
    Reports any discrepancies (indicates non-determinism bugs).
    Does not modify any files.
    Exit code 0 if all replays match, 1 if any discrepancy found.

### A.2 Exit Codes

  0: Success
  1: Error (file I/O, parse, validation)
  2: Lock conflict (concurrent session)
  3: Stale kernel (> 7 days, boot fell back to naive)

### A.3 Environment Variables

  SMM_MEMORY_ROOT: Override default $MEMORY path.
    Default: ~/.claude/MEMORY

  SMM_ARCHIVE_RETENTION: Override archive retention count.
    Default: 30

  SMM_MAX_SNIPPETS: Override maximum kernel snippets.
    Default: 20

  SMM_MAX_KERNEL_BYTES: Override maximum kernel payload bytes.
    Default: 8192

  SMM_GRADIENT_CAP: Override maximum active gradients.
    Default: 50

  SMM_URGENCY_CEILING: Override urgency safety ceiling.
    Default: 0.80

  SMM_DELTA_CLAMP: Override per-dimension delta clamp.
    Default: 0.15

------------------------------------------------------------------------

## APPENDIX B: OPTIONAL SUPABASE VECTOR ADAPTER

For deployments with Supabase, an adapter can sync kernel snippets
to a vector store for semantic retrieval.

### B.1 Table Schema

    CREATE TABLE IF NOT EXISTS smm_kernel_snippets (
      id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
      session_id TEXT NOT NULL,
      source_file TEXT NOT NULL,
      source_type TEXT NOT NULL,
      content_hash TEXT NOT NULL,
      byte_offset_start INTEGER NOT NULL,
      byte_offset_end INTEGER NOT NULL,
      line_range_start INTEGER NOT NULL,
      line_range_end INTEGER NOT NULL,
      summary_tag TEXT NOT NULL,
      relevance_score FLOAT NOT NULL,
      final_score FLOAT NOT NULL,
      embedding VECTOR(3072),
      created_at TIMESTAMPTZ DEFAULT NOW()
    );

    CREATE INDEX idx_kernel_snippets_session ON smm_kernel_snippets(session_id);
    CREATE INDEX idx_kernel_snippets_source ON smm_kernel_snippets(source_file);

### B.2 Sync Contract

After writing continuity.kernel.json, the adapter:
  1. Reads all snippets.
  2. For each snippet, extracts the raw text from the source file
     at the recorded byte offsets.
  3. Generates an embedding using text-embedding-3-large (3072 dim).
  4. Upserts into smm_kernel_snippets (keyed on content_hash +
     session_id).

This is OPTIONAL. The core system operates entirely on local
filesystem. The adapter is a write-only sync; the local files
remain the source of truth.

------------------------------------------------------------------------

## APPENDIX C: GLOSSARY

  Arc Type        Classification of session trajectory quality.
  Boot Sequence   Steps that produce SessionState from persisted files.
  CK              Continuity Kernel. Snippet selection and compression.
  Decay Rate      Per-session exponential decay multiplier for gradients.
  Gradient        A directional bias extracted from session artifacts.
  GSV             Goal-State Vector. 5-dimensional behavior control.
  Kernel          The compressed set of artifact snippets for fast boot.
  Momentum        Scalar measure of session trajectory quality.
  Policy          The behavioral modulation settings derived from GSV.
  Reconstruction  The process of rebuilding context at session start.
  Entropy
  SessionState    The unified boot object consumed by the agent.
  SGL             State Gradient Ledger. Artifact-to-behavior bridge.
  SMM             Session Momentum Model. Trajectory quality capture.
  Snippet         A cited, hashed, byte-range-referenced artifact chunk.

------------------------------------------------------------------------

END OF SPECIFICATION
