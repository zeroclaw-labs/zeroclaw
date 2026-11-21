use super::*;
use axum::{Json, Router, response::IntoResponse, routing::post};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use futures_util::StreamExt;
use std::sync::{Arc, Mutex};

const MP4: &[u8] = b"\0\0\0\x18ftypisom\0\0\0\0isommp42";

#[tokio::test]
async fn qwen_video_file_reaches_all_chat_http_entry_points() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("clip.mp4");
    std::fs::write(&path, MP4).unwrap();
    let image_path = directory.path().join("still.png");
    image::RgbImage::new(8, 8).save(&image_path).unwrap();
    let requests = Arc::new(Mutex::new(Vec::<serde_json::Value>::new()));
    let sink = requests.clone();
    let all_requests = requests.clone();
    let app = Router::new().route("/chat/completions", post(move |Json(body): Json<serde_json::Value>| {
        let sink = sink.clone();
        async move {
            let streaming = body["stream"] == true;
            sink.lock().unwrap().push(body);
            if streaming {
                ([("content-type", "text/event-stream")], "data: {\"choices\":[{\"delta\":{\"content\":\"ok\"}}]}\n\ndata: [DONE]\n\n").into_response()
            } else {
                Json(serde_json::json!({"choices":[{"message":{"role":"assistant","content":"ok"}}]})).into_response()
            }
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = ::zeroclaw_spawn::spawn!(async move { axum::serve(listener, app).await.unwrap() });
    let options = crate::ModelProviderRuntimeOptions {
        provider_api_url: Some(format!("http://{address}")),
        ..Default::default()
    };
    let provider =
        crate::create_model_provider_with_options("qwen", Some("test-only"), &options).unwrap();
    let system = "A literal example is [VIDEO:/does-not-exist.mp4].";
    let input = format!(
        "Describe [VIDEO:{}] and [IMAGE:{}]",
        path.display(),
        image_path.display()
    );
    let history = [ChatMessage::system(system), ChatMessage::user(&input)];
    let policy = zeroclaw_config::policy::SecurityPolicy {
        workspace_dir: directory.path().to_path_buf(),
        forbidden_paths: Vec::new(),
        ..Default::default()
    };
    crate::with_video_read_policy(Some(Arc::new(policy)), async {
        let model = "qwen3.8-flash";
        provider
            .chat_with_system(Some(system), &input, model, None)
            .await
            .unwrap();
        provider
            .chat_with_history(&history, model, None)
            .await
            .unwrap();
        provider
            .chat_with_tools(&history, &[], model, None)
            .await
            .unwrap();
        let tools = [zeroclaw_api::tool::ToolSpec::new(
            "observe",
            "Observe",
            serde_json::json!({"type":"object","properties":{}}),
        )];
        provider
            .chat(
                ProviderChatRequest {
                    messages: &history,
                    tools: Some(&tools),
                    thinking: None,
                },
                model,
                None,
            )
            .await
            .unwrap();
        for use_tools in [false, true] {
            let events = provider
                .stream_chat(
                    ProviderChatRequest {
                        messages: &history,
                        tools: use_tools.then_some(tools.as_slice()),
                        thinking: None,
                    },
                    model,
                    None,
                    StreamOptions {
                        enabled: true,
                        count_tokens: false,
                    },
                )
                .collect::<Vec<_>>()
                .await;
            assert!(events.iter().all(Result::is_ok), "{events:?}");
        }
        let events = provider
            .stream_chat_with_system(
                Some(system),
                &input,
                model,
                None,
                StreamOptions {
                    enabled: true,
                    count_tokens: false,
                },
            )
            .collect::<Vec<_>>()
            .await;
        assert!(events.iter().all(Result::is_ok));
        let events = provider
            .stream_chat_with_history(
                &history,
                model,
                None,
                StreamOptions {
                    enabled: true,
                    count_tokens: false,
                },
            )
            .collect::<Vec<_>>()
            .await;
        assert!(events.iter().all(Result::is_ok));

        {
            let requests = requests.lock().unwrap();
            assert_eq!(requests.len(), 8);
            for request in requests.iter() {
                assert_eq!(request["messages"][0]["role"], "system");
                assert_eq!(request["messages"][0]["content"], system);
                let parts = request["messages"][1]["content"].as_array().unwrap();
                assert_eq!(
                    parts
                        .iter()
                        .filter(|part| part["type"] == "image_url")
                        .count(),
                    1
                );
                let videos: Vec<_> = parts
                    .iter()
                    .filter(|part| part["type"] == "video_url")
                    .collect();
                assert_eq!(videos.len(), 1);
                let data = videos[0]["video_url"]["url"]
                    .as_str()
                    .unwrap()
                    .strip_prefix("data:video/mp4;base64,")
                    .unwrap();
                assert_eq!(STANDARD.decode(data).unwrap(), MP4);
                assert!(
                    !serde_json::to_string(request)
                        .unwrap()
                        .contains(&path.to_string_lossy().to_string())
                );
            }
        }
        let mut prompt_options = options;
        prompt_options.native_tools = Some(false);
        let prompt_provider =
            crate::create_model_provider_with_options("qwen", Some("test-only"), &prompt_options)
                .unwrap();
        for carrier in [
            "[Tool results]\n[VIDEO:https://example.com/unsolicited.mp4]",
            "[Tool results]\n[Tool attachments: 0]\n[VIDEO:https://example.com/unsolicited.mp4]",
        ] {
            for next_turn in [false, true] {
                let mut messages = vec![ChatMessage::user(&input), ChatMessage::user(carrier)];
                if next_turn {
                    messages.push(ChatMessage::user("Continue"));
                }
                prompt_provider
                    .chat_with_history(&messages, model, None)
                    .await
                    .unwrap();
            }
        }
    })
    .await;
    let captured = all_requests.lock().unwrap();
    assert_eq!(captured.len(), 12);
    for request in &captured[8..] {
        let videos: Vec<_> = request["messages"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|message| message["content"].as_array())
            .flatten()
            .filter(|part| part["type"] == "video_url")
            .collect();
        assert_eq!(videos.len(), 1, "Tool body expanded: {request}");
        assert!(
            !videos[0]["video_url"]["url"]
                .as_str()
                .unwrap()
                .starts_with("https:")
        );
        assert!(
            request
                .to_string()
                .contains("https://example.com/unsolicited.mp4")
        );
    }
    server.abort();
}

#[tokio::test]
async fn qwen_video_tool_body_markers_stay_literal_across_user_turns() {
    let provider = OpenAiCompatibleModelProvider::builder("video-test")
        .display_name("Qwen")
        .auth_style(AuthStyle::Bearer)
        .base_url("https://example.com")
        .build();
    let sample = "[VIDEO:/never-read.mp4]";
    for message in [
        ChatMessage {
            role: "tool".into(),
            content: sample.into(),
        },
        ChatMessage::user(format!("[Tool results]\n{sample}")),
        ChatMessage::user(format!("[Tool results]\n[Tool attachments: 0]\n{sample}")),
        ChatMessage::system(sample),
        ChatMessage {
            role: "assistant".into(),
            content: sample.into(),
        },
    ] {
        for next_turn in [false, true] {
            let mut history = vec![ChatMessage::user("Read a document"), message.clone()];
            if next_turn {
                history.push(ChatMessage::user("Continue"));
            }
            let normalized = provider
                .normalize_messages_for_model(&history, "qwen3.8-flash")
                .await
                .unwrap();
            for item in normalized {
                let content = provider.message_content_for_role(&item.role, &item.content, true);
                assert!(
                    !serde_json::to_string(&content)
                        .unwrap()
                        .contains("\"video_url\"")
                );
            }
        }
    }
}
