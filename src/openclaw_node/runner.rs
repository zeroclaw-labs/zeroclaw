/// OpenClaw node runner — orchestrates connection and message delegation to ZeroClaw agent
use anyhow::{anyhow, Result};
use std::path::Path;

use super::client::{NodeInvokeResult, NodeMessageHandler, OpenClawClient};
use super::identity::DeviceIdentity;
use super::protocol::NodeInvokeRequest;
use crate::config::Config;

pub struct OpenClawNodeRunner {
    config: Config,
}

impl OpenClawNodeRunner {
    pub fn new(config: Config) -> Self {
        OpenClawNodeRunner { config }
    }

    pub async fn run(&self) -> Result<()> {
        let openclaw_config = self
            .config
            .openclaw_node
            .as_ref()
            .ok_or(anyhow!("openclaw_node not configured"))?;

        if !openclaw_config.enabled {
            return Err(anyhow!("openclaw_node is disabled"));
        }

        let gateway_url = openclaw_config
            .gateway_url
            .as_ref()
            .ok_or(anyhow!("openclaw_node.gateway_url not configured"))?;

        let node_id = openclaw_config
            .node_id
            .clone()
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

        let display_name = openclaw_config
            .display_name
            .clone()
            .unwrap_or_else(|| hostname::get()
                .ok()
                .and_then(|h| h.into_string().ok())
                .unwrap_or_else(|| "zeroclaw-node".to_string()));

        let device_key_path = openclaw_config
            .device_key_path
            .as_deref()
            .map(Path::new)
            .unwrap_or_else(|| {
                let home = dirs::home_dir().unwrap_or_default();
                Box::leak(Box::new(home.join(".zeroclaw").join("openclaw-device-key")))
            });

        // Load or create device identity
        let device_identity = DeviceIdentity::load_or_create(device_key_path)?;

        // Create handler that delegates to agent
        let handler = Box::new(AgentDelegationHandler {
            config: self.config.clone(),
            node_id: node_id.clone(),
        });

        // Create and run client
        let mut client = OpenClawClient::new(
            gateway_url,
            node_id,
            display_name,
            device_identity,
            openclaw_config.gateway_token.clone(),
        );

        client.run(handler).await
    }
}

struct AgentDelegationHandler {
    config: Config,
    node_id: String,
}

impl NodeMessageHandler for AgentDelegationHandler {
    fn on_invoke(
        &self,
        req: NodeInvokeRequest,
    ) -> futures_util::future::BoxFuture<'static, NodeInvokeResult> {
        let _config = self.config.clone();
        let node_id = self.node_id.clone();

        Box::pin(async move {
            // For now, implement a simple echo/test response
            // TODO: Integrate with crate::agent::process_message()
            // This should parse the command and parameters, route to ZeroClaw's agent,
            // and return the result as JSON

            eprintln!(
                "node.invoke.request: id={}, command={}",
                req.id, req.command
            );

            // Placeholder: echo the params back
            let payload_json = req.params_json.unwrap_or_else(|| {
                serde_json::json!({
                    "status": "placeholder",
                    "message": "ZeroClaw agent integration pending"
                })
                .to_string()
            });

            NodeInvokeResult {
                id: req.id,
                node_id,
                ok: true,
                payload_json: Some(payload_json),
                error: None,
            }
        })
    }

    fn on_connected(&self) {
        eprintln!(
            "openclaw node connected: {} ({})",
            self.node_id,
            self.config
                .openclaw_node
                .as_ref()
                .and_then(|c| c.display_name.as_ref())
                .unwrap_or(&self.node_id)
        );
    }

    fn on_disconnected(&self) {
        eprintln!("openclaw node disconnected: {}", self.node_id);
    }
}
