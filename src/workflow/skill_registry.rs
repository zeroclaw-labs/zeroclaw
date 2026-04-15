// Skill Registry & Resolver (v3.0 Section Shopping)
//
// Composite skills are sub-workflow YAMLs named `skill.NAMESPACE.NAME`.
// When a `tool_call` step's `tool` field starts with `skill.`, the registry
// loads the YAML from the bundled skill library and inlines its steps.
//
// Naming convention:
//   tool: skill.shopping.add_to_cart
//     → resources: src/workflow/skills/shopping/add_to_cart.yaml
//
// Skills are plain workflow YAML (same schema as top-level workflows) with
// the additional convention that their `inputs` are populated from the
// calling step's `args`.

use std::collections::HashMap;

use anyhow::{bail, Context, Result};

use super::parser::{parse_spec, Step, WorkflowSpec};

/// All bundled skills, embedded at compile time.
///
/// Each entry maps a fully-qualified skill slug (`ns.name`) to its YAML source.
pub fn bundled_skills() -> HashMap<&'static str, &'static str> {
    let mut m = HashMap::new();
    // ── Shopping skills (Phase 1: 5 core) ──────────────────────
    m.insert(
        "shopping.search_browser",
        include_str!("skills/shopping/search_browser.yaml"),
    );
    m.insert(
        "shopping.add_to_cart",
        include_str!("skills/shopping/add_to_cart.yaml"),
    );
    m.insert(
        "shopping.fill_payment_method",
        include_str!("skills/shopping/fill_payment_method.yaml"),
    );
    m.insert(
        "shopping.invoke_native_pay",
        include_str!("skills/shopping/invoke_native_pay.yaml"),
    );
    m.insert(
        "shopping.audit_log_payment",
        include_str!("skills/shopping/audit_log_payment.yaml"),
    );
    // ── Shopping skills (Phase 2: 15 extended) ─────────────────
    m.insert(
        "shopping.search_naver_api",
        include_str!("skills/shopping/search_naver_api.yaml"),
    );
    m.insert(
        "shopping.compare_danawa",
        include_str!("skills/shopping/compare_danawa.yaml"),
    );
    m.insert(
        "shopping.goto_checkout",
        include_str!("skills/shopping/goto_checkout.yaml"),
    );
    m.insert(
        "shopping.fill_shipping",
        include_str!("skills/shopping/fill_shipping.yaml"),
    );
    m.insert(
        "shopping.compliance_check",
        include_str!("skills/shopping/compliance_check.yaml"),
    );
    m.insert(
        "shopping.tos_for_site",
        include_str!("skills/shopping/tos_for_site.yaml"),
    );
    m.insert(
        "shopping.preload_ticket_page",
        include_str!("skills/shopping/preload_ticket_page.yaml"),
    );
    m.insert(
        "shopping.seat_assist",
        include_str!("skills/shopping/seat_assist.yaml"),
    );
    m.insert(
        "shopping.parse_receipt",
        include_str!("skills/shopping/parse_receipt.yaml"),
    );
    m.insert(
        "shopping.image_search_to_query",
        include_str!("skills/shopping/image_search_to_query.yaml"),
    );
    m.insert(
        "shopping.coupon_finder",
        include_str!("skills/shopping/coupon_finder.yaml"),
    );
    m.insert(
        "shopping.apply_coupons",
        include_str!("skills/shopping/apply_coupons.yaml"),
    );
    m.insert(
        "shopping.price_watch",
        include_str!("skills/shopping/price_watch.yaml"),
    );
    m.insert(
        "shopping.return_request",
        include_str!("skills/shopping/return_request.yaml"),
    );
    m.insert(
        "shopping.gift_recommender",
        include_str!("skills/shopping/gift_recommender.yaml"),
    );
    m.insert(
        "shopping.outfit_from_weather",
        include_str!("skills/shopping/outfit_from_weather.yaml"),
    );
    m
}

/// Check whether a `tool` field references a composite skill.
pub fn is_skill_call(tool: &str) -> bool {
    tool.starts_with("skill.")
}

/// Parse a skill reference `skill.ns.name` → `ns.name` slug.
pub fn skill_slug(tool: &str) -> Option<&str> {
    tool.strip_prefix("skill.")
}

/// Resolve a skill slug to its parsed WorkflowSpec (sub-workflow).
pub fn resolve_skill(slug: &str) -> Result<WorkflowSpec> {
    let skills = bundled_skills();
    let yaml = skills
        .get(slug)
        .copied()
        .with_context(|| format!("unknown skill: {slug}"))?;
    parse_spec(yaml).with_context(|| format!("skill '{slug}' YAML invalid"))
}

/// Expand a ToolCall step into its skill's steps, inlining the args into inputs.
///
/// Returns the sub-workflow's steps ready to be spliced into the caller.
/// Non-skill tool calls return their original step unchanged.
pub fn expand_skill_step(step: &Step) -> Result<Vec<Step>> {
    let Step::ToolCall(tc) = step else {
        return Ok(vec![step.clone()]);
    };
    if !is_skill_call(&tc.tool) {
        return Ok(vec![step.clone()]);
    }
    let slug = skill_slug(&tc.tool).unwrap_or("");
    let sub = resolve_skill(slug)?;

    // Validate that args are an object (skill inputs keyed by name).
    if let Some(obj) = tc.args.as_object() {
        // For Phase 1 we splice raw steps. Full arg mapping (sub.inputs → vars)
        // happens inside the execution engine when the step is dispatched.
        let _ = obj; // args validated by shape; actual binding in exec.rs
    } else if !tc.args.is_null() {
        bail!(
            "skill '{}' args must be an object, got: {}",
            tc.tool,
            tc.args
        );
    }

    Ok(sub.steps)
}

/// Validate that all bundled skills parse successfully.
pub fn validate_bundled_skills() -> Vec<(String, String)> {
    let mut failures = Vec::new();
    for (slug, yaml) in bundled_skills() {
        if let Err(e) = parse_spec(yaml) {
            failures.push((slug.to_string(), format!("{e:#}")));
        }
    }
    failures
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_skill_calls() {
        assert!(is_skill_call("skill.shopping.add_to_cart"));
        assert!(is_skill_call("skill.any.thing"));
        assert!(!is_skill_call("browser"));
        assert!(!is_skill_call("web_fetch"));
    }

    #[test]
    fn extracts_skill_slug() {
        assert_eq!(
            skill_slug("skill.shopping.add_to_cart"),
            Some("shopping.add_to_cart")
        );
        assert_eq!(skill_slug("browser"), None);
    }

    #[test]
    fn bundled_skills_nonempty() {
        let skills = bundled_skills();
        assert!(!skills.is_empty());
        assert!(skills.contains_key("shopping.add_to_cart"));
    }

    #[test]
    fn all_bundled_skills_parse() {
        let failures = validate_bundled_skills();
        assert!(
            failures.is_empty(),
            "Some skills failed to parse: {failures:?}"
        );
    }

    #[test]
    fn resolve_unknown_skill_errors() {
        let result = resolve_skill("nonexistent.skill");
        assert!(result.is_err());
    }

    #[test]
    fn resolve_known_skill_succeeds() {
        let spec = resolve_skill("shopping.add_to_cart").unwrap();
        assert_eq!(spec.parent_category, "shopping");
        assert!(!spec.steps.is_empty());
    }

    #[test]
    fn expand_non_skill_step_passes_through() {
        use super::super::parser::{Step, ToolCallStep};
        let step = Step::ToolCall(ToolCallStep {
            id: "plain".to_string(),
            tool: "browser".to_string(),
            args: serde_json::json!({"action": "goto"}),
        });
        let expanded = expand_skill_step(&step).unwrap();
        assert_eq!(expanded.len(), 1);
    }

    #[test]
    fn expand_skill_step_returns_sub_steps() {
        use super::super::parser::{Step, ToolCallStep};
        let step = Step::ToolCall(ToolCallStep {
            id: "do_search".to_string(),
            tool: "skill.shopping.search_browser".to_string(),
            args: serde_json::json!({"site": "coupang", "query": "keyboard"}),
        });
        let expanded = expand_skill_step(&step).unwrap();
        assert!(!expanded.is_empty());
    }

    #[test]
    fn expand_skill_step_rejects_non_object_args() {
        use super::super::parser::{Step, ToolCallStep};
        let step = Step::ToolCall(ToolCallStep {
            id: "bad".to_string(),
            tool: "skill.shopping.search_browser".to_string(),
            args: serde_json::json!("invalid"),
        });
        let result = expand_skill_step(&step);
        assert!(result.is_err());
    }
}
