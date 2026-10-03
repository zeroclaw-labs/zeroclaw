//! The splash page's script, run under Node.js with controlled Tauri answers
//! (`splash_startup.cjs`): it opens the dashboard only once the backend
//! reports a ready startup, and a failure stops it for good.

use std::path::Path;
use std::process::Command;

#[test]
fn splash_opens_the_dashboard_only_after_a_ready_startup() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let output = match Command::new("node")
        .arg(root.join("tests").join("splash_startup.cjs"))
        .arg(root.join("splash").join("index.html"))
        .output()
    {
        Ok(output) => output,
        // CI runners always have Node.js; a developer machine may not.
        Err(error)
            if error.kind() == std::io::ErrorKind::NotFound && std::env::var_os("CI").is_none() =>
        {
            eprintln!("skipped: Node.js is not installed");
            return;
        }
        Err(error) => panic!("run the splash checks with node: {error}"),
    };
    let report = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.status.success(), "splash checks failed:\n{report}");
    let passed = report
        .lines()
        .filter(|line| line.starts_with("ok - "))
        .count();
    assert_eq!(passed, 4, "{report}");
}
