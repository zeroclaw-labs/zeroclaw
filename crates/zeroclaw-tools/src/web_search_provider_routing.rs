#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WebSearchProviderRoute {
    DuckDuckGo,
    Brave,
    SearXNG,
    Tavily,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WebSearchProviderResolution {
    pub route: WebSearchProviderRoute,
    pub canonical_provider: &'static str,
    pub used_fallback: bool,
}

pub const DEFAULT_WEB_SEARCH_PROVIDER: &str = "duckduckgo";
const BRAVE_PROVIDER: &str = "brave";
const SEARXNG_PROVIDER: &str = "searxng";
const TAVILY_PROVIDER: &str = "tavily";

pub fn resolve_web_search_provider(raw_model_provider: &str) -> WebSearchProviderResolution {
    let normalized = raw_model_provider.trim().to_ascii_lowercase();
    match normalized.as_str() {
        "" | "default" | "duckduckgo" | "ddg" | "duck-duck-go" | "duck_duck_go" => {
            WebSearchProviderResolution {
                route: WebSearchProviderRoute::DuckDuckGo,
                canonical_provider: DEFAULT_WEB_SEARCH_PROVIDER,
                used_fallback: false,
            }
        }
        "brave" | "brave-search" | "brave_search" => WebSearchProviderResolution {
            route: WebSearchProviderRoute::Brave,
            canonical_provider: BRAVE_PROVIDER,
            used_fallback: false,
        },
        "searxng" | "searx" | "searx-ng" | "searx_ng" => WebSearchProviderResolution {
            route: WebSearchProviderRoute::SearXNG,
            canonical_provider: SEARXNG_PROVIDER,
            used_fallback: false,
        },
        "tavily" | "tavily-search" | "tavily_search" => WebSearchProviderResolution {
            route: WebSearchProviderRoute::Tavily,
            canonical_provider: TAVILY_PROVIDER,
            used_fallback: false,
        },
        // Warns for unknown model_providers, falls back to default.
        // Known non-default model_providers: Brave, SearXNG, Tavily.
        _ => WebSearchProviderResolution {
            route: WebSearchProviderRoute::DuckDuckGo,
            canonical_provider: DEFAULT_WEB_SEARCH_PROVIDER,
            used_fallback: true,
        },
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
            assert_eq!(resolved.canonical_provider, DEFAULT_WEB_SEARCH_PROVIDER);
            assert!(!resolved.used_fallback);
        }
    }

    #[test]
    fn resolve_aliases_to_brave() {
        let brave_aliases = ["brave", "brave-search", "brave_search"];
        for alias in brave_aliases {
            let resolved = resolve_web_search_provider(alias);
            assert_eq!(resolved.route, WebSearchProviderRoute::Brave);
            assert_eq!(resolved.canonical_provider, BRAVE_PROVIDER);
            assert!(!resolved.used_fallback);
        }
    }

    #[test]
    fn resolve_aliases_to_searxng() {
        let searxng_aliases = ["searxng", "searx", "searx-ng", "searx_ng"];
        for alias in searxng_aliases {
            let resolved = resolve_web_search_provider(alias);
            assert_eq!(resolved.route, WebSearchProviderRoute::SearXNG);
            assert_eq!(resolved.canonical_provider, SEARXNG_PROVIDER);
            assert!(!resolved.used_fallback);
        }
    }

    #[test]
    fn resolve_unknown_provider_falls_back_to_default() {
        let resolved = resolve_web_search_provider("bing");
        assert_eq!(resolved.route, WebSearchProviderRoute::DuckDuckGo);
        assert_eq!(resolved.canonical_provider, DEFAULT_WEB_SEARCH_PROVIDER);
        assert!(resolved.used_fallback);

        let resolved2 = resolve_web_search_provider("searxng-plus");
        assert_eq!(resolved2.route, WebSearchProviderRoute::DuckDuckGo);
        assert_eq!(resolved2.canonical_provider, DEFAULT_WEB_SEARCH_PROVIDER);
        assert!(resolved2.used_fallback);
    }
}
