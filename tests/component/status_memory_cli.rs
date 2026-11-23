use std::process::Command;

#[test]
fn status_reports_the_runtime_agents_backend_and_configured_auto_save() {
    for (settings, expected) in [
        ("", "sqlite (auto-save: on)"),
        ("[memory]\nbackend = 'sqlite'", "sqlite (auto-save: on)"),
        (
            "[memory]\nbackend = 'sqlite'\nauto_save = false\n[agents.worker.memory]\nbackend = 'markdown'",
            "markdown (auto-save: off)",
        ),
        (
            "[memory]\nbackend = 'sqlite'\n[agents.worker.memory]\nbackend = 'none'",
            "none (auto-save: on)",
        ),
        (
            "[acp]\ndefault_agent = 'secondary'\n[agents.primary.memory]\nbackend = 'markdown'\n[agents.secondary.memory]\nbackend = 'none'",
            "markdown (auto-save: on)",
        ),
    ] {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.toml"),
            format!("schema_version = 4\nlocale = 'en'\n{settings}\n"),
        )
        .unwrap();
        let output = Command::new(env!("CARGO_BIN_EXE_zeroclaw"))
            .arg("--config-dir")
            .arg(dir.path())
            .arg("status")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8(output.stdout).unwrap();
        let memory = stdout
            .lines()
            .find_map(|line| line.split_once("Memory:").map(|(_, value)| value.trim()));
        assert_eq!(memory, Some(expected), "{settings}");
    }
}
