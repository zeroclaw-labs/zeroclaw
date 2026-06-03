//! Locale RPC methods: serve the in-memory locale registry and fetch
//! translated FTL catalogues from upstream.
//!
//! `locales/list` returns the build's embedded `locales.toml` registry — no
//! file read, no network. `locales/fetch` downloads catalogue bytes from the
//! upstream repository (URL built entirely from constants plus the validated
//! locale/catalog) and returns them so the client writes into its own config
//! dir. The locale is validated against the embedded registry and the catalog
//! against the fixed set, so neither can drive a request to an arbitrary host
//! or path.
//!
//! Every emission runs inside an attribution span (`channel = "rpc"`, the
//! caller's `tui_id` as `session_key`) so locale-fetch events are attributed to
//! the originating TUI session, never orphaned.

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
