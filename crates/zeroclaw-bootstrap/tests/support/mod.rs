//! Local fixtures for the bootstrap launcher tests.
//!
//! Nothing here touches the network. The origin is a `HashMap` keyed by the
//! exact URL the launcher constructs, so a test that expects a fetch must
//! name the same pinned URL the production path would build — a launcher that
//! silently fetched from somewhere else would fail these tests rather than
//! pass them.

#![allow(dead_code)]

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use zeroclaw_bootstrap::error::BootstrapError;
use zeroclaw_bootstrap::fetch::{Fetcher, sha256_hex};
use zeroclaw_bootstrap::origin::PinnedUrl;

/// An in-process stand-in for the pinned release origin.
#[derive(Default)]
pub struct FixtureOrigin {
    bodies: HashMap<String, Vec<u8>>,
    /// URLs that were actually requested, in order.
    pub requested: std::cell::RefCell<Vec<String>>,
}

impl FixtureOrigin {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with(mut self, url: &PinnedUrl, body: impl Into<Vec<u8>>) -> Self {
        self.bodies.insert(url.as_str().to_string(), body.into());
        self
    }
}

impl Fetcher for FixtureOrigin {
    fn fetch(&self, url: &PinnedUrl) -> Result<Vec<u8>, BootstrapError> {
        self.requested.borrow_mut().push(url.as_str().to_string());
        self.bodies
            .get(url.as_str())
            .cloned()
            .ok_or_else(|| BootstrapError::Transport {
                url: url.as_str().to_string(),
                reason: "fixture origin has no such asset".to_string(),
            })
    }
}

/// A fetcher that refuses every request, for tests that must prove no
/// download was attempted.
pub struct NeverFetches;

impl Fetcher for NeverFetches {
    fn fetch(&self, url: &PinnedUrl) -> Result<Vec<u8>, BootstrapError> {
        panic!("the launcher must not fetch {} on this path", url.as_str());
    }
}

/// Builds a `SHA256SUMS` body listing each `(asset_name, body)` pair.
pub fn checksum_manifest(entries: &[(&str, &[u8])]) -> String {
    let mut out = String::new();
    for (name, body) in entries {
        out.push_str(&format!("{}  {name}\n", sha256_hex(body)));
    }
    out
}

/// Builds a gzip tarball containing the given top-level `(name, bytes)`
/// entries.
pub fn tar_gz(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut builder = tar::Builder::new(Vec::new());
    for (name, bytes) in entries {
        let mut header = tar::Header::new_gnu();
        header.set_size(bytes.len() as u64);
        header.set_mode(0o755);
        header.set_cksum();
        builder
            .append_data(&mut header, name, *bytes)
            .expect("fixture tar entry");
    }
    let tar_bytes = builder.into_inner().expect("fixture tar");

    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    encoder.write_all(&tar_bytes).expect("fixture gzip");
    encoder.finish().expect("fixture gzip")
}

/// Builds a gzip tarball whose entry names are written straight into the tar
/// header, bypassing the `tar` crate's own refusal to *create* traversal
/// paths. A hostile archive would be built by exactly such a tool, so the
/// launcher has to be tested against one.
pub fn tar_gz_with_raw_names(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut builder = tar::Builder::new(Vec::new());
    for (name, bytes) in entries {
        let mut header = tar::Header::new_ustar();
        let raw = name.as_bytes();
        assert!(raw.len() < 100, "fixture name too long for a ustar header");
        header.as_old_mut().name[..raw.len()].copy_from_slice(raw);
        header.set_size(bytes.len() as u64);
        header.set_mode(0o755);
        header.set_entry_type(tar::EntryType::Regular);
        header.set_cksum();
        builder.append(&header, *bytes).expect("raw tar entry");
    }
    let tar_bytes = builder.into_inner().expect("fixture tar");

    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    encoder.write_all(&tar_bytes).expect("fixture gzip");
    encoder.finish().expect("fixture gzip")
}

/// Writes an executable file, returning its path.
#[cfg(unix)]
pub fn write_executable(dir: &Path, name: &str, contents: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;

    let path = dir.join(name);
    std::fs::write(&path, contents).expect("fixture script");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
        .expect("fixture script mode");
    path
}

/// A stub that answers `--version` and one `initialize` request, standing in
/// for an installed `zeroclaw control --mcp`.
#[cfg(unix)]
pub fn control_server_stub(
    dir: &Path,
    name: &str,
    version: &str,
    initialize_response: &str,
) -> PathBuf {
    assert!(
        !initialize_response.contains('\''),
        "fixture response must not contain a single quote"
    );
    let script = format!(
        "#!/bin/sh\n\
         if [ \"$1\" = \"--version\" ]; then\n\
         \x20 echo 'zeroclaw {version}'\n\
         \x20 exit 0\n\
         fi\n\
         if [ \"$1\" = \"control\" ]; then\n\
         \x20 while IFS= read -r line; do\n\
         \x20   case \"$line\" in\n\
         \x20     *initialize*)\n\
         \x20       printf '%s\\n' '{initialize_response}'\n\
         \x20       exit 0\n\
         \x20       ;;\n\
         \x20   esac\n\
         \x20 done\n\
         fi\n\
         exit 1\n"
    );
    write_executable(dir, name, &script)
}

/// The advertisement block the control MCP protocol v1 specification defines,
/// carried at `result._meta.zeroclaw_control`.
pub fn initialize_response(version: &str, control_protocol_version: &str) -> String {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "result": {
            "protocolVersion": "2025-06-18",
            "capabilities": { "tools": {} },
            "serverInfo": { "name": "zeroclaw-control", "version": version },
            "_meta": {
                "zeroclaw_control": {
                    "zeroclaw_version": version,
                    "control_protocol_version": control_protocol_version,
                    "config_schema_version": 3,
                    "capabilities": ["agents"],
                    "capability_digest": format!("sha256:{}", "a".repeat(64)),
                }
            }
        }
    })
    .to_string()
}
