//! Broker-side factor handling. The OpenSSH helper receives one response per
//! prompt through a short-lived local socket owned by the current task.
use super::*;
use crate::scoped_credentials::{load_scoped_credential_snapshot, resolve_scoped_value};
use zeroclaw_config::ssh_auth_profile::{
    ChallengeRole, PamExchange, SshAuthProfile, SshAuthentication,
};

pub(super) const ASKPASS_SOCKET: &str = "ZEROCLAW_TASK_SSH_ASKPASS_SOCKET";

pub(super) struct Factors {
    pub profile: SshAuthProfile,
    config: PathBuf,
    bindings: Vec<(String, String)>,
    password: Option<(ScopedCredentialSource, String)>,
    seed: Option<(ScopedCredentialSource, String)>,
}

pub(super) fn select(alias: &str, effective: &str) -> Result<Option<Factors>> {
    let Some(root) = std::env::var_os("ZEROCLAW_SSH_CREDENTIAL_DIR") else {
        return Ok(None);
    };
    let config = PathBuf::from(root).join("config.toml");
    let (mut anchor, snapshot) = load_scoped_credential_snapshot(Some(&config), alias)?;
    if anchor.ssh_profiles.is_empty() {
        return Ok(None);
    }
    ensure_scope(&anchor, effective)?;
    let host = ssh_setting(effective, "hostname").context("missing SSH hostname")?;
    let port = ssh_setting(effective, "port")
        .context("missing SSH port")?
        .parse()?;
    let username = ssh_setting(effective, "user").context("missing SSH username")?;
    let mut matches = std::mem::take(&mut anchor.ssh_profiles)
        .into_iter()
        .filter(|profile| profile.destination.matches(host, port, username));
    let profile = matches.next().context("missing matching SSH profile")?;
    ensure!(matches.next().is_none(), "ambiguous SSH profile");
    profile.validate()?;
    let (password_alias, totp_alias) = match &profile.authentication {
        SshAuthentication::PublicKey { .. } => return Ok(None),
        SshAuthentication::Password { password_alias } => (Some(password_alias), None),
        SshAuthentication::Pam {
            password_alias,
            totp_alias,
            ..
        } => (password_alias.as_ref(), totp_alias.as_ref()),
    };
    ensure!(
        password_alias
            .into_iter()
            .chain(totp_alias)
            .any(|factor| factor == alias),
        "SSH profile must belong to a selected factor"
    );
    let mut bindings = vec![(alias.to_owned(), snapshot)];
    let mut load = |name: &str| -> Result<ScopedHttpCredential> {
        let (record, revision) = load_scoped_credential_snapshot(Some(&config), name)?;
        ensure_scope(&record, effective)?;
        if name == alias {
            ensure!(revision == bindings[0].1, "SSH profile changed");
        } else {
            bindings.push((name.to_owned(), revision));
        }
        Ok(record)
    };
    let password = password_alias
        .map(|name| -> Result<_> {
            let record = load(name)?;
            let ScopedHttpAuth::UserPassword {
                username: account,
                password,
            } = record.auth
            else {
                anyhow::bail!("invalid SSH password alias");
            };
            ensure!(
                account == username,
                "SSH account does not match password alias"
            );
            Ok((record.source, password))
        })
        .transpose()?;
    let seed = totp_alias
        .map(|name| -> Result<_> {
            let record = load(name)?;
            let ScopedHttpAuth::Totp { seed } = record.auth else {
                anyhow::bail!("invalid SSH TOTP alias");
            };
            Ok((record.source, seed))
        })
        .transpose()?;
    Ok(Some(Factors {
        profile,
        config,
        bindings,
        password,
        seed,
    }))
}

impl Factors {
    pub(super) fn fingerprint(&self) -> Result<Vec<u8>> {
        Ok(Sha256::digest(serde_json::to_vec(&self.bindings)?).to_vec())
    }

    pub(super) fn revalidate(&self) -> Result<()> {
        for (alias, expected) in &self.bindings {
            let (_, current) = load_scoped_credential_snapshot(Some(&self.config), alias)?;
            ensure!(&current == expected, "SSH factor or profile changed");
        }
        Ok(())
    }

    async fn password(&self) -> Result<String> {
        let (source, reference) = self.password.as_ref().context("SSH password missing")?;
        resolve_scoped_value(source.clone(), reference.clone()).await
    }

    async fn totp(&self) -> Result<String> {
        let (source, reference) = self.seed.as_ref().context("SSH TOTP missing")?;
        let seed = resolve_scoped_value(source.clone(), reference.clone()).await?;
        let bytes = data_encoding::BASE32_NOPAD
            .decode(
                seed.trim()
                    .trim_end_matches('=')
                    .to_ascii_uppercase()
                    .as_bytes(),
            )
            .map_err(|_| anyhow::anyhow!("invalid SSH TOTP seed"))?;
        ensure!(!bytes.is_empty(), "empty SSH TOTP seed");
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_secs();
        let key = ring::hmac::Key::new(ring::hmac::HMAC_SHA1_FOR_LEGACY_USE_ONLY, &bytes);
        let digest = ring::hmac::sign(&key, &(now / 30).to_be_bytes());
        let hash = digest.as_ref();
        let offset = usize::from(hash[19] & 15);
        let code =
            (u32::from_be_bytes(hash[offset..offset + 4].try_into()?) & 0x7fff_ffff) % 1_000_000;
        Ok(format!("{code:06}"))
    }

    pub(super) async fn authenticate(
        &self,
        listener: UnixListener,
        master: &mut OwnedMaster,
        control: &Path,
    ) -> Result<()> {
        let mut pam = match &self.profile.authentication {
            SshAuthentication::Pam { .. } => Some(PamExchange::start(&self.profile)?),
            _ => None,
        };
        let mut password_sent = false;
        loop {
            if control.exists() {
                if let Some(exchange) = pam {
                    exchange.verify_complete()?;
                } else {
                    ensure!(password_sent, "missing password challenge");
                }
                self.revalidate()?;
                return Ok(());
            }
            ensure!(master.try_wait()?.is_none(), "SSH master exited");
            tokio::select! {
                incoming = listener.accept() => {
                    let (mut stream, _) = incoming?;
                    verify_helper(&stream, master.group)?;
                    let prompt: String = timeout(Duration::from_secs(2), read(&mut stream)).await??;
                    let role = if let Some(exchange) = &mut pam {
                        let target = &self.profile.destination;
                        let prefix = format!("({}@{}) ", target.username, target.host);
                        let label = prompt.strip_prefix(&prefix).unwrap_or(&prompt);
                        exchange.accept_prompt(label, false)?
                    } else {
                        let target = &self.profile.destination;
                        ensure!(!password_sent && prompt.trim() == format!("{}@{}'s password:", target.username, target.host),
                                "unexpected SSH password challenge");
                        password_sent = true;
                        ChallengeRole::Password
                    };
                    self.revalidate()?;
                    let reply = match role {
                        ChallengeRole::Username => self.profile.destination.username.clone(),
                        ChallengeRole::Password => self.password().await?,
                        ChallengeRole::Totp => self.totp().await?,
                        ChallengeRole::PasswordThenTotp => format!("{}{}", self.password().await?, self.totp().await?),
                    };
                    self.revalidate()?;
                    ensure!(!reply.is_empty() && reply.len() < 4096 && !reply.chars().any(char::is_control),
                            "invalid SSH response");
                    timeout(Duration::from_secs(2), write(&mut stream, &reply)).await??;
                }
                _ = sleep(Duration::from_millis(25)) => {}
            }
        }
    }
}

fn verify_helper(stream: &UnixStream, group: i32) -> Result<()> {
    let peer = stream.peer_cred()?;
    let pid = peer.pid().context("missing helper identity")?;
    let path = PathBuf::from(format!("/proc/{pid}"));
    let stat = std::fs::read_to_string(path.join("stat"))?;
    let fields = stat.rsplit_once(") ").context("invalid helper process")?.1;
    let mut fields = fields.split_whitespace();
    let parent: i32 = fields.nth(1).context("missing SSH parent")?.parse()?;
    let process_group: i32 = fields
        .next()
        .context("missing SSH process group")?
        .parse()?;
    ensure!(
        peer.uid() == unsafe { libc::geteuid() }
            && process_group == group
            && parent > 1
            && std::fs::read_link(path.join("exe"))? == std::env::current_exe()?
            && std::fs::read_link(format!("/proc/{parent}/exe"))? == Path::new(SSH),
        "unexpected SSH challenge helper"
    );
    Ok(())
}

pub(super) async fn askpass(path: PathBuf, args: &[String]) -> Result<i32> {
    let [prompt] = args else {
        anyhow::bail!("invalid SSH challenge");
    };
    let mut stream = UnixStream::connect(path).await?;
    write(&mut stream, prompt).await?;
    let result: String = read(&mut stream).await?;
    use std::io::Write as _;
    let mut out = std::io::stdout().lock();
    out.write_all(result.as_bytes())?;
    out.write_all(b"\n")?;
    out.flush()?;
    Ok(0)
}
