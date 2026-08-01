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

Copy the `wit/v0/` directory from the ZeroClaw repository into the crate root
as `wit/`. You do not need a full checkout; fetch just that directory from the
tag matching your target host version:

```bash
git clone --depth 1 --filter=blob:none --sparse \
  https://github.com/zeroclaw-labs/zeroclaw /tmp/zeroclaw-wit
git -C /tmp/zeroclaw-wit sparse-checkout set wit
cp -r /tmp/zeroclaw-wit/wit .
```

The WIT files are the ABI: the host generated its bindings from these exact
files, so your guest bindings must come from the same ones. Pin the version:
WIT worlds evolve with the host, and a component built against newer worlds
than the host binds will fail to instantiate.
