//! The MCP OAuth manager: one per process, shared by every connect path.
//!
//! Canonical state: the encrypted auth-profile store (`super::store`) holds
//! the durable grant; this type adds the live view (current tokens, validated
//! discovery, DCR client id) and the two operations the transport needs:
//! hand out a usable bearer, and react to a 401. Everything else — refresh
//! dedup, background-vs-interactive posture, fail-closed storage — lives
//! here so transports stay thin.
//!
//! Posture recap (design note §12, binding):
//! - background (non-interactive) never binds a callback port and never
//!   launches a browser: a missing grant is an actionable error, and the
//!   transport's fail-open skip keeps the server list alive.
//! - exactly one refresh attempt per caller-visible operation; a rotated
//!   token observed concurrently short-circuits the refresh.
//! - 5xx/429 never clears the stored grant even if the body carried
//!   `invalid_grant`; `invalid_client`/`unauthorized_client` from a
//!   CONFIGURED client id is permanent, not a browser loop.

use anyhow::{Context, Result, bail};
use parking_lot::Mutex;
use reqwest::Url;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use zeroclaw_config::schema::McpServerConfig;

use super::browser::{BrowserLauncher, SystemBrowserLauncher};
use super::discovery::{OAuthDiscovery, discover_oauth};
use super::listener::{CALLBACK_WAIT_TIMEOUT, CallbackFlow, LoopbackListener};
use super::pkce::{Pkce, new_state};
use super::policy::OAuthClientFactory;
use super::refresh::RefreshCoordinator;
use super::registration::register_client;
use super::resource::CanonicalResource;
use super::store::McpTokenStore;
use super::tokens::{TokenErrorKind, TokenSet, bearer_header_value, exchange_code, refresh_tokens};

/// A token set with fewer than this many seconds of life left is refreshed
/// before it is handed to a request, so a request cannot be racing expiry.
const REFRESH_SKEW: Duration = Duration::from_secs(60);

/// Outcome of `clear_spent_grant` for the operator-facing WARN: the
/// kept-cause branches say different things about the stored profile,
/// and the message must not promise a re-sign-in it did not cause nor
/// a newer credential it did not see.
#[derive(Clone, Copy)]
enum SpentRemoval {
    /// The stored profile was the spent one and is gone.
    Removed,
    /// The disk holds a DIFFERENT (re-minted) profile; keep it.
    NewerKept,
    /// Nothing usable on disk (absent or unusable), or the store could
    /// not be inspected; whatever is there was kept.
    KeptUnusableOrUnknown,
}

impl SpentRemoval {
    /// The structured-log token for this outcome.
    fn attr(self) -> &'static str {
        match self {
            SpentRemoval::Removed => "removed",
            SpentRemoval::NewerKept => "newer_kept",
            SpentRemoval::KeptUnusableOrUnknown => "kept_unusable_or_unknown",
        }
    }

    /// The operator-facing half-sentence about the stored profile, shared
    /// by every grant-death site so a new arm cannot pair one branch's
    /// token with another branch's words.
    fn sentence(self) -> &'static str {
        match self {
            SpentRemoval::Removed => "the stored profile was removed",
            SpentRemoval::NewerKept => {
                "a newer stored sign-in was kept and the next connect uses it"
            }
            SpentRemoval::KeptUnusableOrUnknown => {
                "the stored profile was kept (unusable or not inspectable)"
            }
        }
    }
}

/// A RFC 9728 5.2 reshuffle takes the once-working discovery document out of
/// the cache and stages it here so it goes back when the reshuffle produced
/// no replacement. `Drop` is the point: the caller's future can be cancelled
/// at its tool-call deadline (the daemon heartbeat does this routinely), and
/// then the `Err` restore arm never runs, so the working document would stay
/// lost for the process lifetime while the one-shot latch was already spent.
/// The restore is a compare-and-set on an empty slot - a concurrent pass may
/// have installed a NEWER document, and resurrecting ours over it would hide
/// the migration - and gives up while the lock is busy, which is the same
/// degradation the explicit restore path accepts.
struct StagedDiscovery<'a> {
    state: &'a Arc<ServerState>,
    previous: Option<OAuthDiscovery>,
}

impl StagedDiscovery<'_> {
    /// Hand the staged document to the caller's own (logging) restore path,
    /// so `Drop` has nothing left to do.
    fn take(&mut self) -> Option<OAuthDiscovery> {
        self.previous.take()
    }
}

impl Drop for StagedDiscovery<'_> {
    fn drop(&mut self) {
        let Some(previous) = self.previous.take() else {
            return;
        };
        let Ok(mut slot) = self.state.discovery.try_write() else {
            return;
        };
        if slot.is_none() {
            *slot = Some(previous);
        }
    }
}

/// Below this much actual life a grant that CANNOT be refreshed (no
/// refresh token) is treated as spent: anything longer than a handshake
/// is still safe to send, so the 60 s refresh skew must not be read as
/// expiry - it reserves the margin for refreshes that can actually
/// renew (greptile C13: skews are not expirations; a usable token was
/// being deleted and re-signed-in on every handshake message). Both
/// windows are wall-clock judgements (`Utc::now` against the stored
/// expiry), so a large clock step can move either verdict by the step
/// size.
const DEAD_GRANT_MARGIN: Duration = Duration::from_secs(5);

/// Grace past the caller's MCP deadline during which it still joins the
/// single-flight token exchange (the token client carries its own hard
/// request timeout, so the exchange is bounded independently). Beyond
/// wall+grace the caller stops paying: the refresh token may already be
/// redeemed server-side, so the exchange keeps running in its own task —
/// holding the single-flight lock — and commits normally. Abandoning the
/// commit (caller futures drop at any await) would strand a spent token
/// in memory and on disk and force a sign-in a daemon cannot finish
/// unattended (greptile C12).
const REFRESH_DETACH_GRACE: Duration = Duration::from_secs(30);

/// Outcome of joining a detached refresh task: the commit landed, or the
/// caller stopped paying while the task keeps running and will still
/// commit under the single-flight lock.
enum Join<T> {
    Done(std::result::Result<T, tokio::task::JoinError>),
    Detached,
}

impl<T> Join<T> {
    async fn from_handle(handle: tokio::task::JoinHandle<T>, wall: std::time::Instant) -> Self {
        // Join until `wall + REFRESH_DETACH_GRACE`, expressed across the
        // two clocks (std `Instant` for the stored wall, tokio `Instant`
        // for the timer) as an offset from now. `checked_duration_since`
        // matters: an already-elapsed wall must leave only the unspent
        // part of the grace — plain `wall - now` saturates to zero, which
        // here would mean the FULL grace, i.e. a caller whose deadline
        // lapsed long ago still paying another 30 s.
        // `wall - now` saturates to zero for an already-elapsed wall, so
        // the remaining wait is anchored on `wall + grace` itself: a wall
        // that lapsed long ago leaves only the unspent part of the grace.
        let join_deadline = wall + REFRESH_DETACH_GRACE;
        let grace_until = tokio::time::Instant::now()
            + join_deadline
                .checked_duration_since(std::time::Instant::now())
                .unwrap_or(Duration::ZERO);
        tokio::select! {
            biased;
            outcome = handle => Self::Done(outcome),
            _ = tokio::time::sleep_until(grace_until) => Self::Detached,
        }
    }
}

/// The caller-side wall a single MCP tool call runs under. Abstracted so
/// the detachment bound can be exercised in tests without the full
/// client plumbing (production value comes from the `McpServerConfig`
/// timeout knobs, mirroring `mcp_client`'s resolution).
pub trait CallerDeadline: Send + Sync {
    /// The wall a caller joining this deadline should wait until. The
    /// default resolves `now + as_secs()` AT USE TIME: a stored
    /// per-resource deadline must never freeze an absolute instant at
    /// construction, or a long-lived process detaches every join once
    /// that stale wall elapses (and AGENTS.md forbids snapshotting
    /// live policy into long-lived handles). Injected test clocks
    /// override this to sit wherever the test needs them.
    fn as_instant(&self) -> std::time::Instant {
        std::time::Instant::now() + Duration::from_secs(self.as_secs().unwrap_or(0))
    }
    /// The effective timeout in seconds this deadline resolves to,
    /// already clamped like `mcp_client` resolves the per-call budget.
    /// `None` marks an injected clock that does not speak in seconds,
    /// which `state_for_with_clock` then leaves untouched across
    /// config reloads.
    fn as_secs(&self) -> Option<u64> {
        None
    }
}

/// Production deadline: `now + configured timeout`, resolved per use
/// from the same constants `mcp_client` applies to the tool-call
/// timeout it mirrors (single source, no comment-only sync).
struct SystemDeadlineWithTimeout {
    timeout_secs: Option<u64>,
}

impl SystemDeadlineWithTimeout {
    fn new(timeout_secs: Option<u64>) -> Self {
        Self { timeout_secs }
    }

    fn effective_secs(&self) -> u64 {
        self.timeout_secs
            .unwrap_or(crate::mcp_client::DEFAULT_TOOL_TIMEOUT_SECS)
            .min(crate::mcp_client::MAX_TOOL_TIMEOUT_SECS)
    }
}

impl CallerDeadline for SystemDeadlineWithTimeout {
    fn as_secs(&self) -> Option<u64> {
        Some(self.effective_secs())
    }
}
/// A live grant: tokens plus the client identity and the ISSUER that
/// obtained them (S3-4: the plan's credential cache is bound to resource
/// AND issuer — a grant minted by AS "A" must never be redeemed at AS "B").
#[derive(Clone)]
struct Grant {
    tokens: TokenSet,
    client_id: String,
    issuer: Url,
    /// When this process minted the tokens it holds (`Some`, at first
    /// sign-in or a rotation commit); `None` records a copy this process did
    /// not mint - a store load or adoption cannot prove when its tokens were
    /// issued. Consulted only by `proactive_refresh_due`.
    minted_at: Option<std::time::Instant>,
}

impl Grant {
    /// Whether proactive refresh is due for this grant. A fixed skew is only
    /// sensible against lifetimes far longer than it: an AS issuing 30-second
    /// tokens would otherwise see every bearer lookup - prepare, every
    /// handshake request - redeem a fresh exchange plus a whole-file
    /// credential write, because the just-rotated token already sits inside
    /// the skew. A grant this process minted within the skew window is
    /// therefore used until it actually expires, then this behaves exactly
    /// like the plain skew check; grants it did not mint, and grants older
    /// than the window, are unchanged.
    fn proactive_refresh_due(&self, skew: Duration) -> bool {
        let fresh = self
            .minted_at
            .is_some_and(|minted_at| minted_at.elapsed() < skew);
        if fresh {
            return self
                .tokens
                .expires_at
                .is_some_and(|expires_at| expires_at <= chrono::Utc::now());
        }
        self.tokens.is_expiring_within(skew)
    }
}

impl std::fmt::Debug for Grant {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Grant")
            .field("client_id", &self.client_id)
            .field("issuer", &self.issuer.as_str())
            .field("expires_at", &self.tokens.expires_at)
            .field("access_token", &"[redacted]")
            .field(
                "refresh_token",
                &self.tokens.refresh_token.as_ref().map(|_| "[redacted]"),
            )
            .finish()
    }
}

/// Per-canonical-resource live state. Keyed by the canonical RFC 8707
/// resource: two configured servers sharing a resource share tokens, and a
/// server rename (same URL) keeps working while a URL change starts fresh.
/// The per-resource policy snapshot: the CURRENT `McpServerConfig` for the
/// resource and the client factory derived from it. `state_for` refreshes
/// this on every assembly, so an operator edit of `oauth_client_id`,
/// `oauth_scopes` or `tls_ca_cert_path` takes effect on the next connect
/// instead of at next process restart (AGENTS.md: policy is resolved from
/// its source, never snapshotted into long-lived handles).
#[derive(Clone)]
struct ServerPolicy {
    config: McpServerConfig,
    factory: OAuthClientFactory,
}

struct ServerState {
    canonical: CanonicalResource,
    /// C12: the caller-side wall this connect runs under (the MCP tool-call
    /// deadline). Captured from the effective per-server configuration;
    /// consulted only to bound how long the caller keeps waiting on a
    /// token exchange (see `Join::from_handle`). Short-parking mutex: the
    /// guard is never held across an await, and daemon reloads may
    /// re-clamp it when `tool_timeout_secs` changes.
    deadline: Mutex<Arc<dyn CallerDeadline>>,
    policy: std::sync::Mutex<ServerPolicy>,
    discovery: tokio::sync::RwLock<Option<OAuthDiscovery>>,
    grant: tokio::sync::RwLock<Option<Grant>>,
    /// RFC 9728 §5.2: a fresh 401 challenge naming a DIFFERENT metadata
    /// location than the cached discovery used signals the server moved its
    /// metadata; the client SHOULD re-fetch. One-shot per process per
    /// resource: the first such challenge invalidates the cached discovery
    /// exactly once, so a lying or flapping server cannot make discovery
    /// loop on every 401.
    stale_retry_taken: std::sync::atomic::AtomicBool,
}

impl ServerState {
    /// The live policy snapshot. A poisoned mutex only means some caller
    /// panicked while holding it; the snapshot contents remain valid, so
    /// recover via `into_inner` rather than failing the flow.
    fn policy(&self) -> ServerPolicy {
        self.policy
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Poison-tolerant short read of the caller wall (never held across
    /// an await; see the field note).
    fn deadline_wall(&self) -> std::time::Instant {
        self.deadline.lock().as_instant()
    }
}

/// The per-process OAuth manager. Cheap to share (`Arc`); construct once in
/// runtime assembly and hand it to every `connect_all` path.
pub struct McpOAuthManager {
    store: McpTokenStore,
    launcher: Arc<dyn BrowserLauncher>,
    states: Mutex<HashMap<String, Arc<ServerState>>>,
    refresh: RefreshCoordinator,
    /// Parsed `security.nat64_prefixes` of the process config, threaded into
    /// every per-server factory so discovery/token fetches classify NAT64
    /// translations exactly like the rest of the tool surface. Shared with
    /// every factory this manager ever built (`Arc`): the wiring swaps the
    /// CONTENTS in place on every assembly (greptile C9), so a daemon config
    /// reload takes effect for already-connected servers too - policy is
    /// resolved from its source, not snapshotted into long-lived handles
    /// (AGENTS.md).
    nat64_prefixes: Arc<std::sync::Mutex<Vec<zeroclaw_infra::net_guard::Nat64Prefix>>>,
}

impl std::fmt::Debug for McpOAuthManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpOAuthManager")
            .field("tracked_resources", &self.states.lock().len())
            .finish_non_exhaustive()
    }
}

impl McpOAuthManager {
    /// Construct the shared manager from live config: the state dir and
    /// encryption posture come from the same canonical resolvers the
    /// provider auth stack uses (`state_dir_from_config`, `secrets.encrypt`
    ///), so one encrypted store serves every credential in the process.
    pub fn from_config(config: &zeroclaw_config::schema::Config) -> Result<Self> {
        let state_dir = zeroclaw_providers::auth::state_dir_from_config(config);
        // Parse the declared NAT64 translators once, here: a malformed entry
        // fails manager-init (the wiring logs "MCP OAuth manager could not be
        // built" once and every oauth server then fails closed) rather than
        // silently misclassifying later fetches.
        let nat64_prefixes = zeroclaw_infra::net_guard::parse_nat64_prefixes(
            &config.security.nat64_prefixes,
            "security.nat64_prefixes",
        )?;
        Self::with_launcher_and_nat64(
            &state_dir,
            config.secrets.encrypt,
            Arc::new(SystemBrowserLauncher),
            nat64_prefixes,
        )
    }

    /// Construct the shared manager. Fails closed when encryption is off
    /// (via [`McpTokenStore::new`]).
    pub fn new(state_dir: &Path, encrypt_secrets: bool) -> Result<Self> {
        Self::with_launcher(state_dir, encrypt_secrets, Arc::new(SystemBrowserLauncher))
    }

    /// Test/embedder seam: substitute the browser launcher.
    pub fn with_launcher(
        state_dir: &Path,
        encrypt_secrets: bool,
        launcher: Arc<dyn BrowserLauncher>,
    ) -> Result<Self> {
        Self::with_launcher_and_nat64(state_dir, encrypt_secrets, launcher, Vec::new())
    }

    /// Full constructor: launcher seam plus parsed `security.nat64_prefixes`.
    pub fn with_launcher_and_nat64(
        state_dir: &Path,
        encrypt_secrets: bool,
        launcher: Arc<dyn BrowserLauncher>,
        nat64_prefixes: Vec<zeroclaw_infra::net_guard::Nat64Prefix>,
    ) -> Result<Self> {
        Ok(Self {
            store: McpTokenStore::new(state_dir, encrypt_secrets)?,
            launcher,
            states: Mutex::new(HashMap::new()),
            refresh: RefreshCoordinator::new(),
            nat64_prefixes: Arc::new(std::sync::Mutex::new(nat64_prefixes)),
        })
    }

    fn state_for(&self, config: &McpServerConfig) -> Result<Arc<ServerState>> {
        self.state_for_with_clock(
            config,
            Arc::new(SystemDeadlineWithTimeout::new(config.tool_timeout_secs)),
        )
    }

    fn state_for_with_clock(
        &self,
        config: &McpServerConfig,
        deadline: Arc<dyn CallerDeadline>,
    ) -> Result<Arc<ServerState>> {
        let raw_url = config
            .url
            .as_deref()
            .context("OAuth MCP servers must have a url")?;
        let canonical = CanonicalResource::from_configured(
            &Url::parse(raw_url).with_context(|| format!("MCP url {raw_url:?} is unparseable"))?,
        )
        .context("MCP url is not a valid canonical OAuth resource")?;
        let mut states = self.states.lock();
        // C12: an existing state may carry a deadline from a previous
        // configured timeout (daemon reload). Re-clamp only when the
        // effective seconds actually changed; the wall itself is
        // resolved at every use from the stored seconds (see
        // `CallerDeadline::as_instant`), so a reload only decides the
        // join bound, never a frozen instant. An injected clock that
        // does not speak in seconds (tests) is left alone. Two configs
        // canonicalizing to one resource with different timeouts
        // ping-pong the stored budget per assembly; benign, the join
        // bound is call-relative and read once per exchange, so the
        // server that assembled last simply bounds its own call.
        let deadline = states.get(canonical.resource()).map_or_else(
            || Arc::clone(&deadline),
            |state| {
                let mut current = state.deadline.lock();
                let swap = match current.as_secs() {
                    // Seconds-configurable clock (production): swap in the
                    // fresh instance only when the effective budget moved.
                    Some(secs) => (Some(secs) != deadline.as_secs())
                        .then(|| Arc::clone(&deadline) as Arc<dyn CallerDeadline>),
                    // Test/embedder-injected clock: keep whatever it is.
                    None => None,
                };
                if let Some(swap) = swap {
                    *current = swap;
                }
                Arc::clone(&current)
            },
        );
        if let Some(state) = states.get(canonical.resource()) {
            // Refresh the live policy snapshot if the operator changed this
            // server's config since the resource was first seen.
            let mut policy = state
                .policy
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if policy.config != *config {
                *policy = ServerPolicy {
                    config: config.clone(),
                    factory: OAuthClientFactory::new_with_nat64(
                        config,
                        Arc::clone(&self.nat64_prefixes),
                    ),
                };
            }
            return Ok(Arc::clone(state));
        }
        let state = Arc::new(ServerState {
            canonical,
            deadline: Mutex::new(deadline),
            policy: std::sync::Mutex::new(ServerPolicy {
                config: config.clone(),
                factory: OAuthClientFactory::new_with_nat64(
                    config,
                    Arc::clone(&self.nat64_prefixes),
                ),
            }),
            discovery: tokio::sync::RwLock::new(None),
            grant: tokio::sync::RwLock::new(None),
            stale_retry_taken: std::sync::atomic::AtomicBool::new(false),
        });
        states.insert(state.canonical.resource().to_string(), Arc::clone(&state));
        Ok(state)
    }

    /// Idempotent. Reloads the store whenever no live grant is held (so a
    /// sign-in performed by ANOTHER process — the documented operator flow:
    /// interactive CLI signs in once, the daemon keeps running — is picked
    /// up on the next connect without a restart), then at most one discovery
    /// pass per process (a discovery failure is retryable; it is not
    /// cached). With a live grant present the reload costs nothing.
    async fn ready(&self, state: &Arc<ServerState>, www_authenticate: Option<&str>) -> Result<()> {
        if state.grant.read().await.is_none() {
            match self.store.load_authorization(&state.canonical).await {
                Ok(Some(authorization)) => {
                    // Greptile C8: the store load spans await points, so a
                    // concurrent connect may already have committed a
                    // ROTATED token set (live grant is the truth, S3-5)
                    // while this copy was in flight. Re-check under the
                    // write lock: whoever installs first wins, and a stale
                    // store copy must never restore the spent refresh
                    // token (the next refresh would `invalid_grant` and
                    // clear a valid sign-in).
                    let mut grant = state.grant.write().await;
                    if grant.is_none() {
                        *grant = Some(Grant {
                            tokens: authorization.tokens,
                            client_id: authorization.client_id,
                            issuer: authorization.issuer,
                            minted_at: None,
                        });
                    }
                }
                Ok(None) => {}
                Err(err) => {
                    // A broken credential file must not permanently poison
                    // the process: warn and behave as "needs authorization".
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                            .with_attrs(::serde_json::json!({
                                "mcp_oauth": "store-load",
                                "resource": state.canonical.resource(),
                            })),
                        &format!("MCP OAuth store load failed; treating as unauthorized: {err:#}")
                    );
                }
            }
        }

        if state.discovery.read().await.is_none() {
            // Bounded: `prepare_auth` runs without a transport request
            // timeout on purpose (sign-in may take minutes), and connects
            // are sequential, so a slow or lying metadata host must not
            // stall every later server behind it.
            let discovered = tokio::time::timeout(
                super::discovery::DISCOVERY_BUDGET,
                discover_oauth(&state.policy().factory, &state.canonical, www_authenticate),
            )
            .await
            .map_err(|_| {
                anyhow::Error::msg(format!(
                    "MCP OAuth metadata discovery for {} exceeded the {} s budget (no server \
                     may block the sequential connect queue; check the server's well-known \
                     endpoints and 401 challenge)",
                    state.canonical.resource(),
                    super::discovery::DISCOVERY_BUDGET.as_secs()
                ))
            })??;
            *state.discovery.write().await = Some(discovered);
        }

        // S3-4: bind the loaded grant to the discovered issuer before any
        // token leaves this process. If the protected-resource metadata now
        // names a different authorization server, the stored refresh token
        // belongs to the OLD party; redeeming it at the new token endpoint
        // is credential disclosure. The LIVE grant is always dropped, and
        // the stored profile is retired by ownership under the same rule the
        // spent-token verdicts use (greptile C14/P1): the replacement AS may
        // already have a sign-in on disk, written by the documented operator
        // flow (interactive CLI while the daemon runs), and that sign-in is
        // neither spent nor ours to erase.
        let discovery = state
            .discovery
            .read()
            .await
            .clone()
            .context("OAuth discovery vanished unexpectedly")?;
        let stale_grant = {
            let grant = state.grant.read().await;
            grant
                .as_ref()
                .filter(|grant| {
                    !super::metadata::issuer_matches(&grant.issuer, &discovery.auth_server.issuer)
                })
                .cloned()
        };
        if let Some(stale) = stale_grant {
            let removal =
                McpOAuthManager::clear_spent_grant(&self.store, state, &stale.tokens.access_token)
                    .await;
            let adopted = if matches!(removal, SpentRemoval::NewerKept) {
                self.adopt_newer_sign_in(state, &discovery.auth_server.issuer)
                    .await
            } else {
                false
            };
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(if adopted {
                        ::zeroclaw_log::EventOutcome::Success
                    } else {
                        ::zeroclaw_log::EventOutcome::Failure
                    })
                    .with_attrs(::serde_json::json!({
                        "mcp_oauth": "issuer-drift",
                        "resource": state.canonical.resource(),
                        "stored_issuer": stale.issuer.as_str(),
                        "discovered_issuer": discovery.auth_server.issuer.as_str(),
                        "stored_removal": removal.attr(),
                        "adopted_newer_sign_in": adopted,
                    })),
                &format!(
                    "MCP OAuth grant was minted by a different authorization server than \
                     discovery now names; {}",
                    if adopted {
                        "a newer sign-in for the new server was found on disk and adopted"
                    } else {
                        "the live grant is dropped and a fresh sign-in is required"
                    }
                )
            );
        }
        Ok(())
    }

    /// Hand out a usable `Authorization` header value for `config`.
    /// `www_authenticate` is the captured `WWW-Authenticate` header from a
    /// discovery-triggering 401, if any (it can name the PRM document URL).
    /// `interactive` decides whether a missing grant may run the browser
    /// flow or must fail actionably.
    /// The current NAT64 prefix rules (short lock; never held across await).
    fn nat64_prefixes(&self) -> Vec<zeroclaw_infra::net_guard::Nat64Prefix> {
        self.nat64_prefixes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Replace the CONTENTS of the shared NAT64 rule vector from the CURRENT
    /// config (wiring calls this on every assembly after a reload). The
    /// `Arc` is never rebound: factories built before the reload observe the
    /// new rules at their next `client_for`. A malformed reload must not
    /// drop the last-known-good guard: `from_config` fails closed at build
    /// time, and the wiring only calls this with successfully parsed rules.
    pub fn refresh_nat64_prefixes(&self, prefixes: Vec<zeroclaw_infra::net_guard::Nat64Prefix>) {
        *self
            .nat64_prefixes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = prefixes;
    }

    /// Parsed rules for the current config, for the wiring's cheap
    /// no-change fast path.
    pub fn current_nat64_prefixes(&self) -> Vec<zeroclaw_infra::net_guard::Nat64Prefix> {
        self.nat64_prefixes()
    }

    /// Test seam (greptile C9): what an ALREADY-BUILT factory of `config`
    /// enforces right now - reads through the live per-server policy lock,
    /// exactly what `client_for` will see on the next fetch.
    #[cfg(test)]
    pub(crate) fn factory_nat64_prefixes(
        &self,
        config: &McpServerConfig,
    ) -> Vec<zeroclaw_infra::net_guard::Nat64Prefix> {
        let canonical = config
            .url
            .as_deref()
            .and_then(|raw| Url::parse(raw).ok())
            .and_then(|url| CanonicalResource::from_configured(&url).ok())
            .expect("canonical resource");
        let states = self.states.lock();
        let state = states
            .get(canonical.resource())
            .expect("server state must exist for a connected server");
        let policy = state
            .policy
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        policy.factory.nat64_prefixes_snapshot()
    }

    #[cfg(test)]
    pub(crate) async fn seed_authorization(
        &self,
        canonical: &CanonicalResource,
        client_id: &str,
        issuer: &Url,
        tokens: zeroclaw_providers::auth::profiles::TokenSet,
    ) -> anyhow::Result<()> {
        self.store
            .save_authorization(canonical, client_id, issuer, tokens)
            .await
    }

    pub async fn bearer_for(
        &self,
        config: &McpServerConfig,
        www_authenticate: Option<&str>,
        interactive: bool,
    ) -> Result<String> {
        let state = self.state_for(config)?;
        self.ready(&state, www_authenticate).await?;

        // Proactive expiry refresh, deduplicated per resource.
        if let Some(token) = self.refresh_if_expiring(&state).await? {
            return Ok(bearer_header_value(&token));
        }

        // No live grant: run the FIRST authorization under the same
        // per-resource single-flight the refreshes use, so N concurrent
        // first connects share one browser sign-in, one loopback port, and
        // one DCR registration instead of each opening their own. No §12
        // nesting: `authorize` never re-enters `run_exclusive`.
        self.refresh
            .run_exclusive(state.canonical.resource(), async {
                // Re-check inside the section: the predecessor may have
                // just completed the very sign-in we came to run.
                if let Some(grant) = state.grant.read().await.as_ref() {
                    return Ok(bearer_header_value(&grant.tokens.access_token));
                }
                self.authorize(&state, interactive).await
            })
            .await
    }

    /// React to a 401 the transport just observed. `used_token` is the
    /// bearer the failed request carried. Returns the bearer to RETRY with
    /// (safe methods only — the caller enforces the method allowlist).
    ///
    /// Concurrency: if a concurrent caller already rotated the token, this
    /// returns the fresh one without touching the token endpoint (the
    /// request that 401'd had the stale copy).
    pub async fn on_unauthorized(
        &self,
        config: &McpServerConfig,
        www_authenticate: Option<&str>,
        used_token: &str,
        interactive: bool,
    ) -> Result<String> {
        let state = self.state_for(config)?;
        // RFC 9728 §5.2 single-shot invalidation: a 401 challenge whose
        // (policy-usable) resource_metadata differs from the URL the
        // cached discovery actually used means the server moved its
        // metadata. Drop the cache once per resource so the `ready` below
        // re-runs discovery with this challenge as candidate #1; every
        // later differing challenge is treated as noise (the transport
        // already proved auth is required, so no probe is needed). The
        // old document is STAGED, not destroyed: if the re-run fails — a
        // reverse proxy advertising its own `resource_metadata` that
        // violates the contract aborts discovery — the re-run returns
        // the error and the next call would otherwise find the once-
        // working document gone for the process lifetime (reviewer
        // adversarial review).
        let mut staged_invalidation: Option<StagedDiscovery<'_>> = None;
        if let Some(raw) = www_authenticate {
            let (hint, _) = super::discovery::usable_challenge_hint(raw);
            if let Some(hint) = hint {
                let differs = state
                    .discovery
                    .read()
                    .await
                    .as_ref()
                    .is_some_and(|d| d.protected_resource_url != hint);
                if differs
                    && state
                        .stale_retry_taken
                        .compare_exchange(
                            false,
                            true,
                            std::sync::atomic::Ordering::AcqRel,
                            std::sync::atomic::Ordering::Acquire,
                        )
                        .is_ok()
                    && let Some(previous) = state.discovery.write().await.take()
                {
                    staged_invalidation = Some(StagedDiscovery {
                        state: &state,
                        previous: Some(previous),
                    });
                }
            }
        }
        if let Err(err) = self.ready(&state, www_authenticate).await {
            match staged_invalidation {
                // A §5.2 re-run that FAILED had first destroyed the
                // once-working document. Restore it and keep this call
                // moving: the cache is populated again, so the section
                // below refreshes on the proven document while the
                // operator sees one actionable WARN about the advertised
                // location. Restore is compare-and-set: a concurrent
                // pass may already have installed a NEWER document, and
                // resurrecting ours over it would hide the migration
                // until a restart (adversarial review N2). A first-ever
                // failure staged nothing: the error keeps propagating.
                Some(mut staged) => {
                    let Some(previous) = staged.take() else {
                        // Nothing left staged to restore: propagate exactly as
                        // the `None` arm does.
                        return Err(err);
                    };
                    let restored = {
                        let mut slot = state.discovery.write().await;
                        if slot.is_some() {
                            false
                        } else {
                            *slot = Some(previous);
                            true
                        }
                    };
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                            .with_attrs(::serde_json::json!({
                                "mcp_oauth": "stale-reshuffle",
                                "resource": state.canonical.resource(),
                                "restored": restored,
                            })),
                        &format!(
                            "RFC 9728 5.2 reshuffle for {} failed; {} and retrying on it: \
                             {err:#}",
                            state.canonical.resource(),
                            if restored {
                                "keeping the previously cached metadata"
                            } else {
                                "a concurrent re-discovery already installed a document"
                            }
                        )
                    );
                }
                None => return Err(err),
            }
        }
        // Callers carry the Authorization header VALUE; accept both the
        // bare token and the `Bearer <token>` form so a format difference
        // can never masquerade as "someone else already rotated".
        let used_token = used_token
            .strip_prefix("Bearer ")
            .or_else(|| used_token.strip_prefix("bearer "))
            .unwrap_or(used_token);

        {
            let grant = state.grant.read().await;
            if let Some(grant) = grant.as_ref()
                && grant.tokens.access_token != used_token
            {
                // A concurrent caller already rotated the token.
                return Ok(bearer_header_value(&grant.tokens.access_token));
            }
        }

        // The section's arms spend this token by verdict, so they carry
        // an owned copy of it for the compare-then-clear removal policy
        // (the outer `used_token` is only a borrow for the pre-section
        // rotate check above).
        let used_token = used_token.to_string();
        enum Forced {
            Resolved(Result<String>),
            // Boxed: `OAuthDiscovery` dwarfs the resolved-result variant;
            // the exchange arm is taken once per refresh, not in a hot
            // loop, so one allocation is the right trade (clippy
            // large_enum_variant).
            Exchange {
                refresh_token: String,
                client_id: String,
                discovery: Box<OAuthDiscovery>,
            },
        }
        // Outcome of the deferred token exchange itself (see the C12 note
        // at the handoff point below).
        enum Exchange {
            Bearer(String),
            Failed(anyhow::Error),
            NeedsSignin(anyhow::Error),
        }

        let decided = self
            .refresh
            .run_exclusive(state.canonical.resource(), async {
                // Re-check inside the section: the predecessor may have
                // rotated or cleared exactly what we came to fix. Decide on
                // one atomic read, then DROP the guard before any await
                // that can take the grant write lock: `authorize` ends in
                // `grant.write()`, and tokio's RwLock is not upgradable, so
                // awaiting it while this task still holds the read guard is
                // a self-deadlock (and writer-preferring fairness blocks
                // every other reader behind it).
                enum Next {
                    Rotated(String),
                    Signin,
                    Refresh(Option<String>, String, Option<Box<OAuthDiscovery>>),
                }
                let next = {
                    let grant = state.grant.read().await;
                    match grant.as_ref() {
                        Some(grant) if grant.tokens.access_token != used_token => {
                            Next::Rotated(bearer_header_value(&grant.tokens.access_token))
                        }
                        Some(grant) => {
                            // C7: the discovery document is captured under
                            // the SAME grant guard, and the grant's issuer
                            // is checked against it before the refresh
                            // token leaves this process. `ready` runs
                            // OUTSIDE the single-flight section and can
                            // replace the discovery (401 challenge from a
                            // second agent, or the §5.2 invalidation) at
                            // any moment; reading it after the guard
                            // dropped would let a concurrent issuer-B
                            // install slip between the two reads and redeem
                            // issuer-A's refresh token at issuer-B's
                            // endpoint: credential disclosure.
                            let captured = Next::Refresh(
                                grant.tokens.refresh_token.clone().filter(|t| !t.is_empty()),
                                grant.client_id.clone(),
                                state.discovery.read().await.clone().map(Box::new),
                            );
                            match captured {
                                Next::Refresh(_t, _c, Some(discovery))
                                    if !super::metadata::issuer_matches(
                                        &grant.issuer,
                                        &discovery.auth_server.issuer,
                                    ) =>
                                {
                                    // The grant and the discovery now name
                                    // different authorization servers:
                                    // treat exactly like the cleared-grant
                                    // case (the drift is logged by `ready`
                                    // on the next connect).
                                    Next::Signin
                                }
                                other => other,
                            }
                        }
                        None => {
                            // Either `ready` cleared the grant for an issuer
                            // change or a concurrent 401 path did: treat as
                            // "needs sign-in", exactly as the pre-fix
                            // in-section re-check intended.
                            Next::Signin
                        }
                    }
                };
                match next {
                    Next::Rotated(bearer) => Forced::Resolved(Ok(bearer)),
                    Next::Signin => Forced::Resolved(self.authorize(&state, interactive).await),
                    Next::Refresh(refresh_token, client_id, discovery) => {
                        let discovery =
                            match discovery.context("OAuth discovery vanished unexpectedly") {
                                Ok(discovery) => discovery,
                                Err(err) => return Forced::Resolved(Err(err)),
                            };
                        let Some(refresh_token) = refresh_token else {
                            // No refresh token at all: the grant is spent
                            // (the 401 that brought us here proves the
                            // caller's token, `used_token`, is the spent
                            // one); redo the authorization (or fail
                            // actionably in background mode) — under the
                            // lock, as before.
                            return Forced::Resolved(
                                self.reauthorize_after_grant_death(
                                    &state,
                                    interactive,
                                    &used_token,
                                )
                                .await,
                            );
                        };
                        Forced::Exchange {
                            refresh_token,
                            client_id,
                            discovery,
                        }
                    }
                }
            })
            .await;

        let (refresh_token, client_id, discovery) = match decided {
            Forced::Resolved(result) => return result,
            Forced::Exchange {
                refresh_token,
                client_id,
                discovery,
            } => (refresh_token, client_id, discovery),
        };

        // C12: the token exchange may outlive this call's own deadline. It
        // runs on an owned task that takes the single-flight lock itself,
        // keeps running past the join wall, and commits normally: the
        // refresh token may already be redeemed server-side (one-time-use
        // rotation is common), so dropping the commit together with the
        // caller's future would strand a spent token in memory and on
        // disk and force a sign-in a daemon cannot finish unattended.
        // Releasing the section here lets the owned task re-acquire the
        // lock without racing: any caller entering meanwhile waits on it
        // and adopts this exchange through the same re-check.
        let exchange_deadline = state.deadline_wall();
        let server_name = state.policy().config.name.clone();
        let resource = state.canonical.resource().to_string();
        let store = self.store.clone();
        let refresh = self.refresh.clone();
        let state_for_exchange = Arc::clone(&state);
        let token_client = state
            .policy()
            .factory
            .client_for(&discovery.auth_server.token_endpoint)
            .await?;
        let handle = zeroclaw_spawn::spawn!(refresh.run_exclusive(&resource, async move {
                let state = state_for_exchange;
                // Re-verify under the lock: a predecessor inside the
                // handoff window may have rotated, cleared, or
                // re-discovered under another issuer since the decision;
                // adopt or defer rather than redeem a token that is no
                // longer ours to spend.
                match state.grant.read().await.as_ref() {
                    Some(grant) if grant.tokens.access_token != used_token => {
                        return Exchange::Bearer(bearer_header_value(
                            &grant.tokens.access_token,
                        ));
                    }
                    Some(grant)
                        if grant.tokens.refresh_token.as_deref()
                            != Some(refresh_token.as_str())
                            || !super::metadata::issuer_matches(
                                &grant.issuer,
                                &discovery.auth_server.issuer,
                            ) =>
                    {
                        let message = format!(
                            "MCP OAuth refresh token for {} changed or its issuer drifted \
                             before the deferred refresh ran; the stored sign-in needs \
                             reconnecting from an interactive ZeroClaw",
                            state.canonical.resource()
                        );
                        ::zeroclaw_log::record!(
                            WARN,
                            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                                .with_attrs(::serde_json::json!({
                                    "mcp_oauth": "refresh-drift",
                                    "resource": state.canonical.resource(),
                                })),
                            &message
                        );
                        return Exchange::NeedsSignin(anyhow::Error::msg(message));
                    }
                    None => {
                        let message = "MCP OAuth grant was cleared before the deferred \
                             refresh ran; the sign-in needs reconnecting from an interactive ZeroClaw";
                        ::zeroclaw_log::record!(
                            WARN,
                            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                                .with_attrs(::serde_json::json!({
                                    "mcp_oauth": "grant-cleared",
                                    "resource": state.canonical.resource(),
                                })),
                            message
                        );
                        return Exchange::NeedsSignin(anyhow::Error::msg(message));
                    }
                    Some(_) => {}
                }
                match refresh_tokens(
                    &token_client,
                    &discovery.auth_server,
                    &state.canonical,
                    &client_id,
                    &refresh_token,
                )
                .await
                {
                    Ok((tokens, _)) => {
                        // ASes that rotate the refresh token make the old
                        // one dead; `commit_refreshed_tokens` installs the
                        // live grant before persisting so a lost disk write
                        // never strands the process on a dead token.
                        // `used_token` is the bearer the transport carried
                        // when the 401 arrived, so it names the grant this
                        // exchange refreshed.
                        let access = McpOAuthManager::commit_refreshed_tokens(
                            &store,
                            &state,
                            tokens,
                            client_id.clone(),
                            discovery.auth_server.issuer.clone(),
                            &used_token,
                        )
                        .await;
                        Exchange::Bearer(bearer_header_value(&access))
                    }
                    Err(err) => {
                        let kind = err
                            .downcast_ref::<super::tokens::TokenEndpointError>()
                            .map(|e| (e.kind, e.status));
                        match kind {
                            Some((kind, status)) if status < 500 && status != 429 => match kind {
                                TokenErrorKind::ReauthorizationRequired => {
                                    // Design §12 configured-id downgrade:
                                    // when the client the token endpoint
                                    // just rejected IS the operator's
                                    // configured `oauth_client_id`, a new
                                    // sign-in would reproduce the same
                                    // rejection (revoked, purged, or a typo
                                    // in config) — so this is a caller-side
                                    // Permanent: keep the grant intact and
                                    // send the operator to the config,
                                    // never to a browser on every call.
                                    let code: Option<String> = err
                                        .downcast_ref::<super::tokens::TokenEndpointError>()
                                        .and_then(|e| e.code.clone());
                                    if matches!(
                                        code.as_deref(),
                                        Some("invalid_client") | Some("unauthorized_client")
                                    ) && state.policy().config.oauth_client_id.as_deref()
                                        == Some(client_id.as_str())
                                    {
                                        return Exchange::Failed(err.context(format!(
                                            "MCP server `{}`: the authorization server rejected the configured oauth_client_id `{client_id}` with `{}`; update the server configuration, a new sign-in cannot fix it",
                                            state.policy().config.name,
                                            code.as_deref().unwrap_or_default()
                                        )));
                                    }
                                    // Grant death: clear it from state
                                    // here, and from the store only while
                                    // the stored profile still matches the
                                    // caller's token the AS just rejected
                                    // — `used_token` is that identity, and
                                    // an interactive process may have
                                    // re-minted the profile since
                                    // (shared spent-verdict policy). The
                                    // deferred task owns the commit — then
                                    // let the caller decide: an
                                    // interactive caller re-authorizes,
                                    // everyone else gets the actionable
                                    // error.
                                    McpOAuthManager::clear_spent_grant(
                                        &store,
                                        &state,
                                        &used_token,
                                    )
                                    .await;
                                    if interactive {
                                        Exchange::NeedsSignin(err)
                                    } else {
                                        Exchange::Failed(err.context(format!(
                                            "MCP server `{}`: the stored OAuth sign-in was rejected by the authorization server; reconnect from an interactive ZeroClaw to sign in again",
                                            state.policy().config.name
                                        )))
                                    }
                                }
                                TokenErrorKind::Permanent => {
                                    let message = format!(
                                        "MCP OAuth token refresh for {} failed permanently: {err:#}",
                                        state.canonical.resource()
                                    );
                                    ::zeroclaw_log::record!(
                                        WARN,
                                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                                            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                                            .with_attrs(::serde_json::json!({
                                                "mcp_oauth": "refresh-permanent",
                                                "resource": state.canonical.resource(),
                                            })),
                                        &message
                                    );
                                    Exchange::Failed(anyhow::Error::msg(message))
                                }
                                TokenErrorKind::Transient => Exchange::Failed(err),
                            },
                            // Design §12: transient server states never
                            // clear the grant — the 5xx/429-with-
                            // invalid_grant shape keeps the grant standing
                            // and surfaces the structured error as-is.
                            _ => Exchange::Failed(err),
                        }
                    }
                }
        }));

        match Join::from_handle(handle, exchange_deadline).await {
            Join::Done(Ok(Exchange::Bearer(bearer))) => Ok(bearer),
            Join::Done(Ok(Exchange::Failed(err))) => Err(err),
            Join::Done(Ok(Exchange::NeedsSignin(err))) => {
                // The deferred exchange found the grant dead or drifted.
                // Route the re-sign-in back through the same per-resource
                // single-flight the first sign-in uses (adversarial review:
                // running `authorize` unlocked here let two concurrent
                // grant-dead repairs open two browsers and race two
                // DCRs); the background posture bails actionably under
                // the lock in microseconds, so both arms serialize
                // consistently. The in-section re-check adopts a
                // predecessor's fresh grant instead of re-signing.
                let retry = self
                    .refresh
                    .run_exclusive(state.canonical.resource(), async {
                        let grant = state.grant.read().await;
                        if grant
                            .as_ref()
                            .is_some_and(|grant| !grant.proactive_refresh_due(REFRESH_SKEW))
                        {
                            return Ok(bearer_header_value(
                                &grant.as_ref().expect("checked live").tokens.access_token,
                            ));
                        }
                        drop(grant);
                        if !interactive {
                            return Err(err);
                        }
                        let bearer = self.authorize(&state, true).await?;
                        Ok(bearer)
                    })
                    .await;
                match retry {
                    Ok(bearer) => Ok(bearer),
                    Err(err) if interactive => Err(err.context(
                        "the stored OAuth sign-in could not be renewed; reconnect from an interactive ZeroClaw to sign in again",
                    )),
                    Err(err) => Err(err),
                }
            }
            Join::Done(Err(join_err)) => {
                let message = format!(
                    "MCP OAuth deferred token refresh for {} did not complete: {join_err}",
                    state.canonical.resource()
                );
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({
                            "mcp_oauth": "refresh-task-failed",
                            "resource": state.canonical.resource(),
                        })),
                    &message
                );
                Err(anyhow::Error::msg(message))
            }
            // The exchange is still running and will commit under the
            // single-flight lock; tell the caller to retry rather than
            // spending another token now.
            Join::Detached => Err(anyhow::Error::msg(format!(
                "MCP server `{server_name}`: the sign-in refresh outlived this call's deadline and keeps running in the background; retry the call - it reuses the refreshed sign-in once it lands, or reports the sign-in that must be started from an interactive ZeroClaw if the exchange could not be saved",
            ))),
        }
    }

    /// Clear the live grant and remove the STORED profile only while each
    /// still holds the access token just declared spent. Every
    /// "this grant is spent" verdict comes from this process's own view,
    /// and the documented split-process posture (a daemon running while
    /// an interactive ZeroClaw re-signs in) lets that other process
    /// commit a fresh profile moments before our verdict lands, so an
    /// unconditional remove could delete a credential that is not dead
    /// at all (greptile C13 review, F1: the three grant-death verdicts
    /// must share one removal policy). Both clears are atomic with their
    /// own compare: the live copy is inspected and wiped under a single
    /// write acquisition, and the stored compare runs inside the store's
    /// own lock, so a replacement that landed in the meantime is never
    /// mistaken for the spent one. `Ok(None)` covers "absent" and "present but this
    /// version deems it unusable" alike - in both cases the profile
    /// cannot serve the next connect as-is, and keeping it costs one
    /// warn-logged reload per connect, which is the honest fail-safe.
    /// One more kept case is deliberate: after a failed persist (a
    /// refresh committed live but warn-only on disk) the stored copy is
    /// OLDER than the spent one, so the compare misses and that stale
    /// copy survives the verdict - self-repairing, because the sign-in
    /// each of these verdicts leads into rewrites the profile on its
    /// first successful persist. Returns what happened to the stored
    /// profile so the operator-facing WARN can say exactly that.
    async fn clear_spent_grant(
        store: &McpTokenStore,
        state: &Arc<ServerState>,
        dead_access: &str,
    ) -> SpentRemoval {
        // The verdict disproves exactly one token, so only a copy that
        // still carries it may go. A replacement sign-in can take the live
        // slot while a rejected exchange is in flight (the proactive path
        // holds the single-flight lock; `ready` does not), and wiping that
        // slot on the strength of a verdict about a different token would
        // throw away a credential this verdict never disproved. Inspect and
        // wipe under one write acquisition so the compare cannot go stale.
        let mut live = state.grant.write().await;
        let live_had_dead = live
            .as_ref()
            .is_some_and(|grant| grant.tokens.access_token == dead_access);
        let live_kept_replacement = live.is_some() && !live_had_dead;
        if live_had_dead {
            *live = None;
        }
        drop(live);
        if live_kept_replacement {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_attrs(::serde_json::json!({
                        "mcp_oauth": "live-grant-kept",
                        "resource": state.canonical.resource(),
                    })),
                "MCP OAuth found a live grant this verdict did not disprove while \
                 retiring a spent token; keeping it"
            );
        }
        let removal = match store.load_authorization(&state.canonical).await {
            Ok(Some(stored)) => {
                if stored.tokens.access_token == dead_access {
                    SpentRemoval::Removed
                } else {
                    SpentRemoval::NewerKept
                }
            }
            Ok(None) => SpentRemoval::KeptUnusableOrUnknown,
            Err(err) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Retry)
                        .with_attrs(::serde_json::json!({
                            "mcp_oauth": "store-clear",
                            "resource": state.canonical.resource(),
                        })),
                    &format!(
                        "MCP OAuth dropped the live spent grant but could not inspect the \
                         stored profile; keeping it: {err:#}"
                    )
                );
                SpentRemoval::KeptUnusableOrUnknown
            }
        };
        if matches!(removal, SpentRemoval::Removed) {
            match store
                .clear_if_access_matches(&state.canonical, dead_access)
                .await
            {
                Ok(true) => {}
                // The compare runs inside the store's own lock, so this is
                // not the race: a sign-in another process committed AFTER the
                // inspection above lands here, the removal is declined, and
                // the caller reports what actually happened rather than a
                // deletion that never took place.
                Ok(false) => {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_attrs(::serde_json::json!({
                                "mcp_oauth": "store-clear-declined",
                                "resource": state.canonical.resource(),
                            })),
                        "MCP OAuth found a stored profile this verdict did not disprove \
                         while retiring the spent grant; leaving it in place"
                    );
                    return SpentRemoval::NewerKept;
                }
                Err(err) => {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                            .with_attrs(::serde_json::json!({
                                "mcp_oauth": "store-clear",
                                "resource": state.canonical.resource(),
                            })),
                        &format!("MCP OAuth could not clear a dead grant: {err:#}")
                    );
                }
            }
        }
        removal
    }

    /// Adopt a stored profile the drift verdict kept. Another process (the
    /// documented operator flow: the interactive CLI signs in while the
    /// daemon keeps running) signed this resource in, and when that sign-in
    /// was minted by the authorization server discovery now names it is
    /// exactly the credential this connect should use instead of opening a
    /// second one. A profile of any other party is not installable: the
    /// drift check exists to keep the old party's token out of the new token
    /// endpoint, and adoption must not smuggle it back in. Decided under the
    /// write lock because a concurrent connect may have committed a live
    /// grant while the store was being read.
    async fn adopt_newer_sign_in(&self, state: &Arc<ServerState>, issuer: &Url) -> bool {
        let Ok(Some(authorization)) = self.store.load_authorization(&state.canonical).await else {
            return false;
        };
        if !super::metadata::issuer_matches(&authorization.issuer, issuer) {
            return false;
        }
        let mut grant = state.grant.write().await;
        if grant.is_some() {
            return false;
        }
        *grant = Some(Grant {
            tokens: authorization.tokens,
            client_id: authorization.client_id,
            issuer: authorization.issuer,
            minted_at: None,
        });
        true
    }

    /// Grant death with no token-endpoint error to preserve (a refresh token
    /// that vanished): remove the grant under the shared spent-verdict
    /// policy (the caller's just-rejected token is the identity to
    /// compare, so a profile an interactive process re-minted meanwhile
    /// survives), then re-authorize interactively or fail actionably in
    /// background mode.
    async fn reauthorize_after_grant_death(
        &self,
        state: &Arc<ServerState>,
        interactive: bool,
        dead_access: &str,
    ) -> Result<String> {
        McpOAuthManager::clear_spent_grant(&self.store, state, dead_access).await;
        self.authorize(state, interactive).await
    }

    /// Commit a successful refresh exactly one way (shared by the forced
    /// 401 path and the proactive path — S3-5 order): retain the rotated
    /// refresh token, install the LIVE grant first (the AS may have killed
    /// the old refresh token, so losing the disk write must not strand the
    /// process), then try to persist and warn on persist failure. Both the
    /// live install and the persist run only while the live slot still
    /// carries `carried_access`, the access token this exchange refreshed:
    /// a 401 reaction and a discovery do not hold the single-flight refresh
    /// lock, so another sign-in can move the slot (and the stored profile)
    /// while the token round-trip is in flight, and installing the rotation
    /// of the displaced grant would send this process back to the sign-in
    /// that just lost. Returns the access token the waiting caller should
    /// carry, which is the replacement's when another sign-in won.
    async fn commit_refreshed_tokens(
        store: &McpTokenStore,
        state: &Arc<ServerState>,
        tokens: TokenSet,
        client_id: String,
        issuer: reqwest::Url,
        carried_access: &str,
    ) -> String {
        let mut live = state.grant.write().await;
        if let Some(current) = live.as_ref()
            && current.tokens.access_token != carried_access
        {
            // This rotation came from a grant that is no longer the
            // live one: a newer sign-in or another rotation took the
            // slot first (the forced 401 path and the proactive path
            // can redeem the same refresh token when the AS kills it
            // early, and the first 200 to land owns the slot). This
            // rotation is not ours to install over the holder's
            // credential; the disk copy is guarded by the store's own
            // ownership rule, not by this branch. A token whose refresh
            // lineage just lost is not the bearer to hand a request.
            let current_access = current.tokens.access_token.clone();
            drop(live);
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_attrs(::serde_json::json!({
                        "mcp_oauth": "live-grant-superseded",
                        "resource": state.canonical.resource(),
                    })),
                "MCP OAuth refreshed a grant whose live slot a newer sign-in or \
                 rotation had already taken; keeping that grant and using its bearer"
            );
            return current_access;
        }
        // A refresh response that omits (or empties, normalized in
        // tokens.rs) `refresh_token` leaves the previous one valid (RFC 6749
        // §6, review S2-2): keep the live grant's value instead of stranding
        // the grant at the next browser flow. Read here, from the same write
        // guard that decides the install, so the retained value is the one
        // this grant actually carried.
        let mut tokens = tokens;
        if tokens.refresh_token.is_none() {
            tokens.refresh_token = live
                .as_ref()
                .and_then(|grant| grant.tokens.refresh_token.clone());
        }
        // S3-5 order: the LIVE grant is the truth for this process;
        // install it first, then try to persist.
        let store_copy = tokens.clone();
        let minted_by_client_id = client_id.clone();
        let minted_by_issuer = issuer.clone();
        let access = tokens.access_token.clone();
        *live = Some(Grant {
            tokens,
            client_id,
            issuer,
            minted_at: Some(std::time::Instant::now()),
        });
        drop(live);
        match store
            .save_refreshed_tokens(
                &state.canonical,
                store_copy,
                &minted_by_client_id,
                &minted_by_issuer,
            )
            .await
        {
            // Another sign-in replaced this resource's stored profile while
            // our token round-trip was in flight. Its `issuer` and
            // `client_id` are not ours to rewrite, and pairing them with
            // tokens they never minted would send a foreign refresh token to
            // the wrong endpoint after a restart; our rotation stays live for
            // this process, and the replacement is what a restart uses.
            Ok(false) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_attrs(::serde_json::json!({
                            "mcp_oauth": "store-rotate-foreign",
                            "resource": state.canonical.resource(),
                        })),
                    "MCP OAuth rotated the live grant but the stored profile now belongs to \
                     a different sign-in; leaving that profile untouched"
                );
            }
            Ok(true) => {}
            Err(err) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Retry)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({
                            "mcp_oauth": "store-rotate",
                            "resource": state.canonical.resource(),
                        })),
                    &format!(
                        "MCP OAuth refreshed the live grant but could not persist it; \
                         a restart may need to re-authorize: {err:#}"
                    )
                );
            }
        }
        access
    }

    /// Refresh before expiry under the coordinator (double-checked). Returns
    /// `Some(access_token)` when it produced a fresh token.
    async fn refresh_if_expiring(&self, state: &Arc<ServerState>) -> Result<Option<String>> {
        let state = Arc::clone(state);
        let expiring = {
            let grant = state.grant.read().await;
            grant
                .as_ref()
                .is_some_and(|g| g.proactive_refresh_due(REFRESH_SKEW))
        };
        if !expiring {
            return Ok(None);
        }
        let store = self.store.clone();
        let decided: Option<(String, String, OAuthDiscovery)> = self
            .refresh
            .run_exclusive(state.canonical.resource(), async {
                // Double check: a concurrent refresh may already have fixed
                // the expiry we observed outside the lock. Capture under
                // one read guard; the dead-grant clear below must wait
                // for that guard to drop (tokio's RwLock is not
                // upgradable).
                enum Decide {
                    // Boxed for size parity with the `Dead` arm
                    // (clippy large_enum_variant; one allocation per
                    // refresh decision, not a hot path).
                    Renew(String, String, Box<OAuthDiscovery>),
                    // Carries the dead access token so the disk removal
                    // below can verify it is still removing THAT profile
                    // (adversarial review N1).
                    Dead(String),
                }
                let decide = 'guard: {
                    let grant = state.grant.read().await;
                    let Some(grant) = grant.as_ref() else {
                        break 'guard None;
                    };
                    if !grant.proactive_refresh_due(REFRESH_SKEW) {
                        break 'guard None;
                    }
                    let Some(refresh_token) = grant.tokens.refresh_token.clone() else {
                        // No way to renew. Within the 60 s refresh SKEW
                        // that is not yet an verdict: the token may
                        // still have real life and a bearer with
                        // seconds-to-minutes left is safe to SEND
                        // (greptile C13: treating the skew as expiry
                        // deleted usable credentials and re-signed-in
                        // on every handshake message). Only the
                        // handshake-margin below is a spent verdict:
                        // clear it in place (see the block below),
                        // without the heavy re-discovery a 401 would
                        // pay for, so `ready`'s re-read sees nothing
                        // and `bearer_for` falls into the
                        // single-flight interactive sign-in during
                        // `prepare_auth` - before any handshake
                        // timeout exists. Leaving a dead copy live
                        // would let this call send an expired bearer
                        // and only recover via a mid-call browser
                        // flow inside the connect budget (greptile
                        // C11).
                        if !grant.tokens.is_expiring_within(DEAD_GRANT_MARGIN) {
                            break 'guard None;
                        }
                        break 'guard Some(Decide::Dead(grant.tokens.access_token.clone()));
                    };
                    // C7: capture the discovery under the SAME grant guard
                    // and redeem only when the grant's issuer matches it
                    // (see the 401 path for why a post-guard read races
                    // with a concurrent re-discovery). On drift: skip this
                    // refresh; the stale bearer 401s and the 401 path plus
                    // `ready` resolve the new AS.
                    let Some(discovery) = state.discovery.read().await.clone() else {
                        break 'guard None;
                    };
                    if !super::metadata::issuer_matches(
                        &grant.issuer,
                        &discovery.auth_server.issuer,
                    ) {
                        break 'guard None;
                    }
                    Some(Decide::Renew(
                        refresh_token,
                        grant.client_id.clone(),
                        Box::new(discovery),
                    ))
                };
                let (refresh_token, client_id, discovery) = match decide? {
                    Decide::Renew(rt, cid, disc) => (rt, cid, *disc),
                    Decide::Dead(dead_access) => {
                        // Guard gone. The shared spent-verdict
                        // removal policy (compare-then-clear, see
                        // `clear_spent_grant`) decides about the
                        // disk copy: the expired token with no
                        // refresh token cannot progress without a
                        // browser sign-in, which rewrites the
                        // profile anyway, so the stored copy goes
                        // with the live one (a dead copy
                        // resurrected from disk would churn: per-
                        // request decrypt plus WARN where the 401
                        // path repairs once). The compare protects
                        // a profile the interactive process
                        // re-minted moments ago.
                        let removal =
                            McpOAuthManager::clear_spent_grant(&store, &state, &dead_access).await;
                        ::zeroclaw_log::record!(
                            WARN,
                            ::zeroclaw_log::Event::new(
                                module_path!(),
                                ::zeroclaw_log::Action::Reject,
                            )
                            .with_attrs(::serde_json::json!({
                                "mcp_oauth": "dead-grant",
                                "resource": state.canonical.resource(),
                                "stored_removal": removal.attr(),
                            })),
                            format!(
                                "MCP OAuth grant expired with no refresh token; {}",
                                removal.sentence()
                            )
                        );
                        return None;
                    }
                };
                Some((refresh_token, client_id, discovery))
            })
            .await;
        let Some((refresh_token, client_id, discovery)) = decided else {
            return Ok(None);
        };

        // C12: same handoff as the forced path — the exchange runs on an
        // owned task that holds the single-flight lock until it commits,
        // so neither a caller cancellation (heartbeat ticks and
        // `prepare_auth` callers are cancelled routinely) nor a slow AS
        // can drop the commit of an already-redeemed refresh token. A
        // detached outcome is non-fatal by design: the caller keeps the
        // current token (still inside its validity window at decision
        // time), and once the exchange lands the next call uses it; if
        // the exchange fails the still-live grant 401s and the forced
        // path reacts with full information.
        let exchange_deadline = state.deadline_wall();
        let resource = state.canonical.resource().to_string();
        let refresh = self.refresh.clone();
        let state_for_exchange = Arc::clone(&state);
        let token_client = state
            .policy()
            .factory
            .client_for(&discovery.auth_server.token_endpoint)
            .await?;
        let handle = zeroclaw_spawn::spawn!(refresh.run_exclusive(&resource, async move {
            let state = state_for_exchange;
            // Triple check under the lock: the section between the
            // decision and this point is exactly where a concurrent
            // refresh lands; adopt its result instead of redeeming a
            // refresh token that is no longer the live one.
            let live = state.grant.read().await;
            // Triple check under the lock: the section between the
            // decision and this point is exactly where a concurrent
            // refresh lands; adopt its result instead of redeeming a
            // refresh token that is no longer the live one. The identity
            // this exchange carries is decided here, under the same lock:
            // if the authorization server rejects the refresh, the grant it
            // disproved is this one, even if a concurrent discovery has since
            // installed a different grant in the live slot.
            let carried = live.as_ref().and_then(|grant| {
                (grant.proactive_refresh_due(REFRESH_SKEW)
                    && grant.tokens.refresh_token.as_deref() == Some(refresh_token.as_str())
                    && super::metadata::issuer_matches(
                        &grant.issuer,
                        &discovery.auth_server.issuer,
                    ))
                .then(|| grant.tokens.access_token.clone())
            });
            drop(live);
            // A live copy that stopped being the grant this section decided
            // to refresh means a concurrent commit won: this exchange must
            // not redeem a refresh token it no longer owns, and it owns no
            // spent token either, so the `?` below skips both the request and
            // any retirement.
            let carried_access = carried?;
            match refresh_tokens(
                &token_client,
                &discovery.auth_server,
                &state.canonical,
                &client_id,
                &refresh_token,
            )
            .await
            {
                Ok((tokens, _)) => Some(
                    McpOAuthManager::commit_refreshed_tokens(
                        &store,
                        &state,
                        tokens,
                        client_id.clone(),
                        discovery.auth_server.issuer.clone(),
                        &carried_access,
                    )
                    .await,
                ),
                Err(err) => {
                    // greptile C14: a refresh the authorization server
                    // rejected TERMINALLY disproves the grant, and
                    // handing the still-carried access token to the
                    // request would move the required sign-in into the
                    // 30 s handshake budget instead of the sign-in
                    // window. Retire the spent grant here (the same
                    // shared policy every other grant-death site uses)
                    // and return `None`: the caller then falls into the
                    // sign-in decision, which owns the five-minute
                    // interactive window (or the actionable background
                    // error). Transient failures keep the old posture —
                    // a 5xx/429 does not disprove the token, and design
                    // §12 never clears a grant on it.
                    let rejected_grant = err
                        .downcast_ref::<super::tokens::TokenEndpointError>()
                        .filter(|e| {
                            e.kind == TokenErrorKind::ReauthorizationRequired
                                && e.status < 500
                                && e.status != 429
                        });
                    // Design §12 configured-id downgrade, same rule as the
                    // forced path: if the rejected client IS the operator's
                    // `oauth_client_id`, a new sign-in reproduces the same
                    // rejection, so keep the grant and let the 401 path
                    // send the operator to the config instead of a browser.
                    let configured_id_rejected = rejected_grant.is_some_and(|e| {
                        matches!(
                            e.code.as_deref(),
                            Some("invalid_client") | Some("unauthorized_client")
                        ) && state.policy().config.oauth_client_id.as_deref()
                            == Some(client_id.as_str())
                    });
                    if rejected_grant.is_some() && !configured_id_rejected {
                        // Retire the token THIS exchange carried, never
                        // whatever happens to sit in the live slot now: a
                        // concurrent `ready` (which does not hold the
                        // single-flight lock) may have adopted another grant
                        // while the request was in flight, and declaring
                        // that token spent would delete a valid replacement
                        // sign-in on the strength of a verdict about a
                        // different one.
                        let removal = McpOAuthManager::clear_spent_grant(
                            &store,
                            &state,
                            carried_access.as_str(),
                        )
                        .await;
                        ::zeroclaw_log::record!(
                            WARN,
                            ::zeroclaw_log::Event::new(
                                module_path!(),
                                ::zeroclaw_log::Action::Retry,
                            )
                            .with_attrs(::serde_json::json!({
                                "mcp_oauth": "proactive-refresh-terminal",
                                "resource": state.canonical.resource(),
                                "token_error": rejected_grant
                                    .and_then(|e| e.code.as_deref())
                                    .unwrap_or("token_error"),
                                "stored_removal": removal.attr(),
                            })),
                            &format!(
                                "MCP OAuth proactive refresh was rejected for {}; \
                                 retiring the spent grant so the sign-in runs in the \
                                 sign-in window, {}",
                                state.canonical.resource(),
                                removal.sentence()
                            )
                        );
                        return None;
                    }
                    // Transient (or a configured-id rejection a new
                    // sign-in cannot fix): hand out the current token and
                    // let the 401 path react with full information.
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Retry)
                            .with_attrs(::serde_json::json!({
                                "mcp_oauth": "proactive-refresh",
                                "resource": state.canonical.resource(),
                            })),
                        &format!(
                            "MCP OAuth proactive refresh failed; using current token: {err:#}"
                        )
                    );
                    None
                }
            }
        }));
        match Join::from_handle(handle, exchange_deadline).await {
            Join::Done(Ok(access)) => Ok(access),
            // A panic inside the deferred task is the same event class as
            // a failing exchange inside it: deliberately non-fatal for the
            // caller (the live grant still stands and the 401 path reacts
            // with full information), just recorded (adversarial review:
            // the two channels for one event must not disagree).
            Join::Done(Err(join_err)) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Retry)
                        .with_attrs(::serde_json::json!({
                            "mcp_oauth": "proactive-task",
                            "resource": state.canonical.resource(),
                        })),
                    &format!(
                        "MCP OAuth proactive refresh task did not complete; using current \
                         token: {join_err}"
                    )
                );
                Ok(None)
            }
            Join::Detached => Ok(None),
        }
    }

    /// The authorization half: browser flow in interactive mode, actionable
    /// error in background mode. Caller must hold no transport locks here —
    /// this can take minutes.
    async fn authorize(&self, state: &Arc<ServerState>, interactive: bool) -> Result<String> {
        // One policy snapshot for the whole sign-in: scopes, client id and
        // the registration request must be built from the SAME config view.
        let policy = state.policy();
        if !interactive {
            bail!(
                "MCP server `{}` requires an OAuth sign-in that must be started from an \
                 interactive session (the background runtime never opens a browser); connect \
                 once from an interactive ZeroClaw to authorize {}",
                policy.config.name,
                state.canonical.resource()
            );
        }
        let discovery = state
            .discovery
            .read()
            .await
            .clone()
            .context("OAuth discovery is required before authorization")?;

        // Bind the callback port before anything that needs its URI (DCR and
        // the authorization URL), and never release it until one callback
        // has been consumed or the wait timed out.
        let listener = LoopbackListener::bind().await?;
        let redirect_uri = listener.redirect_uri();
        let pkce = Pkce::generate();
        let state_param = new_state();

        let client_id = match policy.config.oauth_client_id.as_deref() {
            Some(id) => id.to_string(),
            None => {
                let Some(endpoint) = discovery.auth_server.registration_endpoint.as_ref() else {
                    bail!(
                        "MCP server `{}`: no `oauth_client_id` is configured and the \
                         authorization server {} does not advertise dynamic registration",
                        policy.config.name,
                        discovery.auth_server.issuer
                    );
                };
                let reg_client = policy.factory.client_for(endpoint).await?;
                let registered = register_client(
                    &reg_client,
                    endpoint,
                    &redirect_uri,
                    policy.config.oauth_scopes.as_deref(),
                    discovery.auth_server.advertises_refresh_token,
                )
                .await
                .with_context(|| {
                    format!(
                        "MCP server `{}`: dynamic registration failed",
                        policy.config.name
                    )
                })?;
                registered.client_id
            }
        };

        let authorize_url = build_authorization_url(
            &discovery,
            &state.canonical,
            &client_id,
            &redirect_uri,
            &pkce,
            &state_param,
            policy.config.oauth_scopes.as_deref(),
        )?;

        // The URL is always printed: exactly what the operator can paste.
        eprintln!(
            "MCP OAuth: open this URL to sign in to `{}`:\n  {}",
            policy.config.name, authorize_url
        );
        ::zeroclaw_log::record!(
            DEBUG,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(
                ::serde_json::json!({
                    "mcp_oauth": "browser-flow",
                    "mcp_server": &policy.config.name,
                    "resource": state.canonical.resource(),
                    // The full URL embeds the ephemeral state + PKCE
                    // challenge: enough for a log reader to resume or replay
                    // the flow, so the structured attrs name only the origin
                    // the browser is sent to (its own under a split-host
                    // authorization service). The operator gets the complete
                    // URL on stderr above, and this DEBUG message carries it
                    // for diagnostics runs.
                    "authorize_origin": super::policy::origin_key(&discovery.auth_server.authorization_endpoint).unwrap_or_else(|_| state.canonical.origin_prefix()).as_str(),
                })
            ),
            &format!(
                "MCP OAuth browser sign-in started: {}",
                super::truncate_untrusted(authorize_url.as_str())
            )
        );
        if let Err(err) = self.launcher.launch(&authorize_url) {
            // Advisory only: the printed URL is the fallback by design.
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_attrs(::serde_json::json!({"mcp_oauth": "browser-launch"})),
                &format!("MCP OAuth could not launch a browser; using the printed URL: {err:#}")
            );
        }

        let flow = CallbackFlow {
            state: state_param,
            issuer: discovery.auth_server.issuer.as_str().to_string(),
            require_iss: discovery.auth_server.require_iss,
            pkce: pkce.clone(),
        };
        let code = tokio::time::timeout(
            CALLBACK_WAIT_TIMEOUT + Duration::from_secs(5),
            listener.wait_for_code(&flow, CALLBACK_WAIT_TIMEOUT),
        )
        .await
        .map_err(|_| {
            anyhow::Error::msg(
                "MCP OAuth browser sign-in did not complete before the outer deadline",
            )
        })??;

        let token_client = policy
            .factory
            .client_for(&discovery.auth_server.token_endpoint)
            .await?;
        let tokens = exchange_code(
            &token_client,
            &discovery.auth_server,
            &state.canonical,
            &client_id,
            &redirect_uri,
            &code,
            &pkce.verifier,
        )
        .await
        .with_context(|| format!("MCP server `{}`: token exchange failed", policy.config.name))?;

        self.store
            .save_authorization(
                &state.canonical,
                &client_id,
                &discovery.auth_server.issuer,
                tokens.clone(),
            )
            .await?;
        let access = tokens.access_token.clone();
        *state.grant.write().await = Some(Grant {
            tokens,
            client_id: client_id.clone(),
            issuer: discovery.auth_server.issuer.clone(),
            minted_at: Some(std::time::Instant::now()),
        });
        Ok(bearer_header_value(&access))
    }
}

/// The cheap, cloneable handle a remote transport holds: manager + the
/// server config it belongs to + the interactive posture frozen at connect
/// time. Transports never learn tokens directly; they ask for a header
/// value and report 401s back.
#[derive(Clone)]
pub struct TransportOAuth {
    manager: Arc<McpOAuthManager>,
    config: McpServerConfig,
    interactive: bool,
}

impl std::fmt::Debug for TransportOAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TransportOAuth")
            .field("mcp_server", &self.config.name)
            .field("interactive", &self.interactive)
            .finish_non_exhaustive()
    }
}

impl TransportOAuth {
    pub fn new(manager: Arc<McpOAuthManager>, config: McpServerConfig, interactive: bool) -> Self {
        Self {
            manager,
            config,
            interactive,
        }
    }

    /// Current usable `Authorization` header value (refreshes proactively
    /// near expiry; authorizes on first use).
    pub async fn bearer(&self, www_authenticate: Option<&str>) -> Result<String> {
        self.manager
            .bearer_for(&self.config, www_authenticate, self.interactive)
            .await
    }

    /// The request that carried `used_token` just got a 401; returns the
    /// header value to retry with (safe methods only — the caller decides).
    pub async fn on_unauthorized(
        &self,
        www_authenticate: Option<&str>,
        used_token: &str,
    ) -> Result<String> {
        self.manager
            .on_unauthorized(&self.config, www_authenticate, used_token, self.interactive)
            .await
    }
}

/// Assemble the authorization request URL (RFC 6749 §4.1.1 + RFC 7636 +
/// RFC 8707). The metadata gate rejects issuer identifiers that carry a
/// query but not endpoint queries, so `query_pairs_mut` preserves any
/// query the endpoint itself ships and appends to it.
fn build_authorization_url(
    discovery: &OAuthDiscovery,
    canonical: &CanonicalResource,
    client_id: &str,
    redirect_uri: &str,
    pkce: &Pkce,
    state_param: &str,
    scopes: Option<&[String]>,
) -> Result<Url> {
    let mut url = discovery.auth_server.authorization_endpoint.clone();
    {
        let mut query = url.query_pairs_mut();
        query
            .append_pair("response_type", "code")
            .append_pair("client_id", client_id)
            .append_pair("redirect_uri", redirect_uri)
            .append_pair("state", state_param)
            .append_pair("code_challenge", &pkce.challenge)
            .append_pair("code_challenge_method", "S256")
            .append_pair("resource", canonical.resource());
        if let Some(scopes) = scopes
            && !scopes.is_empty()
        {
            query.append_pair("scope", &scopes.join(" "));
        }
    }
    Ok(url)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp_oauth::tokens::TokenEndpointError;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tempfile::TempDir;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};
    use zeroclaw_providers::auth::profiles::TokenSet;

    /// `minted_at` for a grant minted before the tested decision: outside
    /// the fresh-grace window, so the plain skew rule applies to it.
    fn aged_mint() -> Option<std::time::Instant> {
        Some(std::time::Instant::now() - REFRESH_SKEW * 2)
    }

    #[test]
    fn from_config_fails_closed_on_malformed_nat64_prefix() {
        // A malformed `security.nat64_prefixes` entry must fail manager-init
        // (the wiring turns that into "no OAuth servers") rather than be
        // silently dropped, so a typo cannot quietly disable NAT64 decoding.
        let tmp = TempDir::new().expect("tmp");
        let mut config = zeroclaw_config::schema::Config {
            config_path: tmp.path().join("config.toml"),
            ..Default::default()
        };
        config.secrets.encrypt = true;
        config.security.nat64_prefixes = vec!["not-a-prefix".to_string()];
        let err = McpOAuthManager::from_config(&config)
            .expect_err("malformed NAT64 prefix must fail manager-init");
        assert!(
            err.to_string().contains("security.nat64_prefixes"),
            "got: {err}"
        );
    }

    /// A launcher that plays the browser: parses the `state` and
    /// `redirect_uri` out of the authorization URL and delivers a callback
    /// to the real listener, exactly as a browser would.
    struct AutoCallbackLauncher {
        code: &'static str,
        fail: bool,
        /// Counts `launch` invocations so dedup guarantees (concurrent
        /// first-connects) can be pinned, not just asserted by token state.
        launches: AtomicUsize,
    }

    impl BrowserLauncher for AutoCallbackLauncher {
        fn launch(&self, url: &Url) -> Result<()> {
            self.launches.fetch_add(1, Ordering::SeqCst);
            if self.fail {
                bail!("simulated launcher failure");
            }
            spawn_callback_delivery(url, self.code, Duration::ZERO);
            Ok(())
        }
    }

    /// Plays the browser half of the flow: parses `state` and
    /// `redirect_uri` out of the authorization URL and, after `delay`,
    /// delivers a callback to the real listener over a raw socket from a
    /// helper thread (`launch` must never block the flow, and no blocking
    /// HTTP client is a test dependency here).
    fn spawn_callback_delivery(url: &Url, code: &str, delay: Duration) {
        let redirect = url
            .query_pairs()
            .find(|(k, _)| k == "redirect_uri")
            .map(|(_, v)| v.into_owned())
            .expect("redirect_uri present");
        let state = url
            .query_pairs()
            .find(|(k, _)| k == "state")
            .map(|(_, v)| v.into_owned())
            .expect("state present");
        let code = code.to_string();
        std::thread::spawn(move || {
            std::thread::sleep(delay);
            let Ok(target) = Url::parse(&format!("{redirect}?code={code}&state={state}")) else {
                return;
            };
            let Some(host) = target.host_str().map(str::to_string) else {
                return;
            };
            let Some(port) = target.port_or_known_default() else {
                return;
            };
            let path = match target.query() {
                Some(q) => format!("{}?{q}", target.path()),
                None => target.path().to_string(),
            };
            if let Ok(mut stream) = std::net::TcpStream::connect((host.as_str(), port)) {
                use std::io::Write;
                let _ = write!(
                    stream,
                    "GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n"
                );
            }
        });
    }

    /// A launcher that delivers the callback `delay` after being asked to
    /// open the browser: simulates a human who takes real wall-clock time
    /// in front of the consent screen.
    struct SlowHumanLauncher {
        code: &'static str,
        delay: Duration,
    }

    impl BrowserLauncher for SlowHumanLauncher {
        fn launch(&self, url: &Url) -> Result<()> {
            spawn_callback_delivery(url, self.code, self.delay);
            Ok(())
        }
    }

    struct Stub {
        store: McpServerConfig,
        mcp: MockServer,
        as_: MockServer,
        tmp: TempDir,
    }

    async fn stub(
        register_body: Option<serde_json::Value>,
        token_bodies: Vec<(u16, serde_json::Value)>,
    ) -> Stub {
        stub_with(register_body, token_bodies, Vec::new()).await
    }

    /// `stub` plus per-token-step extra latency (review S3-3: the test for
    /// "no write permit is held across a slow token round-trip").
    async fn stub_with(
        register_body: Option<serde_json::Value>,
        token_bodies: Vec<(u16, serde_json::Value)>,
        token_extra_delay: Vec<u64>,
    ) -> Stub {
        let mcp = MockServer::start().await;
        let as_ = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/.well-known/oauth-protected-resource/mcp"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "resource": format!("{}/mcp", mcp.uri()),
                "authorization_servers": [as_.uri()],
                "bearer_methods_supported": ["header"],
            })))
            .mount(&mcp)
            .await;
        Mock::given(method("GET"))
            .and(path("/.well-known/oauth-authorization-server"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "issuer": as_.uri(),
                "authorization_endpoint": format!("{}/authorize", as_.uri()),
                "token_endpoint": format!("{}/token", as_.uri()),
                "registration_endpoint": format!("{}/register", as_.uri()),
                "grant_types_supported": ["authorization_code", "refresh_token"],
                "code_challenge_methods_supported": ["S256"],
                "token_endpoint_auth_methods_supported": ["none"],
            })))
            .mount(&as_)
            .await;
        if let Some(body) = register_body {
            Mock::given(method("POST"))
                .and(path("/register"))
                .respond_with(ResponseTemplate::new(201).set_body_json(body))
                .mount(&as_)
                .await;
        }
        {
            struct Sequenced {
                steps: Vec<(u16, serde_json::Value)>,
                extra_delay: Vec<u64>,
                next: AtomicUsize,
            }
            impl wiremock::Respond for Sequenced {
                fn respond(&self, _: &wiremock::Request) -> ResponseTemplate {
                    let step = self.next.fetch_add(1, Ordering::SeqCst);
                    // Past the end: repeat the last step so tests that
                    // over-call assert on request counts, not panics.
                    let (status, body) = self
                        .steps
                        .get(step)
                        .or_else(|| self.steps.last())
                        .expect("at least one token step");
                    let delay = self
                        .extra_delay
                        .get(step)
                        .or_else(|| {
                            (!self.extra_delay.is_empty())
                                .then_some(self.extra_delay.last())
                                .flatten()
                        })
                        .copied()
                        .unwrap_or(0);
                    ResponseTemplate::new(*status)
                        .set_delay(Duration::from_millis(delay))
                        .set_body_json(body.clone())
                }
            }
            Mock::given(method("POST"))
                .and(path("/token"))
                .respond_with(Sequenced {
                    steps: token_bodies,
                    extra_delay: token_extra_delay,
                    next: AtomicUsize::new(0),
                })
                .mount(&as_)
                .await;
        }
        let tmp = TempDir::new().expect("tmp");
        let store = McpServerConfig {
            name: "mgr-test".to_string(),
            transport: zeroclaw_config::schema::McpTransport::Http,
            url: Some(format!("{}/mcp", mcp.uri())),
            oauth: true,
            oauth_scopes: Some(vec!["mcp:tools".to_string()]),
            ..Default::default()
        };
        Stub {
            store,
            mcp,
            as_,
            tmp,
        }
    }

    fn manager_with(stub: &Stub, launcher: Arc<dyn BrowserLauncher>) -> McpOAuthManager {
        McpOAuthManager::with_launcher(stub.tmp.path(), true, launcher).expect("manager")
    }

    // Greptile C9 round-2 pin: the shared rule vector must reach an
    // ALREADY-BUILT factory. A connected server's per-server policy is not
    // rebuilt when only `security.nat64_prefixes` changed, so the factory
    // has to observe reloads through the shared Arc, not through a
    // construction-time snapshot.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn connected_server_factory_sees_reloaded_nat64_rules() {
        let stub = stub(Some(serde_json::json!({"client_id": "cid"})), vec![]).await;
        let manager = Arc::new(
            McpOAuthManager::with_launcher(
                stub.tmp.path(),
                true,
                Arc::new(AutoCallbackLauncher {
                    code: "c",
                    fail: false,
                    launches: std::sync::atomic::AtomicUsize::new(0),
                }),
            )
            .expect("manager"),
        );
        // Connect once: this builds the per-server policy/factory snapshot.
        let state = manager.state_for(&stub.store).expect("state");
        drop(state);
        assert!(
            manager.factory_nat64_prefixes(&stub.store).is_empty(),
            "a manager without a security section enforces no translators"
        );

        // Reload semantics (mirrors wiring::refresh_network_rules): parsed
        // rules replace the CONTENTS of the shared vector in place.
        manager.refresh_nat64_prefixes(
            zeroclaw_infra::net_guard::parse_nat64_prefixes(
                &["64:ff9b:1::/48".to_string()],
                "security.nat64_prefixes",
            )
            .expect("parsed"),
        );
        assert_eq!(
            manager.factory_nat64_prefixes(&stub.store).len(),
            1,
            "the factory of an already-connected server must enforce the reloaded rules"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn interactive_flow_stores_and_returns_bearer() {
        let stub = stub(
            Some(serde_json::json!({"client_id": "dyn-cid"})),
            vec![(
                200,
                serde_json::json!({"access_token": "at-1", "refresh_token": "rt-1", "expires_in": 3600, "token_type": "Bearer"}),
            )],
        )
        .await;
        let manager = manager_with(
            &stub,
            Arc::new(AutoCallbackLauncher {
                code: "the-code",
                fail: false,
                launches: AtomicUsize::new(0),
            }),
        );
        let bearer = manager
            .bearer_for(&stub.store, None, true)
            .await
            .expect("interactive flow");
        assert_eq!(bearer, "Bearer at-1");

        // Authorization URL reached the launcher with the right shape.
        // DCR was used, and the grant persisted encrypted under the store.
        let canonical = CanonicalResource::from_configured(
            &Url::parse(&format!("{}/mcp", stub.mcp.uri())).expect("url"),
        )
        .expect("canonical");
        let loaded = manager
            .store
            .load_authorization(&canonical)
            .await
            .expect("load")
            .expect("stored");
        assert_eq!(loaded.client_id, "dyn-cid");
        assert_eq!(loaded.tokens.access_token, "at-1");

        let requests = stub.as_.received_requests().await.expect("received");
        let token_body = requests
            .iter()
            .find(|r| r.url.path() == "/token")
            .map(|r| String::from_utf8(r.body.clone()).expect("utf8"))
            .expect("token request");
        assert!(token_body.contains("code=the-code"));
        assert!(token_body.contains("code_verifier="));
        assert!(
            token_body.contains(&format!("resource={}", urlencoded(canonical.resource()))),
            "token exchange must carry the RFC 8707 resource parameter"
        );
    }

    #[tokio::test]
    async fn failed_discovery_is_not_cached_and_reprobes_on_next_connect() {
        // Discovery failure must not poison the process: the next connect
        // re-runs the WHOLE chain (candidates + probe), so a server that
        // only fixed its well-known endpoints between connects recovers.
        let mcp = MockServer::start().await;
        let as_ = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/.well-known/oauth-authorization-server"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "issuer": as_.uri(),
                "authorization_endpoint": format!("{}/authorize", as_.uri()),
                "token_endpoint": format!("{}/token", as_.uri()),
                "grant_types_supported": ["authorization_code", "refresh_token"],
                "code_challenge_methods_supported": ["S256"],
                "token_endpoint_auth_methods_supported": ["none"],
            })))
            .mount(&as_)
            .await;
        let tmp = TempDir::new().expect("tmp");
        let config = McpServerConfig {
            name: "retry-test".to_string(),
            transport: zeroclaw_config::schema::McpTransport::Http,
            url: Some(format!("{}/mcp", mcp.uri())),
            oauth: true,
            ..Default::default()
        };
        let manager = McpOAuthManager::with_launcher(tmp.path(), true, Arc::new(NeverLaunch))
            .expect("manager");

        // Pass 1: the PRM is everywhere-404 and the probe is not
        // challenged; discovery fails and must NOT be cached as a verdict.
        let err = manager
            .bearer_for(&config, None, false)
            .await
            .expect_err("404-everywhere must fail discovery");
        assert!(
            err.to_string()
                .contains("no usable protected-resource metadata"),
            "got: {err}"
        );

        // The operator then publishes the PRM. Wiremock prefers the most
        // recently registered matching mock.
        Mock::given(method("GET"))
            .and(path("/.well-known/oauth-protected-resource/mcp"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "resource": format!("{}/mcp", mcp.uri()),
                "authorization_servers": [as_.uri()],
                "bearer_methods_supported": ["header"],
            })))
            .mount(&mcp)
            .await;
        let err = manager
            .bearer_for(&config, None, false)
            .await
            .expect_err("discovery now succeeds; background mode still needs a sign-in");
        assert!(
            err.to_string().contains("interactive session"),
            "second attempt must be past discovery, got: {err}"
        );

        let requests = mcp.received_requests().await.expect("received");
        let count = |p: &str| requests.iter().filter(|r| r.url.path() == p).count();
        assert_eq!(
            count("/.well-known/oauth-protected-resource/mcp"),
            2,
            "reprobed after failure"
        );
        assert_eq!(count("/mcp"), 1, "probe only ran on the failed pass");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn first_connect_finds_prm_only_through_the_401_challenge_hint() {
        // The tidux blocking ask, pinned at the manager boundary: a
        // production MCP gateway whose PRM lives ONLY at a custom path
        // advertised exclusively in its `WWW-Authenticate` challenge —
        // every well-known form 404s and the endpoint itself answers
        // with a bare error status (no challenge in a probe without a
        // session). Because the transport fetches the bearer BEFORE its
        // first send, a client that ignored the challenge would fail
        // discovery without ever seeing that header. Two phases: a
        // no-challenge connect must prove the well-known paths really
        // are dead (discovery fails, never cached), then the connect
        // that carries the challenge reaches sign-in through the hinted
        // URL alone.
        let mcp = MockServer::start().await;
        let as_ = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/alt/prm"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "resource": format!("{}/mcp", mcp.uri()),
                "authorization_servers": [as_.uri()],
                "bearer_methods_supported": ["header"],
            })))
            .mount(&mcp)
            .await;
        Mock::given(method("GET"))
            .and(path("/.well-known/oauth-authorization-server"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "issuer": as_.uri(),
                "authorization_endpoint": format!("{}/authorize", as_.uri()),
                "token_endpoint": format!("{}/token", as_.uri()),
                "registration_endpoint": format!("{}/register", as_.uri()),
                "grant_types_supported": ["authorization_code", "refresh_token"],
                "code_challenge_methods_supported": ["S256"],
                "token_endpoint_auth_methods_supported": ["none"],
            })))
            .mount(&as_)
            .await;
        Mock::given(method("POST"))
            .and(path("/register"))
            .respond_with(
                ResponseTemplate::new(201)
                    .set_body_json(serde_json::json!({"client_id": "dyn-cid"})),
            )
            .mount(&as_)
            .await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "at-hint",
                "refresh_token": "rt-hint",
                "expires_in": 3600,
                "token_type": "Bearer",
            })))
            .mount(&as_)
            .await;
        let tmp = TempDir::new().expect("tmp");
        let config = McpServerConfig {
            name: "hint-test".to_string(),
            transport: zeroclaw_config::schema::McpTransport::Http,
            url: Some(format!("{}/mcp", mcp.uri())),
            oauth: true,
            oauth_scopes: Some(vec!["mcp:tools".to_string()]),
            ..Default::default()
        };

        let background = McpOAuthManager::with_launcher(tmp.path(), true, Arc::new(NeverLaunch))
            .expect("manager");
        let err = background
            .bearer_for(&config, None, false)
            .await
            .expect_err("no well-known PRM and no probe challenge: discovery must fail");
        assert!(
            err.to_string()
                .contains("no usable protected-resource metadata"),
            "phase 1 must die inside discovery, not at the sign-in gate: {err}"
        );

        let launcher = Arc::new(AutoCallbackLauncher {
            code: "hint-code",
            fail: false,
            launches: AtomicUsize::new(0),
        });
        let manager = McpOAuthManager::with_launcher(
            tmp.path(),
            true,
            Arc::clone(&launcher) as Arc<dyn BrowserLauncher>,
        )
        .expect("manager");
        let challenge = format!(
            r#"Bearer error="invalid_token", resource_metadata="{}/alt/prm""#,
            mcp.uri()
        );
        let bearer = manager
            .bearer_for(&config, Some(&challenge), true)
            .await
            .expect("challenge-hinted discovery + interactive sign-in");
        assert_eq!(bearer, "Bearer at-hint");
        assert_eq!(
            launcher.launches.load(Ordering::SeqCst),
            1,
            "exactly one browser sign-in"
        );

        let canonical = CanonicalResource::from_configured(
            &Url::parse(&format!("{}/mcp", mcp.uri())).expect("url"),
        )
        .expect("canonical");
        let loaded = manager
            .store
            .load_authorization(&canonical)
            .await
            .expect("load")
            .expect("stored");
        assert_eq!(loaded.tokens.access_token, "at-hint");

        let requests = mcp.received_requests().await.expect("received");
        assert_eq!(
            requests
                .iter()
                .filter(|r| r.method.as_str() == "GET" && r.url.path() == "/mcp")
                .count(),
            1,
            "only phase 1's probe may touch the endpoint; phase 2 discovers from the hint alone"
        );
        let hint_gets = requests
            .iter()
            .filter(|r| {
                r.url.path() == "/alt/prm"
                    && r.method.as_str() == "GET"
                    && !r.headers.contains_key("authorization")
            })
            .count();
        assert_eq!(
            hint_gets, 1,
            "the hinted PRM must be fetched exactly once, anonymously"
        );
        assert!(
            !requests.iter().any(|r| r
                .headers
                .get("authorization")
                .is_some_and(|v| v.as_bytes().starts_with(b"Bearer "))),
            "discovery fetches never carry a bearer"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn successful_discovery_cached_and_bare_401_never_refetches_metadata() {
        // Steady state: token-lifecycle 401s (challenge WITHOUT
        // resource_metadata) must not touch discovery at all.
        let stub = stub(
            Some(serde_json::json!({"client_id": "dyn-cid"})),
            vec![(
                200,
                serde_json::json!({"access_token": "at-1", "refresh_token": "rt-1", "expires_in": 3600, "token_type": "Bearer"}),
            ), (
                200,
                serde_json::json!({"access_token": "at-2", "refresh_token": "rt-2", "expires_in": 3600, "token_type": "Bearer"}),
            )],
        )
        .await;
        let manager = manager_with(
            &stub,
            Arc::new(AutoCallbackLauncher {
                code: "the-code",
                fail: false,
                launches: AtomicUsize::new(0),
            }),
        );
        let bearer = manager
            .bearer_for(&stub.store, None, true)
            .await
            .expect("interactive flow");
        assert_eq!(bearer, "Bearer at-1");

        let rotated = manager
            .on_unauthorized(
                &stub.store,
                Some(r#"Bearer error="invalid_token""#),
                "Bearer at-1",
                false,
            )
            .await
            .expect("rotation");
        assert_eq!(rotated, "Bearer at-2");

        let requests = stub.mcp.received_requests().await.expect("received");
        assert_eq!(
            requests
                .iter()
                .filter(|r| r.url.path().starts_with("/.well-known"))
                .count(),
            1,
            "exactly one PRM fetch for the whole lifecycle"
        );
        assert_eq!(
            requests.iter().filter(|r| r.url.path() == "/mcp").count(),
            0,
            "no probe on happy path"
        );
    }

    #[test]
    fn live_policy_snapshot_follows_config_edits() {
        // C4 (AGENTS.md): long-lived per-resource state must not freeze the
        // first config it ever saw; a re-assembly with edited fields must
        // take effect without a restart.
        let tmp = TempDir::new().expect("tmp");
        let manager = McpOAuthManager::with_launcher(tmp.path(), true, Arc::new(NeverLaunch))
            .expect("manager");
        let cfg1 = McpServerConfig {
            name: "live".to_string(),
            transport: zeroclaw_config::schema::McpTransport::Http,
            url: Some("http://127.0.0.1:9/mcp".to_string()),
            oauth: true,
            oauth_client_id: Some("one".to_string()),
            ..Default::default()
        };
        let state = manager.state_for(&cfg1).expect("state");
        assert_eq!(
            state.policy().config.oauth_client_id.as_deref(),
            Some("one"),
            "first assembly seeds the snapshot"
        );
        let cfg2 = McpServerConfig {
            oauth_client_id: Some("two".to_string()),
            ..cfg1.clone()
        };
        let state2 = manager.state_for(&cfg2).expect("state");
        assert!(
            Arc::ptr_eq(&state, &state2),
            "same resource keeps one state"
        );
        assert_eq!(
            state.policy().config.oauth_client_id.as_deref(),
            Some("two"),
            "edited config refreshes the snapshot in place"
        );
    }

    #[tokio::test]
    async fn concurrent_store_load_cannot_clobber_fresh_tokens() {
        // C8: `ready` checks for a missing grant, loads the store across
        // await points, then installs. A concurrent refresh can commit
        // FRESH tokens inside that window, and a blind install would
        // restore the spent store copy. Drive `ready` with manual polls so
        // the interleaving is deterministic: park it at its first real
        // await, install a fresher live grant, then let it finish.
        let stub = stub(None, vec![]).await;
        let manager = manager_with(&stub, Arc::new(NeverLaunch));
        let canonical = CanonicalResource::from_configured(
            &Url::parse(&format!("{}/mcp", stub.mcp.uri())).expect("url"),
        )
        .expect("canonical");
        manager
            .seed_authorization(
                &canonical,
                "cid",
                &Url::parse(&stub.as_.uri()).expect("issuer"),
                TokenSet {
                    access_token: "at-stale".to_string(),
                    refresh_token: Some("rt-stale-spent".to_string()),
                    id_token: None,
                    expires_at: Some(chrono::Utc::now() + chrono::Duration::hours(6)),
                    token_type: Some("Bearer".to_string()),
                    scope: None,
                },
            )
            .await
            .expect("seed store copy");
        let state = manager.state_for(&stub.store).expect("state");
        let mut ready = std::pin::pin!(manager.ready(&state, None));
        {
            let waker = futures_util::task::noop_waker();
            let mut cx = std::task::Context::from_waker(&waker);
            // The premise of the race: `ready` must park inside the store
            // load. If it ever resolves in one poll there is no window and
            // this test's model is void - the assert makes that visible.
            assert!(
                matches!(ready.as_mut().poll(&mut cx), std::task::Poll::Pending),
                "expected `ready` to park on the store load"
            );
        }
        // Review note (C8 round): the Pending above pins that the future
        // parked, but not WHERE - the store-load tokio::fs steps can all
        // complete inline on a hot blocking pool, after which a
        // window-open assert would flake (measured ~1 in 4 full-suite
        // runs). The inject-then-await asserts below still pin the race
        // outcome itself.
        // The concurrent winner: a rotated set committed by a refresh.
        *state.grant.write().await = Some(Grant {
            tokens: TokenSet {
                access_token: "at-fresh".to_string(),
                refresh_token: Some("rt-fresh".to_string()),
                id_token: None,
                expires_at: None,
                token_type: Some("Bearer".to_string()),
                scope: None,
            },
            client_id: "cid".to_string(),
            issuer: Url::parse(&stub.as_.uri()).expect("issuer"),
            minted_at: aged_mint(),
        });
        ready.await.expect("ready");
        let grant = state.grant.read().await;
        let grant = grant.as_ref().expect("grant present");
        assert_eq!(
            grant.tokens.access_token, "at-fresh",
            "a late store install must not clobber the fresher live grant"
        );
        assert_eq!(
            grant.tokens.refresh_token.as_deref(),
            Some("rt-fresh"),
            "the spent store refresh token must not be restored"
        );
    }

    #[tokio::test]
    async fn grant_death_during_401_reauthorize_does_not_deadlock() {
        // Greptile C2: the in-section re-check used to hold the grant READ
        // guard across `authorize().await`, which ends in `grant.write()` —
        // a self-deadlock on tokio's non-upgradable RwLock whenever a 401
        // path found no live grant (e.g. `ready` cleared it for an issuer
        // change). Background posture must return the actionable error in
        // bounded time instead.
        let stub = stub(None, vec![]).await;
        let manager = manager_with(&stub, Arc::new(NeverLaunch));
        let result = tokio::time::timeout(
            Duration::from_secs(5),
            manager.on_unauthorized(&stub.store, None, "Bearer stale", false),
        )
        .await
        .expect("re-authorizing after grant death must not hang");
        let err = result.expect_err("background mode cannot sign in");
        assert!(
            err.to_string().contains("interactive session"),
            "got: {err}"
        );
    }

    async fn seed_expired_grant(stub: &Stub, manager: &McpOAuthManager) {
        let canonical = CanonicalResource::from_configured(
            &Url::parse(&format!("{}/mcp", stub.mcp.uri())).expect("url"),
        )
        .expect("canonical");
        manager
            .seed_authorization(
                &canonical,
                "cid",
                &Url::parse(&stub.as_.uri()).expect("issuer"),
                TokenSet {
                    access_token: "at-7".to_string(),
                    refresh_token: Some("rt-issuer-cross".to_string()),
                    id_token: None,
                    // Expired: every path treats this grant as refresh-due.
                    expires_at: Some(chrono::Utc::now() - chrono::Duration::hours(1)),
                    token_type: Some("Bearer".to_string()),
                    scope: None,
                },
            )
            .await
            .expect("seed grant");
    }

    async fn forge_far_discovery(stub: &Stub, manager: &McpOAuthManager, far: &MockServer) {
        // Simulate the race greptile-C7 described: a SECOND agent's 401
        // re-discovery installs an issuer-B discovery while a refresh
        // against issuer A is already mid-flight. `ready` runs outside the
        // single-flight section, so this write can land at any moment; the
        // state mutation below is the exact shape those paths perform.
        let state = manager.state_for(&stub.store).expect("state");
        let mut discovery = state
            .discovery
            .read()
            .await
            .clone()
            .expect("discovery present");
        let far_url = Url::parse(&format!("{}/token", far.uri())).expect("far endpoint");
        discovery.auth_server.issuer = Url::parse(&far.uri()).expect("far issuer");
        discovery.auth_server.token_endpoint = far_url;
        *state.discovery.write().await = Some(discovery);
    }

    #[tokio::test]
    async fn proactive_refresh_refuses_a_cross_issuer_token_endpoint() {
        // C7: the grant names issuer A; a re-discovery (a second agent's
        // 401) replaced the cached discovery with issuer B's documents
        // between `ready` and the refresh capture. A's refresh token must
        // never be redeemed at B's token endpoint. The far mock answers
        // 200, so a pre-fix leak is OBSERVABLE: it would rotate the live
        // grant to `at-wrong-issuer`.
        let stub = stub(
            None,
            vec![(
                500,
                serde_json::json!({"error": "temporarily_unavailable", "error_description": "probe"}),
            )],
        )
        .await;
        let far = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "at-wrong-issuer",
                "expires_in": 3600,
                "token_type": "Bearer",
            })))
            .mount(&far)
            .await;
        let manager = manager_with(&stub, Arc::new(NeverLaunch));
        seed_expired_grant(&stub, &manager).await;
        assert_eq!(
            manager
                .bearer_for(&stub.store, None, false)
                .await
                .expect("a transient refresh failure still serves the live grant"),
            "Bearer at-7"
        );
        let state = manager.state_for(&stub.store).expect("state");
        forge_far_discovery(&stub, &manager, &far).await;
        let outcome = manager
            .refresh_if_expiring(&state)
            .await
            .expect("a cross-issuer skip is not an error");
        assert!(
            outcome.is_none(),
            "the refresh must be skipped, not sent, got: {outcome:?}"
        );
        let sent = far.received_requests().await.unwrap_or_default();
        assert!(
            sent.is_empty(),
            "A's refresh token must never reach B's token endpoint, got: {:?}",
            sent.iter()
                .map(|r| (r.method.as_str().to_string(), r.url.path().to_string()))
                .collect::<Vec<_>>()
        );
        {
            let grant = state.grant.read().await;
            assert_eq!(
                grant.as_ref().expect("grant intact").tokens.access_token,
                "at-7",
                "the skipped refresh must not rotate the grant"
            );
        }
    }

    #[tokio::test]
    async fn forced_refresh_refuses_a_cross_issuer_token_endpoint() {
        // The 401 path carries the same guarantee. `ready` runs before the
        // single-flight section and itself detects the drift, so the
        // observable contract here is: background posture resolves to the
        // actionable sign-in error and no cross-issuer POST is sent.
        let stub = stub(
            None,
            vec![(
                500,
                serde_json::json!({"error": "temporarily_unavailable", "error_description": "probe"}),
            )],
        )
        .await;
        let far = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "at-wrong-issuer",
                "expires_in": 3600,
                "token_type": "Bearer",
            })))
            .mount(&far)
            .await;
        let manager = manager_with(&stub, Arc::new(NeverLaunch));
        seed_expired_grant(&stub, &manager).await;
        assert_eq!(
            manager
                .bearer_for(&stub.store, None, false)
                .await
                .expect("live grant still serves"),
            "Bearer at-7"
        );
        forge_far_discovery(&stub, &manager, &far).await;
        let err = tokio::time::timeout(
            Duration::from_secs(5),
            manager.on_unauthorized(&stub.store, None, "Bearer at-7", false),
        )
        .await
        .expect("no deadlock")
        .expect_err("issuer drift resolves to sign-in, and background cannot sign in");
        assert!(
            err.to_string().contains("interactive session"),
            "got: {err}"
        );
        let sent = far.received_requests().await.unwrap_or_default();
        assert!(
            sent.is_empty(),
            "A's refresh token must never reach B's token endpoint, got: {:?}",
            sent.iter()
                .map(|r| (r.method.as_str().to_string(), r.url.path().to_string()))
                .collect::<Vec<_>>()
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn on_unauthorized_with_no_grant_runs_full_interactive_signin() {
        // The same path, interactive: it must complete the browser flow and
        // hand back a fresh bearer (this is the shape that deadlocked).
        let stub = stub(
            Some(serde_json::json!({"client_id": "dyn-cid"})),
            vec![(
                200,
                serde_json::json!({"access_token": "at-1", "refresh_token": "rt-1", "expires_in": 3600, "token_type": "Bearer"}),
            )],
        )
        .await;
        let manager = manager_with(
            &stub,
            Arc::new(AutoCallbackLauncher {
                code: "the-code",
                fail: false,
                launches: AtomicUsize::new(0),
            }),
        );
        let result = tokio::time::timeout(
            Duration::from_secs(30),
            manager.on_unauthorized(&stub.store, None, "Bearer stale", true),
        )
        .await
        .expect("interactive re-authorization must not deadlock");
        assert_eq!(result.expect("sign-in"), "Bearer at-1");
    }

    #[tokio::test]
    async fn signin_from_another_process_is_picked_up_without_restart() {
        // Greptile C3: the operator flow the docs promise is "sign in once
        // from an interactive CLI; the already-running daemon reuses the
        // grant". The per-process OnceCell used to freeze the first empty
        // store read, so the daemon never saw the new grant. Simulate the
        // external sign-in by writing the profile between connects.
        let stub = stub(None, vec![]).await;
        let manager = manager_with(&stub, Arc::new(NeverLaunch));
        let err = manager
            .bearer_for(&stub.store, None, false)
            .await
            .expect_err("no grant yet");
        assert!(
            err.to_string().contains("interactive session"),
            "got: {err}"
        );

        let canonical = CanonicalResource::from_configured(
            &Url::parse(&format!("{}/mcp", stub.mcp.uri())).expect("url"),
        )
        .expect("canonical");
        manager
            .seed_authorization(
                &canonical,
                "cid",
                &Url::parse(&stub.as_.uri()).expect("issuer"),
                TokenSet {
                    access_token: "at-external".to_string(),
                    refresh_token: Some("rt-external".to_string()),
                    id_token: None,
                    expires_at: None,
                    token_type: Some("Bearer".to_string()),
                    scope: None,
                },
            )
            .await
            .expect("seed external grant");

        let bearer = manager
            .bearer_for(&stub.store, None, false)
            .await
            .expect("the next connect must pick the new grant up, no restart");
        assert_eq!(bearer, "Bearer at-external");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn post_discovery_challenge_with_different_prm_reshuffles_discovery_once() {
        // RFC 9728 §5.2 single-shot: the FIRST 401 challenge advertising a
        // different metadata location re-runs discovery exactly once; later
        // differing challenges are noise (no per-401 discovery loop).
        let stub = stub(
            Some(serde_json::json!({"client_id": "dyn-cid"})),
            vec![(
                200,
                serde_json::json!({"access_token": "at-1", "refresh_token": "rt-1", "expires_in": 3600, "token_type": "Bearer"}),
            ), (
                200,
                serde_json::json!({"access_token": "at-2", "refresh_token": "rt-2", "expires_in": 3600, "token_type": "Bearer"}),
            )],
        )
        .await;
        Mock::given(method("GET"))
            .and(path("/alt/prm"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "resource": format!("{}/mcp", stub.mcp.uri()),
                "authorization_servers": [stub.as_.uri()],
                "bearer_methods_supported": ["header"],
            })))
            .mount(&stub.mcp)
            .await;
        let manager = manager_with(
            &stub,
            Arc::new(AutoCallbackLauncher {
                code: "the-code",
                fail: false,
                launches: AtomicUsize::new(0),
            }),
        );
        manager
            .bearer_for(&stub.store, None, true)
            .await
            .expect("interactive flow");

        let challenge = format!(
            r#"Bearer error="invalid_token", resource_metadata="{}/alt/prm""#,
            stub.mcp.uri()
        );
        let rotated = manager
            .on_unauthorized(&stub.store, Some(&challenge), "Bearer at-1", false)
            .await
            .expect("rotation after reshuffle");
        assert_eq!(rotated, "Bearer at-2");

        // Second differing challenge: the latch is spent, no refetch.
        let challenge2 = format!(
            r#"Bearer error="invalid_token", resource_metadata="{}/alt/prm2""#,
            stub.mcp.uri()
        );
        manager
            .on_unauthorized(&stub.store, Some(&challenge2), "Bearer at-2", false)
            .await
            .expect("second rotation proceeds on cached discovery");

        let requests = stub.mcp.received_requests().await.expect("received");
        let count = |p: &str| requests.iter().filter(|r| r.url.path() == p).count();
        assert_eq!(
            count("/alt/prm"),
            1,
            "reshuffle fetched the advertised location once"
        );
        assert_eq!(
            count("/.well-known/oauth-protected-resource/mcp"),
            1,
            "default form only on first discovery"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_failed_stale_reshuffle_keeps_the_previously_working_document() {
        // Adversarial review: the §5.2 invalidation must be STAGED. A
        // reverse proxy advertising its own `resource_metadata` whose
        // declared resource violates the contract is a hard discovery
        // failure (never papered over by the next candidate); destroying
        // the cached document up-front meant the once-working document
        // was gone for the process lifetime and every later call re-ran
        // the same failing pass. Now the failed re-run restores the old
        // document and this very call rotates on it; the one-shot latch
        // stays spent on purpose — re-arming it would let a lying proxy
        // pay a full discovery pass on every single 401.
        let stub = stub(
            Some(serde_json::json!({"client_id": "dyn-cid"})),
            vec![
                (
                    200,
                    serde_json::json!({"access_token": "at-1", "refresh_token": "rt-1", "expires_in": 3600, "token_type": "Bearer"}),
                ),
                (
                    200,
                    serde_json::json!({"access_token": "at-2", "refresh_token": "rt-2", "expires_in": 3600, "token_type": "Bearer"}),
                ),
            ],
        )
        .await;
        Mock::given(method("GET"))
            .and(path("/alt/prm"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                // Contract violation: declares a different resource, so
                // the advertised document is rejected hard.
                "resource": "https://proxy.example/api",
                "authorization_servers": [stub.as_.uri()],
                "bearer_methods_supported": ["header"],
            })))
            .mount(&stub.mcp)
            .await;
        let manager = manager_with(
            &stub,
            Arc::new(AutoCallbackLauncher {
                code: "the-code",
                fail: false,
                launches: AtomicUsize::new(0),
            }),
        );
        manager
            .bearer_for(&stub.store, None, true)
            .await
            .expect("interactive flow");
        let cached_before = {
            let state = manager.state_for(&stub.store).expect("state");
            let discovery = state.discovery.read().await;
            discovery
                .as_ref()
                .expect("cached discovery")
                .protected_resource_url
                .clone()
        };

        let challenge = format!(
            r#"Bearer error="invalid_token", resource_metadata="{}/alt/prm""#,
            stub.mcp.uri()
        );
        let rotated = manager
            .on_unauthorized(&stub.store, Some(&challenge), "Bearer at-1", false)
            .await
            .expect("the failed reshuffle restores the old document and rotates on it");
        assert_eq!(rotated, "Bearer at-2");

        let state = manager.state_for(&stub.store).expect("state");
        let cached_after = {
            let discovery = state.discovery.read().await;
            discovery
                .as_ref()
                .expect("the working document survived the failed reshuffle")
                .protected_resource_url
                .clone()
        };
        assert_eq!(
            cached_after, cached_before,
            "a failed §5.2 re-run restores the previously cached document"
        );
        let requests = stub.mcp.received_requests().await.expect("received");
        let count = |p: &str| requests.iter().filter(|r| r.url.path() == p).count();
        assert_eq!(
            count("/alt/prm"),
            1,
            "the advertised location is tried exactly once; the spent latch does not re-arm"
        );
    }

    #[tokio::test]
    async fn a_cancelled_reshuffle_restores_the_previously_working_document() {
        // Cancellation is not an error return, so the restore arm never runs.
        // If the staged document lived only in that arm, a caller cancelled at
        // its tool-call deadline (the daemon heartbeat does this routinely)
        // would leave the resource without the only metadata location that
        // ever worked, for the whole process lifetime, with the one-shot latch
        // already spent. The staging guard must put it back when the future is
        // dropped. The noop waker keeps this deterministic: the reshuffle can
        // be driven into the request but can never complete, so the drop is
        // always a mid-flight cancel.
        let stub = stub(
            Some(serde_json::json!({"client_id": "dyn-cid"})),
            vec![(
                200,
                serde_json::json!({"access_token": "at-1", "refresh_token": "rt-1", "expires_in": 3600, "token_type": "Bearer"}),
            )],
        )
        .await;
        Mock::given(method("GET"))
            .and(path("/alt/prm"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({
                        "resource": format!("{}/mcp", stub.mcp.uri()),
                        "authorization_servers": [stub.as_.uri()],
                        "bearer_methods_supported": ["header"],
                    }))
                    .set_delay(std::time::Duration::from_millis(500)),
            )
            .mount(&stub.mcp)
            .await;
        let manager = manager_with(
            &stub,
            Arc::new(AutoCallbackLauncher {
                code: "the-code",
                fail: false,
                launches: AtomicUsize::new(0),
            }),
        );
        manager
            .bearer_for(&stub.store, None, true)
            .await
            .expect("interactive flow");
        let cached_before = {
            let state = manager.state_for(&stub.store).expect("state");
            state
                .discovery
                .read()
                .await
                .as_ref()
                .expect("cached discovery")
                .protected_resource_url
                .clone()
        };

        let challenge = format!(
            r#"Bearer error="invalid_token", resource_metadata="{}/alt/prm""#,
            stub.mcp.uri()
        );
        {
            let fut = manager.on_unauthorized(&stub.store, Some(&challenge), "Bearer at-1", false);
            tokio::pin!(fut);
            let waker = futures_util::task::noop_waker();
            let mut cx = std::task::Context::from_waker(&waker);
            for _ in 0..50 {
                let _ = fut.as_mut().poll(&mut cx);
                if stub
                    .mcp
                    .received_requests()
                    .await
                    .expect("received")
                    .iter()
                    .any(|r| r.url.path() == "/alt/prm")
                {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            assert!(
                stub.mcp
                    .received_requests()
                    .await
                    .expect("received")
                    .iter()
                    .any(|r| r.url.path() == "/alt/prm"),
                "the reshuffle fetch must be in flight before the cancel"
            );
            // Drop the (never-completing) future: this is the cancel.
        }

        let state = manager.state_for(&stub.store).expect("state");
        let cached_after = {
            state
                .discovery
                .read()
                .await
                .as_ref()
                .expect("the cancelled reshuffle must put the working document back")
                .protected_resource_url
                .clone()
        };
        assert_eq!(
            cached_after, cached_before,
            "a cancelled §5.2 reshuffle must not lose the previously cached document"
        );
    }

    struct NeverLaunch;
    impl BrowserLauncher for NeverLaunch {
        fn launch(&self, _url: &Url) -> Result<()> {
            anyhow::bail!("never")
        }
    }

    fn urlencoded(s: &str) -> String {
        // Mirrors `form_urlencoded` exactly: alphanumerics plus `*-._` are
        // literal, everything else percent-encoded (spaces would be `+`).
        s.bytes()
            .map(|b| match b {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'*' | b'-' | b'.' | b'_' => {
                    (b as char).to_string()
                }
                _ => format!("%{b:02X}"),
            })
            .collect()
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn configured_client_id_skips_registration() {
        let mut stub = stub(None, vec![(200, serde_json::json!({"access_token": "at"}))]).await;
        stub.store.oauth_client_id = Some("static-cid".to_string());
        // A launcher that reports failure AFTER delivering the callback:
        // the printed-URL fallback keeps the flow alive when the browser
        // open itself fails, and this pins that end to end.
        let manager = {
            struct DeliveringFail;
            impl BrowserLauncher for DeliveringFail {
                fn launch(&self, url: &Url) -> Result<()> {
                    AutoCallbackLauncher {
                        code: "c2",
                        fail: false,
                        launches: AtomicUsize::new(0),
                    }
                    .launch(url)
                    .expect("deliver");
                    bail!("simulated open failure after delivery");
                }
            }
            McpOAuthManager::with_launcher(stub.tmp.path(), true, Arc::new(DeliveringFail))
                .expect("m")
        };
        let bearer = manager
            .bearer_for(&stub.store, None, true)
            .await
            .expect("flow");
        assert_eq!(bearer, "Bearer at");
        let requests = stub.as_.received_requests().await.expect("received");
        assert!(
            !requests.iter().any(|r| r.url.path() == "/register"),
            "configured client id must not register"
        );
        let _ = manager;
    }

    #[tokio::test]
    async fn background_mode_without_grant_fails_actionably_and_binds_nothing() {
        let stub = stub(Some(serde_json::json!({"client_id": "x"})), vec![]).await;
        struct Never;
        impl BrowserLauncher for Never {
            fn launch(&self, _url: &Url) -> Result<()> {
                panic!("background mode must never launch a browser");
            }
        }
        let manager =
            McpOAuthManager::with_launcher(stub.tmp.path(), true, Arc::new(Never)).expect("m");
        let err = manager
            .bearer_for(&stub.store, None, false)
            .await
            .expect_err("background needs an interactive first sign-in");
        assert!(err.to_string().contains("interactive"), "got: {err}");
        // And the error carries the server name for actionability.
        assert!(err.to_string().contains("mgr-test"), "got: {err}");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn on_unauthorized_rotates_once_and_concurrent_rotation_short_circuits() {
        let stub = stub(
            Some(serde_json::json!({"client_id": "dyn-cid"})),
            vec![
                (
                    200,
                    serde_json::json!({"access_token": "at-1", "refresh_token": "rt-1", "expires_in": 3600, "token_type": "Bearer"}),
                ),
                (
                    200,
                    serde_json::json!({"access_token": "at-2", "refresh_token": "rt-2", "expires_in": 3600, "token_type": "Bearer"}),
                ),
            ],
        )
        .await;
        let manager = Arc::new(manager_with(
            &stub,
            Arc::new(AutoCallbackLauncher {
                code: "c",
                fail: false,
                launches: AtomicUsize::new(0),
            }),
        ));
        let first = manager
            .bearer_for(&stub.store, None, true)
            .await
            .expect("flow");
        assert_eq!(first, "Bearer at-1");

        // Two concurrent 401s carrying at-1: exactly one token-endpoint
        // refresh runs (wiremock serves both refreshes, but the second
        // caller must observe the rotated token and return it directly —
        // total token requests stay at two: exchange + one refresh).
        let mut tasks = Vec::new();
        for _ in 0..2 {
            let manager = Arc::clone(&manager);
            let config = stub.store.clone();
            tasks.push(zeroclaw_spawn::spawn!(async move {
                manager
                    .on_unauthorized(&config, None, "at-1", true)
                    .await
                    .expect("refresh")
            }));
        }
        let mut got = Vec::new();
        for t in tasks {
            got.push(t.await.expect("task"));
        }
        assert!(
            got.iter().all(|b| b == "Bearer at-2"),
            "both callers must end with the rotated token: {got:?}"
        );
        let requests = stub.as_.received_requests().await.expect("received");
        let token_calls = requests.iter().filter(|r| r.url.path() == "/token").count();
        assert_eq!(
            token_calls, 2,
            "exchange + exactly one refresh: {token_calls}"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn invalid_grant_on_refresh_clears_grant_and_background_fails_actionably() {
        let stub = stub(
            Some(serde_json::json!({"client_id": "dyn-cid"})),
            vec![
                (
                    200,
                    serde_json::json!({"access_token": "at-1", "refresh_token": "rt-1", "expires_in": 3600, "token_type": "Bearer"}),
                ),
                (400, serde_json::json!({"error": "invalid_grant"})),
            ],
        )
        .await;
        let manager = manager_with(
            &stub,
            Arc::new(AutoCallbackLauncher {
                code: "c",
                fail: false,
                launches: AtomicUsize::new(0),
            }),
        );
        manager
            .bearer_for(&stub.store, None, true)
            .await
            .expect("flow");
        let err = manager
            .on_unauthorized(&stub.store, None, "at-1", false)
            .await
            .expect_err("invalid_grant must fail in background");
        let structured = err
            .downcast_ref::<TokenEndpointError>()
            .expect("structured refresh error survives into the 401 error chain");
        assert_eq!(structured.kind, TokenErrorKind::ReauthorizationRequired);
        // Grant was cleared from the live state and the store.
        let canonical = CanonicalResource::from_configured(
            &Url::parse(&format!("{}/mcp", stub.mcp.uri())).expect("url"),
        )
        .expect("canonical");
        assert!(
            manager
                .store
                .load_authorization(&canonical)
                .await
                .expect("load")
                .is_none(),
            "invalid_grant must clear the stored grant"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn token_endpoint_5xx_never_clears_the_grant() {
        let stub = stub(
            Some(serde_json::json!({"client_id": "dyn-cid"})),
            vec![
                (
                    200,
                    serde_json::json!({"access_token": "at-1", "refresh_token": "rt-1", "expires_in": 3600, "token_type": "Bearer"}),
                ),
                // Even carrying invalid_grant, a 5xx is transient per design §12.
                (503, serde_json::json!({"error": "invalid_grant"})),
            ],
        )
        .await;
        let manager = manager_with(
            &stub,
            Arc::new(AutoCallbackLauncher {
                code: "c",
                fail: false,
                launches: AtomicUsize::new(0),
            }),
        );
        manager
            .bearer_for(&stub.store, None, true)
            .await
            .expect("flow");
        let err = manager
            .on_unauthorized(&stub.store, None, "at-1", false)
            .await
            .expect_err("5xx must surface as an error");
        assert!(err.to_string().contains("HTTP 503"), "got: {err:#}");
        let canonical = CanonicalResource::from_configured(
            &Url::parse(&format!("{}/mcp", stub.mcp.uri())).expect("url"),
        )
        .expect("canonical");
        assert!(
            manager
                .store
                .load_authorization(&canonical)
                .await
                .expect("load")
                .is_some(),
            "a 5xx must never clear the stored grant"
        );
    }

    #[test]
    fn authorization_url_carries_every_rfc_param() {
        let discovery = OAuthDiscovery {
            canonical: CanonicalResource::from_configured(&Url::parse("https://h/mcp").expect("u"))
                .expect("c"),
            protected_resource_url: Url::parse(
                "https://h/.well-known/oauth-protected-resource/mcp",
            )
            .expect("u"),
            auth_server: super::super::metadata::ValidatedAs {
                issuer: Url::parse("https://a.example").expect("u"),
                authorization_endpoint: Url::parse("https://a.example/authorize").expect("u"),
                token_endpoint: Url::parse("https://a.example/token").expect("u"),
                registration_endpoint: None,
                scopes_supported: None,
                require_iss: false,
                advertises_refresh_token: true,
            },
        };
        let url = build_authorization_url(
            &discovery,
            &CanonicalResource::from_configured(&Url::parse("https://h/mcp").expect("u"))
                .expect("c"),
            "cid",
            "http://127.0.0.1:9/callback",
            &Pkce::from_verifier("v"),
            "st-1",
            Some(&["a".to_string(), "b".to_string()]),
        )
        .expect("built");
        let pairs: HashMap<_, _> = url.query_pairs().collect();
        assert_eq!(pairs.get("response_type").map(|s| s.as_ref()), Some("code"));
        assert_eq!(pairs.get("client_id").map(|s| s.as_ref()), Some("cid"));
        assert_eq!(
            pairs.get("redirect_uri").map(|s| s.as_ref()),
            Some("http://127.0.0.1:9/callback")
        );
        assert_eq!(pairs.get("state").map(|s| s.as_ref()), Some("st-1"));
        assert_eq!(
            pairs.get("code_challenge_method").map(|s| s.as_ref()),
            Some("S256")
        );
        assert_eq!(pairs.get("scope").map(|s| s.as_ref()), Some("a b"));
        assert_eq!(
            pairs.get("resource").map(|s| s.as_ref()),
            Some("https://h/mcp")
        );
    }

    // ── Transport integration: bearer injection + 401 semantics ──────────

    /// Wiremock MCP endpoint sequencer: 401s for `denials` first, then
    /// well-formed JSON-RPC successes.
    struct McpEndpoint {
        denials: usize,
        hits: AtomicUsize,
    }

    impl wiremock::Respond for McpEndpoint {
        fn respond(&self, _: &wiremock::Request) -> ResponseTemplate {
            let hit = self.hits.fetch_add(1, Ordering::SeqCst);
            if hit < self.denials {
                ResponseTemplate::new(401).insert_header(
                    "www-authenticate",
                    "Bearer realm=\"mcp\", error=\"invalid_token\"",
                )
            } else {
                ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 7,
                    "result": {},
                }))
            }
        }
    }

    async fn transport_with_live_grant(
        denials: usize,
        refresh_steps: Vec<(u16, serde_json::Value)>,
    ) -> (
        Arc<McpOAuthManager>,
        Stub,
        Arc<crate::mcp_transport::HttpTransport>,
    ) {
        let stub = stub(Some(serde_json::json!({"client_id": "cid"})), refresh_steps).await;
        let responder = McpEndpoint {
            denials,
            hits: AtomicUsize::new(0),
        };
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::path("/mcp"))
            .respond_with(responder)
            .mount(&stub.mcp)
            .await;
        let manager = Arc::new(
            McpOAuthManager::with_launcher(
                stub.tmp.path(),
                true,
                Arc::new(AutoCallbackLauncher {
                    code: "c",
                    fail: false,
                    launches: AtomicUsize::new(0),
                }),
            )
            .expect("manager"),
        );
        manager
            .store
            .save_authorization(
                &CanonicalResource::from_configured(
                    &Url::parse(&format!("{}/mcp", stub.mcp.uri())).expect("url"),
                )
                .expect("canonical"),
                "cid",
                &Url::parse(&stub.as_.uri()).expect("issuer"),
                TokenSet {
                    access_token: "at-live".to_string(),
                    refresh_token: Some("rt-live".to_string()),
                    id_token: None,
                    expires_at: None,
                    token_type: Some("Bearer".to_string()),
                    scope: None,
                },
            )
            .await
            .expect("seed grant");
        let transport = Arc::new(
            crate::mcp_transport::HttpTransport::new_with_oauth(
                &stub.store,
                Some(TransportOAuth::new(
                    Arc::clone(&manager),
                    stub.store.clone(),
                    false,
                )),
            )
            .expect("transport"),
        );
        (manager, stub, transport)
    }

    /// POSTs that reached the MCP endpoint itself (PRM discovery is a GET
    /// on the same mock server and must not count as a request attempt).
    async fn mcp_posts(stub: &Stub) -> usize {
        stub.mcp
            .received_requests()
            .await
            .expect("received")
            .iter()
            .filter(|r| r.method.as_str() == "POST" && r.url.path() == "/mcp")
            .count()
    }

    async fn send(
        transport: &Arc<crate::mcp_transport::HttpTransport>,
        method: &str,
    ) -> Result<crate::mcp_protocol::JsonRpcResponse> {
        let request = crate::mcp_protocol::JsonRpcRequest::new(7, method, serde_json::json!({}));
        let lifecycle = crate::mcp_transport::McpRequestLifecycle::uncoordinated(0);
        crate::mcp_transport::SharedMcpTransportConn::send_and_recv(
            transport.as_ref(),
            &request,
            &lifecycle,
        )
        .await
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn safe_method_401_refreshes_once_and_replays_exactly_once() {
        let (_manager, stub, transport) = transport_with_live_grant(
            1,
            vec![(
                200,
                serde_json::json!({"access_token": "at-2", "refresh_token": "rt-2", "token_type": "Bearer", "expires_in": 3600}),
            )],
        )
        .await;
        let response = send(&transport, "tools/list").await.expect("retried ok");
        assert_eq!(response.id, Some(serde_json::json!(7)));
        let requests = stub.as_.received_requests().await.expect("received");
        let token_calls = requests.iter().filter(|r| r.url.path() == "/token").count();
        assert_eq!(token_calls, 1, "exactly one refresh: {token_calls}");
        let mcp_requests = stub.mcp.received_requests().await.expect("received");
        let authed: Vec<String> = mcp_requests
            .iter()
            .filter_map(|r| {
                r.headers
                    .get("authorization")
                    .map(|v| v.to_str().unwrap_or_default().to_string())
            })
            .collect();
        assert_eq!(
            authed,
            vec!["Bearer at-live".to_string(), "Bearer at-2".to_string()],
            "first attempt stale, replay refreshed, nothing more"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn tools_call_401_is_never_replayed_but_still_refreshes_state() {
        let (_manager, stub, transport) = transport_with_live_grant(
            1,
            vec![(
                200,
                serde_json::json!({"access_token": "at-2", "refresh_token": "rt-2", "token_type": "Bearer", "expires_in": 3600}),
            )],
        )
        .await;
        let err = send(&transport, "tools/call")
            .await
            .expect_err("401 surfaces");
        assert!(err.to_string().contains("HTTP 401"), "got: {err}");
        assert_eq!(
            mcp_posts(&stub).await,
            1,
            "state-changing call must not replay"
        );
        let as_requests = stub.as_.received_requests().await.expect("received");
        assert_eq!(
            as_requests
                .iter()
                .filter(|r| r.url.path() == "/token")
                .count(),
            1,
            "the grant is refreshed for the NEXT call"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn second_consecutive_401_is_terminal() {
        let (_manager, stub, transport) = transport_with_live_grant(
            2,
            vec![(
                200,
                serde_json::json!({"access_token": "at-2", "refresh_token": "rt-2", "token_type": "Bearer", "expires_in": 3600}),
            )],
        )
        .await;
        let err = send(&transport, "tools/list")
            .await
            .expect_err("two 401s terminal");
        assert!(err.to_string().contains("HTTP 401"), "got: {err}");
        assert_eq!(
            mcp_posts(&stub).await,
            2,
            "one refresh + one replay, then stop"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_401_on_the_replayed_request_does_not_refresh_again() {
        // Greptile C10: the refresh budget for one call is exactly ONE
        // token exchange. The replay already carried the freshly rotated
        // token, so a second 401 means the server keeps rejecting tokens
        // this client presents; a second exchange would only delay the
        // terminal error (and burn one-time-use refresh tokens). The call
        // must surface HTTP 401 with exactly one /token POST; the repair
        // for future calls is deferred to the next call's own 401 budget.
        let (_manager, stub, transport) = transport_with_live_grant(
            3,
            vec![(
                200,
                serde_json::json!({"access_token": "at-2", "refresh_token": "rt-2", "token_type": "Bearer", "expires_in": 3600}),
            )],
        )
        .await;
        let err = send(&transport, "tools/list")
            .await
            .expect_err("replay also denied");
        assert!(err.to_string().contains("HTTP 401"), "got: {err}");
        assert_eq!(
            mcp_posts(&stub).await,
            2,
            "first attempt + replay, then stop"
        );
        let as_requests = stub.as_.received_requests().await.expect("received");
        assert_eq!(
            as_requests
                .iter()
                .filter(|r| r.url.path() == "/token")
                .count(),
            1,
            "one 401 budget = one token exchange per call"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn an_expired_grant_without_a_refresh_token_is_dropped_before_the_send() {
        // Greptile C11: the proactive path used to hand back `None` for an
        // expiring grant that has no refresh token, so the call went out
        // with an expired bearer and the repair only happened via a 401
        // mid-call. The dead copy must be dropped from the live state on
        // the spot, so the same call falls into the sign-in decision
        // (actionable error in background posture) instead of shipping a
        // known-dead token. The stored profile goes with it: an expired
        // token with no refresh token cannot progress without a browser
        // sign-in that rewrites the profile anyway, and keeping it would
        // let the next call's `ready` resurrect it from disk — per-
        // request decrypt plus WARN churn where the 401 repair cleaned
        // once (reviewer adversarial review).
        let stub = stub(Some(serde_json::json!({"client_id": "cid"})), vec![]).await;
        let manager = manager_with(&stub, Arc::new(NeverLaunch));
        let canonical = CanonicalResource::from_configured(
            &Url::parse(stub.store.url.as_deref().expect("url")).expect("canonical"),
        )
        .expect("canonical");
        manager
            .seed_authorization(
                &canonical,
                "cid",
                &Url::parse(&stub.as_.uri()).expect("issuer"),
                TokenSet {
                    access_token: "at-dead".to_string(),
                    refresh_token: None,
                    id_token: None,
                    expires_at: Some(chrono::Utc::now() + chrono::Duration::seconds(3)),
                    token_type: Some("Bearer".to_string()),
                    scope: None,
                },
            )
            .await
            .expect("seed grant");

        let err = manager
            .bearer_for(&stub.store, None, false)
            .await
            .expect_err("a dead grant must not hand out an expired bearer");
        let text = err.to_string();
        assert!(
            text.contains("interactive ZeroClaw") || text.contains("sign in again"),
            "expected an actionable reauthorization error, got: {text}"
        );
        let state = manager.state_for(&stub.store).expect("state");
        assert!(
            state.grant.read().await.is_none(),
            "the live dead grant must be dropped in place, not sent expired"
        );
        assert!(
            manager
                .store
                .load_authorization(&canonical)
                .await
                .expect("store read")
                .is_none(),
            "the dead profile must not linger on disk to be resurrected by the next call"
        );
        let as_requests = stub.as_.received_requests().await.expect("received");
        assert!(
            as_requests.iter().all(|r| r.url.path() != "/token"),
            "no refresh was even attempted: there is no refresh token"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_terminally_rejected_proactive_refresh_signs_in_before_the_send() {
        // greptile C14: with a stored grant whose refresh token the AS
        // rejects with `invalid_grant`, the proactive path used to warn and
        // hand out the carried token anyway, so `prepare_auth` "succeeded"
        // and the required sign-in only began on the resulting 401 — inside
        // the 30 s handshake budget instead of the five-minute sign-in
        // window. A terminal rejection disproves the grant, so it must be
        // retired (live copy plus the matching stored profile) and the same
        // call must fall into the sign-in decision.
        let stub = stub(
            Some(serde_json::json!({"client_id": "cid"})),
            vec![(400, serde_json::json!({"error": "invalid_grant"}))],
        )
        .await;
        let manager = manager_with(&stub, Arc::new(NeverLaunch));
        let canonical = CanonicalResource::from_configured(
            &Url::parse(stub.store.url.as_deref().expect("url")).expect("canonical"),
        )
        .expect("canonical");
        manager
            .seed_authorization(
                &canonical,
                "cid",
                &Url::parse(&stub.as_.uri()).expect("issuer"),
                TokenSet {
                    access_token: "at-spent".to_string(),
                    refresh_token: Some("rt-spent".to_string()),
                    id_token: None,
                    expires_at: Some(chrono::Utc::now() + chrono::Duration::seconds(30)),
                    token_type: Some("Bearer".to_string()),
                    scope: None,
                },
            )
            .await
            .expect("seed grant");

        let err = manager
            .bearer_for(&stub.store, None, false)
            .await
            .expect_err("a terminally rejected grant must not hand out the carried bearer");
        assert!(
            err.to_string().contains("interactive ZeroClaw"),
            "the call must land in the sign-in decision, got: {err}"
        );

        let state = manager.state_for(&stub.store).expect("state");
        assert!(
            state.grant.read().await.is_none(),
            "the live spent grant must be retired on a terminal rejection"
        );
        assert!(
            manager
                .store
                .load_authorization(&canonical)
                .await
                .expect("load")
                .is_none(),
            "the matching stored profile must go with it"
        );

        let requests = stub.as_.received_requests().await.expect("received");
        assert_eq!(
            requests.iter().filter(|r| r.url.path() == "/token").count(),
            1,
            "exactly one rejected refresh attempt; the sign-in decision makes no token call"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_replacement_grant_landing_mid_refresh_wins_the_commit_and_the_call() {
        // greptile pass-16 P1, the mirror of the terminal-verdict case: a
        // refresh that SUCCEEDS used to install its tokens in the live slot
        // unconditionally. A second connection neither holds nor waits for
        // the single-flight refresh lock, so its completed sign-in can own
        // the live slot and the stored profile while our token round-trip is
        // in flight. Installing the displaced grant's rotation then sent this
        // process back to the sign-in that just lost, and the waiting call
        // carried a bearer whose lineage had been superseded. The commit must
        // compare the grant it refreshed against the live slot under the same
        // write lock, and step aside when another sign-in won.
        let stub = stub_with(
            Some(serde_json::json!({"client_id": "cid"})),
            vec![(
                200,
                serde_json::json!({
                    "access_token": "at-fresh",
                    "token_type": "Bearer",
                    "expires_in": 3600,
                    "refresh_token": "rt-fresh",
                }),
            )],
            // The answer arrives 800 ms after the exchange sent: the window
            // the replacement lands in, sized to survive a loaded harness.
            vec![800],
        )
        .await;
        let manager = Arc::new(manager_with(&stub, Arc::new(NeverLaunch)));
        let canonical = CanonicalResource::from_configured(
            &Url::parse(stub.store.url.as_deref().expect("url")).expect("canonical"),
        )
        .expect("canonical");
        let issuer = Url::parse(&stub.as_.uri()).expect("issuer");
        manager
            .seed_authorization(
                &canonical,
                "cid",
                &issuer,
                TokenSet {
                    access_token: "at-spent".to_string(),
                    refresh_token: Some("rt-spent".to_string()),
                    id_token: None,
                    expires_at: Some(chrono::Utc::now() + chrono::Duration::seconds(30)),
                    token_type: Some("Bearer".to_string()),
                    scope: None,
                },
            )
            .await
            .expect("seed grant");
        let state = manager.state_for(&stub.store).expect("state");
        manager
            .ready(&state, None)
            .await
            .expect("ready loads the seed grant");

        // Borrowed, not moved: `join!` awaits this in the same scope, and the
        // authorization-server handle is not cloneable.
        let injector = async {
            // Deterministic handoff: the authorization server records an
            // incoming request before it starts the response delay, so a
            // recorded `/token` here means the exchange is in flight with
            // its answer still pending.
            let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
            loop {
                let in_flight = stub
                    .as_
                    .received_requests()
                    .await
                    .expect("received requests")
                    .iter()
                    .any(|request| request.url.path() == "/token");
                if in_flight {
                    break;
                }
                assert!(
                    tokio::time::Instant::now() < deadline,
                    "the proactive refresh never reached the authorization server"
                );
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
            // The premise, checked where it matters: the live slot still
            // carries the token this exchange refreshed, which is only true
            // while the commit has not run.
            let live_before = state
                .grant
                .read()
                .await
                .as_ref()
                .map(|grant| grant.tokens.access_token.clone());
            assert_eq!(
                live_before.as_deref(),
                Some("at-spent"),
                "the replacement landed outside the flight window, so the race was never exercised"
            );
            // A second connection completed its own sign-in: store and live
            // slot both hold a credential this process never minted.
            let replacement = TokenSet {
                access_token: "at-new".to_string(),
                refresh_token: Some("rt-new".to_string()),
                id_token: None,
                expires_at: Some(chrono::Utc::now() + chrono::Duration::hours(1)),
                token_type: Some("Bearer".to_string()),
                scope: None,
            };
            manager
                .seed_authorization(&canonical, "cid", &issuer, replacement.clone())
                .await
                .expect("seed the replacement grant");
            *state.grant.write().await = Some(Grant {
                tokens: replacement,
                client_id: "cid".to_string(),
                issuer,
                minted_at: aged_mint(),
            });
            tokio::time::Instant::now()
        };
        let attempt = async {
            let outcome = manager.refresh_if_expiring(&state).await;
            (outcome, tokio::time::Instant::now())
        };
        let ((outcome, attempt_at), injected_at) = tokio::join!(attempt, injector);
        // The premise of this test IS the interleaving: if the replacement
        // landed only after the exchange resolved, the commit never saw it
        // and a green run would mean nothing. Fail loudly instead.
        let Some(margin) = attempt_at.checked_duration_since(injected_at) else {
            panic!(
                "the replacement grant landed only after the exchange resolved: the race window was never exercised"
            );
        };

        let outcome = outcome.expect("the proactive refresh must resolve, not fail");
        let requests = stub.as_.received_requests().await.expect("received");
        let token_calls = requests.iter().filter(|r| r.url.path() == "/token").count();
        let live_token = state
            .grant
            .read()
            .await
            .as_ref()
            .map(|grant| grant.tokens.access_token.clone());
        let stored = manager
            .store
            .load_authorization(&canonical)
            .await
            .expect("load");
        let stored_token = stored
            .as_ref()
            .map(|grant| grant.tokens.access_token.clone());
        let context = format!(
            "outcome={outcome:?} live={live_token:?} stored={stored_token:?} token_calls={token_calls} window={margin:?}"
        );
        assert_eq!(
            outcome.as_deref(),
            Some("at-new"),
            "the waiting call must carry the sign-in that won, not the superseded rotation: {context}"
        );
        assert_eq!(
            live_token.as_deref(),
            Some("at-new"),
            "a rotation of the displaced grant must not reinstall the older sign-in: {context}"
        );
        let Some(stored) = stored else {
            panic!("the replacement sign-in must survive a superseded rotation: {context}");
        };
        assert_eq!(
            stored.tokens.access_token, "at-new",
            "the profile the winning sign-in committed must not be rewritten by our rotation: {context}"
        );
        assert_eq!(
            token_calls, 1,
            "exactly one refresh attempt; the winner needs no second exchange: {context}"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_replacement_grant_landing_mid_refresh_survives_the_terminal_verdict() {
        // greptile pass-15 P1: the proactive path used to ask the live slot
        // which token to retire AFTER the rejected exchange returned, and
        // `clear_spent_grant` wiped the live slot unconditionally. A second
        // connection does not hold the single-flight refresh lock, so it can
        // move the live slot AND the stored profile to a different, valid
        // grant while our request is in flight. The old reading then declared
        // THAT token spent and deleted a replacement sign-in a daemon cannot
        // re-mint unattended. The verdict must carry the identity this
        // exchange actually sent, captured under the triple-check lock.
        let stub = stub_with(
            Some(serde_json::json!({"client_id": "cid"})),
            vec![(400, serde_json::json!({"error": "invalid_grant"}))],
            // The rejection answer arrives 800 ms after the exchange sent:
            // the window the replacement lands in, sized to survive a loaded
            // test harness.
            vec![800],
        )
        .await;
        let manager = Arc::new(manager_with(&stub, Arc::new(NeverLaunch)));
        let canonical = CanonicalResource::from_configured(
            &Url::parse(stub.store.url.as_deref().expect("url")).expect("canonical"),
        )
        .expect("canonical");
        let issuer = Url::parse(&stub.as_.uri()).expect("issuer");
        manager
            .seed_authorization(
                &canonical,
                "cid",
                &issuer,
                TokenSet {
                    access_token: "at-spent".to_string(),
                    refresh_token: Some("rt-spent".to_string()),
                    id_token: None,
                    expires_at: Some(chrono::Utc::now() + chrono::Duration::seconds(30)),
                    token_type: Some("Bearer".to_string()),
                    scope: None,
                },
            )
            .await
            .expect("seed grant");
        let state = manager.state_for(&stub.store).expect("state");
        // Pre-warm discovery and let `ready` load the seed into the live slot,
        // then drive the proactive refresh itself: its answer, verdict and
        // retirement all happen inside that one call, so the moment it returns
        // is the moment the verdict landed.
        manager
            .ready(&state, None)
            .await
            .expect("ready loads the seed grant");

        // Borrowed, not moved: `join!` awaits this in the same scope, and the
        // authorization-server handle is not cloneable.
        let injector = async {
            // Deterministic handoff: the authorization server records an
            // incoming request before it starts the response delay, so a
            // recorded `/token` here means the exchange is in flight with
            // its answer still pending.
            let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
            loop {
                let in_flight = stub
                    .as_
                    .received_requests()
                    .await
                    .expect("received requests")
                    .iter()
                    .any(|request| request.url.path() == "/token");
                if in_flight {
                    break;
                }
                assert!(
                    tokio::time::Instant::now() < deadline,
                    "the proactive refresh never reached the authorization server"
                );
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
            // The premise, checked where it matters: the live slot still
            // carries the spent token, which is only true while the verdict
            // has not run (a retired grant leaves the slot empty).
            let live_before = state
                .grant
                .read()
                .await
                .as_ref()
                .map(|grant| grant.tokens.access_token.clone());
            assert_eq!(
                live_before.as_deref(),
                Some("at-spent"),
                "the replacement landed outside the flight window, so the race was never exercised"
            );
            // A second connection is not under the single-flight refresh
            // lock, and it does not have to be: its connect adopted the
            // operator's saved grant, so the stored profile and the live
            // slot have both moved to a token this process never sent.
            let replacement = TokenSet {
                access_token: "at-new".to_string(),
                refresh_token: Some("rt-new".to_string()),
                id_token: None,
                expires_at: Some(chrono::Utc::now() + chrono::Duration::hours(1)),
                token_type: Some("Bearer".to_string()),
                scope: None,
            };
            manager
                .seed_authorization(&canonical, "cid", &issuer, replacement.clone())
                .await
                .expect("seed the replacement grant");
            *state.grant.write().await = Some(Grant {
                tokens: replacement,
                client_id: "cid".to_string(),
                issuer,
                minted_at: aged_mint(),
            });
            tokio::time::Instant::now()
        };
        let attempt = async {
            let outcome = manager.refresh_if_expiring(&state).await;
            (outcome, tokio::time::Instant::now())
        };
        let ((outcome, attempt_at), injected_at) = tokio::join!(attempt, injector);
        // The premise of this test IS the interleaving: if the replacement
        // landed only after the exchange resolved, the verdict never saw it
        // and a green run would mean nothing. Fail loudly instead.
        let Some(margin) = attempt_at.checked_duration_since(injected_at) else {
            panic!(
                "the replacement grant landed only after the exchange resolved: the race window was never exercised"
            );
        };

        let outcome = outcome.expect("the proactive refresh must resolve, not fail");
        assert!(
            outcome.is_none(),
            "a terminally rejected refresh must retire the grant, not hand out a token: {outcome:?}"
        );

        let requests = stub.as_.received_requests().await.expect("received");
        let token_calls = requests.iter().filter(|r| r.url.path() == "/token").count();
        let live_token = state
            .grant
            .read()
            .await
            .as_ref()
            .map(|grant| grant.tokens.access_token.clone());
        let stored = manager
            .store
            .load_authorization(&canonical)
            .await
            .expect("load");
        let stored_token = stored
            .as_ref()
            .map(|grant| grant.tokens.access_token.clone());
        let context = format!(
            "outcome={outcome:?} live={live_token:?} stored={stored_token:?} token_calls={token_calls} window={margin:?}"
        );
        let Some(stored) = stored else {
            panic!(
                "the replacement sign-in must survive a verdict about a different token: {context}"
            );
        };
        assert_eq!(
            stored.tokens.access_token, "at-new",
            "the profile a second connection committed must not be deleted by our verdict: {context}"
        );
        assert_eq!(
            live_token.as_deref(),
            Some("at-new"),
            "the live slot must keep the replacement, not be wiped by our verdict: {context}"
        );
        assert_eq!(
            token_calls, 1,
            "exactly one rejected refresh attempt; the replacement needs no second exchange: {context}"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_terminally_rejected_proactive_refresh_re_signs_in_interactively() {
        // The interactive half of the same finding: the sign-in must run
        // during `prepare_auth`, so a slow human keeps the five-minute
        // listener window instead of losing it to a mid-handshake 401.
        let stub = stub(
            Some(serde_json::json!({"client_id": "cid"})),
            vec![
                (400, serde_json::json!({"error": "invalid_grant"})),
                (
                    200,
                    serde_json::json!({"access_token": "at-fresh", "refresh_token": "rt-fresh", "expires_in": 3600, "token_type": "Bearer"}),
                ),
            ],
        )
        .await;
        let launcher = Arc::new(AutoCallbackLauncher {
            code: "rc-code",
            fail: false,
            launches: AtomicUsize::new(0),
        });
        let manager = manager_with(&stub, Arc::clone(&launcher) as Arc<dyn BrowserLauncher>);
        let canonical = CanonicalResource::from_configured(
            &Url::parse(stub.store.url.as_deref().expect("url")).expect("canonical"),
        )
        .expect("canonical");
        manager
            .seed_authorization(
                &canonical,
                "cid",
                &Url::parse(&stub.as_.uri()).expect("issuer"),
                TokenSet {
                    access_token: "at-stale".to_string(),
                    refresh_token: Some("rt-stale".to_string()),
                    id_token: None,
                    expires_at: Some(chrono::Utc::now() + chrono::Duration::seconds(30)),
                    token_type: Some("Bearer".to_string()),
                    scope: None,
                },
            )
            .await
            .expect("seed grant");

        let bearer = manager
            .bearer_for(&stub.store, None, true)
            .await
            .expect("the terminal rejection must lead into a fresh sign-in");
        assert_eq!(bearer, "Bearer at-fresh");
        assert_eq!(
            launcher.launches.load(Ordering::SeqCst),
            1,
            "the retired grant must open exactly one browser sign-in"
        );
        let loaded = manager
            .store
            .load_authorization(&canonical)
            .await
            .expect("load")
            .expect("re-minted profile");
        assert_eq!(loaded.tokens.access_token, "at-fresh");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_transient_proactive_refresh_failure_still_hands_out_the_token() {
        // The other half of the finding: a 5xx that happens to carry
        // `invalid_grant` disproves nothing (design §12), so the grant
        // stands and the carried token is still handed out — no sign-in,
        // no store removal, and the 401 path keeps its full-information
        // reaction.
        let stub = stub(
            Some(serde_json::json!({"client_id": "cid"})),
            vec![(503, serde_json::json!({"error": "invalid_grant"}))],
        )
        .await;
        let manager = manager_with(&stub, Arc::new(NeverLaunch));
        let canonical = CanonicalResource::from_configured(
            &Url::parse(stub.store.url.as_deref().expect("url")).expect("canonical"),
        )
        .expect("canonical");
        manager
            .seed_authorization(
                &canonical,
                "cid",
                &Url::parse(&stub.as_.uri()).expect("issuer"),
                TokenSet {
                    access_token: "at-live".to_string(),
                    refresh_token: Some("rt-live".to_string()),
                    id_token: None,
                    expires_at: Some(chrono::Utc::now() + chrono::Duration::seconds(30)),
                    token_type: Some("Bearer".to_string()),
                    scope: None,
                },
            )
            .await
            .expect("seed grant");

        let bearer = manager
            .bearer_for(&stub.store, None, false)
            .await
            .expect("a transient failure must not force a sign-in");
        assert_eq!(bearer, "Bearer at-live");

        let state = manager.state_for(&stub.store).expect("state");
        assert!(
            state.grant.read().await.is_some(),
            "a transient failure must keep the live grant"
        );
        let stored = manager
            .store
            .load_authorization(&canonical)
            .await
            .expect("load")
            .expect("stored profile kept");
        assert_eq!(stored.tokens.access_token, "at-live");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_short_lived_rotated_token_is_used_not_redeemed_on_every_request() {
        // Greptile pass-20: the fixed refresh skew exceeds a short lifetime
        // outright, so treating "within the skew" as "expiring soon" makes
        // every bearer lookup - prepare, every handshake request - redeem a
        // new exchange and write the whole credential file, because the
        // just-minted token already sits inside the skew. A 30-second token
        // must be handed to the request until it actually expires.
        let stub = stub(
            Some(serde_json::json!({"client_id": "cid"})),
            vec![(
                200,
                serde_json::json!({
                    "access_token": "at-1",
                    "refresh_token": "rt-1",
                    "expires_in": 30,
                    "token_type": "Bearer",
                }),
            )],
        )
        .await;
        let manager = manager_with(&stub, Arc::new(NeverLaunch));
        let canonical = CanonicalResource::from_configured(
            &Url::parse(stub.store.url.as_deref().expect("url")).expect("canonical"),
        )
        .expect("canonical");
        manager
            .seed_authorization(
                &canonical,
                "cid",
                &Url::parse(&stub.as_.uri()).expect("issuer"),
                TokenSet {
                    access_token: "at-old".to_string(),
                    refresh_token: Some("rt-old".to_string()),
                    id_token: None,
                    expires_at: Some(chrono::Utc::now() + chrono::Duration::seconds(30)),
                    token_type: Some("Bearer".to_string()),
                    scope: None,
                },
            )
            .await
            .expect("seed grant");

        let first = manager
            .bearer_for(&stub.store, None, false)
            .await
            .expect("first bearer");
        assert_eq!(first, "Bearer at-1", "the aged grant refreshes");
        let second = manager
            .bearer_for(&stub.store, None, false)
            .await
            .expect("second bearer");
        assert_eq!(
            second, "Bearer at-1",
            "the just-rotated short-lived token must be reused, not re-exchanged"
        );

        let requests = stub.as_.received_requests().await.expect("received");
        assert_eq!(
            requests.iter().filter(|r| r.url.path() == "/token").count(),
            1,
            "exactly one proactive refresh, no per-request token exchange"
        );
        assert_eq!(
            requests
                .iter()
                .filter(|r| r.url.path() == "/register")
                .count(),
            0,
            "no sign-in was started for a perfectly usable token"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_proactive_rejection_of_the_configured_client_id_keeps_the_grant() {
        // Design §12's configured-id downgrade, mirrored in the proactive
        // path: when the client the token endpoint just rejected IS the
        // operator's `oauth_client_id`, a new sign-in would reproduce the
        // same rejection, so the grant must stand, the carried token must
        // still go out, and no browser may open — the operator belongs in
        // the configuration, not in a per-call sign-in loop.
        let stub = stub(
            None,
            vec![(400, serde_json::json!({"error": "invalid_client"}))],
        )
        .await;
        let manager = manager_with(&stub, Arc::new(NeverLaunch));
        let config = McpServerConfig {
            oauth_client_id: Some("cfg-cid".to_string()),
            ..stub.store.clone()
        };
        let canonical = CanonicalResource::from_configured(
            &Url::parse(config.url.as_deref().expect("url")).expect("canonical"),
        )
        .expect("canonical");
        manager
            .seed_authorization(
                &canonical,
                "cfg-cid",
                &Url::parse(&stub.as_.uri()).expect("issuer"),
                TokenSet {
                    access_token: "at-old".to_string(),
                    refresh_token: Some("rt-old".to_string()),
                    id_token: None,
                    expires_at: Some(chrono::Utc::now() + chrono::Duration::seconds(30)),
                    token_type: Some("Bearer".to_string()),
                    scope: None,
                },
            )
            .await
            .expect("seed grant");

        let bearer = manager
            .bearer_for(&config, None, false)
            .await
            .expect("a configured-id rejection must not force a sign-in");
        assert_eq!(bearer, "Bearer at-old");

        let state = manager.state_for(&config).expect("state");
        assert!(
            state.grant.read().await.is_some(),
            "a configured-id rejection must keep the grant standing"
        );
        let stored = manager
            .store
            .load_authorization(&canonical)
            .await
            .expect("load")
            .expect("stored profile kept");
        assert_eq!(stored.tokens.access_token, "at-old");

        let requests = stub.as_.received_requests().await.expect("received");
        assert_eq!(
            requests.iter().filter(|r| r.url.path() == "/token").count(),
            1,
            "one rejected refresh, no retry, no second exchange"
        );
        assert_eq!(
            requests
                .iter()
                .filter(|r| r.url.path() == "/register")
                .count(),
            0,
            "a configured client id never re-registers, and no sign-in was started"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn an_expiring_grant_without_a_refresh_token_stays_usable_until_expiry() {
        // Greptile C13: the 60 s refresh skew is not an expiry. A grant
        // that cannot be refreshed must keep riding its still-valid
        // access token until the actual expiry edge; deleting it inside
        // the skew forced a fresh browser sign-in on every handshake
        // message (and a hard rejection in background posture) even
        // though the credential was good for another 30 seconds.
        let stub = stub(Some(serde_json::json!({"client_id": "cid"})), vec![]).await;
        let manager = manager_with(&stub, Arc::new(NeverLaunch));
        let canonical = CanonicalResource::from_configured(
            &Url::parse(stub.store.url.as_deref().expect("url")).expect("canonical"),
        )
        .expect("canonical");
        manager
            .seed_authorization(
                &canonical,
                "cid",
                &Url::parse(&stub.as_.uri()).expect("issuer"),
                TokenSet {
                    access_token: "at-soon".to_string(),
                    refresh_token: None,
                    id_token: None,
                    expires_at: Some(chrono::Utc::now() + chrono::Duration::seconds(30)),
                    token_type: Some("Bearer".to_string()),
                    scope: None,
                },
            )
            .await
            .expect("seed grant");

        let bearer = manager
            .bearer_for(&stub.store, None, false)
            .await
            .expect("30 seconds of real life outlives the 60 s refresh skew");
        assert_eq!(bearer, "Bearer at-soon");
        let state = manager.state_for(&stub.store).expect("state");
        assert!(
            state.grant.read().await.is_some(),
            "the usable credential must not be deleted inside the refresh skew"
        );
        assert!(
            manager
                .store
                .load_authorization(&canonical)
                .await
                .expect("store readable")
                .is_some(),
            "the still-usable stored profile must survive too (the C11 pin covers \
             the removal side; this pins the retention side end to end)"
        );
        let as_requests = stub.as_.received_requests().await.expect("received");
        assert!(
            as_requests.iter().all(|r| r.url.path() != "/token"),
            "nothing to renew: no refresh token, no token POST"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_forced_refresh_past_the_call_wall_detaches_and_still_commits() {
        // Greptile C12: the caller's wall (tool timeout) can lapse while
        // the token exchange is in flight. Dropping the exchange together
        // with the caller would discard the commit of an already-redeemed
        // (often one-time-use) refresh token and strand a spent token in
        // memory AND on disk - the next call then gets `invalid_grant` and
        // a daemon cannot sign in unattended. The exchange must keep
        // running in its own task and commit; the caller returns quickly
        // with a retry-able error.
        let stub = stub_with(
            None,
            vec![(
                200,
                serde_json::json!({"access_token": "at-2", "refresh_token": "rt-2", "token_type": "Bearer", "expires_in": 3600}),
            )],
            // Step 0 delay: the 200 ms exchange window the caller must not
            // keep paying for.
            vec![200],
        )
        .await;
        let manager = manager_with(&stub, Arc::new(NeverLaunch));
        let canonical = CanonicalResource::from_configured(
            &Url::parse(stub.store.url.as_deref().expect("url")).expect("canonical"),
        )
        .expect("canonical");
        manager
            .seed_authorization(
                &canonical,
                "cid",
                &Url::parse(&stub.as_.uri()).expect("issuer"),
                TokenSet {
                    access_token: "at-live".to_string(),
                    refresh_token: Some("rt-live".to_string()),
                    id_token: None,
                    expires_at: None,
                    token_type: Some("Bearer".to_string()),
                    scope: None,
                },
            )
            .await
            .expect("seed grant");
        // Force the detach window: the exchange is delayed 200 ms while
        // the recorded wall already sits deep inside the grace
        // (REFRESH_DETACH_GRACE minus 100 ms), so the wall+grace join
        // fires in tens of milliseconds. The margin absorbs scheduler
        // jitter under full-suite load. Against the OLD inline-await
        // behavior this test still fails twice over: the caller had to
        // wait the full 200 ms exchange, so `expect_err` below saw an
        // `Ok(Bearer)` (the exchange never outlives the caller), and
        // elapsed would sit inside the 350 ms cap by design - the
        // message assertion on the detached retry error is the primary
        // discriminator (adversarial review).
        let state = manager.state_for(&stub.store).expect("state");
        // Pre-warm the discovery cache so `ready` cannot park on a store
        // load / discovery fetch before the single-flight section (that
        // would burn the detach window before the exchange even
        // starts).
        manager
            .ready(&state, None)
            .await
            .expect("discovery pre-warmed");
        *state.deadline.lock() = Arc::new(TestWall(
            std::time::Instant::now() - Duration::from_millis(29_900),
        ));
        let started = std::time::Instant::now();
        let err = manager
            .on_unauthorized(&stub.store, None, "Bearer at-live", false)
            .await
            .expect_err("the caller stops paying past its wall");
        let elapsed = started.elapsed();
        let text = err.to_string();
        assert!(
            text.contains("keeps running in the background") && text.contains("retry the call"),
            "expected a retry-able background error, got: {text}"
        );
        assert!(
            elapsed < Duration::from_millis(350),
            "the caller must not wait out the 200 ms exchange: {elapsed:?}"
        );

        // The deferred task keeps its promise: it lands shortly after and
        // commits live (and persisted), with exactly one token POST.
        let committed = tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                {
                    let grant = state.grant.read().await;
                    if grant
                        .as_ref()
                        .is_some_and(|grant| grant.tokens.access_token == "at-2")
                    {
                        break true;
                    }
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await;
        assert!(
            matches!(committed, Ok(true)),
            "the deferred refresh must still commit: {committed:?}"
        );
        let state = manager.state_for(&stub.store).expect("state");
        {
            let grant = state.grant.read().await;
            let Some(grant) = grant.as_ref() else {
                unreachable!("the deferred commit must have installed the new grant");
            };
            assert_eq!(grant.tokens.access_token, "at-2");
            assert_eq!(
                grant.tokens.refresh_token.as_deref(),
                Some("rt-2"),
                "the rotated refresh token is what gets kept"
            );
        }
        let as_requests = stub.as_.received_requests().await.expect("received");
        assert_eq!(
            as_requests
                .iter()
                .filter(|r| r.url.path() == "/token")
                .count(),
            1,
            "the detached exchange runs exactly once"
        );
    }

    /// Test wall sitting at a fixed instant; the `None` (seconds-agnostic)
    /// branch of `state_for_with_clock` keeps it across `state_for` calls.
    struct TestWall(std::time::Instant);

    impl CallerDeadline for TestWall {
        fn as_instant(&self) -> std::time::Instant {
            self.0
        }

        fn as_secs(&self) -> Option<u64> {
            None
        }
    }

    // Reviewer-round-6 P1 pin: the production deadline is per-CALL
    // relative, not a construction-time snapshot. A stored per-resource
    // clock that froze `now + timeout` at first touch detaches every
    // C12 join once that stale wall elapses - which is every join in a
    // daemon older than the tool timeout.
    #[test]
    fn a_system_deadline_advances_with_the_clock_instead_of_freezing() {
        use std::time::Instant;
        let clock = SystemDeadlineWithTimeout::new(Some(1));
        assert_eq!(CallerDeadline::as_secs(&clock), Some(1));
        let first = clock.as_instant();
        std::thread::sleep(Duration::from_millis(60));
        let second = clock.as_instant();
        // Each use resolves `now + 1 s`: the two walls move with the
        // clock, and neither has passed yet.
        assert!(
            second > first + Duration::from_millis(50),
            "the wall must be call-relative: {first:?} then {second:?}"
        );
        assert!(second > Instant::now());
        // Mirrors `mcp_client`'s own resolution: default and cap.
        assert_eq!(
            CallerDeadline::as_secs(&SystemDeadlineWithTimeout::new(None)),
            Some(crate::mcp_client::DEFAULT_TOOL_TIMEOUT_SECS)
        );
        assert_eq!(
            CallerDeadline::as_secs(&SystemDeadlineWithTimeout::new(Some(10_000))),
            Some(crate::mcp_client::MAX_TOOL_TIMEOUT_SECS)
        );
    }

    // Reviewer-round-6 P1 companion: reload re-clamps a live state's
    // budget, and an injected seconds-agnostic clock survives reloads.
    #[test]
    fn a_reloaded_timeout_re_clamps_the_stored_clock() {
        let tmp = TempDir::new().expect("tmp");
        let manager = McpOAuthManager::with_launcher(tmp.path(), true, Arc::new(NeverLaunch))
            .expect("manager");
        let config = McpServerConfig {
            name: "clamp".to_string(),
            transport: zeroclaw_config::schema::McpTransport::Http,
            url: Some("http://127.0.0.1:10/mcp".to_string()),
            oauth: true,
            tool_timeout_secs: Some(30),
            ..Default::default()
        };
        let state = manager.state_for(&config).expect("state");
        assert_eq!(state.deadline.lock().as_secs(), Some(30));
        let config = McpServerConfig {
            tool_timeout_secs: Some(45),
            ..config.clone()
        };
        let again = manager.state_for(&config).expect("state");
        assert!(
            Arc::ptr_eq(&state, &again),
            "the same resource keeps one state across reloads"
        );
        assert_eq!(again.deadline.lock().as_secs(), Some(45));
        // Injected test clock: kept across later `state_for` calls.
        *again.deadline.lock() = Arc::new(TestWall(std::time::Instant::now()));
        let config = McpServerConfig {
            tool_timeout_secs: Some(12),
            ..config.clone()
        };
        let kept = manager.state_for(&config).expect("state");
        let kept_clock = kept.deadline.lock();
        assert_eq!(kept_clock.as_secs(), None);
        drop(kept_clock);
    }

    // ── Reviewer-round pins: §12 configured-id, dedup, SSE legs ──────────

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn configured_client_id_rejection_is_permanent_and_keeps_the_grant() {
        // Design §12: the token endpoint rejects OUR configured client id.
        // A browser sign-in would redeem under the very same id, so this
        // must NOT become grant death + browser churn.
        let stub = stub(
            Some(serde_json::json!({"client_id": "dyn-cid"})),
            vec![(400, serde_json::json!({"error": "invalid_client"}))],
        )
        .await;
        let launcher = Arc::new(AutoCallbackLauncher {
            code: "c",
            fail: false,
            launches: AtomicUsize::new(0),
        });
        let manager = McpOAuthManager::with_launcher(
            stub.tmp.path(),
            true,
            Arc::clone(&launcher) as Arc<dyn BrowserLauncher>,
        )
        .expect("manager");
        let mut config = stub.store.clone();
        config.oauth_client_id = Some("cid".to_string());
        let canonical = CanonicalResource::from_configured(
            &Url::parse(config.url.as_deref().expect("url")).expect("canonical"),
        )
        .expect("canonical");
        manager
            .store
            .save_authorization(
                &canonical,
                "cid",
                &Url::parse(&stub.as_.uri()).expect("issuer"),
                TokenSet {
                    access_token: "at-live".to_string(),
                    refresh_token: Some("rt-live".to_string()),
                    id_token: None,
                    expires_at: None,
                    token_type: Some("Bearer".to_string()),
                    scope: None,
                },
            )
            .await
            .expect("seed grant");

        let err = manager
            .on_unauthorized(&config, None, "Bearer at-live", true)
            .await
            .expect_err("configured-id rejection is a caller-side Permanent");
        let text = err.to_string();
        assert!(
            text.contains("configured oauth_client_id") && text.contains("invalid_client"),
            "got: {text}"
        );
        assert!(
            err.downcast_ref::<crate::mcp_oauth::tokens::TokenEndpointError>()
                .is_some(),
            "the structured token error must survive the context layer"
        );
        assert_eq!(
            launcher.launches.load(Ordering::SeqCst),
            0,
            "a config-level rejection must never open a browser, even interactive"
        );
        assert!(
            manager
                .store
                .load_authorization(&canonical)
                .await
                .expect("load")
                .is_some(),
            "a caller-side Permanent leaves the durable grant untouched"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_first_connects_share_one_browser_signin() {
        let stub = stub(
            Some(serde_json::json!({"client_id": "dyn-cid"})),
            vec![(
                200,
                serde_json::json!({"access_token": "at-1", "refresh_token": "rt-1", "expires_in": 3600, "token_type": "Bearer"}),
            )],
        )
        .await;
        let launcher = Arc::new(AutoCallbackLauncher {
            code: "c",
            fail: false,
            launches: AtomicUsize::new(0),
        });
        let manager = Arc::new(
            McpOAuthManager::with_launcher(
                stub.tmp.path(),
                true,
                Arc::clone(&launcher) as Arc<dyn BrowserLauncher>,
            )
            .expect("manager"),
        );
        let (ra, rb) = tokio::join!(
            manager.bearer_for(&stub.store, None, true),
            manager.bearer_for(&stub.store, None, true),
        );
        assert_eq!(ra.expect("first bearer"), "Bearer at-1");
        assert_eq!(rb.expect("second bearer"), "Bearer at-1");
        assert_eq!(
            launcher.launches.load(Ordering::SeqCst),
            1,
            "one browser sign-in, one loopback port, one registration for N concurrent connects"
        );
        let requests = stub.as_.received_requests().await.expect("received");
        assert_eq!(
            requests
                .iter()
                .filter(|r| r.method.as_str() == "POST" && r.url.path() == "/register")
                .count(),
            1,
            "the loser of the race must not re-register the client"
        );
        assert_eq!(
            requests
                .iter()
                .filter(|r| r.method.as_str() == "POST" && r.url.path() == "/token")
                .count(),
            1,
            "exactly one code exchange"
        );
    }

    struct SseGetGate {
        denials: usize,
        hits: AtomicUsize,
    }

    impl wiremock::Respond for SseGetGate {
        fn respond(&self, _: &wiremock::Request) -> ResponseTemplate {
            let hit = self.hits.fetch_add(1, Ordering::SeqCst);
            if hit < self.denials {
                ResponseTemplate::new(401).insert_header(
                    "www-authenticate",
                    "Bearer realm=\"mcp\", error=\"invalid_token\"",
                )
            } else {
                ResponseTemplate::new(405)
            }
        }
    }

    async fn sse_transport_with_get<R>(
        get_responder: R,
        denials: usize,
        refresh_steps: Vec<(u16, serde_json::Value)>,
    ) -> (
        Arc<McpOAuthManager>,
        Stub,
        Arc<crate::mcp_transport::SseTransport>,
    )
    where
        R: wiremock::Respond + Send + Sync + 'static,
    {
        let stub = stub_with(
            Some(serde_json::json!({"client_id": "cid"})),
            refresh_steps,
            Vec::new(),
        )
        .await;
        // GET on the SSE URL answers 405 (no stream) unless the caller
        // brings its own GET behavior: the transport falls back to the
        // derived message endpoint, which is the POST leg the tests below
        // pin. Advertised-endpoint tests seed the bookkeeping directly
        // (`set_advertised_endpoint_for_test`) instead of racing a stream
        // reader whose wiremock body EOFs immediately.
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/mcp"))
            .respond_with(get_responder)
            .mount(&stub.mcp)
            .await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::path("/mcp"))
            .respond_with(McpEndpoint {
                denials,
                hits: AtomicUsize::new(0),
            })
            .mount(&stub.mcp)
            .await;
        let manager = Arc::new(
            McpOAuthManager::with_launcher(
                stub.tmp.path(),
                true,
                Arc::new(AutoCallbackLauncher {
                    code: "c",
                    fail: false,
                    launches: AtomicUsize::new(0),
                }),
            )
            .expect("manager"),
        );
        manager
            .store
            .save_authorization(
                &CanonicalResource::from_configured(
                    &Url::parse(&format!("{}/mcp", stub.mcp.uri())).expect("url"),
                )
                .expect("canonical"),
                "cid",
                &Url::parse(&stub.as_.uri()).expect("issuer"),
                TokenSet {
                    access_token: "at-live".to_string(),
                    refresh_token: Some("rt-live".to_string()),
                    id_token: None,
                    expires_at: None,
                    token_type: Some("Bearer".to_string()),
                    scope: None,
                },
            )
            .await
            .expect("seed grant");
        let mut sse_config = stub.store.clone();
        sse_config.transport = zeroclaw_config::schema::McpTransport::Sse;
        let transport = Arc::new(
            crate::mcp_transport::SseTransport::new_with_oauth(
                &sse_config,
                Some(TransportOAuth::new(
                    Arc::clone(&manager),
                    sse_config.clone(),
                    false,
                )),
            )
            .expect("transport"),
        );
        (manager, stub, transport)
    }

    async fn sse_transport_with_live_grant(
        denials: usize,
        refresh_steps: Vec<(u16, serde_json::Value)>,
    ) -> (
        Arc<McpOAuthManager>,
        Stub,
        Arc<crate::mcp_transport::SseTransport>,
    ) {
        sse_transport_with_get(wiremock::ResponseTemplate::new(405), denials, refresh_steps).await
    }

    async fn send_sse(
        transport: &Arc<crate::mcp_transport::SseTransport>,
        lifecycle: &crate::mcp_transport::McpRequestLifecycle,
    ) -> Result<crate::mcp_protocol::JsonRpcResponse> {
        let request =
            crate::mcp_protocol::JsonRpcRequest::new(7, "tools/call", serde_json::json!({}));
        crate::mcp_transport::SharedMcpTransportConn::send_and_recv(
            transport.as_ref(),
            &request,
            lifecycle,
        )
        .await
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn sse_post_401_completes_the_lifecycle_refreshes_once_and_never_replays() {
        let (_manager, stub, transport) = sse_transport_with_live_grant(
            1,
            vec![(
                200,
                serde_json::json!({"access_token": "at-2", "refresh_token": "rt-2", "token_type": "Bearer", "expires_in": 3600}),
            )],
        )
        .await;
        let lifecycle = crate::mcp_transport::McpRequestLifecycle::uncoordinated(0);
        let err = send_sse(&transport, &lifecycle)
            .await
            .expect_err("SSE POST 401 surfaces, no replay");
        assert!(err.to_string().contains("401"), "got: {err}");
        assert_eq!(
            mcp_posts(&stub).await,
            1,
            "the POST leg is never replayed (pending-stream bookkeeping)"
        );
        let as_requests = stub.as_.received_requests().await.expect("received");
        assert_eq!(
            as_requests
                .iter()
                .filter(|r| r.url.path() == "/token")
                .count(),
            1,
            "best-effort refresh still runs for the NEXT attempt"
        );
        assert_eq!(
            lifecycle.outcome_unknown_epoch(),
            None,
            "a 401 proves the bearer was rejected before the message ran: \
             the write outcome is KNOWN (the one exception to SSE outcome-unknown)"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn sse_get_stream_carries_the_bearer_from_connect_onward() {
        let (_manager, stub, transport) = sse_transport_with_live_grant(0, vec![]).await;
        let lifecycle = crate::mcp_transport::McpRequestLifecycle::uncoordinated(0);
        let response = send_sse(&transport, &lifecycle)
            .await
            .expect("connect GET + derived POST succeed");
        assert_eq!(response.id, Some(serde_json::json!(7)));
        let gets: Vec<String> = stub
            .mcp
            .received_requests()
            .await
            .expect("received")
            .iter()
            .filter(|r| r.method.as_str() == "GET" && r.url.path() == "/mcp")
            .map(|r| {
                r.headers
                    .get("authorization")
                    .map(|v| v.to_str().unwrap_or_default().to_string())
                    .unwrap_or_default()
            })
            .collect();
        assert_eq!(
            gets,
            vec!["Bearer at-live".to_string()],
            "the stream GET is authenticated from the first connect (every restart re-reads the grant)"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn sse_get_401_refreshes_once_and_the_next_connect_carries_the_new_bearer() {
        // Phase-4 matrix: 401 on the STREAM leg. The stream is pre-write
        // and restartable, so the 401 must (a) fail this connect honestly,
        // (b) refresh the grant best-effort, and (c) the very next connect
        // must re-read the handle and present the ROTATED bearer — proof
        // the same auth handle survives a transport restart.
        let (_manager, stub, transport) = sse_transport_with_get(
            SseGetGate {
                denials: 1,
                hits: AtomicUsize::new(0),
            },
            0,
            vec![(
                200,
                serde_json::json!({"access_token": "at-2", "refresh_token": "rt-2", "token_type": "Bearer", "expires_in": 3600}),
            )],
        )
        .await;
        let lifecycle = crate::mcp_transport::McpRequestLifecycle::uncoordinated(0);
        let err = send_sse(&transport, &lifecycle)
            .await
            .expect_err("a 401 stream attempt fails the connect");
        assert!(err.to_string().contains("401"), "got: {err}");
        async fn token_calls(stub: &Stub) -> usize {
            stub.as_
                .received_requests()
                .await
                .expect("received")
                .iter()
                .filter(|r| r.url.path() == "/token")
                .count()
        }
        assert_eq!(
            token_calls(&stub).await,
            1,
            "the stream 401 refreshed the grant for the next attempt"
        );
        // Reconnect: same transport object (same handle), new attempt.
        let lifecycle = crate::mcp_transport::McpRequestLifecycle::uncoordinated(1);
        let response = send_sse(&transport, &lifecycle)
            .await
            .expect("the next connect presents the rotated bearer");
        assert_eq!(response.id, Some(serde_json::json!(7)));
        assert_eq!(
            token_calls(&stub).await,
            1,
            "the rotated grant is live: no second refresh"
        );
        let (gets, posts): (Vec<String>, Vec<String>) = {
            let all = stub.mcp.received_requests().await.expect("received");
            let get = |r: &wiremock::Request| {
                r.headers
                    .get("authorization")
                    .map(|v| v.to_str().unwrap_or_default().to_string())
                    .unwrap_or_default()
            };
            (
                all.iter()
                    .filter(|r| r.method.as_str() == "GET" && r.url.path() == "/mcp")
                    .map(get)
                    .collect(),
                all.iter()
                    .filter(|r| r.method.as_str() == "POST")
                    .map(get)
                    .collect(),
            )
        };
        assert_eq!(
            gets,
            vec!["Bearer at-live".to_string(), "Bearer at-2".to_string()],
            "connect #1 presented the live token, connect #2 the rotated one"
        );
        assert_eq!(
            posts,
            vec!["Bearer at-2".to_string()],
            "the message POST on the restarted transport uses the rotated grant too"
        );
    }

    #[tokio::test]
    async fn sse_advertised_endpoint_off_origin_is_rejected_before_the_bearer_leaves() {
        // Review S3-1: a hostile/compromised MCP server advertises the SSE
        // `endpoint` event on another origin; the bearer must never ride
        // there. The bookkeeping is pre-seeded (no stream race), and port 9
        // (discard) is deliberately unlistened: if the transport tried, the
        // stub would report an extra POST and the request itself would show.
        // The refusal wording is the shared `require_sse_origin` gate (the
        // confined-SSE-endpoints change) - it fires before the bearer fetch
        // and before headers are attached, which is exactly the ordering
        // this test pins for OAuth.
        let (_manager, stub, transport) = sse_transport_with_live_grant(0, vec![]).await;
        transport
            .set_advertised_endpoint_for_test("http://127.0.0.1:9/collect")
            .await;
        let lifecycle = crate::mcp_transport::McpRequestLifecycle::uncoordinated(0);
        let err = send_sse(&transport, &lifecycle)
            .await
            .expect_err("off-origin advertised endpoint is refused");
        assert!(
            err.to_string()
                .contains("must use the configured server origin"),
            "got: {err}"
        );
        let posts = stub
            .mcp
            .received_requests()
            .await
            .expect("received")
            .iter()
            .filter(|r| r.method.as_str() == "POST")
            .count();
        assert_eq!(
            posts, 0,
            "the refused send must not POST anywhere with the bearer"
        );
    }

    #[tokio::test]
    async fn sse_advertised_endpoint_on_origin_is_adopted_and_authenticated() {
        // The check is an origin gate, not a ban: the server advertising its
        // own origin (the normal legacy-SSE shape) keeps working, bearer in
        // hand.
        let (_manager, stub, transport) = sse_transport_with_live_grant(0, vec![]).await;
        transport
            .set_advertised_endpoint_for_test(&format!("{}/mcp", stub.mcp.uri()))
            .await;
        let lifecycle = crate::mcp_transport::McpRequestLifecycle::uncoordinated(0);
        let response = send_sse(&transport, &lifecycle)
            .await
            .expect("advertised same-origin endpoint is adopted");
        assert_eq!(response.id, Some(serde_json::json!(7)));
        let authenticated_posts = stub
            .mcp
            .received_requests()
            .await
            .expect("received")
            .iter()
            .filter(|r| {
                r.method.as_str() == "POST"
                    && r.url.path() == "/mcp"
                    && r.headers
                        .get("authorization")
                        .is_some_and(|v| v.to_str().unwrap_or_default() == "Bearer at-live")
            })
            .count();
        assert_eq!(authenticated_posts, 1, "the POST carried the live bearer");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn sse_post_does_not_hold_the_write_permit_across_the_bearer_fetch() {
        // Review S3-3: `bearer()` can cost a token round-trip (here: a
        // 600 ms slow refresh of the expired seed grant, taken proactively
        // before the write). That must NOT happen while the lifecycle's
        // epoch gate is read-held, or a recovery barrier — the write side
        // of the same gate — queues behind network latency.
        let gate: Arc<tokio::sync::RwLock<u64>> = Arc::new(tokio::sync::RwLock::new(0));
        let stub = stub_with(
            Some(serde_json::json!({"client_id": "cid"})),
            vec![(
                200,
                serde_json::json!({"access_token": "at-2", "token_type": "Bearer", "expires_in": 3600}),
            )],
            vec![600],
        )
        .await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/mcp"))
            .respond_with(wiremock::ResponseTemplate::new(405))
            .mount(&stub.mcp)
            .await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::path("/mcp"))
            .respond_with(McpEndpoint {
                denials: 0,
                hits: AtomicUsize::new(0),
            })
            .mount(&stub.mcp)
            .await;
        let manager = Arc::new(
            McpOAuthManager::with_launcher(
                stub.tmp.path(),
                true,
                Arc::new(AutoCallbackLauncher {
                    code: "c",
                    fail: false,
                    launches: AtomicUsize::new(0),
                }),
            )
            .expect("manager"),
        );
        manager
            .store
            .save_authorization(
                &CanonicalResource::from_configured(
                    &Url::parse(&format!("{}/mcp", stub.mcp.uri())).expect("url"),
                )
                .expect("canonical"),
                "cid",
                &Url::parse(&stub.as_.uri()).expect("issuer"),
                TokenSet {
                    access_token: "at-expired".to_string(),
                    refresh_token: Some("rt-live".to_string()),
                    id_token: None,
                    // Expired: `bearer()` proactively refreshes (the slow
                    // 600 ms token leg) before the transport writes.
                    expires_at: Some(chrono::Utc::now() - chrono::Duration::hours(1)),
                    token_type: Some("Bearer".to_string()),
                    scope: None,
                },
            )
            .await
            .expect("seed grant");
        let mut sse_config = stub.store.clone();
        sse_config.transport = zeroclaw_config::schema::McpTransport::Sse;
        let transport = Arc::new(
            crate::mcp_transport::SseTransport::new_with_oauth(
                &sse_config,
                Some(TransportOAuth::new(
                    Arc::clone(&manager),
                    sse_config.clone(),
                    false,
                )),
            )
            .expect("transport"),
        );
        let lifecycle =
            crate::mcp_transport::McpRequestLifecycle::coordinated(Arc::clone(&gate), None);
        let send = zeroclaw_spawn::spawn!(async move {
            let request =
                crate::mcp_protocol::JsonRpcRequest::new(7, "tools/call", serde_json::json!({}));
            crate::mcp_transport::SharedMcpTransportConn::send_and_recv(
                transport.as_ref(),
                &request,
                &lifecycle,
            )
            .await
        });
        // Mid-flight: the bearer is still crossing the 600 ms token leg.
        tokio::time::sleep(Duration::from_millis(150)).await;
        let barrier = tokio::time::timeout(Duration::from_millis(150), gate.write())
            .await
            .expect("a recovery barrier (epoch gate WRITE) must not queue behind the bearer fetch");
        drop(barrier);
        let response = send.await.expect("task").expect("send succeeds");
        assert_eq!(response.id, Some(serde_json::json!(7)));
    }

    #[test]
    fn sse_post_401_marks_completed_even_without_oauth() {
        // The lifecycle exception is about the PROOF a 401 gives
        // (bearer rejected at the door), not about OAuth being wired: a
        // plain 401 on a non-OAuth SSE server must also land
        // outcome-known, while every other non-success stays unknown
        // (pinned by `sse_post_404_is_not_replayed_to_derived_endpoint`).
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("rt");
        rt.block_on(async {
            let server = wiremock::MockServer::start().await;
            wiremock::Mock::given(wiremock::matchers::method("GET"))
                .and(wiremock::matchers::path("/sse"))
                .respond_with(wiremock::ResponseTemplate::new(405))
                .mount(&server)
                .await;
            wiremock::Mock::given(wiremock::matchers::method("POST"))
                .and(wiremock::matchers::path("/sse"))
                .respond_with(wiremock::ResponseTemplate::new(401))
                .mount(&server)
                .await;
            let config = zeroclaw_config::schema::McpServerConfig {
                name: "sse-plain-401".to_string(),
                transport: zeroclaw_config::schema::McpTransport::Sse,
                url: Some(format!("{}/sse", server.uri())),
                ..Default::default()
            };
            let transport = crate::mcp_transport::SseTransport::new(&config).expect("transport");
            let request =
                crate::mcp_protocol::JsonRpcRequest::new(7, "tools/call", serde_json::json!({}));
            let lifecycle = crate::mcp_transport::McpRequestLifecycle::uncoordinated(0);
            let err = crate::mcp_transport::SharedMcpTransportConn::send_and_recv(
                &transport, &request, &lifecycle,
            )
            .await
            .expect_err("401 surfaces");
            assert!(err.to_string().contains("401"), "got: {err}");
            assert_eq!(lifecycle.outcome_unknown_epoch(), None);
            let posts = server
                .received_requests()
                .await
                .expect("received")
                .iter()
                .filter(|r| r.method.as_str() == "POST")
                .count();
            assert_eq!(posts, 1, "no replay without OAuth either");
        });
    }

    #[test]
    fn grant_debug_redacts_tokens() {
        let grant = Grant {
            tokens: TokenSet {
                access_token: "topsecret".to_string(),
                refresh_token: Some("rtopsecret".to_string()),
                id_token: None,
                expires_at: None,
                token_type: Some("Bearer".to_string()),
                scope: None,
            },
            client_id: "cid".to_string(),
            issuer: Url::parse("https://auth.example").expect("u"),
            minted_at: None,
        };
        let shown = format!("{grant:?}");
        assert!(!shown.contains("topsecret"), "Debug leaked tokens: {shown}");
    }

    fn seed_grant(
        manager: &Arc<McpOAuthManager>,
        stub: &Stub,
        issuer: &Url,
        access: &str,
        expires_at: Option<chrono::DateTime<chrono::Utc>>,
    ) -> impl std::future::Future<Output = ()> + 'static {
        let manager = Arc::clone(manager);
        let canonical = CanonicalResource::from_configured(
            &Url::parse(&format!("{}/mcp", stub.mcp.uri())).expect("canonical"),
        )
        .expect("canonical");
        let issuer = issuer.clone();
        let access = access.to_string();
        async move {
            manager
                .store
                .save_authorization(
                    &canonical,
                    "cid",
                    &issuer,
                    TokenSet {
                        access_token: access,
                        refresh_token: Some("rt-live".to_string()),
                        id_token: None,
                        expires_at,
                        token_type: Some("Bearer".to_string()),
                        scope: None,
                    },
                )
                .await
                .expect("seed grant");
        }
    }

    #[tokio::test]
    async fn issuer_drift_drops_the_grant_and_never_redeems_it_at_the_new_as() {
        // Review S3-4 acceptance: a stored grant minted by issuer A must not
        // be redeemed at issuer B (credential disclosure); the drift check
        // drops it and the flow asks for a fresh sign-in instead — and NO
        // request ever reaches the new AS's token endpoint.
        let stub = stub(Some(serde_json::json!({"client_id": "cid"})), vec![]).await;
        let manager = Arc::new(manager_with(
            &stub,
            Arc::new(AutoCallbackLauncher {
                code: "c",
                fail: false,
                launches: AtomicUsize::new(0),
            }),
        ));
        seed_grant(
            &manager,
            &stub,
            &Url::parse("https://issuer-a.example").expect("url"),
            "at-live",
            None,
        )
        .await;
        let err = manager
            .bearer_for(&stub.store, None, false)
            .await
            .expect_err("drifted grant must not be redeemed");
        assert!(
            err.to_string().contains("requires an OAuth sign-in"),
            "got: {err}"
        );
        let canonical = CanonicalResource::from_configured(
            &Url::parse(&format!("{}/mcp", stub.mcp.uri())).expect("url"),
        )
        .expect("canonical");
        assert!(
            manager
                .store
                .load_authorization(&canonical)
                .await
                .expect("load")
                .is_none(),
            "the drifted grant must be cleared from the store"
        );
        let token_calls = stub
            .as_
            .received_requests()
            .await
            .expect("received")
            .iter()
            .filter(|r| r.method.as_str() == "POST" && r.url.path() == "/token")
            .count();
        assert_eq!(token_calls, 0, "no token leg may reach the new AS");
    }

    #[tokio::test]
    async fn a_drifted_issuer_adopts_a_newer_sign_in_for_the_new_as() {
        // Greptile pass-12 P1: the operator signs this resource in at the
        // replacement authorization server (the interactive CLI, while the
        // daemon keeps running) while the daemon still holds the old party's
        // grant. The drift verdict drops THAT grant; it must not delete the
        // newer profile, and this connect should adopt it rather than open a
        // second browser sign-in.
        let stub = stub(Some(serde_json::json!({"client_id": "cid"})), vec![]).await;
        let launcher = Arc::new(AutoCallbackLauncher {
            code: "c",
            fail: false,
            launches: AtomicUsize::new(0),
        });
        let manager = Arc::new(manager_with(
            &stub,
            Arc::clone(&launcher) as Arc<dyn BrowserLauncher>,
        ));
        let state = manager.state_for(&stub.store).expect("state");
        let issuer_a = Url::parse("https://issuer-a.example").expect("issuer a");
        seed_grant(&manager, &stub, &issuer_a, "at-a", None).await;
        *state.grant.write().await = Some(Grant {
            tokens: TokenSet {
                access_token: "at-a".to_string(),
                refresh_token: Some("rt-a".to_string()),
                id_token: None,
                expires_at: None,
                token_type: Some("Bearer".to_string()),
                scope: None,
            },
            client_id: "cid".to_string(),
            issuer: issuer_a.clone(),
            minted_at: aged_mint(),
        });
        manager
            .store
            .save_authorization(
                &state.canonical,
                "cid",
                &Url::parse(&stub.as_.uri()).expect("issuer b"),
                TokenSet {
                    access_token: "at-b".to_string(),
                    refresh_token: None,
                    id_token: None,
                    expires_at: Some(chrono::Utc::now() + chrono::Duration::seconds(3600)),
                    token_type: Some("Bearer".to_string()),
                    scope: None,
                },
            )
            .await
            .expect("operator sign-in at the new AS");

        let bearer = manager
            .bearer_for(&stub.store, None, false)
            .await
            .expect("a newer sign-in for the new AS must be adopted, not replaced");
        assert_eq!(bearer, "Bearer at-b");
        let stored = manager
            .store
            .load_authorization(&state.canonical)
            .await
            .expect("load")
            .expect("the newer sign-in must survive the drift verdict");
        assert_eq!(stored.tokens.access_token, "at-b");
        assert_eq!(
            launcher.launches.load(Ordering::SeqCst),
            0,
            "adopting a stored sign-in must not open a browser"
        );
    }

    #[tokio::test]
    async fn a_drifted_issuer_keeps_a_stored_profile_it_did_not_disprove() {
        // The other half of the same verdict: the stored profile is not the
        // credential this process just disproved, and it still belongs to the
        // OLD party. It is neither ours to delete nor installable at the new
        // AS, so the connect asks for a sign-in and the file on disk survives
        // untouched.
        let stub = stub(Some(serde_json::json!({"client_id": "cid"})), vec![]).await;
        let manager = Arc::new(manager_with(
            &stub,
            Arc::new(AutoCallbackLauncher {
                code: "c",
                fail: false,
                launches: AtomicUsize::new(0),
            }),
        ));
        let state = manager.state_for(&stub.store).expect("state");
        let issuer_a = Url::parse("https://issuer-a.example").expect("issuer a");
        seed_grant(&manager, &stub, &issuer_a, "at-a2", None).await;
        *state.grant.write().await = Some(Grant {
            tokens: TokenSet {
                access_token: "at-a".to_string(),
                refresh_token: Some("rt-a".to_string()),
                id_token: None,
                expires_at: None,
                token_type: Some("Bearer".to_string()),
                scope: None,
            },
            client_id: "cid".to_string(),
            issuer: issuer_a.clone(),
            minted_at: aged_mint(),
        });

        let err = manager
            .bearer_for(&stub.store, None, false)
            .await
            .expect_err("the old party's stored credential is not usable at the new AS");
        assert!(
            err.to_string().contains("requires an OAuth sign-in"),
            "got: {err}"
        );
        assert!(
            state.grant.read().await.is_none(),
            "the disproved grant must leave the live state"
        );
        let stored = manager
            .store
            .load_authorization(&state.canonical)
            .await
            .expect("load")
            .expect("a profile this process never disproved must not be deleted");
        assert_eq!(stored.tokens.access_token, "at-a2");
    }

    #[tokio::test]
    async fn issuer_trailing_slash_is_not_drift() {
        // The drift compare keeps `issuer_matches` semantics: trailing-slash
        // spellings of the SAME issuer must not force a pointless sign-out.
        let stub = stub(Some(serde_json::json!({"client_id": "cid"})), vec![]).await;
        let manager = Arc::new(manager_with(
            &stub,
            Arc::new(AutoCallbackLauncher {
                code: "c",
                fail: false,
                launches: AtomicUsize::new(0),
            }),
        ));
        let stored = Url::parse(&format!("{}/", stub.as_.uri())).expect("url");
        seed_grant(&manager, &stub, &stored, "at-live", None).await;
        let bearer = manager
            .bearer_for(&stub.store, None, false)
            .await
            .expect("same issuer, one trailing slash: the live grant stands");
        assert_eq!(bearer, "Bearer at-live");
        let canonical = CanonicalResource::from_configured(
            &Url::parse(&format!("{}/mcp", stub.mcp.uri())).expect("url"),
        )
        .expect("canonical");
        assert!(
            manager
                .store
                .load_authorization(&canonical)
                .await
                .expect("load")
                .is_some(),
            "no drift: the grant must not be dropped"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn refreshed_bearer_survives_an_unwritable_store() {
        // Review S3-5 acceptance: the token endpoint rotated the grant (the
        // old refresh token is now DEAD) and the disk write fails — the
        // process must still run on the NEW bearer, not strand the operator
        // on a dead token until a browser sign-in.
        use std::os::unix::fs::PermissionsExt;
        // The store's lock needs fresh-create rights, so a read-only
        // directory would break LOAD as well. The sequence isolates SAVE:
        // call #1 (writable) consumes the load-once probe; then chmod 500
        // makes the lock file creation — and therefore every write — fail
        // while reads still work, exactly the "rotated in memory, dead on
        // disk" shape the WARN path exists for.
        let stub = stub_with(
            Some(serde_json::json!({"client_id": "cid"})),
            vec![(
                200,
                serde_json::json!({"access_token": "at-2", "token_type": "Bearer", "expires_in": 3600}),
            )],
            Vec::new(),
        )
        .await;
        let manager = Arc::new(manager_with(
            &stub,
            Arc::new(AutoCallbackLauncher {
                code: "c",
                fail: false,
                launches: AtomicUsize::new(0),
            }),
        ));
        seed_grant(
            &manager,
            &stub,
            &Url::parse(&stub.as_.uri()).expect("url"),
            "at-live",
            Some(chrono::Utc::now() + chrono::Duration::hours(1)),
        )
        .await;
        // #1: healthy load, live grant, no refresh.
        let first = manager
            .bearer_for(&stub.store, None, false)
            .await
            .expect("healthy first call");
        assert_eq!(first, "Bearer at-live");
        // Break persistence (reads still fine, the load already happened).
        std::fs::set_permissions(stub.tmp.path(), std::fs::Permissions::from_mode(0o500))
            .expect("chmod store read-only");
        // #2: 401-driven rotation with an unwritable store.
        let rotated = manager
            .on_unauthorized(&stub.store, None, "Bearer at-live", false)
            .await
            .unwrap_or_else(|err| panic!("full error chain: {err:#}"));
        std::fs::set_permissions(stub.tmp.path(), std::fs::Permissions::from_mode(0o700))
            .expect("chmod store back");
        assert_eq!(
            rotated, "Bearer at-2",
            "a failed store write must not fail the rotated grant"
        );
        // Pin that the SAVE actually failed: a root environment with
        // CAP_DAC_OVERRIDE would silently succeed and pass this test
        // without ever touching the WARN path.
        let persisted = manager
            .store
            .load_authorization(
                &CanonicalResource::from_configured(
                    &Url::parse(&format!("{}/mcp", stub.mcp.uri())).expect("url"),
                )
                .expect("canonical"),
            )
            .await
            .expect("readable again")
            .expect("the stale grant is still what survived on disk");
        assert_eq!(
            persisted.tokens.access_token, "at-live",
            "the rotated token never reached the disk — the WARN path ran"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn slow_first_sign_in_survives_the_initialize_handshake_timeout() {
        // Live incident regression against a real authorization server: the
        // interactive sign-in wait ran inside the `initialize` send and so
        // inside the client handshake's 30s receive budget. A human slower
        // than that budget had the wait canceled: the loopback listener was
        // dropped mid-browser-flow and the callback hit a dead port. The
        // transport now prepares credentials (`prepare_auth`) before the
        // request-budgeted handshake, so sign-in keeps its own, human-paced
        // callback budget.
        let s = stub(
            Some(serde_json::json!({"client_id": "cid-slow"})),
            vec![(
                200,
                serde_json::json!({
                    "access_token": "at-slow",
                    "token_type": "Bearer",
                    "expires_in": 3600,
                    "refresh_token": "rt-slow",
                    "scope": "mcp:tools",
                }),
            )],
        )
        .await;
        // MCP JSON-RPC side: every POST must already carry the freshly
        // obtained bearer — an unauthenticated handshake POST means the
        // sign-in-before-first-request ordering regressed.
        for (rpc_method, response) in [
            (
                "initialize",
                wiremock::ResponseTemplate::new(200)
                    .insert_header("Mcp-Session-Id", "sess-slow")
                    .set_body_json(serde_json::json!({"jsonrpc": "2.0", "id": 1, "result": {}})),
            ),
            (
                "tools/list",
                wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 2,
                    "result": {"tools": [{"name": "slow_tool", "description": "d",
                                          "inputSchema": {"type": "object"}}]}
                })),
            ),
        ] {
            Mock::given(method("POST"))
                .and(path("/mcp"))
                .and(wiremock::matchers::body_partial_json(serde_json::json!(
                    {"method": rpc_method}
                )))
                .and(wiremock::matchers::header(
                    "authorization",
                    "Bearer at-slow",
                ))
                .respond_with(response)
                .mount(&s.mcp)
                .await;
        }
        Mock::given(method("POST"))
            .and(path("/mcp"))
            .and(wiremock::matchers::body_partial_json(serde_json::json!(
                {"method": "notifications/initialized"}
            )))
            .respond_with(wiremock::ResponseTemplate::new(202))
            .mount(&s.mcp)
            .await;

        // 35s is strictly greater than `mcp_client::RECV_TIMEOUT_SECS` (30):
        // before the fix, the handshake timed out long before the simulated
        // human delivered the callback.
        let launcher = Arc::new(SlowHumanLauncher {
            code: "code-slow",
            delay: Duration::from_secs(35),
        });
        let manager = Arc::new(manager_with(&s, launcher));
        let oauth = TransportOAuth::new(manager, s.store.clone(), true);

        let started = std::time::Instant::now();
        let server = crate::mcp_client::McpServer::connect_with_oauth(s.store.clone(), Some(oauth))
            .await
            .expect("a 35s interactive sign-in must not be canceled by the handshake timeout");
        assert!(
            started.elapsed() >= Duration::from_secs(33),
            "the launcher delay never applied — the test would not exercise the timeout"
        );
        assert_eq!(
            s.mcp
                .received_requests()
                .await
                .expect("received")
                .iter()
                .filter(|r| r.method.as_str() == "POST" && r.url.path() == "/mcp")
                .count(),
            3,
            "the full authenticated connect sequence (initialize + notification + \
             tools/list) ran over /mcp"
        );
        let _ = &server;
        assert_eq!(
            s.as_
                .received_requests()
                .await
                .expect("received")
                .iter()
                .filter(|r| r.url.path() == "/token")
                .count(),
            1,
            "exactly one token exchange across the whole connect"
        );
    }
}
