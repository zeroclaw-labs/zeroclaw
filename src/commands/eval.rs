//! `zeroclaw eval` — run the agent evaluation harness.

use anyhow::{Context, Result};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;
use zeroclaw_config::schema::Config;
use zeroclaw_eval::{CaseProvider, CaseReport, LlmTrace, Mode, RunDeps, SuiteReport};
use zeroclaw_runtime::agent::agent::build_session_model_provider;

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

/// Build the per-run dependencies for the requested mode, threading the loaded
/// config so live mode can resolve its provider. Replay injects the deterministic
/// trace-replay provider; live resolves `[eval].live_provider` per case.
fn build_run_deps(config: &Config, mode: Mode) -> Result<RunDeps> {
    match mode {
        // Replay's provider wiring is owned by `RunDeps::replay()`; delegate so the
        // trace-replay factory has a single definition. Replay ignores the live-only
        // tool allowlist and timeout.
        Mode::Replay => Ok(RunDeps::replay()),
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
            let cfg = config.clone();
            Ok(RunDeps {
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
            })
        }
    }
}

/// One run's artifact lifecycle: a private, uniquely named directory that is
/// selected by `last-run` only once the run completes.
pub struct RunArtifacts {
    /// `<install>/eval-artifacts`.
    pub root: PathBuf,
    /// This run's owned staging dir; nothing else writes here.
    pub staged: PathBuf,
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
pub async fn run(
    config: &Config,
    suite: PathBuf,
    mode: Mode,
) -> Result<(SuiteReport, RunArtifacts)> {
    let deps = build_run_deps(config, mode)?;
    validate_suite_dir(&suite)?;

    let report = Box::pin(zeroclaw_eval::run_suite(&suite, &deps)).await?;
    let artifacts = prepare_artifacts(config)?;
    Ok((report, artifacts))
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

/// Output format for the eval report.
#[derive(Copy, Clone, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum OutputFormat {
    /// Human-readable table.
    Table,
    /// Machine-readable JSON, for CI artifacts.
    Json,
}

/// Render a suite report in the requested format.
pub fn print_report(report: &SuiteReport, format: OutputFormat) {
    match format {
        OutputFormat::Json => println!("{}", report.to_json()),
        OutputFormat::Table => println!("{}", report.render_table()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zeroclaw_eval::record::{CaseProvenance, RunCompletion, SandboxStamp, ToolSurface};
    use zeroclaw_eval::{GradeCategory, GradeResult, RunRecord};

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
}
