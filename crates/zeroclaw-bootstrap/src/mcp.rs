//! The bootstrap MCP surface: the four operations as stdio JSON-RPC tools.
//!
//! A harness (Claude Code, Codex) speaks one uniform MCP interface for
//! first-run. While ZeroClaw is absent this process answers *bootstrap*-MCP
//! (`bootstrap.status` / `bootstrap.plan` / `bootstrap.install` /
//! `bootstrap.handoff`); once a verified binary exists, `bootstrap.handoff`
//! replaces this process with `zeroclaw control --mcp` so the *same* stdio
//! pipe continues against the installed control server.
//!
//! # The install invariant
//!
//! stdio is the JSON-RPC channel, so the MCP path has no TTY and no
//! human-presence surface. The [`TOOL_INSTALL`] tool therefore performs **no
//! download and no install**. It resolves the current plan, computes the plan
//! digest, and returns a typed `human_approval_required` result naming the
//! exact terminal command a human runs
//! (`zeroclaw-bootstrap install --approve <plan-digest>`). No MCP argument —
//! not even a correct plan digest — routes to an install: there is no branch
//! in [`install_tool`] that calls [`crate::install::install`]. Human presence
//! is the OS user typing that command at a real terminal.
//!
//! # Wire protocol
//!
//! Newline-delimited JSON-RPC 2.0 over stdio. Every frame written to stdout is
//! a JSON-RPC object; all diagnostics go to stderr. `initialize`, `tools/list`,
//! `tools/call`, and `ping` are methods; `ping` is a lifecycle *method*, not a
//! tool, so [`TOOL_STATUS`] and its three siblings are the entire tool surface.

use std::io::{self, BufRead, Write};
use std::path::PathBuf;

use serde_json::{Value, json};

use crate::error::BootstrapError;
use crate::fetch::{Fetcher, HttpFetcher};
use crate::origin::default_release_tag;
use crate::plan::{Channel, HostEnv, InstallPlan};
use crate::status::{self, BinaryState, BootstrapStatus};
use crate::{handoff, install, target};

/// MCP lifecycle protocol version this launcher declares, matching the version
/// [`crate::handoff`] probes the control server with.
const MCP_PROTOCOL_VERSION: &str = "2025-06-18";

/// Bootstrap-MCP advertisement version. Distinct from the control server's
/// control-protocol version: a harness reads this to know it is talking to the
/// pre-install launcher, not the installed control surface.
pub const BOOTSTRAP_PROTOCOL_VERSION: &str = "1.0";

/// Detect the platform and any existing installed binary. No side effects.
pub const TOOL_STATUS: &str = "bootstrap.status";
/// Show the pinned install a human would approve. Reads the checksum manifest;
/// downloads no artifact and writes nothing.
pub const TOOL_PLAN: &str = "bootstrap.plan";
/// Return the exact human-run approval command. Installs nothing; no argument
/// can make it install.
pub const TOOL_INSTALL: &str = "bootstrap.install";
/// Verify the installed control server and hand the stdio pipe off to it.
pub const TOOL_HANDOFF: &str = "bootstrap.handoff";

/// The four tool names, in advertisement order.
pub const TOOL_NAMES: [&str; 4] = [TOOL_STATUS, TOOL_PLAN, TOOL_INSTALL, TOOL_HANDOFF];

/// Ambient facts the server resolves tools against.
///
/// Passed in rather than read from globals so the whole surface is testable
/// without a network or a mutated environment: production wires a
/// [`HttpFetcher`], the real [`HostEnv::from_process`], and the compiled host
/// target; tests wire a fixture fetcher, a temp-rooted env, and a fixed triple.
pub struct ServeCtx<'a> {
    /// Source of bytes at the pinned origin (checksum manifest only, on this
    /// surface).
    pub fetcher: &'a dyn Fetcher,
    /// Environment the install root is derived from.
    pub env: HostEnv,
    /// Host target triple to plan and detect against.
    pub host_target: &'a str,
}

/// What the server does with one parsed request.
pub enum Reaction {
    /// Write this single JSON-RPC frame to stdout.
    Reply(Value),
    /// A notification (no `id`): write nothing, per JSON-RPC 2.0.
    Silent,
    /// A verified handoff: replace this process with the control server so the
    /// same stdio pipe becomes its channel. Only ever produced when
    /// [`handoff::verify`] succeeded.
    Handoff {
        /// Request id, echoed only if the exec somehow fails.
        id: Value,
        /// Verified binary to exec as `zeroclaw control --mcp`.
        binary_path: PathBuf,
        /// Human-readable verification summary, written to stderr before exec.
        summary: String,
    },
}

/// One tool result before it is wrapped in a `tools/call` envelope.
struct ToolOutput {
    text: String,
    structured: Value,
    is_error: bool,
}

/// Builds and runs the stdio server against the real environment.
///
/// The `HttpFetcher` is constructed eagerly but performs no request until a
/// `plan`/`install` tool call needs the checksum manifest; `status`,
/// `handoff`, and the lifecycle methods make no network request.
pub fn run_stdio_server() -> Result<(), BootstrapError> {
    let fetcher = HttpFetcher::new()?;
    let ctx = ServeCtx {
        fetcher: &fetcher,
        env: HostEnv::from_process(),
        host_target: target::HOST_TARGET,
    };
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut out = stdout.lock();
    serve(stdin.lock(), &mut out, &ctx)
        .map_err(|err| BootstrapError::io("serving the bootstrap MCP surface over stdio", &err))
}

/// The read/dispatch/write loop, generic over its streams so tests can drive
/// it with in-memory buffers.
///
/// stdout carries only JSON-RPC frames; nothing here writes anything else to
/// `output`.
pub fn serve<R: BufRead, W: Write>(input: R, output: &mut W, ctx: &ServeCtx<'_>) -> io::Result<()> {
    for line in input.lines() {
        let line = line?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let request: Value = match serde_json::from_str(trimmed) {
            Ok(value) => value,
            Err(_) => {
                write_frame(output, &rpc_error(Value::Null, -32700, "parse error"))?;
                continue;
            }
        };
        match handle_request(ctx, &request) {
            Reaction::Silent => {}
            Reaction::Reply(frame) => write_frame(output, &frame)?,
            Reaction::Handoff {
                id,
                binary_path,
                summary,
            } => {
                // Diagnostics to stderr; stdout stays frame-only until the exec
                // replaces this process and the control server owns the pipe.
                eprintln!("{}", summary.trim_end());
                // On success this replaces the process and never returns; the
                // Ok arm holds an uninhabited `Infallible`, so only an exec
                // failure reaches a frame.
                match handoff::exec_control_server(&binary_path) {
                    Ok(never) => match never {},
                    Err(err) => write_frame(
                        output,
                        &rpc_error(id, -32603, &format!("handoff exec failed: {err}")),
                    )?,
                }
            }
        }
    }
    Ok(())
}

/// Decides the reaction to one parsed request without performing any IO on
/// stdout. This is the whole protocol surface; the loop only moves bytes.
pub fn handle_request(ctx: &ServeCtx<'_>, request: &Value) -> Reaction {
    let id = request.get("id").cloned();
    let method = request.get("method").and_then(Value::as_str);

    // No `id` member => a notification. JSON-RPC forbids a reply to one, so
    // `notifications/initialized` and any other notification are accepted
    // silently.
    let Some(id) = id else {
        return Reaction::Silent;
    };
    let Some(method) = method else {
        return Reaction::Reply(rpc_error(id, -32600, "invalid request: no method"));
    };
    let params = request.get("params").cloned().unwrap_or(Value::Null);

    match method {
        "initialize" => Reaction::Reply(rpc_success(id, initialize_result())),
        "ping" => Reaction::Reply(rpc_success(id, json!({}))),
        "tools/list" => Reaction::Reply(rpc_success(id, tools_list_result())),
        "tools/call" => react_tools_call(ctx, id, &params),
        _ => Reaction::Reply(rpc_error(id, -32601, "method not found")),
    }
}

/// The `initialize` advertisement: identifies this as the pre-install bootstrap
/// surface, distinct from the control server, and names the four tools.
fn initialize_result() -> Value {
    json!({
        "protocolVersion": MCP_PROTOCOL_VERSION,
        "capabilities": { "tools": {} },
        "serverInfo": {
            "name": "zeroclaw-bootstrap",
            "title": "ZeroClaw bootstrap install launcher (pre-install surface)",
            "version": env!("CARGO_PKG_VERSION"),
        },
        "instructions": "Pre-install bootstrap surface: ZeroClaw is not yet the process behind \
            this pipe. Call bootstrap.status to detect the host and any existing binary, \
            bootstrap.plan to review the pinned install, bootstrap.install to obtain the exact \
            terminal command a human runs to approve it (this tool never installs), and \
            bootstrap.handoff once a verified binary exists to continue on `zeroclaw control --mcp` \
            over this same pipe.",
        "_meta": {
            "zeroclaw_bootstrap": {
                "bootstrap_protocol_version": BOOTSTRAP_PROTOCOL_VERSION,
                "surface": "pre-install-bootstrap",
                "distinct_from": "zeroclaw-control",
                "tools": TOOL_NAMES,
                "install_requires_human_presence": true,
            }
        }
    })
}

/// The `tools/list` result: exactly the four bootstrap tools. `ping` is a
/// lifecycle method, not a tool, so it does not appear here.
fn tools_list_result() -> Value {
    let no_args = json!({ "type": "object", "properties": {}, "additionalProperties": false });
    json!({
        "tools": [
            {
                "name": TOOL_STATUS,
                "description": "Detect the host target and any binary already installed at the \
                    per-user install path (its version and sha256, or that it is absent or \
                    unverifiable), plus the next action (configure|install). Read-only.",
                "inputSchema": no_args,
            },
            {
                "name": TOOL_PLAN,
                "description": "Resolve the one pinned install: version, channel, source origin, \
                    artifact digest, signature status, install path, privilege, and the plan \
                    digest a human copies to approve. Reads the release checksum manifest; \
                    downloads no artifact and writes nothing.",
                "inputSchema": no_args,
            },
            {
                "name": TOOL_INSTALL,
                "description": "Return the exact terminal command a human runs to install \
                    (`zeroclaw-bootstrap install --approve <plan-digest>`). Installing changes \
                    the host and needs human presence, which the MCP pipe does not have, so this \
                    tool performs NO download and NO install and returns a human_approval_required \
                    result. No argument, including a correct plan digest, makes it install.",
                "inputSchema": json!({
                    "type": "object",
                    "properties": {
                        "approve": {
                            "type": "string",
                            "description": "Optional plan digest. Reported back as whether it \
                                matches the current plan; it NEVER authorizes an install — only a \
                                human typing the approval command at a terminal does.",
                        }
                    },
                    "additionalProperties": false,
                }),
            },
            {
                "name": TOOL_HANDOFF,
                "description": "Verify the installed control server's identity (product version, \
                    control-protocol range, capability digest, executable identity) and, on \
                    success, replace this process with `zeroclaw control --mcp` so the same pipe \
                    becomes the control channel. With no verified binary it does NOT exec and \
                    returns a route-to-install result instead.",
                "inputSchema": no_args,
            },
        ]
    })
}

/// Dispatches a `tools/call` by tool name.
fn react_tools_call(ctx: &ServeCtx<'_>, id: Value, params: &Value) -> Reaction {
    let name = params.get("name").and_then(Value::as_str).unwrap_or("");
    let arguments = params.get("arguments");

    let output = match name {
        TOOL_STATUS => Ok(status_tool(ctx)),
        TOOL_PLAN => plan_tool(ctx),
        TOOL_INSTALL => {
            let approve = arguments
                .and_then(|a| a.get("approve"))
                .and_then(Value::as_str);
            install_tool(ctx, approve)
        }
        TOOL_HANDOFF => return react_handoff(ctx, id),
        other => Ok(unknown_tool_output(other)),
    };

    let output = output.unwrap_or_else(|err| error_output(&err));
    Reaction::Reply(rpc_success(id, tool_result(&output)))
}

/// `bootstrap.status` — detection only, never a change.
fn status_tool(ctx: &ServeCtx<'_>) -> ToolOutput {
    let report = status::status(&ctx.env, ctx.host_target);
    ToolOutput {
        text: report.render(),
        structured: status_structured(&report),
        is_error: false,
    }
}

fn status_structured(report: &BootstrapStatus) -> Value {
    let existing = match &report.binary {
        BinaryState::Absent => json!({ "state": "absent" }),
        BinaryState::Verified {
            path,
            digest,
            version,
        } => json!({
            "state": "verified",
            "path": path.to_string_lossy(),
            "version": version,
            "sha256": digest,
        }),
        BinaryState::Unverifiable {
            path,
            digest,
            reason,
        } => json!({
            "state": "unverifiable",
            "path": path.to_string_lossy(),
            "sha256": digest,
            "reason": reason,
        }),
    };
    json!({
        "host_target": report.host_triple,
        "published": report.target.is_some(),
        "artifact": report.target.map(target::asset_name),
        "install_dir": report.install_dir.as_ref().map(|dir| dir.to_string_lossy()),
        "existing_binary": existing,
        "next_action": report.recommendation.next_action().token(),
    })
}

/// `bootstrap.plan` — the reviewable, approvable install. Reads the checksum
/// manifest; downloads no artifact and writes nothing.
fn plan_tool(ctx: &ServeCtx<'_>) -> Result<ToolOutput, BootstrapError> {
    let plan = InstallPlan::resolve(
        ctx.fetcher,
        &ctx.env,
        ctx.host_target,
        default_release_tag(),
    )?;
    Ok(ToolOutput {
        text: plan.render(),
        structured: plan_structured(&plan),
        is_error: false,
    })
}

fn plan_structured(plan: &InstallPlan) -> Value {
    json!({
        "version": plan.version,
        "channel": Channel::LABEL,
        "release_tag": plan.tag.as_str(),
        "target": plan.target.triple,
        "asset": plan.asset_name,
        "source_url": plan.source_url.as_str(),
        "manifest_url": plan.manifest_url.as_str(),
        "artifact_sha256": plan.artifact_digest,
        "signature": plan.signature.summary(),
        "install_dir": plan.install_dir.to_string_lossy(),
        "binary_path": plan.binary_path.to_string_lossy(),
        "privilege": plan.privilege.label(),
        "plan_digest": plan.digest(),
    })
}

/// `bootstrap.install` — the human-gated tool. It resolves the plan (reading
/// the checksum manifest, exactly as `plan` does) to name the exact approval
/// command, and returns `human_approval_required`. It performs no download and
/// no install: there is deliberately no branch here that calls
/// [`crate::install::install`], so no argument can make it install.
fn install_tool(ctx: &ServeCtx<'_>, approve: Option<&str>) -> Result<ToolOutput, BootstrapError> {
    let plan = InstallPlan::resolve(
        ctx.fetcher,
        &ctx.env,
        ctx.host_target,
        default_release_tag(),
    )?;
    let plan_digest = plan.digest();
    let approval_command = format!("zeroclaw-bootstrap install --approve {plan_digest}");

    // A supplied digest is validated ONLY to report whether it matches the
    // current plan. It is never a green light: matching or not, the result is
    // the same human_approval_required, and nothing is installed.
    let supplied_matches = approve.map(|token| install::check_approval(&plan, Some(token)).is_ok());

    let text = format!(
        "Installation changes this host and is NOT performed over MCP: the stdio JSON-RPC pipe \
         has no human-presence surface. To install ZeroClaw {version} for {triple}, a human must \
         run this exact command at a terminal:\n\n    {approval_command}\n\nNo MCP argument — not \
         even a correct plan digest — triggers an install; the launcher installs only after the \
         OS user types that command.",
        version = plan.version,
        triple = plan.target.triple,
    );

    let mut structured = json!({
        "status": "human_approval_required",
        "performed_install": false,
        "reason": "Installing changes the host; the MCP pipe is non-interactive and has no \
            human-presence surface, so the launcher names the exact terminal command a human runs \
            instead of installing.",
        "approval_command": approval_command,
        "plan_digest": plan_digest,
        "target": plan.target.triple,
        "install_dir": plan.install_dir.to_string_lossy(),
        "binary_path": plan.binary_path.to_string_lossy(),
    });
    if let Some(matches) = supplied_matches {
        structured["supplied_approval_matches_current_plan"] = json!(matches);
    }

    Ok(ToolOutput {
        text,
        structured,
        is_error: false,
    })
}

/// `bootstrap.handoff` — verify the installed control server, then hand the
/// pipe off to it. Only a successful [`handoff::verify`] produces an exec; any
/// verification failure (absent, unverifiable, wrong protocol, wrong product
/// version) routes to install and never execs.
fn react_handoff(ctx: &ServeCtx<'_>, id: Value) -> Reaction {
    let binary_path = match handoff_binary_path(ctx) {
        Ok(path) => path,
        Err(err) => return Reaction::Reply(rpc_success(id, tool_result(&error_output(&err)))),
    };

    match handoff::verify(&binary_path, None) {
        Ok(verified) => Reaction::Handoff {
            id,
            summary: verified.render(),
            binary_path,
        },
        Err(err) => {
            let output = route_to_install_output(&binary_path, &err);
            Reaction::Reply(rpc_success(id, tool_result(&output)))
        }
    }
}

/// Resolves the install path the handoff would target, refusing an unsupported
/// host or an underivable install root before any process is spawned.
fn handoff_binary_path(ctx: &ServeCtx<'_>) -> Result<PathBuf, BootstrapError> {
    let target = target::resolve(ctx.host_target)?;
    Ok(ctx.env.install_dir(target.family)?.join(target.binary_name))
}

fn route_to_install_output(binary_path: &std::path::Path, err: &BootstrapError) -> ToolOutput {
    let text = format!(
        "No verified control server at {path}: {err}\nRoute: bootstrap.status, then \
         bootstrap.plan, then have a human run the bootstrap.install approval command, then retry \
         bootstrap.handoff. Nothing was executed.",
        path = binary_path.display(),
    );
    ToolOutput {
        text,
        structured: json!({
            "status": "route_to_install",
            "executed_handoff": false,
            "next_action": "install",
            "binary_path": binary_path.to_string_lossy(),
            "reason": err.to_string(),
        }),
        is_error: false,
    }
}

fn error_output(err: &BootstrapError) -> ToolOutput {
    ToolOutput {
        text: err.to_string(),
        structured: json!({ "status": "error", "error": err.to_string() }),
        is_error: true,
    }
}

fn unknown_tool_output(name: &str) -> ToolOutput {
    ToolOutput {
        text: format!("unknown tool `{name}`; call tools/list for the available tools"),
        structured: json!({ "status": "error", "error": "unknown tool", "tool": name }),
        is_error: true,
    }
}

/// Wraps a [`ToolOutput`] in the MCP `tools/call` result envelope.
fn tool_result(output: &ToolOutput) -> Value {
    json!({
        "content": [{ "type": "text", "text": output.text }],
        "structuredContent": output.structured,
        "isError": output.is_error,
    })
}

fn rpc_success(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn rpc_error(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

/// Writes one JSON-RPC frame followed by a newline, then flushes. Serializing a
/// value the launcher itself built cannot fail; the fallback frame keeps stdout
/// valid JSON-RPC even in the impossible case rather than panicking.
fn write_frame<W: Write>(output: &mut W, frame: &Value) -> io::Result<()> {
    let mut line = serde_json::to_string(frame).unwrap_or_else(|_| {
        String::from(
            r#"{"jsonrpc":"2.0","id":null,"error":{"code":-32603,"message":"internal serialization error"}}"#,
        )
    });
    line.push('\n');
    output.write_all(line.as_bytes())?;
    output.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A one-asset fixture origin: the checksum manifest names the target's
    /// archive, and the archive itself is served too, so that a mutation which
    /// wrongly installed would actually write a binary a test could catch.
    struct OneAssetOrigin {
        manifest_url: String,
        manifest: Vec<u8>,
        asset_url: String,
        archive: Vec<u8>,
    }

    const TEST_TRIPLE: &str = "x86_64-unknown-linux-gnu";
    const TEST_ASSET: &str = "zeroclaw-x86_64-unknown-linux-gnu.tar.gz";

    impl OneAssetOrigin {
        fn new() -> Self {
            use std::io::Write as _;
            let binary = b"#!/bin/sh\necho 'zeroclaw 0.8.4'\n";
            // A minimal tar.gz carrying the registry-named binary at top level.
            let mut builder = tar::Builder::new(Vec::new());
            let mut header = tar::Header::new_gnu();
            header.set_size(binary.len() as u64);
            header.set_mode(0o755);
            header.set_cksum();
            builder
                .append_data(&mut header, "zeroclaw", &binary[..])
                .expect("tar");
            let tar_bytes = builder.into_inner().expect("tar inner");
            let mut encoder =
                flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
            encoder.write_all(&tar_bytes).expect("gzip");
            let archive = encoder.finish().expect("gzip finish");

            let tag = crate::origin::ReleaseTag::parse("v0.8.4").expect("tag");
            let manifest = format!("{}  {TEST_ASSET}\n", crate::fetch::sha256_hex(&archive));
            Self {
                manifest_url: crate::origin::PinnedUrl::checksum_manifest(&tag)
                    .as_str()
                    .to_string(),
                manifest: manifest.into_bytes(),
                asset_url: crate::origin::PinnedUrl::asset(&tag, TEST_ASSET)
                    .as_str()
                    .to_string(),
                archive,
            }
        }
    }

    impl Fetcher for OneAssetOrigin {
        fn fetch(&self, url: &crate::origin::PinnedUrl) -> Result<Vec<u8>, BootstrapError> {
            if url.as_str() == self.manifest_url {
                Ok(self.manifest.clone())
            } else if url.as_str() == self.asset_url {
                Ok(self.archive.clone())
            } else {
                Err(BootstrapError::Transport {
                    url: url.as_str().to_string(),
                    reason: "fixture has no such asset".to_string(),
                })
            }
        }
    }

    /// A fetcher that must never be called.
    struct NeverFetches;
    impl Fetcher for NeverFetches {
        fn fetch(&self, url: &crate::origin::PinnedUrl) -> Result<Vec<u8>, BootstrapError> {
            panic!("must not fetch {}", url.as_str());
        }
    }

    fn temp_env(root: &std::path::Path) -> HostEnv {
        HostEnv {
            cargo_home: Some(root.join("cargo")),
            home: Some(root.join("home")),
            user_profile: Some(root.join("profile")),
        }
    }

    fn version_at(structured: &Value, key: &str) -> String {
        structured
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    }

    /// The install tool must return human_approval_required and install
    /// NOTHING, even handed the exact correct plan digest.
    ///
    /// Mutation target (1): a branch that installs when the digest matches makes
    /// this fail on the "binary must not exist" assertions.
    #[test]
    fn install_over_mcp_never_installs_even_with_the_correct_digest() {
        let root = tempfile::tempdir().expect("temp");
        let origin = OneAssetOrigin::new();
        let env = temp_env(root.path());
        let ctx = ServeCtx {
            fetcher: &origin,
            env,
            host_target: TEST_TRIPLE,
        };

        // Recompute the exact digest a human would approve and pass it as the arg.
        let plan = InstallPlan::resolve(&origin, &ctx.env, TEST_TRIPLE, default_release_tag())
            .expect("plan");
        let correct = plan.digest();

        let output = install_tool(&ctx, Some(&correct)).expect("install tool ok");
        assert!(!output.is_error);
        assert_eq!(
            version_at(&output.structured, "status"),
            "human_approval_required"
        );
        assert_eq!(
            output
                .structured
                .get("performed_install")
                .and_then(Value::as_bool),
            Some(false)
        );
        assert_eq!(
            output
                .structured
                .get("supplied_approval_matches_current_plan")
                .and_then(Value::as_bool),
            Some(true),
            "a correct digest is reported as matching — and still does not install"
        );
        assert_eq!(
            version_at(&output.structured, "approval_command"),
            format!("zeroclaw-bootstrap install --approve {correct}")
        );

        // The invariant: nothing on disk.
        assert!(
            !plan.binary_path.exists(),
            "MCP install must not write a binary"
        );
        assert!(
            !plan.install_dir.exists(),
            "MCP install must not create the install directory"
        );
    }

    /// No argument value — correct, wrong, empty, or absent — makes install install.
    #[test]
    fn no_install_argument_installs_anything() {
        let root = tempfile::tempdir().expect("temp");
        let origin = OneAssetOrigin::new();
        let ctx = ServeCtx {
            fetcher: &origin,
            env: temp_env(root.path()),
            host_target: TEST_TRIPLE,
        };
        let plan = InstallPlan::resolve(&origin, &ctx.env, TEST_TRIPLE, default_release_tag())
            .expect("plan");
        let correct = plan.digest();

        for arg in [
            None,
            Some(correct.as_str()),
            Some("sha256:0000000000000000000000000000000000000000000000000000000000000000"),
            Some("true"),
            Some(""),
        ] {
            let output = install_tool(&ctx, arg).expect("ok");
            assert_eq!(
                version_at(&output.structured, "status"),
                "human_approval_required"
            );
            assert!(
                !plan.binary_path.exists(),
                "no argument may install: {arg:?}"
            );
        }
    }

    /// With no verified binary, the handoff reaction is a Reply that routes to
    /// install — never a Handoff (which would exec).
    ///
    /// Mutation target (2): returning `Reaction::Handoff` on a verify failure
    /// makes this fail at the `matches!` assertion, without ever exec'ing.
    #[test]
    fn handoff_without_a_verified_binary_routes_to_install_and_does_not_exec() {
        let root = tempfile::tempdir().expect("temp");
        let ctx = ServeCtx {
            fetcher: &NeverFetches,
            env: temp_env(root.path()),
            host_target: TEST_TRIPLE,
        };
        let request = json!({
            "jsonrpc": "2.0",
            "id": 7,
            "method": "tools/call",
            "params": { "name": TOOL_HANDOFF, "arguments": {} }
        });
        let reaction = handle_request(&ctx, &request);
        assert!(
            matches!(reaction, Reaction::Reply(_)),
            "no verified binary must route to install, never exec"
        );
        let Reaction::Reply(frame) = reaction else {
            unreachable!()
        };
        let structured = &frame["result"]["structuredContent"];
        assert_eq!(version_at(structured, "status"), "route_to_install");
        assert_eq!(
            structured.get("executed_handoff").and_then(Value::as_bool),
            Some(false)
        );
        assert_eq!(version_at(structured, "next_action"), "install");
    }

    #[test]
    fn initialize_advertises_the_pre_install_surface() {
        let result = initialize_result();
        assert_eq!(result["serverInfo"]["name"], "zeroclaw-bootstrap");
        let meta = &result["_meta"]["zeroclaw_bootstrap"];
        assert_eq!(
            meta["bootstrap_protocol_version"],
            BOOTSTRAP_PROTOCOL_VERSION
        );
        assert_eq!(meta["surface"], "pre-install-bootstrap");
        assert_eq!(meta["distinct_from"], "zeroclaw-control");
        assert_eq!(meta["install_requires_human_presence"], true);
    }

    #[test]
    fn tools_list_names_exactly_the_four_tools_and_ping_is_not_one() {
        let result = tools_list_result();
        let names: Vec<&str> = result["tools"]
            .as_array()
            .expect("tools array")
            .iter()
            .map(|t| t["name"].as_str().expect("name"))
            .collect();
        assert_eq!(names, TOOL_NAMES);
        assert!(!names.contains(&"ping"), "ping is a method, not a tool");
    }
}
