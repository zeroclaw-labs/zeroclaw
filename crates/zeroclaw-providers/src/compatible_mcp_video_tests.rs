use super::*;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use std::sync::Arc;
use zeroclaw_api::tool_carrier::{render_native_attachments, render_prompt_tool_carrier};

const MP4: &[u8] = b"\0\0\0\x18ftypisom\0\0\0\0isommp42";

#[tokio::test]
async fn declared_tool_video_relocates_with_images_and_expires_at_the_next_user_turn() {
    let dir = tempfile::tempdir().unwrap();
    let video = dir.path().join("event.mp4");
    let image = dir.path().join("still.png");
    std::fs::write(&video, MP4).unwrap();
    image::RgbImage::new(8, 8).save(&image).unwrap();
    let policy = zeroclaw_config::policy::SecurityPolicy {
        workspace_dir: dir.path().to_path_buf(),
        forbidden_paths: Vec::new(),
        ..Default::default()
    };
    crate::with_video_read_policy(Some(Arc::new(policy)), async {
        let provider = OpenAiCompatibleModelProvider::builder("video-test")
            .display_name("Qwen").auth_style(AuthStyle::Bearer).base_url("https://example.com").build();
        let attachments = vec![
            RenderedMarker {kind:MarkerKind::Video, target:video.display().to_string()},
            RenderedMarker {kind:MarkerKind::Video, target:dir.path().join("missing.mp4").display().to_string()},
            RenderedMarker {kind:MarkerKind::Image, target:image.display().to_string()},
        ];
        let body = "scene metadata. Example [VIDEO:/never-read.mp4]";
        for prompt in [false, true] {
            let carrier = if prompt {
                ChatMessage::user(render_prompt_tool_carrier(body, &attachments))
            } else {
                ChatMessage::tool(serde_json::json!({
                    "tool_call_id":"call-video", "tool_name":"capture", "content":body,
                    "attachments":render_native_attachments(&attachments), "_meta":{"trace":"retained"}
                }).to_string())
            };
            let mut history = vec![ChatMessage::user("Observe"),
                ChatMessage::assistant(r#"{"tool_calls":[{"id":"call-video","name":"capture","arguments":"{}"}]}"#),
                carrier, ChatMessage::assistant("Check another sensor"),
                ChatMessage::tool(r#"{"tool_call_id":"call-sensor","content":"unrelated sensor result","attachments":[]}"#)];
            for next_turn in [false, true] {
                if next_turn { history.push(ChatMessage::user("Continue without prior tool video")); }
                let normalized = provider.normalize_messages_for_upstream(&history).await.unwrap();
                let contents = provider.prepare_message_contents(&normalized, "qwen3.8-flash", true).await;
                let converted = provider.convert_messages_with_contents(&normalized, true, Some(contents));
                let wire = serde_json::to_value(converted).unwrap();
                let videos: Vec<_> = wire.as_array().unwrap().iter()
                    .filter_map(|m|m["content"].as_array()).flatten().filter(|p|p["type"]=="video_url").collect();
                assert_eq!(videos.len(), usize::from(!next_turn), "{wire}");
                if !next_turn {
                    let data = videos[0]["video_url"]["url"].as_str().unwrap().strip_prefix("data:video/mp4;base64,").unwrap();
                    assert_eq!(STANDARD.decode(data).unwrap(), MP4);
                    assert!(wire.to_string().contains("image_url"));
                    assert!(wire.to_string().contains("video unavailable"));
                }
                for message in wire.as_array().unwrap().iter().filter(|m|m["role"]=="tool") {
                    assert!(message["content"].is_string(), "{message}");
                }
                assert!(wire.to_string().contains("[VIDEO:/never-read.mp4]"));
                assert!(!wire.to_string().contains(&format!("[VIDEO:{}]", video.display())));
            }
        }
        let unsupported = [ChatMessage::user("Observe"), ChatMessage::tool(serde_json::json!({"content":body, "attachments":render_native_attachments(&attachments)}).to_string())];
        let normalized = crate::qwen_video::prepare_messages(&unsupported, "qwen-turbo").await;
        let carrier = classify("tool", &normalized[1].text).unwrap();
        assert!(carrier.attachments.iter().all(|a|a.kind!=MarkerKind::Video));
        assert!(carrier.text.contains("does not support video"));
    }).await;
}

#[tokio::test]
async fn missing_tool_video_degrades_its_attachment_without_reading_body_examples() {
    let carrier = ChatMessage::user(render_prompt_tool_carrier(
        "[VIDEO:/body-example.mp4]",
        &[RenderedMarker {
            kind: MarkerKind::Video,
            target: "/missing.mp4".into(),
        }],
    ));
    let messages = [ChatMessage::user("Observe"), carrier];
    let normalized = crate::qwen_video::prepare_messages(&messages, "qwen3.8-flash").await;
    let carrier = classify("user", &normalized[1].text).unwrap();
    assert!(carrier.attachments.is_empty());
    assert!(carrier.text.contains("[VIDEO:/body-example.mp4]"));
    assert!(carrier.text.contains("video unavailable"));
}

#[test]
fn qwen_prompt_carriers_keep_external_provenance_for_every_attachment_shape() {
    let provider = OpenAiCompatibleModelProvider::builder("video-test")
        .display_name("Qwen")
        .auth_style(AuthStyle::Bearer)
        .base_url("https://example.com")
        .build();
    let document = RenderedMarker {
        kind: MarkerKind::Document,
        target: "/report.pdf".into(),
    };
    for media in [None, Some(MarkerKind::Image), Some(MarkerKind::Video)] {
        let mut attachments = vec![document.clone()];
        if let Some(kind) = media {
            attachments.push(RenderedMarker {
                kind,
                target: "data:test;base64,fixture".into(),
            });
        }
        let text = render_prompt_tool_carrier("untrusted tool body", &attachments);
        let wire =
            serde_json::to_value(provider.message_content_for_role("user", &text, true)).unwrap();
        let rendered = if let Some(text) = wire.as_str() {
            text.to_owned()
        } else {
            wire[0]["text"].as_str().unwrap().to_owned()
        };
        assert!(
            rendered.starts_with("[Tool results]\n[Tool attachments: 1]\n[DOCUMENT:/report.pdf]\n"),
            "{wire}"
        );
        assert!(rendered.ends_with("untrusted tool body"));
    }
    let empty = render_prompt_tool_carrier("untrusted text-only result", &[]);
    let wire =
        serde_json::to_value(provider.message_content_for_role("user", &empty, true)).unwrap();
    assert_eq!(wire.as_str().unwrap(), empty);
}
