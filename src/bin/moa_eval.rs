//! PR #8 — RAGAS-style retrieval evaluation harness.
//!
//! Loads JSONL goldens from `tests/evals/`, seeds a fresh SqliteMemory in
//! a temp dir, runs every query through `recall`, and reports per-domain
//! `context_precision@k`, `context_recall@k`, and MRR. CI compares the
//! per-domain numbers against `tests/evals/thresholds.toml` and fails when
//! any `_min` threshold is violated.
//!
//! Designed to run with the default cargo features (no ONNX, no network)
//! so PR pipelines stay fast. The corpus is small by design — the harness
//! detects regressions, not absolute quality. Grow the JSONL files as the
//! product grows.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::TempDir;
use zeroclaw::memory::sqlite::SqliteMemory;
use zeroclaw::memory::traits::{Memory, MemoryCategory};

#[derive(Debug, Clone, Deserialize)]
struct CorpusEntry {
    key: String,
    content: String,
    #[serde(default = "default_category")]
    category: String,
}

fn default_category() -> String {
    "core".to_string()
}

#[derive(Debug, Clone, Deserialize)]
struct GoldEntry {
    query: String,
    gold_keys: Vec<String>,
    #[serde(default = "default_domain")]
    domain: String,
}

fn default_domain() -> String {
    "unknown".to_string()
}

#[derive(Debug, Clone, Serialize)]
struct DomainScore {
    domain: String,
    queries: usize,
    context_recall: f64,
    context_precision: f64,
    mrr: f64,
}

#[derive(Debug, Serialize)]
struct EvalReport {
    top_k: usize,
    sets: Vec<DomainScore>,
    overall: DomainScore,
}

#[derive(Debug, Default, Deserialize)]
struct Thresholds {
    #[serde(default)]
    law: DomainThreshold,
    #[serde(default)]
    ko: DomainThreshold,
    #[serde(default)]
    en: DomainThreshold,
    #[serde(default)]
    overall: OverallThreshold,
}

#[derive(Debug, Default, Deserialize)]
struct DomainThreshold {
    #[serde(default)]
    context_recall_min: Option<f64>,
    #[serde(default)]
    context_precision_min: Option<f64>,
    #[serde(default)]
    mrr_min: Option<f64>,
}

#[derive(Debug, Default, Deserialize)]
struct OverallThreshold {
    /// Regression guard reserved for the CI workflow — the runner currently
    /// reports absolute scores; comparing against a prior-baseline JSON is a
    /// follow-up step performed by the workflow itself, which loads this
    /// value from `tests/evals/thresholds.toml`.
    #[serde(default)]
    #[allow(dead_code)]
    max_regression_fraction: Option<f64>,
}

#[derive(Debug, Default)]
struct Args {
    set: Option<String>,
    top_k: usize,
    output: Option<PathBuf>,
    evals_dir: PathBuf,
    /// PR #8 LLM judge — optional JSONL output containing the raw
    /// retrieval-context tuples the Python judge (`eval_rag_llm.py`)
    /// consumes. One line per gold query.
    emit_retrieval: Option<PathBuf>,
}

fn parse_args() -> Result<Args> {
    let mut args = Args {
        set: None,
        top_k: 10,
        output: None,
        evals_dir: PathBuf::from("tests/evals"),
        emit_retrieval: None,
    };
    let mut iter = std::env::args().skip(1);
    while let Some(a) = iter.next() {
        match a.as_str() {
            "--set" => args.set = iter.next(),
            "--top-k" => {
                let v = iter
                    .next()
                    .context("--top-k requires a value")?
                    .parse::<usize>()
                    .context("--top-k must be a positive integer")?;
                if v == 0 {
                    anyhow::bail!("--top-k must be > 0");
                }
                args.top_k = v;
            }
            "--output" => args.output = iter.next().map(PathBuf::from),
            "--evals-dir" => {
                args.evals_dir = iter
                    .next()
                    .map(PathBuf::from)
                    .context("--evals-dir requires a path")?;
            }
            "--emit-retrieval" => {
                args.emit_retrieval = iter.next().map(PathBuf::from);
            }
            "--help" | "-h" => {
                print_help();
                std::process::exit(0);
            }
            other => anyhow::bail!("unknown argument: {other}"),
        }
    }
    Ok(args)
}

fn print_help() {
    println!(
        "moa_eval — retrieval evaluation harness (PR #8)\n\
         \n\
         USAGE:\n  \
           cargo run --bin moa_eval -- [OPTIONS]\n\
         \n\
         OPTIONS:\n  \
           --set <ko|en|law>          Run a single domain (default: all)\n  \
           --top-k <N>                Recall window size (default: 10)\n  \
           --output <PATH>            Write JSON report to file (default: stdout text)\n  \
           --evals-dir <PATH>         Override evals dir (default: tests/evals)\n  \
           --emit-retrieval <PATH>    Emit retrieval JSONL for LLM judge\n  \
           -h, --help                 Show this help"
    );
}

fn load_jsonl<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<Vec<T>> {
    let raw = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    raw.lines()
        .filter(|l| !l.trim().is_empty() && !l.trim_start().starts_with('#'))
        .map(|l| {
            serde_json::from_str(l)
                .with_context(|| format!("parse line `{l}` in {}", path.display()))
        })
        .collect()
}

async fn seed_corpus(mem: &SqliteMemory, corpus: &[CorpusEntry]) -> Result<()> {
    for entry in corpus {
        let cat = parse_category(&entry.category);
        mem.store(&entry.key, &entry.content, cat, None)
            .await
            .with_context(|| format!("seed {}", entry.key))?;
    }
    Ok(())
}

fn parse_category(s: &str) -> MemoryCategory {
    match s.to_ascii_lowercase().as_str() {
        "core" => MemoryCategory::Core,
        "daily" => MemoryCategory::Daily,
        "conversation" => MemoryCategory::Conversation,
        custom => MemoryCategory::Custom(custom.to_string()),
    }
}

async fn evaluate_set(
    mem: &SqliteMemory,
    golds: &[GoldEntry],
    top_k: usize,
    emit_retrieval: Option<&Path>,
) -> Result<HashMap<String, DomainScore>> {
    // Group by domain so we can report per-set numbers.
    let mut per_domain: HashMap<String, Vec<(f64, f64, f64)>> = HashMap::new();
    // PR #8 LLM judge — stream retrieval tuples to JSONL when --emit-retrieval
    // is set. We buffer into a Vec then write once at the end so a mid-run
    // cargo-kill doesn't leave a half-written file.
    let mut retrieval_out: Vec<serde_json::Value> = Vec::new();

    for g in golds {
        let hits = mem
            .recall(&g.query, top_k, None)
            .await
            .with_context(|| format!("recall query `{}`", g.query))?;

        let retrieved_keys: Vec<&str> = hits.iter().map(|h| h.key.as_str()).collect();
        let gold_set: HashSet<&str> = g.gold_keys.iter().map(String::as_str).collect();

        // context_recall@k = |retrieved ∩ gold| / |gold|
        let intersection = retrieved_keys
            .iter()
            .filter(|k| gold_set.contains(*k))
            .count();
        let recall = if gold_set.is_empty() {
            0.0
        } else {
            intersection as f64 / gold_set.len() as f64
        };
        // context_precision@k = |retrieved ∩ gold| / |retrieved|
        let precision = if retrieved_keys.is_empty() {
            0.0
        } else {
            intersection as f64 / retrieved_keys.len() as f64
        };
        // MRR = 1 / rank of first gold hit (0 if none in top_k).
        let mrr = retrieved_keys
            .iter()
            .enumerate()
            .find(|(_, k)| gold_set.contains(*k))
            .map_or(0.0, |(i, _)| 1.0 / (i + 1) as f64);

        per_domain
            .entry(g.domain.clone())
            .or_default()
            .push((recall, precision, mrr));

        if emit_retrieval.is_some() {
            let contexts: Vec<&str> = hits.iter().map(|h| h.content.as_str()).collect();
            retrieval_out.push(serde_json::json!({
                "query": g.query,
                "gold_keys": g.gold_keys,
                "retrieved_keys": retrieved_keys,
                "retrieved_contexts": contexts,
                // answer intentionally empty — the Python judge treats
                // "" as "synthesize via judge model" or skip faithfulness
                // gracefully. See tests/evals/scripts/eval_rag_llm.py.
                "answer": "",
                "domain": g.domain,
            }));
        }
    }

    if let Some(path) = emit_retrieval {
        use std::io::Write;
        let mut file = fs::File::create(path)
            .with_context(|| format!("create retrieval file {}", path.display()))?;
        for entry in &retrieval_out {
            writeln!(file, "{entry}")?;
        }
        eprintln!("wrote {} retrieval records → {}", retrieval_out.len(), path.display());
    }

    let mut out = HashMap::new();
    for (domain, samples) in per_domain {
        let n = samples.len();
        let (r, p, m) = samples.iter().fold((0.0, 0.0, 0.0), |(a, b, c), (r, p, m)| {
            (a + r, b + p, c + m)
        });
        out.insert(
            domain.clone(),
            DomainScore {
                domain,
                queries: n,
                context_recall: r / n as f64,
                context_precision: p / n as f64,
                mrr: m / n as f64,
            },
        );
    }
    Ok(out)
}

fn enforce_thresholds(report: &EvalReport, thresholds: &Thresholds) -> Vec<String> {
    let mut violations = Vec::new();
    for set in &report.sets {
        let dom = match set.domain.as_str() {
            "law" => &thresholds.law,
            "ko" => &thresholds.ko,
            "en" => &thresholds.en,
            _ => continue,
        };
        if let Some(min) = dom.context_recall_min {
            if set.context_recall < min {
                violations.push(format!(
                    "domain {}: context_recall {:.3} < min {:.3}",
                    set.domain, set.context_recall, min
                ));
            }
        }
        if let Some(min) = dom.context_precision_min {
            if set.context_precision < min {
                violations.push(format!(
                    "domain {}: context_precision {:.3} < min {:.3}",
                    set.domain, set.context_precision, min
                ));
            }
        }
        if let Some(min) = dom.mrr_min {
            if set.mrr < min {
                violations.push(format!(
                    "domain {}: mrr {:.3} < min {:.3}",
                    set.domain, set.mrr, min
                ));
            }
        }
    }
    violations
}

fn print_text_report(report: &EvalReport, violations: &[String]) {
    println!("\n┌─ MoA retrieval eval report ─────────────────");
    println!("│ top-k = {}", report.top_k);
    println!("├─────────────────────────────────────────────");
    for s in &report.sets {
        println!(
            "│ {:>5}  N={:>2}  recall={:.3}  prec={:.3}  mrr={:.3}",
            s.domain, s.queries, s.context_recall, s.context_precision, s.mrr
        );
    }
    println!("├─────────────────────────────────────────────");
    println!(
        "│ {:>5}  N={:>2}  recall={:.3}  prec={:.3}  mrr={:.3}",
        report.overall.domain,
        report.overall.queries,
        report.overall.context_recall,
        report.overall.context_precision,
        report.overall.mrr
    );
    println!("└─────────────────────────────────────────────");
    if violations.is_empty() {
        println!("✓ thresholds OK");
    } else {
        println!("✗ {} threshold violation(s):", violations.len());
        for v in violations {
            println!("    - {v}");
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = parse_args()?;

    let corpus_path = args.evals_dir.join("corpus.jsonl");
    let corpus: Vec<CorpusEntry> = load_jsonl(&corpus_path)?;

    let mut goldens: Vec<GoldEntry> = Vec::new();
    let domains: Vec<&str> = match args.set.as_deref() {
        Some("ko") => vec!["ko"],
        Some("en") => vec!["en"],
        Some("law") => vec!["law"],
        Some(other) => anyhow::bail!("unknown --set value: {other}"),
        None => vec!["ko", "en", "law"],
    };
    for d in &domains {
        let path = args.evals_dir.join(format!("golden_{d}.jsonl"));
        let entries: Vec<GoldEntry> = load_jsonl(&path)?;
        goldens.extend(entries);
    }

    // Fresh DB per run — deterministic, no leftover state.
    let tmp = TempDir::new().context("create temp workspace")?;
    let mem = SqliteMemory::new(tmp.path()).context("open SqliteMemory")?;
    seed_corpus(&mem, &corpus).await?;

    let per_domain = evaluate_set(
        &mem,
        &goldens,
        args.top_k,
        args.emit_retrieval.as_deref(),
    )
    .await?;

    // Build the report. Overall is the unweighted mean across all queries.
    let total_queries: usize = per_domain.values().map(|d| d.queries).sum();
    let overall = if total_queries == 0 {
        DomainScore {
            domain: "overall".into(),
            queries: 0,
            context_recall: 0.0,
            context_precision: 0.0,
            mrr: 0.0,
        }
    } else {
        let (r, p, m) = per_domain.values().fold((0.0, 0.0, 0.0), |(a, b, c), d| {
            let n = d.queries as f64;
            (
                a + d.context_recall * n,
                b + d.context_precision * n,
                c + d.mrr * n,
            )
        });
        let n = total_queries as f64;
        DomainScore {
            domain: "overall".into(),
            queries: total_queries,
            context_recall: r / n,
            context_precision: p / n,
            mrr: m / n,
        }
    };

    let mut sets: Vec<DomainScore> = per_domain.into_values().collect();
    sets.sort_by(|a, b| a.domain.cmp(&b.domain));
    let report = EvalReport {
        top_k: args.top_k,
        sets,
        overall,
    };

    // Threshold enforcement (warnings/failures).
    let thresholds_path = args.evals_dir.join("thresholds.toml");
    let thresholds: Thresholds = if thresholds_path.exists() {
        let raw = fs::read_to_string(&thresholds_path)?;
        toml::from_str(&raw).context("parse thresholds.toml")?
    } else {
        Thresholds::default()
    };
    let violations = enforce_thresholds(&report, &thresholds);

    if let Some(path) = args.output.as_ref() {
        let json = serde_json::to_string_pretty(&serde_json::json!({
            "report": report,
            "violations": violations,
        }))?;
        fs::write(path, json).with_context(|| format!("write {}", path.display()))?;
        println!("wrote report → {}", path.display());
    } else {
        print_text_report(&report, &violations);
    }

    if !violations.is_empty() {
        std::process::exit(1);
    }
    Ok(())
}
