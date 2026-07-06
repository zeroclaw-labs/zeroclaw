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

Clone both in parallel, and delete both when finished.

## Status vocabulary

Cells use the page legend exactly: `supported`, `partial`, `experimental`,
`planned`, `none`, `unknown`. A cell is:

- `supported` when the tree ships a first-class module for it.
- `partial` when support is indirect (reachable only through a generic gateway,
  or covers a subset of the surface).
- `none` when the tree has no equivalent.
- Leave a walked ZeroClaw row with no TOML entry to render `unknown`; fill it in
  as parity is confirmed rather than guessing.

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

### Step 4: Write the TOML

Every key must correspond to a walked row, in the page's canonical order. Keep
the header's `Sources` block current (repo URLs, per-section walk paths, and the
`checked` date). For a brand-new comparison concept that is not part of the
walked channel/provider/tool registries (SOP, for instance), it does not belong
in this TOML; hand-author a small section in `feature-matrix.md` itself with a
static table and note that it is hand-recorded rather than code-walked.

### Step 5: Verify with the guard test

The parity join has a hard-fail CI guard. A TOML key the walk no longer produces,
or a walked row with a stale key, fails the docs build. Run it:

```bash
cargo test -p xtask --lib feature_matrix
```

Both `parity_join_produces_rows_without_stale_keys` and
`channel_column_is_all_supported_from_walk` must pass.

### Step 6: Regenerate and confirm live

Regenerate the gitignored snippets and confirm zero stray `Unknown`/`❓` remain
where you filled cells:

```bash
cargo run -p xtask --bin mdbook -- preprocess
```

Re-fetch the live page and diff the rendered tables against your intended
verdicts. Do not claim a cell from memory; read the rendered HTML.

### Step 7: Clean up and commit

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
- Keep the TOML `Sources` header current, including the `checked` date.
- Run `cargo test -p xtask --lib feature_matrix` before committing; the guard is
  a hard CI fail.
- Confirm the rendered tables from the live HTML, not from what you expect.
- Never touch the ZeroClaw column; it is code-walked and regenerates itself.
