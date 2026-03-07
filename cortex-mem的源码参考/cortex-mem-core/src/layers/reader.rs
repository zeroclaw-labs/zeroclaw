use crate::{CortexFilesystem, FilesystemOperations, Result};
use std::sync::Arc;
use std::collections::HashMap;
use serde::{Deserialize, Serialize};

/// 层级内容包
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LayerBundle {
    pub abstract_text: Option<String>,
    pub overview: Option<String>,
    pub content: Option<String>,
}

/// 层级读取器
/// 
/// 提供并发读取 L0/L1/L2 层级的高性能接口
/// 
/// **注意**: 虽然本地文件系统对并发不敏感，但此组件为未来网络/分布式扩展预留
pub struct LayerReader {
    filesystem: Arc<CortexFilesystem>,
}

impl LayerReader {
    pub fn new(filesystem: Arc<CortexFilesystem>) -> Self {
        Self { filesystem }
    }
    
    /// 并发读取所有层级
    /// 
    /// 为多个 URI 同时读取 L0/L1/L2 层级
    /// 
    /// **性能说明**: 本地文件系统下并发收益有限，但为分布式场景预留
    pub async fn read_all_layers_concurrent(
        &self,
        uris: &[String],
    ) -> Result<HashMap<String, LayerBundle>> {
        use futures::future::join_all;
        
        let tasks: Vec<_> = uris.iter().map(|uri| {
            let uri = uri.clone();
            let filesystem = self.filesystem.clone();
            
            async move {
                let (l0, l1, l2) = tokio::join!(
                    Self::read_abstract_static(&filesystem, &uri),
                    Self::read_overview_static(&filesystem, &uri),
                    filesystem.read(&uri),
                );
                
                (uri, LayerBundle {
                    abstract_text: l0.ok(),
                    overview: l1.ok(),
                    content: l2.ok(),
                })
            }
        }).collect();
        
        let results: Vec<(String, LayerBundle)> = join_all(tasks).await;
        Ok(results.into_iter().collect())
    }
    
    /// 并发读取单个 URI 的所有层级
    pub async fn read_layers(&self, uri: &str) -> Result<LayerBundle> {
        let (l0, l1, l2) = tokio::join!(
            Self::read_abstract_static(&self.filesystem, uri),
            Self::read_overview_static(&self.filesystem, uri),
            self.filesystem.read(uri),
        );
        
        Ok(LayerBundle {
            abstract_text: l0.ok(),
            overview: l1.ok(),
            content: l2.ok(),
        })
    }
    
    /// 静态方法：读取 L0 抽象
    async fn read_abstract_static(filesystem: &Arc<CortexFilesystem>, uri: &str) -> Result<String> {
        let abstract_uri = Self::get_abstract_uri(uri);
        filesystem.read(&abstract_uri).await
    }
    
    /// 静态方法：读取 L1 概览
    async fn read_overview_static(filesystem: &Arc<CortexFilesystem>, uri: &str) -> Result<String> {
        let overview_uri = Self::get_overview_uri(uri);
        filesystem.read(&overview_uri).await
    }
    
    /// 获取 abstract URI
    fn get_abstract_uri(base_uri: &str) -> String {
        let dir = base_uri.rsplit_once('/').map(|(dir, _)| dir).unwrap_or(base_uri);
        format!("{}/.abstract.md", dir)
    }
    
    /// 获取 overview URI
    fn get_overview_uri(base_uri: &str) -> String {
        let dir = base_uri.rsplit_once('/').map(|(dir, _)| dir).unwrap_or(base_uri);
        format!("{}/.overview.md", dir)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_get_abstract_uri() {
        assert_eq!(
            LayerReader::get_abstract_uri("cortex://user/memories/pref_0.md"),
            "cortex://user/memories/.abstract.md"
        );
        
        assert_eq!(
            LayerReader::get_abstract_uri("cortex://session/abc/timeline/2024-01-01/msg_0.md"),
            "cortex://session/abc/timeline/2024-01-01/.abstract.md"
        );
    }
    
    #[test]
    fn test_get_overview_uri() {
        assert_eq!(
            LayerReader::get_overview_uri("cortex://agent/cases/case_0.md"),
            "cortex://agent/cases/.overview.md"
        );
    }
}
