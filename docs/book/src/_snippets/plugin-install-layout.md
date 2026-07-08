<!-- Canonical plugin install/verify steps. Edit here; reuse via {{#include}}. -->
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
