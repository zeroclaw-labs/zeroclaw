# Embedding Privacy & vec2text Defence (PR #5)

**Status**: In force from the `feat/document-pipeline-overhaul` branch onward.
**Scope**: Every embedding vector that leaves the local SQLite brain database — whether over the sync wire or in cold backups.

## Threat model

### 1. vec2text (EMNLP 2023)

Jiang et al., *Text Embeddings Reveal Almost as Much as Text*, demonstrated that an attacker who obtains the raw float embedding of a text snippet can recover ~92% of the original tokens with a small decoder model trained against the same embedding family. The attack:

- does **not** require knowledge of the source text,
- **does** require access to the exact embedding floats and knowledge of the model family used to produce them (BGE-M3 vs. text-embedding-3-small → different decoders),
- works even with truncated / quantised vectors, with a graceful accuracy drop.

Practical consequence: treating embeddings as "opaque numerical summaries" is wrong. A vector is **recoverable plaintext** in the hands of someone who knows the model.

### 2. Why this matters for MoA

MoA's sync protocol (§3 of `docs/ARCHITECTURE.md`) is designed around the "server-non-storage E2E" patent: content lives on-device, deltas transit through a zero-knowledge relay, and the server must never be able to read user memory. Embedding floats are covered by this promise only if they never appear in the clear.

At-rest exposure matters too: a local attacker with disk read access to `~/.local/share/zeroclaw/memory/brain.db` can dump `embedding_cache.embedding` and feed it into vec2text. The defence therefore spans **wire** and **disk**.

## Defences in force

| Surface | Defence | Status |
|---------|---------|--------|
| Sync wire (`SyncPayload.ciphertext`) | ChaCha20-Poly1305 AEAD over the full `DeltaOperation` including any `EmbeddingBlob`. | ✅ Inherited from existing sync encryption. |
| `DeltaOperation::{Store,VaultDocUpsert}.embedding` field on wire | Model-drift rejection on receive: mismatched `(provider, model, version, dim)` triggers discard + backfill-queue entry — foreign-model floats never reach the local cache. | ✅ PR #5. |
| `embedding_cache` at rest | **Not yet encrypted** — planned SQLCipher migration. Operator mitigation today: file-system encryption (FileVault / LUKS / BitLocker) on the device. | ⚠ Deferred to PR #5b. |
| `vault_embeddings` / `ontology_communities.summary_embedding` at rest | Same as above: relies on host FS encryption pending SQLCipher. | ⚠ Deferred to PR #5b. |
| Backup exports (manifest + snapshot) | Sync relay encryption applies to Layer-3 manifests. Local JSON exports (`cortex.export`, `snapshot.export`) are **never** written with raw embedding floats — they omit the `embedding` / `vector` fields by design. | ✅ Verified. |

## Model-drift rejection — detailed contract

`SqliteMemory::accept_remote_embedding(content, blob)` enforces the following on every inbound `EmbeddingBlob`:

1. **Short-circuit** on `NoopEmbedding` (local `dimensions() == 0`) — there is no local vector index to seed, so the blob is silently dropped. Wire payload content already applied.
2. **Provider check** — `blob.provider` must equal local embedder's `name()` (`local_fastembed`, `openai`, `custom_http`, …). Mismatch → discard.
3. **Model check** — `blob.model` must equal `embedder.model()` (`bge-m3`, `text-embedding-3-small`, …). Mismatch → discard.
4. **Version check** — `blob.version` must equal `embedder.version()` (bumped via `EMBEDDING_SCHEMA_VERSION` on semantic changes). Mismatch → discard.
5. **Dimension check** — `blob.dim as usize` must equal `embedder.dimensions()`. Mismatch → discard.
6. **Byte-length check** — `blob.vector.len()` must equal `dim × 4`. A malformed payload is treated as drift and rejected.

On any of 2–6 the blob is rejected AND the content hash is inserted into `embedding_backfill_queue` with the mismatch reason. A future scheduled backfill pass (not shipped in this PR) iterates that queue and re-embeds the content with the local model.

Acceptance (the happy path) seeds `embedding_cache` keyed on `SqliteMemory::content_hash(content)` so the next local `recall()` skips re-embedding.

## What this PR does NOT do

- **SQLCipher / per-row embedding encryption**: Deferred. The at-rest embedding bytes in `embedding_cache` remain unencrypted on disk. See "SQLCipher rollout plan" below for the staged migration.
- **Rotating keys on model upgrade**: When an operator upgrades the embedder (e.g. from `bge-m3` → `bge-m3-v2`), existing `embedding_cache` rows become stale. This PR does not implement a sweeper; instead, `version()` bumps invalidate cache hits naturally on the next access. A cleanup pass belongs to PR #6 consolidation.

## Sender-side embedding attachment (shipped in a follow-up commit)

Resolved: `SyncEngine::record_store_with_embedding(key, content, category, embedding)` now exists and `SyncedMemory::store` auto-fetches the local embedder's cached blob via `Memory::current_embedding_blob(content)`. When the receiving peer's local embedder matches `(provider, model, version, dim)`, it skips the re-embed; when it doesn't, the receive-side drift logic from PR #5 discards the blob and queues re-embedding. Three tests in `memory::sqlite::tests::current_embedding_blob_*` pin the round-trip.

## SQLCipher rollout plan (next session)

The defence-in-depth goal is "local disk attacker can't feed stolen embedding floats into vec2text". FS-level encryption (FileVault, LUKS, BitLocker) covers the baseline today; SQLCipher closes the residual gap when the operator cannot rely on OS-level encryption.

Staged rollout:

1. **Dependency**: switch `rusqlite` to a build with `sqlcipher` feature enabled under a new cargo feature flag `memory-sqlcipher`. The `bundled` feature already in `Cargo.toml` is compatible; the hit is ~2 MB extra binary size.
2. **Key material**: derive the DB key from a master secret using HKDF with a deterministic context string. The master secret itself lives in the existing secret store (`src/security/secret_store.rs`) which already handles platform keychain access. Key rotation goes through the secret-store rotation path — no new plumbing.
3. **Migration**: on first launch with the feature enabled, the code path wraps the existing `brain.db` with `PRAGMA key = '…';` and runs `PRAGMA cipher_migrate;` to convert in place. Backups of the unencrypted DB land in `brain.db.pre-sqlcipher` and are kept for one week before a scheduled wipe.
4. **Testing**: unit tests exercise the feature-gated path by constructing `SqliteMemory::with_options_keyed(workspace, secret, ...)`; the default path stays unchanged so existing installs see zero behaviour change.
5. **CI**: `.github/workflows/feature-matrix.yml` adds a `memory-sqlcipher` lane mirroring the existing `whatsapp-web`/`browser-native` lanes — `cargo check --no-default-features --features memory-sqlcipher`.

Scope control: the feature is intentionally opt-in. Default installs keep the current posture (fast, small, relies on FS encryption) so the SQLCipher binary bloat is paid only by operators who need it.

## How to verify

The canonical tests live in:

- `src/memory/sync.rs::tests::embedding_blob_*` — pack/unpack round-trip, wire compatibility with pre-PR#5 peers (skip-serializing when `None`), little-endian stability.
- `src/memory/sqlite.rs::tests::accept_remote_embedding_*` — 5 cases: model match (cache seeded), model drift (rejected + queued), dim drift (rejected), version drift (rejected), NoopEmbedding host (silent no-op).

Run just the PR #5 slice:

```bash
cargo test --lib memory::sync::tests::embedding_
cargo test --lib memory::sqlite::tests::accept_remote_embedding_
```

Both suites must pass before the defence can be considered in force on any given build.

## References

- Jiang et al. 2023, *Text Embeddings Reveal Almost as Much as Text* (EMNLP 2023). <https://arxiv.org/abs/2310.06816>
- `docs/ARCHITECTURE.md` §3 — patent claim for server-non-storage E2E encrypted memory sync.
- `docs/ARCHITECTURE.md` §6E-7 — PR #5 roadmap entry (status + deferred items).
