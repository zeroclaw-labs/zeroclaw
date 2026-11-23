//! Protocol-level factor policy shared by SSH authorization and transport.

use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SshAuthMode {
    Key,
    KeyThenTotp,
    Password,
    PasswordThenTotp,
    TotpThenPassword,
    PasswordAndTotp,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SshSecretReply {
    Password,
    Totp,
    PasswordAndTotp,
}

use SshSecretReply::{Password, PasswordAndTotp, Totp};

impl SshAuthMode {
    pub fn uses_key(self) -> bool {
        matches!(self, Self::Key | Self::KeyThenTotp)
    }

    pub fn requires_totp(self) -> bool {
        !matches!(self, Self::Key | Self::Password)
    }

    fn replies(self) -> &'static [SshSecretReply] {
        match self {
            Self::Key => &[],
            Self::KeyThenTotp => &[Totp],
            Self::Password => &[Password],
            Self::PasswordThenTotp => &[Password, Totp],
            Self::TotpThenPassword => &[Totp, Password],
            Self::PasswordAndTotp => &[PasswordAndTotp],
        }
    }
}

/// Tracks only the expected factors. The SSH transport supplies protocol events
/// after host-key verification and resolves values only for accepted replies.
pub struct SshAuthSequence {
    mode: SshAuthMode,
    key_accepted: bool,
    sent: usize,
    rejected: bool,
}

impl SshAuthSequence {
    pub fn new(mode: SshAuthMode) -> Self {
        Self {
            mode,
            key_accepted: false,
            sent: 0,
            rejected: false,
        }
    }

    /// Call only after the SSH server accepts the selected public key.
    pub fn public_key_accepted(&mut self, partial_success: bool) -> Result<()> {
        ensure!(!self.rejected, "SSH authentication was rejected");
        self.rejected = true;
        ensure!(
            self.mode.uses_key()
                && !self.key_accepted
                && self.sent == 0
                && partial_success == self.mode.requires_totp(),
            "Unexpected SSH public-key authentication result"
        );
        self.key_accepted = true;
        self.rejected = false;
        Ok(())
    }

    /// Records the single conventional SSH password attempt. Interactive
    /// replies and password authentication cannot be mixed or retried.
    pub fn password_request(&mut self) -> Result<SshSecretReply> {
        ensure!(!self.rejected, "SSH authentication was rejected");
        self.rejected = true;
        ensure!(
            self.mode == SshAuthMode::Password && self.sent == 0,
            "Unexpected SSH password authentication"
        );
        self.sent = 1;
        self.rejected = false;
        Ok(Password)
    }

    /// The flags are the SSH keyboard-interactive echo bits, in wire order.
    /// Check the entire batch before authorizing the transport to release any
    /// secret. A zero-field message is informational and consumes no factor.
    pub fn interactive_prompts(&mut self, echo: &[bool]) -> Result<&'static [SshSecretReply]> {
        ensure!(!self.rejected, "SSH authentication was rejected");
        self.rejected = true;
        ensure!(
            (!self.mode.uses_key() || self.key_accepted)
                && self.mode != SshAuthMode::Key
                && echo.iter().all(|visible| !visible),
            "Unexpected or visible SSH interactive prompt"
        );
        let remaining = &self.mode.replies()[self.sent..];
        ensure!(
            echo.len() <= remaining.len(),
            "Extra SSH authentication prompt"
        );
        self.sent += echo.len();
        self.rejected = false;
        Ok(&remaining[..echo.len()])
    }

    /// Call on SSH user-auth success before opening any command channels.
    pub fn verify_complete(self) -> Result<()> {
        ensure!(
            !self.rejected
                && (!self.mode.uses_key() || self.key_accepted)
                && self.sent == self.mode.replies().len(),
            "Incomplete SSH authentication sequence"
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completes_each_configured_mode() -> Result<()> {
        let mut key = SshAuthSequence::new(SshAuthMode::Key);
        key.public_key_accepted(false)?;
        key.verify_complete()?;
        let mut key_totp = SshAuthSequence::new(SshAuthMode::KeyThenTotp);
        key_totp.public_key_accepted(true)?;
        assert_eq!(key_totp.interactive_prompts(&[false])?, &[Totp]);
        key_totp.verify_complete()?;
        let mut password = SshAuthSequence::new(SshAuthMode::Password);
        assert_eq!(password.password_request()?, Password);
        password.verify_complete()?;
        for (mode, expected) in [
            (SshAuthMode::Password, &[Password][..]),
            (SshAuthMode::PasswordThenTotp, &[Password, Totp][..]),
            (SshAuthMode::TotpThenPassword, &[Totp, Password][..]),
            (SshAuthMode::PasswordAndTotp, &[PasswordAndTotp][..]),
        ] {
            let mut batched = SshAuthSequence::new(mode);
            assert_eq!(batched.interactive_prompts(&[])?, &[]);
            assert_eq!(
                batched.interactive_prompts(&[false; 2][..expected.len()])?,
                expected
            );
            batched.verify_complete()?;
            let mut separate = SshAuthSequence::new(mode);
            for reply in expected {
                assert_eq!(separate.interactive_prompts(&[false])?, &[*reply]);
            }
            separate.verify_complete()?;
        }
        Ok(())
    }

    #[test]
    fn rejects_downgrade_and_extra_or_visible_fields() -> Result<()> {
        let mut early = SshAuthSequence::new(SshAuthMode::KeyThenTotp);
        assert!(early.interactive_prompts(&[false]).is_err());
        assert!(early.public_key_accepted(true).is_err());
        let mut bypass = SshAuthSequence::new(SshAuthMode::KeyThenTotp);
        assert!(bypass.public_key_accepted(false).is_err());
        let mut missing = SshAuthSequence::new(SshAuthMode::PasswordThenTotp);
        missing.interactive_prompts(&[false])?;
        assert!(missing.verify_complete().is_err());
        for flags in [&[false, true][..], &[false, false, false][..]] {
            let mut invalid = SshAuthSequence::new(SshAuthMode::PasswordThenTotp);
            assert!(invalid.interactive_prompts(flags).is_err());
            assert!(invalid.interactive_prompts(&[false, false]).is_err());
        }
        let mut mixed = SshAuthSequence::new(SshAuthMode::Password);
        mixed.password_request()?;
        assert!(mixed.interactive_prompts(&[false]).is_err());
        Ok(())
    }
}
