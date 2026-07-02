# Distributing Plugins

You built a plugin; now it needs to leave your machine without asking the
people who install it to trust you blindly. ZeroClaw's distribution story has
two independent layers: Ed25519 manifest signatures (who published this) and
registry install (how it gets there). This page covers both, checked against
`crates/zeroclaw-plugins/src/signature.rs`, `src/plugin_registry.rs`, and the
install path in `host.rs`.

## Signing

### What is signed

The signature covers the **canonical manifest bytes**: the manifest file's
content with every line whose trimmed form starts with `signature` or
`publisher_key` followed by `=` removed, and trailing empty lines stripped
(`canonical_manifest_bytes` in `signature.rs`). This makes the signature
self-embeddable: you sign the manifest without the signature fields, then add
them, and verification strips them back out before checking.

Two consequences worth knowing:

- The `.wasm` component itself is **not** covered by the signature. What the
  signature attests is the manifest: the name, version, capabilities, and
  permissions a publisher stands behind. Pair it with a registry `sha256`
  digest (below) when the artifact integrity matters in transit.
- Canonicalization is line-based. Reformatting the manifest (reordering
  lines, changing whitespace within a kept line) invalidates the signature.
  Sign last, after the manifest is final.

### Keys and process

Signing uses Ed25519 via the same `ring` primitives the host verifies with.
The signature is base64url (no padding); the public key is hex-encoded. The
crate exposes the full toolchain (`signature.rs`): `generate_signing_key`
produces a PKCS#8 keypair and its hex public key, `sign_manifest` produces
the base64url signature over the canonical bytes, and `public_key_hex`
recovers the public key from a stored private key. There is no CLI wrapper
for signing today; publishers drive these functions from a short Rust helper
in their release pipeline.

The signed manifest then carries two extra fields: `signature` (the base64url
value) and `publisher_key` (your hex public key). Operators who want to trust
you add that hex key to their `plugins.security.trusted_publisher_keys` list:

```bash
zeroclaw config set plugins.security.signature_mode strict
zeroclaw config set plugins.security.trusted_publisher_keys '["<your-key-hex>"]'
```

### How verification behaves

Verification runs at both discovery and install (`enforce_signature_policy`
called from `host.rs`); discovery skips a failing plugin and logs, install
returns the error. The mode matrix, from the operator's side:

| Mode | Unsigned | Signed, key not trusted | Signed, signature invalid | Signed and trusted |
|------|----------|------------------------|---------------------------|--------------------|
| `disabled` | loads | loads, not checked | loads, not checked | loads, not checked |
| `permissive` | loads with warning | loads with warning | loads with warning | loads, verified |
| `strict` | rejected | rejected | rejected | loads |

Note what `strict` means for you as a publisher: an operator in strict mode
loads your plugin only if your exact key is in their trusted set **and** the
manifest bytes verify. Any post-signing manifest edit, by you or by anyone in
the distribution path, bricks the install. That is the point.

## Registry publication

The install path is the local plugin directory; a registry is only a JSON
index consulted at command time (`zeroclaw plugin search` / `install`). The
default index is the `zeroclaw-labs/zeroclaw-plugins` repository's
`registry.json`; private registries are a URL away
(`--registry <url>` per command, or the `ZEROCLAW_PLUGIN_REGISTRY_URL`
environment variable, resolved in that order per `registry_url` in
`src/plugin_registry.rs`).

A registry entry (`PluginRegistryEntry` in
`crates/zeroclaw-plugins/src/registry.rs`) carries: `name`, `version`,
optional `description` and `author`, `capabilities`, the archive `url`, and
an optional `sha256` digest of the zip.

### The archive contract

`zeroclaw plugin install <name>` resolves the entry, downloads the zip,
verifies the digest when present, safely extracts, and hands the extracted
directory to the same `PluginHost::install` path a local install uses. The
extraction is defensive by construction (`src/plugin_registry.rs`), and your
archive must survive it:

- The zip must contain either a root-level `manifest.toml` or exactly one
  nested plugin directory containing one. Zero manifests or more than one is
  a rejected archive.
- Entry names with path traversal, absolute paths, or Windows drive prefixes
  are rejected.
- Download is capped while streaming (50 MiB) so a server withholding
  `Content-Length` cannot force unbounded buffering; extraction is capped at
  the same bound so a zip bomb cannot expand without limit.

Version resolution: when the installer gets a bare name, it picks the **last
matching entry** in the index; a pinned `name@version` selects exactly that
version. Order repeated names in your registry intentionally, oldest first.

### Search is not a trust boundary

`zeroclaw plugin search` is unauthenticated discovery over the index; it
never installs, enables, or executes anything. Install is where the security
happens: digest check, safe extraction, manifest validation, and the
operator's signature policy, identical to a local-path install. Publish
accordingly: assume everything before install is untrusted transport.

## The publisher's checklist

{{#include ../_snippets/plugin-wasm-binary-warning.md}}

1. Finalize the manifest: name, version, capabilities, and the narrowest
   permission set the code uses.
2. Build the component; for skill bundles, validate frontmatter on every
   `SKILL.md` (discovery enforces `name` and `description`).
3. Sign: generate or load your Ed25519 key, sign the canonical manifest
   bytes, embed `signature` and `publisher_key`.
4. Zip the plugin directory (one manifest, no path tricks, under {{#include ../_snippets/plugin-archive-max-mib.md}} MiB).
5. Compute the zip's SHA-256 and publish the registry entry with the digest.
6. Publish your public key hex somewhere operators can verify independently
   of the registry (your repository, your site). The key, not the registry,
   is what `strict` mode operators trust.
7. On every release: bump `version`, re-sign (the version line is inside the
   canonical bytes), re-digest, append the new entry after the old one.
