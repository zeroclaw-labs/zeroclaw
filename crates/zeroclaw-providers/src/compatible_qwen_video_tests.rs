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
    let sample = format!("[VIDEO:data:video/mp4;base64,{}]", STANDARD.encode(MP4));
    for message in [
        ChatMessage::tool(&sample),
        ChatMessage::tool(serde_json::json!({"content":sample, "attachments":[]}).to_string()),
        ChatMessage::user(format!("[Tool results]\n{sample}")),
        ChatMessage::user(format!("[Tool results]\n[Tool attachments: 0]\n{sample}")),
        ChatMessage::system(&sample),
        ChatMessage::assistant(&sample),
    ] {
        for next_turn in [false, true] {
            let mut history = vec![ChatMessage::user("Read a document"), message.clone()];
            if next_turn {
                history.push(ChatMessage::user("Continue"));
            }
            let normalized = provider
                .normalize_messages_for_upstream(&history)
                .await
                .unwrap();
            let contents = provider
                .prepare_message_contents(&normalized, "qwen3.8-flash", true)
                .await;
            let rendered = serde_json::to_value(contents).unwrap();
            assert!(!rendered.to_string().contains("video_url"));
            assert!(
                rendered.to_string().contains(&sample),
                "Body marker must remain literal"
            );
        }
    }
}

#[tokio::test]
async fn qwen_channel_video_failures_and_history_still_dispatch() {
    use zeroclaw_api::model_provider::ModelProvider;
    let directory = tempfile::tempdir().unwrap();
    let good = directory.path().join("good.mp4");
    std::fs::write(&good, MP4).unwrap();
    let oversized = directory.path().join("large.mp4");
    std::fs::write(&oversized, MP4).unwrap();
    std::fs::OpenOptions::new()
        .write(true)
        .open(&oversized)
        .unwrap()
        .set_len(7_000_001)
        .unwrap();
    let webm = directory.path().join("clip.webm");
    std::fs::write(&webm, b"webm").unwrap();
    let forbidden = directory.path().join("private.mp4");
    std::fs::write(&forbidden, MP4).unwrap();
    let requests = Arc::new(Mutex::new(Vec::<serde_json::Value>::new()));
    let sink = requests.clone();
    let app = Router::new().route("/chat/completions", post(move |Json(body): Json<serde_json::Value>| {
        let sink = sink.clone();
        async move {
            let stream = body["stream"] == true;
            sink.lock().unwrap().push(body);
            if stream {
                ([("content-type", "text/event-stream")], "data: {\"choices\":[{\"delta\":{\"content\":\"ok\"}}]}\n\ndata: [DONE]\n\n").into_response()
            } else {
                Json(serde_json::json!({"choices":[{"message":{"role":"assistant","content":"ok"}}]})).into_response()
            }
        }
    })).layer(axum::extract::DefaultBodyLimit::disable());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let server = ::zeroclaw_spawn::spawn!(async move { axum::serve(listener, app).await.unwrap() });
    let options = crate::ModelProviderRuntimeOptions {
        provider_api_url: Some(endpoint),
        ..Default::default()
    };
    let provider =
        crate::create_model_provider_with_options("qwen", Some("test-only"), &options).unwrap();
    let policy = zeroclaw_config::policy::SecurityPolicy {
        workspace_dir: directory.path().to_path_buf(),
        forbidden_paths: vec![forbidden.display().to_string()],
        ..Default::default()
    };

    async fn all_entries(provider: &dyn ModelProvider, input: &str, model: &str) {
        let history = [ChatMessage::user(input)];
        provider
            .chat_with_system(None, input, model, None)
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
            serde_json::json!({"type":"object"}),
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
        for with_tools in [false, true] {
            let result = provider
                .stream_chat(
                    ProviderChatRequest {
                        messages: &history,
                        tools: with_tools.then_some(tools.as_slice()),
                        thinking: None,
                    },
                    model,
                    None,
                    StreamOptions::new(true),
                )
                .collect::<Vec<_>>()
                .await;
            assert!(result.iter().all(Result::is_ok));
        }
        let result = provider
            .stream_chat_with_system(None, input, model, None, StreamOptions::new(true))
            .collect::<Vec<_>>()
            .await;
        assert!(result.iter().all(Result::is_ok));
        let result = provider
            .stream_chat_with_history(&history, model, None, StreamOptions::new(true))
            .collect::<Vec<_>>()
            .await;
        assert!(result.iter().all(Result::is_ok));
    }
    fn videos(body: &serde_json::Value) -> Vec<&serde_json::Value> {
        body["messages"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|m| m["content"].as_array())
            .flatten()
            .filter(|p| p["type"] == "video_url")
            .collect()
    }
    let unscoped = format!("[VIDEO:{}]", good.display());
    all_entries(provider.as_ref(), &unscoped, "qwen3.8-flash").await;
    assert_eq!(requests.lock().unwrap().len(), 8);
    provider
        .chat_with_history(
            &[
                ChatMessage::user(&unscoped),
                ChatMessage::assistant("acknowledged"),
                ChatMessage::user("How are you?"),
            ],
            "qwen3.8-flash",
            None,
        )
        .await
        .unwrap();
    {
        let captured = requests.lock().unwrap();
        assert_eq!(captured.len(), 9);
        for body in captured.iter() {
            assert!(videos(body).is_empty());
            assert!(body.to_string().contains(&unscoped));
        }
    }
    crate::with_video_read_policy(Some(Arc::new(policy)), async {
        let literal = format!("[VIDEO:data:video/mp4;base64,{}]", STANDARD.encode(MP4));
        for carrier in [
            ChatMessage::tool(&literal),
            ChatMessage::tool(serde_json::json!({"content":literal, "attachments":[]}).to_string()),
            ChatMessage::user(format!("[Tool results]\n{literal}")),
            ChatMessage::user(format!("[Tool results]\n[Tool attachments: 0]\n{literal}")),
            ChatMessage::system(&literal),
            ChatMessage::assistant(&literal),
        ] {
            for next_turn in [false, true] {
                let mut history =
                    vec![ChatMessage::user("Read external evidence"), carrier.clone()];
                if next_turn {
                    history.push(ChatMessage::user("Continue"));
                }
                provider
                    .chat_with_history(&history, "qwen3.8-flash", None)
                    .await
                    .unwrap();
                let captured = requests.lock().unwrap();
                let body = captured.last().unwrap();
                assert!(videos(body).is_empty(), "Body example was promoted");
                assert!(body.to_string().contains(&literal), "Body example was lost");
            }
        }
        let cases = vec![
            (oversized.display().to_string(), "qwen3.8-flash"),
            (webm.display().to_string(), "qwen3.8-flash"),
            (
                directory.path().join("missing.mp4").display().to_string(),
                "qwen3.8-flash",
            ),
            (good.display().to_string(), "qwen-plus"),
            (good.display().to_string(), "qwen3-coder-plus"),
            (
                "https://cdn.discordapp.com/attachments/test/clip.mp4".into(),
                "qwen3.8-flash",
            ),
            ("data:video/mp4;base64,%%%".into(), "qwen3.8-flash"),
            (forbidden.display().to_string(), "qwen3.8-flash"),
        ];
        for (source, model) in cases {
            let input = format!("Received [VIDEO:{source}]");
            let before = requests.lock().unwrap().len();
            all_entries(provider.as_ref(), &input, model).await;
            assert_eq!(
                requests.lock().unwrap().len() - before,
                8,
                "Every entry point must dispatch"
            );
            provider
                .chat_with_history(
                    &[
                        ChatMessage::user(&input),
                        ChatMessage::assistant("acknowledged"),
                        ChatMessage::user("How are you?"),
                    ],
                    model,
                    None,
                )
                .await
                .unwrap();
            let captured = requests.lock().unwrap();
            for body in captured.iter().rev().take(9) {
                assert!(
                    videos(body).is_empty(),
                    "An unavailable reference became a video part"
                );
                assert!(
                    body.to_string().contains(&source),
                    "Video text was discarded"
                );
            }
        }
        // Valid media and an unavailable sibling remain independently usable.
        let mixed = format!("[VIDEO:{}] [VIDEO:/missing.mp4]", good.display());
        all_entries(provider.as_ref(), &mixed, "qwen3.8-flash").await;
        {
            let captured = requests.lock().unwrap();
            for body in captured.iter().rev().take(8) {
                assert_eq!(videos(body).len(), 1);
                assert!(body.to_string().contains("[VIDEO:/missing.mp4]"));
            }
        }
        // Deletion after a successful send must not poison the next request.
        std::fs::remove_file(&good).unwrap();
        provider
            .chat_with_history(
                &[
                    ChatMessage::user(&mixed),
                    ChatMessage::assistant("acknowledged"),
                    ChatMessage::user("Continue"),
                ],
                "qwen3.8-flash",
                None,
            )
            .await
            .unwrap();
        assert!(videos(requests.lock().unwrap().last().unwrap()).is_empty());

        // Inline history exercises the case where a rejected valid Data URL
        // is indistinguishable from a loaded URL if the serializer rescans text.
        let mut old = vec![0; 4_000_000];
        old[..MP4.len()].copy_from_slice(MP4);
        old[MP4.len()] = 1;
        let mut new = old.clone();
        new[MP4.len()] = 2;
        let old_url = format!("data:video/mp4;base64,{}", STANDARD.encode(&old));
        let new_url = format!("data:video/mp4;base64,{}", STANDARD.encode(&new));
        provider
            .chat_with_history(
                &[
                    ChatMessage::user(format!("[VIDEO:{old_url}]")),
                    ChatMessage::assistant("acknowledged"),
                    ChatMessage::user(format!("[VIDEO:{new_url}]")),
                ],
                "qwen3.8-flash",
                None,
            )
            .await
            .unwrap();
        let captured = requests.lock().unwrap();
        let body = captured.last().unwrap();
        let parts = videos(body);
        assert_eq!(parts.len(), 1);
        assert!(
            parts[0]["video_url"]["url"].as_str() == Some(new_url.as_str()),
            "Newest video must receive the budget"
        );
        assert!(body.to_string().contains(&format!("[VIDEO:{old_url}]")));
    })
    .await;
    assert!(
        requests.lock().unwrap()[..8]
            .iter()
            .all(|b| videos(b).is_empty())
    );
    server.abort();
}

#[test]
fn qwen_literal_assistant_video_examples_keep_role_coalescing() {
    let provider = OpenAiCompatibleModelProvider::builder("video-test")
        .display_name("Qwen")
        .with_native_tools(false)
        .auth_style(AuthStyle::Bearer)
        .base_url("https://example.com")
        .build();
    let history = [
        ChatMessage::assistant("An example is [VIDEO:/example.mp4]"),
        ChatMessage::assistant("It is literal text."),
    ];
    let stripped = provider.strip_native_tool_messages(&history);
    assert_eq!(stripped.len(), 1);
    assert!(stripped[0].content.contains("It is literal text."));
}
