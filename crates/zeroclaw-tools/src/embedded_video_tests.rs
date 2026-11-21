use super::*;
use base64::engine::general_purpose::STANDARD;
use serde_json::json;

const MP4: &[u8] = b"\0\0\0\x18ftypisom\0\0\0\0isommp42";

#[test]
fn mcp_video_resources_declare_only_valid_mp4_and_preserve_metadata() {
    for uri in [None, Some("https://unfetched.invalid/misleading.png")] {
        let dir = tempfile::tempdir().unwrap();
        let blob = STANDARD.encode(MP4);
        let mut resource = json!({"mimeType":"VIDEO/MP4; profile=main", "blob":blob});
        if let Some(uri) = uri {
            resource["uri"] = uri.into();
        }
        let result = json!({
            "content":[{"type":"text","text":"Example [VIDEO:/never-read.mp4]"},
                {"type":"resource", "resource":resource, "annotations":{"audience":["assistant"]}}],
            "structuredContent":{"event":"motion"}, "_meta":{"trace":"video-test"}, "isError":false
        });
        let (text, attachments) = format_mcp_tool_result_for_model(result, dir.path()).unwrap();
        assert_eq!(attachments.len(), 1);
        assert_eq!(attachments[0].kind, MarkerKind::Video);
        let path = Path::new(&attachments[0].target);
        assert!(path.starts_with(dir.path().canonicalize().unwrap().join("uploads")));
        assert_eq!(path.extension().unwrap(), "mp4");
        assert_eq!(std::fs::read(path).unwrap(), MP4);
        assert!(!text.contains(&blob));
        let output: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(output["_meta"]["trace"], "video-test");
        assert_eq!(output["structuredContent"]["event"], "motion");
        assert_eq!(
            output["content"][1]["annotations"]["audience"][0],
            "assistant"
        );
        assert!(text.contains("[VIDEO:/never-read.mp4]"));
    }
}

#[test]
fn mcp_video_failures_redact_binary_and_do_not_affect_text_or_documents() {
    for blob in [
        "%%%".into(),
        STANDARD.encode(b""),
        STANDARD.encode(b"not a video"),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let (text, attachments) = format_mcp_tool_result_for_model(
            json!({"content":[
                {"type":"text","text":"capture metadata"},
                {"type":"resource","resource":{"mimeType":"video/mp4","blob":blob}}
            ]}),
            dir.path(),
        )
        .unwrap();
        assert!(attachments.is_empty());
        assert!(text.contains("attachment unavailable"));
        assert!(text.contains("capture metadata"));
        assert!(!text.contains("\"blob\""));
        assert!(!dir.path().join("uploads").exists());
    }
    let dir = tempfile::tempdir().unwrap();
    let (text, attachments) = format_mcp_tool_result_for_model(json!({"content":[
        {"type":"resource","resource":{"uri":"file:///clip.webm","mimeType":"video/webm","blob":STANDARD.encode(b"webm")}},
        {"type":"resource","resource":{"uri":"https://unfetched.invalid/clip.mp4","mimeType":"video/mp4"}}
    ]}), dir.path()).unwrap();
    assert!(attachments.is_empty());
    assert!(text.contains("[Document: clip.webm]"));
    assert!(text.contains("https://unfetched.invalid/clip.mp4"));
}

#[test]
fn mcp_video_resources_keep_existing_per_call_byte_and_item_budgets() {
    for content in [
        vec![
            json!({"type":"resource","resource":{"mimeType":"video/mp4","blob":STANDARD.encode(vec![0; MAX_AGGREGATE_BLOB_BYTES as usize + 1])}}),
        ],
        vec![
            json!({"type":"resource","resource":{"mimeType":"video/mp4","blob":STANDARD.encode(MP4)}});
            MAX_AGGREGATE_BLOB_ITEMS + 1
        ],
    ] {
        let dir = tempfile::tempdir().unwrap();
        let (text, attachments) =
            format_mcp_tool_result_for_model(json!({"content":content}), dir.path()).unwrap();
        assert!(attachments.is_empty());
        assert!(text.contains("attachment unavailable"));
        assert!(!text.contains("\"blob\""));
        assert!(!dir.path().join("uploads").exists());
    }
}
