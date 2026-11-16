# Portable memory kernel

The `memory-tachi` feature uses the shared Portable Kernel for two
ZeroClaw-owned databases. It is enabled by default on the root `zeroclawlabs`
crate (and stays optional on `zeroclaw-memory`, so `--no-default-features` and
`slim-control` builds remain free of AGPL dependencies). The backend name
`tachi`, the crate feature `zeroclaw-memory/tachi`, configuration keys and
existing database locations are unchanged.

| Store | Owner role | Location |
|---|---|---|
| Generic memory | `zeroclaw-memory` | `<data_dir>/memory/tachi.db` |
| Companion memory | `zeroclaw-companion` | `[companion_memory].store_dir/companion-memory.db`; default `<data_dir>/companion/companion-memory.db` |

Both require the `PortableKernel` profile. The roles are not interchangeable.
The kernel supplies storage and typed contracts; ZeroClaw continues to own
memory admission, Soul and User Model authority, personality policy and agent
isolation. Hypermem supplies Hyperion's behavior independently. There is no
shared live database or new network memory service.

## Opening a store

A missing database can be created with its exact profile and owner role.
Existing stores must already have the current schema and matching identity.
Runtime and status checks refuse older, unowned, wrong-role or wrong-profile
stores instead of upgrading them. A refusal points to the explicit migration
command below.

## Migrating retired native SQLite memory (`brain.db`)

Operators with an existing native SQLite memory store (`<data_dir>/memory/brain.db`)
must convert it offline into `<data_dir>/memory/tachi.db` with all writers
stopped:

```sh
zeroclaw memory migrate-native \
  --source /private/backup/brain.db \
  --destination /private/upgrade/tachi.db \
  --approved-by local-operator
```

Keep a restorable backup of `brain.db` and its WAL/SHM sidecars before migrating,
verify the output, move the verified `tachi.db` into `<data_dir>/memory/tachi.db`,
and update `[memory].backend` (or per-agent `memory.backend`) to `"tachi"` if
it was pinned to `"sqlite"` or `"lucid"`.

## Supported offline input

The offline converter runs on Unix systems, where it can verify physical file
identity and enforce owner-only staging permissions. Other platforms return an
explicit unsupported-platform error before reading the source. This restriction
belongs to the offline converter; the shared kernel and ordinary backends retain
their existing platform support.

The migration command recognizes the actual standalone `memcore` 1.9.2 schema
38 from revision `eba0c48617e8820831bfd22f91d269c6d70f409f`. A version number alone
is insufficient: the schema and ownership must match. Unknown tables, schema
changes, protected authority or canonical supersession records are refused
before creating a destination.

Generic memory moves through the public snapshot interface; companion memory
uses an explicitly authorized upgrade of a private backup. Both preserve the
recognized ordinary records and verify their complete content. Historical
inputs that the upgrade would normalize cannot silently lose data: a mismatch
refuses publication. This is not an arbitrary SQLite import or an automatic
repair tool.

## Migration procedure

1. Stop all writers to the source, including the ZeroClaw service. Keep a
   restorable backup of the database and any SQLite WAL/SHM and migration marker.
   Do not delete a WAL to make migration pass.
2. Use a build with `memory-tachi` enabled. Choose an existing private directory
   and a destination database filename that does not exist. Source and
   destination must be different files; destination sidecars must also be absent.
   Run as the directory's owner. Group and other-user write permission on the
   destination directory is refused. Keep this directory and its path exclusively
   controlled until the command finishes; concurrent destination writers are
   outside the offline contract.
3. Run the command with explicit paths, store role and operator identity:

   ```sh
   zeroclaw memory migrate-kernel \
     --source /private/backup/tachi.db \
     --destination /private/upgrade/tachi.db \
     --store memory \
     --approved-by local-operator > /private/upgrade/migration-receipt.json
   ```

   For a companion database, use `--store companion` and its own source and new
   destination paths. The command works without an agent configuration and ignores
   normal configuration/bootstrap state, including a malformed `config.toml`.
   It uses the operating system locale for diagnostics. It does not start the
   agent or choose a live database path on your behalf.

4. Check the exit status and JSON receipt. Standard output contains the receipt
   only on success; progress/completion diagnostics use standard error. An empty
   operator identity is rejected. A build without `memory-tachi` refuses before
   touching either database path.
5. With writers still stopped, retain the original backup and move the verified
   database together with its matching `.migration-marker` to the normal store
   location. Start ZeroClaw and check memory retrieval and companion health.
   Never point the two owners at the same file.

The command publishes with no replacement semantics. Running it again against
the same destination refuses; choose a fresh destination for another attempt.
It does not alter the source, switch configuration, start a service or remove a
backup.

## Verification and receipt

Migration requires an offline, quiescent source. It copies the database and WAL
through read-only file handles and checks file identities and complete content
digests before and after copying and again before publication. Any observed
source change aborts migration. SQLite opens only the private copy to obtain a
committed snapshot; the original SHM is checked without updating its read marks.
Uncommitted WAL frames are not imported. Staging is private
(directory mode `0700` and database mode `0600` on Unix). The destination is
checked before and after the migration receipt is persisted, then reopened with
the exact role and migration denied. Only verified files are published.

Verification includes memory IDs, revisions and lifecycle values, metadata,
vectors, history, ordinary state and companion outbox content. Derived recall
cache entries are invalidated. Ordinary auxiliary rows retain their SQLite
value types and applicable sequence high watermarks. Memory and `hard_state`
internal SQLite rowids are not stable record identities; their logical IDs or
namespace/key pairs are verified instead. Kernel-owned schema/identity stamps
are created by the destination kernel. The profile and owner role live inside
`hard_state`; the migration marker describes the schema. All other admitted ordinary content is
included in the explicit verification projection.

The receipt records the operator identity, source/target versions and content
digests. The returned JSON and the receipt in the destination describe the same
migration. Existing receipts remain ordinary historical state; only the newly
written receipt is excluded from its own digest.

## Rollback

If migration is refused, retain the source and inspect the reason. Do not change
schema versions, role stamps, protected rows or sidecars to bypass admission.
Unsupported historical normalization requires a separately reviewed converter.

After an operational cutover, stop writers before rollback. Restore the original
database and its matching sidecars together with the previous binary. An older
binary must not open the new store. Back up writes made after cutover before
restoring the old database; migration does not reconcile diverged stores.
