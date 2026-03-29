use extism_pdk::*;
use serde_json::Value;

#[plugin_fn]
pub fn tool_echo(input: String) -> FnResult<String> {
    let parsed: Value = serde_json::from_str(&input)?;
    let output = serde_json::to_string(&parsed)?;
    Ok(output)
}
