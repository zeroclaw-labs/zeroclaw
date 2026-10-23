use super::*;

#[test]
fn mixed_responses_output_preserves_order_opaque_reasoning_and_media() {
    let wire = serde_json::json!([
        {"type":"reasoning", "encrypted_content":"opaque-continuation", "summary":[]},
        {"type":"message", "content":[
            {"type":"output_text", "text":"before"},
            {"type":"output_audio", "data":"AQID", "format":"wav"},
            {"type":"image_url", "image_url":{"url":"https://fixture.invalid/image"}},
            {"type":"future_content", "item_id":"preserve-me"},
            {"type":"output_text", "text":"after"}
        ]}
    ]);
    let blocks = responses_blocks(wire.as_array().expect("array"));
    assert_eq!(blocks.len(), 6);
    let ProviderOutputBlock::Reasoning { bytes, .. } = &blocks[0] else {
        panic!("reasoning")
    };
    assert_eq!(
        serde_json::from_slice::<Value>(bytes).expect("reasoning bytes"),
        wire[0]
    );
    assert!(matches!(&blocks[1], ProviderOutputBlock::Text {text} if text=="before"));
    assert!(
        matches!(&blocks[2], ProviderOutputBlock::Media {source:ProviderMediaSource::Bytes {bytes}, ..} if bytes==&[1,2,3])
    );
    assert!(
        matches!(&blocks[3], ProviderOutputBlock::Media {source:ProviderMediaSource::Reference {locator, ..}, ..} if locator=="https://fixture.invalid/image")
    );
    assert!(
        matches!(&blocks[4], ProviderOutputBlock::Other {content,..} if content["item_id"]=="preserve-me")
    );
    assert!(matches!(&blocks[5], ProviderOutputBlock::Text {text} if text=="after"));
}

#[test]
fn responses_metadata_retains_item_phase_annotations_tool_identity_and_media_extensions() {
    let wire = serde_json::json!([
        {"type":"message","id":"msg_fixture","role":"assistant","status":"completed","phase":"final_answer","future_item":true,"content":[
            {"type":"output_text","text":"answer","annotations":[{"type":"url_citation","url":"https://fixture.invalid"}],"logprobs":[0.1],"future_text":7}
        ]},
        {"type":"function_call","id":"fc_fixture","call_id":"call_fixture","name":"lookup","namespace":"search","arguments":"{\"value\":1}","status":"completed"},
        {"type":"image_generation_call","id":"ig_fixture","status":"completed","result":"AQID","revised_prompt":"fixture prompt","future_image":true}
    ]);
    let blocks = responses_blocks(wire.as_array().unwrap());
    let metadata: Vec<_> = blocks
        .iter()
        .filter_map(|block| match block {
            ProviderOutputBlock::ProtocolMetadata { metadata } => Some(metadata),
            _ => None,
        })
        .collect();
    assert_eq!(metadata.len(), 4);
    assert_eq!(metadata[0]["fields"]["id"], "msg_fixture");
    assert_eq!(metadata[0]["fields"]["phase"], "final_answer");
    assert!(metadata[0]["fields"].get("content").is_none());
    assert_eq!(
        metadata[1]["position"],
        serde_json::json!({"output_index":0,"content_index":0})
    );
    assert_eq!(
        metadata[1]["fields"]["annotations"],
        wire[0]["content"][0]["annotations"]
    );
    assert_eq!(
        metadata[1]["fields"]["logprobs"],
        wire[0]["content"][0]["logprobs"]
    );
    assert_eq!(metadata[1]["fields"]["future_text"], 7);
    assert!(metadata[1]["fields"].get("text").is_none());
    assert_eq!(metadata[2]["fields"]["id"], "fc_fixture");
    assert_eq!(metadata[2]["fields"]["namespace"], "search");
    assert_eq!(metadata[2]["fields"]["call_id"], "call_fixture");
    assert!(metadata[2]["fields"].get("arguments").is_none());
    assert!(
        matches!(&blocks[3], ProviderOutputBlock::ToolCall { call } if call.id=="call_fixture" && call.arguments=="{\"value\":1}")
    );
    assert_eq!(metadata[3]["fields"]["revised_prompt"], "fixture prompt");
    assert!(metadata[3]["fields"].get("result").is_none());
}

#[test]
fn legacy_response_blocks_need_no_new_fields_and_media_metadata_has_no_binary_copy() {
    let block: ProviderOutputBlock =
        serde_json::from_value(serde_json::json!({"kind":"text","text":"legacy"})).unwrap();
    assert!(matches!(block,ProviderOutputBlock::Text {text} if text=="legacy"));
    let blocks = content_blocks(
        &serde_json::json!({"type":"image","source":{"type":"base64","media_type":"image/png","data":"AQID","detail":"high","future":true}}),
    );
    assert_eq!(blocks.len(), 2);
    let ProviderOutputBlock::ProtocolMetadata { metadata } = &blocks[1] else {
        panic!("metadata")
    };
    assert_eq!(metadata["fields"]["source"]["detail"], "high");
    assert_eq!(metadata["fields"]["source"]["future"], true);
    assert!(metadata["fields"]["source"].get("data").is_none());
    assert!(metadata["fields"]["source"].get("media_type").is_none());
}

#[test]
fn gemini_reasoning_signature_and_inline_media_survive_classification() {
    let thought = serde_json::json!({"thought":true,"text":"fixture thought","thoughtSignature":"opaque","futureField":{"kept":true}});
    let ProviderOutputBlock::Reasoning { bytes, .. } = gemini_block(&thought) else {
        panic!("reasoning");
    };
    assert_eq!(
        serde_json::from_slice::<Value>(&bytes).expect("part"),
        thought
    );
    assert!(
        matches!(gemini_block(&serde_json::json!({"inlineData":{"mimeType":"image/png","data":"AQID"}})),
        ProviderOutputBlock::Media {source:ProviderMediaSource::Bytes {bytes},..} if bytes==vec![1,2,3])
    );
    assert!(
        matches!(gemini_block(&serde_json::json!({"fileData":{"mimeType":"video/mp4","fileUri":"https://fixture.invalid/video"}})),
        ProviderOutputBlock::Media {source:ProviderMediaSource::Reference {locator,..},..} if locator=="https://fixture.invalid/video")
    );
}

#[test]
fn gemini_signature_does_not_turn_text_or_tool_into_reasoning() {
    let text = serde_json::json!({
        "text":"visible answer", "thoughtSignature":"sk-fixture-continuation",
        "futureField":{"token":"fixture-token"},
    });
    let blocks = gemini_blocks(&text);
    assert!(matches!(&blocks[0],ProviderOutputBlock::Text {text} if text=="visible answer"));
    let ProviderOutputBlock::Reasoning { bytes, .. } = &blocks[1] else {
        panic!("explicit replay metadata");
    };
    assert_eq!(
        serde_json::from_slice::<Value>(bytes).expect("replay metadata"),
        serde_json::json!({"thoughtSignature":"sk-fixture-continuation"}),
        "replay metadata does not duplicate the ordinary text or future fields",
    );
    assert!(
        matches!(&blocks[2], ProviderOutputBlock::Other {content,..} if content == &serde_json::json!({"futureField":{"token":"fixture-token"}}))
    );
    let call = serde_json::json!({"functionCall":{"id":"fixture-call","name":"fixture_tool","args":{"value":1}},"thoughtSignature":"sk-fixture-continuation"});
    let blocks = gemini_blocks(&call);
    assert_eq!(blocks.len(), 1, "tool replay metadata already has an owner");
    assert!(
        matches!(&blocks[0],ProviderOutputBlock::ToolCall {call} if call.id=="fixture-call" && call.name=="fixture_tool" && call.extra_content.as_ref() == Some(&serde_json::json!({"thoughtSignature":"sk-fixture-continuation"})))
    );
}

#[test]
fn gemini_thought_preserves_original_text_signatures_and_unknown_fields() {
    let thought = serde_json::json!({
        "thought":true, "text":"ordinary api_key=fixture-key",
        "thoughtSignature":"sk-fixture-continuation",
        "futureField":{"token":"fixture-token","signature":"sk-untrusted-signature"},
    });
    let ProviderOutputBlock::Reasoning { bytes, .. } = gemini_block(&thought) else {
        panic!("reasoning");
    };
    let value: Value = serde_json::from_slice(&bytes).expect("thought");
    assert_eq!(value, thought);

    let malformed = serde_json::json!({
        "thought":true, "text":"ordinary",
        "thoughtSignature":{"api_key":"fixture-key"},
    });
    let ProviderOutputBlock::Reasoning { bytes, .. } = gemini_block(&malformed) else {
        panic!("reasoning");
    };
    let value: Value = serde_json::from_slice(&bytes).expect("malformed signature");
    assert_eq!(value, malformed);
}

#[test]
fn gemini_media_signature_metadata_does_not_duplicate_binary_content() {
    let blocks = gemini_blocks(&serde_json::json!({
        "inlineData":{"mimeType":"image/png","data":"AQID"},
        "thoughtSignature":"sk-fixture-continuation",
    }));
    assert_eq!(blocks.len(), 2);
    assert!(matches!(
        &blocks[0], ProviderOutputBlock::Media {
            source: ProviderMediaSource::Bytes { bytes }, ..
        } if bytes == &[1, 2, 3]
    ));
    let ProviderOutputBlock::Reasoning { bytes, .. } = &blocks[1] else {
        panic!("replay metadata");
    };
    assert_eq!(
        serde_json::from_slice::<Value>(bytes).expect("replay metadata"),
        serde_json::json!({"thoughtSignature":"sk-fixture-continuation"}),
    );
}

#[test]
fn gemini_signature_only_and_future_parts_keep_known_replay_bytes() {
    for future in [false, true] {
        let mut part = serde_json::json!({"thoughtSignature":"sk-fixture-continuation"});
        if future {
            part["futureField"] = serde_json::json!({"token":"fixture-token"});
        }
        let blocks = gemini_blocks(&part);
        assert_eq!(blocks.len(), if future { 2 } else { 1 });
        let ProviderOutputBlock::Reasoning { bytes, .. } = &blocks[0] else {
            panic!("known opaque signature");
        };
        assert_eq!(
            serde_json::from_slice::<Value>(bytes).expect("replay metadata"),
            serde_json::json!({"thoughtSignature":"sk-fixture-continuation"}),
        );
        if future {
            assert!(matches!(
                &blocks[1], ProviderOutputBlock::Other { content, .. }
                    if content == &serde_json::json!({"futureField":{"token":"fixture-token"}})
            ));
        }
    }
}
