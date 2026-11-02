//! Check the composition contract using the repository's documentation gates.
use std::path::Path;
use std::process::Command;

fn main() {
    assert_eq!(std::env::args().skip(1).collect::<Vec<_>>(), ["--check"]);
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(3)
        .unwrap();
    for script in [
        "scripts/ci/docs_quality_gate.sh",
        "scripts/ci/docs_links_gate.sh",
    ] {
        let status = Command::new("bash")
            .current_dir(root)
            .env(
                "DOCS_FILES",
                "docs/book/src/architecture/runtime-composition.md",
            )
            .env("BASE_SHA", "")
            .args([script])
            .status()
            .expect("documentation gate must start");
        assert!(
            status.success(),
            "documentation gate failed: {script}: {status}"
        );
    }
}
