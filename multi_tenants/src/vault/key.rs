use rand::RngExt;
use std::collections::HashMap;
use std::path::Path;

/// Key file format: one line per key version.
/// Format: "{version}:{hex_encoded_32_byte_key}"
/// Lines starting with # are comments. Blank lines ignored.
pub fn load_keys(path: &Path) -> anyhow::Result<(u32, HashMap<u32, [u8; 32]>)> {
    if !path.exists() {
        anyhow::bail!(
            "master key file not found: {}. Run `zcplatform bootstrap` first.",
            path.display()
        );
    }

    let content = std::fs::read_to_string(path)?;
    let mut keys = HashMap::new();
    let mut max_version = 0u32;

    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let parts: Vec<&str> = line.splitn(2, ':').collect();
        if parts.len() != 2 {
            anyhow::bail!("invalid key line format: expected 'version:hex_key'");
        }
        let version: u32 = parts[0].parse()?;
        let key_bytes = hex::decode(parts[1])?;
        if key_bytes.len() != 32 {
            anyhow::bail!(
                "key version {} must be exactly 32 bytes, got {}",
                version,
                key_bytes.len()
            );
        }
        let mut key = [0u8; 32];
        key.copy_from_slice(&key_bytes);
        keys.insert(version, key);
        if version > max_version {
            max_version = version;
        }
    }

    if keys.is_empty() {
        anyhow::bail!("no keys found in key file");
    }

    Ok((max_version, keys))
}

/// Generate a new 32-byte random key and append to key file.
/// Returns the new key version number.
pub fn generate_key(path: &Path) -> anyhow::Result<u32> {
    let mut key = [0u8; 32];
    rand::rng().fill(&mut key);

    let next_version = if path.exists() {
        let (current, _) = load_keys(path)?;
        current + 1
    } else {
        1
    };

    let line = format!("{}:{}\n", next_version, hex::encode(key));

    // Ensure parent dir exists
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    file.write_all(line.as_bytes())?;

    // Restrictive permissions on Unix
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }

    Ok(next_version)
}
