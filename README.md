<p align="center">
  <img src="zeroclaw.png" alt="ClawPilot" width="180" />
</p>

<h1 align="center">ClawPilot</h1>

<p align="center">
  Workspace-first AI workbench and runtime for supervised local knowledge work.
</p>

> Repo name is `clawpilot`; the CLI/runtime command remains `zeroclaw` for compatibility.

## What ClawPilot is now

ClawPilot combines:
- a Rust runtime/orchestrator (`zeroclaw`) for controlled execution, and
- a Mission Control web workbench that is now centered on **Project Workspaces**.

The main user experience is:
1. choose a workspace,
2. pick or type a goal,
3. monitor progress,
4. review approvals,
5. accept the final deliverable.

## Workspace-first model

A workspace stores:
- root path and metadata,
- workspace-level instructions,
- optional folder-level instruction overlays,
- goals, progress entries, and artifacts.

Runtime runs are created from workspace context (path + instructions), then tracked through status/events/results files.

## Approvals and deliverables

ClawPilot supports approval checkpoints for high-impact actions (for example file edits, shell actions, browser actions, and final deliverables when policy requires it).

Mission Control surfaces pending approvals, review notes, and run state transitions so a human can gate execution.

Deliverables are read from runtime result artifacts and shown in the run review panel.

## Phase 5 knowledge workflows

Mission Control now includes focused preset templates for:
1. folder summarization,
2. file organization / cleanup,
3. document synthesis from local source files,
4. data extraction from messy local files,
5. rerun/refine from prior deliverables.

These are preset goals/instruction templates over existing runtime behavior, not a separate hidden agent stack.

## Run the app

### Runtime (Rust)

```bash
cargo run -- --help
```

### Mission Control (Next.js + Convex)

```bash
cd mission-control
npm install
npx convex dev
npm run dev
```

Optional checks:

```bash
npm run lint
npm test
```

## How ClawPilot still differs from Claude CoWork

ClawPilot is meaningfully closer to a coworker-style desktop flow, but it is still different:
- It is self-hosted/repo-local by default, not a managed Claude product.
- Workflow presets are explicit templates, not opaque product-native workflow engines.
- Collaboration and memory UX are narrower; most state is workspace/run centric and file-backed.
- Some advanced polish (deeper artifact diffing, richer rerun context stitching, broader UX automation) remains future work.

## Repository map

- `src/` Rust runtime, tools, providers, security, orchestration
- `mission-control/` Next.js + Convex workspace workbench
- `docs/` architecture and workflow documentation
