use super::scaffold_workspace;
use crate::onboard::common::ProjectContext;
use std::fs;
use tempfile::TempDir;

#[test]
fn scaffold_creates_all_md_files() {
    let tmp = TempDir::new().unwrap();
    let ctx = ProjectContext::default();
    scaffold_workspace(tmp.path(), &ctx).unwrap();

    let expected = [
        "IDENTITY.md",
        "AGENTS.md",
        "HEARTBEAT.md",
        "SOUL.md",
        "USER.md",
        "TOOLS.md",
        "BOOTSTRAP.md",
        "MEMORY.md",
    ];
    for f in &expected {
        assert!(tmp.path().join(f).exists(), "missing file: {f}");
    }
}

#[test]
fn scaffold_creates_all_subdirectories() {
    let tmp = TempDir::new().unwrap();
    let ctx = ProjectContext::default();
    scaffold_workspace(tmp.path(), &ctx).unwrap();

    for dir in &["sessions", "memory", "state", "cron", "skills"] {
        assert!(tmp.path().join(dir).is_dir(), "missing subdirectory: {dir}");
    }
}

#[test]
fn scaffold_bakes_user_name_into_files() {
    let tmp = TempDir::new().unwrap();
    let ctx = ProjectContext {
        user_name: "Alice".into(),
        ..Default::default()
    };
    scaffold_workspace(tmp.path(), &ctx).unwrap();

    let user_md = fs::read_to_string(tmp.path().join("USER.md")).unwrap();
    assert!(
        user_md.contains("**Name:** Alice"),
        "USER.md should contain user name"
    );

    let bootstrap = fs::read_to_string(tmp.path().join("BOOTSTRAP.md")).unwrap();
    assert!(
        bootstrap.contains("**Alice**"),
        "BOOTSTRAP.md should contain user name"
    );
}

#[test]
fn scaffold_bakes_timezone_into_files() {
    let tmp = TempDir::new().unwrap();
    let ctx = ProjectContext {
        timezone: "US/Pacific".into(),
        ..Default::default()
    };
    scaffold_workspace(tmp.path(), &ctx).unwrap();

    let user_md = fs::read_to_string(tmp.path().join("USER.md")).unwrap();
    assert!(
        user_md.contains("**Timezone:** US/Pacific"),
        "USER.md should contain timezone"
    );

    let bootstrap = fs::read_to_string(tmp.path().join("BOOTSTRAP.md")).unwrap();
    assert!(
        bootstrap.contains("US/Pacific"),
        "BOOTSTRAP.md should contain timezone"
    );
}

#[test]
fn scaffold_bakes_agent_name_into_files() {
    let tmp = TempDir::new().unwrap();
    let ctx = ProjectContext {
        agent_name: "Crabby".into(),
        ..Default::default()
    };
    scaffold_workspace(tmp.path(), &ctx).unwrap();

    let identity = fs::read_to_string(tmp.path().join("IDENTITY.md")).unwrap();
    assert!(
        identity.contains("**Name:** Crabby"),
        "IDENTITY.md should contain agent name"
    );

    let soul = fs::read_to_string(tmp.path().join("SOUL.md")).unwrap();
    assert!(
        soul.contains("You are **Crabby**"),
        "SOUL.md should contain agent name"
    );

    let agents = fs::read_to_string(tmp.path().join("AGENTS.md")).unwrap();
    assert!(
        agents.contains("Crabby Personal Assistant"),
        "AGENTS.md should contain agent name"
    );

    let heartbeat = fs::read_to_string(tmp.path().join("HEARTBEAT.md")).unwrap();
    assert!(
        heartbeat.contains("Crabby"),
        "HEARTBEAT.md should contain agent name"
    );

    let bootstrap = fs::read_to_string(tmp.path().join("BOOTSTRAP.md")).unwrap();
    assert!(
        bootstrap.contains("Introduce yourself as Crabby"),
        "BOOTSTRAP.md should contain agent name"
    );
}

#[test]
fn scaffold_bakes_communication_style() {
    let tmp = TempDir::new().unwrap();
    let ctx = ProjectContext {
        communication_style: "Be technical and detailed.".into(),
        ..Default::default()
    };
    scaffold_workspace(tmp.path(), &ctx).unwrap();

    let soul = fs::read_to_string(tmp.path().join("SOUL.md")).unwrap();
    assert!(
        soul.contains("Be technical and detailed."),
        "SOUL.md should contain communication style"
    );

    let user_md = fs::read_to_string(tmp.path().join("USER.md")).unwrap();
    assert!(
        user_md.contains("Be technical and detailed."),
        "USER.md should contain communication style"
    );

    let bootstrap = fs::read_to_string(tmp.path().join("BOOTSTRAP.md")).unwrap();
    assert!(
        bootstrap.contains("Be technical and detailed."),
        "BOOTSTRAP.md should contain communication style"
    );
}

#[test]
fn scaffold_uses_defaults_for_empty_context() {
    let tmp = TempDir::new().unwrap();
    let ctx = ProjectContext::default();
    scaffold_workspace(tmp.path(), &ctx).unwrap();

    let identity = fs::read_to_string(tmp.path().join("IDENTITY.md")).unwrap();
    assert!(
        identity.contains("**Name:** ZeroClaw"),
        "should default agent name to ZeroClaw"
    );

    let user_md = fs::read_to_string(tmp.path().join("USER.md")).unwrap();
    assert!(
        user_md.contains("**Name:** User"),
        "should default user name to User"
    );
    assert!(
        user_md.contains("**Timezone:** UTC"),
        "should default timezone to UTC"
    );

    let soul = fs::read_to_string(tmp.path().join("SOUL.md")).unwrap();
    assert!(
        soul.contains("Be warm, natural, and clear."),
        "should default communication style"
    );
}

#[test]
fn scaffold_does_not_overwrite_existing_files() {
    let tmp = TempDir::new().unwrap();
    let ctx = ProjectContext {
        user_name: "Bob".into(),
        ..Default::default()
    };

    let soul_path = tmp.path().join("SOUL.md");
    fs::write(&soul_path, "# My Custom Soul\nDo not overwrite me.").unwrap();

    scaffold_workspace(tmp.path(), &ctx).unwrap();

    let soul = fs::read_to_string(&soul_path).unwrap();
    assert!(
        soul.contains("Do not overwrite me"),
        "existing files should not be overwritten"
    );
    assert!(
        !soul.contains("You're not a chatbot"),
        "should not contain scaffold content"
    );

    let user_md = fs::read_to_string(tmp.path().join("USER.md")).unwrap();
    assert!(user_md.contains("**Name:** Bob"));
}

#[test]
fn scaffold_is_idempotent() {
    let tmp = TempDir::new().unwrap();
    let ctx = ProjectContext {
        user_name: "Eve".into(),
        agent_name: "Claw".into(),
        ..Default::default()
    };

    scaffold_workspace(tmp.path(), &ctx).unwrap();
    let soul_v1 = fs::read_to_string(tmp.path().join("SOUL.md")).unwrap();

    scaffold_workspace(tmp.path(), &ctx).unwrap();
    let soul_v2 = fs::read_to_string(tmp.path().join("SOUL.md")).unwrap();

    assert_eq!(soul_v1, soul_v2, "scaffold should be idempotent");
}

#[test]
fn scaffold_files_are_non_empty() {
    let tmp = TempDir::new().unwrap();
    let ctx = ProjectContext::default();
    scaffold_workspace(tmp.path(), &ctx).unwrap();

    for f in &[
        "IDENTITY.md",
        "AGENTS.md",
        "HEARTBEAT.md",
        "SOUL.md",
        "USER.md",
        "TOOLS.md",
        "BOOTSTRAP.md",
        "MEMORY.md",
    ] {
        let content = fs::read_to_string(tmp.path().join(f)).unwrap();
        assert!(!content.trim().is_empty(), "{f} should not be empty");
    }
}

#[test]
fn agents_md_references_on_demand_memory() {
    let tmp = TempDir::new().unwrap();
    let ctx = ProjectContext::default();
    scaffold_workspace(tmp.path(), &ctx).unwrap();

    let agents = fs::read_to_string(tmp.path().join("AGENTS.md")).unwrap();
    assert!(
        agents.contains("memory_recall"),
        "AGENTS.md should reference memory_recall for on-demand access"
    );
    assert!(
        agents.contains("on-demand"),
        "AGENTS.md should mention daily notes are on-demand"
    );
}

#[test]
fn memory_md_warns_about_token_cost() {
    let tmp = TempDir::new().unwrap();
    let ctx = ProjectContext::default();
    scaffold_workspace(tmp.path(), &ctx).unwrap();

    let memory = fs::read_to_string(tmp.path().join("MEMORY.md")).unwrap();
    assert!(
        memory.contains("costs tokens"),
        "MEMORY.md should warn about token cost"
    );
    assert!(
        memory.contains("auto-injected"),
        "MEMORY.md should mention it's auto-injected"
    );
}

#[test]
fn tools_md_lists_all_builtin_tools() {
    let tmp = TempDir::new().unwrap();
    let ctx = ProjectContext::default();
    scaffold_workspace(tmp.path(), &ctx).unwrap();

    let tools = fs::read_to_string(tmp.path().join("TOOLS.md")).unwrap();
    for tool in &[
        "shell",
        "file_read",
        "file_write",
        "memory_store",
        "memory_recall",
        "memory_forget",
    ] {
        assert!(
            tools.contains(tool),
            "TOOLS.md should list built-in tool: {tool}"
        );
    }
    assert!(
        tools.contains("Use when:"),
        "TOOLS.md should include 'Use when' guidance"
    );
    assert!(
        tools.contains("Don't use when:"),
        "TOOLS.md should include 'Don't use when' guidance"
    );
}

#[test]
fn soul_md_includes_emoji_awareness_guidance() {
    let tmp = TempDir::new().unwrap();
    let ctx = ProjectContext::default();
    scaffold_workspace(tmp.path(), &ctx).unwrap();

    let soul = fs::read_to_string(tmp.path().join("SOUL.md")).unwrap();
    assert!(
        soul.contains("Use emojis naturally (0-2 max"),
        "SOUL.md should include emoji usage guidance"
    );
    assert!(
        soul.contains("Match emoji density to the user"),
        "SOUL.md should include emoji-awareness guidance"
    );
}

#[test]
fn scaffold_handles_special_characters_in_names() {
    let tmp = TempDir::new().unwrap();
    let ctx = ProjectContext {
        user_name: "José María".into(),
        agent_name: "ZeroClaw-v2".into(),
        timezone: "Europe/Madrid".into(),
        communication_style: "Be direct.".into(),
    };
    scaffold_workspace(tmp.path(), &ctx).unwrap();

    let user_md = fs::read_to_string(tmp.path().join("USER.md")).unwrap();
    assert!(user_md.contains("José María"));

    let soul = fs::read_to_string(tmp.path().join("SOUL.md")).unwrap();
    assert!(soul.contains("ZeroClaw-v2"));
}

#[test]
fn scaffold_full_personalization() {
    let tmp = TempDir::new().unwrap();
    let ctx = ProjectContext {
        user_name: "Argenis".into(),
        timezone: "US/Eastern".into(),
        agent_name: "Claw".into(),
        communication_style:
            "Be friendly, human, and conversational. Show warmth and empathy while staying efficient. Use natural contractions."
                .into(),
    };
    scaffold_workspace(tmp.path(), &ctx).unwrap();

    let identity = fs::read_to_string(tmp.path().join("IDENTITY.md")).unwrap();
    assert!(identity.contains("**Name:** Claw"));

    let soul = fs::read_to_string(tmp.path().join("SOUL.md")).unwrap();
    assert!(soul.contains("You are **Claw**"));
    assert!(soul.contains("Be friendly, human, and conversational"));

    let user_md = fs::read_to_string(tmp.path().join("USER.md")).unwrap();
    assert!(user_md.contains("**Name:** Argenis"));
    assert!(user_md.contains("**Timezone:** US/Eastern"));
    assert!(user_md.contains("Be friendly, human, and conversational"));

    let agents = fs::read_to_string(tmp.path().join("AGENTS.md")).unwrap();
    assert!(agents.contains("Claw Personal Assistant"));

    let bootstrap = fs::read_to_string(tmp.path().join("BOOTSTRAP.md")).unwrap();
    assert!(bootstrap.contains("**Argenis**"));
    assert!(bootstrap.contains("US/Eastern"));
    assert!(bootstrap.contains("Introduce yourself as Claw"));

    let heartbeat = fs::read_to_string(tmp.path().join("HEARTBEAT.md")).unwrap();
    assert!(heartbeat.contains("Claw"));
}
