//! Progressive Disclosure — token-efficient skill injection.
//!
//! Skills are injected into the system prompt at three depth levels:
//!
//! - **L0** (List): Names + descriptions only (~2k tokens for all skills).
//!   Always injected so the agent knows what skills exist.
//! - **L1** (Full): Complete SKILL.md loaded on demand when the agent
//!   identifies a relevant skill for the current task.
//! - **L2** (Reference): Supporting files (checklists, templates) loaded
//!   only for deep specialized work.

use super::store::SkillRecord;
use std::fmt::Write;

/// Compact skill summary for L0 injection.
#[derive(Debug, Clone)]
pub struct SkillSummary {
    pub name: String,
    pub category: Option<String>,
    pub description: String,
    pub use_count: i64,
    pub success_rate: f64,
}

impl From<&SkillRecord> for SkillSummary {
    fn from(r: &SkillRecord) -> Self {
        let success_rate = if r.use_count > 0 {
            r.success_count as f64 / r.use_count as f64
        } else {
            0.0
        };
        Self {
            name: r.name.clone(),
            category: r.category.clone(),
            description: r.description.clone(),
            use_count: r.use_count,
            success_rate,
        }
    }
}

/// Depth level for skill loading.
#[derive(Debug, Clone)]
pub enum SkillDepth {
    /// L0: Skill list only (names + descriptions). Injected in system prompt.
    List,
    /// L1: Full SKILL.md content for a specific skill. Loaded on demand.
    Full(String),
    /// L2: Reference files for a specific skill. Loaded for deep work.
    Reference(String, String),
}

/// Build the L0 skill index for system prompt injection.
///
/// Returns a formatted string listing all available skills with their
/// categories and descriptions. Designed to be compact (~50 bytes per skill)
/// so that even 100 skills fit within ~5k tokens.
pub fn inject_skill_index(skills: &[SkillSummary]) -> String {
    if skills.is_empty() {
        return String::new();
    }

    let mut out = String::with_capacity(skills.len() * 80);
    out.push_str("당신은 다음 학습된 스킬을 보유하고 있습니다:\n");

    for s in skills {
        let cat = s.category.as_deref().unwrap_or("general");
        let _ = write!(out, "  - [{}] {}: {}", cat, s.name, s.description);
        if s.use_count > 0 {
            let _ = write!(out, " ({}회 사용, 성공률 {:.0}%)", s.use_count, s.success_rate * 100.0);
        }
        out.push('\n');
    }

    out.push_str("\n필요하면 skill_view(name)으로 전문을 로드하세요.\n");
    out
}

/// Build the L1 full skill content for on-demand loading.
pub fn format_skill_full(record: &SkillRecord) -> String {
    let mut out = String::with_capacity(record.content_md.len() + 200);
    let _ = write!(
        out,
        "# 스킬: {} (v{})\n카테고리: {}\n사용: {}회, 성공: {}회\n\n---\n\n{}",
        record.name,
        record.version,
        record.category.as_deref().unwrap_or("general"),
        record.use_count,
        record.success_count,
        record.content_md,
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_index_produces_empty_string() {
        assert!(inject_skill_index(&[]).is_empty());
    }

    #[test]
    fn index_contains_all_skills() {
        let skills = vec![
            SkillSummary {
                name: "hwp-fix".into(),
                category: Some("document".into()),
                description: "HWP conversion pitfalls".into(),
                use_count: 5,
                success_rate: 0.8,
            },
            SkillSummary {
                name: "rust-borrow".into(),
                category: Some("coding".into()),
                description: "Borrow checker patterns".into(),
                use_count: 0,
                success_rate: 0.0,
            },
        ];
        let index = inject_skill_index(&skills);
        assert!(index.contains("[document] hwp-fix"));
        assert!(index.contains("[coding] rust-borrow"));
        assert!(index.contains("5회 사용"));
        assert!(index.contains("skill_view"));
    }

    #[test]
    fn format_full_includes_metadata() {
        let record = SkillRecord {
            id: "id1".into(),
            name: "test-skill".into(),
            category: Some("coding".into()),
            description: "Test".into(),
            content_md: "# Test\n\nContent here".into(),
            version: 3,
            use_count: 10,
            success_count: 8,
            created_at: 0,
            updated_at: 0,
            created_by: "agent".into(),
            device_id: "dev".into(),
        };
        let full = format_skill_full(&record);
        assert!(full.contains("v3"));
        assert!(full.contains("사용: 10회"));
        assert!(full.contains("Content here"));
    }
}
