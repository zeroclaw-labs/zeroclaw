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
        "default-src 'none'; frame-ancestors 'none'; base-uri 'none'; form-action 'none'",
    ),
    ("cross-origin-opener-policy", "same-origin"),
    ("cross-origin-embedder-policy", "require-corp"),
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

fn inject(headers: &mut HeaderMap, hsts: bool) {
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
        assert!(headers.get("content-security-policy").is_some());
        assert!(headers.get("strict-transport-security").is_none());
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
