//! Interactive Inkbox onboarding for the CLI Quickstart "Channels" step.
//!
//! Inkbox is a native channel in this fork, so picking it branches out of the
//! generic schema field-form into a live wizard: self-signup (or paste a key),
//! verification, phone provisioning, a webhook signing key, and OpenAI Realtime
//! call config. The SDK round-trips live in
//! [`zeroclaw_runtime::inkbox_onboarding`]; this module owns the prompts and
//! returns `(alias, fields)` for the caller to fold into the submission.
//!
//! All user-facing text routes through `crate::t` (a `cli-*` Fluent key with an
//! English fallback), and the blocking dialoguer prompts mirror the rest of
//! `run_quickstart_cli`.

use std::collections::BTreeMap;

use zeroclaw_runtime::inkbox_onboarding as ob;

const DEFAULT_BASE_URL: &str = "https://inkbox.ai";

/// Run the Inkbox channel wizard.
///
/// # Returns
/// `Some((alias, fields))` to materialize a `[channels.inkbox.<alias>]` block,
/// or `None` if the user backed out (the caller then re-renders the channel
/// list, nothing written).
pub(crate) fn run() -> anyhow::Result<Option<(String, BTreeMap<String, String>)>> {
    let base_url = std::env::var("INKBOX_BASE_URL")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_BASE_URL.to_string());

    println!(
        "  {}",
        crate::t(
            "cli-quickstart-inkbox-intro",
            "API-first email + SMS + voice + identity for one agent.",
        )
    );

    let Some(has_key) = confirm(
        &crate::t(
            "cli-quickstart-inkbox-has-key",
            "Do you already have an Inkbox API key?",
        ),
        false,
    )?
    else {
        return Ok(None);
    };

    // Resolve the identity by signup or pasted key. Each flow owns the phone
    // question (mirroring Hermes' per-flow provisioning) and reports back the
    // resolved number plus whether it was *just* provisioned this run.
    let Some((api_key, handle, phone_number, did_provision)) = (if has_key {
        api_key_flow(&base_url)?
    } else {
        signup_flow(&base_url)?
    }) else {
        return Ok(None);
    };

    let mut fields: BTreeMap<String, String> = BTreeMap::new();
    fields.insert("api_key".into(), api_key.clone());
    fields.insert("identity".into(), handle.clone());
    if base_url != DEFAULT_BASE_URL {
        fields.insert("base_url".into(), base_url.clone());
    }

    // Hermes order: SMS opt-in (only when we just provisioned — a pre-existing
    // number is assumed already opted in) → realtime → iMessage → signing key.
    if did_provision {
        if let Some(number) = phone_number.as_deref() {
            sms_opt_in(&base_url, &api_key, &handle, number)?;
        }
    }
    setup_realtime(&mut fields)?;
    setup_imessage(&base_url, &api_key, &handle)?;
    setup_signing_key(&base_url, &api_key, &mut fields)?;

    let alias = match prompt_alias()? {
        Some(a) => a,
        None => return Ok(None),
    };

    let channel_ref = format!("inkbox.{alias}");
    println!(
        "  {} {}",
        crate::t(
            "cli-quickstart-inkbox-configured",
            "✓ Inkbox channel configured:",
        ),
        channel_ref
    );
    Ok(Some((alias, fields)))
}

/// Offer to provision a local number when the identity has none, mirroring
/// Hermes `_offer_phone_for_existing`: stay silent and provision nothing if it
/// already has one.
///
/// # Arguments
/// * `base_url` - Inkbox API base URL.
/// * `api_key` - the (agent-scoped) key to act as.
/// * `handle` - the agent handle to attach a number to.
/// * `current` - the identity's existing phone number, if any.
///
/// # Returns
/// `(phone, did_provision)` — the number to use (existing or freshly minted)
/// and whether this call provisioned it (gates the SMS opt-in poll).
fn offer_phone_for_existing(
    base_url: &str,
    api_key: &str,
    handle: &str,
    current: Option<String>,
) -> anyhow::Result<(Option<String>, bool)> {
    // Already has one — say nothing and move on (Hermes returns immediately).
    if current.is_some() {
        return Ok((current, false));
    }
    println!(
        "\n  {}",
        crate::t(
            "cli-quickstart-inkbox-no-phone",
            "This agent has no phone number attached.",
        )
    );
    println!(
        "  {}",
        crate::t(
            "cli-quickstart-inkbox-phone-unlocks",
            "A local US number unlocks SMS and voice for this agent.",
        )
    );
    if confirm(
        &crate::t(
            "cli-quickstart-inkbox-provision-now",
            "Provision a local phone number now?",
        ),
        true,
    )? != Some(true)
    {
        return Ok((None, false));
    }
    match ob::provision_phone(base_url, api_key, handle) {
        Ok(number) => {
            println!(
                "  {} {}",
                crate::t("cli-quickstart-inkbox-provisioned", "✓ Provisioned:"),
                number
            );
            Ok((Some(number), true))
        }
        Err(err) => {
            eprintln!(
                "  {} {}",
                crate::t(
                    "cli-quickstart-inkbox-provision-failed",
                    "could not provision a number:",
                ),
                err
            );
            Ok((None, false))
        }
    }
}

/// Self-signup branch: create a fresh identity, verify it, then offer a number.
/// Returns `(api_key, handle, phone, did_provision)`.
fn signup_flow(
    base_url: &str,
) -> anyhow::Result<Option<(String, String, Option<String>, bool)>> {
    println!(
        "  {}",
        crate::t(
            "cli-quickstart-inkbox-signup-intro",
            "We'll create a fresh agent identity for you via self-signup.",
        )
    );

    let Some(email) = input(
        &crate::t(
            "cli-quickstart-inkbox-email",
            "Your email address (for verification)",
        ),
        None,
        false,
    )?
    else {
        return Ok(None);
    };
    let email = email.trim().to_string();
    if !email.contains('@') {
        eprintln!(
            "  {}",
            crate::t(
                "cli-quickstart-inkbox-bad-email",
                "A valid email address is required.",
            )
        );
        return Ok(None);
    }

    let Some(handle) = input(
        &crate::t(
            "cli-quickstart-inkbox-handle",
            "Desired agent handle (globally unique)",
        ),
        None,
        false,
    )?
    else {
        return Ok(None);
    };
    let handle = handle.trim().to_string();
    if handle.is_empty() {
        return Ok(None);
    }

    println!(
        "  {}",
        crate::t("cli-quickstart-inkbox-signing-up", "Calling agent-signup…")
    );
    let signup = match ob::signup(base_url, &email, &handle) {
        Ok(s) => s,
        Err(err) => {
            eprintln!(
                "  {} {}",
                crate::t("cli-quickstart-inkbox-signup-failed", "signup failed:"),
                err
            );
            return Ok(None);
        }
    };
    println!(
        "  {} {}",
        crate::t("cli-quickstart-inkbox-created", "✓ created"),
        signup.agent_handle
    );
    println!(
        "  {} {}",
        crate::t("cli-quickstart-inkbox-mailbox", "mailbox:"),
        signup.email_address
    );
    println!(
        "  {} {}",
        crate::t(
            "cli-quickstart-inkbox-code-sent",
            "A 6-digit code was sent to",
        ),
        email
    );

    loop {
        let Some(entry) = input(
            &crate::t(
                "cli-quickstart-inkbox-code",
                "Verification code (or 'resend')",
            ),
            None,
            true,
        )?
        else {
            return Ok(None);
        };
        let entry = entry.trim().to_string();
        if entry.is_empty() {
            continue;
        }
        if entry.eq_ignore_ascii_case("resend") || entry.eq_ignore_ascii_case("r") {
            match ob::resend(base_url, &signup.api_key) {
                Ok(()) => println!(
                    "  {} {}",
                    crate::t("cli-quickstart-inkbox-resent", "✓ Resent. Check"),
                    email
                ),
                Err(err) => eprintln!(
                    "  {} {}",
                    crate::t("cli-quickstart-inkbox-resend-failed", "resend failed:"),
                    err
                ),
            }
            continue;
        }
        match ob::verify(base_url, &signup.api_key, &entry) {
            Ok(()) => {
                println!(
                    "  {}",
                    crate::t("cli-quickstart-inkbox-verified", "✓ verified")
                );
                break;
            }
            Err(err) => eprintln!(
                "  {} {}",
                crate::t("cli-quickstart-inkbox-bad-code", "wrong code:"),
                err
            ),
        }
    }
    // Fresh identity has no number yet — offer to provision one (Hermes signup
    // provisions here too), which also arms the SMS opt-in poll.
    let (phone, did_provision) =
        offer_phone_for_existing(base_url, &signup.api_key, &signup.agent_handle, None)?;
    Ok(Some((
        signup.api_key,
        signup.agent_handle,
        phone,
        did_provision,
    )))
}

/// Paste-a-key branch: validate the key and confirm its bound identity.
/// Returns `(api_key, handle, phone, did_provision)`.
fn api_key_flow(
    base_url: &str,
) -> anyhow::Result<Option<(String, String, Option<String>, bool)>> {
    let Some(api_key) = password(&crate::t(
        "cli-quickstart-inkbox-paste-key",
        "Paste your Inkbox API key (ApiKey_…)",
    ))?
    else {
        return Ok(None);
    };
    let api_key = api_key.trim().to_string();
    if api_key.is_empty() {
        eprintln!(
            "  {}",
            crate::t("cli-quickstart-inkbox-no-key", "No key provided.")
        );
        return Ok(None);
    }

    // Hermes parity: whoami validates + classifies the key, then we resolve the
    // identity from the key — never by asking. Agent-scoped keys map to exactly
    // one identity; admin keys list and let the operator pick.
    let info = match ob::whoami_scope(base_url, &api_key) {
        Ok(i) => i,
        Err(err) => {
            eprintln!(
                "  {} {}",
                crate::t("cli-quickstart-inkbox-whoami-failed", "whoami failed:"),
                err
            );
            return Ok(None);
        }
    };
    if info.auth == ob::KeyAuth::NotApiKey {
        eprintln!(
            "  {}",
            crate::t(
                "cli-quickstart-inkbox-not-api-key",
                "This wizard requires an API key, but the credential is a JWT.",
            )
        );
        return Ok(None);
    }
    println!(
        "  {} {}",
        crate::t(
            "cli-quickstart-inkbox-key-validated",
            "✓ Key validated. Scope:",
        ),
        info.subtype
    );

    let handles = match ob::list_identity_handles(base_url, &api_key) {
        Ok(h) => h,
        Err(err) => {
            eprintln!(
                "  {} {}",
                crate::t(
                    "cli-quickstart-inkbox-list-failed",
                    "could not list identities:",
                ),
                err
            );
            return Ok(None);
        }
    };
    // The key the channel will store: the pasted key for an agent-scoped key,
    // or a freshly-minted agent-scoped key for the admin path. `created_new`
    // marks the admin "create a new identity" path, whose phone question is
    // handled during creation (so we don't re-offer it afterwards).
    let (effective_key, handle, created_new): (String, String, bool) = match info.auth {
        // Agent-scoped: bound to one identity — use it (warn if the API ever
        // returns more), exactly like Hermes `_pick_agent_scoped`.
        ob::KeyAuth::AgentScoped => {
            if handles.is_empty() {
                eprintln!(
                    "  {}",
                    crate::t(
                        "cli-quickstart-inkbox-no-identities",
                        "Agent-scoped key but no identity returned.",
                    )
                );
                return Ok(None);
            }
            if handles.len() > 1 {
                eprintln!(
                    "  {} {} {}",
                    crate::t(
                        "cli-quickstart-inkbox-agent-multi-a",
                        "Agent-scoped key returned",
                    ),
                    handles.len(),
                    crate::t(
                        "cli-quickstart-inkbox-agent-multi-b",
                        "identities; using the first.",
                    ),
                );
            }
            println!(
                "  {} {}",
                crate::t(
                    "cli-quickstart-inkbox-bound",
                    "This API key is bound to identity:",
                ),
                handles[0]
            );
            (api_key.clone(), handles[0].clone(), false)
        }
        // Admin-scoped: pick an existing identity OR create a new one, then mint
        // an agent-scoped key so the gateway never stores the admin key
        // (Hermes `_pick_admin_scoped` + `_create_identity` + `_mint_agent_scoped_key`).
        _ => {
            let create_label = crate::t(
                "cli-quickstart-inkbox-create-new",
                "+ Create a new identity",
            );
            let mut items = handles.clone();
            items.push(create_label);
            let Some(idx) = dialoguer::FuzzySelect::new()
                .with_prompt(crate::t(
                    "cli-quickstart-inkbox-pick-identity",
                    "Select the identity this gateway runs as",
                ))
                .items(&items)
                .default(0)
                .max_length(items.len().max(1))
                .interact_opt()?
            else {
                return Ok(None);
            };
            let (chosen, created_new) = if idx < handles.len() {
                (handles[idx].clone(), false)
            } else {
                match create_new_identity(base_url, &api_key)? {
                    Some(h) => (h, true),
                    None => return Ok(None),
                }
            };
            let minted = match ob::mint_agent_key(base_url, &api_key, &chosen) {
                Ok(k) => k,
                Err(err) => {
                    eprintln!(
                        "  {} {}",
                        crate::t(
                            "cli-quickstart-inkbox-mint-failed",
                            "could not mint a scoped key:",
                        ),
                        err
                    );
                    return Ok(None);
                }
            };
            println!(
                "  {} {}",
                crate::t(
                    "cli-quickstart-inkbox-minted",
                    "✓ minted an agent-scoped key for",
                ),
                chosen
            );
            (minted, chosen, created_new)
        }
    };

    match ob::fetch_identity(base_url, &effective_key, &handle) {
        Ok(id) => {
            println!(
                "  {} {}",
                crate::t("cli-quickstart-inkbox-key-bound", "✓ key validated for"),
                id.handle
            );
            if let Some(phone) = &id.phone_number {
                println!(
                    "  {} {}",
                    crate::t("cli-quickstart-inkbox-phone", "phone:"),
                    phone
                );
            }
            let handle = id.handle;
            let (phone, did_provision) = if created_new {
                // The create sub-flow already asked about (and may have minted)
                // a number — don't re-offer; a number present means we just
                // minted it, which arms the SMS opt-in poll (Hermes parity).
                let just_provisioned = id.phone_number.is_some();
                (id.phone_number, just_provisioned)
            } else {
                // Existing identity (agent-scoped or admin-pick): offer a number
                // only if it has none, mirroring `_offer_phone_for_existing`.
                offer_phone_for_existing(base_url, &effective_key, &handle, id.phone_number)?
            };
            Ok(Some((effective_key, handle, phone, did_provision)))
        }
        Err(err) => {
            eprintln!(
                "  {} {}",
                crate::t(
                    "cli-quickstart-inkbox-handle-failed",
                    "could not load that identity:",
                ),
                err
            );
            Ok(None)
        }
    }
}

/// Admin-key "create a new identity" sub-flow (Hermes `_create_identity`):
/// prompt handle + optional display name, offer a phone number, then create it.
fn create_new_identity(base_url: &str, api_key: &str) -> anyhow::Result<Option<String>> {
    let Some(handle) = input(
        &crate::t(
            "cli-quickstart-inkbox-new-handle",
            "Agent handle for the new identity (globally unique)",
        ),
        None,
        false,
    )?
    else {
        return Ok(None);
    };
    let handle = handle.trim().to_string();
    if handle.is_empty() {
        return Ok(None);
    }
    let display = input(
        &crate::t(
            "cli-quickstart-inkbox-new-display",
            "Display name (shown to recipients, optional)",
        ),
        None,
        true,
    )?
    .map(|s| s.trim().to_string())
    .filter(|s| !s.is_empty());
    let provision = matches!(
        confirm(
            &crate::t(
                "cli-quickstart-inkbox-new-phone",
                "Provision a local phone number? (unlocks SMS + voice)",
            ),
            true,
        )?,
        Some(true)
    );
    match ob::create_identity(base_url, api_key, &handle, display.as_deref(), provision) {
        Ok(h) => {
            println!(
                "  {} {}",
                crate::t("cli-quickstart-inkbox-new-created", "✓ created identity"),
                h
            );
            Ok(Some(h))
        }
        Err(err) => {
            eprintln!(
                "  {} {}",
                crate::t(
                    "cli-quickstart-inkbox-new-failed",
                    "could not create identity:",
                ),
                err
            );
            Ok(None)
        }
    }
}

/// Webhook signing key: paste an existing one or mint a fresh key.
fn setup_signing_key(
    base_url: &str,
    api_key: &str,
    fields: &mut BTreeMap<String, String>,
) -> anyhow::Result<()> {
    println!(
        "\n  {}",
        crate::t(
            "cli-quickstart-inkbox-signing-header",
            "--- Webhook signing key ---"
        )
    );
    println!(
        "  {}",
        crate::t(
            "cli-quickstart-inkbox-signing-explain1",
            "Inkbox signs outbound webhooks with an HMAC over the body.",
        )
    );
    println!(
        "  {}",
        crate::t(
            "cli-quickstart-inkbox-signing-explain2",
            "Without the matching key, the gateway cannot verify inbound Inkbox traffic.",
        )
    );

    if let Some(true) = confirm(
        &crate::t(
            "cli-quickstart-inkbox-have-signing",
            "Do you already have an Inkbox signing key?",
        ),
        false,
    )? {
        if let Some(key) = password(&crate::t(
            "cli-quickstart-inkbox-paste-signing",
            "Paste your Inkbox signing key",
        ))? {
            let key = key.trim().to_string();
            if !key.is_empty() {
                fields.insert("signing_key".into(), key);
                println!(
                    "  {}",
                    crate::t(
                        "cli-quickstart-inkbox-signing-saved",
                        "✓ Saved signing key. Signature verification enabled.",
                    )
                );
            }
        }
        return Ok(());
    }

    println!(
        "  {}",
        crate::t(
            "cli-quickstart-inkbox-signing-rotate1",
            "Minting a new key here rotates any existing key for your org.",
        )
    );
    println!(
        "  {}",
        crate::t(
            "cli-quickstart-inkbox-signing-rotate2",
            "Any other gateway using the old key will fail verification until updated.",
        )
    );
    if let Some(true) = confirm(
        &crate::t(
            "cli-quickstart-inkbox-gen-signing",
            "Generate a new signing key now?",
        ),
        true,
    )? {
        match ob::create_signing_key(base_url, api_key) {
            Ok(key) => {
                fields.insert("signing_key".into(), key);
                println!(
                    "  {}",
                    crate::t(
                        "cli-quickstart-inkbox-signing-generated",
                        "✓ Generated and saved signing key. Signature verification enabled.",
                    )
                );
            }
            Err(err) => eprintln!(
                "  {} {}",
                crate::t(
                    "cli-quickstart-inkbox-signing-failed",
                    "could not create signing key:",
                ),
                err
            ),
        }
    }
    Ok(())
}

/// OpenAI Realtime call config. Detects a key from the environment; the live
/// websocket probe is deferred — the key is validated on the first call.
fn setup_realtime(fields: &mut BTreeMap<String, String>) -> anyhow::Result<()> {
    let detected = std::env::var("INKBOX_REALTIME_API_KEY")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| {
            std::env::var("OPENAI_API_KEY")
                .ok()
                .filter(|s| !s.is_empty())
        });

    println!(
        "\n  {}",
        crate::t(
            "cli-quickstart-inkbox-rt-header",
            "--- OpenAI Realtime calls ---"
        )
    );
    println!(
        "  {}",
        crate::t(
            "cli-quickstart-inkbox-rt-explain1",
            "Realtime calls send raw phone audio to OpenAI Realtime.",
        )
    );
    println!(
        "  {}",
        crate::t(
            "cli-quickstart-inkbox-rt-explain2",
            "This requires an OpenAI API key with /v1/realtime permission.",
        )
    );
    if detected.is_some() {
        println!(
            "  {}",
            crate::t(
                "cli-quickstart-inkbox-rt-found",
                "Found an existing OpenAI API key in your environment.",
            )
        );
    } else {
        println!(
            "  {}",
            crate::t(
                "cli-quickstart-inkbox-rt-none",
                "No OpenAI API key was detected for Realtime.",
            )
        );
    }

    let Some(true) = confirm(
        &crate::t(
            "cli-quickstart-inkbox-rt-enable",
            "Use OpenAI Realtime API for phone calls?",
        ),
        detected.is_some(),
    )?
    else {
        println!(
            "  {}",
            crate::t(
                "cli-quickstart-inkbox-rt-disabled",
                "Realtime disabled. Calls will use Inkbox STT/TTS.",
            )
        );
        return Ok(());
    };

    let key = match detected {
        Some(k) => k,
        None => match password(&crate::t(
            "cli-quickstart-inkbox-rt-paste",
            "Paste your OpenAI API key for Realtime calls",
        ))? {
            Some(k) if !k.trim().is_empty() => k.trim().to_string(),
            _ => {
                println!(
                    "  {}",
                    crate::t(
                        "cli-quickstart-inkbox-rt-skip",
                        "No OpenAI API key entered. Realtime disabled; calls will use Inkbox STT/TTS.",
                    )
                );
                return Ok(());
            }
        },
    };
    fields.insert("realtime_enabled".into(), "true".into());
    fields.insert("realtime_api_key".into(), key);
    println!(
        "  {}",
        crate::t(
            "cli-quickstart-inkbox-rt-on",
            "✓ Realtime calls are enabled for this agent.",
        )
    );
    Ok(())
}

/// Prompt for the channel alias with no pre-filled default (the handle often
/// isn't a valid alias — hyphens), re-prompting until it's a valid alias key.
/// Returns `None` if the user backs out.
fn prompt_alias() -> anyhow::Result<Option<String>> {
    loop {
        let Some(raw) = input(
            &crate::t(
                "cli-quickstart-inkbox-alias",
                "Alias for this Inkbox channel",
            ),
            None,
            false,
        )?
        else {
            return Ok(None);
        };
        let candidate = raw.trim().to_string();
        if candidate.is_empty() {
            continue;
        }
        match zeroclaw_config::helpers::validate_alias_key(&candidate) {
            Ok(()) => return Ok(Some(candidate)),
            Err(err) => eprintln!(
                "  {} {}",
                crate::t("cli-quickstart-inkbox-bad-alias", "invalid alias:"),
                err
            ),
        }
    }
}

/// Seconds to poll for an inbound opt-in / connect message before giving up.
const POLL_SECS: u64 = 90;

/// SMS opt-in walkthrough: tell the user to text START, then poll for the
/// inbound START that unlocks outbound SMS (Hermes parity; time-bounded poll).
fn sms_opt_in(base_url: &str, api_key: &str, handle: &str, number: &str) -> anyhow::Result<()> {
    println!(
        "\n  {}",
        crate::t("cli-quickstart-inkbox-sms-header", "--- SMS opt-in ---")
    );
    println!(
        "  {} {} {}",
        crate::t("cli-quickstart-inkbox-sms-text-start", "Text START to"),
        number,
        crate::t(
            "cli-quickstart-inkbox-sms-line1b",
            "to enable SMS from this agent",
        ),
    );
    println!(
        "  {}",
        crate::t(
            "cli-quickstart-inkbox-sms-line2",
            "to your phone. Do this from every phone you want to message it from.",
        )
    );
    println!(
        "\n  {}",
        crate::t(
            "cli-quickstart-inkbox-sms-waiting-header",
            "--- Waiting for your START text ---",
        )
    );
    println!(
        "  {} {}.",
        crate::t(
            "cli-quickstart-inkbox-sms-polling",
            "Polling every 3s for an inbound START to",
        ),
        number,
    );
    println!(
        "  {}",
        crate::t(
            "cli-quickstart-inkbox-sms-without",
            "Without it, the agent cannot send outbound SMS to that phone later.",
        )
    );
    let found = poll_with_spinner(
        &crate::t(
            "cli-quickstart-inkbox-sms-listening",
            "Listening for START...",
        ),
        || {
            ob::check_sms_start(base_url, api_key, handle)
                .ok()
                .flatten()
        },
    );
    match found {
        Some(sender) => println!(
            "  {} {}",
            crate::t(
                "cli-quickstart-inkbox-sms-confirmed",
                "✓ Got it. SMS opt-in confirmed from",
            ),
            sender,
        ),
        None => println!(
            "  {} {} {}",
            crate::t("cli-quickstart-inkbox-sms-text-start", "Text START to"),
            number,
            crate::t(
                "cli-quickstart-inkbox-sms-later-b",
                "anytime to enable outbound SMS.",
            ),
        ),
    }
    Ok(())
}

/// iMessage connect walkthrough: enable iMessage, walk the user through texting
/// the router, then (opt-in) poll for the first inbound message and greet back.
fn setup_imessage(base_url: &str, api_key: &str, handle: &str) -> anyhow::Result<()> {
    println!(
        "\n  {}",
        crate::t("cli-quickstart-inkbox-imsg-header", "--- iMessage ---")
    );
    println!(
        "  {}",
        crate::t(
            "cli-quickstart-inkbox-imsg-explain1",
            "Inkbox can make this agent reachable over iMessage from your iPhone.",
        )
    );
    println!(
        "  {}",
        crate::t(
            "cli-quickstart-inkbox-imsg-explain2",
            "No number to provision — you connect through the Inkbox iMessage router.",
        )
    );
    // Hermes parity: skip the enable prompt entirely when iMessage is already
    // on; surface phones already connected so a rerun doesn't read like a
    // first-time setup (and defaults the walkthrough off when one exists).
    let status = match ob::imessage_status(base_url, api_key, handle) {
        Ok(s) => s,
        Err(err) => {
            eprintln!(
                "  {} {}",
                crate::t(
                    "cli-quickstart-inkbox-imsg-status-failed",
                    "could not check iMessage status:",
                ),
                err,
            );
            return Ok(());
        }
    };
    let mut connected = status.connected;
    if status.enabled {
        println!(
            "  {}",
            crate::t(
                "cli-quickstart-inkbox-imsg-already-enabled",
                "✓ iMessage is already enabled for this agent.",
            )
        );
    } else {
        let Some(true) = confirm(
            &crate::t(
                "cli-quickstart-inkbox-imsg-enable",
                "Enable iMessage for this agent?",
            ),
            true,
        )?
        else {
            return Ok(());
        };
        if let Err(err) = ob::enable_imessage(base_url, api_key, handle) {
            eprintln!(
                "  {} {}",
                crate::t(
                    "cli-quickstart-inkbox-imsg-enable-failed",
                    "could not enable iMessage:",
                ),
                err,
            );
            return Ok(());
        }
        println!(
            "  {}",
            crate::t(
                "cli-quickstart-inkbox-imsg-enabled",
                "✓ iMessage enabled for this agent.",
            )
        );
        connected = Vec::new();
    }
    if !connected.is_empty() {
        println!(
            "  {} {}",
            crate::t(
                "cli-quickstart-inkbox-imsg-already-connected",
                "✓ Already connected:",
            ),
            connected.join(", "),
        );
    }
    // Default the walkthrough off when a phone is already connected (Hermes
    // `not connected`): reconnecting another iPhone is the rare case.
    let connect_q = if connected.is_empty() {
        crate::t(
            "cli-quickstart-inkbox-imsg-connect",
            "Connect your iPhone to this agent now?",
        )
    } else {
        crate::t(
            "cli-quickstart-inkbox-imsg-connect-another",
            "Connect another iPhone to this agent now?",
        )
    };
    let Some(true) = confirm(&connect_q, connected.is_empty())? else {
        return Ok(());
    };
    let (number, connect_command) = match ob::imessage_connect_info(base_url, api_key) {
        Ok(info) => info,
        Err(err) => {
            eprintln!(
                "  {} {}",
                crate::t(
                    "cli-quickstart-inkbox-imsg-router-failed",
                    "could not fetch the iMessage router:",
                ),
                err,
            );
            return Ok(());
        }
    };
    println!(
        "  {}",
        crate::t(
            "cli-quickstart-inkbox-imsg-steps-intro",
            "From your iPhone, in the Messages app:",
        )
    );
    println!(
        "    {} \"{}\" {} {}",
        crate::t("cli-quickstart-inkbox-imsg-step1", "1. Text"),
        connect_command,
        crate::t("cli-quickstart-inkbox-imsg-step1b", "to"),
        number,
    );
    println!(
        "    {}",
        crate::t(
            "cli-quickstart-inkbox-imsg-step2",
            "2. Inkbox texts you back from the number now assigned to this agent.",
        )
    );
    println!(
        "    {}",
        crate::t(
            "cli-quickstart-inkbox-imsg-step3",
            "3. Send any first message (e.g. \"hi\") in that NEW thread.",
        )
    );
    println!(
        "  {}",
        crate::t(
            "cli-quickstart-inkbox-imsg-only-after",
            "The agent can only message you after you message it first.",
        )
    );
    println!(
        "\n  {}",
        crate::t(
            "cli-quickstart-inkbox-imsg-waiting-header",
            "--- Waiting for your first iMessage ---",
        )
    );
    println!(
        "  {}",
        crate::t(
            "cli-quickstart-inkbox-imsg-polling",
            "Polling every 3s for an inbound iMessage to this agent.",
        )
    );
    // The poll returns "<conversation_id>|<sender>" so we can greet back.
    let found = poll_with_spinner(
        &crate::t(
            "cli-quickstart-inkbox-imsg-listening",
            "Listening for your first iMessage...",
        ),
        || {
            ob::check_first_imessage(base_url, api_key, handle)
                .ok()
                .flatten()
                .map(|(cid, sender)| format!("{cid}|{sender}"))
        },
    );
    let Some(found) = found else {
        println!(
            "  {}",
            crate::t(
                "cli-quickstart-inkbox-imsg-later",
                "Skipped. The agent replies over iMessage once you connect and message it.",
            )
        );
        return Ok(());
    };
    let (cid_str, sender) = found.split_once('|').unwrap_or((found.as_str(), ""));
    println!(
        "  {} {}.",
        crate::t(
            "cli-quickstart-inkbox-imsg-connected",
            "✓ Got it. First iMessage received from",
        ),
        sender,
    );
    if let Ok(cid) = cid_str.parse() {
        let welcome = crate::t(
            "cli-quickstart-inkbox-imsg-welcome",
            "You're connected! This is your iMessage channel to your ZeroClaw agent. Anything you send here goes straight to the agent, and its replies show up right in this thread.",
        );
        match ob::send_imessage_welcome(base_url, api_key, handle, cid, &welcome) {
            Ok(()) => println!(
                "  {}",
                crate::t(
                    "cli-quickstart-inkbox-imsg-welcome-sent",
                    "✓ Sent a welcome message back on that thread.",
                )
            ),
            Err(err) => eprintln!(
                "  {} {}",
                crate::t(
                    "cli-quickstart-inkbox-imsg-welcome-failed",
                    "could not send the welcome message:",
                ),
                err,
            ),
        }
    }
    Ok(())
}

/// Poll `check` every ~3s for up to [`POLL_SECS`], animating a spinner. Returns
/// the first `Some(detail)` from `check`, or `None` on timeout.
fn poll_with_spinner<F>(label: &str, mut check: F) -> Option<String>
where
    F: FnMut() -> Option<String>,
{
    use std::io::Write;
    let spinner = ['|', '/', '-', '\\'];
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(POLL_SECS);
    let mut next_poll = std::time::Instant::now();
    let mut tick = 0usize;
    let clear = "\r                                                              \r";
    loop {
        if std::time::Instant::now() >= deadline {
            print!("{clear}");
            let _ = std::io::stdout().flush();
            return None;
        }
        if std::time::Instant::now() >= next_poll {
            if let Some(found) = check() {
                print!("{clear}");
                let _ = std::io::stdout().flush();
                return Some(found);
            }
            next_poll = std::time::Instant::now() + std::time::Duration::from_secs(3);
        }
        print!("\r  {} {}", spinner[tick % spinner.len()], label);
        let _ = std::io::stdout().flush();
        tick += 1;
        std::thread::sleep(std::time::Duration::from_millis(300));
    }
}

// ── dialoguer helpers (Ctrl+C → `None`, mirroring `prompt_for_field`) ──

fn confirm(prompt: &str, default: bool) -> anyhow::Result<Option<bool>> {
    Ok(dialoguer::Confirm::new()
        .with_prompt(prompt)
        .default(default)
        .interact_opt()?)
}

fn input(prompt: &str, default: Option<&str>, allow_empty: bool) -> anyhow::Result<Option<String>> {
    let mut builder = dialoguer::Input::<String>::new()
        .with_prompt(prompt)
        .allow_empty(allow_empty);
    if let Some(d) = default {
        builder = builder.default(d.to_string());
    }
    match builder.interact_text() {
        Ok(v) => Ok(Some(v)),
        Err(e) => map_interrupt(e),
    }
}

fn password(prompt: &str) -> anyhow::Result<Option<String>> {
    match dialoguer::Password::new()
        .with_prompt(prompt)
        .allow_empty_password(true)
        .interact()
    {
        Ok(v) => Ok(Some(v)),
        Err(e) => map_interrupt(e),
    }
}

/// Map a dialoguer Ctrl+C interrupt to `Ok(None)` ("backed out"); bubble any
/// other IO error.
fn map_interrupt(e: dialoguer::Error) -> anyhow::Result<Option<String>> {
    let io: std::io::Error = e.into();
    if io.kind() == std::io::ErrorKind::Interrupted {
        Ok(None)
    } else {
        Err(io.into())
    }
}
