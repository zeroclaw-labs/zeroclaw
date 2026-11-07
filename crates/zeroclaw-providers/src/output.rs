//! Decode provider protocol content only. Transport headers never enter this
//! path, and unknown semantic blocks remain opaque content.
use base64::Engine;
use serde_json::Value;
use zeroclaw_api::model_provider::{ProviderMediaSource, ProviderOutputBlock, ToolCall};

#[cfg(test)]
mod tests;

pub(crate) fn content_block(value: &Value) -> ProviderOutputBlock {
    let kind = value
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    match kind {
        "text" | "output_text" => {
            if let Some(text) = value.get("text").and_then(Value::as_str) {
                return ProviderOutputBlock::Text { text: text.into() };
            }
        }
        "thinking" | "redacted_thinking" | "reasoning" => {
            // Preserve the complete observed content block, including
            // signature/encrypted continuation and omission markers.
            return ProviderOutputBlock::Reasoning {
                representation: kind.into(),
                bytes: value.to_string().into_bytes(),
                replay_requirement: "provider_defined".into(),
            };
        }
        "tool_use" | "function_call" => {
            if let Some(name) = value.get("name").and_then(Value::as_str) {
                return ProviderOutputBlock::ToolCall {
                    call: ToolCall {
                        id: value
                            .get("call_id")
                            .or_else(|| value.get("id"))
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .into(),
                        name: name.into(),
                        arguments: value
                            .get("arguments")
                            .map(|arg| {
                                arg.as_str()
                                    .map(str::to_owned)
                                    .unwrap_or_else(|| arg.to_string())
                            })
                            .or_else(|| value.get("input").map(Value::to_string))
                            .unwrap_or_else(|| "{}".into()),
                        extra_content: value.get("extra_content").cloned(),
                    },
                };
            }
        }
        "image"
        | "image_url"
        | "output_image"
        | "image_generation_call"
        | "audio"
        | "output_audio"
        | "video"
        | "output_video" => {
            let source = value
                .get("source")
                .or_else(|| value.get("audio"))
                .unwrap_or(value);
            let locator = source
                .get("url")
                .or_else(|| {
                    value
                        .get("image_url")
                        .and_then(|image| image.get("url").or(Some(image)))
                })
                .and_then(Value::as_str);
            let data = source
                .get("data")
                .or_else(|| value.get("result"))
                .and_then(Value::as_str);
            let format = source
                .get("media_type")
                .or_else(|| source.get("mimeType"))
                .or_else(|| source.get("format"))
                .and_then(Value::as_str)
                .map(str::to_owned);
            let media_type = if kind.contains("image") {
                "image"
            } else if kind.contains("audio") {
                "audio"
            } else {
                "video"
            };
            let received = if let Some(data_uri) = locator.filter(|uri| uri.starts_with("data:")) {
                data_uri
                    .split_once(";base64,")
                    .and_then(|(_, data)| {
                        base64::engine::general_purpose::STANDARD.decode(data).ok()
                    })
                    .map(|bytes| ProviderMediaSource::Bytes { bytes })
            } else if let Some(data) = data {
                base64::engine::general_purpose::STANDARD
                    .decode(data)
                    .ok()
                    .map(|bytes| ProviderMediaSource::Bytes { bytes })
            } else {
                locator.map(|locator| ProviderMediaSource::Reference {
                    locator: locator.into(),
                    expires_at: None,
                })
            };
            if let Some(source) = received {
                return ProviderOutputBlock::Media {
                    media_type: media_type.into(),
                    format,
                    source,
                };
            }
        }
        _ => {}
    }
    ProviderOutputBlock::Other {
        type_name: kind.into(),
        content: value.clone(),
    }
}

/// Supplemental fields have one owner: protocol evidence, never a second copy
/// of normalized text, tool arguments or media bytes. Unknown block types and
/// reasoning already retain their complete observed representation.
pub(crate) fn content_metadata(value: &Value) -> Option<ProviderOutputBlock> {
    let block = content_block(value);
    metadata_for(value, &block)
}

fn metadata_for(value: &Value, block: &ProviderOutputBlock) -> Option<ProviderOutputBlock> {
    let excluded: &[&str] = match block {
        ProviderOutputBlock::Text { .. } => &["type", "text"],
        ProviderOutputBlock::ToolCall { .. } => {
            // Without call_id, id is the canonical call identity itself.
            if value.get("call_id").is_none() {
                &["type", "id", "name", "arguments", "input", "extra_content"]
            } else {
                &["type", "name", "arguments", "input", "extra_content"]
            }
        }
        ProviderOutputBlock::Media { .. } => {
            &["type", "data", "url", "result", "format", "media_type"]
        }
        _ => return None,
    };
    let fields: serde_json::Map<String, Value> = value
        .as_object()?
        .iter()
        .filter_map(|(key, value)| {
            if excluded.contains(&key.as_str()) {
                return None;
            }
            if matches!(block, ProviderOutputBlock::Media { .. })
                && ["source", "audio", "image_url"].contains(&key.as_str())
            {
                let fields: serde_json::Map<String, Value> = value
                    .as_object()?
                    .iter()
                    .filter(|(key, _)| {
                        !["data", "url", "format", "media_type", "mimeType"].contains(&key.as_str())
                    })
                    .map(|(key, value)| (key.clone(), value.clone()))
                    .collect();
                return (!fields.is_empty()).then(|| (key.clone(), Value::Object(fields)));
            }
            Some((key.clone(), value.clone()))
        })
        .collect();
    if matches!(block, ProviderOutputBlock::ToolCall { .. })
        && fields.len() == 1
        && fields.contains_key("call_id")
    {
        return None;
    }
    if fields.is_empty() {
        return None;
    }
    Some(ProviderOutputBlock::ProtocolMetadata {
        metadata: serde_json::json!({
            "kind":"content_metadata", "type_name":value.get("type"), "fields":fields,
        }),
    })
}

pub(crate) fn content_blocks(value: &Value) -> Vec<ProviderOutputBlock> {
    let block = content_block(value);
    let metadata = metadata_for(value, &block);
    std::iter::once(block).chain(metadata).collect()
}

/// Item identity/status/phase belong to the provider item, not to every part.
pub(crate) fn message_metadata(item: &Value) -> Option<Value> {
    let fields: serde_json::Map<String, Value> = item
        .as_object()?
        .iter()
        .filter(|(key, _)| !["type", "content"].contains(&key.as_str()))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    if fields.is_empty() {
        return None;
    }
    Some(serde_json::json!({
        "kind":"provider_item", "type_name":"message", "fields":fields,
    }))
}

fn position_metadata(
    block: &mut ProviderOutputBlock,
    output_index: usize,
    content_index: Option<usize>,
) {
    if let ProviderOutputBlock::ProtocolMetadata { metadata } = block {
        metadata["position"] =
            serde_json::json!({"output_index":output_index,"content_index":content_index});
    }
}

pub(crate) fn chat_tool_metadata(
    call_id: Option<&str>,
    fields: &serde_json::Map<String, Value>,
    function_fields: Option<&serde_json::Map<String, Value>>,
) -> Option<ProviderOutputBlock> {
    if fields.is_empty() && function_fields.is_none_or(|fields| fields.is_empty()) {
        return None;
    }
    let mut fields = fields.clone();
    fields.remove("type");
    if let Some(function) = function_fields.filter(|fields| !fields.is_empty()) {
        fields.insert("function".into(), Value::Object(function.clone()));
    }
    if fields.is_empty() {
        return None;
    }
    Some(ProviderOutputBlock::ProtocolMetadata {
        metadata: serde_json::json!({
            "kind":"content_metadata", "type_name":"function_call", "call_id":call_id, "fields":fields,
        }),
    })
}

pub(crate) fn responses_blocks(output: &[Value]) -> Vec<ProviderOutputBlock> {
    let mut blocks = Vec::new();
    for (output_index, item) in output.iter().enumerate() {
        if item.get("type").and_then(Value::as_str) == Some("message")
            && let Some(parts) = item.get("content").and_then(Value::as_array)
        {
            if let Some(metadata) = message_metadata(item) {
                let mut block = ProviderOutputBlock::ProtocolMetadata { metadata };
                position_metadata(&mut block, output_index, None);
                blocks.push(block);
            }
            for (content_index, part) in parts.iter().enumerate() {
                blocks.extend(content_blocks(part).into_iter().map(|mut block| {
                    position_metadata(&mut block, output_index, Some(content_index));
                    block
                }));
            }
            continue;
        }
        blocks.extend(content_blocks(item).into_iter().map(|mut block| {
            position_metadata(&mut block, output_index, None);
            block
        }));
    }
    blocks
}

/// Preserve ordered content parts before building the legacy display text.
pub(crate) fn assistant_content(
    content: Option<&Value>,
) -> (Option<String>, Vec<ProviderOutputBlock>) {
    let blocks = match content {
        Some(Value::String(text)) => vec![ProviderOutputBlock::Text { text: text.clone() }],
        Some(Value::Array(parts)) => content_parts(parts),
        Some(value) => vec![ProviderOutputBlock::Other {
            type_name: "assistant.content".into(),
            content: value.clone(),
        }],
        None => vec![],
    };
    let text = blocks
        .iter()
        .filter_map(|block| match block {
            ProviderOutputBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<String>();
    ((!text.is_empty()).then_some(text), blocks)
}

pub(crate) fn content_parts(parts: &[Value]) -> Vec<ProviderOutputBlock> {
    parts
        .iter()
        .enumerate()
        .flat_map(|(index, part)| {
            content_blocks(part).into_iter().map(move |mut block| {
                position_metadata(&mut block, 0, Some(index));
                block
            })
        })
        .collect()
}

pub(crate) fn gemini_block(value: &Value) -> ProviderOutputBlock {
    if value.get("thought").and_then(Value::as_bool) == Some(true) {
        let content = value.clone();
        return ProviderOutputBlock::Reasoning {
            representation: "gemini.part/v1".into(),
            bytes: content.to_string().into_bytes(),
            replay_requirement: "provider_defined".into(),
        };
    }
    if let Some(call) = value
        .get("functionCall")
        .or_else(|| value.get("function_call"))
        && let Some(name) = call.get("name").and_then(Value::as_str)
    {
        return ProviderOutputBlock::ToolCall {
            call: ToolCall {
                id: call.get("id").and_then(Value::as_str).unwrap_or("").into(),
                name: name.into(),
                arguments: call
                    .get("args")
                    .map(Value::to_string)
                    .unwrap_or_else(|| "{}".into()),
                extra_content: value
                    .get("thoughtSignature")
                    .filter(|signature| signature.is_string())
                    .map(|signature| serde_json::json!({"thoughtSignature":signature})),
            },
        };
    }
    if let Some(text) = value.get("text").and_then(Value::as_str) {
        return ProviderOutputBlock::Text { text: text.into() };
    }
    if let Some(inline) = value.get("inlineData").or_else(|| value.get("inline_data")) {
        let mime = inline
            .get("mimeType")
            .or_else(|| inline.get("mime_type"))
            .and_then(Value::as_str)
            .unwrap_or("application/octet-stream");
        if let Some(data) = inline.get("data").and_then(Value::as_str)
            && let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(data)
        {
            return ProviderOutputBlock::Media {
                media_type: mime.split('/').next().unwrap_or("unknown").into(),
                format: Some(mime.into()),
                source: ProviderMediaSource::Bytes { bytes },
            };
        }
    }
    if let Some(file) = value.get("fileData")
        && let Some(locator) = file.get("fileUri").and_then(Value::as_str)
    {
        let mime = file
            .get("mimeType")
            .and_then(Value::as_str)
            .unwrap_or("application/octet-stream");
        return ProviderOutputBlock::Media {
            media_type: mime.split('/').next().unwrap_or("unknown").into(),
            format: Some(mime.into()),
            source: ProviderMediaSource::Reference {
                locator: locator.into(),
                expires_at: None,
            },
        };
    }
    ProviderOutputBlock::Other {
        type_name: "gemini.part".into(),
        content: value.clone(),
    }
}

/// A signature can accompany ordinary text, a function call or media. It
/// must not change that part's semantic type. Preserve replay metadata as a
/// separate content block before projecting legacy display text.
pub(crate) fn gemini_blocks(value: &Value) -> Vec<ProviderOutputBlock> {
    let block = gemini_block(value);
    if matches!(&block, ProviderOutputBlock::Reasoning { .. }) {
        return vec![block];
    }
    let tool_call = matches!(&block, ProviderOutputBlock::ToolCall { .. });
    let unknown = matches!(&block, ProviderOutputBlock::Other { .. });
    let signature = value
        .get("thoughtSignature")
        .filter(|value| value.is_string());
    if unknown && signature.is_none() {
        return vec![block];
    }
    let mut blocks = if unknown { Vec::new() } else { vec![block] };
    if !tool_call && let Some(signature) = signature {
        blocks.push(ProviderOutputBlock::Reasoning {
            representation: "gemini.part_replay/v1".into(),
            bytes: serde_json::json!({"thoughtSignature":signature})
                .to_string()
                .into_bytes(),
            replay_requirement: "provider_defined".into(),
        });
    }
    let metadata: serde_json::Map<String, Value> = value
        .as_object()
        .into_iter()
        .flatten()
        .filter(|(key, _)| {
            if key.as_str() == "thoughtSignature" {
                return signature.is_none();
            }
            if unknown {
                return true;
            }
            !matches!(
                key.as_str(),
                "text"
                    | "thought"
                    | "functionCall"
                    | "function_call"
                    | "inlineData"
                    | "inline_data"
                    | "fileData"
                    | "file_data"
            )
        })
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    if !metadata.is_empty() {
        blocks.push(ProviderOutputBlock::Other {
            type_name: if unknown {
                "gemini.part"
            } else {
                "gemini.part_metadata"
            }
            .into(),
            content: Value::Object(metadata),
        });
    }
    blocks
}
