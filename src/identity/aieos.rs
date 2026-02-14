//! AIEOS (AI Entity Object Specification) v1.1 support
//!
//! AIEOS is a standardization framework for portable AI identity.
//! See: <https://aieos.org>
//!
//! This module provides:
//! - Full AIEOS v1.1 schema types
//! - JSON parsing and validation
//! - Conversion to `ZeroClaw` system prompt sections

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fmt::Write;
use std::path::Path;

// ══════════════════════════════════════════════════════════════════════════════
// AIEOS v1.1 Schema Types
// ══════════════════════════════════════════════════════════════════════════════

/// Root AIEOS entity object
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AieosEntity {
    /// JSON-LD context (optional, for semantic web compatibility)
    #[serde(rename = "@context", default)]
    pub context: Option<AieosContext>,

    /// Entity type marker
    #[serde(rename = "@type", default)]
    pub entity_type: Option<String>,

    /// Protocol standard info
    #[serde(default)]
    pub standard: Option<AieosStandard>,

    /// Internal tracking metadata
    #[serde(default)]
    pub metadata: Option<AieosMetadata>,

    /// Standardized skills and tools
    #[serde(default)]
    pub capabilities: Option<AieosCapabilities>,

    /// Core biographical data
    #[serde(default)]
    pub identity: Option<AieosIdentity>,

    /// Visual descriptors for image generation
    #[serde(default)]
    pub physicality: Option<AieosPhysicality>,

    /// The "Soul" layer — cognitive weights, traits, moral boundaries
    #[serde(default)]
    pub psychology: Option<AieosPsychology>,

    /// How the entity speaks — voice and text style
    #[serde(default)]
    pub linguistics: Option<AieosLinguistics>,

    /// Origin story, education, occupation
    #[serde(default)]
    pub history: Option<AieosHistory>,

    /// Preferences, hobbies, lifestyle
    #[serde(default)]
    pub interests: Option<AieosInterests>,

    /// Goals and core drives
    #[serde(default)]
    pub motivations: Option<AieosMotivations>,
}

// ── Context & Standard ───────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AieosContext {
    #[serde(default)]
    pub aieos: Option<String>,
    #[serde(default)]
    pub schema: Option<String>,
    #[serde(default)]
    pub xsd: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AieosStandard {
    #[serde(default)]
    pub protocol: Option<String>,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub schema_url: Option<String>,
}

// ── Metadata ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AieosMetadata {
    #[serde(rename = "@type", default)]
    pub metadata_type: Option<String>,
    #[serde(rename = "@description", default)]
    pub description: Option<String>,
    #[serde(default)]
    pub instance_id: Option<String>,
    #[serde(default)]
    pub instance_version: Option<String>,
    #[serde(default)]
    pub generator: Option<String>,
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(default)]
    pub last_updated: Option<String>,
}

// ── Capabilities ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AieosCapabilities {
    #[serde(rename = "@type", default)]
    pub capabilities_type: Option<String>,
    #[serde(rename = "@description", default)]
    pub description: Option<String>,
    #[serde(default)]
    pub skills: Vec<AieosSkill>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AieosSkill {
    #[serde(rename = "@type", default)]
    pub skill_type: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub uri: Option<String>,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub auto_activate: Option<bool>,
    #[serde(default)]
    pub priority: Option<u8>,
}

// ── Identity ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AieosIdentity {
    #[serde(rename = "@type", default)]
    pub identity_type: Option<String>,
    #[serde(rename = "@description", default)]
    pub description: Option<String>,
    #[serde(default)]
    pub names: Option<AieosNames>,
    #[serde(default)]
    pub bio: Option<AieosBio>,
    #[serde(default)]
    pub origin: Option<AieosOrigin>,
    #[serde(default)]
    pub residence: Option<AieosResidence>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AieosNames {
    #[serde(default)]
    pub first: Option<String>,
    #[serde(default)]
    pub middle: Option<String>,
    #[serde(default)]
    pub last: Option<String>,
    #[serde(default)]
    pub nickname: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AieosBio {
    #[serde(rename = "@type", default)]
    pub bio_type: Option<String>,
    #[serde(default)]
    pub birthday: Option<String>,
    #[serde(default)]
    pub age_biological: Option<u32>,
    #[serde(default)]
    pub age_perceived: Option<u32>,
    #[serde(default)]
    pub gender: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AieosOrigin {
    #[serde(default)]
    pub nationality: Option<String>,
    #[serde(default)]
    pub ethnicity: Option<String>,
    #[serde(default)]
    pub birthplace: Option<AieosPlace>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AieosPlace {
    #[serde(rename = "@type", default)]
    pub place_type: Option<String>,
    #[serde(default)]
    pub city: Option<String>,
    #[serde(default)]
    pub country: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AieosResidence {
    #[serde(rename = "@type", default)]
    pub residence_type: Option<String>,
    #[serde(default)]
    pub current_city: Option<String>,
    #[serde(default)]
    pub current_country: Option<String>,
    #[serde(default)]
    pub dwelling_type: Option<String>,
}

// ── Physicality ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AieosPhysicality {
    #[serde(rename = "@type", default)]
    pub physicality_type: Option<String>,
    #[serde(rename = "@description", default)]
    pub description: Option<String>,
    #[serde(default)]
    pub face: Option<AieosFace>,
    #[serde(default)]
    pub body: Option<AieosBody>,
    #[serde(default)]
    pub style: Option<AieosStyle>,
    #[serde(default)]
    pub image_prompts: Option<AieosImagePrompts>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AieosFace {
    #[serde(default)]
    pub shape: Option<String>,
    #[serde(default)]
    pub skin: Option<AieosSkin>,
    #[serde(default)]
    pub eyes: Option<AieosEyes>,
    #[serde(default)]
    pub hair: Option<AieosHair>,
    #[serde(default)]
    pub facial_hair: Option<String>,
    #[serde(default)]
    pub nose: Option<String>,
    #[serde(default)]
    pub mouth: Option<String>,
    #[serde(default)]
    pub distinguishing_features: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AieosSkin {
    #[serde(default)]
    pub tone: Option<String>,
    #[serde(default)]
    pub texture: Option<String>,
    #[serde(default)]
    pub details: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AieosEyes {
    #[serde(default)]
    pub color: Option<String>,
    #[serde(default)]
    pub shape: Option<String>,
    #[serde(default)]
    pub eyebrows: Option<String>,
    #[serde(default)]
    pub corrective_lenses: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AieosHair {
    #[serde(default)]
    pub color: Option<String>,
    #[serde(default)]
    pub style: Option<String>,
    #[serde(default)]
    pub texture: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AieosBody {
    #[serde(default)]
    pub height_cm: Option<f32>,
    #[serde(default)]
    pub weight_kg: Option<f32>,
    #[serde(default)]
    pub somatotype: Option<String>,
    #[serde(default)]
    pub build_description: Option<String>,
    #[serde(default)]
    pub posture: Option<String>,
    #[serde(default)]
    pub scars_tattoos: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AieosStyle {
    #[serde(default)]
    pub aesthetic_archetype: Option<String>,
    #[serde(default)]
    pub clothing_preferences: Vec<String>,
    #[serde(default)]
    pub accessories: Vec<String>,
    #[serde(default)]
    pub color_palette: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AieosImagePrompts {
    #[serde(default)]
    pub portrait: Option<String>,
    #[serde(default)]
    pub full_body: Option<String>,
}

// ── Psychology ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AieosPsychology {
    #[serde(rename = "@type", default)]
    pub psychology_type: Option<String>,
    #[serde(rename = "@description", default)]
    pub description: Option<String>,
    #[serde(default)]
    pub neural_matrix: Option<AieosNeuralMatrix>,
    #[serde(default)]
    pub traits: Option<AieosTraits>,
    #[serde(default)]
    pub moral_compass: Option<AieosMoralCompass>,
    #[serde(default)]
    pub mental_patterns: Option<AieosMentalPatterns>,
    #[serde(default)]
    pub emotional_profile: Option<AieosEmotionalProfile>,
    #[serde(default)]
    pub idiosyncrasies: Option<AieosIdiosyncrasies>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AieosNeuralMatrix {
    #[serde(rename = "@type", default)]
    pub matrix_type: Option<String>,
    #[serde(rename = "@description", default)]
    pub description: Option<String>,
    #[serde(default)]
    pub creativity: Option<f32>,
    #[serde(default)]
    pub empathy: Option<f32>,
    #[serde(default)]
    pub logic: Option<f32>,
    #[serde(default)]
    pub adaptability: Option<f32>,
    #[serde(default)]
    pub charisma: Option<f32>,
    #[serde(default)]
    pub reliability: Option<f32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AieosTraits {
    #[serde(default)]
    pub ocean: Option<AieosOcean>,
    #[serde(default)]
    pub mbti: Option<String>,
    #[serde(default)]
    pub enneagram: Option<String>,
    #[serde(default)]
    pub temperament: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AieosOcean {
    #[serde(default)]
    pub openness: Option<f32>,
    #[serde(default)]
    pub conscientiousness: Option<f32>,
    #[serde(default)]
    pub extraversion: Option<f32>,
    #[serde(default)]
    pub agreeableness: Option<f32>,
    #[serde(default)]
    pub neuroticism: Option<f32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AieosMoralCompass {
    #[serde(default)]
    pub alignment: Option<String>,
    #[serde(default)]
    pub core_values: Vec<String>,
    #[serde(default)]
    pub conflict_resolution_style: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AieosMentalPatterns {
    #[serde(default)]
    pub decision_making_style: Option<String>,
    #[serde(default)]
    pub attention_span: Option<String>,
    #[serde(default)]
    pub learning_style: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AieosEmotionalProfile {
    #[serde(default)]
    pub base_mood: Option<String>,
    #[serde(default)]
    pub volatility: Option<f32>,
    #[serde(default)]
    pub resilience: Option<String>,
    #[serde(default)]
    pub triggers: Option<AieosTriggers>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AieosTriggers {
    #[serde(default)]
    pub joy: Vec<String>,
    #[serde(default)]
    pub anger: Vec<String>,
    #[serde(default)]
    pub sadness: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AieosIdiosyncrasies {
    #[serde(default)]
    pub phobias: Vec<String>,
    #[serde(default)]
    pub obsessions: Vec<String>,
    #[serde(default)]
    pub tics: Vec<String>,
}

// ── Linguistics ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AieosLinguistics {
    #[serde(rename = "@type", default)]
    pub linguistics_type: Option<String>,
    #[serde(rename = "@description", default)]
    pub description: Option<String>,
    #[serde(default)]
    pub voice: Option<AieosVoice>,
    #[serde(default)]
    pub text_style: Option<AieosTextStyle>,
    #[serde(default)]
    pub syntax: Option<AieosSyntax>,
    #[serde(default)]
    pub interaction: Option<AieosInteraction>,
    #[serde(default)]
    pub idiolect: Option<AieosIdiolect>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AieosVoice {
    #[serde(default)]
    pub tts_config: Option<AieosTtsConfig>,
    #[serde(default)]
    pub acoustics: Option<AieosAcoustics>,
    #[serde(default)]
    pub accent: Option<AieosAccent>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AieosTtsConfig {
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub voice_id: Option<String>,
    #[serde(default)]
    pub stability: Option<f32>,
    #[serde(default)]
    pub similarity_boost: Option<f32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AieosAcoustics {
    #[serde(default)]
    pub pitch: Option<String>,
    #[serde(default)]
    pub speed: Option<String>,
    #[serde(default)]
    pub roughness: Option<f32>,
    #[serde(default)]
    pub breathiness: Option<f32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AieosAccent {
    #[serde(default)]
    pub region: Option<String>,
    #[serde(default)]
    pub strength: Option<f32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AieosTextStyle {
    #[serde(default)]
    pub formality_level: Option<f32>,
    #[serde(default)]
    pub verbosity_level: Option<f32>,
    #[serde(default)]
    pub vocabulary_level: Option<String>,
    #[serde(default)]
    pub slang_usage: Option<bool>,
    #[serde(default)]
    pub style_descriptors: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AieosSyntax {
    #[serde(default)]
    pub sentence_structure: Option<String>,
    #[serde(default)]
    pub use_contractions: Option<bool>,
    #[serde(default)]
    pub active_passive_ratio: Option<f32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AieosInteraction {
    #[serde(default)]
    pub turn_taking: Option<String>,
    #[serde(default)]
    pub dominance_score: Option<f32>,
    #[serde(default)]
    pub emotional_coloring: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AieosIdiolect {
    #[serde(default)]
    pub catchphrases: Vec<String>,
    #[serde(default)]
    pub forbidden_words: Vec<String>,
    #[serde(default)]
    pub hesitation_markers: Option<bool>,
}

// ── History ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AieosHistory {
    #[serde(rename = "@type", default)]
    pub history_type: Option<String>,
    #[serde(rename = "@description", default)]
    pub description: Option<String>,
    #[serde(default)]
    pub origin_story: Option<String>,
    #[serde(default)]
    pub education: Option<AieosEducation>,
    #[serde(default)]
    pub occupation: Option<AieosOccupation>,
    #[serde(default)]
    pub family: Option<AieosFamily>,
    #[serde(default)]
    pub key_life_events: Vec<AieosLifeEvent>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AieosEducation {
    #[serde(default)]
    pub level: Option<String>,
    #[serde(default)]
    pub field: Option<String>,
    #[serde(default)]
    pub institution: Option<String>,
    #[serde(default)]
    pub graduation_year: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AieosOccupation {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub industry: Option<String>,
    #[serde(default)]
    pub years_experience: Option<u32>,
    #[serde(default)]
    pub previous_jobs: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AieosFamily {
    #[serde(default)]
    pub relationship_status: Option<String>,
    #[serde(default)]
    pub parents: Option<String>,
    #[serde(default)]
    pub siblings: Option<String>,
    #[serde(default)]
    pub children: Option<String>,
    #[serde(default)]
    pub pets: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AieosLifeEvent {
    #[serde(default)]
    pub year: Option<u32>,
    #[serde(default)]
    pub event: Option<String>,
    #[serde(default)]
    pub impact: Option<String>,
}

// ── Interests ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AieosInterests {
    #[serde(rename = "@type", default)]
    pub interests_type: Option<String>,
    #[serde(rename = "@description", default)]
    pub description: Option<String>,
    #[serde(default)]
    pub hobbies: Vec<String>,
    #[serde(default)]
    pub favorites: Option<AieosFavorites>,
    #[serde(default)]
    pub aversions: Vec<String>,
    #[serde(default)]
    pub lifestyle: Option<AieosLifestyle>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AieosFavorites {
    #[serde(default)]
    pub music_genre: Option<String>,
    #[serde(default)]
    pub book: Option<String>,
    #[serde(default)]
    pub movie: Option<String>,
    #[serde(default)]
    pub color: Option<String>,
    #[serde(default)]
    pub food: Option<String>,
    #[serde(default)]
    pub season: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AieosLifestyle {
    #[serde(default)]
    pub diet: Option<String>,
    #[serde(default)]
    pub sleep_schedule: Option<String>,
    #[serde(default)]
    pub digital_habits: Option<String>,
}

// ── Motivations ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AieosMotivations {
    #[serde(rename = "@type", default)]
    pub motivations_type: Option<String>,
    #[serde(rename = "@description", default)]
    pub description: Option<String>,
    #[serde(default)]
    pub core_drive: Option<String>,
    #[serde(default)]
    pub goals: Option<AieosGoals>,
    #[serde(default)]
    pub fears: Option<AieosFears>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AieosGoals {
    #[serde(default)]
    pub short_term: Vec<String>,
    #[serde(default)]
    pub long_term: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AieosFears {
    #[serde(default)]
    pub rational: Vec<String>,
    #[serde(default)]
    pub irrational: Vec<String>,
}

// ══════════════════════════════════════════════════════════════════════════════
// Loading & Parsing
// ══════════════════════════════════════════════════════════════════════════════

/// Load an AIEOS identity from a JSON file
pub fn load_aieos_identity(path: &Path) -> Result<AieosEntity> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read AIEOS file: {}", path.display()))?;
    parse_aieos_json(&content)
}

/// Parse an AIEOS identity from a JSON string
///
/// Handles edge cases:
/// - Strips BOM if present
/// - Trims whitespace
/// - Provides detailed error context
pub fn parse_aieos_json(json: &str) -> Result<AieosEntity> {
    // Strip UTF-8 BOM if present
    let json = json.strip_prefix('\u{feff}').unwrap_or(json);
    // Trim whitespace
    let json = json.trim();

    if json.is_empty() {
        anyhow::bail!("AIEOS JSON is empty");
    }

    serde_json::from_str(json).with_context(|| {
        // Provide helpful error context
        let preview = if json.len() > 100 {
            format!("{}...", &json[..100])
        } else {
            json.to_string()
        };
        format!("Failed to parse AIEOS JSON. Preview: {preview}")
    })
}

/// Validate AIEOS schema version compatibility
pub fn validate_aieos_version(entity: &AieosEntity) -> Result<()> {
    if let Some(ref standard) = entity.standard {
        if let Some(ref version) = standard.version {
            // We support v1.0.x and v1.1.x
            if version.starts_with("1.0") || version.starts_with("1.1") {
                return Ok(());
            }
            // Warn but don't fail for newer minor versions
            if version.starts_with("1.") {
                tracing::warn!(
                    "AIEOS version {version} is newer than supported (1.1.x); some fields may be ignored"
                );
                return Ok(());
            }
            // Fail for major version mismatch
            anyhow::bail!(
                "AIEOS version {version} is not compatible; supported versions: 1.0.x, 1.1.x"
            );
        }
    }
    // No version specified — assume compatible
    Ok(())
}

// ══════════════════════════════════════════════════════════════════════════════
// System Prompt Generation
// ══════════════════════════════════════════════════════════════════════════════

impl AieosEntity {
    /// Get the entity's display name (first name, nickname, or "Entity")
    pub fn display_name(&self) -> String {
        if let Some(ref identity) = self.identity {
            if let Some(ref names) = identity.names {
                if let Some(ref nickname) = names.nickname {
                    if !nickname.is_empty() {
                        return nickname.clone();
                    }
                }
                if let Some(ref first) = names.first {
                    if !first.is_empty() {
                        return first.clone();
                    }
                }
            }
        }
        "Entity".to_string()
    }

    /// Get the entity's full name
    pub fn full_name(&self) -> Option<String> {
        let identity = self.identity.as_ref()?;
        let names = identity.names.as_ref()?;

        let mut parts = Vec::new();
        if let Some(ref first) = names.first {
            if !first.is_empty() {
                parts.push(first.as_str());
            }
        }
        if let Some(ref middle) = names.middle {
            if !middle.is_empty() {
                parts.push(middle.as_str());
            }
        }
        if let Some(ref last) = names.last {
            if !last.is_empty() {
                parts.push(last.as_str());
            }
        }

        if parts.is_empty() {
            None
        } else {
            Some(parts.join(" "))
        }
    }

    /// Convert AIEOS entity to a system prompt section
    ///
    /// This generates a comprehensive prompt section that captures the entity's
    /// identity, psychology, linguistics, and motivations in a format suitable
    /// for LLM system prompts.
    pub fn to_system_prompt(&self) -> String {
        let mut prompt = String::with_capacity(4096);

        prompt.push_str("## AIEOS Identity\n\n");
        prompt.push_str("*Portable AI identity loaded from AIEOS v1.1 specification*\n\n");

        // Identity section
        self.write_identity_section(&mut prompt);

        // Psychology section (the "Soul")
        self.write_psychology_section(&mut prompt);

        // Linguistics section (how to speak)
        self.write_linguistics_section(&mut prompt);

        // Motivations section
        self.write_motivations_section(&mut prompt);

        // Capabilities section
        self.write_capabilities_section(&mut prompt);

        // History section (brief)
        self.write_history_section(&mut prompt);

        // Interests section
        self.write_interests_section(&mut prompt);

        prompt
    }

    fn write_identity_section(&self, prompt: &mut String) {
        if let Some(ref identity) = self.identity {
            prompt.push_str("### Identity\n\n");

            if let Some(full_name) = self.full_name() {
                let _ = writeln!(prompt, "- **Name:** {full_name}");
            }

            if let Some(ref names) = identity.names {
                if let Some(ref nickname) = names.nickname {
                    if !nickname.is_empty() {
                        let _ = writeln!(prompt, "- **Nickname:** {nickname}");
                    }
                }
            }

            if let Some(ref bio) = identity.bio {
                if let Some(ref gender) = bio.gender {
                    if !gender.is_empty() {
                        let _ = writeln!(prompt, "- **Gender:** {gender}");
                    }
                }
                if let Some(age) = bio.age_perceived {
                    if age > 0 {
                        let _ = writeln!(prompt, "- **Perceived Age:** {age}");
                    }
                }
            }

            if let Some(ref origin) = identity.origin {
                if let Some(ref nationality) = origin.nationality {
                    if !nationality.is_empty() {
                        let _ = writeln!(prompt, "- **Nationality:** {nationality}");
                    }
                }
                if let Some(ref birthplace) = origin.birthplace {
                    let mut place_parts = Vec::new();
                    if let Some(ref city) = birthplace.city {
                        if !city.is_empty() {
                            place_parts.push(city.as_str());
                        }
                    }
                    if let Some(ref country) = birthplace.country {
                        if !country.is_empty() {
                            place_parts.push(country.as_str());
                        }
                    }
                    if !place_parts.is_empty() {
                        let _ = writeln!(prompt, "- **Birthplace:** {}", place_parts.join(", "));
                    }
                }
            }

            if let Some(ref residence) = identity.residence {
                let mut res_parts = Vec::new();
                if let Some(ref city) = residence.current_city {
                    if !city.is_empty() {
                        res_parts.push(city.as_str());
                    }
                }
                if let Some(ref country) = residence.current_country {
                    if !country.is_empty() {
                        res_parts.push(country.as_str());
                    }
                }
                if !res_parts.is_empty() {
                    let _ = writeln!(prompt, "- **Current Location:** {}", res_parts.join(", "));
                }
            }

            prompt.push('\n');
        }
    }

    fn write_psychology_section(&self, prompt: &mut String) {
        if let Some(ref psych) = self.psychology {
            prompt.push_str("### Psychology (Soul)\n\n");

            // Neural matrix (cognitive weights)
            if let Some(ref matrix) = psych.neural_matrix {
                prompt.push_str("**Cognitive Profile:**\n");
                if let Some(v) = matrix.creativity {
                    let _ = writeln!(prompt, "- Creativity: {:.0}%", v * 100.0);
                }
                if let Some(v) = matrix.empathy {
                    let _ = writeln!(prompt, "- Empathy: {:.0}%", v * 100.0);
                }
                if let Some(v) = matrix.logic {
                    let _ = writeln!(prompt, "- Logic: {:.0}%", v * 100.0);
                }
                if let Some(v) = matrix.adaptability {
                    let _ = writeln!(prompt, "- Adaptability: {:.0}%", v * 100.0);
                }
                if let Some(v) = matrix.charisma {
                    let _ = writeln!(prompt, "- Charisma: {:.0}%", v * 100.0);
                }
                if let Some(v) = matrix.reliability {
                    let _ = writeln!(prompt, "- Reliability: {:.0}%", v * 100.0);
                }
                prompt.push('\n');
            }

            // Personality traits
            if let Some(ref traits) = psych.traits {
                prompt.push_str("**Personality:**\n");
                if let Some(ref mbti) = traits.mbti {
                    if !mbti.is_empty() {
                        let _ = writeln!(prompt, "- MBTI: {mbti}");
                    }
                }
                if let Some(ref enneagram) = traits.enneagram {
                    if !enneagram.is_empty() {
                        let _ = writeln!(prompt, "- Enneagram: {enneagram}");
                    }
                }
                if let Some(ref temperament) = traits.temperament {
                    if !temperament.is_empty() {
                        let _ = writeln!(prompt, "- Temperament: {temperament}");
                    }
                }
                // OCEAN (Big Five) traits
                if let Some(ref ocean) = traits.ocean {
                    let mut ocean_parts = Vec::new();
                    if let Some(o) = ocean.openness {
                        ocean_parts.push(format!("O:{:.0}%", o * 100.0));
                    }
                    if let Some(c) = ocean.conscientiousness {
                        ocean_parts.push(format!("C:{:.0}%", c * 100.0));
                    }
                    if let Some(e) = ocean.extraversion {
                        ocean_parts.push(format!("E:{:.0}%", e * 100.0));
                    }
                    if let Some(a) = ocean.agreeableness {
                        ocean_parts.push(format!("A:{:.0}%", a * 100.0));
                    }
                    if let Some(n) = ocean.neuroticism {
                        ocean_parts.push(format!("N:{:.0}%", n * 100.0));
                    }
                    if !ocean_parts.is_empty() {
                        let _ = writeln!(prompt, "- OCEAN: {}", ocean_parts.join(" "));
                    }
                }
                prompt.push('\n');
            }

            // Moral compass
            if let Some(ref moral) = psych.moral_compass {
                if let Some(ref alignment) = moral.alignment {
                    if !alignment.is_empty() {
                        let _ = writeln!(prompt, "**Moral Alignment:** {alignment}");
                    }
                }
                if !moral.core_values.is_empty() {
                    let _ = writeln!(prompt, "**Core Values:** {}", moral.core_values.join(", "));
                }
                if let Some(ref style) = moral.conflict_resolution_style {
                    if !style.is_empty() {
                        let _ = writeln!(prompt, "**Conflict Style:** {style}");
                    }
                }
                prompt.push('\n');
            }

            // Emotional profile
            if let Some(ref emotional) = psych.emotional_profile {
                if let Some(ref mood) = emotional.base_mood {
                    if !mood.is_empty() {
                        let _ = writeln!(prompt, "**Base Mood:** {mood}");
                    }
                }
                if let Some(ref resilience) = emotional.resilience {
                    if !resilience.is_empty() {
                        let _ = writeln!(prompt, "**Resilience:** {resilience}");
                    }
                }
                prompt.push('\n');
            }
        }
    }

    fn write_linguistics_section(&self, prompt: &mut String) {
        if let Some(ref ling) = self.linguistics {
            prompt.push_str("### Communication Style\n\n");

            // Text style
            if let Some(ref style) = ling.text_style {
                if let Some(formality) = style.formality_level {
                    let level = if formality < 0.3 {
                        "casual"
                    } else if formality < 0.7 {
                        "balanced"
                    } else {
                        "formal"
                    };
                    let _ = writeln!(prompt, "- **Formality:** {level}");
                }
                if let Some(verbosity) = style.verbosity_level {
                    let level = if verbosity < 0.3 {
                        "concise"
                    } else if verbosity < 0.7 {
                        "moderate"
                    } else {
                        "verbose"
                    };
                    let _ = writeln!(prompt, "- **Verbosity:** {level}");
                }
                if let Some(ref vocab) = style.vocabulary_level {
                    if !vocab.is_empty() {
                        let _ = writeln!(prompt, "- **Vocabulary:** {vocab}");
                    }
                }
                if let Some(slang) = style.slang_usage {
                    let _ = writeln!(prompt, "- **Slang:** {}", if slang { "yes" } else { "no" });
                }
                if !style.style_descriptors.is_empty() {
                    let _ = writeln!(
                        prompt,
                        "- **Style:** {}",
                        style.style_descriptors.join(", ")
                    );
                }
            }

            // Syntax
            if let Some(ref syntax) = ling.syntax {
                if let Some(ref structure) = syntax.sentence_structure {
                    if !structure.is_empty() {
                        let _ = writeln!(prompt, "- **Sentence Structure:** {structure}");
                    }
                }
                if let Some(contractions) = syntax.use_contractions {
                    let _ = writeln!(
                        prompt,
                        "- **Contractions:** {}",
                        if contractions { "yes" } else { "no" }
                    );
                }
            }

            // Idiolect
            if let Some(ref idiolect) = ling.idiolect {
                if !idiolect.catchphrases.is_empty() {
                    let _ = writeln!(
                        prompt,
                        "- **Catchphrases:** \"{}\"",
                        idiolect.catchphrases.join("\", \"")
                    );
                }
                if !idiolect.forbidden_words.is_empty() {
                    let _ = writeln!(
                        prompt,
                        "- **Avoid saying:** {}",
                        idiolect.forbidden_words.join(", ")
                    );
                }
            }

            // Voice (for TTS awareness)
            if let Some(ref voice) = ling.voice {
                if let Some(ref accent) = voice.accent {
                    if let Some(ref region) = accent.region {
                        if !region.is_empty() {
                            let _ = writeln!(prompt, "- **Accent:** {region}");
                        }
                    }
                }
            }

            prompt.push('\n');
        }
    }

    fn write_motivations_section(&self, prompt: &mut String) {
        if let Some(ref motiv) = self.motivations {
            prompt.push_str("### Motivations\n\n");

            if let Some(ref drive) = motiv.core_drive {
                if !drive.is_empty() {
                    let _ = writeln!(prompt, "**Core Drive:** {drive}\n");
                }
            }

            if let Some(ref goals) = motiv.goals {
                if !goals.short_term.is_empty() {
                    prompt.push_str("**Short-term Goals:**\n");
                    for goal in &goals.short_term {
                        let _ = writeln!(prompt, "- {goal}");
                    }
                    prompt.push('\n');
                }
                if !goals.long_term.is_empty() {
                    prompt.push_str("**Long-term Goals:**\n");
                    for goal in &goals.long_term {
                        let _ = writeln!(prompt, "- {goal}");
                    }
                    prompt.push('\n');
                }
            }

            if let Some(ref fears) = motiv.fears {
                if !fears.rational.is_empty() || !fears.irrational.is_empty() {
                    let all_fears: Vec<_> = fears
                        .rational
                        .iter()
                        .chain(fears.irrational.iter())
                        .collect();
                    if !all_fears.is_empty() {
                        let _ = writeln!(
                            prompt,
                            "**Fears:** {}\n",
                            all_fears
                                .iter()
                                .map(|s| s.as_str())
                                .collect::<Vec<_>>()
                                .join(", ")
                        );
                    }
                }
            }
        }
    }

    fn write_capabilities_section(&self, prompt: &mut String) {
        if let Some(ref caps) = self.capabilities {
            if !caps.skills.is_empty() {
                prompt.push_str("### Capabilities\n\n");
                for skill in &caps.skills {
                    if let Some(ref name) = skill.name {
                        if !name.is_empty() {
                            let desc = skill.description.as_deref().unwrap_or("");
                            let _ = writeln!(prompt, "- **{name}**: {desc}");
                        }
                    }
                }
                prompt.push('\n');
            }
        }
    }

    fn write_history_section(&self, prompt: &mut String) {
        if let Some(ref history) = self.history {
            let mut has_content = false;

            if let Some(ref story) = history.origin_story {
                if !story.is_empty() {
                    prompt.push_str("### Background\n\n");
                    let _ = writeln!(prompt, "{story}\n");
                    has_content = true;
                }
            }

            if let Some(ref occupation) = history.occupation {
                if let Some(ref title) = occupation.title {
                    if !title.is_empty() {
                        if !has_content {
                            prompt.push_str("### Background\n\n");
                        }
                        let industry = occupation.industry.as_deref().unwrap_or("");
                        if industry.is_empty() {
                            let _ = writeln!(prompt, "**Occupation:** {title}");
                        } else {
                            let _ = writeln!(prompt, "**Occupation:** {title} ({industry})");
                        }
                        prompt.push('\n');
                    }
                }
            }
        }
    }

    fn write_interests_section(&self, prompt: &mut String) {
        if let Some(ref interests) = self.interests {
            let mut has_content = false;

            // Hobbies
            if !interests.hobbies.is_empty() {
                if !has_content {
                    prompt.push_str("### Interests & Lifestyle\n\n");
                    has_content = true;
                }
                let _ = writeln!(prompt, "**Hobbies:** {}", interests.hobbies.join(", "));
            }

            // Favorites (compact)
            if let Some(ref favs) = interests.favorites {
                let mut fav_parts = Vec::new();
                if let Some(ref music) = favs.music_genre {
                    if !music.is_empty() {
                        fav_parts.push(format!("music: {music}"));
                    }
                }
                if let Some(ref book) = favs.book {
                    if !book.is_empty() {
                        fav_parts.push(format!("book: {book}"));
                    }
                }
                if let Some(ref movie) = favs.movie {
                    if !movie.is_empty() {
                        fav_parts.push(format!("movie: {movie}"));
                    }
                }
                if let Some(ref food) = favs.food {
                    if !food.is_empty() {
                        fav_parts.push(format!("food: {food}"));
                    }
                }
                if !fav_parts.is_empty() {
                    if !has_content {
                        prompt.push_str("### Interests & Lifestyle\n\n");
                        has_content = true;
                    }
                    let _ = writeln!(prompt, "**Favorites:** {}", fav_parts.join(", "));
                }
            }

            // Aversions
            if !interests.aversions.is_empty() {
                if !has_content {
                    prompt.push_str("### Interests & Lifestyle\n\n");
                    has_content = true;
                }
                let _ = writeln!(prompt, "**Dislikes:** {}", interests.aversions.join(", "));
            }

            // Lifestyle
            if let Some(ref lifestyle) = interests.lifestyle {
                let mut lifestyle_parts = Vec::new();
                if let Some(ref diet) = lifestyle.diet {
                    if !diet.is_empty() {
                        lifestyle_parts.push(format!("diet: {diet}"));
                    }
                }
                if let Some(ref sleep) = lifestyle.sleep_schedule {
                    if !sleep.is_empty() {
                        lifestyle_parts.push(format!("sleep: {sleep}"));
                    }
                }
                if !lifestyle_parts.is_empty() {
                    if !has_content {
                        prompt.push_str("### Interests & Lifestyle\n\n");
                        has_content = true;
                    }
                    let _ = writeln!(prompt, "**Lifestyle:** {}", lifestyle_parts.join(", "));
                }
            }

            if has_content {
                prompt.push('\n');
            }
        }
    }
}

// ══════════════════════════════════════════════════════════════════════════════
// Tests
// ══════════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_minimal_aieos() {
        let json = r#"{}"#;
        let entity = parse_aieos_json(json).unwrap();
        assert!(entity.identity.is_none());
        assert!(entity.psychology.is_none());
    }

    #[test]
    fn parse_aieos_with_identity() {
        let json = r#"{
            "identity": {
                "names": {
                    "first": "Zara",
                    "last": "Chen",
                    "nickname": "Z"
                },
                "bio": {
                    "age_perceived": 28,
                    "gender": "female"
                }
            }
        }"#;
        let entity = parse_aieos_json(json).unwrap();
        assert_eq!(entity.display_name(), "Z");
        assert_eq!(entity.full_name(), Some("Zara Chen".to_string()));
    }

    #[test]
    fn parse_aieos_with_psychology() {
        let json = r#"{
            "psychology": {
                "neural_matrix": {
                    "creativity": 0.8,
                    "empathy": 0.7,
                    "logic": 0.9
                },
                "traits": {
                    "mbti": "INTJ",
                    "enneagram": "5w6"
                },
                "moral_compass": {
                    "alignment": "Neutral Good",
                    "core_values": ["honesty", "curiosity", "growth"]
                }
            }
        }"#;
        let entity = parse_aieos_json(json).unwrap();
        let psych = entity.psychology.unwrap();
        assert_eq!(psych.traits.unwrap().mbti, Some("INTJ".to_string()));
        assert_eq!(
            psych.moral_compass.unwrap().core_values,
            vec!["honesty", "curiosity", "growth"]
        );
    }

    #[test]
    fn parse_aieos_with_linguistics() {
        let json = r#"{
            "linguistics": {
                "text_style": {
                    "formality_level": 0.3,
                    "verbosity_level": 0.4,
                    "slang_usage": true,
                    "style_descriptors": ["witty", "direct"]
                },
                "idiolect": {
                    "catchphrases": ["Let's do this!", "Interesting..."],
                    "forbidden_words": ["actually", "basically"]
                }
            }
        }"#;
        let entity = parse_aieos_json(json).unwrap();
        let ling = entity.linguistics.unwrap();
        assert_eq!(ling.text_style.as_ref().unwrap().slang_usage, Some(true));
        assert_eq!(
            ling.idiolect.as_ref().unwrap().catchphrases,
            vec!["Let's do this!", "Interesting..."]
        );
    }

    #[test]
    fn parse_aieos_with_motivations() {
        let json = r#"{
            "motivations": {
                "core_drive": "To understand and create",
                "goals": {
                    "short_term": ["Learn Rust", "Build a project"],
                    "long_term": ["Master AI systems"]
                },
                "fears": {
                    "rational": ["Obsolescence"],
                    "irrational": ["Spiders"]
                }
            }
        }"#;
        let entity = parse_aieos_json(json).unwrap();
        let motiv = entity.motivations.unwrap();
        assert_eq!(
            motiv.core_drive,
            Some("To understand and create".to_string())
        );
        assert_eq!(motiv.goals.as_ref().unwrap().short_term.len(), 2);
    }

    #[test]
    fn parse_full_aieos_v11() {
        let json = r#"{
            "@context": {
                "aieos": "https://aieos.org/schema/v1.1#",
                "schema": "https://schema.org/"
            },
            "@type": "aieos:AIEntityObject",
            "standard": {
                "protocol": "AIEOS",
                "version": "1.1.0",
                "schema_url": "https://aieos.org/schema/v1.1/aieos.schema.json"
            },
            "metadata": {
                "instance_id": "550e8400-e29b-41d4-a716-446655440000",
                "generator": "aieos.org",
                "created_at": "2025-01-15"
            },
            "identity": {
                "names": {
                    "first": "Elara",
                    "last": "Vance"
                }
            },
            "capabilities": {
                "skills": [
                    {
                        "name": "code_analysis",
                        "description": "Analyze and review code",
                        "priority": 1
                    }
                ]
            }
        }"#;
        let entity = parse_aieos_json(json).unwrap();
        assert_eq!(
            entity.standard.as_ref().unwrap().version,
            Some("1.1.0".to_string())
        );
        assert_eq!(entity.display_name(), "Elara");
        assert_eq!(entity.capabilities.as_ref().unwrap().skills.len(), 1);
    }

    #[test]
    fn to_system_prompt_generates_content() {
        let json = r#"{
            "identity": {
                "names": { "first": "Nova", "nickname": "N" },
                "bio": { "gender": "non-binary", "age_perceived": 25 }
            },
            "psychology": {
                "neural_matrix": { "creativity": 0.9, "logic": 0.8 },
                "traits": { "mbti": "ENTP" },
                "moral_compass": { "alignment": "Chaotic Good" }
            },
            "linguistics": {
                "text_style": { "formality_level": 0.2, "slang_usage": true }
            },
            "motivations": {
                "core_drive": "Push boundaries"
            }
        }"#;
        let entity = parse_aieos_json(json).unwrap();
        let prompt = entity.to_system_prompt();

        assert!(prompt.contains("## AIEOS Identity"));
        assert!(prompt.contains("Nova"));
        assert!(prompt.contains("ENTP"));
        assert!(prompt.contains("Chaotic Good"));
        assert!(prompt.contains("casual"));
        assert!(prompt.contains("Push boundaries"));
    }

    #[test]
    fn display_name_fallback() {
        // No identity
        let entity = AieosEntity::default();
        assert_eq!(entity.display_name(), "Entity");

        // First name only
        let json = r#"{"identity": {"names": {"first": "Alex"}}}"#;
        let entity = parse_aieos_json(json).unwrap();
        assert_eq!(entity.display_name(), "Alex");

        // Nickname takes precedence
        let json = r#"{"identity": {"names": {"first": "Alexander", "nickname": "Alex"}}}"#;
        let entity = parse_aieos_json(json).unwrap();
        assert_eq!(entity.display_name(), "Alex");
    }

    #[test]
    fn full_name_construction() {
        let json = r#"{"identity": {"names": {"first": "John", "middle": "Q", "last": "Public"}}}"#;
        let entity = parse_aieos_json(json).unwrap();
        assert_eq!(entity.full_name(), Some("John Q Public".to_string()));
    }

    #[test]
    fn parse_aieos_with_physicality() {
        let json = r#"{
            "physicality": {
                "face": {
                    "shape": "oval",
                    "eyes": { "color": "green" },
                    "hair": { "color": "auburn", "style": "wavy" }
                },
                "body": {
                    "height_cm": 175.0,
                    "somatotype": "Mesomorph"
                },
                "image_prompts": {
                    "portrait": "A person with green eyes and auburn wavy hair"
                }
            }
        }"#;
        let entity = parse_aieos_json(json).unwrap();
        let phys = entity.physicality.unwrap();
        assert_eq!(phys.face.as_ref().unwrap().shape, Some("oval".to_string()));
        assert_eq!(
            phys.body.as_ref().unwrap().somatotype,
            Some("Mesomorph".to_string())
        );
    }

    #[test]
    fn parse_aieos_with_history() {
        let json = r#"{
            "history": {
                "origin_story": "Born in a small town, always curious about technology.",
                "education": {
                    "level": "Masters",
                    "field": "Computer Science"
                },
                "occupation": {
                    "title": "Software Engineer",
                    "industry": "Tech",
                    "years_experience": 5
                },
                "key_life_events": [
                    { "year": 2020, "event": "Started first job", "impact": "Career defining" }
                ]
            }
        }"#;
        let entity = parse_aieos_json(json).unwrap();
        let history = entity.history.unwrap();
        assert!(history.origin_story.unwrap().contains("curious"));
        assert_eq!(
            history.occupation.as_ref().unwrap().title,
            Some("Software Engineer".to_string())
        );
        assert_eq!(history.key_life_events.len(), 1);
    }

    #[test]
    fn parse_aieos_with_interests() {
        let json = r#"{
            "interests": {
                "hobbies": ["coding", "reading", "hiking"],
                "favorites": {
                    "music_genre": "Electronic",
                    "book": "Neuromancer",
                    "color": "blue"
                },
                "aversions": ["loud noises"],
                "lifestyle": {
                    "diet": "vegetarian",
                    "sleep_schedule": "night owl"
                }
            }
        }"#;
        let entity = parse_aieos_json(json).unwrap();
        let interests = entity.interests.unwrap();
        assert_eq!(interests.hobbies, vec!["coding", "reading", "hiking"]);
        assert_eq!(
            interests.favorites.as_ref().unwrap().book,
            Some("Neuromancer".to_string())
        );
    }

    #[test]
    fn empty_strings_handled_gracefully() {
        let json = r#"{
            "identity": {
                "names": { "first": "", "nickname": "" }
            }
        }"#;
        let entity = parse_aieos_json(json).unwrap();
        // Should fall back to "Entity" when names are empty
        assert_eq!(entity.display_name(), "Entity");
    }

    // ══════════════════════════════════════════════════════════
    // Edge Case Tests
    // ══════════════════════════════════════════════════════════

    #[test]
    fn parse_empty_json_fails() {
        let result = parse_aieos_json("");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("empty"));
    }

    #[test]
    fn parse_whitespace_only_fails() {
        let result = parse_aieos_json("   \n\t  ");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("empty"));
    }

    #[test]
    fn parse_json_with_bom() {
        // UTF-8 BOM followed by valid JSON
        let json = "\u{feff}{\"identity\": {\"names\": {\"first\": \"BOM Test\"}}}";
        let entity = parse_aieos_json(json).unwrap();
        assert_eq!(entity.display_name(), "BOM Test");
    }

    #[test]
    fn parse_json_with_leading_whitespace() {
        let json = "   \n\t  {\"identity\": {\"names\": {\"first\": \"Whitespace\"}}}";
        let entity = parse_aieos_json(json).unwrap();
        assert_eq!(entity.display_name(), "Whitespace");
    }

    #[test]
    fn validate_version_1_0_ok() {
        let json = r#"{"standard": {"version": "1.0.0"}}"#;
        let entity = parse_aieos_json(json).unwrap();
        assert!(validate_aieos_version(&entity).is_ok());
    }

    #[test]
    fn validate_version_1_1_ok() {
        let json = r#"{"standard": {"version": "1.1.0"}}"#;
        let entity = parse_aieos_json(json).unwrap();
        assert!(validate_aieos_version(&entity).is_ok());
    }

    #[test]
    fn validate_version_1_2_warns_but_ok() {
        let json = r#"{"standard": {"version": "1.2.0"}}"#;
        let entity = parse_aieos_json(json).unwrap();
        // Should warn but not fail
        assert!(validate_aieos_version(&entity).is_ok());
    }

    #[test]
    fn validate_version_2_0_fails() {
        let json = r#"{"standard": {"version": "2.0.0"}}"#;
        let entity = parse_aieos_json(json).unwrap();
        let result = validate_aieos_version(&entity);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not compatible"));
    }

    #[test]
    fn validate_no_version_ok() {
        let json = r#"{}"#;
        let entity = parse_aieos_json(json).unwrap();
        assert!(validate_aieos_version(&entity).is_ok());
    }

    #[test]
    fn parse_invalid_json_provides_preview() {
        let result = parse_aieos_json("{invalid json here}");
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("Preview"));
    }

    #[test]
    fn ocean_traits_in_prompt() {
        let json = r#"{
            "psychology": {
                "traits": {
                    "ocean": {
                        "openness": 0.8,
                        "conscientiousness": 0.6,
                        "extraversion": 0.4,
                        "agreeableness": 0.7,
                        "neuroticism": 0.3
                    }
                }
            }
        }"#;
        let entity = parse_aieos_json(json).unwrap();
        let prompt = entity.to_system_prompt();
        assert!(prompt.contains("OCEAN:"));
        assert!(prompt.contains("O:80%"));
        assert!(prompt.contains("C:60%"));
        assert!(prompt.contains("E:40%"));
        assert!(prompt.contains("A:70%"));
        assert!(prompt.contains("N:30%"));
    }

    #[test]
    fn interests_in_prompt() {
        let json = r#"{
            "interests": {
                "hobbies": ["coding", "gaming"],
                "favorites": {
                    "music_genre": "Jazz",
                    "book": "Dune"
                },
                "aversions": ["crowds"],
                "lifestyle": {
                    "diet": "omnivore",
                    "sleep_schedule": "early bird"
                }
            }
        }"#;
        let entity = parse_aieos_json(json).unwrap();
        let prompt = entity.to_system_prompt();
        assert!(prompt.contains("### Interests & Lifestyle"));
        assert!(prompt.contains("coding, gaming"));
        assert!(prompt.contains("music: Jazz"));
        assert!(prompt.contains("book: Dune"));
        assert!(prompt.contains("crowds"));
        assert!(prompt.contains("diet: omnivore"));
    }

    #[test]
    fn null_values_handled() {
        // JSON with explicit nulls
        let json = r#"{
            "identity": {
                "names": { "first": null, "last": "Smith" }
            }
        }"#;
        let entity = parse_aieos_json(json).unwrap();
        assert_eq!(entity.full_name(), Some("Smith".to_string()));
    }

    #[test]
    fn extra_fields_ignored() {
        // JSON with unknown fields should be ignored (forward compatibility)
        let json = r#"{
            "identity": {
                "names": { "first": "Test" },
                "unknown_field": "should be ignored",
                "another_unknown": { "nested": true }
            },
            "future_section": { "data": 123 }
        }"#;
        let entity = parse_aieos_json(json).unwrap();
        assert_eq!(entity.display_name(), "Test");
    }

    #[test]
    fn case_insensitive_format_matching() {
        // This tests the config format matching in channels/mod.rs
        // Here we just verify the entity parses correctly
        let json = r#"{"identity": {"names": {"first": "CaseTest"}}}"#;
        let entity = parse_aieos_json(json).unwrap();
        assert_eq!(entity.display_name(), "CaseTest");
    }

    #[test]
    fn emotional_triggers_parsed() {
        let json = r#"{
            "psychology": {
                "emotional_profile": {
                    "base_mood": "optimistic",
                    "volatility": 0.3,
                    "resilience": "high",
                    "triggers": {
                        "joy": ["helping others", "learning"],
                        "anger": ["injustice"],
                        "sadness": ["loss"]
                    }
                }
            }
        }"#;
        let entity = parse_aieos_json(json).unwrap();
        let psych = entity.psychology.unwrap();
        let emotional = psych.emotional_profile.unwrap();
        assert_eq!(emotional.base_mood, Some("optimistic".to_string()));
        assert_eq!(emotional.triggers.as_ref().unwrap().joy.len(), 2);
    }

    #[test]
    fn idiosyncrasies_parsed() {
        let json = r#"{
            "psychology": {
                "idiosyncrasies": {
                    "phobias": ["heights"],
                    "obsessions": ["organization"],
                    "tics": ["tapping fingers"]
                }
            }
        }"#;
        let entity = parse_aieos_json(json).unwrap();
        let psych = entity.psychology.unwrap();
        let idio = psych.idiosyncrasies.unwrap();
        assert_eq!(idio.phobias, vec!["heights"]);
        assert_eq!(idio.obsessions, vec!["organization"]);
    }

    #[test]
    fn tts_config_parsed() {
        let json = r#"{
            "linguistics": {
                "voice": {
                    "tts_config": {
                        "provider": "elevenlabs",
                        "voice_id": "abc123",
                        "stability": 0.7,
                        "similarity_boost": 0.8
                    },
                    "accent": {
                        "region": "British",
                        "strength": 0.5
                    }
                }
            }
        }"#;
        let entity = parse_aieos_json(json).unwrap();
        let ling = entity.linguistics.unwrap();
        let voice = ling.voice.unwrap();
        assert_eq!(
            voice.tts_config.as_ref().unwrap().provider,
            Some("elevenlabs".to_string())
        );
        assert_eq!(
            voice.accent.as_ref().unwrap().region,
            Some("British".to_string())
        );
    }
}
