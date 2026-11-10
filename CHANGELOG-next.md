# ZeroClaw v0.9.0

ZeroClaw v0.9.0 is the first release in which people other than the operator can sign in, spanning **117 commits** from **22 contributors**. The dashboard can sign people in through your own OpenID Connect provider, accounts that aren't administrators get their own scoped chat, and the daemon keeps each account inside what its permission profile allows. The dashboard can also be served through a ZeroRelay, servers can install a build without the dashboard, and the configuration moves to schema version 4: read [Breaking Changes](#breaking-changes) before upgrading.

## In brief

Dashboard sign-in through your own identity provider, with chat for accounts that aren't administrators.

Plus the dashboard through ZeroRelay, a CLI-only server build, and tighter isolation between accounts.

## Highlights

- **Dashboard sign-in through your identity provider.** It's opt-in and off by default.
  - `[gateway.dashboard_sign_in]` offers one or more `[oidc.<alias>]` providers on the dashboard's sign-in screen.
  - The access token stays in the gateway's memory, and the browser holds only an `HttpOnly` session cookie.
  - Every request is checked again against the account's current grants, so a changed permission profile or a removed provider takes effect on the next request.
  - Pairing keeps working alongside it, and an install without the new table behaves exactly as before.

  (#11965, closes #11961; the dashboard offers config pages only to accounts that can read config, #12251)
- **Accounts that aren't administrators get their own dashboard chat.**
  - A signed-in account whose profile grants `sessions` can chat with the agents its grants allow, as itself. The agent uses only the tools the profile grants.
  - Its conversations are its own, and nobody else's dashboard lists them.
  - A conversation lasts as long as its chat; see [Known issues](#known-issues).
  - Revoking access, signing out or the session expiring closes its chats, even while a reply is streaming.

  (#12244, closes #11984; built on #11964 and #12232)
- **Accounts stay inside what their permission profile allows.**
  - Tools that act without knowing who asked are withheld from every account that isn't an administrator, whatever its selectors. Those are SOP steps, scheduled and cron turns, turns on peer agents, nested sub-agents and pipelines, saved daemon config, and the session tools that reach other accounts' sessions. Starting or approving an SOP over RPC needs an administrator (#12232, closes #11123, #11127, #12046 and #12051).
  - A session's owner is checked again at the moment a change is written, so access taken away while a request waits can't still change the session (#11234, #12239, closes #11126).
  - Delegated agents keep the owner's memory scope, and owned sessions' sub-agents and pipelines keep their memory private (#11225, closes #11198; #11266, closes #11239).
  - Cron jobs added over RPC go through the same approval as the CLI and the gateway (#11149, closes #12260).
  - The daemon log isn't readable by scoped accounts (#11326).
  - `[gateway.pairing_dashboard]`'s limits are enforced (#12213, closes #9394).
- **The dashboard through a ZeroRelay.**
  - A relay operator can give a daemon its own dashboard address, such as `https://<label>.relay.example.com/`, so it can be reached without a VPN, an open port or a tunnel. People sign in there with your identity provider.
  - Pairing codes and paired tokens are refused at that address, because the relay would see them. The relay decrypts this traffic to forward it, and the docs say so wherever it applies.
  - Responses reach the browser whole (#12355, closes #12354).

  (#12270)
- **ZeroCode signs in with OIDC, without pairing.**
  - `zerocode login` works from an empty client directory. You approve the sign-in at the identity provider, and the daemon issues the client's certificate after checking the token and the account's grants. Plain `zerocode` then reuses the saved login.
  - An account that may only chat can open ZeroCode: the Config and Logs tabs show the daemon's reason instead of ending startup.

  (#12317, closes #12347; #12359, closes #12357)
- **A CLI-only build for servers.**
  - `install.sh --cli-only` installs ZeroClaw without the web dashboard, and `zeroclaw update --standard` switches to the standard build on request.
  - In that build, dashboard commands say plainly that the dashboard isn't included, and how to get it.

  (#12111, closes #11971; #11975, #11973, #11269)

## What's New

### Sign-in and accounts

- **Dashboard sign-in:** opt-in OIDC sign-in, with sessions in the gateway's memory, CSRF protection, a cookie bound to the dashboard's origin, and sign-out across tabs. WebSocket routes now connect over the gateway's own TLS listener (#11965, closes #11951 and #11961).
- **Scoped dashboard chat** for accounts that aren't administrators, through the core on a shared RPC client (#12244, closes #11984).
- **ZeroCode:** `zerocode login` for pairing-free OIDC sign-in with saved, URL-keyed credentials (#12317). It opens for accounts the daemon refuses Config or Logs (#12359).
- **Orphaned remote sessions are retired.** A remote client's sessions that nobody reconnects to are closed after `wss.orphaned_session_timeout_secs` (30 minutes by default), so they no longer fill the daemon's 64 live sessions (#11964, closes #11962).
- **Renaming an agent** carries the new name into permission-profile selectors (#11226, closes #11009).
- **The dashboard** offers config pages only to accounts that can read config (#12251, closes #12249).

### Relay

- **Relay-served dashboards:** `[relay.dashboard]` on the daemon and `[dashboard]` on the relay, both off by default. The daemon side requires dashboard sign-in (#12270).

### ZeroCode, Dashboard and Web

- **ZeroCode:**
  - `/load-session` resumes a saved session by name or id (#12071, closes #12069);
  - Config save and cancel behave the same in every editor (#11511, closes #11486);
  - a dropped question from the agent is answered and recorded instead of leaving the agent waiting (#11624, closes #11623).
- **Web:**
  - a toolbar toggle keeps the live Thinking box collapsed (#11920, closes #11919);
  - the chat header stays on screen in narrow layouts (#11933).

### Channels

- **Signal** sends and receives files: photos, voice notes and documents are saved into the agent's workspace (#11556, closes #7891).
- **Email** can be configured inbound-only, without SMTP. Leave `smtp_host` and `from_address` unset or blank (#12003).
- **Telegram's** notice for people who aren't authorized can be set with `channels.telegram.<alias>.unauthorized_message`, which accepts `{identity}` and `{bind_command}`. The default distinguishes first-user pairing from an existing access list (#11728, closes #10400).
- **WhatsApp Web** renders horizontal rules and setext headings (#11054, closes #11052).

### Install and builds

- **CLI-only:**
  - `install.sh --cli-only` (or `--preset cli-only`) and `zeroclaw update --standard` (#12111);
  - builds without the gateway point dashboard commands to the standard build (#11975);
  - agent deletion and rename clean up owned state without the gateway (#11973);
  - metrics and embedded-web no longer switch the gateway on (#11269).
- **The Linux and Windows desktop apps** embed the dashboard, as the macOS app does (#11272).

### Runtime, Skills and Tools

- **A built-in `zeroclaw` docs skill** helps an agent find the documentation for the installed version (#9579).
- **A tool call that can't run** in the session is refused before it asks for approval (#12254, closes #12248).
- **Configured agent turns keep the repeated-tool-failure safeguards on,** and preserve the work already done (#11507, closes #11484).
- **`keep_tool_context_turns` counts turns again** instead of acting as a switch (#11297).
- **Claude 4.7 and later models get the thinking format they accept.** Opus, Sonnet, Haiku, Fable and Mythos models from version 4.7 on use adaptive thinking, including names a gateway decorates with a prefix, a date, or a suffix such as `-fast`. Before, newer models such as Claude Opus 5.5 rejected the request. Older models keep the budget format (#12352).
- **No more forced temperatures.** These now leave the temperature to the turn or the model, so they work with models that reject one, such as `claude-opus-5-5`:
  - the skill review after a turn (#12363, closes #12362);
  - the heartbeat's decision step and the model-routing tool's probe (#12368, closes #12366).
- **Turn entry points take the capabilities they build from.** This is groundwork for the isolation work above (#11174).

### Security

- **Advisories fixed:**
  - Wasmtime 48.0.8 for RUSTSEC-2026-0420 to RUSTSEC-2026-0424 (#12129, closes #12128). Precompiled `.cwasm` components built for 48.0.7 keep working.
  - `vodozemac` 0.11.2 for RUSTSEC-2026-0445 and RUSTSEC-2026-0451, in the Matrix channel (#12273, closes #12272).
  - `pulldown-cmark` 0.13.6 for RUSTSEC-2026-0414 and RUSTSEC-2026-0433 (#12095, closes #12094; #12200, closes #12196).
- **OIDC enrollment:**
  - browser enrollment callbacks are bound to the browser that started them (#12157, closes #12050);
  - the discovered authorization endpoint is held to the URL policy (#12313, closes #12312);
  - Basic credentials are form-encoded (#11968, closes #11957);
  - enrollment codes are redeemed on the daemon's own pairing guard (#11940).
- **Gateway:**
  - OIDC bearer verification on the config routes is rate-limited (#12233, closes #12047);
  - behind a trusted proxy, limits are keyed on the proxy-appended `X-Forwarded-For` hop (#12114, closes #12052).
- **Command scanning:**
  - POSIX commands are split at substitution and subshell boundaries (#11822);
  - a bare semicolon or pipe in an incoming message is no longer treated as command injection (#12068).
- **Accepted with a tracking issue:** RUSTSEC-2026-0459, SQLite bundled in `libsqlite3-sys` (#12321; see [Known issues](#known-issues)), and RUSTSEC-2026-0441, `pin-utils` unmaintained (#12253).

### Plugins

- **An interrupted `plugin install`'s stage** is cleared by the next install (#11855, closes #11792).
- **Discovery admits a component only beside the manifest that described it,** so a discovery during `plugin update` can't pair one version's manifest with another's component (#12040, closes #11562).

### Documentation

- **A new [Identity providers](https://docs.zeroclaw.com/v0.9.0/en/security/identity-providers.html) page** lists what a provider needs for sign-in, the providers tested with v0.9.0 and their settings, those known not to work yet, and the v0.9.0 limitations (#12365).
- **The generated API reference and the per-language "page not found" pages** are kept out of search engines, so they no longer compete with the docs pages (#12327, closes #12324).
- Email approval routes start the IMAP listener (#12243, closes #12217).
- Gateway policy publication guidance (#11268) and a `call_channel!` correction (#11686).
- Community links (#11639, #11415).

## Bug Fixes

| Area | Fix |
|---|---|
| ZeroRelay | Dashboard and frontdoor responses reach the browser whole instead of waiting for the idle timeout (#12355, closes #12354) |
| ZeroCode | Opens for an account the daemon refuses Config or Logs (#12359, closes #12357) |
| ZeroCode | Clipboard image temporary files are cleaned up in more disconnect paths (#10485) |
| Web | The in-flight prompt survives a reload while the agent is still answering (#11571, closes #11517) |
| Web | The config form's Save stays inactive after the Enable/Disable pill saves (#12100, closes #12087) |
| Install | A glibc host gets the glibc archive even with a musl loader installed (#12160, closes #12153) |
| Install | Install and uninstall messages show the package version, not the build id (#12159, closes #12154) |
| CLI | Leaving quickstart says nothing was configured, instead of claiming no config was written (#12162, closes #12155) |
| Config | Lifecycle remedy commands work when the config directory starts with a dash (#12172, closes #11952) |
| Channels | Allowlisted Discord bots can't answer tool approvals (#12065, closes #12064) |
| Channels | Telegram withdraws its acknowledgement reaction when a reply finishes (#11739) |
| Channels | "Reply delivered" is logged only after the platform accepts the reply (#11979, closes #12093) |
| Runtime | Turns a killed process left marked running are settled (#11523, closes #11432) |
| MCP | Tool-result images move to a user message for OpenAI-compatible endpoints that reject them in a tool result (#10502, closes #10501) |
| MCP | Deferred MCP tool names are listed in sorted order, so prompt caching holds (#12177, closes #12176) |
| Tools | `deliver_file` no longer asks the model to write the attachment URI into its reply (#12179, closes #12178) |
| Tools | The model-routing tool publishes which fields a scenario update needs (#11720, closes #11708) |
| ACP | A shell tool call's title is the command, not `shell` (#12286) |
| SOP | Step scope groups match the registered tool names (#11283) |
| Containers | Image builds stage every workspace member during dependency prefetch (#12212, closes #12210) |

## Breaking Changes

- **Configuration schema version 4** (#11218, closes #11579).
  - After upgrading, run `zeroclaw config migrate` once. Until then configs load, migrated in memory, but `config set`, `config init`, `config patch` and `models set` refuse.
  - `[security.nevis]`, the eight agent-inline tunables and `summary_model` are no longer config keys. Set tunables on the agent's `[runtime_profiles.<profile>]`, and use `[oidc.<alias>]` with `[users]` and `[permission_profiles]` in place of Nevis. Top-level `[twitter]` and `[reddit]` and `[channels.notion]` are dropped.
  - A config without `schema_version` prints a warning on every command until it has one.
  - **The downgrade copy.** The first rewrite of a version 3 file keeps it as `config.toml.v3.backup`: owner-only, written once, and readable by an older binary. **It still holds every retired key, including plaintext secrets** such as a Nevis `client_secret`. Delete it once you no longer need to downgrade.
- **Accounts that aren't administrators lose tools that act without knowing who asked** (#12232). That includes SOP execution and decisions over RPC, cron agent jobs, peer turns, nested sub-agents and pipelines, saved daemon config and other accounts' sessions, even with wildcard selectors. Run and approve SOPs as an administrator or the shared operator. Granting `tools:execute` alone doesn't restore them.
- **Under supervised autonomy, RPC `cron/add` and `cron/patch` refuse medium- and high-risk commands** instead of storing jobs that fail at run time (#11149). Jobs stored earlier still fail on every tick with `requires explicit approval`; `cron list` shows them. Remove or patch them.
- **The filesystem channel also refuses macOS's and Windows's own system roots,** as it already refused the Unix ones (#11406).
  - If a `[channels.filesystem.<alias>]` listener stops starting with `path '...' is a broad system root`, point `paths` at a narrower folder, for example `/Users/<name>/Inbox` instead of `/Users/<name>`.
  - Or set `allow_broad_roots = true` on that alias.
- **`[gateway.pairing_dashboard]` is now read** (#12213). A saved `code_ttl_secs` above 600 loads as 600 with a warning, and `max_pending_codes` is dropped. Values outside the new ranges are refused at load: `code_ttl_secs` 30–600, `max_failed_attempts` 1–100, `lockout_secs` 1–86400.
- **Remote sessions that nobody reconnects to are closed after 30 minutes** (#11964). Set `wss.orphaned_session_timeout_secs = 0` to keep them until the daemon restarts.

## Known issues

These issues ship in v0.9.0. None of them weakens a security check; where behaviour is affected, it fails closed. [Identity providers](https://docs.zeroclaw.com/v0.9.0/en/security/identity-providers.html) lists the providers tested for sign-in and what each needs.

**Sign-in and accounts:**
- **Google, Microsoft Entra ID, Okta and Amazon Cognito aren't supported for sign-in yet.** Only the providers on the Identity providers page are known to work: Keycloak 26.8, self-hosted Zitadel v4.20 and Authelia 4.39. Check any other provider against the page's requirements before relying on it.
- **Logto can't sign people in yet.** Its access tokens for people are opaque unless the request names an RFC 8707 `resource`, which ZeroClaw doesn't send. Logto service clients work with a default API resource and an RSA signing key; ZeroClaw doesn't verify Logto's default ES384 key.
- **There's no per-provider certificate authority, no configurable scopes and no RFC 8707 `resource` yet.** An issuer's certificate must chain to a CA the daemon already trusts, and only the `openid` scope is requested.
- **A provider outage when a session is verified again signs dashboard users out.** That's at an introspection revalidation deadline, or after a config reload or policy change. The dashboard says the session expired; sign in again once the provider is back (#12096).
- **The CLI's gateway admin commands don't work when the gateway serves TLS itself.** That covers `gateway get-paircode`, `sop pending`, `logs`, `approve`, `deny` and `gateway restart`, which always connect over `http://` (#12091).
- **A scoped account's dashboard chat ends with its connection.** Reloading, leaving the chat or a dropped connection ends the conversation; a new chat starts a new one, and the transcript can't be reopened in the dashboard. Use ZeroCode for history that outlasts a chat.

**ZeroCode** (both are regressions from #12317, with fixes planned for v0.9.1):
- **Signing in to a second daemon with `zerocode login` cuts the first daemon's manual connection off from its credentials.** Nothing is deleted. Re-run `zerocode login` for the first daemon, or set `[connection.wss].uri` back by hand.
- **The connection editor can show and edit the wrong TLS settings when the URL lacks its trailing slash.** Type the URL with its trailing slash, or re-run `zerocode login`.

**Desktop:**
- **Plugins aren't confirmed to run in the notarized macOS desktop app** (#11829).

**Dependencies:**
- **The web dashboard's build and test tooling has open advisories:** `brace-expansion`, `vite`, `happy-dom` and `postcss-selector-parser`. None of them is in the shipped dashboard, so they affect only people building or testing the web code. The bumps land after v0.9.0 (#11853).
- **The bundled SQLite advisory RUSTSEC-2026-0459 is accepted for this release.** It's local, and needs a crafted database and `-wal` pair. The fix needs `rusqlite` 0.41, which `matrix-sdk-sqlite` doesn't allow yet (#12320).

**Plugins** (carried from v0.8.7):
- **The host doesn't record refused WebSocket and socket connections;** only HTTP refusals write a `plugin_egress_denied` record (#11304).
- **A plugin channel that never reaches its service can stay quiet** at the default log level, with its `/health` component left at `starting`.
- **Ten registry channel packages haven't run against their live services:** bluesky, gitea, mattermost, mochat, notion, reddit, telegram, twitter, wechat and wecom.
- **Package lock files created before v0.8.7 keep their mode** (#11360).
- **A reinstalled package can meet configuration it refuses, and installing over an installed package is refused** (#10162).
- **Registry packages have no versioned WIT compatibility check yet.**

**Channels** (carried from v0.8.7):
- **Slack doesn't show "is thinking…" in ordinary channel threads** (#11416).

## Binary size

Per-target size of the `zeroclaw` binary from the release-recipe qualification builds of v0.8.7 and v0.9.0. The cap is 80 MiB (83,886,080 bytes); the largest v0.9.0 binary leaves 5.60 MiB of headroom.

```text
target                               v0.8.7      v0.9.0       delta  delta %
x86_64-unknown-linux-gnu         76,715,464  77,969,256  +1,253,792    +1.6%
x86_64-unknown-linux-musl        76,774,224  78,012,720  +1,238,496    +1.6%
aarch64-unknown-linux-gnu        55,978,512  56,897,504    +918,992    +1.6%
aarch64-unknown-linux-musl       51,685,280  52,616,576    +931,296    +1.8%
x86_64-apple-darwin              64,408,904  65,437,152  +1,028,248    +1.6%
aarch64-apple-darwin             42,860,816  43,561,504    +700,688    +1.6%
x86_64-pc-windows-msvc           64,529,408  65,561,600  +1,032,192    +1.6%
armv7-unknown-linux-gnueabihf    53,063,388  54,075,852  +1,012,464    +1.9%
arm-unknown-linux-gnueabihf      53,382,876  54,407,628  +1,024,752    +1.9%
aarch64-linux-android            45,540,096  46,446,520    +906,424    +2.0%
```

## Contributors

- @Aarlington
- @almirb
- @Audacity88
- @GaijinSystems
- @IftekharUddin
- @joalvaradon
- @JordanTheJet
- @Leon-SK668
- @magnus919
- @metalmon
- @mov-xound-glitch
- @nick-pape
- @nickludlam
- @NiuBlibing
- @Rixnei
- @RustLangLatam
- @singlerider
- @tidux
- @tmielika
- @ump45nose
- @vadelma-agent
- @wray-lee

## Full Changelog

https://github.com/zeroclaw-labs/zeroclaw/compare/v0.8.7...v0.9.0
