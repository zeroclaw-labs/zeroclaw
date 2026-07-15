//! Local operator control path for minting additional enrollment pairing codes.
//!
//! This deliberately does not live on the enrollment HTTP route: relay-routed
//! enrollment traffic reaches the daemon over loopback, so a "localhost only"
//! HTTP admin endpoint would be indistinguishable from a remote relay client.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use super::EnrollServer;

const REQUESTS_DIR: &str = "enroll_paircode_requests";
const RESPONSES_DIR: &str = "enroll_paircode_responses";

#[derive(Debug, Serialize, Deserialize)]
struct PaircodeRequest {
    id: String,
    action: String,
    created_at_unix: i64,
}

#[derive(Debug, Serialize, Deserialize)]
struct PaircodeResponse {
    id: String,
    success: bool,
    pairing_code: Option<String>,
    sas: Option<String>,
    message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeneratedEnrollmentPaircode {
    pub pairing_code: String,
    pub sas: String,
}

pub async fn request_new_paircode(
    data_dir: &Path,
    timeout: Duration,
) -> Result<GeneratedEnrollmentPaircode> {
    let id = request_id();
    let request = PaircodeRequest {
        id: id.clone(),
        action: "new".to_string(),
        created_at_unix: now_unix(),
    };
    let requests = requests_dir(data_dir);
    let responses = responses_dir(data_dir);
    std::fs::create_dir_all(&requests).with_context(|| format!("create {}", requests.display()))?;
    std::fs::create_dir_all(&responses)
        .with_context(|| format!("create {}", responses.display()))?;

    let request_path = requests.join(format!("{id}.json"));
    let response_path = responses.join(format!("{id}.json"));
    write_json_atomic(&request_path, &request)?;

    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        match std::fs::read(&response_path) {
            Ok(bytes) => {
                let _ = std::fs::remove_file(&response_path);
                let response: PaircodeResponse =
                    serde_json::from_slice(&bytes).context("parse paircode response")?;
                if !response.success {
                    anyhow::bail!("{}", response.message);
                }
                let pairing_code = response
                    .pairing_code
                    .filter(|v| !v.trim().is_empty())
                    .context("daemon response omitted pairing_code")?;
                let sas = response
                    .sas
                    .filter(|v| !v.trim().is_empty())
                    .context("daemon response omitted SAS")?;
                return Ok(GeneratedEnrollmentPaircode { pairing_code, sas });
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error).with_context(|| format!("read {}", response_path.display()));
            }
        }
        if tokio::time::Instant::now() >= deadline {
            let _ = std::fs::remove_file(&request_path);
            anyhow::bail!(
                "timed out waiting for running daemon to mint an enrollment pairing code"
            );
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

pub(crate) fn spawn_request_loop(
    server: std::sync::Arc<EnrollServer>,
    data_dir: PathBuf,
    cancel: CancellationToken,
) {
    zeroclaw_spawn::spawn!(async move {
        if let Err(error) = request_loop(server, data_dir, cancel).await {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_attrs(::serde_json::json!({ "error": format!("{error:#}") })),
                "enrollment paircode admin loop exited"
            );
        }
    });
}

async fn request_loop(
    server: std::sync::Arc<EnrollServer>,
    data_dir: PathBuf,
    cancel: CancellationToken,
) -> Result<()> {
    let requests = requests_dir(&data_dir);
    let responses = responses_dir(&data_dir);
    std::fs::create_dir_all(&requests).with_context(|| format!("create {}", requests.display()))?;
    std::fs::create_dir_all(&responses)
        .with_context(|| format!("create {}", responses.display()))?;

    loop {
        process_requests(&server, &requests, &responses)?;
        tokio::select! {
            _ = cancel.cancelled() => return Ok(()),
            _ = tokio::time::sleep(Duration::from_millis(250)) => {}
        }
    }
}

fn process_requests(server: &EnrollServer, requests: &Path, responses: &Path) -> Result<()> {
    for entry in
        std::fs::read_dir(requests).with_context(|| format!("read {}", requests.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(_) => continue,
        };
        let request: PaircodeRequest = match serde_json::from_slice(&bytes) {
            Ok(request) => request,
            Err(error) => {
                let _ = std::fs::remove_file(&path);
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                        .with_attrs(::serde_json::json!({ "error": error.to_string() })),
                    "discarded malformed enrollment paircode request"
                );
                continue;
            }
        };
        let response = handle_request(server, request);
        write_json_atomic(&responses.join(format!("{}.json", response.id)), &response)?;
        let _ = std::fs::remove_file(&path);
    }
    Ok(())
}

fn handle_request(server: &EnrollServer, request: PaircodeRequest) -> PaircodeResponse {
    if request.action != "new" {
        return PaircodeResponse {
            id: request.id,
            success: false,
            pairing_code: None,
            sas: None,
            message: format!("unknown enrollment paircode action: {}", request.action),
        };
    }

    match server.pairing.generate_new_pairing_code() {
        Some(code) => match sas_for_code(&server.ca_cert_pem, &code) {
            Ok(sas) => PaircodeResponse {
                id: request.id,
                success: true,
                pairing_code: Some(code),
                sas: Some(sas),
                message: "New enrollment pairing code generated".to_string(),
            },
            Err(error) => PaircodeResponse {
                id: request.id,
                success: false,
                pairing_code: None,
                sas: None,
                message: format!("could not compute enrollment SAS: {error:#}"),
            },
        },
        None => PaircodeResponse {
            id: request.id,
            success: false,
            pairing_code: None,
            sas: None,
            message: "enrollment pairing is disabled".to_string(),
        },
    }
}

fn sas_for_code(ca_cert_pem: &str, code: &str) -> Result<String> {
    let fingerprint = zeroclaw_tls::single_cert_pem_sha256_fingerprint(ca_cert_pem)?;
    Ok(zeroclaw_tls::enrollment_sas(code, &fingerprint))
}

fn requests_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("state").join(REQUESTS_DIR)
}

fn responses_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("state").join(RESPONSES_DIR)
}

fn write_json_atomic<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let tmp = path.with_extension("json.tmp");
    let bytes = serde_json::to_vec(value)?;
    std::fs::write(&tmp, bytes).with_context(|| format!("write {}", tmp.display()))?;
    std::fs::rename(&tmp, path)
        .with_context(|| format!("rename {} to {}", tmp.display(), path.display()))?;
    Ok(())
}

fn request_id() -> String {
    use ring::rand::{SecureRandom, SystemRandom};
    let mut bytes = [0u8; 16];
    if SystemRandom::new().fill(&mut bytes).is_ok() {
        return hex::encode(bytes);
    }
    format!("{}-{}", std::process::id(), now_unix())
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use zeroclaw_config::pairing::PairingGuard;

    use super::*;
    use crate::enroll::RelayProfile;
    use crate::security::cert_ledger::CertLedger;

    fn test_server(pairing: PairingGuard) -> Arc<EnrollServer> {
        let (ca_cert, ca_key) = zeroclaw_tls::testing::gen_ca();
        let (srv_cert, srv_key) =
            zeroclaw_tls::testing::gen_server_cert(&ca_cert, &ca_key, &["localhost".into()]);
        let dir = tempfile::tempdir().unwrap();
        let cert_path = dir.path().join("server.crt");
        let key_path = dir.path().join("server.key");
        std::fs::write(&cert_path, &srv_cert).unwrap();
        std::fs::write(&key_path, &srv_key).unwrap();
        let acceptor = zeroclaw_tls::build_tls_acceptor(&zeroclaw_tls::ServerConfigParams {
            cert_path: cert_path.to_string_lossy().into_owned(),
            key_path: key_path.to_string_lossy().into_owned(),
            client_auth: None,
        })
        .unwrap();

        Arc::new(EnrollServer {
            bind_addr: "127.0.0.1:0".parse().unwrap(),
            acceptor,
            ca_cert_pem: ca_cert,
            ca_key_pem: zeroize::Zeroizing::new(ca_key),
            ledger: Arc::new(CertLedger::open_in_memory(None).unwrap()),
            pairing: Arc::new(pairing),
            static_client_pins_configured: false,
            allow_unpaired_until: None,
            relay_profile: RelayProfile::default(),
            paircode_admin_data_dir: None,
        })
    }

    #[tokio::test]
    async fn running_loop_mints_additional_code_without_restart() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let data_dir = tempfile::tempdir().unwrap();
        let pairing = PairingGuard::new(true, &[String::from("already-paired-token")]);
        assert!(pairing.pairing_code().is_none());

        let server = test_server(pairing.clone());
        let cancel = CancellationToken::new();
        let loop_data_dir = data_dir.path().to_path_buf();
        let loop_cancel = cancel.clone();
        let loop_handle = zeroclaw_spawn::spawn!(request_loop(server, loop_data_dir, loop_cancel));

        let generated = request_new_paircode(data_dir.path(), Duration::from_secs(2))
            .await
            .unwrap();
        assert!(!generated.pairing_code.trim().is_empty());
        assert_eq!(
            pairing.pairing_code().as_deref(),
            Some(generated.pairing_code.as_str())
        );

        let reservation = pairing
            .reserve_pair(&generated.pairing_code, "test-device")
            .await
            .unwrap()
            .expect("freshly minted code should be redeemable");
        reservation.commit();
        assert!(pairing.pairing_code().is_none());

        cancel.cancel();
        loop_handle.await.unwrap().unwrap();
    }
}
