//! LearningBrain — the core cognitive engine.
//!
//! Combines RVF memory, SONA micro-LoRA learning, experience replay,
//! knowledge graph, and neural (DentateGyrus + HDC) filtering into a
//! single in-process brain for the ZeroClaw agent.

use anyhow::Result;
use ruvector_gnn::ReplayBuffer;
use ruvector_sona::SonaEngine;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet, VecDeque};

use super::knowledge_graph::KnowledgeGraph;
use super::neural_filter::NeuralFilter;
use super::rvf_memory::RvfMemory;
use super::text_store::StoredMemory;

pub const EMBED_DIM: usize = 384;
const REPLAY_CAPACITY: usize = 10_000;
const REPLAY_MIN_FOR_TRAINING: usize = 64;
const DEDUP_WINDOW: usize = 500;

// ── Telemetry types ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct LearningTelemetry {
    pub rescued_learn_memories: u64,
    pub backfilled_learn_memories: u64,
    pub consolidated_memories: u64,
    pub last_consolidation_ts: i64,
    pub tiers: BTreeMap<String, TierTelemetry>,
    #[serde(default)]
    pub consolidated_groups: BTreeMap<String, usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TierTelemetry {
    pub attempts: u64,
    pub local_successes: u64,
    pub cloud_rescues: u64,
    pub direct_cloud: u64,
    pub forced_requests: u64,
    pub last_model_used: String,
    pub last_seen_ts: i64,
    #[serde(default)]
    pub families: BTreeMap<String, FamilyTelemetry>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct FamilyTelemetry {
    pub attempts: u64,
    pub local_successes: u64,
    pub cloud_rescues: u64,
    pub direct_cloud: u64,
    pub forced_requests: u64,
    pub last_model_used: String,
    pub last_seen_ts: i64,
}

pub struct ConsolidationCandidate {
    pub group_key: String,
    pub source_count: usize,
    pub content: String,
    pub tags: Vec<String>,
}

// ── Routing tiers ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoutingTier {
    Cloud,
    LocalCode,
    LocalMid,
    LocalFast,
}

impl RoutingTier {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Cloud => "tier1_cloud",
            Self::LocalCode => "tier2_code_27b",
            Self::LocalMid => "tier2_local_9b",
            Self::LocalFast => "tier3_local_0.8b",
        }
    }
}

// ── LearningBrain ────────────────────────────────────────────────────────────

pub struct LearningBrain {
    pub rvf_memory: RvfMemory,
    sona: SonaEngine,
    replay: ReplayBuffer,
    pub knowledge_graph: KnowledgeGraph,
    neural_filter: NeuralFilter,
    recent_codes: VecDeque<Vec<f32>>,
    seen_hashes: HashSet<u64>,
    sona_path: String,
    pub telemetry: LearningTelemetry,
    telemetry_path: String,
}

impl LearningBrain {
    pub async fn new(rvf_path: &str, db_path: &str, branch: &str) -> Result<Self> {
        let rvf_memory = RvfMemory::open(rvf_path, db_path, branch).await?;

        let rvf_abs = shellexpand::tilde(rvf_path).to_string();
        let kg_path = rvf_abs
            .strip_suffix(".rvf")
            .map(|stem| format!("{}.kg.redb", stem))
            .unwrap_or_else(|| format!("{}.kg.redb", &rvf_abs));
        let knowledge_graph = KnowledgeGraph::open_persistent(&kg_path);

        let sona_path = rvf_abs
            .strip_suffix(".rvf")
            .map(|stem| format!("{}.sona.json", stem))
            .unwrap_or_else(|| format!("{}.sona.json", &rvf_abs));
        let mut sona = SonaEngine::new(EMBED_DIM);
        if let Err(e) = sona.load_snapshot(&sona_path) {
            tracing::debug!(path = %sona_path, error = %e, "SONA snapshot not found, starting fresh");
        } else {
            let patterns = sona.get_all_patterns().len();
            if patterns > 0 {
                tracing::info!(path = %sona_path, patterns, "SONA snapshot restored");
            }
        }

        let replay = ReplayBuffer::new(REPLAY_CAPACITY);

        let telemetry_path = rvf_abs
            .strip_suffix(".rvf")
            .map(|stem| format!("{}.learning.json", stem))
            .unwrap_or_else(|| format!("{}.learning.json", &rvf_abs));
        let mut telemetry = std::fs::read_to_string(&telemetry_path)
            .ok()
            .and_then(|raw| serde_json::from_str::<LearningTelemetry>(&raw).ok())
            .unwrap_or_default();
        normalize_learning_telemetry(&mut telemetry);

        let neural_filter = NeuralFilter::new(EMBED_DIM)?;
        let mut brain = Self {
            rvf_memory,
            sona,
            replay,
            knowledge_graph,
            neural_filter,
            recent_codes: VecDeque::with_capacity(DEDUP_WINDOW + 1),
            seen_hashes: HashSet::new(),
            sona_path,
            telemetry,
            telemetry_path,
        };
        let _ = brain.backfill_learn_telemetry();
        Ok(brain)
    }

    pub fn save_learning_telemetry(&self) {
        match serde_json::to_string_pretty(&self.telemetry) {
            Ok(json) => {
                if let Err(e) = std::fs::write(&self.telemetry_path, json) {
                    tracing::warn!(path = %self.telemetry_path, error = %e, "telemetry save failed");
                }
            }
            Err(e) => tracing::warn!(error = %e, "telemetry serialisation failed"),
        }
    }

    /// Ingest content — record as positive learning signal.
    pub async fn process_and_learn(
        &mut self,
        content: &str,
        embedding: Vec<f32>,
        sender: &str,
        channel: &str,
        tags: &[&str],
        _is_important: bool,
    ) -> Result<()> {
        let timestamp = chrono::Utc::now().timestamp();

        // 1. Exact dedup via content hash
        let content_hash = fnv64(content);
        if !self.seen_hashes.insert(content_hash) {
            tracing::debug!(content_preview = &content[..content.len().min(40)], "exact duplicate blocked");
            return Ok(());
        }

        // 2. DentateGyrus soft telemetry
        let sparse_code = self.neural_filter.sparse_code(&embedding);
        let codes_slice: Vec<Vec<f32>> = self.recent_codes.iter().cloned().collect();
        if self.neural_filter.is_suspicious_duplicate(&embedding, &codes_slice) {
            tracing::debug!("high sparse-code similarity — possible near-duplicate (stored anyway)");
        }
        if self.recent_codes.len() >= DEDUP_WINDOW {
            self.recent_codes.pop_front();
        }
        self.recent_codes.push_back(sparse_code);

        // 3. Stable UUID
        let memory_id = uuid::Uuid::new_v4().to_string();

        // 4. SONA positive trajectory
        let mut builder = self.sona.begin_trajectory(embedding.clone());
        builder.add_step(embedding.clone(), embedding.clone(), 1.0);
        self.sona.end_trajectory(builder, 1.0);

        // 5. Replay buffer
        self.replay.add(&embedding, &[]);

        // 6. HDC tag signature
        self.neural_filter.store_tag_signature(&format!("mem:{}", memory_id), tags);

        // 7. Knowledge graph
        if let Err(e) = self.knowledge_graph.record_memory(
            &memory_id, content, sender, channel, tags, timestamp,
        ) {
            tracing::warn!(error = %e, "knowledge graph record error");
        }

        // 8. RVF + SQLite
        self.rvf_memory
            .save_message(&memory_id, content, embedding, sender, channel, tags, timestamp)
            .await?;

        Ok(())
    }

    /// Explicit quality feedback from zeroclaw (0.0 = bad, 1.0 = good).
    pub fn record_feedback(&mut self, quality: f32) {
        let clamped = quality.clamp(0.0, 1.0);
        let emb = vec![clamped; EMBED_DIM];
        let mut builder = self.sona.begin_trajectory(emb.clone());
        builder.add_step(emb.clone(), emb, clamped);
        self.sona.end_trajectory(builder, clamped);
        self.sona.tick();
        self.save_sona_snapshot();
    }

    pub fn record_routing_outcome(
        &mut self,
        query: &str,
        tier_label: &str,
        model_used: &str,
        used_cloud_fallback: bool,
        forced_request: bool,
    ) {
        let family = classify_task_family(query);
        let now = chrono::Utc::now().timestamp();
        let canonical_tier = normalize_tier_label(tier_label);

        let tier = self.telemetry.tiers.entry(canonical_tier.clone()).or_default();
        tier.attempts += 1;
        tier.last_model_used = model_used.to_string();
        tier.last_seen_ts = now;
        if forced_request { tier.forced_requests += 1; }

        let fam = tier.families.entry(family.clone()).or_default();
        fam.attempts += 1;
        fam.last_model_used = model_used.to_string();
        fam.last_seen_ts = now;
        if forced_request { fam.forced_requests += 1; }

        if canonical_tier == RoutingTier::Cloud.label() {
            tier.direct_cloud += 1;
            fam.direct_cloud += 1;
        } else if used_cloud_fallback {
            tier.cloud_rescues += 1;
            fam.cloud_rescues += 1;
            self.telemetry.rescued_learn_memories += 1;
        } else {
            tier.local_successes += 1;
            fam.local_successes += 1;
        }

        self.save_learning_telemetry();
    }

    pub fn backfill_learn_telemetry(&mut self) -> Result<usize> {
        normalize_learning_telemetry(&mut self.telemetry);
        let learn_memories = self.rvf_memory.text_store.recent_by_channel("learn", 5000)?;
        let mut grouped: BTreeMap<(String, String), usize> = BTreeMap::new();
        let mut tier_totals: BTreeMap<String, usize> = BTreeMap::new();

        for memory in &learn_memories {
            let tier = extract_tier_from_tags(&memory.tags)
                .or_else(|| extract_tier_from_learn_content(&memory.content))
                .map(|raw| normalize_tier_label(&raw))
                .unwrap_or_else(|| "unknown".to_string());
            let family = classify_task_family(&extract_question_from_learn(&memory.content));
            *grouped.entry((tier.clone(), family)).or_insert(0) += 1;
            *tier_totals.entry(tier).or_insert(0) += 1;
        }

        self.telemetry.rescued_learn_memories =
            self.telemetry.rescued_learn_memories.max(learn_memories.len() as u64);
        self.telemetry.backfilled_learn_memories = learn_memories.len() as u64;

        for ((tier_name, family), count) in grouped {
            let tier = self.telemetry.tiers.entry(tier_name.clone()).or_default();
            if tier.last_model_used.is_empty() {
                tier.last_model_used = tier_name.clone();
            }
            let fam = tier.families.entry(family).or_default();
            fam.attempts = fam.attempts.max(count as u64);
            fam.cloud_rescues = fam.cloud_rescues.max(count as u64);
            if fam.last_model_used.is_empty() {
                fam.last_model_used = tier_name;
            }
        }

        for (tier_name, count) in tier_totals {
            let tier = self.telemetry.tiers.entry(tier_name.clone()).or_default();
            tier.attempts = tier.attempts.max(count as u64);
            tier.cloud_rescues = tier.cloud_rescues.max(count as u64);
            if tier.last_model_used.is_empty() {
                tier.last_model_used = tier_name;
            }
        }

        self.save_learning_telemetry();
        Ok(learn_memories.len())
    }

    pub fn suggest_routing_bias(
        &self,
        query: &str,
        _complexity: f32,
        code_complexity: f32,
        _local_confidence: f32,
        conversation_mode: bool,
        base_tier: &RoutingTier,
    ) -> Option<RoutingTier> {
        if matches!(base_tier, RoutingTier::LocalCode | RoutingTier::LocalMid | RoutingTier::LocalFast) {
            return None;
        }
        if conversation_mode && _complexity > 0.45 {
            return None;
        }

        let family = classify_task_family(query);

        if family == "code" && code_complexity >= 0.30 {
            if let Some(code_stats) = self.telemetry.tiers
                .get(RoutingTier::LocalCode.label())
                .and_then(|t| t.families.get("code"))
            {
                let attempts = code_stats.local_successes + code_stats.cloud_rescues;
                if attempts >= 3 && code_stats.local_successes >= 2 {
                    let success_rate = code_stats.local_successes as f32 / attempts as f32;
                    if success_rate >= 0.66 { return Some(RoutingTier::LocalCode); }
                }
            }
        }

        let fast = self.telemetry.tiers
            .get(RoutingTier::LocalFast.label())
            .and_then(|t| t.families.get(&family));
        if let Some(stats) = fast {
            let attempts = stats.local_successes + stats.cloud_rescues;
            if attempts >= 4 && _local_confidence >= 0.70 {
                let success_rate = stats.local_successes as f32 / attempts as f32;
                if success_rate >= 0.85 { return Some(RoutingTier::LocalFast); }
            }
        }

        let mid = self.telemetry.tiers
            .get(RoutingTier::LocalMid.label())
            .and_then(|t| t.families.get(&family));
        if let Some(stats) = mid {
            let attempts = stats.local_successes + stats.cloud_rescues;
            if attempts >= 4 && _local_confidence >= 0.60 {
                let success_rate = stats.local_successes as f32 / attempts as f32;
                if success_rate >= 0.75 { return Some(RoutingTier::LocalMid); }
            }
        }

        None
    }

    pub fn plan_learn_consolidations(&self) -> Result<Vec<ConsolidationCandidate>> {
        let learn_memories = self.rvf_memory.text_store.recent_by_channel("learn", 250)?;
        let mut grouped: BTreeMap<String, Vec<StoredMemory>> = BTreeMap::new();

        for memory in learn_memories {
            let tier = extract_tier_from_tags(&memory.tags)
                .or_else(|| extract_tier_from_learn_content(&memory.content))
                .map(|raw| normalize_tier_label(&raw))
                .unwrap_or_else(|| "unknown".to_string());
            let question = extract_question_from_learn(&memory.content);
            let family = classify_task_family(&question);
            let key = format!("{}|{}", tier, family);
            grouped.entry(key).or_default().push(memory);
        }

        let mut candidates = Vec::new();
        for (key, memories) in grouped {
            if memories.len() < 3 { continue; }
            let already = self.telemetry.consolidated_groups.get(&key).copied().unwrap_or(0);
            if memories.len() < already + 3 { continue; }

            let key_for_report = key.clone();
            let mut parts = key.split('|');
            let tier = parts.next().unwrap_or("unknown");
            let family = parts.next().unwrap_or("general");

            let mut examples = Vec::new();
            for memory in memories.iter().take(3) {
                let question = extract_question_from_learn(&memory.content);
                if !question.is_empty() && !examples.contains(&question) {
                    examples.push(question);
                }
            }

            let mut summary = format!(
                "learn-summary: [{}|{}] consolidated {} cloud rescues. ",
                tier, family, memories.len()
            );
            summary.push_str("Representative asks: ");
            if examples.is_empty() {
                summary.push_str("no representative examples extracted.");
            } else {
                for (idx, example) in examples.iter().enumerate() {
                    if idx > 0 { summary.push_str(" | "); }
                    summary.push_str(&truncate_for_summary(example, 100));
                }
            }
            summary.push_str(" Guidance: prefer recalling previous cloud-crafted answers for this family before escalating again.");

            candidates.push(ConsolidationCandidate {
                group_key: key_for_report,
                source_count: memories.len(),
                content: summary,
                tags: vec![
                    "learn-summary:".to_string(),
                    format!("tier:{}", tier),
                    format!("family:{}", family),
                ],
            });
        }

        Ok(candidates)
    }

    pub fn mark_consolidation_applied(&mut self, group_key: &str, source_count: usize) {
        self.telemetry
            .consolidated_groups
            .insert(group_key.to_string(), source_count);
        self.telemetry.consolidated_memories += 1;
        self.telemetry.last_consolidation_ts = chrono::Utc::now().timestamp();
        self.save_learning_telemetry();
    }

    pub fn learning_report_json(
        &self,
        learn_memory_count: usize,
        learn_summary_count: usize,
    ) -> serde_json::Value {
        let mut promotion_candidates = Vec::new();
        let mut rescue_hotspots = Vec::new();
        for (tier, stats) in &self.telemetry.tiers {
            if tier == "tier1_cloud" { continue; }
            for (family, fam) in &stats.families {
                let attempts = fam.local_successes + fam.cloud_rescues;
                if attempts < 4 { continue; }
                let success_rate = fam.local_successes as f32 / attempts as f32;
                let target = promotion_threshold_for_tier(tier);
                if success_rate >= target {
                    promotion_candidates.push(serde_json::json!({
                        "tier": tier, "family": family, "attempts": attempts,
                        "local_successes": fam.local_successes,
                        "cloud_rescues": fam.cloud_rescues,
                        "success_rate": success_rate, "target_success_rate": target,
                        "recommendation": "safe_to_bias_local",
                        "last_model_used": fam.last_model_used,
                    }));
                }
                if fam.cloud_rescues >= 2 && success_rate < target {
                    rescue_hotspots.push(serde_json::json!({
                        "tier": tier, "family": family, "attempts": attempts,
                        "cloud_rescues": fam.cloud_rescues,
                        "local_successes": fam.local_successes,
                        "success_rate": success_rate, "target_success_rate": target,
                        "recommendation": if success_rate + 0.10 >= target {
                            "nearly_ready_monitor_quality"
                        } else {
                            "collect_more_local_rehearsal"
                        },
                        "last_model_used": fam.last_model_used,
                    }));
                }
            }
        }

        serde_json::json!({
            "status": "ok",
            "learn_memory_count": learn_memory_count,
            "learn_summary_count": learn_summary_count,
            "rescued_learn_memories": self.telemetry.rescued_learn_memories,
            "backfilled_learn_memories": self.telemetry.backfilled_learn_memories,
            "consolidated_memories": self.telemetry.consolidated_memories,
            "last_consolidation_ts": self.telemetry.last_consolidation_ts,
            "tiers": self.telemetry.tiers,
            "promotion_candidates": promotion_candidates,
            "rescue_hotspots": rescue_hotspots,
        })
    }

    pub fn save_sona_snapshot(&self) {
        if let Err(e) = self.sona.save_snapshot(&self.sona_path) {
            tracing::warn!(path = %self.sona_path, error = %e, "SONA snapshot save failed");
        }
    }

    /// Recall with SONA-optimised query embedding + graph context expansion.
    pub async fn smart_recall(
        &mut self,
        query_embedding: Vec<f32>,
        k: usize,
    ) -> Result<Vec<(rvf_runtime::options::SearchResult, Option<String>)>> {
        // Apply micro-LoRA adaptation
        let mut lora_out = vec![0.0f32; query_embedding.len()];
        self.sona.apply_micro_lora(&query_embedding, &mut lora_out);

        // Blend: 70% original + 30% LoRA delta
        let blended: Vec<f32> = query_embedding
            .iter()
            .zip(lora_out.iter())
            .map(|(orig, opt)| 0.7 * orig + 0.3 * opt)
            .collect();

        let mut builder = self.sona.begin_trajectory(query_embedding.clone());
        builder.add_step(blended.clone(), blended.clone(), 0.5);

        self.replay.add(&query_embedding, &[]);

        let results = self.rvf_memory.recall(blended, k).await?;

        // Estimate quality from similarity scores
        let quality = if results.is_empty() {
            0.2
        } else {
            let avg = results.iter().map(|(r, _)| r.distance).sum::<f32>() / results.len() as f32;
            (1.0_f32 - avg.min(1.0_f32)).max(0.0_f32)
        };
        self.sona.end_trajectory(builder, quality);

        // 1-hop graph context expansion
        let hit_ids: Vec<String> = results
            .iter()
            .filter_map(|(r, _)| {
                self.rvf_memory
                    .text_store
                    .get_message_id_by_vector_id(r.id)
                    .ok()
                    .flatten()
                    .map(|mid| format!("mem:{}", mid))
            })
            .collect();
        let expanded = self.knowledge_graph.expand_context(&hit_ids);
        if !expanded.is_empty() {
            tracing::debug!(
                expanded = expanded.len(),
                graph_nodes = self.knowledge_graph.node_count(),
                graph_edges = self.knowledge_graph.edge_count(),
                "knowledge graph context expansion"
            );
        }

        Ok(results)
    }

    /// Find memory IDs by tag similarity via HDC.
    pub fn find_memories_by_tags(&self, tags: &[&str], k: usize) -> Vec<(String, f32)> {
        self.neural_filter.find_by_tags(tags, k)
    }

    /// Background consolidation: SONA tick + replay training + RVF compaction.
    pub async fn background_consolidation(&mut self) -> Result<()> {
        // 1. SONA tick
        if let Some(summary) = self.sona.tick() {
            tracing::debug!(summary = %summary, "SONA background tick");
        }

        // 2. Experience replay
        if self.replay.len() >= REPLAY_MIN_FOR_TRAINING {
            let batch = self.replay.sample(32);
            for entry in &batch {
                let emb = entry.query.to_vec();
                let mut builder = self.sona.begin_trajectory(emb.clone());
                builder.add_step(emb.clone(), emb, 0.6);
                self.sona.end_trajectory(builder, 0.6);
            }
            if let Some(summary) = self.sona.tick() {
                tracing::debug!(batch_size = batch.len(), summary = %summary, "SONA replay");
            }

            let drift = self.replay.detect_distribution_shift(256);
            if drift > 0.3 {
                tracing::info!(drift = drift, "distribution shift detected");
            }
        }

        tracing::debug!(
            graph_nodes = self.knowledge_graph.node_count(),
            graph_edges = self.knowledge_graph.edge_count(),
            hdc_entries = self.neural_filter.hdc_size(),
            "brain stats"
        );

        // 3. RVF compaction
        self.rvf_memory.compact().await?;

        // 4. Persist
        self.save_sona_snapshot();
        self.save_learning_telemetry();
        Ok(())
    }
}

// ── Helpers ──────────────────────────────────────────────────────────────────

fn promotion_threshold_for_tier(tier: &str) -> f32 {
    match tier {
        "tier2_code_27b" => 0.66,
        "tier2_local_9b" => 0.75,
        "tier3_local_0.8b" => 0.85,
        _ => 0.80,
    }
}

fn classify_task_family(query: &str) -> String {
    let lower = query.to_lowercase();
    if contains_any_phrase(&lower, &["async/await", "who am i"])
        || has_any_token(&lower, &["rust", "python", "code", "function", "compile", "debug", "sql", "regex", "async", "api"])
    {
        "code".to_string()
    } else if has_any_token(&lower, &["plan", "roadmap", "strategy", "steps", "schedule", "organize"]) {
        "planning".to_string()
    } else if contains_any_phrase(&lower, &["who am i"])
        || has_any_token(&lower, &["remember", "memory", "preference", "profile", "mike"])
    {
        "memory".to_string()
    } else if has_any_token(&lower, &["what", "who", "when", "where", "capital", "difference", "explain"]) {
        "factual".to_string()
    } else if has_any_token(&lower, &["research", "compare", "benchmark", "analyze", "evaluate"]) {
        "research".to_string()
    } else {
        "general".to_string()
    }
}

fn has_any_token(text: &str, tokens: &[&str]) -> bool {
    let words: Vec<&str> = text
        .split(|ch: char| !ch.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect();
    tokens.iter().any(|t| words.iter().any(|w| w == t))
}

fn contains_any_phrase(text: &str, phrases: &[&str]) -> bool {
    phrases.iter().any(|p| text.contains(p))
}

fn extract_tier_from_tags(tags: &str) -> Option<String> {
    tags.split(',')
        .find_map(|tag| tag.trim().strip_prefix("tier:").map(str::to_string))
}

fn extract_tier_from_learn_content(content: &str) -> Option<String> {
    let start = content.find('[')? + 1;
    let end = content[start..].find('→')? + start;
    Some(content[start..end].trim().to_string())
}

fn extract_question_from_learn(content: &str) -> String {
    let start = match content.find(" Q: ") {
        Some(idx) => idx + 4,
        None => return String::new(),
    };
    let tail = &content[start..];
    tail.split("\nA:").next().unwrap_or("").trim().to_string()
}

fn truncate_for_summary(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max_chars).collect();
    out.push_str("...");
    out
}

fn normalize_learning_telemetry(telemetry: &mut LearningTelemetry) {
    let mut normalized_tiers: BTreeMap<String, TierTelemetry> = BTreeMap::new();
    for (raw_tier, stats) in std::mem::take(&mut telemetry.tiers) {
        let canonical = normalize_tier_label(&raw_tier);
        merge_tier_telemetry(normalized_tiers.entry(canonical).or_default(), stats);
    }
    telemetry.tiers = normalized_tiers;

    let mut normalized_groups = BTreeMap::new();
    for (raw_key, count) in std::mem::take(&mut telemetry.consolidated_groups) {
        let canonical = normalize_group_key(&raw_key);
        let entry = normalized_groups.entry(canonical).or_insert(0);
        *entry = (*entry).max(count);
    }
    telemetry.consolidated_groups = normalized_groups;
}

fn merge_tier_telemetry(into: &mut TierTelemetry, from: TierTelemetry) {
    into.attempts += from.attempts;
    into.local_successes += from.local_successes;
    into.cloud_rescues += from.cloud_rescues;
    into.direct_cloud += from.direct_cloud;
    into.forced_requests += from.forced_requests;
    if from.last_seen_ts >= into.last_seen_ts {
        into.last_seen_ts = from.last_seen_ts;
        if !from.last_model_used.is_empty() {
            into.last_model_used = from.last_model_used.clone();
        }
    } else if into.last_model_used.is_empty() && !from.last_model_used.is_empty() {
        into.last_model_used = from.last_model_used.clone();
    }
    for (family, fam_stats) in from.families {
        merge_family_telemetry(into.families.entry(family).or_default(), fam_stats);
    }
}

fn merge_family_telemetry(into: &mut FamilyTelemetry, from: FamilyTelemetry) {
    into.attempts += from.attempts;
    into.local_successes += from.local_successes;
    into.cloud_rescues += from.cloud_rescues;
    into.direct_cloud += from.direct_cloud;
    into.forced_requests += from.forced_requests;
    if from.last_seen_ts >= into.last_seen_ts {
        into.last_seen_ts = from.last_seen_ts;
        if !from.last_model_used.is_empty() {
            into.last_model_used = from.last_model_used;
        }
    } else if into.last_model_used.is_empty() && !from.last_model_used.is_empty() {
        into.last_model_used = from.last_model_used;
    }
}

fn normalize_group_key(raw: &str) -> String {
    let mut parts = raw.splitn(2, '|');
    let tier = parts.next().unwrap_or("unknown");
    let family = parts.next().unwrap_or("general");
    format!("{}|{}", normalize_tier_label(tier), family)
}

fn normalize_tier_label(raw: &str) -> String {
    match raw.trim() {
        "qwen3.5:0.8b" | "tier3_local_0.8b" => RoutingTier::LocalFast.label().to_string(),
        "qwen3.5:9b" | "tier2_local_9b" => RoutingTier::LocalMid.label().to_string(),
        "qwen3.5:27b" | "tier2_code_27b" => RoutingTier::LocalCode.label().to_string(),
        "qwen2.5:0.5b" | "tier3_local_0.5b" => RoutingTier::LocalFast.label().to_string(),
        "qwen2.5:7b-instruct-q4_K_M" | "tier2_local_7b" => RoutingTier::LocalMid.label().to_string(),
        "qwen2.5-coder:32b-instruct-q5_K_M" | "tier2_code_32b" => RoutingTier::LocalCode.label().to_string(),
        "tier1_cloud" => RoutingTier::Cloud.label().to_string(),
        other => other.to_string(),
    }
}

/// FNV-1a 64-bit hash for exact content deduplication.
fn fnv64(s: &str) -> u64 {
    let mut hash: u64 = 14695981039346656037;
    for byte in s.as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(1099511628211);
    }
    hash
}
