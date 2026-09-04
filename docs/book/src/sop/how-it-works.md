# How SOPs run

## Runtime contract

- SOP definitions are loaded from `<shared>/sops/<sop_name>/SOP.toml` plus optional `SOP.md`.
- CLI `zeroclaw sop` manages definitions offline: `list`, `validate`, `show`, `graph`, `rename`, `delete`.
- SOP runs are started by a live event fan-in (authenticated webhook, MQTT, filesystem, or AMQP), by the daemon's periodic SOP maintenance tick for `cron` triggers, or by the in-agent tool `sop_execute`. The remaining trigger types (peripheral and calendar) are defined and matched but not yet wired to a live event source (see [SOP Fan-In](./fan-in/overview.md)).
- Run progression uses tools: `sop_status`, `sop_approve`, `sop_advance`.
- Run state is process-local by default. With `sop.persist_runs = true`, successful initialization of the default SQLite backend stores it under `<data_dir>/sop/runs.db` and restores active runs after restart. Initialization failure logs a warning and falls back to process-local memory.
- SOP audit records are persisted in the configured Memory backend under category `sop`.

Run state and audit history are separate surfaces. See [Background work lifecycle](../architecture/background-work-lifecycle.md) for lifecycle ownership, cancellation, and restart semantics.

## Event flow

```mermaid
graph LR
    MQTT[MQTT listener] -->|topic match| Dispatch
    TOOL[sop_execute tool] -->|manual| Dispatch
    WH[Webhook request] -->|authenticated HTTP fan-in| Dispatch
    CRON[Cron trigger] -->|daemon maintenance tick| Dispatch
    GPIO[Peripheral trigger] -.->|defined, unwired| Dispatch

    Dispatch --> Engine[SOP Engine]
    Engine --> Run[SOP Run]
    Run --> Action{Action}
    Action -->|ExecuteStep| Agent[Agent Loop]
    Action -->|WaitApproval| Human[Operator]
    Human -->|sop_approve| Run
```

## Getting started

1. `sops_dir` is unset by default, so runtime SOP loading is off out of the box. Opt in by setting `sops_dir` through the gateway, zerocode, or `zeroclaw config set`. A relative value resolves against the install root (the directory holding `config.toml`), so the documented `shared/sops` yields `<install>/shared/sops`, the same directory the SOP author writes to. An absolute or `~`-prefixed value is used as-is. Setting it back to `""` (or removing it) disables runtime SOP loading again; the CLI still falls back to `<install>/shared/sops` for offline inspection.

   > **Migrating from an earlier build?** Relative `sops_dir` values now resolve against the install root, matching how `skill-bundles` directories resolve. Earlier builds had **two** different roots for the same setting, so check both before upgrading:
   >
   > | Surface on earlier builds | Root it used | Where `sops_dir = "shared/sops"` landed |
   > | --- | --- | --- |
   > | Runtime loading and local `zeroclaw sop` CLI | `data_dir` | `<data_dir>/shared/sops` |
   > | Web and RPC SOP authoring | `<install>/shared` | `<install>/shared/shared/sops` (doubled segment) |
   >
   > Both now resolve to the single canonical `<install>/shared/sops`. Inspect **both** old locations and move any definitions you find there into `<install>/shared/sops`; definitions left behind in either tree become invisible after upgrade. Definitions authored through the old web or RPC surface are the easiest to miss, because they sit in the doubled writer path rather than the location the docs described.
   >
   > Any other relative value shifts the same way: `sops_dir = "my-sops"` moves from `<data_dir>/my-sops` (runtime and CLI) or `<install>/shared/my-sops` (web and RPC authoring) to `<install>/my-sops`. Absolute and `~`-prefixed values are unaffected.
   >
   > The unset case moves too: the offline CLI fallback used to scan `<data_dir>/sops` and now scans `<install>/shared/sops`.
   >
   > `zeroclaw sop list` reads the new location, so an empty listing after upgrade means definitions are still sitting in one of the old trees.

2. Create a SOP directory, for example:

   ```text
   ~/.zeroclaw/shared/sops/deploy-prod/SOP.toml
   ~/.zeroclaw/shared/sops/deploy-prod/SOP.md
   ```

3. Validate and inspect definitions:

   <div class="os-tabs-src">

   #### sh

   ```sh
   zeroclaw sop list
   zeroclaw sop validate
   zeroclaw sop show deploy-prod
   ```

   </div>

4. Trigger runs via configured event sources, or manually from an agent turn with `sop_execute`.

## Renaming a SOP

A SOP's name is its address: the definition lives in `<sops>/<name>/`, and every
authoring surface persists a SOP under the name it was handed. Saving an edited
SOP under a different name would write a second copy and leave the original
behind, so an edit-save rejects a name change outright. Renaming is its own
operation.

<div class="os-tabs-src">

#### sh

```sh
zeroclaw sop rename deploy-prod deploy-production
```

</div>

The daemon exposes the same operation as the `sops/rename` RPC method, taking
`{"from": "...", "to": "..."}`, and as `POST /api/sops/{name}/rename` with a
`{"to": "..."}` body. A taken target name comes back as a conflict
(`409`, RPC code `-32020`); an unknown source SOP comes back as not found
(`404`, RPC code `-32021`).

A rename:

- refuses a target name that is already taken, so it can never merge two SOPs;
- refuses a target that is not a single path component, the same check every
  other name-taking SOP operation applies;
- refuses a source that is not a real directory sitting in the SOP root. A
  symlink there would pass the name check and then redirect every read and write
  at whatever it points to, so rename checks without following links and confirms
  the source resolves inside the root before reading anything;
- re-runs strict save validation, so it cannot put a definition back on disk
  that the authoring surface would have refused to write;
- changes only the `[sop] name` value in `SOP.toml`, keeping that line's own
  comment and spacing. Steps, triggers, other comments, and `SOP.md` are left
  exactly as they were; the one cosmetic change is that a single-quoted name
  comes back double-quoted.

The directory move is a single filesystem rename and it happens last, so the SOP
exists in exactly one place at every instant: an interrupted rename can leave
neither two copies nor none. If a rename is interrupted between the manifest
rewrite and the move, the SOP keeps its old directory while the manifest already
carries the new name, so `zeroclaw sop list` reports the new name. Re-running the
same rename finishes the job.

Renaming rewrites the definition on disk, exactly like saving or deleting one. A
daemon that already loaded the old definition keeps running it until it reloads
its SOPs, and run history, audit records, and anything else that captured the old
name keeps the old name.

Authoring writes are serialized. Creating, saving, deleting, and renaming all
take an advisory lock on a `.sop-authoring.lock` file in the SOP root before
touching anything, so two of them cannot interleave even across processes: the
CLI writing into a root a running daemon also authors into waits its turn rather
than racing it. An authoring call that cannot take the lock within ten seconds
fails rather than queueing indefinitely. Reads are not serialized and do not need
to be, because every write lands through an atomic rename, so a reader sees one
whole revision or the other.

Each step commits through a rename, so both an ordinary reader and a killed
process see one whole revision. The directory holding each renamed entry is
flushed afterwards, so on Unix the ordering also survives a machine crash. macOS
honors the flush for ordering without forcing the device cache to drain, and
Windows has no directory-sync primitive, so on those platforms the last step of
the guarantee is the filesystem's to keep rather than something ZeroClaw can
force.

For trigger routing and auth details, see [SOP Fan-In](./fan-in/overview.md).
