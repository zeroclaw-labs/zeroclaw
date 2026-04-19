//! CLI Plugin - Basic test fixture for CLI execution
//!
//! This plugin provides a simple tool that calls the `echo` command via
//! `cli_exec`. Used for testing basic CLI capability functionality.

use extism_pdk::*;
use serde::{Deserialize, Serialize};
use zeroclaw_plugin_sdk::cli::cli_exec;

/// Input for the echo tool.
#[derive(Debug, Deserialize)]
struct EchoInput {
    /// Message to echo.
    message: String,
}

/// Output from the echo tool.
#[derive(Debug, Serialize)]
struct EchoOutput {
    /// The echoed message from stdout.
    stdout: String,
    /// Any stderr output.
    stderr: String,
    /// Exit code from the command.
    exit_code: i32,
}

/// Executes `echo` with the provided message.
///
/// This tool demonstrates basic CLI execution capability by calling
/// the `echo` command with a user-provided message.
#[plugin_fn]
pub fn tool_cli_echo(input: String) -> FnResult<String> {
    let parsed: EchoInput = serde_json::from_str(&input)?;

    let result = cli_exec("echo", &[&parsed.message], None, None)?;

    let output = EchoOutput {
        stdout: result.stdout,
        stderr: result.stderr,
        exit_code: result.exit_code,
    };

    Ok(serde_json::to_string(&output)?)
}
