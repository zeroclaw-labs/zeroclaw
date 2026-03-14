# Google Workspace CLI Tool Implementation Plan

## Overview
Implement a native `gws` tool for ZeroClaw that wraps the Google Workspace CLI (`gws`) binary and exposes Gmail, Calendar, Drive, and other services through a structured interface.

## Tool Design

### Tool Name
`gws` (Google Workspace)

### Services to Support
- `gmail` — Email operations (fetch, search, send, etc.)
- `calendar` — Calendar events (list, create, update, delete)
- `drive` — File operations
- `docs` — Document operations
- `tasks` — Task list operations
- `keep` — Notes/Keep operations

### Parameters
```json
{
  "service": "gmail",           // Required: gmail, calendar, drive, etc.
  "resource": "messages",        // Required: service-specific resource
  "method": "list",              // Required: service-specific method
  "params": {                    // Optional: method parameters as key-value pairs
    "maxResults": 10,
    "query": "is:unread"
  }
}
```

### Execution Flow
1. Validate service/resource/method combination
2. Call `gws <service> <resource> <method>` with params
3. Parse output and return structured result
4. Handle authentication via local Google credentials (already configured via `.env`)

## Implementation Steps

### 1. Create Tool Module
File: `src/tools/gws.rs`
- Struct: `GoogleWorkspaceTool`
- Implements: `Tool` trait
- Constructor: `new(security: Arc<SecurityPolicy>) -> Self`

### 2. Parameter Schema
Define JSON schema for:
- Service enum (gmail, calendar, drive, etc.)
- Resource (context-dependent on service)
- Method (context-dependent on service)
- Params (flexible object)

### 3. Execute Implementation
- Validate `gws` CLI is installed
- Build command: `gws <service> <resource> <method> [--params]`
- Handle JSON parameters properly
- Timeout: 30 seconds
- Max output: 5MB

### 4. Security Integration
- Restrict to allowed services (configurable)
- Validate resource/method combinations
- Rate limit calls
- Log all operations

### 5. Register Tool
- Add `pub mod gws;` to `src/tools/mod.rs`
- Export: `pub use gws::GoogleWorkspaceTool;`
- Register in `all_tools_with_runtime()` function

## Dependencies
- `tokio` (async execution)
- `serde_json` (JSON handling)
- `regex` (validation)
- Google Workspace CLI (external binary requirement)

## Testing Strategy
1. Unit tests for parameter validation
2. Integration tests (if `gws` CLI available)
3. Mock tests for command construction
4. Security policy enforcement tests

## Contribution Checklist
- [ ] Implementation complete
- [ ] Tests pass
- [ ] Code formatted (cargo fmt)
- [ ] Clippy checks pass
- [ ] Documentation added
- [ ] AGENTS.md updated (§7.3)
- [ ] Changelog entry added
- [ ] PR submitted to zeroclaw-labs/zeroclaw

## Notes
- Tool depends on `gws` CLI being installed and authenticated locally
- Credentials come from local Google OAuth (not stored in ZeroClaw config)
- Aligns with ZeroClaw's "edge computing" philosophy (no external API dependency)
