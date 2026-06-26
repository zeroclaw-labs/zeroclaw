//! Inkbox channel onboarding — the live SDK round-trips behind the Quickstart
//! "Channels" step. The CLI surface owns the prompts; this module owns the
//! network calls so they live in one place and the blocking SDK always runs
//! off the tokio runtime.

use std::sync::Arc;

use inkbox::whoami::types::{
    AUTH_SUBTYPE_API_KEY_AGENT_SCOPED_CLAIMED, AUTH_SUBTYPE_API_KEY_AGENT_SCOPED_UNCLAIMED,
    WhoamiResponse,
};
use inkbox::{Inkbox, InkboxError};

/// Result of a successful self-signup: an unverified API key bound to a freshly
/// created agent identity, plus its hosted mailbox.
#[derive(Debug, Clone)]
pub struct Signup {
    /// Agent-scoped API key for the new identity (unverified until claimed).
    pub api_key: String,
    /// Globally unique agent handle (also the mailbox local part).
    pub agent_handle: String,
    /// The identity's hosted email address.
    pub email_address: String,
}

/// Where a pasted API key sits in the Inkbox auth model.
#[derive(Debug, Clone)]
pub enum KeyScope {
    /// Admin-scoped key — can manage many identities under the org.
    Admin,
    /// Key already scoped to a single agent identity.
    Agent {
        /// Whether the agent has been claimed by a human.
        claimed: bool,
    },
    /// A non-API-key credential (e.g. a JWT) — unusable for a gateway.
    NotApiKey,
}

/// Minimal identity view the wizard needs to confirm a key works and to surface
/// the agent's mailbox / phone in the summary.
#[derive(Debug, Clone)]
pub struct Identity {
    /// The agent handle this key is bound to.
    pub handle: String,
    /// The identity's mailbox address, if any.
    pub email_address: Option<String>,
    /// The identity's phone number in E.164, if one is provisioned.
    pub phone_number: Option<String>,
}

const SIGNUP_NOTE: &str = "Setting up a ZeroClaw agent on Inkbox.";
/// Agent host reported to Inkbox at signup, so it can tailor post-verify guidance.
const HARNESS: &str = "zeroclaw";

/// Run a blocking SDK call off the tokio runtime.
///
/// `reqwest::blocking` builds (and drops) a temporary tokio runtime internally,
/// which panics on a tokio worker thread — so every call hops to a plain OS
/// thread first and only the owned result crosses back.
///
/// # Arguments
/// * `f` - the blocking SDK closure to run, yielding `Result<T, InkboxError>`.
///
/// # Returns
/// The closure's value, or an `anyhow::Error` carrying the SDK error / a panic.
fn off_runtime<T, F>(f: F) -> anyhow::Result<T>
where
    F: FnOnce() -> Result<T, InkboxError> + Send + 'static,
    T: Send + 'static,
{
    match std::thread::spawn(f).join() {
        Ok(Ok(v)) => Ok(v),
        Ok(Err(e)) => Err(anyhow::Error::msg(e.to_string())),
        Err(_) => Err(anyhow::Error::msg("Inkbox SDK call panicked")),
    }
}

/// Begin agent self-signup, creating an unverified identity.
///
/// # Arguments
/// * `base_url` - Inkbox API base URL.
/// * `human_email` - the human's email; receives the verification code.
/// * `handle` - desired (globally unique) agent handle.
///
/// # Returns
/// The new identity's [`Signup`] (api key, handle, mailbox).
pub fn signup(base_url: &str, human_email: &str, handle: &str) -> anyhow::Result<Signup> {
    let (email, h, base) = (
        human_email.to_string(),
        handle.to_string(),
        base_url.to_string(),
    );
    let resp = off_runtime(move || {
        Inkbox::signup(
            &email,
            SIGNUP_NOTE,
            None,
            Some(&h),
            None,
            Some(HARNESS),
            Some(&base),
            None,
        )
    })?;
    Ok(Signup {
        api_key: resp.api_key,
        agent_handle: resp.agent_handle,
        email_address: resp.email_address,
    })
}

/// Submit the 6-digit verification code to claim the agent.
///
/// # Arguments
/// * `base_url` - Inkbox API base URL.
/// * `api_key` - the unverified agent key from [`signup`].
/// * `code` - the 6-digit code from the verification email.
///
/// # Returns
/// `Ok(())` once the code is accepted; an error otherwise.
pub fn verify(base_url: &str, api_key: &str, code: &str) -> anyhow::Result<()> {
    let (key, c, base) = (api_key.to_string(), code.to_string(), base_url.to_string());
    off_runtime(move || Inkbox::verify_signup(&key, &c, Some(&base), None))?;
    Ok(())
}

/// Resend the verification email (the server enforces a cooldown).
///
/// # Arguments
/// * `base_url` - Inkbox API base URL.
/// * `api_key` - the unverified agent key from [`signup`].
///
/// # Returns
/// `Ok(())` if the resend was accepted.
pub fn resend(base_url: &str, api_key: &str) -> anyhow::Result<()> {
    let (key, base) = (api_key.to_string(), base_url.to_string());
    off_runtime(move || Inkbox::resend_signup_verification(&key, Some(&base), None))?;
    Ok(())
}

/// Classify a pasted API key so the wizard can branch (admin vs agent).
///
/// # Arguments
/// * `base_url` - Inkbox API base URL.
/// * `api_key` - the key to inspect.
///
/// # Returns
/// The key's [`KeyScope`].
pub fn key_scope(base_url: &str, api_key: &str) -> anyhow::Result<KeyScope> {
    let (key, base) = (api_key.to_string(), base_url.to_string());
    let who = off_runtime(move || {
        let client = Inkbox::builder(key).base_url(base).build()?;
        client.whoami()
    })?;
    Ok(match who {
        WhoamiResponse::ApiKey(k) => {
            let sub = k.auth_subtype.as_deref().unwrap_or_default();
            if sub == AUTH_SUBTYPE_API_KEY_AGENT_SCOPED_CLAIMED {
                KeyScope::Agent { claimed: true }
            } else if sub == AUTH_SUBTYPE_API_KEY_AGENT_SCOPED_UNCLAIMED {
                KeyScope::Agent { claimed: false }
            } else {
                // Admin-scoped, or any unrecognised api-key subtype → admin.
                KeyScope::Admin
            }
        }
        WhoamiResponse::Jwt(_) => KeyScope::NotApiKey,
    })
}

/// Validate a key against a specific handle and fetch the agent's mailbox/phone.
///
/// # Arguments
/// * `base_url` - Inkbox API base URL.
/// * `api_key` - the key to act as.
/// * `handle` - the agent handle to look up.
///
/// # Returns
/// The resolved [`Identity`]; errors if the key cannot read that handle.
pub fn fetch_identity(base_url: &str, api_key: &str, handle: &str) -> anyhow::Result<Identity> {
    let (key, base, h) = (
        api_key.to_string(),
        base_url.to_string(),
        handle.to_string(),
    );
    off_runtime(move || {
        let client = Arc::new(Inkbox::builder(key).base_url(base).build()?);
        let id = client.get_identity(&h)?;
        Ok(Identity {
            handle: id.agent_handle(),
            email_address: id.email_address(),
            phone_number: id.phone_number().map(|p| p.number),
        })
    })
}

/// Provision a local US phone number and link it to the agent.
///
/// # Arguments
/// * `base_url` - Inkbox API base URL.
/// * `api_key` - the key to act as.
/// * `handle` - the agent handle to assign the number to.
///
/// # Returns
/// The provisioned number in E.164.
pub fn provision_phone(base_url: &str, api_key: &str, handle: &str) -> anyhow::Result<String> {
    let (key, base, h) = (
        api_key.to_string(),
        base_url.to_string(),
        handle.to_string(),
    );
    let number = off_runtime(move || {
        let client = Inkbox::builder(key).base_url(base).build()?;
        client.phone_numbers().provision(&h, "local", None)
    })?;
    Ok(number.number)
}

/// Mint (or rotate) the org's webhook signing key.
///
/// # Arguments
/// * `base_url` - Inkbox API base URL.
/// * `api_key` - the key to act as.
///
/// # Returns
/// The `whsec_…` signing key string (shown once).
pub fn create_signing_key(base_url: &str, api_key: &str) -> anyhow::Result<String> {
    let (key, base) = (api_key.to_string(), base_url.to_string());
    let sk = off_runtime(move || {
        let client = Inkbox::builder(key).base_url(base).build()?;
        client.create_signing_key()
    })?;
    Ok(sk.signing_key)
}
