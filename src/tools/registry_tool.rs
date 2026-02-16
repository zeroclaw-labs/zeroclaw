use super::traits::{Tool, ToolResult};
use async_trait::async_trait;
use serde_json::{Map, Value};

#[derive(Debug, Clone)]
pub struct RegistryTool {
    pub id: String,
    pub tenant_id: String,
    pub name: String,
    pub description: String,
    pub schema: Value,
    pub handler_code: String,
    pub sandbox_config: Option<Value>,
}

impl RegistryTool {
    pub fn normalized_schema(&self) -> Value {
        normalize_tool_schema(&self.schema)
    }
}

#[async_trait]
impl Tool for RegistryTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn parameters_schema(&self) -> Value {
        self.normalized_schema()
    }

    async fn execute(&self, args: Value) -> anyhow::Result<ToolResult> {
        execute_handler_with_quilt(self, args).await
    }
}

fn normalize_param_type(raw: Option<&str>) -> &'static str {
    match raw.unwrap_or("object") {
        "string" => "string",
        "number" => "number",
        "integer" => "integer",
        "boolean" => "boolean",
        "array" => "array",
        "object" => "object",
        _ => "object",
    }
}

fn normalize_tool_schema(schema: &Value) -> Value {
    let Some(obj) = schema.as_object() else {
        return serde_json::json!({"type":"object","properties":{},"required":[]});
    };

    if obj.get("type").and_then(Value::as_str) == Some("object") {
        return schema.clone();
    }

    let Some(parameters) = obj.get("parameters").and_then(Value::as_array) else {
        return serde_json::json!({"type":"object","properties":{},"required":[]});
    };

    let mut properties = Map::new();
    let mut required: Vec<Value> = Vec::new();

    for param in parameters {
        let Some(param_obj) = param.as_object() else {
            continue;
        };
        let Some(name) = param_obj.get("name").and_then(Value::as_str) else {
            continue;
        };

        let mut prop = Map::new();
        prop.insert(
            "type".to_string(),
            Value::String(
                normalize_param_type(param_obj.get("type").and_then(Value::as_str)).to_string(),
            ),
        );

        if let Some(desc) = param_obj.get("description").and_then(Value::as_str) {
            if !desc.trim().is_empty() {
                prop.insert("description".to_string(), Value::String(desc.to_string()));
            }
        }

        properties.insert(name.to_string(), Value::Object(prop));

        if param_obj
            .get("required")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            required.push(Value::String(name.to_string()));
        }
    }

    serde_json::json!({
        "type": "object",
        "properties": properties,
        "required": required,
    })
}

fn sandbox_u32(sandbox: Option<&Value>, key: &str) -> Option<u32> {
    sandbox
        .and_then(|cfg| cfg.get(key))
        .and_then(Value::as_u64)
        .and_then(|v| u32::try_from(v).ok())
}

fn sandbox_u64(sandbox: Option<&Value>, key: &str) -> Option<u64> {
    sandbox.and_then(|cfg| cfg.get(key)).and_then(Value::as_u64)
}

fn js_exec_wrapper(handler_code: &str, args: &Value) -> anyhow::Result<String> {
    let handler_json = serde_json::to_string(handler_code)?;
    let args_json = serde_json::to_string(args)?;
    let template = r#"
const handlerSource = __ARIA_HANDLER_CODE__;
const args = __ARIA_TOOL_ARGS__;

const originalLog = console.log;
console.log = (...parts) => process.stderr.write(parts.join(' ') + '\n');

function parseMethodSyntax(source) {
  const m = source.match(/^(async\s+)?([A-Za-z_$][\w$]*)\s*\(([^)]*)\)\s*\{([\s\S]*)\}\s*$/);
  if (!m) return null;
  const isAsync = Boolean(m[1]);
  const params = m[3] || '';
  const body = m[4] || '';
  const AsyncFunction = Object.getPrototypeOf(async function () {}).constructor;
  return isAsync ? new AsyncFunction(params, body) : new Function(params, body);
}

async function resolveHandler(source) {
  const trimmed = source.trim();
  if (!trimmed) throw new Error('Empty handler code');

  if (trimmed.startsWith('class ')) {
    const Cls = eval(`(${trimmed})`);
    if (typeof Cls !== 'function') throw new Error('Invalid class handler');
    const instance = new Cls();
    if (typeof instance.execute !== 'function') {
      throw new Error('Class handler must implement execute(args)');
    }
    return async (input) => instance.execute(input);
  }

  try {
    const fn = eval(`(${trimmed})`);
    if (typeof fn === 'function') return fn;
  } catch {}

  const methodFn = parseMethodSyntax(trimmed);
  if (typeof methodFn === 'function') {
    return methodFn;
  }

  throw new Error('Could not parse handler_code into executable function');
}

(async () => {
  try {
    const fn = await resolveHandler(handlerSource);
    const result = await fn(args);
    process.stdout.write(JSON.stringify({ ok: true, result }));
  } catch (err) {
    process.stdout.write(
      JSON.stringify({
        ok: false,
        error: err instanceof Error ? err.message : String(err),
      })
    );
    process.exitCode = 1;
  } finally {
    console.log = originalLog;
  }
})();
"#;

    Ok(template
        .replace("__ARIA_HANDLER_CODE__", &handler_json)
        .replace("__ARIA_TOOL_ARGS__", &args_json))
}

fn parse_exec_json(stdout: &str) -> Result<Value, String> {
    let mut trimmed = stdout.trim();
    if trimmed.is_empty() {
        return Err("Tool handler produced empty output".to_string());
    }

    if let Some(last) = trimmed.lines().last() {
        trimmed = last.trim();
    }

    serde_json::from_str::<Value>(trimmed)
        .map_err(|e| format!("Invalid handler output JSON: {e}. Raw: {trimmed}"))
}

fn parse_tool_result(payload: Value) -> ToolResult {
    let Some(obj) = payload.as_object() else {
        return ToolResult {
            success: true,
            output: payload.to_string(),
            error: None,
        };
    };

    if obj.get("success").is_some() || obj.get("output").is_some() || obj.get("error").is_some() {
        let success = obj.get("success").and_then(Value::as_bool).unwrap_or(true);
        let output = obj
            .get("output")
            .map(|v| {
                if let Some(s) = v.as_str() {
                    s.to_string()
                } else {
                    v.to_string()
                }
            })
            .unwrap_or_default();
        let error = obj
            .get("error")
            .and_then(Value::as_str)
            .map(ToString::to_string)
            .or_else(|| {
                if success {
                    None
                } else {
                    Some("Tool execution failed".to_string())
                }
            });

        return ToolResult {
            success,
            output,
            error,
        };
    }

    ToolResult {
        success: true,
        output: payload.to_string(),
        error: None,
    }
}

async fn execute_handler_with_quilt(
    tool: &RegistryTool,
    args: Value,
) -> anyhow::Result<ToolResult> {
    use crate::quilt::client::{
        QuiltClient, QuiltContainerState, QuiltExecCommand, QuiltExecParams,
    };

    let quilt = match QuiltClient::from_env() {
        Ok(client) => client,
        Err(_) => {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(
                    "Registry tool execution requires Quilt. Set QUILT_API_URL and QUILT_API_KEY."
                        .to_string(),
                ),
            });
        }
    };

    let sandbox = tool.sandbox_config.as_ref();
    let timeout_ms = sandbox_u64(sandbox, "timeoutMs").or(Some(60_000));

    let containers = quilt
        .list_containers()
        .await
        .map_err(|e| anyhow::anyhow!("Failed to list Quilt containers: {e}"))?;
    let container = containers
        .iter()
        .find(|c| c.name == "aria-exec")
        .cloned()
        .or_else(|| {
            containers
                .iter()
                .find(|c| c.state == QuiltContainerState::Running)
                .cloned()
        })
        .ok_or_else(|| anyhow::anyhow!("No usable Quilt container found (expected aria-exec)"))?;

    if container.state != QuiltContainerState::Running {
        quilt.start_container(&container.id).await?;
    }

    let script_path = format!("/tmp/aria-tool-{}.js", uuid::Uuid::new_v4());
    let wrapper = js_exec_wrapper(&tool.handler_code, &args)?;
    let write_script = QuiltExecParams {
        command: QuiltExecCommand::Vec(vec![
            "sh".into(),
            "-c".into(),
            format!("cat > {script_path} << 'ARIA_TOOL_EOF'\n{wrapper}\nARIA_TOOL_EOF"),
        ]),
        workdir: Some("/tmp".into()),
        capture_output: Some(true),
        timeout_ms: Some(10_000),
        detach: Some(false),
    };
    quilt.exec(&container.id, write_script).await?;

    let exec = QuiltExecParams {
        command: QuiltExecCommand::Vec(vec!["node".into(), script_path.clone()]),
        workdir: Some("/tmp".into()),
        capture_output: Some(true),
        timeout_ms,
        detach: Some(false),
    };

    let exec_result = quilt.exec(&container.id, exec).await;
    let _ = quilt
        .exec(
            &container.id,
            QuiltExecParams {
                command: QuiltExecCommand::Vec(vec!["rm".into(), "-f".into(), script_path]),
                workdir: Some("/tmp".into()),
                capture_output: Some(true),
                timeout_ms: Some(5_000),
                detach: Some(false),
            },
        )
        .await;

    match exec_result {
        Ok(out) => {
            let parsed = match parse_exec_json(&out.stdout) {
                Ok(v) => v,
                Err(e) => {
                    return Ok(ToolResult {
                        success: false,
                        output: out.stdout,
                        error: Some(e),
                    })
                }
            };

            if out.exit_code != 0 {
                let msg = parsed
                    .get("error")
                    .and_then(Value::as_str)
                    .map(ToString::to_string)
                    .unwrap_or_else(|| format!("Handler exited with code {}", out.exit_code));

                return Ok(ToolResult {
                    success: false,
                    output: parsed.to_string(),
                    error: Some(msg),
                });
            }

            let wrapped = parsed
                .get("result")
                .cloned()
                .unwrap_or_else(|| parsed.clone());
            Ok(parse_tool_result(wrapped))
        }
        Err(e) => Ok(ToolResult {
            success: false,
            output: String::new(),
            error: Some(format!("Failed to execute tool in Quilt: {e}")),
        }),
    }
}

pub fn load_registry_tools(
    db: &crate::aria::db::AriaDb,
    tenant_id: &str,
) -> anyhow::Result<Vec<RegistryTool>> {
    db.with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT id, name, description, schema, handler_code, sandbox_config
             FROM aria_tools
             WHERE tenant_id = ?1 AND status = 'active'
             ORDER BY updated_at DESC",
        )?;

        let rows = stmt.query_map([tenant_id], |row| {
            let schema_str: String = row.get(3)?;
            let sandbox_str: Option<String> = row.get(5)?;

            let schema = serde_json::from_str::<Value>(&schema_str).unwrap_or_else(
                |_| serde_json::json!({"type": "object", "properties": {}, "required": []}),
            );
            let sandbox_config = sandbox_str.and_then(|s| serde_json::from_str::<Value>(&s).ok());

            Ok(RegistryTool {
                id: row.get(0)?,
                tenant_id: tenant_id.to_string(),
                name: row.get(1)?,
                description: row.get::<_, Option<String>>(2)?.unwrap_or_default(),
                schema,
                handler_code: row.get::<_, Option<String>>(4)?.unwrap_or_default(),
                sandbox_config,
            })
        })?;

        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    })
}

#[cfg(test)]
mod tests {
    use super::normalize_tool_schema;

    #[test]
    fn normalizes_parameter_list_schema() {
        let raw = serde_json::json!({
            "parameters": [
                {"name": "query", "type": "string", "description": "Search query", "required": true},
                {"name": "limit", "type": "number", "required": false}
            ]
        });

        let normalized = normalize_tool_schema(&raw);
        assert_eq!(normalized["type"], "object");
        assert_eq!(normalized["properties"]["query"]["type"], "string");
        assert_eq!(normalized["required"], serde_json::json!(["query"]));
    }
}
