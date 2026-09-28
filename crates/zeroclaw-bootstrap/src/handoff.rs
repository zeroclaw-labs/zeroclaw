//! Handoff to the installed control server.
//!
//! Handoff is the last thing the launcher does. It verifies that the binary at
//! the install path is the one it verified, that the server it starts
//! advertises a control protocol this launcher understands, and that the
//! advertisement is complete — then it replaces itself with the real server
//! and stops being part of the system. It configures nothing and reads no
//! config.

use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use crate::error::BootstrapError;
use crate::status::require_verified_version;

/// Major version of the control protocol this launcher speaks. A different
/// major version fails closed; the launcher never downgrades.
pub const SUPPORTED_CONTROL_MAJOR: u32 = 1;

/// Human-readable form of the accepted range, for refusal messages.
pub const SUPPORTED_CONTROL_RANGE: &str = ">=1.0 and <2.0";

/// MCP lifecycle protocol version the launcher declares.
const MCP_PROTOCOL_VERSION: &str = "2025-06-18";

/// How long the launcher waits for the server's `initialize` result.
const PROBE_TIMEOUT: Duration = Duration::from_secs(20);

/// The advertisement block a verified control server returns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Advertisement {
    /// Product version of the running server.
    pub zeroclaw_version: String,
    /// `major.minor` of the control protocol.
    pub control_protocol_version: String,
    /// Canonical config schema version.
    pub config_schema_version: i64,
    /// Capability identifiers the server implements.
    pub capabilities: Vec<String>,
    /// Digest over the canonical capability set.
    pub capability_digest: String,
}

/// Parses and validates the advertisement carried by an `initialize` result.
///
/// The carrier is `result._meta.zeroclaw_control`, per the control MCP
/// protocol v1 specification. Every field is required: a partial
/// advertisement is a refusal, not a server the launcher fills in defaults
/// for.
pub fn parse_advertisement(
    initialize_result: &serde_json::Value,
) -> Result<Advertisement, BootstrapError> {
    let missing = |field: &'static str| BootstrapError::AdvertisementIncomplete { field };

    let block = initialize_result
        .get("result")
        .and_then(|r| r.get("_meta"))
        .and_then(|m| m.get("zeroclaw_control"))
        .ok_or(missing("result._meta.zeroclaw_control"))?;

    let zeroclaw_version = block
        .get("zeroclaw_version")
        .and_then(serde_json::Value::as_str)
        .filter(|v| !v.is_empty())
        .ok_or(missing("zeroclaw_version"))?
        .to_string();

    let control_protocol_version = block
        .get("control_protocol_version")
        .and_then(serde_json::Value::as_str)
        .filter(|v| !v.is_empty())
        .ok_or(missing("control_protocol_version"))?
        .to_string();

    let config_schema_version = block
        .get("config_schema_version")
        .and_then(serde_json::Value::as_i64)
        .ok_or(missing("config_schema_version"))?;

    let capabilities_value = block
        .get("capabilities")
        .and_then(serde_json::Value::as_array)
        .ok_or(missing("capabilities"))?;
    let mut capabilities = Vec::with_capacity(capabilities_value.len());
    for entry in capabilities_value {
        capabilities.push(entry.as_str().ok_or(missing("capabilities"))?.to_string());
    }

    let capability_digest = block
        .get("capability_digest")
        .and_then(serde_json::Value::as_str)
        .filter(|digest| is_sha256_digest(digest))
        .ok_or(missing("capability_digest"))?
        .to_string();

    Ok(Advertisement {
        zeroclaw_version,
        control_protocol_version,
        config_schema_version,
        capabilities,
        capability_digest,
    })
}

fn is_sha256_digest(value: &str) -> bool {
    value
        .strip_prefix("sha256:")
        .is_some_and(|hex| hex.len() == 64 && hex.chars().all(|c| c.is_ascii_hexdigit()))
}

/// Checks the advertised protocol version against this launcher's range.
pub fn check_protocol_version(advertised: &str) -> Result<(), BootstrapError> {
    let refuse = || BootstrapError::UnsupportedProtocolVersion {
        advertised: advertised.to_string(),
        supported: SUPPORTED_CONTROL_RANGE.to_string(),
    };
    let major = advertised
        .split('.')
        .next()
        .and_then(|m| m.parse::<u32>().ok())
        .ok_or_else(refuse)?;
    // A `major.minor` string is required; a bare major is not a version.
    if advertised.split('.').count() < 2 {
        return Err(refuse());
    }
    if major != SUPPORTED_CONTROL_MAJOR {
        return Err(refuse());
    }
    Ok(())
}

/// Checks the advertisement against the identity the launcher verified on
/// disk.
pub fn check_identity(
    advertisement: &Advertisement,
    verified_version: &str,
    verified_binary_digest: &str,
    expected_binary_digest: Option<&str>,
) -> Result<(), BootstrapError> {
    check_protocol_version(&advertisement.control_protocol_version)?;

    if advertisement.zeroclaw_version != verified_version {
        return Err(BootstrapError::ProductVersionMismatch {
            expected: verified_version.to_string(),
            advertised: advertisement.zeroclaw_version.clone(),
        });
    }

    if let Some(expected) = expected_binary_digest
        && expected != verified_binary_digest
    {
        return Err(BootstrapError::ExecutableIdentityMismatch {
            expected: expected.to_string(),
            actual: verified_binary_digest.to_string(),
        });
    }

    Ok(())
}

/// Result of verifying a control server before handing off to it.
#[derive(Debug, Clone)]
pub struct VerifiedHandoff {
    /// Advertisement the server returned.
    pub advertisement: Advertisement,
    /// Version read from the binary on disk.
    pub verified_version: String,
    /// SHA-256 of the binary on disk.
    pub binary_digest: String,
}

impl VerifiedHandoff {
    /// Human-readable verification summary.
    ///
    /// The trailing `next` block names the route's destination: once handoff
    /// execs the server, configuration happens on the control surface. It stays
    /// honest about what that surface is — read-only by default, genesis first
    /// when the instance has no trust root, and mutations gated behind the
    /// separate operator enablement ceremony — so the reader never mistakes
    /// "configure" for "mutate without approval".
    pub fn render(&self) -> String {
        format!(
            "Handoff verification\n\
             \x20 binary sha256     {}\n\
             \x20 product version   {} (binary and advertisement agree)\n\
             \x20 control protocol  {} (accepted range {})\n\
             \x20 config schema     {}\n\
             \x20 capabilities      {}\n\
             \x20 capability digest {}\n\
             \x20 next              configure this instance on the control server this hands off to:\n\
             \x20                   run `zeroclaw control genesis` first if it has no trust root yet;\n\
             \x20                   the surface is read-only until an operator enables mutations\n",
            self.binary_digest,
            self.verified_version,
            self.advertisement.control_protocol_version,
            SUPPORTED_CONTROL_RANGE,
            self.advertisement.config_schema_version,
            if self.advertisement.capabilities.is_empty() {
                "(none)".to_string()
            } else {
                self.advertisement.capabilities.join(", ")
            },
            self.advertisement.capability_digest,
        )
    }
}

/// Starts the control server, reads its `initialize` result, verifies it, and
/// shuts the probe down.
///
/// The probe is a separate short-lived process from the server the harness
/// will actually talk to. Verifying and then exec'ing means the harness never
/// speaks to a server the launcher has not checked.
pub fn verify(
    binary_path: &Path,
    expected_binary_digest: Option<&str>,
) -> Result<VerifiedHandoff, BootstrapError> {
    let (verified_version, binary_digest) = require_verified_version(binary_path)?;

    let response = probe_initialize(binary_path)?;
    let advertisement = parse_advertisement(&response)?;
    check_identity(
        &advertisement,
        &verified_version,
        &binary_digest,
        expected_binary_digest,
    )?;

    Ok(VerifiedHandoff {
        advertisement,
        verified_version,
        binary_digest,
    })
}

/// Sends one `initialize` request over stdio and returns the parsed response.
fn probe_initialize(binary_path: &Path) -> Result<serde_json::Value, BootstrapError> {
    let mut child = Command::new(binary_path)
        .arg("control")
        .arg("--mcp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|err| BootstrapError::HandoffProbeFailed {
            reason: format!("could not start `control --mcp`: {err}"),
        })?;

    let request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": MCP_PROTOCOL_VERSION,
            "clientInfo": {
                "name": "zeroclaw-bootstrap",
                "version": env!("CARGO_PKG_VERSION"),
            },
            "capabilities": {},
            "_meta": {
                "zeroclaw_control": {
                    "supported_control_protocol_versions": [format!("{SUPPORTED_CONTROL_MAJOR}.0")],
                }
            }
        }
    });

    let probe_result = (|| -> Result<serde_json::Value, String> {
        let mut stdin = child.stdin.take().ok_or("child stdin unavailable")?;
        let stdout = child.stdout.take().ok_or("child stdout unavailable")?;

        let mut line = serde_json::to_string(&request).map_err(|err| err.to_string())?;
        line.push('\n');
        stdin
            .write_all(line.as_bytes())
            .and_then(|()| stdin.flush())
            .map_err(|err| format!("writing initialize: {err}"))?;

        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let reader = BufReader::new(stdout);
            for line in reader.lines() {
                let Ok(line) = line else { break };
                if line.trim().is_empty() {
                    continue;
                }
                let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) else {
                    continue;
                };
                // Skip notifications and unrelated ids; only the reply to the
                // launcher's own request counts.
                if value.get("id").and_then(serde_json::Value::as_i64) != Some(1) {
                    continue;
                }
                let _ = tx.send(value);
                break;
            }
        });

        rx.recv_timeout(PROBE_TIMEOUT).map_err(|_| {
            format!(
                "no initialize result within {} seconds",
                PROBE_TIMEOUT.as_secs()
            )
        })
    })();

    // The probe process has served its purpose either way.
    let _ = child.kill();
    let _ = child.wait();

    let response = probe_result.map_err(|reason| BootstrapError::HandoffProbeFailed { reason })?;

    if let Some(error) = response.get("error") {
        return Err(BootstrapError::HandoffProbeFailed {
            reason: format!("server returned a JSON-RPC error: {error}"),
        });
    }

    Ok(response)
}

/// Replaces this process with the verified control server.
///
/// On Unix this never returns on success. On other platforms the server is
/// run as a child and its exit code is propagated.
pub fn exec_control_server(binary_path: &Path) -> Result<std::convert::Infallible, BootstrapError> {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let err = Command::new(binary_path).arg("control").arg("--mcp").exec();
        Err(BootstrapError::io(
            format!("replacing this process with {}", binary_path.display()),
            &err,
        ))
    }
    #[cfg(not(unix))]
    {
        let status = Command::new(binary_path)
            .arg("control")
            .arg("--mcp")
            .status()
            .map_err(|err| {
                BootstrapError::io(format!("running {}", binary_path.display()), &err)
            })?;
        std::process::exit(status.code().unwrap_or(1));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn advertisement_json(protocol: &str) -> serde_json::Value {
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {
                "protocolVersion": MCP_PROTOCOL_VERSION,
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "zeroclaw-control", "version": "0.8.4" },
                "_meta": {
                    "zeroclaw_control": {
                        "zeroclaw_version": "0.8.4",
                        "control_protocol_version": protocol,
                        "config_schema_version": 3,
                        "capabilities": ["agents"],
                        "capability_digest": format!("sha256:{}", "a".repeat(64)),
                    }
                }
            }
        })
    }

    #[test]
    fn parses_a_complete_advertisement() {
        let parsed = parse_advertisement(&advertisement_json("1.0")).expect("complete block");
        assert_eq!(parsed.zeroclaw_version, "0.8.4");
        assert_eq!(parsed.control_protocol_version, "1.0");
        assert_eq!(parsed.config_schema_version, 3);
        assert_eq!(parsed.capabilities, vec!["agents".to_string()]);
        assert!(parsed.capability_digest.starts_with("sha256:"));
    }

    #[test]
    fn refuses_a_missing_carrier() {
        let bare = serde_json::json!({ "jsonrpc": "2.0", "id": 1, "result": {} });
        assert!(matches!(
            parse_advertisement(&bare),
            Err(BootstrapError::AdvertisementIncomplete {
                field: "result._meta.zeroclaw_control"
            })
        ));
    }

    #[test]
    fn refuses_each_missing_advertisement_field() {
        for field in [
            "zeroclaw_version",
            "control_protocol_version",
            "config_schema_version",
            "capabilities",
            "capability_digest",
        ] {
            let mut value = advertisement_json("1.0");
            value["result"]["_meta"]["zeroclaw_control"]
                .as_object_mut()
                .expect("object")
                .remove(field);
            assert!(
                matches!(
                    parse_advertisement(&value),
                    Err(BootstrapError::AdvertisementIncomplete { .. })
                ),
                "removing `{field}` must be refused"
            );
        }
    }

    #[test]
    fn refuses_a_malformed_capability_digest() {
        let mut value = advertisement_json("1.0");
        value["result"]["_meta"]["zeroclaw_control"]["capability_digest"] =
            serde_json::json!("not-a-digest");
        assert!(parse_advertisement(&value).is_err());
    }

    #[test]
    fn accepts_supported_protocol_minors_and_refuses_other_majors() {
        assert!(check_protocol_version("1.0").is_ok());
        assert!(check_protocol_version("1.7").is_ok());
        for bad in ["2.0", "0.9", "3.1", "abc", "1", ""] {
            assert!(
                check_protocol_version(bad).is_err(),
                "protocol `{bad}` must be refused"
            );
        }
    }

    #[test]
    fn refuses_a_product_version_the_binary_does_not_report() {
        let advertisement = parse_advertisement(&advertisement_json("1.0")).expect("parses");
        assert!(matches!(
            check_identity(&advertisement, "0.7.0", "deadbeef", None),
            Err(BootstrapError::ProductVersionMismatch { .. })
        ));
    }

    #[test]
    fn refuses_a_binary_that_is_not_the_verified_artifact() {
        let advertisement = parse_advertisement(&advertisement_json("1.0")).expect("parses");
        assert!(matches!(
            check_identity(&advertisement, "0.8.4", "aaaa", Some("bbbb")),
            Err(BootstrapError::ExecutableIdentityMismatch { .. })
        ));
        assert!(check_identity(&advertisement, "0.8.4", "aaaa", Some("aaaa")).is_ok());
    }
}
