//! Default starter templates for the per-workspace personality files.

use super::personality::EDITABLE_PERSONALITY_FILES;
use std::path::Path;

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

pub async fn ensure_personality_preset(
    workspace_dir: &Path,
    ctx: &TemplateContext,
) -> std::io::Result<Vec<&'static str>> {
    let mut written = Vec::new();
    for (filename, content) in render_preset_default(ctx) {
        let path = workspace_dir.join(filename);
        let needs_seed = match tokio::fs::read_to_string(&path).await {
            Ok(existing) => existing.trim().is_empty(),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => true,
            Err(err) => return Err(err),
        };
        if needs_seed {
            tokio::fs::write(&path, &content).await?;
            written.push(filename);
        }
    }
    Ok(written)
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
    fn including_memory_renders_memory_md() {
        let ctx = TemplateContext {
            include_memory: true,
            ..TemplateContext::default()
        };
        let rendered = render_preset_default(&ctx);

        assert!(
            rendered.iter().any(|(n, _)| *n == "MEMORY.md"),
            "MEMORY.md should be available for full per-agent template renders"
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
        assert!(
            on.contains("Daily notes"),
            "memory-on AGENTS.md must mention daily notes"
        );
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

    #[tokio::test]
    async fn ensure_preset_seeds_every_editable_file_with_substitution() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = TemplateContext {
            agent: "Nova".to_string(),
            ..TemplateContext::default()
        };
        let written = ensure_personality_preset(dir.path(), &ctx).await.unwrap();

        // The full editable set lands on disk, populated (not empty).
        for f in EDITABLE_PERSONALITY_FILES {
            assert!(written.contains(f), "preset should seed {f}");
            let body = tokio::fs::read_to_string(dir.path().join(f)).await.unwrap();
            assert!(!body.trim().is_empty(), "{f} must be populated, not blank");
        }
        // Substitution actually ran — no raw placeholders left behind.
        let soul = tokio::fs::read_to_string(dir.path().join("SOUL.md"))
            .await
            .unwrap();
        assert!(soul.contains("You are **Nova**"));
        assert!(!soul.contains("{agent}"));
    }

    #[tokio::test]
    async fn ensure_preset_preserves_existing_user_content() {
        let dir = tempfile::tempdir().unwrap();
        let custom = "# My own SOUL\nHand-written.\n";
        tokio::fs::write(dir.path().join("SOUL.md"), custom)
            .await
            .unwrap();

        let written = ensure_personality_preset(dir.path(), &TemplateContext::default())
            .await
            .unwrap();

        assert!(
            !written.contains(&"SOUL.md"),
            "files with real content must never be overwritten"
        );
        let soul = tokio::fs::read_to_string(dir.path().join("SOUL.md"))
            .await
            .unwrap();
        assert_eq!(soul, custom);
    }

    #[tokio::test]
    async fn ensure_preset_reseeds_blank_files() {
        let dir = tempfile::tempdir().unwrap();
        // A stray 0-byte / whitespace-only file is what the existence-only
        // guard could never heal; the preset must reseed it.
        tokio::fs::write(dir.path().join("AGENTS.md"), "   \n\t\n")
            .await
            .unwrap();

        let written = ensure_personality_preset(dir.path(), &TemplateContext::default())
            .await
            .unwrap();

        assert!(
            written.contains(&"AGENTS.md"),
            "blank file must be reseeded, not skipped"
        );
        let agents = tokio::fs::read_to_string(dir.path().join("AGENTS.md"))
            .await
            .unwrap();
        assert!(!agents.trim().is_empty());
    }
}
