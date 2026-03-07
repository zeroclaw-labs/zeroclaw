// MCP Tool Definitions

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

/// Get all MCP tool definitions
pub fn get_mcp_tool_definitions() -> Vec<ToolDefinition> {
    vec![
        // ==================== Tiered Access Tools ====================
        ToolDefinition {
            name: "abstract".to_string(),
            description: "获取内容的 L0 抽象摘要（~100 tokens），用于快速判断相关性".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "uri": {
                        "type": "string",
                        "description": "内容的 URI（如 cortex://session/{session_id}/...）"
                    }
                },
                "required": ["uri"]
            }),
        },
        ToolDefinition {
            name: "overview".to_string(),
            description: "获取内容的 L1 概览（~2000 tokens），包含核心信息和使用场景".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "uri": {
                        "type": "string",
                        "description": "内容的 URI"
                    }
                },
                "required": ["uri"]
            }),
        },
        ToolDefinition {
            name: "read".to_string(),
            description: "获取 L2 完整内容，仅在需要详细信息时使用".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "uri": {
                        "type": "string",
                        "description": "内容的 URI"
                    }
                },
                "required": ["uri"]
            }),
        },
        // ==================== Search Tools ====================
        ToolDefinition {
            name: "search".to_string(),
            description: "智能搜索记忆，支持关键词/向量/混合检索和递归搜索".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "搜索查询"
                    },
                    "engine": {
                        "type": "string",
                        "enum": ["keyword", "vector", "hybrid"],
                        "description": "检索引擎类型（keyword=关键词, vector=向量, hybrid=混合）",
                        "default": "keyword"
                    },
                    "recursive": {
                        "type": "boolean",
                        "description": "是否递归搜索子目录",
                        "default": true
                    },
                    "return_layers": {
                        "type": "array",
                        "items": {
                            "type": "string",
                            "enum": ["L0", "L1", "L2"]
                        },
                        "description": "返回哪些层级的内容（L0=摘要, L1=概览, L2=完整内容）",
                        "default": ["L0"]
                    },
                    "scope": {
                        "type": "string",
                        "description": "搜索范围 URI（如 cortex://session/{session_id}）",
                        "default": "cortex://session"
                    },
                    "limit": {
                        "type": "integer",
                        "description": "最大结果数",
                        "default": 10
                    }
                },
                "required": ["query"]
            }),
        },
        ToolDefinition {
            name: "find".to_string(),
            description: "快速查找内容，返回 L0 摘要".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "查找关键词"
                    },
                    "scope": {
                        "type": "string",
                        "description": "查找范围 URI"
                    },
                    "limit": {
                        "type": "integer",
                        "description": "最大结果数",
                        "default": 5
                    }
                },
                "required": ["query"]
            }),
        },
        // ==================== Filesystem Tools ====================
        ToolDefinition {
            name: "ls".to_string(),
            description: "列出目录内容，浏览文件系统结构".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "uri": {
                        "type": "string",
                        "description": "目录 URI（如 cortex://session/{session_id}/timeline）"
                    },
                    "recursive": {
                        "type": "boolean",
                        "description": "是否递归列出子目录",
                        "default": false
                    },
                    "include_abstracts": {
                        "type": "boolean",
                        "description": "是否包含文件的 L0 摘要",
                        "default": false
                    }
                },
                "required": ["uri"]
            }),
        },
        ToolDefinition {
            name: "explore".to_string(),
            description: "智能探索记忆空间，结合搜索和浏览".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "探索查询"
                    },
                    "start_uri": {
                        "type": "string",
                        "description": "起始 URI",
                        "default": "cortex://session"
                    },
                    "max_depth": {
                        "type": "integer",
                        "description": "最大探索深度",
                        "default": 3
                    },
                    "return_layers": {
                        "type": "array",
                        "items": {
                            "type": "string",
                            "enum": ["L0", "L1", "L2"]
                        },
                        "description": "返回哪些层级",
                        "default": ["L0"]
                    }
                },
                "required": ["query"]
            }),
        },
        // ==================== Storage Tools ====================
        ToolDefinition {
            name: "store".to_string(),
            description: "存储新内容，自动生成 L0/L1 分层摘要".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "content": {
                        "type": "string",
                        "description": "要存储的内容"
                    },
                    "thread_id": {
                        "type": "string",
                        "description": "线程 ID"
                    },
                    "metadata": {
                        "type": "object",
                        "description": "元数据（标签、重要性等）"
                    },
                    "auto_generate_layers": {
                        "type": "boolean",
                        "description": "是否自动生成 L0/L1 摘要",
                        "default": true
                    }
                },
                "required": ["content", "thread_id"]
            }),
        },
    ]
}

/// Get a specific tool definition by name
pub fn get_mcp_tool_definition(name: &str) -> Option<ToolDefinition> {
    get_mcp_tool_definitions()
        .into_iter()
        .find(|def| def.name == name)
}
