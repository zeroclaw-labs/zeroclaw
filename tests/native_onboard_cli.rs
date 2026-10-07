#![cfg(all(feature = "agent-runtime", unix))]

use std::path::Path;
use std::process::{Command, Output};

fn command(root: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_zeroclaw"));
    command.args([
        "--config-dir",
        root.to_str().unwrap(),
        "native-onboard",
        "--client",
        "chatgpt-plan",
        "--provider-alias",
        "subscriber",
        "--agent-alias",
        "assistant",
        "--model",
        "model-fixture",
        "--risk-preset",
        "balanced",
        "--expected-billing",
        "subscription",
    ]);
    command.env("LC_ALL", "C").env("LANG", "C");
    command
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn native_onboard_cli_help_exposes_explicit_opt_ins_without_initializing_root() {
    let parent = tempfile::tempdir().unwrap();
    let root = parent.path().join("fresh");
    let output = command(&root).arg("--help").output().unwrap();
    assert!(output.status.success(), "{}", stderr(&output));
    let help = String::from_utf8_lossy(&output.stdout);
    assert!(
        help.contains("claude-code"),
        "typed native client must be advertised"
    );
    for flag in [
        "--client",
        "--provider-alias",
        "--agent-alias",
        "--model",
        "--risk-preset",
        "--expected-billing",
        "--accept-yolo",
        "--accept-api-billing",
        "--native-config-dir",
        "--auth-profile",
    ] {
        assert!(help.contains(flag), "missing {flag}");
    }
    assert!(!root.exists());
}

#[test]
fn native_onboard_cli_refuses_arbitrary_precreated_root_before_auth_or_config_load() {
    let root = tempfile::tempdir().unwrap();
    let original = b"operator-owned data";
    std::fs::write(root.path().join("keep"), original).unwrap();
    let output = command(root.path()).output().unwrap();
    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("owned fresh instance"),
        "{}",
        stderr(&output)
    );
    assert_eq!(std::fs::read(root.path().join("keep")).unwrap(), original);
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
}

#[test]
fn native_onboard_cli_refuses_root_symlink_without_writing_its_target() {
    let parent = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let root = parent.path().join("alias");
    std::os::unix::fs::symlink(outside.path(), &root).unwrap();
    let output = command(&root).output().unwrap();
    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("owned fresh instance"),
        "{}",
        stderr(&output)
    );
    assert_eq!(std::fs::read_dir(outside.path()).unwrap().count(), 0);
}

#[test]
fn native_onboard_cli_yolo_requires_fresh_acceptance_before_creating_root() {
    let parent = tempfile::tempdir().unwrap();
    let root = parent.path().join("fresh");
    let mut args = command(&root)
        .get_args()
        .map(|a| a.to_owned())
        .collect::<Vec<_>>();
    let risk = args.iter().position(|a| a == "--risk-preset").unwrap();
    args[risk + 1] = "yolo".into();
    let output = Command::new(env!("CARGO_BIN_EXE_zeroclaw"))
        .args(args)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("--accept-yolo"),
        "{}",
        stderr(&output)
    );
    assert!(!root.exists());
}

#[test]
fn native_onboard_cli_rejects_plan_api_billing_before_creating_root() {
    let parent = tempfile::tempdir().unwrap();
    let root = parent.path().join("fresh");
    let mut args = command(&root)
        .get_args()
        .map(|a| a.to_owned())
        .collect::<Vec<_>>();
    let billing = args.iter().position(|a| a == "--expected-billing").unwrap();
    args[billing + 1] = "api".into();
    args.push("--accept-api-billing".into());
    let output = Command::new(env!("CARGO_BIN_EXE_zeroclaw"))
        .args(args)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("subscription"),
        "{}",
        stderr(&output)
    );
    assert!(!root.exists());
}

#[test]
fn native_onboard_cli_accepts_claude_client_then_requires_native_account_before_root_creation() {
    let parent = tempfile::tempdir().unwrap();
    let root = parent.path().join("fresh");
    let mut args = command(&root)
        .get_args()
        .map(|a| a.to_owned())
        .collect::<Vec<_>>();
    let client = args.iter().position(|a| a == "--client").unwrap();
    args[client + 1] = "claude-code".into();
    args.push("--native-config-dir".into());
    args.push(parent.path().join("missing-account").into_os_string());
    let output = Command::new(env!("CARGO_BIN_EXE_zeroclaw"))
        .args(args)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("existing absolute native account directory"),
        "{}",
        stderr(&output)
    );
    assert!(!root.exists());
}

struct ClaudeFixture {
    directory: tempfile::TempDir,
    account: std::path::PathBuf,
    bin: std::path::PathBuf,
    argv: std::path::PathBuf,
    input: std::path::PathBuf,
}

impl ClaudeFixture {
    fn new(auth: serde_json::Value, reply: &str) -> Self {
        use std::os::unix::fs::PermissionsExt;
        let directory = tempfile::Builder::new()
            .prefix("native-claude-cli-")
            .tempdir()
            .unwrap();
        let account = directory.path().join("native account");
        let bin = directory.path().join("bin with spaces");
        std::fs::create_dir(&account).unwrap();
        std::fs::create_dir(&bin).unwrap();
        std::fs::write(
            account.join(".credentials.json"),
            b"synthetic-native-private-sentinel",
        )
        .unwrap();
        let argv = directory.path().join("argv");
        let input = directory.path().join("input");
        let auth_file = directory.path().join("auth.json");
        let result = directory.path().join("result.json");
        std::fs::write(&auth_file, serde_json::to_vec(&auth).unwrap()).unwrap();
        std::fs::write(
            &result,
            serde_json::to_vec(&serde_json::json!({
                "type":"result", "subtype":"success", "is_error":false, "result":reply,
                "usage":{"input_tokens":3,"output_tokens":2}
            }))
            .unwrap(),
        )
        .unwrap();
        let quote = |path: &Path| format!("'{}'", path.to_str().unwrap().replace('\'', "'\\''"));
        let script = format!(
            "#!/bin/sh\nset -eu\nprintf '%s\\n' \"$@\" >> {argv}\nprintf '__END__\\n' >> {argv}\nif [ \"$1\" = '--version' ]; then printf '2.1.289 (Claude Code)\\n'; exit 0; fi\nif [ \"$2\" = 'auth' ]; then if [ -f {expected} ] && [ \"${{CLAUDE_CONFIG_DIR-__NATIVE_DEFAULT__}}\" != \"$(/bin/cat {expected})\" ]; then printf '{{\"loggedIn\":false}}'; else /bin/cat {auth}; fi; exit 0; fi\nprintf '%s\\n' \"${{CLAUDE_CONFIG_DIR-__NATIVE_DEFAULT__}}\" >> {account_log}\n/bin/cat >> {input}\n/bin/cat {result}\n",
            argv = quote(&argv),
            auth = quote(&auth_file),
            input = quote(&input),
            result = quote(&result),
            account_log = quote(&directory.path().join("selected-account")),
            expected = quote(&directory.path().join("expected-account")),
        );
        std::fs::write(bin.join("claude"), script).unwrap();
        std::fs::set_permissions(bin.join("claude"), std::fs::Permissions::from_mode(0o700))
            .unwrap();
        Self {
            directory,
            account,
            bin,
            argv,
            input,
        }
    }

    fn subscription(reply: &str) -> Self {
        Self::new(
            serde_json::json!({"loggedIn":true,"authMethod":"claude.ai","apiProvider":"firstParty"}),
            reply,
        )
    }

    fn command(&self, root: &Path, billing: &str, risk: &str) -> Command {
        let mut arguments = command(root)
            .get_args()
            .map(|a| a.to_owned())
            .collect::<Vec<_>>();
        for (flag, value) in [
            ("--client", "claude-code"),
            ("--expected-billing", billing),
            ("--risk-preset", risk),
        ] {
            let index = arguments.iter().position(|a| a == flag).unwrap();
            arguments[index + 1] = value.into();
        }
        arguments.push("--native-config-dir".into());
        arguments.push(self.account.as_os_str().to_owned());
        let mut command = Command::new(env!("CARGO_BIN_EXE_zeroclaw"));
        command
            .args(arguments)
            .env_clear()
            .env("PATH", &self.bin)
            .env("HOME", self.directory.path())
            .env("LANG", "C")
            .env("LC_ALL", "C");
        command
    }

    fn receipt(root: &Path) -> serde_json::Value {
        serde_json::from_slice(&std::fs::read(root.join("native-onboard.json")).unwrap()).unwrap()
    }
}

#[test]
fn claude_native_cli_ready_uses_canonical_configuration_and_normal_agent_turn() {
    for (billing, risk) in [
        ("subscription", "balanced"),
        ("subscription", "yolo"),
        ("api", "balanced"),
    ] {
        let auth = serde_json::json!({"loggedIn":true,"authMethod":if billing == "api" {"api_key"} else {"claude.ai"},"apiProvider":"firstParty"});
        let fixture = ClaudeFixture::new(auth, "NATIVE_ONBOARD_READY");
        let root = fixture.directory.path().join("fresh");
        let mut command = fixture.command(&root, billing, risk);
        if risk == "yolo" {
            command.arg("--accept-yolo");
        }
        if billing == "api" {
            command.arg("--accept-api-billing");
        }
        let output = command.output().unwrap();
        assert!(output.status.success(), "{}", stderr(&output));
        let receipt = ClaudeFixture::receipt(&root);
        assert_eq!(receipt["phase"], "ready");
        assert_eq!(receipt["schema_version"], 1);
        assert_eq!(receipt["request"]["client"], "claude-code");
        assert_eq!(
            receipt["last_validation"]["model_provider"],
            "claude_code_native.subscriber"
        );
        assert!(
            receipt["last_validation"]["at_unix_seconds"]
                .as_u64()
                .unwrap()
                > 0
        );
        let config: zeroclaw_config::schema::Config =
            toml::from_str(&std::fs::read_to_string(root.join("config.toml")).unwrap()).unwrap();
        assert_eq!(config.providers.models.iter_entries().count(), 1);
        let native = &config.providers.models.claude_code_native["subscriber"];
        assert_eq!(native.base.model.as_deref(), Some("model-fixture"));
        assert!(native.base.api_key.is_none());
        assert!(native.base.fallback.is_empty());
        assert_eq!(
            native.claude_config_dir.as_deref(),
            fixture.account.to_str()
        );
        assert_eq!(
            config.agents["assistant"].model_provider.as_str(),
            "claude_code_native.subscriber"
        );
        assert_eq!(config.memory.embedding_provider, "none");
        assert!(config.model_routes.is_empty());
        assert!(config.reliability.api_keys.is_empty());
        assert!(!root.join("auth-profiles.json").exists());
        assert!(!root.join(".credentials.json").exists());
        let persisted = std::fs::read_to_string(root.join("config.toml")).unwrap();
        assert!(!persisted.contains("synthetic-native-private-sentinel"));
        assert_eq!(
            std::fs::read(fixture.account.join(".credentials.json")).unwrap(),
            b"synthetic-native-private-sentinel"
        );
        let argv = std::fs::read_to_string(&fixture.argv).unwrap();
        let inference = argv
            .split("__END__\n")
            .find(|call| call.contains("\n-p\n"))
            .unwrap();
        for pair in [
            "--tools\n\n",
            "--disallowedTools\n*\n",
            "--mcp-config\n{\"mcpServers\":{}}\n",
            "--max-turns\n1\n",
            "--model\nmodel-fixture\n",
        ] {
            assert!(inference.contains(pair), "missing native closure: {pair}");
        }
        assert!(inference.contains("--safe-mode\n"));
        assert!(inference.contains("--strict-mcp-config\n"));
        assert!(inference.contains("--no-session-persistence\n"));
        assert_eq!(argv.matches("\n-p\n").count(), 1);
        let input = std::fs::read_to_string(&fixture.input).unwrap();
        assert!(input.contains("NATIVE_ONBOARD_READY"));
        assert!(
            input.contains("file_read"),
            "normal agent must provide ZeroClaw tools to its sole tool loop"
        );
        let before = std::fs::read(root.join("config.toml")).unwrap();
        let output = command.output().unwrap();
        assert!(output.status.success(), "{}", stderr(&output));
        assert_eq!(
            std::fs::read(root.join("config.toml")).unwrap(),
            before,
            "owned resume must not rewrite config"
        );
    }
}

#[test]
fn claude_native_cli_refuses_bad_auth_billing_and_wrong_engine_marker() {
    for auth in [
        serde_json::json!({"loggedIn":false}),
        serde_json::json!({"loggedIn":true,"authMethod":"api_key","apiProvider":"firstParty"}),
        serde_json::json!({"loggedIn":true,"authMethod":"unknown","apiProvider":"firstParty"}),
    ] {
        let fixture = ClaudeFixture::new(auth, "NATIVE_ONBOARD_READY");
        let root = fixture.directory.path().join("fresh");
        let output = fixture
            .command(&root, "subscription", "balanced")
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(
            !root.join("config.toml").exists(),
            "native auth/billing refusal must precede persistence"
        );
        assert!(
            stderr(&output).contains("claude auth login"),
            "{}",
            stderr(&output)
        );
        assert_eq!(ClaudeFixture::receipt(&root)["phase"], "failed");
        assert!(!fixture.input.exists());
        assert!(!root.join("auth-profiles.json").exists());
    }
    let fixture = ClaudeFixture::subscription("WRONG_MARKER");
    let root = fixture.directory.path().join("fresh");
    let output = fixture
        .command(&root, "subscription", "balanced")
        .output()
        .unwrap();
    assert!(!output.status.success());
    let receipt = ClaudeFixture::receipt(&root);
    assert_eq!(receipt["phase"], "failed");
    assert!(receipt["last_validation"].is_null());
    assert!(root.join("config.toml").is_file());
    assert!(!stderr(&output).contains("WRONG_MARKER"));
}

#[test]
fn claude_native_cli_requires_consents_and_existing_absolute_account_before_root() {
    for (billing, risk) in [("api", "balanced"), ("subscription", "yolo")] {
        let fixture = ClaudeFixture::subscription("NATIVE_ONBOARD_READY");
        let root = fixture.directory.path().join("fresh");
        let output = fixture.command(&root, billing, risk).output().unwrap();
        assert!(!output.status.success());
        assert!(!root.exists());
        assert!(!fixture.argv.exists());
    }
    for account in [
        "native account".to_string(),
        "/missing-native-account-fixture".to_string(),
    ] {
        let fixture = ClaudeFixture::subscription("NATIVE_ONBOARD_READY");
        let root = fixture.directory.path().join("fresh");
        let mut args = fixture
            .command(&root, "subscription", "balanced")
            .get_args()
            .map(|a| a.to_owned())
            .collect::<Vec<_>>();
        let index = args
            .iter()
            .position(|a| a == "--native-config-dir")
            .unwrap();
        args[index + 1] = account.into();
        let output = Command::new(env!("CARGO_BIN_EXE_zeroclaw"))
            .args(args)
            .current_dir(fixture.directory.path())
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(!root.exists());
    }
}

#[test]
fn claude_native_cli_binds_inherited_or_default_account_and_preserves_foreign_root() {
    for inherited in [false, true] {
        let fixture = ClaudeFixture::subscription("NATIVE_ONBOARD_READY");
        let root = fixture.directory.path().join("fresh");
        std::os::unix::fs::symlink(&fixture.account, fixture.directory.path().join(".claude"))
            .unwrap();
        let selected = fixture.command(&root, "subscription", "balanced");
        let mut args = selected
            .get_args()
            .map(|a| a.to_owned())
            .collect::<Vec<_>>();
        let index = args
            .iter()
            .position(|a| a == "--native-config-dir")
            .unwrap();
        args.drain(index..index + 2);
        let mut command = Command::new(env!("CARGO_BIN_EXE_zeroclaw"));
        command
            .args(args)
            .env_clear()
            .env("PATH", &fixture.bin)
            .env("HOME", fixture.directory.path())
            .env("LANG", "C");
        if inherited {
            command.env("CLAUDE_CONFIG_DIR", &fixture.account);
        }
        let output = command.output().unwrap();
        assert!(output.status.success(), "{}", stderr(&output));
        let selected = ClaudeFixture::receipt(&root)["request"]["native_config_dir"].clone();
        if inherited {
            assert_eq!(selected, fixture.account.to_str().unwrap());
        } else {
            assert!(selected.is_null());
        }
    }
    let fixture = ClaudeFixture::subscription("NATIVE_ONBOARD_READY");
    let root = fixture.directory.path().join("foreign");
    std::fs::create_dir(&root).unwrap();
    std::fs::write(root.join("keep"), b"operator-owned").unwrap();
    let output = fixture
        .command(&root, "subscription", "balanced")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert_eq!(std::fs::read(root.join("keep")).unwrap(), b"operator-owned");
    assert_eq!(std::fs::read_dir(&root).unwrap().count(), 1);
    assert!(!fixture.argv.exists());
}

#[test]
fn claude_native_cli_refuses_account_overlap_before_root_creation() {
    let fixture = ClaudeFixture::subscription("NATIVE_ONBOARD_READY");
    let root = fixture.account.join("fresh-instance");
    let output = fixture
        .command(&root, "subscription", "balanced")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(stderr(&output).contains("overlap"), "{}", stderr(&output));
    assert!(!root.exists());
    assert!(!fixture.argv.exists());
    assert_eq!(
        std::fs::read(fixture.account.join(".credentials.json")).unwrap(),
        b"synthetic-native-private-sentinel"
    );
}

#[test]
fn claude_native_cli_refuses_irrelevant_auth_profile_before_root_creation() {
    let fixture = ClaudeFixture::subscription("NATIVE_ONBOARD_READY");
    let root = fixture.directory.path().join("fresh");
    let output = fixture
        .command(&root, "subscription", "balanced")
        .args(["--auth-profile", "other_profile"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("--auth-profile"),
        "{}",
        stderr(&output)
    );
    assert!(!root.exists());
    assert!(!fixture.argv.exists());
}

#[test]
fn claude_native_cli_preserves_native_default_authentication_selector() {
    let fixture = ClaudeFixture::subscription("NATIVE_ONBOARD_READY");
    std::os::unix::fs::symlink(&fixture.account, fixture.directory.path().join(".claude")).unwrap();
    std::fs::write(
        fixture.directory.path().join("expected-account"),
        "__NATIVE_DEFAULT__",
    )
    .unwrap();
    let root = fixture.directory.path().join("fresh");
    let selected = fixture.command(&root, "subscription", "balanced");
    let mut args = selected
        .get_args()
        .map(|a| a.to_owned())
        .collect::<Vec<_>>();
    let index = args
        .iter()
        .position(|a| a == "--native-config-dir")
        .unwrap();
    args.drain(index..index + 2);
    let output = Command::new(env!("CARGO_BIN_EXE_zeroclaw"))
        .args(args)
        .env_clear()
        .env("PATH", &fixture.bin)
        .env("HOME", fixture.directory.path())
        .env("LANG", "C")
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(ClaudeFixture::receipt(&root)["request"]["native_config_dir"].is_null());
    let config: zeroclaw_config::schema::Config =
        toml::from_str(&std::fs::read_to_string(root.join("config.toml")).unwrap()).unwrap();
    assert!(
        config.providers.models.claude_code_native["subscriber"]
            .claude_config_dir
            .is_none()
    );
    assert_eq!(
        std::fs::read_to_string(fixture.directory.path().join("selected-account"))
            .unwrap()
            .trim(),
        "__NATIVE_DEFAULT__"
    );
}

#[test]
fn claude_native_cli_preserves_explicit_and_inherited_literal_symlink_selector() {
    for inherited in [false, true] {
        let fixture = ClaudeFixture::subscription("NATIVE_ONBOARD_READY");
        let alias = fixture.directory.path().join("native-account-alias");
        std::os::unix::fs::symlink(&fixture.account, &alias).unwrap();
        std::fs::write(
            fixture.directory.path().join("expected-account"),
            alias.to_str().unwrap(),
        )
        .unwrap();
        let root = fixture.directory.path().join("fresh");
        let selected = fixture.command(&root, "subscription", "balanced");
        let mut args = selected
            .get_args()
            .map(|a| a.to_owned())
            .collect::<Vec<_>>();
        let index = args
            .iter()
            .position(|a| a == "--native-config-dir")
            .unwrap();
        if inherited {
            args.drain(index..index + 2);
        } else {
            args[index + 1] = alias.as_os_str().to_owned();
        }
        let mut command = Command::new(env!("CARGO_BIN_EXE_zeroclaw"));
        command
            .args(args)
            .env_clear()
            .env("PATH", &fixture.bin)
            .env("HOME", fixture.directory.path())
            .env("LANG", "C");
        if inherited {
            command.env("CLAUDE_CONFIG_DIR", &alias);
        }
        let output = command.output().unwrap();
        assert!(output.status.success(), "{}", stderr(&output));
        assert_eq!(
            ClaudeFixture::receipt(&root)["request"]["native_config_dir"],
            alias.to_str().unwrap()
        );
        assert_eq!(
            std::fs::read_to_string(fixture.directory.path().join("selected-account"))
                .unwrap()
                .trim(),
            alias.to_str().unwrap()
        );
    }
}
