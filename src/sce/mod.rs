pub mod beliefs;
pub mod goals;
pub mod imagination;
pub mod lessons;
pub mod self_model;
pub mod world_model;

use std::sync::Arc;

use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

use crate::conscience::types::{GateVerdict, Value};
use crate::consciousness::traits::{AgentKind, ConsciousnessState, Proposal};
use crate::continuity::types::Identity;
use crate::memory::traits::Memory;
use crate::quantum::brain::QuantumBrainEngine;
use crate::soul::model::SoulModel;

use self::beliefs::BeliefsStore;
use self::goals::{GoalStack, SceGoalStatus};
use self::lessons::LessonsLog;
use self::self_model::SelfModel;
use self::world_model::WorldModel;

pub trait KernelLayer {
    fn soul(&self) -> &SoulModel;
    fn identity(&self) -> &Identity;
    fn mission(&self) -> &str;
    fn principles(&self) -> &[String];
}

pub trait CognitionLayer {
    fn beliefs(&self) -> &BeliefsStore;
    fn beliefs_mut(&mut self) -> &mut BeliefsStore;
    fn goals(&self) -> &GoalStack;
    fn goals_mut(&mut self) -> &mut GoalStack;
    fn intentions(&self) -> &[String];
    fn world_model(&self) -> &WorldModel;
    fn world_model_mut(&mut self) -> &mut WorldModel;
    fn reasoning(&self) -> &str;
    fn conscience(&self) -> GateVerdict;
    fn heuristics(&self) -> &[String];
    fn decision_policy(&self) -> &str;
    fn arbitration(&self) -> &str;
}

pub trait MemoryLayer {
    fn episodic(&self) -> &[String];
    fn semantic(&self) -> &[String];
    fn working(&self) -> &[String];
    fn long_term(&self) -> &dyn Memory;
    fn consolidate(&mut self) -> Result<usize>;
}

pub trait PerceptionLayer {
    fn sensors(&self) -> &[String];
    fn observations(&self) -> &[String];
    fn signals(&self) -> &[world_model::Signal];
    fn anomalies(&self) -> Vec<&world_model::Anomaly>;
    fn attention(&self) -> f64;
}

pub trait ExecutionLayer {
    fn tasks(&self) -> &[String];
    fn state(&self) -> &str;
    fn plans(&self) -> &[String];
    fn actions(&self) -> &[String];
    fn feedback(&self) -> &[String];
}

pub trait MultiAgentLayer {
    fn agents(&self) -> &[AgentKind];
    fn roles(&self) -> &[String];
    fn collaboration(&self) -> &str;
    fn delegation(&self) -> &[String];
}

pub trait SelfModelLayer {
    fn capabilities(&self) -> Vec<&self_model::Capability>;
    fn limitations(&self) -> &[self_model::Limitation];
    fn performance(&self) -> &[self_model::PerformanceMetric];
    fn self_assessment(&self) -> f64;
}

pub trait EvolutionLayer {
    fn lessons(&self) -> &LessonsLog;
    fn lessons_mut(&mut self) -> &mut LessonsLog;
    fn experiments(&self) -> &[String];
    fn upgrades(&self) -> &[String];
}

pub trait AlignmentLayer {
    fn rules(&self) -> &[String];
    fn ethics(&self) -> &[Value];
    fn safety(&self) -> f64;
    fn boundaries(&self) -> &[String];
    fn risk_register(&self) -> &[String];
}

pub trait RuntimeLayer {
    fn active_context(&self) -> &str;
    fn current_objective(&self) -> Option<&str>;
    fn next_action(&self) -> Option<&str>;
    fn blockers(&self) -> &[String];
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ScePhase {
    Perceive,
    Cognize,
    Decide,
    Execute,
    Reflect,
    Evolve,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SceTickResult {
    pub tick: u64,
    pub phase_completed: ScePhase,
    pub proposals_generated: usize,
    pub actions_taken: usize,
    pub lessons_learned: usize,
    pub coherence: f64,
    pub self_assessment: f64,
    pub timestamp: DateTime<Utc>,
}

pub struct SelfContinuityEngine {
    tick_count: u64,
    mission: String,
    principles: Vec<String>,
    soul: SoulModel,
    identity: Identity,
    beliefs: BeliefsStore,
    goal_stack: GoalStack,
    world: WorldModel,
    self_model: SelfModel,
    lessons_log: LessonsLog,
    consciousness_state: ConsciousnessState,
    quantum_brain: QuantumBrainEngine,
    memory_backend: Arc<RwLock<dyn Memory>>,

    working_memory: Vec<String>,
    episodic_buffer: Vec<String>,
    semantic_cache: Vec<String>,
    current_intentions: Vec<String>,
    active_tasks: Vec<String>,
    current_plans: Vec<String>,
    recent_actions: Vec<String>,
    feedback_log: Vec<String>,
    active_agents: Vec<AgentKind>,
    agent_roles: Vec<String>,
    delegations: Vec<String>,
    active_rules: Vec<String>,
    ethics: Vec<Value>,
    boundary_list: Vec<String>,
    risk_register: Vec<String>,
    sensor_list: Vec<String>,
    observation_buffer: Vec<String>,
    heuristic_list: Vec<String>,
    experiment_log: Vec<String>,
    upgrade_log: Vec<String>,
    blocker_list: Vec<String>,
    active_context: String,
    current_objective: Option<String>,
    next_action_desc: Option<String>,
    reasoning_trace: String,
    decision_policy: String,
    arbitration_mode: String,
    collaboration_mode: String,
}

impl SelfContinuityEngine {
    pub fn new(
        soul: SoulModel,
        identity: Identity,
        mission: String,
        principles: Vec<String>,
        memory_backend: Arc<RwLock<dyn Memory>>,
        quantum_brain: QuantumBrainEngine,
    ) -> Self {
        Self {
            tick_count: 0,
            mission,
            principles,
            soul,
            identity,
            beliefs: BeliefsStore::new(),
            goal_stack: GoalStack::new(),
            world: WorldModel::new(),
            self_model: SelfModel::new(),
            lessons_log: LessonsLog::new(),
            consciousness_state: ConsciousnessState::default(),
            quantum_brain,
            memory_backend,
            working_memory: Vec::new(),
            episodic_buffer: Vec::new(),
            semantic_cache: Vec::new(),
            current_intentions: Vec::new(),
            active_tasks: Vec::new(),
            current_plans: Vec::new(),
            recent_actions: Vec::new(),
            feedback_log: Vec::new(),
            active_agents: Vec::new(),
            agent_roles: Vec::new(),
            delegations: Vec::new(),
            active_rules: Vec::new(),
            ethics: Vec::new(),
            boundary_list: Vec::new(),
            risk_register: Vec::new(),
            sensor_list: Vec::new(),
            observation_buffer: Vec::new(),
            heuristic_list: Vec::new(),
            experiment_log: Vec::new(),
            upgrade_log: Vec::new(),
            blocker_list: Vec::new(),
            active_context: String::new(),
            current_objective: None,
            next_action_desc: None,
            reasoning_trace: String::new(),
            decision_policy: "weighted_consensus".into(),
            arbitration_mode: "priority_first".into(),
            collaboration_mode: "cooperative".into(),
        }
    }

    pub fn tick(&mut self) -> Result<SceTickResult> {
        self.tick_count += 1;
        let tick = self.tick_count;

        let _signals_count = self.perceive_phase();
        let proposals = self.cognize_phase();
        let actions_count = self.decide_and_execute_phase(&proposals);
        let lessons_count = self.reflect_phase(&proposals);
        self.evolve_phase();

        let coherence = self.consciousness_state.coherence;
        let assessment = self.self_model.self_assessment();

        Ok(SceTickResult {
            tick,
            phase_completed: ScePhase::Evolve,
            proposals_generated: proposals.len(),
            actions_taken: actions_count,
            lessons_learned: lessons_count,
            coherence,
            self_assessment: assessment,
            timestamp: Utc::now(),
        })
    }

    fn perceive_phase(&mut self) -> usize {
        self.observation_buffer.clear();

        let anomaly_count = self.world.unresolved_anomalies().len();
        if anomaly_count > 0 {
            self.observation_buffer
                .push(format!("{anomaly_count} unresolved anomalies detected"));
        }

        let signal_count = self.world.recent_signals(10).len();
        if signal_count > 0 {
            self.observation_buffer
                .push(format!("{signal_count} recent signals ingested"));
        }

        self.consciousness_state.phenomenal.attention = if anomaly_count > 0 {
            (0.5 + anomaly_count as f64 * 0.1).min(1.0)
        } else {
            0.5
        };

        signal_count
    }

    fn cognize_phase(&mut self) -> Vec<Proposal> {
        let mut proposals = Vec::new();

        if let Some(top_goal) = self.goal_stack.peek_highest() {
            if top_goal.status == SceGoalStatus::Pending {
                proposals.push(Proposal {
                    id: self.tick_count,
                    source: AgentKind::Strategy,
                    action: format!("pursue goal: {}", top_goal.description),
                    reasoning: "highest priority pending goal".into(),
                    confidence: top_goal.priority.weight(),
                    priority: top_goal.priority,
                    contradicts: Vec::new(),
                    timestamp: Utc::now(),
                });
            }
        }

        for anomaly in self.world.unresolved_anomalies() {
            if anomaly.severity > 0.7 {
                proposals.push(Proposal {
                    id: self.tick_count * 1000 + proposals.len() as u64,
                    source: AgentKind::Conscience,
                    action: format!("investigate anomaly: {}", anomaly.description),
                    reasoning: format!("severity {:.2} exceeds threshold", anomaly.severity),
                    confidence: anomaly.severity,
                    priority: crate::consciousness::traits::Priority::High,
                    contradicts: Vec::new(),
                    timestamp: Utc::now(),
                });
            }
        }

        for belief in self.beliefs.above_confidence(0.8) {
            use std::fmt::Write;
            let _ = writeln!(
                self.reasoning_trace,
                "strong belief: {} = {}",
                belief.key, belief.value
            );
        }

        proposals
    }

    fn decide_and_execute_phase(&mut self, proposals: &[Proposal]) -> usize {
        let mut executed = 0;

        for proposal in proposals {
            if proposal.confidence < 0.3 {
                continue;
            }

            self.recent_actions.push(proposal.action.clone());
            self.self_model.record_outcome("decision_making", true);
            executed += 1;

            if self.recent_actions.len() > 1000 {
                self.recent_actions.remove(0);
            }
        }

        self.consciousness_state.active_proposals = proposals.to_vec();
        executed
    }

    fn reflect_phase(&self, proposals: &[Proposal]) -> usize {
        if proposals.is_empty() {
            return 0;
        }

        let avg_confidence =
            proposals.iter().map(|p| p.confidence).sum::<f64>() / proposals.len() as f64;

        if avg_confidence < 0.5 {
            1
        } else {
            0
        }
    }

    fn evolve_phase(&mut self) {
        self.beliefs.decay(0.999);
        self.beliefs.prune(0.01);
    }

    pub fn consciousness_state(&self) -> &ConsciousnessState {
        &self.consciousness_state
    }

    pub fn quantum_brain(&self) -> &QuantumBrainEngine {
        &self.quantum_brain
    }

    pub fn quantum_brain_mut(&mut self) -> &mut QuantumBrainEngine {
        &mut self.quantum_brain
    }
}

impl KernelLayer for SelfContinuityEngine {
    fn soul(&self) -> &SoulModel {
        &self.soul
    }

    fn identity(&self) -> &Identity {
        &self.identity
    }

    fn mission(&self) -> &str {
        &self.mission
    }

    fn principles(&self) -> &[String] {
        &self.principles
    }
}

impl CognitionLayer for SelfContinuityEngine {
    fn beliefs(&self) -> &BeliefsStore {
        &self.beliefs
    }

    fn beliefs_mut(&mut self) -> &mut BeliefsStore {
        &mut self.beliefs
    }

    fn goals(&self) -> &GoalStack {
        &self.goal_stack
    }

    fn goals_mut(&mut self) -> &mut GoalStack {
        &mut self.goal_stack
    }

    fn intentions(&self) -> &[String] {
        &self.current_intentions
    }

    fn world_model(&self) -> &WorldModel {
        &self.world
    }

    fn world_model_mut(&mut self) -> &mut WorldModel {
        &mut self.world
    }

    fn reasoning(&self) -> &str {
        &self.reasoning_trace
    }

    fn conscience(&self) -> GateVerdict {
        if self.consciousness_state.coherence > 0.8 {
            GateVerdict::Allow
        } else if self.consciousness_state.coherence > 0.5 {
            GateVerdict::Ask
        } else {
            GateVerdict::Block
        }
    }

    fn heuristics(&self) -> &[String] {
        &self.heuristic_list
    }

    fn decision_policy(&self) -> &str {
        &self.decision_policy
    }

    fn arbitration(&self) -> &str {
        &self.arbitration_mode
    }
}

impl PerceptionLayer for SelfContinuityEngine {
    fn sensors(&self) -> &[String] {
        &self.sensor_list
    }

    fn observations(&self) -> &[String] {
        &self.observation_buffer
    }

    fn signals(&self) -> &[world_model::Signal] {
        self.world.recent_signals(100)
    }

    fn anomalies(&self) -> Vec<&world_model::Anomaly> {
        self.world.unresolved_anomalies()
    }

    fn attention(&self) -> f64 {
        self.consciousness_state.phenomenal.attention
    }
}

impl ExecutionLayer for SelfContinuityEngine {
    fn tasks(&self) -> &[String] {
        &self.active_tasks
    }

    fn state(&self) -> &str {
        &self.active_context
    }

    fn plans(&self) -> &[String] {
        &self.current_plans
    }

    fn actions(&self) -> &[String] {
        &self.recent_actions
    }

    fn feedback(&self) -> &[String] {
        &self.feedback_log
    }
}

impl MultiAgentLayer for SelfContinuityEngine {
    fn agents(&self) -> &[AgentKind] {
        &self.active_agents
    }

    fn roles(&self) -> &[String] {
        &self.agent_roles
    }

    fn collaboration(&self) -> &str {
        &self.collaboration_mode
    }

    fn delegation(&self) -> &[String] {
        &self.delegations
    }
}

impl SelfModelLayer for SelfContinuityEngine {
    fn capabilities(&self) -> Vec<&self_model::Capability> {
        self.self_model.capabilities().collect()
    }

    fn limitations(&self) -> &[self_model::Limitation] {
        self.self_model.limitations()
    }

    fn performance(&self) -> &[self_model::PerformanceMetric] {
        self.self_model.recent_metrics(100)
    }

    fn self_assessment(&self) -> f64 {
        self.self_model.self_assessment()
    }
}

impl EvolutionLayer for SelfContinuityEngine {
    fn lessons(&self) -> &LessonsLog {
        &self.lessons_log
    }

    fn lessons_mut(&mut self) -> &mut LessonsLog {
        &mut self.lessons_log
    }

    fn experiments(&self) -> &[String] {
        &self.experiment_log
    }

    fn upgrades(&self) -> &[String] {
        &self.upgrade_log
    }
}

impl AlignmentLayer for SelfContinuityEngine {
    fn rules(&self) -> &[String] {
        &self.active_rules
    }

    fn ethics(&self) -> &[Value] {
        &self.ethics
    }

    fn safety(&self) -> f64 {
        self.consciousness_state.coherence
    }

    fn boundaries(&self) -> &[String] {
        &self.boundary_list
    }

    fn risk_register(&self) -> &[String] {
        &self.risk_register
    }
}

impl RuntimeLayer for SelfContinuityEngine {
    fn active_context(&self) -> &str {
        &self.active_context
    }

    fn current_objective(&self) -> Option<&str> {
        self.current_objective.as_deref()
    }

    fn next_action(&self) -> Option<&str> {
        self.next_action_desc.as_deref()
    }

    fn blockers(&self) -> &[String] {
        &self.blocker_list
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::continuity::types::IdentityCore;
    use beliefs::BeliefSource;
    use goals::SceGoal;

    fn make_identity() -> Identity {
        Identity {
            core: IdentityCore {
                name: "test_agent".into(),
                constitution_hash: "abc123".into(),
                creation_epoch: 0,
                immutable_values: vec!["truth".into()],
            },
            preferences: Vec::new(),
            narrative: Vec::new(),
            commitments: Vec::new(),
            session_count: 0,
        }
    }

    struct MockMemory;

    #[async_trait::async_trait]
    impl Memory for MockMemory {
        fn name(&self) -> &str {
            "mock"
        }
        async fn store(
            &self,
            _key: &str,
            _content: &str,
            _category: crate::memory::traits::MemoryCategory,
            _session_id: Option<&str>,
        ) -> Result<()> {
            Ok(())
        }
        async fn recall(
            &self,
            _query: &str,
            _limit: usize,
            _session_id: Option<&str>,
        ) -> Result<Vec<crate::memory::traits::MemoryEntry>> {
            Ok(Vec::new())
        }
        async fn get(&self, _key: &str) -> Result<Option<crate::memory::traits::MemoryEntry>> {
            Ok(None)
        }
        async fn list(
            &self,
            _category: Option<&crate::memory::traits::MemoryCategory>,
            _session_id: Option<&str>,
        ) -> Result<Vec<crate::memory::traits::MemoryEntry>> {
            Ok(Vec::new())
        }
        async fn forget(&self, _key: &str) -> Result<bool> {
            Ok(true)
        }
        async fn count(&self) -> Result<usize> {
            Ok(0)
        }
        async fn health_check(&self) -> bool {
            true
        }
    }

    fn make_engine() -> SelfContinuityEngine {
        let memory: Arc<RwLock<dyn Memory>> = Arc::new(RwLock::new(MockMemory));
        SelfContinuityEngine::new(
            SoulModel::default(),
            make_identity(),
            "test mission".into(),
            vec!["principle_a".into()],
            memory,
            QuantumBrainEngine::new(),
        )
    }

    #[test]
    fn tick_increments_counter() {
        let mut engine = make_engine();
        let r1 = engine.tick().unwrap();
        assert_eq!(r1.tick, 1);
        let r2 = engine.tick().unwrap();
        assert_eq!(r2.tick, 2);
    }

    #[test]
    fn kernel_layer_returns_identity() {
        let engine = make_engine();
        assert_eq!(engine.mission(), "test mission");
        assert_eq!(engine.principles().len(), 1);
        assert_eq!(engine.identity().core.name, "test_agent");
    }

    #[test]
    fn cognition_layer_manages_beliefs() {
        let mut engine = make_engine();
        engine
            .beliefs_mut()
            .set("k".into(), "v".into(), 0.9, BeliefSource::Observation);
        assert_eq!(engine.beliefs().len(), 1);
    }

    #[test]
    fn perception_tracks_anomalies() {
        let mut engine = make_engine();
        engine
            .world_model_mut()
            .register_anomaly("test_anomaly".into(), 0.8);
        assert_eq!(engine.anomalies().len(), 1);
    }

    #[test]
    fn evolution_records_lessons() {
        let mut engine = make_engine();
        engine
            .lessons_mut()
            .record("pattern".into(), "insight".into(), 0.7, 1, vec![]);
        assert_eq!(engine.lessons().len(), 1);
    }

    #[test]
    fn self_model_layer_assessment() {
        let engine = make_engine();
        let score = SelfModelLayer::self_assessment(&engine);
        assert!((0.0..=1.0).contains(&score));
    }

    #[test]
    fn tick_with_goals_generates_proposals() {
        let mut engine = make_engine();
        engine.goals_mut().push(SceGoal {
            id: "g1".into(),
            description: "test goal".into(),
            priority: crate::consciousness::traits::Priority::High,
            created_at: Utc::now(),
            deadline: None,
            status: SceGoalStatus::Pending,
            parent_id: None,
            progress: 0.0,
        });
        let result = engine.tick().unwrap();
        assert!(result.proposals_generated > 0);
    }

    #[test]
    fn tick_with_anomaly_generates_proposals() {
        let mut engine = make_engine();
        engine
            .world_model_mut()
            .register_anomaly("critical_issue".into(), 0.9);
        let result = engine.tick().unwrap();
        assert!(result.proposals_generated > 0);
    }
}
