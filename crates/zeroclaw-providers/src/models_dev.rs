//! Unauthenticated cross-provider model catalog via models.dev.

use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

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

/// UNIX seconds of the last failed catalog fetch, or `0` if none failed yet.
/// Consulted by [`ensure_catalog_loaded`] to bound retry frequency; the
/// `get_or_try_init` path used by explicit listings is unaffected.
static LAST_CATALOG_FETCH_FAILURE_UNIX: AtomicI64 = AtomicI64::new(0);

/// Serializes catalog fetches: at most one in-flight network attempt at any
/// time. Concurrent [`ensure_catalog_loaded`] callers wait on this lock, so an
/// outage cannot start multiple 10-second fetch attempts in parallel. The
/// retry-deadline check runs both before and inside the lock, so a caller that
/// waited for a failed fetch re-evaluates the deadline that fetch recorded
/// before starting its own attempt.
static CATALOG_FETCH_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Injectable fetcher for tests. Production code never sets this; tests
/// install an RAII guard so [`ensure_catalog_loaded`] can be exercised without
/// network access. The `Fn` returns a boxed future so the override can own
/// captured counters.
type CatalogFetchFuture = std::pin::Pin<Box<dyn Future<Output = Result<Arc<Catalog>>> + Send>>;
static CATALOG_FETCH_OVERRIDE: std::sync::Mutex<
    Option<Arc<dyn Fn() -> CatalogFetchFuture + Send + Sync>>,
> = std::sync::Mutex::new(None);

/// Run the catalog fetch, honoring the test override when installed. The
/// override lock is released before awaiting the fetch (the boxed future does
/// not borrow it), so a fetch that internally awaits is never blocked on the
/// override mutex.
async fn run_catalog_fetch() -> Result<Arc<Catalog>> {
    let override_fetch = match CATALOG_FETCH_OVERRIDE.lock() {
        Ok(guard) => guard.clone(),
        Err(poisoned) => poisoned.into_inner().clone(),
    };
    if let Some(fetch) = override_fetch {
        return (fetch)().await;
    }
    fetch_catalog().await
}

fn now_unix_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
        .unwrap_or(0)
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
    // 1. Already populated (possibly by the explicit-listing path) — success
    //    regardless of any stale backoff deadline. Checking this before the
    //    deadline means a stale failure timestamp can never reject warming
    //    when the catalog actually loaded via another path.
    if CACHED_CATALOG.get().is_some() {
        return Ok(());
    }

    // Monotonic retry deadline: within `CATALOG_FETCH_RETRY_BACKOFF_SECS` of a
    // failure, reject the fetch so a models.dev outage does not add a
    // 10-second timeout to every tool-loop iteration's warm call.
    let in_retry_backoff = || {
        let last_failure = LAST_CATALOG_FETCH_FAILURE_UNIX.load(Ordering::Acquire);
        last_failure != 0 && now_unix_secs() < last_failure + CATALOG_FETCH_RETRY_BACKOFF_SECS
    };
    if in_retry_backoff() {
        anyhow::bail!("models.dev catalog fetch is in retry backoff after a recent failure");
    }

    // 2. Single-flight: serialize fetches so an outage cannot start multiple
    //    network attempts concurrently. A caller that waited for a failed
    //    fetch re-evaluates the deadline that fetch just recorded before
    //    starting its own attempt (the check inside the lock).
    let _guard = CATALOG_FETCH_LOCK.lock().await;
    if CACHED_CATALOG.get().is_some() {
        return Ok(());
    }
    if in_retry_backoff() {
        anyhow::bail!("models.dev catalog fetch is in retry backoff after a recent failure");
    }

    // 3. Own the only in-flight slot: fetch, cache the result, and record the
    //    failure deadline if it failed.
    match run_catalog_fetch().await {
        Ok(catalog) => {
            let _ = CACHED_CATALOG.set(catalog);
            Ok(())
        }
        Err(err) => {
            LAST_CATALOG_FETCH_FAILURE_UNIX.store(now_unix_secs(), Ordering::Release);
            Err(err)
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
            let catalog = CACHED_CATALOG.get_or_try_init(fetch_catalog).await?;
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
            let catalog = CACHED_CATALOG.get_or_try_init(fetch_catalog).await?;
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

    /// RAII guard that installs a catalog-fetch override for a test and
    /// restores the previous override (normally `None`) on drop — including on
    /// panic, so a failing lifecycle test cannot leak its fetcher into
    /// sibling tests.
    struct CatalogFetchOverrideGuard(Option<Arc<dyn Fn() -> CatalogFetchFuture + Send + Sync>>);

    impl CatalogFetchOverrideGuard {
        fn install(fetch: Arc<dyn Fn() -> CatalogFetchFuture + Send + Sync>) -> Self {
            let mut guard = CATALOG_FETCH_OVERRIDE
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let previous = guard.replace(fetch);
            drop(guard);
            Self(previous)
        }
    }

    impl Drop for CatalogFetchOverrideGuard {
        fn drop(&mut self) {
            let mut guard = CATALOG_FETCH_OVERRIDE
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            *guard = self.0.take();
        }
    }

    /// RAII guard that clears the process-global failure timestamp on drop, so
    /// a lifecycle test that records a failure cannot put sibling tests into
    /// backoff.
    struct CatalogFailureTimestampGuard;

    impl Drop for CatalogFailureTimestampGuard {
        fn drop(&mut self) {
            LAST_CATALOG_FETCH_FAILURE_UNIX.store(0, Ordering::Release);
        }
    }

    /// Serializes the catalog-lifecycle tests: they share the process-global
    /// `CACHED_CATALOG` / `LAST_CATALOG_FETCH_FAILURE_UNIX`, so concurrent
    /// execution would interleave their mutations.
    static CATALOG_LIFECYCLE_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

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
        let _serial = CATALOG_LIFECYCLE_TEST_LOCK.lock().await;
        // If another lifecycle test already populated the process-global
        // cache, the backoff branch is unreachable (the cached path wins) —
        // assert that and return rather than fail spuriously.
        if CACHED_CATALOG.get().is_some() {
            assert!(ensure_catalog_loaded().await.is_ok());
            return;
        }

        // Install a fetch override that must never run: inside the backoff
        // window the warm call bails before any fetch attempt.
        let calls = Arc::new(AtomicUsize::new(0));
        let _fetch = CatalogFetchOverrideGuard::install(Arc::new({
            let calls = Arc::clone(&calls);
            move || {
                calls.fetch_add(1, Ordering::SeqCst);
                Box::pin(async { Err(anyhow::Error::msg("fetch must not run inside backoff")) })
                    as CatalogFetchFuture
            }
        }));
        let _ts = CatalogFailureTimestampGuard;

        // Regression for the tool-loop retry gap: after a failed fetch, a
        // models.dev outage must not add a 10-second timeout to every
        // iteration's warm call.
        LAST_CATALOG_FETCH_FAILURE_UNIX.store(now_unix_secs() - 5, Ordering::Relaxed);
        let err = ensure_catalog_loaded()
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
        let _serial = CATALOG_LIFECYCLE_TEST_LOCK.lock().await;
        if CACHED_CATALOG.get().is_some() {
            assert!(ensure_catalog_loaded().await.is_ok());
            return;
        }

        // No prior failure recorded (the default state) must never trigger the
        // backoff branch — the fetch runs (via the override, so no network).
        let calls = Arc::new(AtomicUsize::new(0));
        let _fetch = CatalogFetchOverrideGuard::install(Arc::new({
            let calls = Arc::clone(&calls);
            move || {
                calls.fetch_add(1, Ordering::SeqCst);
                Box::pin(async { Err(anyhow::Error::msg("simulated outage")) })
                    as CatalogFetchFuture
            }
        }));
        let _ts = CatalogFailureTimestampGuard;

        LAST_CATALOG_FETCH_FAILURE_UNIX.store(0, Ordering::Relaxed);
        let err = ensure_catalog_loaded()
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
        let _serial = CATALOG_LIFECYCLE_TEST_LOCK.lock().await;
        if CACHED_CATALOG.get().is_some() {
            assert!(ensure_catalog_loaded().await.is_ok());
            return;
        }

        // Two concurrent warm calls must share one in-flight fetch: the second
        // caller waits on the fetch lock and then observes the failure deadline
        // the first fetch recorded, so it bails in backoff instead of starting
        // a second network attempt.
        let calls = Arc::new(AtomicUsize::new(0));
        let _fetch = CatalogFetchOverrideGuard::install(Arc::new({
            let calls = Arc::clone(&calls);
            move || {
                calls.fetch_add(1, Ordering::SeqCst);
                Box::pin(async { Err(anyhow::Error::msg("simulated outage")) })
                    as CatalogFetchFuture
            }
        }));
        let _ts = CatalogFailureTimestampGuard;

        let (r1, r2) = tokio::join!(ensure_catalog_loaded(), ensure_catalog_loaded());
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
        let _serial = CATALOG_LIFECYCLE_TEST_LOCK.lock().await;
        if CACHED_CATALOG.get().is_some() {
            assert!(ensure_catalog_loaded().await.is_ok());
            return;
        }

        // First fetch fails (records the deadline); the second attempt inside
        // the window is suppressed by backoff; once the window expires the
        // third attempt runs and succeeds. Exercises the full lifecycle
        // without network access.
        let calls = Arc::new(AtomicUsize::new(0));
        let _fetch = CatalogFetchOverrideGuard::install(Arc::new({
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
        }));
        let _ts = CatalogFailureTimestampGuard;

        assert!(
            ensure_catalog_loaded().await.is_err(),
            "the first fetch must fail and record the retry deadline"
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        let err = ensure_catalog_loaded()
            .await
            .expect_err("inside the backoff window the retry must be suppressed");
        assert!(err.to_string().contains("backoff"));
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "no fetch may run inside the backoff window"
        );

        // Expire the deadline and observe recovery.
        LAST_CATALOG_FETCH_FAILURE_UNIX.store(
            now_unix_secs() - CATALOG_FETCH_RETRY_BACKOFF_SECS - 1,
            Ordering::Release,
        );
        assert!(
            ensure_catalog_loaded().await.is_ok(),
            "once the backoff window expires the fetch must retry and recover"
        );
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn ensure_catalog_loaded_skips_backoff_when_cache_populated() {
        let _serial = CATALOG_LIFECYCLE_TEST_LOCK.lock().await;
        // Populate the cache (if a sibling test has not already), then set a
        // fresh failure timestamp inside the backoff window. The populated
        // cache must win over the stale deadline: warming must never be
        // rejected when the catalog actually loaded via another path.
        if CACHED_CATALOG.get().is_none() {
            let catalog = Arc::new(parse_catalog(TINY_CATALOG.as_bytes()).unwrap());
            let _ = CACHED_CATALOG.set(catalog);
        }
        let _ts = CatalogFailureTimestampGuard;
        LAST_CATALOG_FETCH_FAILURE_UNIX.store(now_unix_secs(), Ordering::Release);

        assert!(
            ensure_catalog_loaded().await.is_ok(),
            "a populated catalog must not be rejected by a stale backoff deadline"
        );
    }
}
