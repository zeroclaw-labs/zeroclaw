//! Opt-in pixel-only proof through the real agent, dispatcher, and OAuth provider.

use anyhow::{Context, Result};
use async_trait::async_trait;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use zeroclaw::agent::agent::Agent;
use zeroclaw::agent::dispatcher::NativeToolDispatcher;
use zeroclaw::providers::ModelProviderRuntimeOptions;
use zeroclaw::providers::auth::{AuthService, openai_oauth};
use zeroclaw::providers::openai_codex::OpenAiCodexModelProvider;
use zeroclaw::tools::scoped::ScopedToolRegistry;
use zeroclaw_api::media::{MarkerKind, RenderedMarker};
use zeroclaw_api::tool::{Tool, ToolOutput, ToolResult};

// Synthetic 64x64 solid-color PNGs. Labels stay in the test oracle; the tool
// returns only image bytes and a neutral caption, with no answer in its text.
const PALETTE: [(&str, &str); 6] = [
    (
        "red",
        "iVBORw0KGgoAAAANSUhEUgAAAEAAAABACAIAAAAlC+aJAAAAb0lEQVR4nO3PAQkAAAyEwO9feoshgnABdLep8QUNyPEFDcjxBQ3I8QUNyPEFDcjxBQ3I8QUNyPEFDcjxBQ3I8QUNyPEFDcjxBQ3I8QUNyPEFDcjxBQ3I8QUNyPEFDcjxBQ3I8QUNyPEFDcjxBQ3IPanc8OLDQitxAAAAAElFTkSuQmCC",
    ),
    (
        "green",
        "iVBORw0KGgoAAAANSUhEUgAAAEAAAABACAIAAAAlC+aJAAAAcElEQVR4nO3PAQkAAAyEwO+fZwEXQwThAuh2c+MLGpDjCxqQ4wsakOMLGpDjCxqQ4wsakOMLGpDjCxqQ4wsakOMLGpDjCxqQ4wsakOMLGpDjCxqQ4wsakOMLGpDjCxqQ4wsakOMLGpDjCxqQ4wsacHsYSECm5YvD+gAAAABJRU5ErkJggg==",
    ),
    (
        "blue",
        "iVBORw0KGgoAAAANSUhEUgAAAEAAAABACAIAAAAlC+aJAAAAb0lEQVR4nO3PAQkAAAyEwO9feoshgnABdNvJ8QUNyPEFDcjxBQ3I8QUNyPEFDcjxBQ3I8QUNyPEFDcjxBQ3I8QUNyPEFDcjxBQ3I8QUNyPEFDcjxBQ3I8QUNyPEFDcjxBQ3I8QUNyPEFDcjxBQ2oPcf88OIhvJ6vAAAAAElFTkSuQmCC",
    ),
    (
        "yellow",
        "iVBORw0KGgoAAAANSUhEUgAAAEAAAABACAIAAAAlC+aJAAAAdklEQVR4nO3PQQkAMAzAwPo33Yno4xgEIuAyu/N1XtCAFjSgBQ1oQQNa0IAWNKAFDWhBA1rQgBY0oAUNaEEDWtCAFjSgBQ1oQQNa0IAWNKAFDWhBA1rQgBY0oAUNaEEDWtCAFjSgBQ1oQQNa0IAWNKAFDWjBsQcyl+HSnr9raQAAAABJRU5ErkJggg==",
    ),
    (
        "magenta",
        "iVBORw0KGgoAAAANSUhEUgAAAEAAAABACAIAAAAlC+aJAAAAdklEQVR4nO3PQQkAMAzAwPo33Yno4xgEIuAyO/t1wwUNaEEDWtCAFjSgBQ1oQQNa0IAWNKAFDWhBA1rQgBY0oAUNaEEDWtCAFjSgBQ1oQQNa0IAWNKAFDWhBA1rQgBY0oAUNaEEDWtCAFjSgBQ1oQQNa0IAWHHtBp+HSthEAJgAAAABJRU5ErkJggg==",
    ),
    (
        "cyan",
        "iVBORw0KGgoAAAANSUhEUgAAAEAAAABACAIAAAAlC+aJAAAAdklEQVR4nO3PQQkAMAzAwPo33Yno4xgEIuAys/t3XtCAFjSgBQ1oQQNa0IAWNKAFDWhBA1rQgBY0oAUNaEEDWtCAFjSgBQ1oQQNa0IAWNKAFDWhBA1rQgBY0oAUNaEEDWtCAFjSgBQ1oQQNa0IAWNKAFDWjBrQdQt+HSQM5t4gAAAABJRU5ErkJggg==",
    ),
];

struct SyntheticImages {
    images: Vec<RenderedMarker>,
    calls: Arc<AtomicUsize>,
}

zeroclaw_api::mock_tool_attribution!(SyntheticImages);

#[async_trait]
impl Tool for SyntheticImages {
    fn name(&self) -> &str {
        "synthetic_images"
    }

    fn description(&self) -> &str {
        "Return four synthetic images for visual inspection."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({"type": "object", "properties": {}, "additionalProperties": false})
    }

    async fn execute(&self, _args: serde_json::Value) -> Result<ToolResult> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        eprintln!("tool_invocation=synthetic_images");
        let mut output = ToolOutput::text("Four images attached in order.");
        for image in &self.images {
            output = output.with_attachment(image.clone());
        }
        Ok(ToolResult {
            success: true,
            output,
            error: None,
        })
    }
}

#[tokio::test]
#[ignore = "requires explicit ZEROCLAW_TEST_CODEX_AUTH_FILE and live OpenAI Codex access"]
async fn agent_reads_randomized_tool_images() -> Result<()> {
    let auth_file = std::env::var("ZEROCLAW_TEST_CODEX_AUTH_FILE")
        .context("set ZEROCLAW_TEST_CODEX_AUTH_FILE to an explicitly supplied Codex auth.json")?;
    let state = tempfile::tempdir()?;
    let workspace = tempfile::tempdir()?;
    let auth = AuthService::new(state.path(), true);
    openai_oauth::import_codex_auth_profile(&auth, "test", Path::new(&auth_file)).await?;
    let options = ModelProviderRuntimeOptions {
        zeroclaw_dir: Some(state.path().to_path_buf()),
        auth_profile_override: Some("test".into()),
        secrets_encrypt: true,
        ..Default::default()
    };
    let provider = OpenAiCodexModelProvider::new("test", &options, None)?;
    let choices = uuid::Uuid::new_v4();
    let selected: Vec<_> = choices.as_bytes()[..4]
        .iter()
        .map(|byte| PALETTE[usize::from(*byte) % PALETTE.len()])
        .collect();
    let expected: Vec<_> = selected.iter().map(|(label, _)| *label).collect();
    let calls = Arc::new(AtomicUsize::new(0));
    let tool = SyntheticImages {
        images: selected
            .iter()
            .map(|(_, png)| RenderedMarker {
                kind: MarkerKind::Image,
                target: format!("data:image/png;base64,{png}"),
            })
            .collect(),
        calls: Arc::clone(&calls),
    };
    // A direct provider instance keeps fallback disabled throughout the real loop.
    let mut agent = Agent::builder()
        .model_provider(Box::new(provider))
        .tools(ScopedToolRegistry::from_raw_for_test(vec![Box::new(tool)]))
        .memory(crate::support::helpers::make_memory())
        .observer(crate::support::helpers::make_observer())
        .tool_dispatcher(Box::new(NativeToolDispatcher))
        .workspace_dir(workspace.path().to_path_buf())
        .model_name("gpt-5.3-codex".into())
        .model_provider_name("openai-codex".into())
        .build()?;
    eprintln!("provider=openai-codex model=gpt-5.3-codex fallback=disabled");
    let response = tokio::time::timeout(
        std::time::Duration::from_secs(180),
        agent.turn(
            "Call synthetic_images exactly once. Inspect the four returned images in order. \
         Reply with only a JSON array of four lowercase color names, choosing from red, \
         green, blue, yellow, magenta, cyan. Determine each answer from the pixels.",
        ),
    )
    .await
    .context("live tool-image test timed out")??;
    let observed: Vec<String> = serde_json::from_str(response.trim())
        .context("model did not return the requested JSON color array")?;
    let invocation_count = calls.load(Ordering::SeqCst);
    eprintln!("tool_calls={invocation_count} expected={expected:?} observed={observed:?}");
    assert_eq!(invocation_count, 1);
    assert_eq!(observed, expected);
    Ok(())
}
