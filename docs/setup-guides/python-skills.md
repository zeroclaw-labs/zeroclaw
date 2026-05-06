# Running Python Skills

ZeroClaw's default skill sandbox is an ephemeral `alpine:latest` container with `--network none` and read-only rootfs. That sandbox is excellent for static-shell skills but cannot run Python (or R, Julia, Node) scripts out of the box because:

1. `alpine:latest` ships without `python3`.
2. The container's rootfs is read-only, so runtime `pip install` is blocked.
3. The workspace directory isn't bind-mounted by default, so the interpreter can't see your skill's script files.
4. Network is disabled, so scripts that fetch from external APIs fail.

This guide covers the configuration surface that governs Python skill execution, what's intentionally blocked, and the two deployment patterns that work today.

## Configuration surface

Python skill execution is gated by three independent layers. All three must cooperate.

### 1. The `skills.allow_scripts` master gate

Script execution is **off by default**. Without this flag the runtime refuses to invoke any script-bearing skill, regardless of sandbox backend.

```toml
# ~/.zeroclaw/config.toml
[skills]
allow_scripts = true
```

Set via the CLI:

```console
$ zeroclaw config set skills.allow_scripts true
```

Or toggle for a one-off session via environment variable (CI, test harnesses):

```console
$ ZEROCLAW_SKILLS_ALLOW_SCRIPTS=1 zeroclaw daemon
```

### 2. The `allowed_commands` allowlist

The policy layer enforces a per-autonomy-level allowlist of command names. `python3` must be present for Python scripts to run.

```toml
[security.autonomy.<level>]
allowed_commands = [
  "python3",
  "git",
  "cargo",
  # ...
]
```

`<level>` matches whichever autonomy level is active (`read_only`, `assist`, `autonomous`, etc.). Inspect your active config:

```console
$ zeroclaw config list --filter security.autonomy
$ zeroclaw config get security.autonomy.assist.allowed_commands
```

The policy layer strips leading `KEY=VAL` env-var prefixes before matching, so patterns like:

```
PYTHONPATH=/opt/mylib python3 script.py
```

are matched against `python3` (not `PYTHONPATH=/opt/mylib`), and work correctly.

### 3. The sandbox backend

The sandbox wraps the script invocation once it clears policy. Two backends are viable for Python (see Pattern A and Pattern B below).

### Introspection

Use `zeroclaw config list` to audit what's loaded from your `config.toml`:

```console
$ zeroclaw config list --filter skills
$ zeroclaw config list --filter security
$ zeroclaw config schema > schema.json    # full JSON schema
```

## What's intentionally blocked

Even with `skills.allow_scripts = true` and `python3` in `allowed_commands`, the policy layer blocks **inline evaluation** arguments:

- `python3 -c '<code>'` — blocked
- `python3 -m <module>` — blocked
- Equivalent `node -e`, `ruby -e`, `perl -e` patterns — blocked

This is intentional. Inline-eval arguments are a common prompt-injection vector (the LLM emits a one-liner that the sandbox would otherwise execute verbatim). The policy layer forces scripts to live on disk where they're visible to audit tooling. Workaround if you genuinely need one-shot evaluation: write a small script file and invoke it by path.

## Pattern A — Native execution (backend = "none")

Best for: trusted dev environments, home labs, single-user boxes where you're not worried about skill isolation beyond the policy allowlist.

Set all three:

```toml
# ~/.zeroclaw/config.toml

[skills]
allow_scripts = true

[runtime]
kind = "native"

[security.sandbox]
backend = "none"
enabled = false
```

Under this config ZeroClaw runs skill subprocesses directly on the host. Your host's Python (and its installed packages) is what runs — no container layer.

**What you give up:** no container isolation for the skill subprocess. The policy allowlist + filesystem permissions are your only guards. Don't use this pattern for untrusted third-party skills.

**What you gain:** zero container overhead. On a Raspberry Pi 4 this is a ~500 ms reduction in wall-clock time per skill invocation vs. the Docker path.

## Pattern B — Custom skill-exec Docker image

Best for: multi-tenant deployments, production, any scenario where you want container isolation without trusting the skill's code.

Starter Dockerfile:

```dockerfile
# Dockerfile.skill-exec
FROM python:3.12-alpine

# Keep this list tight — every package adds attack surface.
RUN pip install --no-cache-dir \
    polars \
    pandas \
    requests \
    numpy

WORKDIR /workspace
```

Build and tag:

```console
$ docker build -f Dockerfile.skill-exec -t my-org/zeroclaw-skill-exec:latest .
$ docker push my-org/zeroclaw-skill-exec:latest    # if using a remote registry
```

Point ZeroClaw at it:

```toml
[skills]
allow_scripts = true

[runtime]
kind = "native"

[security.sandbox]
backend = "docker"
image = "my-org/zeroclaw-skill-exec:latest"
```

**Network access:** `DockerSandbox` launches with `--network none` by default. If your skill fetches from external APIs, either fetch in the orchestration layer (native Rust code in the daemon) before handing data to the skill, or build network-access configuration into your deployment.

**Workspace access:** skills need their script files readable and their output directory writable. Workspace bind-mount support landed in PR #5905 — the `DockerSandbox` now emits `-v <workspace>:<workspace>:ro` when a workspace is configured, keeping paths stable inside and outside the container.

**Multi-arch images:** for mixed Raspberry Pi (aarch64) + x86_64 fleets, use `docker buildx`:

```console
$ docker buildx build --platform linux/amd64,linux/arm64 \
    -f Dockerfile.skill-exec \
    -t my-org/zeroclaw-skill-exec:latest \
    --push .
```

## Which pattern should I pick?

| Scenario | Pattern |
|---|---|
| Dev machine, trusted skills only | A (native) |
| Home lab, skills you wrote yourself | A or B, preference |
| Production agent for a team | B (custom image) |
| Multi-tenant SaaS | B + additional per-tenant controls |
| Air-gapped industrial edge device | B, image built offline |
| Tight resource envelope (Pi Zero, ESP32-class) | A |

## Future patterns

The `SandboxBackend` enum in `crates/zeroclaw-runtime/src/security/` is a small trait-based abstraction; adding new backends requires only a new enum variant and a `Sandbox` impl that wraps the command — architecturally identical to the existing Docker / Firejail / Landlock backends, with no Rust-level coupling. Projects like [gVisor](https://github.com/google/gvisor) (application-kernel sandbox), [Kata Containers](https://github.com/kata-containers/kata-containers) (lightweight VM isolation), and [NemoClaw](https://github.com/NVIDIA/NemoClaw) (agentic-skill sandboxing over OpenShell) fit this pattern naturally; community contributions adding them as `SandboxBackend::` variants are welcome.

## Related issues

- **#5719** — `runtime.kind = "native"` didn't bypass Docker in sandbox auto-detection. Resolved by #5904 (merged).
- **#5720** — workspace bind-mount + `PYTHONPATH` env-prefix handling. Workspace mount landed in #5905; env-prefix already works via `skip_env_assignments` (see Configuration surface → `allowed_commands`).
- **#5722** — tracking issue for this documentation.

## References

- `crates/zeroclaw-runtime/src/security/docker.rs` — `DockerSandbox` with workspace bind-mount
- `crates/zeroclaw-runtime/src/security/detect.rs` — sandbox auto-detection with `runtime.kind` awareness
- `crates/zeroclaw-config/src/policy.rs` — `skip_env_assignments`, `strict_inline_eval`, allowlist matching
- `crates/zeroclaw-config/src/schema.rs` — `SkillsConfig::allow_scripts`, `AutonomyConfig::allowed_commands`
- `scripts/rpi-config.toml` — default Raspberry Pi profile config (reference allowed_commands list)
