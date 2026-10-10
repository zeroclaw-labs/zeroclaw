//! Sign a plugin manifest for distribution.
//!
//! ZeroClaw has no `plugin sign` command; publishers sign from their release
//! pipeline. This example wraps the crate's own signing functions, so the
//! bytes it signs are the bytes the host verifies. Run it from a ZeroClaw
//! checkout with paths into the plugin's directory, and keep the private key
//! outside both trees:
//!
//! ```text
//! cargo run -p zeroclaw-plugins --example sign_manifest -- keygen ~/.keys/my-plugin.pk8
//! cargo run -p zeroclaw-plugins --example sign_manifest -- \
//!     sign path/to/my-plugin/manifest.toml path/to/my-plugin/signed/manifest.toml \
//!     --key ~/.keys/my-plugin.pk8 --payload path/to/my-plugin/my-plugin.wasm
//! ```
//!
//! `keygen` writes a new PKCS#8 Ed25519 private key and prints its hex public
//! key, the value operators add to `plugins.security.trusted_publisher_keys`.
//! It creates the key's directory when needed, readable by the owner only on
//! Unix, and refuses to overwrite an existing file.
//!
//! `sign` writes the signed manifest, creating its directory when needed, and
//! prints the same public key. For a package that ships a component
//! (`wasm_path`), `--payload` first records the component's SHA-256 in the
//! root `wasm_sha256` entry. Strict signature mode refuses such a package
//! without that entry, so `sign` warns when a manifest with `wasm_path` has
//! none. A manifest without `wasm_path`, such as a skill bundle, takes no
//! payload: the host rejects a digest there. The signature is checked with
//! the host's verifier before the manifest is written.

use std::fs;
use std::io::Write;
use std::path::Path;

use anyhow::{Context, Result, bail, ensure};
use zeroclaw_plugins::signature::{self, SignatureMode};
use zeroize::Zeroizing;

const USAGE: &str = "usage:
  sign_manifest keygen <private-key-out>
  sign_manifest sign <manifest-in> <manifest-out> --key <private-key> [--payload <component>]";

const PAYLOAD_DIGEST: &str = "wasm_sha256";

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    run(&args)
}

fn run(args: &[String]) -> Result<()> {
    match args.split_first() {
        Some((command, rest)) if command == "keygen" => keygen(rest),
        Some((command, rest)) if command == "sign" => sign(rest),
        _ => bail!(USAGE),
    }
}

fn keygen(args: &[String]) -> Result<()> {
    let [key_path] = args else { bail!(USAGE) };
    let (private_key, public_key) = signature::generate_signing_key()?;
    let private_key = Zeroizing::new(private_key);
    write_private_key(Path::new(key_path), &private_key)?;
    println!("{public_key}");
    Ok(())
}

fn sign(args: &[String]) -> Result<()> {
    let mut positional = Vec::new();
    let mut key_path = None;
    let mut payload_path = None;
    let mut remaining = args.iter();
    while let Some(arg) = remaining.next() {
        match arg.as_str() {
            "--key" => key_path = Some(remaining.next().context(USAGE)?),
            "--payload" => payload_path = Some(remaining.next().context(USAGE)?),
            _ => positional.push(arg),
        }
    }
    let [manifest_in, manifest_out] = positional.as_slice() else {
        bail!(USAGE)
    };
    let key_path = key_path.context(USAGE)?;

    let private_key = Zeroizing::new(
        fs::read(key_path).with_context(|| format!("read the private key {key_path}"))?,
    );
    let mut manifest = fs::read_to_string(manifest_in)
        .with_context(|| format!("read the manifest {manifest_in}"))?;
    if let Some(payload_path) = payload_path {
        let payload =
            fs::read(payload_path).with_context(|| format!("read the component {payload_path}"))?;
        manifest = record_payload_digest(&manifest, &payload)?;
    }

    let signed = signature::sign_manifest_document(&manifest, &private_key)?;
    let public_key = signature::public_key_hex(&private_key)?;
    let root = verify_signature(&signed, &public_key)?;
    let ships_component = root.contains_key("wasm_path");
    ensure!(
        ships_component || payload_path.is_none(),
        "the manifest declares no wasm_path, so it takes no --payload: hosts reject a \
         {PAYLOAD_DIGEST} entry without one"
    );
    if ships_component && !root.contains_key(PAYLOAD_DIGEST) {
        eprintln!(
            "warning: the manifest has no {PAYLOAD_DIGEST}; hosts in strict signature mode \
             refuse it. Pass --payload to record the component's digest."
        );
    }

    write_manifest(Path::new(manifest_out), &signed)?;
    println!("{public_key}");
    Ok(())
}

/// Set the root `wasm_sha256` entry to the digest of the component being
/// shipped, replacing a stale value from an earlier build.
fn record_payload_digest(manifest: &str, payload: &[u8]) -> Result<String> {
    let mut document = manifest
        .parse::<toml_edit::DocumentMut>()
        .context("the manifest is not valid TOML")?;
    document.as_table_mut().insert(
        PAYLOAD_DIGEST,
        toml_edit::value(signature::sha256_hex(payload)),
    );
    Ok(document.to_string())
}

/// Check the embedded signature the way a host in strict mode does, trusting
/// only the key that signed it. Returns the parsed root table.
fn verify_signature(signed: &str, public_key: &str) -> Result<toml::Table> {
    let root: toml::Table = toml::from_str(signed).context("the signed manifest does not parse")?;
    let name = root
        .get("name")
        .and_then(toml::Value::as_str)
        .unwrap_or("manifest");
    let verdict = signature::enforce_signature_policy(
        name,
        signed,
        root.get("signature").and_then(toml::Value::as_str),
        root.get("publisher_key").and_then(toml::Value::as_str),
        &[public_key.to_string()],
        SignatureMode::Strict,
    )?;
    ensure!(
        verdict.is_valid(),
        "the signed manifest did not verify: {verdict:?}"
    );
    Ok(root)
}

fn write_manifest(path: &Path, signed: &str) -> Result<()> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)
            .with_context(|| format!("create the directory {}", parent.display()))?;
    }
    fs::write(path, signed).with_context(|| format!("write the signed manifest {}", path.display()))
}

fn write_private_key(path: &Path, private_key: &[u8]) -> Result<()> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        create_private_dir(parent)?;
    }
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .with_context(|| format!("create the private key file {}", path.display()))?;
    file.write_all(private_key)
        .with_context(|| format!("write the private key file {}", path.display()))?;
    Ok(())
}

/// Create the directories a private key will live in. New directories are
/// readable by the owner only on Unix; an existing directory keeps its mode.
fn create_private_dir(dir: &Path) -> Result<()> {
    if dir.is_dir() {
        return Ok(());
    }
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder
        .create(dir)
        .with_context(|| format!("create the private key directory {}", dir.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn command(args: &[&str]) -> Result<()> {
        run(&args
            .iter()
            .map(|arg| (*arg).to_string())
            .collect::<Vec<_>>())
    }

    #[test]
    fn keygen_creates_private_key_and_refuses_overwrite() {
        let directory = tempfile::tempdir().expect("isolated keygen fixture");
        let key_path = directory.path().join("private").join("publisher.pk8");
        let key_arg = key_path.to_str().expect("fixture path is UTF-8");

        command(&["keygen", key_arg]).expect("keygen succeeds");
        let original = Zeroizing::new(fs::read(&key_path).expect("read generated key"));
        signature::public_key_hex(&original).expect("generated key is valid PKCS#8");
        assert!(command(&["keygen", key_arg]).is_err());
        let retained = Zeroizing::new(fs::read(&key_path).expect("read retained key"));
        assert!(
            original.as_slice() == retained.as_slice(),
            "keygen overwrote the key"
        );

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&key_path).unwrap().permissions().mode() & 0o777,
                0o600
            );
            assert_eq!(
                fs::metadata(key_path.parent().unwrap())
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
        }
    }

    #[test]
    fn sign_payload_records_digest_and_verifies_with_strict_host_policy() {
        let directory = tempfile::tempdir().expect("isolated signing fixture");
        let key_path = directory.path().join("publisher.pk8");
        let input = directory.path().join("manifest.toml");
        let output = directory.path().join("signed").join("manifest.toml");
        let payload = directory.path().join("component.wasm");
        let payload_bytes = b"controlled payload bytes";
        command(&["keygen", key_path.to_str().unwrap()]).expect("generate publisher key");
        fs::write(
            &input,
            "name = \"signing-fixture\"\nversion = \"0.1.0\"\nwasm_path = \"component.wasm\"\n",
        )
        .expect("write input manifest");
        fs::write(&payload, payload_bytes).expect("write payload fixture");

        command(&[
            "sign",
            input.to_str().unwrap(),
            output.to_str().unwrap(),
            "--key",
            key_path.to_str().unwrap(),
            "--payload",
            payload.to_str().unwrap(),
        ])
        .expect("sign command succeeds");

        let private_key = Zeroizing::new(fs::read(&key_path).expect("read publisher key"));
        let public_key =
            signature::public_key_hex(&private_key).expect("derive trusted public key");
        let signed = fs::read_to_string(&output).expect("read command output");
        let root: toml::Table = toml::from_str(&signed).expect("signed output is TOML");
        let expected_digest = signature::sha256_hex(payload_bytes);
        assert_eq!(
            root[PAYLOAD_DIGEST].as_str(),
            Some(expected_digest.as_str())
        );
        let verdict = signature::enforce_signature_policy(
            "signing-fixture",
            &signed,
            root.get("signature").and_then(toml::Value::as_str),
            root.get("publisher_key").and_then(toml::Value::as_str),
            &[public_key],
            SignatureMode::Strict,
        )
        .expect("host strict signature policy accepts output");
        assert!(verdict.is_valid());
    }

    #[test]
    fn sign_payload_without_wasm_path_refuses_before_creating_output() {
        let directory = tempfile::tempdir().expect("isolated refused signing fixture");
        let key_path = directory.path().join("publisher.pk8");
        let input = directory.path().join("manifest.toml");
        let output = directory.path().join("not-created").join("manifest.toml");
        let payload = directory.path().join("component.wasm");
        command(&["keygen", key_path.to_str().unwrap()]).expect("generate publisher key");
        fs::write(&input, "name = \"skill-fixture\"\nversion = \"0.1.0\"\n")
            .expect("write manifest");
        fs::write(&payload, b"controlled payload bytes").expect("write payload fixture");

        assert!(
            command(&[
                "sign",
                input.to_str().unwrap(),
                output.to_str().unwrap(),
                "--key",
                key_path.to_str().unwrap(),
                "--payload",
                payload.to_str().unwrap(),
            ])
            .is_err()
        );
        assert!(!output.exists());
        assert!(!output.parent().unwrap().exists());
    }
}
