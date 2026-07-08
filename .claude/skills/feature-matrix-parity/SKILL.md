---
name: feature-matrix-parity
description: "Update the OpenClaw and Hermes comparison columns of the ZeroClaw feature-and-support matrix. Use this skill when the user wants to refresh, fill, or verify parity data in docs/book/feature-matrix-parity.toml, add a new comparison row or section to the feature matrix, or re-walk the competitor repos for support status. Trigger on: 'update the feature matrix', 'refresh parity', 'fill the parity TOML', 'walk OpenClaw and Hermes', 'the matrix shows Unknown', 're-verify the comparison columns', 'add a row to the feature matrix'."
---

# ZeroClaw Feature Matrix Parity

You maintain the two competitor columns (OpenClaw, Hermes) of the feature and
support matrix at `docs/book/src/reference/feature-matrix.md`. The ZeroClaw
column is walked from the binary's own registries at docs-build time and is
never hand-edited. Only the external columns live in
`docs/book/feature-matrix-parity.toml`, and that file is the single reviewable
source for every parity fact.

The cardinal rule: every cell you write is source-walked from the competitor's
actual tree, never inferred from a folder name, an alias, or memory. A directory
called `signal` is not proof of Signal support; the implementing module is.

## Clone URLs

Both repositories are recorded in the parity TOML header (the `Sources` block).
Read them from there so there is one source of truth:

- OpenClaw: `https://github.com/openclaw/openclaw`
- Hermes: `https://github.com/NousResearch/hermes-agent`

Always shallow-clone. These trees are large (OpenClaw ~340MB, Hermes ~200MB even
shallow) and full history is never needed for a registry walk:

```bash
git clone --depth 1 https://github.com/openclaw/openclaw /tmp/parity-openclaw
git clone --depth 1 https://github.com/NousResearch/hermes-agent /tmp/parity-hermes
```

Clone both in parallel, and delete both when finished. For a quick single-file
spot-check without a full clone, the GitHub contents API reads any tree file
directly, which is enough to confirm one module's existence:

```bash
gh api "repos/openclaw/openclaw/contents/<path>" --jq '.content' | base64 -d
```

## Status vocabulary

Cells use the page legend exactly: `supported`, `partial`, `experimental`,
`planned`, `none`, `unknown`. The verdict turns on what the tree actually *wires
and runs*, not on what a module merely defines. Finding a named module is
necessary but not sufficient: before calling a cell `supported`, rule out the
downgrade patterns below.

- `supported` when the tree ships a first-class module for it AND it is wired
  into a real path and on by default. A module that exists but fails one of the
  downgrade patterns below is not `supported`; it is `partial` or `none` per
  those rules.
- `partial` when support is real but qualified. Use it when the module is:
  - reachable only through a generic gateway rather than a dedicated module, or
  - covers a subset of the surface, or
  - **default-off / opt-in** (present but disabled unless explicitly enabled;
    read the config defaults, not just that the code path exists), or
  - **selectively wired** (live on some trigger/transport paths, dead on others).
- `none` when the tree has no equivalent, OR the only match is
  **defined-but-dead** (a module with no non-test callers) or a
  **degraded/orphan path** (it starts but produces no end-to-end behavior). A
  directory name or alias is not a match; the implementing, *called* module is.
- Leave a walked ZeroClaw row with no TOML entry to render `unknown`; fill it in
  as parity is confirmed rather than guessing.

The loose-alias downgrades in Step 3 are the same rule applied to naming: a
speech- or image-only module is not LLM-chat `supported`, and a similarly named
but distinct product (GitHub Copilot vs GitHub Models) is not the same slot.

## Workflow

### Step 1: Read the live page for ground truth

The rendered page is the authority for which rows exist, not the TOML keys (the
TOML can be stale or incomplete). If a docs server is running, fetch it and
extract every table verbatim:

```bash
curl -s "http://127.0.0.1:3000/en/reference/feature-matrix.html" -o /tmp/fm-live.html
```

Parse out the Channel, Provider slot, and Tool tables. The full walked key set
(for providers this is dozens of slots, not a handful) is what you must account
for. If no server is running, walk the registries directly: the channel
inventory (`ChannelsConfig::channels`), `canonical_model_provider_slots`, and
`default_tools`.

### Step 2: Shallow-clone both competitors

Clone in parallel per the URLs above. Confirm both landed (non-trivial size)
before walking; an interrupted clone leaves an empty stub.

Capture each clone's exact HEAD before walking:

```bash
git -C /tmp/parity-openclaw rev-parse HEAD
git -C /tmp/parity-hermes rev-parse HEAD
```

That SHA, not just the date, is what pins the verdicts to a reproducible commit.
`--depth 1` grabs whatever HEAD was current at clone time, so without the SHA a
re-walk cannot tell what actually moved. You record both in the TOML `Sources`
header in Step 5.

### Step 3: Walk every row against real modules

For each row in each section, find the implementing module in each tree:

- OpenClaw: provider and platform plugins live in `extensions/`; channels also
  in `src/channels/`; tools in `extensions/openshell` and `src/` file/glob/ripgrep
  code.
- Hermes: platforms in `plugins/platforms/` and `gateway/platforms/`; providers
  in `agent/*_adapter.py`, the `_AUX_DIRECT_API_BASE_URLS` registry, the
  `hermes_cli` model-setup provider branches, and
  `website/docs/integrations/providers.md`; tools in `tools/`.

Resolve aliases (e.g. `bedrock` maps to `amazon-bedrock`, `llamacpp` to
`llama-cpp`, `glm` to a z.ai/zhipu module), then VERIFY the matched module
actually implements that capability. Downgrade loose matches: a module that only
does speech or image generation is not LLM-chat support; a similarly named but
distinct product (for example GitHub Copilot vs GitHub Models) is not the same
slot. When in doubt, `grep` the module contents for the capability before
calling it `supported`.

### Step 4: Check the issue tracker for `planned` verdicts

A slot with no module in the tree is not automatically `none`. Before marking
`none`, check whether the competitor has an open feature request tracking it,
which makes the cell `planned` instead. Neither project publishes a ROADMAP.md
or project board, so the open issue tracker is the authoritative planned signal.

Search at the title level and filter to genuine requests, not incidental
mentions in bug reports:

```bash
gh api repos/openclaw/openclaw --jq '.open_issues_count'   # confirm the repo and tracker are live
gh search issues --repo openclaw/openclaw --state open "<slot> in:title" --json number,title,url
gh search issues --repo NousResearch/hermes-agent --state open "<slot> in:title" --json number,title,url
```

A raw keyword count is noise: an issue that merely mentions "bedrock" is not a
roadmap commitment, and a bug filed against a slot proves that slot is already
`supported` (people only file bugs against features that exist). A title like
`Feature Request: add <slot> as a native provider` with `state: open` and no
module in the tree is `planned`. Record the issue number and the check date in
the TOML header so the verdict is auditable. Re-verify against the tree: if the
feature request was since merged, the slot is `supported`, not `planned`.

### Step 5: Write the TOML

Every key must correspond to a walked row, in the page's canonical order. Keep
the header's `Sources` block current (repo URLs, the pinned clone-HEAD SHA per
repo from Step 2, per-section walk paths, and the `checked` date). For a
brand-new comparison concept that is not part of the
walked channel/provider/tool registries (SOP, for instance), it does not belong
in this TOML; hand-author a small section in `feature-matrix.md` itself with a
static table and note that it is hand-recorded rather than code-walked.

### Step 6: Verify with the guard test

The parity join has a hard-fail CI guard. A TOML key the walk no longer produces,
or a walked row with a stale key, fails the docs build. Run it:

```bash
cargo test -p xtask --lib feature_matrix
```

Both `parity_join_produces_rows_without_stale_keys` and
`channel_column_is_all_supported_from_walk` must pass.

### Step 7: Regenerate and confirm live

Regenerate the gitignored snippets and confirm zero stray `Unknown`/`❓` remain
where you filled cells:

```bash
cargo run -p xtask --bin mdbook -- preprocess
```

Re-fetch the live page and diff the rendered tables against your intended
verdicts. Do not claim a cell from memory; read the rendered HTML.

### Step 8: Clean up and commit

Delete both clones. Commit with a scoped, multi-line message that records the
supported/none counts per column and names any loose matches you downgraded and
why, so the verdicts are auditable from the log.

## Rules

- Every cell is source-walked from the competitor's actual tree. Never infer
  from a directory name, an alias, or prior memory.
- Always shallow-clone (`--depth 1`) and always delete the clones when done.
- Read the live rendered page for the row set, not the existing TOML keys.
- Verify loose alias matches against module contents before marking `supported`;
  downgrade speech/image-only or different-product matches to `none`.
- Before marking a slot `none`, check the competitor's open issue tracker: an
  open feature request with no module in the tree makes the cell `planned`, not
  `none`. A bug filed against a slot proves it is already `supported`.
- Keep the TOML `Sources` header current, including the `checked` date and the
  pinned per-repo commit SHA the columns were walked against.
- Run `cargo test -p xtask --lib feature_matrix` before committing; the guard is
  a hard CI fail.
- Confirm the rendered tables from the live HTML, not from what you expect.
- Never touch the ZeroClaw column; it is code-walked and regenerates itself.
