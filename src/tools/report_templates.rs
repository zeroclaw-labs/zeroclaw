//! Report template engine for project delivery intelligence.
//!
//! Provides built-in templates for weekly status, sprint review, risk register,
//! and milestone reports with multi-language support (EN, DE, FR, IT).

use std::collections::HashMap;

/// Supported report output formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReportFormat {
    Markdown,
    Html,
}

/// A named section within a report template.
#[derive(Debug, Clone)]
pub struct TemplateSection {
    pub heading: String,
    pub body: String,
}

/// A report template with named sections and variable placeholders.
#[derive(Debug, Clone)]
pub struct ReportTemplate {
    pub name: String,
    pub sections: Vec<TemplateSection>,
    pub format: ReportFormat,
}

/// Escape a string for safe inclusion in HTML output.
fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#x27;")
}

impl ReportTemplate {
    /// Render the template by substituting `{{key}}` placeholders with values.
    pub fn render(&self, vars: &HashMap<String, String>) -> String {
        let mut out = String::new();
        for section in &self.sections {
            let heading = substitute(&section.heading, vars);
            let body = substitute(&section.body, vars);
            match self.format {
                ReportFormat::Markdown => {
                    out.push_str(&format!("## {heading}\n\n{body}\n\n"));
                }
                ReportFormat::Html => {
                    let heading = escape_html(&heading);
                    let body = escape_html(&body);
                    out.push_str(&format!("<h2>{heading}</h2>\n<p>{body}</p>\n"));
                }
            }
        }
        out.trim_end().to_string()
    }
}

/// Single-pass placeholder substitution.
///
/// Scans `template` left-to-right for `{{key}}` tokens and replaces them with
/// the corresponding value from `vars`.  Because the scan is single-pass,
/// values that themselves contain `{{...}}` sequences are emitted literally
/// and never re-expanded, preventing injection of new placeholders.
fn substitute(template: &str, vars: &HashMap<String, String>) -> String {
    let mut result = String::with_capacity(template.len());
    let bytes = template.as_bytes();
    let len = bytes.len();
    let mut i = 0;

    while i < len {
        if i + 1 < len && bytes[i] == b'{' && bytes[i + 1] == b'{' {
            // Find the closing `}}`.
            if let Some(close) = template[i + 2..].find("}}") {
                let key = &template[i + 2..i + 2 + close];
                if let Some(value) = vars.get(key) {
                    result.push_str(value);
                } else {
                    // Unknown placeholder: emit as-is.
                    result.push_str(&template[i..i + 2 + close + 2]);
                }
                i += 2 + close + 2;
                continue;
            }
        }
        result.push(template.as_bytes()[i] as char);
        i += 1;
    }

    result
}

// ── Built-in templates ────────────────────────────────────────────

/// Return the built-in weekly status template for the given language.
pub fn weekly_status_template(lang: &str) -> ReportTemplate {
    let (name, sections) = match lang {
        "de" => (
            "Wochenstatus",
            vec![
                TemplateSection {
                    heading: "Zusammenfassung".into(),
                    body: "Projekt: {{project_name}} | Zeitraum: {{period}}".into(),
                },
                TemplateSection {
                    heading: "Erledigt".into(),
                    body: "{{completed}}".into(),
                },
                TemplateSection {
                    heading: "In Bearbeitung".into(),
                    body: "{{in_progress}}".into(),
                },
                TemplateSection {
                    heading: "Blockiert".into(),
                    body: "{{blocked}}".into(),
                },
                TemplateSection {
                    heading: "Naechste Schritte".into(),
                    body: "{{next_steps}}".into(),
                },
            ],
        ),
        "fr" => (
            "Statut hebdomadaire",
            vec![
                TemplateSection {
                    heading: "Resume".into(),
                    body: "Projet: {{project_name}} | Periode: {{period}}".into(),
                },
                TemplateSection {
                    heading: "Termine".into(),
                    body: "{{completed}}".into(),
                },
                TemplateSection {
                    heading: "En cours".into(),
                    body: "{{in_progress}}".into(),
                },
                TemplateSection {
                    heading: "Bloque".into(),
                    body: "{{blocked}}".into(),
                },
                TemplateSection {
                    heading: "Prochaines etapes".into(),
                    body: "{{next_steps}}".into(),
                },
            ],
        ),
        "it" => (
            "Stato settimanale",
            vec![
                TemplateSection {
                    heading: "Riepilogo".into(),
                    body: "Progetto: {{project_name}} | Periodo: {{period}}".into(),
                },
                TemplateSection {
                    heading: "Completato".into(),
                    body: "{{completed}}".into(),
                },
                TemplateSection {
                    heading: "In corso".into(),
                    body: "{{in_progress}}".into(),
                },
                TemplateSection {
                    heading: "Bloccato".into(),
                    body: "{{blocked}}".into(),
                },
                TemplateSection {
                    heading: "Prossimi passi".into(),
                    body: "{{next_steps}}".into(),
                },
            ],
        ),
        _ => (
            "Weekly Status",
            vec![
                TemplateSection {
                    heading: "Summary".into(),
                    body: "Project: {{project_name}} | Period: {{period}}".into(),
                },
                TemplateSection {
                    heading: "Completed".into(),
                    body: "{{completed}}".into(),
                },
                TemplateSection {
                    heading: "In Progress".into(),
                    body: "{{in_progress}}".into(),
                },
                TemplateSection {
                    heading: "Blocked".into(),
                    body: "{{blocked}}".into(),
                },
                TemplateSection {
                    heading: "Next Steps".into(),
                    body: "{{next_steps}}".into(),
                },
            ],
        ),
    };
    ReportTemplate {
        name: name.into(),
        sections,
        format: ReportFormat::Markdown,
    }
}

/// Return the built-in sprint review template for the given language.
pub fn sprint_review_template(lang: &str) -> ReportTemplate {
    let (name, sections) = match lang {
        "de" => (
            "Sprint-Uebersicht",
            vec![
                TemplateSection {
                    heading: "Sprint".into(),
                    body: "{{sprint_dates}}".into(),
                },
                TemplateSection {
                    heading: "Erledigt".into(),
                    body: "{{completed}}".into(),
                },
                TemplateSection {
                    heading: "In Bearbeitung".into(),
                    body: "{{in_progress}}".into(),
                },
                TemplateSection {
                    heading: "Blockiert".into(),
                    body: "{{blocked}}".into(),
                },
                TemplateSection {
                    heading: "Velocity".into(),
                    body: "{{velocity}}".into(),
                },
            ],
        ),
        "fr" => (
            "Revue de sprint",
            vec![
                TemplateSection {
                    heading: "Sprint".into(),
                    body: "{{sprint_dates}}".into(),
                },
                TemplateSection {
                    heading: "Termine".into(),
                    body: "{{completed}}".into(),
                },
                TemplateSection {
                    heading: "En cours".into(),
                    body: "{{in_progress}}".into(),
                },
                TemplateSection {
                    heading: "Bloque".into(),
                    body: "{{blocked}}".into(),
                },
                TemplateSection {
                    heading: "Velocite".into(),
                    body: "{{velocity}}".into(),
                },
            ],
        ),
        "it" => (
            "Revisione sprint",
            vec![
                TemplateSection {
                    heading: "Sprint".into(),
                    body: "{{sprint_dates}}".into(),
                },
                TemplateSection {
                    heading: "Completato".into(),
                    body: "{{completed}}".into(),
                },
                TemplateSection {
                    heading: "In corso".into(),
                    body: "{{in_progress}}".into(),
                },
                TemplateSection {
                    heading: "Bloccato".into(),
                    body: "{{blocked}}".into(),
                },
                TemplateSection {
                    heading: "Velocita".into(),
                    body: "{{velocity}}".into(),
                },
            ],
        ),
        _ => (
            "Sprint Review",
            vec![
                TemplateSection {
                    heading: "Sprint".into(),
                    body: "{{sprint_dates}}".into(),
                },
                TemplateSection {
                    heading: "Completed".into(),
                    body: "{{completed}}".into(),
                },
                TemplateSection {
                    heading: "In Progress".into(),
                    body: "{{in_progress}}".into(),
                },
                TemplateSection {
                    heading: "Blocked".into(),
                    body: "{{blocked}}".into(),
                },
                TemplateSection {
                    heading: "Velocity".into(),
                    body: "{{velocity}}".into(),
                },
            ],
        ),
    };
    ReportTemplate {
        name: name.into(),
        sections,
        format: ReportFormat::Markdown,
    }
}

/// Return the built-in risk register template for the given language.
pub fn risk_register_template(lang: &str) -> ReportTemplate {
    let (name, sections) = match lang {
        "de" => (
            "Risikoregister",
            vec![
                TemplateSection {
                    heading: "Projekt".into(),
                    body: "{{project_name}}".into(),
                },
                TemplateSection {
                    heading: "Risiken".into(),
                    body: "{{risks}}".into(),
                },
                TemplateSection {
                    heading: "Massnahmen".into(),
                    body: "{{mitigations}}".into(),
                },
            ],
        ),
        "fr" => (
            "Registre des risques",
            vec![
                TemplateSection {
                    heading: "Projet".into(),
                    body: "{{project_name}}".into(),
                },
                TemplateSection {
                    heading: "Risques".into(),
                    body: "{{risks}}".into(),
                },
                TemplateSection {
                    heading: "Mesures".into(),
                    body: "{{mitigations}}".into(),
                },
            ],
        ),
        "it" => (
            "Registro dei rischi",
            vec![
                TemplateSection {
                    heading: "Progetto".into(),
                    body: "{{project_name}}".into(),
                },
                TemplateSection {
                    heading: "Rischi".into(),
                    body: "{{risks}}".into(),
                },
                TemplateSection {
                    heading: "Mitigazioni".into(),
                    body: "{{mitigations}}".into(),
                },
            ],
        ),
        _ => (
            "Risk Register",
            vec![
                TemplateSection {
                    heading: "Project".into(),
                    body: "{{project_name}}".into(),
                },
                TemplateSection {
                    heading: "Risks".into(),
                    body: "{{risks}}".into(),
                },
                TemplateSection {
                    heading: "Mitigations".into(),
                    body: "{{mitigations}}".into(),
                },
            ],
        ),
    };
    ReportTemplate {
        name: name.into(),
        sections,
        format: ReportFormat::Markdown,
    }
}

/// Return the built-in milestone report template for the given language.
pub fn milestone_report_template(lang: &str) -> ReportTemplate {
    let (name, sections) = match lang {
        "de" => (
            "Meilensteinbericht",
            vec![
                TemplateSection {
                    heading: "Projekt".into(),
                    body: "{{project_name}}".into(),
                },
                TemplateSection {
                    heading: "Meilensteine".into(),
                    body: "{{milestones}}".into(),
                },
                TemplateSection {
                    heading: "Status".into(),
                    body: "{{status}}".into(),
                },
            ],
        ),
        "fr" => (
            "Rapport de jalons",
            vec![
                TemplateSection {
                    heading: "Projet".into(),
                    body: "{{project_name}}".into(),
                },
                TemplateSection {
                    heading: "Jalons".into(),
                    body: "{{milestones}}".into(),
                },
                TemplateSection {
                    heading: "Statut".into(),
                    body: "{{status}}".into(),
                },
            ],
        ),
        "it" => (
            "Report milestone",
            vec![
                TemplateSection {
                    heading: "Progetto".into(),
                    body: "{{project_name}}".into(),
                },
                TemplateSection {
                    heading: "Milestone".into(),
                    body: "{{milestones}}".into(),
                },
                TemplateSection {
                    heading: "Stato".into(),
                    body: "{{status}}".into(),
                },
            ],
        ),
        _ => (
            "Milestone Report",
            vec![
                TemplateSection {
                    heading: "Project".into(),
                    body: "{{project_name}}".into(),
                },
                TemplateSection {
                    heading: "Milestones".into(),
                    body: "{{milestones}}".into(),
                },
                TemplateSection {
                    heading: "Status".into(),
                    body: "{{status}}".into(),
                },
            ],
        ),
    };
    ReportTemplate {
        name: name.into(),
        sections,
        format: ReportFormat::Markdown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn weekly_status_renders_with_variables() {
        let tpl = weekly_status_template("en");
        let mut vars = HashMap::new();
        vars.insert("project_name".into(), "ZeroClaw".into());
        vars.insert("period".into(), "2026-W10".into());
        vars.insert("completed".into(), "- Task A\n- Task B".into());
        vars.insert("in_progress".into(), "- Task C".into());
        vars.insert("blocked".into(), "None".into());
        vars.insert("next_steps".into(), "- Task D".into());

        let rendered = tpl.render(&vars);
        assert!(rendered.contains("Project: ZeroClaw"));
        assert!(rendered.contains("Period: 2026-W10"));
        assert!(rendered.contains("- Task A"));
        assert!(rendered.contains("## Completed"));
    }

    #[test]
    fn weekly_status_de_renders_german_headings() {
        let tpl = weekly_status_template("de");
        let vars = HashMap::new();
        let rendered = tpl.render(&vars);
        assert!(rendered.contains("## Zusammenfassung"));
        assert!(rendered.contains("## Erledigt"));
    }

    #[test]
    fn weekly_status_fr_renders_french_headings() {
        let tpl = weekly_status_template("fr");
        let vars = HashMap::new();
        let rendered = tpl.render(&vars);
        assert!(rendered.contains("## Resume"));
        assert!(rendered.contains("## Termine"));
    }

    #[test]
    fn weekly_status_it_renders_italian_headings() {
        let tpl = weekly_status_template("it");
        let vars = HashMap::new();
        let rendered = tpl.render(&vars);
        assert!(rendered.contains("## Riepilogo"));
        assert!(rendered.contains("## Completato"));
    }

    #[test]
    fn html_format_renders_tags() {
        let mut tpl = weekly_status_template("en");
        tpl.format = ReportFormat::Html;
        let mut vars = HashMap::new();
        vars.insert("project_name".into(), "Test".into());
        vars.insert("period".into(), "W1".into());
        vars.insert("completed".into(), "Done".into());
        vars.insert("in_progress".into(), "WIP".into());
        vars.insert("blocked".into(), "None".into());
        vars.insert("next_steps".into(), "Next".into());

        let rendered = tpl.render(&vars);
        assert!(rendered.contains("<h2>Summary</h2>"));
        assert!(rendered.contains("<p>Project: Test | Period: W1</p>"));
    }

    #[test]
    fn sprint_review_template_has_velocity_section() {
        let tpl = sprint_review_template("en");
        let section_headings: Vec<&str> = tpl.sections.iter().map(|s| s.heading.as_str()).collect();
        assert!(section_headings.contains(&"Velocity"));
    }

    #[test]
    fn risk_register_template_has_risk_sections() {
        let tpl = risk_register_template("en");
        let section_headings: Vec<&str> = tpl.sections.iter().map(|s| s.heading.as_str()).collect();
        assert!(section_headings.contains(&"Risks"));
        assert!(section_headings.contains(&"Mitigations"));
    }

    #[test]
    fn milestone_template_all_languages() {
        for lang in &["en", "de", "fr", "it"] {
            let tpl = milestone_report_template(lang);
            assert!(!tpl.name.is_empty());
            assert_eq!(tpl.sections.len(), 3);
        }
    }

    #[test]
    fn substitute_leaves_unknown_placeholders() {
        let vars = HashMap::new();
        let result = substitute("Hello {{name}}", &vars);
        assert_eq!(result, "Hello {{name}}");
    }

    #[test]
    fn substitute_replaces_all_occurrences() {
        let mut vars = HashMap::new();
        vars.insert("x".into(), "1".into());
        let result = substitute("{{x}} and {{x}}", &vars);
        assert_eq!(result, "1 and 1");
    }
}
