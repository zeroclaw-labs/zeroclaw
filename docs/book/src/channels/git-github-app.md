# Creating a GitHub App

The [Git channel](./git.md) with `provider = "github"` authenticates as a
**GitHub App**, not as a user. The app has its own bot identity
(`your-app[bot]`), its own permissions, and works on every repository it is
installed on. This page walks the one-time app creation and maps each value
onto the channel config as it exists on this branch.

Works for both a **personal account** and an **organization**: the only
difference is where the app is owned and who can install it.

## 1. Create the app

GitHub → **Settings → Developer settings → GitHub Apps → New GitHub App**.

- **Personal app:** create it under your own *Developer settings*.
- **Org app:** switch to the org first (*Settings → Developer settings* on the
  organization), so the org owns the app and org admins can manage it.

Fill in:

1. **Name** and **Homepage URL**: any valid URL; the app never serves web
   traffic.
2. **Webhook → Active:** **uncheck** it. The channel polls the REST API; it
   receives no webhooks and needs no public URL.
3. **Repository permissions**: grant exactly what the channel uses and nothing
   more:
   - **Issues:** Read & write
   - **Pull requests:** Read & write
   - **Contents:** Read-only (needed for PR file listings and releases)
   - **Metadata:** Read-only (mandatory; auto-selected)
   - **Actions:** Read-only, only if you route `workflow_run.*` events
   - Leave all other permissions at *No access*.
4. **Where can this app be installed?**: "Only on this account" is fine for a
   personal or single-org setup.

Click **Create GitHub App**.

## 2. Collect the two credentials

On the app's settings page after creation:

- **App ID**: shown near the top. This is `app_id`.
- **Private keys → Generate a private key.** GitHub downloads a single
  `.pem` (RS256). This is your signing key; GitHub keeps only the public half,
  so store this copy safely. This maps to either `private_key` (inline) or
  `private_key_path`.

## 3. Install the app

App settings → **Install App** → pick your account or org → choose **All
repositories** or a specific selection. Installation is what grants the app
access to repos; without it the app can authenticate but sees nothing.

If the app is installed on exactly one account, you can leave
`installation_id` unset: the channel lists installations on first use and
auto-selects the sole one, failing fast if it finds zero or several. Set
`installation_id` explicitly only when the app has multiple installations
(one channel alias serves one installation).

## 4. Map onto the config (this branch)

The GitHub provider reads these `GitConfig` fields
(`crates/zeroclaw-config/src/schema.rs`):

| Field | Source | Notes |
| --- | --- | --- |
| `provider` | `"github"` | Default; may be omitted. |
| `app_id` | App ID from step 2 | `u64`. |
| `private_key` | Inline PEM | Secret, encrypted at rest. **Preferred.** Takes precedence over `private_key_path`. |
| `private_key_path` | Path to the `.pem` | Fallback. Used only when `private_key` is unset or blank. Chmod `0600`; looser permissions log a startup warning. |
| `installation_id` | Installation | Optional; set only for multi-install apps. |
| `repos` | `["owner/repo", …]` | Empty polls every repo the installation can see. |

You supply the key **either** inline **or** by path, not both. Inline keeps the
key in the one encrypted config store instead of a second plaintext file on
disk; when both are present, `private_key` wins.

### Inline key (preferred)

```bash
zeroclaw config set channels.git.default.provider github
zeroclaw config set channels.git.default.app-id 123456
zeroclaw config set channels.git.default.private-key "$(cat ~/Downloads/your-app.private-key.pem)"
zeroclaw config set channels.git.default.repos '["your-org/your-repo"]'
zeroclaw config set channels.git.default.enabled true
```

The resulting TOML (`private_key` stored as an `enc2:` secret):

```toml
[channels.git.default]
enabled = true
provider = "github"
app_id = 123456
private_key = "enc2:…"
repos = ["your-org/your-repo"]
```

### Key on disk (fallback)

```bash
chmod 600 ~/.zeroclaw/github-app.pem
zeroclaw config set channels.git.default.provider github
zeroclaw config set channels.git.default.app-id 123456
zeroclaw config set channels.git.default.private-key-path ~/.zeroclaw/github-app.pem
zeroclaw config set channels.git.default.repos '["your-org/your-repo"]'
zeroclaw config set channels.git.default.enabled true
```

## 5. Verify

Build with the channel and start the daemon:

```bash
cargo build --features channel-git
```

On startup the channel mints an app JWT, exchanges it for an installation
token, and resolves its own bot login. A `pull_request.opened` or an
@-mention of the app on an issue should now reach the agent. If startup fails
with an installation error, the app has zero or multiple installations; set
`installation_id`. See the [Git channel](./git.md) page for event routing,
peer-group binding, and operating notes.
