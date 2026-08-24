# zeroclaw-bootstrap — client guidance

`zeroclaw-bootstrap` is the small distribution client a model-driven harness
(Claude Code, Codex, or any MCP host) runs to reach a configurable ZeroClaw
instance on a host where the ZeroClaw binary may not exist yet. An MCP server
inside the ZeroClaw binary cannot install that binary when it is absent; this
launcher closes that gap and nothing more. It is not a second configuration
service: it never reads or writes `config.toml`, holds no config schema or
provider catalog, and cannot approve a proposal.

This document is the canonical entry-point routing the harness follows. It
describes only what the built launcher and the control binary actually do; it
duplicates no schema, so it does not drift when either surface changes.

## The entry-point decision

The first-run question is a single two-branch route: **is ZeroClaw installed?
If it is, configure it; if it is not, install it, then configure it.** Run
`status` first and let its answer choose the branch.

1. **Run `zeroclaw-bootstrap status`.** It detects the host target, whether a
   verified ZeroClaw binary is already installed, and prints the recommended
   next step. Read its machine-readable `next action` line:
   - `next action  configure` — ZeroClaw is installed and verified. Go to the
     installed branch.
   - `next action  install` — ZeroClaw is absent (or a file is present but its
     identity could not be verified, i.e. a repair). Go to the not-installed
     branch.

2. **Not installed → `plan`, human approval, `install`, then `handoff`.**
   - Run `zeroclaw-bootstrap plan`. It selects one pinned artifact and prints
     the version, channel, source URL, expected artifact digest, signature
     status, install path, privilege, and an `--approve <plan-digest>` token.
   - **Show the human that plan and get an explicit decision.** Installation
     changes executable state and always requires a human decision. The
     `--approve` token is a hash of the exact plan, so a model cannot satisfy
     the decision by asserting approval — a human has to copy the digest across.
   - Run `zeroclaw-bootstrap install --approve <plan-digest>`. It downloads
     exactly the approved immutable artifact, verifies its digest before writing
     anything, installs it under the per-user location, and prints the installed
     binary's SHA-256.
   - Then continue to the installed branch (`handoff`).

3. **Installed → `handoff`.** Run
   `zeroclaw-bootstrap handoff --expect-binary-sha256 <sha>` (the digest `install`
   printed; omit it after a `status`-verified install). Handoff verifies the
   installed server's product version, control-protocol range, capability
   digest, and executable identity, then `exec`s `zeroclaw control --mcp` — the
   read-only control/management surface. That is the configure destination.

4. **After handoff you are connected to the control server. Configure there.**
   Handoff hands the session to `zeroclaw control --mcp`. Configuration happens
   on that surface, not in this launcher:
   - If the instance is **not yet a managed trust root**, the first configure
     step is trust-root genesis: `zeroclaw control genesis`. Genesis seals the
     host key, registers the first operator, assigns the instance identity, and
     writes the immutable genesis record. It leaves mutations disabled.
   - If the instance **already has a trust root**, inspect and configure through
     the control surface (register an external client with
     `zeroclaw control register-client`, review agents, providers, and other
     managed state).
   - **The control surface is read-only by default.** Mutations require the
     separate operator enablement ceremony (`zeroclaw control enable-mutations`),
     which needs a high-assurance operator backchannel and an approval-signing
     key source outside the requester tool surface. "Configure" here means
     "connect to the management surface and set up" — never "mutate without
     approval".

## The four launcher operations

| Operation | Effect |
|---|---|
| `status` | Detect the platform, an existing binary, and its verified version; print the next action (`configure` or `install`) and route |
| `plan` | Select one supported artifact and show version, channel, source, digest, signature status, install path, and privilege, plus the `--approve` token |
| `install` | Download and install exactly the approved immutable artifact (`--approve <plan-digest>` is the human decision) |
| `handoff` | Verify the installed control server's initialization identity, then exec `zeroclaw control --mcp` |

## Invariants the harness can rely on

- **The launcher never edits configuration.** It cannot read or write
  `config.toml`, collect provider credentials, initialize the management trust
  root, or approve anything. All configuration is done on the installed control
  surface after handoff.
- **Installation always needs a human decision.** `install` refuses without the
  `--approve` plan digest, and that digest is not derivable from the request.
- **No arbitrary inputs.** The launcher accepts no download URL, install root,
  release asset name, shell command, or config path. The origin is pinned at
  compile time; the target and artifact mapping is generated from the canonical
  distribution registry; the install directory is derived from the platform
  family.
- **No silent replacement.** An existing binary whose identity cannot be
  verified yields a repair recommendation, never an overwrite or an execution.
- **Digest verification, not signature verification.** The artifact is hashed
  and compared against the release checksum manifest before installation.
  Release SLSA provenance is published but not verified by this launcher; `plan`
  prints the out-of-band `gh attestation verify` command.

## Reference

The control plane, trust-root genesis, client registration, and operator
enablement are specified in the control-plane architecture document
(`docs/book/src/architecture/chat-management-control-plane.md`, sections
"Bootstrap before ZeroClaw exists" and "Skill, plugin, and Zerona"). The
control subcommands named here (`genesis`, `register-client`,
`enable-mutations`, `--mcp`) belong to the installed `zeroclaw control` binary,
not to this launcher.
