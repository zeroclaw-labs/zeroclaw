//! Loopback brokers check the migrated client's wire protocol and TLS boundary.

use super::*;
use rumqttc::tokio_rustls::{TlsAcceptor, rustls};
use rumqttc::{ConnectionError, TlsConfiguration, TlsError};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::time::timeout;

const DEADLINE: Duration = Duration::from_secs(10);
const TOPIC: &str = "sensors/temperature";
const PAYLOAD: &[u8] = b"{\"temperature\":21}";

fn config(port: u16, tls: bool, qos: u8) -> MqttConfig {
    MqttConfig {
        enabled: true,
        broker_url: format!("{}://127.0.0.1:{port}", if tls { "mqtts" } else { "mqtt" }),
        client_id: "mqtt-migration-test".into(),
        topics: vec![TOPIC.into()],
        qos,
        username: Some("fixture-user".into()),
        password: Some("fixture-password".into()),
        use_tls: tls,
        keep_alive_secs: 37,
        excluded_tools: vec![],
    }
}

async fn read_packet(stream: &mut (impl AsyncRead + Unpin)) -> (u8, Vec<u8>) {
    let header = stream.read_u8().await.expect("MQTT fixed header");
    let mut remaining = 0usize;
    let mut multiplier = 1;
    for _ in 0..4 {
        let byte = stream.read_u8().await.expect("MQTT remaining length");
        remaining += usize::from(byte & 0x7f) * multiplier;
        if byte & 0x80 == 0 {
            assert!(remaining <= 4096, "bounded fixture packet");
            let mut body = vec![0; remaining];
            stream.read_exact(&mut body).await.expect("MQTT body");
            return (header, body);
        }
        multiplier *= 128;
    }
    panic!("invalid MQTT remaining length");
}

fn read_string<'a>(body: &mut &'a [u8]) -> &'a [u8] {
    let len = usize::from(u16::from_be_bytes([body[0], body[1]]));
    let value = &body[2..2 + len];
    *body = &body[2 + len..];
    value
}

async fn broker_session(stream: &mut (impl AsyncRead + AsyncWrite + Unpin), qos: u8) {
    let (header, connect) = read_packet(stream).await;
    assert_eq!(header, 0x10, "MQTT CONNECT");
    assert_eq!(&connect[..7], b"\x00\x04MQTT\x04", "MQTT 3.1.1");
    assert_eq!(connect[7], 0xc2, "username, password, clean session");
    assert_eq!(u16::from_be_bytes([connect[8], connect[9]]), 37);
    let mut fields = &connect[10..];
    assert_eq!(read_string(&mut fields), b"mqtt-migration-test");
    assert_eq!(read_string(&mut fields), b"fixture-user");
    assert_eq!(read_string(&mut fields), b"fixture-password");
    assert!(fields.is_empty());
    stream.write_all(&[0x20, 2, 0, 0]).await.expect("CONNACK");

    let (header, subscribe) = read_packet(stream).await;
    assert_eq!(header, 0x82, "MQTT SUBSCRIBE flags");
    let packet_id = &subscribe[..2];
    assert_ne!(packet_id, &[0, 0]);
    let mut fields = &subscribe[2..];
    assert_eq!(read_string(&mut fields), TOPIC.as_bytes());
    assert_eq!(fields, &[qos], "requested subscription QoS");
    stream
        .write_all(&[0x90, 3, packet_id[0], packet_id[1], qos])
        .await
        .expect("SUBACK");
}

fn listener_state() -> (Arc<Mutex<SopEngine>>, Arc<SopAuditLogger>) {
    (
        Arc::new(Mutex::new(SopEngine::new(
            zeroclaw_config::schema::SopConfig::default(),
        ))),
        Arc::new(SopAuditLogger::new(Arc::new(
            zeroclaw_memory::NoneMemory::new("mqtt-test"),
        ))),
    )
}

#[tokio::test]
async fn mqtt_listener_preserves_mqtt311_credentials_keepalive_and_all_subscription_qos() {
    for qos in 0..=2 {
        let broker = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let config = config(broker.local_addr().unwrap().port(), false, qos);
        let (engine, audit) = listener_state();
        timeout(DEADLINE, async {
            tokio::select! {
                result = run_mqtt_sop_listener(&config, engine, audit, None) => {
                    panic!("listener exited before subscription: {result:?}");
                }
                () = async {
                    let (mut stream, _) = broker.accept().await.unwrap();
                    broker_session(&mut stream, qos).await;
                } => {}
            }
        })
        .await
        .expect("listener CONNECT/SUBSCRIBE deadline");
        // select! drops the listener and its socket, including on panic/timeout.
    }
}

struct TlsFixture {
    ca: rustls::pki_types::CertificateDer<'static>,
    acceptor: TlsAcceptor,
}

impl TlsFixture {
    fn new(server_name: &str) -> Self {
        let ca_key = rcgen::KeyPair::generate().unwrap();
        let mut ca_params = rcgen::CertificateParams::new(vec![]).unwrap();
        ca_params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        let ca = ca_params.self_signed(&ca_key).unwrap();
        let server_key = rcgen::KeyPair::generate().unwrap();
        let server = rcgen::CertificateParams::new(vec![server_name.into()])
            .unwrap()
            .signed_by(&server_key, &ca, &ca_key)
            .unwrap();
        let server_config = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::aws_lc_rs::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(
            vec![server.der().clone()],
            rustls::pki_types::PrivatePkcs8KeyDer::from(server_key.serialize_der()).into(),
        )
        .unwrap();
        Self {
            ca: ca.der().clone(),
            acceptor: TlsAcceptor::from(Arc::new(server_config)),
        }
    }

    fn trust_ca(&self, options: &mut MqttOptions) {
        // A private CA is an explicit fixture trust decision. The ordinary
        // certificate and hostname verifier remains active.
        assert!(matches!(options.transport(), Transport::Tls(_)));
        let mut roots = rustls::RootCertStore::empty();
        roots.add(self.ca.clone()).unwrap();
        let client = rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::aws_lc_rs::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_root_certificates(roots)
        .with_no_client_auth();
        options.set_transport(Transport::tls_with_config(TlsConfiguration::Rustls(
            Arc::new(client),
        )));
    }
}

#[tokio::test]
async fn mqtt_tls_with_explicitly_trusted_ca_receives_publish() {
    let fixture = TlsFixture::new("127.0.0.1");
    let broker = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let config = config(broker.local_addr().unwrap().port(), true, 1);
    let mut options = mqtt_options(&config).unwrap();
    fixture.trust_ca(&mut options);
    let (client, mut eventloop) = AsyncClient::builder(options)
        .capacity(64)
        .try_build()
        .unwrap();
    client.subscribe(TOPIC, QoS::AtLeastOnce).await.unwrap();
    let (consumed, receipt) = tokio::sync::oneshot::channel();
    timeout(DEADLINE, async {
        tokio::join!(
            async {
                let (stream, _) = broker.accept().await.unwrap();
                let mut stream = fixture.acceptor.accept(stream).await.unwrap();
                broker_session(&mut stream, 1).await;
                let remaining = 2 + TOPIC.len() + PAYLOAD.len();
                let mut publish = vec![0x30, u8::try_from(remaining).unwrap()];
                publish.extend_from_slice(&u16::try_from(TOPIC.len()).unwrap().to_be_bytes());
                publish.extend_from_slice(TOPIC.as_bytes());
                publish.extend_from_slice(PAYLOAD);
                stream.write_all(&publish).await.unwrap();
                stream.flush().await.unwrap();
                // Keep TLS open while the client consumes the buffered MQTT
                // packet; an abrupt EOF can otherwise mask a valid Publish.
                receipt.await.expect("client consumed Publish");
            },
            async {
                loop {
                    if let Event::Incoming(Packet::Publish(message)) = eventloop
                        .poll()
                        .await
                        .expect("verified TLS MQTT connection")
                    {
                        assert_eq!(message.topic.as_ref(), TOPIC.as_bytes());
                        assert_eq!(message.payload.as_ref(), PAYLOAD);
                        assert_eq!(message.qos, QoS::AtMostOnce);
                        consumed.send(()).expect("broker is still connected");
                        break;
                    }
                }
            }
        );
    })
    .await
    .expect("trusted TLS MQTT deadline");
}

async fn rejected_tls(fixture: &TlsFixture, explicitly_trust_ca: bool) -> rustls::CertificateError {
    let broker = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let config = config(broker.local_addr().unwrap().port(), true, 1);
    let mut options = mqtt_options(&config).unwrap();
    if explicitly_trust_ca {
        fixture.trust_ca(&mut options);
    }
    let (_client, mut eventloop) = AsyncClient::builder(options)
        .capacity(64)
        .try_build()
        .unwrap();
    let (server_result, client_result) = timeout(DEADLINE, async {
        tokio::join!(
            async {
                let (stream, _) = broker.accept().await.unwrap();
                fixture.acceptor.accept(stream).await
            },
            eventloop.poll(),
        )
    })
    .await
    .expect("rejected TLS handshake deadline");
    assert!(server_result.is_err(), "broker must observe TLS rejection");
    let tls_error = match client_result.expect_err("invalid certificate must fail") {
        ConnectionError::Tls(TlsError::Io(error)) => error
            .get_ref()
            .and_then(|inner| inner.downcast_ref::<rustls::Error>())
            .expect("rustls certificate error, not a connection failure")
            .clone(),
        ConnectionError::Tls(TlsError::TLS(error)) => error,
        error => panic!("expected certificate verification failure, got {error:?}"),
    };
    match tls_error {
        rustls::Error::InvalidCertificate(error) => error,
        error => panic!("expected invalid certificate, got {error:?}"),
    }
}

#[tokio::test]
async fn mqtt_default_tls_rejects_untrusted_ca() {
    let error = rejected_tls(&TlsFixture::new("127.0.0.1"), false).await;
    assert_eq!(error, rustls::CertificateError::UnknownIssuer);
}

#[tokio::test]
async fn mqtt_tls_rejects_wrong_hostname_even_with_trusted_ca() {
    let error = rejected_tls(&TlsFixture::new("other.example.invalid"), true).await;
    assert!(matches!(
        error,
        rustls::CertificateError::NotValidForName
            | rustls::CertificateError::NotValidForNameContext { .. }
    ));
}

#[tokio::test]
async fn mqtt_tls_listener_reconnect_never_falls_back_to_plaintext() {
    let broker = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let config = config(broker.local_addr().unwrap().port(), true, 1);
    let (engine, audit) = listener_state();
    timeout(DEADLINE, async {
        tokio::select! {
            result = run_mqtt_sop_listener(&config, engine, audit, None) => {
                panic!("listener exited before TLS retry: {result:?}");
            }
            () = async {
                for _ in 0..2 {
                    let (mut stream, _) = broker.accept().await.unwrap();
                    let mut first_bytes = [0; 3];
                    stream.read_exact(&mut first_bytes).await.unwrap();
                    assert_eq!(first_bytes[0], 0x16, "TLS handshake, not MQTT CONNECT");
                    assert_eq!(first_bytes[1], 0x03, "TLS record version");
                    // A plaintext broker response must fail the handshake. The
                    // next attempt must send another ClientHello, never secrets.
                    stream.write_all(&[0x20, 2, 0, 0]).await.unwrap();
                }
            } => {}
        }
    })
    .await
    .expect("TLS reconnect deadline");
}

#[test]
fn mqtt_options_rejects_keepalive_that_cannot_fit_mqtt311() {
    let mut config = config(1883, false, 1);
    config.keep_alive_secs = u64::from(u16::MAX) + 1;
    assert!(mqtt_options(&config).is_err());
    config.keep_alive_secs = u64::from(u16::MAX);
    assert!(mqtt_options(&config).is_ok());
}
