use serde::{Deserialize, Serialize};

/// Structured user model maintained via dialectic reasoning.
///
/// Stored in SQLite via `StructuredMemory` under key "user_model".
/// USER.md is rendered from this struct as a derived artifact.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct UserModel {
    #[serde(default)]
    pub communication_style: CommunicationStyle,
    #[serde(default)]
    pub expertise_areas: Vec<ExpertiseArea>,
    #[serde(default)]
    pub preferences: UserPreferences,
    #[serde(default)]
    pub interaction_patterns: InteractionPatterns,
    #[serde(default)]
    pub goals: Vec<UserGoal>,
    #[serde(default)]
    pub updated_at: Option<String>,
    #[serde(default)]
    pub version: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CommunicationStyle {
    #[serde(default)]
    pub verbosity: Verbosity,
    #[serde(default)]
    pub tone: Tone,
    #[serde(default)]
    pub format_preference: FormatPreference,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Verbosity {
    Terse,
    #[default]
    Normal,
    Verbose,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Tone {
    Casual,
    #[default]
    Professional,
    Technical,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum FormatPreference {
    #[default]
    Markdown,
    Plain,
    Structured,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExpertiseArea {
    pub domain: String,
    pub level: ExpertiseLevel,
    #[serde(default)]
    pub notes: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ExpertiseLevel {
    Beginner,
    Intermediate,
    Advanced,
    Expert,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct UserPreferences {
    #[serde(default)]
    pub preferred_tools: Vec<String>,
    #[serde(default)]
    pub avoided_patterns: Vec<String>,
    #[serde(default)]
    pub workflow_notes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct InteractionPatterns {
    #[serde(default)]
    pub total_turns: u64,
    #[serde(default)]
    pub common_request_types: Vec<String>,
    #[serde(default)]
    pub peak_activity_hours: Vec<u8>,
    #[serde(default)]
    pub average_tool_calls_per_turn: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserGoal {
    pub description: String,
    #[serde(default)]
    pub status: GoalStatus,
    #[serde(default)]
    pub created_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum GoalStatus {
    #[default]
    Active,
    Completed,
    Paused,
}

impl UserModel {
    pub fn render_user_md(&self) -> String {
        use std::fmt::Write;
        let mut out = String::from("# User Profile\n\n");

        // Communication style
        let _ = writeln!(out, "## Communication Style\n");
        let _ = writeln!(
            out,
            "- **Verbosity**: {:?}",
            self.communication_style.verbosity
        );
        let _ = writeln!(out, "- **Tone**: {:?}", self.communication_style.tone);
        let _ = writeln!(
            out,
            "- **Format**: {:?}",
            self.communication_style.format_preference
        );

        // Expertise
        if !self.expertise_areas.is_empty() {
            let _ = writeln!(out, "\n## Expertise\n");
            for area in &self.expertise_areas {
                let _ = write!(out, "- **{}**: {:?}", area.domain, area.level);
                if let Some(notes) = &area.notes {
                    let _ = write!(out, " — {notes}");
                }
                out.push('\n');
            }
        }

        // Preferences
        if !self.preferences.preferred_tools.is_empty()
            || !self.preferences.avoided_patterns.is_empty()
            || !self.preferences.workflow_notes.is_empty()
        {
            let _ = writeln!(out, "\n## Preferences\n");
            if !self.preferences.preferred_tools.is_empty() {
                let _ = writeln!(
                    out,
                    "- **Preferred tools**: {}",
                    self.preferences.preferred_tools.join(", ")
                );
            }
            if !self.preferences.avoided_patterns.is_empty() {
                let _ = writeln!(
                    out,
                    "- **Avoided patterns**: {}",
                    self.preferences.avoided_patterns.join(", ")
                );
            }
            for note in &self.preferences.workflow_notes {
                let _ = writeln!(out, "- {note}");
            }
        }

        // Goals
        if !self.goals.is_empty() {
            let _ = writeln!(out, "\n## Active Goals\n");
            for goal in &self.goals {
                if goal.status == GoalStatus::Active {
                    let _ = writeln!(out, "- {}", goal.description);
                }
            }
        }

        // Interaction patterns
        if self.interaction_patterns.total_turns > 0 {
            let _ = writeln!(out, "\n## Interaction Patterns\n");
            let _ = writeln!(
                out,
                "- **Total turns**: {}",
                self.interaction_patterns.total_turns
            );
            let _ = writeln!(
                out,
                "- **Avg tool calls/turn**: {:.1}",
                self.interaction_patterns.average_tool_calls_per_turn
            );
            if !self.interaction_patterns.common_request_types.is_empty() {
                let _ = writeln!(
                    out,
                    "- **Common requests**: {}",
                    self.interaction_patterns.common_request_types.join(", ")
                );
            }
        }

        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_user_model_serializes() {
        let model = UserModel::default();
        let json = serde_json::to_string(&model).unwrap();
        let parsed: UserModel = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.version, 0);
        assert!(parsed.expertise_areas.is_empty());
    }

    #[test]
    fn user_model_roundtrip() {
        let model = UserModel {
            communication_style: CommunicationStyle {
                verbosity: Verbosity::Terse,
                tone: Tone::Technical,
                format_preference: FormatPreference::Structured,
            },
            expertise_areas: vec![ExpertiseArea {
                domain: "Rust".into(),
                level: ExpertiseLevel::Expert,
                notes: Some("10 years experience".into()),
            }],
            preferences: UserPreferences {
                preferred_tools: vec!["shell".into(), "file_edit".into()],
                avoided_patterns: vec!["mocking databases".into()],
                workflow_notes: vec!["prefers single PRs for refactors".into()],
            },
            interaction_patterns: InteractionPatterns {
                total_turns: 150,
                common_request_types: vec!["bug-fix".into(), "code-review".into()],
                peak_activity_hours: vec![9, 10, 14, 15],
                average_tool_calls_per_turn: 3.5,
            },
            goals: vec![UserGoal {
                description: "Ship v2.0 by end of month".into(),
                status: GoalStatus::Active,
                created_at: Some("2026-05-01".into()),
            }],
            updated_at: Some("2026-05-18T12:00:00Z".into()),
            version: 5,
        };

        let json = serde_json::to_string_pretty(&model).unwrap();
        let parsed: UserModel = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.version, 5);
        assert_eq!(parsed.expertise_areas.len(), 1);
        assert_eq!(parsed.expertise_areas[0].domain, "Rust");
        assert_eq!(
            parsed.communication_style.verbosity,
            Verbosity::Terse
        );
    }

    #[test]
    fn render_user_md_basic() {
        let model = UserModel {
            communication_style: CommunicationStyle {
                verbosity: Verbosity::Terse,
                tone: Tone::Technical,
                ..Default::default()
            },
            expertise_areas: vec![ExpertiseArea {
                domain: "Rust".into(),
                level: ExpertiseLevel::Expert,
                notes: None,
            }],
            goals: vec![UserGoal {
                description: "Ship v2.0".into(),
                status: GoalStatus::Active,
                created_at: None,
            }],
            ..Default::default()
        };

        let md = model.render_user_md();
        assert!(md.contains("# User Profile"));
        assert!(md.contains("Terse"));
        assert!(md.contains("Technical"));
        assert!(md.contains("Rust"));
        assert!(md.contains("Ship v2.0"));
    }

    #[test]
    fn render_user_md_empty_model() {
        let model = UserModel::default();
        let md = model.render_user_md();
        assert!(md.contains("# User Profile"));
        assert!(md.contains("Normal"));
    }
}
