// ============================================================
// MoA v3.0 — Workflow Engine Skeleton
// Locations:
//   src/workflow/mod.rs
//   src/workflow/parser.rs     (YAML → IR)
//   src/workflow/exec.rs       (IR → 실행)
//   src/workflow/scaffold.rs   (Voice → YAML 생성기)
//   src/workflow/registry.rs   (도구 화이트리스트)
//
// 본 파일은 단일 스켈레톤으로 병합되어 있다. Claude Code 는 위 경로로 분리하라.
// ============================================================

use std::collections::HashMap;

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

// ────────────────────────────────────────────────────────────
// 1. Intermediate Representation (IR)
//    — YAML 를 파싱한 후의 내부 표현. Schema 검증 통과 보장.
// ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct WorkflowSpec {
    pub name: String,
    pub parent_category: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub inputs: Vec<InputDef>,
    pub steps: Vec<Step>,
    #[serde(default)]
    pub post_hooks: Vec<PostHook>,
    pub limits: Limits,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct InputDef {
    pub name: String,
    #[serde(rename = "type")]
    pub input_type: String,     // "string"/"date"/"number"/"file"/"selection"
    #[serde(default)]
    pub prompt: Option<String>,
    #[serde(default)]
    pub default: Option<serde_json::Value>,
    #[serde(default)]
    pub autofill_from: Option<String>, // ontology/memory lookup 경로
    #[serde(default)]
    pub required: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Step {
    MemoryRecall(MemoryRecallStep),
    MemoryStore(MemoryStoreStep),
    Sql(SqlStep),
    Llm(LlmStep),
    ToolCall(ToolCallStep),
    FileWrite(FileWriteStep),
    CalendarAdd(CalendarAddStep),
    PhoneAction(PhoneActionStep),
    Shell(ShellStep),
    Conditional(ConditionalStep),
    Loop(LoopStep),
    UserConfirm(UserConfirmStep),
}

// 간략 구조체들 (실제 구현 시 필드 확장)
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct MemoryRecallStep {
    pub id: String,
    pub query: String,
    #[serde(default = "default_rrf")]
    pub search_mode: String,          // "rrf" | "weighted"
    #[serde(default = "default_top_k")]
    pub top_k: usize,
}
fn default_rrf() -> String { "rrf".to_string() }
fn default_top_k() -> usize { 20 }

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct MemoryStoreStep {
    pub id: String,
    pub content: String,              // 템플릿 가능
    #[serde(default)]
    pub timeline_event_type: Option<String>,
    #[serde(default)]
    pub link_to_ontology: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SqlStep { pub id: String, pub query: String }

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct LlmStep {
    pub id: String,
    pub model: String,
    #[serde(default)]
    pub system: Option<String>,
    #[serde(default)]
    pub user: Option<String>,
    #[serde(default)]
    pub user_template: Option<String>,
    #[serde(default)]
    pub output: Option<String>,       // 결과 변수명
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ToolCallStep {
    pub id: String,
    pub tool: String,
    pub args: serde_json::Value,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct FileWriteStep {
    pub id: String,
    pub path: String,
    #[serde(default)]
    pub content_from: Option<String>, // "{{step_id.output}}"
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CalendarAddStep {
    pub id: String,
    pub title: String,
    pub date: String,
    #[serde(default)]
    pub duration_min: Option<u32>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PhoneActionStep {
    pub id: String,
    pub action: String,               // 'create_caller_ontology' / 'mark_vip' 등
    pub args: serde_json::Value,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ShellStep { pub id: String, pub command: String }

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ConditionalStep {
    pub id: String,
    pub cond: String,                 // 간단 expr language
    pub then: Vec<Step>,
    #[serde(default, rename = "else")]
    pub else_: Option<Vec<Step>>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct LoopStep {
    pub id: String,
    pub over: String,                 // "{{step_id.results}}"
    pub as_var: String,
    pub body: Vec<Step>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct UserConfirmStep {
    pub id: String,
    pub message: String,
    #[serde(default)]
    pub timeout_sec: Option<u32>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PostHook {
    #[serde(rename = "type")]
    pub hook_type: String,            // 'calendar_add'/'notify'/…
    pub args: serde_json::Value,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Limits {
    pub max_tokens_per_run: u32,
    pub max_llm_calls_per_run: u32,
    #[serde(default)]
    pub max_runtime_sec: Option<u32>,
}

// ────────────────────────────────────────────────────────────
// 2. Parser: YAML text → WorkflowSpec
// ────────────────────────────────────────────────────────────

pub fn parse_spec(yaml: &str) -> Result<WorkflowSpec> {
    let spec: WorkflowSpec = serde_yaml::from_str(yaml)
        .context("failed to parse workflow YAML")?;
    validate_spec(&spec)?;
    Ok(spec)
}

fn validate_spec(spec: &WorkflowSpec) -> Result<()> {
    // 1) step id 유일성
    let mut ids = std::collections::HashSet::new();
    for step in &spec.steps {
        let id = step_id(step);
        if !ids.insert(id.to_string()) {
            bail!("duplicate step id: {}", id);
        }
    }
    // 2) 비용 상한 필수
    if spec.limits.max_tokens_per_run == 0 || spec.limits.max_llm_calls_per_run == 0 {
        bail!("limits.max_tokens_per_run and max_llm_calls_per_run must be > 0");
    }
    // 3) 권한 검증은 registry 에서 별도 수행
    Ok(())
}

fn step_id(step: &Step) -> &str {
    match step {
        Step::MemoryRecall(s) => &s.id,
        Step::MemoryStore(s) => &s.id,
        Step::Sql(s) => &s.id,
        Step::Llm(s) => &s.id,
        Step::ToolCall(s) => &s.id,
        Step::FileWrite(s) => &s.id,
        Step::CalendarAdd(s) => &s.id,
        Step::PhoneAction(s) => &s.id,
        Step::Shell(s) => &s.id,
        Step::Conditional(s) => &s.id,
        Step::Loop(s) => &s.id,
        Step::UserConfirm(s) => &s.id,
    }
}

// ────────────────────────────────────────────────────────────
// 3. Execution Engine
// ────────────────────────────────────────────────────────────

pub struct ExecContext<'a> {
    pub db: &'a crate::memory::Db,
    pub tools: &'a crate::workflow::registry::ToolRegistry,
    pub device_id: String,
    pub vars: HashMap<String, serde_json::Value>,
    pub cost: CostTracker,
    pub limits: Limits,
}

#[derive(Default, Debug, Clone)]
pub struct CostTracker {
    pub tokens_in: u32,
    pub tokens_out: u32,
    pub llm_calls: u32,
}

impl CostTracker {
    fn check(&self, limits: &Limits) -> Result<()> {
        if self.tokens_in + self.tokens_out > limits.max_tokens_per_run {
            bail!("token budget exceeded");
        }
        if self.llm_calls > limits.max_llm_calls_per_run {
            bail!("llm call budget exceeded");
        }
        Ok(())
    }
}

pub async fn execute(
    spec: &WorkflowSpec,
    inputs: serde_json::Value,
    ctx: &mut ExecContext<'_>,
) -> Result<WorkflowRunResult> {
    // 1) workflow_runs INSERT (status=running)
    let run_uuid = uuid::Uuid::new_v4().to_string();
    let input_sha256 = sha256_hex(&serde_json::to_vec(&inputs)?);
    // TODO(Claude Code): 실제 DB INSERT

    // 2) inputs → vars
    if let Some(obj) = inputs.as_object() {
        for (k, v) in obj {
            ctx.vars.insert(format!("input.{}", k), v.clone());
        }
    }

    // 3) step 순차 실행
    for step in &spec.steps {
        execute_step(step, ctx).await
            .with_context(|| format!("step failed: {}", step_id(step)))?;
        ctx.cost.check(&ctx.limits)?;
    }

    // 4) post hooks
    for hook in &spec.post_hooks {
        run_post_hook(hook, ctx).await?;
    }

    // 5) workflow_runs UPDATE (status=success, output_sha256, cost)
    let output = serde_json::to_value(&ctx.vars)?;
    let output_sha256 = sha256_hex(&serde_json::to_vec(&output)?);
    // TODO(Claude Code): 실제 DB UPDATE

    Ok(WorkflowRunResult {
        run_uuid,
        input_sha256,
        output_sha256,
        output,
        cost: ctx.cost.clone(),
    })
}

async fn execute_step(step: &Step, ctx: &mut ExecContext<'_>) -> Result<()> {
    match step {
        Step::MemoryRecall(s) => {
            let query = render_template(&s.query, &ctx.vars)?;
            // RRF 검색 호출
            let cfg = crate::memory::hybrid::HybridSearchConfig {
                mode: parse_search_mode(&s.search_mode),
                top_k: s.top_k,
                ..Default::default()
            };
            let results = crate::memory::hybrid::hybrid_search(ctx.db, &query, None, &cfg).await?;
            ctx.vars.insert(format!("{}.results", s.id), serde_json::to_value(&results)?);
        }
        Step::Llm(s) => {
            // TODO: 권한 체크 (model 이 사용자에게 허용되었는지)
            ctx.cost.llm_calls += 1;
            let _rendered_system = s.system.as_deref()
                .map(|t| render_template(t, &ctx.vars)).transpose()?;
            let _rendered_user = s.user_template.as_deref()
                .or(s.user.as_deref())
                .map(|t| render_template(t, &ctx.vars))
                .transpose()?;
            // TODO(Claude Code): provider 호출, 토큰 카운트 반영
            // ctx.cost.tokens_in += ...; ctx.cost.tokens_out += ...;
        }
        Step::ToolCall(s) => {
            ctx.tools.check_permission(&s.tool, &ctx.vars)?;
            let _result = ctx.tools.invoke(&s.tool, s.args.clone()).await?;
            // ctx.vars.insert(format!("{}.output", s.id), result);
        }
        Step::UserConfirm(s) => {
            // TODO: Tauri IPC 로 사용자에게 confirmation 프롬프트 전송
            // 타임아웃 시 status=paused 로 재개 가능해야 함
            let _ = s;
        }
        // 나머지 step 타입들은 유사 패턴으로 구현
        _ => {
            // TODO(Claude Code): 각 Step 변형별 구현
        }
    }
    Ok(())
}

async fn run_post_hook(_hook: &PostHook, _ctx: &mut ExecContext<'_>) -> Result<()> {
    // TODO
    Ok(())
}

fn parse_search_mode(s: &str) -> crate::memory::hybrid::SearchMode {
    match s {
        "rrf" => crate::memory::hybrid::SearchMode::Rrf,
        _ => crate::memory::hybrid::SearchMode::Weighted,
    }
}

fn render_template(tpl: &str, vars: &HashMap<String, serde_json::Value>) -> Result<String> {
    // TODO(Claude Code): handlebars 또는 간단 `{{var}}` 치환기 구현.
    //   - 점 표기법 지원: {{step_id.results}}
    //   - 안전: 치환 실패 시 에러 (누락 변수 허용 X)
    let mut out = tpl.to_string();
    for (k, v) in vars {
        let placeholder = format!("{{{{{}}}}}", k);
        let replacement = match v {
            serde_json::Value::String(s) => s.clone(),
            _ => v.to_string(),
        };
        out = out.replace(&placeholder, &replacement);
    }
    Ok(out)
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    hex::encode(h.finalize())
}

// ────────────────────────────────────────────────────────────
// 4. Result
// ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct WorkflowRunResult {
    pub run_uuid: String,
    pub input_sha256: String,
    pub output_sha256: String,
    pub output: serde_json::Value,
    pub cost: CostTracker,
}

// ────────────────────────────────────────────────────────────
// 5. Scaffolder (Voice → YAML)  — 별도 파일 src/workflow/scaffold.rs
// ────────────────────────────────────────────────────────────

pub mod scaffold {
    use super::*;

    pub struct ScaffoldRequest {
        pub utterance: String,
        pub user_category_hint: Option<String>,
        pub known_tools: Vec<String>,
    }

    pub struct ScaffoldResponse {
        pub yaml: String,
        pub estimated_cost_tokens: u32,
        pub warnings: Vec<String>,
    }

    pub async fn generate(req: ScaffoldRequest) -> Result<ScaffoldResponse> {
        // 1) 유사 워크플로우 few-shot 3개 검색 (RRF on workflows_fts)
        // 2) tool_registry 필터 (사용자 권한)
        // 3) Claude Opus 호출 → YAML 초안
        // 4) parse_spec() 로 검증
        // 5) dry-run mock
        // 6) 비용 추정 (토크나이저로 prompt 길이 × 스텝 수)
        // 7) Err 나면 재시도 1회 (LLM 에 에러 메시지 포함해서 self-correction)
        let _ = req;
        Err(anyhow!("TODO(Claude Code): implement scaffolder"))
    }
}

// ────────────────────────────────────────────────────────────
// 6. Tool Registry  — src/workflow/registry.rs
// ────────────────────────────────────────────────────────────

pub mod registry {
    use super::*;

    pub struct ToolRegistry {
        // TODO: HashMap<String, Arc<dyn Tool>>
    }

    impl ToolRegistry {
        pub fn check_permission(&self, _tool: &str, _vars: &HashMap<String, serde_json::Value>) -> Result<()> {
            // 카테고리별 화이트리스트 + 파일 경로 sandbox
            Ok(())
        }
        pub async fn invoke(&self, _tool: &str, _args: serde_json::Value) -> Result<serde_json::Value> {
            Err(anyhow!("TODO(Claude Code): wire MoA tool dispatcher"))
        }
    }
}

// ────────────────────────────────────────────────────────────
// 7. 테스트
// ────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_YAML: &str = r#"
name: "상담일지 작성"
parent_category: "document"
inputs:
  - name: client_name
    type: string
    required: true
steps:
  - type: memory_recall
    id: fetch
    query: "{{input.client_name}} 상담 내역"
    search_mode: rrf
    top_k: 10
  - type: llm
    id: draft
    model: claude-opus-4-6
    system: "법률 문서 보조"
    user_template: "의뢰인: {{input.client_name}}\n자료: {{fetch.results}}"
limits:
  max_tokens_per_run: 30000
  max_llm_calls_per_run: 5
"#;

    #[test]
    fn parses_sample() {
        let spec = parse_spec(SAMPLE_YAML).expect("parse");
        assert_eq!(spec.name, "상담일지 작성");
        assert_eq!(spec.steps.len(), 2);
    }

    #[test]
    fn rejects_duplicate_step_ids() {
        let yaml = SAMPLE_YAML.replace("id: draft", "id: fetch");
        assert!(parse_spec(&yaml).is_err());
    }
}
