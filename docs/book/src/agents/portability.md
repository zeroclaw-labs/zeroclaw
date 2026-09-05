# Agent portability

An agent is not a file. It is a config entry that references providers,
profiles, bundles, and MCP servers, plus a workspace directory on disk. Moving
one to another install means moving both halves, and deciding what should not
move at all.

An **agent bundle** is the portable form:

```text
<bundle>/
├── zeroclaw-agent.toml    — manifest: format version, root alias, provenance,
│                             required secrets, carried skill bundles,
│                             dropped refs, risk flags
├── config.toml            — the config closure this agent needs
├── workspace/             — the agent's workspace tree
└── skills/<alias>/        — content of each skill bundle the agent references
```

`config.toml` is a **fragment**, not a whole config. It carries only the
entries the agent references, and it is meant to be read before it is trusted.

## Exporting

<div class="os-tabs-src">

#### sh

```sh
zeroclaw agents export <alias> --out ./my-agent
```

</div>

| Flag | Effect |
| --- | --- |
| `--out <dir>` | Destination bundle directory, created if absent. |
| `--force` | Replace a destination that already has contents. |

Export is read-only against the install and reports three things: the
capabilities a receiving operator would be accepting, the credentials that were
scrubbed, and the configuration that could not travel.

A bundle is **published, not merged**. The export is built in a staging
directory beside the destination and swapped in once it is complete, so:

- `--force` *replaces* the destination. A file from an earlier export that the
  new manifest does not describe is gone, rather than left to look like part of
  the bundle.
- A failure the export reports, such as an unreadable workspace file or a full
  disk, leaves the destination as it was. The staged tree is discarded and any
  existing bundle is put back, so there is no partially written bundle to
  mistake for a complete one.
- A destination that overlaps a tree the export reads is refused before
  anything is created, in both directions and for the workspace and every skill
  bundle alike: an `--out` *inside* a source would have the copy consume its own
  output, and an `--out` that *contains* one would replace the tree being
  exported.
- The names the bundle format controls, a skill-bundle alias and each
  component of a retained identity path, are held to one grammar: what every
  supported target can materialize, not what the exporting host accepts. That
  rules out separators and control characters, and also the Windows names a
  Unix host cannot see are special, such as `con`, `nul` or `lpt1` with or
  without an extension, and names ending in a space or a dot. A reference that
  breaks the rule is dropped with its reason rather than written into a bundle
  that cannot be opened on the other side. Workspace and skill content travels
  under whatever names it already has; this is about the names the format
  itself chooses.
- The closure is proved before it is written. The rendered `config.toml` is
  parsed back and put through the same validation the importing install runs,
  on a config holding nothing else. A reference the source install could
  satisfy but the bundle cannot, such as a provider or bundle entry that was
  never configured, refuses the export and names the field, rather than
  producing a bundle that fails on arrival.
- What `--force` admits is one directory, not one path. The whole copy sits
  between the moment the destination is looked at and the moment the bundle is
  swapped in, so the decision is carried forward as the identity of the object
  it was made about. A destination that appears during that window, or one that
  is replaced by another directory, is refused: it is not the thing the
  operator was shown, and nothing is retired or deleted. The same binding
  covers the parent directory, which is opened once and used for every step
  that follows.

### If the export is killed partway

The swap is two renames, not one: the existing bundle is moved aside, then the
staged one is moved in. That is enough to roll back a failure the export
reports, but it is not crash-atomic. If the process or the host dies between
those two renames, the destination is missing and the previous bundle is intact
beside it, under a name that pairs with the staging directory:

```text
<parent>/
├── .zeroclaw-export-<token>       staged bundle, incomplete (safe to delete)
└── .zeroclaw-export-old-<token>   the previous bundle, unchanged
```

Recover by renaming `.zeroclaw-export-old-<token>` back to the destination.
Both directories are hidden and share the same `<token>`, so a crashed export
leaves a matching pair. A successful export leaves neither.

### What the closure carries

Starting from `[agents.<alias>]`, the export follows every reference that can
be reconstituted elsewhere:

- the agent's `risk_profile` and `runtime_profile` entries;
- its `skill_bundles`, `knowledge_bundles`, and `mcp_bundles` entries;
- the `[mcp.servers]` entries those bundles actually grant, resolved through
  the same path the runtime uses, so a server removed by a bundle's `exclude`
  is absent from the bundle too;
- every provider entry the agent names (`model_provider`, `classifier_provider`,
  `summary_provider`, `tts_provider`, `transcription_provider`), carried
  **keyless**;
- the provider a carried runtime profile names in its own
  `context_compression.summary_provider`, which the agent never mentions but
  the target validates per profile;
- the **content** of each referenced skill bundle, under `skills/<alias>/`.
  Skills live in the install-wide `<install>/shared/skills/` tree rather than
  the agent's workspace, so config alone would import an agent whose skills do
  not exist. Each bundle's own `include` and `exclude` filter the copy, so a
  skill the bundle excludes does not travel.

The manifest's `skill_bundles` list records what the bundle *contains*, not what
the export set out to carry. A bundle configured on the agent but with no
directory on the exporting host is named in `dropped` with reason
`source_missing` instead, and raises no `carried_skills` flag, so the artifact
never advertises a `skills/<alias>/` tree it does not have.

The test of the closure is that it stands alone: parsed on an install with no
other entries present, it passes `Config::validate()`.

Values equal to the schema default are pruned, so the fragment shows the
choices someone actually made rather than the whole schema.

### What it deliberately leaves behind

Each omission is recorded in the manifest's `dropped` list with a reason, so
nothing disappears silently.

| Dropped | Reason |
| --- | --- |
| `channels` | Names accounts and credentials that exist only on the source install. |
| `delegates`, `workspace.access`, `workspace.read_memory_from` | Name sibling agents that will not exist on the target. |
| `workspace.path` | A source-host absolute path. |
| `identity.aieos_path` | Kept only when it resolves to a file inside the exported workspace under the format's own path grammar: `/`-separated, no `..`, no backslashes, drive or UNC prefixes, or control characters, since the importing host may read those differently than the exporting one. A kept path is rewritten into that grammar (normalized, `/`-separated on every exporting platform), so the string in `config.toml` is the string that names the carried file. Paths into `memory/`, and paths whose file the copy did not carry, are dropped too. |
| `delegate_same_risk_profile` | Set to `false`: same-profile auto-delegation would otherwise reach agents on the target this one has never been paired with. |
| `skill_bundles.<alias>.directory` | Dropped when absolute, since it names the source host, and the target resolves its default location for the alias. A directory outside `<install>/shared/`, the tree the skill-bundle contract owns, is dropped together with its content: the bundle's config travels, but its skills are not the install's to export. |
| `a2a` | An outward-facing surface; the agent must be re-published deliberately. |
| `cron_jobs` | Not carried by bundle format 1. |
| `workspace/memory/` | The memory store. See below. |
| `workspace/MEMORY_SNAPSHOT.md` | The core-memory export the store re-hydrates from: memory in another form. |

### Memory does not travel

Bundle format 1 carries no memory, and there is no flag to opt in. The store is
a live SQLite database in WAL mode: while the agent is running, its committed
state is spread across the database and its `-wal` sidecar, so copying those
files one at a time can capture a torn or stale database. A bundle is not a
place to discover that. Carrying memory needs a real snapshot boundary, taken
through SQLite's own backup API against a quiesced store, which a later format
version can add.

Both forms memory takes on disk stay behind: the store under `workspace/memory/`
and the `MEMORY_SNAPSHOT.md` at the workspace root that an agent re-hydrates the
store from. An imported agent starts with an empty memory.

Symlinks inside the workspace are skipped rather than followed: a link's target
may sit outside the workspace, and it would resolve differently on the
receiving host anyway.

The source roots themselves are checked first. A workspace or skill-bundle
directory that is a symlink is refused rather than followed, because following
it would put the whole copy outside the tree the bundle claims to carry before
any of the per-entry checks below run. Point the config at the real directory
and export again.

Its ancestors are checked too, by construction rather than by comparison: a
source that uses its default location is opened by walking each component from
the install root (`shared`, then `skills`, then the bundle directory; `agents`,
then the alias, then `workspace`), each through the previous component's
handle, refusing a symlink at every step. A redirected `shared/skills` or `agents` is
refused no matter where it points, even somewhere else inside the install,
because skill content is only ever read through real directories under
`shared/` and a default workspace through real directories under `agents/`.
Symlinks *above* the install root are the operator's own path and are followed,
with the same trust the config file's location gets. A `workspace.path` the
operator configured elsewhere is exempt: there the configured location is the
boundary, and only its final component is refused as a link.

The workspace stays live while the export runs, since the agent that owns it can
be writing to it through an ordinary tool call. So the copy never re-opens a
path by name. The workspace root is opened once, and every entry below it is
classified *and* read through a handle on the directory that holds it, with both
steps refusing to traverse a symlink. That is what makes the skip a guarantee
rather than a check: an entry replaced by a link in between fails the export
rather than redirecting it.

Refusing to follow a name is not the same as proving the bytes belong to this
tree, so two further checks run on the handle the copy is about to read. A file
carrying more than one name is skipped, because a hard link is a second name for
one object that may live anywhere on the host. And the opened object must be the
one that was classified, compared by filesystem identity rather than by shape:
an entry unlinked and replaced by a different host file would otherwise pass
every test, being a regular file with one link inside the same directory.

Refusing the link matters even when it points *inside* the workspace. The bundle
has content boundaries of its own, and a link that never escapes the root can
still cross them: an admitted file replaced by a link to `memory/brain.db` would
carry the memory store under the admitted name, and an admitted skill replaced by
a link to one the bundle excludes would carry the excluded content. Neither
leaves the workspace, and neither is allowed.

### Credentials

Every field the schema marks secret is scrubbed to an empty string, and its
config path is listed under `required_secrets` in the manifest. The paths are
the ones `zeroclaw config set` accepts, so filling a bundle in is a direct
copy-paste:

```sh
zeroclaw config set providers.models.anthropic.main.api-key
zeroclaw config set mcp.servers.github.env.GITHUB_TOKEN
```

Scrubbing is verified, not assumed: if encrypted config ciphertext survives
into the closure, the export aborts rather than writing the bundle.

#### What scrubbing does not do

Scrubbing blanks the fields the schema marks secret. It is not credential
detection, and it does not look at the values it carries. Every other string in
the closure travels exactly as configured:

| Carried as written | A credential ends up there when |
| --- | --- |
| `mcp.servers.<name>.url` | The endpoint carries a token or signed query string. |
| `mcp.servers.<name>.command`, `.args` | A key is passed on the command line rather than through `env`. |
| `providers.*.api_url` | A self-hosted endpoint embeds an access token. |

The manifest repeats stdio server command lines verbatim in `risk_flags`, so a
credential in `args` is in the manifest as well as the config fragment.

Read those values before sharing a bundle. Two things need your eyes rather than
the schema's: the strings described here, and the carried files described in
[Bundle content is not scanned](#bundle-content-is-not-scanned).

### Bundle content is not scanned

Scrubbing is a schema-driven pass over the config closure. It does not reach the
files a bundle carries, and nothing else does either. Filters decide *which*
files travel; nothing inspects what is inside the ones that do.

Each regular file that survives the workspace and skill filters is copied
byte for byte and is never scanned for secrets. What does not travel at all is
listed elsewhere on this page: the [memory store and its
snapshot](#memory-does-not-travel), [symlinks, special files, and hard
links](#exporting), skills a bundle's `exclude` rejects, and loose state at a
skill bundle's root.

So a `.env` file in the workspace, an API token pasted into a note, a
`.git/config` whose remote URL carries a credential, or a private document the
agent was working on all travel exactly as they are. The export tells you how
many files it carried, but it cannot tell you what is in them.

Read the carried files yourself before sharing a bundle, the same way you would
read a repository before making it public. `config.toml` and the manifest are
the parts the export can vouch for.

### Risk flags

The manifest's `risk_flags` list names each capability in the bundle that
widens the receiving install's trust boundary, bound to the config path that
grants it.

| Flag | Raised by |
| --- | --- |
| `full_autonomy` | `level = "full"`, no per-operation approval gate. |
| `filesystem_escape` | The *effective* policy is unconfined: `workspace_only = false`, `level = "full"` (which forces confinement off whatever `workspace_only` says), or `workspace.unrestricted_filesystem = true`. |
| `sandbox_disabled` | `sandbox_enabled = false`. |
| `approval_bypass` | `block_high_risk_commands` or `require_approval_for_medium_risk` turned off. |
| `env_passthrough` | Non-empty `shell_env_passthrough`: host environment variables reach shell subprocesses. |
| `extra_filesystem_roots` | Non-empty `allowed_roots`. |
| `delegation_enabled` | `delegation_policy.mode = "allow"`. |
| `process_spawn` | A stdio MCP server, which starts a local process on the target host. |
| `untrusted_startup_context` | An MCP server's `pinned_resources`: server-controlled text read into the system prompt at startup. |
| `carried_skills` | The bundle carries a skill bundle's content: instructions the agent reads, and files it may run. |

A bundle from an untrusted source is untrusted input. Read `config.toml` and
the manifest's risk flags before importing one, the same way you would read a
script before running it.

## Importing

Not yet implemented. A bundle is applied by hand today: merge `config.toml`
into your install's config, namespacing any alias that collides with one you
already have, copy `workspace/` into the agent's workspace directory, copy each
`skills/<alias>/` into the directory *your* install resolves for that bundle
alias (`<install>/shared/skills/<alias>` unless the bundle sets `directory`),
and supply the credentials listed in `required_secrets`.

Two rules the future `zeroclaw agents import` will enforce, and that a manual
merge should follow:

- **An import never overwrites an existing entry.** A bundle referencing
  `risk_profiles.default` must not modify *your* `default` profile. Namespace
  the incoming alias, or explicitly point the agent at a local one.
- **The merged config must pass `Config::validate()` before it is saved.** A
  dangling reference is a failed import, not a broken next boot.

## Format version

Bundles carry `format_version = 1`. An exported closure is self-sufficient: it
loads and validates on a fresh install with no other entries present.
