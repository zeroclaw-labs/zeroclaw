# Creating a Gitea / Forgejo token (Codeberg)

The [Git channel](./git.md) with `provider = "gitea"` or `provider = "forgejo"` authenticates with a **personal access token** against the instance's Gitea-compatible REST API, and replies as the token owner. Both providers share one internal implementation; the only real difference from GitHub is that there is no app, only a token and an explicit API base URL.

Examples below use [**Codeberg**](https://codeberg.org) (a public Forgejo instance). For a self-hosted Gitea or Forgejo, substitute your own host.

> **Official docs:** Forgejo's [Access Token Scope](https://forgejo.org/docs/latest/user/token-scope/) is the upstream reference for token scopes; on Codeberg, follow [Generating an Access Token](https://docs.codeberg.org/advanced/access-token/). Gitea instances expose the same token UI.

## 1. Use a dedicated bot account

Create the token on a **separate bot account**, not your operator account. The channel ignores its own activity: if the token owner is also the human who @-mentions the app, those messages are silently skipped. A dedicated account keeps the bot's replies and your own comments distinct.

On Codeberg, register a normal second account for the bot and invite it to the target repositories (or the org) with write access.

## 2. Generate the token

As the bot account: **Settings → Applications → Manage Access Tokens** (Codeberg: `https://codeberg.org/user/settings/applications`).

1. Give the token a name (e.g. `zeroclaw`).
2. **Select scopes.** The channel needs, at minimum:
   - `read:user`: the channel resolves its own bot identity from `/user` at startup.
   - **Repository** read plus issue/PR write. On Forgejo/Codeberg the scopes are grouped `read:repository` + `write:repository` and `read:issue` + `write:issue`. If the UI only offers coarse `repository` / `issue` scopes, tick those.
3. **Generate Token** and copy it. It is shown once.

The token needs repository read access plus issue/PR comment write access for replies and reactions. Grant nothing beyond what the target repos require.

## 3. Find the API base URL

This is the part with no default. The channel **fails closed at startup** if `api_base_url` is unset, because every request carries the token as a bearer credential and it will not guess a host to send it to.

The value is the instance root plus `/api/v1`:

- Codeberg: `https://codeberg.org/api/v1`
- Public Gitea service: `https://gitea.com/api/v1`
- Self-hosted: `https://git.example.org/api/v1`

## 4. Map onto the config

Set each field below on whichever surface you prefer. The access token is an encrypted secret and gets its own masked widget; the rest are plain fields.

**`provider`**: `gitea` for a Gitea instance, `forgejo` for a Forgejo instance (including Codeberg). They behave identically, and an unknown value is a clear startup error rather than a silent fallback.

{{#config-set channels.git.<alias>.provider}}

**`api_base_url`**: the instance root plus `/api/v1` (step 3). Required; startup fails closed without it.

{{#config-set channels.git.<alias>.api_base_url}}

**`access_token`**: the token from step 2.

{{#secret-config channels.git.<alias>.access_token}}

**`repos`**: the `owner/repo` list to watch. Leave empty to poll every repo the token can see.

{{#config-set channels.git.<alias>.repos}}

For the full field reference, see the [Git channel](./git.md#configure) page.

## 5. Verify

The git channel is **not** in the lean default build. Build it with `--features channel-git` (or the broader `channels-full`):

```bash
cargo build --features channel-git
```

`channel-git` pulls in every wired forge provider in one build; there is no smaller per-provider subset, and building a bare `provider-*` feature without `channel-git` does not register the channel.

On startup the channel calls `/user` to resolve its bot login, logs an `IDENTITY OK` line, and begins polling. @-mention the bot on an issue or PR in a configured repo to confirm it replies. If startup fails complaining about `api_base_url`, the base URL is missing or blank. See the [Git channel](./git.md) page for event routing, peer-group binding, and operating notes.

## Next steps

- **Back to the channel:** [Git channel](./git.md) for event routing, streaming, rate budget, and safety.
- **Restrict who can reach the agent:** [Peer Groups](./peer-groups.md).
- **Drive automation from forge events:** [Standard Operating Procedures](../sop/index.md) and the [Git SOP fan-in](../sop/fan-in/git.md).
- **New to ZeroClaw?** [Quickstart](../getting-started/quickstart.md) and [Concepts](../getting-started/concepts.md).
