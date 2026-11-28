#![cfg(feature = "portable-ssh")]

use std::{borrow::Cow, future::ready, sync::Arc, time::Duration};

use anyhow::Result;
use russh::{
    MethodKind, MethodSet,
    keys::{Algorithm, PrivateKey, PublicKey, ssh_key::LineEnding},
    server::{self, Auth, Response, VerifiedPublicKey},
};
use tokio::net::TcpListener;
use zeroclaw_config::{schema::SshCredentialDestination, ssh_auth_sequence::SshAuthMode};
use zeroclaw_tools::portable_ssh_auth::{Factor, connect_authenticated};

const USER: &str = "fixture";
const PASSWORD: &str = "synthetic-password";
const OTP: &str = "123456";
const COMBINED: &str = "synthetic-password123456";

struct Server {
    key: PublicKey,
    expected: &'static [&'static str],
    conventional_password: bool,
    echo: bool,
    key_only: bool,
    key_accepted: bool,
}

impl server::Handler for Server {
    type Error = russh::Error;

    async fn auth_password(&mut self, user: &str, candidate: &str) -> Result<Auth, Self::Error> {
        Ok(
            if user == USER && self.conventional_password && candidate == PASSWORD {
                Auth::Accept
            } else {
                Auth::reject()
            },
        )
    }

    async fn auth_publickey(
        &mut self,
        user: &str,
        key: VerifiedPublicKey<'_>,
    ) -> Result<Auth, Self::Error> {
        if user != USER || key.public_key.key_data() != self.key.key_data() {
            return Ok(Auth::reject());
        }
        self.key_accepted = true;
        Ok(if self.key_only {
            Auth::Accept
        } else {
            Auth::Reject {
                proceed_with_methods: Some(MethodSet::from(&[MethodKind::KeyboardInteractive][..])),
                partial_success: true,
            }
        })
    }

    async fn auth_keyboard_interactive<'a>(
        &'a mut self,
        user: &str,
        _: &str,
        response: Option<Response<'a>>,
    ) -> Result<Auth, Self::Error> {
        if user != USER
            || self.expected.is_empty()
            || (self.expected == [OTP] && !self.key_accepted)
        {
            return Ok(Auth::reject());
        }
        Ok(match response {
            None => Auth::Partial {
                name: Cow::Borrowed("Prueba"),
                instructions: Cow::Borrowed(""),
                prompts: Cow::Owned(
                    self.expected
                        .iter()
                        .map(|_| (Cow::Borrowed("別の入力"), self.echo))
                        .collect(),
                ),
            },
            Some(values) => {
                if values
                    .map(|value| value.to_vec())
                    .eq(self.expected.iter().map(|value| value.as_bytes().to_vec()))
                {
                    Auth::Accept
                } else {
                    Auth::reject()
                }
            }
        })
    }
}

#[tokio::test]
async fn supports_six_modes_without_interpreting_prompt_text() -> Result<()> {
    for (mode, expected, conventional) in [
        (SshAuthMode::Key, &[][..], false),
        (SshAuthMode::KeyThenTotp, &[OTP][..], false),
        (SshAuthMode::Password, &[][..], true),
        (SshAuthMode::Password, &[PASSWORD][..], false),
        (SshAuthMode::PasswordThenTotp, &[PASSWORD, OTP][..], false),
        (SshAuthMode::TotpThenPassword, &[OTP, PASSWORD][..], false),
        (SshAuthMode::PasswordAndTotp, &[COMBINED][..], false),
    ] {
        let mut calls = Vec::new();
        assert!(
            probe(
                mode,
                expected,
                conventional,
                false,
                mode == SshAuthMode::Key,
                false,
                &mut calls
            )
            .await?
        );
        assert_eq!(calls.contains(&Factor::Totp), mode.requires_totp());
    }
    Ok(())
}

#[tokio::test]
async fn withholds_secrets_on_bad_pin_visible_fields_and_key_only_success() -> Result<()> {
    let mut calls = Vec::new();
    assert!(
        !probe(
            SshAuthMode::Password,
            &[PASSWORD],
            false,
            false,
            false,
            true,
            &mut calls
        )
        .await?
    );
    assert!(calls.is_empty());
    assert!(
        !probe(
            SshAuthMode::PasswordThenTotp,
            &[PASSWORD, OTP],
            false,
            true,
            false,
            false,
            &mut calls
        )
        .await?
    );
    assert!(calls.is_empty());
    assert!(
        !probe(
            SshAuthMode::KeyThenTotp,
            &[OTP],
            false,
            false,
            true,
            false,
            &mut calls
        )
        .await?
    );
    assert_eq!(calls, [Factor::PrivateKey]);
    Ok(())
}

async fn probe(
    mode: SshAuthMode,
    expected: &'static [&'static str],
    conventional_password: bool,
    echo: bool,
    key_only: bool,
    wrong_pin: bool,
    calls: &mut Vec<Factor>,
) -> Result<bool> {
    let user_key = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519)?;
    let host_key = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519)?;
    let supplied_pin = if wrong_pin {
        user_key.public_key()
    } else {
        host_key.public_key()
    }
    .clone();
    let config = Arc::new(server::Config {
        keys: vec![host_key],
        methods: MethodSet::from(
            &[if mode.uses_key() {
                MethodKind::PublicKey
            } else if conventional_password {
                MethodKind::Password
            } else {
                MethodKind::KeyboardInteractive
            }][..],
        ),
        auth_rejection_time: Duration::ZERO,
        auth_rejection_time_initial: Some(Duration::ZERO),
        ..server::Config::default()
    });
    let behavior = Server {
        key: user_key.public_key().clone(),
        expected,
        conventional_password,
        echo,
        key_only,
        key_accepted: false,
    };
    let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
    let addr = listener.local_addr()?;
    let server = zeroclaw_spawn::spawn!(async move {
        let (stream, _) = listener.accept().await?;
        server::run_stream(config, stream, behavior).await?.await?;
        Ok::<(), anyhow::Error>(())
    });
    let private_key = user_key.to_openssh(LineEnding::LF)?;
    let destination = SshCredentialDestination {
        host: addr.ip().to_string(),
        port: addr.port(),
        username: USER.into(),
    };
    let result = connect_authenticated(&destination, supplied_pin, mode, |factor| {
        calls.push(factor);
        ready(Ok(match factor {
            Factor::PrivateKey => private_key.as_str().into(),
            Factor::Password => PASSWORD.into(),
            Factor::Totp => OTP.into(),
        }))
    })
    .await;
    if let Ok(session) = result {
        session
            .disconnect(russh::Disconnect::ByApplication, "", "")
            .await?;
        server.await??;
        Ok(true)
    } else {
        server.abort();
        Ok(false)
    }
}

#[test]
fn scoped_binding_rejects_another_account_and_revoked_totp() -> Result<()> {
    use serde_json::json;
    use zeroclaw_config::{schema::Config, secrets::SecretStore};
    use zeroclaw_tools::portable_ssh_auth::PortableSshBinding;

    let temp = tempfile::tempdir()?;
    let store = SecretStore::new(temp.path(), true);
    let catalog = temp.path().join("config.toml");
    let mut config = Config::default();
    config.secrets.encrypt = true;
    let destination = || SshCredentialDestination {
        host: "fixture.example.test".into(),
        port: 2222,
        username: USER.into(),
    };
    let record = |username: &str| {
        json!({
            "origin": "",
            "ssh_profiles": [{
                "destination": destination(),
                "authentication": {
                    "mode":"portable", "sequence":"password_then_totp",
                    "primary_alias":"LOGIN", "totp_alias":"OTP"
                }
            }],
            "auth": {"kind":"user_password","username":username,"password":PASSWORD}
        })
    };
    config
        .http_request
        .scoped_credentials
        .insert("LOGIN".into(), store.encrypt(&record(USER).to_string())?);
    config.http_request.scoped_credentials.insert(
        "OTP".into(),
        store.encrypt(
            &json!({"origin":"","auth":{"kind":"totp","seed":"JBSWY3DPEHPK3PXP"}}).to_string(),
        )?,
    );
    std::fs::write(&catalog, toml::to_string(&config)?)?;

    let mut other = destination();
    other.username = "other".into();
    assert!(PortableSshBinding::load(&catalog, "LOGIN", other).is_err());
    let bound = PortableSshBinding::load(&catalog, "LOGIN", destination())?;
    bound.revalidate()?;

    config.http_request.scoped_credentials.remove("OTP");
    std::fs::write(&catalog, toml::to_string(&config)?)?;
    assert!(bound.revalidate().is_err());
    assert!(PortableSshBinding::load(&catalog, "LOGIN", destination()).is_err());
    Ok(())
}
