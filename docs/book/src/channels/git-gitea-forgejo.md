# Creating a Gitea / Forgejo token (Codeberg)

The [Git channel](./git.md) with `provider = "gitea"` or `provider = "forgejo"`
authenticates with a **personal access token** against the instance's
Gitea-compatible REST API, and replies as the token owner. Both providers share
one internal implementation; the only real difference from GitHub is that there
is no app, only a token and an explicit API base URL.

Examples below use [**Codeberg**](https://codeberg.org) (a public Forgejo
instance). For a self-hosted Gitea or Forgejo, substitute your own host.

## 1. Use a dedicated bot account

Create the token on a **separate bot account**, not your operator account. The
channel ignores its own activity: if the token owner is also the human who
@-mentions the app, those messages are silently skipped. A dedicated account
keeps the bot's replies and your own comments distinct.

On Codeberg, register a normal second account for the bot and invite it to the
target repositories (or the org) with write access.

## 2. Generate the token

As the bot account: **Settings → Applications → Manage Access Tokens**
(Codeberg: `https://codeberg.org/user/settings/applications`).

1. Give the token a name (e.g. `zeroclaw`).
2. **Select scopes.** The channel needs, at minimum:
   - `read:user`: the channel resolves its own bot identity from `/user` at
     startup.
   - **Repository** read plus issue/PR write. On Forgejo/Codeberg the scopes
     are grouped `read:repository` + `write:repository` and
     `read:issue` + `write:issue`. If the UI only offers coarse
     `repository` / `issue` scopes, tick those.
3. **Generate Token** and copy it. It is shown once.

The token needs repository read access plus issue/PR comment write access for
replies and reactions. Grant nothing beyond what the target repos require.

## 3. Find the API base URL

This is the part with no default. The channel **fails closed at startup** if
`api_base_url` is unset, because every request carries the token as a bearer
credential and it will not guess a host to send it to.

The value is the instance root plus `/api/v1`:

- Codeberg: `https://codeberg.org/api/v1`
- Public Gitea service: `https://gitea.com/api/v1`
- Self-hosted: `https://git.example.org/api/v1`

## 4. Map onto the config (this branch)

The Gitea/Forgejo provider reads these `GitConfig` fields
(`crates/zeroclaw-config/src/schema.rs`):

| Field | Source | Notes |
| --- | --- | --- |
| `provider` | `"forgejo"` or `"gitea"` | Both use the Gitea-compatible provider; pick the one matching your instance. |
| `api_base_url` | Instance root + `/api/v1` | **Required.** Startup fails closed without it. |
| `access_token` | Token from step 2 | Secret, encrypted at rest. |
| `repos` | `["owner/repo", …]` | Empty polls every repo the token can see. |

Codeberg example:

```bash
zeroclaw config set channels.git.codeberg.provider forgejo
zeroclaw config set channels.git.codeberg.api-base-url https://codeberg.org/api/v1
zeroclaw config set channels.git.codeberg.access-token "$CODEBERG_TOKEN"
zeroclaw config set channels.git.codeberg.repos '["your-org/your-repo"]'
zeroclaw config set channels.git.codeberg.enabled true
```

The resulting TOML (`access_token` stored as an `enc2:` secret):

```toml
[channels.git.codeberg]
enabled = true
provider = "forgejo"
api_base_url = "https://codeberg.org/api/v1"
access_token = "enc2:…"
repos = ["your-org/your-repo"]
```

Use `provider = "gitea"` for a Gitea instance and `provider = "forgejo"` for a
Forgejo instance (including Codeberg); they behave identically. An unknown
`provider` value is a clear startup error, not a silent fallback.

## 5. Verify

Build with the channel and start the daemon:

```bash
cargo build --features channel-git
```

On startup the channel calls `/user` to resolve its bot login, logs an
`IDENTITY OK` line, and begins polling. @-mention the bot on an issue or PR in
a configured repo to confirm it replies. If startup fails complaining about
`api_base_url`, the base URL is missing or blank. See the
[Git channel](./git.md) page for event routing, peer-group binding, and
operating notes.
