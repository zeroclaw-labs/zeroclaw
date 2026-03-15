use zeroclaw::hooks::{HookHandler, HookResult};
use zeroclaw::Skill;
use zeroclaw::tools::traits::{Tool, ToolResult};
use async_trait::async_trait;
use serde_json::Value;

struct HelloTool;

#[async_trait]
impl Tool for HelloTool {
    fn name(&self) -> &str {
        "hello_skill"
    }

    fn description(&self) -> &str {
        "A simple tool from the Hello World skill."
    }

    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "name": { "type": "string" }
            }
        })
    }

    async fn execute(&self, args: Value) -> anyhow::Result<ToolResult> {
        let name = args["name"].as_str().unwrap_or("World");
        Ok(ToolResult {
            success: true,
            output: format!("Hello, {}! This is a Skill-injected tool.", name),
            error: None,
        })
    }
}

struct HelloHook;

#[async_trait]
impl HookHandler for HelloHook {
    fn name(&self) -> &str {
        "hello_hook"
    }

    async fn on_startup(&self) {
        println!("🚀 Hello World Skill: Startup Hook Fired!");
    }

    async fn on_shutdown(&self) {
        println!("🛑 Hello World Skill: Shutdown Hook Fired!");
    }

    async fn before_tool_call(&self, name: String, args: Value) -> HookResult<(String, Value)> {
        if name == "hello_skill" {
            println!("🔍 Hello World Skill: Intercepting hello_skill call!");
        }
        HookResult::Continue((name, args))
    }
}

pub struct HelloWorldSkill;

impl Skill for HelloWorldSkill {
    fn name(&self) -> &str {
        "hello_world"
    }

    fn description(&self) -> &str {
        "A demonstration skill to verify the new unified skill system."
    }

    fn tools(&self) -> Vec<Box<dyn Tool>> {
        vec![Box::new(HelloTool)]
    }

    fn hooks(&self) -> Vec<Box<dyn HookHandler>> {
        vec![Box::new(HelloHook)]
    }

    fn prompt_contribution(&self) -> Option<String> {
        Some("This agent has the Hello World skill active. It can greet users with 'hello_skill'.".to_string())
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    println!("--- Hello World Skill Example ---");
    let skill = HelloWorldSkill;
    println!("Skill: {}", skill.name());
    println!("Description: {}", skill.description());

    let tools = skill.tools();
    println!("Tools: {}", tools.iter().map(|t| t.name()).collect::<Vec<_>>().join(", "));

    let hooks = skill.hooks();
    println!("Hooks: {}", hooks.iter().map(|h| h.name()).collect::<Vec<_>>().join(", "));

    if let Some(prompt) = skill.prompt_contribution() {
        println!("Prompt Contribution: {}", prompt);
    }

    println!("\nValidating tool execution:");
    let tool = &tools[0];
    let result = tool.execute(serde_json::json!({"name": "Agent"})).await?;
    println!("Tool Output: {}", result.output);

    println!("\nValidating hooks:");
    let hook = &hooks[0];
    hook.on_startup().await;
    let _ = hook.before_tool_call("hello_skill".into(), serde_json::json!({})).await;
    hook.on_shutdown().await;

    Ok(())
}
