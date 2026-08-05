//! Peripheral trait — hardware boards (STM32, RPi GPIO) that expose tools.

use async_trait::async_trait;

use crate::tool::Tool;

#[async_trait]
pub trait Peripheral: Send + Sync {
    fn name(&self) -> &str;

    fn board_type(&self) -> &str;

    async fn connect(&mut self) -> anyhow::Result<()>;

    async fn disconnect(&mut self) -> anyhow::Result<()>;

    async fn health_check(&self) -> bool;

    fn tools(&self) -> Vec<Box<dyn Tool>>;
}
