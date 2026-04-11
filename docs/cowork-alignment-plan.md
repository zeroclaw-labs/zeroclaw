# Phase 1 — Mission Control → Project Workspace Alignment Plan

## Goal
Shift Mission Control from a broad dashboard into a **Project Workspace-centered** workflow with a simple, deterministic primary path:

1. choose workspace
2. enter goal
3. view progress area
4. view changed files/artifacts area

## Scope (Phase 1 only)

- Introduce a first-class Project Workspace domain model in Convex.
- Add instruction layers:
  - global instructions (workspace-level)
  - folder instructions (path-scoped within workspace)
- Restructure the Next.js UI to prioritize the workspace-first flow.
- Keep implementation incremental and compilable.
- Avoid wiring the full Rust runtime live bridge in this phase.

## Data model changes

New first-class entities:

- `projectWorkspaces`
  - identity + metadata (`name`, `slug`, `rootPath`, `description`)
  - `globalInstructions`
  - `active` marker for default selection
- `workspaceGoals`
  - user-entered goals by workspace
  - status lifecycle (`queued`, `in_progress`, `blocked`, `done`)
- `workspaceProgress`
  - progress feed items tied to goals/workspace
- `workspaceArtifacts`
  - changed files/artifacts timeline for workspace output review
- `folderInstructions`
  - directory-specific instruction overlays by workspace and path

## UX structure changes

Mission Control page becomes a focused workspace console:

- **Step 1**: Workspace picker + workspace details
- **Step 2**: Goal entry composer
- **Step 3**: Progress stream panel
- **Step 4**: Changed files/artifacts panel
- Support panels:
  - Global Instructions editor
  - Folder Instructions editor/list
  - Recent Activity (demoted, lower priority)

## Out of scope (Phase 1)

- No end-to-end runtime bridge to Rust execution loop.
- No automated file diff ingestion from runtime.
- No advanced goal orchestration (multi-agent planner, retries, scheduling graph).

## Runtime wiring needed later (for Phase 2+)

- Connect goal submission to Rust runtime job/session creation.
- Stream runtime events into `workspaceProgress`.
- Publish changed file/artifact events from runtime into `workspaceArtifacts`.
- Apply global/folder instruction context when constructing runtime prompts.

## Risk and rollback

- Risk: existing broad dashboard flow is replaced in UI.
- Mitigation: keep data/API changes isolated to Mission Control Convex layer.
- Rollback: revert Mission Control app + Convex schema/functions in a single commit.
