//! The refusal a runtime tool returns for a call that carries an argument its
//! schema does not declare. `zeroclaw_api` decides what to refuse; the
//! sentence is worded here so it comes from the runtime Fluent catalogue.

use zeroclaw_api::tool::{ToolResult, UndeclaredArguments, find_undeclared_arguments};

/// The refusal for a call whose top-level arguments include a name the closed
/// `schema` does not declare, or `None` when there is nothing to refuse.
///
/// Call it first in `execute` with the tool's own `parameters_schema()`, so a
/// caller learns that an option it sent was not applied. Which schemas are
/// enforced is decided by [`find_undeclared_arguments`].
pub(crate) fn refuse_undeclared_arguments(
    schema: &serde_json::Value,
    args: &serde_json::Value,
) -> Option<ToolResult> {
    let found = find_undeclared_arguments(schema, args)?;
    Some(ToolResult::err(refusal_text(
        &found,
        crate::i18n::get_required_cli_string_with_args,
    )))
}

/// The catalogue lookup is a parameter so tests can render the English source
/// whatever locale the test process detects.
fn refusal_text(
    found: &UndeclaredArguments,
    lookup: fn(&str, &[(&str, &str)]) -> String,
) -> String {
    let names = found
        .undeclared
        .iter()
        .map(|name| format!("'{name}'"))
        .collect::<Vec<_>>()
        .join(", ");
    let count_plural = if found.undeclared.len() == 1 {
        "one"
    } else {
        "other"
    };
    if found.declared.is_empty() {
        return lookup(
            "tool-error-undeclared-arguments-none-declared",
            &[("names", names.as_str()), ("count_plural", count_plural)],
        );
    }
    let declared = found.declared.join(", ");
    lookup(
        "tool-error-undeclared-arguments",
        &[
            ("names", names.as_str()),
            ("declared", declared.as_str()),
            ("count_plural", count_plural),
        ],
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn closed_schema() -> serde_json::Value {
        json!({
            "type": "object",
            "additionalProperties": false,
            "properties": { "prompt": {"type": "string"}, "agent": {"type": "string"} }
        })
    }

    fn english_refusal(schema: &serde_json::Value, args: &serde_json::Value) -> String {
        let found = find_undeclared_arguments(schema, args).expect("a call to refuse");
        refusal_text(&found, crate::i18n::get_english_cli_string_with_args)
    }

    // The tests below compare whole strings byte for byte: published docs quote
    // this wording, and a Unicode isolation mark around a placeable would also
    // fail the comparison.

    #[test]
    fn one_undeclared_argument_is_named_in_the_singular() {
        assert_eq!(
            english_refusal(&closed_schema(), &json!({"model": 1})),
            "Unknown argument 'model'. Use agent, prompt."
        );
    }

    #[test]
    fn several_undeclared_arguments_are_named_sorted_in_the_plural() {
        let args = json!({"prompt": "p", "zeta": "secret-one", "model": "secret-two"});
        assert_eq!(
            english_refusal(&closed_schema(), &args),
            "Unknown arguments 'model', 'zeta'. Use agent, prompt."
        );
    }

    #[test]
    fn a_tool_without_declared_arguments_says_it_takes_none() {
        let schema = json!({"type": "object", "additionalProperties": false});
        assert_eq!(
            english_refusal(&schema, &json!({"x": 1})),
            "Unknown argument 'x'. This tool takes no arguments."
        );
        assert_eq!(
            english_refusal(&schema, &json!({"y": 1, "x": 2})),
            "Unknown arguments 'x', 'y'. This tool takes no arguments."
        );
    }

    #[test]
    fn the_refusal_is_a_failed_result_and_declared_calls_pass() {
        let refusal = refuse_undeclared_arguments(&closed_schema(), &json!({"model": 1}))
            .expect("an undeclared argument is refused");
        assert!(!refusal.success);
        assert!(refusal.error.is_some_and(|error| !error.is_empty()));
        assert!(refuse_undeclared_arguments(&closed_schema(), &json!({"agent": "a"})).is_none());
    }
}
