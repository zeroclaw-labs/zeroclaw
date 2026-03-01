//! Agent-team orchestration primitives for token-aware collaboration.
//!
//! This module provides a repository-native implementation for:
//! - A2A-Lite handoff message validation/compaction
//! - Team-topology token/latency/quality estimation
//! - Budget-aware degradation policies
//! - Recommendation logic for choosing a topology under gates

use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};

const MIN_SUMMARY_CHARS: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Ord, PartialOrd)]
#[serde(rename_all = "snake_case")]
pub enum TeamTopology {
    Single,
    LeadSubagent,
    StarTeam,
    MeshTeam,
}

impl TeamTopology {
    #[must_use]
    pub const fn all() -> [Self; 4] {
        [
            Self::Single,
            Self::LeadSubagent,
            Self::StarTeam,
            Self::MeshTeam,
        ]
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Single => "single",
            Self::LeadSubagent => "lead_subagent",
            Self::StarTeam => "star_team",
            Self::MeshTeam => "mesh_team",
        }
    }

    fn participants(self, max_workers: usize) -> usize {
        match self {
            Self::Single => 1,
            Self::LeadSubagent => 2,
            Self::StarTeam | Self::MeshTeam => max_workers.min(5),
        }
    }

    fn execution_factor(self) -> f64 {
        match self {
            Self::Single => 1.00,
            Self::LeadSubagent => 0.95,
            Self::StarTeam => 0.92,
            Self::MeshTeam => 0.97,
        }
    }

    fn base_pass_rate(self) -> f64 {
        match self {
            Self::Single => 0.78,
            Self::LeadSubagent => 0.84,
            Self::StarTeam => 0.88,
            Self::MeshTeam => 0.82,
        }
    }

    fn cache_factor(self) -> f64 {
        match self {
            Self::Single => 0.05,
            Self::LeadSubagent => 0.08,
            Self::StarTeam => 0.10,
            Self::MeshTeam => 0.10,
        }
    }

    fn coordination_messages(self, rounds: u32, participants: usize, sync_multiplier: f64) -> u64 {
        if self == Self::Single {
            return 0;
        }

        let workers = participants.saturating_sub(1).max(1) as u64;
        let rounds = u64::from(rounds);
        let lead_messages = 2 * workers * rounds;

        let base_messages = match self {
            Self::Single => 0,
            Self::LeadSubagent => lead_messages,
            Self::StarTeam => {
                let broadcast = workers * rounds;
                lead_messages + broadcast
            }
            Self::MeshTeam => {
                let peer_messages = workers * workers.saturating_sub(1) * rounds;
                lead_messages + peer_messages
            }
        };

        ((base_messages as f64) * sync_multiplier).round() as u64
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BudgetTier {
    Low,
    Medium,
    High,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct TeamBudgetProfile {
    pub tier: BudgetTier,
    pub summary_cap_tokens: u32,
    pub max_workers: usize,
    pub compaction_interval_rounds: u32,
    pub message_budget_per_task: u32,
    pub quality_modifier: f64,
}

impl TeamBudgetProfile {
    #[must_use]
    pub const fn from_tier(tier: BudgetTier) -> Self {
        match tier {
            BudgetTier::Low => Self {
                tier,
                summary_cap_tokens: 80,
                max_workers: 3,
                compaction_interval_rounds: 3,
                message_budget_per_task: 10,
                quality_modifier: -0.03,
            },
            BudgetTier::Medium => Self {
                tier,
                summary_cap_tokens: 120,
                max_workers: 5,
                compaction_interval_rounds: 5,
                message_budget_per_task: 20,
                quality_modifier: 0.0,
            },
            BudgetTier::High => Self {
                tier,
                summary_cap_tokens: 180,
                max_workers: 8,
                compaction_interval_rounds: 8,
                message_budget_per_task: 32,
                quality_modifier: 0.02,
            },
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkloadProfile {
    Implementation,
    Debugging,
    Research,
    Mixed,
}

#[derive(Debug, Clone, Copy)]
struct WorkloadTuning {
    execution_multiplier: f64,
    sync_multiplier: f64,
    summary_multiplier: f64,
    latency_multiplier: f64,
    quality_modifier: f64,
}

impl WorkloadProfile {
    fn tuning(self) -> WorkloadTuning {
        match self {
            Self::Implementation => WorkloadTuning {
                execution_multiplier: 1.00,
                sync_multiplier: 1.00,
                summary_multiplier: 1.00,
                latency_multiplier: 1.00,
                quality_modifier: 0.00,
            },
            Self::Debugging => WorkloadTuning {
                execution_multiplier: 1.12,
                sync_multiplier: 1.25,
                summary_multiplier: 1.12,
                latency_multiplier: 1.18,
                quality_modifier: -0.02,
            },
            Self::Research => WorkloadTuning {
                execution_multiplier: 0.95,
                sync_multiplier: 0.90,
                summary_multiplier: 0.95,
                latency_multiplier: 0.92,
                quality_modifier: 0.01,
            },
            Self::Mixed => WorkloadTuning {
                execution_multiplier: 1.03,
                sync_multiplier: 1.08,
                summary_multiplier: 1.05,
                latency_multiplier: 1.06,
                quality_modifier: 0.00,
            },
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProtocolMode {
    A2aLite,
    Transcript,
}

#[derive(Debug, Clone, Copy)]
struct ProtocolTuning {
    summary_multiplier: f64,
    artifact_discount: f64,
    latency_penalty_per_message_s: f64,
    cache_bonus: f64,
    quality_modifier: f64,
}

impl ProtocolMode {
    fn tuning(self) -> ProtocolTuning {
        match self {
            Self::A2aLite => ProtocolTuning {
                summary_multiplier: 1.00,
                artifact_discount: 0.18,
                latency_penalty_per_message_s: 0.00,
                cache_bonus: 0.02,
                quality_modifier: 0.01,
            },
            Self::Transcript => ProtocolTuning {
                summary_multiplier: 2.20,
                artifact_discount: 0.00,
                latency_penalty_per_message_s: 0.012,
                cache_bonus: -0.01,
                quality_modifier: -0.02,
            },
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DegradationPolicy {
    None,
    Auto,
    Aggressive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecommendationMode {
    Balanced,
    Cost,
    Quality,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct GateThresholds {
    pub max_coordination_ratio: f64,
    pub min_pass_rate: f64,
    pub max_p95_latency_s: f64,
}

impl Default for GateThresholds {
    fn default() -> Self {
        Self {
            max_coordination_ratio: 0.20,
            min_pass_rate: 0.80,
            max_p95_latency_s: 180.0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OrchestrationEvalParams {
    pub tasks: u32,
    pub avg_task_tokens: u32,
    pub coordination_rounds: u32,
    pub workload: WorkloadProfile,
    pub protocol: ProtocolMode,
    pub degradation_policy: DegradationPolicy,
    pub recommendation_mode: RecommendationMode,
    pub gates: GateThresholds,
}

impl Default for OrchestrationEvalParams {
    fn default() -> Self {
        Self {
            tasks: 24,
            avg_task_tokens: 1400,
            coordination_rounds: 4,
            workload: WorkloadProfile::Mixed,
            protocol: ProtocolMode::A2aLite,
            degradation_policy: DegradationPolicy::None,
            recommendation_mode: RecommendationMode::Balanced,
            gates: GateThresholds::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelTier {
    Primary,
    Economy,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GateOutcome {
    pub coordination_ratio_ok: bool,
    pub quality_ok: bool,
    pub latency_ok: bool,
    pub budget_ok: bool,
}

impl GateOutcome {
    #[must_use]
    pub const fn pass(&self) -> bool {
        self.coordination_ratio_ok && self.quality_ok && self.latency_ok && self.budget_ok
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TopologyEvaluation {
    pub topology: TeamTopology,
    pub participants: usize,
    pub model_tier: ModelTier,
    pub tasks: u32,
    pub tasks_per_worker: f64,
    pub workload: WorkloadProfile,
    pub protocol: ProtocolMode,
    pub degradation_applied: bool,
    pub degradation_actions: Vec<String>,
    pub execution_tokens: u64,
    pub coordination_tokens: u64,
    pub cache_savings_tokens: u64,
    pub total_tokens: u64,
    pub coordination_ratio: f64,
    pub estimated_pass_rate: f64,
    pub estimated_defect_escape: f64,
    pub estimated_p95_latency_s: f64,
    pub estimated_throughput_tpd: f64,
    pub budget_limit_tokens: u64,
    pub budget_headroom_tokens: i64,
    pub budget_ok: bool,
    pub gates: GateOutcome,
}

impl TopologyEvaluation {
    #[must_use]
    pub const fn gate_pass(&self) -> bool {
        self.gates.pass()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecommendationScore {
    pub topology: TeamTopology,
    pub score: f64,
    pub gate_pass: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OrchestrationRecommendation {
    pub mode: RecommendationMode,
    pub recommended_topology: Option<TeamTopology>,
    pub reason: String,
    pub scores: Vec<RecommendationScore>,
    pub used_gate_filtered_pool: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OrchestrationReport {
    pub budget: TeamBudgetProfile,
    pub params: OrchestrationEvalParams,
    pub evaluations: Vec<TopologyEvaluation>,
    pub recommendation: OrchestrationRecommendation,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskNodeSpec {
    pub id: String,
    pub depends_on: Vec<String>,
    pub ownership_keys: Vec<String>,
    pub estimated_execution_tokens: u32,
    pub estimated_coordination_tokens: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlannedTaskBudget {
    pub task_id: String,
    pub execution_tokens: u64,
    pub coordination_tokens: u64,
    pub total_tokens: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionBatch {
    pub index: usize,
    pub task_ids: Vec<String>,
    pub ownership_locks: Vec<String>,
    pub estimated_total_tokens: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionPlan {
    pub topological_order: Vec<String>,
    pub budgets: Vec<PlannedTaskBudget>,
    pub batches: Vec<ExecutionBatch>,
    pub total_estimated_tokens: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlannerConfig {
    pub max_parallel: usize,
    pub run_budget_tokens: Option<u64>,
    pub min_coordination_tokens_per_task: u32,
}

impl Default for PlannerConfig {
    fn default() -> Self {
        Self {
            max_parallel: 4,
            run_budget_tokens: None,
            min_coordination_tokens_per_task: 8,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanError {
    EmptyTaskId,
    DuplicateTaskId(String),
    MissingDependency { task_id: String, dependency: String },
    SelfDependency(String),
    CycleDetected(Vec<String>),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanValidationError {
    MissingTaskInPlan(String),
    DuplicateTaskInPlan(String),
    UnknownTaskInPlan(String),
    BatchIndexMismatch {
        expected: usize,
        actual: usize,
    },
    DependencyOrderViolation {
        task_id: String,
        dependency: String,
    },
    OwnershipConflictInBatch {
        batch_index: usize,
        ownership_key: String,
    },
    BudgetMismatch(String),
    BatchTokenMismatch(usize),
    TotalTokenMismatch,
    InvalidHandoffMessage(String),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExecutionPlanDiagnostics {
    pub task_count: usize,
    pub batch_count: usize,
    pub critical_path_len: usize,
    pub max_parallelism: usize,
    pub mean_parallelism: f64,
    pub parallelism_efficiency: f64,
    pub dependency_edges: usize,
    pub ownership_lock_count: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OrchestrationBundle {
    pub report: OrchestrationReport,
    pub selected_topology: TeamTopology,
    pub selected_evaluation: TopologyEvaluation,
    pub planner_config: PlannerConfig,
    pub plan: ExecutionPlan,
    pub diagnostics: ExecutionPlanDiagnostics,
    pub handoff_messages: Vec<A2ALiteMessage>,
    pub estimated_handoff_tokens: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrchestrationError {
    Plan(PlanError),
    Validation(PlanValidationError),
    NoTopologyCandidate,
}

impl From<PlanError> for OrchestrationError {
    fn from(value: PlanError) -> Self {
        Self::Plan(value)
    }
}

impl From<PlanValidationError> for OrchestrationError {
    fn from(value: PlanValidationError) -> Self {
        Self::Validation(value)
    }
}

#[must_use]
pub fn derive_planner_config(
    selected: &TopologyEvaluation,
    tasks: &[TaskNodeSpec],
    budget: TeamBudgetProfile,
) -> PlannerConfig {
    let worker_width = match selected.topology {
        TeamTopology::Single => 1,
        _ => selected.participants.saturating_sub(1).max(1),
    };

    let max_parallel = worker_width.min(tasks.len().max(1));
    let execution_sum = tasks
        .iter()
        .map(|task| u64::from(task.estimated_execution_tokens))
        .sum::<u64>();
    let coordination_allowance = (tasks.len() as u64) * u64::from(budget.message_budget_per_task);
    let min_coordination_tokens_per_task = (budget.message_budget_per_task / 2).max(4);

    PlannerConfig {
        max_parallel,
        run_budget_tokens: Some(execution_sum.saturating_add(coordination_allowance)),
        min_coordination_tokens_per_task,
    }
}

#[must_use]
pub fn estimate_handoff_tokens(message: &A2ALiteMessage) -> u64 {
    fn text_tokens(text: &str) -> u64 {
        ((text.chars().count() as f64) / 4.0).ceil() as u64
    }

    let artifact_tokens = message
        .artifacts
        .iter()
        .map(|item| text_tokens(item))
        .sum::<u64>();
    let needs_tokens = message
        .needs
        .iter()
        .map(|item| text_tokens(item))
        .sum::<u64>();

    8 + text_tokens(&message.summary)
        + text_tokens(&message.next_action)
        + artifact_tokens
        + needs_tokens
}

#[must_use]
pub fn estimate_batch_handoff_tokens(messages: &[A2ALiteMessage]) -> u64 {
    messages.iter().map(estimate_handoff_tokens).sum()
}

pub fn orchestrate_task_graph(
    run_id: &str,
    budget: TeamBudgetProfile,
    params: &OrchestrationEvalParams,
    topologies: &[TeamTopology],
    tasks: &[TaskNodeSpec],
    handoff_policy: HandoffPolicy,
) -> Result<OrchestrationBundle, OrchestrationError> {
    let report = evaluate_team_topologies(budget, params, topologies);
    let Some(selected_topology) = report
        .recommendation
        .recommended_topology
        .or_else(|| report.evaluations.first().map(|row| row.topology))
    else {
        return Err(OrchestrationError::NoTopologyCandidate);
    };

    let Some(selected_evaluation) = report
        .evaluations
        .iter()
        .find(|row| row.topology == selected_topology)
        .cloned()
    else {
        return Err(OrchestrationError::NoTopologyCandidate);
    };

    let planner_config = derive_planner_config(&selected_evaluation, tasks, budget);
    let plan = build_conflict_aware_execution_plan(tasks, planner_config)?;
    validate_execution_plan(&plan, tasks)?;
    let diagnostics = analyze_execution_plan(&plan, tasks)?;
    let handoff_messages = build_batch_handoff_messages(run_id, &plan, tasks, handoff_policy)?;
    let estimated_handoff_tokens = estimate_batch_handoff_tokens(&handoff_messages);

    Ok(OrchestrationBundle {
        report,
        selected_topology,
        selected_evaluation,
        planner_config,
        plan,
        diagnostics,
        handoff_messages,
        estimated_handoff_tokens,
    })
}

pub fn validate_execution_plan(
    plan: &ExecutionPlan,
    tasks: &[TaskNodeSpec],
) -> Result<(), PlanValidationError> {
    let task_map = tasks
        .iter()
        .map(|t| (t.id.clone(), t))
        .collect::<HashMap<_, _>>();
    let budget_map = plan
        .budgets
        .iter()
        .map(|b| (b.task_id.clone(), b))
        .collect::<HashMap<_, _>>();

    let mut topo_seen = HashSet::<String>::new();
    let mut topo_idx = HashMap::<String, usize>::new();
    for (idx, task_id) in plan.topological_order.iter().enumerate() {
        if !task_map.contains_key(task_id) {
            return Err(PlanValidationError::UnknownTaskInPlan(task_id.clone()));
        }
        if !topo_seen.insert(task_id.clone()) {
            return Err(PlanValidationError::DuplicateTaskInPlan(task_id.clone()));
        }
        topo_idx.insert(task_id.clone(), idx);
    }

    for task in tasks {
        if !topo_seen.contains(&task.id) {
            return Err(PlanValidationError::MissingTaskInPlan(task.id.clone()));
        }
    }

    for task in tasks {
        let Some(task_pos) = topo_idx.get(&task.id) else {
            return Err(PlanValidationError::MissingTaskInPlan(task.id.clone()));
        };
        for dep in &task.depends_on {
            let Some(dep_pos) = topo_idx.get(dep) else {
                return Err(PlanValidationError::MissingTaskInPlan(dep.clone()));
            };
            if dep_pos >= task_pos {
                return Err(PlanValidationError::DependencyOrderViolation {
                    task_id: task.id.clone(),
                    dependency: dep.clone(),
                });
            }
        }
    }

    let mut seen = HashSet::<String>::new();
    let mut task_to_batch = HashMap::<String, usize>::new();
    let mut batch_token_sum = 0_u64;

    for budget in &plan.budgets {
        if !task_map.contains_key(&budget.task_id) {
            return Err(PlanValidationError::UnknownTaskInPlan(
                budget.task_id.clone(),
            ));
        }
        if budget.total_tokens
            != budget
                .execution_tokens
                .saturating_add(budget.coordination_tokens)
        {
            return Err(PlanValidationError::BudgetMismatch(budget.task_id.clone()));
        }
    }

    for (batch_idx, batch) in plan.batches.iter().enumerate() {
        if batch.index != batch_idx {
            return Err(PlanValidationError::BatchIndexMismatch {
                expected: batch_idx,
                actual: batch.index,
            });
        }

        let mut lock_set = HashSet::<String>::new();
        let mut expected_batch_tokens = 0_u64;

        for task_id in &batch.task_ids {
            if !task_map.contains_key(task_id) {
                return Err(PlanValidationError::UnknownTaskInPlan(task_id.clone()));
            }
            if !seen.insert(task_id.clone()) {
                return Err(PlanValidationError::DuplicateTaskInPlan(task_id.clone()));
            }
            task_to_batch.insert(task_id.clone(), batch_idx);

            if let Some(b) = budget_map.get(task_id) {
                expected_batch_tokens = expected_batch_tokens.saturating_add(b.total_tokens);
            } else {
                return Err(PlanValidationError::BudgetMismatch(task_id.clone()));
            }

            let Some(task) = task_map.get(task_id) else {
                return Err(PlanValidationError::UnknownTaskInPlan(task_id.clone()));
            };

            for key in &task.ownership_keys {
                if !lock_set.insert(key.clone()) {
                    return Err(PlanValidationError::OwnershipConflictInBatch {
                        batch_index: batch_idx,
                        ownership_key: key.clone(),
                    });
                }
            }
        }

        if batch.estimated_total_tokens != expected_batch_tokens {
            return Err(PlanValidationError::BatchTokenMismatch(batch_idx));
        }
        batch_token_sum = batch_token_sum.saturating_add(batch.estimated_total_tokens);
    }

    for task in tasks {
        if !seen.contains(&task.id) {
            return Err(PlanValidationError::MissingTaskInPlan(task.id.clone()));
        }
    }

    for task in tasks {
        let Some(task_batch) = task_to_batch.get(&task.id) else {
            return Err(PlanValidationError::MissingTaskInPlan(task.id.clone()));
        };
        for dep in &task.depends_on {
            let Some(dep_batch) = task_to_batch.get(dep) else {
                return Err(PlanValidationError::MissingTaskInPlan(dep.clone()));
            };
            if dep_batch >= task_batch {
                return Err(PlanValidationError::DependencyOrderViolation {
                    task_id: task.id.clone(),
                    dependency: dep.clone(),
                });
            }
        }
    }

    if plan.total_estimated_tokens != batch_token_sum {
        return Err(PlanValidationError::TotalTokenMismatch);
    }

    Ok(())
}

pub fn analyze_execution_plan(
    plan: &ExecutionPlan,
    tasks: &[TaskNodeSpec],
) -> Result<ExecutionPlanDiagnostics, PlanValidationError> {
    validate_execution_plan(plan, tasks)?;

    let task_map = tasks
        .iter()
        .map(|t| (t.id.clone(), t))
        .collect::<HashMap<_, _>>();

    let mut longest = HashMap::<String, usize>::new();
    for task_id in &plan.topological_order {
        let Some(task) = task_map.get(task_id) else {
            return Err(PlanValidationError::UnknownTaskInPlan(task_id.clone()));
        };

        let depth = task
            .depends_on
            .iter()
            .filter_map(|dep| longest.get(dep).copied())
            .max()
            .unwrap_or(0)
            + 1;

        longest.insert(task_id.clone(), depth);
    }

    let task_count = tasks.len();
    let batch_count = plan.batches.len();
    let max_parallelism = plan
        .batches
        .iter()
        .map(|b| b.task_ids.len())
        .max()
        .unwrap_or(0);
    let mean_parallelism = if batch_count == 0 {
        0.0
    } else {
        task_count as f64 / batch_count as f64
    };
    let parallelism_efficiency = if batch_count == 0 || max_parallelism == 0 {
        0.0
    } else {
        mean_parallelism / max_parallelism as f64
    };
    let dependency_edges = tasks.iter().map(|t| t.depends_on.len()).sum::<usize>();
    let ownership_lock_count = plan
        .batches
        .iter()
        .map(|b| b.ownership_locks.len())
        .sum::<usize>();
    let critical_path_len = longest.values().copied().max().unwrap_or(0);

    Ok(ExecutionPlanDiagnostics {
        task_count,
        batch_count,
        critical_path_len,
        max_parallelism,
        mean_parallelism: round4(mean_parallelism),
        parallelism_efficiency: round4(parallelism_efficiency),
        dependency_edges,
        ownership_lock_count,
    })
}

pub fn build_conflict_aware_execution_plan(
    tasks: &[TaskNodeSpec],
    config: PlannerConfig,
) -> Result<ExecutionPlan, PlanError> {
    validate_tasks(tasks)?;

    let order = topological_sort(tasks)?;
    let budgets = allocate_task_budgets(
        tasks,
        config.run_budget_tokens,
        config.min_coordination_tokens_per_task,
    );

    let budgets_by_id = budgets
        .iter()
        .map(|x| (x.task_id.clone(), x.clone()))
        .collect::<HashMap<_, _>>();
    let task_map = tasks
        .iter()
        .map(|t| (t.id.clone(), t))
        .collect::<HashMap<_, _>>();

    let mut completed = HashSet::<String>::new();
    let mut pending = order.iter().cloned().collect::<HashSet<_>>();
    let mut batches = Vec::<ExecutionBatch>::new();

    let max_parallel = config.max_parallel.max(1);

    while !pending.is_empty() {
        let candidates = order
            .iter()
            .filter(|id| pending.contains(*id))
            .filter_map(|id| {
                let task = task_map.get(id)?;
                let deps_satisfied = task.depends_on.iter().all(|dep| completed.contains(dep));
                if deps_satisfied {
                    Some((*id).clone())
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();

        if candidates.is_empty() {
            let mut unresolved = pending.iter().cloned().collect::<Vec<_>>();
            unresolved.sort();
            return Err(PlanError::CycleDetected(unresolved));
        }

        let mut locks = HashSet::<String>::new();
        let mut batch_ids = Vec::<String>::new();

        for candidate in &candidates {
            if batch_ids.len() >= max_parallel {
                break;
            }

            let Some(task) = task_map.get(candidate) else {
                continue;
            };

            if has_ownership_conflict(&task.ownership_keys, &locks) {
                continue;
            }

            batch_ids.push(candidate.clone());
            task.ownership_keys.iter().for_each(|key| {
                locks.insert(key.clone());
            });
        }

        if batch_ids.is_empty() {
            // Conflict pressure: guarantee forward progress with single-candidate fallback.
            batch_ids.push(candidates[0].clone());
            if let Some(task) = task_map.get(&batch_ids[0]) {
                task.ownership_keys.iter().for_each(|key| {
                    locks.insert(key.clone());
                });
            }
        }

        let mut lock_list = locks.into_iter().collect::<Vec<_>>();
        lock_list.sort();

        let mut token_sum = 0_u64;
        for task_id in &batch_ids {
            if let Some(b) = budgets_by_id.get(task_id) {
                token_sum = token_sum.saturating_add(b.total_tokens);
            }
            pending.remove(task_id);
            completed.insert(task_id.clone());
        }

        batches.push(ExecutionBatch {
            index: batches.len(),
            task_ids: batch_ids,
            ownership_locks: lock_list,
            estimated_total_tokens: token_sum,
        });
    }

    let total_estimated_tokens = budgets.iter().map(|x| x.total_tokens).sum::<u64>();

    Ok(ExecutionPlan {
        topological_order: order,
        budgets,
        batches,
        total_estimated_tokens,
    })
}

#[must_use]
pub fn allocate_task_budgets(
    tasks: &[TaskNodeSpec],
    run_budget_tokens: Option<u64>,
    min_coordination_tokens_per_task: u32,
) -> Vec<PlannedTaskBudget> {
    let mut budgets = tasks
        .iter()
        .map(|task| {
            let execution = u64::from(task.estimated_execution_tokens);
            let coordination = u64::from(
                task.estimated_coordination_tokens
                    .max(min_coordination_tokens_per_task),
            );
            PlannedTaskBudget {
                task_id: task.id.clone(),
                execution_tokens: execution,
                coordination_tokens: coordination,
                total_tokens: execution.saturating_add(coordination),
            }
        })
        .collect::<Vec<_>>();

    let Some(limit) = run_budget_tokens else {
        return budgets;
    };

    let execution_sum = budgets.iter().map(|x| x.execution_tokens).sum::<u64>();
    if execution_sum >= limit {
        // No room for coordination tokens while preserving execution estimates.
        budgets.iter_mut().for_each(|item| {
            item.coordination_tokens = 0;
            item.total_tokens = item.execution_tokens;
        });
        return budgets;
    }

    let requested_coord_sum = budgets.iter().map(|x| x.coordination_tokens).sum::<u64>();
    let allowed_coord_sum = limit.saturating_sub(execution_sum);

    if requested_coord_sum <= allowed_coord_sum {
        return budgets;
    }

    if budgets.is_empty() {
        return budgets;
    }

    let floor = u64::from(min_coordination_tokens_per_task);
    let floors_sum = floor.saturating_mul(budgets.len() as u64);

    if allowed_coord_sum <= floors_sum {
        let base = allowed_coord_sum / budgets.len() as u64;
        let mut remainder = allowed_coord_sum % budgets.len() as u64;
        for item in &mut budgets {
            let bump = u64::from(remainder > 0);
            remainder = remainder.saturating_sub(1);
            item.coordination_tokens = base.saturating_add(bump);
            item.total_tokens = item
                .execution_tokens
                .saturating_add(item.coordination_tokens);
        }
        return budgets;
    }

    let extra_target = allowed_coord_sum.saturating_sub(floors_sum);

    let mut extra_requests = budgets
        .iter()
        .map(|x| x.coordination_tokens.saturating_sub(floor))
        .collect::<Vec<_>>();
    let extra_request_sum = extra_requests.iter().sum::<u64>();

    if extra_request_sum == 0 {
        budgets.iter_mut().for_each(|item| {
            item.coordination_tokens = floor;
            item.total_tokens = item
                .execution_tokens
                .saturating_add(item.coordination_tokens);
        });
        return budgets;
    }

    let mut allocated_extra = vec![0_u64; budgets.len()];
    let mut remaining_extra = extra_target;

    for (idx, req) in extra_requests.iter_mut().enumerate() {
        if *req == 0 {
            continue;
        }
        let share = extra_target.saturating_mul(*req) / extra_request_sum;
        let bounded = share.min(*req).min(remaining_extra);
        allocated_extra[idx] = bounded;
        remaining_extra = remaining_extra.saturating_sub(bounded);
    }

    let mut i = 0;
    while remaining_extra > 0 && i < budgets.len() * 2 {
        let idx = i % budgets.len();
        let req = extra_requests[idx];
        if allocated_extra[idx] < req {
            allocated_extra[idx] = allocated_extra[idx].saturating_add(1);
            remaining_extra = remaining_extra.saturating_sub(1);
        }
        i += 1;
    }

    budgets.iter_mut().enumerate().for_each(|(idx, item)| {
        item.coordination_tokens = floor.saturating_add(allocated_extra[idx]);
        item.total_tokens = item
            .execution_tokens
            .saturating_add(item.coordination_tokens);
    });

    budgets
}

fn validate_tasks(tasks: &[TaskNodeSpec]) -> Result<(), PlanError> {
    let mut ids = HashSet::<String>::new();
    let all = tasks.iter().map(|x| x.id.clone()).collect::<HashSet<_>>();

    for task in tasks {
        if task.id.trim().is_empty() {
            return Err(PlanError::EmptyTaskId);
        }
        if !ids.insert(task.id.clone()) {
            return Err(PlanError::DuplicateTaskId(task.id.clone()));
        }

        for dep in &task.depends_on {
            if dep == &task.id {
                return Err(PlanError::SelfDependency(task.id.clone()));
            }
            if !all.contains(dep) {
                return Err(PlanError::MissingDependency {
                    task_id: task.id.clone(),
                    dependency: dep.clone(),
                });
            }
        }
    }
    Ok(())
}

fn topological_sort(tasks: &[TaskNodeSpec]) -> Result<Vec<String>, PlanError> {
    let mut indegree = tasks
        .iter()
        .map(|task| (task.id.clone(), 0_usize))
        .collect::<HashMap<_, _>>();
    let mut outgoing = HashMap::<String, Vec<String>>::new();

    for task in tasks {
        for dep in &task.depends_on {
            *indegree.entry(task.id.clone()).or_insert(0) += 1;
            outgoing
                .entry(dep.clone())
                .or_default()
                .push(task.id.clone());
        }
    }

    let mut zero = indegree
        .iter()
        .filter_map(|(id, deg)| (*deg == 0).then_some(id.clone()))
        .collect::<BTreeSet<_>>();
    let mut queue = VecDeque::<String>::new();
    for id in zero.iter() {
        queue.push_back(id.clone());
    }

    let mut order = Vec::<String>::new();
    while let Some(node) = queue.pop_front() {
        zero.remove(&node);
        order.push(node.clone());

        if let Some(next) = outgoing.get(&node) {
            for succ in next {
                if let Some(entry) = indegree.get_mut(succ) {
                    *entry = entry.saturating_sub(1);
                    if *entry == 0 && zero.insert(succ.clone()) {
                        queue.push_back(succ.clone());
                    }
                }
            }
        }
    }

    if order.len() != tasks.len() {
        let mut unresolved = indegree
            .into_iter()
            .filter_map(|(id, deg)| (deg > 0).then_some(id))
            .collect::<Vec<_>>();
        unresolved.sort();
        return Err(PlanError::CycleDetected(unresolved));
    }

    Ok(order)
}

fn has_ownership_conflict(ownership_keys: &[String], locks: &HashSet<String>) -> bool {
    ownership_keys.iter().any(|k| locks.contains(k))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum A2AStatus {
    Queued,
    Running,
    Blocked,
    Done,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RiskLevel {
    Low,
    Medium,
    High,
    Critical,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct A2ALiteMessage {
    pub run_id: String,
    pub task_id: String,
    pub sender: String,
    pub recipient: String,
    pub status: A2AStatus,
    pub confidence: u8,
    pub risk_level: RiskLevel,
    pub summary: String,
    pub artifacts: Vec<String>,
    pub needs: Vec<String>,
    pub next_action: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct HandoffPolicy {
    pub max_summary_chars: usize,
    pub max_artifacts: usize,
    pub max_needs: usize,
}

impl Default for HandoffPolicy {
    fn default() -> Self {
        Self {
            max_summary_chars: 320,
            max_artifacts: 8,
            max_needs: 6,
        }
    }
}

impl A2ALiteMessage {
    pub fn validate(&self, policy: HandoffPolicy) -> Result<(), String> {
        if self.run_id.trim().is_empty() {
            return Err("run_id must not be empty".to_string());
        }
        if self.task_id.trim().is_empty() {
            return Err("task_id must not be empty".to_string());
        }
        if self.sender.trim().is_empty() {
            return Err("sender must not be empty".to_string());
        }
        if self.recipient.trim().is_empty() {
            return Err("recipient must not be empty".to_string());
        }
        if self.next_action.trim().is_empty() {
            return Err("next_action must not be empty".to_string());
        }

        let summary_len = self.summary.chars().count();
        if summary_len < MIN_SUMMARY_CHARS {
            return Err("summary is too short for reliable handoff".to_string());
        }
        if summary_len > policy.max_summary_chars {
            return Err("summary exceeds max_summary_chars".to_string());
        }

        if self.confidence > 100 {
            return Err("confidence must be in [0,100]".to_string());
        }

        if self.artifacts.len() > policy.max_artifacts {
            return Err("too many artifacts".to_string());
        }
        if self.needs.len() > policy.max_needs {
            return Err("too many dependency needs".to_string());
        }

        if self.artifacts.iter().any(|x| x.trim().is_empty()) {
            return Err("artifact pointers must not be empty".to_string());
        }
        if self.needs.iter().any(|x| x.trim().is_empty()) {
            return Err("needs entries must not be empty".to_string());
        }

        Ok(())
    }

    #[must_use]
    pub fn compact_for_handoff(&self, policy: HandoffPolicy) -> Self {
        let mut compacted = self.clone();
        compacted.summary = truncate_chars(&self.summary, policy.max_summary_chars);
        compacted.artifacts.truncate(policy.max_artifacts);
        compacted.needs.truncate(policy.max_needs);
        compacted
    }
}

pub fn build_batch_handoff_messages(
    run_id: &str,
    plan: &ExecutionPlan,
    tasks: &[TaskNodeSpec],
    policy: HandoffPolicy,
) -> Result<Vec<A2ALiteMessage>, PlanValidationError> {
    validate_execution_plan(plan, tasks)?;

    let mut messages = Vec::<A2ALiteMessage>::new();
    for batch in &plan.batches {
        let summary = format!(
            "Execute batch {} with tasks [{}]; ownership locks [{}]; estimated_tokens={}.",
            batch.index,
            batch.task_ids.join(","),
            batch.ownership_locks.join(","),
            batch.estimated_total_tokens
        );

        let risk_level = if batch.task_ids.len() > 3 || batch.estimated_total_tokens > 12_000 {
            RiskLevel::High
        } else if batch.task_ids.len() > 1 || batch.estimated_total_tokens > 4_000 {
            RiskLevel::Medium
        } else {
            RiskLevel::Low
        };

        let needs = if batch.index == 0 {
            Vec::new()
        } else {
            vec![format!("batch-{}", batch.index - 1)]
        };

        let msg = A2ALiteMessage {
            run_id: run_id.to_string(),
            task_id: format!("batch-{}", batch.index),
            sender: "planner".to_string(),
            recipient: "worker_pool".to_string(),
            status: A2AStatus::Queued,
            confidence: 90,
            risk_level,
            summary,
            artifacts: batch
                .task_ids
                .iter()
                .map(|task_id| format!("task://{task_id}"))
                .collect(),
            needs,
            next_action: "dispatch_batch".to_string(),
        }
        .compact_for_handoff(policy);

        msg.validate(policy)
            .map_err(|_| PlanValidationError::InvalidHandoffMessage(msg.task_id.clone()))?;
        messages.push(msg);
    }

    Ok(messages)
}

#[must_use]
pub fn evaluate_team_topologies(
    budget: TeamBudgetProfile,
    params: &OrchestrationEvalParams,
    topologies: &[TeamTopology],
) -> OrchestrationReport {
    let evaluations: Vec<_> = topologies
        .iter()
        .copied()
        .map(|topology| evaluate_topology(budget, params, topology))
        .collect();

    let recommendation = recommend_topology(&evaluations, params.recommendation_mode);

    OrchestrationReport {
        budget,
        params: params.clone(),
        evaluations,
        recommendation,
    }
}

#[must_use]
pub fn evaluate_all_budget_tiers(
    params: &OrchestrationEvalParams,
    topologies: &[TeamTopology],
) -> Vec<OrchestrationReport> {
    [BudgetTier::Low, BudgetTier::Medium, BudgetTier::High]
        .into_iter()
        .map(TeamBudgetProfile::from_tier)
        .map(|budget| evaluate_team_topologies(budget, params, topologies))
        .collect()
}

fn evaluate_topology(
    budget: TeamBudgetProfile,
    params: &OrchestrationEvalParams,
    topology: TeamTopology,
) -> TopologyEvaluation {
    let base = compute_metrics(
        budget,
        params,
        topology,
        topology.participants(budget.max_workers),
        1.0,
        0.0,
        ModelTier::Primary,
        false,
        Vec::new(),
    );

    if params.degradation_policy == DegradationPolicy::None || topology == TeamTopology::Single {
        return base;
    }

    let pressure = !base.budget_ok || base.coordination_ratio > params.gates.max_coordination_ratio;
    if !pressure {
        return base;
    }

    let (participant_delta, summary_scale, quality_penalty) = match params.degradation_policy {
        DegradationPolicy::None => (0, 1.0, 0.0),
        DegradationPolicy::Auto => (1, 0.82, -0.01),
        DegradationPolicy::Aggressive => (2, 0.65, -0.03),
    };

    let reduced_participants = base.participants.saturating_sub(participant_delta).max(2);
    let actions = vec![
        format!(
            "reduce_participants:{}->{}",
            base.participants, reduced_participants
        ),
        format!("tighten_summary_scale:{summary_scale}"),
        "switch_model_tier:economy".to_string(),
    ];

    compute_metrics(
        budget,
        params,
        topology,
        reduced_participants,
        summary_scale,
        quality_penalty,
        ModelTier::Economy,
        true,
        actions,
    )
}

#[allow(clippy::too_many_arguments)]
fn compute_metrics(
    budget: TeamBudgetProfile,
    params: &OrchestrationEvalParams,
    topology: TeamTopology,
    participants: usize,
    summary_scale: f64,
    extra_quality_modifier: f64,
    model_tier: ModelTier,
    degradation_applied: bool,
    degradation_actions: Vec<String>,
) -> TopologyEvaluation {
    let workload = params.workload.tuning();
    let protocol = params.protocol.tuning();

    let parallelism = if topology == TeamTopology::Single {
        1.0
    } else {
        participants.saturating_sub(1).max(1) as f64
    };

    let execution_tokens = ((params.tasks as f64)
        * (params.avg_task_tokens as f64)
        * topology.execution_factor()
        * workload.execution_multiplier)
        .round() as u64;

    let base_summary_tokens = ((params.avg_task_tokens as f64) * 0.08).round() as u64;
    let mut summary_tokens = base_summary_tokens
        .max(24)
        .min(u64::from(budget.summary_cap_tokens));
    summary_tokens = ((summary_tokens as f64)
        * workload.summary_multiplier
        * protocol.summary_multiplier
        * summary_scale)
        .round()
        .max(16.0) as u64;

    let messages = topology.coordination_messages(
        params.coordination_rounds,
        participants,
        workload.sync_multiplier,
    );

    let raw_coordination_tokens = messages * summary_tokens;

    let compaction_events =
        (params.coordination_rounds / budget.compaction_interval_rounds.max(1)) as f64;
    let compaction_discount = (compaction_events * 0.10).min(0.35);

    let mut coordination_tokens =
        ((raw_coordination_tokens as f64) * (1.0 - compaction_discount)).round() as u64;

    coordination_tokens =
        ((coordination_tokens as f64) * (1.0 - protocol.artifact_discount)).round() as u64;

    let cache_factor = (topology.cache_factor() + protocol.cache_bonus).clamp(0.0, 0.30);
    let cache_savings_tokens = ((execution_tokens as f64) * cache_factor).round() as u64;

    let total_tokens = execution_tokens
        .saturating_add(coordination_tokens)
        .saturating_sub(cache_savings_tokens)
        .max(1);

    let coordination_ratio = coordination_tokens as f64 / total_tokens as f64;

    let pass_rate = (topology.base_pass_rate()
        + budget.quality_modifier
        + workload.quality_modifier
        + protocol.quality_modifier
        + extra_quality_modifier)
        .clamp(0.0, 0.99);

    let defect_escape = (1.0 - pass_rate).clamp(0.0, 1.0);

    let base_latency_s = (params.tasks as f64 / parallelism) * 6.0 * workload.latency_multiplier;
    let sync_penalty_s = messages as f64 * (0.02 + protocol.latency_penalty_per_message_s);
    let p95_latency_s = base_latency_s + sync_penalty_s;

    let throughput_tpd = (params.tasks as f64 / p95_latency_s.max(1.0)) * 86_400.0;

    let budget_limit_tokens = u64::from(params.tasks)
        .saturating_mul(u64::from(params.avg_task_tokens))
        .saturating_add(
            u64::from(params.tasks).saturating_mul(u64::from(budget.message_budget_per_task)),
        );

    let budget_ok = total_tokens <= budget_limit_tokens;

    let gates = GateOutcome {
        coordination_ratio_ok: coordination_ratio <= params.gates.max_coordination_ratio,
        quality_ok: pass_rate >= params.gates.min_pass_rate,
        latency_ok: p95_latency_s <= params.gates.max_p95_latency_s,
        budget_ok,
    };

    let budget_headroom_tokens = budget_limit_tokens as i64 - total_tokens as i64;

    TopologyEvaluation {
        topology,
        participants,
        model_tier,
        tasks: params.tasks,
        tasks_per_worker: round4(params.tasks as f64 / parallelism),
        workload: params.workload,
        protocol: params.protocol,
        degradation_applied,
        degradation_actions,
        execution_tokens,
        coordination_tokens,
        cache_savings_tokens,
        total_tokens,
        coordination_ratio: round4(coordination_ratio),
        estimated_pass_rate: round4(pass_rate),
        estimated_defect_escape: round4(defect_escape),
        estimated_p95_latency_s: round2(p95_latency_s),
        estimated_throughput_tpd: round2(throughput_tpd),
        budget_limit_tokens,
        budget_headroom_tokens,
        budget_ok,
        gates,
    }
}

fn recommend_topology(
    evaluations: &[TopologyEvaluation],
    mode: RecommendationMode,
) -> OrchestrationRecommendation {
    if evaluations.is_empty() {
        return OrchestrationRecommendation {
            mode,
            recommended_topology: None,
            reason: "no_results".to_string(),
            scores: Vec::new(),
            used_gate_filtered_pool: false,
        };
    }

    let gate_passed: Vec<&TopologyEvaluation> =
        evaluations.iter().filter(|x| x.gate_pass()).collect();
    let pool = if gate_passed.is_empty() {
        evaluations.iter().collect::<Vec<_>>()
    } else {
        gate_passed
    };
    let used_gate_filtered_pool = evaluations.iter().any(TopologyEvaluation::gate_pass);

    let max_tokens = pool.iter().map(|x| x.total_tokens).max().unwrap_or(1) as f64;
    let max_latency = pool
        .iter()
        .map(|x| x.estimated_p95_latency_s)
        .fold(0.0_f64, f64::max)
        .max(1.0);

    let (w_quality, w_cost, w_latency) = match mode {
        RecommendationMode::Balanced => (0.45, 0.35, 0.20),
        RecommendationMode::Cost => (0.25, 0.55, 0.20),
        RecommendationMode::Quality => (0.65, 0.20, 0.15),
    };

    let mut scores = pool
        .iter()
        .map(|row| {
            let quality = row.estimated_pass_rate;
            let cost_norm = 1.0 - (row.total_tokens as f64 / max_tokens);
            let latency_norm = 1.0 - (row.estimated_p95_latency_s / max_latency);
            let score = (quality * w_quality) + (cost_norm * w_cost) + (latency_norm * w_latency);

            RecommendationScore {
                topology: row.topology,
                score: round5(score),
                gate_pass: row.gate_pass(),
            }
        })
        .collect::<Vec<_>>();

    scores.sort_by(|a, b| b.score.total_cmp(&a.score));

    OrchestrationRecommendation {
        mode,
        recommended_topology: scores.first().map(|x| x.topology),
        reason: "weighted_score".to_string(),
        scores,
        used_gate_filtered_pool,
    }
}

fn truncate_chars(input: &str, max_chars: usize) -> String {
    let char_count = input.chars().count();
    if char_count <= max_chars {
        return input.to_string();
    }

    if max_chars <= 3 {
        return "...".chars().take(max_chars).collect();
    }

    let mut out = input.chars().take(max_chars - 3).collect::<String>();
    out.push_str("...");
    out
}

fn round2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

fn round4(v: f64) -> f64 {
    (v * 10_000.0).round() / 10_000.0
}

fn round5(v: f64) -> f64 {
    (v * 100_000.0).round() / 100_000.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn by_topology(rows: &[TopologyEvaluation]) -> BTreeMap<TeamTopology, TopologyEvaluation> {
        rows.iter()
            .cloned()
            .map(|x| (x.topology, x))
            .collect::<BTreeMap<_, _>>()
    }

    #[test]
    fn a2a_message_validate_and_compact() {
        let msg = A2ALiteMessage {
            run_id: "run-1".to_string(),
            task_id: "task-22".to_string(),
            sender: "worker-a".to_string(),
            recipient: "lead".to_string(),
            status: A2AStatus::Done,
            confidence: 91,
            risk_level: RiskLevel::Medium,
            summary: "This is a handoff summary with enough content to validate correctly."
                .to_string(),
            artifacts: vec![
                "artifact://a".to_string(),
                "artifact://b".to_string(),
                "artifact://c".to_string(),
            ],
            needs: vec!["review".to_string(), "approve".to_string()],
            next_action: "handoff_to_review".to_string(),
        };

        let strict = HandoffPolicy {
            max_summary_chars: 32,
            max_artifacts: 2,
            max_needs: 1,
        };

        assert!(msg.validate(strict).is_err());

        let compacted = msg.compact_for_handoff(strict);
        assert!(compacted.validate(strict).is_ok());
        assert_eq!(compacted.artifacts.len(), 2);
        assert_eq!(compacted.needs.len(), 1);
        assert!(compacted.summary.chars().count() <= strict.max_summary_chars);
    }

    #[test]
    fn coordination_ratio_increases_by_topology_density() {
        let params = OrchestrationEvalParams::default();
        let budget = TeamBudgetProfile::from_tier(BudgetTier::Medium);
        let report = evaluate_team_topologies(budget, &params, &TeamTopology::all());
        let rows = by_topology(&report.evaluations);

        assert!(
            rows[&TeamTopology::Single].coordination_ratio
                < rows[&TeamTopology::LeadSubagent].coordination_ratio
        );
        assert!(
            rows[&TeamTopology::LeadSubagent].coordination_ratio
                < rows[&TeamTopology::StarTeam].coordination_ratio
        );
        assert!(
            rows[&TeamTopology::StarTeam].coordination_ratio
                < rows[&TeamTopology::MeshTeam].coordination_ratio
        );
    }

    #[test]
    fn transcript_mode_costs_more_than_a2a_lite() {
        let base_params = OrchestrationEvalParams {
            protocol: ProtocolMode::A2aLite,
            ..OrchestrationEvalParams::default()
        };
        let transcript_params = OrchestrationEvalParams {
            protocol: ProtocolMode::Transcript,
            ..OrchestrationEvalParams::default()
        };

        let budget = TeamBudgetProfile::from_tier(BudgetTier::Medium);

        let base = evaluate_team_topologies(budget, &base_params, &[TeamTopology::StarTeam]);
        let transcript =
            evaluate_team_topologies(budget, &transcript_params, &[TeamTopology::StarTeam]);

        assert!(
            transcript.evaluations[0].coordination_tokens > base.evaluations[0].coordination_tokens
        );
    }

    #[test]
    fn auto_degradation_recovers_mesh_under_pressure() {
        let no_degrade = OrchestrationEvalParams {
            degradation_policy: DegradationPolicy::None,
            ..OrchestrationEvalParams::default()
        };

        let auto_degrade = OrchestrationEvalParams {
            degradation_policy: DegradationPolicy::Auto,
            ..OrchestrationEvalParams::default()
        };

        let budget = TeamBudgetProfile::from_tier(BudgetTier::Medium);

        let base = evaluate_team_topologies(budget, &no_degrade, &[TeamTopology::MeshTeam]);
        let recovered = evaluate_team_topologies(budget, &auto_degrade, &[TeamTopology::MeshTeam]);

        let base_row = &base.evaluations[0];
        let recovered_row = &recovered.evaluations[0];

        assert!(!base_row.gate_pass());
        assert!(recovered_row.gate_pass());
        assert!(recovered_row.degradation_applied);
        assert!(recovered_row.participants < base_row.participants);
        assert!(recovered_row.coordination_tokens < base_row.coordination_tokens);
    }

    #[test]
    fn recommendation_prefers_star_for_medium_default_profile() {
        let params = OrchestrationEvalParams::default();
        let budget = TeamBudgetProfile::from_tier(BudgetTier::Medium);
        let report = evaluate_team_topologies(budget, &params, &TeamTopology::all());

        assert_eq!(
            report.recommendation.recommended_topology,
            Some(TeamTopology::StarTeam)
        );
    }

    #[test]
    fn evaluate_all_budget_tiers_returns_three_reports() {
        let params = OrchestrationEvalParams {
            degradation_policy: DegradationPolicy::Auto,
            ..OrchestrationEvalParams::default()
        };

        let reports =
            evaluate_all_budget_tiers(&params, &[TeamTopology::Single, TeamTopology::StarTeam]);
        assert_eq!(reports.len(), 3);
        assert_eq!(reports[0].budget.tier, BudgetTier::Low);
        assert_eq!(reports[1].budget.tier, BudgetTier::Medium);
        assert_eq!(reports[2].budget.tier, BudgetTier::High);
    }

    fn task(
        id: &str,
        depends_on: &[&str],
        ownership: &[&str],
        exec_tokens: u32,
        coord_tokens: u32,
    ) -> TaskNodeSpec {
        TaskNodeSpec {
            id: id.to_string(),
            depends_on: depends_on.iter().map(|x| x.to_string()).collect(),
            ownership_keys: ownership.iter().map(|x| x.to_string()).collect(),
            estimated_execution_tokens: exec_tokens,
            estimated_coordination_tokens: coord_tokens,
        }
    }

    #[test]
    fn conflict_aware_plan_respects_dependencies_and_locks() {
        let tasks = vec![
            task("A", &[], &["core"], 120, 20),
            task("B", &["A"], &["module-x"], 100, 20),
            task("C", &["A"], &["module-x"], 90, 20),
            task("D", &["A"], &["module-y"], 80, 20),
        ];

        let plan = build_conflict_aware_execution_plan(
            &tasks,
            PlannerConfig {
                max_parallel: 3,
                run_budget_tokens: None,
                min_coordination_tokens_per_task: 8,
            },
        )
        .expect("plan should be built");

        assert_eq!(plan.topological_order.first(), Some(&"A".to_string()));
        assert_eq!(plan.batches[0].task_ids, vec!["A".to_string()]);

        // B and C share the same ownership lock and must not be in the same batch.
        for batch in &plan.batches {
            let has_b = batch.task_ids.contains(&"B".to_string());
            let has_c = batch.task_ids.contains(&"C".to_string());
            assert!(!(has_b && has_c));
        }
    }

    #[test]
    fn cycle_is_reported_for_invalid_dag() {
        let tasks = vec![
            task("A", &["C"], &["core"], 100, 20),
            task("B", &["A"], &["api"], 100, 20),
            task("C", &["B"], &["docs"], 100, 20),
        ];

        let err = build_conflict_aware_execution_plan(&tasks, PlannerConfig::default())
            .expect_err("cycle must fail");

        match err {
            PlanError::CycleDetected(nodes) => {
                assert!(nodes.contains(&"A".to_string()));
                assert!(nodes.contains(&"B".to_string()));
                assert!(nodes.contains(&"C".to_string()));
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn budget_allocator_scales_coordination_under_pressure() {
        let tasks = vec![
            task("T1", &[], &["a"], 100, 50),
            task("T2", &[], &["b"], 100, 50),
            task("T3", &[], &["c"], 100, 50),
        ];

        let allocated = allocate_task_budgets(&tasks, Some(360), 8);
        let total = allocated.iter().map(|x| x.total_tokens).sum::<u64>();
        assert!(total <= 360);
        assert!(allocated.iter().all(|x| x.coordination_tokens >= 8));
    }

    #[test]
    fn validate_plan_detects_batch_ownership_conflict() {
        let tasks = vec![
            task("A", &[], &["same-file"], 100, 20),
            task("B", &[], &["same-file"], 110, 20),
        ];

        let plan = ExecutionPlan {
            topological_order: vec!["A".to_string(), "B".to_string()],
            budgets: vec![
                PlannedTaskBudget {
                    task_id: "A".to_string(),
                    execution_tokens: 100,
                    coordination_tokens: 20,
                    total_tokens: 120,
                },
                PlannedTaskBudget {
                    task_id: "B".to_string(),
                    execution_tokens: 110,
                    coordination_tokens: 20,
                    total_tokens: 130,
                },
            ],
            batches: vec![ExecutionBatch {
                index: 0,
                task_ids: vec!["A".to_string(), "B".to_string()],
                ownership_locks: vec!["same-file".to_string()],
                estimated_total_tokens: 250,
            }],
            total_estimated_tokens: 250,
        };

        let err = validate_execution_plan(&plan, &tasks).expect_err("must fail due to conflict");
        assert!(matches!(
            err,
            PlanValidationError::OwnershipConflictInBatch { .. }
        ));
    }

    #[test]
    fn analyze_plan_produces_expected_diagnostics() {
        let tasks = vec![
            task("A", &[], &["core"], 120, 20),
            task("B", &["A"], &["module-x"], 100, 20),
            task("C", &["A"], &["module-y"], 90, 20),
            task("D", &["B", "C"], &["api"], 80, 20),
        ];

        let plan = build_conflict_aware_execution_plan(
            &tasks,
            PlannerConfig {
                max_parallel: 2,
                run_budget_tokens: None,
                min_coordination_tokens_per_task: 8,
            },
        )
        .expect("plan should succeed");

        let diag = analyze_execution_plan(&plan, &tasks).expect("diagnostics must pass");
        assert_eq!(diag.task_count, 4);
        assert!(diag.batch_count >= 3);
        assert_eq!(diag.critical_path_len, 3);
        assert!(diag.max_parallelism >= 1);
        assert!(diag.parallelism_efficiency > 0.0);
        assert_eq!(diag.dependency_edges, 4);
    }

    #[test]
    fn batch_handoff_messages_are_generated_and_valid() {
        let tasks = vec![
            task("A", &[], &["core"], 120, 20),
            task("B", &["A"], &["module-x"], 100, 20),
            task("C", &["A"], &["module-y"], 90, 20),
        ];

        let plan = build_conflict_aware_execution_plan(
            &tasks,
            PlannerConfig {
                max_parallel: 2,
                run_budget_tokens: None,
                min_coordination_tokens_per_task: 8,
            },
        )
        .expect("plan should be built");

        let policy = HandoffPolicy {
            max_summary_chars: 180,
            max_artifacts: 4,
            max_needs: 2,
        };

        let messages = build_batch_handoff_messages("run-xyz", &plan, &tasks, policy)
            .expect("handoff generation should pass");

        assert_eq!(messages.len(), plan.batches.len());
        for msg in messages {
            assert!(msg.validate(policy).is_ok());
            assert_eq!(msg.run_id, "run-xyz");
            assert_eq!(msg.status, A2AStatus::Queued);
            assert_eq!(msg.recipient, "worker_pool");
        }
    }

    #[test]
    fn validate_plan_rejects_invalid_topological_order() {
        let tasks = vec![
            task("A", &[], &["core"], 100, 20),
            task("B", &["A"], &["api"], 100, 20),
        ];

        let plan = ExecutionPlan {
            topological_order: vec!["B".to_string(), "A".to_string()],
            budgets: vec![
                PlannedTaskBudget {
                    task_id: "A".to_string(),
                    execution_tokens: 100,
                    coordination_tokens: 20,
                    total_tokens: 120,
                },
                PlannedTaskBudget {
                    task_id: "B".to_string(),
                    execution_tokens: 100,
                    coordination_tokens: 20,
                    total_tokens: 120,
                },
            ],
            batches: vec![
                ExecutionBatch {
                    index: 0,
                    task_ids: vec!["A".to_string()],
                    ownership_locks: vec!["core".to_string()],
                    estimated_total_tokens: 120,
                },
                ExecutionBatch {
                    index: 1,
                    task_ids: vec!["B".to_string()],
                    ownership_locks: vec!["api".to_string()],
                    estimated_total_tokens: 120,
                },
            ],
            total_estimated_tokens: 240,
        };

        let err = validate_execution_plan(&plan, &tasks).expect_err("order should be rejected");
        assert!(matches!(
            err,
            PlanValidationError::DependencyOrderViolation { .. }
        ));
    }

    #[test]
    fn validate_plan_rejects_batch_index_mismatch() {
        let tasks = vec![task("A", &[], &["core"], 100, 20)];
        let plan = ExecutionPlan {
            topological_order: vec!["A".to_string()],
            budgets: vec![PlannedTaskBudget {
                task_id: "A".to_string(),
                execution_tokens: 100,
                coordination_tokens: 20,
                total_tokens: 120,
            }],
            batches: vec![ExecutionBatch {
                index: 3,
                task_ids: vec!["A".to_string()],
                ownership_locks: vec!["core".to_string()],
                estimated_total_tokens: 120,
            }],
            total_estimated_tokens: 120,
        };

        let err = validate_execution_plan(&plan, &tasks).expect_err("must fail");
        assert!(matches!(
            err,
            PlanValidationError::BatchIndexMismatch {
                expected: 0,
                actual: 3
            }
        ));
    }

    #[test]
    fn derive_planner_config_uses_selected_topology_and_budget() {
        let tasks = vec![
            task("A", &[], &["core"], 120, 20),
            task("B", &["A"], &["module-x"], 100, 20),
            task("C", &["A"], &["module-y"], 90, 20),
            task("D", &["B", "C"], &["api"], 80, 20),
        ];

        let budget = TeamBudgetProfile::from_tier(BudgetTier::Medium);
        let params = OrchestrationEvalParams::default();
        let report = evaluate_team_topologies(budget, &params, &TeamTopology::all());
        let selected = report
            .evaluations
            .iter()
            .find(|row| row.topology == report.recommendation.recommended_topology.unwrap())
            .expect("selected topology must exist");

        let cfg = derive_planner_config(selected, &tasks, budget);
        let expected_exec = tasks
            .iter()
            .map(|t| u64::from(t.estimated_execution_tokens))
            .sum::<u64>();
        let expected_budget = expected_exec + (tasks.len() as u64 * 20);

        assert!(cfg.max_parallel >= 1);
        assert!(cfg.max_parallel <= tasks.len());
        assert_eq!(cfg.run_budget_tokens, Some(expected_budget));
        assert_eq!(cfg.min_coordination_tokens_per_task, 10);
    }

    #[test]
    fn handoff_compaction_reduces_estimated_tokens() {
        let message = A2ALiteMessage {
            run_id: "run-1".to_string(),
            task_id: "task-1".to_string(),
            sender: "lead".to_string(),
            recipient: "worker".to_string(),
            status: A2AStatus::Running,
            confidence: 90,
            risk_level: RiskLevel::Medium,
            summary:
                "This summary is deliberately verbose so compaction can reduce communication token usage."
                    .to_string(),
            artifacts: vec![
                "artifact://alpha".to_string(),
                "artifact://beta".to_string(),
                "artifact://gamma".to_string(),
            ],
            needs: vec![
                "dependency-review".to_string(),
                "architecture-signoff".to_string(),
            ],
            next_action: "dispatch".to_string(),
        };

        let loose = HandoffPolicy {
            max_summary_chars: 240,
            max_artifacts: 8,
            max_needs: 6,
        };
        let strict = HandoffPolicy {
            max_summary_chars: 48,
            max_artifacts: 1,
            max_needs: 1,
        };

        let loose_msg = message.compact_for_handoff(loose);
        let strict_msg = message.compact_for_handoff(strict);

        assert!(loose_msg.validate(loose).is_ok());
        assert!(strict_msg.validate(strict).is_ok());
        assert!(estimate_handoff_tokens(&strict_msg) < estimate_handoff_tokens(&loose_msg));
    }

    #[test]
    fn orchestrate_task_graph_returns_valid_bundle() {
        let tasks = vec![
            task("A", &[], &["core"], 120, 20),
            task("B", &["A"], &["module-x"], 100, 20),
            task("C", &["A"], &["module-y"], 90, 20),
            task("D", &["B", "C"], &["api"], 80, 20),
        ];

        let budget = TeamBudgetProfile::from_tier(BudgetTier::Medium);
        let params = OrchestrationEvalParams::default();
        let policy = HandoffPolicy {
            max_summary_chars: 180,
            max_artifacts: 4,
            max_needs: 2,
        };

        let bundle = orchestrate_task_graph(
            "run-e2e",
            budget,
            &params,
            &TeamTopology::all(),
            &tasks,
            policy,
        )
        .expect("orchestration should succeed");

        assert_eq!(
            bundle.selected_topology,
            bundle.report.recommendation.recommended_topology.unwrap()
        );
        assert!(validate_execution_plan(&bundle.plan, &tasks).is_ok());
        assert_eq!(bundle.handoff_messages.len(), bundle.plan.batches.len());
        assert_eq!(
            bundle.estimated_handoff_tokens,
            estimate_batch_handoff_tokens(&bundle.handoff_messages)
        );
        assert_eq!(bundle.diagnostics.task_count, tasks.len());
    }
}
