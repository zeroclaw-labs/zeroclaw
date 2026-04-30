//! Rocket.Chat integration debugger.
//!
//! Tests every API call the channel makes against a real RC server.
//!
//! Usage (from repo root):
//!   cargo run -p zeroclaw-channels --example rocketchat_debug
//!
//! Required env vars:
//!   RocketChat_URL      e.g. https://chat.example.com
//!   RocketChat_USER_ID  bot user ID (from Admin > Users)
//!   RocketChat_TOKEN    bot auth token (from Admin > Users > Personal Access Tokens)
//!
//! Optional env vars:
//!   RC_ROOMS    comma-separated room names/IDs to probe   (default: general)
//!   RC_USERS    comma-separated usernames to probe DMs for (default: empty)
//!   RC_SEND_TO  recipient (room_id or room_id:thread_id) to send a test message

use reqwest::Client;
use serde_json::Value;
use std::env;

struct Rc {
    url: String,
    user_id: String,
    token: String,
    client: Client,
}

impl Rc {
    fn new(url: String, user_id: String, token: String) -> Self {
        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .unwrap();
        Self {
            url: url.trim_end_matches('/').to_string(),
            user_id,
            token,
            client,
        }
    }

    fn endpoint(&self, path: &str) -> String {
        format!("{}/api/v1/{}", self.url, path)
    }

    async fn get(&self, path: &str) -> Result<Value, String> {
        let resp = self
            .client
            .get(self.endpoint(path))
            .header("X-Auth-Token", &self.token)
            .header("X-User-Id", &self.user_id)
            .send()
            .await
            .map_err(|e| format!("request failed: {e}"))?;
        let status = resp.status();
        let body: Value = resp
            .json()
            .await
            .map_err(|e| format!("json parse failed: {e}"))?;
        if !status.is_success() {
            return Err(format!("HTTP {status}: {body}"));
        }
        Ok(body)
    }

    async fn post(&self, path: &str, payload: Value) -> Result<Value, String> {
        let resp = self
            .client
            .post(self.endpoint(path))
            .header("X-Auth-Token", &self.token)
            .header("X-User-Id", &self.user_id)
            .json(&payload)
            .send()
            .await
            .map_err(|e| format!("request failed: {e}"))?;
        let status = resp.status();
        let body: Value = resp
            .json()
            .await
            .map_err(|e| format!("json parse failed: {e}"))?;
        if !status.is_success() {
            return Err(format!("HTTP {status}: {body}"));
        }
        Ok(body)
    }
}

fn ok(label: &str, detail: &str) {
    println!("  ✅  {label}  {detail}");
}
fn fail(label: &str, detail: &str) {
    println!("  ❌  {label}  {detail}");
}
fn info(msg: &str) {
    println!("      {msg}");
}
fn section(title: &str) {
    println!("\n── {title} ──────────────────────────────────────");
}

#[tokio::main]
async fn main() {
    let url = env::var("RC_URL")
        .or_else(|_| env::var("RocketChat_URL"))
        .unwrap_or_else(|_| {
            eprintln!("ERROR: RocketChat_URL not set");
            std::process::exit(1);
        });
    let user_id = env::var("RC_USER_ID")
        .or_else(|_| env::var("RocketChat_USER_ID"))
        .unwrap_or_else(|_| {
            eprintln!("ERROR: RocketChat_USER_ID not set");
            std::process::exit(1);
        });
    let token = env::var("RC_TOKEN")
        .or_else(|_| env::var("RocketChat_TOKEN"))
        .unwrap_or_else(|_| {
            eprintln!("ERROR: RocketChat_TOKEN not set");
            std::process::exit(1);
        });
    let rooms_raw = env::var("RC_ROOMS").unwrap_or_else(|_| "general".to_string());
    let rooms: Vec<&str> = rooms_raw
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    let users_raw = env::var("RC_USERS").unwrap_or_default();
    let dm_users: Vec<&str> = users_raw
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    let send_to = env::var("RC_SEND_TO").ok();

    println!("╔══════════════════════════════════════════════╗");
    println!("║      Rocket.Chat Channel Debug Tool          ║");
    println!("╚══════════════════════════════════════════════╝");
    println!("  Server : {url}");
    println!("  User ID: {user_id}");
    println!("  Rooms  : {rooms:?}");
    println!("  DM users: {dm_users:?}");

    let rc = Rc::new(url, user_id.clone(), token);

    // ── 1. Auth ──────────────────────────────────────────────────────────────
    section("1. Authentication");
    match rc.get("me").await {
        Ok(v) => {
            let bot_id = v.get("_id").and_then(|i| i.as_str()).unwrap_or("?");
            let bot_name = v.get("username").and_then(|u| u.as_str()).unwrap_or("?");
            ok("GET /me", &format!("id={bot_id}  username={bot_name}"));
            if bot_id != user_id {
                fail(
                    "user_id mismatch",
                    &format!("config={user_id}  server={bot_id}"),
                );
            }
        }
        Err(e) => {
            fail("GET /me", &e);
            println!(
                "\n  → Auth failed — check RocketChat_USER_ID and RocketChat_TOKEN, then rerun."
            );
            std::process::exit(1);
        }
    }

    // ── 2. Room probe ────────────────────────────────────────────────────────
    section("2. Room resolution  (allowed_rooms)");
    let mut resolved_rooms: Vec<(String, &'static str, String)> = Vec::new(); // (id, endpoint, original)

    for room in &rooms {
        println!("\n  Probing: \"{room}\"");

        // by roomId
        for ep in ["channels", "groups"] {
            let path = format!("{ep}.info?roomId={room}");
            match rc.get(&path).await {
                Ok(v) if v.get("success").and_then(|s| s.as_bool()).unwrap_or(false) => {
                    let key = if ep == "channels" { "channel" } else { "group" };
                    let id = v
                        .get(key)
                        .and_then(|c| c.get("_id"))
                        .and_then(|i| i.as_str())
                        .unwrap_or(*room);
                    let name = v
                        .get(key)
                        .and_then(|c| c.get("name"))
                        .and_then(|n| n.as_str())
                        .unwrap_or("?");
                    ok(
                        &format!("{ep}.info?roomId="),
                        &format!("→ id={id}  name={name}"),
                    );
                    resolved_rooms.push((id.to_string(), ep, room.to_string()));
                    continue;
                }
                Ok(v) => info(&format!(
                    "{ep}.info?roomId= → success=false  {}",
                    v.get("error").and_then(|e| e.as_str()).unwrap_or("")
                )),
                Err(e) => info(&format!("{ep}.info?roomId= → {e}")),
            }
        }

        // by roomName
        for ep in ["channels", "groups"] {
            let path = format!("{ep}.info?roomName={room}");
            match rc.get(&path).await {
                Ok(v) if v.get("success").and_then(|s| s.as_bool()).unwrap_or(false) => {
                    let key = if ep == "channels" { "channel" } else { "group" };
                    let id = v
                        .get(key)
                        .and_then(|c| c.get("_id"))
                        .and_then(|i| i.as_str())
                        .unwrap_or(*room);
                    let name = v
                        .get(key)
                        .and_then(|c| c.get("name"))
                        .and_then(|n| n.as_str())
                        .unwrap_or("?");
                    ok(
                        &format!("{ep}.info?roomName="),
                        &format!("→ id={id}  name={name}"),
                    );
                    if resolved_rooms.iter().all(|(id2, _, _)| id2 != id) {
                        resolved_rooms.push((id.to_string(), ep, room.to_string()));
                    }
                }
                Ok(v) => info(&format!(
                    "{ep}.info?roomName= → success=false  {}",
                    v.get("error").and_then(|e| e.as_str()).unwrap_or("")
                )),
                Err(e) => info(&format!("{ep}.info?roomName= → {e}")),
            }
        }

        // as DM room ID
        let path = format!("im.info?roomId={room}");
        match rc.get(&path).await {
            Ok(v) if v.get("success").and_then(|s| s.as_bool()).unwrap_or(false) => {
                let id = v
                    .get("room")
                    .and_then(|r| r.get("_id"))
                    .and_then(|i| i.as_str())
                    .unwrap_or(*room);
                ok("im.info?roomId=", &format!("→ DM room id={id}"));
                if resolved_rooms.iter().all(|(id2, _, _)| id2 != id) {
                    resolved_rooms.push((id.to_string(), "im", room.to_string()));
                }
            }
            Ok(v) => info(&format!(
                "im.info?roomId= → success=false  {}",
                v.get("error").and_then(|e| e.as_str()).unwrap_or("")
            )),
            Err(e) => info(&format!("im.info?roomId= → {e}")),
        }

        if resolved_rooms.iter().all(|(_, _, orig)| orig != room) {
            fail(
                "NOT RESOLVED",
                &format!("room '{room}' not found by any method"),
            );
        }
    }

    // ── 3. DM discovery ──────────────────────────────────────────────────────
    section("3. DM discovery  (dm_replies + allowed_users)");
    if dm_users.is_empty() {
        info("RC_USERS not set — skipping DM probes");
        info("Set RC_USERS=alice,bob to test DM resolution");
    } else {
        // Test im.list first
        println!("\n  Testing im.list (used when allowed_users = [\"*\"]):");
        match rc.get("im.list").await {
            Ok(v) => {
                let count = v
                    .get("ims")
                    .and_then(|i| i.as_array())
                    .map(|a| a.len())
                    .unwrap_or(0);
                ok("im.list", &format!("→ {count} subscribed DMs"));
                if count == 0 {
                    info("No DMs yet — bot may not have chatted with anyone");
                }
            }
            Err(e) => fail("im.list", &e),
        }

        // Per-user DM probe
        for username in &dm_users {
            println!("\n  DM probe for user: \"{username}\"");

            // Step 1: users.info
            let path = format!("users.info?username={username}");
            let uid = match rc.get(&path).await {
                Ok(v) if v.get("success").and_then(|s| s.as_bool()).unwrap_or(false) => {
                    let uid = v
                        .get("user")
                        .and_then(|u| u.get("_id"))
                        .and_then(|i| i.as_str())
                        .unwrap_or("?")
                        .to_string();
                    ok("users.info", &format!("→ userId={uid}"));
                    uid
                }
                Ok(v) => {
                    fail(
                        "users.info",
                        &format!(
                            "success=false  {}",
                            v.get("error").and_then(|e| e.as_str()).unwrap_or("")
                        ),
                    );
                    continue;
                }
                Err(e) => {
                    fail("users.info", &e);
                    continue;
                }
            };

            // Step 2: im.info?userId=
            let path = format!("im.info?userId={uid}");
            match rc.get(&path).await {
                Ok(v) if v.get("success").and_then(|s| s.as_bool()).unwrap_or(false) => {
                    let room_id = v
                        .get("room")
                        .and_then(|r| r.get("_id"))
                        .and_then(|i| i.as_str())
                        .unwrap_or("?");
                    ok("im.info?userId=", &format!("→ DM roomId={room_id}"));
                    resolved_rooms.push((room_id.to_string(), "im", format!("dm:{username}")));
                }
                Ok(v) => {
                    let err_msg = v.get("error").and_then(|e| e.as_str()).unwrap_or("unknown");
                    fail("im.info?userId=", &format!("success=false: {err_msg}"));
                    info("→ Trying im.create as fallback (creates DM if needed)...");

                    // Fallback: im.create (RC >= 0.50)
                    match rc
                        .post("im.create", serde_json::json!({"username": username}))
                        .await
                    {
                        Ok(v) => {
                            let room_id = v
                                .get("room")
                                .and_then(|r| r.get("_id"))
                                .and_then(|i| i.as_str())
                                .unwrap_or("?");
                            ok("im.create", &format!("→ DM roomId={room_id}"));
                            resolved_rooms.push((
                                room_id.to_string(),
                                "im",
                                format!("dm:{username}"),
                            ));
                        }
                        Err(e) => fail("im.create", &e),
                    }
                }
                Err(e) => {
                    fail("im.info?userId=", &e);
                    info("→ Trying im.create as fallback...");
                    match rc
                        .post("im.create", serde_json::json!({"username": username}))
                        .await
                    {
                        Ok(v) => {
                            let room_id = v
                                .get("room")
                                .and_then(|r| r.get("_id"))
                                .and_then(|i| i.as_str())
                                .unwrap_or("?");
                            ok("im.create", &format!("→ DM roomId={room_id}"));
                            resolved_rooms.push((
                                room_id.to_string(),
                                "im",
                                format!("dm:{username}"),
                            ));
                        }
                        Err(e2) => fail("im.create", &e2),
                    }
                }
            }
        }
    }

    // ── 4. History fetch ─────────────────────────────────────────────────────
    section("4. History fetch  (polling test)");
    let oldest = "2020-01-01T00:00:00.000Z";
    for (room_id, endpoint, orig) in &resolved_rooms {
        let path =
            format!("{endpoint}.history?roomId={room_id}&oldest={oldest}&count=5&inclusive=false");
        match rc.get(&path).await {
            Ok(v) => {
                let count = v
                    .get("messages")
                    .and_then(|m| m.as_array())
                    .map(|a| a.len())
                    .unwrap_or(0);
                ok(
                    &format!("{endpoint}.history  ({orig})"),
                    &format!("→ {count} messages"),
                );
            }
            Err(e) => fail(&format!("{endpoint}.history  ({orig})"), &e),
        }
    }

    // ── 5. Send test ─────────────────────────────────────────────────────────
    section("5. Send message  (optional)");
    match &send_to {
        None => info("RC_SEND_TO not set — skipping send test"),
        Some(recipient) => {
            let (room_id, tmid) = recipient
                .split_once(':')
                .map(|(r, t)| (r, Some(t)))
                .unwrap_or((recipient.as_str(), None));
            let mut msg =
                serde_json::json!({"rid": room_id, "msg": "[zeroclaw debug] connection test ✅"});
            if let Some(t) = tmid {
                msg.as_object_mut().unwrap().insert("tmid".into(), t.into());
            }
            match rc
                .post("chat.sendMessage", serde_json::json!({"message": msg}))
                .await
            {
                Ok(_) => ok("chat.sendMessage", &format!("→ sent to {recipient}")),
                Err(e) => fail("chat.sendMessage", &e),
            }
        }
    }

    // ── 6. Typing indicator ──────────────────────────────────────────────────
    section("6. Typing indicator  (RC >= 5.0)");
    if let Some((room_id, _, _)) = resolved_rooms.first() {
        match rc
            .post(
                "chat.typing",
                serde_json::json!({"roomId": room_id, "status": true}),
            )
            .await
        {
            Ok(_) => ok("chat.typing", "supported"),
            Err(e) => {
                fail("chat.typing", &e);
                info("→ Typing indicator requires Rocket.Chat >= 5.0");
                info("   Set typing_indicator = false in config to suppress this error");
            }
        }
    } else {
        info("No resolved rooms — skipping typing test");
    }

    // ── Summary ──────────────────────────────────────────────────────────────
    println!("\n╔══════════════════════════════════════════════╗");
    println!("║  Resolved rooms ready for polling:           ║");
    println!("╚══════════════════════════════════════════════╝");
    if resolved_rooms.is_empty() {
        println!("  ❌  NONE — bot will fail to start (no valid rooms)");
    } else {
        for (id, ep, orig) in &resolved_rooms {
            println!("  ✅  [{ep}]  id={id}  (from \"{orig}\")");
        }
    }
    println!();
}
