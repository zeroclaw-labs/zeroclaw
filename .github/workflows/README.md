# Workflow Directory Layout

GitHub Actions only loads workflow entry files from:

- `.github/workflows/*.yml`
- `.github/workflows/*.yaml`

Subdirectories are not valid locations for workflow entry files.

Repository convention:

1. Keep runnable workflow entry files at `.github/workflows/` root.
2. Keep cross-tooling/local CI scripts under `dev/` or `scripts/ci/` when used outside Actions.

Workflow behavior documentation in this directory:

- `.github/workflows/master-branch-flow.md`

## Bespoke CI gate provenance

Every repository-specific CI gate must record the invariant it protects, the
issue, pull request, or incident that motivated it, and the condition under
which it can be retired. Keep a short breadcrumb beside the job or the step in the
workflow YAML.

For script-backed gates, keep the durable explanation in this
registry rather than in source comments covered by the comment-hygiene gate.

| Gate | Protected invariant | Origin | Retirement condition |
|---|---|---|---|
| Repository Structure | Index gitlinks match `.gitmodules`, and only the approved translation submodule is present. | [PR #8516](https://github.com/zeroclaw-labs/zeroclaw/pull/8516) | Retire only when submodules are removed or an equivalent repository-policy check enforces the same restriction. |
| Zerocode RPC Boundary | Zerocode depends only on shared `zeroclaw-api` contracts; runtime and other implementation behavior remains behind RPC. | [PR #7850](https://github.com/zeroclaw-labs/zeroclaw/pull/7850) | Retire only if the architecture intentionally permits implementation dependencies or an equivalent dependency-boundary check replaces it. |
| Nix Hash Drift | `nix/hashes.json` contains exactly the fixed-output hash keys required by Git dependencies in `Cargo.lock`. | [PR #8336](https://github.com/zeroclaw-labs/zeroclaw/pull/8336) | Retire only when Nix no longer uses the tracked hash registry or another mechanism enforces the same synchronization. |
| Installer Drift | Tracked installer, packaging, container, Nix, and installation-documentation surfaces match the canonical generator specification. | [PR #7558](https://github.com/zeroclaw-labs/zeroclaw/pull/7558) | Retire only when those surfaces are no longer generated and tracked, or an equivalent consistency check replaces this gate. |
| Windows Recovery Change Filter | `scripts/ci/windows_recovery_change_filter.sh` skips only the runtime task-owner recovery test job (`windows-task-owner-recovery`, parallel to the Windows build leg) for unrelated PRs, while keeping Windows compilation and voice-wake checks automatic. Relevant paths and missing change evidence select execution; master pushes and merge-queue runs always execute it. | [PR #10867](https://github.com/zeroclaw-labs/zeroclaw/pull/10867) | Retire only when the recovery tests run unconditionally at acceptable cost or equivalent required Windows coverage replaces this job and its selection contract. |
| Plugin Artifact Smoke | `scripts/ci/plugin_artifact_smoke.sh` takes one `zeroclaw` binary and proves it installs and executes a real tool component through the operator's commands and an agent turn. The plugin stays unused until the operator enables the plugin system, turns discovery on, and approves the tool. Values configured for a package that does not request them are not delivered. Payload digests and the host's WIT world are enforced at install, strict signature mode refuses unsigned, untrusted, and edited packages, and a plugin that does not load is skipped instead of stopping the agent. With the opposite expectation the same script requires that the binary has no `plugin` command. | [Issue #10994](https://github.com/zeroclaw-labs/zeroclaw/issues/10994) | Retire only when no build or artifact carries the plugin host, or an equivalent check executes a plugin from every artifact that claims plugin support. |
