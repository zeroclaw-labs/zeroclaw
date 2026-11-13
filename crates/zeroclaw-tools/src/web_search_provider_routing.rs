pub use zeroclaw_config::schema::DEFAULT_WEB_SEARCH_PROVIDER;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WebSearchProviderRoute {
    DuckDuckGo,
    Brave,
    SearXNG,
    Tavily,
    Jina,
    Bocha,
    AnySearch,
    Serply,
    Keenable,
    Parallel,
}

/// Provider HTTP-failure status surfaced to the agent via the error message's
/// `search_status=` tag. Only the classes `classify_http_status` actually
/// produces appear here — no speculative variants (wire-or-remove).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchStatus {
    Blocked,
    Unavailable,
    ClientError,
}

impl SearchStatus {
    /// Stable lowercase tag embedded in the agent-visible error message
    /// (`search_status=<tag>`). This is an error-text tag, not a structured
    /// wire or log-attr contract — the runtime forwards the error as opaque text.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Blocked => "blocked",
            Self::Unavailable => "unavailable",
            Self::ClientError => "client_error",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WebSearchProviderResolution {
    pub route: WebSearchProviderRoute,
    pub canonical_provider: &'static str,
    pub used_fallback: bool,
}

fn named_provider(name: &str) -> Option<(WebSearchProviderRoute, &'static str)> {
    use WebSearchProviderRoute as Route;

    Some(match name {
        "duckduckgo" | "ddg" | "duck-duck-go" | "duck_duck_go" => (Route::DuckDuckGo, "duckduckgo"),
        "brave" | "brave-search" | "brave_search" => (Route::Brave, "brave"),
        "searxng" | "searx" | "searx-ng" | "searx_ng" => (Route::SearXNG, "searxng"),
        "tavily" | "tavily-search" | "tavily_search" => (Route::Tavily, "tavily"),
        "jina" | "jina-ai" | "jina_ai" => (Route::Jina, "jina"),
        "bocha" | "bochaai" | "bocha-ai" | "bocha_ai" | "bocha-search" | "bocha_search" => {
            (Route::Bocha, "bocha")
        }
        "anysearch" | "any-search" | "any_search" => (Route::AnySearch, "anysearch"),
        "serply" | "serply-search" | "serply_search" | "serply-io" | "serply_io" => {
            (Route::Serply, "serply")
        }
        "keenable" | "keenable-search" | "keenable_search" => (Route::Keenable, "keenable"),
        "parallel" => (Route::Parallel, "parallel"),
        _ => return None,
    })
}

pub fn resolve_web_search_provider(raw_model_provider: &str) -> WebSearchProviderResolution {
    let normalized = raw_model_provider.trim().to_ascii_lowercase();
    let named = named_provider(&normalized);
    let used_fallback = named.is_none() && !matches!(normalized.as_str(), "" | "default");
    let (route, canonical_provider) = named.unwrap_or_else(|| {
        named_provider(DEFAULT_WEB_SEARCH_PROVIDER)
            .expect("the built-in default must name a provider")
    });
    WebSearchProviderResolution {
        route,
        canonical_provider,
        used_fallback,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_aliases_to_duckduckgo() {
        let ddg_aliases = ["duckduckgo", "ddg", "duck-duck-go", "duck_duck_go"];
        for alias in ddg_aliases {
            let resolved = resolve_web_search_provider(alias);
            assert_eq!(resolved.route, WebSearchProviderRoute::DuckDuckGo);
            assert_eq!(resolved.canonical_provider, "duckduckgo");
            assert!(!resolved.used_fallback);
        }
    }

    #[test]
    fn resolve_aliases_to_brave() {
        let brave_aliases = ["brave", "brave-search", "brave_search"];
        for alias in brave_aliases {
            let resolved = resolve_web_search_provider(alias);
            assert_eq!(resolved.route, WebSearchProviderRoute::Brave);
            assert_eq!(resolved.canonical_provider, "brave");
            assert!(!resolved.used_fallback);
        }
    }

    #[test]
    fn resolve_aliases_to_searxng() {
        let searxng_aliases = ["searxng", "searx", "searx-ng", "searx_ng"];
        for alias in searxng_aliases {
            let resolved = resolve_web_search_provider(alias);
            assert_eq!(resolved.route, WebSearchProviderRoute::SearXNG);
            assert_eq!(resolved.canonical_provider, "searxng");
            assert!(!resolved.used_fallback);
        }
    }

    #[test]
    fn resolve_aliases_to_tavily() {
        let tavily_aliases = ["tavily", "tavily-search", "tavily_search"];
        for alias in tavily_aliases {
            let resolved = resolve_web_search_provider(alias);
            assert_eq!(resolved.route, WebSearchProviderRoute::Tavily);
            assert_eq!(resolved.canonical_provider, "tavily");
            assert!(!resolved.used_fallback);
        }
    }

    #[test]
    fn resolve_aliases_to_jina() {
        let jina_aliases = ["jina", "jina-ai", "jina_ai"];
        for alias in jina_aliases {
            let resolved = resolve_web_search_provider(alias);
            assert_eq!(resolved.route, WebSearchProviderRoute::Jina);
            assert_eq!(resolved.canonical_provider, "jina");
            assert!(!resolved.used_fallback);
        }
    }

    #[test]
    fn resolve_aliases_to_bocha() {
        let bocha_aliases = [
            "bocha",
            "bochaai",
            "bocha-ai",
            "bocha_ai",
            "bocha-search",
            "bocha_search",
        ];
        for alias in bocha_aliases {
            let resolved = resolve_web_search_provider(alias);
            assert_eq!(resolved.route, WebSearchProviderRoute::Bocha);
            assert_eq!(resolved.canonical_provider, "bocha");
            assert!(!resolved.used_fallback);
        }
    }

    #[test]
    fn resolve_aliases_to_anysearch() {
        for alias in ["anysearch", "any-search", "any_search"] {
            let resolved = resolve_web_search_provider(alias);
            assert_eq!(resolved.route, WebSearchProviderRoute::AnySearch);
            assert_eq!(resolved.canonical_provider, "anysearch");
            assert!(!resolved.used_fallback);
        }
    }

    #[test]
    fn resolve_aliases_to_serply() {
        let serply_aliases = [
            "serply",
            "serply-search",
            "serply_search",
            "serply-io",
            "serply_io",
        ];
        for alias in serply_aliases {
            let resolved = resolve_web_search_provider(alias);
            assert_eq!(resolved.route, WebSearchProviderRoute::Serply);
            assert_eq!(resolved.canonical_provider, "serply");
            assert!(!resolved.used_fallback);
        }
    }

    #[test]
    fn resolve_aliases_to_keenable() {
        let keenable_aliases = ["keenable", "keenable-search", "keenable_search"];
        for alias in keenable_aliases {
            let resolved = resolve_web_search_provider(alias);
            assert_eq!(resolved.route, WebSearchProviderRoute::Keenable);
            assert_eq!(resolved.canonical_provider, "keenable");
            assert!(!resolved.used_fallback);
        }
    }

    #[test]
    fn default_and_explicit_parallel_route_without_fallback() {
        for alias in ["", "   ", "default", "  DEFAULT ", "parallel"] {
            let resolved = resolve_web_search_provider(alias);
            assert_eq!(resolved.route, WebSearchProviderRoute::Parallel);
            assert_eq!(resolved.canonical_provider, "parallel");
            assert!(!resolved.used_fallback);
        }
    }

    #[test]
    fn unknown_provider_falls_back_to_parallel() {
        for name in ["bing", "searxng-plus"] {
            let resolved = resolve_web_search_provider(name);
            assert_eq!(resolved.route, WebSearchProviderRoute::Parallel);
            assert_eq!(resolved.canonical_provider, "parallel");
            assert!(resolved.used_fallback);
        }
    }

    #[test]
    fn resolution_trims_whitespace_and_ignores_case() {
        let r = resolve_web_search_provider("  BRAVE  ");
        assert_eq!(r.route, WebSearchProviderRoute::Brave);
        assert!(!r.used_fallback);

        let r = resolve_web_search_provider("Tavily-Search");
        assert_eq!(r.route, WebSearchProviderRoute::Tavily);
        assert!(!r.used_fallback);
    }

    #[test]
    fn search_status_as_str_returns_stable_tags() {
        // The agent-visible error tag depends on these exact lowercase strings.
        assert_eq!(SearchStatus::Blocked.as_str(), "blocked");
        assert_eq!(SearchStatus::Unavailable.as_str(), "unavailable");
        assert_eq!(SearchStatus::ClientError.as_str(), "client_error");
    }
}
