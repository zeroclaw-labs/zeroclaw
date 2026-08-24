//! Unauthenticated cross-provider model catalog via models.dev.

use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;
#[cfg(test)]
use std::sync::Mutex;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::{Duration, Instant};

use crate::pricing::{ModelRates, sane_mtok};
use anyhow::Result;
use serde::Deserialize;
use tokio::sync::OnceCell;

const CATALOG_URL: &str = "https://models.dev/api.json";
const FETCH_TIMEOUT_SECS: u64 = 10;
/// Minimum gap between retry attempts after a failed catalog fetch. Without
/// this, a models.dev outage adds a full 10-second timeout to every tool-loop
/// iteration that runs the per-turn warm call.
const CATALOG_FETCH_RETRY_BACKOFF_SECS: i64 = 60;

/// Serializes the tests that mutate the process-global catalog state
/// (`CACHED_CATALOG` and the global lifecycle). Shared by the lifecycle tests
/// in this module and the catalog-injection tests in `compatible.rs` /
/// `reliable.rs`, so a lifecycle test that seeds `TINY_CATALOG` cannot race an
/// injection test's `CACHED_CATALOG.set` in the same test binary (OnceCell
/// `set` fails on an already-populated cell and the injection then silently
/// looks up the wrong catalog).
#[cfg(test)]
static CATALOG_LIFECYCLE_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Test-only: acquire the process-global catalog-state lock before mutating
/// `CACHED_CATALOG`. Catalog-injection tests in sibling modules share the same
/// global state as this module's lifecycle tests and must serialize on the
/// same lock.
#[cfg(test)]
pub(crate) async fn __private_test_catalog_lock() -> tokio::sync::MutexGuard<'static, ()> {
    CATALOG_LIFECYCLE_TEST_LOCK.lock().await
}

/// Injectable fetcher for tests. Production never sets this; tests construct an
/// isolated [`CatalogLifecycle`] with their own `CatalogFetchFuture` instead of
/// mutating process-global state. The `Fn` returns a boxed future so the
/// override can own captured counters.
type CatalogFetchFuture = std::pin::Pin<Box<dyn Future<Output = Result<Arc<Catalog>>> + Send>>;

/// Run the catalog fetch live against models.dev.
async fn run_catalog_fetch() -> Result<Arc<Catalog>> {
    fetch_catalog().await
}

/// Monotonic clock reading in seconds, offset away from zero. Uses a
/// process-start anchor (`std::time::Instant`, which is unaffected by
/// wall-clock adjustments), so the retry deadline is genuinely monotonic — a
/// system clock jump cannot extend or bypass the backoff window. The offset
/// guarantees the reading is never `0` (which is the "no prior failure"
/// sentinel in [`CatalogLifecycle::last_failure`]): at process start, elapsed
/// is `0s`, which would falsely read as "no failure". Offsetting both `now`
/// and recorded deadlines by the same constant keeps the relative backoff
/// comparison unchanged. The production lifecycle uses this clock; lifecycle
/// tests inject a manual clock via [`CatalogLifecycle`] instead.
const CATALOG_CLOCK_OFFSET_SECS: i64 = 1_000_000_000;

fn now_monotonic_secs() -> i64 {
    static PROCESS_START: OnceLock<Instant> = OnceLock::new();
    let start = *PROCESS_START.get_or_init(Instant::now);
    i64::try_from(start.elapsed().as_secs())
        .map(|elapsed| elapsed + CATALOG_CLOCK_OFFSET_SECS)
        .unwrap_or(i64::MAX)
}

/// The catalog lifecycle: one fetch/retry policy owned by every caller
/// (warming via `ensure_catalog_loaded`, listings via `list_models_for` /
/// `list_models_with_context_for`). The single-flight lock, the monotonic
/// retry deadline, the clock, and the fetcher are all injectable, so a test
/// can construct an isolated lifecycle (its own cache + clock + fetcher) and
/// every transition test always runs — no dependency on the process-global
/// cache state.
struct CatalogLifecycle {
    in_flight: tokio::sync::Mutex<()>,
    last_failure: AtomicI64,
    clock: Box<dyn Fn() -> i64 + Send + Sync>,
    fetcher: Box<dyn Fn() -> CatalogFetchFuture + Send + Sync>,
}

impl CatalogLifecycle {
    fn production() -> Self {
        Self {
            in_flight: tokio::sync::Mutex::const_new(()),
            last_failure: AtomicI64::new(0),
            clock: Box::new(now_monotonic_secs),
            fetcher: Box::new(|| Box::pin(run_catalog_fetch())),
        }
    }

    /// Populate `cache` following the single lifecycle policy: populated
    /// cache wins over any stale deadline; a fresh failure deadline rejects
    /// the fetch inside the backoff window; concurrent callers share one
    /// in-flight fetch via `in_flight`; a failed fetch records the deadline.
    async fn ensure_loaded(&self, cache: &OnceCell<Arc<Catalog>>) -> Result<()> {
        // 1. Already populated (possibly by another path) — success regardless
        //    of any stale backoff deadline. Checking this before the deadline
        //    means a stale failure timestamp can never reject warming when the
        //    catalog actually loaded via another path.
        if cache.get().is_some() {
            return Ok(());
        }

        // Monotonic retry deadline: within `CATALOG_FETCH_RETRY_BACKOFF_SECS`
        // of a failure, reject the fetch so a models.dev outage does not add a
        // 10-second timeout to every tool-loop iteration's warm call.
        let in_retry_backoff = || {
            let last_failure = self.last_failure.load(Ordering::Acquire);
            last_failure != 0 && (self.clock)() < last_failure + CATALOG_FETCH_RETRY_BACKOFF_SECS
        };
        if in_retry_backoff() {
            anyhow::bail!("models.dev catalog fetch is in retry backoff after a recent failure");
        }

        // 2. Single-flight: serialize fetches so an outage cannot start
        //    multiple network attempts concurrently. A caller that waited for
        //    a failed fetch re-evaluates the deadline that fetch just recorded
        //    before starting its own attempt (the check inside the lock).
        let _guard = self.in_flight.lock().await;
        if cache.get().is_some() {
            return Ok(());
        }
        if in_retry_backoff() {
            anyhow::bail!("models.dev catalog fetch is in retry backoff after a recent failure");
        }

        // 3. Own the only in-flight slot: fetch, cache the result, and record
        //    the failure deadline if it failed.
        match (self.fetcher)().await {
            Ok(catalog) => {
                let _ = cache.set(catalog);
                Ok(())
            }
            Err(err) => {
                self.last_failure.store((self.clock)(), Ordering::Release);
                Err(err)
            }
        }
    }
}

/// Process-wide lifecycle used by production (`ensure_catalog_loaded` and the
/// listing functions). Its fetcher fetches live against models.dev; tests that
/// need to exercise a fetch failure construct their own isolated
/// `CatalogLifecycle` rather than mutating this global.
static GLOBAL_CATALOG_LIFECYCLE: OnceLock<CatalogLifecycle> = OnceLock::new();

fn global_catalog_lifecycle() -> &'static CatalogLifecycle {
    GLOBAL_CATALOG_LIFECYCLE.get_or_init(CatalogLifecycle::production)
}

#[derive(Debug, Deserialize)]
pub(crate) struct ProviderEntry {
    #[serde(default)]
    models: HashMap<String, ModelEntry>,
}

#[derive(Debug, Deserialize)]
struct ModelEntry {
    id: String,
    #[serde(default)]
    cost: Option<ModelCost>,
    #[serde(default)]
    limit: Option<ModelLimit>,
    /// models.dev `modalities` block. Carries the per-model `input` and
    /// `output` modality lists (e.g. `input: ["text", "image"]`). Previously
    /// dropped during deserialization; per-model vision support is now
    /// resolved through this field.
    #[serde(default)]
    modalities: Option<Modalities>,
}

/// models.dev `cost` block: USD per 1M tokens (the same unit ZeroClaw's rate
/// sheet uses, so no conversion is needed).
#[derive(Debug, Deserialize, Clone, Copy, Default)]
struct ModelCost {
    #[serde(default)]
    input: Option<f64>,
    #[serde(default)]
    output: Option<f64>,
    #[serde(default)]
    cache_read: Option<f64>,
}

/// models.dev `limit` block: `context` is the model's maximum input window in
/// tokens, the same unit `providers.models.<type>.<alias>.context_window` uses.
#[derive(Debug, Deserialize, Clone, Copy, Default)]
struct ModelLimit {
    #[serde(default)]
    context: Option<u64>,
}

/// models.dev `modalities` block — only the `input` dimension is consumed
/// today. Membership of `"image"` in `input` is what callers use to decide
/// whether a model can accept vision attachments; `output` (and any future
/// modality vectors we do not yet read) are tolerated by `serde` defaults
/// rather than deserialized into named fields.
#[derive(Debug, Deserialize, Clone, Default, PartialEq, Eq)]
struct Modalities {
    #[serde(default)]
    input: Vec<String>,
}

impl Modalities {
    /// Whether this model advertises image input support. Conservative: only
    /// an explicit `"image"` token in `input` flips it on. Malformed
    /// catalog entries (missing `modalities` or empty `input`) yield
    /// `false`; callers fall back to the family default in that case.
    fn supports_image_input(&self) -> bool {
        self.input.iter().any(|m| m == "image")
    }
}

pub(crate) type Catalog = HashMap<String, ProviderEntry>;

/// Process-wide cached catalog. Public so `OpenAiCompatibleModelProvider::capabilities_for_model()`
/// can do a non-blocking lookup for per-model vision support.
///
/// LIFECYCLE CONTRACT: this is a process-lifetime snapshot, not a refreshable
/// cache. Upstream models.dev modality corrections (a model gaining or losing
/// image input, a new provider family) are invisible until the process
/// restarts. The catalog is used as live routing authority for per-model
/// vision capability — do not assume capabilities refresh in-process; callers
/// that need to track upstream changes must restart or consult a provider
/// listing that is not backed by this snapshot.
pub(crate) static CACHED_CATALOG: OnceCell<Arc<Catalog>> = OnceCell::const_new();

/// Fetch and parse the models.dev catalog fresh (no process cache). Used by the
/// live-pricing refresher so its fallback tracks upstream changes per cycle;
/// the cached [`list_models_for`] path stays on the process-lifetime cache.
pub(crate) async fn fetch_catalog() -> Result<Arc<Catalog>> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(FETCH_TIMEOUT_SECS))
        .build()?;
    let response = client.get(CATALOG_URL).send().await?.error_for_status()?;
    let bytes = response.bytes().await?;
    Ok(Arc::new(parse_catalog(&bytes)?))
}

/// Parse the models.dev JSON into the in-memory `Catalog` shape. Pure
/// function — unit tests construct minimal JSON byte slices and assert
/// the filter logic without any network call.
pub(crate) fn parse_catalog(bytes: &[u8]) -> Result<Catalog> {
    Ok(serde_json::from_slice(bytes)?)
}

/// Filter a parsed catalog for a model_provider key. Sorted, deduped.
/// Pure — separated from the live fetch so it can be unit-tested.
pub(crate) fn filter_models(catalog: &Catalog, provider_key: &str) -> Result<Vec<String>> {
    let entry = catalog.get(provider_key).ok_or_else(|| {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                .with_attrs(::serde_json::json!({"model_provider": provider_key})),
            "models_dev: provider not in catalog"
        );
        anyhow::Error::msg(format!(
            "model_provider {provider_key:?} is not in the models.dev catalog"
        ))
    })?;
    let mut ids: Vec<String> = entry.models.values().map(|m| m.id.clone()).collect();
    ids.sort();
    ids.dedup();
    Ok(ids)
}

/// Ensure the process-wide catalog is populated, so the synchronous
/// per-model capability gate (`capabilities_for_model`) can resolve
/// vision support from it even on paths that never run a models.dev
/// listing (e.g. credentialed OpenAI-compatible providers that list
/// through their native `/models` endpoint). No-op once loaded.
/// Called from async agent-turn context before the capability query.
pub(crate) async fn ensure_catalog_loaded() -> Result<()> {
    global_catalog_lifecycle()
        .ensure_loaded(&CACHED_CATALOG)
        .await
}

/// Entry point the compatible resolve override uses before querying per-model
/// vision. In production it is exactly [`ensure_catalog_loaded`]. Under
/// `#[cfg(test)]` it can be redirected at an isolated failing-catalog scenario
/// (its own local cache + lifecycle) so a test can deterministically force the
/// credentialed resolve branch to surface `Err` regardless of whether the
/// process-global `CACHED_CATALOG` has already been populated by a sibling test
/// (a `OnceCell` can never be reset). Non-`#[cfg(test)]` behavior is unchanged.
pub(crate) async fn ensure_catalog_loaded_for_resolve() -> Result<()> {
    #[cfg(test)]
    {
        let scenario = TEST_RESOLVE_CATALOG
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if let Some(scenario) = scenario {
            return scenario.lifecycle.ensure_loaded(&scenario.cache).await;
        }
    }
    ensure_catalog_loaded().await
}

/// Test-only isolated catalog scenario installed for the compatible resolve
/// override: its own `CatalogLifecycle` (with the test's fetcher) and its own
/// local, always-empty `OnceCell` cache. Because the cache belongs to the
/// scenario and never to the process-global `CACHED_CATALOG`, the lifecycle's
/// failing fetcher always runs and deterministically surfaces `Err` — no
/// dependence on (or mutation of) the global snapshot.
#[cfg(test)]
struct IsolatedResolveCatalog {
    lifecycle: CatalogLifecycle,
    cache: OnceCell<Arc<Catalog>>,
}

#[cfg(test)]
static TEST_RESOLVE_CATALOG: Mutex<Option<Arc<IsolatedResolveCatalog>>> = Mutex::new(None);

/// RAII guard that installs an isolated failing-catalog scenario for
/// [`ensure_catalog_loaded_for_resolve`]. Held for the test's duration; the
/// tests in `compatible.rs` and `vision_override.rs` build their provider
/// chains while a guard is live and assert the credentialed resolve branch
/// fails. Restores the previous override on drop. Serializes on the same
/// process-global catalog-state lock as the other catalog-mutating tests so it
/// never races a sibling test seeding `CACHED_CATALOG`.
#[cfg(test)]
pub(crate) struct ResolveCatalogIsolationGuard {
    _previous: Option<Arc<IsolatedResolveCatalog>>,
    _lock: tokio::sync::MutexGuard<'static, ()>,
}

#[cfg(test)]
impl ResolveCatalogIsolationGuard {
    /// Install a fresh scenario whose lattice-aware fetcher always fails, on a
    /// local empty cache, so an `Err` is deterministic and independent of the
    /// global `CACHED_CATALOG` state.
    pub(crate) async fn install_with_failing_fetch() -> Self {
        let lock = __private_test_catalog_lock().await;
        let scene = Arc::new(IsolatedResolveCatalog {
            lifecycle: CatalogLifecycle {
                in_flight: tokio::sync::Mutex::const_new(()),
                last_failure: AtomicI64::new(0),
                clock: Box::new(now_monotonic_secs),
                fetcher: Box::new(|| {
                    Box::pin(async { Err(anyhow::Error::msg("models.dev catalog unavailable")) })
                        as CatalogFetchFuture
                }),
            },
            cache: OnceCell::const_new(),
        });
        let mut slot = TEST_RESOLVE_CATALOG
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let previous = slot.replace(scene);
        Self {
            _previous: previous,
            _lock: lock,
        }
    }
}

#[cfg(test)]
impl Drop for ResolveCatalogIsolationGuard {
    fn drop(&mut self) {
        if let Ok(mut slot) = TEST_RESOLVE_CATALOG.lock() {
            *slot = self._previous.take();
        }
    }
}

/// Look up model IDs for a model_provider, keyed by `models.dev`'s model_provider name.
///
/// First call fetches the catalog; subsequent calls hit the cache. The
/// returned list is sorted for stable menu rendering.
///
/// Attribution: the models.dev catalog is a global, pre-authentication
/// metadata source with no concrete `Attributable` thing of its own.
/// We wrap the body with `scope!(model_provider_type: "models_dev",
/// model_provider_alias: "catalog", …)` so the `filter_models` warning
/// (and any future record! inside `fetch_catalog`) lands with the
/// model_provider_type and model_provider_alias slots populated.
pub async fn list_models_for(provider_key: &str) -> Result<Vec<String>> {
    ::zeroclaw_log::scope!(
        model_provider_type: "models_dev",
        model_provider_alias: "catalog",
        => async move {
            // Route through the shared lifecycle owner (`ensure_catalog_loaded`)
            // so listing and warming share ONE fetch/retry policy: the same
            // single-flight lock and monotonic backoff deadline. The old
            // `get_or_try_init` path bypassed the backoff and could start a
            // parallel fetch while a warm was in retry backoff.
            ensure_catalog_loaded().await?;
            let catalog = CACHED_CATALOG.get().expect(
                "ensure_catalog_loaded populated the catalog or returned Err",
            );
            filter_models(catalog, provider_key)
        }
    )
    .await
}

/// Same listing as [`list_models_for`], each id paired with the context window
/// the catalog publishes for it. `None` means the catalog has no `limit.context`
/// for that model — callers surface that as "unknown", never as a default.
pub async fn list_models_with_context_for(
    provider_key: &str,
) -> Result<Vec<(String, Option<usize>)>> {
    ::zeroclaw_log::scope!(
        model_provider_type: "models_dev",
        model_provider_alias: "catalog",
        => async move {
            // Same shared lifecycle owner as `list_models_for` (see there).
            ensure_catalog_loaded().await?;
            let catalog = CACHED_CATALOG.get().expect(
                "ensure_catalog_loaded populated the catalog or returned Err",
            );
            let ids = filter_models(catalog, provider_key)?;
            let windows = context_windows_from_catalog(catalog, provider_key);
            Ok(ids
                .into_iter()
                .map(|id| {
                    let ctx = windows.get(&id).copied();
                    (id, ctx)
                })
                .collect())
        }
    )
    .await
}

/// Per-model pricing for one model_provider from a parsed catalog, as a
/// `model_id -> ModelRates` map. Models with no `cost` block are omitted;
/// like `rates_catalog`, this emptiness filter is load-bearing for downstream
/// consumers. Pure, unit-testable without the network. Rates are USD per 1M
/// tokens verbatim (no conversion).
pub(crate) fn pricing_from_catalog(
    catalog: &Catalog,
    provider_key: &str,
) -> HashMap<String, ModelRates> {
    let mut out = HashMap::new();
    let Some(entry) = catalog.get(provider_key) else {
        return out;
    };
    for model in entry.models.values() {
        let Some(cost) = model.cost else { continue };
        // models.dev `cost` is already USD per 1M tokens, no scaling. Each
        // dimension is sanity-bounded so a malformed catalog entry can't bill
        // an absurd cost (same ceiling as the gateway path).
        let rates = ModelRates {
            input_per_mtok: cost.input.and_then(sane_mtok),
            output_per_mtok: cost.output.and_then(sane_mtok),
            cached_input_per_mtok: cost.cache_read.and_then(sane_mtok),
        };
        if !rates.is_empty() {
            out.insert(model.id.clone(), rates);
        }
    }
    out
}

/// Largest context window treated as plausible. Frontier models are at 1M–10M
/// tokens; anything past this is a malformed catalog entry, not a real limit.
const MAX_PLAUSIBLE_CONTEXT_WINDOW: u64 = 100_000_000;

/// Reject zero and absurd values so a malformed catalog entry can't widen an
/// agent's trim budget past anything the model could actually accept.
fn sane_context_window(raw: u64) -> Option<usize> {
    if raw == 0 || raw > MAX_PLAUSIBLE_CONTEXT_WINDOW {
        return None;
    }
    usize::try_from(raw).ok()
}

/// Per-model context windows for a provider, in tokens. Mirrors
/// [`pricing_from_catalog`]: a view materialized from the catalog on demand,
/// never stored. Models with no `limit.context` are absent from the map.
pub(crate) fn context_windows_from_catalog(
    catalog: &Catalog,
    provider_key: &str,
) -> HashMap<String, usize> {
    let mut out = HashMap::new();
    let Some(entry) = catalog.get(provider_key) else {
        return out;
    };
    for model in entry.models.values() {
        if let Some(ctx) = model
            .limit
            .and_then(|l| l.context)
            .and_then(sane_context_window)
        {
            out.insert(model.id.clone(), ctx);
        }
    }
    out
}

/// Per-model vision support resolved from the parsed catalog.
///
/// Returns `Some(true)` when the model is in the catalog and its
/// `modalities.input` lists `"image"`. Returns `Some(false)` when the model
/// is in the catalog but does not advertise image input. Returns `None`
/// when the model isn't in the catalog, the provider key isn't, or the
/// catalog entry has no `modalities` block at all — callers should fall
/// back to the family default in that case.
///
/// Pure / sync / no network. Used by `OpenAiCompatibleModelProvider::capabilities_for_model()`
/// to resolve per-model vision capability from the models.dev catalog.
pub(crate) fn model_supports_vision(
    catalog: &Catalog,
    provider_key: &str,
    model_id: &str,
) -> Option<bool> {
    let entry = catalog.get(provider_key)?;
    let model = entry.models.get(model_id)?;
    Some(model.modalities.as_ref()?.supports_image_input())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    const TINY_CATALOG: &str = r#"{
        "anthropic": {
            "models": {
                "claude-sonnet-4-6": {"id": "claude-sonnet-4-6"},
                "claude-opus-4-7":   {"id": "claude-opus-4-7"}
            }
        },
        "xai": {
            "models": {
                "grok-4.3":     {"id": "grok-4.3"},
                "grok-2-vision":{"id": "grok-2-vision"}
            }
        },
        "empty": { "models": {} }
    }"#;

    /// Build an isolated catalog lifecycle for a transition test: its own
    /// cache, a manually-advanceable clock, and a fetch function that counts
    /// calls and lets the test decide success/failure. Because every field is
    /// owned by the instance, tests never depend on the process-global cache
    /// state — every transition test always runs. The clock is an `Arc` so the
    /// closure can own it for the `'static` bound on `CatalogLifecycle`.
    fn test_lifecycle(
        now: Arc<std::sync::Mutex<i64>>,
        fetch: impl Fn() -> CatalogFetchFuture + Send + Sync + 'static,
    ) -> CatalogLifecycle {
        CatalogLifecycle {
            in_flight: tokio::sync::Mutex::const_new(()),
            last_failure: AtomicI64::new(0),
            clock: Box::new(move || *now.lock().unwrap_or_else(|e| e.into_inner())),
            fetcher: Box::new(fetch),
        }
    }

    #[test]
    fn parses_catalog_with_typical_shape() {
        let catalog = parse_catalog(TINY_CATALOG.as_bytes()).expect("parses");
        assert_eq!(catalog.len(), 3);
        assert!(catalog.contains_key("anthropic"));
        assert!(catalog.contains_key("xai"));
    }

    #[test]
    fn filter_returns_sorted_ids() {
        let catalog = parse_catalog(TINY_CATALOG.as_bytes()).unwrap();
        let ids = filter_models(&catalog, "xai").unwrap();
        assert_eq!(ids, vec!["grok-2-vision", "grok-4.3"]);
    }

    #[test]
    fn filter_dedups() {
        // Models.dev model_id values could in theory collide; the filter
        // dedups the output list so the menu doesn't render duplicates.
        let raw = r#"{"x": {"models": {"a": {"id": "m1"}, "b": {"id": "m1"}}}}"#;
        let catalog = parse_catalog(raw.as_bytes()).unwrap();
        let ids = filter_models(&catalog, "x").unwrap();
        assert_eq!(ids, vec!["m1"]);
    }

    #[test]
    fn filter_returns_empty_for_empty_entry() {
        let catalog = parse_catalog(TINY_CATALOG.as_bytes()).unwrap();
        let ids = filter_models(&catalog, "empty").unwrap();
        assert!(ids.is_empty());
    }

    #[test]
    fn filter_errors_on_unknown_key() {
        let catalog = parse_catalog(TINY_CATALOG.as_bytes()).unwrap();
        let err = filter_models(&catalog, "missing").expect_err("must error");
        assert!(err.to_string().contains("missing"));
    }

    #[test]
    fn parse_errors_on_malformed_json() {
        assert!(parse_catalog(b"not json").is_err());
    }

    #[test]
    fn pricing_from_catalog_reads_cost_and_skips_unpriced() {
        // `cost` is USD per 1M tokens; models without it are omitted.
        let raw = r#"{
            "kilo": {
                "models": {
                    "a": {"id": "minimax-m2.7", "cost": {"input": 0.3, "output": 1.2, "cache_read": 0.06}},
                    "b": {"id": "no-cost-model"}
                }
            }
        }"#;
        let catalog = parse_catalog(raw.as_bytes()).unwrap();
        let map = pricing_from_catalog(&catalog, "kilo");
        let m = map.get("minimax-m2.7").expect("priced");
        assert_eq!(m.input_per_mtok, Some(0.3));
        assert_eq!(m.output_per_mtok, Some(1.2));
        assert_eq!(m.cached_input_per_mtok, Some(0.06));
        assert!(!map.contains_key("no-cost-model"));
        // Unknown provider key yields an empty map, not an error.
        assert!(pricing_from_catalog(&catalog, "absent").is_empty());
    }

    #[test]
    fn context_windows_from_catalog_reads_limit_and_skips_unlimited() {
        // `limit.context` is the max input window in tokens; models without a
        // `limit` block are omitted rather than defaulted.
        let raw = r#"{
            "anthropic": {
                "models": {
                    "a": {"id": "claude-opus-4-8", "limit": {"context": 1000000, "output": 128000}},
                    "b": {"id": "claude-opus-4-5", "limit": {"context": 200000}},
                    "c": {"id": "no-limit-model"},
                    "d": {"id": "empty-limit-model", "limit": {}}
                }
            }
        }"#;
        let catalog = parse_catalog(raw.as_bytes()).unwrap();
        let map = context_windows_from_catalog(&catalog, "anthropic");
        assert_eq!(map.get("claude-opus-4-8"), Some(&1_000_000));
        assert_eq!(map.get("claude-opus-4-5"), Some(&200_000));
        assert!(!map.contains_key("no-limit-model"));
        assert!(!map.contains_key("empty-limit-model"));
        // Unknown provider key yields an empty map, not an error.
        assert!(context_windows_from_catalog(&catalog, "absent").is_empty());
    }

    #[test]
    fn context_windows_reject_zero_and_absurd_values() {
        // A malformed entry must not widen an agent's trim budget.
        let raw = r#"{
            "p": {
                "models": {
                    "a": {"id": "zero", "limit": {"context": 0}},
                    "b": {"id": "absurd", "limit": {"context": 999999999999}},
                    "c": {"id": "ok", "limit": {"context": 8192}}
                }
            }
        }"#;
        let catalog = parse_catalog(raw.as_bytes()).unwrap();
        let map = context_windows_from_catalog(&catalog, "p");
        assert!(!map.contains_key("zero"));
        assert!(!map.contains_key("absurd"));
        assert_eq!(map.get("ok"), Some(&8192));
    }

    #[test]
    fn unknown_limit_fields_do_not_break_parsing() {
        // models.dev adds fields over time; parsing must tolerate them so a
        // catalog change can't break model listing.
        let raw = r#"{
            "p": { "models": { "a": {"id": "m", "limit": {"context": 4096, "future_field": 7}} } }
        }"#;
        let catalog = parse_catalog(raw.as_bytes()).unwrap();
        assert_eq!(
            context_windows_from_catalog(&catalog, "p").get("m"),
            Some(&4096)
        );
    }

    #[test]
    fn model_supports_vision_reads_modalities_input_image() {
        // models.dev `modalities.input` advertises "image" for vision models
        // and is absent for text-only models. The helper must read that field
        // and return Some(bool) for cataloged models.
        let raw = r#"{
            "xai": {
                "models": {
                    "grok-2-vision": {"id": "grok-2-vision",
                                      "modalities": {"input": ["text", "image"], "output": ["text"]}},
                    "grok-4.3":     {"id": "grok-4.3",
                                      "modalities": {"input": ["text"], "output": ["text"]}}
                }
            }
        }"#;
        let catalog = parse_catalog(raw.as_bytes()).unwrap();
        assert_eq!(
            model_supports_vision(&catalog, "xai", "grok-2-vision"),
            Some(true)
        );
        assert_eq!(
            model_supports_vision(&catalog, "xai", "grok-4.3"),
            Some(false)
        );
    }

    #[test]
    fn model_supports_vision_returns_none_for_missing_modalities_block() {
        // Old-shape entries (no `modalities` block) must yield None, not
        // false — callers fall back to the family default in that case.
        let raw = r#"{
            "xai": {
                "models": {
                    "grok-4.3": {"id": "grok-4.3"}
                }
            }
        }"#;
        let catalog = parse_catalog(raw.as_bytes()).unwrap();
        assert_eq!(model_supports_vision(&catalog, "xai", "grok-4.3"), None);
    }

    #[test]
    fn model_supports_vision_returns_none_for_unknown_provider_or_model() {
        let raw = r#"{
            "xai": {
                "models": {
                    "grok-2-vision": {"id": "grok-2-vision",
                                      "modalities": {"input": ["text", "image"], "output": ["text"]}}
                }
            }
        }"#;
        let catalog = parse_catalog(raw.as_bytes()).unwrap();
        // Unknown model id within a known provider.
        assert_eq!(model_supports_vision(&catalog, "xai", "grok-99"), None);
        // Unknown provider key.
        assert_eq!(
            model_supports_vision(&catalog, "absent", "grok-2-vision"),
            None
        );
    }

    #[test]
    fn model_supports_vision_does_not_match_non_image_modality_aliases() {
        // Defensive: only an exact "image" token in `input` flips vision on.
        // "images" (plural) and "image_url" (a wire-format alias used in
        // OpenAI's request shape) must NOT count — they are not what
        // models.dev emits and a future schema drift should surface as a
        // false negative, not a silent true.
        let raw = r#"{
            "fake": {
                "models": {
                    "alias-1": {"id": "alias-1",
                                "modalities": {"input": ["text", "images"], "output": ["text"]}},
                    "alias-2": {"id": "alias-2",
                                "modalities": {"input": ["text", "image_url"], "output": ["text"]}}
                }
            }
        }"#;
        let catalog = parse_catalog(raw.as_bytes()).unwrap();
        assert_eq!(
            model_supports_vision(&catalog, "fake", "alias-1"),
            Some(false)
        );
        assert_eq!(
            model_supports_vision(&catalog, "fake", "alias-2"),
            Some(false)
        );
    }

    #[tokio::test]
    async fn ensure_catalog_loaded_respects_retry_backoff() {
        // Isolated lifecycle: a failure recorded 5s ago, clock frozen now, so
        // the fetch must be suppressed (it would otherwise add a 10-second
        // timeout to every tool-loop iteration's warm call). No process-global
        // state — this always runs.
        let now = Arc::new(std::sync::Mutex::new(CATALOG_CLOCK_OFFSET_SECS));
        let calls = Arc::new(AtomicUsize::new(0));
        let cache = OnceCell::const_new();
        let lifecycle = test_lifecycle(Arc::clone(&now), {
            let calls = Arc::clone(&calls);
            move || {
                calls.fetch_add(1, Ordering::SeqCst);
                Box::pin(async { Err(anyhow::Error::msg("fetch must not run inside backoff")) })
                    as CatalogFetchFuture
            }
        });
        lifecycle.last_failure.store(
            *now.lock().unwrap_or_else(|e| e.into_inner()) - 5,
            Ordering::Relaxed,
        );

        let err = lifecycle
            .ensure_loaded(&cache)
            .await
            .expect_err("a recent fetch failure must put the warm call in backoff");
        assert!(
            err.to_string().contains("backoff"),
            "backoff error must be descriptive, got: {err}"
        );
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "no fetch may run while the failure is inside the backoff window"
        );
    }

    #[tokio::test]
    async fn ensure_catalog_loaded_does_not_backoff_without_prior_failure() {
        // No prior failure recorded (the default state) must never trigger the
        // backoff branch — the fetch runs and surfaces its error.
        let now = Arc::new(std::sync::Mutex::new(CATALOG_CLOCK_OFFSET_SECS));
        let calls = Arc::new(AtomicUsize::new(0));
        let cache = OnceCell::const_new();
        let lifecycle = test_lifecycle(Arc::clone(&now), {
            let calls = Arc::clone(&calls);
            move || {
                calls.fetch_add(1, Ordering::SeqCst);
                Box::pin(async { Err(anyhow::Error::msg("simulated outage")) })
                    as CatalogFetchFuture
            }
        });

        let err = lifecycle
            .ensure_loaded(&cache)
            .await
            .expect_err("with no prior failure the fetch must run and surface its error");
        assert!(
            !err.to_string().contains("backoff"),
            "no prior failure must not put the call in backoff, got: {err}"
        );
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "the fetch must run exactly once when there is no backoff"
        );
    }

    #[tokio::test]
    async fn ensure_catalog_loaded_serializes_concurrent_fetches() {
        // Two concurrent warm calls must share one in-flight fetch: the second
        // caller waits on the fetch lock and then observes the failure deadline
        // the first fetch recorded, so it bails in backoff instead of starting
        // a second network attempt. Isolated lifecycle — always runs.
        let now = Arc::new(std::sync::Mutex::new(CATALOG_CLOCK_OFFSET_SECS));
        let calls = Arc::new(AtomicUsize::new(0));
        let cache = OnceCell::const_new();
        let lifecycle = test_lifecycle(Arc::clone(&now), {
            let calls = Arc::clone(&calls);
            move || {
                calls.fetch_add(1, Ordering::SeqCst);
                Box::pin(async { Err(anyhow::Error::msg("simulated outage")) })
                    as CatalogFetchFuture
            }
        });

        let (r1, r2) = tokio::join!(
            lifecycle.ensure_loaded(&cache),
            lifecycle.ensure_loaded(&cache)
        );
        assert!(
            r1.is_err() && r2.is_err(),
            "a failing fetch must surface an error to every concurrent caller"
        );
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "concurrent warm calls must share one in-flight fetch, not start parallel attempts"
        );
    }

    #[tokio::test]
    async fn ensure_catalog_loaded_recovers_after_backoff_window() {
        // First fetch fails (records the deadline); the second attempt inside
        // the window is suppressed by backoff; advancing the clock past the
        // window makes the third attempt run and succeed. Isolated lifecycle
        // with a manually-advanced clock — every transition always runs.
        let now = Arc::new(std::sync::Mutex::new(CATALOG_CLOCK_OFFSET_SECS));
        let calls = Arc::new(AtomicUsize::new(0));
        let cache = OnceCell::const_new();
        let lifecycle = test_lifecycle(Arc::clone(&now), {
            let calls = Arc::clone(&calls);
            move || {
                let n = calls.fetch_add(1, Ordering::SeqCst) + 1;
                if n == 1 {
                    Box::pin(async { Err(anyhow::Error::msg("simulated outage")) })
                        as CatalogFetchFuture
                } else {
                    let catalog = Arc::new(parse_catalog(TINY_CATALOG.as_bytes()).unwrap());
                    Box::pin(async move { Ok(catalog) }) as CatalogFetchFuture
                }
            }
        });

        assert!(
            lifecycle.ensure_loaded(&cache).await.is_err(),
            "the first fetch must fail and record the retry deadline"
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        let err = lifecycle
            .ensure_loaded(&cache)
            .await
            .expect_err("inside the backoff window the retry must be suppressed");
        assert!(err.to_string().contains("backoff"));
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "no fetch may run inside the backoff window"
        );

        // Advance the clock past the window and observe recovery.
        *now.lock().unwrap_or_else(|e| e.into_inner()) += CATALOG_FETCH_RETRY_BACKOFF_SECS + 1;
        assert!(
            lifecycle.ensure_loaded(&cache).await.is_ok(),
            "once the backoff window expires the fetch must retry and recover"
        );
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn ensure_catalog_loaded_skips_backoff_when_cache_populated() {
        // A populated catalog must win over a fresh failure timestamp inside
        // the backoff window: warming must never be rejected when the catalog
        // actually loaded via another path. Isolated cache seeded first.
        let now = Arc::new(std::sync::Mutex::new(CATALOG_CLOCK_OFFSET_SECS));
        let cache = OnceCell::const_new();
        let catalog = Arc::new(parse_catalog(TINY_CATALOG.as_bytes()).unwrap());
        let _ = cache.set(catalog);
        let lifecycle = test_lifecycle(Arc::clone(&now), {
            move || {
                Box::pin(async { Err(anyhow::Error::msg("fetch must not run")) })
                    as CatalogFetchFuture
            }
        });
        lifecycle.last_failure.store(
            *now.lock().unwrap_or_else(|e| e.into_inner()),
            Ordering::Release,
        );

        assert!(
            lifecycle.ensure_loaded(&cache).await.is_ok(),
            "a populated catalog must not be rejected by a stale backoff deadline"
        );
    }
}
