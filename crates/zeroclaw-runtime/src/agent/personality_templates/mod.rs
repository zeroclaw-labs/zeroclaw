//! Default starter templates for the per-workspace personality files.
//!
//! Recovered verbatim from the pre-#5951 onboarding wizard's
//! `scaffold_workspace()` (commit `0c622e607^:crates/zeroclaw-runtime/src/onboard/wizard.rs`).
//! The wizard rewrite (#5960) shipped without a workspace-scaffolder, so
//! these templates were dormant in git history. They are restored here
//! for the dashboard's Personality onboarding step (#6175 follow-up) and
//! exposed via `GET /api/personality/templates`.
//!
//! Each `*.md` file in this directory is the literal template; they
//! get embedded via `include_str!` and substituted with values from
//! [`TemplateContext`] at render time. `AGENTS.md` has two variants
//! (regular and `no-memory`) since it's the only file whose body
//! changes based on whether persistent memory is enabled.
//!
//! Placeholders: `{agent}`, `{user}`, `{tz}`, `{comm_style}`. They
//! render harmlessly as plain text if a `.md` file is previewed in
//! GitHub.

use super::personality::EDITABLE_PERSONALITY_FILES;

const IDENTITY: &str = include_str!("IDENTITY.md");
const SOUL: &str = include_str!("SOUL.md");
const USER: &str = include_str!("USER.md");
const HEARTBEAT: &str = include_str!("HEARTBEAT.md");
const TOOLS: &str = include_str!("TOOLS.md");
const MEMORY: &str = include_str!("MEMORY.md");
const AGENTS: &str = include_str!("AGENTS.md");
const AGENTS_NO_MEMORY: &str = include_str!("AGENTS.no-memory.md");

/// Per-render context — substituted into the templates' placeholders.
/// Values default to neutral placeholders the user can edit in-place
/// once the template is loaded into the editor.
#[derive(Debug, Clone)]
pub struct TemplateContext {
    pub agent: String,
    pub user: String,
    pub timezone: String,
    pub communication_style: String,
    /// When `false`, omits MEMORY.md from the rendered set and renders
    /// the no-memory variant of AGENTS.md.
    pub include_memory: bool,
}

impl Default for TemplateContext {
    fn default() -> Self {
        Self {
            agent: "ZeroClaw".to_string(),
            user: "User".to_string(),
            timezone: "UTC".to_string(),
            communication_style:
                "Be warm, natural, and clear. Use occasional relevant emojis (1-2 max) and avoid robotic phrasing."
                    .to_string(),
            include_memory: true,
        }
    }
}

fn substitute(template: &str, ctx: &TemplateContext) -> String {
    template
        .replace("{agent}", &ctx.agent)
        .replace("{user}", &ctx.user)
        .replace("{tz}", &ctx.timezone)
        .replace("{comm_style}", &ctx.communication_style)
}

/// Render one personality file from the default preset, or `None` when
/// the filename is outside the editable allowlist (or when MEMORY.md
/// is requested with `include_memory = false`).
///
/// `BOOTSTRAP.md` is intentionally not rendered — it's a first-run
/// scaffold the agent reads once and deletes; the dashboard editor
/// doesn't expose it. The original wizard owned BOOTSTRAP.md
/// generation directly during workspace scaffolding.
#[must_use]
pub fn render(filename: &str, ctx: &TemplateContext) -> Option<String> {
    let raw = match filename {
        "IDENTITY.md" => IDENTITY,
        "SOUL.md" => SOUL,
        "USER.md" => USER,
        "HEARTBEAT.md" => HEARTBEAT,
        "TOOLS.md" => TOOLS,
        "AGENTS.md" => {
            if ctx.include_memory {
                AGENTS
            } else {
                AGENTS_NO_MEMORY
            }
        }
        "MEMORY.md" if ctx.include_memory => MEMORY,
        _ => return None,
    };
    Some(substitute(raw, ctx))
}

/// Render the full default preset for every editable file.
#[must_use]
pub fn render_preset_default(ctx: &TemplateContext) -> Vec<(&'static str, String)> {
    EDITABLE_PERSONALITY_FILES
        .iter()
        .copied()
        .filter_map(|f| render(f, ctx).map(|content| (f, content)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_preset_covers_every_editable_file() {
        let ctx = TemplateContext::default();
        let rendered = render_preset_default(&ctx);
        let names: Vec<&str> = rendered.iter().map(|(n, _)| *n).collect();
        for f in EDITABLE_PERSONALITY_FILES {
            assert!(
                names.contains(f),
                "default preset missing {f}; only had {names:?}"
            );
        }
    }

    #[test]
    fn bootstrap_is_not_a_template() {
        let ctx = TemplateContext::default();
        assert!(
            render("BOOTSTRAP.md", &ctx).is_none(),
            "BOOTSTRAP.md is owned by first-run scaffolding, not the editor"
        );
    }

    #[test]
    fn excluding_memory_drops_memory_md() {
        let ctx = TemplateContext {
            include_memory: false,
            ..TemplateContext::default()
        };
        let rendered = render_preset_default(&ctx);
        assert!(
            !rendered.iter().any(|(n, _)| *n == "MEMORY.md"),
            "MEMORY.md should be skipped when include_memory = false"
        );
    }

    #[test]
    fn excluding_memory_picks_no_memory_agents_variant() {
        let on = render(
            "AGENTS.md",
            &TemplateContext {
                include_memory: true,
                ..TemplateContext::default()
            },
        )
        .unwrap();
        let off = render(
            "AGENTS.md",
            &TemplateContext {
                include_memory: false,
                ..TemplateContext::default()
            },
        )
        .unwrap();
        assert!(on.contains("Daily notes"), "memory-on AGENTS.md must mention daily notes");
        assert!(
            off.contains("memory.backend = \"none\""),
            "memory-off AGENTS.md must mention disabled memory"
        );
    }

    #[test]
    fn substitutes_agent_name_into_soul() {
        let ctx = TemplateContext {
            agent: "Nova".to_string(),
            ..TemplateContext::default()
        };
        let soul = render("SOUL.md", &ctx).unwrap();
        assert!(soul.contains("You are **Nova**"));
        assert!(soul.contains("Always introduce yourself as Nova"));
    }

    #[test]
    fn unknown_filename_returns_none() {
        let ctx = TemplateContext::default();
        assert!(render("OTHER.md", &ctx).is_none());
    }
}
