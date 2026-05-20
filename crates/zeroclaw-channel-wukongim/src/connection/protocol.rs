// src/connection/protocol.rs
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

pub const WUKONGIM_RPC_VERSION: &str = "2.0";

pub struct WkMessageType;
impl WkMessageType {
    pub const TEXT: u32 = 1;
    pub const IMAGE: u32 = 2;
    pub const FILE: u32 = 5;
    pub const MARKDOWN: u32 = 14;
    pub const INTERACTIVE_CARD: u32 = 20;
    pub const INTERACTIVE_RESPONSE: u32 = 21;
    pub const CMD: u32 = 99;
}

pub struct WkChannelType;
impl WkChannelType {
    pub const PERSONAL: u8 = 1;
    pub const GROUP: u8 = 2;
}

#[derive(Debug, Serialize, Deserialize)]
pub struct JsonRpcRequest<P> {
    pub jsonrpc: String,
    pub method: String,
    pub id: String,
    pub params: P,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(bound(deserialize = "R: DeserializeOwned"))]
pub struct JsonRpcResponse<R> {
    pub jsonrpc: String,
    pub id: Option<String>,
    pub result: Option<R>,
    pub error: Option<JsonRpcError>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(bound(deserialize = "P: DeserializeOwned"))]
pub struct JsonRpcNotification<P> {
    pub jsonrpc: String,
    pub method: String,
    pub params: P,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct JsonRpcError {
    pub code: i32,
    pub message: String,
    pub data: Option<serde_json::Value>,
}

#[derive(Debug, Serialize, Deserialize, Default)]
pub struct Header {
    #[serde(rename = "noPersist", skip_serializing_if = "Option::is_none")]
    pub no_persist: Option<bool>,
    #[serde(rename = "redDot", skip_serializing_if = "Option::is_none")]
    pub red_dot: Option<bool>,
    #[serde(rename = "syncOnce", skip_serializing_if = "Option::is_none")]
    pub sync_once: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dup: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end: Option<bool>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ConnectParams {
    pub uid: String,
    pub token: String,
    #[serde(rename = "deviceId")]
    pub device_id: String,
    #[serde(rename = "deviceFlag")]
    pub device_flag: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<i32>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SendParams {
    #[serde(rename = "fromUid", skip_serializing_if = "Option::is_none")]
    pub from_uid: Option<String>,
    #[serde(rename = "clientMsgNo")]
    pub client_msg_no: String,
    #[serde(rename = "channelId")]
    pub channel_id: String,
    #[serde(rename = "channelType")]
    pub channel_type: u8,
    pub payload: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub header: Option<Header>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub setting: Option<u32>,
    #[serde(rename = "msgKey", skip_serializing_if = "Option::is_none")]
    pub msg_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expire: Option<u32>,
    #[serde(rename = "streamNo", skip_serializing_if = "Option::is_none")]
    pub stream_no: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub topic: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecvNotificationParams {
    #[serde(rename = "messageId")]
    pub message_id: String,
    #[serde(rename = "messageSeq")]
    pub message_seq: u32,
    #[serde(rename = "fromUid")]
    pub from_uid: String,
    #[serde(rename = "channelId")]
    pub channel_id: String,
    #[serde(rename = "channelType")]
    pub channel_type: u8,
    pub payload: serde_json::Value,
    pub timestamp: i64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct RecvAckParams {
    #[serde(rename = "messageId")]
    pub message_id: String,
    #[serde(rename = "messageSeq")]
    pub message_seq: u32,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SyncRequest {
    pub uid: String,
    pub version: i64,
    #[serde(rename = "last_msg_seqs")]
    pub last_msg_seqs: String,
    #[serde(rename = "msg_count")]
    pub msg_count: u32,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SyncMessage {
    #[serde(rename = "message_id")]
    pub message_id: serde_json::Value, // Handle number or string
    #[serde(rename = "message_seq")]
    pub message_seq: u32,
    #[serde(rename = "from_uid")]
    pub from_uid: String,
    pub payload: serde_json::Value,
    pub timestamp: i64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SyncResponse {
    pub conversations: Vec<SyncConversation>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SyncConversation {
    pub channel_id: String,
    pub channel_type: u8,
    pub unread: Option<u32>,
    pub timestamp: i64,
    pub last_msg_seq: u32,
    pub version: i64,
    pub recents: Option<Vec<SyncMessage>>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ClearUnreadRequest {
    pub uid: String,
    pub channel_id: String,
    pub channel_type: u8,
    #[serde(rename = "message_seq")]
    pub message_seq: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_type_constants() {
        assert_eq!(WkMessageType::TEXT, 1);
        assert_eq!(WkMessageType::IMAGE, 2);
        assert_eq!(WkMessageType::MARKDOWN, 14);
        assert_eq!(WkMessageType::INTERACTIVE_CARD, 20);
        assert_eq!(WkMessageType::INTERACTIVE_RESPONSE, 21);
        assert_eq!(WkMessageType::CMD, 99);
    }

    #[test]
    fn channel_type_constants() {
        assert_eq!(WkChannelType::PERSONAL, 1);
        assert_eq!(WkChannelType::GROUP, 2);
    }

    #[test]
    fn jsonrpc_request_roundtrip() {
        let req: JsonRpcRequest<serde_json::Value> = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            method: "ping".to_string(),
            id: "abc".to_string(),
            params: serde_json::json!({}),
        };
        let s = serde_json::to_string(&req).unwrap();
        assert!(s.contains("\"jsonrpc\":\"2.0\""));
        assert!(s.contains("\"method\":\"ping\""));
        assert!(s.contains("\"id\":\"abc\""));
    }

    #[test]
    fn recv_notification_params_deserializes() {
        let json = r#"{
            "messageId":"m1","messageSeq":5,"fromUid":"u1",
            "channelId":"c1","channelType":1,"payload":"dGVzdA==","timestamp":9999
        }"#;
        let p: RecvNotificationParams = serde_json::from_str(json).unwrap();
        assert_eq!(p.message_id, "m1");
        assert_eq!(p.channel_type, 1);
        assert_eq!(p.timestamp, 9999);
    }

    #[test]
    fn header_skips_none_fields() {
        let h = Header::default();
        assert_eq!(serde_json::to_string(&h).unwrap(), "{}");
    }
}
