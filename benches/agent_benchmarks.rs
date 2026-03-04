//! Performance benchmarks for ZeroClaw hot paths.
//!
//! Benchmarks cover:
//!   - Tool dispatch (XML parsing, native parsing)
//!   - Memory store/recall cycles (SQLite backend)
//!   - Agent turn cycle (full orchestration loop)
//!
//! Run: `cargo bench`
//!
//! Ref: https://github.com/zeroclaw-labs/zeroclaw/issues/618 (item 7)

use criterion::{criterion_group, criterion_main, Criterion};
use std::hint::black_box;
use std::sync::{Arc, Mutex};

use zeroclaw::agent::agent::Agent;
use zeroclaw::agent::dispatcher::{NativeToolDispatcher, ToolDispatcher, XmlToolDispatcher};
use zeroclaw::config::MemoryConfig;
use zeroclaw::memory;
use zeroclaw::memory::{Memory, MemoryCategory};
use zeroclaw::observability::{NoopObserver, Observer};
use zeroclaw::providers::{ChatRequest, ChatResponse, Provider, ToolCall};
use zeroclaw::tools::{Tool, ToolResult};

use anyhow::Result;
use async_trait::async_trait;

// ─────────────────────────────────────────────────────────────────────────────
// Mock infrastructure (mirrors test mocks, kept local for benchmark isolation)
// ─────────────────────────────────────────────────────────────────────────────

struct BenchProvider {
    responses: Mutex<Vec<ChatResponse>>,
}

impl BenchProvider {
    fn text_only(text: &str) -> Self {
        Self {
            responses: Mutex::new(vec![ChatResponse {
                text: Some(text.into()),
                tool_calls: vec![],
                usage: None,
                reasoning_content: None,
                quota_metadata: None,
                stop_reason: None,
                raw_stop_reason: None,
            }]),
        }
    }

    fn with_tool_then_text() -> Self {
        Self {
            responses: Mutex::new(vec![
                ChatResponse {
                    text: Some(String::new()),
                    tool_calls: vec![ToolCall {
                        id: "tc1".into(),
                        name: "noop".into(),
                        arguments: "{}".into(),
                    }],
                    usage: None,
                    reasoning_content: None,
                    quota_metadata: None,
                    stop_reason: None,
                    raw_stop_reason: None,
                },
                ChatResponse {
                    text: Some("done".into()),
                    tool_calls: vec![],
                    usage: None,
                    reasoning_content: None,
                    quota_metadata: None,
                    stop_reason: None,
                    raw_stop_reason: None,
                },
            ]),
        }
    }
}

#[async_trait]
impl Provider for BenchProvider {
    async fn chat_with_system(
        &self,
        _system_prompt: Option<&str>,
        _message: &str,
        _model: &str,
        _temperature: f64,
    ) -> Result<String> {
        Ok("fallback".into())
    }

    async fn chat(
        &self,
        _request: ChatRequest<'_>,
        _model: &str,
        _temperature: f64,
    ) -> Result<ChatResponse> {
        let mut guard = self.responses.lock().unwrap();
        if guard.is_empty() {
            return Ok(ChatResponse {
                text: Some("done".into()),
                tool_calls: vec![],
                usage: None,
                reasoning_content: None,
                quota_metadata: None,
                stop_reason: None,
                raw_stop_reason: None,
            });
        }
        Ok(guard.remove(0))
    }
}

struct NoopTool;

#[async_trait]
impl Tool for NoopTool {
    fn name(&self) -> &str {
        "noop"
    }
    fn description(&self) -> &str {
        "Does nothing"
    }
    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({"type": "object"})
    }
    async fn execute(&self, _args: serde_json::Value) -> Result<ToolResult> {
        Ok(ToolResult {
            success: true,
            output: String::new(),
            error: None,
        })
    }
}

fn make_memory() -> Arc<dyn Memory> {
    let cfg = MemoryConfig {
        backend: "none".into(),
        ..MemoryConfig::default()
    };
    Arc::from(memory::create_memory(&cfg, std::path::Path::new("/tmp"), None).unwrap())
}

fn make_sqlite_memory(dir: &std::path::Path) -> Arc<dyn Memory> {
    let cfg = MemoryConfig {
        backend: "sqlite".into(),
        ..MemoryConfig::default()
    };
    Arc::from(memory::create_memory(&cfg, dir, None).unwrap())
}

fn make_observer() -> Arc<dyn Observer> {
    Arc::from(NoopObserver {})
}

// ─────────────────────────────────────────────────────────────────────────────
// Benchmark: XML tool-call parsing
// ─────────────────────────────────────────────────────────────────────────────

fn bench_xml_parsing(c: &mut Criterion) {
    let dispatcher = XmlToolDispatcher;

    let single_tool = ChatResponse {
        text: Some(
            r#"Here is my analysis.
<tool_call>
{"name": "search", "arguments": {"query": "zeroclaw architecture"}}
</tool_call>
Let me know if you need more."#
                .into(),
        ),
        tool_calls: vec![],
        usage: None,
        reasoning_content: None,
        quota_metadata: None,
        stop_reason: None,
        raw_stop_reason: None,
    };

    let multi_tool = ChatResponse {
        text: Some(
            r#"<tool_call>
{"name": "read_file", "arguments": {"path": "src/main.rs"}}
</tool_call>
<tool_call>
{"name": "search", "arguments": {"query": "config"}}
</tool_call>
<tool_call>
{"name": "list_dir", "arguments": {"path": "src/"}}
</tool_call>"#
                .into(),
        ),
        tool_calls: vec![],
        usage: None,
        reasoning_content: None,
        quota_metadata: None,
        stop_reason: None,
        raw_stop_reason: None,
    };

    c.bench_function("xml_parse_single_tool_call", |b| {
        b.iter(|| dispatcher.parse_response(black_box(&single_tool)))
    });

    c.bench_function("xml_parse_multi_tool_call", |b| {
        b.iter(|| dispatcher.parse_response(black_box(&multi_tool)))
    });
}

// ─────────────────────────────────────────────────────────────────────────────
// Benchmark: Native tool-call parsing
// ─────────────────────────────────────────────────────────────────────────────

fn bench_native_parsing(c: &mut Criterion) {
    let dispatcher = NativeToolDispatcher;

    let response = ChatResponse {
        text: Some("I'll help you.".into()),
        tool_calls: vec![
            ToolCall {
                id: "tc1".into(),
                name: "search".into(),
                arguments: r#"{"query": "zeroclaw"}"#.into(),
            },
            ToolCall {
                id: "tc2".into(),
                name: "read_file".into(),
                arguments: r#"{"path": "src/main.rs"}"#.into(),
            },
        ],
        usage: None,
        reasoning_content: None,
        quota_metadata: None,
        stop_reason: None,
        raw_stop_reason: None,
    };

    c.bench_function("native_parse_tool_calls", |b| {
        b.iter(|| dispatcher.parse_response(black_box(&response)))
    });
}

// ─────────────────────────────────────────────────────────────────────────────
// Benchmark: Memory store + recall (SQLite)
// ─────────────────────────────────────────────────────────────────────────────

fn bench_memory_operations(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let tmp = tempfile::TempDir::new().unwrap();
    let mem = make_sqlite_memory(tmp.path());

    // Seed with entries for recall benchmarks
    rt.block_on(async {
        for i in 0..100 {
            mem.store(
                &format!("key_{i}"),
                &format!("Content entry number {i} about zeroclaw agent runtime"),
                MemoryCategory::Core,
                None,
            )
            .await
            .unwrap();
        }
    });

    c.bench_function("memory_store_single", |b| {
        let counter = std::sync::atomic::AtomicUsize::new(1000);
        b.iter(|| {
            let idx = counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            rt.block_on(async {
                mem.store(
                    &format!("bench_key_{idx}"),
                    "Benchmark content for store operation",
                    MemoryCategory::Daily,
                    None,
                )
                .await
                .unwrap();
            });
        });
    });

    c.bench_function("memory_recall_top10", |b| {
        b.iter(|| {
            rt.block_on(async {
                mem.recall(black_box("zeroclaw agent"), 10, None)
                    .await
                    .unwrap()
            })
        });
    });

    c.bench_function("memory_count", |b| {
        b.iter(|| rt.block_on(async { mem.count().await.unwrap() }));
    });
}

// ─────────────────────────────────────────────────────────────────────────────
// Benchmark: Full agent turn cycle
// ─────────────────────────────────────────────────────────────────────────────

fn bench_agent_turn(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().unwrap();

    c.bench_function("agent_turn_text_only", |b| {
        b.iter(|| {
            rt.block_on(async {
                let provider = Box::new(BenchProvider::text_only("benchmark response"));
                let mut agent = Agent::builder()
                    .provider(provider)
                    .tools(vec![Box::new(NoopTool) as Box<dyn Tool>])
                    .memory(make_memory())
                    .observer(make_observer())
                    .tool_dispatcher(Box::new(NativeToolDispatcher))
                    .workspace_dir(std::path::PathBuf::from("/tmp"))
                    .build()
                    .unwrap();
                agent.turn(black_box("hello")).await.unwrap()
            })
        });
    });

    c.bench_function("agent_turn_with_tool_call", |b| {
        b.iter(|| {
            rt.block_on(async {
                let provider = Box::new(BenchProvider::with_tool_then_text());
                let mut agent = Agent::builder()
                    .provider(provider)
                    .tools(vec![Box::new(NoopTool) as Box<dyn Tool>])
                    .memory(make_memory())
                    .observer(make_observer())
                    .tool_dispatcher(Box::new(NativeToolDispatcher))
                    .workspace_dir(std::path::PathBuf::from("/tmp"))
                    .build()
                    .unwrap();
                agent.turn(black_box("run tool")).await.unwrap()
            })
        });
    });
}

criterion_group!(
    benches,
    bench_xml_parsing,
    bench_native_parsing,
    bench_memory_operations,
    bench_agent_turn,
);
criterion_main!(benches);
