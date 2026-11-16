use axum::http::StatusCode;
use serde::Deserialize;
use serde_json::{Value, json};
use std::time::Duration;
use zeroclaw_config::schema::ScopedHttpAuth;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SecretWrite {
    value: String,
    #[serde(default)]
    token: String,
}

impl SecretWrite {
    pub(crate) fn reference<'a>(&self, auth: &'a ScopedHttpAuth) -> &'a str {
        match auth {
            ScopedHttpAuth::Totp { seed } => seed,
            ScopedHttpAuth::SshKey { private_key, .. } => private_key,
            ScopedHttpAuth::UserPassword { password, .. } => password,
            ScopedHttpAuth::ApiKey { value, .. } => value,
        }
    }

    pub(crate) fn valid_for(&self, auth: &ScopedHttpAuth) -> bool {
        let value = self.value.as_str();
        if !(valid_token(&self.token) || self.token.is_empty() && managed_writer()) {
            return false;
        }
        match auth {
            ScopedHttpAuth::Totp { .. } => {
                (16..=256).contains(&value.len())
                    && matches!(value.len() % 8, 0 | 2 | 4 | 5 | 7)
                    && value
                        .bytes()
                        .all(|b| matches!(b, b'A'..=b'Z' | b'2'..=b'7'))
            }
            ScopedHttpAuth::SshKey { .. } => {
                value.len() <= 32768
                    && value.starts_with("-----BEGIN OPENSSH PRIVATE KEY-----")
                    && value.ends_with("-----END OPENSSH PRIVATE KEY-----")
                    && !value.contains('\0')
            }
            ScopedHttpAuth::ApiKey { .. } => {
                !value.trim().is_empty()
                    && value.len() <= 4096
                    && value.bytes().all(|b| b.is_ascii_graphic())
            }
            ScopedHttpAuth::UserPassword { .. } => {
                !value.trim().is_empty()
                    && value.len() <= 4096
                    && !value.chars().any(char::is_control)
            }
        }
    }

    pub(crate) async fn apply(self, reference: &str, replace: bool) -> Result<(), StatusCode> {
        use reqwest::{Client, Url, redirect::Policy};
        let bad = StatusCode::BAD_REQUEST;
        let upstream = StatusCode::BAD_GATEWAY;
        let disabled = StatusCode::PRECONDITION_FAILED;
        let prefix = configured_prefix().ok_or(disabled)?;
        let parts = reference.split('/').collect::<Vec<_>>();
        let [mount, path @ .., field] = parts.as_slice() else {
            return Err(bad);
        };
        if !in_scope(&prefix, reference) || path.is_empty() {
            return Err(bad);
        }
        let base =
            Url::parse(&std::env::var("BAO_ADDR").map_err(|_| disabled)?).map_err(|_| disabled)?;
        if base.scheme() != "https"
            || base.path() != "/"
            || base.query().is_some()
            || base.fragment().is_some()
            || !base.username().is_empty()
            || base.password().is_some()
        {
            return Err(disabled);
        }
        let mut client = Client::builder()
            .https_only(true)
            .redirect(Policy::none())
            .no_proxy()
            .timeout(Duration::from_secs(8));
        if let Some(path) = std::env::var_os("BAO_CACERT") {
            let pem = std::fs::read(path).map_err(|_| disabled)?;
            for cert in reqwest::Certificate::from_pem_bundle(&pem).map_err(|_| disabled)? {
                client = client.add_root_certificate(cert);
            }
        }
        let client = client.build().map_err(|_| upstream)?;
        let endpoint = |kind: &str| -> Result<Url, StatusCode> {
            let mut url = base.clone();
            url.path_segments_mut()
                .map_err(|_| disabled)?
                .clear()
                .extend(["v1", *mount, kind])
                .extend(path.iter().copied());
            Ok(url)
        };
        let managed;
        let token = if self.token.is_empty() {
            let path = writer_token_path()
                .filter(|path| std::path::Path::new(path).is_absolute())
                .ok_or(disabled)?;
            // OpenBao Agent may replace its sink when the lease changes.
            // Read it per mutation and cap the bytes admitted as a header.
            use tokio::io::AsyncReadExt as _;
            if !tokio::fs::metadata(&path)
                .await
                .map_err(|_| disabled)?
                .is_file()
            {
                return Err(disabled);
            }
            let file = tokio::fs::File::open(path).await.map_err(|_| disabled)?;
            if !file.metadata().await.map_err(|_| disabled)?.is_file() {
                return Err(disabled);
            }
            let mut raw = String::new();
            file.take(16386)
                .read_to_string(&mut raw)
                .await
                .map_err(|_| disabled)?;
            if raw.len() >= 16386 {
                return Err(disabled);
            }
            managed = raw;
            managed.trim()
        } else {
            self.token.as_str()
        };
        if !valid_token(token) {
            return Err(if self.token.is_empty() { disabled } else { bad });
        }
        let mut token = reqwest::header::HeaderValue::from_str(token).map_err(|_| bad)?;
        token.set_sensitive(true);
        let version = if replace {
            let mut response = client
                .get(endpoint("metadata")?)
                .header("X-Vault-Token", token.clone())
                .send()
                .await
                .map_err(|_| upstream)?
                .error_for_status()
                .map_err(|_| upstream)?;
            let mut raw = Vec::new();
            while let Some(chunk) = response.chunk().await.map_err(|_| upstream)? {
                if raw.len() + chunk.len() > 65536 {
                    return Err(upstream);
                }
                raw.extend_from_slice(&chunk);
            }
            serde_json::from_slice::<Value>(&raw)
                .map_err(|_| upstream)?
                .pointer("/data/current_version")
                .and_then(Value::as_u64)
                .filter(|v| *v > 0)
                .ok_or(upstream)?
        } else {
            0
        };
        let mut request = client
            .request(
                if replace {
                    reqwest::Method::PATCH
                } else {
                    reqwest::Method::POST
                },
                endpoint("data")?,
            )
            .header("X-Vault-Token", token);
        if replace {
            request = request.header("Content-Type", "application/merge-patch+json");
        }
        let status = request
            .json(&json!({ "options": {"cas": version}, "data": { (*field): self.value } }))
            .send()
            .await
            .map_err(|_| upstream)?
            .status();
        if status.is_success() {
            Ok(())
        } else if status.as_u16() == 400 {
            Err(StatusCode::CONFLICT)
        } else {
            Err(upstream)
        }
    }
}

const WRITER_TOKEN_PATH: &str = "ZEROCLAW_OPENBAO_WRITE_TOKEN_FILE";

fn writer_token_path() -> Option<std::ffi::OsString> {
    std::env::var_os(WRITER_TOKEN_PATH).or_else(|| std::env::var_os("BAO_TOKEN_PATH"))
}

pub(crate) fn managed_writer() -> bool {
    writer_token_path()
        .is_some_and(|path| std::path::Path::new(&path).is_absolute())
}

fn valid_token(token: &str) -> bool {
    !token.is_empty() && token.len() <= 1024 && token.bytes().all(|b| b.is_ascii_graphic())
}

pub(crate) fn configured_prefix() -> Option<String> {
    let prefix = std::env::var("ZEROCLAW_OPENBAO_WRITE_PREFIX").ok()?;
    let address = std::env::var("BAO_ADDR").ok()?;
    (address.starts_with("https://") && in_scope(&prefix, &format!("{prefix}/example/value")))
        .then_some(prefix)
}

fn in_scope(prefix: &str, reference: &str) -> bool {
    let valid = |segment: &str| {
        !segment.is_empty()
            && segment != "."
            && segment != ".."
            && segment
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
    };
    prefix.split('/').count() >= 2
        && prefix.split('/').all(valid)
        && reference.split('/').all(valid)
        && reference
            .strip_prefix(prefix)
            .and_then(|s| s.strip_prefix('/'))
            .is_some_and(|rest| rest.split('/').count() >= 2)
}

#[cfg(test)]
mod tests {
    use super::{SecretWrite, in_scope, valid_token};

    #[test]
    fn managed_request_omits_the_token_without_relaxing_header_validation() {
        let write: SecretWrite = serde_json::from_str(r#"{"value":"synthetic-secret"}"#).unwrap();
        assert!(write.token.is_empty());
        assert!(valid_token("synthetic-token"));
        assert!(!valid_token(""));
        assert!(!valid_token("unsafe\r\nheader"));
    }

    #[test]
    fn provisioning_scope_is_segment_bound_and_excludes_the_prefix_record() {
        assert!(in_scope("secret/jarvis", "secret/jarvis/new/seed"));
        for invalid in [
            "secret/jarvis/seed",
            "secret/jarvis2/new/seed",
            "secret/jarvis/../seed",
            "secret/jarvis/a%2Fb/seed",
        ] {
            assert!(!in_scope("secret/jarvis", invalid), "{invalid}");
        }
    }
}
