use parking_lot::Mutex;
use serde::Serialize;
use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

#[derive(Debug, Default, Serialize)]
pub struct ToolCallRecord {
    pub name: String,
    pub args: serde_json::Value,
    pub output: String,
    pub success: bool,
    pub duration_ms: u64,
}

#[derive(Debug, Default, Serialize)]
pub struct TurnRecord {
    pub turn: usize,
    pub timestamp: String,
    pub prompt_preview: String,
    pub selected_tools: Vec<String>,
    pub tool_calls: Vec<ToolCallRecord>,
    pub reply_preview: String,
    pub llm_duration_ms: Option<u64>,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
}

#[derive(Debug, Default, Serialize)]
pub struct SessionSummary {
    pub total_turns: usize,
    pub total_tool_calls: usize,
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub elapsed_ms: u64,
    pub unique_tools: Vec<String>,
    pub tool_frequency: HashMap<String, usize>,
    pub tool_sequence: Vec<Vec<String>>,
}

#[derive(Debug, Default, Serialize)]
pub struct SessionData {
    pub session_id: String,
    pub start_time: String,
    pub end_time: Option<String>,
    pub channel: String,
    pub provider: String,
    pub model: String,
    pub user_query: String,
    pub turns: Vec<TurnRecord>,
    pub summary: SessionSummary,
}

/// Active session handle. Cheaply cloneable (Arc-backed).
#[derive(Clone)]
pub struct SessionRecorder(Arc<Mutex<SessionData>>);

impl SessionRecorder {
    pub fn new(data: SessionData) -> Self {
        Self(Arc::new(Mutex::new(data)))
    }

    pub fn init_turn(&self, turn: usize) {
        let mut data = self.0.lock();
        while data.turns.len() <= turn {
            let idx = data.turns.len();
            data.turns.push(TurnRecord {
                turn: idx,
                timestamp: chrono::Utc::now().to_rfc3339(),
                ..Default::default()
            });
        }
    }

    pub fn record_prompt(&self, turn: usize, preview: &str) {
        let mut data = self.0.lock();
        if let Some(t) = data.turns.get_mut(turn) {
            t.prompt_preview = preview.chars().take(500).collect();
        }
    }

    pub fn record_llm_response(
        &self,
        turn: usize,
        reply: &str,
        in_tok: Option<u64>,
        out_tok: Option<u64>,
        ms: u64,
        _debug: bool,
    ) {
        let mut data = self.0.lock();
        if let Some(t) = data.turns.get_mut(turn) {
            t.reply_preview = reply.chars().take(500).collect();
            t.llm_duration_ms = Some(ms);
            t.input_tokens = in_tok;
            t.output_tokens = out_tok;
        }
    }

    pub fn record_selected_tools(&self, turn: usize, tools: &[&str]) {
        let mut data = self.0.lock();
        if let Some(t) = data.turns.get_mut(turn) {
            t.selected_tools = tools.iter().map(|s| s.to_string()).collect();
        }
    }

    pub fn record_tool_call(
        &self,
        name: &str,
        args: &serde_json::Value,
        output: &str,
        success: bool,
        ms: u64,
    ) {
        let mut data = self.0.lock();
        if let Some(t) = data.turns.last_mut() {
            t.tool_calls.push(ToolCallRecord {
                name: name.to_string(),
                args: args.clone(),
                output: output.chars().take(8000).collect(),
                success,
                duration_ms: ms,
            });
        }
    }

    pub fn finalize_and_write(
        &self,
        dir: &std::path::Path,
        start: std::time::Instant,
        max_files: usize,
    ) {
        let mut data = self.0.lock();
        data.end_time = Some(chrono::Utc::now().to_rfc3339());
        build_summary_locked(&mut data, start);
        let _ = write_report_locked(&data, dir);
        evict_old_reports(dir, max_files);
    }

    pub fn finalize_and_write_error(
        &self,
        dir: &std::path::Path,
        start: std::time::Instant,
        error: &str,
        max_files: usize,
    ) {
        let mut data = self.0.lock();
        data.end_time = Some(chrono::Utc::now().to_rfc3339());
        // Record error in the last turn's reply preview
        if let Some(t) = data.turns.last_mut() {
            t.reply_preview = format!("[ERROR] {}", error.chars().take(500).collect::<String>());
        }
        build_summary_locked(&mut data, start);
        let _ = write_report_locked(&data, dir);
        evict_old_reports(dir, max_files);
    }
}

fn build_summary_locked(data: &mut SessionData, start: std::time::Instant) {
    let mut freq: HashMap<String, usize> = HashMap::new();
    let mut unique: BTreeSet<String> = BTreeSet::new();
    let mut total_calls = 0usize;
    let mut in_tok = 0u64;
    let mut out_tok = 0u64;
    let mut seq = vec![];

    for turn in &data.turns {
        in_tok += turn.input_tokens.unwrap_or(0);
        out_tok += turn.output_tokens.unwrap_or(0);
        total_calls += turn.tool_calls.len();
        let names: Vec<String> = turn.tool_calls.iter().map(|t| t.name.clone()).collect();
        for n in &names {
            *freq.entry(n.clone()).or_default() += 1;
            unique.insert(n.clone());
        }
        seq.push(names);
    }

    data.summary = SessionSummary {
        total_turns: data.turns.len(),
        total_tool_calls: total_calls,
        total_input_tokens: in_tok,
        total_output_tokens: out_tok,
        #[allow(clippy::cast_possible_truncation)]
        elapsed_ms: start.elapsed().as_millis() as u64,
        unique_tools: unique.into_iter().collect(),
        tool_frequency: freq,
        tool_sequence: seq,
    };
}

fn write_report_locked(
    data: &SessionData,
    dir: &std::path::Path,
) -> anyhow::Result<std::path::PathBuf> {
    std::fs::create_dir_all(dir)?;
    let ts = data
        .start_time
        .replace([':', '-', 'T'], "_")
        .split('.')
        .next()
        .unwrap_or("unknown")
        .to_string();
    let id_prefix = &data.session_id[..8.min(data.session_id.len())];
    let filename = format!("session_{}_{}.json", ts, id_prefix);
    let path = dir.join(filename);
    let json = serde_json::to_string_pretty(data)?;
    std::fs::write(&path, json)?;
    tracing::info!(path = %path.display(), "session.report.written");
    Ok(path)
}

fn evict_old_reports(dir: &std::path::Path, max_files: usize) {
    if max_files == 0 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut files: Vec<std::path::PathBuf> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.extension().is_some_and(|ext| ext == "json")
                && p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with("session_"))
        })
        .collect();
    if files.len() <= max_files {
        return;
    }
    files.sort();
    for f in &files[..files.len() - max_files] {
        let _ = std::fs::remove_file(f);
    }
}
