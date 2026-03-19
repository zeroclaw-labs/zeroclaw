//! Lazy heat decay and reactivation for graph nodes.
//!
//! Heat models attention: recently accessed or emotionally salient nodes
//! have higher heat. Decay is exponential: `heat * e^(-lambda * days_since_access)`.

use chrono::{DateTime, Utc};

/// Calculate decayed heat for a node.
///
/// Formula: `current_heat * e^(-lambda * days_elapsed)`
pub fn decay_heat(current_heat: f64, lambda: f64, last_accessed: &DateTime<Utc>) -> f64 {
    let now = Utc::now();
    let elapsed = now.signed_duration_since(*last_accessed);
    let days = elapsed.num_seconds() as f64 / 86_400.0;
    if days <= 0.0 {
        return current_heat;
    }
    current_heat * (-lambda * days).exp()
}

/// Reactivate a node by boosting its heat.
///
/// Uses additive boost clamped to [0.0, max_heat].
pub fn reactivate(current_heat: f64, boost: f64, max_heat: f64) -> f64 {
    (current_heat + boost).min(max_heat).max(0.0)
}

/// Check if a node is "hot" (above threshold after decay).
pub fn is_hot(heat: f64, threshold: f64) -> bool {
    heat >= threshold
}

/// Build a CozoDB Datalog update script for lazy heat decay.
/// Updates heat for all nodes in the given relation that haven't been accessed recently.
pub fn build_decay_update_query(relation: &str, _lambda: f64) -> String {
    // CozoDB doesn't have exp() natively, so we compute on the Rust side
    // and push updates. This function generates the select query.
    format!(
        r#"?[id, heat, last_accessed] := *{relation}{{id, heat, last_accessed}}, heat > 0.01"#,
        relation = relation
    )
}

/// Build a CozoDB Datalog put script to update heat values.
pub fn build_heat_put_query(relation: &str) -> String {
    format!(":put {relation} {{id => heat, last_accessed}}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    #[test]
    fn fresh_node_has_no_decay() {
        let now = Utc::now();
        let heat = decay_heat(1.0, 0.05, &now);
        assert!((heat - 1.0).abs() < 0.01);
    }

    #[test]
    fn old_node_decays_significantly() {
        let old = Utc::now() - Duration::days(30);
        let heat = decay_heat(1.0, 0.05, &old);
        assert!(heat < 0.3, "heat should be < 0.3 after 30 days, got {heat}");
    }

    #[test]
    fn reactivation_boosts_heat() {
        let heat = reactivate(0.2, 0.5, 1.0);
        assert!((heat - 0.7).abs() < 0.01);
    }

    #[test]
    fn reactivation_clamps_to_max() {
        let heat = reactivate(0.8, 0.5, 1.0);
        assert!((heat - 1.0).abs() < 0.01);
    }

    #[test]
    fn hot_detection() {
        assert!(is_hot(0.5, 0.3));
        assert!(!is_hot(0.2, 0.3));
    }

    #[test]
    fn decay_query_contains_relation() {
        let q = build_decay_update_query("concept", 0.05);
        assert!(q.contains("concept"));
    }
}
