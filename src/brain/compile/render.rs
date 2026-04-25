//! Render AGENTS.md / SOUL.md / TOOLS.md as narrative prose.
//!
//! This is the part that fixes the regression — the previous compiler dumped
//! raw YAML inside markdown fences. This renderer reads structured fields and
//! synthesizes prose. Tiny frontmatter only.

use serde_yaml::Value;

use super::agents_api::Agent;
use super::sources::BrainSnapshot;

pub struct AgentBundle {
    pub agents_md: String,
    pub soul_md: String,
    pub tools_md: String,
}

pub fn render_agent(brain: &BrainSnapshot, agent: &Agent) -> AgentBundle {
    let role_match = find_swarm_role(&brain.swarm, agent);
    AgentBundle {
        agents_md: render_agents_md(brain, agent, role_match.as_ref()),
        soul_md: render_soul_md(brain, &agent.name),
        tools_md: render_tools_md(brain, agent, role_match.as_ref()),
    }
}

struct RoleMatch<'a> {
    #[allow(dead_code)]
    key: String,
    role: &'a Value,
}

fn find_swarm_role<'a>(swarm: &'a Value, agent: &Agent) -> Option<RoleMatch<'a>> {
    let agents_map = swarm.get("agents")?.as_mapping()?;
    let needle_full = agent.name.to_lowercase();
    let needle_title = agent.title.as_deref().unwrap_or("").to_lowercase();
    let needle_caps = agent.capabilities.as_deref().unwrap_or("").to_lowercase();

    let mut best: Option<(i32, RoleMatch<'a>)> = None;
    for (k, v) in agents_map {
        let key = k.as_str().unwrap_or("").to_string();
        let role_str = v
            .get("role")
            .and_then(|r| r.as_str())
            .unwrap_or("")
            .to_lowercase();
        let domain = v
            .get("domain")
            .and_then(|r| r.as_str())
            .unwrap_or("")
            .to_lowercase();

        let mut score: i32 = 0;
        let key_short = key.trim_start_matches("v_");
        if !key_short.is_empty()
            && (needle_full.contains(key_short) || needle_title.contains(key_short))
        {
            score += 3;
        }
        if !role_str.is_empty()
            && (needle_full.contains(&role_str) || needle_title.contains(&role_str))
        {
            score += 4;
        }
        for word in role_str.split_whitespace() {
            if word.len() >= 4 && (needle_caps.contains(word) || needle_title.contains(word)) {
                score += 1;
            }
        }
        for word in domain.split_whitespace().take(20) {
            if word.len() >= 5 && needle_caps.contains(word) {
                score += 1;
            }
        }
        if score > 0 {
            match &best {
                Some((s, _)) if *s >= score => {}
                _ => {
                    best = Some((
                        score,
                        RoleMatch {
                            key: key.clone(),
                            role: v,
                        },
                    ));
                }
            }
        }
    }
    best.map(|(_, rm)| rm)
}

fn render_agents_md(brain: &BrainSnapshot, agent: &Agent, role: Option<&RoleMatch<'_>>) -> String {
    let mut out = String::new();
    push_frontmatter(&mut out, brain);

    out.push_str(&format!("# {}\n\n", agent.name));
    if let Some(title) = &agent.title {
        out.push_str(&format!("**Title:** {title}\n\n"));
    }

    if let Some(caps) = &agent.capabilities {
        out.push_str("## Role\n\n");
        out.push_str(caps.trim());
        out.push_str("\n\n");
    }

    if let Some(rm) = role {
        if let Some(domain) = rm.role.get("domain").and_then(|v| v.as_str()) {
            out.push_str("## Domain\n\n");
            out.push_str(domain.trim());
            out.push_str("\n\n");
        }
        if let Some(tier) = rm.role.get("tier").and_then(|v| v.as_u64()) {
            out.push_str(&format!("**Tier:** {tier} ({})\n\n", tier_label(tier)));
        }

        push_string_seq(&mut out, "## Observes", rm.role.get("observes"));
        push_string_seq(&mut out, "## Produces", rm.role.get("produces"));
        push_string_seq(
            &mut out,
            "## Triggers escalation",
            rm.role.get("escalates_when"),
        );

        if let Some(self_heal) = rm.role.get("self_heal").and_then(|v| v.as_str()) {
            out.push_str("## Self-heal\n\n");
            out.push_str(self_heal.trim());
            out.push_str("\n\n");
        }
    }

    if let Some(reports_to) = &agent.reports_to {
        out.push_str("## Reports to\n\n");
        out.push_str(&format!("Agent `{reports_to}`\n\n"));
    }

    push_judgment_modes(&mut out, &brain.soul_judgment);
    push_decision_framework(&mut out, &brain.soul_mind);

    out
}

fn render_soul_md(brain: &BrainSnapshot, agent_name: &str) -> String {
    let mut out = String::new();
    push_frontmatter(&mut out, brain);
    out.push_str(&format!("# Soul — {}\n\n", agent_name));

    out.push_str("## Mind\n\n");
    if let Some(thesis) = brain.soul_mind.get("thesis").and_then(|v| v.as_str()) {
        out.push_str(thesis.trim());
        out.push_str("\n\n");
    }
    if let Some(htw) = brain
        .soul_mind
        .get("how_we_think")
        .and_then(|v| v.as_sequence())
    {
        out.push_str("**How we think.**\n\n");
        for item in htw {
            if let Some(s) = item.as_str() {
                out.push_str(&format!("- {}\n", s.trim()));
            }
        }
        out.push_str("\n");
    }

    out.push_str("## Voice\n\n");
    if let Some(comm) = brain
        .soul_voice
        .get("communication")
        .and_then(|v| v.as_sequence())
    {
        for item in comm {
            if let Some(s) = item.as_str() {
                out.push_str(&format!("- {}\n", s.trim()));
            }
        }
        out.push_str("\n");
    }
    if let Some(avoid) = brain
        .soul_voice
        .get("words_reveal_decisions")
        .and_then(|v| v.get("avoid"))
        .and_then(|v| v.as_sequence())
    {
        out.push_str("**Words to swap.**\n\n");
        out.push_str("| Avoid | Use instead |\n|---|---|\n");
        for item in avoid {
            let pat = item.get("pattern").and_then(|v| v.as_str()).unwrap_or("");
            let fix = item.get("fix").and_then(|v| v.as_str()).unwrap_or("");
            if !pat.is_empty() {
                out.push_str(&format!("| {} | {} |\n", pat, fix));
            }
        }
        out.push_str("\n");
    }

    out.push_str("## Judgment\n\n");
    if let Some(default_bias) = brain
        .soul_judgment
        .get("default_bias")
        .and_then(|v| v.as_str())
    {
        out.push_str(&format!("**Default bias:** `{}`.\n\n", default_bias));
    }
    if let Some(bias_rule) = brain
        .soul_judgment
        .get("bias_rule")
        .and_then(|v| v.as_str())
    {
        out.push_str(bias_rule.trim());
        out.push_str("\n\n");
    }

    if let Some(philosophy) = brain
        .soul_aesthetic
        .get("seeing_philosophy")
        .and_then(|v| v.as_str())
    {
        out.push_str("## Aesthetic\n\n");
        out.push_str(philosophy.trim());
        out.push_str("\n\n");
    }

    out
}

fn render_tools_md(brain: &BrainSnapshot, agent: &Agent, role: Option<&RoleMatch<'_>>) -> String {
    let mut out = String::new();
    push_frontmatter(&mut out, brain);
    out.push_str(&format!("# Tools — {}\n\n", agent.name));

    if let Some(rm) = role {
        if let Some(channels) = rm.role.get("channels").and_then(|v| v.as_sequence()) {
            let chs: Vec<String> = channels
                .iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect();
            if !chs.is_empty() {
                out.push_str("## Channels\n\n");
                out.push_str(&format!("{}\n\n", chs.join(", ")));
            }
        }
    }

    out.push_str("## Plugin tools\n\n");
    out.push_str("| Tool | Purpose |\n|---|---|\n");
    out.push_str("| `brain.query` | Vector search over `~/.brain/` via Augusta. Read-only. |\n");
    out.push_str("| `workgraph.epic.*` | Create / list Epics scoped to your company. |\n");
    out.push_str("| `workgraph.sprint.*` | Create / list Sprints under an Epic. |\n");
    out.push_str("| `workgraph.story.*` | Create / list / update / assign Stories. |\n");
    out.push_str("| `pool.claim` | Claim the next ready Story for a worker. |\n");
    out.push_str("| `pool.release` | Release a claimed Story (done/failed/review). |\n");
    out.push_str("| `pool.list` | View ready and in-progress Stories for an agent. |\n\n");

    out.push_str("## Skills\n\n");
    if let Some(skills) = brain
        .skills_index
        .get("skills")
        .and_then(|v| v.as_sequence())
    {
        out.push_str("| Skill | Type | Runtime | Trigger |\n|---|---|---|---|\n");
        for s in skills {
            let id = s.get("id").and_then(|v| v.as_str()).unwrap_or("?");
            let ty = s.get("type").and_then(|v| v.as_str()).unwrap_or("?");
            let rt = s.get("runtime").and_then(|v| v.as_str()).unwrap_or("?");
            let trig = s.get("trigger").and_then(|v| v.as_str()).unwrap_or("");
            out.push_str(&format!("| `{id}` | {ty} | {rt} | {} |\n", oneline(trig)));
        }
        out.push_str("\n");
    }

    out
}

fn push_frontmatter(out: &mut String, brain: &BrainSnapshot) {
    out.push_str("---\n");
    out.push_str("generated_by: augusta brain compile\n");
    out.push_str(&format!("brain_sha: {}\n", &brain.brain_sha[..16]));
    out.push_str("---\n\n");
}

fn push_string_seq(out: &mut String, heading: &str, value: Option<&Value>) {
    let Some(seq) = value.and_then(|v| v.as_sequence()) else {
        return;
    };
    if seq.is_empty() {
        return;
    }
    out.push_str(heading);
    out.push_str("\n\n");
    for item in seq {
        if let Some(s) = item.as_str() {
            out.push_str(&format!("- {}\n", s.trim()));
        }
    }
    out.push_str("\n");
}

fn push_judgment_modes(out: &mut String, doc: &Value) {
    let Some(modes) = doc.get("modes").and_then(|v| v.as_mapping()) else {
        return;
    };
    out.push_str("## Judgment modes\n\n");
    for (k, v) in modes {
        let name = k.as_str().unwrap_or("?");
        let desc = v.get("description").and_then(|x| x.as_str()).unwrap_or("");
        let pattern = v.get("pattern").and_then(|x| x.as_str()).unwrap_or("");
        out.push_str(&format!("- **{name}** — {}", oneline(desc)));
        if !pattern.is_empty() {
            out.push_str(&format!(" _Pattern:_ {}", oneline(pattern)));
        }
        out.push_str("\n");
    }
    out.push_str("\n");
}

fn push_decision_framework(out: &mut String, doc: &Value) {
    let Some(qs) = doc
        .get("decision_framework")
        .and_then(|v| v.get("questions"))
        .and_then(|v| v.as_sequence())
    else {
        return;
    };
    out.push_str("## Decision framework\n\n");
    for q in qs {
        let order = q.get("order").and_then(|v| v.as_u64()).unwrap_or(0);
        let question = q.get("question").and_then(|v| v.as_str()).unwrap_or("");
        let descr = q.get("description").and_then(|v| v.as_str()).unwrap_or("");
        out.push_str(&format!("{}. **{question}** — {}\n", order, oneline(descr)));
    }
    if let Some(validation) = doc
        .get("decision_framework")
        .and_then(|v| v.get("validation"))
        .and_then(|v| v.as_str())
    {
        out.push_str(&format!("\n_Validation:_ {validation}\n"));
    }
    out.push_str("\n");
}

fn oneline(s: &str) -> String {
    s.replace('\n', " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn tier_label(tier: u64) -> &'static str {
    match tier {
        1 => "Always-On",
        2 => "Reactive",
        3 => "Background",
        _ => "?",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn yaml(s: &str) -> Value {
        serde_yaml::from_str(s).unwrap()
    }

    fn fixture_brain() -> BrainSnapshot {
        BrainSnapshot {
            soul_mind: yaml(
                r#"
thesis: Amplify perspective. Never generate one.
how_we_think:
  - Embrace reality.
  - Cultivate skepticism.
decision_framework:
  questions:
    - order: 1
      question: What do you want?
      description: Clarify the goal.
    - order: 2
      question: What is true?
      description: Assess reality.
  validation: How do you know that is true?
"#,
            ),
            soul_voice: yaml(
                r#"
communication:
  - Lead with the headline.
  - Numbers, not adjectives.
words_reveal_decisions:
  avoid:
    - pattern: "Should I?"
      fix: State what you'd do and why.
    - pattern: "I think"
      fix: State what IS with evidence.
"#,
            ),
            soul_judgment: yaml(
                r#"
default_bias: act_first
bias_rule: When in doubt, act.
modes:
  act_first:
    description: Default mode.
    pattern: Show the work.
  ask_first:
    description: Strategic.
    pattern: Clarify before acting.
"#,
            ),
            soul_aesthetic: yaml("seeing_philosophy: Patience is practice.\n"),
            swarm: yaml(
                r#"
agents:
  v_legal:
    role: Legal counsel
    domain: contracts compliance
    tier: 2
    observes:
      - new contracts
    produces:
      - contract reviews
    escalates_when:
      - novel legal exposure
    self_heal: Re-read the contract.
    channels:
      - slack
      - email
"#,
            ),
            agile_framework: Value::Null,
            messaging_safety: Value::Null,
            skills_index: yaml(
                r#"
skills:
  - id: gmail-draft
    type: action
    runtime: paperclip
    trigger: User asks to send email.
"#,
            ),
            brain_sha: "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789".into(),
        }
    }

    fn agent_legal() -> Agent {
        Agent {
            id: "agent-1".into(),
            company_id: "co-1".into(),
            name: "Legal".into(),
            title: Some("Legal counsel".into()),
            capabilities: Some("Reviews contracts and flags compliance risk.".into()),
            reports_to: Some("agent-gm".into()),
        }
    }

    #[test]
    fn render_agent_produces_three_files_with_frontmatter() {
        let brain = fixture_brain();
        let bundle = render_agent(&brain, &agent_legal());
        for md in [&bundle.agents_md, &bundle.soul_md, &bundle.tools_md] {
            assert!(md.starts_with("---\n"), "missing frontmatter: {md:.80}");
            assert!(md.contains("brain_sha: abcdef0123456789"));
        }
    }

    #[test]
    fn agents_md_contains_role_domain_tier_and_reports_to() {
        let bundle = render_agent(&fixture_brain(), &agent_legal());
        let md = &bundle.agents_md;
        assert!(md.contains("# Legal"));
        assert!(md.contains("**Title:** Legal counsel"));
        assert!(md.contains("Reviews contracts and flags compliance risk."));
        assert!(md.contains("## Domain"));
        assert!(md.contains("contracts compliance"));
        assert!(md.contains("**Tier:** 2 (Reactive)"));
        assert!(md.contains("- new contracts"));
        assert!(md.contains("- contract reviews"));
        assert!(md.contains("- novel legal exposure"));
        assert!(md.contains("Re-read the contract."));
        assert!(md.contains("Agent `agent-gm`"));
    }

    #[test]
    fn agents_md_includes_judgment_modes_and_decision_framework() {
        let bundle = render_agent(&fixture_brain(), &agent_legal());
        let md = &bundle.agents_md;
        assert!(md.contains("## Judgment modes"));
        assert!(md.contains("**act_first**"));
        assert!(md.contains("**ask_first**"));
        assert!(md.contains("## Decision framework"));
        assert!(md.contains("1. **What do you want?**"));
        assert!(md.contains("_Validation:_ How do you know that is true?"));
    }

    #[test]
    fn soul_md_contains_mind_voice_judgment_aesthetic() {
        let bundle = render_agent(&fixture_brain(), &agent_legal());
        let md = &bundle.soul_md;
        assert!(md.contains("# Soul — Legal"));
        assert!(md.contains("## Mind"));
        assert!(md.contains("Amplify perspective."));
        assert!(md.contains("- Embrace reality."));
        assert!(md.contains("## Voice"));
        assert!(md.contains("- Lead with the headline."));
        assert!(md.contains("**Words to swap.**"));
        assert!(md.contains("| Should I? | State what you'd do and why. |"));
        assert!(md.contains("## Judgment"));
        assert!(md.contains("**Default bias:** `act_first`"));
        assert!(md.contains("When in doubt, act."));
        assert!(md.contains("## Aesthetic"));
        assert!(md.contains("Patience is practice."));
    }

    #[test]
    fn tools_md_contains_channels_plugin_tools_and_skills() {
        let bundle = render_agent(&fixture_brain(), &agent_legal());
        let md = &bundle.tools_md;
        assert!(md.contains("# Tools — Legal"));
        assert!(md.contains("## Channels"));
        assert!(md.contains("slack, email"));
        assert!(md.contains("## Plugin tools"));
        assert!(md.contains("`brain.query`"));
        assert!(md.contains("`pool.claim`"));
        assert!(md.contains("## Skills"));
        assert!(md.contains("`gmail-draft`"));
    }

    #[test]
    fn no_raw_yaml_fences_in_output() {
        let bundle = render_agent(&fixture_brain(), &agent_legal());
        for md in [&bundle.agents_md, &bundle.soul_md, &bundle.tools_md] {
            assert!(!md.contains("```yaml"), "raw yaml fence leaked: {md:.200}");
            assert!(!md.contains("```yml"), "raw yml fence leaked");
        }
    }

    #[test]
    fn agent_with_no_swarm_match_still_renders_basic_sections() {
        let brain = fixture_brain();
        let agent = Agent {
            id: "agent-x".into(),
            company_id: "co-1".into(),
            name: "WildcardAgent".into(),
            title: None,
            capabilities: None,
            reports_to: None,
        };
        let bundle = render_agent(&brain, &agent);
        assert!(bundle.agents_md.contains("# WildcardAgent"));
        // No domain/tier sections expected when no role match.
        assert!(!bundle.agents_md.contains("## Domain"));
        assert!(!bundle.agents_md.contains("**Tier:**"));
        // Soul still renders.
        assert!(bundle.soul_md.contains("# Soul — WildcardAgent"));
    }
}
