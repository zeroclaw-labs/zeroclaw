use axum::extract::Request;
use axum::http::HeaderValue;
use axum::http::header::{HeaderMap, HeaderName};
use axum::middleware::Next;
use axum::response::Response;

const SECURITY_HEADERS: &[(&str, &str)] = &[
    ("x-content-type-options", "nosniff"),
    ("x-frame-options", "DENY"),
    ("referrer-policy", "no-referrer"),
    (
        "content-security-policy",
        "default-src 'self'; \
         script-src 'self' 'unsafe-inline'; \
         style-src 'self' 'unsafe-inline'; \
         img-src 'self' data:; \
         font-src 'self'; \
         connect-src 'self' ws: wss:; \
         object-src 'none'; \
         frame-ancestors 'none'; \
         base-uri 'none'; \
         form-action 'self'",
    ),
    ("cross-origin-opener-policy", "same-origin"),
    ("cross-origin-resource-policy", "same-origin"),
    ("x-permitted-cross-domain-policies", "none"),
    (
        "permissions-policy",
        "geolocation=(), microphone=(), camera=()",
    ),
];

const HSTS_HEADER: (&str, &str) = (
    "strict-transport-security",
    "max-age=63072000; includeSubDomains",
);

pub async fn apply(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    inject(response.headers_mut(), false);
    response
}

pub async fn apply_with_hsts(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    inject(response.headers_mut(), true);
    response
}

pub(crate) fn inject(headers: &mut HeaderMap, hsts: bool) {
    for (name, value) in SECURITY_HEADERS {
        set_if_absent(headers, name, value);
    }
    if hsts {
        set_if_absent(headers, HSTS_HEADER.0, HSTS_HEADER.1);
    }
}

fn set_if_absent(headers: &mut HeaderMap, name: &'static str, value: &'static str) {
    let header_name = HeaderName::from_static(name);
    if headers.contains_key(&header_name) {
        return;
    }
    headers.insert(header_name, HeaderValue::from_static(value));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn injects_baseline_headers_without_hsts() {
        let mut headers = HeaderMap::new();
        inject(&mut headers, false);
        assert_eq!(headers.get("x-content-type-options").unwrap(), "nosniff");
        assert_eq!(headers.get("x-frame-options").unwrap(), "DENY");
        assert!(headers.get("strict-transport-security").is_none());
    }

    #[test]
    fn csp_permits_same_origin_dashboard_assets() {
        let mut headers = HeaderMap::new();
        inject(&mut headers, false);
        let csp = headers
            .get("content-security-policy")
            .unwrap()
            .to_str()
            .unwrap();
        assert!(csp.contains("default-src 'self'"));
        assert!(csp.contains("script-src 'self'"));
        assert!(csp.contains("connect-src 'self' ws: wss:"));
        assert!(csp.contains("frame-ancestors 'none'"));
    }

    #[test]
    fn injects_hsts_only_when_requested() {
        let mut headers = HeaderMap::new();
        inject(&mut headers, true);
        assert_eq!(
            headers.get("strict-transport-security").unwrap(),
            "max-age=63072000; includeSubDomains"
        );
    }

    #[test]
    fn preserves_existing_header_values() {
        let mut headers = HeaderMap::new();
        headers.insert(
            HeaderName::from_static("content-security-policy"),
            HeaderValue::from_static("default-src 'self'"),
        );
        inject(&mut headers, false);
        assert_eq!(
            headers.get("content-security-policy").unwrap(),
            "default-src 'self'"
        );
    }
}
