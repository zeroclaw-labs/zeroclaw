# Feature Plan: Unified Browser Integration

This document outlines the tasks to provide ZeroClaw with a production-grade, native web automation system, achieving parity with OpenClaw's browser capabilities while maintaining ZeroClaw's high-performance Rust core.

## Epic Overview

- **User Value**: Enables the agent to navigate the web, interact with SaaS tools, and extract information from complex web apps without relying on fragile external CLIs or slow WebDriver setups.
- **Success Metrics**: 
    - Supports native CDP (Chrome DevTools Protocol) for high-speed automation.
    - Zero external Node.js dependencies for basic browsing.
    - Successfully navigates, clicks, and extracts data from JS-heavy sites (e.g., GitHub, Linear).
- **Scope**: Implementing a new `ChromiumBackend` using the `chromiumoxide` crate, refactoring `BrowserTool` to use it by default, and unifying `browser_open` logic.
- **Constraints**: Must support headless and headful modes. Must enforce strict domain allowlists.

## Architecture Decisions

### ADR 007: CDP over WebDriver
- **Context**: ZeroClaw currently has a `fantoccini` (WebDriver) backend. WebDriver requires a separate binary (chromedriver) and is often slower/less flexible than direct CDP.
- **Decision**: Adopt `chromiumoxide` as the primary backend for ZeroClaw.
- **Rationale**: Direct CDP allows for better control over the browser session, faster execution, and better support for modern agentic perception (e.g., accessibility tree extraction).
- **Patterns Applied**: Adapter Pattern.

### ADR 008: Unified Web Interface
- **Context**: We have `browser` (automation) and `browser_open` (desktop launcher). This is confusing for the LLM.
- **Decision**: Unify both into `BrowserTool`. Add a `mode: "automated" | "launcher"` parameter if needed, but prefer a single `browser` tool that "just works."
- **Rationale**: Simplifies the tool surface for the agent.

## Story Breakdown

### Story 1: Native CDP Backend [1 week]
Implement the core browsing engine in Rust.

#### Acceptance Criteria
- [ ] `src/browser/mod.rs` provides a clean wrapper over `chromiumoxide`.
- [ ] Supports starting/stopping a headless Chrome instance.
- [ ] Basic navigation and HTML extraction work.

#### Atomic Tasks
- **Task 1.1: Browser Module Foundation [2h]** ✅ Completed
    - Objective: Create `src/browser/` and implement browser lifecycle management (start/stop).
    - Context: `Cargo.toml`, `src/browser/mod.rs`.
- **Task 1.2: Implementation of Core Actions [4h]** ✅ Completed
    - Objective: Implement `navigate`, `click`, `type`, and `get_content` using CDP.
    - Context: `src/browser/mod.rs`.

### Story 2: Enhanced Perception [3 days]
Give the agent "eyes" to see the page structure.

#### Acceptance Criteria
- [x] `snapshot` action returns a metadata-rich accessibility tree (like OpenClaw).
- [x] `screenshot` action supports capturing specific elements.

#### Atomic Tasks
- **Task 2.1: Semantic Snapshot [3h]** ✅ Completed
    - Objective: Implement a script to extract interactive elements and their accessibility roles.
    - Context: `src/browser/snapshot.js` (embedded), `src/browser/mod.rs`.

### Story 3: Tool Refactoring & Unification [2 days]
Update the tool interface to use the new engine.

#### Acceptance Criteria
- [x] `BrowserTool` uses `ChromiumBackend` by default.
- [x] `browser_open` logic is integrated as a fallback or specific action.

#### Atomic Tasks
- **Task 3.1: Refactor BrowserTool [3h]** ✅ Completed
    - Objective: Replace `AgentBrowser` backend logic with the new native backend.
    - Context: `src/tools/browser.rs`.

## Known Issues
- **Chrome Path Discovery**: Need to robustly find the Chrome/Chromium binary on macOS, Linux, and Windows.

## Dependency Visualization
```
Task 1.1 (Foundation) ──► Task 1.2 (Core Actions) ──► Task 3.1 (Refactor Tool)
                                     │
                                     ▼
                              Task 2.1 (Snapshot)
```
