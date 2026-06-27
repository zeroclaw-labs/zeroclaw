# Tool: shell

Shell command execution. Runs arbitrary commands in the workspace via `sh -c`.

## Location

crates/zeroclaw-runtime/src/tools/shell.rs

Wrapped by RateLimitedTool + PathGuardedTool for security (rate limits, path allowlist).
