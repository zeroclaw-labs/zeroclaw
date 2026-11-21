# Changelog

All notable changes to this project will be documented in this file.

<!-- changelog start -->

## [0.19.1](https://github.com/matrix-org/matrix-rust-sdk/tree/0.19.1) - 2026-09-18

No significant changes.

## [0.19.0](https://github.com/matrix-org/matrix-rust-sdk/tree/0.19.0) - 2026-09-16

### Added

- Add `SqliteStoreBuilder::high_entropy_passphrase`, a faster alternative to
  `SqliteStoreBuilder::passphrase` for randomly generated, high-entropy
  passphrases. Using this setting once migrates the database from a
  passphrase-based setup to a key-based setup. After the migration,
  `SqliteStoreBuilder::key` can be used instead and is equivalent to
  `SqliteStoreBuilder::high_entropy_passphrase`.

  Do **NOT** use it with human-chosen passphrases, as migrating those to a
  key-based setup would remove their brute-force protection.
  ([#6878](https://github.com/matrix-org/matrix-rust-sdk/pulls/6878))

### Changed

- Only keep the latest gossip request for each secret.
  ([#6631](https://github.com/matrix-org/matrix-rust-sdk/pulls/6631))
- The `EventId`s stored in the Event Cache database are now encrypted as hashes,
  they cannot be decrypted.

  The gap `prev_batch_token` is also encoded from a Rust `String` instead of a
  JSON representation of a `String`, saving a bit of computation and storage
  space.

  The Event Cache database is reset.
  ([#6739](https://github.com/matrix-org/matrix-rust-sdk/pulls/6739))
- [**breaking**] The `open_with_key()` method for the various store
  implementations now accepts a key with any length instead of a 32-byte long
  array. ([#6878](https://github.com/matrix-org/matrix-rust-sdk/pulls/6878))
- The `event-cache` feature flag has been split into `event-cache-store` and
  `media-store`. Both are enabled by default.
  ([#6896](https://github.com/matrix-org/matrix-rust-sdk/pulls/6896))
- Cleared the Sliding Sync `pos` value from the crypto store after
  [this PR](https://github.com/matrix-org/matrix-rust-sdk/pull/6872) emptied the
  event cache. With this, the initial sync should at least return the most
  recent events for the rooms in the room list, which would be missing with he
  previous `pos` value and would force the clients to paginate to load _any_
  events for _any_ timeline, not even the room's latest event would be present.
  ([#7016](https://github.com/matrix-org/matrix-rust-sdk/pulls/7016))

### Fixed

- Double-quotes are for SQL identifiers, while single-quotes are for string
  literals. SQLite however accepts both quotes for string literals, depending on
  some configuration. See the
  [_Double-quoted String Literals Are Accepted_ Section of the SQLite documentation][sqlite-quirks-string-literals].
  This patch fixes a bug where double-quotes were used for a string literal in a
  migration file for the crypto store.

  [sqlite-quirks-string-literals]:
  [<https://sqlite.org/quirks.html#double_quoted_string_literals_are_>][https-sqlite-org-quirks-html-double-quoted-string-literals-are]
  accepted ([#6653](https://github.com/matrix-org/matrix-rust-sdk/pulls/6653))
- Add a uniqueness constraint on `event_chunks` table that ensures an `Event`
  cannot be in a `LinkedChunk` more than once.
  ([#6872](https://github.com/matrix-org/matrix-rust-sdk/pulls/6872))
- Fix a bug in `SqliteCryptoStore::mark_inbound_group_sessions_as_backed_up`,
  `SqliteStateStore::get_profiles` and `SqliteStateStore::get_global_profiles`,
  where passing a large number of sessions or users were erroring to “too many
  SQL variables” in SQLite. This error is due to a manual misuse of the
  `repeat_vars` function in the `chunk_large_query_over` method. The fix is to
  make misuses impossible by introducing the new `ChunkFromLargeQuery` type,
  replacing the `Vec<Key>` in `chunk_large_query_over`, removing the need to
  manually use `repeat_vars`.
  ([#6946](https://github.com/matrix-org/matrix-rust-sdk/pulls/6946))

## [0.18.0](https://github.com/matrix-org/matrix-rust-sdk/tree/0.18.0) - 2026-06-02

No significant changes.

## [0.17.0] - 2026-05-08

### Features

- Implement `CryptoStore::get_pending_key_bundle_details_for_room` and
  `CryptoStore::get_all_rooms_pending_key_bundle`, and process
  `rooms_pending_key_bundle` field in `Changes`.
  ([#6199](https://github.com/matrix-org/matrix-rust-sdk/pull/6199)),
  ([#6233](https://github.com/matrix-org/matrix-rust-sdk/pull/6233))
- Implement new method `CryptoStore::has_downloaded_all_room_keys`, and process
  `room_key_backups_fully_downloaded` field in `Changes`.
  ([#6017](https://github.com/matrix-org/matrix-rust-sdk/pull/6017))
  ([#6044](https://github.com/matrix-org/matrix-rust-sdk/pull/6044))
- [**breaking**] In `EventCacheStore::handle_linked_chunk_updates`, new chunks
  may no longer reference chunk identifiers which do not yet exist in the store
  ([#6061](https://github.com/matrix-org/matrix-rust-sdk/pull/6061))

### Bug fixes

- Fix a panic when the SQLite connection is aborted.
  ([#6091](https://github.com/matrix-org/matrix-rust-sdk/pull/6091))

### Refactor

- Add migration to `SqliteCryptoStore` that removes cross-process lock
  generation key from `kv` table, as this is tracked in `lease_locks` table.
  ([#6326](https://github.com/matrix-org/matrix-rust-sdk/pull/6326))

## [0.16.1] - 2026-05-08

No notable changes in this release.

## [0.16.0] - 2025-12-04

### Features

- Implement new method `CryptoStore::get_withheld_sessions_by_room_id`.
  ([#5819](https://github.com/matrix-org/matrix-rust-sdk/pull/5819))
- [**breaking**] `SqliteCryptoStore::get_withheld_info` now returns
  `Result<Option<RoomKeyWithheldEntry>>`.
  ([#5737](https://github.com/matrix-org/matrix-rust-sdk/pull/5737))
- Implement a new constructor that allows to open `SqliteCryptoStore` with a
  cryptographic key
  ([#5472](https://github.com/matrix-org/matrix-rust-sdk/pull/5472))
- Implement `StateStore::upsert_thread_subscriptions()` method for bulk upserts.
  ([#5848](https://github.com/matrix-org/matrix-rust-sdk/pull/5848))

### Refactor

- [breaking] Change the logic for opening a store so as to use a `Secret` enum
  in the function `open_with_pool` instead of a `passphrase`
  ([#5472](https://github.com/matrix-org/matrix-rust-sdk/pull/5472))

## [0.14.0] - 2025-09-04

No notable changes in this release.

## [0.13.0] - 2025-07-10

### Security fixes

- Fix SQL injection vulnerability in `find_event_relations()`.
  ([d0c0100](https://github.com/matrix-org/matrix-rust-sdk/commit/d0c01006e4808db5eb96ad5c496416f284d8bd3c),
  Moderate, [CVE-2025-53549](https://www.cve.org/CVERecord?id=CVE-2025-53549),
  [GHSA-275g-g844-73jh](https://github.com/matrix-org/matrix-rust-sdk/security/advisories/GHSA-275g-g844-73jh))

## [0.12.0] - 2025-06-10

### Bug fixes

- Fix a `UNIQUE` constraint violation in the event cache store
  ([#5001](https://github.com/matrix-org/matrix-rust-sdk/pull/5001))

## [0.11.0] - 2025-04-11

### Features

- Implement the new method of `EventCacheStoreMedia` for
  `SqliteEventCacheStore`.
  ([#4603](https://github.com/matrix-org/matrix-rust-sdk/pull/4603))
- Defragment an sqlite state store after removing a room.
  ([#4651](https://github.com/matrix-org/matrix-rust-sdk/pull/4651))
- Add `SqliteStoreConfig` and the `open_with_config` constructor on all the
  stores, it allows to control the maximum size of the pool of connections to
  SQLite for example.
  ([#4826](https://github.com/matrix-org/matrix-rust-sdk/pull/4826))
- Add `SqliteStoreConfig::path()` to override the path given to the constructor
  ([#4870](https://github.com/matrix-org/matrix-rust-sdk/pull/4870/))
- Implement `Clone` and `Debug` on `SqliteStoreConfig`
  ([#4870](https://github.com/matrix-org/matrix-rust-sdk/pull/4870/))
- Add `SqliteStoreConfig::with_low_memory_config` constructor
  ([#4894](https://github.com/matrix-org/matrix-rust-sdk/pull/4894))

## [0.10.0] - 2025-02-04

### Features

- [**breaking**] `SqliteEventCacheStore` implements the new APIs of
  `EventCacheStore` for `MediaRetentionPolicy`. See the changelog of
  `matrix-sdk-base` for more details.
  ([#4571](https://github.com/matrix-org/matrix-rust-sdk/pull/4571))
- The SQLite databases are optimized during the construction of the stores. It
  should improve the performance of the queries.
  ([#4602](https://github.com/matrix-org/matrix-rust-sdk/pull/4602))
- The size of the WAL files is now limited to 10MB. This avoids cases where the
  WAL file takes as much space as the database.
  ([#4602](https://github.com/matrix-org/matrix-rust-sdk/pull/4602))

## [0.9.0] - 2024-12-18

### Features

- Add support for persisting LinkedChunks in the SQLite store. This is a step
  towards implementing event cache support, enabling a persisted cache of
  events. ([#4340](https://github.com/matrix-org/matrix-rust-sdk/pull/4340))
  ([#4362](https://github.com/matrix-org/matrix-rust-sdk/pull/4362))

## [0.8.0] - 2024-11-19

### Bug fixes

- Use the `DisplayName` struct to protect against homoglyph attacks.

### Refactor

- Move `event_cache_store/` to `event_cache/store/` in `matrix-sdk-base`.

[https-sqlite-org-quirks-html-double-quoted-string-literals-are]: https://sqlite.org/quirks.html#double_quoted_string_literals_are_
