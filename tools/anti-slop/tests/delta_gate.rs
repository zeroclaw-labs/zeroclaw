use std::fs;
use std::path::Path;
use std::process::{Command, Output};

fn run_git(repo: &Path, args: &[&str]) {
    let output = Command::new("git")
        .current_dir(repo)
        .args(args)
        .output()
        .expect("git should start");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn repository_with(source: &str) -> tempfile::TempDir {
    let repo = tempfile::tempdir().expect("temporary repo should be created");
    fs::create_dir(repo.path().join("src")).expect("src should be created");
    fs::write(repo.path().join("src/lib.rs"), source).expect("fixture should be written");
    run_git(repo.path(), &["init", "-q"]);
    run_git(repo.path(), &["add", "src/lib.rs"]);
    run_git(
        repo.path(),
        &[
            "-c",
            "user.name=anti-slop-test",
            "-c",
            "user.email=anti-slop@example.invalid",
            "-c",
            "core.hooksPath=/dev/null",
            "commit",
            "--no-gpg-sign",
            "-qm",
            "fixture",
        ],
    );
    repo
}

fn run_checker(repo: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_anti-slop"))
        .current_dir(repo)
        .args(["--changed-since", "HEAD", "."])
        .output()
        .expect("anti-slop should start")
}

#[test]
fn deleting_a_safety_comment_fails_the_delta_gate() {
    let repo = repository_with(
        "fn boundary() {\n    // SAFETY: the fixture owns the invariant.\n    unsafe { std::hint::unreachable_unchecked() }\n}\n",
    );
    fs::write(
        repo.path().join("src/lib.rs"),
        "fn boundary() {\n    unsafe { std::hint::unreachable_unchecked() }\n}\n",
    )
    .expect("comment should be deleted");

    let output = run_checker(repo.path());
    assert_eq!(output.status.code(), Some(1));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("require-safety-comment-for-unsafe"));
    assert!(stdout.contains("anti-slop: found 1 violation"));
}

#[test]
fn unchanged_baseline_diagnostics_do_not_block_clean_edits() {
    let repo = repository_with("fn legacy(value: Option<u8>) { let _ = value.unwrap(); }\n");
    fs::write(
        repo.path().join("src/lib.rs"),
        "fn legacy(value: Option<u8>) { let _ = value.unwrap(); }\nfn clean() {}\n",
    )
    .expect("clean function should be appended");

    let output = run_checker(repo.path());
    assert!(
        output.status.success(),
        "checker failed: {}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("no violations"));
}

#[test]
fn removing_test_only_cfg_exposes_the_existing_panic() {
    let repo = repository_with(
        "struct Fixture;\n#[cfg(test)]\nimpl Fixture {\n    fn helper(value: Option<u8>) { let _ = value.unwrap(); }\n}\n",
    );
    fs::write(
        repo.path().join("src/lib.rs"),
        "struct Fixture;\nimpl Fixture {\n    fn helper(value: Option<u8>) { let _ = value.unwrap(); }\n}\n",
    )
    .expect("test-only cfg should be deleted");

    let output = run_checker(repo.path());
    assert_eq!(output.status.code(), Some(1));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("require-invariant-comment-for-panics"));
    assert!(stdout.contains("anti-slop: found 1 violation"));
}

#[test]
fn production_file_rename_preserves_the_baseline() {
    let repo = repository_with("fn legacy(value: Option<u8>) { let _ = value.unwrap(); }\n");
    run_git(repo.path(), &["mv", "src/lib.rs", "src/moved.rs"]);

    let output = run_checker(repo.path());
    assert!(
        output.status.success(),
        "checker failed: {}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn moving_a_test_file_to_production_exposes_its_panic() {
    let repo = tempfile::tempdir().expect("temporary repo should be created");
    fs::create_dir(repo.path().join("tests")).expect("tests should be created");
    fs::write(
        repo.path().join("tests/fixture.rs"),
        "fn helper(value: Option<u8>) { let _ = value.unwrap(); }\n",
    )
    .expect("test fixture should be written");
    run_git(repo.path(), &["init", "-q"]);
    run_git(repo.path(), &["add", "tests/fixture.rs"]);
    run_git(
        repo.path(),
        &[
            "-c",
            "user.name=anti-slop-test",
            "-c",
            "user.email=anti-slop@example.invalid",
            "-c",
            "core.hooksPath=/dev/null",
            "commit",
            "--no-gpg-sign",
            "-qm",
            "fixture",
        ],
    );
    fs::create_dir(repo.path().join("src")).expect("src should be created");
    run_git(repo.path(), &["mv", "tests/fixture.rs", "src/fixture.rs"]);

    let output = run_checker(repo.path());
    assert_eq!(output.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("require-invariant-comment-for-panics")
    );
}
