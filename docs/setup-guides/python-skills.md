---
title: "Running Python Skills"
description: "Configure ZeroClaw to run skills that invoke the Python interpreter (and other interpreted languages like R, Julia, Node) on your host."
---

# Running Python Skills

ZeroClaw's default skill sandbox is an ephemeral `alpine:latest` container with `--network none` and `--read-only` rootfs. That sandbox is excellent for static-shell skills but cannot run Python (or R, Julia, Node) scripts out of the box because:

1. `alpine:latest` ships without `python3`
2. The container's rootfs is read-only, so runtime `pip install` is blocked
3. The workspace directory isn't bind-mounted by default, so the interpreter can't see your skill's script files
4. Network is disabled, so scripts that fetch from external APIs fail

You have two patterns to work around this. Pick whichever fits your deployment.

## Pattern A — Native execution (backend = "none")

Best for: trusted dev environments, home labs, single-user boxes where you're not worried about skill isolation.

Set both:

```toml
# ~/.zeroclaw/config.toml

[runtime]
kind = "native"

[security.sandbox]
backend = "none"
enabled = false
```

Under this config, ZeroClaw runs skill subprocesses directly on the host. The `allowed_commands` list in your profile config (`scripts/rpi-config.toml` or the profile you copied from) is still the enforcement layer — it controls which binaries can be invoked. Make sure `python3` is in that list:

```toml
allowed_commands = [
  "python3",
  # ...
]
```

**What you give up**: no container isolation for the skill's subprocess. Allowlist enforcement and filesystem permissions are your only guards. Don't use this pattern for untrusted third-party skills.

**What you gain**: zero container overhead. On a Raspberry Pi 4 this is a ~500 ms reduction in wall-clock time per skill invocation vs. the Docker path. Your host's Python / R / whatever — with their installed packages — is what runs.

## Pattern B — Custom skill-exec Docker image

Best for: multi-tenant deployments, production, any scenario where you want strong isolation without trusting the skill's code.

Starter Dockerfile for a Python skill-exec image:

```dockerfile
# Dockerfile.skill-exec
FROM python:3.12-alpine

# Add interpreter-specific deps your skills need.
# Keep this list tight — every package adds attack surface.
RUN pip install --no-cache-dir \
    polars \
    pandas \
    requests \
    numpy

# Optional: create a non-root user for the skill to run as.
# ZeroClaw's DockerSandbox will set uid=0 by default inside the container;
# if you want stricter isolation, add `USER skill` here and a matching
# user create step.

WORKDIR /workspace
```

Build and tag:

```console
$ docker build -f Dockerfile.skill-exec -t my-org/zeroclaw-skill-exec:latest .
$ docker push my-org/zeroclaw-skill-exec:latest    # if using a remote registry
```

Point ZeroClaw at it:

```toml
# ~/.zeroclaw/config.toml

[runtime]
kind = "native"   # or "docker"; see below

[security.sandbox]
backend = "docker"
image = "my-org/zeroclaw-skill-exec:latest"
```

**Network access**: the default `DockerSandbox` launches with `--network none`. If your skill fetches from external APIs (market data, language-server APIs, etc.), either:

- Accept that fetches have to happen in the orchestration layer (native Rust code in the ZeroClaw daemon) before handing data to the skill, OR
- Relax the network via config (pending upstream support — see #5720).

**Workspace access**: skills need their script files readable and their output directory writable. Workspace bind-mount support is tracked in #5720.

**Multi-arch images**: if your user base includes both Raspberry Pi (aarch64) and x86_64 servers, build with `docker buildx`:

```console
$ docker buildx build --platform linux/amd64,linux/arm64 \
    -f Dockerfile.skill-exec \
    -t my-org/zeroclaw-skill-exec:latest \
    --push .
```

## Which pattern should you pick?

| Scenario | Pattern |
|---|---|
| Dev machine, trusted skills only | A (native) |
| Home lab, you wrote your own skills | A or B, preference |
| Production agent for a team | B (custom image) |
| Multi-tenant SaaS | B + additional per-tenant controls |
| Air-gapped industrial edge device | B, image built offline |
| Tight resource envelope (Pi Zero, ESP32-class) | A |

## Related issues

- **#5719** — `runtime.kind = "native"` doesn't bypass Docker in auto-detection. Affects users on Pattern A who rely on auto-detection.
- **#5720** — PYTHONPATH env prefix handling and DockerSandbox workspace mount. Affects users on Pattern B who hit path translation issues.
- **#5722** — tracking issue for this documentation and related Python-skill UX work.

## References

- `crates/zeroclaw-runtime/src/security/docker.rs` — DockerSandbox implementation
- `crates/zeroclaw-runtime/src/security/detect.rs` — sandbox auto-detection
- `scripts/rpi-config.toml` — default Raspberry Pi profile config (reference allowed_commands list)
