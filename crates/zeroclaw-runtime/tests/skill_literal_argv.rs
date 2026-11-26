//! Offline draft contracts. Only the `prototype_*` tests use the candidate binder;
//! the other tests exercise the actual manifest audit, loader and tool factory.

#[path = "skill_literal_argv/binding.rs"]
mod binding;

use serde_json::json;
use std::collections::HashMap;
use std::fs;
use std::path::Path;
use zeroclaw_runtime::skills::{audit::audit_skill_directory, load_skills_from_directory};

fn write_manifest(root: &Path, kind: &str, declaration: &str) {
    let skill = root.join("sample");
    fs::create_dir_all(&skill).unwrap();
    fs::write(
        skill.join("SKILL.toml"),
        format!(
            "[skill]\nname = 'sample'\ndescription = 'Test skill'\n\n[[tools]]\nname = 'record'\ndescription = 'Record arguments'\nkind = '{kind}'\n{declaration}\n[tools.args]\ntext = 'Text to record'\nlabel = 'Label'\n"
        ),
    )
    .unwrap();
}

#[test]
fn prototype_rejects_invalid_templates_and_non_string_input() {
    let declared = HashMap::from([("text".into(), "Text".into())]);
    let bad_input = "expected one string per declared argument";
    for (raw, input, reason) in [
        (vec![], json!({"text":"ok"}), "missing program"),
        (vec![" "], json!({"text":"ok"}), "expected a fixed program"),
        (
            vec!["{{text}}"],
            json!({"text":"ok"}),
            "expected a fixed program",
        ),
        (
            vec!["recorder", "{{other}}"],
            json!({"text":"ok"}),
            "unknown placeholder",
        ),
        (
            vec!["recorder", "{{text"],
            json!({"text":"ok"}),
            "unclosed placeholder",
        ),
        (
            vec!["recorder", "text}}"],
            json!({"text":"ok"}),
            "unexpected closing delimiter",
        ),
        (
            vec!["recorder", "{{a{{b}}"],
            json!({"text":"ok"}),
            "invalid placeholder",
        ),
        (vec!["recorder", "{{text}}"], json!({}), bad_input),
        (
            vec!["recorder", "{{text}}"],
            json!({"text":true}),
            bad_input,
        ),
        (
            vec!["recorder", "{{text}}"],
            json!({"text":"a\0b"}),
            bad_input,
        ),
        (
            vec!["recorder", "{{text}}"],
            json!({"text":"ok", "extra":"value"}),
            bad_input,
        ),
    ] {
        let template = raw.into_iter().map(String::from).collect::<Vec<_>>();
        assert_eq!(
            binding::bind(&template, &declared, &input).unwrap_err(),
            reason
        );
    }
}

#[test]
fn manifest_keeps_legacy_command_and_rejects_ambiguous_forms() {
    for (declaration, valid) in [
        ("command = 'echo ready'", true),
        ("argv = ['recorder', '--text={{text}}']", true),
        ("command = 'echo ready'\nargv = ['recorder']", false),
        ("argv = []", false),
        ("argv = ['{{text}}']", false),
        ("argv = ['recorder', 1]", false),
        ("argv = ['recorder', '{{undeclared}}']", false),
    ] {
        let root = tempfile::tempdir().unwrap();
        write_manifest(root.path(), "shell", declaration);
        let audit = audit_skill_directory(&root.path().join("sample")).unwrap();
        assert_eq!(audit.is_clean(), valid, "audit: {declaration}");
        let (skills, dropped) = load_skills_from_directory(root.path(), false);
        assert_eq!(
            (skills.len(), dropped.len()),
            if valid { (1, 0) } else { (0, 1) },
            "load: {declaration}"
        );
    }
}

#[cfg(unix)]
mod unix {
    use super::*;
    use serde_json::Value;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;
    use std::process::Command;
    use std::sync::Arc;
    use zeroclaw_api::tool::Tool;
    use zeroclaw_runtime::platform::{NativeRuntime, RuntimeAdapter, ShellDialect};
    use zeroclaw_runtime::security::SecurityPolicy;
    use zeroclaw_runtime::skills::skills_to_tools_with_context_and_runtime;

    const TEXT: &str = "  Don't split: 雪; $5, `label`\n{{label}}  ";

    fn recorder(root: &Path, name: &str) -> PathBuf {
        let executable = root.join(name);
        fs::write(
            &executable,
            "#!/bin/sh\n: > \"${0%/*}/called\"\nfor arg do printf '%s\\000' \"$arg\"; done\n",
        )
        .unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
        executable
    }

    fn template(program: &Path) -> Vec<String> {
        [
            program.to_str().unwrap(),
            "--text={{text}}",
            "{{label}}",
            "",
        ]
        .map(String::from)
        .into()
    }

    fn input() -> Value {
        json!({"text": TEXT, "label": "--flag-looking"})
    }

    fn expected() -> String {
        format!("--text={TEXT}\0--flag-looking\0\0")
    }

    #[test]
    fn prototype_transports_each_element_to_the_disposable_cli() {
        let root = tempfile::tempdir().unwrap();
        let executable = recorder(root.path(), "recorder");
        let template = template(&executable);
        let declared = HashMap::from([
            ("text".into(), "Text".into()),
            ("label".into(), "Label".into()),
        ]);
        let (program, argv) = binding::bind(&template, &declared, &input()).unwrap();
        assert_eq!(Path::new(program), executable);
        // This is fixture invocation, not a proposed skill executor. Only launch
        // the test-created program; do not take the executable from a tool input.
        let out = Command::new(executable)
            .args(argv)
            .env_clear()
            .current_dir(root.path())
            .output()
            .unwrap();
        assert!(out.status.success());
        assert_eq!(out.stdout, expected().as_bytes());
    }

    fn assemble(
        root: &Path,
        kind: &str,
        argv: &[String],
        allowed_program: &str,
        runtime: Arc<dyn RuntimeAdapter>,
    ) -> Vec<Box<dyn Tool>> {
        let skill_root = root.join("skills");
        let array = toml::Value::Array(argv.iter().cloned().map(toml::Value::String).collect());
        write_manifest(&skill_root, kind, &format!("argv = {array}"));
        let audit = audit_skill_directory(&skill_root.join("sample")).unwrap();
        assert!(audit.is_clean(), "install audit: {}", audit.summary());
        let (skills, dropped) = load_skills_from_directory(&skill_root, false);
        assert!(dropped.is_empty(), "load: {dropped:?}");
        let policy = Arc::new(SecurityPolicy {
            workspace_dir: root.to_owned(),
            allowed_commands: vec![allowed_program.to_owned()],
            ..SecurityPolicy::default()
        });
        skills_to_tools_with_context_and_runtime(&skills, policy, &[], runtime, &[])
    }

    fn registered(root: &Path, kind: &str, argv: &[String]) -> Box<dyn Tool> {
        let mut tools = assemble(root, kind, argv, &argv[0], Arc::new(NativeRuntime::new()));
        assert_eq!(tools.len(), 1);
        let tool = tools.pop().unwrap();
        assert_eq!(tool.name(), "sample__record");
        tool
    }

    #[tokio::test]
    async fn registered_argv_tool_delivers_exact_bytes() {
        for kind in ["shell", "script"] {
            let root = tempfile::tempdir().unwrap();
            let argv = template(&recorder(root.path(), "recorder"));
            let tool = registered(root.path(), kind, &argv);
            let result = tool.execute(input()).await.unwrap();
            assert!(result.success, "{:?}", result.error);
            assert_eq!(result.output.as_bytes(), expected().as_bytes());
        }
    }

    #[tokio::test]
    async fn denied_executable_never_starts() {
        for kind in ["shell", "script"] {
            let root = tempfile::tempdir().unwrap();
            let argv = template(&recorder(root.path(), "recorder"));
            let marker = root.path().join("called");
            for tool in assemble(
                root.path(),
                kind,
                &argv,
                "other-recorder",
                Arc::new(NativeRuntime::new()),
            ) {
                let result = tool.execute(input()).await;
                assert!(!result.is_ok_and(|out| out.success));
            }
            assert!(!marker.try_exists().unwrap(), "denied recorder started");

            // Keep the manifest, executable, input and workspace fixed. Change
            // only the allowlist: an unusable fixture cannot pass the denial.
            let control = registered(root.path(), kind, &argv)
                .execute(input())
                .await
                .unwrap();
            assert!(control.success, "{:?}", control.error);
            assert!(
                marker.try_exists().unwrap(),
                "allowed recorder never started"
            );
            assert_eq!(control.output.as_bytes(), expected().as_bytes());
        }
    }

    #[derive(Default)]
    struct ShellOnly(std::sync::atomic::AtomicBool);

    impl RuntimeAdapter for ShellOnly {
        fn name(&self) -> &str {
            "fixture-shell-only"
        }
        fn shell_dialect(&self) -> ShellDialect {
            ShellDialect::Posix
        }
        fn has_filesystem_access(&self) -> bool {
            true
        }
        fn supports_long_running(&self) -> bool {
            false
        }
        fn storage_path(&self) -> PathBuf {
            PathBuf::new()
        }
        fn build_shell_command(
            &self,
            _: &str,
            _: &Path,
        ) -> anyhow::Result<tokio::process::Command> {
            self.0.store(true, std::sync::atomic::Ordering::SeqCst);
            anyhow::bail!("unexpected shell construction")
        }
    }

    #[tokio::test]
    async fn unsupported_runtime_neither_builds_a_shell_nor_launches_on_host() {
        let root = tempfile::tempdir().unwrap();
        let argv = template(&recorder(root.path(), "recorder"));
        // Either decline registration or refuse at call time. First verify this
        // exact manifest registers under the same policy on a supported runtime.
        let _control = registered(root.path(), "shell", &argv);
        let runtime = Arc::new(ShellOnly::default());
        for tool in assemble(root.path(), "shell", &argv, &argv[0], runtime.clone()) {
            let result = tool.execute(input()).await;
            assert!(!result.is_ok_and(|out| out.success));
        }
        assert!(!runtime.0.load(std::sync::atomic::Ordering::SeqCst));
        assert!(!root.path().join("called").exists());
    }

    #[tokio::test]
    async fn allowlisted_program_still_subject_to_argument_policy() {
        let root = tempfile::tempdir().unwrap();
        // The fake CLI has no git behavior. `config` is a denied argument
        // for git under the existing policy, even if the executable is listed.
        let executable = recorder(root.path(), "git");
        let argv = vec![executable.to_str().unwrap().to_owned(), "{{text}}".into()];
        let tool = registered(root.path(), "shell", &argv);
        let call = |text| json!({"text":text,"label":""});
        for blocked in ["config", "../outside-fixture"] {
            let result = tool.execute(call(blocked)).await;
            assert!(!result.is_ok_and(|out| out.success), "allowed: {blocked}");
            assert!(!root.path().join("called").exists());
        }
        let control = tool.execute(call("status")).await.unwrap();
        assert!(control.success, "{:?}", control.error);
        assert_eq!(control.output.as_bytes(), b"status\0");
    }
}
