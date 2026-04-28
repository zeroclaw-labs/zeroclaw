# Master Branch Delivery Flows

How code moves from a PR to a shipped release.

Use with:

- [`docs/book/src/maintainers/ci-and-actions.md`](../../docs/book/src/maintainers/ci-and-actions.md)
- [`docs/book/src/maintainers/release-runbook.md`](../../docs/book/src/maintainers/release-runbook.md)

Last updated: **May 2026** (post-v0.7.4 cleanup).

---

## Branching Model

ZeroClaw uses a single default branch: `master`. All contributor PRs target
`master` directly. There is no `dev` or promotion branch.

Maintainers with merge authority: `theonlyhennygod` and `JordanTheJet`.

---

## Active Workflows

| File | Trigger | Purpose |
|---|---|---|
| `ci.yml` | `pull_request` → `master` | Lint + test + build on every PR |
| `release-stable-manual.yml` | `workflow_dispatch`, tag push `v*` | Stable release (manual, version-gated) |
| `cross-platform-build-manual.yml` | `workflow_dispatch` | Full platform build matrix (manual smoke check) |
| `pr-path-labeler.yml` | `pull_request` lifecycle | Automatic path-based PR labeling |

---

## Event Summary

| Event | What runs |
|---|---|
| PR opened or updated against `master` | `ci.yml` (full lint + test + build + strict delta) |
| Manual dispatch | `cross-platform-build-manual.yml` or `release-stable-manual.yml` |
| Tag push `vX.Y.Z` | `release-stable-manual.yml` (full release pipeline) |

There is no automatic CI run on push to master and no automatic release on
merge. Releases are always intentional — either a manual dispatch or a
deliberate tag push.

---

## Step-by-Step

### 1) PR → `master`

1. Contributor opens or updates a PR targeting `master`.
2. `ci.yml` runs:
   - `lint` — `cargo fmt --all -- --check`, `cargo clippy -D warnings`,
     `cargo check --features ci-all`, strict delta lint on changed lines
     (PRs only).
   - `build` — matrix across `x86_64-unknown-linux-gnu`,
     `aarch64-apple-darwin`, `x86_64-pc-windows-msvc`.
   - `check` — matrix: all features + no default features.
   - `check-32bit` — `i686-unknown-linux-gnu`, no default features.
   - `bench` — benchmarks compile check.
   - `test` — `cargo nextest run --locked` on `ubuntu-latest`.
   - `security` — `cargo deny check`.
   - `CI Required Gate` — composite job; branch protection requires this.
3. Maintainer reviews and merges once the gate is green and review policy is
   satisfied.

### 2) Stable Release (manual)

See [`docs/book/src/maintainers/release-runbook.md`](../../docs/book/src/maintainers/release-runbook.md)
for the full procedure. In summary:

1. Maintainer verifies CI is green on the version bump PR.
2. Version bump PR is merged.
3. Maintainer triggers `release-stable-manual.yml` via `workflow_dispatch`
   with the version number, or pushes an annotated tag `vX.Y.Z`.
4. Workflow builds all targets, creates the GitHub Release, publishes to
   crates.io, pushes Docker images, and notifies distribution channels.
5. Maintainer approves the three environment gates
   (`github-releases`, `crates-io`, `docker`) when prompted.

### 3) Full Platform Build (manual)

1. Maintainer runs `cross-platform-build-manual.yml` via `workflow_dispatch`.
2. Build-only across additional targets not covered by the PR build matrix.
3. No tests, no publish. Used to verify cross-compilation health.

---

## Build Targets by Workflow

| Target | `ci.yml` | `cross-platform-build-manual.yml` | `release-stable-manual.yml` |
|---|:---:|:---:|:---:|
| `x86_64-unknown-linux-gnu` | ✓ | | ✓ |
| `aarch64-unknown-linux-gnu` | | ✓ | ✓ |
| `armv7-unknown-linux-gnueabihf` | | | ✓ |
| `arm-unknown-linux-gnueabihf` | | | ✓ |
| `aarch64-apple-darwin` | ✓ | | ✓ |
| `aarch64-linux-android` | | | ✓ (experimental) |
| `x86_64-apple-darwin` | | ✓ | |
| `x86_64-pc-windows-msvc` | ✓ | ✓ | ✓ |

---

## Diagrams

### PR to master

```mermaid
flowchart TD
  A["PR opened or updated → master"] --> B["ci.yml"]
  B --> L["lint\nfmt · clippy · check-features · strict-delta"]
  L --> T["test\ncargo nextest"]
  L --> BLD["build\nLinux · macOS · Windows"]
  L --> CHK["check\nall features · no default features"]
  L --> C32["check-32bit\ni686-unknown-linux-gnu"]
  L --> BCH["bench\ncompile check"]
  L --> SEC["security\ncargo deny check"]
  T & BLD & CHK & C32 & BCH & SEC --> G["CI Required Gate"]
  G -->|red| D["PR stays open"]
  G -->|green| R["Maintainer merges"]
```

### Stable release

```mermaid
flowchart TD
  A["workflow_dispatch: version=X.Y.Z\nor tag push vX.Y.Z"] --> V["validate\nsemver · Cargo.toml match · tag uniqueness"]
  V --> BLD["build all targets"]
  BLD --> PUB["publish\nGitHub Release · SHA256SUMS"]
  PUB --> CR["crates-io"]
  PUB --> DOC["docker\nGHCR :vX.Y.Z + :latest"]
  PUB --> DIST["scoop · aur · homebrew"]
  PUB --> ANN["discord · tweet"]
```

---

## Troubleshooting

1. **Gate red on PR** — check the `lint` job first (fmt/clippy failures are
   the most common cause), then `test`, then `build`.
2. **Release validate failed** — `Cargo.toml` version does not match the
   input, or the tag already exists. Fix the version bump PR and re-trigger.
3. **Need a full cross-platform build** — run `cross-platform-build-manual.yml`
   manually from the Actions tab.