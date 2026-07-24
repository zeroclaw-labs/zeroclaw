#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WebSearchProviderRoute {
    DuckDuckGo,
    Brave,
    SearXNG,
    Tavily,
    Jina,
    Bocha,
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

pub const DEFAULT_WEB_SEARCH_PROVIDER: &str = "duckduckgo";
const BRAVE_PROVIDER: &str = "brave";
const SEARXNG_PROVIDER: &str = "searxng";
const TAVILY_PROVIDER: &str = "tavily";
const JINA_PROVIDER: &str = "jina";
const BOCHA_PROVIDER: &str = "bocha";

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
        "jina" | "jina-ai" | "jina_ai" => WebSearchProviderResolution {
            route: WebSearchProviderRoute::Jina,
            canonical_provider: JINA_PROVIDER,
            used_fallback: false,
        },
        "bocha" | "bochaai" | "bocha-ai" | "bocha_ai" | "bocha-search" | "bocha_search" => {
            WebSearchProviderResolution {
                route: WebSearchProviderRoute::Bocha,
                canonical_provider: BOCHA_PROVIDER,
                used_fallback: false,
            }
        }
        // Warns for unknown model_providers, falls back to default.
        // Known non-default model_providers: Brave, SearXNG, Tavily, Jina, Bocha.
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
    fn resolve_aliases_to_tavily() {
        let tavily_aliases = ["tavily", "tavily-search", "tavily_search"];
        for alias in tavily_aliases {
            let resolved = resolve_web_search_provider(alias);
            assert_eq!(resolved.route, WebSearchProviderRoute::Tavily);
            assert_eq!(resolved.canonical_provider, TAVILY_PROVIDER);
            assert!(!resolved.used_fallback);
        }
    }

    #[test]
    fn resolve_aliases_to_jina() {
        let jina_aliases = ["jina", "jina-ai", "jina_ai"];
        for alias in jina_aliases {
            let resolved = resolve_web_search_provider(alias);
            assert_eq!(resolved.route, WebSearchProviderRoute::Jina);
            assert_eq!(resolved.canonical_provider, JINA_PROVIDER);
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
            assert_eq!(resolved.canonical_provider, BOCHA_PROVIDER);
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

    #[test]
    fn empty_and_default_route_to_duckduckgo_without_fallback() {
        for alias in ["", "default"] {
            let r = resolve_web_search_provider(alias);
            assert_eq!(r.route, WebSearchProviderRoute::DuckDuckGo);
            assert_eq!(r.canonical_provider, DEFAULT_WEB_SEARCH_PROVIDER);
            // An explicit empty / "default" is the configured default, not an
            // unknown-provider fallback (so it must not set used_fallback).
            assert!(!r.used_fallback);
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
