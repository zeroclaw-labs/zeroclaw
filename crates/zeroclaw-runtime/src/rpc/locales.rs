//! Locale RPC methods: serve the in-memory locale registry and fetch
//! translated FTL catalogues from upstream.

use ::zeroclaw_log::Instrument as _;
use zeroclaw_api::jsonrpc::error_codes::*;
use zeroclaw_api::jsonrpc::{
    FetchedCatalog, JsonRpcError, LocaleOption, LocalesFetchRequest, LocalesFetchResponse,
    LocalesListResponse,
};

fn rpc_err(code: i32, msg: impl Into<String>) -> JsonRpcError {
    JsonRpcError {
        code,
        message: msg.into(),
        data: None,
    }
}

/// Attribution span keyed to the calling TUI session.
fn locale_span(tui_id: Option<&str>) -> ::zeroclaw_log::Span {
    ::zeroclaw_log::info_span!(
        target: "zeroclaw_log_internal_scope",
        "zeroclaw_scope",
        session_key = %tui_id.unwrap_or("rpc"),
        channel = "rpc",
    )
}

/// Handle `locales/list` — the embedded locale registry. No network.
pub fn handle_locales_list(tui_id: Option<&str>) -> Result<serde_json::Value, JsonRpcError> {
    let span = locale_span(tui_id);
    let _guard = span.enter();
    let locales: Vec<LocaleOption> = crate::i18n::available_locales()
        .iter()
        .map(|o| LocaleOption {
            code: o.code.clone(),
            label: o.label.clone(),
        })
        .collect();
    ::zeroclaw_log::record!(
        DEBUG,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
            .with_attrs(::serde_json::json!({ "count": locales.len() })),
        "locales/list served from embedded registry"
    );
    serde_json::to_value(LocalesListResponse { locales })
        .map_err(|e| rpc_err(INTERNAL_ERROR, e.to_string()))
}

/// Handle `locales/fetch` — download FTL catalogue bytes from upstream.
pub async fn handle_locales_fetch(
    params: &serde_json::Value,
    tui_id: Option<&str>,
) -> Result<serde_json::Value, JsonRpcError> {
    let span = locale_span(tui_id);
    async move {
        let req: LocalesFetchRequest = serde_json::from_value(params.clone())
            .map_err(|e| rpc_err(INVALID_PARAMS, e.to_string()))?;

        // Validate locale against the embedded registry + a syntactic allowlist.
        let locale = match validate_locale(&req.locale) {
            Ok(l) => l,
            Err(e) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({ "locale": req.locale })),
                    "locales/fetch rejected: locale not in registry or invalid shape"
                );
                return Err(e);
            }
        };

        // Select catalogues by name from the fixed table (never a caller path).
        let selected: Vec<&(&str, &str, &str)> = if req.catalog.is_empty() {
            zeroclaw_config::schema::FTL_CATALOGS.iter().collect()
        } else {
            let mut out = Vec::new();
            for name in &req.catalog {
                match zeroclaw_config::schema::FTL_CATALOGS
                    .iter()
                    .find(|(n, _, _)| n == name)
                {
                    Some(entry) => out.push(entry),
                    None => {
                        ::zeroclaw_log::record!(
                            WARN,
                            ::zeroclaw_log::Event::new(
                                module_path!(),
                                ::zeroclaw_log::Action::Reject
                            )
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                            .with_attrs(::serde_json::json!({ "catalog": name })),
                            "locales/fetch rejected: unknown catalog"
                        );
                        return Err(rpc_err(INVALID_PARAMS, format!("unknown catalog '{name}'")));
                    }
                }
            }
            out
        };

        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(
                ::serde_json::json!({
                    "locale": locale,
                    "catalogs": selected.iter().map(|(n, _, _)| *n).collect::<Vec<_>>(),
                })
            ),
            "locales/fetch started"
        );

        let version = env!("CARGO_PKG_VERSION");
        let refs = [format!("v{version}"), "master".to_string()];
        let client = reqwest::Client::new();

        let mut catalogs = Vec::new();
        let mut skipped = Vec::new();
        for (name, path_tmpl, out_name) in selected {
            let repo_path = path_tmpl.replace("{locale}", &locale);
            let mut content: Option<String> = None;
            for git_ref in &refs {
                let url = format!(
                    "https://raw.githubusercontent.com/zeroclaw-labs/zeroclaw/{git_ref}/{repo_path}"
                );
                let resp = match client.get(&url).send().await {
                    Ok(r) => r,
                    Err(e) => {
                        ::zeroclaw_log::record!(
                            WARN,
                            ::zeroclaw_log::Event::new(
                                module_path!(),
                                ::zeroclaw_log::Action::Note
                            )
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                            .with_attrs(::serde_json::json!({ "catalog": name, "url": url })),
                            "locales/fetch network error"
                        );
                        return Err(rpc_err(INTERNAL_ERROR, e.to_string()));
                    }
                };
                if resp.status().is_success() {
                    content = Some(
                        resp.text()
                            .await
                            .map_err(|e| rpc_err(INTERNAL_ERROR, e.to_string()))?,
                    );
                    break;
                }
            }
            match content {
                Some(c) => catalogs.push(FetchedCatalog {
                    name: (*name).to_string(),
                    filename: (*out_name).to_string(),
                    content: c,
                }),
                None => {
                    ::zeroclaw_log::record!(
                        DEBUG,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_attrs(::serde_json::json!({ "catalog": name, "locale": locale })),
                        "locales/fetch: catalogue not on upstream, skipped"
                    );
                    skipped.push((*name).to_string());
                }
            }
        }

        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(
                ::serde_json::json!({
                    "locale": locale,
                    "fetched": catalogs.len(),
                    "skipped": skipped,
                })
            ),
            "locales/fetch completed"
        );

        serde_json::to_value(LocalesFetchResponse {
            locale,
            catalogs,
            skipped,
        })
        .map_err(|e| rpc_err(INTERNAL_ERROR, e.to_string()))
    }
    .instrument(span)
    .await
}

/// Validate `locale` against the embedded registry and a strict syntactic
/// allowlist (no slashes/dots), defeating path traversal and host injection.
fn validate_locale(locale: &str) -> Result<String, JsonRpcError> {
    let ok_shape = !locale.is_empty()
        && locale.len() <= 16
        && locale
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-');
    if !ok_shape {
        return Err(rpc_err(
            INVALID_PARAMS,
            format!("invalid locale '{locale}'"),
        ));
    }
    if !crate::i18n::available_locales()
        .iter()
        .any(|o| o.code == locale)
    {
        return Err(rpc_err(
            INVALID_PARAMS,
            format!("locale '{locale}' not in registry"),
        ));
    }
    Ok(locale.to_string())
}

#[cfg(test)]
mod tests {
    //! The locale validator is the only thing standing between a hostile
    //! RPC payload and the raw GitHub URL we build from it. Any shape
    //! relaxation here would let a caller drive the fetch to an
    //! arbitrary host or path. Lock the boundaries down.

    use super::*;
    use serde_json::json;
    use zeroclaw_api::jsonrpc::LocalesFetchRequest;

    /// Pick a known locale from the embedded registry. Tests should
    /// never hardcode `en` — if the registry is ever trimmed, the
    /// tests catch it here instead of at the assertion site.
    fn known_locale() -> String {
        crate::i18n::available_locales()
            .first()
            .map(|o| o.code.clone())
            .expect("embedded locale registry is non-empty")
    }

    #[test]
    fn validate_locale_rejects_empty_string() {
        let err = validate_locale("").unwrap_err();
        assert_eq!(err.code, INVALID_PARAMS);
        assert!(err.message.contains("invalid locale"));
    }

    #[test]
    fn validate_locale_rejects_over_sixteen_chars() {
        // 17 ASCII alphanumeric chars — well-formed syntax but past the
        // 16-char cap that bounds downstream URL/path usage.
        let too_long = "a".repeat(17);
        let err = validate_locale(&too_long).unwrap_err();
        assert_eq!(err.code, INVALID_PARAMS);
    }

    #[test]
    fn validate_locale_rejects_path_separators_and_dots() {
        // The shape check is what stops `../etc/passwd` and `foo/bar`
        // from ever reaching the URL builder — every separator below
        // must be rejected.
        for bad in [
            "en/../etc",
            "en/../../x",
            "../etc",
            "en/US",
            "en.us",
            "en us",
            "en\n",
            "en\t",
            "日本語",
            "en!",
        ] {
            let err = match validate_locale(bad) {
                Ok(_) => panic!("expected rejection for {bad:?}"),
                Err(e) => e,
            };
            assert_eq!(err.code, INVALID_PARAMS, "wrong code for {bad:?}");
        }
    }

    #[test]
    fn validate_locale_rejects_well_formed_unknown_locale() {
        // Syntactically valid but absent from the registry — must
        // fail at the registry check, not the shape check.
        let err = validate_locale("zz").unwrap_err();
        assert_eq!(err.code, INVALID_PARAMS);
        assert!(
            err.message.contains("not in registry"),
            "registry-miss error path: {}",
            err.message
        );
    }

    #[test]
    fn validate_locale_accepts_every_embedded_locale() {
        // Each locale in the registry must round-trip through validate —
        // adding a locale to `locales.toml` must never silently break
        // fetch for it.
        for opt in crate::i18n::available_locales() {
            assert_eq!(
                validate_locale(&opt.code).unwrap(),
                opt.code,
                "registry locale {:?} rejected",
                opt.code
            );
        }
    }

    #[test]
    fn handle_locales_list_returns_embedded_registry() {
        let v = handle_locales_list(Some("tui-1")).unwrap();
        let locales = v.get("locales").and_then(|x| x.as_array()).unwrap();
        assert!(!locales.is_empty(), "registry should never be empty");
        let first = &locales[0];
        assert!(first.get("code").and_then(|c| c.as_str()).is_some());
        assert!(
            first.get("label").and_then(|c| c.as_str()).is_some(),
            "label missing from entry: {first}"
        );
    }

    #[tokio::test]
    async fn handle_locales_fetch_rejects_params_missing_locale_field() {
        // Empty object can't deserialize into `LocalesFetchRequest`
        // (locale is required, not defaulted) — must surface as
        // INVALID_PARAMS, not INTERNAL_ERROR.
        let err = handle_locales_fetch(&json!({}), Some("tui-1"))
            .await
            .unwrap_err();
        assert_eq!(err.code, INVALID_PARAMS);
    }

    #[tokio::test]
    async fn handle_locales_fetch_rejects_invalid_locale_shape() {
        let err = handle_locales_fetch(&json!({"locale": "en/../etc"}), Some("tui-1"))
            .await
            .unwrap_err();
        assert_eq!(err.code, INVALID_PARAMS);
        assert!(
            err.message.contains("invalid locale"),
            "shape-rejection path: {}",
            err.message
        );
    }

    #[tokio::test]
    async fn handle_locales_fetch_rejects_unknown_locale() {
        let err = handle_locales_fetch(&json!({"locale": "zz"}), Some("tui-1"))
            .await
            .unwrap_err();
        assert_eq!(err.code, INVALID_PARAMS);
        assert!(
            err.message.contains("not in registry"),
            "registry-miss path: {}",
            err.message
        );
    }

    #[tokio::test]
    async fn handle_locales_fetch_rejects_unknown_catalog() {
        // Valid locale, but the catalog isn't in the fixed
        // `FTL_CATALOGS` table. Rejection must happen *before* the
        // HTTP request — if it didn't, we'd be testing the network.
        let err = handle_locales_fetch(
            &json!({
                "locale": known_locale(),
                "catalog": ["definitely-not-a-real-catalog"]
            }),
            Some("tui-1"),
        )
        .await
        .unwrap_err();
        assert_eq!(err.code, INVALID_PARAMS);
        assert!(
            err.message.contains("unknown catalog"),
            "expected catalog-rejection path, got: {}",
            err.message
        );
    }

    #[tokio::test]
    async fn locales_fetch_request_deserializes_with_default_empty_catalog() {
        // The handler relies on `catalog` defaulting to empty when
        // the caller omits it — verify the type itself honors that,
        // since `handle_locales_fetch` would otherwise fall through
        // to the all-catalogs branch.
        let req: LocalesFetchRequest =
            serde_json::from_value(json!({"locale": known_locale()})).unwrap();
        assert!(req.catalog.is_empty());
    }
}
