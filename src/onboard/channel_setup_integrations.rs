use anyhow::Result;
use console::style;
use dialoguer::Input;

use crate::config::schema::WhatsAppConfig;
use crate::config::{ChannelsConfig, MatrixConfig, WebhookConfig};
use crate::onboard::common::print_bullet;

pub(crate) fn setup_matrix_channel(config: &mut ChannelsConfig) -> Result<()> {
    println!();
    println!(
        "  {} {}",
        style("Matrix Setup").white().bold(),
        style("— self-hosted, federated chat").dim()
    );
    print_bullet("You need a Matrix account and an access token.");
    print_bullet("Get a token via Element → Settings → Help & About → Access Token.");
    println!();

    let homeserver: String = Input::new()
        .with_prompt("  Homeserver URL (e.g. https://matrix.org)")
        .interact_text()?;

    if homeserver.trim().is_empty() {
        println!("  {} Skipped", style("→").dim());
        return Ok(());
    }

    let access_token: String = Input::new().with_prompt("  Access token").interact_text()?;

    if access_token.trim().is_empty() {
        println!("  {} Skipped — token required", style("→").dim());
        return Ok(());
    }

    let hs = homeserver.trim_end_matches('/');
    print!("  {} Testing connection... ", style("⏳").dim());
    let client = reqwest::blocking::Client::new();
    match client
        .get(format!("{hs}/_matrix/client/v3/account/whoami"))
        .header("Authorization", format!("Bearer {access_token}"))
        .send()
    {
        Ok(resp) if resp.status().is_success() => {
            let data: serde_json::Value = resp.json().unwrap_or_default();
            let user_id = data
                .get("user_id")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("unknown");
            println!(
                "\r  {} Connected as {user_id}        ",
                style("✅").green().bold()
            );
        }
        _ => {
            println!(
                "\r  {} Connection failed — check homeserver URL and token",
                style("❌").red().bold()
            );
            return Ok(());
        }
    }

    let room_id: String = Input::new()
        .with_prompt("  Room ID (e.g. !abc123:matrix.org)")
        .interact_text()?;

    let users_str: String = Input::new()
        .with_prompt("  Allowed users (comma-separated @user:server, or * for all)")
        .default("*".into())
        .interact_text()?;

    let allowed_users = if users_str.trim() == "*" {
        vec!["*".into()]
    } else {
        users_str.split(',').map(|s| s.trim().to_string()).collect()
    };

    config.matrix = Some(MatrixConfig {
        homeserver: homeserver.trim_end_matches('/').to_string(),
        access_token: access_token.trim().to_string(),
        room_id,
        allowed_users,
    });

    Ok(())
}

pub(crate) fn setup_whatsapp_channel(config: &mut ChannelsConfig) -> Result<()> {
    println!();
    println!(
        "  {} {}",
        style("WhatsApp Setup").white().bold(),
        style("— Business Cloud API").dim()
    );
    print_bullet("1. Go to developers.facebook.com and create a WhatsApp app");
    print_bullet("2. Add the WhatsApp product and get your phone number ID");
    print_bullet("3. Generate a temporary access token (System User)");
    print_bullet("4. Configure webhook URL to: https://your-domain/whatsapp");
    println!();

    let access_token: String = Input::new()
        .with_prompt("  Access token (from Meta Developers)")
        .interact_text()?;

    if access_token.trim().is_empty() {
        println!("  {} Skipped", style("→").dim());
        return Ok(());
    }

    let phone_number_id: String = Input::new()
        .with_prompt("  Phone number ID (from WhatsApp app settings)")
        .interact_text()?;

    if phone_number_id.trim().is_empty() {
        println!("  {} Skipped — phone number ID required", style("→").dim());
        return Ok(());
    }

    let verify_token: String = Input::new()
        .with_prompt("  Webhook verify token (create your own)")
        .default("zeroclaw-whatsapp-verify".into())
        .interact_text()?;

    print!("  {} Testing connection... ", style("⏳").dim());
    let client = reqwest::blocking::Client::new();
    let url = format!(
        "https://graph.facebook.com/v18.0/{}",
        phone_number_id.trim()
    );
    match client
        .get(&url)
        .header("Authorization", format!("Bearer {}", access_token.trim()))
        .send()
    {
        Ok(resp) if resp.status().is_success() => {
            println!(
                "\r  {} Connected to WhatsApp API        ",
                style("✅").green().bold()
            );
        }
        _ => {
            println!(
                "\r  {} Connection failed — check access token and phone number ID",
                style("❌").red().bold()
            );
            return Ok(());
        }
    }

    let users_str: String = Input::new()
        .with_prompt("  Allowed phone numbers (comma-separated +1234567890, or * for all)")
        .default("*".into())
        .interact_text()?;

    let allowed_numbers = if users_str.trim() == "*" {
        vec!["*".into()]
    } else {
        users_str.split(',').map(|s| s.trim().to_string()).collect()
    };

    config.whatsapp = Some(WhatsAppConfig {
        access_token: access_token.trim().to_string(),
        phone_number_id: phone_number_id.trim().to_string(),
        verify_token: verify_token.trim().to_string(),
        allowed_numbers,
    });

    Ok(())
}

pub(crate) fn setup_webhook_channel(config: &mut ChannelsConfig) -> Result<()> {
    println!();
    println!(
        "  {} {}",
        style("Webhook Setup").white().bold(),
        style("— HTTP endpoint for custom integrations").dim()
    );

    let port: String = Input::new()
        .with_prompt("  Port")
        .default("8080".into())
        .interact_text()?;

    let secret: String = Input::new()
        .with_prompt("  Secret (optional, Enter to skip)")
        .allow_empty(true)
        .interact_text()?;

    config.webhook = Some(WebhookConfig {
        port: port.parse().unwrap_or(8080),
        secret: if secret.is_empty() {
            None
        } else {
            Some(secret)
        },
    });
    println!(
        "  {} Webhook on port {}",
        style("✅").green().bold(),
        style(&port).cyan()
    );

    Ok(())
}
