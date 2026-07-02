<!-- Canonical plugin crate scaffold. Edit here; reuse via {{#include}}. -->
Create the crate and add the guest-side dependencies:

```bash
cargo new --lib my-plugin
cd my-plugin
cargo add wit-bindgen@0.46
cargo add serde --features derive
cargo add serde_json
```

Then make two manual edits to the package manifest:

1. Set the library `crate-type` to `["cdylib", "rlib"]`. `cdylib` is what the
   component build produces; `rlib` lets the same crate's pure-logic modules
   compile and unit-test natively on the host.
2. In the release profile, set `opt-level = "s"`, `lto = true`, and
   `strip = true`. Component size is download and load time; there is no
   reason to ship debug symbols across the plugin boundary.

Copy `wit/v0/` from the ZeroClaw source tree into the crate root as `wit/`.
The WIT files are the ABI: the host generated its bindings from these exact
files, so your guest bindings must come from the same ones.
