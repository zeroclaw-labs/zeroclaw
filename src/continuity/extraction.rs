use super::{PreferenceCategory, PreferenceModel};

pub fn sanitize_tool_name(name: &str) -> String {
    name.chars()
        .filter(|c| c.is_alphanumeric() || *c == '_' || *c == '-')
        .take(64)
        .collect()
}

pub fn extract_tool_preference(
    pref: &mut PreferenceModel,
    tool_name: &str,
    success: bool,
) -> Result<(), String> {
    if !success {
        return Ok(());
    }
    let key = format!("tool_affinity:{}", sanitize_tool_name(tool_name));
    pref.update(&key, "preferred", 0.3, PreferenceCategory::Technical)
}

pub fn extract_channel_preference(
    pref: &mut PreferenceModel,
    channel_name: &str,
) -> Result<(), String> {
    pref.update(
        "preferred_channel",
        channel_name,
        0.4,
        PreferenceCategory::Communication,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::continuity::types::DriftLimits;

    #[test]
    fn tool_preference_only_on_success() {
        let mut model = PreferenceModel::new(DriftLimits::default());
        let _ = extract_tool_preference(&mut model, "shell", false);
        assert!(model.get("tool_affinity:shell").is_none());

        let _ = extract_tool_preference(&mut model, "shell", true);
        assert!(model.get("tool_affinity:shell").is_some());
    }

    #[test]
    fn channel_preference_recorded() {
        let mut model = PreferenceModel::new(DriftLimits::default());
        extract_channel_preference(&mut model, "telegram").unwrap();
        let pref = model.get("preferred_channel").unwrap();
        assert_eq!(pref.value, "telegram");
    }

    #[test]
    fn sanitize_strips_special_chars() {
        assert_eq!(super::sanitize_tool_name("shell;rm -rf /"), "shellrm-rf");
        assert_eq!(super::sanitize_tool_name(&"a".repeat(100)), "a".repeat(64));
    }
}
