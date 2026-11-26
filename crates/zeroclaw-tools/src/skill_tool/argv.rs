//! Bind only placeholders from the manifest, never text introduced by a value.

use serde_json::Value;
use std::collections::HashMap;

pub fn validate(
    template: &[String],
    declared: &HashMap<String, String>,
) -> Result<(), &'static str> {
    let (_, args) = parts(template)?;
    for arg in args {
        render(arg, |key| declared.contains_key(key).then_some(""))?;
    }
    Ok(())
}

fn parts(template: &[String]) -> Result<(&str, &[String]), &'static str> {
    let (program, args) = template.split_first().ok_or("missing program")?;
    let path = std::path::Path::new(program);
    if program.trim().is_empty()
        || program.trim() != program
        || program.contains(['\0', '{', '}'])
        || path.file_name().is_none()
        || (!path.is_absolute() && path.file_name() != Some(program.as_ref()))
    {
        return Err("expected a fixed program");
    }
    Ok((program, args))
}

fn render<'a>(
    mut rest: &str,
    value: impl Fn(&str) -> Option<&'a str>,
) -> Result<String, &'static str> {
    if rest.contains('\0') {
        return Err("NUL in template");
    }
    let mut out = String::new();
    while let Some((prefix, after)) = rest.split_once("{{") {
        let (key, suffix) = after.split_once("}}").ok_or("unclosed placeholder")?;
        if prefix.contains("}}") || key.is_empty() || key.contains(['{', '}']) {
            return Err("invalid placeholder");
        }
        out.push_str(prefix);
        out.push_str(value(key).ok_or("unknown placeholder")?);
        rest = suffix;
    }
    if rest.contains("}}") {
        return Err("unexpected closing delimiter");
    }
    out.push_str(rest);
    Ok(out)
}

pub fn bind<'a>(
    template: &'a [String],
    declared: &HashMap<String, String>,
    input: &Value,
) -> Result<(&'a str, Vec<String>), &'static str> {
    let (program, args) = parts(template)?;
    let values = input.as_object().ok_or("expected named arguments")?;
    if values.len() != declared.len()
        || !declared.keys().all(|key| {
            values
                .get(key)
                .and_then(Value::as_str)
                .is_some_and(|s| !s.contains('\0'))
        })
    {
        return Err("expected one string per declared argument");
    }
    let bound = args
        .iter()
        .map(|arg| render(arg, |key| values.get(key).and_then(Value::as_str)))
        .collect::<Result<_, _>>()?;
    Ok((program, bound))
}

/// Prepare, but never launch, the exact policy-checked invocation. The caller
/// must apply its selected sandbox and supervise the process before execution.
pub fn prepare(
    template: &[String],
    declared: &HashMap<String, String>,
    input: &Value,
    policy: &zeroclaw_config::policy::SecurityPolicy,
    runtime: &dyn zeroclaw_api::runtime_traits::RuntimeAdapter,
) -> anyhow::Result<tokio::process::Command> {
    let (program, args) = bind(template, declared, input).map_err(anyhow::Error::msg)?;
    policy
        .validate_literal_argv(program, &args)
        .map_err(anyhow::Error::msg)?;
    runtime.build_literal_command(program, &args, &policy.workspace_dir)
}
