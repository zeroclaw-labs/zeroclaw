<!-- GSD:project-start source:PROJECT.md -->
## Project

**osAgent**

A tailored fork of zeroclaw v0.7.5 producing two compile-time-separated binaries — `/usr/local/bin/engineer` and `/usr/local/bin/wizard` — that drop into the sovereign-shield platform's existing systemd units to replace the current generic zeroclaw deployment. The engineer is the platform-maintenance agent (talks via Telegram/Slack to the customer's ops team, executes privileged actions through the operator AMQP bridge); the wizard is the planner/install-guide agent that writes secrets to Vault, reachable via OS-MDashboard plus a customer-chosen chat (Telegram, Slack, Mattermost, Matrix, WhatsApp-Cloud, or Signal).

**Core Value:** **Wizard cannot exfiltrate Vault secrets via an MCP server because the wizard binary has compile-time zero MCP, verified by CI (`nm osagent-wizard | grep -i mcp` must return empty).** Every other property — channels, providers, sandbox shape, subagents, audit — flows from "agents that touch high-value secrets must have a provably small attack surface."

### Constraints

- **Upstream license**: zeroclaw is MIT OR Apache-2.0 — fork preserves both, adds osAgent NOTICE. Compatible with any closed-source customer deployment.
- **Compile-time wizard MCP exclusion**: the wizard binary's freestanding ELF symbols MUST NOT contain MCP code; CI enforces via `nm` grep gate. This is the load-bearing safety property and cannot be relaxed.
- **Drop-in to systemd**: binaries are named `engineer` and `wizard` (matching agent identities) so existing systemd units at `/etc/systemd/system/{engineer,wizard}.service` continue to work without unit edits.
- **Install-guide phase ordering preserved**: install-guide CLAUDE.md documents 14 invariant phase orderings, mTLS cert provisioning patterns, sandbox-allowed home-mirror, ExecStartPre preflight, AMQP env file. osAgent install ansible task must respect all of them.
- **No webhook ingress**: rejected by user on security grounds. Custom integration per weird-stack customer instead.
- **Drop the sandbox auto-detect chain**: zeroclaw's default Auto→Landlock→Firejail→Docker→Noop probe is what bit us in 2026-04-22 (Docker wrap broke engineer's bridge access). osAgent ships single `none` backend; config-load rejects `sandbox.enabled != false` unless build feature explicitly overrides.
- **No silent fallback in local-only mode**: airgap customers depend on the property that "local-only" means "no cloud traffic, period." osAgent emits an alert and refuses to serve when oracle is unreachable in local-only mode — never silent failover.
- **Single-tenant config invariant**: every path in config must start with `/opt/sovereign-shield/<customer_id>/` (asserted at config-load). Cross-customer data leakage at the config layer = refuse to start.
- **Plan-then-execute discipline preserved for install-guide work**: any phase that edits sovereign-shield-install-guide MUST emit a Touch/Change/Impact/Rollback plan before edits, per that repo's CLAUDE.md hard rule.
<!-- GSD:project-end -->

<!-- GSD:stack-start source:research/STACK.md -->
## Technology Stack

## Executive Summary
| # | Layer | Pick | Why |
|---|-------|------|-----|
| 1 | AMQP-mTLS client | **`lapin` 4.10** + manual `tokio-rustls` 0.26 TLS stream | Only lapin supports plugging in a pre-built TLS stream via `Connection::connector`, which is what lets us override `ServerName` when dialing 127.0.0.1 with a `shield.internal` SAN cert. `amqprs` 2.x ties TLS to the URI host — won't work for our cert-name-vs-dial-address split without an upstream patch. |
| 2 | SQLite-with-encryption | **`rusqlite` 0.40 with `bundled-sqlcipher-vendored-openssl` feature**, wrapped by `tokio-rusqlite` 0.7 | Vendored OpenSSL avoids host-OpenSSL ABI roulette on Debian/Ubuntu LTS. No system sqlcipher package or C++ toolchain pain. Cost: ~10s longer first compile, ~3 MB binary growth — acceptable. |
| 3 | Vault client | **`vaultrs` 0.8.0** | First-class KV v2 + AppRole, async (reqwest under the hood), MIT. Cleanly wraps for idempotency-key middleware (decision #8 in PROJECT.md). |
| 4 | Markdown-frontmatter parser | **`gray_matter` 0.3.2** with YAML engine | Single small crate, parses Claude-Code-style `---\nyaml\n---\nbody` format directly. No need to assemble pulldown-cmark + serde_yaml manually. |
| 5 | Ed25519 / SSH signature verification | **`ssh-key` 0.6.7** for git-SSH-signed commits (decision #17, #27), **`ed25519-dalek` 2.2** for arbitrary Ed25519 (skill catalog signatures) | `ssh-key`'s `SshSig` matches the on-disk format `git -c gpg.format=ssh` emits exactly. `ed25519-dalek` is the standard for raw key/sig pairs. Both Apache-2.0/MIT. |
| 6 | Telegram bot | **`teloxide` 0.17.0** | Two bots per customer (decision #41) — teloxide's `Bot` is cheap to clone, webhook-via-axum integration matches our existing gateway. MIT. Frankenstein is faster but WTFPL (license-review red flag at some customers). |
| 7 | Slack bot | **`slack-morphism` 2.22.0** | Built-in `SlackEventSignatureVerifier` route, Apache-2.0, actively maintained. |
| 8 | Matrix client | **`matrix-sdk` 0.18.0** | Official matrix.org Rust SDK; Apache-2.0; full E2EE; mature. |
| 9 | Mattermost / WhatsApp-Cloud / Signal | **Wrap `reqwest` in-house for Mattermost and WhatsApp-Cloud**; **out-of-process `signal-cli` daemon for Signal** | All Rust Mattermost crates are abandoned or one-author hobby projects. `whatsapp-cloud-api` 0.5.4 exists but is 0% documented — a 200-line in-house wrapper is lower-risk. **Signal libraries (`presage`, `libsignal-service`, `libsignal`) are all AGPL-3.0 — using any of them statically linked contaminates the whole binary.** Run signal-cli as a separate process (GPLv3, mere-aggregation safe) and talk to it via its JSON-RPC daemon mode. |
## Recommended Stack
### Core Runtime (inherited from zeroclaw — no redecision needed)
| Crate | Version | Purpose | License | Confidence |
|---|---|---|---|---|
| `tokio` | 1.52.3 | Async runtime | MIT | HIGH |
| `tokio-util` | 0.7.18 | `CancellationToken` for lifecycle gates (decisions #18, #21) | MIT | HIGH |
| `axum` | 0.8.9 | Gateway HTTP + WebSocket server (`/ws/chat` for OS-MDashboard) | MIT | HIGH |
| `reqwest` | 0.13.4 | HTTP client (LLM providers, Vault transport, Mattermost/WhatsApp wrappers) | MIT OR Apache-2.0 | HIGH |
| `serde` | 1.0.228 | Serialization | MIT OR Apache-2.0 | HIGH |
| `serde_yaml` | 0.9.x | YAML parsing (subagent frontmatter, MANIFEST.toml YAML siblings) | MIT OR Apache-2.0 | HIGH |
| `toml_edit` | 0.25.12 | Format-preserving TOML edits (config migrations) | MIT OR Apache-2.0 | HIGH |
| `clap` | 4.6.1 | CLI parsing (`osagent-rescue`, `osagent manifest`, `osagent rotate-channel-secret`) | MIT OR Apache-2.0 | HIGH |
| `tracing` | 0.1.44 | Structured logging | MIT | HIGH |
| `tracing-subscriber` | 0.3.23 | Subscriber composition (journald sink + file sink for audit dual-sink #22) | MIT | HIGH |
| `anyhow` | 1.0.102 | Application-level error type | MIT OR Apache-2.0 | HIGH |
| `thiserror` | 2.0.18 | Library-level error derives | MIT OR Apache-2.0 | HIGH |
| `uuid` | 1.23.3 | Correlation IDs | MIT OR Apache-2.0 | HIGH |
| `hex` | 0.4.3 | Hex encoding (hash-chain audit lines) | MIT OR Apache-2.0 | HIGH |
### Net-New for osAgent
| Crate | Version | Purpose | License | Confidence |
|---|---|---|---|---|
| **`lapin`** | **4.10.0** | Native AMQP 0.9.1 client → operator service (decision: replaces shell-invoked `engineer-amqp-bridge`) | MIT | HIGH |
| **`tokio-rustls`** | **0.26.x** | TLS stream layer for lapin + signal-cli bridge; gives us `ServerName` override needed for `127.0.0.1`-with-`shield.internal`-SAN dialing | MIT OR Apache-2.0 OR ISC | HIGH |
| **`rustls`** | **0.23.40** | TLS implementation underneath tokio-rustls (zeroclaw v0.7.5 already pinned to `ring` provider, not `aws-lc-rs` — preserve that pin) | MIT OR Apache-2.0 OR ISC | HIGH |
| **`rustls-pemfile`** | **2.2.0** | Parse client cert + key PEM files at startup | MIT OR Apache-2.0 OR ISC | HIGH |
| **`rustls-pki-types`** | latest from `rustls` workspace | `ServerName::try_from("rabbitmq.shield.internal")` for the override | MIT OR Apache-2.0 | HIGH |
| **`rusqlite`** | **0.40.1** | SQLite bindings with `bundled-sqlcipher-vendored-openssl` feature flag | MIT | HIGH |
| **`tokio-rusqlite`** | **0.7.0** | Async wrapper around rusqlite for our SQLite-on-its-own-thread pattern | MIT | HIGH |
| **`vaultrs`** | **0.8.0** | Vault client: KV v2 writes, AppRole auth, async via reqwest | MIT | HIGH |
| **`gray_matter`** | **0.3.2** | YAML frontmatter extraction from `.claude/agents/*.md` subagent definitions (decision #37) | MIT | HIGH |
| **`ssh-key`** | **0.6.7** | Verify `git -c gpg.format=ssh` signed commits — wizard signs skill catalog / subagent prompts, engineer verifies before invoke (decisions #17, #27) | Apache-2.0 OR MIT | HIGH |
| **`ed25519-dalek`** | **2.2.0** | Raw Ed25519 verify (skill provenance signatures that aren't git-format) | BSD-3-Clause | HIGH (BSD-3 is compatible with MIT/Apache-2.0 distribution; flag for license-review checklist) |
| **`sha2`** | **0.11.0** | SHA-256 for hash-chain audit log (decision #22, #39) and idempotency-key hashing (decision #8) | MIT OR Apache-2.0 | HIGH |
| **`blake3`** | **1.8.5** | Optional: faster alternative for large-blob hashing (skill catalog manifest), if SHA-256 perf becomes an issue. Default to SHA-256. | CC0-1.0 OR Apache-2.0 | HIGH |
| **`teloxide`** | **0.17.0** | Telegram Bot API; webhook-axum integration; two bot instances per customer (decision #41) | MIT | HIGH |
| **`slack-morphism`** | **2.22.0** | Slack Web API + Events API + signature verification | Apache-2.0 | HIGH |
| **`matrix-sdk`** | **0.18.0** | Matrix client | Apache-2.0 | HIGH |
| **`tokio-tungstenite`** | **0.29.0** | WS client for outbound signal-cli daemon connection if we choose WS over JSON-RPC stdio | MIT | HIGH |
### Build / Test / Lint (workspace conventions)
| Tool | Purpose | Notes |
|---|---|---|
| `cargo` workspace | Two binaries under one `Cargo.toml` with `members = ["crates/*", "bins/engineer", "bins/wizard"]` | Top-level `[workspace.dependencies]` pins all versions; member crates use `dep.workspace = true` |
| Cargo features `engineer-bin`, `wizard-bin` | Compile-time module exclusion | **MCP modules must be feature-gated as `#[cfg(feature = "engineer-bin")]` at the `mod mcp;` declaration in `crates/zeroclaw-tools/src/lib.rs`**, NOT only at the public-symbol layer — otherwise the wizard binary still compiles MCP code into the rlib and `nm` will see symbols |
| `cargo deny` 0.18+ | License + advisory + ban policy | Add `deny.toml` entries: ban `presage`, `libsignal-service`, `libsignal-protocol` (AGPL contamination); ban `frankenstein` (WTFPL — informal); allow only `MIT`, `Apache-2.0`, `ISC`, `BSD-3-Clause`, `CC0-1.0`, `MIT-0` |
| `cargo audit` | RUSTSEC advisory check | Wire into PR CI gate |
| `cargo clippy --workspace --all-targets --all-features -- -D warnings` | Lint | Both binary feature sets must pass independently: `--features engineer-bin` and `--features wizard-bin` |
| `cargo fmt --check` | Format gate | |
| `nm $TARGET/release/osagent-wizard \| grep -i -E '(mcp\|model_context)' \|\| true` | CI safety gate (decision #25) | Must produce zero output; non-zero exit fails the build. Run on both debug and release. |
| `cargo nextest` | Faster test runner | Optional but strongly recommended for the integration-test matrix |
### Installation Sketch
# Cargo.toml at workspace root — abridged
# Net-new
# Channels
# Build host only — vendored sqlcipher OpenSSL means runtime needs nothing extra
# clang is NOT required (rusqlite bundled mode uses cc, not bindgen, when feature flags are set correctly)
## Alternatives Considered
| Recommended | Alternative | When the Alternative Would Be Right |
|---|---|---|
| `lapin` 4.10 | `amqprs` 2.1.5 | If we controlled the cert and could make SAN == dial address (no IP/hostname split). amqprs has cleaner ergonomics but its TLS is wired through its `OpenConnectionArguments` URI, not a pluggable stream — we cannot inject our own `ServerName` without forking it. |
| `rusqlite` + `bundled-sqlcipher-vendored-openssl` | `libsql` (Turso fork of SQLite) | If we wanted server-mode or a built-in async driver. libsql is great but adds a 200 KLOC dependency we don't need and its "embedded encryption" story is less proven than upstream sqlcipher. Reject. |
| `rusqlite` + `bundled-sqlcipher-vendored-openssl` | System sqlcipher package + `rusqlite` `sqlcipher` (non-bundled) feature | If the deploy target had a stable, recent `libsqlcipher-dev` package across all customer distros. Ubuntu 22.04 ships sqlcipher 4.5; 24.04 ships 4.5.x; Debian 12 ships 4.5. Bundled is safer because (a) we control the version, (b) no host-OpenSSL ABI mismatch surprises, (c) airgap installs don't depend on apt mirrors. |
| `vaultrs` 0.8 | `hashicorp_vault` | hashicorp_vault is older, less actively maintained, blocking-API in places. vaultrs is the modern choice. |
| `gray_matter` 0.3 | `frontmatter` crate | `frontmatter` is older, less features. `gray_matter` mirrors the JS `gray-matter` library's API which our Claude-Code-style subagent format effectively borrows from. |
| `ssh-key` (for git-SSH-signed) + `ed25519-dalek` (for raw) | `ring` 0.17 for everything | `ring` does Ed25519 verify but does NOT parse SSH signature format (`SSHSIG` magic envelope). We'd reimplement what `ssh-key::SshSig` already gives us. Use `ring` only via the rustls provider where zeroclaw already uses it. |
| `teloxide` 0.17 | `frankenstein` 0.50 (WTFPL) | frankenstein is leaner and faster, but **WTFPL** is informally drafted and gets flagged by enterprise license scanners. teloxide (MIT) is the safe long-term pick. |
| `slack-morphism` 2.22 | `slack-rust` / hand-rolled `reqwest` wrapper | slack-morphism's built-in `SlackEventSignatureVerifier` route is the high-value piece — webhook signature verification is a place we don't want to roll our own crypto. |
| `matrix-sdk` 0.18 | `ruma` (lower-level building blocks) | ruma is what matrix-sdk uses internally. If our needs were minimal (no E2EE, no room state) ruma direct would be lighter. We need E2EE for the privacy story → matrix-sdk wins. |
| **In-house `reqwest` wrapper for Mattermost** | `mattermost_api`, `mattermost-client`, `mattermost-rust-client`, `mattermost-bot` | All four are abandoned or single-author projects with last commits 2+ years ago. A 200-line in-house wrapper around `reqwest` calling Mattermost v4 REST API + WebSocket for events is lower-risk and lower-maintenance than depending on dead crates. Mattermost API is stable. |
| **In-house `reqwest` wrapper for WhatsApp-Cloud** | `whatsapp-cloud-api` 0.5.4 | The crate exists (MIT/Apache-2.0) but documentation coverage is 0% and the API is small (send-message, upload-media, templates). Wrap it yourself for less maintenance surface. Reconsider if `whatsapp-cloud-api` gets sustained contributions. |
| **Out-of-process `signal-cli` (GPLv3) via JSON-RPC** | `presage` 0.7 (AGPL-3.0) | **AGPL contamination risk**: linking `presage` (or `libsignal-service`) into our binary forces the entire osAgent binary under AGPL-3.0, which would block all closed-source customer redistributions. signal-cli as a separate Java process talking JSON-RPC over a Unix socket is **mere aggregation** (FSF's classic stance) and keeps our binary MIT/Apache-2.0. Cost: requires a JVM on the engineer/wizard host for Signal customers — acceptable per-customer trade. |
## What NOT to Use
| Avoid | Why | Use Instead |
|---|---|---|
| `presage`, `libsignal-service`, `libsignal-protocol`, `libsignal` | **AGPL-3.0**. Statically linking into osAgent contaminates the whole binary, which breaks our MIT/Apache-2.0 dual license and forces us to publish full source to every customer we ship to. | `signal-cli` (GPLv3) as a separate process; talk JSON-RPC over Unix socket. License boundary at the process edge. |
| `frankenstein` (Telegram) | **WTFPL** — informal license, gets flagged by `cargo deny` default policies and by enterprise SBOM scanners. | `teloxide` 0.17 (MIT). |
| `aws-lc-rs` as rustls crypto provider | zeroclaw v0.7.5 explicitly switched away because of `.eh_frame` strip issues at build time. Preserve their pin. | `ring` (already wired in upstream). |
| `amqprs` 2.1.5 | TLS wired through URI — cannot inject custom `ServerName` for our 127.0.0.1-dial / shield.internal-SAN cert pattern without an upstream patch. Otherwise a fine crate. | `lapin` 4.10 with manual `tokio-rustls` stream construction. |
| sandbox auto-detect chain (Auto→Landlock→Firejail→Docker→Noop) from upstream | Bit us on 2026-04-22 — Docker wrap broke engineer's bridge access. PROJECT.md constraint: ship `none` only; config-load rejects `sandbox.enabled != false`. | Hard-code single `none` backend; gate any other behind explicit build feature `experimental-sandbox` (not in default builds). |
| Extism (WASM plugins from upstream) | Strip per PROJECT.md decision (`zeroclaw-plugins` dropped). Extism is a great crate; we just don't need plugin loading and don't want the attack surface. | Drop the whole crate; remove from workspace members. |
| Qdrant / Postgres memory backends from upstream | Strip per PROJECT.md — sqlite-only memory. | `rusqlite` with sqlcipher feature. |
| `git2` (libgit2 Rust bindings) for signed-commit verification | Heavyweight, brings libgit2 as a system dep, and we only need to verify a signature blob — not do full git operations. | `ssh-key::SshSig` reads the raw signature payload directly. Engineer can shell out to `git verify-commit` if it ever needs full git semantics, but verify-from-Rust is preferred for skill catalog integrity (no shell tool in the loop). |
| `hashicorp_vault` crate | Older, mixed sync/async API, less active. | `vaultrs`. |
| Embedded Postgres + Diesel | Out of scope per PROJECT.md. SQLite only. | `rusqlite` + `tokio-rusqlite`. |
| Outbound HTTP webhook subscriptions (zeroclaw upstream feature) | Strip per STRIP-05. Generic outbound webhooks are an exfil channel. | Drop the module entirely; do not provide a config knob. |
## Stack Patterns by Variant
- Engineer/wizard talk only to `ola-management-oracle` (Ollama-compatible local LLM proxy).
- All `reqwest::Client` instances must be constructed with an explicit **deny-listed-domains** middleware layer — if any code path emits a request to `*.anthropic.com`, `*.openai.com`, `*.googleapis.com`, etc., the call returns `Err` and an alert is emitted to the audit log + channel.
- No silent failover — per Constraints in PROJECT.md, `local-only` means "no cloud traffic, period." When oracle is unreachable, refuse to serve.
- Channel implication: WhatsApp-Cloud and Telegram both hit `*.telegram.org` and `*.whatsapp.com` (cloud APIs) — these channels are **incompatible with `local-only`**; config-load must reject the combination with a clear error. Mattermost (self-hosted), Matrix (self-hosted homeserver possible), and signal-cli (peer-to-peer over Signal servers — still cloud, but the metadata story differs) need per-customer review.
- Same deny-list infrastructure as above, but in **warn-and-fallback** mode rather than refuse.
- No restriction. Standard reqwest clients.
- MCP modules compile in (decision #1 — MCP only on engineer).
- All five Vault-related modules are **excluded** (`#[cfg(feature = "wizard-bin")]` on their `mod` declarations).
- Subagent invoke is enabled (engineer can spawn subagents per #14, #18).
- MCP modules **must not** compile in. The `mod mcp;` declaration is `#[cfg(feature = "engineer-bin")]`-gated. Confirmed by `nm` CI gate.
- Vault writer modules compile in with 2-person ack hard-coded (decision #5), customer-path assertion (decision #6), idempotency wrapper (decision #8).
- Subagent invoke is disabled (wizard does not spawn subagents in M3 per PROJECT.md scope; revisit M4).
- Separate `[[bin]]` in workspace with **no LLM crates, no channels, no MCP**. Direct AMQP via lapin only. Reuses the AMQP and audit crates only.
## Version Compatibility Matrix
| Package A | Compatible With | Notes |
|---|---|---|
| `lapin` 4.10 | `tokio-rustls` 0.26 | Manual stream construction; pass result into `Connection::connector(uri, |connect_opts| async move { ... })`. Lapin does not import rustls directly — you bring your own stream type implementing `AsyncRead + AsyncWrite + Unpin + Send + 'static`. |
| `rustls` 0.23 | `tokio-rustls` 0.26 | These versions must move in lockstep; mismatched majors will not compile. |
| `rustls` 0.23 | `rustls-pemfile` 2.2 | Both depend on `rustls-pki-types` 1.x — keep that in `[workspace.dependencies]` to avoid duplicate crate compilation. |
| `rusqlite` 0.40 | `tokio-rusqlite` 0.7 | `tokio-rusqlite` 0.7 depends on `rusqlite ^0.37` per its manifest — verify whether 0.40 is semver-compatible with that range. If not, either pin `rusqlite` to 0.37 (which still has the sqlcipher features) or use a newer `tokio-rusqlite` if one releases. **Action item for M1 build-out: confirm exact compatible pairing on first `cargo build`.** |
| `matrix-sdk` 0.18 | `tokio` 1.48+ | matrix-sdk has its own internal rustls + sqlite — ensure feature flags (`rustls-tls`, `sqlite`) are set and `default-features = false` to avoid pulling native-tls. |
| `teloxide` 0.17 | `axum` 0.8 | `webhooks-axum` feature targets axum 0.8 in the 0.17 line. Verify on first build (axum 0.7 → 0.8 was a breaking change). |
| `slack-morphism` 2.22 | `axum` 0.8 | Verify with `--features axum` whether 2.22 has caught up to axum 0.8 or is still on 0.7. If still on 0.7, wrap slack-morphism's hyper-based router separately rather than sharing the axum router — small operational overhead. |
| `ed25519-dalek` 2.2 | `rand_core` 0.6 | ed25519-dalek 2.x exposes `rand_core` 0.6 in its public API. If we add `rand` to the workspace, pick a `rand` version that uses `rand_core` 0.6. |
| `vaultrs` 0.8 | `reqwest` 0.12 vs 0.13 | vaultrs 0.8 was released 2026-03-17 — verify whether it uses reqwest 0.12 (probable) or has caught up to 0.13. Mismatch causes a second reqwest compilation and 8 MB extra binary size, not a correctness issue. **Acceptable risk; flag for M2.** |
## Critical Integration Risks (flag for roadmap)
### Risk 1: AMQP mTLS with `ServerName` ≠ dial address — HIGH risk, MEDIUM complexity
### Risk 2: `tokio-rusqlite` 0.7 vs `rusqlite` 0.40 version compatibility — LOW risk, LOW complexity, immediate verification
### Risk 3: AGPL contamination from any Signal Rust SDK — HIGH risk, AVOID-AT-DESIGN-TIME
- Add `cargo deny` ban entries for `presage`, `libsignal-service`, `libsignal-protocol`, `libsignal-client`, `libsignal-bridge`.
- Implement Signal channel via `signal-cli` daemon-mode JSON-RPC over Unix socket. License boundary is the process edge (FSF mere-aggregation).
- Document this choice in `documentation/osAgent/adr/0006-signal-via-signal-cli-process-bridge.md`.
### Risk 4: rustls provider pinning — MEDIUM risk
### Risk 5: `matrix-sdk` SQLite duplication with our app SQLite — LOW risk, design decision
## Sources
- [lapin docs.rs](https://docs.rs/lapin/latest/lapin/) — confirmed 4.10.0, rustls default, tokio default, `Connection::connect_with_stream`/`connector` API
- [amqprs docs.rs](https://docs.rs/amqprs/latest/amqprs/) — confirmed 2.1.5, TLS via OpenConnectionArguments URI (the limiting factor for our use case)
- [rusqlite docs.rs](https://docs.rs/rusqlite/latest/rusqlite/) — confirmed 0.40.1
- [tokio-rusqlite docs.rs](https://docs.rs/tokio-rusqlite/latest/tokio_rusqlite/) — confirmed 0.7.0, depends on `rusqlite ^0.37` (Risk 2)
- [vaultrs docs.rs](https://docs.rs/vaultrs/latest/vaultrs/) — confirmed 0.8.0 (2026-03-17), MIT, KV v2 + AppRole, async via reqwest
- [gray_matter docs.rs](https://docs.rs/gray_matter/latest/gray_matter/) — confirmed 0.3.2, MIT, YAML/TOML/JSON frontmatter
- [ssh-key docs.rs](https://docs.rs/ssh-key/latest/ssh_key/) — confirmed 0.6.7, Apache-2.0/MIT, `SshSig` for `ssh-keygen -Y sign/verify` format which matches `git -c gpg.format=ssh`
- [ed25519-dalek docs.rs](https://docs.rs/ed25519-dalek/latest/ed25519_dalek/) — confirmed 2.2.0, BSD-3-Clause
- [teloxide docs.rs](https://docs.rs/teloxide/latest/teloxide/) — confirmed 0.17.0, MIT, `webhooks-axum` feature
- [slack-morphism docs.rs](https://docs.rs/slack-morphism/latest/slack_morphism/) — confirmed 2.22.0, Apache-2.0, `SlackEventSignatureVerifier` built-in
- [matrix-sdk docs.rs](https://docs.rs/matrix-sdk/latest/matrix_sdk/) — confirmed 0.18.0, Apache-2.0, tokio 1.48+
- [whatsapp-cloud-api docs.rs](https://docs.rs/whatsapp-cloud-api/) — confirmed 0.5.4, MIT/Apache-2.0, 0% documented (justifies in-house wrapper)
- [signal-cli GitHub](https://github.com/AsamK/signal-cli) — confirmed GPLv3, JSON-RPC daemon mode supported
- [libsignal GitHub](https://github.com/signalapp/libsignal) — confirmed AGPL-3.0, "use outside of Signal is unsupported" — explicit avoidance signal
- [presage GitHub](https://github.com/whisperfish/presage) — confirmed AGPL-3.0 (v0.7.0 2025-05-14)
- [rustls docs.rs](https://docs.rs/rustls/latest/rustls/) — confirmed 0.23.40, ServerName override pattern at connection time (not in ClientConfig)
- [rustls issue #1310: SNI with IP](https://github.com/rustls/rustls/issues/1310) — confirms the InvalidServerName behavior and the workaround pattern
- [axum docs.rs](https://docs.rs/axum/latest/axum/) — confirmed 0.8.9, WS feature
- [tokio docs.rs](https://docs.rs/tokio/latest/tokio/) — confirmed 1.52.3
- [tokio-util CancellationToken docs.rs](https://docs.rs/tokio-util/latest/tokio_util/sync/struct.CancellationToken.html) — confirmed parent/child semantics for decision #18
<!-- GSD:stack-end -->

<!-- GSD:conventions-start source:CONVENTIONS.md -->
## Conventions

Conventions not yet established. Will populate as patterns emerge during development.
<!-- GSD:conventions-end -->

<!-- GSD:architecture-start source:ARCHITECTURE.md -->
## Architecture

Architecture not yet mapped. Follow existing patterns found in the codebase.
<!-- GSD:architecture-end -->

<!-- GSD:workflow-start source:GSD defaults -->
## GSD Workflow Enforcement

Before using Edit, Write, or other file-changing tools, start work through a GSD command so planning artifacts and execution context stay in sync.

Use these entry points:
- `/gsd:quick` for small fixes, doc updates, and ad-hoc tasks
- `/gsd:debug` for investigation and bug fixing
- `/gsd:execute-phase` for planned phase work

Do not make direct repo edits outside a GSD workflow unless the user explicitly asks to bypass it.
<!-- GSD:workflow-end -->



<!-- GSD:profile-start -->
## Developer Profile

> Profile not yet configured. Run `/gsd:profile-user` to generate your developer profile.
> This section is managed by `generate-claude-profile` -- do not edit manually.
<!-- GSD:profile-end -->
