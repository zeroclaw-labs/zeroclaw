//! XMPP/Jabber channel integration.
//!
//! Connects to an XMPP server and listens for messages in MUC rooms
//! or direct messages. Uses plain TCP+TLS with SASL PLAIN auth.
//!
//! Config:
//! ```toml
//! [channels_config.xmpp]
//! jid = "bot@jabber.example.com"
//! password = "..."
//! server = "jabber.example.com"
//! rooms = ["room@conference.example.com"]
//! ```

use super::traits::{Channel, ChannelMessage, SendMessage};
use async_trait::async_trait;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::Mutex;

pub struct XmppChannelConfig {
    pub jid: String,
    pub password: String,
    pub server: String,
    pub port: u16,
    pub rooms: Vec<String>,
}

pub struct XmppChannel {
    jid: String,
    password: String,
    server: String,
    port: u16,
    rooms: Vec<String>,
    writer: Arc<
        Mutex<Option<tokio::io::WriteHalf<tokio_rustls::client::TlsStream<tokio::net::TcpStream>>>>,
    >,
}

impl XmppChannel {
    pub fn new(cfg: XmppChannelConfig) -> Self {
        Self {
            jid: cfg.jid,
            password: cfg.password,
            server: cfg.server,
            port: cfg.port,
            rooms: cfg.rooms,
            writer: Arc::new(Mutex::new(None)),
        }
    }

    fn local_part(&self) -> &str {
        self.jid.split('@').next().unwrap_or(&self.jid)
    }
}

#[async_trait]
impl Channel for XmppChannel {
    fn name(&self) -> &str {
        "xmpp"
    }

    async fn send(&self, message: &SendMessage) -> anyhow::Result<()> {
        let mut guard = self.writer.lock().await;
        let writer = guard
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("XMPP not connected"))?;

        // Escape XML special characters
        let body = message
            .content
            .replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;");

        let stanza = format!(
            "<message to='{}' type='chat'><body>{}</body></message>",
            message.recipient, body
        );

        writer.write_all(stanza.as_bytes()).await?;
        writer.flush().await?;
        Ok(())
    }

    async fn listen(&self, tx: tokio::sync::mpsc::Sender<ChannelMessage>) -> anyhow::Result<()> {
        use tokio_rustls::TlsConnector;

        let tcp = tokio::net::TcpStream::connect((&*self.server, self.port)).await?;

        let tls_config = tokio_rustls::rustls::ClientConfig::builder()
            .with_native_root_certs()
            .with_no_client_auth();
        let connector = TlsConnector::from(Arc::new(tls_config));
        let server_name =
            tokio_rustls::rustls::pki_types::ServerName::try_from(self.server.clone())?;
        let tls = connector.connect(server_name, tcp).await?;

        let (reader, writer) = tokio::io::split(tls);
        {
            let mut guard = self.writer.lock().await;
            *guard = Some(writer);
        }

        // Send stream opening
        {
            let mut guard = self.writer.lock().await;
            if let Some(ref mut w) = *guard {
                let open = format!(
                    "<?xml version='1.0'?><stream:stream to='{}' xmlns='jabber:client' xmlns:stream='http://etherx.jabber.org/streams' version='1.0'>",
                    self.server
                );
                w.write_all(open.as_bytes()).await?;
                w.flush().await?;

                // SASL PLAIN auth
                let auth_str = format!("\0{}\0{}", self.local_part(), self.password);
                let encoded = base64_encode(auth_str.as_bytes());
                let auth = format!(
                    "<auth xmlns='urn:ietf:params:xml:ns:xmpp-sasl' mechanism='PLAIN'>{encoded}</auth>"
                );
                w.write_all(auth.as_bytes()).await?;
                w.flush().await?;
            }
        }

        // Join MUC rooms
        for room in &self.rooms {
            let mut guard = self.writer.lock().await;
            if let Some(ref mut w) = *guard {
                let presence = format!(
                    "<presence to='{}/{}'><x xmlns='http://jabber.org/protocol/muc'/></presence>",
                    room,
                    self.local_part()
                );
                w.write_all(presence.as_bytes()).await?;
                w.flush().await?;
            }
        }

        // Read messages (simplified XML parsing — production would use an XML parser)
        let mut buf_reader = BufReader::new(reader);
        let mut line = String::new();

        loop {
            line.clear();
            match buf_reader.read_line(&mut line).await {
                Ok(0) => break,
                Err(e) => {
                    tracing::debug!("XMPP read error: {e}");
                    break;
                }
                Ok(_) => {}
            }

            // Simple body extraction (production: use quick-xml)
            if let Some(body) = extract_body(&line) {
                let sender = extract_attr(&line, "from").unwrap_or_default();
                if sender.contains(&format!("/{}", self.local_part())) {
                    continue; // Skip own messages
                }

                let msg = ChannelMessage {
                    id: uuid::Uuid::new_v4().to_string(),
                    sender: sender.clone(),
                    reply_target: sender,
                    content: body,
                    channel: "xmpp".to_string(),
                    timestamp: 0,
                    thread_ts: None,
                    interruption_scope_id: None,
                    attachments: Vec::new(),
                };

                if tx.send(msg).await.is_err() {
                    break;
                }
            }
        }

        Ok(())
    }

    async fn health_check(&self) -> bool {
        tokio::net::TcpStream::connect((&*self.server, self.port))
            .await
            .is_ok()
    }
}

fn extract_body(xml: &str) -> Option<String> {
    let start = xml.find("<body>")? + 6;
    let end = xml.find("</body>")?;
    if start < end {
        Some(
            xml[start..end]
                .replace("&amp;", "&")
                .replace("&lt;", "<")
                .replace("&gt;", ">"),
        )
    } else {
        None
    }
}

fn extract_attr(xml: &str, attr: &str) -> Option<String> {
    let pattern = format!("{attr}='");
    let start = xml.find(&pattern)? + pattern.len();
    let end = xml[start..].find('\'')?;
    Some(xml[start..start + end].to_string())
}

fn base64_encode(input: &[u8]) -> String {
    const CHARS: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut result = String::new();
    for chunk in input.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
        let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
        let triple = (b0 << 16) | (b1 << 8) | b2;
        result.push(CHARS[((triple >> 18) & 0x3f) as usize] as char);
        result.push(CHARS[((triple >> 12) & 0x3f) as usize] as char);
        if chunk.len() > 1 {
            result.push(CHARS[((triple >> 6) & 0x3f) as usize] as char);
        } else {
            result.push('=');
        }
        if chunk.len() > 2 {
            result.push(CHARS[(triple & 0x3f) as usize] as char);
        } else {
            result.push('=');
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_name() {
        let ch = XmppChannel::new(XmppChannelConfig {
            jid: "bot@example.com".into(),
            password: "pass".into(),
            server: "example.com".into(),
            port: 5222,
            rooms: vec![],
        });
        assert_eq!(ch.name(), "xmpp");
    }

    #[test]
    fn extract_body_works() {
        assert_eq!(
            extract_body("<message><body>hello</body></message>"),
            Some("hello".into())
        );
        assert_eq!(extract_body("<message>no body</message>"), None);
    }

    #[test]
    fn extract_attr_works() {
        assert_eq!(
            extract_attr("<message from='alice@example.com'>", "from"),
            Some("alice@example.com".into())
        );
    }

    #[test]
    fn base64_encode_works() {
        assert_eq!(base64_encode(b"hello"), "aGVsbG8=");
        assert_eq!(base64_encode(b"\0user\0pass"), "AHVzZXIAcGFzcw==");
    }

    #[test]
    fn local_part_extraction() {
        let ch = XmppChannel::new(XmppChannelConfig {
            jid: "bot@example.com".into(),
            password: "p".into(),
            server: "example.com".into(),
            port: 5222,
            rooms: vec![],
        });
        assert_eq!(ch.local_part(), "bot");
    }
}
