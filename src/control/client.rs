use anyhow::Result;
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::{Duration, Instant};

pub struct ControlClient {
    base_url: String,
    bot_id: String,
    bot_name: String,
    http: reqwest::Client,
    started_at: Instant,
}

impl ControlClient {
    pub fn new(control_url: &str, bot_id: &str, bot_name: &str) -> Self {
        Self {
            base_url: control_url.trim_end_matches('/').to_string(),
            bot_id: bot_id.to_string(),
            bot_name: bot_name.to_string(),
            http: reqwest::Client::new(),
            started_at: Instant::now(),
        }
    }

    pub async fn heartbeat(&self, extra: &Value) -> Result<Vec<Value>> {
        let mut body = json!({
            "bot_id": self.bot_id,
            "name": self.bot_name,
            "uptime_secs": self.started_at.elapsed().as_secs(),
        });
        if let Some(obj) = extra.as_object() {
            for (k, v) in obj {
                body[k.clone()] = v.clone();
            }
        }
        let resp = self
            .http
            .post(format!("{}/api/control/heartbeat", self.base_url))
            .json(&body)
            .timeout(Duration::from_secs(10))
            .send()
            .await?;
        let data: Value = resp.json().await?;
        let cmds = data
            .get("pending_commands")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        Ok(cmds)
    }

    pub async fn ack_command(
        &self,
        command_id: &str,
        status: &str,
        result: Option<&str>,
    ) -> Result<()> {
        let mut body = json!({
            "command_id": command_id,
            "status": status,
        });
        if let Some(r) = result {
            body["result"] = json!(r);
        }
        self.http
            .post(format!("{}/api/control/commands/ack", self.base_url))
            .json(&body)
            .timeout(Duration::from_secs(10))
            .send()
            .await?;
        Ok(())
    }

    pub fn bot_id(&self) -> &str {
        &self.bot_id
    }
}

pub async fn run_heartbeat_loop(
    client: Arc<ControlClient>,
    interval_secs: u64,
    extra: Value,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) {
    let mut tick = tokio::time::interval(Duration::from_secs(interval_secs));
    loop {
        tokio::select! {
            _ = tick.tick() => {
                match client.heartbeat(&extra).await {
                    Ok(cmds) => {
                        for cmd in cmds {
                            let cmd_id = cmd.get("id").and_then(|v| v.as_str()).unwrap_or("");
                            let kind = cmd.get("kind").and_then(|v| v.as_str()).unwrap_or("");
                            tracing::info!(command_id = cmd_id, kind, "received pending command");
                            let result = execute_command(&cmd).await;
                            let (status, result_str) = match result {
                                Ok(r) => ("acked", r),
                                Err(e) => ("failed", e.to_string()),
                            };
                            if let Err(e) = client.ack_command(cmd_id, status, Some(&result_str)).await {
                                tracing::warn!(command_id = cmd_id, error = %e, "failed to ack command");
                            }
                        }
                    }
                    Err(e) => {
                        tracing::debug!(error = %e, "heartbeat failed");
                    }
                }
            }
            _ = shutdown.changed() => {
                tracing::info!("heartbeat loop shutting down");
                break;
            }
        }
    }
}

#[allow(clippy::unused_async)]
async fn execute_command(cmd: &Value) -> Result<String> {
    let kind = cmd.get("kind").and_then(|v| v.as_str()).unwrap_or("");
    let payload = cmd.get("payload").and_then(|v| v.as_str()).unwrap_or("{}");
    match kind {
        "reload_config" => {
            tracing::info!("reloading config");
            Ok("config reloaded".to_string())
        }
        "restart" => {
            tracing::info!("restart requested");
            Ok("restart scheduled".to_string())
        }
        "stop" => {
            tracing::info!("stop requested");
            Ok("stop acknowledged".to_string())
        }
        "run_agent" => {
            tracing::info!(payload, "run agent requested");
            Ok(format!("agent run queued: {payload}"))
        }
        "shell" => {
            tracing::info!("shell command denied by security policy");
            Ok("shell execution denied".to_string())
        }
        _ => Ok(format!("unknown command kind: {kind}")),
    }
}
