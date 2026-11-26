//! Draft binder, compiled only into the contract test until the skills owner is settled.
//! A bound vector is data, not an authorized command or an execution capability.

use std::collections::HashMap;

use serde_json::Value;

pub(super) fn bind<'a>(
    template: &'a [String],
    declared: &HashMap<String, String>,
    input: &Value,
) -> Result<(&'a str, Vec<String>), &'static str> {
    let (program, templates) = template.split_first().ok_or("missing program")?;
    if program.trim().is_empty() || program.contains(['\0', '{', '}']) {
        return Err("expected a fixed program");
    }
    let values = input.as_object().ok_or("expected named arguments")?;
    if values.len() != declared.len()
        || !declared.keys().all(|k| {
            values
                .get(k)
                .and_then(Value::as_str)
                .is_some_and(|s| !s.contains('\0'))
        })
    {
        return Err("expected one string per declared argument");
    }

    let mut arguments = Vec::with_capacity(templates.len());
    for template in templates {
        if template.contains('\0') {
            return Err("NUL in template");
        }
        let mut bound = String::with_capacity(template.len());
        let mut rest = template.as_str();
        while let Some((prefix, after_open)) = rest.split_once("{{") {
            let (key, suffix) = after_open.split_once("}}").ok_or("unclosed placeholder")?;
            if prefix.contains("}}") || key.is_empty() || key.contains(['{', '}']) {
                return Err("invalid placeholder");
            }
            bound.push_str(prefix);
            bound.push_str(
                values
                    .get(key)
                    .and_then(Value::as_str)
                    .ok_or("unknown placeholder")?,
            );
            // Continue in the manifest, never in the substituted text.
            rest = suffix;
        }
        if rest.contains("}}") {
            return Err("unexpected closing delimiter");
        }
        bound.push_str(rest);
        arguments.push(bound);
    }
    Ok((program.as_str(), arguments))
}
