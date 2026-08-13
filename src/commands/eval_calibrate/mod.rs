//! `zeroclaw eval calibrate` command implementations.

use std::path::Path;
use zeroclaw_eval::calibration::JsonlError;
use zeroclaw_runtime::i18n::get_required_cli_string_with_args;

pub mod finalize;
pub mod label;

pub(super) fn localized_jsonl_error(path: &Path, error: &JsonlError) -> anyhow::Error {
    let path = path.display().to_string();
    let message = match error {
        JsonlError::Io(source) => {
            let error = source.to_string();
            get_required_cli_string_with_args(
                "cli-eval-calibrate-jsonl-io",
                &[("path", path.as_str()), ("error", error.as_str())],
            )
        }
        JsonlError::Decode { line, source } => {
            let line = line.to_string();
            let error = source.to_string();
            get_required_cli_string_with_args(
                "cli-eval-calibrate-jsonl-decode",
                &[
                    ("path", path.as_str()),
                    ("line", line.as_str()),
                    ("error", error.as_str()),
                ],
            )
        }
        JsonlError::Encode(source) => {
            let error = source.to_string();
            get_required_cli_string_with_args(
                "cli-eval-calibrate-jsonl-encode",
                &[("path", path.as_str()), ("error", error.as_str())],
            )
        }
        JsonlError::WrongSchema {
            line,
            expected,
            found,
        } => {
            let line = line.to_string();
            get_required_cli_string_with_args(
                "cli-eval-calibrate-jsonl-wrong-schema",
                &[
                    ("path", path.as_str()),
                    ("line", line.as_str()),
                    ("found", found.as_str()),
                    ("expected", expected),
                ],
            )
        }
    };
    anyhow::Error::msg(message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jsonl_errors_are_localized_by_variant_at_the_command_boundary() {
        let path = Path::new("labels.jsonl");
        let wrong_schema = JsonlError::WrongSchema {
            line: 3,
            expected: "expected/v1",
            found: "found/v0".to_string(),
        };
        assert_eq!(
            localized_jsonl_error(path, &wrong_schema).to_string(),
            "Calibration JSONL labels.jsonl line 3 has schema 'found/v0', expected 'expected/v1'."
        );

        let decode = JsonlError::Decode {
            line: 4,
            source: serde_json::from_str::<serde_json::Value>("{")
                .expect_err("fixture must be malformed"),
        };
        let message = localized_jsonl_error(path, &decode).to_string();
        assert!(message.starts_with("Invalid calibration JSONL labels.jsonl at line 4:"));
    }
}
