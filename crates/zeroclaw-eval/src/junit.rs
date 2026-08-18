//! JUnit XML report format for CI consumers. Hand-rolled (no XML dependency).

use crate::report::{CaseReport, SuiteReport, case_id};

/// Escape XML text/attribute content and strip control characters (below 0x20
/// except tab and newline), which are illegal in XML 1.0.
fn escape(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for c in input.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            '\t' | '\n' => out.push(c),
            c if (c as u32) < 0x20 => {} // drop other control chars
            c => out.push(c),
        }
    }
    out
}

fn duration_secs(case: &CaseReport) -> f64 {
    case.record
        .as_ref()
        .map_or(0.0, |r| r.duration_ms as f64 / 1000.0)
}

/// Render a suite report as JUnit XML. `skipped` holds case ids that are
/// unverifiable against a baseline (rendered as `<skipped/>`, neither pass nor
/// fail).
pub fn render_junit(report: &SuiteReport, skipped: &[&str]) -> String {
    let is_skipped = |case: &CaseReport| skipped.contains(&case_id(case));

    let mut tests = 0usize;
    let mut failures = 0usize;
    let mut errors = 0usize;
    let mut skipped_count = 0usize;
    let mut time = 0.0f64;
    for case in &report.cases {
        tests += 1;
        time += duration_secs(case);
        if is_skipped(case) {
            skipped_count += 1;
        } else if case.error.is_some() {
            errors += 1;
        } else if !case.passed() {
            failures += 1;
        }
    }

    let mut xml = String::new();
    xml.push_str(&format!(
        "<testsuite name=\"zeroclaw-eval\" tests=\"{tests}\" failures=\"{failures}\" errors=\"{errors}\" skipped=\"{skipped_count}\" time=\"{time:.3}\">\n"
    ));
    for case in &report.cases {
        xml.push_str(&format!(
            "  <testcase name=\"{}\" classname=\"{}\" time=\"{:.3}\">",
            escape(case_id(case)),
            escape(&case.source),
            duration_secs(case)
        ));
        if is_skipped(case) {
            xml.push_str("<skipped/>");
        } else if case.error.is_some() {
            // Run errors can contain provider payloads. JUnit is commonly retained
            // or uploaded by CI, so keep the artifact transcript/PII-safe.
            xml.push_str("<error message=\"run error\"/>");
        } else {
            let failing: Vec<&crate::grader::GradeResult> = case
                .grades
                .iter()
                .filter(|grade| !grade.passed && !grade.diagnostic)
                .collect();
            if let Some(first) = failing.first() {
                // Grade details may embed the model's final response. Preserve safe
                // check/category metadata without copying response content into CI.
                let body: String = failing
                    .iter()
                    .map(|grade| format!("{} ({})", grade.check, grade.category.as_str()))
                    .collect::<Vec<_>>()
                    .join("\n");
                xml.push_str(&format!(
                    "<failure message=\"{}\">{}</failure>",
                    escape(&first.check),
                    escape(&body)
                ));
            }
        }
        xml.push_str("</testcase>\n");
    }
    xml.push_str("</testsuite>\n");
    xml
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grader::{GradeCategory, GradeResult};

    fn grade(check: &str, passed: bool, detail: &str) -> GradeResult {
        GradeResult {
            check: check.to_string(),
            passed,
            detail: detail.to_string(),
            category: GradeCategory::Response,
            diagnostic: false,
        }
    }

    fn case(name: &str, grades: Vec<GradeResult>, error: Option<&str>) -> CaseReport {
        CaseReport {
            name: name.to_string(),
            source: "fixture.json".to_string(),
            record: None,
            grades,
            error: error.map(str::to_string),
            repeat: None,
            cluster: None,
        }
    }

    #[test]
    fn junit_escapes_and_strips_control_chars() {
        let report = SuiteReport {
            cases: vec![case(
                "weird <\"&'> name",
                vec![grade("check<x>", false, "line1\nline2\u{0007}bell")],
                None,
            )],
        };
        let xml = render_junit(&report, &[]);
        // The case name is escaped in the attribute.
        assert!(xml.contains("name=\"weird &lt;&quot;&amp;&apos;&gt; name\""));
        // The safe failure body escapes the check and omits response-derived detail.
        assert!(xml.contains("check&lt;x&gt; (response)"));
        assert!(!xml.contains("line1"));
        assert!(!xml.contains('\u{0007}'));
    }

    #[test]
    fn junit_counts_match_suite_report() {
        let report = SuiteReport {
            cases: vec![
                case("ok", vec![grade("c", true, "")], None),
                case("bad", vec![grade("c", false, "")], None),
                case("err", vec![], Some("boom")),
                case("changed", vec![grade("c", false, "")], None),
            ],
        };
        let xml = render_junit(&report, &["changed"]);
        assert!(xml.contains("tests=\"4\""));
        assert!(xml.contains("failures=\"1\"")); // only "bad"
        assert!(xml.contains("errors=\"1\"")); // only "err"
        assert!(xml.contains("skipped=\"1\"")); // only "changed"
        assert!(xml.contains("<skipped/>"));
        assert!(xml.contains("<error message=\"run error\"/>"));
        assert!(!xml.contains("boom"));
    }

    #[test]
    fn diagnostic_failure_is_not_a_junit_failure() {
        let mut diagnostic = grade("judge:quality", false, "below threshold");
        diagnostic.category = GradeCategory::Judge;
        diagnostic.diagnostic = true;
        let report = SuiteReport {
            cases: vec![case("diagnostic", vec![diagnostic], None)],
        };
        let xml = render_junit(&report, &[]);
        assert!(xml.contains("failures=\"0\""));
        assert!(!xml.contains("<failure"));
    }

    #[test]
    fn junit_omits_response_and_error_details() {
        const RESPONSE_SENTINEL: &str = "PRIVATE_MODEL_RESPONSE_7f6d";
        const ERROR_SENTINEL: &str = "PRIVATE_PROVIDER_ERROR_3a91";
        let report = SuiteReport {
            cases: vec![
                case(
                    "failed",
                    vec![grade("response_contains", false, RESPONSE_SENTINEL)],
                    None,
                ),
                case("errored", vec![], Some(ERROR_SENTINEL)),
            ],
        };
        let xml = render_junit(&report, &[]);
        assert!(!xml.contains(RESPONSE_SENTINEL));
        assert!(!xml.contains(ERROR_SENTINEL));
        assert!(xml.contains("response_contains (response)"));
    }
}
