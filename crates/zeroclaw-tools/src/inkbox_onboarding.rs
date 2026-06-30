//! Inkbox channel onboarding — the live SDK round-trips behind the Quickstart
//! "Channels" step. The CLI surface owns the prompts; this module owns the
//! network calls so they live in one place and the blocking SDK always runs
//! off the tokio runtime.

use std::sync::Arc;

use inkbox::identities::types::{IdentityPhoneNumberCreateOptions, Unset};
use inkbox::whoami::types::{
    AUTH_SUBTYPE_API_KEY_AGENT_SCOPED_CLAIMED, AUTH_SUBTYPE_API_KEY_AGENT_SCOPED_UNCLAIMED,
    WhoamiResponse,
};
use inkbox::{Inkbox, InkboxError};
use uuid::Uuid;

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

/// How a pasted key authenticates, from `whoami` — used to branch on
/// `auth_subtype`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyAuth {
    /// Agent-scoped key — bound to exactly one identity.
    AgentScoped,
    /// Admin-scoped (or unrecognised api-key subtype) — sees many identities.
    AdminScoped,
    /// A non-API-key credential (a JWT) — unusable for a gateway.
    NotApiKey,
}

/// Result of `whoami` the wizard needs to classify a pasted key.
#[derive(Debug, Clone)]
pub struct WhoamiInfo {
    /// Auth classification used to branch (agent vs admin vs JWT).
    pub auth: KeyAuth,
    /// Raw `auth_subtype` string (e.g. `api_key.agent_scoped.claimed`).
    pub subtype: String,
    /// The caller's organization id.
    pub organization_id: String,
}

/// Call `whoami` to validate a pasted key and classify it.
///
/// # Arguments
/// * `base_url` - Inkbox API base URL.
/// * `api_key` - the key to inspect.
///
/// # Returns
/// A [`WhoamiInfo`] with the auth classification, raw subtype, and org id.
pub fn whoami_scope(base_url: &str, api_key: &str) -> anyhow::Result<WhoamiInfo> {
    let (key, base) = (api_key.to_string(), base_url.to_string());
    let who = off_runtime(move || {
        let client = Inkbox::builder(key).base_url(base).build()?;
        client.whoami()
    })?;
    Ok(match who {
        WhoamiResponse::ApiKey(k) => {
            let subtype = k.auth_subtype.unwrap_or_default();
            let auth = if subtype == AUTH_SUBTYPE_API_KEY_AGENT_SCOPED_CLAIMED
                || subtype == AUTH_SUBTYPE_API_KEY_AGENT_SCOPED_UNCLAIMED
            {
                KeyAuth::AgentScoped
            } else {
                // Admin-scoped, or any unrecognised api-key subtype → list path.
                KeyAuth::AdminScoped
            };
            WhoamiInfo {
                auth,
                subtype,
                organization_id: k.organization_id.unwrap_or_default(),
            }
        }
        WhoamiResponse::Jwt(j) => WhoamiInfo {
            auth: KeyAuth::NotApiKey,
            subtype: "jwt".to_string(),
            organization_id: j.organization_id.unwrap_or_default(),
        },
    })
}

/// List the agent handles a key can see (one for an agent-scoped key, many for
/// an admin key). Used in the paste-a-key flow.
///
/// # Arguments
/// * `base_url` - Inkbox API base URL.
/// * `api_key` - the key to act as.
///
/// # Returns
/// The visible agent handles.
pub fn list_identity_handles(base_url: &str, api_key: &str) -> anyhow::Result<Vec<String>> {
    let (key, base) = (api_key.to_string(), base_url.to_string());
    let ids = off_runtime(move || {
        let client = Inkbox::builder(key).base_url(base).build()?;
        client.list_identities()
    })?;
    Ok(ids.into_iter().map(|s| s.agent_handle).collect())
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

/// Check for an inbound `START` opt-in text; returns the sender's number once
/// seen. Used to poll for the carrier SMS opt-in after provisioning a number.
///
/// # Arguments
/// * `base_url` - Inkbox API base URL.
/// * `api_key` - the key to act as.
/// * `handle` - the agent handle whose number to inspect.
///
/// # Returns
/// `Some(sender_number)` once an inbound `START` is seen, else `None`.
pub fn check_sms_start(
    base_url: &str,
    api_key: &str,
    handle: &str,
) -> anyhow::Result<Option<String>> {
    let (key, base, h) = (
        api_key.to_string(),
        base_url.to_string(),
        handle.to_string(),
    );
    off_runtime(move || {
        let client = Arc::new(Inkbox::builder(key).base_url(base).build()?);
        let id = client.get_identity(&h)?;
        let texts = id.list_texts(20, 0, None, None)?;
        Ok(texts.into_iter().find_map(|t| {
            let is_start = t
                .text
                .as_deref()
                .is_some_and(|s| s.trim().eq_ignore_ascii_case("start"));
            if t.direction.eq_ignore_ascii_case("inbound") && is_start {
                t.remote_phone_number
            } else {
                None
            }
        }))
    })
}

/// iMessage configuration state for an identity, so reruns can skip the enable
/// prompt and not read like a first-time setup.
#[derive(Debug, Clone, Default)]
pub struct ImessageStatus {
    /// Whether shared-iMessage reachability is already enabled.
    pub enabled: bool,
    /// Remote numbers already connected through the router (empty if disabled).
    pub connected: Vec<String>,
}

/// Read whether iMessage is enabled for the identity and which phones are
/// already connected.
///
/// # Arguments
/// * `base_url` - Inkbox API base URL.
/// * `api_key` - the key to act as.
/// * `handle` - the agent handle to inspect.
///
/// # Returns
/// An [`ImessageStatus`] (enabled flag + connected remote numbers).
pub fn imessage_status(
    base_url: &str,
    api_key: &str,
    handle: &str,
) -> anyhow::Result<ImessageStatus> {
    let (key, base, h) = (
        api_key.to_string(),
        base_url.to_string(),
        handle.to_string(),
    );
    off_runtime(move || {
        let client = Arc::new(Inkbox::builder(key).base_url(base).build()?);
        let id = client.get_identity(&h)?;
        let enabled = id.imessage_enabled();
        // Listing assignments requires iMessage on, so only ask when enabled.
        let connected = if enabled {
            id.list_imessage_assignments(5, 0)
                .map(|v| v.into_iter().map(|a| a.remote_number).collect())
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        Ok(ImessageStatus { enabled, connected })
    })
}

/// Enable shared-iMessage reachability on the identity.
///
/// # Arguments
/// * `base_url` - Inkbox API base URL.
/// * `api_key` - the key to act as.
/// * `handle` - the agent handle to enable iMessage on.
///
/// # Returns
/// `Ok(())` once iMessage is enabled.
pub fn enable_imessage(base_url: &str, api_key: &str, handle: &str) -> anyhow::Result<()> {
    let (key, base, h) = (
        api_key.to_string(),
        base_url.to_string(),
        handle.to_string(),
    );
    off_runtime(move || {
        let client = Arc::new(Inkbox::builder(key).base_url(base).build()?);
        let id = client.get_identity(&h)?;
        id.update(None, Unset::Omit, Unset::Omit, Some(true), None, None)
    })
}

/// Fetch the iMessage router number and the `connect` command a phone texts it.
///
/// # Arguments
/// * `base_url` - Inkbox API base URL.
/// * `api_key` - the key to act as.
///
/// # Returns
/// `(router_number, connect_command)`.
pub fn imessage_connect_info(base_url: &str, api_key: &str) -> anyhow::Result<(String, String)> {
    let (key, base) = (api_key.to_string(), base_url.to_string());
    off_runtime(move || {
        let client = Inkbox::builder(key).base_url(base).build()?;
        let triage = client.imessages().get_triage_number()?;
        Ok((triage.number, triage.connect_command))
    })
}

/// Check for the first inbound iMessage; returns its conversation + sender once
/// seen. Used to poll for the iPhone connect during onboarding.
///
/// # Arguments
/// * `base_url` - Inkbox API base URL.
/// * `api_key` - the key to act as.
/// * `handle` - the agent handle to inspect.
///
/// # Returns
/// `Some((conversation_id, sender_number))` once an inbound iMessage arrives.
pub fn check_first_imessage(
    base_url: &str,
    api_key: &str,
    handle: &str,
) -> anyhow::Result<Option<(Uuid, String)>> {
    let (key, base, h) = (
        api_key.to_string(),
        base_url.to_string(),
        handle.to_string(),
    );
    off_runtime(move || {
        let client = Arc::new(Inkbox::builder(key).base_url(base).build()?);
        let id = client.get_identity(&h)?;
        let msgs = id.list_imessages(None, 10, 0, None, None)?;
        Ok(msgs
            .into_iter()
            .find(|m| m.direction.eq_ignore_ascii_case("inbound"))
            .map(|m| (m.conversation_id, m.remote_number)))
    })
}

/// Send a welcome iMessage into the freshly connected conversation.
///
/// # Arguments
/// * `base_url` - Inkbox API base URL.
/// * `api_key` - the key to act as.
/// * `handle` - the agent handle that owns the conversation.
/// * `conversation_id` - the conversation to reply into.
/// * `text` - the welcome message body.
///
/// # Returns
/// `Ok(())` once sent.
pub fn send_imessage_welcome(
    base_url: &str,
    api_key: &str,
    handle: &str,
    conversation_id: Uuid,
    text: &str,
) -> anyhow::Result<()> {
    let (key, base, h, body) = (
        api_key.to_string(),
        base_url.to_string(),
        handle.to_string(),
        text.to_string(),
    );
    off_runtime(move || {
        let client = Arc::new(Inkbox::builder(key).base_url(base).build()?);
        let id = client.get_identity(&h)?;
        id.send_imessage(None, Some(&conversation_id), Some(&body), None, None)
            .map(|_| ())
    })
}

/// Create a new agent identity (admin-key path), optionally provisioning a
/// local phone number.
///
/// # Arguments
/// * `base_url` - Inkbox API base URL.
/// * `api_key` - an admin-scoped key.
/// * `handle` - desired (globally unique) agent handle.
/// * `display_name` - optional display name shown to recipients.
/// * `provision_phone` - provision a local US number when `true`.
///
/// # Returns
/// The created identity's handle.
pub fn create_identity(
    base_url: &str,
    api_key: &str,
    handle: &str,
    display_name: Option<&str>,
    provision_phone: bool,
) -> anyhow::Result<String> {
    let (key, base, h, dn) = (
        api_key.to_string(),
        base_url.to_string(),
        handle.to_string(),
        display_name.map(|s| s.to_string()),
    );
    off_runtime(move || {
        let client = Arc::new(Inkbox::builder(key).base_url(base).build()?);
        let phone = provision_phone.then(IdentityPhoneNumberCreateOptions::default);
        let id = client.create_identity_with(
            &h,
            dn.as_deref(),
            Unset::Omit,
            None,
            None,
            Unset::Omit,
            None,
            phone.as_ref(),
            None,
        )?;
        Ok(id.agent_handle())
    })
}

/// Mint an agent-scoped API key for an identity so the gateway never stores the
/// admin key (admin-key path).
///
/// # Arguments
/// * `base_url` - Inkbox API base URL.
/// * `api_key` - an admin-scoped key.
/// * `handle` - the identity to scope the new key to.
///
/// # Returns
/// The newly minted agent-scoped API key.
pub fn mint_agent_key(base_url: &str, api_key: &str, handle: &str) -> anyhow::Result<String> {
    let (key, base, h) = (
        api_key.to_string(),
        base_url.to_string(),
        handle.to_string(),
    );
    off_runtime(move || {
        let client = Arc::new(Inkbox::builder(key).base_url(base).build()?);
        let id = client.get_identity(&h)?;
        let label = format!("ZeroClaw gateway - {}", id.agent_handle());
        let created = client.api_keys().create(
            &label,
            Some("Auto-minted by zeroclaw quickstart; scoped to one identity."),
            Some(id.id()),
        )?;
        Ok(created.api_key)
    })
}
