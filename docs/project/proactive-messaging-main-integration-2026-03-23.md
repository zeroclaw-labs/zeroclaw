# Proactive Messaging -> Main Integration Plan

Date: 2026-03-23

## Objective

Promote the `feat/proactive-messaging` branch from the de facto deployment line for the `ai-agents` environment into the canonical `main` branch without losing newer `origin/main` work.

This is an integration task, not a branch replacement task.

## Current State

- The live `ai-agents/zeroclaw` workload runs `zeroclaw 1.4.15`.
- The `1.4.15` version bump commit exists only on `feat/proactive-messaging`.
- The live Sam and Walter sandbox manifests map to branch-owned `k8s/sam/**` and `k8s/walter/**` files.
- `feat/proactive-messaging` is `103` commits ahead of `origin/main`.
- `origin/main` is `2402` commits ahead of `feat/proactive-messaging`.
- Local `main` was stale and mis-tracked to `upstream/main`; the practical integration base is `origin/main`.

## Decision

Treat `feat/proactive-messaging` as the operational source of truth for the deployed agent environment, but not as the overall repository baseline.

The correct end state is:

1. Merge `feat/proactive-messaging` into current `origin/main`.
2. Resolve the limited conflict set deliberately.
3. Validate runtime, channel, ACP, MCP transport, and Kubernetes behavior.
4. Rebuild and redeploy from `main`.
5. Retire `feat/proactive-messaging` after production confirmation.

## Branch-Only Capability Map

The branch-only history from 2026-03-03 through 2026-03-21 includes:

- proactive messaging queueing and guardrails
- ACP-over-HTTP server/client lifecycle
- conversation recovery and mid-task injection
- tool output presentation layer
- Signal attachment, edit, and draft improvements
- malformed tool-call sanitization
- image marker extraction fixes
- Sam and Walter Kubernetes environments
- Vault, sandbox, and network policy wiring
- Vikunja and cron-management skills

Branch-only release trail:

- `1.4.10` at `5694f934`
- `1.4.11` at `930b6082`
- `1.4.12` at `2d6e33fc`
- `1.4.15` at `9b078cb6`

## Merge Feasibility

A synthetic merge of `feat/proactive-messaging` into `origin/main` applied broadly and produced conflicts in only four files:

- `src/agent/loop_.rs`
- `src/agent/loop_/history.rs`
- `src/channels/mod.rs`
- `src/tools/mcp_transport.rs`

This indicates the integration is feasible as a normal merge PR, not a replay or branch reset.

## Conflict Resolution Rules

### `src/agent/loop_.rs`

- Preserve current `origin/main` cancellation and timeout behavior.
- Reintroduce branch-side recovery-aware invocation logic.
- Keep injection handling and recovery attempts compatible with current agent-loop structure.
- Avoid duplicate timeout/retry layers.

### `src/agent/loop_/history.rs`

- Preserve current `origin/main` history model changes.
- Keep branch-side malformed `tool_call` sanitization.
- Ensure storage and replay paths still support current mainline message semantics.

### `src/channels/mod.rs`

- Preserve all current `origin/main` channel registrations.
- Re-add branch-side injection support and Signal integration points.
- Keep exports stable for downstream callers.

### `src/tools/mcp_transport.rs`

- Preserve current `origin/main` MCP transport structure.
- Keep session ID persistence only once.
- Do not leave duplicate header parsing or divergent request-flow behavior.

## Execution Plan

### Phase 1: Prepare Integration Branch

1. Create a clean worktree from `origin/main`.
2. Create branch `integration/proactive-messaging-into-main`.
3. Merge `feat/proactive-messaging` with `--no-ff`.

### Phase 2: Resolve Merge Conflicts

1. Resolve the four conflict files with mainline-first semantics plus branch capabilities.
2. Review adjacent files to ensure the merged code paths remain coherent.

### Phase 3: Subsystem Review

Review and validate at least the following files and areas:

- `src/agent/loop_.rs`
- `src/agent/recovery.rs`
- `src/proactive_messaging/**`
- `src/channels/signal.rs`
- `src/channels/injection.rs`
- `src/gateway/acp_server.rs`
- `src/tools/mcp_transport.rs`
- `src/tools/browser.rs`
- `src/tools/image_info.rs`
- `src/tools/send_user_message.rs`
- `src/tools/manage_outbound_queue.rs`
- `k8s/sam/**`
- `k8s/walter/**`

### Phase 4: Normalize Version and Image Provenance

1. Choose the post-merge release version.
2. Make `Cargo.toml`, image tags, and runtime `--version` agree.
3. Fix the current Walter image/tag mismatch before future releases rely on tags as truth.
4. Add commit/digest traceability to container builds if missing.

### Phase 5: Validation

Run:

```bash
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test
```

Focused validation:

- ACP session lifecycle
- message injection flow
- Signal edit/draft behavior
- malformed tool-call sanitization
- image marker extraction
- MCP session continuity
- Kubernetes manifest sanity for Sam and Walter

### Phase 6: Deployment

1. Build merged images from the integration branch.
2. Validate in a non-destructive environment if available.
3. Deploy to `ai-agents`.
4. Confirm pod image tag, digest, and in-container version all match the merged `main` lineage.

### Phase 7: Promotion and Retirement

1. Open PR from the integration branch to `main`.
2. Merge after validation and review.
3. Rebuild production images from `main`.
4. Confirm live workloads are running `main`-derived images.
5. Delete `feat/proactive-messaging` after the rollback window closes.

## Rollback

- Revert the integration PR commit if runtime behavior regresses.
- Redeploy the last known good image digest for Sam and Walter.
- Do not delete `feat/proactive-messaging` until merged `main` has been validated in production.

## Working Notes

- `ai-agents/zeroclaw` currently runs `citizendaniel/zeroclaw-sam:v1.4.15` and reports `zeroclaw 1.4.15`.
- `ai-agents/zeroclaw-k8s-agent` currently runs image tag `v1.4.12` but reports `zeroclaw 1.4.11`, so image tags are not fully trustworthy.
- Promotion should preserve the deployed branch capabilities while inheriting current `origin/main` security and platform work.
