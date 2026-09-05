//! `zeroclaw eval` — run the agent evaluation harness.

use anyhow::{Context, Result};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;
use zeroclaw_config::schema::Config;
use zeroclaw_eval::baseline::{self, Baseline, CaseComparison, SuiteKind};
use zeroclaw_eval::{
    CaseProvider, CaseReport, HistoryReceipt, HistoryRun, LlmTrace, Mode, RunDeps, SuiteReport,
    write_history_receipt,
};
use zeroclaw_runtime::agent::agent::build_session_model_provider;
use zeroclaw_runtime::i18n::{get_required_cli_string, get_required_cli_string_with_args};

#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};

/// Artifact root for eval diagnostics, relative to the install root.
///
/// Anchored to the config's install root rather than the process working
/// directory: a CWD-relative `target/...` path lets a run from a nested directory
/// drop unredacted transcripts into a tracked location, because the repository
/// ignores only the root-anchored `/target`.
const ARTIFACT_SUBDIR: &str = "eval-artifacts";

/// Owner-only pointer file naming the latest complete run directory.
const LAST_RUN_POINTER: &str = "last-run";
/// Stable file used to serialize completed-run publication across processes.
const PUBLISH_LOCK: &str = ".publish.lock";
/// Directory containing immutable, uniquely named run artifacts.
const RUNS_SUBDIR: &str = "runs";
/// Directory containing active runs that publishers must never clean up.
const STAGING_SUBDIR: &str = "staging";

/// Directory permissions for the artifact root: owner-only.
#[cfg(unix)]
const DIR_MODE: u32 = 0o700;
/// File permissions for a transcript dump: owner-only.
#[cfg(unix)]
const FILE_MODE: u32 = 0o600;

/// How many suffixed candidates a dump will try before giving up.
const MAX_DUMP_COLLISIONS: u32 = 1024;

/// The stable, private artifact root: `<install>/eval-artifacts`.
///
/// The dumps contain full conversation history, tool arguments and results, and
/// error strings — content that can include credentials surfaced by a tool. The
/// automatic dump requires no opt-in, so the location must not depend on where
/// the operator happened to run the command from.
#[must_use]
pub fn artifact_root(config: &Config) -> PathBuf {
    config.install_root_dir().join(ARTIFACT_SUBDIR)
}

/// Create `dir` (and parents) owner-only. On Unix the mode is applied at creation
/// time, so a new directory is never briefly world-readable. An existing leaf
/// directory is tightened too; dumps must not inherit a permissive prior mode.
pub fn create_private_dir(dir: &Path) -> Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    builder.mode(DIR_MODE);
    builder
        .create(dir)
        .with_context(|| format!("creating private eval artifact dir {}", dir.display()))?;
    #[cfg(unix)]
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(DIR_MODE))
        .with_context(|| format!("securing eval artifact dir {}", dir.display()))?;
    Ok(())
}

/// A unique staging directory for one run: `<artifact_root>/staging/<run_id>`.
///
/// Concurrent eval processes must not share a destructive directory, so the id
/// carries the pid and a monotonic timestamp and the directory is claimed with
/// `create_new` semantics (a plain `create_dir` fails if it already exists).
pub fn stage_run_dir(root: &Path) -> Result<PathBuf> {
    create_private_dir(&root.join(STAGING_SUBDIR))?;
    let pid = std::process::id();
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    for n in 0..MAX_DUMP_COLLISIONS {
        let candidate = root.join(STAGING_SUBDIR).join(format!("{stamp}-{pid}-{n}"));
        let mut builder = std::fs::DirBuilder::new();
        #[cfg(unix)]
        builder.mode(DIR_MODE);
        match builder.create(&candidate) {
            Ok(()) => return Ok(candidate),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => {
                return Err(e)
                    .with_context(|| format!("staging eval run dir at {}", candidate.display()));
            }
        }
    }
    anyhow::bail!(
        "could not claim a staging dir under {} after {MAX_DUMP_COLLISIONS} attempts",
        root.join(STAGING_SUBDIR).display()
    )
}

/// Open the stable publication lock file with owner-only permissions.
fn open_publish_lock(root: &Path) -> Result<std::fs::File> {
    let path = root.join(PUBLISH_LOCK);
    let mut options = std::fs::OpenOptions::new();
    options.read(true).write(true).create(true);
    #[cfg(unix)]
    options.mode(FILE_MODE);
    let file = options
        .open(&path)
        .with_context(|| format!("opening eval publication lock {}", path.display()))?;
    #[cfg(unix)]
    file.set_permissions(std::fs::Permissions::from_mode(FILE_MODE))
        .with_context(|| format!("securing eval publication lock {}", path.display()))?;
    Ok(file)
}

/// Resolve `last-run` while the caller holds the publication lock.
fn resolve_last_run_unlocked(root: &Path) -> Result<PathBuf> {
    let pointer = root.join(LAST_RUN_POINTER);
    let run_id = std::fs::read_to_string(&pointer)
        .with_context(|| format!("reading latest eval run pointer {}", pointer.display()))?;
    let run_id = run_id.trim();
    let mut components = Path::new(run_id).components();
    let valid = matches!(components.next(), Some(std::path::Component::Normal(_)))
        && components.next().is_none();
    if !valid || run_id.is_empty() {
        anyhow::bail!(
            "invalid latest eval run pointer {}: {run_id:?}",
            pointer.display()
        );
    }
    let run = root.join(RUNS_SUBDIR).join(run_id);
    if !run.is_dir() {
        anyhow::bail!(
            "latest eval run pointer {} names missing directory {}",
            pointer.display(),
            run.display()
        );
    }
    Ok(run)
}

/// Resolve the owner-only `last-run` pointer to an immutable completed run.
///
/// The pointer stores one directory name, not a path. Rejecting every other
/// shape prevents a corrupted pointer from escaping `<artifact_root>/runs`.
pub fn resolve_last_run(root: &Path) -> Result<PathBuf> {
    let lock = open_publish_lock(root)?;
    lock.lock_shared()
        .with_context(|| format!("locking eval artifact root {} for reading", root.display()))?;
    let resolved = resolve_last_run_unlocked(root);
    lock.unlock()
        .with_context(|| format!("unlocking eval artifact root {}", root.display()))?;
    resolved
}

/// Write and atomically replace the completed-run pointer.
fn replace_last_run_pointer(root: &Path, run_id: &str) -> Result<()> {
    let mut pointer = tempfile::Builder::new()
        .prefix(".last-run-")
        .suffix(".partial")
        .tempfile_in(root)
        .with_context(|| format!("staging latest eval run pointer under {}", root.display()))?;
    #[cfg(unix)]
    pointer
        .as_file()
        .set_permissions(std::fs::Permissions::from_mode(FILE_MODE))
        .with_context(|| format!("securing latest eval run pointer under {}", root.display()))?;
    writeln!(pointer, "{run_id}")
        .with_context(|| format!("writing latest eval run pointer under {}", root.display()))?;
    pointer
        .as_file()
        .sync_all()
        .with_context(|| format!("syncing latest eval run pointer under {}", root.display()))?;
    let target = root.join(LAST_RUN_POINTER);
    pointer
        .persist(&target)
        .map(|_| ())
        .map_err(|error| error.error)
        .with_context(|| format!("replacing latest eval run pointer {}", target.display()))
}

/// Remove every completed run except `keep` while holding the publication lock.
fn remove_retired_runs(root: &Path, keep: &Path) -> Result<()> {
    let runs = root.join(RUNS_SUBDIR);
    for entry in std::fs::read_dir(&runs)
        .with_context(|| format!("listing completed eval runs under {}", runs.display()))?
    {
        let entry = entry
            .with_context(|| format!("reading completed eval run under {}", runs.display()))?;
        let path = entry.path();
        if path == keep {
            continue;
        }
        if !entry
            .file_type()
            .with_context(|| format!("reading eval artifact type {}", path.display()))?
            .is_dir()
        {
            anyhow::bail!(
                "unexpected entry in completed eval runs: {}",
                path.display()
            );
        }
        match std::fs::remove_dir_all(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("removing retired eval run {}", path.display()));
            }
        }
    }
    Ok(())
}

/// Publish a staged run by atomically replacing the `last-run` pointer.
///
/// Active runs live outside the completed-run directory. An OS file lock then
/// serializes the staging rename, pointer replacement, and retired-run cleanup.
/// Concurrent publishers can therefore select only one complete run and can
/// never erase another process's in-progress artifacts.
pub fn publish_run(root: &Path, staged: &Path) -> Result<PathBuf> {
    let expected_parent = root.join(STAGING_SUBDIR);
    if staged.parent() != Some(expected_parent.as_path()) || !staged.is_dir() {
        anyhow::bail!(
            "refusing to publish eval run outside {}: {}",
            expected_parent.display(),
            staged.display()
        );
    }
    let run_id = staged
        .file_name()
        .and_then(std::ffi::OsStr::to_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            anyhow::Error::msg(format!(
                "eval run directory has no UTF-8 name: {}",
                staged.display()
            ))
        })?;

    create_private_dir(&root.join(RUNS_SUBDIR))?;
    let lock = open_publish_lock(root)?;
    lock.lock().with_context(|| {
        format!(
            "locking eval artifact root {} for publication",
            root.display()
        )
    })?;

    let completed = root.join(RUNS_SUBDIR).join(run_id);
    let publication = (|| {
        std::fs::rename(staged, &completed).with_context(|| {
            format!(
                "completing eval run {} as {}",
                staged.display(),
                completed.display()
            )
        })?;
        if let Err(error) = replace_last_run_pointer(root, run_id) {
            let cleanup = std::fs::remove_dir_all(&completed);
            return match cleanup {
                Ok(()) => Err(error),
                Err(cleanup_error) => Err(error.context(format!(
                    "also failed to remove unpublished run {}: {cleanup_error}",
                    completed.display()
                ))),
            };
        }
        remove_retired_runs(root, &completed)?;
        Ok(completed)
    })();

    let unlock = lock
        .unlock()
        .with_context(|| format!("unlocking eval artifact root {}", root.display()));
    match (publication, unlock) {
        (Ok(path), Ok(())) => Ok(path),
        (Err(error), Ok(())) | (Ok(_), Err(error)) => Err(error),
        (Err(error), Err(unlock_error)) => Err(error.context(unlock_error.to_string())),
    }
}

/// Post-run options gathered from the `eval run` flags.
pub struct FinalizeOpts {
    pub format: OutputFormat,
    pub dump_records: Option<PathBuf>,
    pub baseline: Option<PathBuf>,
    pub write_baseline: Option<PathBuf>,
    pub suite_kind: Option<SuiteKind>,
    pub history_dir: Option<PathBuf>,
}

/// Handle the post-run flow (dumps, baselines, comparison, printing) and return
/// the process exit code. Kept together so `main` only wires flags.
///
/// Every accepted format is emitted exactly once, including baseline writes.
pub async fn finalize(
    config: &Config,
    mode: Mode,
    suite_path: &Path,
    provider_ref: &str,
    report: SuiteReport,
    artifacts: RunArtifacts,
    opts: FinalizeOpts,
) -> Result<i32> {
    let kind = SuiteKind::resolve(suite_path, opts.suite_kind);
    let history_suite_dir = history_suite_dir(suite_path);
    // Table mode prints incrementally; JSON is one complete document, so it is
    // deferred until after the baseline comparison (when any) so the artifact
    // can carry the gate outcome.
    if opts.format == OutputFormat::Table {
        println!("{}", report.render_table());
    }

    let dump_result = write_dumps(&report, opts.dump_records.as_deref(), &artifacts.staged);
    let wrote_auto = match dump_result {
        Ok(wrote_auto) => wrote_auto,
        Err(error) => {
            if let Err(cleanup_error) = artifacts.discard() {
                return Err(error.context(format!(
                    "also failed to discard unpublished eval artifacts: {cleanup_error}"
                )));
            }
            return Err(error);
        }
    };
    let published = artifacts.publish()?;
    if wrote_auto && opts.format == OutputFormat::Table {
        let dir = published.display().to_string();
        println!(
            "{}",
            get_required_cli_string_with_args(
                "cli-eval-failed-case-records",
                &[("dir", dir.as_str())],
            )
        );
    }

    // --write-baseline: persist the run and exit with its normal code.
    if let Some(path) = &opts.write_baseline {
        // Fail closed before touching the baseline target: an incomplete run must
        // neither create a baseline nor replace an existing good one. The run's own
        // exit code is carried in the error context so it is reported alongside the
        // write failure rather than replaced by it.
        let run_code = report.exit_code(kind, None);
        let baseline = Baseline::from_report(&report)
            .with_context(|| format!("--write-baseline aborted (run exit code {run_code})"))?;
        write_baseline_atomically(path, &baseline.to_json()?)?;
        match opts.format {
            OutputFormat::Json => println!("{}", report.to_json(kind, None)),
            OutputFormat::Junit => {
                print!("{}", zeroclaw_eval::junit::render_junit(&report, &[], &[]));
            }
            OutputFormat::Table => {}
        }
        let _ = write_run_history(
            config,
            opts.history_dir.as_deref(),
            &report,
            HistoryRun {
                recorded_at: chrono::Utc::now(),
                suite_dir: history_suite_dir.clone(),
                suite_kind: kind,
                mode,
                provider_ref: provider_ref.to_string(),
            },
            None,
        );
        return Ok(run_code);
    }

    // --baseline: compare, apply the live flakiness rule, and report.
    let comparison = match &opts.baseline {
        Some(path) => {
            let baseline = Baseline::from_json(&std::fs::read_to_string(path)?)?;
            let mut cmp = baseline::compare(&report, &baseline)?;
            if mode == Mode::Live {
                let rerun_passed =
                    Box::pin(rerun_live_regressions(config, suite_path, &cmp)).await?;
                let flaky = baseline::downgrade_flaky_regressions(&mut cmp, mode, &rerun_passed);
                if opts.format == OutputFormat::Table {
                    for id in &flaky {
                        println!(
                            "{}",
                            get_required_cli_string_with_args(
                                "cli-eval-flaky-unconfirmed-regression",
                                &[("id", id)],
                            )
                        );
                    }
                }
            }
            if opts.format == OutputFormat::Table {
                print_comparison(&cmp, kind, &report, &baseline);
            }
            Some(cmp)
        }
        None => {
            if kind == SuiteKind::Capability && opts.format == OutputFormat::Table {
                print_capability_summary(&report, None);
            }
            None
        }
    };

    let _ = write_run_history(
        config,
        opts.history_dir.as_deref(),
        &report,
        HistoryRun {
            recorded_at: chrono::Utc::now(),
            suite_dir: history_suite_dir,
            suite_kind: kind,
            mode,
            provider_ref: provider_ref.to_string(),
        },
        comparison.as_ref(),
    );

    match opts.format {
        OutputFormat::Json => println!("{}", report.to_json(kind, comparison.as_ref())),
        OutputFormat::Junit => {
            // Cases unverifiable against the baseline render as <skipped/>.
            let skipped: Vec<&str> = comparison
                .as_ref()
                .map(|cmp| {
                    cmp.per_case
                        .iter()
                        .filter(|(_, c)| matches!(c, CaseComparison::Unverifiable))
                        .map(|(id, _)| id.as_str())
                        .collect()
                })
                .unwrap_or_default();
            // Flaky-unconfirmed live cases are "reported, never gated" and exit
            // 0, so they must not render as <failure>. Read them off the
            // comparison rather than the local list, so the XML classification
            // and the exit code are driven by the same source of truth.
            let flaky: Vec<&str> = comparison
                .as_ref()
                .map(|cmp| {
                    cmp.per_case
                        .iter()
                        .filter(|(_, c)| matches!(c, CaseComparison::FlakyUnconfirmed))
                        .map(|(id, _)| id.as_str())
                        .collect()
                })
                .unwrap_or_default();
            print!(
                "{}",
                zeroclaw_eval::junit::render_junit(&report, &skipped, &flaky)
            );
        }
        OutputFormat::Table => {}
    }

    Ok(report.exit_code(kind, comparison.as_ref()))
}

/// Retain a useful logical suite reference without embedding an absolute host
/// workspace path. Relative invocations are preserved unless they traverse a
/// parent; absolute paths under the current directory become relative, and
/// other absolute paths fall back to their final component.
fn history_suite_dir(suite_path: &Path) -> String {
    let safe_path = if suite_path.is_absolute() {
        std::env::current_dir()
            .ok()
            .and_then(|cwd| suite_path.strip_prefix(cwd).ok())
            .filter(|path| is_safe_logical_suite_path(path))
    } else if is_safe_logical_suite_path(suite_path) {
        Some(suite_path)
    } else {
        None
    };

    safe_path
        .map(|path| path.components().collect::<PathBuf>())
        .or_else(|| {
            std::fs::canonicalize(suite_path)
                .ok()
                .and_then(|path| path.file_name().map(PathBuf::from))
        })
        .or_else(|| suite_path.file_name().map(PathBuf::from))
        .map_or_else(String::new, |path| path.display().to_string())
}

fn is_safe_logical_suite_path(path: &Path) -> bool {
    let mut has_name = false;
    for component in path.components() {
        match component {
            std::path::Component::Normal(_) => has_name = true,
            std::path::Component::CurDir => {}
            std::path::Component::Prefix(_)
            | std::path::Component::RootDir
            | std::path::Component::ParentDir => return false,
        }
    }
    has_name
}

fn resolved_history_dir(override_dir: Option<&Path>, configured_dir: &str) -> Option<PathBuf> {
    match override_dir {
        Some(path) if path.as_os_str().is_empty() => None,
        Some(path) => Some(path.to_path_buf()),
        None if configured_dir.trim().is_empty() => None,
        None => Some(PathBuf::from(configured_dir)),
    }
}

fn path_is_under_target(path: &Path) -> bool {
    path.components().any(|component| {
        matches!(
            component,
            std::path::Component::Normal(name) if name == std::ffi::OsStr::new("target")
        )
    })
}

/// Persist one best-effort history receipt after baseline classification. A
/// history failure is observational only and never changes the eval exit code.
fn write_run_history(
    config: &Config,
    override_dir: Option<&Path>,
    report: &SuiteReport,
    run: HistoryRun,
    comparison: Option<&baseline::BaselineComparison>,
) -> Option<PathBuf> {
    write_run_history_with(
        config,
        override_dir,
        report,
        run,
        comparison,
        write_history_receipt,
    )
}

fn write_run_history_with(
    config: &Config,
    override_dir: Option<&Path>,
    report: &SuiteReport,
    run: HistoryRun,
    comparison: Option<&baseline::BaselineComparison>,
    writer: impl FnOnce(&Path, &HistoryReceipt) -> Result<PathBuf>,
) -> Option<PathBuf> {
    let dir = resolved_history_dir(override_dir, &config.eval.history_dir)?;
    if path_is_under_target(&dir) {
        eprintln!(
            "{}",
            get_required_cli_string("cli-eval-history-target-warning")
        );
    }

    match HistoryReceipt::from_report(report, run, comparison)
        .and_then(|receipt| writer(&dir, &receipt))
    {
        Ok(path) => Some(path),
        Err(_) => {
            // History is best effort and its destination can contain host identity.
            // Keep retained CI stderr useful without echoing the configured path or
            // the path-rich anyhow chain from the writer.
            eprintln!(
                "{}",
                get_required_cli_string("cli-eval-history-write-warning")
            );
            None
        }
    }
}

/// Re-run each regressed case against the same config, returning whether the
/// re-run confirmed a pass, keyed by case id. Used only for live suites.
///
/// The re-run honors the case's own effective repeat policy via
/// [`zeroclaw_eval::run_case_repeated`], so a `repeat = k` case must clear
/// `pass^k` again to be excused as flaky. Re-running once would let a single
/// lucky attempt downgrade a real regression on a case whose documented
/// contract is "passes iff all k runs pass".
async fn rerun_live_regressions(
    config: &Config,
    suite_path: &Path,
    comparison: &baseline::BaselineComparison,
) -> Result<BTreeMap<String, bool>> {
    let deps = build_run_deps(config, Mode::Live)?;
    rerun_live_regressions_with_deps(suite_path, comparison, &deps).await
}

/// Dependency-injected core of [`rerun_live_regressions`]. Keeping the retry
/// decision here lets the regression exercise the same suite loading, repeat
/// runner, and baseline verdict used by production without calling a provider.
async fn rerun_live_regressions_with_deps(
    suite_path: &Path,
    comparison: &baseline::BaselineComparison,
    deps: &RunDeps,
) -> Result<BTreeMap<String, bool>> {
    let regressed: Vec<&str> = comparison
        .per_case
        .iter()
        .filter(|(_, c)| matches!(c, CaseComparison::Regression { .. }))
        .map(|(id, _)| id.as_str())
        .collect();
    let mut out = BTreeMap::new();
    if regressed.is_empty() {
        return Ok(out);
    }
    let traces = zeroclaw_eval::case::load_suite(suite_path)?;
    for (_, trace) in &traces {
        let id = trace.display_id();
        if regressed.contains(&id) {
            let passed = matches!(
                Box::pin(zeroclaw_eval::run_case_repeated(trace, deps)).await,
                Ok((outcome, repeat))
                    if outcome.grades.iter().all(|g| g.passed)
                        && repeat.as_ref().is_none_or(|stats| stats.establishes_pass_hat_k())
            );
            out.insert(id.to_string(), passed);
        }
    }
    Ok(out)
}

/// Print a compact per-case comparison summary.
fn print_comparison(
    comparison: &baseline::BaselineComparison,
    kind: SuiteKind,
    report: &SuiteReport,
    baseline: &Baseline,
) {
    println!();
    println!(
        "{}",
        get_required_cli_string("cli-eval-baseline-comparison")
    );
    for (id, c) in &comparison.per_case {
        let line = match c {
            CaseComparison::New => get_required_cli_string("cli-eval-comparison-new"),
            CaseComparison::Removed => get_required_cli_string("cli-eval-comparison-removed"),
            CaseComparison::Unverifiable => {
                get_required_cli_string("cli-eval-comparison-unverifiable")
            }
            CaseComparison::CurrentError => {
                get_required_cli_string("cli-eval-comparison-current-error")
            }
            CaseComparison::Improvement => {
                get_required_cli_string("cli-eval-comparison-improvement")
            }
            CaseComparison::FlakyUnconfirmed => {
                get_required_cli_string("cli-eval-comparison-flaky-unconfirmed")
            }
            CaseComparison::Regression { categories } => {
                let cats: Vec<&str> = categories.iter().map(|c| c.as_str()).collect();
                let categories = cats.join(", ");
                get_required_cli_string_with_args(
                    "cli-eval-comparison-regression",
                    &[("categories", categories.as_str())],
                )
            }
            CaseComparison::Unchanged { token_delta_pct } => match token_delta_pct {
                Some(pct) => {
                    let pct = format!("{pct:+.0}");
                    get_required_cli_string_with_args(
                        "cli-eval-comparison-unchanged-tokens",
                        &[("pct", pct.as_str())],
                    )
                }
                None => get_required_cli_string("cli-eval-comparison-unchanged"),
            },
        };
        println!("    {id}: {line}");
    }
    if kind == SuiteKind::Capability {
        print_capability_summary(report, Some(baseline));
    }
}

/// Print the localized capability summary (pass rate, trend, saturation).
fn print_capability_summary(report: &SuiteReport, baseline: Option<&Baseline>) {
    let stats = report.capability_stats(baseline);
    let rate = format!("{:.0}", stats.pass_rate);
    let line = match stats.baseline_rate {
        Some(brate) => {
            let brate = format!("{brate:.0}");
            get_required_cli_string_with_args(
                "cli-eval-capability-pass-rate-was",
                &[("rate", rate.as_str()), ("baseline_rate", brate.as_str())],
            )
        }
        None => {
            get_required_cli_string_with_args("cli-eval-capability-pass-rate", &[("rate", &rate)])
        }
    };
    println!("  {line}");
    if stats.saturated {
        println!(
            "{}",
            get_required_cli_string("cli-eval-capability-saturation-warning")
        );
    }
}

/// Build the per-run dependencies for the requested mode, threading the loaded
/// config so live mode can resolve its provider. Replay injects the deterministic
/// trace-replay provider; live resolves `[eval].live_provider` per case.
fn build_run_deps(config: &Config, mode: Mode) -> Result<RunDeps> {
    let judge = build_judge_deps(config)?;
    let mut deps = match mode {
        // Replay's provider wiring is owned by `RunDeps::replay()`; delegate so the
        // trace-replay factory has a single definition. Replay ignores the live-only
        // tool allowlist and timeout.
        Mode::Replay => RunDeps::replay(),
        Mode::Live => {
            // Trim so validation (which trims) and runtime resolution agree: a
            // whitespace-padded ref must not pass `Config::validate` then miss here.
            let provider_ref = config.eval.live_provider.as_str().trim().to_string();
            zeroclaw_eval::ensure_live_provider(&provider_ref)?;
            // Resolve the model once for the receipt label; the closure builds a
            // fresh provider per case (isolation) and must be `'static`, so it owns
            // a config clone.
            let (_, _provider_type, resolved_model) =
                build_session_model_provider(config, &provider_ref, None)?;
            let receipt_ref = format!("{provider_ref}:{resolved_model}");
            if judge
                .as_ref()
                .is_some_and(|item| item.judge_ref.split(':').next() == Some(provider_ref.as_str()))
            {
                println!("{}", get_required_cli_string("cli-eval-self-judge-warning"));
            }
            let cfg = config.clone();
            RunDeps {
                mode,
                provider: Box::new(move |_trace: &LlmTrace| {
                    let (provider, provider_type, resolved_model) =
                        build_session_model_provider(&cfg, &provider_ref, None)?;
                    Ok(CaseProvider {
                        provider,
                        provider_name: Some(provider_type),
                        model_name: Some(resolved_model),
                        finish_turn: None,
                    })
                }),
                provider_ref: receipt_ref,
                live_tools: config.eval.live_allowed_tools.clone(),
                case_timeout: Duration::from_secs(config.eval.case_timeout_secs),
                judge: None,
            }
        }
    };
    deps.judge = judge;
    Ok(deps)
}

fn fixed_identity_judge_config(config: &Config, provider_ref: &str) -> Result<Config> {
    let (family, alias) = provider_ref.split_once('.').ok_or_else(|| {
        anyhow::Error::msg(format!(
            "model_provider reference `{provider_ref}` must be `<type>.<alias>`"
        ))
    })?;
    let mut judge_config = config.clone();
    judge_config.model_routes.clear();
    let profile = judge_config
        .providers
        .models
        .iter_entries_mut()
        .find(|(entry_family, entry_alias, _)| *entry_family == family && *entry_alias == alias)
        .map(|(_, _, profile)| profile)
        .ok_or_else(|| {
            anyhow::Error::msg(format!("unknown model_provider reference `{provider_ref}`"))
        })?;
    profile.fallback.clear();
    profile.fallback_models.clear();
    Ok(judge_config)
}

/// Resolve optional diagnostic judge dependencies. Fallback models, aliases,
/// and routes are disabled on a per-run config clone so `judge_ref` names the
/// model actually queried rather than only the first candidate in a resilient
/// chain.
fn build_judge_deps(config: &Config) -> Result<Option<zeroclaw_eval::grader::JudgeDeps>> {
    let provider_ref = config.eval.judge_provider.as_str().trim().to_string();
    if provider_ref.is_empty() {
        return Ok(None);
    }

    let judge_config = fixed_identity_judge_config(config, &provider_ref)?;
    let (provider, _provider_type, model) =
        build_session_model_provider(&judge_config, &provider_ref, None)?;
    let judge_ref = format!("{provider_ref}:{model}");

    Ok(Some(zeroclaw_eval::grader::JudgeDeps {
        provider: std::sync::Arc::from(provider),
        model,
        judge_ref,
    }))
}

/// One run's artifact lifecycle: a private, uniquely named directory that is
/// selected by `last-run` only once the run completes.
pub struct RunArtifacts {
    /// `<install>/eval-artifacts`.
    pub root: PathBuf,
    /// This run's owned staging dir; nothing else writes here.
    pub staged: PathBuf,
}

/// One completed eval run and the canonical provider reference used to run it.
pub struct EvalRun {
    pub report: SuiteReport,
    pub provider_ref: String,
}

impl RunArtifacts {
    /// Publish the staged run through the atomic `last-run` pointer.
    pub fn publish(self) -> Result<PathBuf> {
        publish_run(&self.root, &self.staged)
    }

    /// Remove an unpublished staging directory, surfacing every error except an
    /// already-absent directory.
    pub fn discard(self) -> Result<()> {
        match std::fs::remove_dir_all(&self.staged) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error).with_context(|| {
                format!("discarding unpublished eval run {}", self.staged.display())
            }),
        }
    }
}

/// Prepare this run's artifact staging area.
///
/// Deliberately called only after the suite returns a report, so provider,
/// fixture-loading, and suite-level errors cannot leave empty staging dirs or
/// disturb the previous completed run.
pub fn prepare_artifacts(config: &Config) -> Result<RunArtifacts> {
    let root = artifact_root(config);
    create_private_dir(&root)?;
    let staged = stage_run_dir(&root)?;
    Ok(RunArtifacts { root, staged })
}

/// Validate the suite directory before anything destructive happens.
fn validate_suite_dir(suite: &Path) -> Result<()> {
    if !suite.is_dir() {
        anyhow::bail!("eval suite directory not found: {}", suite.display());
    }
    Ok(())
}

/// Run a suite of eval cases and return the aggregated report plus this run's
/// artifact staging area.
///
/// Ordering matters: provider and suite validation plus execution happen before
/// staging. A rejected invocation therefore leaves no new artifact state, and
/// two completed concurrent runs still receive unique staging directories.
pub async fn run(config: &Config, suite: PathBuf, mode: Mode) -> Result<(EvalRun, RunArtifacts)> {
    let deps = build_run_deps(config, mode)?;
    let provider_ref = deps.provider_ref.clone();
    validate_suite_dir(&suite)?;

    let report = Box::pin(zeroclaw_eval::run_suite(&suite, &deps)).await?;
    let artifacts = prepare_artifacts(config)?;
    Ok((
        EvalRun {
            report,
            provider_ref,
        },
        artifacts,
    ))
}

/// Sanitize a case id into a filesystem-safe stem.
fn dump_stem(case_id: &str) -> String {
    case_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Return the `n`th candidate name for a sanitized case id.
fn dump_candidate(dir: &Path, case_id: &str, n: u32) -> PathBuf {
    let stem = dump_stem(case_id);
    if n == 0 {
        dir.join(format!("{stem}.json"))
    } else {
        dir.join(format!("{stem}_{n}.json"))
    }
}

/// Write one case's dump into `dir`. Includes the record (provenance is always
/// present; completion data only for a run that finished) plus grades and the
/// error string, so an errored case still yields an inspectable artifact.
///
/// The payload is staged in a sibling temp file inside the same private dir,
/// flushed with `sync_all`, then renamed into place — so a concurrent reader
/// never observes a partial transcript.
fn write_case_dump(dir: &Path, case: &CaseReport) -> Result<PathBuf> {
    create_private_dir(dir)?;
    let dump = serde_json::json!({
        "case_id": case.name,
        "record": case.record,
        "grades": case.grades,
        "error": case.error,
        "repeat": case.repeat,
    });
    let json = serde_json::to_vec_pretty(&dump)?;

    // The final name does not exist until the complete, synced tempfile is
    // persisted with no-clobber semantics. This avoids the visible empty
    // placeholder produced by claiming the final name before writing.
    let mut staged = tempfile::Builder::new()
        .prefix(".eval-case-")
        .suffix(".json.partial")
        .tempfile_in(dir)
        .with_context(|| format!("staging eval transcript dump under {}", dir.display()))?;
    #[cfg(unix)]
    staged
        .as_file()
        .set_permissions(std::fs::Permissions::from_mode(FILE_MODE))
        .with_context(|| format!("securing eval transcript dump under {}", dir.display()))?;
    staged
        .write_all(&json)
        .with_context(|| format!("writing eval transcript dump under {}", dir.display()))?;
    staged
        .as_file()
        .sync_all()
        .with_context(|| format!("syncing eval transcript dump under {}", dir.display()))?;

    for n in 0..MAX_DUMP_COLLISIONS {
        let final_path = dump_candidate(dir, &case.name, n);
        match staged.persist_noclobber(&final_path) {
            Ok(_) => return Ok(final_path),
            Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {
                staged = error.file;
            }
            Err(error) => {
                return Err(error.error).with_context(|| {
                    format!("publishing eval transcript dump {}", final_path.display())
                });
            }
        }
    }
    anyhow::bail!(
        "could not create a transcript dump for case {:?} in {} after {MAX_DUMP_COLLISIONS} attempts",
        case.name,
        dir.display()
    )
}

/// Write case dumps: `explicit_dir` (from `--dump-records`) receives every case;
/// `auto_dir` receives only failed/errored cases. Returns `true` if any auto-dump
/// was written, so the caller can print the footer.
pub fn write_dumps(
    report: &SuiteReport,
    explicit_dir: Option<&Path>,
    auto_dir: &Path,
) -> Result<bool> {
    if let Some(dir) = explicit_dir {
        for case in &report.cases {
            write_case_dump(dir, case)?;
        }
    }
    let mut any_auto = false;
    for case in &report.cases {
        if !case.passed() {
            write_case_dump(auto_dir, case)?;
            any_auto = true;
        }
    }
    Ok(any_auto)
}

/// Write `contents` to `path` via a unique sibling temp file plus atomic persist,
/// so the target is either the old file or the complete new one, never a truncated
/// intermediate. A failed write leaves any existing baseline untouched.
fn write_baseline_atomically(path: &Path, contents: &str) -> Result<()> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)?;

    let mut tmp = tempfile::Builder::new()
        .prefix(".zeroclaw-baseline-")
        .suffix(".tmp")
        .tempfile_in(parent)?;
    tmp.write_all(contents.as_bytes())?;
    tmp.as_file().sync_all()?;
    tmp.persist(path).map_err(|error| error.error)?;
    Ok(())
}

/// Output format for the eval report.
#[derive(Copy, Clone, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum OutputFormat {
    /// Human-readable table.
    Table,
    /// Machine-readable JSON, for CI artifacts.
    Json,
    /// JUnit XML, for CI test reporters.
    Junit,
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use zeroclaw_api::attribution::{Attributable, ModelProviderKind, ProviderKind, Role};
    use zeroclaw_api::model_provider::{
        ChatRequest, ChatResponse, ModelProvider, ProviderCapabilities,
    };
    use zeroclaw_eval::record::{CaseProvenance, RunCompletion, SandboxStamp, ToolSurface};
    use zeroclaw_eval::{GradeCategory, GradeResult, RunRecord};

    struct StaticTextProvider;

    impl Attributable for StaticTextProvider {
        fn role(&self) -> Role {
            Role::Provider(ProviderKind::Model(ModelProviderKind::Custom))
        }

        fn alias(&self) -> &str {
            "eval-retry-test"
        }
    }

    #[async_trait]
    impl ModelProvider for StaticTextProvider {
        fn capabilities(&self) -> ProviderCapabilities {
            ProviderCapabilities::default()
        }

        async fn chat_with_system(
            &self,
            _system: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<String> {
            Ok("ok".to_string())
        }

        async fn chat(
            &self,
            _request: ChatRequest<'_>,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<ChatResponse> {
            Ok(ChatResponse {
                text: Some("ok".to_string()),
                tool_calls: Vec::new(),
                usage: None,
                reasoning_content: None,
            })
        }
    }

    fn provenance(case_id: &str) -> CaseProvenance {
        CaseProvenance {
            schema: zeroclaw_eval::record::RECORD_SCHEMA.to_string(),
            mode: Mode::Replay,
            case_id: case_id.to_string(),
            case_hash: "deadbeef".to_string(),
            provider_ref: "scripted".to_string(),
            tool_surface: ToolSurface::default(),
            sandbox: SandboxStamp {
                autonomy: "supervised".to_string(),
                workspace_only: false,
            },
            judge_ref: None,
        }
    }

    fn record(case_id: &str) -> RunRecord {
        RunRecord {
            provenance: provenance(case_id),
            completion: Some(RunCompletion {
                final_response: "x".to_string(),
                ..RunCompletion::default()
            }),
        }
    }

    fn case_report(name: &str, passed: bool) -> CaseReport {
        CaseReport {
            name: name.to_string(),
            source: "f.json".to_string(),
            record: Some(record(name)),
            grades: if passed {
                vec![GradeResult::new(
                    "test-grade".to_string(),
                    true,
                    "passed",
                    GradeCategory::Response,
                )]
            } else {
                Vec::new()
            },
            error: if passed {
                None
            } else {
                Some("boom".to_string())
            },
            repeat: None,
            cluster: None,
        }
    }

    #[test]
    fn dump_records_writes_all_cases() {
        let report = SuiteReport {
            cases: vec![case_report("pass", true), case_report("fail", false)],
        };
        let explicit = tempfile::tempdir().unwrap();
        let auto = tempfile::tempdir().unwrap();
        write_dumps(&report, Some(explicit.path()), auto.path()).unwrap();
        assert!(explicit.path().join("pass.json").exists());
        assert!(explicit.path().join("fail.json").exists());
    }

    #[test]
    fn failed_case_autodumps_record() {
        let report = SuiteReport {
            cases: vec![case_report("fail", false)],
        };
        let auto = tempfile::tempdir().unwrap();
        let any = write_dumps(&report, None, auto.path()).unwrap();
        assert!(any, "a failed case must report an auto-dump");
        assert!(auto.path().join("fail.json").exists());
    }

    #[test]
    fn passing_case_does_not_autodump() {
        let report = SuiteReport {
            cases: vec![case_report("pass", true)],
        };
        let auto = tempfile::tempdir().unwrap();
        let any = write_dumps(&report, None, auto.path()).unwrap();
        assert!(!any, "a passing case must not auto-dump");
        assert!(!auto.path().join("pass.json").exists());
    }

    #[test]
    fn colliding_case_ids_do_not_overwrite() {
        // "a/b" and "a_b" both sanitize to "a_b"; both must still be written.
        let report = SuiteReport {
            cases: vec![case_report("a/b", false), case_report("a_b", false)],
        };
        let explicit = tempfile::tempdir().unwrap();
        let auto = tempfile::tempdir().unwrap();
        write_dumps(&report, Some(explicit.path()), auto.path()).unwrap();
        let count = std::fs::read_dir(explicit.path()).unwrap().count();
        assert_eq!(count, 2, "colliding ids must produce two files, not one");
    }

    /// An errored case with provenance but no completion data: the shape the
    /// runner now preserves and `--write-baseline` must refuse to serialize.
    fn errored_case(name: &str) -> CaseReport {
        let mut c = case_report(name, false);
        c.record = Some(RunRecord::from_provenance(provenance(name)));
        c
    }

    fn test_artifacts(root: &Path) -> RunArtifacts {
        let root = root.join("eval-artifacts");
        create_private_dir(&root).unwrap();
        let staged = stage_run_dir(&root).unwrap();
        RunArtifacts { root, staged }
    }

    fn write_baseline_opts(path: &Path) -> FinalizeOpts {
        FinalizeOpts {
            format: OutputFormat::Table,
            dump_records: None,
            baseline: None,
            write_baseline: Some(path.to_path_buf()),
            suite_kind: None,
            history_dir: None,
        }
    }

    #[test]
    fn atomic_baseline_write_replaces_existing_file_without_litter() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("baseline.json");
        std::fs::write(&target, "old").unwrap();

        write_baseline_atomically(&target, "complete-new-baseline").unwrap();

        assert_eq!(
            std::fs::read_to_string(&target).unwrap(),
            "complete-new-baseline"
        );
        let entries: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(entries, vec![std::ffi::OsString::from("baseline.json")]);
    }

    #[test]
    fn fixed_identity_judge_config_removes_every_alternate_dispatch_path() {
        let config: Config = toml::from_str(
            r#"
                [providers.models.custom.judge]
                model = "primary"
                fallback_models = ["backup-model"]
                fallback = ["custom.backup"]

                [providers.models.custom.backup]
                model = "other"

                [[model_routes]]
                hint = "primary"
                model_provider = "custom.backup"
                model = "routed"
            "#,
        )
        .unwrap();

        let isolated = fixed_identity_judge_config(&config, "custom.judge").unwrap();
        let profile = isolated.providers.models.find("custom", "judge").unwrap();
        assert!(profile.fallback.is_empty());
        assert!(profile.fallback_models.is_empty());
        assert!(isolated.model_routes.is_empty());

        let original = config.providers.models.find("custom", "judge").unwrap();
        assert_eq!(original.fallback_models, ["backup-model"]);
        assert_eq!(original.fallback[0].as_str(), "custom.backup");
        assert_eq!(config.model_routes.len(), 1);
    }

    #[tokio::test]
    async fn write_baseline_does_not_create_file_on_run_error() {
        // A case errored after producing provenance but before completion. The
        // baseline would be untrustworthy, so the write must be refused outright.
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("nested").join("baseline.json");
        let artifact_dir = tempfile::tempdir().unwrap();
        let report = SuiteReport {
            cases: vec![case_report("ok", true), errored_case("boom-case")],
        };

        let err = finalize(
            &Config::default(),
            Mode::Replay,
            Path::new("evals/regression"),
            "scripted",
            report,
            test_artifacts(artifact_dir.path()),
            write_baseline_opts(&target),
        )
        .await
        .expect_err("an errored report must fail --write-baseline");
        assert!(
            format!("{err:#}").contains("boom-case"),
            "the failure must name the errored case: {err:#}"
        );
        assert!(
            !target.exists(),
            "no partial baseline may be left behind at {}",
            target.display()
        );
    }

    #[tokio::test]
    async fn write_baseline_preserves_existing_file_on_run_error() {
        // An existing good baseline must survive a failed run byte-for-byte: the
        // refusal happens before the target is created, truncated, or replaced.
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("baseline.json");
        let artifact_dir = tempfile::tempdir().unwrap();
        let good = SuiteReport {
            cases: vec![case_report("ok", true)],
        };
        finalize(
            &Config::default(),
            Mode::Replay,
            Path::new("evals/regression"),
            "scripted",
            good,
            test_artifacts(artifact_dir.path()),
            write_baseline_opts(&target),
        )
        .await
        .expect("a complete report writes a baseline");
        let before = std::fs::read(&target).unwrap();
        assert!(!before.is_empty());

        let broken = SuiteReport {
            cases: vec![case_report("ok", true), errored_case("boom-case")],
        };
        let err = finalize(
            &Config::default(),
            Mode::Replay,
            Path::new("evals/regression"),
            "scripted",
            broken,
            test_artifacts(artifact_dir.path()),
            write_baseline_opts(&target),
        )
        .await
        .expect_err("an errored report must fail --write-baseline");
        assert!(format!("{err:#}").contains("boom-case"));

        let after = std::fs::read(&target).unwrap();
        assert_eq!(
            before, after,
            "the existing baseline must be byte-identical after a failed run"
        );
        // The atomic write must not leave its scratch file behind either.
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n != "baseline.json")
            .collect();
        assert!(
            leftovers.is_empty(),
            "stray files left behind: {leftovers:?}"
        );
    }

    #[test]
    fn errored_case_auto_dump_retains_provenance() {
        // The auto-dump exists for failed cases. Writing `"record": null` there
        // strips the case hash, mode, provider, tool surface, and sandbox stamp
        // from exactly the artifact a diagnosing operator opens.
        let mut errored = case_report("timeout-case", false);
        errored.record = Some(RunRecord::from_provenance(provenance("timeout-case")));
        let report = SuiteReport {
            cases: vec![errored],
        };
        let auto = tempfile::tempdir().unwrap();
        write_dumps(&report, None, auto.path()).unwrap();

        let content = std::fs::read_to_string(auto.path().join("timeout-case.json")).unwrap();
        let v: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert!(
            !v["record"].is_null(),
            "an errored case's dump must not carry \"record\": null: {content}"
        );
        assert_eq!(v["record"]["case_hash"], "deadbeef");
        assert_eq!(v["record"]["mode"], "replay");
        assert_eq!(v["record"]["provider_ref"], "scripted");
        assert!(v["record"]["tool_surface"].is_object());
        assert!(v["record"]["sandbox"].is_object());
    }

    /// The `--baseline` retry must honor `pass^k`. A `repeat = 5` case that
    /// regressed is only excused when the retry itself clears pass^k; a retry
    /// that passes 4 of 5 must stay a gating regression.
    ///
    /// `rerun_live_regressions` needs a live provider, so this pins the pure
    /// boundary it feeds: the bool it inserts per case drives
    /// `downgrade_flaky_regressions`, and therefore the exit code. The
    /// companion test `repeat_retry_uses_pass_hat_k_not_one_lucky_run` in
    /// `zeroclaw-eval` proves `run_case_repeated` produces `false` for 4/5.
    #[test]
    fn baseline_retry_excuses_only_a_pass_hat_k_rerun() {
        use zeroclaw_eval::baseline::{BaselineComparison, CaseComparison, SuiteKind};

        let regression = || CaseComparison::Regression {
            categories: Vec::new(),
        };
        let report = SuiteReport {
            cases: vec![case_report("flaky", false)],
        };

        let mut cmp = BaselineComparison {
            per_case: [("flaky".to_string(), regression())].into_iter().collect(),
        };
        let rerun: BTreeMap<String, bool> = [("flaky".to_string(), false)].into_iter().collect();
        let flaky =
            zeroclaw_eval::baseline::downgrade_flaky_regressions(&mut cmp, Mode::Live, &rerun);
        assert!(
            flaky.is_empty(),
            "a retry short of pass^k must not be excused as flaky"
        );
        assert_eq!(
            report.exit_code(SuiteKind::Regression, Some(&cmp)),
            1,
            "an unexcused regression must gate"
        );

        let mut cmp_ok = BaselineComparison {
            per_case: [("flaky".to_string(), regression())].into_iter().collect(),
        };
        let rerun_ok: BTreeMap<String, bool> = [("flaky".to_string(), true)].into_iter().collect();
        let flaky_ok = zeroclaw_eval::baseline::downgrade_flaky_regressions(
            &mut cmp_ok,
            Mode::Live,
            &rerun_ok,
        );
        assert_eq!(flaky_ok, vec!["flaky".to_string()]);
    }

    /// Drive the production baseline retry composition: two passing attempts
    /// followed by a provider-construction error must remain a regression, even
    /// though the representative completed run passed every grade.
    #[tokio::test]
    async fn truncated_baseline_retry_remains_gating() {
        use zeroclaw_eval::baseline::{BaselineComparison, CaseComparison, SuiteKind};

        let suite = tempfile::tempdir().unwrap();
        std::fs::write(
            suite.path().join("partial.json"),
            r#"{
                "id": "partial-retry",
                "model_name": "partial-retry",
                "repeat": 3,
                "turns": [{ "user_input": "run" }],
                "expects": { "response_contains": ["ok"] }
            }"#,
        )
        .unwrap();

        let builds = Arc::new(AtomicUsize::new(0));
        let builds_for_factory = Arc::clone(&builds);
        let deps = RunDeps {
            mode: Mode::Live,
            provider: Box::new(move |_trace| {
                let attempt = builds_for_factory.fetch_add(1, Ordering::SeqCst) + 1;
                if attempt == 3 {
                    anyhow::bail!("provider failed on attempt 3");
                }
                Ok(zeroclaw_eval::CaseProvider::from_provider(Box::new(
                    StaticTextProvider,
                )))
            }),
            provider_ref: "test.retry:model".to_string(),
            live_tools: Vec::new(),
            case_timeout: Duration::from_secs(5),
            judge: None,
        };

        let mut cmp = BaselineComparison {
            per_case: [(
                "partial-retry".to_string(),
                CaseComparison::Regression {
                    categories: Vec::new(),
                },
            )]
            .into_iter()
            .collect(),
        };
        let rerun = rerun_live_regressions_with_deps(suite.path(), &cmp, &deps)
            .await
            .unwrap();
        assert_eq!(builds.load(Ordering::SeqCst), 3);
        assert_eq!(rerun.get("partial-retry"), Some(&false));

        let flaky =
            zeroclaw_eval::baseline::downgrade_flaky_regressions(&mut cmp, Mode::Live, &rerun);
        assert!(
            flaky.is_empty(),
            "a truncated retry must not be excused as flaky"
        );

        let report = SuiteReport {
            cases: vec![case_report("partial-retry", false)],
        };
        assert_eq!(
            report.exit_code(SuiteKind::Regression, Some(&cmp)),
            1,
            "the incomplete retry leaves the original regression gating"
        );
    }

    #[test]
    fn record_dump_contains_indexed_repeat_attempts() {
        let sample = zeroclaw_eval::stats::RunSample {
            passed: true,
            input_tokens: 3,
            output_tokens: 2,
            duration_ms: 17,
            llm_calls: 1,
            checks: vec![("response_contains".to_string(), true, false)],
        };
        let mut case = case_report("partial", false);
        case.repeat = Some(zeroclaw_eval::stats::RepeatStats::from_partial_runs(
            3,
            &[sample],
            "provider timeout".to_string(),
        ));

        let dir = tempfile::tempdir().unwrap();
        write_case_dump(dir.path(), &case).unwrap();
        let dump: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(dir.path().join("partial.json")).unwrap(),
        )
        .unwrap();
        let attempts = dump["repeat"]["attempts"].as_array().unwrap();
        assert_eq!(attempts.len(), 2);
        assert_eq!(attempts[0]["attempt"].as_u64(), Some(1));
        assert_eq!(attempts[0]["outcome"].as_str(), Some("passed"));
        assert_eq!(attempts[1]["attempt"].as_u64(), Some(2));
        assert_eq!(attempts[1]["outcome"].as_str(), Some("error"));
        assert_eq!(attempts[1]["error"].as_str(), Some("provider timeout"));
        assert!(
            attempts[0].get("history").is_none(),
            "minimal attempt receipts must not duplicate transcripts"
        );
    }

    #[test]
    fn errored_case_is_dumped_with_error() {
        let mut errored = case_report("err", false);
        // An errored case never completed, so it carries provenance only.
        errored.record = Some(RunRecord::from_provenance(provenance("err")));
        let report = SuiteReport {
            cases: vec![errored],
        };
        let auto = tempfile::tempdir().unwrap();
        let any = write_dumps(&report, None, auto.path()).unwrap();
        assert!(any, "an errored case must auto-dump");
        let content = std::fs::read_to_string(auto.path().join("err.json")).unwrap();
        assert!(
            content.contains("boom"),
            "the error string must be captured"
        );
    }

    /// A config whose install root (config_path's parent) is a temp dir.
    fn config_at(root: &Path) -> Config {
        let mut config = Config::default();
        config.config_path = root.join("config.toml");
        config
    }

    #[test]
    fn artifact_root_is_anchored_to_install_root_not_cwd() {
        // The automatic dump requires no opt-in and can contain workspace content
        // or credentials. A CWD-relative path lets a run from a nested directory
        // drop it somewhere `git add` would pick up, because the repo ignores only
        // the root-anchored `/target`.
        let tmp = tempfile::tempdir().unwrap();
        let config = config_at(tmp.path());
        let root = artifact_root(&config);
        assert!(
            root.starts_with(tmp.path()),
            "artifact root {} must be anchored under the install root {}",
            root.display(),
            tmp.path().display()
        );
        assert!(
            root.is_absolute(),
            "an absolute root cannot be reinterpreted by the process CWD: {}",
            root.display()
        );
        assert!(
            !root.to_string_lossy().contains("target"),
            "the root must not depend on the gitignored /target convention"
        );
    }

    #[test]
    fn dump_root_is_stable_from_nested_cwd() {
        // Resolve the root, then resolve it again as if the process had moved into
        // a nested subdirectory. A CWD-relative literal would differ; an anchored
        // root is identical.
        let tmp = tempfile::tempdir().unwrap();
        let config = config_at(tmp.path());
        let before = artifact_root(&config);

        let nested = tmp.path().join("a/b/c");
        std::fs::create_dir_all(&nested).unwrap();
        let after = artifact_root(&config);
        assert_eq!(
            before, after,
            "the artifact root must not move when the process runs from a nested dir"
        );
    }

    #[cfg(unix)]
    #[test]
    fn dump_file_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("dumps");
        let path = write_case_dump(&dir, &case_report("secretive", false)).unwrap();

        let file_mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            file_mode, 0o600,
            "transcript dumps carry conversation history and tool results; \
             they must not be group- or world-readable (got {file_mode:o})"
        );
        let dir_mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            dir_mode, 0o700,
            "the dump dir must be owner-only (got {dir_mode:o})"
        );
    }

    #[cfg(unix)]
    #[test]
    fn existing_dump_dir_is_tightened_to_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("existing-dumps");
        std::fs::create_dir(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();

        write_case_dump(&dir, &case_report("private", false)).unwrap();

        let mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700, "an existing dump dir must be tightened");
    }

    #[cfg(unix)]
    #[test]
    fn dump_create_new_refuses_existing_symlink() {
        // A dangling symlink planted at the candidate path must not be followed:
        // `O_CREAT|O_EXCL` fails on it, so the writer advances to the next suffix
        // and the symlink target is never created.
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("dumps");
        create_private_dir(&dir).unwrap();
        let outside = tmp.path().join("outside-target.json");
        std::os::unix::fs::symlink(&outside, dir.join("victim.json")).unwrap();

        let path = write_case_dump(&dir, &case_report("victim", false)).unwrap();
        assert!(
            !outside.exists(),
            "the write followed a dangling symlink out of the dump dir to {}",
            outside.display()
        );
        assert_ne!(
            path,
            dir.join("victim.json"),
            "the collision must advance the suffix rather than reuse the symlink path"
        );
        assert!(path.exists());
    }

    #[test]
    fn concurrent_dumps_do_not_clobber() {
        // Two writers racing on the same case id must produce two distinct,
        // complete files — the check-then-write sequence could hand both the same
        // path and let one truncate the other.
        let tmp = tempfile::tempdir().unwrap();
        let dir = std::sync::Arc::new(tmp.path().join("dumps"));
        create_private_dir(&dir).unwrap();

        let handles: Vec<_> = (0..8)
            .map(|i| {
                let dir = std::sync::Arc::clone(&dir);
                std::thread::spawn(move || {
                    write_case_dump(&dir, &case_report("same-id", false))
                        .unwrap_or_else(|e| panic!("writer {i} failed: {e}"))
                })
            })
            .collect();
        let paths: Vec<PathBuf> = handles.into_iter().map(|h| h.join().unwrap()).collect();

        let unique: std::collections::BTreeSet<_> = paths.iter().collect();
        assert_eq!(
            unique.len(),
            8,
            "each writer must own a distinct path: {paths:?}"
        );
        for p in &paths {
            let content = std::fs::read_to_string(p).unwrap();
            let v: serde_json::Value = serde_json::from_str(&content)
                .unwrap_or_else(|e| panic!("{} is not complete JSON: {e}", p.display()));
            assert_eq!(v["case_id"], "same-id");
        }
        // No staging leftovers.
        let partials = std::fs::read_dir(&*dir)
            .unwrap()
            .filter_map(std::result::Result::ok)
            .filter(|e| e.path().to_string_lossy().ends_with(".partial"))
            .count();
        assert_eq!(
            partials, 0,
            "publication must leave no partial files behind"
        );
    }

    #[test]
    fn publication_runs_after_provider_validation() {
        // An invalid live provider must be rejected before anything destructive:
        // the previous run's artifacts have to survive a rejected invocation.
        let tmp = tempfile::tempdir().unwrap();
        let config = config_at(tmp.path());
        let root = artifact_root(&config);
        create_private_dir(&root).unwrap();
        let previous = stage_run_dir(&root).unwrap();
        std::fs::write(previous.join("prior.json"), "{\"keep\":true}").unwrap();
        publish_run(&root, &previous).unwrap();

        // `[eval].live_provider` is empty by default, so live mode is rejected.
        let rejected = build_run_deps(&config, Mode::Live);
        assert!(rejected.is_err(), "empty live_provider must be rejected");
        assert!(
            resolve_last_run(&root).unwrap().join("prior.json").exists(),
            "a rejected invocation must not destroy the previous run's diagnostics"
        );

        // A missing suite directory is likewise rejected before staging.
        assert!(validate_suite_dir(&tmp.path().join("no-such-suite")).is_err());
        assert!(resolve_last_run(&root).unwrap().join("prior.json").exists());
    }

    #[test]
    fn concurrent_runs_do_not_share_artifact_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("artifacts");
        create_private_dir(&root).unwrap();
        let a = stage_run_dir(&root).unwrap();
        let b = stage_run_dir(&root).unwrap();
        assert_ne!(a, b, "two runs must stage into distinct directories");
        assert!(a.is_dir() && b.is_dir());
    }

    #[test]
    fn publish_resolves_last_run_to_exactly_one_complete_run() {
        // Publishing run B over run A must make the pointer resolve to B's
        // immutable records only — never a mix of the two.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("artifacts");
        create_private_dir(&root).unwrap();

        let first = stage_run_dir(&root).unwrap();
        std::fs::write(first.join("old-case.json"), "{}").unwrap();
        publish_run(&root, &first).unwrap();

        let second = stage_run_dir(&root).unwrap();
        std::fs::write(second.join("new-case.json"), "{}").unwrap();
        let published = publish_run(&root, &second).unwrap();

        assert_eq!(published, resolve_last_run(&root).unwrap());
        assert!(published.join("new-case.json").exists());
        assert!(
            !first.exists(),
            "the previous run's sensitive records must be retired"
        );
        assert!(
            !published.join("old-case.json").exists(),
            "last-run must not mix records from a previous run"
        );
        let names: Vec<String> = std::fs::read_dir(&published)
            .unwrap()
            .filter_map(std::result::Result::ok)
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        assert_eq!(names, vec!["new-case.json".to_string()]);
    }

    #[test]
    fn concurrent_publishers_select_one_complete_run() {
        let tmp = tempfile::tempdir().unwrap();
        let root = std::sync::Arc::new(tmp.path().join("artifacts"));
        create_private_dir(&root).unwrap();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));

        let handles: Vec<_> = (0..8)
            .map(|n| {
                let root = std::sync::Arc::clone(&root);
                let barrier = std::sync::Arc::clone(&barrier);
                std::thread::spawn(move || {
                    let staged = stage_run_dir(&root).unwrap();
                    std::fs::write(staged.join(format!("case-{n}.json")), format!("{n}")).unwrap();
                    barrier.wait();
                    publish_run(&root, &staged)
                })
            })
            .collect();

        for handle in handles {
            handle.join().unwrap().unwrap();
        }
        let published = resolve_last_run(&root).unwrap();
        let files: Vec<_> = std::fs::read_dir(&published)
            .unwrap()
            .filter_map(std::result::Result::ok)
            .collect();
        assert_eq!(
            files.len(),
            1,
            "the selected run must be complete and unmixed"
        );
        assert!(files[0].path().extension().is_some_and(|ext| ext == "json"));
        assert_eq!(
            std::fs::read_dir(root.join(RUNS_SUBDIR)).unwrap().count(),
            1,
            "serialized publication must retire every superseded completed run"
        );
    }

    #[test]
    fn publication_does_not_remove_an_active_staging_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("artifacts");
        create_private_dir(&root).unwrap();
        let active = stage_run_dir(&root).unwrap();
        std::fs::write(active.join("in-progress.json"), "partial").unwrap();
        let completed = stage_run_dir(&root).unwrap();
        std::fs::write(completed.join("done.json"), "{}").unwrap();

        publish_run(&root, &completed).unwrap();

        assert!(
            active.join("in-progress.json").exists(),
            "publishing one process must not erase another process's staging dir"
        );
    }

    #[cfg(unix)]
    #[test]
    fn retired_run_cleanup_failure_is_surfaced() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("artifacts");
        create_private_dir(&root).unwrap();
        let first = stage_run_dir(&root).unwrap();
        std::fs::write(first.join("private.json"), "{}").unwrap();
        let first = publish_run(&root, &first).unwrap();
        std::fs::set_permissions(&first, std::fs::Permissions::from_mode(0o500)).unwrap();

        let second = stage_run_dir(&root).unwrap();
        std::fs::write(second.join("new.json"), "{}").unwrap();
        let error = publish_run(&root, &second)
            .expect_err("a failed retired-run cleanup must fail publication");

        std::fs::set_permissions(&first, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(
            error.to_string().contains("removing retired eval run"),
            "cleanup failure must identify the retained path: {error}"
        );
        assert!(
            resolve_last_run(&root).unwrap().join("new.json").exists(),
            "the pointer must still name the newly completed run"
        );
    }

    #[test]
    fn last_run_pointer_rejects_path_traversal() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("artifacts");
        create_private_dir(&root).unwrap();
        std::fs::write(root.join(LAST_RUN_POINTER), "../outside\n").unwrap();

        let error = resolve_last_run(&root).expect_err("pointer traversal must be rejected");
        assert!(
            error
                .to_string()
                .contains("invalid latest eval run pointer")
        );
    }

    #[test]
    fn history_disabled_by_default_writes_nothing() {
        let config = Config::default();
        let report = SuiteReport {
            cases: vec![case_report("pass", true)],
        };
        let writer_called = std::cell::Cell::new(false);
        let written = write_run_history_with(
            &config,
            None,
            &report,
            HistoryRun {
                recorded_at: chrono::Utc::now(),
                suite_dir: "evals/regression".to_string(),
                suite_kind: SuiteKind::Regression,
                mode: Mode::Replay,
                provider_ref: "scripted".to_string(),
            },
            None,
            |_, _| {
                writer_called.set(true);
                anyhow::bail!("disabled history must not invoke its writer")
            },
        );
        assert!(written.is_none());
        assert!(!writer_called.get());
        assert!(resolved_history_dir(None, &config.eval.history_dir).is_none());
    }

    #[test]
    fn history_suite_dir_omits_absolute_workspace_prefix() {
        let cwd = std::env::current_dir().expect("test process has a current directory");
        let inside = cwd.join("evals").join("regression");
        assert_eq!(
            history_suite_dir(&inside),
            Path::new("evals").join("regression").display().to_string()
        );

        let outside = tempfile::tempdir().expect("outside suite root");
        let outside_suite = outside.path().join("private-identity").join("capability");
        assert_eq!(history_suite_dir(&outside_suite), "capability");
        assert!(!history_suite_dir(&outside_suite).contains("private-identity"));

        let traversing = cwd.join("..").join("private-identity").join("capability");
        assert_eq!(history_suite_dir(&traversing), "capability");
        assert!(!history_suite_dir(&traversing).contains("private-identity"));

        assert!(!history_suite_dir(Path::new(".")).is_empty());
        assert!(!history_suite_dir(Path::new("..")).is_empty());
    }

    #[tokio::test]
    async fn history_written_on_write_baseline_early_return() {
        let config = Config::default();
        let history_root = tempfile::tempdir().unwrap();
        let baseline_root = tempfile::tempdir().unwrap();
        let report = SuiteReport {
            cases: vec![case_report("pass", true)],
        };
        let code = finalize(
            &config,
            Mode::Replay,
            Path::new("evals/regression"),
            "scripted",
            report,
            RunArtifacts {
                root: artifact_root(&config),
                staged: stage_run_dir(&artifact_root(&config)).unwrap(),
            },
            FinalizeOpts {
                format: OutputFormat::Json,
                dump_records: None,
                baseline: None,
                write_baseline: Some(baseline_root.path().join("baseline.json")),
                suite_kind: None,
                history_dir: Some(history_root.path().to_path_buf()),
            },
        )
        .await
        .unwrap();
        assert_eq!(code, 0);
        assert!(baseline_root.path().join("baseline.json").exists());
        assert_eq!(
            std::fs::read_dir(history_root.path().join("regression"))
                .unwrap()
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn history_write_failure_does_not_change_exit_code() {
        let config = Config::default();
        let temp = tempfile::tempdir().unwrap();
        let blocked = temp.path().join("not-a-directory");
        std::fs::write(&blocked, "file").unwrap();
        let report = SuiteReport {
            cases: vec![case_report("pass", true)],
        };
        let code = finalize(
            &config,
            Mode::Replay,
            Path::new("evals/regression"),
            "scripted",
            report,
            RunArtifacts {
                root: artifact_root(&config),
                staged: stage_run_dir(&artifact_root(&config)).unwrap(),
            },
            FinalizeOpts {
                format: OutputFormat::Json,
                dump_records: None,
                baseline: None,
                write_baseline: None,
                suite_kind: None,
                history_dir: Some(blocked),
            },
        )
        .await
        .unwrap();
        assert_eq!(code, 0);
    }
}
