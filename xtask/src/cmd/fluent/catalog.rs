use fluent_syntax::{ast, parser};
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FtlError {
    pub line: usize,
    pub message: String,
}

pub fn message_ids(src: &str) -> Result<Vec<String>, Vec<FtlError>> {
    let resource = match parser::parse(src) {
        Ok(resource) => resource,
        Err((_resource, errors)) => {
            return Err(errors
                .into_iter()
                .map(|error| FtlError {
                    line: line_for_offset(src, error.pos.start),
                    message: error.to_string(),
                })
                .collect());
        }
    };

    Ok(resource
        .body
        .into_iter()
        .filter_map(|entry| match entry {
            ast::Entry::Message(message) => Some(message.id.name.to_string()),
            _ => None,
        })
        .collect())
}

pub fn message_ids_from_file(path: &Path) -> anyhow::Result<Vec<String>> {
    let src = std::fs::read_to_string(path)?;
    message_ids(&src)
        .map_err(|errors| anyhow::anyhow!("{}: {}", path.display(), format_errors(errors)))
}

pub fn format_errors(errors: Vec<FtlError>) -> String {
    errors
        .into_iter()
        .map(|error| format!("line {}: {}", error.line, error.message))
        .collect::<Vec<_>>()
        .join("; ")
}

fn line_for_offset(src: &str, offset: usize) -> usize {
    let offset = src
        .char_indices()
        .map(|(index, _)| index)
        .take_while(|index| *index <= offset)
        .last()
        .unwrap_or(0);

    src[..offset].bytes().filter(|byte| *byte == b'\n').count() + 1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_ids_ignore_multiline_value_text() {
        let src = r#"
cli-agent-long-about =
    Start the AI agent loop.

    Examples:
      zeroclaw agent

cli-about = The fastest, smallest AI assistant.
"#;

        assert_eq!(
            message_ids(src).unwrap(),
            vec!["cli-agent-long-about", "cli-about"]
        );
    }

    #[test]
    fn message_ids_report_parse_errors_with_line_numbers() {
        let errors = message_ids(
            r#"
valid-key = ok
bad key = nope
"#,
        )
        .unwrap_err();

        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].line, 3);
    }

    #[test]
    fn message_ids_report_parse_errors_after_multibyte_text() {
        let errors = message_ids(
            r#"
valid-key = 日本語
bad key = nope
"#,
        )
        .unwrap_err();

        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].line, 3);
    }
}
