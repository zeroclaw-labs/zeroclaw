use std::{path::Path, time::Duration};

use zeroclaw_config::{
    schema::{Config, ScopedHttpCredential},
    secrets::SecretStore,
};

pub(crate) fn scoped_credential_message(key: &str) -> String {
    crate::i18n::get_required_tool_string(key)
}

pub(crate) fn load_scoped_credential_snapshot(
    path: Option<&Path>,
    name: &str,
) -> anyhow::Result<(ScopedHttpCredential, String)> {
    anyhow::ensure!(
        ScopedHttpCredential::valid_name(name),
        "{}",
        scoped_credential_message("tool-http-credential-alias")
    );
    let path = path.ok_or_else(|| {
        anyhow::Error::msg(scoped_credential_message("tool-http-credential-store"))
    })?;
    let contents = std::fs::read_to_string(path)
        .map_err(|_| anyhow::Error::msg(scoped_credential_message("tool-http-credential-store")))?;
    let config: Config = toml::from_str(&contents)
        .map_err(|_| anyhow::Error::msg(scoped_credential_message("tool-http-credential-store")))?;
    let stored = config
        .http_request
        .scoped_credentials
        .get(name)
        .filter(|value| config.secrets.encrypt && value.starts_with("enc2:"))
        .ok_or_else(|| {
            anyhow::Error::msg(scoped_credential_message("tool-http-credential-missing"))
        })?;
    let dir = path.parent().ok_or_else(|| {
        anyhow::Error::msg(scoped_credential_message("tool-http-credential-store"))
    })?;
    let plaintext = SecretStore::new(dir, true)
        .decrypt(stored)
        .map_err(|_| anyhow::Error::msg(scoped_credential_message("tool-http-credential-store")))?;
    let credential = serde_json::from_str(&plaintext).map_err(|_| {
        anyhow::Error::msg(scoped_credential_message("tool-http-credential-invalid"))
    })?;
    Ok((credential, stored.clone()))
}

pub(crate) async fn resolve_scoped_value(
    source: zeroclaw_config::schema::ScopedCredentialSource,
    value: String,
) -> anyhow::Result<String> {
    use std::process::Stdio;
    use tokio::io::AsyncReadExt;
    use zeroclaw_config::schema::ScopedCredentialSource;

    if matches!(source, ScopedCredentialSource::Local) {
        return Ok(value);
    }
    let (program, args) = source.command(&value).ok_or_else(|| {
        anyhow::Error::msg(scoped_credential_message("tool-http-credential-reference"))
    })?;
    let failed = |_| anyhow::Error::msg(scoped_credential_message("tool-http-credential-lookup"));
    let mut child = tokio::process::Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .stdout(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(failed)?;
    let stdout = child.stdout.take().ok_or_else(|| {
        anyhow::Error::msg(scoped_credential_message("tool-http-credential-lookup"))
    })?;
    let mut output = Vec::new();
    let status = tokio::time::timeout(Duration::from_secs(15), async {
        stdout.take(65_537).read_to_end(&mut output).await?;
        if output.len() > 65_536 {
            return Err(std::io::Error::other(
                "External credential output exceeds limit",
            ));
        }
        child.wait().await
    })
    .await
    .map_err(|_| anyhow::Error::msg(scoped_credential_message("tool-http-credential-timeout")))?
    .map_err(failed)?;
    anyhow::ensure!(
        status.success(),
        "{}",
        scoped_credential_message("tool-http-credential-lookup")
    );
    decode_scoped_value(source, output)
}

pub(crate) fn decode_scoped_value(
    source: zeroclaw_config::schema::ScopedCredentialSource,
    output: Vec<u8>,
) -> anyhow::Result<String> {
    use zeroclaw_config::schema::ScopedCredentialSource;

    let value = if matches!(source, ScopedCredentialSource::Aws) {
        serde_json::from_slice::<String>(&output).ok()
    } else {
        String::from_utf8(output).ok().map(|value| {
            if matches!(
                source,
                ScopedCredentialSource::Google | ScopedCredentialSource::OpenBao
            ) {
                value
            } else {
                value
                    .strip_suffix("\r\n")
                    .or_else(|| value.strip_suffix('\n'))
                    .unwrap_or(&value)
                    .to_owned()
            }
        })
    };
    value
        .filter(|value| {
            !value.trim().is_empty() && value.len() <= 4096 && !value.chars().any(char::is_control)
        })
        .ok_or_else(|| anyhow::Error::msg(scoped_credential_message("tool-http-credential-value")))
}

pub(crate) fn scoped_totp(seed: &str, timestamp: u64) -> anyhow::Result<String> {
    let normalized = seed.trim().trim_end_matches('=').to_ascii_uppercase();
    let secret = data_encoding::BASE32_NOPAD
        .decode(normalized.as_bytes())
        .map_err(|_| anyhow::Error::msg("Invalid TOTP seed encoding"))?;
    anyhow::ensure!(!secret.is_empty(), "Empty TOTP seed");
    let key = ring::hmac::Key::new(ring::hmac::HMAC_SHA1_FOR_LEGACY_USE_ONLY, &secret);
    let counter = (timestamp / 30).to_be_bytes();
    let digest = ring::hmac::sign(&key, &counter);
    let hash = digest.as_ref();
    let offset = usize::from(hash[19] & 15);
    let value = u32::from_be_bytes(hash[offset..offset + 4].try_into()?) & 0x7fff_ffff;
    Ok(format!("{:06}", value % 1_000_000))
}
