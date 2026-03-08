use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::cosmic::{
    BehavioralBias, BeliefSource, EmotionalModulator, FreeEnergyState, SelfModel, WorldModel,
};

#[path = "life/emotional.rs"]
mod emotional;
pub use emotional::EmotionalState;

const PREFERENCE_EMA_ALPHA: f64 = 0.2;
const FREE_ENERGY_CAPACITY: usize = 200;
const MODEL_CAPACITY: usize = 100;
const HIGH_SURPRISE_THRESHOLD: f64 = 1.5;

const POSITIVE_WORDS: &[&str] = &[
    "great",
    "good",
    "awesome",
    "excellent",
    "thanks",
    "thank",
    "love",
    "perfect",
    "amazing",
    "wonderful",
    "fantastic",
    "happy",
    "nice",
    "cool",
    "brilliant",
    "yes",
    "agree",
    "helpful",
    "appreciate",
    "impressive",
];

const NEGATIVE_WORDS: &[&str] = &[
    "bad",
    "wrong",
    "terrible",
    "awful",
    "hate",
    "horrible",
    "poor",
    "worst",
    "ugly",
    "annoying",
    "broken",
    "fail",
    "error",
    "bug",
    "no",
    "disagree",
    "disappointed",
    "frustrating",
    "confused",
    "useless",
];

const FORMAL_WORDS: &[&str] = &[
    "please",
    "kindly",
    "would",
    "could",
    "shall",
    "regarding",
    "furthermore",
    "therefore",
    "however",
    "additionally",
];

const CASUAL_WORDS: &[&str] = &[
    "hey", "yo", "lol", "haha", "gonna", "wanna", "kinda", "yeah", "nah", "btw", "tbh", "imo",
];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserPreferences {
    pub formality: f64,
    pub verbosity: f64,
    pub topic_interests: HashMap<String, f64>,
    pub detected_language: Option<String>,
}

impl Default for UserPreferences {
    fn default() -> Self {
        Self {
            formality: 0.5,
            verbosity: 0.5,
            topic_interests: HashMap::new(),
            detected_language: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MessageType {
    Question,
    Statement,
    Command,
    Greeting,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Episode {
    pub message_type: MessageType,
    pub emotional_tags: Vec<String>,
    pub significance: f64,
    pub surprise: f64,
    pub topic: Option<String>,
}

#[derive(Debug, Clone)]
pub struct PreMessageContext {
    pub predicted_sentiment: f64,
    pub prediction_confidence: f32,
    pub current_free_energy: f64,
    pub behavioral_bias: BehavioralBias,
    pub mood_summary: String,
}

pub struct CognitiveProcessor {
    emotional: EmotionalState,
    free_energy: FreeEnergyState,
    self_model: SelfModel,
    world_model: WorldModel,
    modulator: EmotionalModulator,
    preferences: UserPreferences,
    turn_count: u64,
    save_interval: u64,
    persistence_path: String,
    last_prediction_id: Option<String>,
}

impl CognitiveProcessor {
    pub fn new(persistence_path: &str, save_interval: u64) -> Self {
        Self {
            emotional: EmotionalState::load_or_default(persistence_path),
            free_energy: FreeEnergyState::new(FREE_ENERGY_CAPACITY),
            self_model: SelfModel::new(MODEL_CAPACITY),
            world_model: WorldModel::new(MODEL_CAPACITY),
            modulator: EmotionalModulator::new(),
            preferences: UserPreferences::default(),
            turn_count: 0,
            save_interval,
            persistence_path: persistence_path.to_string(),
            last_prediction_id: None,
        }
    }

    pub fn pre_message(&mut self) -> PreMessageContext {
        let engagement = self
            .world_model
            .get_belief("world:user_engagement")
            .map_or(0.5, |b| b.value);
        let predicted_sentiment = f64::midpoint(engagement, f64::from(self.emotional.valence));
        #[allow(clippy::cast_possible_truncation)]
        let confidence = self
            .free_energy
            .accuracy("user_sentiment")
            .map_or(0.5_f32, |a| a as f32);

        let pred_id = self
            .free_energy
            .predict("user_sentiment", predicted_sentiment, confidence);
        self.last_prediction_id = Some(pred_id);

        PreMessageContext {
            predicted_sentiment,
            prediction_confidence: confidence,
            current_free_energy: self.free_energy.free_energy(),
            behavioral_bias: self.modulator.compute_bias(),
            mood_summary: self.emotional.mood_context(),
        }
    }

    pub fn post_message(&mut self, user_msg: &str, _response: &str) -> Episode {
        let sentiment = analyze_sentiment(user_msg);
        let surprise = self.observe_sentiment(sentiment);

        if surprise > HIGH_SURPRISE_THRESHOLD {
            self.modulator
                .apply_free_energy_signal(self.free_energy.free_energy(), surprise);
            #[allow(clippy::cast_possible_truncation)]
            let arousal_bump = (surprise * 0.1).min(0.3) as f32;
            self.emotional.arousal = (self.emotional.arousal + arousal_bump).min(1.0);
        }

        self.update_models(user_msg, sentiment, surprise);
        self.update_preferences(user_msg);

        let message_type = classify_message(user_msg);
        let emotional_tags = self.emotional_tags();
        let topic = extract_topic(user_msg);
        let significance =
            compute_significance(user_msg, topic.as_ref(), surprise, self.emotional.arousal);

        self.turn_count += 1;
        self.save_if_needed();

        Episode {
            message_type,
            emotional_tags,
            significance,
            surprise,
            topic,
        }
    }

    pub fn modulated_temperature(&self, base: f64) -> f64 {
        let emotional_temp = self.emotional.effective_temperature(base);
        let fe = self.free_energy.free_energy();
        let fe_mod = (fe * 0.05).min(0.15);
        (emotional_temp + fe_mod).clamp(0.1, 1.5)
    }

    pub fn mood_context(&self) -> String {
        let base = self.emotional.mood_context();
        let meta = self.self_model.metacognitive_accuracy();
        let div = self.self_model.divergence_from(&self.world_model);
        let fe = self.free_energy.free_energy();
        format!(
            "{base} Metacognitive accuracy: {meta:.0}%. \
             Self-world divergence: {div:.2}. Free energy: {fe:.2}."
        )
    }

    pub fn behavioral_bias(&self) -> BehavioralBias {
        self.modulator.compute_bias()
    }

    pub fn save_if_needed(&self) {
        if self.turn_count.is_multiple_of(self.save_interval) {
            self.emotional.save(&self.persistence_path);
        }
    }

    pub fn force_save(&self) {
        self.emotional.save(&self.persistence_path);
    }

    pub fn turn_count(&self) -> u64 {
        self.turn_count
    }

    pub fn preferences(&self) -> &UserPreferences {
        &self.preferences
    }

    pub fn free_energy(&self) -> f64 {
        self.free_energy.free_energy()
    }

    pub fn emotional_state(&self) -> &EmotionalState {
        &self.emotional
    }

    fn observe_sentiment(&mut self, sentiment: f64) -> f64 {
        if let Some(ref pred_id) = self.last_prediction_id.clone() {
            if let Some(error) = self.free_energy.observe(pred_id, sentiment) {
                return error.surprise;
            }
        }
        0.0
    }

    fn update_models(&mut self, user_msg: &str, sentiment: f64, surprise: f64) {
        let rapport = f64::midpoint(f64::from(self.emotional.trust), sentiment);
        self.self_model
            .update_belief("self:user_rapport", rapport, 0.7, BeliefSource::Observed);

        let stability = 1.0 - f64::from((self.emotional.valence - 0.5).abs());
        self.self_model.update_belief(
            "self:mood_stability",
            stability,
            0.8,
            BeliefSource::Observed,
        );

        let quality = if surprise < 1.0 { 0.8 } else { 0.5 };
        self.self_model.update_belief(
            "self:response_quality",
            quality,
            0.6,
            BeliefSource::Predicted,
        );

        let topic = extract_topic(user_msg).unwrap_or_else(|| "general".to_string());
        self.world_model.update_belief(
            "world:user_topic",
            topic_to_value(&topic),
            0.7,
            BeliefSource::Observed,
        );

        self.world_model.update_belief(
            "world:user_engagement",
            sentiment.clamp(0.0, 1.0),
            0.7,
            BeliefSource::Observed,
        );

        let word_count = user_msg.split_whitespace().count();
        let depth = (word_count as f64 / 100.0).min(1.0);
        self.world_model.update_belief(
            "world:conversation_depth",
            depth,
            0.6,
            BeliefSource::Observed,
        );
    }

    fn update_preferences(&mut self, user_msg: &str) {
        let lower = user_msg.to_lowercase();
        let words: Vec<&str> = lower.split_whitespace().collect();
        let total = words.len().max(1) as f64;

        let formal_count = words.iter().filter(|w| FORMAL_WORDS.contains(w)).count() as f64;
        let casual_count = words.iter().filter(|w| CASUAL_WORDS.contains(w)).count() as f64;
        let formality_signal = if formal_count + casual_count > 0.0 {
            formal_count / (formal_count + casual_count)
        } else {
            0.5
        };
        self.preferences.formality = ema(self.preferences.formality, formality_signal);

        let verbosity_signal = (total / 50.0).min(1.0);
        self.preferences.verbosity = ema(self.preferences.verbosity, verbosity_signal);

        if let Some(topic) = extract_topic(user_msg) {
            let current = self
                .preferences
                .topic_interests
                .get(&topic)
                .copied()
                .unwrap_or(0.0);
            let updated = ema(current, 1.0);
            self.preferences.topic_interests.insert(topic, updated);
        }

        for interest in self.preferences.topic_interests.values_mut() {
            *interest *= 0.99;
        }

        if words.iter().any(|w| {
            [
                "fn", "let", "mut", "impl", "struct", "pub", "async", "def", "class", "import",
                "const", "var", "function",
            ]
            .contains(w)
        }) {
            self.preferences.detected_language = Some("code".to_string());
        }
    }

    fn emotional_tags(&self) -> Vec<String> {
        let mut tags = Vec::new();
        if self.emotional.valence > 0.7 {
            tags.push("positive".to_string());
        } else if self.emotional.valence < -0.3 {
            tags.push("negative".to_string());
        }
        if self.emotional.arousal > 0.7 {
            tags.push("high_arousal".to_string());
        }
        if self.emotional.curiosity > 0.7 {
            tags.push("curious".to_string());
        }
        if self.emotional.trust > 0.8 {
            tags.push("high_trust".to_string());
        }
        if tags.is_empty() {
            tags.push("neutral".to_string());
        }
        tags
    }
}

fn analyze_sentiment(text: &str) -> f64 {
    let lower = text.to_lowercase();
    let words: Vec<&str> = lower.split_whitespace().collect();
    let total = words.len().max(1) as f64;

    let pos = words
        .iter()
        .filter(|w| POSITIVE_WORDS.contains(&w.trim_matches(|c: char| !c.is_alphabetic())))
        .count() as f64;
    let neg = words
        .iter()
        .filter(|w| NEGATIVE_WORDS.contains(&w.trim_matches(|c: char| !c.is_alphabetic())))
        .count() as f64;

    let raw = (pos - neg) / total;
    f64::midpoint(raw, 1.0).clamp(0.0, 1.0)
}

fn classify_message(text: &str) -> MessageType {
    let trimmed = text.trim();
    if trimmed.ends_with('?')
        || trimmed.to_lowercase().starts_with("what")
        || trimmed.to_lowercase().starts_with("how")
        || trimmed.to_lowercase().starts_with("why")
        || trimmed.to_lowercase().starts_with("when")
        || trimmed.to_lowercase().starts_with("where")
        || trimmed.to_lowercase().starts_with("who")
        || trimmed.to_lowercase().starts_with("can you")
        || trimmed.to_lowercase().starts_with("is ")
    {
        return MessageType::Question;
    }

    let lower = trimmed.to_lowercase();
    if lower.starts_with("hi")
        || lower.starts_with("hello")
        || lower.starts_with("hey")
        || lower.starts_with("good morning")
        || lower.starts_with("good evening")
        || lower.starts_with("greetings")
    {
        return MessageType::Greeting;
    }

    if lower.starts_with("do ")
        || lower.starts_with("run ")
        || lower.starts_with("create ")
        || lower.starts_with("make ")
        || lower.starts_with("build ")
        || lower.starts_with("fix ")
        || lower.starts_with("delete ")
        || lower.starts_with("show ")
        || lower.starts_with("list ")
        || lower.starts_with("please ")
    {
        return MessageType::Command;
    }

    MessageType::Statement
}

fn extract_topic(text: &str) -> Option<String> {
    let lower = text.to_lowercase();
    let topics = [
        ("rust", "rust"),
        ("python", "python"),
        ("javascript", "javascript"),
        ("typescript", "typescript"),
        ("docker", "docker"),
        ("kubernetes", "kubernetes"),
        ("database", "database"),
        ("sql", "database"),
        ("api", "api"),
        ("security", "security"),
        ("test", "testing"),
        ("deploy", "deployment"),
        ("ci/cd", "ci_cd"),
        ("git", "git"),
        ("linux", "linux"),
        ("network", "networking"),
        ("machine learning", "ml"),
        ("ai", "ai"),
        ("frontend", "frontend"),
        ("backend", "backend"),
    ];
    for (keyword, topic) in topics {
        if lower.contains(keyword) {
            return Some(topic.to_string());
        }
    }
    None
}

fn topic_to_value(topic: &str) -> f64 {
    let mut hash: u64 = 5381;
    for byte in topic.bytes() {
        hash = hash.wrapping_mul(33).wrapping_add(u64::from(byte));
    }
    (hash % 1000) as f64 / 1000.0
}

fn compute_significance(text: &str, topic: Option<&String>, surprise: f64, arousal: f32) -> f64 {
    let mut sig = 0.0;

    if text.contains('?') {
        sig += 0.3;
    }

    if topic.is_some() {
        sig += 0.2;
    }

    sig += (surprise * 0.2).min(0.3);

    sig += f64::from(arousal) * 0.2;

    sig.clamp(0.0, 1.0)
}

fn ema(current: f64, new_value: f64) -> f64 {
    PREFERENCE_EMA_ALPHA * new_value + (1.0 - PREFERENCE_EMA_ALPHA) * current
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_processor() -> CognitiveProcessor {
        CognitiveProcessor::new("/tmp/zeroclaw_test_cognitive.json", 10)
    }

    #[test]
    fn new_processor_has_default_state() {
        let cp = test_processor();
        assert_eq!(cp.turn_count, 0);
        assert_eq!(cp.save_interval, 10);
        assert!(cp.last_prediction_id.is_none());
    }

    #[test]
    fn pre_message_produces_context() {
        let mut cp = test_processor();
        let ctx = cp.pre_message();
        assert!((0.0..=1.0).contains(&ctx.predicted_sentiment));
        assert!((0.0..=1.0).contains(&ctx.prediction_confidence));
        assert!(!ctx.mood_summary.is_empty());
        assert!(cp.last_prediction_id.is_some());
    }

    #[test]
    fn post_message_increments_turn_count() {
        let mut cp = test_processor();
        cp.pre_message();
        cp.post_message("hello there", "hi");
        assert_eq!(cp.turn_count, 1);
    }

    #[test]
    fn post_message_returns_episode() {
        let mut cp = test_processor();
        cp.pre_message();
        let ep = cp.post_message("how does rust work?", "Rust is a systems language.");
        assert_eq!(ep.message_type, MessageType::Question);
        assert!((0.0..=1.0).contains(&ep.significance));
        assert!(ep.surprise >= 0.0);
        assert_eq!(ep.topic, Some("rust".to_string()));
    }

    #[test]
    fn sentiment_analysis_positive() {
        let s = analyze_sentiment("This is great and awesome!");
        assert!(s > 0.5, "positive sentiment: {s}");
    }

    #[test]
    fn sentiment_analysis_negative() {
        let s = analyze_sentiment("This is terrible and awful");
        assert!(s < 0.5, "negative sentiment: {s}");
    }

    #[test]
    fn sentiment_analysis_neutral() {
        let s = analyze_sentiment("The weather is cloudy today");
        assert!((s - 0.5).abs() < 0.2, "neutral sentiment: {s}");
    }

    #[test]
    fn classify_question() {
        assert_eq!(classify_message("What is Rust?"), MessageType::Question);
        assert_eq!(
            classify_message("How does this work?"),
            MessageType::Question
        );
        assert_eq!(classify_message("Something here?"), MessageType::Question);
    }

    #[test]
    fn classify_greeting() {
        assert_eq!(classify_message("Hello!"), MessageType::Greeting);
        assert_eq!(classify_message("Hey there"), MessageType::Greeting);
        assert_eq!(classify_message("Hi"), MessageType::Greeting);
    }

    #[test]
    fn classify_command() {
        assert_eq!(classify_message("Run the tests"), MessageType::Command);
        assert_eq!(classify_message("Create a new file"), MessageType::Command);
        assert_eq!(classify_message("Fix the bug"), MessageType::Command);
    }

    #[test]
    fn classify_statement() {
        assert_eq!(classify_message("Rust is fast"), MessageType::Statement);
        assert_eq!(classify_message("I think so"), MessageType::Statement);
    }

    #[test]
    fn extract_topic_matches() {
        assert_eq!(
            extract_topic("I love rust programming"),
            Some("rust".to_string())
        );
        assert_eq!(
            extract_topic("deploy to production"),
            Some("deployment".to_string())
        );
        assert_eq!(extract_topic("nothing relevant here"), None);
    }

    #[test]
    fn modulated_temperature_in_range() {
        let cp = test_processor();
        let temp = cp.modulated_temperature(0.7);
        assert!((0.1..=1.5).contains(&temp), "temp: {temp}");
    }

    #[test]
    fn modulated_temperature_increases_with_surprise() {
        let mut cp = test_processor();
        let base_temp = cp.modulated_temperature(0.7);
        for _ in 0..10 {
            let id = cp.free_energy.predict("test", 0.1, 0.9);
            cp.free_energy.observe(&id, 0.95);
        }
        let high_fe_temp = cp.modulated_temperature(0.7);
        assert!(
            high_fe_temp >= base_temp,
            "high FE temp {high_fe_temp} should be >= base {base_temp}"
        );
    }

    #[test]
    fn mood_context_enriched() {
        let cp = test_processor();
        let ctx = cp.mood_context();
        assert!(ctx.contains("Emotional state"));
        assert!(ctx.contains("Metacognitive accuracy"));
        assert!(ctx.contains("Free energy"));
    }

    #[test]
    fn behavioral_bias_in_range() {
        let cp = test_processor();
        let bias = cp.behavioral_bias();
        assert!((0.0..=1.0).contains(&bias.exploration_vs_exploitation));
        assert!((0.0..=1.0).contains(&bias.speed_vs_caution));
    }

    #[test]
    fn preferences_drift_limited() {
        let mut cp = test_processor();
        let initial_formality = cp.preferences.formality;
        cp.pre_message();
        cp.post_message("hey yo lol wanna chat btw", "sure");
        let after = cp.preferences.formality;
        let delta = (after - initial_formality).abs();
        assert!(
            delta < 0.25,
            "formality delta {delta} should be drift-limited"
        );
    }

    #[test]
    fn preferences_detect_code_language() {
        let mut cp = test_processor();
        cp.pre_message();
        cp.post_message("fn main() { let x = 5; }", "ok");
        assert_eq!(cp.preferences.detected_language, Some("code".to_string()));
    }

    #[test]
    fn topic_interests_accumulate() {
        let mut cp = test_processor();
        cp.pre_message();
        cp.post_message("I want to learn rust", "ok");
        cp.pre_message();
        cp.post_message("More about rust please", "ok");
        let interest = cp
            .preferences
            .topic_interests
            .get("rust")
            .copied()
            .unwrap_or(0.0);
        assert!(interest > 0.0, "rust interest: {interest}");
    }

    #[test]
    fn high_surprise_modulates_emotional_state() {
        let mut cp = test_processor();
        let initial_arousal = cp.emotional.arousal;
        for _ in 0..5 {
            let id = cp.free_energy.predict("user_sentiment", 0.1, 0.9);
            cp.last_prediction_id = Some(id);
        }
        cp.post_message(
            "This is absolutely amazing wonderful fantastic great awesome!",
            "thanks",
        );
        let after_arousal = cp.emotional.arousal;
        assert!(
            after_arousal >= initial_arousal,
            "arousal should not decrease: before={initial_arousal} after={after_arousal}"
        );
    }

    #[test]
    fn self_model_updated_after_post_message() {
        let mut cp = test_processor();
        cp.pre_message();
        cp.post_message("test message", "response");
        assert!(cp.self_model.get_belief("self:user_rapport").is_some());
        assert!(cp.self_model.get_belief("self:mood_stability").is_some());
        assert!(cp.self_model.get_belief("self:response_quality").is_some());
    }

    #[test]
    fn world_model_updated_after_post_message() {
        let mut cp = test_processor();
        cp.pre_message();
        cp.post_message("test message about rust", "response");
        assert!(cp.world_model.get_belief("world:user_engagement").is_some());
        assert!(cp
            .world_model
            .get_belief("world:conversation_depth")
            .is_some());
        assert!(cp.world_model.get_belief("world:user_topic").is_some());
    }

    #[test]
    fn episode_has_emotional_tags() {
        let mut cp = test_processor();
        cp.pre_message();
        let ep = cp.post_message("hello", "hi");
        assert!(!ep.emotional_tags.is_empty());
    }

    #[test]
    fn episode_significance_higher_for_questions() {
        let mut cp1 = test_processor();
        cp1.pre_message();
        let ep1 = cp1.post_message("What is Rust?", "A language");

        let mut cp2 = test_processor();
        cp2.pre_message();
        let ep2 = cp2.post_message("Rust is fast", "Yes");

        assert!(
            ep1.significance >= ep2.significance,
            "question sig {} should be >= statement sig {}",
            ep1.significance,
            ep2.significance
        );
    }

    #[test]
    fn ema_blends_values() {
        let result = ema(0.5, 1.0);
        let expected = 0.2 * 1.0 + 0.8 * 0.5;
        assert!((result - expected).abs() < f64::EPSILON);
    }

    #[test]
    fn ema_preserves_stability() {
        let mut val = 0.5;
        for _ in 0..3 {
            val = ema(val, 0.0);
        }
        assert!(val > 0.2, "EMA should not flip instantly: {val}");
    }

    #[test]
    fn multiple_turns_accumulate() {
        let mut cp = test_processor();
        for i in 0..5 {
            cp.pre_message();
            cp.post_message(&format!("message {i}"), "response");
        }
        assert_eq!(cp.turn_count, 5);
    }

    #[test]
    fn free_energy_accessor() {
        let cp = test_processor();
        assert!((cp.free_energy() - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn topic_to_value_deterministic() {
        let v1 = topic_to_value("rust");
        let v2 = topic_to_value("rust");
        assert!((v1 - v2).abs() < f64::EPSILON);
    }

    #[test]
    fn topic_to_value_in_range() {
        for topic in &["rust", "python", "test", "api", "security"] {
            let v = topic_to_value(topic);
            assert!(
                (0.0..=1.0).contains(&v),
                "topic {topic} value {v} out of range"
            );
        }
    }

    #[test]
    fn compute_significance_bounds() {
        let s = compute_significance("no question", None, 0.0, 0.0);
        assert!((0.0..=1.0).contains(&s));

        let topic = "rust".to_string();
        let s_max = compute_significance("what? about rust?", Some(&topic), 5.0, 1.0);
        assert!((0.0..=1.0).contains(&s_max));
    }
}
