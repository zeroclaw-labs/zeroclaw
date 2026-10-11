//! Process-boundary regressions: hooks pause actual recovery, not a model of it.
use super::*;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, Command, Stdio};

#[test]
fn recovery_process_child() {
    let Ok(root) = std::env::var("ZC_RECOVERY_ROOT") else {
        return;
    };
    let mut host = PluginHost::from_plugins_dir(Path::new(&root)).unwrap();
    if std::env::var("ZC_RECOVERY_ACTION").as_deref() == Ok("stage") {
        let tx = host
            .root()
            .unwrap()
            .transaction("race", "installing")
            .unwrap();
        tx.dir.create_dir(recovery::PACKAGE).unwrap();
        tx.dir.write("package/bytes", b"active owner").unwrap();
        println!("STAGE:{}", tx.entry);
        std::io::stdout().flush().unwrap();
        recovery::pause("stage-owned");
        // Dropping the process/lease without cleanup simulates abandonment.
        return;
    }
    if std::env::var("ZC_RECOVERY_ACTION").as_deref() == Ok("install") {
        let source = std::env::var("ZC_RECOVERY_SOURCE").unwrap();
        let result = host.install(&source);
        println!("RESULT:{result:?}");
        return;
    }
    if std::env::var("ZC_RECOVERY_ACTION").as_deref() == Ok("recover") {
        let result = host.recover_interrupted_update("race");
        println!("RESULT:{result:?}");
        return;
    }
    if std::env::var("ZC_RECOVERY_ACTION").as_deref() == Ok("update") {
        let source = Path::new(&root).join(".source");
        let admitted = host.admit_update("race", source.to_str().unwrap()).unwrap();
        let result = host
            .update_admitted(admitted)
            .map(|replaced| replaced.previous_version);
        println!("RESULT:{result:?}");
        return;
    }
    if std::env::var("ZC_RECOVERY_ACTION").as_deref() == Ok("healthy-late") {
        tests::write_tool_source(
            &Path::new(&root).join("race"),
            "race",
            b"\0asm healthy restored",
        );
    }
    let result = host.remove_with_report("race");
    println!("RESULT:{result:?}");
}

struct Paused {
    child: Child,
    output: BufReader<std::process::ChildStdout>,
    lines: String,
}
impl Paused {
    fn start(root: &Path, step: &str, action: &str) -> Self {
        Self::spawn(root, step, action, None)
    }
    fn start_install(root: &Path, step: &str, source: &Path) -> Self {
        Self::spawn(root, step, "install", Some(source))
    }
    fn spawn(root: &Path, step: &str, action: &str, source: Option<&Path>) -> Self {
        let mut command = Command::new(std::env::current_exe().unwrap());
        if let Some(source) = source {
            command.env("ZC_RECOVERY_SOURCE", source);
        }
        let mut child = command
            .args([
                "--exact",
                "host::recovery_tests::recovery_process_child",
                "--nocapture",
            ])
            .env("ZC_RECOVERY_ROOT", root)
            .env("ZC_RECOVERY_BARRIER", step)
            .env("ZC_RECOVERY_ACTION", action)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let output = BufReader::new(child.stdout.take().unwrap());
        let mut child = Self {
            child,
            output,
            lines: String::new(),
        };
        loop {
            let mut line = String::new();
            assert_ne!(
                child.output.read_line(&mut line).unwrap(),
                0,
                "child ended before barrier: {}",
                child.lines
            );
            child.lines.push_str(&line);
            if line.starts_with("BARRIER:") {
                break;
            }
        }
        child
    }
    fn advance(&mut self, step: &str) {
        writeln!(self.child.stdin.as_mut().unwrap(), "continue").unwrap();
        loop {
            let mut line = String::new();
            assert_ne!(
                self.output.read_line(&mut line).unwrap(),
                0,
                "child ended before second barrier: {}",
                self.lines
            );
            self.lines.push_str(&line);
            if line.trim() == format!("BARRIER:{step}") {
                break;
            }
        }
    }
    /// Let the child past its barrier without waiting for it.
    fn release(&mut self) {
        writeln!(self.child.stdin.as_mut().unwrap(), "continue").unwrap();
    }
    fn running(&mut self) -> bool {
        self.child.try_wait().unwrap().is_none()
    }
    /// Wait for a released child and return everything it printed.
    fn finish(mut self) -> String {
        drop(self.child.stdin.take());
        self.output.read_to_string(&mut self.lines).unwrap();
        assert!(self.child.wait().unwrap().success(), "{}", self.lines);
        self.lines.clone()
    }
    fn resume(mut self) -> String {
        writeln!(self.child.stdin.take().unwrap(), "continue").unwrap();
        self.output.read_to_string(&mut self.lines).unwrap();
        assert!(self.child.wait().unwrap().success(), "{}", self.lines);
        self.lines.clone()
    }
    fn crash(mut self) {
        self.child.kill().unwrap();
        self.child.wait().unwrap();
    }
}
impl Drop for Paused {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn broken(root: &Path) {
    std::fs::create_dir(root.join("race")).unwrap();
    std::fs::write(root.join("race/manifest.toml"), "name =").unwrap();
}
fn healthy(root: &Path) -> Vec<(String, Vec<u8>)> {
    tests::write_tool_source(&root.join("race"), "race", b"\0asm healthy replacement");
    tests::package_bytes(&root.join("race"))
}

#[test]
fn separate_remover_validates_replacement_before_claim_and_restores_it() {
    let root = tempfile::tempdir().unwrap();
    broken(root.path());
    let paused = Paused::start(root.path(), "before-claim", "remove");
    std::fs::rename(root.path().join("race"), root.path().join("old")).unwrap();
    std::fs::create_dir(root.path().join("race")).unwrap();
    let expected = healthy(root.path());
    let output = paused.resume();
    assert!(
        output.contains("Namespace") || output.contains("namespace"),
        "{output}"
    );
    assert_eq!(tests::package_bytes(&root.path().join("race")), expected);
    assert!(root.path().join("old/manifest.toml").exists());
}

#[test]
fn separate_remover_deletes_only_claimed_generation_after_final_replacement() {
    let root = tempfile::tempdir().unwrap();
    broken(root.path());
    let paused = Paused::start(root.path(), "before-delete", "remove");
    std::fs::create_dir(root.path().join("race")).unwrap();
    let expected = healthy(root.path());
    assert!(paused.resume().contains("RESULT:Ok"));
    assert_eq!(tests::package_bytes(&root.path().join("race")), expected);
}

/// A remover paused just before its delete holds the package lock, so a second
/// remover and an installer that reached the lock wait instead of finishing
/// under it. The paused
/// remover then deletes only the generation it claimed, and the healthy
/// package the installer publishes afterwards survives byte for byte.
#[test]
fn a_second_remover_and_an_installer_wait_for_a_remover_paused_before_its_delete() {
    let root = tempfile::tempdir().unwrap();
    broken(root.path());
    let source = tempfile::tempdir().unwrap();
    tests::write_tool_source(source.path(), "race", b"\0asm healthy replacement");
    let expected = tests::package_bytes(source.path());

    let first = Paused::start(root.path(), "before-delete", "remove");
    let mut second = Paused::start(root.path(), "remove-before-lock", "remove");
    let mut installer = Paused::start_install(root.path(), "install-before-lock", source.path());
    second.release();
    installer.release();
    std::thread::sleep(std::time::Duration::from_millis(500));
    assert!(
        second.running(),
        "a second remover finished under the first"
    );
    assert!(installer.running(), "an installer finished under a remover");

    assert!(first.resume().contains("RESULT:Ok"));
    let second = second.finish();
    let installer = installer.finish();
    assert!(installer.contains("RESULT:Ok(\"race\")"), "{installer}");
    // Whichever took the lock first, the second remover never deletes the
    // healthy package: it finds nothing, or admission accepts what it claims.
    assert!(
        second.contains("RESULT:Err(NotFound") || second.contains("admission accepts this package"),
        "{second}"
    );
    assert_eq!(tests::package_bytes(&root.path().join("race")), expected);
    assert_eq!(
        tests::dir_entries(root.path()),
        [".zeroclaw-package-lock-v1", "race"]
    );
}

/// A process stopped between creating a transaction's entry and its lease
/// leaves an empty entry with no lease. It holds nothing, so the next recovery
/// removes it instead of refusing every later remove. An entry without a lease
/// that holds a package is still kept and reported.
#[test]
fn an_entry_stopped_before_its_lease_is_removed_and_one_holding_a_package_is_kept() {
    let root = tempfile::tempdir().unwrap();
    broken(root.path());
    Paused::start(root.path(), "transaction-entry", "remove").crash();
    let entry = tests::dir_entries(root.path())
        .into_iter()
        .find(|p| p.contains("recovering-v1"))
        .unwrap();
    assert!(tests::dir_entries(&root.path().join(&entry)).is_empty());
    let mut host = PluginHost::from_plugins_dir(root.path()).unwrap();
    host.remove("race").unwrap();
    assert_eq!(
        tests::dir_entries(root.path()),
        [".zeroclaw-package-lock-v1"]
    );

    let held = root
        .path()
        .join(".race.recovering-v1-0123456789abcdef0123456789abcdef");
    std::fs::create_dir_all(held.join("package")).unwrap();
    std::fs::write(held.join("package/keep"), b"claimed bytes").unwrap();
    broken(root.path());
    assert!(matches!(
        host.remove("race"),
        Err(PluginError::RecoveryRetained { .. })
    ));
    assert_eq!(
        std::fs::read(held.join("package/keep")).unwrap(),
        b"claimed bytes"
    );
}

/// A remover stopped once it has begun deleting leaves the claim marked, so the
/// retry finishes that delete rather than putting back what is left. That holds
/// even when the remainder would pass admission, as a skill bundle stopped
/// between its skills can, which the test stands in for by making the claim a
/// healthy package.
#[test]
fn a_delete_stopped_part_way_is_finished_by_the_retry() {
    for step in ["delete-marked", "entry-deleted"] {
        let root = tempfile::tempdir().unwrap();
        // The manifest parses but declares a skill bundle that has no skill,
        // so admission rejects it, and every entry is one an install writes.
        std::fs::create_dir(root.path().join("race")).unwrap();
        std::fs::write(
            root.path().join("race/manifest.toml"),
            "name = \"race\"\nversion = \"0.1.0\"\nwasm_path = \"plugin.wasm\"\ncapabilities = [\"tool\", \"skill\"]\n",
        )
        .unwrap();
        let legacy = root.path().join(".race.installing-4242");
        std::fs::create_dir(&legacy).unwrap();
        std::fs::write(root.path().join("race/plugin.wasm"), b"\0asm partial").unwrap();
        std::fs::create_dir(root.path().join("race/skills")).unwrap();
        std::fs::write(root.path().join("race/skills/notes.md"), b"notes").unwrap();
        Paused::start(root.path(), step, "remove").crash();
        let claim = tests::dir_entries(root.path())
            .into_iter()
            .find(|p| p.contains("recovering-v1"))
            .unwrap();
        let package = root.path().join(&claim).join("package");
        std::fs::remove_dir_all(&package).unwrap();
        std::fs::create_dir(&package).unwrap();
        tests::write_tool_source(&package, "race", b"\0asm what is left");

        let mut host = PluginHost::from_plugins_dir(root.path()).unwrap();
        assert_eq!(
            host.remove_with_report("race").unwrap().as_slice(),
            std::slice::from_ref(&legacy),
            "{step}"
        );
        assert_eq!(
            tests::dir_entries(root.path()),
            [".race.installing-4242", ".zeroclaw-package-lock-v1"],
            "{step}"
        );
    }
}

/// An empty claim is removed by an empty-only unlink, so one that gains an
/// entry after its verdict is kept, not emptied.
#[test]
fn an_empty_claim_that_gains_an_entry_after_its_verdict_is_kept() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("race")).unwrap();
    let paused = Paused::start(root.path(), "after-verdict", "remove");
    let claim = tests::dir_entries(root.path())
        .into_iter()
        .find(|p| p.contains("recovering-v1"))
        .unwrap();
    let late = root.path().join(&claim).join("package/keep");
    std::fs::write(&late, b"late bytes").unwrap();
    let output = paused.resume();
    assert!(output.contains("RecoveryRetained"), "{output}");
    assert_eq!(std::fs::read(&late).unwrap(), b"late bytes");
}

/// When the package is deleted but its transaction cannot be finished, the
/// remove reports the transaction entry it kept rather than a bare I/O error,
/// and so does a retry that finishes a delete an earlier remove had begun.
#[test]
fn a_remove_that_deleted_but_could_not_finish_names_the_entry() {
    let root = tempfile::tempdir().unwrap();
    broken(root.path());
    let paused = Paused::start(root.path(), "package-deleted", "remove");
    let claim = tests::dir_entries(root.path())
        .into_iter()
        .find(|p| p.contains("recovering-v1"))
        .unwrap();
    std::fs::write(root.path().join(&claim).join("stray"), b"stray").unwrap();
    let output = paused.resume();
    assert!(
        output.contains("RecoveryRetained") && output.contains(&claim),
        "{output}"
    );
    assert!(!root.path().join("race").exists());

    let root = tempfile::tempdir().unwrap();
    broken(root.path());
    Paused::start(root.path(), "delete-marked", "remove").crash();
    let claim = tests::dir_entries(root.path())
        .into_iter()
        .find(|p| p.contains("recovering-v1"))
        .unwrap();
    std::fs::write(root.path().join(&claim).join("stray"), b"stray").unwrap();
    let mut host = PluginHost::from_plugins_dir(root.path()).unwrap();
    let retried = host.remove("race");
    assert!(
        matches!(&retried, Err(PluginError::RecoveryRetained { path, .. }) if path.contains(&claim)),
        "{retried:?}"
    );
    assert!(!root.path().join(&claim).join("package").exists());
}

/// A refusal that put the package back but could not clear its transaction
/// entry names that entry, and still says why the package was kept.
#[test]
fn a_refusal_that_put_the_package_back_but_could_not_finish_names_the_entry() {
    let root = tempfile::tempdir().unwrap();
    broken(root.path());
    let paused = Paused::start(root.path(), "before-restore", "healthy-late");
    let claim = tests::dir_entries(root.path())
        .into_iter()
        .find(|p| p.contains("recovering-v1"))
        .unwrap();
    std::fs::write(root.path().join(&claim).join("stray"), b"stray").unwrap();
    let output = paused.resume();
    assert!(
        output.contains("RecoveryRetained")
            && output.contains(&claim)
            && output.contains("admission accepts this package"),
        "{output}"
    );
    assert!(root.path().join("race/manifest.toml").is_file());
}

/// An install that published its package but could not clear its stage has
/// still installed it: only the stage's entry is left, for the stage sweep.
#[test]
fn an_install_that_published_but_could_not_clear_its_stage_succeeds() {
    let root = tempfile::tempdir().unwrap();
    let source = tempfile::tempdir().unwrap();
    tests::write_tool_source(source.path(), "race", b"\0asm installed");
    let paused = Paused::start_install(root.path(), "install-published", source.path());
    let stage = tests::dir_entries(root.path())
        .into_iter()
        .find(|p| p.contains("installing-v1"))
        .unwrap();
    std::fs::write(root.path().join(&stage).join("stray"), b"stray").unwrap();
    let output = paused.resume();
    assert!(output.contains("RESULT:Ok(\"race\")"), "{output}");
    assert_eq!(
        std::fs::read(root.path().join("race/plugin.wasm")).unwrap(),
        b"\0asm installed"
    );
}

#[test]
fn crash_after_claim_restores_then_recovers_on_retry() {
    let root = tempfile::tempdir().unwrap();
    broken(root.path());
    Paused::start(root.path(), "after-claim", "remove").crash();
    assert!(!root.path().join("race").exists());
    let mut host = PluginHost::from_plugins_dir(root.path()).unwrap();
    host.remove("race").unwrap();
    assert_eq!(
        tests::dir_entries(root.path()),
        [".zeroclaw-package-lock-v1"]
    );
    let source = tempfile::tempdir().unwrap();
    tests::write_tool_source(source.path(), "race", b"\0asm reinstall");
    assert_eq!(
        host.install(source.path().to_str().unwrap()).unwrap(),
        "race"
    );
}

#[test]
fn healthy_claim_restore_never_clobbers_and_crash_retry_retains_both() {
    let root = tempfile::tempdir().unwrap();
    broken(root.path());
    // A late healthy package is unknown to the child's loaded map.
    let paused = Paused::start(root.path(), "after-claim", "remove");
    let claim = tests::dir_entries(root.path())
        .into_iter()
        .find(|p| p.contains("recovering-v1"))
        .unwrap();
    tests::write_tool_source(
        &root.path().join(&claim).join("package"),
        "race",
        b"\0asm healthy claim",
    );
    std::fs::create_dir(root.path().join("race")).unwrap();
    let expected = healthy(root.path());
    let output = paused.resume();
    assert!(output.contains("RecoveryRetained"), "{output}");
    assert_eq!(tests::package_bytes(&root.path().join("race")), expected);
    let retained = tests::package_bytes(&root.path().join(&claim).join("package"));
    let mut host =
        PluginHost::from_plugins_dir_with_security(root.path(), SignatureMode::Strict, vec![])
            .unwrap();
    let blocked = host.remove("race");
    assert!(
        matches!(blocked, Err(PluginError::RecoveryRetained { .. })),
        "{blocked:?}"
    );
    assert_eq!(
        tests::package_bytes(&root.path().join(&claim).join("package")),
        retained
    );
    assert_eq!(tests::package_bytes(&root.path().join("race")), expected);
    std::fs::rename(root.path().join("race"), root.path().join("second")).unwrap();
    let restored = host.remove("race");
    assert!(
        matches!(restored, Err(PluginError::UnadmittedPackage { .. })),
        "{restored:?}"
    );
    assert_eq!(tests::package_bytes(&root.path().join("race")), retained);
}

#[test]
fn real_active_stage_survives_and_abandoned_protocol_stage_is_cleaned() {
    let root = tempfile::tempdir().unwrap();
    broken(root.path());
    let stage = Paused::start(root.path(), "stage-owned", "stage");
    let name = stage
        .lines
        .lines()
        .find_map(|line| line.strip_prefix("STAGE:"))
        .unwrap()
        .to_string();
    let mut host = PluginHost::from_plugins_dir(root.path()).unwrap();
    let retained = host.remove_with_report("race").unwrap();
    assert!(retained.contains(&root.path().join(&name)));
    assert_eq!(
        std::fs::read(root.path().join(&name).join("package/bytes")).unwrap(),
        b"active owner"
    );
    stage.crash();
    broken(root.path());
    host.remove("race").unwrap();
    assert!(!root.path().join(name).exists());
}

#[cfg(unix)]
#[test]
fn paused_stage_cleanup_does_not_delete_replacement_generation() {
    let root = tempfile::tempdir().unwrap();
    broken(root.path());
    let owner = Paused::start(root.path(), "stage-owned", "stage");
    let stage = owner
        .lines
        .lines()
        .find_map(|line| line.strip_prefix("STAGE:"))
        .unwrap()
        .to_string();
    owner.crash();
    let remover = Paused::start(root.path(), "before-stage-delete", "remove");
    std::fs::rename(root.path().join(&stage), root.path().join("claimed-old")).unwrap();
    std::fs::create_dir(root.path().join(&stage)).unwrap();
    std::fs::write(
        root.path().join(&stage).join("new-live-bytes"),
        b"never delete",
    )
    .unwrap();
    let output = remover.resume();
    assert!(output.contains("RecoveryRetained"), "{output}");
    assert_eq!(
        std::fs::read(root.path().join(&stage).join("new-live-bytes")).unwrap(),
        b"never delete"
    );
}

#[cfg(unix)]
#[test]
fn retained_root_and_replaced_coordination_lock_refuse_namespace_changes() {
    for swap_root in [true, false] {
        let parent = tempfile::tempdir().unwrap();
        let root = parent.path().join("plugins");
        std::fs::create_dir(&root).unwrap();
        broken(&root);
        let paused = Paused::start(&root, "before-claim", "remove");
        if swap_root {
            std::fs::rename(&root, parent.path().join("old-root")).unwrap();
            let other = parent.path().join("other");
            std::fs::create_dir(&other).unwrap();
            std::os::unix::fs::symlink(&other, &root).unwrap();
            std::fs::write(other.join("keep"), b"outside").unwrap();
        } else {
            std::fs::rename(
                root.join(".zeroclaw-package-lock-v1"),
                root.join("old-lock"),
            )
            .unwrap();
            std::fs::write(root.join(".zeroclaw-package-lock-v1"), b"").unwrap();
        }
        let output = paused.resume();
        assert!(output.contains("NamespaceChanged"), "{output}");
        if swap_root {
            assert_eq!(
                std::fs::read(parent.path().join("other/keep")).unwrap(),
                b"outside"
            );
        } else {
            assert_eq!(
                std::fs::read(root.join("race/manifest.toml")).unwrap(),
                b"name ="
            );
        }
    }
}

#[test]
fn crashed_healthy_restore_is_retried_without_deleting_its_bytes() {
    let root = tempfile::tempdir().unwrap();
    broken(root.path());
    Paused::start(root.path(), "before-restore", "healthy-late").crash();
    let claim = tests::dir_entries(root.path())
        .into_iter()
        .find(|p| p.contains("recovering-v1"))
        .unwrap();
    let expected = tests::package_bytes(&root.path().join(&claim).join("package"));
    let mut host = PluginHost::from_plugins_dir(root.path()).unwrap();
    assert!(matches!(
        host.remove("race"),
        Err(PluginError::UnadmittedPackage { .. })
    ));
    assert_eq!(tests::package_bytes(&root.path().join("race")), expected);
    assert!(!root.path().join(claim).exists());
}

#[test]
fn abandoned_empty_claim_and_legacy_stage_have_distinct_retry_outcomes() {
    let root = tempfile::tempdir().unwrap();
    broken(root.path());
    let legacy = root.path().join(".race.installing-4242");
    std::fs::create_dir(&legacy).unwrap();
    std::fs::write(legacy.join("keep"), b"legacy bytes").unwrap();
    Paused::start(root.path(), "claim-created", "remove").crash();
    let mut host = PluginHost::from_plugins_dir(root.path()).unwrap();
    assert_eq!(
        host.remove_with_report("race").unwrap().as_slice(),
        std::slice::from_ref(&legacy)
    );
    assert_eq!(std::fs::read(legacy.join("keep")).unwrap(), b"legacy bytes");
    assert!(matches!(host.remove("race"), Err(PluginError::NotFound(_))));
    assert!(legacy.exists());
}

#[test]
fn no_final_package_never_sweeps_even_provably_abandoned_stage() {
    let root = tempfile::tempdir().unwrap();
    let stage = Paused::start(root.path(), "stage-owned", "stage");
    let name = stage
        .lines
        .lines()
        .find_map(|line| line.strip_prefix("STAGE:"))
        .unwrap()
        .to_string();
    stage.crash();
    let mut host = PluginHost::from_plugins_dir(root.path()).unwrap();
    assert!(matches!(host.remove("race"), Err(PluginError::NotFound(_))));
    assert_eq!(
        std::fs::read(root.path().join(name).join("package/bytes")).unwrap(),
        b"active owner"
    );
}

/// A host that only discovers pins nothing. Once a package operation opens
/// the root, its ancestors are pinned against rename until the host drops.
#[cfg(windows)]
#[test]
fn windows_parent_capabilities_pin_renames_once_the_root_is_open() {
    let parent = tempfile::tempdir().unwrap();
    let ancestor = parent.path().join("ancestor");
    std::fs::create_dir(&ancestor).unwrap();
    let root = ancestor.join("plugins");
    std::fs::create_dir(&root).unwrap();
    let moved = parent.path().join("moved");
    let host = PluginHost::from_plugins_dir(&root).unwrap();
    std::fs::rename(&ancestor, &moved).unwrap();
    std::fs::rename(&moved, &ancestor).unwrap();
    host.root().unwrap();
    assert!(std::fs::rename(&ancestor, &moved).is_err());
    drop(host);
    std::fs::rename(&ancestor, &moved).unwrap();
}

#[test]
fn namespace_failure_is_never_a_structural_recovery_verdict() {
    assert!(!is_structural_admission_failure(
        &PluginError::NamespaceChanged("changed generation".into())
    ));
    assert!(!is_structural_admission_failure(
        &PluginError::RecoveryRetained {
            path: "synthetic".into(),
            reason: "refused".into()
        }
    ));
}

#[test]
fn claimed_package_completed_before_delete_is_restored_healthy() {
    let root = tempfile::tempdir().unwrap();
    broken(root.path());
    let paused = Paused::start(root.path(), "before-delete", "remove");
    let claim = tests::dir_entries(root.path())
        .into_iter()
        .find(|p| p.contains("recovering-v1"))
        .unwrap();
    let package = root.path().join(&claim).join("package");
    tests::write_tool_source(&package, "race", b"\0asm completed during stage IO");
    let expected = tests::package_bytes(&package);
    let output = paused.resume();
    assert!(output.contains("UnadmittedPackage"), "{output}");
    assert_eq!(tests::package_bytes(&root.path().join("race")), expected);
    assert!(!root.path().join(claim).exists());
}

#[test]
fn directory_to_file_substitution_and_occupied_restore_preserve_both_files() {
    let root = tempfile::tempdir().unwrap();
    broken(root.path());
    let mut paused = Paused::start(root.path(), "before-claim,after-claim", "remove");
    std::fs::rename(root.path().join("race"), root.path().join("old-directory")).unwrap();
    std::fs::write(root.path().join("race"), b"substituted file").unwrap();
    paused.advance("after-claim");
    std::fs::write(root.path().join("race"), b"concurrent occupant").unwrap();
    let output = paused.resume();
    assert!(output.contains("RecoveryRetained"), "{output}");
    assert_eq!(
        std::fs::read(root.path().join("race")).unwrap(),
        b"concurrent occupant"
    );
    let claim = tests::dir_entries(root.path())
        .into_iter()
        .find(|p| p.contains("recovering-v1"))
        .unwrap();
    assert_eq!(
        std::fs::read(root.path().join(claim).join("package")).unwrap(),
        b"substituted file"
    );
    assert_eq!(
        std::fs::read(root.path().join("old-directory/manifest.toml")).unwrap(),
        b"name ="
    );
}

/// `race` installed with `wasm`, as discovery will load it.
fn installed(root: &Path, wasm: &[u8]) -> Vec<(String, Vec<u8>)> {
    std::fs::create_dir(root.join("race")).unwrap();
    tests::write_tool_source(&root.join("race"), "race", wasm);
    tests::package_bytes(&root.join("race"))
}

/// The source the `update` child replaces `race` with. Hidden, so discovery
/// never loads it.
fn update_source(root: &Path, wasm: &[u8]) -> Vec<(String, Vec<u8>)> {
    std::fs::create_dir(root.join(".source")).unwrap();
    tests::write_tool_source(&root.join(".source"), "race", wasm);
    tests::package_bytes(&root.join(".source"))
}

/// Stages and claims left in `root`: hidden entries other than the package
/// lock and the update source.
fn leftovers(root: &Path) -> Vec<String> {
    tests::dir_entries(root)
        .into_iter()
        .filter(|name| {
            name.starts_with('.') && name != ".zeroclaw-package-lock-v1" && name != ".source"
        })
        .collect()
}

#[test]
fn an_update_stopped_after_its_claim_is_put_back_by_another_process() {
    let root = tempfile::tempdir().unwrap();
    let previous = installed(root.path(), b"\0asm previous");
    update_source(root.path(), b"\0asm replacement");
    let paused = Paused::start(root.path(), "update-claimed", "update");
    assert!(!root.path().join("race").exists(), "the claim moved it");
    paused.crash();

    let mut host = PluginHost::from_plugins_dir(root.path()).unwrap();
    assert_eq!(
        host.recover_interrupted_update("race").unwrap(),
        UpdateRecovery::Restored
    );
    assert_eq!(tests::package_bytes(&root.path().join("race")), previous);
    assert!(
        leftovers(root.path()).is_empty(),
        "{:?}",
        leftovers(root.path())
    );
}

#[test]
fn an_update_stopped_after_publishing_is_finished_by_another_process() {
    let root = tempfile::tempdir().unwrap();
    installed(root.path(), b"\0asm previous");
    let replacement = update_source(root.path(), b"\0asm replacement");
    let paused = Paused::start(root.path(), "update-published", "update");
    paused.crash();

    let mut host = PluginHost::from_plugins_dir(root.path()).unwrap();
    assert_eq!(
        host.recover_interrupted_update("race").unwrap(),
        UpdateRecovery::Swept {
            removed: 1,
            kept: Vec::new()
        }
    );
    assert_eq!(tests::package_bytes(&root.path().join("race")), replacement);
    assert!(
        leftovers(root.path()).is_empty(),
        "{:?}",
        leftovers(root.path())
    );
}

/// A host built before another process's update claimed the package still
/// judges installation from the plugins directory under the lock: the claim
/// that process left when it was killed is the only copy, and it is put back.
#[test]
fn recovery_by_a_host_built_before_the_claim_puts_the_package_back() {
    let root = tempfile::tempdir().unwrap();
    let previous = installed(root.path(), b"\0asm previous");
    update_source(root.path(), b"\0asm replacement");
    let mut host = PluginHost::from_plugins_dir(root.path()).unwrap();
    assert!(
        host.get_plugin("race").is_some(),
        "premise: built before the claim"
    );
    let paused = Paused::start(root.path(), "update-claimed", "update");
    paused.crash();

    assert_eq!(
        host.recover_interrupted_update("race").unwrap(),
        UpdateRecovery::Restored
    );
    assert_eq!(tests::package_bytes(&root.path().join("race")), previous);
    assert!(
        leftovers(root.path()).is_empty(),
        "{:?}",
        leftovers(root.path())
    );
}

#[test]
fn an_update_stopped_before_its_claim_leaves_nothing_to_recover() {
    let root = tempfile::tempdir().unwrap();
    let previous = installed(root.path(), b"\0asm previous");
    update_source(root.path(), b"\0asm replacement");
    let paused = Paused::start(root.path(), "update-before-claim", "update");
    paused.crash();

    let mut host = PluginHost::from_plugins_dir(root.path()).unwrap();
    assert_eq!(
        host.recover_interrupted_update("race").unwrap(),
        UpdateRecovery::Nothing
    );
    assert_eq!(tests::package_bytes(&root.path().join("race")), previous);
    assert!(
        leftovers(root.path()).is_empty(),
        "{:?}",
        leftovers(root.path())
    );
}

/// The window an update holds the previous generation in is its own: a
/// recovery started in another process meanwhile waits for the package lock
/// instead of putting the claim back or deleting it, then finds nothing left
/// to do.
#[test]
fn recovery_waits_for_an_update_another_process_is_running() {
    let root = tempfile::tempdir().unwrap();
    let previous = installed(root.path(), b"\0asm previous");
    let replacement = update_source(root.path(), b"\0asm replacement");
    let paused = Paused::start(root.path(), "update-claimed", "update");
    let mut host = PluginHost::from_plugins_dir(root.path()).unwrap();
    assert!(host.get_plugin("race").is_none(), "the claim moved it");

    let (done, finished) = std::sync::mpsc::channel();
    let recovery = std::thread::spawn(move || {
        let result = host.recover_interrupted_update("race");
        done.send(()).unwrap();
        (result, host)
    });
    std::thread::sleep(std::time::Duration::from_millis(300));
    assert!(
        finished.try_recv().is_err(),
        "recovery must wait for the running update"
    );
    let claim = tests::dir_entries(root.path())
        .into_iter()
        .find(|name| name.starts_with(".race.replacing-v1-"))
        .expect("the running update's claim");
    assert_eq!(
        tests::package_bytes(&root.path().join(claim).join(recovery::PACKAGE)),
        previous
    );

    let output = paused.resume();
    assert!(output.contains("RESULT:Ok(\"0.1.0\")"), "{output}");
    let (result, host) = recovery.join().unwrap();
    assert_eq!(result.unwrap(), UpdateRecovery::Nothing);
    assert!(
        host.get_plugin("race").is_some(),
        "the host sees what the update installed while it waited"
    );
    assert_eq!(tests::package_bytes(&root.path().join("race")), replacement);
    assert!(
        leftovers(root.path()).is_empty(),
        "{:?}",
        leftovers(root.path())
    );
}

/// Recovery judges installation under the lock, not when its host was built.
/// A recovery waits before the lock with the package already discovered, while
/// an update in another process claims the package and is killed: the claim is
/// the only copy, and the recovery puts it back.
#[test]
fn recovery_judges_installation_after_taking_the_lock() {
    let root = tempfile::tempdir().unwrap();
    let previous = installed(root.path(), b"\0asm previous");
    update_source(root.path(), b"\0asm replacement");
    let recovering = Paused::start(root.path(), "recovery-before-lock", "recover");
    let updating = Paused::start(root.path(), "update-claimed", "update");
    assert!(!root.path().join("race").exists(), "the update claimed it");
    updating.crash();

    let output = recovering.resume();
    assert!(output.contains("RESULT:Ok(Restored)"), "{output}");
    assert_eq!(tests::package_bytes(&root.path().join("race")), previous);
    assert!(
        leftovers(root.path()).is_empty(),
        "{:?}",
        leftovers(root.path())
    );
}

/// Listing displaced packages takes the package lock, so it never holds the
/// lease of a claim a running update owns while a recovery judges it.
#[test]
fn listing_displaced_packages_waits_for_a_running_update() {
    let root = tempfile::tempdir().unwrap();
    installed(root.path(), b"\0asm previous");
    update_source(root.path(), b"\0asm replacement");
    let paused = Paused::start(root.path(), "update-claimed", "update");
    let host = PluginHost::from_plugins_dir(root.path()).unwrap();

    let (done, finished) = std::sync::mpsc::channel();
    let listing = std::thread::spawn(move || {
        let names = host.displaced_packages();
        done.send(()).unwrap();
        names
    });
    std::thread::sleep(std::time::Duration::from_millis(300));
    assert!(
        finished.try_recv().is_err(),
        "the listing must wait for the running update"
    );

    let output = paused.resume();
    assert!(output.contains("RESULT:Ok(\"0.1.0\")"), "{output}");
    assert!(listing.join().unwrap().unwrap().is_empty());
}
