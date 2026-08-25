<!-- Canonical plugin install/verify steps. Edit here; reuse via {{#include}}. -->
> **These commands need a binary with the plugin host compiled in.** The
> prebuilt release binaries the installer ships are built without the
> `plugins-wasm` feature, so `zeroclaw plugin ...` is an unrecognized
> subcommand there and installed plugins are never discovered. Build from
> source with a plugin execution backend, e.g.
> `cargo build --release --features plugins-wasm-cranelift`.

Each plugin lives in its own subdirectory of the plugins directory (default
`~/.zeroclaw/plugins/`, resolved through `plugins.plugins_dir`), holding the
manifest and the component named to match the manifest's `wasm_path`:

```text
~/.zeroclaw/plugins/
└── my-plugin/
    ├── manifest.toml
    └── my-plugin.wasm
```

Install from a local directory (this validates the manifest shape and runs the
signature policy before copying anything):

```bash
zeroclaw plugin install ./my-plugin/
```

Enable the plugin system and confirm discovery:

```bash
zeroclaw config set plugins.enabled true
zeroclaw plugin list
zeroclaw plugin info my-plugin
```

`zeroclaw plugin list` and `zeroclaw plugin info` confirm a package is installed
and discoverable, but discovery is not activation. `plugins.enabled = true`
turns the plugin host on; auto-discovered tool and skill capabilities load at
runtime only when `plugins.auto_discover = true` as well, and that flag is
`false` by default (fail-closed):

```bash
zeroclaw config set plugins.auto_discover true
```

So `plugins.enabled = true` on its own gives you the channels you declare under
`[channels.plugin.<alias>]` and no plugin tools or skills: a tool or skill
package can appear in `zeroclaw plugin list` yet contribute nothing at runtime.
Explicit channel bindings are operator-named rather than auto-discovered, so they
do not need `auto_discover`; the flag gates only auto-discovered tools and
skills.

A plugin missing from `zeroclaw plugin list` was skipped at discovery: check
the startup log for the skip warning (malformed manifest, missing `wasm_path`
file, or signature policy rejection).
