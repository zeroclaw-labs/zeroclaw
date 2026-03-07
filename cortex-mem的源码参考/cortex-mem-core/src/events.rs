use crate::Result;
use tokio::sync::mpsc;
use std::fmt;

/// 会话生命周期事件
#[derive(Debug, Clone)]
pub enum SessionEvent {
    /// 会话创建
    Created { session_id: String },
    
    /// 消息添加
    MessageAdded { 
        session_id: String, 
        message_id: String 
    },
    
    /// 会话关闭
    Closed { session_id: String },
}

/// 文件系统事件
#[derive(Debug, Clone)]
pub enum FilesystemEvent {
    /// 文件创建
    FileCreated { uri: String },
    
    /// 文件修改
    FileModified { uri: String },
    
    /// 文件删除
    FileDeleted { uri: String },
}

/// 统一事件枚举
#[derive(Debug, Clone)]
pub enum CortexEvent {
    Session(SessionEvent),
    Filesystem(FilesystemEvent),
}

impl fmt::Display for CortexEvent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CortexEvent::Session(event) => match event {
                SessionEvent::Created { session_id } => {
                    write!(f, "SessionCreated({})", session_id)
                }
                SessionEvent::MessageAdded { session_id, message_id } => {
                    write!(f, "MessageAdded({}, {})", session_id, message_id)
                }
                SessionEvent::Closed { session_id } => {
                    write!(f, "SessionClosed({})", session_id)
                }
            },
            CortexEvent::Filesystem(event) => match event {
                FilesystemEvent::FileCreated { uri } => {
                    write!(f, "FileCreated({})", uri)
                }
                FilesystemEvent::FileModified { uri } => {
                    write!(f, "FileModified({})", uri)
                }
                FilesystemEvent::FileDeleted { uri } => {
                    write!(f, "FileDeleted({})", uri)
                }
            },
        }
    }
}

/// 事件总线 - 基于mpsc的简单实现
#[derive(Clone)]
pub struct EventBus {
    tx: mpsc::UnboundedSender<CortexEvent>,
}

impl EventBus {
    /// 创建新的事件总线（返回发送端和接收端）
    pub fn new() -> (Self, mpsc::UnboundedReceiver<CortexEvent>) {
        let (tx, rx) = mpsc::unbounded_channel();
        (Self { tx }, rx)
    }
    
    /// 发布事件
    pub fn publish(&self, event: CortexEvent) -> Result<()> {
        self.tx
            .send(event)
            .map_err(|e| crate::Error::Other(format!("Failed to publish event: {}", e)))
    }
}

impl Default for EventBus {
    fn default() -> Self {
        let (bus, _) = Self::new();
        bus
    }
}
