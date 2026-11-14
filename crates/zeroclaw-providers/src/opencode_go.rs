//! OpenCode Go catalogue policy. The configured endpoint selects the service;
//! an operator's alias name does not. Capacity comes from the catalogue at use
//! time; no second per-model table or runtime cache is introduced.

use serde_json::Value;
use zeroclaw_config::schema::ModelProviderConfig;

pub(crate) const BASE_URL: &str = "https://opencode.ai/zen/go/v1";
const CURRENT_CATALOG_BASE_URL: &str = "https://opencode.ai/inference/go/v1";

/// Accept only the documented Go origins and paths. Never infer Go from an
/// alias, a host suffix, a query string, or an unrelated compatible endpoint.
pub(crate) fn catalog_base_url(base: &str) -> Option<&'static str> {
    let url = reqwest::Url::parse(base).ok()?;
    if url.scheme() != "https"
        || url.host_str() != Some("opencode.ai")
        || url.port_or_known_default() != Some(443)
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return None;
    }
    match url.path().trim_end_matches('/') {
        "/zen/go/v1" => Some(BASE_URL),
        "/inference/go/v1" | "/inference/go/openai/v1" => Some(CURRENT_CATALOG_BASE_URL),
        _ => None,
    }
}

pub(crate) fn context_window_from_limit(limit: &Value) -> Option<usize> {
    crate::models_dev::sane_context_window(limit.get("context")?.as_u64()?)
}

fn context_window_from_catalog(catalog: &Value, model: &str) -> Option<usize> {
    let mut matches = catalog
        .get("data")?
        .as_array()?
        .iter()
        .filter(|entry| entry.get("id").and_then(Value::as_str) == Some(model));
    let entry = matches.next()?;
    // Conflicting or duplicated entries are unknown, not a reason to guess.
    if matches.next().is_some() {
        return None;
    }
    context_window_from_limit(entry.get("limit")?)
}

async fn fetch_context_window_at(url: &str, model: &str) -> Option<usize> {
    // This catalogue is public. Do not forward the configured inference key,
    // and do not follow a redirect away from the selected catalogue.
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .ok()?;
    let response = client.get(url).send().await.ok()?;
    if !response.status().is_success() {
        return None;
    }
    let bytes =
        crate::compatible::read_body_capped(response, crate::compatible::MAX_MODELS_RESPONSE_BYTES)
            .await
            .ok()?;
    let catalog = serde_json::from_slice(&bytes).ok()?;
    context_window_from_catalog(&catalog, model)
}

pub(crate) async fn fetch_context_window(config: &ModelProviderConfig) -> Option<usize> {
    let base = catalog_base_url(config.uri.as_deref()?)?;
    let model = config.model.as_deref()?.trim();
    if model.is_empty() {
        return None;
    }
    fetch_context_window_at(&format!("{base}/models"), model).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn opencode_go_catalog_selection_requires_the_exact_service() {
        for base in [BASE_URL, "https://opencode.ai:443/zen/go/v1/"] {
            assert_eq!(catalog_base_url(base), Some(BASE_URL));
        }
        for base in [
            CURRENT_CATALOG_BASE_URL,
            "https://opencode.ai/inference/go/openai/v1/",
        ] {
            assert_eq!(catalog_base_url(base), Some(CURRENT_CATALOG_BASE_URL));
        }
        for base in [
            "https://opencode.ai/zen/v1",
            "https://opencode.ai/zen/go/v10",
            "http://opencode.ai/zen/go/v1",
            "https://opencode.ai:444/zen/go/v1",
            "https://opencode.ai.example.invalid/zen/go/v1",
            "https://user:password@opencode.ai/zen/go/v1",
            "https://opencode.ai/zen/go/v1?token=synthetic",
            "https://opencode.ai/zen/go/v1#fragment",
        ] {
            assert_eq!(catalog_base_url(base), None, "{base}");
        }
    }

    #[test]
    fn opencode_go_capacity_uses_exact_model_and_valid_limit() {
        let catalog = json!({"data": [
            {"id": "constructed-small", "limit": {"context": 32000}},
            {"id": "constructed-large", "limit": {"context": 1000000}}
        ]});
        assert_eq!(
            context_window_from_catalog(&catalog, "constructed-large"),
            Some(1000000)
        );
        assert_eq!(context_window_from_catalog(&catalog, "constructed"), None);
        for value in [
            json!(null),
            json!(0),
            json!(-1),
            json!("1000"),
            json!(100000001_u64),
        ] {
            let invalid = json!({"data": [{"id": "constructed", "limit": {"context": value}}]});
            assert_eq!(context_window_from_catalog(&invalid, "constructed"), None);
        }
        assert_eq!(
            context_window_from_catalog(
                &json!({"data": [
                    {"id": "constructed", "limit": {"context": 1000}},
                    {"id": "constructed", "limit": {"context": 2000}}
                ]}),
                "constructed"
            ),
            None
        );
    }

    #[test]
    fn opencode_go_configured_profile_reaches_the_go_factory() {
        use zeroclaw_config::schema::{Config, OpencodeModelProviderConfig};
        let mut config = Config::default();
        config.providers.models.opencode.insert(
            "constructed-alias".into(),
            OpencodeModelProviderConfig {
                base: ModelProviderConfig {
                    uri: Some(BASE_URL.into()),
                    model: Some("constructed-model".into()),
                    ..ModelProviderConfig::default()
                },
                ..OpencodeModelProviderConfig::default()
            },
        );
        let options =
            crate::provider_runtime_options_for_alias(&config, "opencode", "constructed-alias");
        let provider = crate::create_model_provider_for_alias(
            &config,
            "opencode",
            "constructed-alias",
            None,
            &options,
        )
        .unwrap();
        assert_eq!(provider.default_base_url(), Some(BASE_URL));
    }

    #[tokio::test]
    async fn opencode_go_discovery_leaves_other_profiles_unknown() {
        for uri in [
            None,
            Some("https://opencode.ai/zen/v1"),
            Some("http://127.0.0.1:1/v1"),
        ] {
            let config = ModelProviderConfig {
                uri: uri.map(str::to_owned),
                model: Some("constructed".into()),
                api_key: Some("synthetic-key-must-not-leave".into()),
                ..ModelProviderConfig::default()
            };
            assert_eq!(crate::fetch_context_window("opencode", &config).await, None);
        }
    }

    #[tokio::test]
    async fn opencode_go_catalog_transport_is_anonymous_and_rejects_redirects() {
        use axum::{
            Json, Router,
            http::{HeaderMap, StatusCode},
            response::Redirect,
            routing::get,
        };
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };
        let target_hits = Arc::new(AtomicUsize::new(0));
        let hits = target_hits.clone();
        let app = Router::new()
            .route(
                "/models",
                get(|headers: HeaderMap| async move {
                    assert!(!headers.contains_key("authorization"));
                    Json(json!({"data": [{"id": "constructed", "limit": {"context": 64000}}]}))
                }),
            )
            .route(
                "/redirect",
                get(|| async { Redirect::temporary("/target") }),
            )
            .route(
                "/target",
                get(move || {
                    let hits = hits.clone();
                    async move {
                        hits.fetch_add(1, Ordering::SeqCst);
                        "unexpected"
                    }
                }),
            )
            .route(
                "/failure",
                get(|| async {
                    (
                        StatusCode::UNAUTHORIZED,
                        Json(json!({"data": [
                            {"id": "constructed", "limit": {"context": 64000}}
                        ]})),
                    )
                }),
            )
            .route("/invalid", get(|| async { "not JSON" }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = zeroclaw_spawn::spawn!(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let _stop = scopeguard::guard(server, |task| task.abort());
        assert_eq!(
            fetch_context_window_at(&format!("{base}/models"), "constructed").await,
            Some(64000)
        );
        for path in ["redirect", "failure", "invalid"] {
            assert_eq!(
                fetch_context_window_at(&format!("{base}/{path}"), "constructed").await,
                None
            );
        }
        assert_eq!(target_hits.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn opencode_go_legacy_factory_fetches_the_selected_catalog_without_a_key() {
        use axum::{Json, Router, http::HeaderMap, routing::get};
        let app = Router::new().route(
            "/v1/models",
            get(|headers: HeaderMap| async move {
                assert!(!headers.contains_key("authorization"));
                Json(json!({"data": [{"id": "constructed-go-only"}]}))
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/v1", listener.local_addr().unwrap());
        let server = zeroclaw_spawn::spawn!(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let _stop = scopeguard::guard(server, |task| task.abort());
        // Use the same factory as `models refresh --model-provider opencode-go`.
        // The explicit URL supplies a constructed server, never a vendor service.
        let provider =
            crate::create_model_provider_with_url("opencode-go", Some(""), Some(&base)).unwrap();
        assert_eq!(
            provider.list_models().await.unwrap(),
            vec!["constructed-go-only"]
        );
        let provider = crate::create_model_provider_with_options(
            "opencode-go",
            Some(""),
            &crate::ModelProviderRuntimeOptions {
                provider_api_url: Some(base),
                ..crate::ModelProviderRuntimeOptions::default()
            },
        )
        .unwrap();
        assert_eq!(
            provider.list_models().await.unwrap(),
            vec!["constructed-go-only"]
        );
    }
}
