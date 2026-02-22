use serde_json::{json, Value};

/// Build a single route entry for Caddy's config API.
/// Matches `hostname` and reverse-proxies to `127.0.0.1:{port}`.
pub fn build_route(hostname: &str, port: u16) -> Value {
    json!({
        "match": [
            {
                "host": [hostname]
            }
        ],
        "handle": [
            {
                "handler": "reverse_proxy",
                "upstreams": [
                    {
                        "dial": format!("127.0.0.1:{}", port)
                    }
                ]
            }
        ],
        "terminal": true
    })
}

/// Build initial Caddy config JSON with platform API route.
/// Listens on :80 only (designed for Cloudflare Flexible SSL termination).
/// Routes both bare `{domain}` and `api.{domain}` to the platform.
pub fn build_initial_config(domain: &str, platform_port: u16) -> Value {
    json!({
        "admin": {
            "listen": "localhost:2019"
        },
        "apps": {
            "http": {
                "servers": {
                    "srv0": {
                        "listen": [":80"],
                        "routes": [
                            build_route(domain, platform_port),
                            build_route(&format!("api.{}", domain), platform_port)
                        ]
                    }
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_route_json_structure() {
        let route = build_route("tenant-a.example.com", 10001);

        // Verify match.host
        let host = &route["match"][0]["host"][0];
        assert_eq!(host, "tenant-a.example.com");

        // Verify handle.upstreams
        let dial = &route["handle"][0]["upstreams"][0]["dial"];
        assert_eq!(dial, "127.0.0.1:10001");

        // Verify handler type
        let handler = &route["handle"][0]["handler"];
        assert_eq!(handler, "reverse_proxy");

        // Verify terminal flag
        assert_eq!(route["terminal"], true);
    }

    #[test]
    fn test_build_route_port_formatting() {
        let route = build_route("foo.example.com", 10999);
        let dial = &route["handle"][0]["upstreams"][0]["dial"];
        assert_eq!(dial, "127.0.0.1:10999");
    }

    #[test]
    fn test_build_initial_config_structure() {
        let config = build_initial_config("example.com", 8080);

        // Verify NO TLS section (Cloudflare handles TLS)
        assert!(config["apps"]["tls"].is_null());

        // Verify two platform routes: bare domain + api subdomain
        let routes = &config["apps"]["http"]["servers"]["srv0"]["routes"];
        assert!(routes.is_array());
        assert_eq!(routes.as_array().unwrap().len(), 2);

        // Verify bare domain route
        let bare_host = &routes[0]["match"][0]["host"][0];
        assert_eq!(bare_host, "example.com");
        let bare_dial = &routes[0]["handle"][0]["upstreams"][0]["dial"];
        assert_eq!(bare_dial.as_str().unwrap(), "127.0.0.1:8080");

        // Verify api subdomain route
        let api_host = &routes[1]["match"][0]["host"][0];
        assert_eq!(api_host, "api.example.com");
        let api_dial = &routes[1]["handle"][0]["upstreams"][0]["dial"];
        assert_eq!(api_dial.as_str().unwrap(), "127.0.0.1:8080");

        // Verify server listens on HTTP only (Cloudflare Flexible SSL)
        let listen = &config["apps"]["http"]["servers"]["srv0"]["listen"];
        let ports: Vec<&str> = listen
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(ports, vec![":80"]);

        // Verify admin API is local-only
        assert_eq!(config["admin"]["listen"], "localhost:2019");
    }
}
