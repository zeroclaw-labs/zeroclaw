//! Requests on a dashboard connection a relay forwarded to this daemon.
//!
//! The relay bridge hands such a connection over in process
//! ([`zeroclaw_runtime::relay_dashboard`]); the accept loop serves it like a
//! TCP connection, with three differences decided here, before routing:
//!
//! - **The peer is the browser.** `ConnectInfo` is the browser's address as
//!   the relay reported it, which is never loopback, so every loopback check
//!   in the gateway answers "remote" without knowing about the relay.
//! - **Forwarded headers are dropped.** The relay adds none, so any
//!   `X-Forwarded-*`, `X-Real-IP` or `Forwarded` header came from the
//!   browser. A gateway that trusts forwarded headers for a proxy in front
//!   of its own listener must not take a browser's word for its address or
//!   scheme, here or in any reader added later.
//! - **The origin is fixed.** A request whose `Host` is not the dashboard's
//!   is refused with 421, and the request carries a [`ViaRelay`] marker that
//!   the scheme, callback URI and own-origin decisions read instead of the
//!   request's `Host`.

use std::sync::Arc;

use axum::Json;
use axum::http::{HeaderMap, HeaderName, Request, StatusCode, header};
use axum::response::{IntoResponse, Response};
use zeroclaw_runtime::relay_dashboard::DashboardOrigin;

/// Marks a request that reached the gateway through a relay-served
/// dashboard, with the origin that dashboard is served at.
///
/// Only [`RelayConnection::admit`] inserts it, as a request extension, which
/// no client can set.
#[derive(Clone, Debug)]
pub(crate) struct ViaRelay {
    origin: Arc<str>,
}

impl ViaRelay {
    /// `https://<host>[:port]`, as a browser sends it in `Origin`.
    pub(crate) fn origin(&self) -> &str {
        &self.origin
    }
}

/// How one relay-forwarded connection's requests are admitted.
#[derive(Clone)]
pub(crate) struct RelayConnection {
    origin: DashboardOrigin,
    marker: ViaRelay,
}

impl RelayConnection {
    pub(crate) fn new(origin: DashboardOrigin) -> Self {
        let marker = ViaRelay {
            origin: Arc::from(origin.origin()),
        };
        Self { origin, marker }
    }

    /// Prepare `request` for routing, or return the response to send instead.
    pub(crate) fn admit<B>(&self, request: &mut Request<B>) -> Result<(), Box<Response>> {
        strip_forwarded(request.headers_mut());
        if !self.names_this_dashboard(request) {
            return Err(Box::new(misdirected(&self.origin)));
        }
        request.extensions_mut().insert(self.marker.clone());
        Ok(())
    }

    /// Exactly one `Host`, naming this dashboard, and no other authority in
    /// an absolute-form request target.
    fn names_this_dashboard<B>(&self, request: &Request<B>) -> bool {
        let mut hosts = request.headers().get_all(header::HOST).iter();
        let (Some(host), None) = (hosts.next(), hosts.next()) else {
            return false;
        };
        let Ok(host) = host.to_str() else {
            return false;
        };
        self.origin.is_named_by(host)
            && request
                .uri()
                .authority()
                .is_none_or(|authority| self.origin.is_named_by(authority.as_str()))
    }
}

/// Remove every header a proxy would use to report the client's address,
/// scheme or host.
pub(crate) fn strip_forwarded(headers: &mut HeaderMap) {
    let forwarded: Vec<HeaderName> = headers
        .keys()
        .filter(|name| is_forwarded(name.as_str()))
        .cloned()
        .collect();
    for name in forwarded {
        headers.remove(&name);
    }
}

fn is_forwarded(name: &str) -> bool {
    name.starts_with("x-forwarded-") || name == "x-real-ip" || name == "forwarded"
}

fn misdirected(origin: &DashboardOrigin) -> Response {
    (
        StatusCode::MISDIRECTED_REQUEST,
        [(header::CONNECTION, "close")],
        Json(serde_json::json!({
            "error": format!(
                "This connection serves only {}. Open the dashboard at that address.",
                origin.origin()
            ),
        })),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;

    fn connection() -> RelayConnection {
        RelayConnection::new(DashboardOrigin::new("abc.relay.example", 443).unwrap())
    }

    fn request(uri: &str, headers: &[(&str, &str)]) -> Request<Body> {
        let mut builder = Request::builder().uri(uri);
        for (name, value) in headers {
            builder = builder.header(*name, *value);
        }
        builder.body(Body::empty()).unwrap()
    }

    #[test]
    fn a_request_for_the_dashboard_is_marked_with_its_origin() {
        let mut admitted = request("/api/auth/methods", &[("host", "ABC.relay.example")]);
        connection().admit(&mut admitted).unwrap();
        let marker = admitted.extensions().get::<ViaRelay>().unwrap();
        assert_eq!(marker.origin(), "https://abc.relay.example");
    }

    #[test]
    fn a_request_naming_another_host_is_misdirected() {
        for headers in [
            vec![("host", "evil.example")],
            vec![("host", "abc.relay.example.evil.example")],
            vec![("host", "abc.relay.example:8443")],
            vec![("host", "abc.relay.example"), ("host", "evil.example")],
            vec![],
        ] {
            let mut refused = request("/", &headers);
            let response = connection().admit(&mut refused).unwrap_err();
            assert_eq!(
                response.status(),
                StatusCode::MISDIRECTED_REQUEST,
                "{headers:?}"
            );
            assert!(refused.extensions().get::<ViaRelay>().is_none());
        }
        // An absolute-form target names its own authority, which has to be
        // the dashboard's too.
        let mut absolute = request("https://evil.example/", &[("host", "abc.relay.example")]);
        assert_eq!(
            connection().admit(&mut absolute).unwrap_err().status(),
            StatusCode::MISDIRECTED_REQUEST
        );
        let mut own = request(
            "https://abc.relay.example/",
            &[("host", "abc.relay.example")],
        );
        connection().admit(&mut own).unwrap();
    }

    #[test]
    fn forwarded_headers_from_the_browser_are_dropped() {
        let mut admitted = request(
            "/",
            &[
                ("host", "abc.relay.example"),
                ("x-forwarded-for", "127.0.0.1"),
                ("X-Forwarded-Proto", "http"),
                ("x-forwarded-host", "evil.example"),
                ("x-real-ip", "127.0.0.1"),
                ("forwarded", "for=127.0.0.1;proto=http"),
                ("x-request-id", "kept"),
            ],
        );
        connection().admit(&mut admitted).unwrap();
        let headers = admitted.headers();
        for gone in [
            "x-forwarded-for",
            "x-forwarded-proto",
            "x-forwarded-host",
            "x-real-ip",
            "forwarded",
        ] {
            assert!(!headers.contains_key(gone), "{gone}");
        }
        assert_eq!(headers["x-request-id"], "kept");
    }
}
