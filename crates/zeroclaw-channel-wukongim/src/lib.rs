//! WuKongIM channel implementation for ZeroClaw.
//!
//! 模块结构按职责域划分：
//! - [`connection`] — WebSocket 连接与通信（JSON-RPC 2.0 协议）
//! - [`messaging`] — 消息收发与媒体处理
//! - [`filter`]    — 权限校验与消息过滤
//! - [`approval`]  — 工具调用审批流程
//! - [`config`]    — 配置构造

pub mod approval;
pub mod channel;
pub mod config;
pub mod connection;
pub mod filter;
pub mod messaging;

pub use channel::WuKongIMChannel;
