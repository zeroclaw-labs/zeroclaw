use futures_util::StreamExt;

pub(crate) struct BoundedBody {
    pub bytes: Vec<u8>,
    pub overflowed: bool,
}

pub(crate) async fn read_bounded(
    response: reqwest::Response,
    limit: Option<usize>,
) -> anyhow::Result<BoundedBody> {
    let hard_cap = limit.map(|value| value.saturating_add(1));
    let mut stream = response.bytes_stream();
    let mut bytes = Vec::new();

    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        let Some(cap) = hard_cap else {
            bytes.extend_from_slice(&chunk);
            continue;
        };

        if bytes.len() >= cap {
            break;
        }
        let remaining = cap - bytes.len();
        bytes.extend_from_slice(&chunk[..chunk.len().min(remaining)]);
        if bytes.len() >= cap {
            break;
        }
    }

    let overflowed = limit.is_some_and(|value| bytes.len() > value);
    if let Some(value) = limit {
        bytes.truncate(value);
    }

    Ok(BoundedBody { bytes, overflowed })
}

/// Read a response body up to `limit` bytes and return UTF-8-safe text.
///
/// The boolean is true when the response exceeded the limit. The reader
/// retains at most one byte beyond the limit to detect overflow.
pub async fn read_text(
    response: reqwest::Response,
    limit: Option<usize>,
) -> anyhow::Result<(String, bool)> {
    let mut body = read_bounded(response, limit).await?;
    if body.overflowed
        && let Err(error) = std::str::from_utf8(&body.bytes)
        && error.error_len().is_none()
    {
        body.bytes.truncate(error.valid_up_to());
    }
    Ok((
        String::from_utf8_lossy(&body.bytes).into_owned(),
        body.overflowed,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    async fn chunked_response(chunks: &[&[u8]]) -> reqwest::Response {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let owned_chunks = chunks
            .iter()
            .map(|chunk| chunk.to_vec())
            .collect::<Vec<_>>();

        zeroclaw_spawn::spawn!(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                let mut buffer = [0_u8; 1024];
                let read = stream.read(&mut buffer).await.unwrap();
                assert!(read > 0, "client closed before completing request headers");
                request.extend_from_slice(&buffer[..read]);
            }
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n",
                )
                .await
                .unwrap();
            for chunk in owned_chunks {
                stream
                    .write_all(format!("{:x}\r\n", chunk.len()).as_bytes())
                    .await
                    .unwrap();
                stream.write_all(&chunk).await.unwrap();
                stream.write_all(b"\r\n").await.unwrap();
            }
            stream.write_all(b"0\r\n\r\n").await.unwrap();
        });

        reqwest::get(format!("http://{addr}")).await.unwrap()
    }

    #[tokio::test]
    async fn bounded_reader_stops_after_limit_plus_one_without_content_length() {
        let response = chunked_response(&[b"hello", b" world", b" ignored"]).await;
        let body = read_bounded(response, Some(8)).await.unwrap();

        assert_eq!(body.bytes, b"hello wo");
        assert!(body.overflowed);
    }

    #[tokio::test]
    async fn unlimited_reader_preserves_the_complete_body() {
        let response = chunked_response(&[b"hello", b" world"]).await;
        let body = read_bounded(response, None).await.unwrap();

        assert_eq!(body.bytes, b"hello world");
        assert!(!body.overflowed);
    }

    #[tokio::test]
    async fn exact_limit_is_not_reported_as_overflow() {
        let response = chunked_response(&[b"hello", b" wo"]).await;
        let body = read_bounded(response, Some(8)).await.unwrap();

        assert_eq!(body.bytes, b"hello wo");
        assert!(!body.overflowed);
    }

    #[tokio::test]
    async fn truncated_text_drops_an_incomplete_final_codepoint() {
        let response = chunked_response(&["abcéz".as_bytes()]).await;
        let (text, overflowed) = read_text(response, Some(4)).await.unwrap();

        assert_eq!(text, "abc");
        assert!(overflowed);
        assert!(!text.contains('\u{fffd}'));
    }
}
