# Web dashboard (`gateway.web_dist_dir`)

The gateway daemon ships its HTTP API in the binary, but the web dashboard
HTML/JS/CSS lives on disk in a `web/dist/` directory produced by Vite. The
`gateway.web_dist_dir` setting (and its `ZEROCLAW_gateway__web_dist_dir`
schema-mirror env-var override) tells the daemon where that directory is.
When neither the setting nor a known fallback location contains a built
`index.html`, the gateway boots in **API-only mode** and the dashboard URL
returns a "not available" message.

## TL;DR

<div class="os-tabs-src">

#### sh

```sh
# Equivalent env-var override (in-memory only, never persisted)
export ZEROCLAW_gateway__web_dist_dir="/absolute/path/to/zeroclaw/web/dist"
```

</div>

Then build the bundle once:

<div class="os-tabs-src">

#### sh

```sh
cargo web build
```

</div>

…and restart the daemon. The startup log changes from

```text
Web dashboard: not available — no web/dist found. Build with `cargo web build` …
```

to

```text
Web dashboard: serving from /absolute/path/to/zeroclaw/web/dist
```

## What the setting does

`gateway.web_dist_dir` is an `Option<String>` pointing at the directory that
contains a built `index.html`. At gateway start, the daemon:

1. Reads the value from `config.toml` (or the env-var override).
2. Verifies the directory exists AND contains `index.html` on this machine.
3. If yes, serves the dashboard from that path.
4. If no, logs a WARN ("path doesn't contain `index.html` on this machine;
   falling back to auto-detect") and tries the auto-detect candidates below.
5. If auto-detect also turns up nothing, the gateway runs in API-only mode
   and `GET /` returns a "not available" message that points back here.

The value is treated as a hint, not a hard requirement. A stale path (typo,
host-specific path copied from another machine, missing build) demotes to
auto-detect rather than crashing every dashboard request.

## Default: auto-detect order

When `gateway.web_dist_dir` is unset (or set to a path with no `index.html`),
the daemon probes these locations in order and serves from the first one that
contains `index.html`:

| # | Candidate | When it matches |
|---|-----------|-----------------|
| 1 | `./web/dist` (relative to CWD) | Running `cargo run` from the repo root in dev |
| 2 | `<dir-of-binary>/web/dist` | The packaged binary ships `web/dist` next to itself |
| 3 | `/zeroclaw-data/web/dist` | Standard Docker / packaged-volume layout |
| 4 | `/usr/share/zeroclawlabs/web/dist` | AUR / system package install |
| 5 | `${XDG_DATA_HOME:-~/.local/share}/zeroclaw/web/dist` | Prebuilt-binary installer (per-user) |

If you're on one of those distributions and the dashboard "just works", you
don't need to set `gateway.web_dist_dir` at all, the auto-detect found it.

## How to obtain a `web/dist`

You have three options. Pick whichever matches how you installed ZeroClaw.

### A) Source checkout (developers / packagers)

<div class="os-tabs-src">

#### sh

```sh
git clone https://github.com/zeroclaw-labs/zeroclaw.git
cd zeroclaw
cargo web build           # alias for `cargo run -p xtask --bin web -- build`
                          # auto-runs `npm install` on first run
```

</div>

The bundle lands in `web/dist/`. Point `web_dist_dir` at the absolute path of
that directory, or run the daemon from the repo root and let auto-detect
candidate 1 pick it up.

The full set of `cargo web` subcommands (`dev`, `check`, `gen-api`, etc.) is
documented in [Building the web dashboard](../developing/web.md).

### B) Pre-built release artifact

Release archives on the [Releases page](https://github.com/zeroclaw-labs/zeroclaw/releases)
ship the daemon with `web/dist/` already populated alongside the binary.
Auto-detect candidate 2 finds it; no `gateway.web_dist_dir` configuration
needed.

### C) Docker image

The official Docker image places the bundle at `/zeroclaw-data/web/dist`
(auto-detect candidate 3). It works out of the box; you only need to set
`web_dist_dir` if you mount your own volume over that path.

## Override precedence

The value is resolved with the standard config-layer order:

1. `ZEROCLAW_gateway__web_dist_dir` (schema-mirror env var, see
   [Environment variables](../reference/env-vars.md))
2. `gateway.web_dist_dir` in `config.toml`
3. Auto-detect (the five candidates above)

Env-var overrides apply to the in-memory `Config` only; they are never
written back to `config.toml`.

## Schema-mirror grammar: deriving `ZEROCLAW_gateway__web_dist_dir`

The general operator override grammar (see
[Environment variables](../reference/env-vars.md)) maps the dotted TOML path
to an env-var name mechanically:

```text
TOML path:  gateway.web_dist_dir
            ─────── ─────────────
            section field-name (snake_case, kept as-is)

Env var:    ZEROCLAW_gateway__web_dist_dir
            ─────────       ──            ────────────
            prefix          path-separator  field-name
                            (`.` → `__`)    (unchanged)
```

The same three steps produce env-var names for every other gateway knob,
e.g. `gateway.request_timeout_secs` becomes
`ZEROCLAW_gateway__request_timeout_secs`.

## Common pitfalls

### Don't use `~` or `$HOME`

A literal tilde is **not** expanded by the gateway — use an absolute path for `gateway.web_dist_dir`. Shell variables (`$HOME`, `%USERPROFILE%`) are likewise not expanded; pre-expand them in the env var if you set the value that way:

<div class="os-tabs-src">

#### sh

```sh
export ZEROCLAW_gateway__web_dist_dir="$HOME/zeroclaw/web/dist"   # shell expands $HOME
```

</div>

Companion [PR #6961](https://github.com/zeroclaw-labs/zeroclaw/pull/6961) adds
the targeted "looks like an unexpanded `~` / `$VAR`,
[`shellexpand`](https://crates.io/crates/shellexpand) it before writing this
value" check tracked in
[issue #6079](https://github.com/zeroclaw-labs/zeroclaw/issues/6079) to both
`zeroclaw doctor` and `zeroclaw self-test` as a Warn-severity diagnostic.
Neither command surfaces it on current `master`, until #6961 lands, expand
`~` / `$VAR` yourself before writing `gateway.web_dist_dir` (for example
write `/home/alice/zeroclaw/web/dist` instead of `~/zeroclaw/web/dist`).

### Relative paths resolve against CWD, not the config file

`web_dist_dir = "web/dist"` is interpreted relative to the daemon's working
directory at start time, not relative to the location of `config.toml`. If
you ship a config to another host or invoke the daemon from a different
directory (e.g. via systemd), the relative form will look in the wrong place.
**Use absolute paths in `config.toml`.**

### "Stale path" WARN at startup

```text
WARN gateway.web_dist_dir points at a path that doesn't contain index.html
on this machine; falling back to auto-detect. Update or remove the setting
in config.toml to silence this warning.
```

This means the path is syntactically valid but the file isn't there yet.
Either run `cargo web build`, fix the path, or remove the setting entirely
and let auto-detect handle it.

### "Web dashboard: not available" at startup

```text
INFO Web dashboard: not available — no web/dist found. Build with
`cargo web build` and point gateway.web_dist_dir at the resulting
web/dist directory.
```

API endpoints still work, only the HTML/JS bundle is missing. Build it
(option A/B/C above) or set the path.

## See also

- [Environment variables](../reference/env-vars.md): full schema-mirror grammar
- [Gateway HTTP API](./api.md): what the dashboard talks to
- [Building the web dashboard](../developing/web.md): `cargo web` subcommands and what gets generated
