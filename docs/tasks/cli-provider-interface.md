# CLI Provider Interface - Feature Plan (Pivoted)

## Epic Overview

### User Value
Enable ZeroClaw to use CLI-based AI agents (Claude CLI, Gemini CLI, OpenCode CLI) as high-level task executors. ZeroClaw dispatches complex goals to these CLIs, which then execute their own internal tools and return a final result.

### Success Metrics
- [x] Successfully query Claude CLI and receive JSON response
- [x] Successfully query Gemini CLI and receive JSON response
- [x] Successfully query OpenCode CLI
- [x] Passed conversation history via stdin to avoid ARG_MAX limits
- [ ] Robustly handle multi-turn internal CLI agent sessions (10min+ timeouts)
- [ ] ANSI escape codes and progress artifacts stripped from output

---

## Story Breakdown

### Story 1: Foundation (Completed)
**Tasks:**
- [x] 1.1: Create CLI Provider Module
- [x] 1.2: Implement Claude JSON Parser
- [x] 1.3: Register CLI Provider in Factory
- [x] 1.4: Use stdin for prompt passing

### Story 2: Multi-CLI Support (Completed)
**Tasks:**
- [x] 2.1: Implement Gemini JSON Parser
- [x] 2.2: Implement OpenCode Integration
- [x] 2.3: Configuration Schema for CLI Providers

### Story 3: Autonomous Agent Integration (In Progress)
**Tasks:**
- [ ] 3.1: Robust Output Sanitization (ANSI stripping)
- [ ] 3.2: Usage & Token Extraction
- [ ] 3.3: Capabilities & Timeout Adjustment

---

## Atomic Tasks

### Task 3.1: Robust Output Sanitization [2h]
**Objective:** Strip ANSI codes, progress bars, and "thinking" artifacts from CLI output.

**Context Boundary:**
- Files: `src/providers/cli.rs`
- Concepts: Regex sanitization, ANSI stripping

### Task 3.2: Usage & Token Extraction [2h]
**Objective:** Extract actual token usage from CLI JSON responses.

**Context Boundary:**
- Files: `src/providers/cli.rs`, `src/providers/traits.rs`
- Concepts: TokenUsage struct, JSON extraction

### Task 3.3: Capabilities & Timeout Adjustment [1h]
**Objective:** Declare capabilities correctly and ensure timeouts allow for internal agentic work.

**Context Boundary:**
- Files: `src/providers/cli.rs`
- Concepts: ProviderCapabilities, Timeouts

---

## Integration Checkpoints

| After Story | What Should Be Verifiable |
|-------------|--------------------------|
| Story 3 | ZeroClaw dispatches a complex task to Claude CLI and gets back a clean final answer after 5 mins. |
