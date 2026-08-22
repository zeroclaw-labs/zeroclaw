//! JUnit XML report format for CI consumers. Hand-rolled (no XML dependency).

use crate::report::{CaseReport, SuiteReport, case_id};

/// The `name` attribute of the emitted `<testsuite>` root.
pub const SUITE_NAME: &str = "zeroclaw-eval";

/// The character used to replace scalars that no XML 1.0 document may contain.
/// Replacement (rather than deletion) keeps otherwise-distinct ids distinct.
const XML_REPLACEMENT: char = '\u{FFFD}';

/// The XML 1.0 §2.2 `Char` production.
///
/// Anything outside this set makes a document unparseable no matter how it is
/// escaped, so it must be replaced before it reaches an attribute value or an
/// element body. Note this excludes `U+FFFE` and `U+FFFF`, which an "everything
/// below `U+0020`" filter lets through.
fn is_xml_char(c: char) -> bool {
    matches!(c,
        '\u{9}' | '\u{A}' | '\u{D}'
        | '\u{20}'..='\u{D7FF}'
        | '\u{E000}'..='\u{FFFD}'
        | '\u{10000}'..='\u{10FFFF}')
}

/// Escape one character for an element body. `\t`, `\n` and `\r` are legal
/// literal characters in content, so they are passed through.
fn push_escaped_text(out: &mut String, c: char) {
    match c {
        '&' => out.push_str("&amp;"),
        '<' => out.push_str("&lt;"),
        '>' => out.push_str("&gt;"),
        '"' => out.push_str("&quot;"),
        '\'' => out.push_str("&apos;"),
        c => out.push(c),
    }
}

/// Escape XML element-body content: replace every non-`Char` scalar, then escape
/// markup. Order matters — sanitising after escaping would leave the illegal
/// scalars in place.
fn escape(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for c in input.chars() {
        let c = if is_xml_char(c) { c } else { XML_REPLACEMENT };
        push_escaped_text(&mut out, c);
    }
    out
}

/// Escape XML attribute content.
///
/// Same sanitising as [`escape`], plus numeric references for `\t`, `\n` and
/// `\r`: an XML parser normalises literal whitespace in an attribute value to a
/// space, so a raw newline in a case id would silently change on round-trip.
fn escape_attr(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for c in input.chars() {
        let c = if is_xml_char(c) { c } else { XML_REPLACEMENT };
        match c {
            '\t' => out.push_str("&#9;"),
            '\n' => out.push_str("&#10;"),
            '\r' => out.push_str("&#13;"),
            c => push_escaped_text(&mut out, c),
        }
    }
    out
}

/// The `message` on a `<skipped/>` produced by a flaky-unconfirmed live case.
pub const FLAKY_UNCONFIRMED_MESSAGE: &str = "flaky-unconfirmed: regressed against the baseline but passed on re-run (reported, never gated)";

fn duration_secs(case: &CaseReport) -> f64 {
    case.record
        .as_ref()
        .map_or(0.0, |r| r.duration_ms as f64 / 1000.0)
}

/// The `check: detail` lines for every failing grade, in order. Used for both
/// the `<failure>` body and the flaky case's `<system-out>`.
///
/// ⚠️ These details can carry the model's complete final response (a failed
/// `response_contains` check reports what was actually produced). CI reporters
/// retain JUnit bodies as artifacts and annotations — see the handling note in
/// `docs/book/src/ops/eval-harness.md`. Escaping protects document structure,
/// not confidentiality.
fn failure_body(case: &CaseReport) -> String {
    case.grades
        .iter()
        .filter(|g| !g.passed)
        .map(|g| format!("{}: {}", g.check, g.detail))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Render a suite report as JUnit XML.
///
/// `skipped` holds case ids that are unverifiable against a baseline; `flaky`
/// holds live case ids that regressed but passed on their single re-run. Both
/// render as `<skipped/>` — neither pass nor fail — because both are documented
/// as "reported, never gated" and both exit 0. Rendering a flaky case as
/// `<failure>` while the process exits 0 would put the XML and the exit status
/// in direct contradiction, which is the first thing a CI consumer hits. A
/// flaky case additionally carries its reason in the `<skipped message=…>` and
/// the failing check details in `<system-out>`, so the signal is not lost.
pub fn render_junit(report: &SuiteReport, skipped: &[&str], flaky: &[&str]) -> String {
    let is_flaky = |case: &CaseReport| flaky.contains(&case_id(case));
    let is_skipped = |case: &CaseReport| skipped.contains(&case_id(case)) || is_flaky(case);

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
    xml.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    xml.push_str(&format!(
        "<testsuite name=\"{}\" tests=\"{tests}\" failures=\"{failures}\" errors=\"{errors}\" skipped=\"{skipped_count}\" time=\"{time:.3}\">\n",
        escape_attr(SUITE_NAME)
    ));
    for case in &report.cases {
        xml.push_str(&format!(
            "  <testcase name=\"{}\" classname=\"{}\" time=\"{:.3}\">",
            escape_attr(case_id(case)),
            escape_attr(&case.source),
            duration_secs(case)
        ));
        if is_flaky(case) {
            // Reported, never gated: skipped, with the reason and the failing
            // checks preserved so the run is still diagnosable.
            xml.push_str(&format!(
                "<skipped message=\"{}\"/>",
                escape_attr(FLAKY_UNCONFIRMED_MESSAGE)
            ));
            let detail = failure_body(case);
            if !detail.is_empty() {
                xml.push_str(&format!("<system-out>{}</system-out>", escape(&detail)));
            }
        } else if is_skipped(case) {
            xml.push_str("<skipped/>");
        } else if let Some(err) = &case.error {
            xml.push_str(&format!(
                "<error message=\"{}\">{}</error>",
                escape_attr(err),
                escape(err)
            ));
        } else {
            let failing: Vec<&crate::grader::GradeResult> =
                case.grades.iter().filter(|g| !g.passed).collect();
            if let Some(first) = failing.first() {
                xml.push_str(&format!(
                    "<failure message=\"{}\">{}</failure>",
                    escape_attr(&first.check),
                    escape(&failure_body(case))
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
        }
    }

    fn case(name: &str, grades: Vec<GradeResult>, error: Option<&str>) -> CaseReport {
        CaseReport {
            name: name.to_string(),
            source: "fixture.json".to_string(),
            record: None,
            grades,
            error: error.map(str::to_string),
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
        let xml = render_junit(&report, &[], &[]);
        // The case name is escaped in the attribute.
        assert!(xml.contains("name=\"weird &lt;&quot;&amp;&apos;&gt; name\""));
        // The failure body escapes and replaces the control char (bell), keeps newline.
        assert!(xml.contains("check&lt;x&gt;: line1\nline2\u{FFFD}bell"));
        assert!(!xml.contains('\u{0007}'));
    }

    /// One element start: its name plus its attributes as entity-decoded
    /// `(key, value)` pairs, in document order.
    type Element = (String, Vec<(String, String)>);

    /// Read the whole document with a real XML parser, returning every element
    /// start (name plus its attribute values, already entity-decoded).
    fn parse_elements(xml: &str) -> Result<Vec<Element>, String> {
        use quick_xml::events::Event;
        let mut reader = quick_xml::Reader::from_str(xml);
        let mut out = Vec::new();
        loop {
            match reader.read_event() {
                Ok(Event::Eof) => return Ok(out),
                Ok(Event::Start(e)) | Ok(Event::Empty(e)) => {
                    let name = String::from_utf8_lossy(e.name().as_ref()).into_owned();
                    let mut attrs = Vec::new();
                    for a in e.attributes() {
                        let a = a.map_err(|err| format!("attribute: {err}"))?;
                        let key = String::from_utf8_lossy(a.key.as_ref()).into_owned();
                        let value = a
                            .decoded_and_normalized_value(
                                quick_xml::XmlVersion::Explicit1_0,
                                reader.decoder(),
                            )
                            .map_err(|err| format!("attribute value: {err}"))?
                            .into_owned();
                        attrs.push((key, value));
                    }
                    out.push((name, attrs));
                }
                Ok(_) => {}
                Err(e) => return Err(format!("parse error at {}: {e}", reader.error_position())),
            }
        }
    }

    fn attr<'a>(elements: &'a [Element], element: &str, key: &str) -> Option<&'a str> {
        elements
            .iter()
            .find(|(name, _)| name == element)?
            .1
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    #[test]
    fn junit_output_parses_with_xml_edge_characters() {
        // U+FFFE and U+FFFF are outside the XML 1.0 `Char` production: escaping
        // cannot rescue them, so they must be replaced before they reach a sink.
        let hostile = "id\u{FFFE}a\u{FFFF}b\u{1}c\u{B}d<>&\"'";
        let mut c = case(
            hostile,
            vec![grade(hostile, false, &format!("detail {hostile}"))],
            None,
        );
        c.source = format!("source{hostile}.json");
        let report = SuiteReport { cases: vec![c] };

        let xml = render_junit(&report, &[], &[]);
        let elements = parse_elements(&xml).expect("rendered JUnit must parse as XML 1.0");

        // No non-`Char` scalar survives anywhere in the document.
        assert!(!xml.contains('\u{FFFE}'));
        assert!(!xml.contains('\u{FFFF}'));
        assert!(!xml.contains('\u{1}'));
        assert!(!xml.contains('\u{B}'));

        let expected = "id\u{FFFD}a\u{FFFD}b\u{FFFD}c\u{FFFD}d<>&\"'";
        assert_eq!(attr(&elements, "testcase", "name"), Some(expected));
        assert_eq!(
            attr(&elements, "testcase", "classname"),
            Some(format!("source{expected}.json").as_str())
        );
        assert_eq!(attr(&elements, "failure", "message"), Some(expected));
        assert_eq!(
            elements.iter().filter(|(n, _)| n == "testsuite").count(),
            1,
            "exactly one <testsuite> root"
        );
    }

    /// Read the document's character data with a real parser, resolving entity
    /// and character references back to the text the writer was handed.
    fn parse_text(xml: &str) -> Result<String, String> {
        use quick_xml::events::Event;
        let mut reader = quick_xml::Reader::from_str(xml);
        let mut text = String::new();
        loop {
            match reader.read_event() {
                Ok(Event::Eof) => return Ok(text),
                Ok(Event::Text(t)) => {
                    text.push_str(&t.xml10_content().map_err(|e| format!("text: {e}"))?);
                }
                Ok(Event::GeneralRef(r)) => {
                    if let Some(c) = r.resolve_char_ref().map_err(|e| format!("charref: {e}"))? {
                        text.push(c);
                    } else {
                        let name = r.decode().map_err(|e| format!("ref: {e}"))?;
                        let resolved = quick_xml::escape::resolve_predefined_entity(&name)
                            .ok_or_else(|| format!("unresolved entity &{name};"))?;
                        text.push_str(resolved);
                    }
                }
                Ok(_) => {}
                Err(e) => return Err(format!("parse error at {}: {e}", reader.error_position())),
            }
        }
    }

    #[test]
    fn junit_escapes_markup_in_failure_bodies() {
        let body_input = "a < b & c > d \" e ' f\u{FFFE}g";
        let report = SuiteReport {
            cases: vec![case(
                "plain-id",
                vec![grade("chk", false, body_input)],
                None,
            )],
        };
        let xml = render_junit(&report, &[], &[]);
        // Raw markup never reaches the body verbatim.
        assert!(xml.contains("a &lt; b &amp; c &gt; d"));

        let text = parse_text(&xml).expect("failure body must parse");
        assert!(
            text.contains("chk: a < b & c > d \" e ' f\u{FFFD}g"),
            "round-tripped failure body was {text:?}"
        );
    }

    #[test]
    fn junit_error_sink_sanitizes_message_and_body() {
        let report = SuiteReport {
            cases: vec![case("err-id", vec![], Some("boom \u{FFFF}<bad>"))],
        };
        let xml = render_junit(&report, &[], &[]);
        let elements = parse_elements(&xml).expect("errored case must still parse");
        assert_eq!(
            attr(&elements, "error", "message"),
            Some("boom \u{FFFD}<bad>")
        );
    }

    #[test]
    fn junit_attribute_newlines_survive_round_trip() {
        // A literal newline in an attribute value is normalised to a space by
        // any conforming parser, so it must be written as a numeric reference.
        let report = SuiteReport {
            cases: vec![case("two\nlines\there", vec![], None)],
        };
        let xml = render_junit(&report, &[], &[]);
        let elements = parse_elements(&xml).expect("must parse");
        assert_eq!(
            attr(&elements, "testcase", "name"),
            Some("two\nlines\there")
        );
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
        let xml = render_junit(&report, &["changed"], &[]);
        assert!(xml.contains("tests=\"4\""));
        assert!(xml.contains("failures=\"1\"")); // only "bad"
        assert!(xml.contains("errors=\"1\"")); // only "err"
        assert!(xml.contains("skipped=\"1\"")); // only "changed"
        assert!(xml.contains("<skipped/>"));
        assert!(xml.contains("<error message=\"boom\">boom</error>"));
    }

    #[test]
    fn flaky_unconfirmed_case_is_skipped_not_failed() {
        // "Reported, never gated" and exit 0: rendering the case as <failure>
        // would put the XML in direct contradiction with the exit status.
        let report = SuiteReport {
            cases: vec![
                case("solid", vec![grade("c", true, "")], None),
                case(
                    "flappy",
                    vec![grade("response_contains", false, "wanted 'hi', got 'yo'")],
                    None,
                ),
            ],
        };
        let xml = render_junit(&report, &[], &["flappy"]);
        let elements = parse_elements(&xml).expect("must parse");

        assert!(xml.contains("skipped=\"1\""));
        assert!(
            xml.contains("failures=\"0\""),
            "a flaky-unconfirmed case must not be counted as a failure: {xml}"
        );
        assert!(
            !xml.contains("<failure"),
            "a flaky-unconfirmed case must not render as <failure>: {xml}"
        );
        assert_eq!(
            attr(&elements, "skipped", "message"),
            Some(FLAKY_UNCONFIRMED_MESSAGE),
            "the skip must state why it was not gated"
        );
        // The signal is preserved rather than discarded.
        let text = parse_text(&xml).expect("must parse");
        assert!(
            text.contains("response_contains: wanted 'hi', got 'yo'"),
            "the failing check must survive in <system-out>, got {text:?}"
        );
    }

    #[test]
    fn flaky_and_unverifiable_both_skip_without_double_counting() {
        let report = SuiteReport {
            cases: vec![
                case("hashchanged", vec![grade("c", false, "")], None),
                case("flappy", vec![grade("c", false, "")], None),
            ],
        };
        // A case listed in both lists must be counted once.
        let xml = render_junit(&report, &["hashchanged", "flappy"], &["flappy"]);
        assert!(xml.contains("tests=\"2\""));
        assert!(xml.contains("skipped=\"2\""));
        assert!(xml.contains("failures=\"0\""));
        parse_elements(&xml).expect("must parse");
    }
}
