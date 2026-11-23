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

/// An operator authorization tied to the exact destination and encrypted catalog
/// records. The task broker keeps this binding with a connection and calls
/// revalidate before granting each new command channel.
pub struct PortableSshBinding {
    catalog: std::path::PathBuf,
    destination: SshCredentialDestination,
    mode: SshAuthMode,
    primary: (String, String),
    totp: Option<(String, String)>,
}

impl PortableSshBinding {
    pub fn load(
        catalog: &std::path::Path,
        alias: &str,
        destination: SshCredentialDestination,
    ) -> Result<Self> {
        use crate::scoped_credentials::load_scoped_credential_snapshot;
        use zeroclaw_config::{schema::ScopedHttpAuth, ssh_auth_profile::SshAuthentication};

        let (record, revision) = load_scoped_credential_snapshot(Some(catalog), alias)?;
        let mut profiles = record.ssh_profiles.iter().filter(|profile| {
            profile
                .destination
                .matches(&destination.host, destination.port, &destination.username)
        });
        let profile = profiles
            .next()
            .context("SSH destination is not authorized")?;
        ensure!(profiles.next().is_none(), "Ambiguous SSH authorization");
        profile.validate()?;
        let SshAuthentication::Portable {
            sequence,
            primary_alias,
            totp_alias,
        } = &profile.authentication
        else {
            anyhow::bail!("SSH authorization does not select a portable mode");
        };
        ensure!(primary_alias == alias, "SSH primary alias does not match");
        match &record.auth {
            ScopedHttpAuth::SshKey { .. } if sequence.uses_key() => {}
            ScopedHttpAuth::UserPassword { username, .. }
                if !sequence.uses_key() && username == &destination.username => {}
            _ => anyhow::bail!("SSH credential kind or username does not match"),
        }
        let totp = totp_alias
            .as_ref()
            .map(|alias| -> Result<_> {
                let (credential, revision) = load_scoped_credential_snapshot(Some(catalog), alias)?;
                ensure!(
                    matches!(credential.auth, ScopedHttpAuth::Totp { .. }),
                    "Invalid SSH TOTP alias"
                );
                Ok((alias.clone(), revision))
            })
            .transpose()?;
        Ok(Self {
            catalog: catalog.to_path_buf(),
            destination,
            mode: *sequence,
            primary: (alias.to_owned(), revision),
            totp,
        })
    }

    fn read(
        &self,
        binding: &(String, String),
    ) -> Result<zeroclaw_config::schema::ScopedHttpCredential> {
        let (record, revision) = crate::scoped_credentials::load_scoped_credential_snapshot(
            Some(&self.catalog),
            &binding.0,
        )?;
        ensure!(revision == binding.1, "SSH grant or credential changed");
        Ok(record)
    }

    pub fn revalidate(&self) -> Result<()> {
        self.read(&self.primary)?;
        if let Some(totp) = &self.totp {
            self.read(totp)?;
        }
        Ok(())
    }

    async fn resolve(&self, factor: Factor) -> Result<String> {
        use crate::scoped_credentials::{resolve_scoped_value, scoped_totp};
        use zeroclaw_config::schema::ScopedHttpAuth;

        self.revalidate()?;
        let binding = if factor == Factor::Totp {
            self.totp.as_ref().context("SSH TOTP is not authorized")?
        } else {
            &self.primary
        };
        let record = self.read(binding)?;
        let reference = match (factor, record.auth) {
            (Factor::PrivateKey, ScopedHttpAuth::SshKey { private_key, .. }) => private_key,
            (Factor::Password, ScopedHttpAuth::UserPassword { password, .. }) => password,
            (Factor::Totp, ScopedHttpAuth::Totp { seed }) => seed,
            _ => anyhow::bail!("SSH factor does not match the authorization"),
        };
        let value = resolve_scoped_value(record.source, reference).await?;
        self.revalidate()?;
        if factor == Factor::Totp {
            let timestamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_secs();
            return scoped_totp(&value, timestamp);
        }
        Ok(value)
    }

    pub async fn connect(&self, host_key: PublicKey) -> Result<client::Handle<PinnedSshHost>> {
        self.revalidate()?;
        let session = connect_authenticated(&self.destination, host_key, self.mode, |factor| {
            self.resolve(factor)
        })
        .await?;
        self.revalidate()?;
        Ok(session)
    }
}

/// Return the single operator-pinned public key for this endpoint. Refuse
/// ambiguous pin configuration rather than choosing a key by server input.
pub fn host_pin(
    destination: &SshCredentialDestination,
    path: &std::path::Path,
) -> Result<PublicKey> {
    ensure!(path.is_absolute(), "SSH pin file path must be absolute");
    let keys =
        russh::keys::known_hosts::known_host_keys_path(&destination.host, destination.port, path)?;
    let mut keys = keys.into_iter().map(|(_, key)| key);
    let key = keys
        .next()
        .context("No pinned key for this SSH destination")?;
    ensure!(
        keys.all(|other| other.key_data() == key.key_data()),
        "Ambiguous SSH host key pins"
    );
    Ok(key)
}

const MAX_TASK_CONNECTIONS: usize = 32;
const OUTPUT_LIMIT: usize = 1024 * 1024;

struct TaskConnection {
    binding: PortableSshBinding,
    pin: PublicKey,
    session: client::Handle<PinnedSshHost>,
}

/// One pool per agent task. Dropping it drops all client handles; an explicit
/// close sends SSH disconnects before the broker returns.
#[derive(Default)]
pub struct PortableSshTask {
    connections: std::collections::HashMap<(String, String, u16, String), TaskConnection>,
}

pub struct PortableSshOutput {
    pub status: u32,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

impl PortableSshTask {
    /// The catalog and host pin must come from operator-owned configuration.
    pub async fn execute(
        &mut self,
        catalog: &std::path::Path,
        alias: &str,
        destination: SshCredentialDestination,
        pin: PublicKey,
        command: &str,
        command_timeout: Duration,
    ) -> Result<PortableSshOutput> {
        ensure!(
            !command.is_empty() && !command.contains('\0'),
            "Invalid SSH command"
        );
        let key = (
            alias.to_owned(),
            destination.host.to_ascii_lowercase(),
            destination.port,
            destination.username.clone(),
        );
        // Revocation closes the old connection and rejects this command. An
        // explicit subsequent request may establish a newly authorized one.
        if let Some(current) = self.connections.get(&key)
            && (current.binding.catalog != catalog
                || current.pin.key_data() != pin.key_data()
                || current.binding.revalidate().is_err())
        {
            if let Some(previous) = self.connections.remove(&key) {
                drop(previous);
            }
            anyhow::bail!("SSH authorization or host pin changed");
        }
        if self
            .connections
            .get(&key)
            .is_some_and(|entry| entry.session.is_closed())
        {
            self.connections.remove(&key);
        }
        if !self.connections.contains_key(&key) {
            ensure!(
                self.connections.len() < MAX_TASK_CONNECTIONS,
                "Too many open SSH connections"
            );
            let binding = PortableSshBinding::load(catalog, alias, destination)?;
            let session = binding.connect(pin.clone()).await?;
            self.connections.insert(
                key.clone(),
                TaskConnection {
                    binding,
                    pin,
                    session,
                },
            );
        }
        let current = self
            .connections
            .get(&key)
            .context("Missing SSH task connection")?;
        let result = tokio::time::timeout(command_timeout, execute_channel(current, command)).await;
        match result {
            Ok(Ok(output)) => Ok(output),
            other => {
                self.connections.remove(&key);
                other.context("SSH command deadline exceeded")?
            }
        }
    }

    pub async fn close(self) {
        let _ = tokio::time::timeout(Duration::from_secs(3), async {
            for entry in self.connections.into_values() {
                let _ = entry
                    .session
                    .disconnect(russh::Disconnect::ByApplication, "", "")
                    .await;
            }
        })
        .await;
    }
}

async fn execute_channel(current: &TaskConnection, command: &str) -> Result<PortableSshOutput> {
    use russh::ChannelMsg;
    current.binding.revalidate()?;
    let mut channel = current.session.channel_open_session().await?;
    current.binding.revalidate()?;
    channel.exec(true, command).await?;
    channel.eof().await?;
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let mut status = None;
    while let Some(message) = channel.wait().await {
        let (destination, bytes) = match message {
            ChannelMsg::Data { data } => (&mut stdout, data),
            ChannelMsg::ExtendedData { data, ext: 1 } => (&mut stderr, data),
            ChannelMsg::ExitStatus { exit_status } => {
                status = Some(exit_status);
                continue;
            }
            _ => continue,
        };
        ensure!(
            destination.len() + bytes.len() <= OUTPUT_LIMIT,
            "SSH command output exceeded its limit"
        );
        destination.extend_from_slice(&bytes);
    }
    Ok(PortableSshOutput {
        status: status.context("SSH command ended without an exit status")?,
        stdout,
        stderr,
    })
}
