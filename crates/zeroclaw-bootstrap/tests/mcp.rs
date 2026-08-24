//! The bootstrap MCP surface end to end: the four tools over JSON-RPC 2.0, and
//! the stdout frame discipline.
//!
//! Nothing here touches the network. Tool calls that need release bytes are
//! driven against `support`'s in-process fixture origin; the frame-discipline
//! test spawns the real binary but exercises only network-free methods.

mod support;

use std::io::Write;
use std::process::{Command, Stdio};

use serde_json::{Value, json};
use support::{FixtureOrigin, NeverFetches, checksum_manifest, tar_gz};

use zeroclaw_bootstrap::mcp::{
    self, Reaction, ServeCtx, TOOL_HANDOFF, TOOL_INSTALL, TOOL_NAMES, TOOL_PLAN, TOOL_STATUS,
};
use zeroclaw_bootstrap::origin::{PinnedUrl, ReleaseTag};
use zeroclaw_bootstrap::plan::{HostEnv, InstallPlan};

const TRIPLE: &str = "x86_64-unknown-linux-gnu";
const ASSET: &str = "zeroclaw-x86_64-unknown-linux-gnu.tar.gz";
const BINARY_BODY: &[u8] = b"#!/bin/sh\necho 'zeroclaw 0.8.4'\n";
const BIN: &str = env!("CARGO_BIN_EXE_zeroclaw-bootstrap");

fn tag() -> ReleaseTag {
    ReleaseTag::parse("v0.8.4").expect("valid tag")
}

fn temp_env(root: &std::path::Path) -> HostEnv {
    HostEnv {
        cargo_home: Some(root.join("cargo")),
        home: Some(root.join("home")),
        user_profile: Some(root.join("profile")),
    }
}

/// A fixture origin carrying both the checksum manifest and the artifact, so a
/// regression that wrongly installed would actually write a binary a test can
/// see.
fn full_origin() -> FixtureOrigin {
    let archive = tar_gz(&[("zeroclaw", BINARY_BODY)]);
    let manifest = checksum_manifest(&[(ASSET, &archive)]);
    FixtureOrigin::new()
        .with(&PinnedUrl::checksum_manifest(&tag()), manifest.into_bytes())
        .with(&PinnedUrl::asset(&tag(), ASSET), archive)
}

/// Drives one `tools/call` and returns the reply frame, asserting the reaction
/// is a plain reply (never a handoff exec).
fn call(ctx: &ServeCtx<'_>, tool: &str, arguments: Value) -> Value {
    let request = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": { "name": tool, "arguments": arguments },
    });
    match mcp::handle_request(ctx, &request) {
        Reaction::Reply(frame) => frame,
        _ => panic!("expected a Reply frame for `{tool}`"),
    }
}

fn method(ctx: &ServeCtx<'_>, id: i64, method: &str) -> Value {
    let request = json!({ "jsonrpc": "2.0", "id": id, "method": method });
    match mcp::handle_request(ctx, &request) {
        Reaction::Reply(frame) => frame,
        _ => panic!("expected a Reply frame for `{method}`"),
    }
}

#[test]
fn initialize_advertises_the_pre_install_bootstrap_surface() {
    let origin = NeverFetches;
    let root = tempfile::tempdir().expect("temp");
    let ctx = ServeCtx {
        fetcher: &origin,
        env: temp_env(root.path()),
        host_target: TRIPLE,
    };
    let frame = method(&ctx, 1, "initialize");

    assert_eq!(frame["id"], 1);
    let result = &frame["result"];
    assert_eq!(result["serverInfo"]["name"], "zeroclaw-bootstrap");
    assert!(result["capabilities"]["tools"].is_object());

    let meta = &result["_meta"]["zeroclaw_bootstrap"];
    assert_eq!(meta["bootstrap_protocol_version"], "1.0");
    assert_eq!(meta["surface"], "pre-install-bootstrap");
    assert_eq!(meta["distinct_from"], "zeroclaw-control");
    assert_eq!(meta["install_requires_human_presence"], true);
    let advertised: Vec<&str> = meta["tools"]
        .as_array()
        .expect("tools array")
        .iter()
        .map(|t| t.as_str().expect("tool name"))
        .collect();
    assert_eq!(advertised, TOOL_NAMES);
}

#[test]
fn tools_list_is_exactly_the_four_bootstrap_tools() {
    let origin = NeverFetches;
    let root = tempfile::tempdir().expect("temp");
    let ctx = ServeCtx {
        fetcher: &origin,
        env: temp_env(root.path()),
        host_target: TRIPLE,
    };
    let frame = method(&ctx, 2, "tools/list");

    let tools = frame["result"]["tools"].as_array().expect("tools array");
    let names: Vec<&str> = tools
        .iter()
        .map(|t| t["name"].as_str().expect("name"))
        .collect();
    assert_eq!(names, TOOL_NAMES, "exactly the four bootstrap tools");
    assert!(!names.contains(&"ping"), "ping is a method, not a tool");

    for tool in tools {
        assert!(
            tool.get("description").is_some(),
            "each tool has a description"
        );
        assert_eq!(
            tool["inputSchema"]["type"], "object",
            "each tool declares an object inputSchema"
        );
    }
}

#[test]
fn ping_is_a_method_returning_an_empty_result() {
    let origin = NeverFetches;
    let root = tempfile::tempdir().expect("temp");
    let ctx = ServeCtx {
        fetcher: &origin,
        env: temp_env(root.path()),
        host_target: TRIPLE,
    };
    let frame = method(&ctx, 3, "ping");
    assert_eq!(frame["id"], 3);
    assert_eq!(frame["result"], json!({}));
}

#[test]
fn status_reports_the_absent_fixture() {
    let origin = NeverFetches;
    let root = tempfile::tempdir().expect("temp");
    let ctx = ServeCtx {
        fetcher: &origin,
        env: temp_env(root.path()),
        host_target: TRIPLE,
    };
    let frame = call(&ctx, TOOL_STATUS, json!({}));
    assert_eq!(frame["result"]["isError"], false);

    let sc = &frame["result"]["structuredContent"];
    assert_eq!(sc["host_target"], TRIPLE);
    assert_eq!(sc["published"], true);
    assert_eq!(sc["artifact"], ASSET);
    assert_eq!(sc["existing_binary"]["state"], "absent");
    assert_eq!(sc["next_action"], "install");
}

#[cfg(unix)]
#[test]
fn status_reports_an_installed_verified_binary() {
    let root = tempfile::tempdir().expect("temp");
    let bin_dir = root.path().join("cargo").join("bin");
    std::fs::create_dir_all(&bin_dir).expect("bin dir");
    support::write_executable(
        &bin_dir,
        "zeroclaw",
        "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then echo 'zeroclaw 0.8.4'; exit 0; fi\nexit 1\n",
    );
    let origin = NeverFetches;
    let ctx = ServeCtx {
        fetcher: &origin,
        env: temp_env(root.path()),
        host_target: TRIPLE,
    };
    let frame = call(&ctx, TOOL_STATUS, json!({}));
    let sc = &frame["result"]["structuredContent"];
    assert_eq!(sc["existing_binary"]["state"], "verified");
    assert_eq!(sc["existing_binary"]["version"], "0.8.4");
    assert_eq!(
        sc["existing_binary"]["sha256"]
            .as_str()
            .expect("sha256")
            .len(),
        64
    );
    assert_eq!(sc["next_action"], "configure");
}

#[test]
fn plan_returns_the_pinned_selection_and_the_digest_a_human_approves() {
    let origin = full_origin();
    let root = tempfile::tempdir().expect("temp");
    let env = temp_env(root.path());
    let expected = InstallPlan::resolve(&origin, &env, TRIPLE, tag()).expect("plan");
    let ctx = ServeCtx {
        fetcher: &origin,
        env,
        host_target: TRIPLE,
    };
    let frame = call(&ctx, TOOL_PLAN, json!({}));
    assert_eq!(frame["result"]["isError"], false);

    let sc = &frame["result"]["structuredContent"];
    assert_eq!(sc["version"], "0.8.4");
    assert_eq!(sc["channel"], "stable");
    assert_eq!(sc["release_tag"], "v0.8.4");
    assert_eq!(sc["target"], TRIPLE);
    assert_eq!(sc["asset"], ASSET);
    assert!(
        sc["source_url"]
            .as_str()
            .expect("source url")
            .starts_with("https://github.com/zeroclaw-labs/zeroclaw/releases/download/")
    );
    assert_eq!(sc["artifact_sha256"].as_str().expect("digest").len(), 64);
    assert_eq!(sc["privilege"], "none (per-user install directory)");
    assert_eq!(
        sc["plan_digest"].as_str().expect("plan digest"),
        expected.digest(),
        "the MCP plan digest is the same token `plan` prints for a human to approve"
    );
}

#[test]
fn install_over_mcp_requires_human_presence_and_installs_nothing() {
    let origin = full_origin();
    let root = tempfile::tempdir().expect("temp");
    let env = temp_env(root.path());
    let plan = InstallPlan::resolve(&origin, &env, TRIPLE, tag()).expect("plan");
    let correct = plan.digest();
    let ctx = ServeCtx {
        fetcher: &origin,
        env,
        host_target: TRIPLE,
    };

    // Even handed the exact digest a human would approve, the tool refuses.
    let frame = call(&ctx, TOOL_INSTALL, json!({ "approve": correct }));
    assert_eq!(frame["result"]["isError"], false);

    let sc = &frame["result"]["structuredContent"];
    assert_eq!(sc["status"], "human_approval_required");
    assert_eq!(sc["performed_install"], false);
    assert_eq!(sc["supplied_approval_matches_current_plan"], true);
    assert_eq!(
        sc["approval_command"],
        format!("zeroclaw-bootstrap install --approve {correct}")
    );
    // The human-run command names the exact terminal invocation.
    let text = frame["result"]["content"][0]["text"]
        .as_str()
        .expect("text content");
    assert!(text.contains(&format!("zeroclaw-bootstrap install --approve {correct}")));

    // The invariant: the MCP path wrote nothing.
    assert!(
        !plan.binary_path.exists(),
        "MCP install must not write a binary"
    );
    assert!(
        !plan.install_dir.exists(),
        "MCP install must not create the install directory"
    );
}

#[test]
fn no_install_argument_makes_the_mcp_path_install() {
    let origin = full_origin();
    let root = tempfile::tempdir().expect("temp");
    let env = temp_env(root.path());
    let plan = InstallPlan::resolve(&origin, &env, TRIPLE, tag()).expect("plan");
    let correct = plan.digest();
    let ctx = ServeCtx {
        fetcher: &origin,
        env,
        host_target: TRIPLE,
    };

    for arg in [
        json!({}),
        json!({ "approve": correct }),
        json!({ "approve": "sha256:0000000000000000000000000000000000000000000000000000000000000000" }),
        json!({ "approve": "true" }),
        json!({ "approve": "" }),
    ] {
        let frame = call(&ctx, TOOL_INSTALL, arg.clone());
        assert_eq!(
            frame["result"]["structuredContent"]["status"], "human_approval_required",
            "argument {arg} must not install"
        );
        assert!(
            !plan.binary_path.exists(),
            "argument {arg} must not write a binary"
        );
    }
}

#[test]
fn handoff_without_a_verified_binary_routes_to_install_and_never_execs() {
    let origin = NeverFetches;
    let root = tempfile::tempdir().expect("temp");
    let ctx = ServeCtx {
        fetcher: &origin,
        env: temp_env(root.path()),
        host_target: TRIPLE,
    };
    let request = json!({
        "jsonrpc": "2.0",
        "id": 4,
        "method": "tools/call",
        "params": { "name": TOOL_HANDOFF, "arguments": {} },
    });
    let reaction = mcp::handle_request(&ctx, &request);
    assert!(
        matches!(reaction, Reaction::Reply(_)),
        "with no verified binary the handoff must route to install, never exec"
    );
    let Reaction::Reply(frame) = reaction else {
        unreachable!()
    };
    let sc = &frame["result"]["structuredContent"];
    assert_eq!(sc["status"], "route_to_install");
    assert_eq!(sc["executed_handoff"], false);
    assert_eq!(sc["next_action"], "install");
}

#[test]
fn an_unknown_method_is_a_json_rpc_error() {
    let origin = NeverFetches;
    let root = tempfile::tempdir().expect("temp");
    let ctx = ServeCtx {
        fetcher: &origin,
        env: temp_env(root.path()),
        host_target: TRIPLE,
    };
    let frame = method(&ctx, 9, "does/not/exist");
    assert_eq!(frame["id"], 9);
    assert_eq!(frame["error"]["code"], -32601);
    assert!(frame.get("result").is_none());
}

#[test]
fn an_unknown_tool_is_an_error_result_not_a_protocol_error() {
    let origin = NeverFetches;
    let root = tempfile::tempdir().expect("temp");
    let ctx = ServeCtx {
        fetcher: &origin,
        env: temp_env(root.path()),
        host_target: TRIPLE,
    };
    let frame = call(&ctx, "bootstrap.nope", json!({}));
    // A tools/call for an unknown tool is a successful RPC carrying an error
    // result, not a JSON-RPC method error.
    assert!(frame.get("result").is_some());
    assert_eq!(frame["result"]["isError"], true);
}

#[test]
fn a_notification_gets_no_reply() {
    let origin = NeverFetches;
    let root = tempfile::tempdir().expect("temp");
    let ctx = ServeCtx {
        fetcher: &origin,
        env: temp_env(root.path()),
        host_target: TRIPLE,
    };
    let request = json!({ "jsonrpc": "2.0", "method": "notifications/initialized" });
    assert!(matches!(
        mcp::handle_request(&ctx, &request),
        Reaction::Silent
    ));
}

/// The real binary: stdout carries only JSON-RPC frames, one per request that
/// expects a reply, and nothing else.
#[test]
fn stdout_carries_only_json_rpc_frames() {
    let root = tempfile::tempdir().expect("temp");
    let mut child = Command::new(BIN)
        .arg("mcp")
        // Hermetic: never see the developer's real install.
        .env("CARGO_HOME", root.path().join("cargo"))
        .env("HOME", root.path().join("home"))
        .env("USERPROFILE", root.path().join("profile"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn mcp server");

    let input = [
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
        r#"{"jsonrpc":"2.0","id":3,"method":"ping"}"#,
        r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"bootstrap.status","arguments":{}}}"#,
        r#"{"jsonrpc":"2.0","id":5,"method":"nope"}"#,
    ]
    .join("\n");

    {
        let mut stdin = child.stdin.take().expect("child stdin");
        stdin.write_all(input.as_bytes()).expect("write stdin");
        stdin.write_all(b"\n").expect("write newline");
    } // dropping stdin closes it, ending the server's read loop at EOF

    let output = child.wait_with_output().expect("wait for server");
    assert!(
        output.status.success(),
        "server exited nonzero; stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8(output.stdout).expect("stdout is utf-8");
    let frames: Vec<&str> = stdout
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
    assert_eq!(
        frames.len(),
        5,
        "expected exactly 5 reply frames (the notification is silent); stdout was:\n{stdout}"
    );

    let mut ids = Vec::new();
    for line in &frames {
        let frame: Value = serde_json::from_str(line)
            .unwrap_or_else(|err| panic!("stdout line is not a JSON-RPC frame ({err}): {line:?}"));
        assert_eq!(
            frame["jsonrpc"], "2.0",
            "every stdout frame is JSON-RPC 2.0: {line}"
        );
        assert!(
            frame.get("result").is_some() || frame.get("error").is_some(),
            "every reply carries a result or an error: {line}"
        );
        ids.push(frame["id"].as_i64().expect("reply id"));
    }
    assert_eq!(ids, vec![1, 2, 3, 4, 5], "one reply per request, in order");
}
