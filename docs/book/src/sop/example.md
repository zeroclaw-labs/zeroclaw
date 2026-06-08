# Worked Example: The StageX Auto-Update Bot

[stagehand](https://codeberg.org/singlerider/stagehand) is a production ZeroClaw bot. It watches the upstream release feed, bumps a [StageX](https://codeberg.org/stagex/stagex) package, builds it, verifies it reproduces by digest, pushes the change, opens a draft pull request, and announces the result. No human touches it until the PR exists.

It is the reference SOP deployment: the pipeline is a deterministic SOP, the release feed arrives over an AMQP channel, and the agent fires the SOP with the `sop_execute` tool. AMQP is a transport, not a native SOP trigger; only MQTT is wired to a live SOP listener (see [Connectivity](./connectivity.md)). The channel lifts each release into the agent loop, and the agent starts the run. That separation is what makes the pattern reusable.

Every command, config key, tool name, status value, and audit key below maps to a concrete definition in the codebase.

## 1. The Build

stagehand needs the AMQP and Matrix channels compiled in. Both are feature-gated and off by default.

<div class="os-tabs-src">

#### sh

```sh
cargo build --release --features channel-amqp,channel-matrix
```

</div>

The result is a `zeroclaw` binary that loads the `amqp` and `matrix` channel types. A binary built without `channel-amqp` rejects an `amqp` channel block at startup and logs a warning instead of loading it.

## 2. The Artifacts

Three things live under the ZeroClaw install root:

| Artifact | Location | Role |
|---|---|---|
| config | `~/.zeroclaw/` | The agent, the AMQP + Matrix channels, and the `[sop]` block. |
| `sops/stagex-update/` | `<install>/shared/sops/stagex-update/` | The pipeline: `SOP.toml` (metadata) + `SOP.md` (the eight steps). |
| `skills/stagex-update/` | `<install>/shared/skills/stagex-update/` | The glue that fires the SOP on a release event. |

The config wires the agent to two channels (`amqp.anitya`, `matrix.announce`), runs it at full autonomy so it commits, pushes, and opens the PR without a gate, and points `[sop]` at `shared/sops` in `deterministic` execution mode. The agent never merges; a maintainer adopts the branch and merges via signed commit.

### The AMQP channel

The `amqp.anitya` channel consumes Fedora Messaging's public feed. It binds the Anitya version-update routing key on the `amq.topic` exchange and connects over `amqps://` with client mutual TLS; Fedora's broker requires a client certificate, so the channel presents the configured `client_cert` and `client_key`. The channel validates its configuration at load: `amqp_url` must use `amqp://` or `amqps://`, an `amqps://` URL requires `ca_cert`, `client_cert` and `client_key` must be supplied together, the exchange must be non-empty, and at least one routing key must be bound.

Each delivery's JSON body is mapped into the agent's inbound message by `content_template`, whose `{dotted.path}` placeholders resolve against the body, turning a release delivery into *"New release: bzip2 1.0.9 (was 1.0.8). Bump the StageX package for bzip2."* The `thread_id_field` dotted path correlates replies back to the originating event. Credentials and certificates are supplied at deploy time and never committed: the Codeberg push token is an environment variable the agent's shell reads, the Matrix access token is set on the running instance, and the Fedora CA and client certificates are placed on the host.

## 3. Validation

The `zeroclaw sop` surface is three subcommands. There is no `run` subcommand; runs start from a trigger or from the `sop_execute` tool.

<div class="os-tabs-src">

#### sh

```sh
zeroclaw sop list
zeroclaw sop validate stagex-update
zeroclaw sop show stagex-update
```

</div>

Validation surfaces warnings for an empty name or description, no triggers, no steps (a missing or empty `SOP.md`), and step-numbering gaps. A missing-steps warning means the run would fail at execution time. Drive the same checks from the [zerocode](../zerocode/overview.md) terminal interface when iterating; the CLI is the reproducible deploy-time check.

## 4. Deployment

The bot runs as a long-lived daemon so it stays connected to the broker and the Matrix room.

<div class="os-tabs-src">

#### sh

```sh
zeroclaw daemon
```

</div>

On an always-on host it runs as a managed service that restarts with the machine (see [Service & daemon](../ops/service.md)):

<div class="os-tabs-src">

#### sh

```sh
zeroclaw service install
zeroclaw service start
```

</div>

The AMQP channel connects to the broker, binds its routing key, and consumes deliveries. The bot is idle until upstream ships a release.

## 5. A Release Flows Through

Anitya publishes a version-update delivery. The AMQP channel receives it, applies the `content_template`, and hands the agent an inbound message naming the package, the new version, and the old version. The agent fires the pipeline:

```jsonc
// tool: sop_execute
// args: { "name": "stagex-update", "payload": "{\"project\":{\"name\":\"bzip2\"},\"version\":\"1.0.9\",\"old_version\":\"1.0.8\"}" }
```

`sop_execute` starts a `SopRun` with a manual trigger and forwards the `payload` into the run context. The lifecycle from here is identical to any other run; the trigger source is the only thing that differs.

## 6. The Run

Because `[sop]` runs in `deterministic` mode, the steps execute sequentially with no LLM round-trip between them. Each step's output pipes to the next, and only the patch-sourcing step calls the model, which is local, so package source never leaves the host. A checkpoint step pauses for human approval; this pipeline runs straight through to the draft PR.

```
running → completed
```

The eight steps, parsed from the SOP's `## Steps` section:

| # | Step | What it does | Tools |
|---|---|---|---|
| 1 | Resolve | Map the upstream project to the real StageX package; read the current version; stop if not strictly newer. | `shell`, `file_read` |
| 2 | Bump + hash | Set the new version, run `make fetch`, write the correct source hash, re-fetch until clean. | `shell`, `file_write` |
| 3 | Build | Build just this package; retry once on a hash failure. | `shell` |
| 4 | Patch if broken | On a build break, refresh or source a patch using the local model; flag genuine API breaks. | `shell`, `file_read`, `file_write`, `http_request` |
| 5 | Digest repro | `make digests`, build a second time, confirm the digest is unchanged. | `shell` |
| 6 | Commit + push | Commit on a per-package, per-version branch; push to the fork. | `shell`, `git_operations` |
| 7 | Open draft PR | Fill the PR template, attach digests, mark ready only on a clean reproduced build. | `http_request` |
| 8 | Announce | Post the outcome to the Matrix room: package, version delta, repro status, digest, PR URL. | `shell` |

The agent closes out each step with a `sop_advance` call reporting the result:

```jsonc
// tool: sop_advance
// args: { "run_id": "<run-id>", "status": "completed", "output": "Bumped bzip2 1.0.8 → 1.0.9; source hash re-derived and verified." }
```

`status` is one of `completed`, `failed`, or `skipped`. When the final step is advanced, the run transitions to `completed` and its `completed_at` timestamp is set.

Progress is visible from an agent turn at any point:

```jsonc
// tool: sop_status
// args: { "sop_name": "stagex-update", "include_metrics": true }
```

### Headless safety

When a delivery arrives with no agent loop active to drive the steps, the runtime records the run and logs each pending action rather than silently dropping work. The run waits for an agent turn to drive it forward.

## 7. The Audit Trail

`SopAuditLogger` persists every transition into the configured Memory backend under category `sop`. One update run leaves these keys:

| Key | Contents |
|---|---|
| `sop_run_<run-id>` | Full run snapshot, written at start and updated on completion. |
| `sop_step_<run-id>_1` … `_8` | One per-step result: status, output, timestamps. |
| `sop_approval_<run-id>_<step>` | An operator approval record, when a checkpoint step requires one. |
| `sop_timeout_approve_<run-id>_<step>` | A timeout auto-approval record, when a checkpoint approval times out. |

`include_metrics: true` on `sop_status` adds SOP-specific aggregates; `include_gate_status: true` adds trust-phase and gate-evaluator state. These come through `sop_status`, not Prometheus. The `/metrics` endpoint, when the observability backend is `prometheus`, exposes only the general `zeroclaw_*` families.

## 8. The Guarantees

Each guarantee traces to the pipeline:

- **Source never leaves the host.** The patch-sourcing step runs against a local model, so package source never reaches a remote provider.
- **The build proves itself.** Step 5 builds twice and compares digests; the PR is marked ready only when both match.
- **A human owns the merge.** The bot stops at "draft PR opened"; a maintainer adopts the branch and merges via signed commit. It never auto-merges.
- **The run is reconstructable.** The run snapshot and every step result are persisted under category `sop`, keyed by run ID.

## 9. The Pattern

An inbound channel ingests an event, the agent fires an SOP with `sop_execute`, and a deterministic pipeline does the work. Swap the AMQP feed for any channel and the steps for any procedure, and the lifecycle, approval gates, and audit keys are identical. Where a step needs human judgment, mark it a checkpoint and the run pauses for an approval before continuing; the only difference from the unattended path is who advances the run.
