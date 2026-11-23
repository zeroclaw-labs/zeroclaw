//! Protocol adapter for the portable, destination-scoped SSH broker.
use std::{future::Future, sync::Arc, time::Duration};

use anyhow::{Context, Result, ensure};
use russh::{
    MethodKind,
    client::{self, AuthResult, KeyboardInteractiveAuthResponse},
    keys::{HashAlg, PrivateKeyWithHashAlg, PublicKey, PublicKeyOrCertificate, decode_secret_key},
};
use zeroclaw_config::{
    schema::SshCredentialDestination,
    ssh_auth_sequence::{SshAuthMode, SshAuthSequence, SshSecretReply},
};

const AUTH_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_INTERACTIVE_MESSAGES: usize = 16;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Factor {
    PrivateKey,
    Password,
    Totp,
}

pub struct PinnedSshHost(PublicKey);

impl client::Handler for PinnedSshHost {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        presented: &PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        Ok(
            matches!(presented, PublicKeyOrCertificate::PublicKey { key, .. } if self.0.key_data() == key.key_data()),
        )
    }
}

/// The caller supplies an authorized destination and a trusted pin. The resolver
/// must revalidate each grant before returning its value. No factor is resolved
/// until the host-key handshake succeeds.
pub async fn connect_authenticated<F, Fut>(
    destination: &SshCredentialDestination,
    host_key: PublicKey,
    mode: SshAuthMode,
    mut resolve: F,
) -> Result<client::Handle<PinnedSshHost>>
where
    F: FnMut(Factor) -> Fut,
    Fut: Future<Output = Result<String>>,
{
    ensure!(destination.is_valid(), "Invalid SSH destination");
    tokio::time::timeout(AUTH_TIMEOUT, async {
        let config = client::Config {
            inactivity_timeout: Some(Duration::from_secs(30 * 60)),
            ..client::Config::default()
        };
        let mut session = client::connect(
            Arc::new(config),
            (destination.host.as_str(), destination.port),
            PinnedSshHost(host_key),
        )
        .await
        .map_err(|_| anyhow::Error::msg("Could not verify the pinned SSH transport"))?;
        let username = &destination.username;
        let mut sequence = SshAuthSequence::new(mode);
        if mode.uses_key() {
            let key =
                decode_secret_key(&resolve(Factor::PrivateKey).await?, None).map_err(|_| {
                    anyhow::Error::msg("Could not decode the authorized SSH private key")
                })?;
            let hash = if key.algorithm().is_rsa() {
                session
                    .best_supported_rsa_hash()
                    .await?
                    .unwrap_or(Some(HashAlg::Sha512))
                    .context("The SSH server does not offer an RSA SHA-2 signature")?
                    .into()
            } else {
                None
            };
            match session
                .authenticate_publickey(username, PrivateKeyWithHashAlg::new(Arc::new(key), hash))
                .await?
            {
                AuthResult::Success => {
                    sequence.public_key_accepted(false)?;
                    sequence.verify_complete()?;
                    return Ok(session);
                }
                AuthResult::Failure {
                    partial_success,
                    remaining_methods,
                } => {
                    ensure!(
                        partial_success
                            && remaining_methods.contains(&MethodKind::KeyboardInteractive),
                        "SSH public key was not accepted for interactive verification"
                    );
                    sequence.public_key_accepted(true)?;
                }
            }
        } else if mode == SshAuthMode::Password {
            // Discover the offered method before using the password. An ordinary
            // authentication failure is terminal, even if the server offers PAM.
            let AuthResult::Failure {
                remaining_methods,
                partial_success: false,
            } = session.authenticate_none(username).await?
            else {
                anyhow::bail!("Unexpected SSH authentication method discovery result");
            };
            if remaining_methods.contains(&MethodKind::Password) {
                sequence.password_request()?;
                ensure!(
                    session
                        .authenticate_password(username, resolve(Factor::Password).await?)
                        .await?
                        .success(),
                    "SSH password authentication failed"
                );
                sequence.verify_complete()?;
                return Ok(session);
            }
            ensure!(
                remaining_methods.contains(&MethodKind::KeyboardInteractive),
                "The SSH server does not offer a password authentication method"
            );
        }

        let mut response = session
            .authenticate_keyboard_interactive_start(username, None)
            .await?;
        for _ in 0..MAX_INTERACTIVE_MESSAGES {
            match response {
                KeyboardInteractiveAuthResponse::Success => {
                    sequence.verify_complete()?;
                    return Ok(session);
                }
                KeyboardInteractiveAuthResponse::Failure { .. } => {
                    anyhow::bail!("SSH interactive authentication failed")
                }
                KeyboardInteractiveAuthResponse::InfoRequest { prompts, .. } => {
                    let flags: Vec<_> = prompts.iter().map(|prompt| prompt.echo).collect();
                    let replies = sequence.interactive_prompts(&flags)?;
                    let mut values = Vec::with_capacity(replies.len());
                    for reply in replies {
                        values.push(match reply {
                            SshSecretReply::Password => resolve(Factor::Password).await?,
                            SshSecretReply::Totp => resolve(Factor::Totp).await?,
                            SshSecretReply::PasswordAndTotp => {
                                let mut value = resolve(Factor::Password).await?;
                                value.push_str(&resolve(Factor::Totp).await?);
                                value
                            }
                        });
                    }
                    response = session
                        .authenticate_keyboard_interactive_respond(values)
                        .await?;
                }
            }
        }
        anyhow::bail!("Too many SSH interactive authentication messages")
    })
    .await
    .context("SSH authentication deadline exceeded")?
}
