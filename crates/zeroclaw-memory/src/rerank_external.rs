//! Client for an operator-configured rerank endpoint, used by the memory
//! injection path when `rerank_strategy = "external"`.
//!
//! The endpoint receives the recall query and the text of each candidate
//! memory, and answers with a relevance order. It speaks the `/v1/rerank`
//! shape that llama.cpp's `--reranking` server serves, which the Cohere- and
//! Jina-style APIs share: a request `{query, documents, top_n, model?}` and a
//! reply `{results: [{index, relevance_score}]}`.
//!
//! The client only orders. [`ExternalRerankSettings::rerank_pool`] runs the
//! whole external strategy over a recall pool and falls back to the local
//! order on any failure; nothing here logs memory text.

use crate::rerank::{self, RerankConfig};
use crate::traits::MemoryEntry;
use std::collections::HashSet;
use std::time::{Duration, Instant};

/// Longest text sent for the query or for one candidate, in characters.
pub const MAX_RERANK_TEXT_CHARS: usize = 1_500;

/// Largest reply body accepted from the endpoint.
const MAX_RESPONSE_BYTES: usize = 1 << 20;

/// Proxy service key for the endpoint's HTTP client.
pub const RERANK_PROXY_SERVICE_KEY: &str = "memory.rerank";

/// Resolved settings for one rerank endpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalRerankSettings {
    /// Base URL, or the full URL of the rerank route.
    pub endpoint: String,
    /// Model name passed through in the request; empty omits it.
    pub model: String,
    /// Whole-request timeout.
    pub timeout: Duration,
    /// Most candidates sent per call.
    pub max_candidates: usize,
}

/// Why a rerank call produced no usable order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RerankFailure {
    /// The request did not complete within the timeout.
    Timeout,
    /// The request could not be sent or the reply not read.
    Transport,
    /// The endpoint answered with a non-success status.
    Status(u16),
    /// The reply was not the expected shape, or was too large.
    Malformed,
    /// The reply named a candidate index out of range, or one twice.
    InvalidIndex,
}

impl RerankFailure {
    /// Stable reason label for logs and records.
    #[must_use]
    pub fn reason(self) -> &'static str {
        match self {
            Self::Timeout => "timeout",
            Self::Transport => "transport",
            Self::Status(_) => "status",
            Self::Malformed => "malformed",
            Self::InvalidIndex => "invalid_index",
        }
    }
}

impl ExternalRerankSettings {
    /// The URL the request goes to. An endpoint whose path already ends in
    /// `/rerank` is used as given; one with another explicit path gets
    /// `/rerank` appended; a bare origin gets `/v1/rerank`. Only the path
    /// changes, so a query string stays where it was.
    #[must_use]
    pub fn rerank_url(&self) -> String {
        let endpoint = self.endpoint.trim();
        let Ok(mut url) = reqwest::Url::parse(endpoint) else {
            // Config validation rejects this; the request then fails as a
            // transport error and the local order is kept.
            return endpoint.to_string();
        };
        let path = url.path().trim_end_matches('/').to_string();
        let route = if path.ends_with("/rerank") {
            path
        } else if path.is_empty() {
            "/v1/rerank".to_string()
        } else {
            format!("{path}/rerank")
        };
        url.set_path(&route);
        url.to_string()
    }

    /// The external strategy over a recall pool: prepare it as
    /// [`rerank::run`] does (floor included), send the head to the endpoint,
    /// and select from the returned order. Any failure keeps the prepared
    /// order, which is what the `none` strategy returns. The record names the
    /// reason and the candidate count, never memory text.
    pub async fn rerank_pool(
        &self,
        pool: Vec<MemoryEntry>,
        config: &RerankConfig,
        is_eligible: impl Fn(&MemoryEntry) -> bool,
        query: &str,
        turn_id: &str,
    ) -> Vec<MemoryEntry> {
        let prepared = rerank::prepare_for_external(pool, config, is_eligible);
        // Nothing to order with fewer than two candidates, and the strategy
        // runs only once the pool reaches the configured threshold, as `run`
        // does.
        if prepared.len() < 2 || prepared.len() < config.threshold {
            return rerank::finish(prepared, config);
        }
        let sent = prepared
            .len()
            .min(self.max_candidates)
            .min(rerank::bounded_pool_cap(
                config.final_limit,
                config.candidate_pool_cap,
            ));
        let documents: Vec<&str> = prepared[..sent]
            .iter()
            .map(|entry| entry.content.as_str())
            .collect();

        let started = Instant::now();
        match self.rerank(query, &documents).await {
            Ok(order) => {
                ::zeroclaw_log::record!(
                    DEBUG,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_category(::zeroclaw_log::EventCategory::Memory)
                        .with_outcome(::zeroclaw_log::EventOutcome::Success)
                        .with_attrs(::serde_json::json!({
                            "candidates": sent,
                            "ranked": order.len(),
                            "elapsed_ms": started.elapsed().as_millis(),
                            "turn_id": turn_id,
                        })),
                    "memory external rerank applied"
                );
                rerank::finish(rerank::apply_external_order(prepared, sent, &order), config)
            }
            Err(failure) => {
                let status = match failure {
                    RerankFailure::Status(code) => Some(code),
                    _ => None,
                };
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_category(::zeroclaw_log::EventCategory::Memory)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({
                            "reason": failure.reason(),
                            "status": status,
                            "candidates": sent,
                            "elapsed_ms": started.elapsed().as_millis(),
                            "turn_id": turn_id,
                        })),
                    "memory external rerank failed; kept the local order"
                );
                rerank::finish(prepared, config)
            }
        }
    }

    /// Ask the endpoint to order `documents` for `query`. Returns positions
    /// into `documents`, most relevant first. The caller sends at most
    /// `max_candidates` documents; this method does not trim them.
    pub async fn rerank(
        &self,
        query: &str,
        documents: &[&str],
    ) -> Result<Vec<usize>, RerankFailure> {
        if documents.is_empty() {
            return Ok(Vec::new());
        }
        let mut body = serde_json::json!({
            "query": clip(query),
            "documents": documents.iter().map(|doc| clip(doc)).collect::<Vec<_>>(),
            "top_n": documents.len(),
        });
        if !self.model.trim().is_empty() {
            body["model"] = serde_json::Value::String(self.model.trim().to_string());
        }

        let client = zeroclaw_config::schema::build_runtime_proxy_client(RERANK_PROXY_SERVICE_KEY);
        let mut response = client
            .post(self.rerank_url())
            .timeout(self.timeout)
            .json(&body)
            .send()
            .await
            .map_err(classify_error)?;
        let status = response.status();
        if !status.is_success() {
            return Err(RerankFailure::Status(status.as_u16()));
        }
        if response
            .content_length()
            .is_some_and(|len| len > MAX_RESPONSE_BYTES as u64)
        {
            return Err(RerankFailure::Malformed);
        }
        // Read in chunks so a reply without a length, or with a false one,
        // is refused before it grows past the cap.
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(classify_error)? {
            if bytes.len() + chunk.len() > MAX_RESPONSE_BYTES {
                return Err(RerankFailure::Malformed);
            }
            bytes.extend_from_slice(&chunk);
        }
        parse_order(&bytes, documents.len())
    }
}

fn classify_error(error: reqwest::Error) -> RerankFailure {
    if error.is_timeout() {
        RerankFailure::Timeout
    } else {
        RerankFailure::Transport
    }
}

fn clip(text: &str) -> &str {
    match text.char_indices().nth(MAX_RERANK_TEXT_CHARS) {
        Some((byte_index, _)) => &text[..byte_index],
        None => text,
    }
}

/// Read a rerank reply into positions, most relevant first. Every index must
/// be below `sent` and appear once, and every score must be a finite number;
/// otherwise the whole reply is refused. Ties keep the reply's order.
pub fn parse_order(body: &[u8], sent: usize) -> Result<Vec<usize>, RerankFailure> {
    let value: serde_json::Value =
        serde_json::from_slice(body).map_err(|_| RerankFailure::Malformed)?;
    let results = value
        .get("results")
        .and_then(serde_json::Value::as_array)
        .ok_or(RerankFailure::Malformed)?;

    let mut scored: Vec<(usize, f64)> = Vec::with_capacity(results.len());
    let mut seen: HashSet<usize> = HashSet::with_capacity(results.len());
    for result in results {
        let index = result
            .get("index")
            .and_then(serde_json::Value::as_u64)
            .ok_or(RerankFailure::Malformed)?;
        let score = result
            .get("relevance_score")
            .and_then(serde_json::Value::as_f64)
            .filter(|score| score.is_finite())
            .ok_or(RerankFailure::Malformed)?;
        let index = usize::try_from(index).map_err(|_| RerankFailure::InvalidIndex)?;
        if index >= sent || !seen.insert(index) {
            return Err(RerankFailure::InvalidIndex);
        }
        scored.push((index, score));
    }

    scored.sort_by(|left, right| {
        right
            .1
            .partial_cmp(&left.1)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    Ok(scored.into_iter().map(|(index, _)| index).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(endpoint: &str) -> ExternalRerankSettings {
        ExternalRerankSettings {
            endpoint: endpoint.into(),
            model: String::new(),
            timeout: Duration::from_millis(500),
            max_candidates: 16,
        }
    }

    #[test]
    fn a_bare_origin_gets_the_v1_route() {
        assert_eq!(
            settings("http://127.0.0.1:8098").rerank_url(),
            "http://127.0.0.1:8098/v1/rerank"
        );
        assert_eq!(
            settings("http://127.0.0.1:8098/").rerank_url(),
            "http://127.0.0.1:8098/v1/rerank"
        );
    }

    #[test]
    fn an_explicit_path_gets_rerank_appended_and_a_full_route_is_kept() {
        assert_eq!(
            settings("https://rerank.example.com/api/v2").rerank_url(),
            "https://rerank.example.com/api/v2/rerank"
        );
        assert_eq!(
            settings("http://127.0.0.1:8098/v1/rerank").rerank_url(),
            "http://127.0.0.1:8098/v1/rerank"
        );
    }

    #[test]
    fn a_query_string_stays_after_the_route() {
        assert_eq!(
            settings("https://rank.example/api?tenant=blue").rerank_url(),
            "https://rank.example/api/rerank?tenant=blue"
        );
        assert_eq!(
            settings("https://rank.example/?tenant=blue").rerank_url(),
            "https://rank.example/v1/rerank?tenant=blue"
        );
        assert_eq!(
            settings("https://rank.example/v1/rerank/?tenant=blue").rerank_url(),
            "https://rank.example/v1/rerank?tenant=blue"
        );
    }

    #[tokio::test]
    async fn a_reply_over_the_cap_without_a_length_is_refused() {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0u8; 8192];
            let _ = stream.read(&mut request);
            let _ = stream.write_all(
                b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ntransfer-encoding: chunked\r\n\r\n",
            );
            let chunk = vec![b' '; 64 * 1024];
            for _ in 0..(MAX_RESPONSE_BYTES / chunk.len() + 2) {
                let header = format!("{:x}\r\n", chunk.len());
                if stream.write_all(header.as_bytes()).is_err()
                    || stream.write_all(&chunk).is_err()
                    || stream.write_all(b"\r\n").is_err()
                {
                    return;
                }
            }
            let _ = stream.write_all(b"0\r\n\r\n");
        });

        let result = settings(&format!("http://{address}"))
            .rerank("query", &["one", "two"])
            .await;
        assert_eq!(result, Err(RerankFailure::Malformed));
        server.join().unwrap();
    }

    #[test]
    fn a_reply_is_ordered_by_score_whatever_its_own_order() {
        let body = br#"{"results":[{"index":0,"relevance_score":0.1},{"index":2,"relevance_score":0.9},{"index":1,"relevance_score":0.5}]}"#;
        assert_eq!(parse_order(body, 3), Ok(vec![2, 1, 0]));
    }

    #[test]
    fn a_partial_reply_is_accepted() {
        let body = br#"{"results":[{"index":3,"relevance_score":2.5}]}"#;
        assert_eq!(parse_order(body, 4), Ok(vec![3]));
    }

    #[test]
    fn ties_keep_the_reply_order() {
        let body =
            br#"{"results":[{"index":1,"relevance_score":0.5},{"index":0,"relevance_score":0.5}]}"#;
        assert_eq!(parse_order(body, 2), Ok(vec![1, 0]));
    }

    #[test]
    fn an_out_of_range_or_repeated_index_refuses_the_reply() {
        let out_of_range =
            br#"{"results":[{"index":0,"relevance_score":0.2},{"index":5,"relevance_score":0.9}]}"#;
        assert_eq!(
            parse_order(out_of_range, 3),
            Err(RerankFailure::InvalidIndex)
        );
        let repeated =
            br#"{"results":[{"index":1,"relevance_score":0.2},{"index":1,"relevance_score":0.9}]}"#;
        assert_eq!(parse_order(repeated, 3), Err(RerankFailure::InvalidIndex));
    }

    #[test]
    fn a_wrong_shape_refuses_the_reply() {
        for body in [
            &b"not json"[..],
            br#"{"data":[]}"#,
            br#"{"results":[{"index":"0","relevance_score":0.2}]}"#,
            br#"{"results":[{"index":0}]}"#,
            br#"{"results":[{"index":-1,"relevance_score":0.2}]}"#,
            br#"{"results":[{"index":0,"relevance_score":"high"}]}"#,
        ] {
            assert_eq!(
                parse_order(body, 3),
                Err(RerankFailure::Malformed),
                "{}",
                String::from_utf8_lossy(body)
            );
        }
    }

    #[test]
    fn long_text_is_clipped_on_a_character_boundary() {
        let long = "é".repeat(MAX_RERANK_TEXT_CHARS + 10);
        let clipped = clip(&long);
        assert_eq!(clipped.chars().count(), MAX_RERANK_TEXT_CHARS);
        assert_eq!(clip("short"), "short");
    }

    #[test]
    fn failure_reasons_are_stable_labels() {
        assert_eq!(RerankFailure::Timeout.reason(), "timeout");
        assert_eq!(RerankFailure::Status(503).reason(), "status");
        assert_eq!(RerankFailure::InvalidIndex.reason(), "invalid_index");
    }
}
