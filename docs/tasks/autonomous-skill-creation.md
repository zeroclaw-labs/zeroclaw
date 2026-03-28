# Feature Plan: Autonomous Skill Creation

This document outlines the architectural changes and implementation tasks to enable ZeroClaw to autonomously create, test, and register new skills, achieving "Level 3 Autonomy" parity with OpenClaw.

## Epic Overview

- **User Value**: Allows the agent to expand its own capabilities on-the-fly. If an agent encounters a problem it can't solve with current tools, it can "teach itself" a new skill by writing a script and documentation.
- **Success Metrics**: 
    - Agent can generate a valid `SKILL.md` and associated `scripts/`.
    - Agent can run a "Dry Run" or "Test" of the new skill in a sandbox.
    - Agent can register the skill so it's available in future turns.
- **Scope**: Implementing a `SkillDeveloper` tool that manages the `.gemini/skills/` directory and interfaces with the internal skill loading system.
- **Constraints**: Must ensure security (sandboxing) and prevent the agent from breaking its own core runtime.

## Architecture Decisions

### ADR 009: Self-Managed Skill Directory
- **Context**: ZeroClaw loads skills from standard locations. We need a safe "incubation" area for agent-created skills.
- **Decision**: Use `.gemini/skills/autonomous/` as the default location for agent-generated skills.
- **Rationale**: Keeps human-authored skills separate from machine-authored ones, allowing for easier auditing and cleanup.

### ADR 010: Test-Before-Install Workflow
- **Context**: A bug in an autonomous skill could crash the agent.
- **Decision**: Force a mandatory validation/test step before a skill is moved from "incubation" to "active."
- **Rationale**: Ensures the agent verifies its own "learning" before committing to it.

## Story Breakdown

### Story 1: Skill Development Tooling [1 week]
Build the tools the agent needs to write its own code.

#### Acceptance Criteria
- [ ] `SkillDeveloperTool` can create directory structures.
- [ ] Supports writing `SKILL.md` with correct YAML frontmatter.
- [ ] Can execute a "Self-Audit" on a generated skill.

#### Atomic Tasks
- **Task 1.1: Implement SkillDeveloperTool [4h]**
    - Objective: Create a tool that allows file operations restricted to the `skills/` directory.
    - Context: `src/tools/skills.rs`.
- **Task 1.2: Skill Validation Logic [2h]**
    - Objective: Implement an internal validator that checks for YAML errors and required sections.
    - Context: `src/skills/mod.rs`.

### Story 2: Autonomous Registry Integration [1 week]
Enable the agent to reload its capabilities without a restart.

#### Acceptance Criteria
- [ ] Agent can trigger a "Hot Reload" of the skill registry.
- [ ] New tools provided by the autonomous skill become visible in the same session.

#### Atomic Tasks
- **Task 2.1: Hot Reloading Registry [4h]**
    - Objective: Change `ToolRegistry` to allow dynamic addition of new tools during a session.
    - Context: `src/agent/loop_.rs`, `src/tools/mod.rs`.

## Known Issues
- **Infinite Loops**: A generated skill might trigger an infinite agent loop. Need `max_recursion_depth` guards.

## Context Preparation Guide
- **Files to load**: `src/tools/mod.rs`, `src/skills/mod.rs`, `src/agent/loop_.rs`.
- **Concepts**: Dynamic Dispatch, Hot Reloading, File I/O.
