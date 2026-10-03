pub fn t(key: &str) -> &str {
    match key {
        "skills.title" => "Skills",
        "skills.agent" => "Agent",
        "skills.search" => "Search skills…",
        "skills.reload" => "Reload",
        "skills.empty" => "This agent has no skills.",
        "skills.load_error" => "Failed to load skills",
        "skills.skipped_count" => "skill(s) skipped (failed security audit)",
        "skills.skill_md" => "SKILL.md",
        "skills.shadows" => "shadows",
        "common.edit" => "Edit",
        _ => key,
    }
}
