// Host-side WIT `http-helpers` implementation for the `channel-plugin`
// world. Pure convenience sugar layered on the same proxy-aware client
// `send_request` uses and the same allow-list `websocket`/`gateway` apply —
// not a parallel security boundary. Saves a guest from hand-rolling
// multipart encoding or attachment downloads over raw `wasi:http`.

use super::bindings::channel::zeroclaw::plugin::http_helpers::{
    Host, HttpHeader, HttpResponse, MultipartField,
};
use super::bindings::channel::zeroclaw::plugin::types::MediaAttachment;
use super::plugin_store::PluginStore;

fn apply_headers(
    mut builder: reqwest::RequestBuilder,
    headers: &[HttpHeader],
) -> reqwest::RequestBuilder {
    for h in headers {
        builder = builder.header(&h.name, &h.value);
    }
    builder
}

impl Host for PluginStore {
    async fn send_multipart(
        &mut self,
        url: String,
        headers: Vec<HttpHeader>,
        fields: Vec<MultipartField>,
        timeout_ms: Option<u32>,
    ) -> Result<HttpResponse, String> {
        if !self.is_url_host_allowed(&url) {
            return Err(format!(
                "send-multipart denied: {url} is not in this plugin's allow-list"
            ));
        }

        let mut form = reqwest::multipart::Form::new();
        for field in fields {
            let mut part = reqwest::multipart::Part::bytes(field.data);
            if let Some(file_name) = field.file_name {
                part = part.file_name(file_name);
            }
            if let Some(content_type) = field.content_type {
                part = part
                    .mime_str(&content_type)
                    .map_err(|e| format!("invalid multipart content-type: {e}"))?;
            }
            form = form.part(field.name, part);
        }

        let mut builder = self.proxy_client().post(&url).multipart(form);
        builder = apply_headers(builder, &headers);
        if let Some(ms) = timeout_ms {
            builder = builder.timeout(std::time::Duration::from_millis(u64::from(ms)));
        }

        let resp = builder
            .send()
            .await
            .map_err(|e| format!("send-multipart request failed: {e}"))?;
        let status = resp.status().as_u16();
        let resp_headers = resp
            .headers()
            .iter()
            .map(|(name, value)| HttpHeader {
                name: name.to_string(),
                value: value.to_str().unwrap_or_default().to_string(),
            })
            .collect();
        let body = resp
            .bytes()
            .await
            .map_err(|e| format!("failed reading send-multipart response body: {e}"))?
            .to_vec();

        Ok(HttpResponse {
            status,
            headers: resp_headers,
            body,
        })
    }

    async fn download_to_attachment(
        &mut self,
        url: String,
        headers: Vec<HttpHeader>,
        max_bytes: u32,
    ) -> Result<MediaAttachment, String> {
        if !self.is_url_host_allowed(&url) {
            return Err(format!(
                "download-to-attachment denied: {url} is not in this plugin's allow-list"
            ));
        }

        let builder = apply_headers(self.proxy_client().get(&url), &headers);
        let resp = builder
            .send()
            .await
            .map_err(|e| format!("download-to-attachment request failed: {e}"))?;
        if !resp.status().is_success() {
            return Err(format!(
                "download-to-attachment failed: status {}",
                resp.status()
            ));
        }
        let mime_type = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        let file_name = url
            .rsplit('/')
            .next()
            .filter(|s| !s.is_empty())
            .unwrap_or("attachment")
            .to_string();

        let data = resp
            .bytes()
            .await
            .map_err(|e| format!("failed reading download-to-attachment body: {e}"))?;
        if data.len() > max_bytes as usize {
            return Err(format!(
                "download-to-attachment exceeded max-bytes ({} > {max_bytes})",
                data.len()
            ));
        }

        Ok(MediaAttachment {
            file_name,
            data: data.to_vec(),
            mime_type,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::component::v0::plugin_store::PluginStore;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    async fn allowed_store() -> PluginStore {
        let perms = vec![crate::FineGrainedPermission::Http(
            crate::AddressString::new("127.0.0.1").unwrap(),
        )];
        PluginStore::with_permissions(&perms, &crate::PluginNetworkConfig::default())
            .await
            .unwrap()
    }

    /// One-shot server that records the request body it receives and replies
    /// with a fixed body, so both multipart upload and plain download can be
    /// exercised against something real.
    async fn spawn_capturing_server(
        response_body: &'static [u8],
        content_type: &'static str,
    ) -> (u16, tokio::task::JoinHandle<Vec<u8>>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = zeroclaw_spawn::spawn!(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 65536];
            let n = stream.read(&mut buf).await.unwrap_or(0);
            let received = buf[..n].to_vec();
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                content_type,
                response_body.len()
            );
            let _ = stream.write_all(response.as_bytes()).await;
            let _ = stream.write_all(response_body).await;
            let _ = stream.shutdown().await;
            received
        });
        (port, handle)
    }

    #[tokio::test]
    async fn send_multipart_denied_for_host_outside_allow_list() {
        let mut store = PluginStore::with_permissions(&[], &crate::PluginNetworkConfig::default())
            .await
            .unwrap();
        let err = Host::send_multipart(
            &mut store,
            "http://127.0.0.1:9/".to_string(),
            vec![],
            vec![],
            None,
        )
        .await
        .expect_err("host outside allow-list must be denied before any network call");
        assert!(err.contains("not in this plugin's allow-list"));
    }

    #[tokio::test]
    async fn send_multipart_reaches_real_server_with_fields() {
        let (port, server) = spawn_capturing_server(b"ok", "text/plain").await;
        let mut store = allowed_store().await;

        let fields = vec![
            MultipartField {
                name: "payload_json".to_string(),
                file_name: None,
                content_type: Some("application/json".to_string()),
                data: br#"{"content":"hi"}"#.to_vec(),
            },
            MultipartField {
                name: "files[0]".to_string(),
                file_name: Some("note.txt".to_string()),
                content_type: Some("text/plain".to_string()),
                data: b"hello".to_vec(),
            },
        ];

        let resp = Host::send_multipart(
            &mut store,
            format!("http://127.0.0.1:{port}/"),
            vec![],
            fields,
            Some(5000),
        )
        .await
        .expect("allow-listed host must be permitted");
        assert_eq!(resp.status, 200);
        assert_eq!(resp.body, b"ok");

        let received = String::from_utf8_lossy(&server.await.unwrap()).into_owned();
        assert!(received.contains("payload_json"));
        assert!(received.contains("note.txt"));
        assert!(received.contains("hello"));
    }

    #[tokio::test]
    async fn download_to_attachment_denied_for_host_outside_allow_list() {
        let mut store = PluginStore::with_permissions(&[], &crate::PluginNetworkConfig::default())
            .await
            .unwrap();
        let err = Host::download_to_attachment(
            &mut store,
            "http://127.0.0.1:9/file.bin".to_string(),
            vec![],
            1024,
        )
        .await
        .expect_err("host outside allow-list must be denied before any network call");
        assert!(err.contains("not in this plugin's allow-list"));
    }

    #[tokio::test]
    async fn download_to_attachment_fetches_real_server_body() {
        let (port, _server) = spawn_capturing_server(b"the-file-bytes", "image/png").await;
        let mut store = allowed_store().await;

        let attachment = Host::download_to_attachment(
            &mut store,
            format!("http://127.0.0.1:{port}/photo.png"),
            vec![],
            1024,
        )
        .await
        .expect("allow-listed host must be permitted");
        assert_eq!(attachment.data, b"the-file-bytes");
        assert_eq!(attachment.mime_type.as_deref(), Some("image/png"));
        assert_eq!(attachment.file_name, "photo.png");
    }

    #[tokio::test]
    async fn download_to_attachment_rejects_body_over_max_bytes() {
        let (port, _server) = spawn_capturing_server(b"way-too-long-a-body", "text/plain").await;
        let mut store = allowed_store().await;

        let err = Host::download_to_attachment(
            &mut store,
            format!("http://127.0.0.1:{port}/big.bin"),
            vec![],
            4,
        )
        .await
        .expect_err("a body larger than max-bytes must be rejected");
        assert!(err.contains("exceeded max-bytes"));
    }
}
