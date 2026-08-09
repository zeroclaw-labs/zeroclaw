//! Live model pricing: a single process-global snapshot of token prices
//! fetched from providers' own `/models` listings (with the models.dev catalog
//! as a secondary source for models the gateway doesn't price), used as a
//! FALLBACK beneath the operator's `[cost.rates]` config.

use crate::traits::{ModelInfo, ModelPricing};
use futures_util::future::join_all;
use parking_lot::RwLock;
use std::collections::HashMap;
use std::sync::{Arc, LazyLock, OnceLock};
use std::time::Duration;
use zeroclaw_config::schema::Config;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ModelRates {
    pub input_per_mtok: Option<f64>,
    pub output_per_mtok: Option<f64>,
    pub cached_input_per_mtok: Option<f64>,
}

impl ModelRates {
    /// True when no dimension carries a rate.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.input_per_mtok.is_none()
            && self.output_per_mtok.is_none()
            && self.cached_input_per_mtok.is_none()
    }

    /// True when every dimension carries a rate (nothing left to fill).
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.input_per_mtok.is_some()
            && self.output_per_mtok.is_some()
            && self.cached_input_per_mtok.is_some()
    }

    /// Per-dimension precedence merge: `self` wins, `fallback` fills only the
    /// dimensions `self` left unset (`Option::or`). A `Some(0.0)` in `self`
    /// (a deliberately-free rate) is preserved, never overridden.
    #[must_use]
    pub fn or(self, fallback: ModelRates) -> ModelRates {
        ModelRates {
            input_per_mtok: self.input_per_mtok.or(fallback.input_per_mtok),
            output_per_mtok: self.output_per_mtok.or(fallback.output_per_mtok),
            cached_input_per_mtok: self
                .cached_input_per_mtok
                .or(fallback.cached_input_per_mtok),
        }
    }
}

/// Upper bound on a sane per-1M-token rate. At `$1`/token this sits orders of
/// magnitude above any real model price, so a genuine rate never trips it; it
/// only catches a parsing artifact or a hostile/buggy gateway reporting an
/// absurd value (which would otherwise bill a fortune).
const MAX_SANE_PER_MTOK: f64 = 1_000_000.0;

pub(crate) fn sane_mtok(rate: f64) -> Option<f64> {
    (0.0..=MAX_SANE_PER_MTOK).contains(&rate).then_some(rate)
}

fn per_token_str_to_mtok(value: &Option<String>) -> Option<f64> {
    let parsed: f64 = value.as_deref()?.trim().parse().ok()?;
    sane_mtok(parsed * 1_000_000.0)
}

/// Normalize a provider's `/models` [`ModelPricing`] (per-token strings) into
/// per-1M-token [`ModelRates`]. `prompt`→input, `completion`→output,
/// `input_cache_read`→cached.
pub(crate) fn normalize_pricing(pricing: &ModelPricing) -> ModelRates {
    ModelRates {
        input_per_mtok: per_token_str_to_mtok(&pricing.prompt),
        output_per_mtok: per_token_str_to_mtok(&pricing.completion),
        cached_input_per_mtok: per_token_str_to_mtok(&pricing.input_cache_read),
    }
}

pub type PriceSnapshot = HashMap<String, HashMap<String, ModelRates>>;

/// How often the background task re-fetches prices.
const REFRESH_INTERVAL: Duration = Duration::from_secs(60 * 60);

/// The ONE owner of live prices. Empty until the first successful refresh.
static LIVE_PRICES: LazyLock<RwLock<Arc<PriceSnapshot>>> =
    LazyLock::new(|| RwLock::new(Arc::new(HashMap::new())));

static CONFIG_HANDLE: LazyLock<RwLock<Option<Arc<RwLock<Config>>>>> =
    LazyLock::new(|| RwLock::new(None));

/// Guards against spawning more than one refresher when both the channels
/// orchestrator and the gateway start in the same process.
static REFRESHER_STARTED: OnceLock<()> = OnceLock::new();

/// Non-blocking read of the current price snapshot. Returns an `Arc` clone of
/// whatever the last successful refresh produced (empty if none yet). Never
/// fetches, never `.await`s; safe to call from the synchronous cost path.
#[must_use]
pub fn current_snapshot() -> Arc<PriceSnapshot> {
    Arc::clone(&LIVE_PRICES.read())
}

/// Model-id candidate forms, most specific first: the id verbatim, then the
/// path-suffix form (`vendor/slug` → `slug`). The single owner of the
/// candidate policy: snapshot assembly (`match_pricing`) and lookup both
/// consume it, and the config-rate resolver mirrors it.
pub fn model_id_candidates(model_id: &str) -> impl Iterator<Item = &str> {
    std::iter::once(model_id).chain(model_id.rsplit_once('/').map(|(_, suffix)| suffix))
}

#[must_use]
pub fn lookup<'a>(
    snapshot: &'a PriceSnapshot,
    provider_ref: &str,
    model_id: &str,
) -> Option<&'a ModelRates> {
    let probe = |key: &str| -> Option<&'a ModelRates> {
        let models = snapshot.get(key)?;
        model_id_candidates(model_id).find_map(|id| models.get(id))
    };
    probe(provider_ref).or_else(|| {
        provider_ref
            .split_once('.')
            .and_then(|(family, _alias)| probe(family))
    })
}

/// Replace the global snapshot. The refresher is the only production writer.
fn store_snapshot(snapshot: PriceSnapshot) {
    *LIVE_PRICES.write() = Arc::new(snapshot);
}

/// True when at least one model provider opts into `live_pricing`.
fn any_live_pricing(config: &Config) -> bool {
    config
        .providers
        .models
        .iter_entries()
        .any(|(_, _, base)| base.live_pricing)
}

/// Spawn the background price refresher, once per process.
///
/// No-op when no provider currently sets `live_pricing = true`: zero network,
/// zero task; the cost path keeps reading an empty snapshot. This cheap
/// pre-check runs *before* claiming the once-per-process guard, so a later
/// `/admin/reload` (or a re-`start_channels`) that newly enables a provider can
/// still start the refresher. Idempotent: a second concurrent caller (e.g. the
/// gateway after the channels supervisor, in a combined process) returns at the
/// guard without building any provider handles.
///
/// Every call re-binds `CONFIG_HANDLE` before anything else, and the running
/// task re-resolves it each cycle, so a daemon reload (which re-instantiates
/// the config `Arc` and re-runs both call sites) re-points the refresher at
/// the current config, and toggling `live_pricing` on a provider (or changing
/// its model/endpoint) is honored on the next refresh without a restart. Each
/// cycle rebuilds the per-gateway poll set via the normal factory path
/// (reusing each provider's configured `base_url`, credentials, and options),
/// fetches one `/models` per gateway, and fills only the flagged models,
/// falling back to the models.dev catalog for models a gateway doesn't price
/// (or providers with no HTTP listing, e.g. the `kilocli` subprocess gateway).
/// A fetch error for one source keeps the previous snapshot rather than
/// regressing good prices to empty; disabling `live_pricing` on the last
/// flagged provider instead clears the snapshot on the next cycle, so stale
/// prices stop filling after an opt-out.
pub fn spawn_refresher(config: Arc<RwLock<Config>>) {
    // Re-bind before the enabled pre-check so even a "nothing enabled yet"
    // call leaves the freshest handle for a refresher started later.
    *CONFIG_HANDLE.write() = Some(Arc::clone(&config));
    if !any_live_pricing(&config.read()) {
        return;
    }
    if REFRESHER_STARTED.set(()).is_err() {
        return; // already running
    }

    ::zeroclaw_spawn::spawn!(async {
        loop {
            // Re-resolve the handle (re-bound across daemon reloads), then
            // clone the config under the lock and build/poll without holding
            // it. The handle is bound above before this task can exist, and
            // never unbound, so the `expect` cannot fire.
            let handle = CONFIG_HANDLE
                .read()
                .clone()
                .expect("config handle is bound before the refresher is spawned");
            let cfg = handle.read().clone();
            let (groups, total_aliases_per_family) = enabled_pricing_groups(&cfg);
            if groups.is_empty() {
                if !current_snapshot().is_empty() {
                    store_snapshot(PriceSnapshot::new());
                }
            } else {
                refresh_once(&groups, &total_aliases_per_family).await;
            }
            tokio::time::sleep(REFRESH_INTERVAL).await;
        }
    });
}

/// One model whose price we want filled: the composite alias (`<type>.<alias>`)
/// the snapshot is keyed by, the gateway's model id to match, and the
/// models.dev provider key to fall back to when the gateway carries no price.
struct WantedModel {
    /// Composite alias (`<type>.<alias>`, e.g. `kilo.primary`). The snapshot
    /// is keyed by this, preserving the alias boundary so a non-opted-in alias
    /// cannot inherit prices from an opted-in same-family alias via the
    /// bare-family fallback.
    provider_key: String,
    /// Same as `provider_key`; kept for clarity at use sites that build handles.
    composite: String,
    model_id: String,
    models_dev_key: Option<String>,
}

/// A gateway to poll once. `handle` is built from one representative alias on
/// the gateway (they share the endpoint); `wanted` lists every flagged model on
/// that same gateway whose price should be filled from the single catalog
/// response.
struct GatewayGroup {
    handle: Arc<dyn crate::traits::ModelProvider>,
    wanted: Vec<WantedModel>,
}

fn enabled_pricing_groups(
    config: &zeroclaw_config::schema::Config,
) -> (Vec<GatewayGroup>, HashMap<String, usize>) {
    // Count TOTAL aliases per family (opted-in and not), used later to decide
    // whether a bare-family snapshot entry is safe to add.
    let mut total_aliases_per_family: HashMap<String, usize> = HashMap::new();
    for (ty, _alias, _base) in config.providers.models.iter_entries() {
        *total_aliases_per_family.entry(ty.to_string()).or_default() += 1;
    }

    // gateway key -> (representative built handle, wanted models)
    let mut groups: HashMap<String, GatewayGroup> = HashMap::new();
    for (ty, alias, base) in config.providers.models.iter_entries() {
        if !base.live_pricing {
            continue;
        }
        // Only a model with a known id can be matched against the catalog.
        let Some(model_id) = base
            .model
            .as_deref()
            .map(str::trim)
            .filter(|m| !m.is_empty())
        else {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({ "provider": format!("{ty}.{alias}") })),
                "live pricing: provider opted in but sets no `model`; nothing to price"
            );
            continue;
        };
        let composite = format!("{ty}.{alias}");
        // Aliases on the same explicit endpoint share one call; aliases on a
        // family default share one call per family type.
        let gateway_key = base
            .uri
            .as_deref()
            .map(str::trim)
            .filter(|u| !u.is_empty())
            .map_or_else(|| format!("type:{ty}"), str::to_string);

        let wanted = WantedModel {
            // Keyed by composite alias to preserve the alias boundary; the
            // snapshot uses this directly so a non-opted-in alias can never
            // inherit rates from an opted-in same-family alias.
            provider_key: composite.clone(),
            composite,
            model_id: model_id.to_string(),
            // models.dev provider key for this family, for the fallback path.
            models_dev_key: crate::catalog::catalog_source_for(ty)
                .and_then(|(md_key, _)| md_key)
                .map(str::to_string),
        };

        if let Some(group) = groups.get_mut(&gateway_key) {
            group.wanted.push(wanted);
            continue;
        }
        // First alias for this gateway: build the representative handle.
        let options = crate::options_for_provider_ref(
            config,
            &wanted.composite,
            &crate::ModelProviderRuntimeOptions::default(),
        );
        match crate::create_model_provider_for_alias(
            config,
            ty,
            alias,
            base.api_key.as_deref(),
            &options,
        ) {
            Ok(handle) => {
                groups.insert(
                    gateway_key,
                    GatewayGroup {
                        handle: Arc::from(handle),
                        wanted: vec![wanted],
                    },
                );
            }
            Err(error) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({
                            "provider": wanted.composite,
                            "error": format!("{error}"),
                        })),
                    "live pricing: could not build provider; skipping gateway"
                );
            }
        }
    }
    (groups.into_values().collect(), total_aliases_per_family)
}

/// Match a flagged model id against a `model_id -> rates` catalog through
/// [`model_id_candidates`].
fn match_pricing<'a>(
    catalog: &'a HashMap<String, ModelRates>,
    model_id: &str,
) -> Option<&'a ModelRates> {
    model_id_candidates(model_id).find_map(|id| catalog.get(id))
}

/// Normalize a provider's `list_models_with_pricing()` result into a
/// `model_id -> ModelRates` catalog, dropping models that carry no price.
/// The emptiness filter here (and its twin in `pricing_from_catalog`) is
/// load-bearing: downstream consumers treat any catalog hit as priced.
fn rates_catalog(models: Vec<ModelInfo>) -> HashMap<String, ModelRates> {
    models
        .into_iter()
        .filter_map(|info| {
            let rates = info
                .pricing
                .as_ref()
                .map(normalize_pricing)
                .unwrap_or_default();
            (!rates.is_empty()).then_some((info.id, rates))
        })
        .collect()
}

fn assemble_snapshot(
    gateway_results: &[(&[WantedModel], HashMap<String, ModelRates>)],
    models_dev: &HashMap<String, HashMap<String, ModelRates>>,
    total_aliases_per_family: &HashMap<String, usize>,
) -> PriceSnapshot {
    let mut out = PriceSnapshot::new();
    // Count opted-in aliases per family (each WantedModel is one <type>.<alias>).
    let mut opted_in_per_family: HashMap<String, usize> = HashMap::new();

    for (wanted, catalog) in gateway_results {
        for want in *wanted {
            let family = want
                .provider_key
                .split_once('.')
                .map(|(f, _)| f)
                .unwrap_or(&want.provider_key);
            *opted_in_per_family.entry(family.to_string()).or_default() += 1;

            let gw = match_pricing(catalog, &want.model_id).copied();
            let md = want
                .models_dev_key
                .as_deref()
                .and_then(|md_key| models_dev.get(md_key))
                .and_then(|md_catalog| match_pricing(md_catalog, &want.model_id))
                .copied();
            // Per-dimension precedence: gateway wins each dimension it sets,
            // models.dev fills the rest. Both catalogs drop empty rates at
            // construction, so a present entry carries at least one rate.
            let rates = match (gw, md) {
                (Some(g), Some(m)) => Some(g.or(m)),
                (Some(g), None) => Some(g),
                (None, Some(m)) => Some(m),
                (None, None) => None,
            };
            if let Some(rates) = rates {
                // Key by COMPOSITE alias to preserve the alias boundary.
                out.entry(want.provider_key.clone())
                    .or_default()
                    .insert(want.model_id.clone(), rates);
            }
        }
    }

    for (family, opted_in_count) in &opted_in_per_family {
        let total = total_aliases_per_family.get(family).copied().unwrap_or(0);
        if *opted_in_count < total {
            continue;
        }
        // Merge all composite alias entries for this family into one map.
        let prefix = format!("{family}.");
        let mut family_map: HashMap<String, ModelRates> = HashMap::new();
        for (composite_key, model_map) in &out {
            if !composite_key.starts_with(&prefix) {
                continue;
            }
            for (model_id, &rates) in model_map {
                let slot = family_map.entry(model_id.clone()).or_default();
                *slot = ModelRates {
                    input_per_mtok: slot.input_per_mtok.or(rates.input_per_mtok),
                    output_per_mtok: slot.output_per_mtok.or(rates.output_per_mtok),
                    cached_input_per_mtok: slot
                        .cached_input_per_mtok
                        .or(rates.cached_input_per_mtok),
                };
            }
        }
        if !family_map.is_empty() {
            out.insert(family.clone(), family_map);
        }
    }

    out
}

/// Poll each gateway once, fall back to models.dev for models a gateway didn't
/// price, and publish the merged snapshot. Keeps the previous snapshot if no
/// source succeeded (never regress good prices to empty on a transient outage).
async fn refresh_once(groups: &[GatewayGroup], total_aliases_per_family: &HashMap<String, usize>) {
    // ── Gather all gateway catalogs concurrently (one `/models` call per
    // gateway; first-fill latency is bounded by the slowest gateway, not the
    // sum of all of them) ──
    let mut any_ok = false;
    let catalogs = join_all(groups.iter().map(|group| async {
        crate::ProviderDispatch::from_ref(&*group.handle)
            .list_models_with_pricing()
            .await
    }))
    .await;
    let mut gateway_results: Vec<(&[WantedModel], HashMap<String, ModelRates>)> =
        Vec::with_capacity(groups.len());
    for (group, result) in groups.iter().zip(catalogs) {
        let catalog = match result {
            Ok(models) => {
                any_ok = true;
                rates_catalog(models)
            }
            Err(error) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(::serde_json::json!({ "error": format!("{error}") })),
                    "live pricing: refresh failed for gateway; falling back to models.dev"
                );
                HashMap::new()
            }
        };
        gateway_results.push((group.wanted.as_slice(), catalog));
    }

    // ── models.dev fallback (one fresh catalog fetch per cycle) ──
    // Only the keys for models their own gateway left unpriced: the same
    // `match_pricing` probe `assemble_snapshot` merges by.
    let mut needed_keys: Vec<&str> = gateway_results
        .iter()
        .flat_map(|(wanted, catalog)| {
            wanted.iter().filter_map(move |w| {
                if match_pricing(catalog, &w.model_id).is_some() {
                    None
                } else {
                    w.models_dev_key.as_deref()
                }
            })
        })
        .collect();
    needed_keys.sort_unstable();
    needed_keys.dedup();

    let mut models_dev: HashMap<String, HashMap<String, ModelRates>> = HashMap::new();
    if !needed_keys.is_empty() {
        // Fetch fresh (not the process-cached catalog) so the fallback tracks
        // upstream price changes on the same cadence as the gateways.
        match crate::models_dev::fetch_catalog().await {
            Ok(catalog) => {
                for key in needed_keys {
                    let map = crate::models_dev::pricing_from_catalog(&catalog, key);
                    if !map.is_empty() {
                        any_ok = true;
                        models_dev.insert(key.to_string(), map);
                    }
                }
            }
            Err(error) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(::serde_json::json!({ "error": format!("{error}") })),
                    "live pricing: models.dev fallback fetch failed; keeping previous snapshot"
                );
            }
        }
    }

    if any_ok {
        store_snapshot(assemble_snapshot(
            &gateway_results,
            &models_dev,
            total_aliases_per_family,
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup_keyed_by_family_with_bare_type_fallback() {
        let mut snap = PriceSnapshot::new();
        // Keyed by provider family `<type>`, NOT the `<type>.<alias>` composite:
        // this is the bare name the cost path resolves to (see `provider_pricing`).
        snap.entry("opencode".to_string()).or_default().insert(
            "minimax-m2.5".to_string(),
            ModelRates {
                input_per_mtok: Some(0.3),
                output_per_mtok: Some(1.2),
                cached_input_per_mtok: Some(0.06),
            },
        );
        // The cost path passes the bare family: direct hit.
        let hit = lookup(&snap, "opencode", "minimax-m2.5").expect("family hit");
        assert_eq!(hit.input_per_mtok, Some(0.3));
        // A `<type>.<alias>` ref falls back to the bare family (provider_pricing parity).
        assert!(lookup(&snap, "opencode.zen", "minimax-m2.5").is_some());
        // A recorded `vendor/slug` id degrades to the stored slug.
        assert!(lookup(&snap, "opencode", "minimax/minimax-m2.5").is_some());
        assert!(lookup(&snap, "opencode", "other-model").is_none());
        assert!(lookup(&snap, "other", "minimax-m2.5").is_none());
    }

    #[test]
    fn model_id_candidates_yield_verbatim_then_suffix() {
        assert_eq!(
            model_id_candidates("anthropic/claude-sonnet-4-5").collect::<Vec<_>>(),
            vec!["anthropic/claude-sonnet-4-5", "claude-sonnet-4-5"]
        );
        assert_eq!(
            model_id_candidates("minimax-m2.5").collect::<Vec<_>>(),
            vec!["minimax-m2.5"]
        );
    }

    #[test]
    fn normalize_pricing_scales_per_token_strings_to_mtok() {
        // 0.000003 USD/token -> 3.0 USD per 1M tokens; absent/garbage -> None.
        let p = ModelPricing {
            prompt: Some("0.000003".into()),
            completion: Some("0.000015".into()),
            input_cache_read: Some("0.0000003".into()),
            input_cache_write: None,
        };
        let r = normalize_pricing(&p);
        assert_eq!(r.input_per_mtok, Some(3.0));
        assert_eq!(r.output_per_mtok, Some(15.0));
        assert_eq!(r.cached_input_per_mtok, Some(0.3));

        let empty = ModelPricing {
            prompt: Some("abc".into()),
            completion: None,
            input_cache_read: Some("".into()),
            input_cache_write: None,
        };
        assert!(normalize_pricing(&empty).is_empty());

        // An absurd per-token value ("5" -> $5M per 1M tokens) is rejected by
        // the sanity ceiling rather than billed.
        let absurd = ModelPricing {
            prompt: Some("5".into()),
            completion: None,
            input_cache_read: None,
            input_cache_write: None,
        };
        assert!(normalize_pricing(&absurd).is_empty());
    }

    #[test]
    fn current_snapshot_is_synchronous_and_non_blocking() {
        // Reading the global must never block or require an async runtime:
        // this whole test runs without a tokio runtime.
        store_snapshot(PriceSnapshot::new());
        for _ in 0..10_000 {
            let snap = current_snapshot();
            assert!(lookup(&snap, "x.y", "z").is_none());
        }
    }

    fn want(composite: &str, model: &str, md_key: Option<&str>) -> WantedModel {
        WantedModel {
            // provider_key is the composite alias, matching what
            // enabled_pricing_groups now stores (not the bare family).
            provider_key: composite.to_string(),
            composite: composite.to_string(),
            model_id: model.to_string(),
            models_dev_key: md_key.map(str::to_string),
        }
    }

    fn rate(input: f64) -> ModelRates {
        ModelRates {
            input_per_mtok: Some(input),
            output_per_mtok: None,
            cached_input_per_mtok: None,
        }
    }

    fn full_rate(input: Option<f64>, output: Option<f64>, cached: Option<f64>) -> ModelRates {
        ModelRates {
            input_per_mtok: input,
            output_per_mtok: output,
            cached_input_per_mtok: cached,
        }
    }

    #[test]
    fn assemble_gateway_wins_then_models_dev_fills_gaps() {
        let wanted = vec![
            want("kilo.a", "minimax/m2.7", Some("kilo")), // priced by gateway
            want("kilo.b", "only-on-mdev", Some("kilo")), // gateway blank → models.dev
            want("kilo.c", "vendor/slug", Some("kilo")),  // suffix match on gateway
            want("kilo.d", "nowhere", Some("kilo")),      // neither source
            want("kilo.f", "partial", Some("kilo")),      // gateway in+out, mdev cache
            want("x.e", "no-key", None),                  // no fallback key
        ];
        let mut gateway = HashMap::new();
        gateway.insert("minimax/m2.7".to_string(), rate(0.3));
        gateway.insert("slug".to_string(), rate(0.7)); // matched via suffix
        // Gateway prices input+output for `partial` but leaves cache_read unset.
        gateway.insert("partial".to_string(), full_rate(Some(2.0), Some(4.0), None));
        let gateway_results = vec![(wanted.as_slice(), gateway)];

        let mut md = HashMap::new();
        // models.dev also lists the gateway-priced model at a different rate;
        // the gateway must win.
        md.insert("minimax/m2.7".to_string(), rate(9.9));
        md.insert("only-on-mdev".to_string(), rate(0.5));
        // models.dev has all three for `partial`; only cache_read should be used.
        md.insert(
            "partial".to_string(),
            full_rate(Some(9.9), Some(9.9), Some(0.5)),
        );
        let models_dev = HashMap::from([("kilo".to_string(), md)]);

        // All 5 kilo aliases and the 1 x alias are in the wanted list (all opted
        // in), so bare-family entries are added for "kilo" (5==5) but not "x"
        // (x.e priced nothing, so "x" never gets a non-empty family map).
        let total = HashMap::from([("kilo".to_string(), 5usize), ("x".to_string(), 1usize)]);
        let snap = assemble_snapshot(&gateway_results, &models_dev, &total);

        // All aliases opted in → bare "kilo" entry exists (backward compat path).
        assert_eq!(snap["kilo"]["minimax/m2.7"].input_per_mtok, Some(0.3));
        assert_eq!(snap["kilo"]["only-on-mdev"].input_per_mtok, Some(0.5));
        assert_eq!(snap["kilo"]["vendor/slug"].input_per_mtok, Some(0.7));
        // Per-dimension merge: gateway wins input+output, models.dev backfills
        // ONLY the cache_read dimension the gateway left unset.
        let partial = &snap["kilo"]["partial"];
        assert_eq!(partial.input_per_mtok, Some(2.0)); // gateway
        assert_eq!(partial.output_per_mtok, Some(4.0)); // gateway
        assert_eq!(partial.cached_input_per_mtok, Some(0.5)); // models.dev backfill
        // Neither source: absent from the family map (left at $0).
        assert!(!snap["kilo"].contains_key("nowhere"));
        // Composite alias keys are also present.
        assert_eq!(snap["kilo.a"]["minimax/m2.7"].input_per_mtok, Some(0.3));
        assert_eq!(snap["kilo.b"]["only-on-mdev"].input_per_mtok, Some(0.5));
        // The no-key case priced nothing; its composite key and bare "x" are absent.
        assert!(!snap.contains_key("x.e"));
        assert!(!snap.contains_key("x"));
    }

    #[test]
    fn sane_mtok_rejects_nonfinite_negative_and_absurd() {
        assert_eq!(sane_mtok(3.0), Some(3.0));
        assert_eq!(sane_mtok(0.0), Some(0.0));
        assert_eq!(sane_mtok(MAX_SANE_PER_MTOK), Some(MAX_SANE_PER_MTOK));
        assert!(sane_mtok(-1.0).is_none());
        assert!(sane_mtok(f64::INFINITY).is_none());
        assert!(sane_mtok(f64::NAN).is_none());
        // Above the ceiling ($1/token) -> rejected (parsing artifact / hostile gateway).
        assert!(sane_mtok(MAX_SANE_PER_MTOK * 2.0).is_none());
    }

    // Alias-boundary regression test 1: when one alias opts in and another does
    // not, the non-opted-in alias must NOT receive live prices -- neither by
    // composite lookup nor by the bare-family split_once fallback.
    #[test]
    fn alias_boundary_non_opted_in_alias_gets_no_live_pricing() {
        // kilo.primary opted in; kilo.secondary did NOT.
        // Total kilo aliases: 2. Only 1 opted in -> no bare "kilo" entry.
        let wanted = vec![want("kilo.primary", "minimax-m2.7", Some("kilo"))];
        let mut gateway = HashMap::new();
        gateway.insert("minimax-m2.7".to_string(), rate(0.3));
        let gateway_results = vec![(wanted.as_slice(), gateway)];
        let models_dev = HashMap::new();
        let total = HashMap::from([("kilo".to_string(), 2usize)]);

        let snap = assemble_snapshot(&gateway_results, &models_dev, &total);

        // kilo.primary (opted in) has composite rates.
        assert_eq!(
            snap["kilo.primary"]["minimax-m2.7"].input_per_mtok,
            Some(0.3)
        );
        // No bare "kilo" entry: mixed opt-in state means the bare-family lookup
        // must not surface kilo.primary's rates to kilo.secondary.
        assert!(
            !snap.contains_key("kilo"),
            "bare-family key must not exist when not all aliases opted in"
        );
        // A kilo.secondary composite lookup finds nothing (it never opted in).
        assert!(
            lookup(&snap, "kilo.secondary", "minimax-m2.7").is_none(),
            "non-opted-in alias must not inherit prices via composite lookup"
        );
        // A bare-family lookup for "kilo" also finds nothing.
        assert!(
            lookup(&snap, "kilo", "minimax-m2.7").is_none(),
            "bare-family lookup must not return prices when only some aliases opted in"
        );
    }

    // Alias-boundary regression test 2: when two aliases of the same family opt
    // in through different gateways with different rates for the same model, each
    // alias retains its own source rates in the snapshot (no cross-alias collapse).
    #[test]
    fn alias_boundary_two_opted_in_aliases_have_own_source_rates() {
        let wanted = [
            want("kilo.primary", "minimax-m2.7", Some("kilo")),
            want("kilo.secondary", "minimax-m2.7", Some("kilo")),
        ];
        // Two gateway groups: primary sees $0.30/MTok, secondary sees $0.90/MTok.
        let mut gateway_a = HashMap::new();
        gateway_a.insert("minimax-m2.7".to_string(), rate(0.3));
        let mut gateway_b = HashMap::new();
        gateway_b.insert("minimax-m2.7".to_string(), rate(0.9));
        // Each alias is its own gateway group (different URIs / gateways).
        let primary_slice = std::slice::from_ref(&wanted[0]);
        let secondary_slice = std::slice::from_ref(&wanted[1]);
        let gateway_results = vec![(primary_slice, gateway_a), (secondary_slice, gateway_b)];
        let models_dev = HashMap::new();
        // Both aliases opted in (2 == 2 total) -> bare "kilo" entry is added too.
        let total = HashMap::from([("kilo".to_string(), 2usize)]);

        let snap = assemble_snapshot(&gateway_results, &models_dev, &total);

        // Each alias retains its own gateway's rates in the composite slot.
        assert_eq!(
            snap["kilo.primary"]["minimax-m2.7"].input_per_mtok,
            Some(0.3),
            "kilo.primary must use its own gateway rate"
        );
        assert_eq!(
            snap["kilo.secondary"]["minimax-m2.7"].input_per_mtok,
            Some(0.9),
            "kilo.secondary must use its own gateway rate"
        );
        // Composite lookup preserves per-alias isolation.
        assert_eq!(
            lookup(&snap, "kilo.primary", "minimax-m2.7").and_then(|r| r.input_per_mtok),
            Some(0.3)
        );
        assert_eq!(
            lookup(&snap, "kilo.secondary", "minimax-m2.7").and_then(|r| r.input_per_mtok),
            Some(0.9)
        );
        // The bare "kilo" entry exists (all aliases opted in) and carries merged
        // rates (first-wins per dimension); bare-family cost-path queries resolve.
        assert!(
            snap.contains_key("kilo"),
            "bare-family entry must exist when all aliases opted in"
        );
        assert!(lookup(&snap, "kilo", "minimax-m2.7").is_some());
    }
}
