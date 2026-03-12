//! Declarative expectation verification for trace fixtures.

use super::trace::TraceExpects;

/// Verify trace expectations against actual test results.
///
/// - `expects`: declarative expectations from the trace fixture
/// - `final_response`: the final text response from the agent
/// - `tools_called`: names of tools that were actually called
/// - `label`: test label for error messages
pub fn verify_expects(
    expects: &TraceExpects,
    final_response: &str,
    tools_called: &[String],
    label: &str,
) {
    for needle in &expects.response_contains {
        assert!(
            final_response.contains(needle),
            "[{label}] Expected response to contain \"{needle}\", got: {final_response}"
        );
    }

    for needle in &expects.response_not_contains {
        assert!(
            !final_response.contains(needle),
            "[{label}] Expected response NOT to contain \"{needle}\", got: {final_response}"
        );
    }

    for tool in &expects.tools_used {
        assert!(
            tools_called.iter().any(|t| t == tool),
            "[{label}] Expected tool \"{tool}\" to be used, but tools called were: {tools_called:?}"
        );
    }

    for tool in &expects.tools_not_used {
        assert!(
            !tools_called.iter().any(|t| t == tool),
            "[{label}] Expected tool \"{tool}\" NOT to be used, but it was called"
        );
    }

    if let Some(max) = expects.max_tool_calls {
        assert!(
            tools_called.len() <= max,
            "[{label}] Expected at most {max} tool calls, got {}",
            tools_called.len()
        );
    }

    for pattern in &expects.response_matches {
        let re = regex::Regex::new(pattern).unwrap_or_else(|e| {
            panic!("[{label}] Invalid regex pattern \"{pattern}\": {e}");
        });
        assert!(
            re.is_match(final_response),
            "[{label}] Expected response to match regex \"{pattern}\", got: {final_response}"
        );
    }
}
