# mastodon-post

A WIT component plugin for ZeroClaw that posts a status (toot) to a Mastodon
instance. It's the reference example for **host-injected credentials**: the
plugin needs an access token, but the token is injected by the host at the HTTP
boundary — the WASM guest never sees its value.

## Get an access token

1. On your Mastodon instance (e.g. `https://mastodon.social`), go to
   **Preferences → Development → New application**.
2. Name it anything; under **Scopes** keep at least `write:statuses`.
3. Submit, open the application, and copy **Your access token**.

## Build

```bash
rustup target add wasm32-wasip2
cd plugins/mastodon-post
cargo build --target wasm32-wasip2 --release
cp target/wasm32-wasip2/release/mastodon_post.wasm mastodon_post.wasm
```

## Configure (the secret lives host-side, never in the plugin)

```toml
[plugins]
enabled = true

[http_request]
# The host enforces this allowlist for plugin HTTP. Set it to your instance so
# the token is only ever sent there.
allowed_domains = ["mastodon.social"]

# The token the host injects. With [secrets].encrypt on, this is stored
# encrypted at rest; or set the MASTODON_TOKEN environment variable instead.
[http_request.secrets]
MASTODON_TOKEN = "<your access token>"
```

Then install and run:

```bash
zeroclaw plugin install plugins/mastodon-post
zeroclaw agent -a <your-agent> -m 'Post "hello from a wasm plugin" to my Mastodon at https://mastodon.social'
```

## How the credential reaches Mastodon

1. The guest checks `secret-exists("MASTODON_TOKEN")` (bool only) and POSTs to
   `<instance>/api/v1/statuses`.
2. The host (`PluginHttp`) matches the manifest `[[credentials]]` grant against
   the request URL and adds `Authorization: Bearer <token>` — resolving the
   value from `[http_request].secrets` (decrypting if needed) or the
   environment. The guest never receives the token.

See `docs/book/src/developing/plugin-protocol.md` for the full protocol.
