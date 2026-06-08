# Running Python Skills

ZeroClaw can run Python skills, but realistic Python work usually needs one of two explicit deployment choices:

- run the skill on a trusted host Python environment, or
- run it inside a custom Docker runtime image that already contains Python and the packages the skill needs.

The default configuration is intentionally conservative. It blocks many copy-paste Python patterns until you decide which trust boundary you want.

This page covers Python scripts invoked through the built-in shell tool. If a `SKILL.toml` defines its own `[[tools]]` entry with `kind = "shell"` or `kind = "script"`, that skill tool currently executes as a host subprocess under shell policy, not through `runtime.kind = "docker"`. For containerized Python execution today, either have the skill instructions call Python scripts through the built-in shell tool, or make the skill tool command explicitly run the container boundary you want.

## The Three Layers

Python skill execution is controlled by three separate layers.

| Layer | Config surface | What it decides |
|---|---|---|
| Skill audit | `[skills].allow_scripts` | Whether shell-like helper files can load from a skill package. Python `.py` helpers are allowed by default. |
| Shell policy | `[risk_profiles.<alias>].allowed_commands` | Whether the shell tool may invoke `python`, `python3`, `pip`, or another executable. |
| Execution boundary | `[risk_profiles.<alias>].sandbox_*` and `[runtime]` | Where the allowed command actually runs, and what filesystem, network, and resource limits apply. |

Python helper files do not require `allow_scripts = true`. Enable shell-like helper files only after you have reviewed the skill source:

```toml
[skills]
allow_scripts = true
```

Allow the interpreter in the risk profile used by the agent:

```toml
[agents.assistant]
risk_profile = "assistant"

[risk_profiles.assistant]
allowed_commands = ["python3", "python"]
```

`allowed_commands` is a strict executable allowlist when it is non-empty. The shell policy still checks destructive patterns and interpreter argument risks on top of that allowlist.

Prefer installing Python packages at image build time, in a reviewed local virtual environment, or in another setup step outside the agent turn. Add `pip` to a trusted profile only when runtime package installation is an intentional part of that deployment.

## What Stays Blocked

ZeroClaw deliberately blocks inline interpreter execution such as:

<div class="os-tabs-src">

#### sh

```sh
python3 -c 'print("hello")'
python3 -m http.server
python3 -m pip install requests
node -e 'console.log(process.env)'
```

</div>

For Python skills, put code in an auditable script file and run that file:

<div class="os-tabs-src">

#### sh

```sh
python3 skills/portfolio/run.py
```

</div>

This makes the executable file reviewable by the skill audit path and avoids turning a shell command string into an arbitrary code container.

Environment-variable prefixes such as `PYTHONPATH=... python3 script.py` are also policy-sensitive. Prefer a wrapper script, a project-local virtual environment, or explicit configuration inside the script when you need stable runtime environment setup.

## Pattern A: Trusted Native Python

Use native execution when the skills are trusted and you want them to use the host's Python installation, packages, filesystem permissions, and network.

```toml
[agents.assistant]
risk_profile = "assistant"

[runtime]
kind = "native"

[risk_profiles.assistant]
level = "supervised"
allowed_commands = ["python3", "python"]
sandbox_enabled = false
sandbox_backend = "none"
```

This is appropriate for local development, a single-user workstation, or a home lab where you wrote the skill. It removes OS-level sandboxing for tool runs under that profile, so normal user permissions and ZeroClaw policy checks are the remaining guardrails.

Do not use this pattern for unreviewed third-party skills or multi-tenant deployments.

## Pattern B: Custom Docker Runtime Image

Use Docker when you want Python dependencies to live in a repeatable container image and you still want a runtime boundary around built-in shell execution.

Create an image with the packages your skills need:

```dockerfile
# Dockerfile.skill-exec
FROM python:3.12-slim

RUN pip install --no-cache-dir \
    pandas \
    polars \
    requests

WORKDIR /workspace
```

Build it:

<div class="os-tabs-src">

#### sh

```sh
docker build -f Dockerfile.skill-exec -t zeroclaw-python-skills:local .
```

</div>

Point ZeroClaw at the image:

```toml
[agents.assistant]
risk_profile = "assistant"

[runtime]
kind = "docker"

[runtime.docker]
image = "zeroclaw-python-skills:local"
network = "none"
read_only_rootfs = true
mount_workspace = true

[risk_profiles.assistant]
level = "supervised"
allowed_commands = ["python3", "python"]
sandbox_enabled = false
sandbox_backend = "none"
```

`runtime.kind = "docker"` runs shell invocations in an ephemeral container. Docker-specific image, network, memory, CPU, read-only rootfs, and workspace mount settings live under `[runtime.docker]`.

The `sandbox_backend = "none"` line avoids wrapping the Docker runtime in a second, separate sandbox container. In this pattern the Docker runtime is the execution boundary for built-in shell invocations, and `[runtime.docker]` is where the image and container limits are configured.

If a skill needs outbound HTTP, change `runtime.docker.network` deliberately, for example:

```toml
[runtime.docker]
network = "bridge"
```

If a skill needs to write package caches, reports, or temporary state outside the mounted workspace, review whether it should instead write under `/workspace`, then relax `read_only_rootfs` only when that is not enough.

## Workspace Mounts

When `runtime.docker.mount_workspace = true`, ZeroClaw mounts the configured workspace at `/workspace` in the container and sets the container workdir there. Skill scripts should use workspace-relative paths whenever possible.

If your workspace path must be constrained further, configure:

```toml
[runtime.docker]
allowed_workspace_roots = ["/srv/zeroclaw-workspaces"]
```

ZeroClaw validates the host workspace path against that allowlist before adding the Docker volume mount.

## Choosing a Pattern

- Use trusted native Python when you wrote or reviewed the skills and want the lowest latency on a single-user host.
- Use a custom Docker runtime image when you need repeatable dependencies, production packaging, or an explicit container boundary for built-in shell calls.
- Use stricter risk profiles, narrower command allowlists, and containerized execution for unreviewed or multi-tenant skill sources.

## See Also

- [Skills](./skills.md)
- [Autonomy levels](../security/autonomy.md)
- [Sandboxing](../security/sandboxing.md)
- [Docker & containers](../setup/container.md)
