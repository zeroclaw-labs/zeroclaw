//! Protocol capture adapter. Frozen blocks cross the API port; this adapter
//! never opens storage and never gets a session writer or permission grant.
use super::*;
use zeroclaw_api::session_events::capture::{
    ProviderDispatchIntent, ProviderObservationPort, current_provider_admission,
};
use zeroclaw_api::session_events::*;

// A synchronous response API can consume an asynchronous wire protocol. The
// existing dispatch owns the sole recorder; the adapter only lends observed
// observations to it. Nested composite dispatches mask their parent's scope.
tokio::task_local! {
    static ADAPTER_CAPTURE: Arc<tokio::sync::Mutex<Option<Recorder>>>;
}

pub(super) async fn scope_adapter_capture<F: Future>(
    recorder: &mut Option<Recorder>,
    future: F,
) -> F::Output {
    let bound = Arc::new(tokio::sync::Mutex::new(recorder.take()));
    let result = ADAPTER_CAPTURE.scope(bound.clone(), future).await;
    *recorder = bound.lock().await.take();
    result
}

pub(crate) async fn observe_adapter_event(event: &StreamEvent) -> anyhow::Result<()> {
    if let Ok(bound) = ADAPTER_CAPTURE.try_with(Arc::clone)
        && let Some(recorder) = bound.lock().await.as_mut()
    {
        anyhow::ensure!(
            !matches!(event, StreamEvent::Final),
            "adapter_cannot_settle_dispatch"
        );
        recorder.event(event).await.map_err(stop)?;
    }
    Ok(())
}

pub(crate) async fn observe_adapter_response(
    response: &ChatResponse,
    streamed: bool,
) -> anyhow::Result<()> {
    if let Ok(bound) = ADAPTER_CAPTURE.try_with(Arc::clone)
        && let Some(recorder) = bound.lock().await.as_mut()
    {
        // Streamed leaves already have durable byte references. JSON replies
        // have no earlier checkpoints and enter the same semantic port here.
        if !streamed {
            for block in &response.output_blocks {
                recorder
                    .event(&StreamEvent::OutputBlock(block.clone()))
                    .await
                    .map_err(stop)?;
            }
            if !response.output_blocks.iter().any(|block| {
                matches!(
                    block,
                    zeroclaw_api::model_provider::ProviderOutputBlock::Text { .. }
                )
            }) && let Some(text) = &response.text
            {
                recorder
                    .event(&StreamEvent::TextDelta(
                        zeroclaw_api::model_provider::StreamChunk::delta(text.clone()),
                    ))
                    .await
                    .map_err(stop)?;
            }
        }
        if !streamed {
            use zeroclaw_api::model_provider::ProviderOutputBlock;
            if !response
                .output_blocks
                .iter()
                .any(|b| matches!(b, ProviderOutputBlock::Reasoning { .. }))
                && let Some(reasoning) = &response.reasoning_content
            {
                recorder
                    .event(&StreamEvent::ReasoningFinalized(reasoning.clone()))
                    .await
                    .map_err(stop)?;
            }
            if !response
                .output_blocks
                .iter()
                .any(|b| matches!(b, ProviderOutputBlock::ToolCall { .. }))
            {
                for call in &response.tool_calls {
                    recorder
                        .event(&StreamEvent::OutputBlock(ProviderOutputBlock::ToolCall {
                            call: call.clone(),
                        }))
                        .await
                        .map_err(stop)?;
                }
            }
        }
        if let Some(usage) = &response.usage {
            recorder
                .event(&StreamEvent::Usage(usage.clone()))
                .await
                .map_err(stop)?;
        }
        recorder.adapter_response_observed = true;
    }
    Ok(())
}

fn metadata_codec(metadata: &serde_json::Value) -> &'static str {
    // Protocol values are received content, never storage graph structure.
    // Even a wire field named payload_id must remain ordinary provider data.
    if is_content_metadata(metadata)
        || (metadata.get("kind").and_then(serde_json::Value::as_str) == Some("partial_content")
            && metadata
                .get("component")
                .and_then(serde_json::Value::as_str)
                == Some("reasoning"))
    {
        "session.reasoning/v1"
    } else {
        "session.bytes/v1"
    }
}

fn is_content_metadata(metadata: &serde_json::Value) -> bool {
    matches!(
        metadata.get("kind").and_then(serde_json::Value::as_str),
        Some("content_metadata" | "provider_item")
    )
}

fn content_metadata_block(
    metadata: &serde_json::Value,
    record: &PayloadRecord,
) -> Option<ResponseBlock> {
    is_content_metadata(metadata).then(|| ResponseBlock::Other {
        type_name: "provider.content_metadata".into(),
        classified_body_ref: StoredPayloadRef::from(record),
    })
}

pub(super) fn intent(
    provider: &dyn ModelProvider,
    model: &str,
    messages: &[ChatMessage],
    tools: Option<serde_json::Value>,
    temperature: Option<f64>,
    thinking: Option<zeroclaw_api::model_provider::NativeThinkingParams>,
) -> ProviderDispatchIntent {
    ProviderDispatchIntent {
        requested_route: None,
        previous_attempt_event_id: None,
        provider_ref: provider_reference(provider),
        model: model.into(),
        messages: messages.to_vec(),
        tools,
        temperature,
        thinking,
        tool_round_policy: zeroclaw_api::model_provider::ToolRoundPolicy::Batch,
    }
}

pub(super) struct Recorder {
    port: Arc<dyn ProviderObservationPort>,
    blocks: Vec<CapturedSegment>,
    last_progress: Option<EventId>,
    captured_bytes: usize,
    usage_ref: Option<StoredPayloadRef>,
    finish_ref: Option<StoredPayloadRef>,
    served_ref: Option<StoredPayloadRef>,
    adapter_response_observed: bool,
    // A one-event decoding cursor, indexing the canonical block without
    // retaining another copy of signed bytes.
    pending_signed_projection: Option<usize>,
    pending_position: Option<zeroclaw_api::model_provider::ProviderContentPosition>,
}

struct CapturedSegment {
    block: ResponseBlock,
    position: Option<zeroclaw_api::model_provider::ProviderContentPosition>,
    // Transient coordinate cursor derived from the captured bytes, never a
    // field in the durable payload reference.
    component_end: u64,
}

fn payload_len(payloads: &[PayloadRecord], reference: &StoredPayloadRef) -> anyhow::Result<u64> {
    let payload = payloads
        .iter()
        .find(|payload| payload.payload_id == reference.payload_id)
        .ok_or_else(|| anyhow::Error::msg("session_capture_payload_missing"))?;
    u64::try_from(payload.body.len())
        .map_err(|_| anyhow::Error::msg("session_capture_payload_size_overflow"))
}

impl Recorder {
    fn retain_block(&mut self, block: ResponseBlock) -> anyhow::Result<()> {
        // A byte limit alone cannot bound millions of one-byte deltas and
        // their reference envelopes. Stop capture before that index grows.
        anyhow::ensure!(
            self.blocks.len() < 16_384,
            "session_capture_block_limit_exceeded"
        );
        self.blocks.push(CapturedSegment {
            block,
            position: None,
            component_end: 0,
        });
        Ok(())
    }
    fn ordered_blocks(&self) -> Vec<ResponseBlock> {
        let mut segments: Vec<_> = self.blocks.iter().collect();
        order_provider_segments(&mut segments, |segment| segment.position.as_ref());
        segments
            .into_iter()
            .map(|segment| segment.block.clone())
            .collect()
    }
    pub(super) async fn begin(
        provider: &dyn ModelProvider,
        mut intent: ProviderDispatchIntent,
    ) -> anyhow::Result<Option<Self>> {
        if provider.is_dispatch_composite() {
            return Ok(None);
        }
        let Some(port) = current_provider_admission() else {
            return Ok(None);
        };
        if let Some((provider_ref, model)) = accounting::current_exact_route() {
            intent.provider_ref = provider_ref;
            intent.model = model;
        }
        (intent.requested_route, intent.previous_attempt_event_id) =
            accounting::capture_route_and_previous();
        let port = port.admit(intent).await?;
        Ok(Some(Self {
            port,
            blocks: Vec::new(),
            last_progress: None,
            captured_bytes: 0,
            usage_ref: None,
            finish_ref: None,
            served_ref: None,
            adapter_response_observed: false,
            pending_signed_projection: None,
            pending_position: None,
        }))
    }

    fn leaf(
        &mut self,
        codec: &str,
        bytes: Vec<u8>,
        payloads: &mut Vec<PayloadRecord>,
    ) -> anyhow::Result<StoredPayloadRef> {
        let bytes = match codec {
            // Provider continuation is already a protocol content surface.
            // Its signed/opaque bytes must survive a request round trip exactly.
            "session.reasoning/v1" => bytes,
            "session.text/v1"
            | "session.error-detail/v1"
            | "session.tool-result/v1"
            | "session.locator/v1" => {
                let text = std::str::from_utf8(&bytes)?;
                text.to_owned().into_bytes()
            }
            "session.tool-arguments/v1" => {
                let arguments: serde_json::Value = serde_json::from_slice(&bytes)?;
                canonical_json(&arguments)?
            }
            _ => bytes,
        };
        let mut bytes = bytes;
        if codec == "session.error-detail/v1" {
            // Diagnostics have a separate bounded reserve so exhausting the
            // content budget cannot discard the reason capture stopped.
            if bytes.len() > 64 * 1024 {
                let text = std::str::from_utf8(&bytes)?;
                let mut end = 64 * 1024;
                while !text.is_char_boundary(end) {
                    end -= 1;
                }
                bytes.truncate(end);
                bytes.extend_from_slice(b" [truncated]");
            }
        } else {
            self.charge(bytes.len())?;
        }
        let payload = PayloadRecord::new(PayloadId::new(), codec.into(), bytes)?;
        let reference = StoredPayloadRef::from(&payload);
        payloads.push(payload);
        Ok(reference)
    }

    fn charge(&mut self, byte_len: usize) -> anyhow::Result<()> {
        let next = self
            .captured_bytes
            .checked_add(byte_len)
            .ok_or_else(|| anyhow::Error::msg("session_capture_size_overflow"))?;
        anyhow::ensure!(next <= 32 * 1024 * 1024, "session_capture_limit_exceeded");
        self.captured_bytes = next;
        Ok(())
    }

    fn text(
        &mut self,
        text: &str,
        payloads: &mut Vec<PayloadRecord>,
    ) -> anyhow::Result<ResponseBlock> {
        Ok(ResponseBlock::Text {
            text_ref: self.leaf("session.text/v1", text.as_bytes().to_vec(), payloads)?,
        })
    }

    fn reasoning(
        &mut self,
        text: &str,
        finalized: bool,
        payloads: &mut Vec<PayloadRecord>,
    ) -> anyhow::Result<ResponseBlock> {
        Ok(ResponseBlock::Reasoning {
            representation_kind: if finalized {
                "provider_replay"
            } else {
                "human_readable_delta"
            }
            .into(),
            payload_ref: self.leaf(
                "session.reasoning/v1",
                text.to_owned().into_bytes(),
                payloads,
            )?,
            completion: if finalized {
                ContentCompletion::Complete
            } else {
                ContentCompletion::Partial
            },
            replay_requirement: if finalized {
                "provider_defined"
            } else {
                "display_only"
            }
            .into(),
        })
    }

    fn tool_call(
        &mut self,
        call: &zeroclaw_api::model_provider::ToolCall,
        payloads: &mut Vec<PayloadRecord>,
    ) -> anyhow::Result<ResponseBlock> {
        let args = self.leaf(
            if serde_json::from_str::<serde_json::Value>(&call.arguments).is_ok() {
                "session.tool-arguments/v1"
            } else {
                "session.text/v1"
            },
            call.arguments.as_bytes().to_vec(),
            payloads,
        )?;
        let opaque = match &call.extra_content {
            Some(value) => {
                Some(self.leaf("session.reasoning/v1", canonical_json(value)?, payloads)?)
            }
            None => None,
        };
        let record = PayloadRecord::json(
            "session.tool-call/v1",
            &serde_json::json!({"provider_call_id":call.id,"name":call.name,"arguments_ref":args,"extension_ref":opaque}),
        )?;
        let block = ResponseBlock::ToolCall {
            tool_call_ref: StoredPayloadRef::from(&record),
        };
        payloads.push(record);
        Ok(block)
    }

    pub(super) async fn response(&mut self, response: &ChatResponse) -> anyhow::Result<()> {
        let prior = (
            self.blocks.len(),
            self.captured_bytes,
            self.finish_ref.clone(),
            self.usage_ref.clone(),
            self.served_ref.clone(),
        );
        let result = self.response_inner(response).await;
        if result.is_err() {
            self.blocks.truncate(prior.0);
            self.captured_bytes = prior.1;
            self.finish_ref = prior.2;
            self.usage_ref = prior.3;
            self.served_ref = prior.4;
        }
        result
    }

    async fn response_inner(&mut self, response: &ChatResponse) -> anyhow::Result<()> {
        if self.adapter_response_observed {
            return self.complete(Vec::new(), self.usage_ref.clone()).await;
        }
        let mut payloads = Vec::new();
        for block in &response.output_blocks {
            if let zeroclaw_api::model_provider::ProviderOutputBlock::ProtocolMetadata {
                metadata,
            } = block
            {
                let record = PayloadRecord::json(metadata_codec(metadata), metadata)?;
                self.charge(record.body.len())?;
                if metadata.get("kind").and_then(serde_json::Value::as_str)
                    == Some("served_attribution")
                {
                    self.served_ref = Some(StoredPayloadRef::from(&record));
                }
                if metadata.get("kind").and_then(serde_json::Value::as_str) == Some("finish")
                    || (self.finish_ref.is_none()
                        && metadata.get("kind").and_then(serde_json::Value::as_str)
                            == Some("completion_signal"))
                {
                    self.finish_ref = Some(StoredPayloadRef::from(&record));
                }
                if let Some(block) = content_metadata_block(metadata, &record) {
                    self.retain_block(block)?;
                }
                payloads.push(record);
            } else {
                let block = self.output_block(block, &mut payloads)?;
                self.retain_block(block)?;
            }
        }
        use zeroclaw_api::model_provider::ProviderOutputBlock;
        // Legacy projections may accompany a partial ordered output. Fill
        // missing categories once; never duplicate a category already captured.
        if !response
            .output_blocks
            .iter()
            .any(|b| matches!(b, ProviderOutputBlock::Reasoning { .. }))
            && let Some(reasoning) = &response.reasoning_content
        {
            let block = self.reasoning(reasoning, true, &mut payloads)?;
            self.retain_block(block)?;
        }
        if !response
            .output_blocks
            .iter()
            .any(|b| matches!(b, ProviderOutputBlock::Text { .. }))
            && let Some(text) = &response.text
        {
            let block = self.text(text, &mut payloads)?;
            self.retain_block(block)?;
        }
        if !response
            .output_blocks
            .iter()
            .any(|b| matches!(b, ProviderOutputBlock::ToolCall { .. }))
        {
            for call in &response.tool_calls {
                let block = self.tool_call(call, &mut payloads)?;
                self.retain_block(block)?;
            }
        }
        let usage = if let Some(usage) = &response.usage {
            let record = usage_payload(usage)?;
            let reference = StoredPayloadRef::from(&record);
            payloads.push(record);
            Some(reference)
        } else {
            None
        };
        self.complete(payloads, usage).await
    }

    fn output_block(
        &mut self,
        block: &zeroclaw_api::model_provider::ProviderOutputBlock,
        payloads: &mut Vec<PayloadRecord>,
    ) -> anyhow::Result<ResponseBlock> {
        use zeroclaw_api::model_provider::{ProviderMediaSource, ProviderOutputBlock};
        Ok(match block {
            ProviderOutputBlock::ProtocolMetadata { .. } => {
                anyhow::bail!("session_protocol_metadata_is_not_content")
            }
            ProviderOutputBlock::Text { text } => self.text(text, payloads)?,
            ProviderOutputBlock::Reasoning {
                representation,
                bytes,
                replay_requirement,
            } => ResponseBlock::Reasoning {
                representation_kind: representation.clone(),
                payload_ref: self.leaf("session.reasoning/v1", bytes.clone(), payloads)?,
                completion: ContentCompletion::Complete,
                replay_requirement: replay_requirement.clone(),
            },
            ProviderOutputBlock::ToolCall { call } => self.tool_call(call, payloads)?,
            ProviderOutputBlock::Media {
                media_type,
                format,
                source,
            } => {
                let descriptor = PayloadRecord::json(
                    "session.evidence/v1",
                    &serde_json::json!({"media_type":media_type,"format":format}),
                )?;
                let media_descriptor_ref = StoredPayloadRef::from(&descriptor);
                payloads.push(descriptor);
                let received = match source {
                    ProviderMediaSource::Bytes { bytes } => MediaReceived::Captured {
                        representation_ref: self.leaf(
                            "session.bytes/v1",
                            bytes.clone(),
                            payloads,
                        )?,
                    },
                    ProviderMediaSource::Reference {
                        locator,
                        expires_at,
                    } => MediaReceived::ReferenceOnly {
                        locator_ref: self.leaf(
                            "session.locator/v1",
                            locator.as_bytes().to_vec(),
                            payloads,
                        )?,
                        expiry: *expires_at,
                    },
                };
                ResponseBlock::Media {
                    media_descriptor_ref,
                    received,
                }
            }
            ProviderOutputBlock::Other { type_name, content } => ResponseBlock::Other {
                type_name: type_name.clone(),
                classified_body_ref: self.leaf(
                    "session.reasoning/v1",
                    canonical_json(&content.clone())?,
                    payloads,
                )?,
            },
        })
    }

    pub(super) async fn text_response(&mut self, text: &str) -> anyhow::Result<()> {
        if self.adapter_response_observed {
            return self.complete(Vec::new(), self.usage_ref.clone()).await;
        }
        let mut payloads = Vec::new();
        let block = self.text(text, &mut payloads)?;
        self.retain_block(block)?;
        self.complete(payloads, None).await
    }

    async fn complete(
        &mut self,
        mut payloads: Vec<PayloadRecord>,
        usage: Option<StoredPayloadRef>,
    ) -> anyhow::Result<()> {
        if self.finish_ref.is_none() {
            let evidence = PayloadRecord::json(
                "session.evidence/v1",
                &serde_json::json!({"observed":"adapter_response_returned"}),
            )?;
            self.finish_ref = Some(StoredPayloadRef::from(&evidence));
            payloads.push(evidence);
        }
        let record = PayloadRecord::json(
            "session.response/v1",
            &ProviderResponsePayload {
                blocks: self.ordered_blocks(),
                completion: ContentCompletion::Complete,
                finish_evidence_ref: self.finish_ref.clone(),
                usage_ref: usage,
                reasoning_absence: if self
                    .blocks
                    .iter()
                    .any(|segment| matches!(segment.block, ResponseBlock::Reasoning { .. }))
                {
                    None
                } else {
                    Some("not_returned".into())
                },
            },
        )?;
        let response_ref = StoredPayloadRef::from(&record);
        payloads.push(record);
        let receipt = self
            .port
            .observe(
                EventData::ProviderAttemptCompleted {
                    response_ref,
                    served_attribution_ref: self.served_ref.clone(),
                    last_progress_event_id: self.last_progress.clone(),
                },
                payloads,
            )
            .await?;
        if let Some(id) = receipt.event_ids.last() {
            accounting::record_capture_outcome(id.clone());
        }
        Ok(())
    }

    pub(super) async fn error(&mut self, error: &anyhow::Error) -> anyhow::Result<bool> {
        let mut payloads = Vec::new();
        let http = error
            .chain()
            .find_map(|cause| cause.downcast_ref::<super::ProviderHttpFailure>());
        let protocol = error.chain().find_map(|cause| {
            cause
                .downcast_ref::<super::ProviderProtocolFailure>()
                .or_else(|| crate::grok_cli::protocol_failure(cause))
        });
        let process = error
            .chain()
            .find_map(|cause| cause.downcast_ref::<crate::cli_output::ProcessFailure>());
        let panic = error
            .chain()
            .any(|cause| cause.is::<super::ProviderAdapterPanic>());
        let transport = error
            .chain()
            .find_map(|cause| cause.downcast_ref::<reqwest::Error>());
        let stream_error = error
            .chain()
            .find_map(|cause| cause.downcast_ref::<zeroclaw_api::model_provider::StreamError>());
        let stream_http = match stream_error {
            Some(zeroclaw_api::model_provider::StreamError::HttpStatus { status, .. }) => {
                Some(*status)
            }
            _ => None,
        };
        let missing_final = matches!(
            stream_error,
            Some(zeroclaw_api::model_provider::StreamError::MissingFinal(_))
        ) || error.to_string() == "stream_eof_without_final";
        let refusal = matches!(
            stream_error,
            Some(zeroclaw_api::model_provider::StreamError::ModelRefusal(_))
        ) || error
            .chain()
            .any(|cause| cause.is::<zeroclaw_api::model_provider::ModelRefusalError>());
        let definite = http.is_some() || stream_http.is_some() || protocol.is_some() || refusal;
        let connect = transport.is_some_and(reqwest::Error::is_connect)
            || matches!(
                stream_error,
                Some(zeroclaw_api::model_provider::StreamError::ConnectFailed(_))
            );
        let decode = transport.is_some_and(reqwest::Error::is_decode)
            || error.chain().any(|cause| {
                cause.is::<serde_json::Error>()
                    || cause.is::<std::str::Utf8Error>()
                    || cause.is::<std::string::FromUtf8Error>()
            })
            || matches!(
                stream_error,
                Some(
                    zeroclaw_api::model_provider::StreamError::Json(_)
                        | zeroclaw_api::model_provider::StreamError::InvalidSse(_)
                )
            );
        let detail = if let Some(http) = http {
            DiagnosticDetail::Available {
                detail_ref: self.leaf(
                    "session.error-detail/v1",
                    http.detail.as_bytes().to_vec(),
                    &mut payloads,
                )?,
            }
        } else if let Some(protocol) = protocol {
            DiagnosticDetail::Available {
                detail_ref: self.leaf(
                    "session.error-detail/v1",
                    canonical_json(protocol.error())?,
                    &mut payloads,
                )?,
            }
        } else if let Some(process) = process {
            DiagnosticDetail::Available {
                detail_ref: self.leaf(
                    "session.error-detail/v1",
                    canonical_json(&process.detail())?,
                    &mut payloads,
                )?,
            }
        } else if let Some(zeroclaw_api::model_provider::StreamError::InvalidSse(detail)) =
            stream_error
        {
            DiagnosticDetail::Available {
                detail_ref: self.leaf(
                    "session.error-detail/v1",
                    detail.as_bytes().to_vec(),
                    &mut payloads,
                )?,
            }
        } else if !panic {
            DiagnosticDetail::Available {
                detail_ref: self.leaf(
                    "session.error-detail/v1",
                    format!("{error:#}").into_bytes(),
                    &mut payloads,
                )?,
            }
        } else {
            DiagnosticDetail::Unavailable {
                reason_key: "panic_payload_is_untrusted".into(),
            }
        };
        // The provider response body is the canonical source for protocol codes.
        // Retain codes only when the provider actually supplied them.
        let body = http
            .map(|http| http.detail.as_str())
            .or(match stream_error {
                Some(zeroclaw_api::model_provider::StreamError::HttpStatus { message, .. }) => {
                    Some(message.as_str())
                }
                _ => None,
            });
        let provider_code = protocol
            .and_then(super::ProviderProtocolFailure::code)
            .or_else(|| {
                body.and_then(|body| {
                    let body: serde_json::Value = serde_json::from_str(body).ok()?;
                    body.pointer("/error/code")
                        .or_else(|| body.pointer("/error/type"))
                        .or_else(|| body.get("code"))
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_owned)
                })
            });
        let context_limit = provider_code.as_deref().is_some_and(|code| {
            matches!(
                code,
                "context_length_exceeded" | "context_window_exceeded" | "prompt_too_long"
            )
        });
        let diagnostic = PayloadRecord::json(
            "session.diagnostic/v1",
            &ProviderDiagnostic {
                error_key: if error.to_string().starts_with("session_capture_") {
                    "provider_capture_limit"
                } else if panic {
                    "provider_adapter_panicked"
                } else if connect {
                    "provider_connect_error"
                } else if transport.is_some_and(reqwest::Error::is_timeout)
                    || matches!(
                        stream_error,
                        Some(zeroclaw_api::model_provider::StreamError::ResponseTimeout(
                            _
                        ))
                    )
                {
                    "provider_response_timeout"
                } else if decode {
                    "provider_decode_error"
                } else if context_limit {
                    "provider_context_limit"
                } else if refusal {
                    "provider_refused"
                } else if missing_final {
                    "provider_missing_final"
                } else if process.is_some_and(crate::cli_output::ProcessFailure::is_exit) {
                    "provider_process_exit"
                } else if http.is_some() || stream_http.is_some() {
                    "provider_http_error"
                } else if protocol.is_some() {
                    "provider_protocol_error"
                } else {
                    "provider_outcome_unresolved"
                }
                .into(),
                stage: if error.to_string().starts_with("session_capture_") {
                    DiagnosticStage::Capture
                } else if connect {
                    DiagnosticStage::Connect
                } else if decode {
                    DiagnosticStage::Decode
                } else if !self.blocks.is_empty() || missing_final {
                    DiagnosticStage::ConsumeStream
                } else {
                    DiagnosticStage::AwaitResponse
                },
                detail,
                protocol_evidence: http
                    .map(|http| http.status)
                    .or(stream_http)
                    .map(|status| {
                        vec![ProtocolEvidence::Http {
                            status_code: status,
                            provider_error_code: provider_code.clone(),
                            provider_request_id: http
                                .and_then(|failure| failure.request_id.clone()),
                            retry_after: http.and_then(|failure| failure.retry_after.clone()),
                        }]
                    })
                    .unwrap_or_else(|| {
                        if let Some(process) = process {
                            return process
                                .status
                                .map(|status| {
                                    vec![ProtocolEvidence::Transport {
                                        code: format!("{}: {status}", process.protocol),
                                    }]
                                })
                                .unwrap_or_default();
                        }
                        protocol
                            .map(|protocol| {
                                vec![ProtocolEvidence::StreamTermination {
                                    evidence: protocol
                                        .code()
                                        .map(|code| format!("provider_error_code={code}"))
                                        .unwrap_or_else(|| "provider_error_frame".into()),
                                }]
                            })
                            .unwrap_or_default()
                    }),
            },
        )?;
        let error_ref = StoredPayloadRef::from(&diagnostic);
        payloads.push(diagnostic);
        let partial_response_ref = if self.blocks.is_empty() {
            None
        } else {
            let record = PayloadRecord::json(
                "session.response/v1",
                &ProviderResponsePayload {
                    blocks: self.ordered_blocks(),
                    completion: ContentCompletion::Partial,
                    finish_evidence_ref: self.finish_ref.clone(),
                    usage_ref: self.usage_ref.clone(),
                    reasoning_absence: None,
                },
            )?;
            let reference = StoredPayloadRef::from(&record);
            payloads.push(record);
            Some(reference)
        };
        if definite {
            let receipt = self
                .port
                .observe(
                    EventData::ProviderAttemptFailed {
                        error_ref,
                        partial_response_ref,
                        served_attribution_ref: self.served_ref.clone(),
                        last_progress_event_id: self.last_progress.clone(),
                    },
                    payloads,
                )
                .await?;
            if let Some(id) = receipt.event_ids.last() {
                accounting::record_capture_outcome(id.clone());
            }
        } else {
            let observation = PayloadRecord::json(
                "session.provider-observation/v1",
                &ProviderObservationPayload {
                    operations: vec![ProviderObservation::DiagnosticObserved {
                        error_ref: error_ref.clone(),
                    }],
                },
            )?;
            let observation_ref = StoredPayloadRef::from(&observation);
            payloads.push(observation);
            let receipt = self
                .port
                .observe(
                    EventData::ProviderResponseProgressObserved { observation_ref },
                    payloads,
                )
                .await?;
            self.last_progress = receipt.event_ids.last().cloned();
            self.port
                .observe(
                    EventData::InvocationOutcomeUnknown {
                        last_observed_event_id: self
                            .last_progress
                            .clone()
                            .unwrap_or_else(|| self.port.started_event_id()),
                        reason_key: "provider_outcome_unresolved".into(),
                        observed_partial_ref: partial_response_ref,
                        error_ref: Some(error_ref),
                    },
                    Vec::new(),
                )
                .await?;
        }
        Ok(definite)
    }

    async fn event(&mut self, event: &StreamEvent) -> anyhow::Result<()> {
        if let StreamEvent::ContentPosition(position) = event {
            anyhow::ensure!(
                self.pending_position.is_none(),
                "session_capture_unconsumed_protocol_position"
            );
            self.pending_position = Some(position.clone());
            return Ok(());
        }
        let position = self.pending_position.take();
        let signed_projection = self.pending_signed_projection.take();
        let prior = (
            self.blocks.len(),
            self.captured_bytes,
            self.finish_ref.clone(),
            self.usage_ref.clone(),
        );
        let result = self.event_inner(event, signed_projection, position).await;
        if result.is_err() {
            self.blocks.truncate(prior.0);
            self.captured_bytes = prior.1;
            self.finish_ref = prior.2;
            self.usage_ref = prior.3;
        } else if matches!(
            event,
            StreamEvent::OutputBlock(
                zeroclaw_api::model_provider::ProviderOutputBlock::Reasoning {
                    representation,
                    replay_requirement,
                    ..
                }
            ) if matches!(representation.as_str(), crate::anthropic::SIGNED_REPLAY_REPRESENTATION | "redacted_thinking")
                && replay_requirement == "provider_defined"
        ) {
            self.pending_signed_projection = self.blocks.len().checked_sub(1);
        }
        result
    }

    async fn event_inner(
        &mut self,
        event: &StreamEvent,
        signed_projection: Option<usize>,
        position: Option<zeroclaw_api::model_provider::ProviderContentPosition>,
    ) -> anyhow::Result<()> {
        if matches!(event, StreamEvent::Final) {
            return self.complete(Vec::new(), self.usage_ref.clone()).await;
        }
        let mut payloads = Vec::new();
        let mut blocks = Vec::new();
        let mut operations = Vec::new();
        match event {
            StreamEvent::ProtocolMetadata(metadata)
            | StreamEvent::OutputBlock(
                zeroclaw_api::model_provider::ProviderOutputBlock::ProtocolMetadata { metadata },
            ) => {
                let mut metadata = metadata.clone();
                if is_content_metadata(&metadata)
                    && let Some(position) = &position
                {
                    metadata["position"] = serde_json::to_value(position)?;
                }
                let metadata = &metadata;
                let record = PayloadRecord::json(metadata_codec(metadata), metadata)?;
                self.charge(record.body.len())?;
                if metadata.get("kind").and_then(serde_json::Value::as_str)
                    == Some("served_attribution")
                {
                    self.served_ref = Some(StoredPayloadRef::from(&record));
                }
                if metadata.get("kind").and_then(serde_json::Value::as_str) == Some("finish")
                    || (self.finish_ref.is_none()
                        && metadata.get("kind").and_then(serde_json::Value::as_str)
                            == Some("completion_signal"))
                {
                    self.finish_ref = Some(StoredPayloadRef::from(&record));
                }
                if metadata.get("kind").and_then(serde_json::Value::as_str)
                    == Some("provider_tool_activity")
                {
                    let activity = metadata.get("activity").unwrap_or(metadata);
                    operations.push(ProviderObservation::ProviderToolActivityObserved {
                        provider_call_id: metadata
                            .get("provider_call_id")
                            .or_else(|| activity.get("call_id"))
                            .or_else(|| activity.get("item_id"))
                            .or_else(|| activity.get("id"))
                            .and_then(serde_json::Value::as_str)
                            .map(str::to_owned),
                        activity_ref: StoredPayloadRef::from(&record),
                    });
                    blocks.push(ResponseBlock::Other {
                        type_name: "provider_tool_activity".into(),
                        classified_body_ref: StoredPayloadRef::from(&record),
                    });
                } else {
                    if let Some(block) = content_metadata_block(metadata, &record) {
                        blocks.push(block);
                    }
                    if metadata.get("kind").and_then(serde_json::Value::as_str)
                        == Some("partial_content")
                    {
                        let type_name = metadata
                            .get("type_name")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or("protocol.partial_content");
                        if record.codec == "session.reasoning/v1" {
                            blocks.push(ResponseBlock::Reasoning {
                                representation_kind: type_name.into(),
                                payload_ref: StoredPayloadRef::from(&record),
                                completion: ContentCompletion::Partial,
                                replay_requirement: "display_only".into(),
                            });
                        } else {
                            blocks.push(ResponseBlock::Other {
                                type_name: type_name.into(),
                                classified_body_ref: StoredPayloadRef::from(&record),
                            });
                        }
                    }
                    operations.push(ProviderObservation::MetadataObserved {
                        metadata_ref: StoredPayloadRef::from(&record),
                    });
                }
                payloads.push(record);
            }
            StreamEvent::OutputBlock(block) => {
                blocks.push(self.output_block(block, &mut payloads)?)
            }
            StreamEvent::TextDelta(chunk) => {
                if let Some(reasoning) = &chunk.reasoning {
                    blocks.push(self.reasoning(reasoning, false, &mut payloads)?);
                }
                if !chunk.delta.is_empty() {
                    blocks.push(self.text(&chunk.delta, &mut payloads)?);
                }
            }
            StreamEvent::ThinkingDelta(text) => {
                blocks.push(self.reasoning(text, false, &mut payloads)?)
            }
            StreamEvent::ReasoningFinalized(text) => {
                let already_captured = matches!(
                    signed_projection
                        .and_then(|index| self.blocks.get(index))
                        .map(|segment| &segment.block),
                    Some(ResponseBlock::Reasoning {
                        representation_kind,
                        completion: ContentCompletion::Complete,
                        replay_requirement,
                        ..
                    }) if replay_requirement == "provider_defined"
                        && (representation_kind == crate::anthropic::SIGNED_REPLAY_REPRESENTATION
                            || representation_kind == "redacted_thinking")
                );
                if !already_captured {
                    blocks.push(self.reasoning(text, true, &mut payloads)?);
                }
            }
            StreamEvent::ToolCall(call) => blocks.push(self.tool_call(call, &mut payloads)?),
            StreamEvent::Usage(usage) => {
                let record = usage_payload(usage)?;
                self.usage_ref = Some(StoredPayloadRef::from(&record));
                operations.push(ProviderObservation::MetadataObserved {
                    metadata_ref: StoredPayloadRef::from(&record),
                });
                payloads.push(record);
            }
            StreamEvent::PreExecutedToolCall {
                provider_call_id,
                name,
                args,
            } => {
                let args = self.leaf(
                    if serde_json::from_str::<serde_json::Value>(args).is_ok() {
                        "session.tool-arguments/v1"
                    } else {
                        "session.text/v1"
                    },
                    args.as_bytes().to_vec(),
                    &mut payloads,
                )?;
                let record = PayloadRecord::json(
                    "session.evidence/v1",
                    &serde_json::json!({"kind":"provider_tool_call","name":name,"arguments_ref":args}),
                )?;
                operations.push(ProviderObservation::ProviderToolActivityObserved {
                    provider_call_id: provider_call_id.clone(),
                    activity_ref: StoredPayloadRef::from(&record),
                });
                blocks.push(ResponseBlock::Other {
                    type_name: "provider_tool_call".into(),
                    classified_body_ref: StoredPayloadRef::from(&record),
                });
                payloads.push(record);
            }
            StreamEvent::PreExecutedToolResult {
                provider_call_id,
                name,
                output,
            } => {
                let output = self.leaf(
                    "session.tool-result/v1",
                    output.as_bytes().to_vec(),
                    &mut payloads,
                )?;
                let record = PayloadRecord::json(
                    "session.evidence/v1",
                    &serde_json::json!({"kind":"provider_tool_result","name":name,"result_ref":output}),
                )?;
                operations.push(ProviderObservation::ProviderToolActivityObserved {
                    provider_call_id: provider_call_id.clone(),
                    activity_ref: StoredPayloadRef::from(&record),
                });
                blocks.push(ResponseBlock::Other {
                    type_name: "provider_tool_result".into(),
                    classified_body_ref: StoredPayloadRef::from(&record),
                });
                payloads.push(record);
            }
            StreamEvent::Final | StreamEvent::ContentPosition(_) => {}
        }
        for block in blocks {
            let index = self.blocks.len();
            let record = PayloadRecord::json("session.evidence/v1", &block)?;
            let reference = StoredPayloadRef::from(&record);
            if let Some(position) = &position {
                let (component, _) = block.component();
                let byte_offset = self
                    .blocks
                    .iter()
                    .filter(|segment| {
                        segment.position.as_ref() == Some(position)
                            && segment.block.component().0 == component
                    })
                    .map(|segment| segment.component_end)
                    .next_back()
                    .unwrap_or(0);
                let component_end = byte_offset
                    .checked_add(payload_len(&payloads, block.component().1)?)
                    .ok_or_else(|| {
                        anyhow::Error::msg("session_capture_component_offset_overflow")
                    })?;
                operations.push(ProviderObservation::ProtocolSegmentObserved {
                    position: position.clone(),
                    component: component.into(),
                    byte_offset,
                    segment_ref: reference,
                });
                payloads.push(record);
                self.retain_block(block)?;
                if let Some(segment) = self.blocks.last_mut() {
                    segment.position = Some(position.clone());
                    segment.component_end = component_end;
                }
                continue;
            }
            operations.push(ProviderObservation::BlockOpened {
                block_index: index,
                descriptor_ref: reference.clone(),
            });
            // Each adapter-emitted segment has its own stable coordinate.
            // Final response references the same bytes rather than a second
            // accumulated copy; signed finalized reasoning remains separate.
            let component = match &block {
                ResponseBlock::Text { text_ref } => Some(("text", text_ref)),
                ResponseBlock::Reasoning { payload_ref, .. } => Some(("reasoning", payload_ref)),
                ResponseBlock::Media {
                    received: MediaReceived::Captured { representation_ref },
                    ..
                }
                | ResponseBlock::Media {
                    received:
                        MediaReceived::Partial {
                            representation_ref, ..
                        },
                    ..
                } => Some(("media", representation_ref)),
                ResponseBlock::Other {
                    classified_body_ref,
                    ..
                } => Some(("opaque", classified_body_ref)),
                _ => None,
            };
            if let Some((component, bytes_ref)) = component {
                operations.push(ProviderObservation::BytesAppended {
                    block_index: index,
                    component: component.into(),
                    byte_offset: 0,
                    bytes_ref: bytes_ref.clone(),
                });
            }
            operations.push(ProviderObservation::BlockFinalized {
                block_index: index,
                finalized_block_ref: reference,
            });
            payloads.push(record);
            self.retain_block(block)?;
        }
        if !operations.is_empty() {
            let record = PayloadRecord::json(
                "session.provider-observation/v1",
                &ProviderObservationPayload { operations },
            )?;
            let observation_ref = StoredPayloadRef::from(&record);
            payloads.push(record);
            let receipt = self
                .port
                .observe(
                    EventData::ProviderResponseProgressObserved { observation_ref },
                    payloads,
                )
                .await?;
            self.last_progress = receipt.event_ids.last().cloned();
        }
        if matches!(event,StreamEvent::TextDelta(chunk) if chunk.is_final) {
            self.complete(Vec::new(), self.usage_ref.clone()).await?;
        }
        Ok(())
    }
}

/// First poll admits before polling the underlying network stream. Each
/// observed block commits before downstream consumers can display or discard it.
pub(super) fn stream_events(
    inner: stream::BoxStream<'static, StreamResult<StreamEvent>>,
    provider: &dyn ModelProvider,
    intent: ProviderDispatchIntent,
) -> stream::BoxStream<'static, StreamResult<StreamEvent>> {
    if provider.is_dispatch_composite() {
        return inner;
    }
    let Some(admission) = current_provider_admission() else {
        return inner;
    };
    struct State {
        inner: stream::BoxStream<'static, StreamResult<StreamEvent>>,
        admission: Option<(
            Arc<dyn zeroclaw_api::session_events::capture::ProviderAdmissionPort>,
            ProviderDispatchIntent,
        )>,
        recorder: Option<Recorder>,
        stopped: bool,
    }
    let state = State {
        inner,
        admission: Some((admission, intent)),
        recorder: None,
        stopped: false,
    };
    stream::unfold(state, |mut state| async move {
        if state.stopped {
            return None;
        }
        if let Some((port, mut intent)) = state.admission.take() {
            if let Some((provider_ref, model)) = accounting::current_exact_route() {
                intent.provider_ref = provider_ref;
                intent.model = model;
            }
            (intent.requested_route, intent.previous_attempt_event_id) =
                accounting::capture_route_and_previous();
            match port.admit(intent).await {
                Ok(port) => {
                    state.recorder = Some(Recorder {
                        port,
                        blocks: Vec::new(),
                        last_progress: None,
                        captured_bytes: 0,
                        usage_ref: None,
                        finish_ref: None,
                        served_ref: None,
                        adapter_response_observed: false,
                        pending_signed_projection: None,
                        pending_position: None,
                    })
                }
                Err(error) => {
                    state.stopped = true;
                    return Some((Err(capture_error(error)), state));
                }
            }
        }
        use futures_util::FutureExt;
        let item = match std::panic::AssertUnwindSafe(state.inner.next())
            .catch_unwind()
            .await
        {
            Ok(item) => item,
            Err(_) => {
                let error = anyhow::Error::new(super::ProviderAdapterPanic);
                let captured = if let Some(recorder) = &mut state.recorder {
                    recorder.error(&error).await.map(|_| ())
                } else {
                    Ok(())
                };
                state.stopped = true;
                return Some((
                    Err(capture_error(match captured {
                        Ok(()) => stop(error),
                        Err(error) => error,
                    })),
                    state,
                ));
            }
        };
        if let Some(Err(error)) = item {
            let classified = anyhow::Error::new(error);
            let captured = if let Some(recorder) = &mut state.recorder {
                recorder.error(&classified).await
            } else {
                Ok(true)
            };
            state.stopped = true;
            let result = match captured {
                Ok(true) => {
                    match classified.downcast::<zeroclaw_api::model_provider::StreamError>() {
                        Ok(error) => Err(error),
                        Err(error) => Err(capture_error(error)),
                    }
                }
                Ok(false) => Err(capture_error(stop(classified))),
                Err(error) => Err(capture_error(error)),
            };
            return Some((result, state));
        }
        if let Some(recorder) = &mut state.recorder {
            let captured = match &item {
                Some(Ok(event)) => recorder.event(event).await,
                Some(Err(error)) => Err(stop(anyhow::Error::msg(crate::sanitize_api_error(
                    &error.to_string(),
                )))),
                None => {
                    match recorder
                        .error(&anyhow::Error::msg("stream_eof_without_final"))
                        .await
                    {
                        Ok(_) => Err(stop(anyhow::Error::msg("stream_eof_without_final"))),
                        Err(error) => Err(error),
                    }
                }
            };
            if let Err(error) = captured {
                state.stopped = true;
                if error.to_string().starts_with("session_capture_") {
                    // A local capacity limit leaves the remote outcome unknown.
                    // Settle with durable partials before exposing the error.
                    if let Err(capture_failure) = recorder.error(&error).await {
                        return Some((Err(capture_error(capture_failure)), state));
                    }
                }
                return Some((Err(capture_error(error)), state));
            }
            if matches!(item, Some(Ok(StreamEvent::Final)) | Some(Err(_)))
                || matches!(&item,Some(Ok(StreamEvent::TextDelta(chunk))) if chunk.is_final)
            {
                state.recorder = None;
            }
        }
        item.map(|item| (item, state))
    })
    .boxed()
}

pub(super) fn stream_chunks(
    inner: stream::BoxStream<'static, StreamResult<zeroclaw_api::model_provider::StreamChunk>>,
    provider: &dyn ModelProvider,
    intent: ProviderDispatchIntent,
) -> stream::BoxStream<'static, StreamResult<zeroclaw_api::model_provider::StreamChunk>> {
    if provider.is_dispatch_composite() || current_provider_admission().is_none() {
        return inner;
    }
    stream_events(
        inner
            .map(|result| result.map(StreamEvent::TextDelta))
            .boxed(),
        provider,
        intent,
    )
    .map(|result| {
        result.and_then(|event| match event {
            StreamEvent::TextDelta(chunk) => Ok(chunk),
            _ => Err(capture_error(anyhow::Error::msg(
                "invalid captured chunk event",
            ))),
        })
    })
    .boxed()
}

/// Project only after structured dispatch has captured every semantic event.
pub(super) fn project_chunks(
    inner: stream::BoxStream<'static, StreamResult<StreamEvent>>,
    count_tokens: bool,
) -> stream::BoxStream<'static, StreamResult<zeroclaw_api::model_provider::StreamChunk>> {
    use zeroclaw_api::model_provider::{ProviderOutputBlock, StreamChunk};
    stream::unfold((inner, false), move |(mut inner, finished)| async move {
        if finished {
            return None;
        }
        while let Some(result) = inner.next().await {
            let chunk = match result {
                Err(error) => return Some((Err(error), (inner, true))),
                Ok(StreamEvent::Final) => StreamChunk::final_chunk(),
                Ok(StreamEvent::TextDelta(chunk)) => chunk,
                Ok(StreamEvent::ThinkingDelta(reasoning)) => StreamChunk::reasoning(reasoning),
                Ok(StreamEvent::OutputBlock(ProviderOutputBlock::Text { text })) => {
                    let chunk = StreamChunk::delta(text);
                    if count_tokens {
                        chunk.with_token_estimate()
                    } else {
                        chunk
                    }
                }
                Ok(_) => continue,
            };
            let finished = chunk.is_final;
            return Some((Ok(chunk), (inner, finished)));
        }
        None
    })
    .boxed()
}

pub(super) fn stop(error: anyhow::Error) -> anyhow::Error {
    zeroclaw_api::session_events::capture::capture_stopped(error)
}
fn capture_error(error: anyhow::Error) -> zeroclaw_api::model_provider::StreamError {
    zeroclaw_api::model_provider::StreamError::CaptureStopped(
        stop(error).reallocate_into_boxed_dyn_error_without_backtrace(),
    )
}

fn usage_payload(
    usage: &zeroclaw_api::model_provider::TokenUsage,
) -> Result<PayloadRecord, ContractError> {
    PayloadRecord::json(
        "session.evidence/v1",
        &serde_json::json!({"input_tokens":usage.input_tokens,"output_tokens":usage.output_tokens,"cached_input_tokens":usage.cached_input_tokens,"cache_creation_input_tokens":usage.cache_creation_input_tokens}),
    )
}

#[cfg(test)]
mod tests;
