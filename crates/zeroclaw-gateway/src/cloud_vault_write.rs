use crate::openbao_write::SecretWrite;
use axum::http::StatusCode;
use base64::{Engine, engine::general_purpose::STANDARD};
use reqwest::{Client, Method, Request, Url, header::HeaderValue};
use serde_json::{Value, json};
use std::time::Duration;
use zeroclaw_config::schema::ScopedCredentialSource as Source;

const BAD: StatusCode = StatusCode::BAD_REQUEST;
const DISABLED: StatusCode = StatusCode::PRECONDITION_FAILED;
const UPSTREAM: StatusCode = StatusCode::BAD_GATEWAY;
const CONFLICT: StatusCode = StatusCode::CONFLICT;

fn identifier(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
}

fn vault_id(s: &str) -> bool {
    s.len() == 26
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
}

fn hyphen_name(s: &str) -> bool {
    identifier(s) && !s.contains('_')
}

fn prefix_var(source: Source) -> Option<&'static str> {
    Some(match source {
        Source::OnePassword => "ZEROCLAW_1PASSWORD_WRITE_PREFIX",
        Source::Azure => "ZEROCLAW_AZURE_WRITE_PREFIX",
        Source::Aws => "ZEROCLAW_AWS_WRITE_PREFIX",
        Source::Google => "ZEROCLAW_GOOGLE_WRITE_PREFIX",
        _ => return None,
    })
}

fn configured(source: Source) -> Option<String> {
    let prefix = std::env::var(prefix_var(source)?).ok()?;
    let example = match source {
        Source::OnePassword => format!("{prefix}/example/value"),
        Source::Google => format!("{prefix}example/latest"),
        Source::Azure => format!("{prefix}example"),
        Source::Aws => format!("{prefix}/example"),
        _ => return None,
    };
    target(source, &prefix, &example).ok()?;
    if matches!(source, Source::OnePassword) {
        https_origin(&std::env::var("ZEROCLAW_1PASSWORD_CONNECT_URL").ok()?).ok()?;
    }
    Some(prefix)
}

pub(crate) fn prefixes() -> Value {
    json!({
        "one_password": configured(Source::OnePassword),
        "azure": configured(Source::Azure),
        "aws": configured(Source::Aws),
        "google": configured(Source::Google),
    })
}

#[derive(Debug, PartialEq)]
enum Target<'a> {
    OnePassword {
        vault: &'a str,
        item: &'a str,
        field: &'a str,
    },
    Azure {
        vault: &'a str,
        name: &'a str,
    },
    Aws {
        region: &'a str,
        name: &'a str,
    },
    Google {
        project: &'a str,
        name: &'a str,
    },
}

fn target<'a>(source: Source, scope: &str, reference: &'a str) -> Result<Target<'a>, StatusCode> {
    let parts: Vec<_> = reference.split('/').collect();
    match (source, parts.as_slice()) {
        (Source::OnePassword, ["op:", "", vault, item, field])
            if vault_id(vault)
                && scope.strip_prefix("op://") == Some(*vault)
                && identifier(item)
                && identifier(field) =>
        {
            Ok(Target::OnePassword { vault, item, field })
        }
        (Source::Azure, [vault, name])
            if hyphen_name(vault)
                && hyphen_name(name)
                && name.len() <= 127
                && named_scope(scope, vault, name, hyphen_name) =>
        {
            Ok(Target::Azure { vault, name })
        }
        (Source::Aws, [region, path @ ..])
            if hyphen_name(region)
                && !region.starts_with("cn-")
                && !region.starts_with("us-gov-")
                && path.len() >= 2
                && path.iter().all(|p| identifier(p))
                && reference
                    .strip_prefix(scope)
                    .is_some_and(|rest| rest.starts_with('/'))
                && scope.split('/').count() >= 2
                && scope.split('/').all(identifier) =>
        {
            Ok(Target::Aws {
                region,
                name: reference.split_once('/').ok_or(BAD)?.1,
            })
        }
        (Source::Google, [project, name, "latest"])
            if hyphen_name(project)
                && identifier(name)
                && named_scope(scope, project, name, identifier) =>
        {
            Ok(Target::Google { project, name })
        }
        _ => Err(BAD),
    }
}

fn named_scope(scope: &str, parent: &str, name: &str, valid: impl Fn(&str) -> bool) -> bool {
    scope.split_once('/').is_some_and(|(p, prefix)| {
        p == parent
            && prefix.ends_with('-')
            && valid(prefix)
            && name
                .strip_prefix(prefix)
                .is_some_and(|rest| !rest.is_empty())
    })
}

fn https_origin(raw: &str) -> Result<Url, StatusCode> {
    let url = Url::parse(raw).map_err(|_| DISABLED)?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || url.path() != "/"
        || url.query().is_some()
        || url.fragment().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(DISABLED);
    }
    Ok(url)
}

pub(crate) async fn apply(
    source: Source,
    write: SecretWrite,
    reference: &str,
    replace: bool,
) -> Result<Option<String>, StatusCode> {
    let scope = configured(source).ok_or(DISABLED)?;
    let dest = target(source, &scope, reference)?;
    let mut client = Client::builder()
        .https_only(true)
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(8));
    if matches!(source, Source::OnePassword)
        && let Some(path) = std::env::var_os("ZEROCLAW_1PASSWORD_CONNECT_CA")
    {
        let pem = std::fs::read(path).map_err(|_| DISABLED)?;
        for cert in reqwest::Certificate::from_pem_bundle(&pem).map_err(|_| DISABLED)? {
            client = client.add_root_certificate(cert);
        }
    }
    let client = client.build().map_err(|_| UPSTREAM)?;
    let origin = match &dest {
        Target::Azure { vault, .. } => format!("https://{vault}.vault.azure.net"),
        Target::Aws { region, .. } => format!("https://secretsmanager.{region}.amazonaws.com"),
        Target::Google { .. } => "https://secretmanager.googleapis.com".into(),
        Target::OnePassword { .. } => {
            std::env::var("ZEROCLAW_1PASSWORD_CONNECT_URL").map_err(|_| DISABLED)?
        }
    };
    execute(&client, &https_origin(&origin)?, dest, write, replace).await
}

async fn execute(
    client: &Client,
    base: &Url,
    dest: Target<'_>,
    write: SecretWrite,
    replace: bool,
) -> Result<Option<String>, StatusCode> {
    let join = |path: &str| base.join(path).map_err(|_| BAD);
    match dest {
        Target::OnePassword { vault, item, field } => {
            let items = format!("v1/vaults/{vault}/items");
            let bearer = |method, path: &str| {
                Ok::<_, StatusCode>(
                    client
                        .request(method, join(path)?)
                        .bearer_auth(&write.token),
                )
            };
            if replace {
                if !vault_id(item) {
                    return Err(BAD);
                }
                let path = format!("{items}/{item}");
                success(
                    send(bearer(Method::PATCH, &path)?.json(&json!([
                        {"op":"replace","path":format!("/fields/{field}/value"),"value":write.value}
                    ])))
                    .await?
                    .status(),
                )?;
                Ok(None)
            } else {
                let mut list = join(&items)?;
                list.query_pairs_mut()
                    .append_pair("filter", &format!("title eq \"{item}\""));
                let existing =
                    response_json(send(client.get(list).bearer_auth(&write.token)).await?).await?;
                if !existing.as_array().ok_or(UPSTREAM)?.is_empty() {
                    return Err(CONFLICT);
                }
                let added = response_json(
                    send(bearer(Method::POST, &items)?.json(&json!({
                        "title": item, "vault":{"id":vault}, "category":"SECURE_NOTE",
                        "fields":[{"label":field,"type":"CONCEALED","value":write.value}]
                    })))
                    .await?,
                )
                .await?;
                let id = added
                    .get("id")
                    .and_then(Value::as_str)
                    .filter(|id| vault_id(id))
                    .ok_or(UPSTREAM)?;
                let fields = added
                    .get("fields")
                    .and_then(Value::as_array)
                    .ok_or(UPSTREAM)?;
                let mut matching = fields
                    .iter()
                    .filter(|f| f.get("label").and_then(Value::as_str) == Some(field));
                let field_id = matching
                    .next()
                    .and_then(|f| f.get("id"))
                    .and_then(Value::as_str)
                    .filter(|id| identifier(id))
                    .ok_or(UPSTREAM)?;
                if matching.next().is_some() {
                    return Err(UPSTREAM);
                }
                Ok(Some(format!("op://{vault}/{id}/{field_id}")))
            }
        }
        Target::Azure { name, .. } => {
            let version = "api-version=7.4";
            let versions = join(&format!("secrets/{name}/versions?{version}&maxresults=1"))?;
            let current = send(client.get(versions).bearer_auth(&write.token)).await?;
            let status = current.status();
            let exists = if status == StatusCode::NOT_FOUND {
                false
            } else {
                !response_json(current)
                    .await?
                    .get("value")
                    .and_then(Value::as_array)
                    .ok_or(UPSTREAM)?
                    .is_empty()
            };
            if exists != replace {
                return Err(if exists {
                    CONFLICT
                } else {
                    StatusCode::NOT_FOUND
                });
            }
            success(
                send(
                    client
                        .put(join(&format!("secrets/{name}?{version}"))?)
                        .bearer_auth(&write.token)
                        .json(&json!({"value":write.value})),
                )
                .await?
                .status(),
            )?;
            Ok(None)
        }
        Target::Google { project, name } => {
            if !replace {
                let mut url = join(&format!("v1/projects/{project}/secrets"))?;
                url.query_pairs_mut().append_pair("secretId", name);
                success(
                    send(
                        client
                            .post(url)
                            .bearer_auth(&write.token)
                            .json(&json!({"replication":{"automatic":{}}})),
                    )
                    .await?
                    .status(),
                )?;
            }
            let req = client
                .post(join(&format!(
                    "v1/projects/{project}/secrets/{name}:addVersion"
                ))?)
                .bearer_auth(&write.token)
                .json(&json!({"payload":{"data":STANDARD.encode(write.value.as_bytes())}}));
            let result = send(req)
                .await
                .and_then(|response| success(response.status()));
            if !replace && result.is_err() {
                return Err(StatusCode::FAILED_DEPENDENCY);
            }
            result?;
            Ok(None)
        }
        Target::Aws { region, name } => {
            let response = client
                .execute(aws_request(
                    client,
                    base,
                    region,
                    name,
                    &write,
                    replace,
                    chrono::Utc::now(),
                )?)
                .await
                .map_err(|_| UPSTREAM)?;
            let status = response.status();
            if status == StatusCode::BAD_REQUEST {
                let code = response
                    .headers()
                    .get("x-amzn-errortype")
                    .and_then(|v| v.to_str().ok())
                    .map(|s| s.split(':').next().unwrap_or_default());
                if code == Some("ResourceExistsException") {
                    return Err(CONFLICT);
                }
                if code == Some("ResourceNotFoundException") {
                    return Err(StatusCode::NOT_FOUND);
                }
            }
            success(status)?;
            Ok(None)
        }
    }
}

fn aws_request(
    client: &Client,
    base: &Url,
    region: &str,
    name: &str,
    write: &SecretWrite,
    replace: bool,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<Request, StatusCode> {
    use hmac::{Hmac, Mac};
    use sha2::{Digest, Sha256};
    use zeroize::Zeroizing;
    let id = write.aws_access_key_id.as_deref().ok_or(BAD)?;
    let key = write.aws_secret_access_key.as_deref().ok_or(BAD)?;
    let action = if replace {
        "secretsmanager.PutSecretValue"
    } else {
        "secretsmanager.CreateSecret"
    };
    let key_field = if replace { "SecretId" } else { "Name" };
    let payload = json!({(key_field): name, "SecretString": write.value, "ClientRequestToken": uuid::Uuid::new_v4().to_string()}).to_string();
    let timestamp = now.format("%Y%m%dT%H%M%SZ").to_string();
    let day = &timestamp[..8];
    let content_type = "application/x-amz-json-1.1";
    let host = base.host_str().ok_or(BAD)?;
    let signed = "content-type;host;x-amz-date;x-amz-security-token;x-amz-target";
    let digest = |x: &[u8]| hex::encode(Sha256::digest(x));
    let canonical = Zeroizing::new(format!(
        "POST\n/\n\ncontent-type:{content_type}\nhost:{host}\nx-amz-date:{timestamp}\nx-amz-security-token:{}\nx-amz-target:{action}\n\n{signed}\n{}",
        write.token,
        digest(payload.as_bytes())
    ));
    let scope = format!("{day}/{region}/secretsmanager/aws4_request");
    let string = format!(
        "AWS4-HMAC-SHA256\n{timestamp}\n{scope}\n{}",
        digest(canonical.as_bytes())
    );
    let mac = |k: &[u8], data: &str| -> Result<Zeroizing<Vec<u8>>, StatusCode> {
        let mut h = Hmac::<Sha256>::new_from_slice(k).map_err(|_| BAD)?;
        h.update(data.as_bytes());
        Ok(Zeroizing::new(h.finalize().into_bytes().to_vec()))
    };
    let day_key = mac(Zeroizing::new(format!("AWS4{key}")).as_bytes(), day)?;
    let regional = mac(&day_key, region)?;
    let service = mac(&regional, "secretsmanager")?;
    let signing = mac(&service, "aws4_request")?;
    let signature = hex::encode(&*mac(&signing, &string)?);
    let mut auth = HeaderValue::from_str(&format!(
        "AWS4-HMAC-SHA256 Credential={id}/{scope}, SignedHeaders={signed}, Signature={signature}"
    ))
    .map_err(|_| BAD)?;
    let mut token = HeaderValue::from_str(&write.token).map_err(|_| BAD)?;
    auth.set_sensitive(true);
    token.set_sensitive(true);
    client
        .post(base.clone())
        .header("Content-Type", content_type)
        .header("X-Amz-Date", timestamp)
        .header("X-Amz-Security-Token", token)
        .header("X-Amz-Target", action)
        .header("Authorization", auth)
        .body(payload)
        .build()
        .map_err(|_| BAD)
}

async fn send(req: reqwest::RequestBuilder) -> Result<reqwest::Response, StatusCode> {
    req.send().await.map_err(|_| UPSTREAM)
}

fn success(status: StatusCode) -> Result<(), StatusCode> {
    match status {
        s if s.is_success() => Ok(()),
        StatusCode::CONFLICT
        | StatusCode::NOT_FOUND
        | StatusCode::FORBIDDEN
        | StatusCode::UNAUTHORIZED => Err(status),
        _ => Err(UPSTREAM),
    }
}

async fn response_json(mut r: reqwest::Response) -> Result<Value, StatusCode> {
    success(r.status())?;
    let mut bytes = Vec::new();
    while let Some(part) = r.chunk().await.map_err(|_| UPSTREAM)? {
        if bytes.len() + part.len() > 65536 {
            return Err(UPSTREAM);
        }
        bytes.extend_from_slice(&part);
    }
    serde_json::from_slice(&bytes).map_err(|_| UPSTREAM)
}
#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{body_json, header, method, path, query_param},
    };

    fn write() -> SecretWrite {
        SecretWrite {
            token: "synthetic-test-token".into(),
            value: "synthetic-value".into(),
            aws_access_key_id: None,
            aws_secret_access_key: None,
        }
    }

    #[test]
    fn restricts_provider_containers_and_refuses_pinned_google_versions() {
        assert!(
            target(
                Source::Azure,
                "example-vault/jarvis-",
                "example-vault/jarvis-totp"
            )
            .is_ok()
        );
        assert!(
            target(
                Source::Azure,
                "example-vault/jarvis-",
                "other-vault/jarvis-totp"
            )
            .is_err()
        );
        assert!(
            target(
                Source::Google,
                "project/jarvis-",
                "project/jarvis-totp/latest"
            )
            .is_ok()
        );
        assert!(target(Source::Google, "project/jarvis-", "project/jarvis-totp/1").is_err());
        assert!(target(Source::Aws, "us-east-1/jarvis", "us-east-1/jarvis2/totp").is_err());
        assert!(target(Source::Aws, "us-east-1/jarvis", "us-east-1/jarvis/../totp").is_err());
        assert!(
            target(
                Source::OnePassword,
                "op://aaaaaaaaaaaaaaaaaaaaaaaaaa",
                "op://bbbbbbbbbbbbbbbbbbbbbbbbbb/totp/seed"
            )
            .is_err()
        );
        assert!(https_origin("http://example.test").is_err());
        assert!(https_origin("https://example.test/path").is_err());
    }

    #[tokio::test]
    async fn one_password_creates_and_rotates_by_canonical_ids() {
        let server = MockServer::start().await;
        let base = Url::parse(&server.uri()).unwrap();
        let vault = "aaaaaaaaaaaaaaaaaaaaaaaaaa";
        let id = "bbbbbbbbbbbbbbbbbbbbbbbbbb";
        let items = format!("/v1/vaults/{vault}/items");
        Mock::given(method("GET"))
            .and(path(&items))
            .and(query_param("filter", "title eq \"fixture\""))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST")).and(path(&items)).and(header("authorization","Bearer synthetic-test-token"))
            .and(body_json(json!({"title":"fixture","vault":{"id":vault},"category":"SECURE_NOTE","fields":[{"label":"seed","type":"CONCEALED","value":"synthetic-value"}]})))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({"id":id,"fields":[{"id":"generated-id","label":"seed"}]}))).expect(1).mount(&server).await;
        let result = execute(
            &Client::new(),
            &base,
            Target::OnePassword {
                vault,
                item: "fixture",
                field: "seed",
            },
            write(),
            false,
        )
        .await
        .unwrap();
        assert_eq!(result, Some(format!("op://{vault}/{id}/generated-id")));
        Mock::given(method("PATCH")).and(path(format!("{items}/{id}")))
            .and(body_json(json!([{"op":"replace","path":"/fields/generated-id/value","value":"synthetic-value"}])))
            .respond_with(ResponseTemplate::new(200)).expect(1).mount(&server).await;
        assert_eq!(
            execute(
                &Client::new(),
                &base,
                Target::OnePassword {
                    vault,
                    item: id,
                    field: "generated-id"
                },
                write(),
                true
            )
            .await,
            Ok(None)
        );
    }

    #[tokio::test]
    async fn azure_create_checks_metadata_and_replace_uses_existing_secret() {
        let server = MockServer::start().await;
        let base = Url::parse(&server.uri()).unwrap();
        let dest = || Target::Azure {
            vault: "example",
            name: "jarvis-totp",
        };
        Mock::given(method("GET"))
            .and(path("/secrets/jarvis-totp/versions"))
            .and(query_param("api-version", "7.4"))
            .respond_with(ResponseTemplate::new(404))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("PUT"))
            .and(path("/secrets/jarvis-totp"))
            .and(body_json(json!({"value":"synthetic-value"})))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;
        assert_eq!(
            execute(&Client::new(), &base, dest(), write(), false).await,
            Ok(None)
        );
        server.verify().await;
        server.reset().await;
        Mock::given(method("GET"))
            .and(path("/secrets/jarvis-totp/versions"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({"value":[{"id":"metadata-only"}]})),
            )
            .expect(2)
            .mount(&server)
            .await;
        assert_eq!(
            execute(&Client::new(), &base, dest(), write(), false).await,
            Err(CONFLICT)
        );
        Mock::given(method("PUT"))
            .and(path("/secrets/jarvis-totp"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;
        assert_eq!(
            execute(&Client::new(), &base, dest(), write(), true).await,
            Ok(None)
        );
    }

    #[tokio::test]
    async fn google_reports_partial_create_and_can_add_version_to_existing_record() {
        let server = MockServer::start().await;
        let base = Url::parse(&server.uri()).unwrap();
        let dest = || Target::Google {
            project: "project",
            name: "jarvis-totp",
        };
        Mock::given(method("POST"))
            .and(path("/v1/projects/project/secrets"))
            .and(query_param("secretId", "jarvis-totp"))
            .and(body_json(json!({"replication":{"automatic":{}}})))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;
        let version = "/v1/projects/project/secrets/jarvis-totp:addVersion";
        let body = json!({"payload":{"data":STANDARD.encode("synthetic-value")}});
        Mock::given(method("POST"))
            .and(path(version))
            .and(body_json(body.clone()))
            .respond_with(ResponseTemplate::new(403))
            .expect(1)
            .mount(&server)
            .await;
        assert_eq!(
            execute(&Client::new(), &base, dest(), write(), false).await,
            Err(StatusCode::FAILED_DEPENDENCY)
        );
        server.verify().await;
        server.reset().await;
        Mock::given(method("POST"))
            .and(path(version))
            .and(body_json(body))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;
        assert_eq!(
            execute(&Client::new(), &base, dest(), write(), true).await,
            Ok(None)
        );
    }

    #[tokio::test]
    async fn aws_signs_create_and_replace_without_putting_secrets_in_url() {
        let server = MockServer::start().await;
        let base = Url::parse(&server.uri()).unwrap();
        let dest = || Target::Aws {
            region: "us-east-1",
            name: "jarvis/totp",
        };
        for (replacing, action, field) in [
            (false, "secretsmanager.CreateSecret", "Name"),
            (true, "secretsmanager.PutSecretValue", "SecretId"),
        ] {
            Mock::given(method("POST"))
                .and(path("/"))
                .and(header("x-amz-target", action))
                .and(header("x-amz-security-token", "synthetic-test-token"))
                .and(header("content-type", "application/x-amz-json-1.1"))
                .and(move |req: &wiremock::Request| {
                    let Ok(body) = serde_json::from_slice::<Value>(&req.body) else {
                        return false;
                    };
                    let auth = req
                        .headers
                        .get("authorization")
                        .and_then(|v| v.to_str().ok())
                        .unwrap_or_default();
                    body[field] == "jarvis/totp"
                        && body["SecretString"] == "synthetic-value"
                        && body["ClientRequestToken"]
                            .as_str()
                            .is_some_and(|value| uuid::Uuid::parse_str(value).is_ok())
                        && auth.starts_with("AWS4-HMAC-SHA256 Credential=ASIAEXAMPLE123456789/")
                        && auth.contains("/us-east-1/secretsmanager/aws4_request")
                        && auth.split("Signature=").nth(1).is_some_and(|sig| {
                            sig.len() == 64 && sig.bytes().all(|b| b.is_ascii_hexdigit())
                        })
                        && req.url.query().is_none()
                })
                .respond_with(ResponseTemplate::new(200))
                .expect(1)
                .mount(&server)
                .await;
            let mut w = write();
            w.aws_access_key_id = Some("ASIAEXAMPLE123456789".into());
            w.aws_secret_access_key = Some("a".repeat(40));
            assert_eq!(
                execute(&Client::new(), &base, dest(), w, replacing).await,
                Ok(None)
            );
        }
    }
}
