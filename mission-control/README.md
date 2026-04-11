# ClawPilot Workbench (Mission Control)

Mission Control is now a **workspace-first workbench** for supervised knowledge work.

Primary flow:
1. Select a Project Workspace
2. Pick a workflow preset or type a goal
3. Monitor runtime progress
4. Review approvals
5. Receive the deliverable

## Phase 5 workflow presets

The workbench now ships focused presets for:
- Folder summarization
- File organization / cleanup
- Document synthesis from local files
- Data extraction from messy local files
- Task rerun/refine from prior deliverables

These presets are templates over the existing runtime queue + results bridge. They do not claim new runtime capabilities beyond what the current toolchain can execute.

## Setup

1. Install dependencies:

```bash
npm install
```

2. Start Convex in this folder:

```bash
npx convex dev
```

3. Start Next.js:

```bash
npm run dev
```

4. Optional checks:

```bash
npm run lint
npm run test
```

`npm run test` executes both `tests/*.test.js` and `lib/*.test.js`.

Set `NEXT_PUBLIC_CONVEX_URL` if your Convex URL differs from the default local URL.
