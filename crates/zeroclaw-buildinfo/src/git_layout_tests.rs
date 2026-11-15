use super::*;
use std::fs;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);

struct Repository {
    root: PathBuf,
    checkout: PathBuf,
}

impl Repository {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "zeroclaw-buildinfo-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
        ));
        let checkout = root.join("checkout");
        fs::create_dir_all(&checkout).unwrap();
        let repository = Self { root, checkout };
        repository.run(
            &repository.checkout,
            &["init", "-q", "-b", "fixture", "--template="],
        );
        repository.run(
            &repository.checkout,
            &["commit", "-qm", "fixture", "--allow-empty"],
        );
        repository
    }

    fn run(&self, dir: &Path, args: &[&str]) {
        let output = Command::new("git")
            .args([
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "-c",
                "commit.gpgSign=false",
                "-c",
                "core.hooksPath=",
            ])
            .args(args)
            .current_dir(dir)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", self.root.join("no-global-config"))
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

impl Drop for Repository {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn linked_worktree_watches_its_own_head_and_shared_branch_ref() {
    let repository = Repository::new();
    let linked = repository.root.join("linked");
    repository.run(
        &repository.checkout,
        &["worktree", "add", "-qb", "linked", linked.to_str().unwrap()],
    );
    assert!(linked.join(".git").is_file());
    let head = git_path(&linked, "HEAD").unwrap();
    let branch = git_path(&linked, "refs/heads/linked").unwrap();
    let watched = watched_refs(&linked);
    assert!(watched.contains(&head));
    assert!(watched.contains(&branch));
    assert_ne!(head, git_path(&repository.checkout, "HEAD").unwrap());
    let previous_ref = fs::read(&branch).unwrap();
    let previous_id = build_id(&linked);
    repository.run(
        &linked,
        &["commit", "-qm", "advance linked", "--allow-empty"],
    );
    assert_ne!(fs::read(&branch).unwrap(), previous_ref);
    assert_ne!(build_id(&linked), previous_id);
    assert_eq!(build_id(&repository.checkout), previous_id);
    assert!(
        instructions(&linked, "1.2.3", &build_id(&linked))
            .contains(&format!("cargo:rerun-if-changed={}", branch.display()))
    );
}

#[test]
fn packed_branch_ref_remains_watched_and_can_become_loose_again() {
    let repository = Repository::new();
    repository.run(&repository.checkout, &["pack-refs", "--all", "--prune"]);
    let packed = git_path(&repository.checkout, "packed-refs").unwrap();
    let branch = git_path(&repository.checkout, "refs/heads/fixture").unwrap();
    assert!(!branch.exists());
    assert!(watched_refs(&repository.checkout).contains(&packed));
    let previous_id = build_id(&repository.checkout);
    repository.run(
        &repository.checkout,
        &["commit", "-qm", "advance packed", "--allow-empty"],
    );
    assert!(branch.is_file());
    assert!(watched_refs(&repository.checkout).contains(&branch));
    assert_ne!(build_id(&repository.checkout), previous_id);
}

#[test]
fn detached_head_is_watched_when_a_commit_advances_it() {
    let repository = Repository::new();
    repository.run(&repository.checkout, &["checkout", "-q", "--detach"]);
    let head = git_path(&repository.checkout, "HEAD").unwrap();
    assert!(watched_refs(&repository.checkout).contains(&head));
    let previous_ref = fs::read(&head).unwrap();
    let previous_id = build_id(&repository.checkout);
    repository.run(
        &repository.checkout,
        &["commit", "-qm", "advance detached", "--allow-empty"],
    );
    assert_ne!(fs::read(&head).unwrap(), previous_ref);
    assert_ne!(build_id(&repository.checkout), previous_id);
}
