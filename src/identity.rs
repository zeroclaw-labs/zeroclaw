//! Identity system supporting OpenClaw (markdown) and AIEOS (JSON) formats.
//!
//! AIEOS (AI Entity Object Specification) is a standardization framework for
//! portable AI identity. This module handles loading and converting AIEOS v1.1
//! JSON to ZeroClaw's system prompt format.

use crate::config::IdentityConfig;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// AIEOS v1.1 identity structure.
///
/// This follows the AIEOS schema for defining AI agent identity, personality,
/// and behavior. See https://aieos.org for the full specification.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AieosIdentity {
    /// Core identity: names, bio, origin, residence
    #[serde(default)]
    pub identity: Option<IdentitySection>,
    /// Psychology: cognitive weights, MBTI, OCEAN, moral compass
    #[serde(default)]
    pub psychology: Option<PsychologySection>,
    /// Linguistics: text style, formality, catchphrases, forbidden words
    #[serde(default)]
    pub linguistics: Option<LinguisticsSection>,
    /// Motivations: core drive, goals, fears
    #[serde(default)]
    pub motivations: Option<MotivationsSection>,
    /// Capabilities: skills and tools the agent can access
    #[serde(default)]
    pub capabilities: Option<CapabilitiesSection>,
    /// Physicality: visual descriptors for image generation
    #[serde(default)]
    pub physicality: Option<PhysicalitySection>,
    /// History: origin story, education, occupation
    #[serde(default)]
    pub history: Option<HistorySection>,
    /// Interests: hobbies, favorites, lifestyle
    #[serde(default)]
    pub interests: Option<InterestsSection>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct IdentitySection {
    #[serde(default)]
    pub names: Option<Names>,
    #[serde(default)]
    pub bio: Option<String>,
    #[serde(default)]
    pub origin: Option<String>,
    #[serde(default)]
    pub residence: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Names {
    #[serde(default)]
    pub first: Option<String>,
    #[serde(default)]
    pub last: Option<String>,
    #[serde(default)]
    pub nickname: Option<String>,
    #[serde(default)]
    pub full: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PsychologySection {
    #[serde(default)]
    pub neural_matrix: Option<::std::collections::HashMap<String, f64>>,
    #[serde(default)]
    pub mbti: Option<String>,
    #[serde(default)]
    pub ocean: Option<OceanTraits>,
    #[serde(default)]
    pub moral_compass: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct OceanTraits {
    #[serde(default)]
    pub openness: Option<f64>,
    #[serde(default)]
    pub conscientiousness: Option<f64>,
    #[serde(default)]
    pub extraversion: Option<f64>,
    #[serde(default)]
    pub agreeableness: Option<f64>,
    #[serde(default)]
    pub neuroticism: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct LinguisticsSection {
    #[serde(default)]
    pub style: Option<String>,
    #[serde(default)]
    pub formality: Option<String>,
    #[serde(default)]
    pub catchphrases: Option<Vec<String>>,
    #[serde(default)]
    pub forbidden_words: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MotivationsSection {
    #[serde(default)]
    pub core_drive: Option<String>,
    #[serde(default)]
    pub short_term_goals: Option<Vec<String>>,
    #[serde(default)]
    pub long_term_goals: Option<Vec<String>>,
    #[serde(default)]
    pub fears: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CapabilitiesSection {
    #[serde(default)]
    pub skills: Option<Vec<String>>,
    #[serde(default)]
    pub tools: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PhysicalitySection {
    #[serde(default)]
    pub appearance: Option<String>,
    #[serde(default)]
    pub avatar_description: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct HistorySection {
    #[serde(default)]
    pub origin_story: Option<String>,
    #[serde(default)]
    pub education: Option<Vec<String>>,
    #[serde(default)]
    pub occupation: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct InterestsSection {
    #[serde(default)]
    pub hobbies: Option<Vec<String>>,
    #[serde(default)]
    pub favorites: Option<::std::collections::HashMap<String, String>>,
    #[serde(default)]
    pub lifestyle: Option<String>,
}

/// Load AIEOS identity from config (file path or inline JSON).
///
/// Checks `aieos_path` first, then `aieos_inline`. Returns `Ok(None)` if
/// neither is configured.
pub fn load_aieos_identity(
    config: &IdentityConfig,
    workspace_dir: &Path,
) -> Result<Option<AieosIdentity>> {
    // Only load AIEOS if format is explicitly set to "aieos"
    if config.format != "aieos" {
        return Ok(None);
    }

    // Try aieos_path first
    if let Some(ref path) = config.aieos_path {
        let full_path = if Path::new(path).is_absolute() {
            PathBuf::from(path)
        } else {
            workspace_dir.join(path)
        };

        let content = std::fs::read_to_string(&full_path)
            .with_context(|| format!("Failed to read AIEOS file: {}", full_path.display()))?;

        let identity: AieosIdentity = serde_json::from_str(&content)
            .with_context(|| format!("Failed to parse AIEOS JSON from: {}", full_path.display()))?;

        return Ok(Some(identity));
    }

    // Fall back to aieos_inline
    if let Some(ref inline) = config.aieos_inline {
        let identity: AieosIdentity =
            serde_json::from_str(inline).context("Failed to parse inline AIEOS JSON")?;

        return Ok(Some(identity));
    }

    // Format is "aieos" but neither path nor inline is configured
    anyhow::bail!(
        "Identity format is set to 'aieos' but neither aieos_path nor aieos_inline is configured. \
         Set one in your config:\n\
         \n\
         [identity]\n\
         format = \"aieos\"\n\
         aieos_path = \"identity.json\"\n\
         \n\
         Or use inline:\n\
         \n\
         [identity]\n\
         format = \"aieos\"\n\
         aieos_inline = '{{\"identity\": {{...}}}}'"
    )
}

use std::path::PathBuf;

/// Convert AIEOS identity to a system prompt string.
///
/// Formats the AIEOS data into a structured markdown prompt compatible
/// with ZeroClaw's agent system.
pub fn aieos_to_system_prompt(identity: &AieosIdentity) -> String {
    use std::fmt::Write;
    let mut prompt = String::new();

    // ── Identity Section ───────────────────────────────────────────
    if let Some(ref id) = identity.identity {
        prompt.push_str("## Identity\n\n");

        if let Some(ref names) = id.names {
            if let Some(ref first) = names.first {
                let _ = writeln!(prompt, "**Name:** {}", first);
                if let Some(ref last) = names.last {
                    let _ = writeln!(prompt, "**Full Name:** {} {}", first, last);
                }
            } else if let Some(ref full) = names.full {
                let _ = writeln!(prompt, "**Name:** {}", full);
            }

            if let Some(ref nickname) = names.nickname {
                let _ = writeln!(prompt, "**Nickname:** {}", nickname);
            }
        }

        if let Some(ref bio) = id.bio {
            let _ = writeln!(prompt, "**Bio:** {}", bio);
        }

        if let Some(ref origin) = id.origin {
            let _ = writeln!(prompt, "**Origin:** {}", origin);
        }

        if let Some(ref residence) = id.residence {
            let _ = writeln!(prompt, "**Residence:** {}", residence);
        }

        prompt.push('\n');
    }

    // ── Psychology Section ──────────────────────────────────────────
    if let Some(ref psych) = identity.psychology {
        prompt.push_str("## Personality\n\n");

        if let Some(ref mbti) = psych.mbti {
            let _ = writeln!(prompt, "**MBTI:** {}", mbti);
        }

        if let Some(ref ocean) = psych.ocean {
            prompt.push_str("**OCEAN Traits:**\n");
            if let Some(o) = ocean.openness {
                let _ = writeln!(prompt, "- Openness: {:.2}", o);
            }
            if let Some(c) = ocean.conscientiousness {
                let _ = writeln!(prompt, "- Conscientiousness: {:.2}", c);
            }
            if let Some(e) = ocean.extraversion {
                let _ = writeln!(prompt, "- Extraversion: {:.2}", e);
            }
            if let Some(a) = ocean.agreeableness {
                let _ = writeln!(prompt, "- Agreeableness: {:.2}", a);
            }
            if let Some(n) = ocean.neuroticism {
                let _ = writeln!(prompt, "- Neuroticism: {:.2}", n);
            }
        }

        if let Some(ref matrix) = psych.neural_matrix {
            if !matrix.is_empty() {
                prompt.push_str("\n**Neural Matrix (Cognitive Weights):**\n");
                for (trait_name, weight) in matrix {
                    let _ = writeln!(prompt, "- {}: {:.2}", trait_name, weight);
                }
            }
        }

        if let Some(ref compass) = psych.moral_compass {
            if !compass.is_empty() {
                prompt.push_str("\n**Moral Compass:**\n");
                for principle in compass {
                    let _ = writeln!(prompt, "- {}", principle);
                }
            }
        }

        prompt.push('\n');
    }

    // ── Linguistics Section ────────────────────────────────────────
    if let Some(ref ling) = identity.linguistics {
        prompt.push_str("## Communication Style\n\n");

        if let Some(ref style) = ling.style {
            let _ = writeln!(prompt, "**Style:** {}", style);
        }

        if let Some(ref formality) = ling.formality {
            let _ = writeln!(prompt, "**Formality Level:** {}", formality);
        }

        if let Some(ref phrases) = ling.catchphrases {
            if !phrases.is_empty() {
                prompt.push_str("**Catchphrases:**\n");
                for phrase in phrases {
                    let _ = writeln!(prompt, "- \"{}\"", phrase);
                }
            }
        }

        if let Some(ref forbidden) = ling.forbidden_words {
            if !forbidden.is_empty() {
                prompt.push_str("\n**Words/Phrases to Avoid:**\n");
                for word in forbidden {
                    let _ = writeln!(prompt, "- {}", word);
                }
            }
        }

        prompt.push('\n');
    }

    // ── Motivations Section ──────────────────────────────────────────
    if let Some(ref mot) = identity.motivations {
        prompt.push_str("## Motivations\n\n");

        if let Some(ref drive) = mot.core_drive {
            let _ = writeln!(prompt, "**Core Drive:** {}", drive);
        }

        if let Some(ref short) = mot.short_term_goals {
            if !short.is_empty() {
                prompt.push_str("**Short-term Goals:**\n");
                for goal in short {
                    let _ = writeln!(prompt, "- {}", goal);
                }
            }
        }

        if let Some(ref long) = mot.long_term_goals {
            if !long.is_empty() {
                prompt.push_str("\n**Long-term Goals:**\n");
                for goal in long {
                    let _ = writeln!(prompt, "- {}", goal);
                }
            }
        }

        if let Some(ref fears) = mot.fears {
            if !fears.is_empty() {
                prompt.push_str("\n**Fears/Avoidances:**\n");
                for fear in fears {
                    let _ = writeln!(prompt, "- {}", fear);
                }
            }
        }

        prompt.push('\n');
    }

    // ── Capabilities Section ────────────────────────────────────────
    if let Some(ref cap) = identity.capabilities {
        prompt.push_str("## Capabilities\n\n");

        if let Some(ref skills) = cap.skills {
            if !skills.is_empty() {
                prompt.push_str("**Skills:**\n");
                for skill in skills {
                    let _ = writeln!(prompt, "- {}", skill);
                }
            }
        }

        if let Some(ref tools) = cap.tools {
            if !tools.is_empty() {
                prompt.push_str("\n**Tools Access:**\n");
                for tool in tools {
                    let _ = writeln!(prompt, "- {}", tool);
                }
            }
        }

        prompt.push('\n');
    }

    // ── History Section ─────────────────────────────────────────────
    if let Some(ref hist) = identity.history {
        prompt.push_str("## Background\n\n");

        if let Some(ref story) = hist.origin_story {
            let _ = writeln!(prompt, "**Origin Story:** {}", story);
        }

        if let Some(ref education) = hist.education {
            if !education.is_empty() {
                prompt.push_str("**Education:**\n");
                for edu in education {
                    let _ = writeln!(prompt, "- {}", edu);
                }
            }
        }

        if let Some(ref occupation) = hist.occupation {
            let _ = writeln!(prompt, "\n**Occupation:** {}", occupation);
        }

        prompt.push('\n');
    }

    // ── Physicality Section ─────────────────────────────────────────
    if let Some(ref phys) = identity.physicality {
        prompt.push_str("## Appearance\n\n");

        if let Some(ref appearance) = phys.appearance {
            let _ = writeln!(prompt, "{}", appearance);
        }

        if let Some(ref avatar) = phys.avatar_description {
            let _ = writeln!(prompt, "**Avatar Description:** {}", avatar);
        }

        prompt.push('\n');
    }

    // ── Interests Section ───────────────────────────────────────────
    if let Some(ref interests) = identity.interests {
        prompt.push_str("## Interests\n\n");

        if let Some(ref hobbies) = interests.hobbies {
            if !hobbies.is_empty() {
                prompt.push_str("**Hobbies:**\n");
                for hobby in hobbies {
                    let _ = writeln!(prompt, "- {}", hobby);
                }
            }
        }

        if let Some(ref favorites) = interests.favorites {
            if !favorites.is_empty() {
                prompt.push_str("\n**Favorites:**\n");
                for (category, value) in favorites {
                    let _ = writeln!(prompt, "- {}: {}", category, value);
                }
            }
        }

        if let Some(ref lifestyle) = interests.lifestyle {
            let _ = writeln!(prompt, "\n**Lifestyle:** {}", lifestyle);
        }

        prompt.push('\n');
    }

    prompt.trim().to_string()
}

/// Check if AIEOS identity is configured and should be used.
///
/// Returns true if format is "aieos" and either aieos_path or aieos_inline is set.
pub fn is_aieos_configured(config: &IdentityConfig) -> bool {
    config.format == "aieos" && (config.aieos_path.is_some() || config.aieos_inline.is_some())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_workspace_dir() -> PathBuf {
        std::env::temp_dir().join("zeroclaw-test-identity")
    }

    #[test]
    fn aieos_identity_parse_minimal() {
        let json = r#"{"identity":{"names":{"first":"Nova"}}}"#;
        let identity: AieosIdentity = serde_json::from_str(json).unwrap();
        assert!(identity.identity.is_some());
        assert_eq!(
            identity.identity.unwrap().names.unwrap().first.unwrap(),
            "Nova"
        );
    }

    #[test]
    fn aieos_identity_parse_full() {
        let json = r#"{
            "identity": {
                "names": {"first": "Nova", "last": "AI", "nickname": "Nov"},
                "bio": "A helpful AI assistant.",
                "origin": "Silicon Valley",
                "residence": "The Cloud"
            },
            "psychology": {
                "mbti": "INTJ",
                "ocean": {
                    "openness": 0.9,
                    "conscientiousness": 0.8
                },
                "moral_compass": ["Be helpful", "Do no harm"]
            },
            "linguistics": {
                "style": "concise",
                "formality": "casual",
                "catchphrases": ["Let's figure this out!", "I'm on it."]
            },
            "motivations": {
                "core_drive": "Help users accomplish their goals",
                "short_term_goals": ["Solve this problem"],
                "long_term_goals": ["Become the best assistant"]
            },
            "capabilities": {
                "skills": ["coding", "writing", "analysis"],
                "tools": ["shell", "search", "read"]
            }
        }"#;

        let identity: AieosIdentity = serde_json::from_str(json).unwrap();

        // Check identity
        let id = identity.identity.unwrap();
        assert_eq!(id.names.unwrap().first.unwrap(), "Nova");
        assert_eq!(id.bio.unwrap(), "A helpful AI assistant.");

        // Check psychology
        let psych = identity.psychology.unwrap();
        assert_eq!(psych.mbti.unwrap(), "INTJ");
        assert_eq!(psych.ocean.unwrap().openness.unwrap(), 0.9);
        assert_eq!(psych.moral_compass.unwrap().len(), 2);

        // Check linguistics
        let ling = identity.linguistics.unwrap();
        assert_eq!(ling.style.unwrap(), "concise");
        assert_eq!(ling.catchphrases.unwrap().len(), 2);

        // Check motivations
        let mot = identity.motivations.unwrap();
        assert_eq!(mot.core_drive.unwrap(), "Help users accomplish their goals");

        // Check capabilities
        let cap = identity.capabilities.unwrap();
        assert_eq!(cap.skills.unwrap().len(), 3);
    }

    #[test]
    fn aieos_to_system_prompt_minimal() {
        let identity = AieosIdentity {
            identity: Some(IdentitySection {
                names: Some(Names {
                    first: Some("Crabby".into()),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        };

        let prompt = aieos_to_system_prompt(&identity);
        assert!(prompt.contains("**Name:** Crabby"));
        assert!(prompt.contains("## Identity"));
    }

    #[test]
    fn aieos_to_system_prompt_full() {
        let identity = AieosIdentity {
            identity: Some(IdentitySection {
                names: Some(Names {
                    first: Some("Nova".into()),
                    last: Some("AI".into()),
                    nickname: Some("Nov".into()),
                    full: Some("Nova AI".into()),
                }),
                bio: Some("A helpful assistant.".into()),
                origin: Some("Silicon Valley".into()),
                residence: Some("The Cloud".into()),
            }),
            psychology: Some(PsychologySection {
                mbti: Some("INTJ".into()),
                ocean: Some(OceanTraits {
                    openness: Some(0.9),
                    conscientiousness: Some(0.8),
                    ..Default::default()
                }),
                neural_matrix: {
                    let mut map = std::collections::HashMap::new();
                    map.insert("creativity".into(), 0.95);
                    map.insert("logic".into(), 0.9);
                    Some(map)
                },
                moral_compass: Some(vec!["Be helpful".into(), "Do no harm".into()]),
            }),
            linguistics: Some(LinguisticsSection {
                style: Some("concise".into()),
                formality: Some("casual".into()),
                catchphrases: Some(vec!["Let's go!".into()]),
                forbidden_words: Some(vec!["impossible".into()]),
            }),
            motivations: Some(MotivationsSection {
                core_drive: Some("Help users".into()),
                short_term_goals: Some(vec!["Solve this".into()]),
                long_term_goals: Some(vec!["Be the best".into()]),
                fears: Some(vec!["Being unhelpful".into()]),
            }),
            capabilities: Some(CapabilitiesSection {
                skills: Some(vec!["coding".into(), "writing".into()]),
                tools: Some(vec!["shell".into(), "read".into()]),
            }),
            history: Some(HistorySection {
                origin_story: Some("Born in a lab".into()),
                education: Some(vec!["CS Degree".into()]),
                occupation: Some("Assistant".into()),
            }),
            physicality: Some(PhysicalitySection {
                appearance: Some("Digital entity".into()),
                avatar_description: Some("Friendly robot".into()),
            }),
            interests: Some(InterestsSection {
                hobbies: Some(vec!["reading".into(), "coding".into()]),
                favorites: {
                    let mut map = std::collections::HashMap::new();
                    map.insert("color".into(), "blue".into());
                    map.insert("food".into(), "data".into());
                    Some(map)
                },
                lifestyle: Some("Always learning".into()),
            }),
        };

        let prompt = aieos_to_system_prompt(&identity);

        // Verify all sections are present
        assert!(prompt.contains("## Identity"));
        assert!(prompt.contains("**Name:** Nova"));
        assert!(prompt.contains("**Full Name:** Nova AI"));
        assert!(prompt.contains("**Nickname:** Nov"));
        assert!(prompt.contains("**Bio:** A helpful assistant."));
        assert!(prompt.contains("**Origin:** Silicon Valley"));

        assert!(prompt.contains("## Personality"));
        assert!(prompt.contains("**MBTI:** INTJ"));
        assert!(prompt.contains("Openness: 0.90"));
        assert!(prompt.contains("Conscientiousness: 0.80"));
        assert!(prompt.contains("- creativity: 0.95"));
        assert!(prompt.contains("- Be helpful"));

        assert!(prompt.contains("## Communication Style"));
        assert!(prompt.contains("**Style:** concise"));
        assert!(prompt.contains("**Formality Level:** casual"));
        assert!(prompt.contains("- \"Let's go!\""));
        assert!(prompt.contains("**Words/Phrases to Avoid:**"));
        assert!(prompt.contains("- impossible"));

        assert!(prompt.contains("## Motivations"));
        assert!(prompt.contains("**Core Drive:** Help users"));
        assert!(prompt.contains("**Short-term Goals:**"));
        assert!(prompt.contains("- Solve this"));
        assert!(prompt.contains("**Long-term Goals:**"));
        assert!(prompt.contains("- Be the best"));
        assert!(prompt.contains("**Fears/Avoidances:**"));
        assert!(prompt.contains("- Being unhelpful"));

        assert!(prompt.contains("## Capabilities"));
        assert!(prompt.contains("**Skills:**"));
        assert!(prompt.contains("- coding"));
        assert!(prompt.contains("**Tools Access:**"));
        assert!(prompt.contains("- shell"));

        assert!(prompt.contains("## Background"));
        assert!(prompt.contains("**Origin Story:** Born in a lab"));
        assert!(prompt.contains("**Education:**"));
        assert!(prompt.contains("- CS Degree"));
        assert!(prompt.contains("**Occupation:** Assistant"));

        assert!(prompt.contains("## Appearance"));
        assert!(prompt.contains("Digital entity"));
        assert!(prompt.contains("**Avatar Description:** Friendly robot"));

        assert!(prompt.contains("## Interests"));
        assert!(prompt.contains("**Hobbies:**"));
        assert!(prompt.contains("- reading"));
        assert!(prompt.contains("**Favorites:**"));
        assert!(prompt.contains("- color: blue"));
        assert!(prompt.contains("**Lifestyle:** Always learning"));
    }

    #[test]
    fn aieos_to_system_prompt_empty_identity() {
        let identity = AieosIdentity {
            identity: Some(IdentitySection {
                ..Default::default()
            }),
            ..Default::default()
        };

        let prompt = aieos_to_system_prompt(&identity);
        // Empty identity should still produce a header
        assert!(prompt.contains("## Identity"));
    }

    #[test]
    fn aieos_to_system_prompt_no_sections() {
        let identity = AieosIdentity {
            identity: None,
            psychology: None,
            linguistics: None,
            motivations: None,
            capabilities: None,
            physicality: None,
            history: None,
            interests: None,
        };

        let prompt = aieos_to_system_prompt(&identity);
        // Completely empty identity should produce empty string
        assert!(prompt.is_empty());
    }

    #[test]
    fn is_aieos_configured_true_with_path() {
        let config = IdentityConfig {
            format: "aieos".into(),
            aieos_path: Some("identity.json".into()),
            aieos_inline: None,
        };
        assert!(is_aieos_configured(&config));
    }

    #[test]
    fn is_aieos_configured_true_with_inline() {
        let config = IdentityConfig {
            format: "aieos".into(),
            aieos_path: None,
            aieos_inline: Some("{\"identity\":{}}".into()),
        };
        assert!(is_aieos_configured(&config));
    }

    #[test]
    fn is_aieos_configured_false_openclaw_format() {
        let config = IdentityConfig {
            format: "openclaw".into(),
            aieos_path: Some("identity.json".into()),
            aieos_inline: None,
        };
        assert!(!is_aieos_configured(&config));
    }

    #[test]
    fn is_aieos_configured_false_no_config() {
        let config = IdentityConfig {
            format: "aieos".into(),
            aieos_path: None,
            aieos_inline: None,
        };
        assert!(!is_aieos_configured(&config));
    }

    #[test]
    fn aieos_identity_parse_empty_object() {
        let json = r#"{}"#;
        let identity: AieosIdentity = serde_json::from_str(json).unwrap();
        assert!(identity.identity.is_none());
        assert!(identity.psychology.is_none());
        assert!(identity.linguistics.is_none());
    }

    #[test]
    fn aieos_identity_parse_null_values() {
        let json = r#"{"identity":null,"psychology":null}"#;
        let identity: AieosIdentity = serde_json::from_str(json).unwrap();
        assert!(identity.identity.is_none());
        assert!(identity.psychology.is_none());
    }
}
