//! `zeroclaw eval` — run the agent evaluation harness.

use anyhow::{Context, Result};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;
use zeroclaw_config::schema::Config;
use zeroclaw_eval::{CaseReport, LlmTrace, Mode, RunDeps, SuiteReport};
use zeroclaw_runtime::agent::agent::build_session_model_provider;

#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};

/// Artifact root for eval diagnostics, relative to the install root.
///
/// Anchored to the config's install root rather than the process working
/// directory: a CWD-relative `target/...` path lets a run from a nested directory
/// drop unredacted transcripts into a tracked location, because the repository
/// ignores only the root-anchored `/target`.
const ARTIFACT_SUBDIR: &str = "eval-artifacts";

/// Name of the pointer directory that always resolves to exactly one complete run.
const LAST_RUN_LINK: &str = "last-run";

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

/// The directory `zeroclaw eval-last-run` reads: `<artifact_root>/last-run`.
#[must_use]
pub fn last_run_dir(config: &Config) -> PathBuf {
    artifact_root(config).join(LAST_RUN_LINK)
}

/// Create `dir` (and parents) owner-only. On Unix the mode is applied at creation
/// time, so the directory is never briefly world-readable.
pub fn create_private_dir(dir: &Path) -> Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    builder.mode(DIR_MODE);
    builder
        .create(dir)
        .with_context(|| format!("creating private eval artifact dir {}", dir.display()))
}

/// Remove `dir`, tolerating only its absence.
///
/// Swallowing every removal error means a failed cleanup leaves the previous
/// run's unredacted transcripts in place while the command reports success, and
/// lets stale records mix with current ones under suffixed names.
pub fn remove_artifacts(dir: &Path) -> Result<()> {
    match std::fs::remove_dir_all(dir) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e)
            .with_context(|| format!("clearing previous eval run artifacts at {}", dir.display())),
    }
}

/// A unique staging directory for one run: `<artifact_root>/runs/<run_id>`.
///
/// Concurrent eval processes must not share a destructive directory, so the id
/// carries the pid and a monotonic timestamp and the directory is claimed with
/// `create_new` semantics (a plain `create_dir` fails if it already exists).
pub fn stage_run_dir(root: &Path) -> Result<PathBuf> {
    create_private_dir(&root.join("runs"))?;
    let pid = std::process::id();
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    for n in 0..MAX_DUMP_COLLISIONS {
        let candidate = root.join("runs").join(format!("{stamp}-{pid}-{n}"));
        let mut builder = std::fs::DirBuilder::new();
        #[cfg(unix)]
        builder.mode(DIR_MODE);
        match builder.create(&candidate) {
            Ok(()) => return Ok(candidate),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                // Collision on this candidate; try the next one.
            }
            Err(e) => {
                return Err(e)
                    .with_context(|| format!("staging eval run dir at {}", candidate.display()));
            }
        }
    }
    anyhow::bail!(
        "could not claim a staging dir under {} after {MAX_DUMP_COLLISIONS} attempts",
        root.join("runs").display()
    )
}

/// Publish a staged run as `last-run`, atomically replacing any previous one.
///
/// The old `last-run` is moved aside and only then removed, so a reader never
/// observes a missing or half-written directory, and `eval-last-run` always
/// resolves to exactly one complete run rather than a mix of two.
pub fn publish_run(root: &Path, staged: &Path) -> Result<PathBuf> {
    let target = root.join(LAST_RUN_LINK);
    let retired = root.join(format!(".{LAST_RUN_LINK}-retiring-{}", std::process::id()));
    remove_artifacts(&retired)?;
    if target.exists() {
        std::fs::rename(&target, &retired).with_context(|| {
            format!(
                "retiring previous {} at {}",
                LAST_RUN_LINK,
                target.display()
            )
        })?;
    }
    std::fs::rename(staged, &target).with_context(|| {
        format!(
            "publishing eval run {} as {}",
            staged.display(),
            target.display()
        )
    })?;
    remove_artifacts(&retired)?;
    Ok(target)
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
                    let (provider, _provider_type, _resolved_model) =
                        build_session_model_provider(&cfg, &provider_ref, None)?;
                    // Live mode has no scripted steps, so no turn boundary to enforce.
                    Ok(provider.into())
                }),
                provider_ref: receipt_ref,
                live_tools: config.eval.live_allowed_tools.clone(),
                case_timeout: Duration::from_secs(config.eval.case_timeout_secs),
            })
        }
    }
}

/// One run's artifact lifecycle: a private staging dir that is published as
/// `last-run` only once the run completes.
pub struct RunArtifacts {
    /// `<install>/eval-artifacts`.
    pub root: PathBuf,
    /// This run's owned staging dir; nothing else writes here.
    pub staged: PathBuf,
}

impl RunArtifacts {
    /// Publish the staged run as `last-run`, replacing any previous one.
    pub fn publish(self) -> Result<PathBuf> {
        publish_run(&self.root, &self.staged)
    }
}

/// Prepare this run's artifact staging area.
///
/// Deliberately called *after* provider and suite validation: cleanup that runs
/// before validation destroys the previous run's diagnostics on an invocation
/// that never produces replacements.
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
/// Ordering matters: the provider and suite are validated first, then the run
/// stages into its own directory. A rejected invocation therefore leaves the
/// previous run's artifacts intact, and two concurrent runs never share a
/// destructive directory.
pub async fn run(
    config: &Config,
    suite: PathBuf,
    mode: Mode,
) -> Result<(SuiteReport, RunArtifacts)> {
    // Validation before destruction.
    let deps = build_run_deps(config, mode)?;
    validate_suite_dir(&suite)?;

    let artifacts = prepare_artifacts(config)?;
    let report = Box::pin(zeroclaw_eval::run_suite(&suite, &deps)).await?;
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

/// Atomically create a new owner-only file for `case_id` under `dir`, returning
/// the handle and its path.
///
/// `O_CREAT|O_EXCL` binds to a *new* regular file, so a dangling symlink planted
/// at the candidate path is not followed and a competing writer loses the race
/// instead of having its output truncated. A check-then-write `exists()` loop
/// offers neither guarantee.
fn create_new_dump_file(dir: &Path, case_id: &str) -> Result<(std::fs::File, PathBuf)> {
    let stem = dump_stem(case_id);
    for n in 0..MAX_DUMP_COLLISIONS {
        let path = if n == 0 {
            dir.join(format!("{stem}.json"))
        } else {
            dir.join(format!("{stem}_{n}.json"))
        };
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        opts.mode(FILE_MODE);
        match opts.open(&path) {
            Ok(f) => return Ok((f, path)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                // Collision on this candidate; try the next one.
            }
            Err(e) => {
                return Err(e)
                    .with_context(|| format!("creating eval transcript dump {}", path.display()));
            }
        }
    }
    anyhow::bail!(
        "could not create a transcript dump for case {case_id:?} in {} after {MAX_DUMP_COLLISIONS} attempts",
        dir.display()
    )
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

    // Claim the final name first so collision handling stays atomic, then write
    // the bytes through a staging file and publish by rename.
    let (final_file, final_path) = create_new_dump_file(dir, &case.name)?;
    drop(final_file);

    let staging = final_path.with_extension("json.partial");
    let _ = std::fs::remove_file(&staging);
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    opts.mode(FILE_MODE);
    let mut f = opts
        .open(&staging)
        .with_context(|| format!("staging eval transcript dump {}", staging.display()))?;
    f.write_all(&json)?;
    f.sync_all()?;
    drop(f);
    std::fs::rename(&staging, &final_path)
        .with_context(|| format!("publishing eval transcript dump {}", final_path.display()))?;
    Ok(final_path)
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
    use zeroclaw_eval::RunRecord;
    use zeroclaw_eval::record::{CaseProvenance, RunCompletion, SandboxStamp, ToolSurface};

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
            grades: Vec::new(),
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
    fn cleanup_missing_dir_is_ok() {
        let tmp = tempfile::tempdir().unwrap();
        remove_artifacts(&tmp.path().join("never-existed")).expect("NotFound must be tolerated");
    }

    #[cfg(unix)]
    #[test]
    fn cleanup_failure_is_surfaced_not_swallowed() {
        use std::os::unix::fs::PermissionsExt;
        // If cleanup fails and the next suite passes, old sensitive transcripts
        // remain while the command reports success. The error must propagate and
        // name the artifact root.
        let tmp = tempfile::tempdir().unwrap();
        let parent = tmp.path().join("locked");
        let victim = parent.join("stale-run");
        std::fs::create_dir_all(&victim).unwrap();
        std::fs::write(victim.join("t.json"), "{}").unwrap();
        // Read-only parent: the entry cannot be unlinked.
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o500)).unwrap();

        let err = remove_artifacts(&victim);
        // Restore before asserting so the tempdir can always be cleaned up.
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o700)).unwrap();

        let err = err.expect_err("a failed cleanup must not be swallowed");
        assert!(
            err.to_string().contains("stale-run"),
            "the error must name the artifact path: {err}"
        );
    }

    #[test]
    fn cleanup_runs_after_provider_validation() {
        // An invalid live provider must be rejected before anything destructive:
        // the previous run's artifacts have to survive a rejected invocation.
        let tmp = tempfile::tempdir().unwrap();
        let config = config_at(tmp.path());
        let root = artifact_root(&config);
        let previous = root.join(LAST_RUN_LINK);
        create_private_dir(&previous).unwrap();
        std::fs::write(previous.join("prior.json"), "{\"keep\":true}").unwrap();

        // `[eval].live_provider` is empty by default, so live mode is rejected.
        let rejected = build_run_deps(&config, Mode::Live);
        assert!(rejected.is_err(), "empty live_provider must be rejected");
        assert!(
            previous.join("prior.json").exists(),
            "a rejected invocation must not destroy the previous run's diagnostics"
        );

        // A missing suite directory is likewise rejected before staging.
        assert!(validate_suite_dir(&tmp.path().join("no-such-suite")).is_err());
        assert!(previous.join("prior.json").exists());
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
        // Publishing run B over run A must leave `last-run` holding B's records
        // only — never a mix of the two under suffixed names.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("artifacts");
        create_private_dir(&root).unwrap();

        let first = stage_run_dir(&root).unwrap();
        std::fs::write(first.join("old-case.json"), "{}").unwrap();
        publish_run(&root, &first).unwrap();

        let second = stage_run_dir(&root).unwrap();
        std::fs::write(second.join("new-case.json"), "{}").unwrap();
        let published = publish_run(&root, &second).unwrap();

        assert!(published.join("new-case.json").exists());
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
}
