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

A plugin missing from `zeroclaw plugin list` was skipped at discovery: check
the startup log for the skip warning (malformed manifest, missing `wasm_path`
file, or signature policy rejection).
