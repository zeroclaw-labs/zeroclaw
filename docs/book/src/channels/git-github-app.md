# Creating a GitHub App

The [Git channel](./git.md) with `provider = "github"` authenticates as a **GitHub App**, not as a user. The app has its own bot identity (`your-app[bot]`), its own permissions, and works on every repository it is installed on. This page walks the one-time app creation and maps each value onto the channel config.

Works for both a **personal account** and an **organization**: the only difference is where the app is owned and who can install it.

> **Official docs:** GitHub's own [Registering a GitHub App](https://docs.github.com/en/apps/creating-github-apps/registering-a-github-app/registering-a-github-app) and [Managing private keys for GitHub Apps](https://docs.github.com/en/apps/creating-github-apps/authenticating-with-a-github-app/managing-private-keys-for-github-apps) are the upstream reference for everything on this page.

## 1. Create the app

GitHub → **Settings → Developer settings → GitHub Apps → New GitHub App**.

- **Personal app:** create it under your own *Developer settings*.
- **Org app:** switch to the org first (*Settings → Developer settings* on the organization), so the org owns the app and org admins can manage it.

Fill in:

1. **Name** and **Homepage URL**: any valid URL; the app never serves web traffic.
2. **Webhook → Active:** **uncheck** it. The channel polls the REST API; it receives no webhooks and needs no public URL.
3. **Repository permissions**: grant exactly what the channel uses and nothing more:
   - **Issues:** Read & write
   - **Pull requests:** Read & write
   - **Contents:** Read-only (needed for PR file listings and releases)
   - **Metadata:** Read-only (mandatory; auto-selected)
   - **Actions:** Read-only, only if you route `workflow_run.*` events
   - Leave all other permissions at *No access*.
4. **Where can this app be installed?**: "Only on this account" is fine for a personal or single-org setup.

Click **Create GitHub App**.

## 2. Collect the two credentials

On the app's settings page after creation:

- **App ID**: shown near the top. This is `app_id`.
- **Private keys → Generate a private key.** GitHub downloads a single `.pem` (RS256). This is your signing key; GitHub keeps only the public half. You paste its contents into `private_key`; the `.pem` file itself is not referenced at runtime, so once the key is stored you can delete the download.

## 3. Install the app

App settings → **Install App** → pick your account or org → choose **All repositories** or a specific selection. Installation is what grants the app access to repos; without it the app can authenticate but sees nothing.

If the app is installed on exactly one account, you can leave `installation_id` unset: the channel lists installations on first use and auto-selects the sole one, failing fast if it finds zero or several. Set `installation_id` explicitly only when the app has multiple installations (one channel alias serves one installation).

## 4. Map onto the config

Set each field below on whichever surface you prefer. The private key is an encrypted secret and gets its own masked widget; the rest are plain fields.

**`provider`**: set to `github`.

{{#config-set channels.git.<alias>.provider}}

**`app_id`**: the App ID from step 2.

{{#config-set channels.git.<alias>.app_id}}

**`private_key`**: the PEM from step 2. It lives inline in the one encrypted config store; there is no separate key file on disk to protect. Paste the full PEM, BEGIN/END lines included; the dashboard renders it as a masked multi-line field.

{{#secret-config channels.git.<alias>.private_key}}

**`repos`**: the `owner/repo` list to watch. Leave empty to poll every repo the installation can see.

{{#config-set channels.git.<alias>.repos}}

**`installation_id`**: optional, only when the app has multiple installations (step 3). With a single installation, leave it unset and the channel auto-selects.

{{#config-set channels.git.<alias>.installation_id}}

For the full field reference, see the [Git channel](./git.md#configure) page.

## 5. Verify

The git channel is **not** in the lean default build. Build it with `--features channel-git` (or the broader `channels-full`):

```bash
cargo build --features channel-git
```

`channel-git` pulls in every wired forge provider in one build; there is no smaller per-provider subset, and building a bare `provider-*` feature without `channel-git` does not register the channel.

On startup the channel mints an app JWT, exchanges it for an installation token, and resolves its own bot login. A `pull_request.opened` or an @-mention of the app on an issue should now reach the agent. If startup fails with a missing-private-key error, `private_key` is unset or blank; if it fails with an installation error, the app has zero or multiple installations, so set `installation_id`. See the [Git channel](./git.md) page for event routing, peer-group binding, and operating notes.

## Next steps

- **Back to the channel:** [Git channel](./git.md) for event routing, streaming, rate budget, and safety.
- **Restrict who can reach the agent:** [Peer Groups](./peer-groups.md).
- **Drive automation from forge events:** [Standard Operating Procedures](../sop/index.md) and the [Git SOP fan-in](../sop/fan-in/git.md).
- **New to ZeroClaw?** [Quickstart](../getting-started/quickstart.md) and [Concepts](../getting-started/concepts.md).
