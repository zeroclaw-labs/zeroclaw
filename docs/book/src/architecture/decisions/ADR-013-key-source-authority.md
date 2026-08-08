---
id: ADR-013
title: Master key acquisition uses one configured key-source authority
date: 2026-07-25
status: proposed
relates-to:
  - https://github.com/zeroclaw-labs/zeroclaw/issues/9127
  - https://github.com/zeroclaw-labs/zeroclaw/pull/9194
  - docs/book/src/security/model.md
  - docs/book/src/architecture/config-lifecycle.md
  - crates/zeroclaw-config/src/secrets.rs
---

# ADR-013: Master Key Acquisition Uses One Configured Key-Source Authority

## Context

When secrets encryption is enabled, ZeroClaw normally persists non-empty `#[secret]` values in the `enc2:` format with one master key per configuration root. The current implementation obtains that key from `.secret_key`, a plaintext hex file protected by filesystem permissions. That default is practical for local development and deployments that mount protected key material, but it cannot express operating-system keychains, passphrase-derived keys, or external secret systems.

The key location is only one part of the contract. Every production consumer must agree on which source owns the key, how first-use provisioning differs from temporary unavailability, and what happens when a configured source cannot provide the expected key. A direct `.secret_key` read outside the canonical secrets boundary, an implicit fallback to another source, or an unsafe backend switch can make existing ciphertext unreadable or weaken the deployment's intended protection.

Accepted RFC [#9127](https://github.com/zeroclaw-labs/zeroclaw/issues/9127) defines a phased key-source architecture. Initial implementation [#9194](https://github.com/zeroclaw-labs/zeroclaw/pull/9194) is limited to a behavior-preserving file-source extraction. This record captures the durable target without claiming that configured non-file sources or migration support have shipped.

## Decision

### Use one canonical key-source boundary

Master key acquisition is owned by a `KeySource` boundary in the configuration and secrets subsystem. `SecretStore` and every other production consumer of deployment key material must use that boundary rather than reading `.secret_key`, invoking a platform store, or caching an independently obtained key directly.

One configured source is authoritative for a deployment at a time. The file source remains the backward-compatible default. Adding another source must not change the `enc2:` ciphertext format or the ChaCha20-Poly1305 encryption contract.

Whether a non-encryption consumer receives scoped access to the source or derives a purpose-specific subkey is a separate security decision. This ADR requires canonical acquisition but does not choose a derivation or compatibility contract for TUI identity signing or another protocol.

The boundary may expose key bytes only for the duration of a synchronous operation. This is a correctness and lifetime constraint, not a sandbox: code executing inside that operation could still copy the bytes. Implementations must minimize copies and clear temporary material where the platform and dependency model permit it.

This raw-key boundary applies only to sources that can return exportable 32-byte key material. Non-exportable secure elements expose cryptographic operations rather than key bytes and require a separate operation-based boundary and architecture decision.

### Separate provisioning state from availability

A source must distinguish these states:

- local key material exists and can be verified;
- local key material needs initialization; or
- key material is externally provisioned and has no meaningful local existence check.

A local provisioning probe must not unexpectedly execute a helper, contact a network service, prompt a user, or unlock a keychain. Actual key access is a separate operation and may fail because the configured source is unavailable, locked, misconfigured, or returns the wrong key.

Initialization creates new key material only for a source that explicitly supports it. File initialization must publish a complete restrictive file without replacing existing material or accepting symlink redirection. Rotation is not initialization and requires its own guarded operation.

### Fail closed without changing authority

When an enabled feature requires the configured key and the source cannot provide it, that feature's startup or credential operation fails with safe source-specific diagnostics. ZeroClaw must not silently fall back to `.secret_key`, generate replacement material, or try another backend. Raw key bytes and helper output that may contain them must not appear in logs or returned errors. Whether optional TUI identity signing may remain disabled when no key is available is outside this decision.

Source implementations must state their threat model and operational dependencies. An operating-system keychain does not protect a compromised ZeroClaw process; a passphrase source depends on user interaction and password strength; an external helper depends on its executable, environment, transport, and upstream secret system. A backend name alone is not a security guarantee.

External helpers, when implemented, run an explicitly configured absolute executable without a shell intermediary. The initial contract accepts no arguments; later argument support requires separate review and must represent values separately rather than parsing a shell command. Execution is bounded by a timeout, and the implementation retains and reaps the child on timeout or exit. The helper returns exactly one 32-byte key as 64 lowercase hexadecimal characters; raw stdout and stderr never enter logs or returned errors. The initial contract inherits the process environment and must document that exposure. Retries and caches are bounded, expired key material is cleared, and refresh failure remains fail-closed.

### Keep migration and rotation separate

Moving the same master key to another source is migration. Generating a new key and re-encrypting every protected value is rotation. They have different failure and rollback rules and must not be represented as one generic backend change.

Changing the configured source while encrypted values exist requires a verified migration path. Until migration tooling ships, ZeroClaw must reject a source change that cannot prove access to the key that decrypts the existing `enc2:` values. Migration must preserve the old source until the new source has been written and read back successfully. Rotation must retain the old key and original configuration until every value has been re-encrypted and the new configuration is committed atomically.

### Adopt the boundary in safety order

The file-source extraction lands first without changing configuration or ciphertext. Production consumers and fail-closed source selection move behind that boundary before a non-file source is enabled. Non-file sources then land one at a time with source-specific threat models and tests. Migration and rotation remain separately reviewed flows after source selection is stable.

This ADR remains proposed until all of these conditions are met:

- the file source preserves compatibility with existing `.secret_key` and `enc2:` data against literal pre-extraction key and ciphertext fixtures and publishes new key files without replacement or symlink following;
- every production consumer obtains installation key material through the canonical key-source boundary;
- configuration selects exactly one source, defaults compatibly to the file source, and fails closed without fallback or replacement-key generation;
- at least one supported non-file source proves that the boundary works beyond the file implementation; and
- source switching is rejected unless existing ciphertext can be decrypted or a documented, rollback-capable migration completes successfully.

## Consequences

Positive consequences:

- Desktop, server, and container deployments can choose an exportable key authority that matches their operating environment.
- All credential consumers share one source of truth and one fail-closed lifecycle.
- The existing file-backed deployment remains the compatibility baseline.
- Migration, rotation, and ordinary startup cannot be confused silently.

Negative consequences:

- Startup now needs explicit provisioning and availability semantics for each source.
- Non-file sources add platform dependencies, prompts, external-process behavior, or service availability that the file source does not have.
- Backend switching cannot be a simple configuration edit when encrypted values already exist.
- The transition must find and remove direct key-file reads across production consumers before the boundary is complete.

## References

- [RFC #9127: Key-source abstraction and deployment classification](https://github.com/zeroclaw-labs/zeroclaw/issues/9127)
- [PR #9194: File-backed key-source extraction](https://github.com/zeroclaw-labs/zeroclaw/pull/9194)
- [Security model](../../security/model.md)
- [Config lifecycle](../config-lifecycle.md)
- `crates/zeroclaw-config/src/secrets.rs`
- `crates/zeroclaw-runtime/src/rpc/tui_identity.rs`
