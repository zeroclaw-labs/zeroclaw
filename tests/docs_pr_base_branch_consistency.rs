use std::fs;
use std::path::Path;

fn read_repo_file(path: &str) -> String {
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    fs::read_to_string(repo_root.join(path))
        .unwrap_or_else(|err| panic!("failed to read {path}: {err}"))
}

#[test]
fn contributor_docs_use_main_as_default_pr_base_branch() {
    let contributing = read_repo_file("CONTRIBUTING.md");
    let pr_workflow = read_repo_file("docs/pr-workflow.md");

    assert!(
        contributing
            .contains("Open a PR against `main` using the PR template (`dev` is used only when maintainers explicitly request integration batching)"),
        "CONTRIBUTING.md must set main as the default contributor PR base"
    );

    assert!(
        pr_workflow
            .contains("Normal contributor PR base is `main` by default; use `dev` only when maintainers explicitly request integration batching."),
        "docs/pr-workflow.md must match the main-first contributor PR base policy"
    );

    assert!(
        !contributing.contains("Open a PR against `dev` using the PR template"),
        "CONTRIBUTING.md still contains stale dev-first PR guidance"
    );
}
