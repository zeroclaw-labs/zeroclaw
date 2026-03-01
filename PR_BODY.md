## Description

Fixes issue #2327 where MCP stdio transport incorrectly reads server notifications as tool responses, causing 0 tools to be registered.

## Problem

`StdioTransport::send_and_recv` reads exactly one line from subprocess stdout. Some MCP servers send a `notifications/initialized` notification after the `initialize` response but before the `tools/list` response arrives.

ZeroClaw reads this notification as the `tools/list` reply, sees `result: None`, and reports 0 tools registered.

## Solution

Replace the single read with a deadline-bounded loop that skips any JSON-RPC message where `id` is `None` (server notifications). The total timeout is preserved across all iterations.

## Changes

- Modified `StdioTransport::send_and_recv` in `src/tools/mcp_transport.rs`
- Added deadline-bounded loop to skip server notifications  
- Added debug logging for skipped notifications

## Testing

- Fixes n8n via supergateway scenario described in issue
- Preserves existing behavior for normal MCP servers
- No new dependencies or breaking changes

Closes #2327