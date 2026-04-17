# `.agent-state/` — Multi-Agent Coordination

This directory is shared scratchpad for any AI coding assistant
(Claude Code, Cursor, Codex, Aider, etc.) working on this repository.

**Why it exists**: you probably use multiple AI tools. Their
conversation histories don't cross-pollinate. Without a shared
coordination surface, each session re-derives context from scratch
and sometimes clobbers in-flight work from another tool.

## Files

| File | Owner | Cadence |
|---|---|---|
| `CURRENT-WORK.md` | **Current** session | Append on entry + exit |
| `DECISIONS.md` | All sessions | Append-only (ADR-lite) |
| `HANDOFF.md` | When pausing mid-task | Overwrite (what next agent needs to know) |

## Protocol for AI assistants

On session start:
1. `git status` — if dirty, read `CURRENT-WORK.md` last entry to see whose work that is.
2. Check `HANDOFF.md` — if non-empty, there's incomplete work. Read, then either continue it or stash to a branch before doing something new.

On session end (or context switch):
1. Append one entry to `CURRENT-WORK.md` with branch, files touched, validation results, next step.
2. If pausing mid-task, also overwrite `HANDOFF.md` with structured checklist.
3. If you made a lasting architectural choice, append to `DECISIONS.md`.

## Notes for humans

- These files are NOT noise. They're how the team (you + N AI assistants) stay in sync.
- Safe to commit. Contains no secrets (enforced by convention).
- If an agent forgets to log, YOU can remind: "update `.agent-state/CURRENT-WORK.md` before ending".
