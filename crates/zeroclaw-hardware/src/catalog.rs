//! Canonical hardware tool-name and capability catalog.

pub const BASE_TOOLS: &[&str] = &["gpio_read", "gpio_write"];

/// probe-rs backed introspection tools (in `zeroclaw-tools`).
pub const PROBE_TOOLS: &[&str] = &[
    "hardware_board_info",
    "hardware_memory_map",
    "hardware_memory_read",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_duplicate_tool_names_across_sets() {
        let mut all: Vec<&str> = BASE_TOOLS.iter().chain(PROBE_TOOLS).copied().collect();
        let before = all.len();
        all.sort_unstable();
        all.dedup();
        assert_eq!(all.len(), before, "duplicate tool name across catalog sets");
    }
}
