use super::{
    GoalReattachPeer, OCCURRENCE_OWNER, RpcContext, assert_ordinary_prompt_acknowledgement,
    insert_session_with_owner,
};
use async_trait::async_trait;
use futures_util::StreamExt as _;
use serde_json::{Value, json};
use std::sync::Arc;
use tokio::sync::Notify;
use zeroclaw_api::attribution::{Attributable, ModelProviderKind, ProviderKind, Role};
use zeroclaw_api::model_provider::{
    ChatMessage, ChatRequest, ChatResponse, ModelProvider, StreamChunk, StreamError, StreamEvent,
    StreamOptions, StreamResult,
};

const SID: &str = "ordinary-live-occurrence";
const LIVE_PREFIX: &str = "This definite reply starts streaming before the model has finished. ";
const LIVE_TAIL: &str = "It reaches the canonical transcript.";
const NARRATION: &str = "Narrating before the provider tool. ";
const AFTER_TOOL: &str = "Continuing after the provider tool.";
const LOST: &str = "This visible partial response will be interrupted before it finishes.";
const FALLBACK: &str = "The nonstream fallback is a different response.";

struct Provider(Arc<Notify>);

#[async_trait]
impl ModelProvider for Provider {
    async fn chat_with_system(
        &self,
        _: Option<&str>,
        _: &str,
        _: &str,
        _: Option<f64>,
    ) -> anyhow::Result<String> {
        anyhow::bail!("fixture must use full request")
    }

    async fn chat(
        &self,
        request: ChatRequest<'_>,
        _: &str,
        _: Option<f64>,
    ) -> anyhow::Result<ChatResponse> {
        assert!(query(request).contains("fallback-before-output"));
        Ok(ChatResponse {
            text: Some(FALLBACK.into()),
            tool_calls: Vec::new(),
            usage: None,
            reasoning_content: None,
        })
    }

    fn supports_streaming(&self) -> bool {
        true
    }

    fn supports_streaming_tool_events(&self) -> bool {
        true
    }

    fn stream_chat(
        &self,
        request: ChatRequest<'_>,
        _: &str,
        _: Option<f64>,
        _: StreamOptions,
    ) -> futures_util::stream::BoxStream<'static, StreamResult<StreamEvent>> {
        let out = |text| Ok(StreamEvent::TextDelta(StreamChunk::delta(text)));
        let error = || {
            Err(StreamError::ModelProvider(
                "fixture stream interrupted".into(),
            ))
        };
        let prompt = query(request);
        if prompt.contains("accepted-live") {
            let release = Arc::clone(&self.0);
            futures_util::stream::iter([out(LIVE_PREFIX)])
                .chain(futures_util::stream::once(async move {
                    release.notified().await;
                    Ok(StreamEvent::TextDelta(StreamChunk::delta(LIVE_TAIL)))
                }))
                .chain(futures_util::stream::iter([Ok(StreamEvent::Final)]))
                .boxed()
        } else {
            let events = if prompt.contains("tool-boundary") {
                vec![
                    out(NARRATION),
                    Ok(StreamEvent::PreExecutedToolCall {
                        name: "fixture-tool".into(),
                        args: "{}".into(),
                    }),
                    Ok(StreamEvent::PreExecutedToolResult {
                        name: "fixture-tool".into(),
                        output: "fixture result".into(),
                    }),
                    Ok(StreamEvent::ThinkingDelta(
                        "transient provider notice".into(),
                    )),
                    out(AFTER_TOOL),
                    Ok(StreamEvent::Final),
                ]
            } else if prompt.contains("visible-interruption") {
                vec![out(LOST), error()]
            } else {
                assert!(prompt.contains("fallback-before-output"), "{prompt}");
                vec![error()]
            };
            futures_util::stream::iter(events).boxed()
        }
    }
}

fn query<'a>(request: ChatRequest<'a>) -> &'a str {
    &request
        .messages
        .iter()
        .rev()
        .find(|row| row.role == "user")
        .unwrap()
        .content
}

impl Attributable for Provider {
    fn role(&self) -> Role {
        Role::Provider(ProviderKind::Model(ModelProviderKind::Custom))
    }
    fn alias(&self) -> &str {
        "ordinary-live-occurrence-fixture"
    }
}

pub(super) async fn assert_ordinary_stream_identity(
    ctx: &Arc<RpcContext>,
    peer: &mut impl GoalReattachPeer,
    negotiated: bool,
) {
    use super::super::Method;

    let release = Arc::new(Notify::new());
    let path = ctx.config.read().data_dir.clone();
    insert_session_with_owner(
        ctx,
        &path,
        SID,
        Box::new(Provider(Arc::clone(&release))),
        Some(OCCURRENCE_OWNER),
    )
    .await;
    ctx.session_backend
        .as_ref()
        .unwrap()
        .append(
            &format!("rpc_{SID}"),
            &ChatMessage::user("existing history"),
        )
        .unwrap();

    for (index, prompt) in [
        "accepted-live",
        "tool-boundary",
        "visible-interruption",
        "fallback-before-output",
    ]
    .into_iter()
    .enumerate()
    {
        let id = 30 + 2 * index as u64;
        peer.write_request(
            Method::SessionPrompt,
            &json!({"session_id": SID, "prompt": prompt}),
            id,
        )
        .await;
        let mut fragments = Vec::new();
        let mut tool_events = 0;
        let mut notice_events = 0;
        let mut acknowledged = false;
        let mut terminal = None;
        while !acknowledged || terminal.is_none() {
            let frame = peer.next_frame().await;
            if frame["id"] == id {
                assert!(!acknowledged, "{frame}");
                if index == 2 {
                    assert!(frame["error"].is_object(), "{frame}");
                } else {
                    assert_ordinary_prompt_acknowledgement(&frame, SID);
                }
                acknowledged = true;
                continue;
            }
            assert_eq!(frame["method"], "session/update", "{frame}");
            let params = &frame["params"];
            assert_eq!(params["session_id"], SID, "{frame}");
            if !negotiated {
                assert!(params.get("reply_occurrence").is_none(), "{frame}");
            }
            match params["type"].as_str() {
                Some("agent_message_chunk") => {
                    let occurrence = params
                        .get("reply_occurrence")
                        .and_then(Value::as_str)
                        .map(str::to_owned);
                    let text = params["text"].as_str().unwrap().to_owned();
                    if index == 0 && fragments.is_empty() {
                        assert!(
                            LIVE_PREFIX.starts_with(&text) && !text.trim().is_empty(),
                            "{frame}"
                        );
                        release.notify_one();
                    }
                    fragments.push((text, occurrence));
                }
                Some("tool_call" | "tool_result") => {
                    assert!(params.get("reply_occurrence").is_none(), "{frame}");
                    tool_events += 1;
                }
                Some("agent_thought_chunk") => {
                    assert!(params.get("reply_occurrence").is_none(), "{frame}");
                    notice_events += 1;
                }
                Some("turn_complete") => {
                    assert!(terminal.is_none(), "{frame}");
                    terminal = Some(params.clone());
                }
                _ => assert!(params.get("reply_occurrence").is_none(), "{frame}"),
            }
        }
        let terminal = terminal.unwrap();
        let expected = [
            format!("{LIVE_PREFIX}{LIVE_TAIL}"),
            format!("{NARRATION}{AFTER_TOOL}"),
            LOST.into(),
            FALLBACK.into(),
        ];
        let visible = fragments
            .iter()
            .map(|(text, _)| text.as_str())
            .collect::<String>();
        assert!(visible.starts_with(&expected[index]), "{visible}");
        assert_eq!(
            terminal["outcome"],
            if index == 2 { "failed" } else { "completed" }
        );
        let page = peer
            .result(Method::SessionMessages, json!({"session_id": SID}), id + 1)
            .await;
        let rows = page["messages"].as_array().unwrap();
        if !negotiated {
            assert!(rows.iter().all(|row| row.get("reply_occurrence").is_none()));
        }
        let markers = fragments
            .iter()
            .filter_map(|(_, id)| id.as_deref())
            .collect::<Vec<_>>();
        if index == 1 {
            assert_eq!(tool_events, 2);
            assert_eq!(notice_events, 1);
        }
        if negotiated {
            match index {
                0 | 3 => {
                    let id = terminal["reply_occurrence"]
                        .as_str()
                        .filter(|id| !id.is_empty())
                        .unwrap();
                    assert!(!markers.is_empty() && markers.len() == fragments.len());
                    assert!(markers.iter().all(|marker| *marker == id));
                    assert!(
                        rows.iter().any(|row| row["content"] == expected[index]
                            && row["reply_occurrence"] == id),
                        "{page}"
                    );
                    if index == 0 {
                        assert!(fragments.len() >= 2, "must receive multiple live fragments");
                    }
                }
                1 | 2 => {
                    assert!(
                        !markers.is_empty(),
                        "provisional fragments must be observable: {fragments:?}"
                    );
                    if index == 1 {
                        assert_ne!(markers.first(), markers.last());
                    }
                    if index == 2 {
                        assert!(terminal.get("reply_occurrence").is_none());
                    }
                    assert!(
                        markers
                            .iter()
                            .all(|id| rows.iter().all(|row| row["reply_occurrence"] != *id)),
                        "{page}"
                    );
                }
                _ => unreachable!(),
            }
        }
    }
}
