//! Pure setup serialization + server-message/affective parsing (no I/O).
//! Populated in Tasks 3-5.

use base64::Engine as _;
use serde_json::{json, Value};

use crate::types::{Role, ServerEvent, SetupConfig};

/// A `parse_server_message` failure. Malformed JSON or malformed base64
/// audio inside an otherwise well-formed message; never panics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WireError(pub String);

impl std::fmt::Display for WireError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for WireError {}

impl From<serde_json::Error> for WireError {
    fn from(e: serde_json::Error) -> Self {
        WireError(format!("bad JSON: {e}"))
    }
}

/// Decode one `inlineData.data` base64 blob to PCM16 samples. Proto3 JSON's
/// `bytes` field accepts both standard and URL-safe base64 alphabets (with
/// or without padding); try each so a server that picks either form still
/// parses.
fn decode_pcm16(b64: &str) -> Result<Vec<i16>, WireError> {
    use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD, URL_SAFE, URL_SAFE_NO_PAD};
    let bytes = STANDARD
        .decode(b64)
        .or_else(|_| STANDARD_NO_PAD.decode(b64))
        .or_else(|_| URL_SAFE.decode(b64))
        .or_else(|_| URL_SAFE_NO_PAD.decode(b64))
        .map_err(|e| WireError(format!("bad base64 audio: {e}")))?;
    Ok(bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|b| i16::from_le_bytes(*b))
        .collect())
}

/// Parse one server WebSocket frame (binary or text, UTF-8 JSON either way)
/// into zero or more [`ServerEvent`]s.
///
/// Mirrors the google-genai SDK's `_LiveServerMessage_from_mldev` converter
/// path (`_live_converters.py`) and `LiveServerMessage`'s `.text`/`.data`
/// accessors (`types.py`). A `modelTurn` part's `thought: true` flag marks
/// the model's internal reasoning/annotation channel, but the SDK only
/// filters on it for **text** (`.text` does `if part.thought: continue`
/// before concatenating). `.data` (audio) concatenates every part's
/// `inlineData` unconditionally — it does not branch on `thought` at all.
/// This crate has no part-level text-extraction path (model text comes from
/// `outputTranscription`, not `modelTurn.parts`), so in practice `thought`
/// currently has nothing to filter here: all `inlineData` audio is passed
/// through regardless of `thought`, matching `.data` exactly. If a
/// part-text path is ever added, gate *that* on `thought`, not audio.
///
/// Where kutsu's `proto.rs::parse_server_message` and the SDK disagree, the
/// SDK wins; this function is that mirror, generalized (no `end_call`
/// special-casing — `ToolCall` carries the raw name/id/args for any tool).
pub fn parse_server_message(bytes: &[u8]) -> Result<Vec<ServerEvent>, WireError> {
    let v: Value = serde_json::from_slice(bytes)?;
    let mut out = Vec::new();

    if v.get("setupComplete").is_some() {
        out.push(ServerEvent::SetupComplete);
    }

    if let Some(sc) = v.get("serverContent") {
        if let Some(parts) = sc.pointer("/modelTurn/parts").and_then(|p| p.as_array()) {
            for part in parts {
                // Mirror the SDK's `.data` accessor: every part's
                // `inlineData` is surfaced, thought-agnostic. `thought`
                // only ever gates text extraction (see the SDK's `.text`),
                // and this crate has no part-level text path to gate.
                if let Some(data) = part.pointer("/inlineData/data").and_then(|d| d.as_str()) {
                    out.push(ServerEvent::OutputAudio(decode_pcm16(data)?));
                }
            }
        }
        if let Some(t) = sc
            .get("outputTranscription")
            .and_then(|t| t.get("text"))
            .and_then(|t| t.as_str())
        {
            let final_ = sc
                .pointer("/outputTranscription/finished")
                .and_then(|f| f.as_bool())
                .unwrap_or(false);
            out.push(ServerEvent::Transcript {
                role: Role::Model,
                text: t.to_string(),
                final_,
            });
        }
        if let Some(t) = sc
            .get("inputTranscription")
            .and_then(|t| t.get("text"))
            .and_then(|t| t.as_str())
        {
            let final_ = sc
                .pointer("/inputTranscription/finished")
                .and_then(|f| f.as_bool())
                .unwrap_or(false);
            out.push(ServerEvent::Transcript {
                role: Role::User,
                text: t.to_string(),
                final_,
            });
        }
        if sc
            .get("interrupted")
            .and_then(|i| i.as_bool())
            .unwrap_or(false)
        {
            out.push(ServerEvent::Interrupted);
        }
        if sc
            .get("turnComplete")
            .and_then(|t| t.as_bool())
            .unwrap_or(false)
        {
            out.push(ServerEvent::TurnComplete);
        }
    }

    if let Some(calls) = v
        .pointer("/toolCall/functionCalls")
        .and_then(|c| c.as_array())
    {
        for call in calls {
            let name = call
                .get("name")
                .and_then(|n| n.as_str())
                .unwrap_or_default()
                .to_string();
            let id = call
                .get("id")
                .and_then(|i| i.as_str())
                .unwrap_or_default()
                .to_string();
            let args = call.get("args").cloned().unwrap_or(Value::Null);
            out.push(ServerEvent::ToolCall { name, id, args });
        }
    }

    if let Some(h) = v
        .pointer("/sessionResumptionUpdate/newHandle")
        .and_then(|h| h.as_str())
    {
        out.push(ServerEvent::ResumptionHandle(h.to_string()));
    }

    if v.get("goAway").is_some() {
        out.push(ServerEvent::GoAway);
    }

    Ok(out)
}

/// Build the first `setup` message for a Gemini Live session. Every value is
/// sourced from `cfg`; the crate does not assemble prompts or know about
/// scenarios/gender/language directives — `cfg.system_instruction` arrives
/// fully built by the caller.
///
/// Wire shape ported verbatim from kutsu's `proto::build_setup`
/// (`src/proto.rs`), which is correct post-fixes:
/// - `enableAffectiveDialog` and `thinkingConfig` nest under
///   `generationConfig`; `proactivity` is a top-level `setup` field. Putting
///   `enableAffectiveDialog` directly under `setup` is what triggered close
///   1007 ("Unknown name enableAffectiveDialog at 'setup'") — see the
///   google-genai `_live_converters.py` converter paths.
pub fn build_setup(cfg: &SetupConfig) -> Value {
    let native = cfg.is_native();

    let mut speech = json!({
        "voiceConfig": { "prebuiltVoiceConfig": { "voiceName": cfg.voice } }
    });
    if !native {
        if let Some(lang) = &cfg.language {
            speech["languageCode"] = json!(lang);
        }
    }

    let function_decls: Vec<Value> = cfg
        .functions
        .iter()
        .map(|f| {
            let mut d = json!({
                "name": f.name,
                "description": f.description,
                "parameters": f.parameters,
            });
            if native {
                d["behavior"] = json!("NON_BLOCKING");
            }
            d
        })
        .collect();

    let aad = if native {
        // LOW start-sensitivity: don't let caller-side background noise (a real
        // trunk / a caller in a car) be mistaken for speech onset. LOW alone
        // under-triggered on the quiet real-line audio (~-32 dBFS), stalling
        // Gemini for 15-19 s before it committed the caller's turn — so the
        // uplink is level-normalised (AGC) BEFORE Gemini sees it (see the bridge
        // uplink task), lifting soft real speech above LOW's threshold without
        // reintroducing noise-triggering. END_SENSITIVITY_HIGH commits the turn
        // readily once real speech stops.
        json!({
            "startOfSpeechSensitivity": "START_SENSITIVITY_LOW",
            "endOfSpeechSensitivity": "END_SENSITIVITY_HIGH",
            "prefixPaddingMs": 300,
            "silenceDurationMs": 100
        })
    } else {
        json!({
            "startOfSpeechSensitivity": "START_SENSITIVITY_LOW",
            "prefixPaddingMs": 1000
        })
    };

    let mut setup = json!({
        "model": format!("models/{}", cfg.model.model_id()),
        "generationConfig": {
            "responseModalities": ["AUDIO"],
            "temperature": cfg.temperature,
            "speechConfig": speech
        },
        "systemInstruction": { "parts": [ { "text": cfg.system_instruction } ] },
        "tools": [ { "functionDeclarations": function_decls } ],
        "realtimeInputConfig": { "automaticActivityDetection": aad },
        "sessionResumption": { "handle": cfg.resume_handle },
        "inputAudioTranscription": {},
        "outputAudioTranscription": {},
        // long-running sessions: sliding-window compression keeps the
        // session under the context cap.
        "contextWindowCompression": { "slidingWindow": {} }
    });

    // Disable model "thinking" in BOTH modes for lowest latency. Per the SDK
    // (ThinkingConfig.thinkingBudget) 0 = DISABLED; it nests under
    // generationConfig in LiveClientSetup. Valid for both live models
    // (native = 2.5-flash-native-audio, half-cascade = 3.1-flash-live, both
    // thinking-capable — the API errors only when set on a non-thinking model).
    setup["generationConfig"]["thinkingConfig"] = json!({ "thinkingBudget": 0 });

    if native {
        // native-audio v1alpha extras — wire placement per the google-genai
        // live converter (`_live_converters.py`): `enableAffectiveDialog` nests
        // under `generationConfig`; `proactivity` is a top-level `setup` field.
        // Affective dialog is re-enabled (unlike kutsu's old proto.rs): the
        // crate's `parse_server_message` strips the `<ctrl95>`/`emotion_*`
        // thought parts, so they never reach transcript/audio.
        setup["generationConfig"]["enableAffectiveDialog"] = json!(true);
        setup["proactivity"] = json!({ "proactiveAudio": true });
    }

    json!({ "setup": setup })
}

/// Serialize one uplink audio frame as a `realtime_input` message: PCM16 @
/// 16 kHz, little-endian, base64-encoded. Ported byte-for-byte from kutsu's
/// `gemini_live::build_realtime_input` — the serialized bytes must stay
/// identical (the session layer's uplink is asserted against this exact
/// shape).
pub fn build_realtime_input(pcm: &[i16]) -> String {
    use base64::engine::general_purpose::STANDARD;
    let mut bytes = Vec::with_capacity(pcm.len() * 2);
    for &s in pcm {
        bytes.extend_from_slice(&s.to_le_bytes());
    }
    let data = STANDARD.encode(&bytes);
    json!({
        "realtime_input": { "audio": { "mimeType": "audio/pcm;rate=16000", "data": data } }
    })
    .to_string()
}

/// Build a `client_content` message carrying a single completed user turn.
/// The caller (kutsu) uses this to hand the model the first/next turn (e.g.
/// GREET_CUE / RESUME_CUE); the crate itself is content-agnostic. Ported
/// verbatim from kutsu's `gemini_live::build_client_content`.
pub fn build_client_content(text: &str) -> String {
    json!({
        "client_content": {
            "turns": [ { "role": "user", "parts": [ { "text": text } ] } ],
            "turnComplete": true
        }
    })
    .to_string()
}

/// Acknowledge a tool call by `call_id` with a trivial success response.
/// kutsu owns any tool semantics (e.g. `end_call`); this only closes the
/// function-call loop on the wire. Ported verbatim from kutsu's
/// `gemini_live::build_tool_response`.
pub fn build_tool_response(call_id: &str) -> String {
    json!({
        "tool_response": { "functionResponses": [ { "id": call_id, "response": { "ok": true } } ] }
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{FunctionDecl, Model};

    const TEMPERATURE: f32 = 0.8;

    fn cfg(model: Model, resume_handle: Option<String>) -> SetupConfig {
        SetupConfig {
            model,
            voice: "Autonoe".into(),
            language: if model.is_native() {
                None
            } else {
                Some("en-US".into())
            },
            system_instruction: "Be nice.".into(),
            temperature: TEMPERATURE,
            functions: vec![FunctionDecl {
                name: "end_call".into(),
                description: "Call this exactly once, at the end of the conversation, \
                              with the collected information and a final disposition."
                    .into(),
                parameters: serde_json::json!({"type":"object","required":["disposition"]}),
            }],
            resume_handle,
        }
    }

    #[test]
    fn half_cascade_setup_shape() {
        let s = build_setup(&cfg(Model::HalfCascade, None));
        let setup = &s["setup"];
        assert_eq!(setup["model"], "models/gemini-3.1-flash-live-preview");
        assert_eq!(setup["generationConfig"]["responseModalities"][0], "AUDIO");
        assert_eq!(setup["generationConfig"]["temperature"], TEMPERATURE as f64);
        assert_eq!(
            setup["generationConfig"]["speechConfig"]["voiceConfig"]["prebuiltVoiceConfig"]
                ["voiceName"],
            "Autonoe"
        );
        assert_eq!(
            setup["generationConfig"]["speechConfig"]["languageCode"],
            "en-US"
        );
        // Exactly one tool: end_call, parameters == the FunctionDecl's own
        // `parameters` JSON schema (from `functions[0]`).
        let decl = &setup["tools"][0]["functionDeclarations"][0];
        assert_eq!(decl["name"], "end_call");
        assert_eq!(decl["parameters"]["required"][0], "disposition");
        assert!(decl["behavior"].is_null(), "NON_BLOCKING is native-only");
        // VAD half-cascade.
        let aad = &setup["realtimeInputConfig"]["automaticActivityDetection"];
        assert_eq!(aad["startOfSpeechSensitivity"], "START_SENSITIVITY_LOW");
        assert_eq!(aad["prefixPaddingMs"], 1000);
        assert!(aad["silenceDurationMs"].is_null());
        // Transcription both ways on.
        assert!(setup["inputAudioTranscription"].is_object());
        assert!(setup["outputAudioTranscription"].is_object());
        // No native-only fields.
        assert!(setup["enableAffectiveDialog"].is_null());
        assert!(setup["generationConfig"]["enableAffectiveDialog"].is_null());
        // Thinking is disabled in BOTH modes (native + half-cascade).
        assert_eq!(
            setup["generationConfig"]["thinkingConfig"]["thinkingBudget"],
            0
        );
        assert!(setup["proactivity"].is_null());
        // Top-level wrapper key is snake_case `setup`.
        assert!(s.get("setup").is_some());
    }

    #[test]
    fn native_audio_setup_shape() {
        let s = build_setup(&cfg(Model::NativeAudio, Some("H1".into())));
        let setup = &s["setup"];
        assert_eq!(
            setup["model"],
            "models/gemini-2.5-flash-native-audio-latest"
        );
        // No languageCode on native.
        assert!(setup["generationConfig"]["speechConfig"]["languageCode"].is_null());
        // proactivity is a top-level setup field (path per the google-genai live
        // converter).
        assert!(setup["proactivity"]["proactiveAudio"] == true);
        // enableAffectiveDialog nests under generationConfig, NOT top-level
        // setup (top-level caused close 1007).
        assert!(setup["enableAffectiveDialog"].is_null());
        assert_eq!(setup["generationConfig"]["enableAffectiveDialog"], true);
        // Reasoning disabled on native (lower latency).
        assert_eq!(
            setup["generationConfig"]["thinkingConfig"]["thinkingBudget"],
            0
        );
        // NON_BLOCKING behavior on end_call.
        assert_eq!(
            setup["tools"][0]["functionDeclarations"][0]["behavior"],
            "NON_BLOCKING"
        );
        // Native VAD: LOW start-sensitivity (ignore caller-side background
        // noise), HIGH end-sensitivity (commit the turn once speech stops).
        let aad = &setup["realtimeInputConfig"]["automaticActivityDetection"];
        assert_eq!(aad["startOfSpeechSensitivity"], "START_SENSITIVITY_LOW");
        assert_eq!(aad["endOfSpeechSensitivity"], "END_SENSITIVITY_HIGH");
        assert_eq!(aad["prefixPaddingMs"], 300);
        assert_eq!(aad["silenceDurationMs"], 100);
        // Resumption handle carried.
        assert_eq!(setup["sessionResumption"]["handle"], "H1");
        // Top-level wrapper key is snake_case `setup`.
        assert!(s.get("setup").is_some());
    }

    #[test]
    fn build_setup_emits_context_window_compression() {
        let c = cfg(Model::HalfCascade, None);
        let v = build_setup(&c);
        let cwc = &v["setup"]["contextWindowCompression"];
        assert!(
            cwc.get("slidingWindow").is_some(),
            "setup must enable sliding-window context compression, got: {v}"
        );
    }

    #[test]
    fn resume_handle_none_serializes_null() {
        let s = build_setup(&cfg(Model::HalfCascade, None));
        assert!(s["setup"]["sessionResumption"]["handle"].is_null());
    }

    #[test]
    fn realtime_input_is_byte_identical_to_kutsu() {
        // PCM16 [0, 1] -> LE bytes 00 00 01 00 -> base64 "AAABAA==". Keys are
        // emitted in serde_json's default (sorted) order, matching kutsu's.
        assert_eq!(
            build_realtime_input(&[0i16, 1i16]),
            r#"{"realtime_input":{"audio":{"data":"AAABAA==","mimeType":"audio/pcm;rate=16000"}}}"#
        );
    }

    #[test]
    fn client_content_and_tool_response_shapes() {
        assert_eq!(
            build_client_content("hi"),
            r#"{"client_content":{"turnComplete":true,"turns":[{"parts":[{"text":"hi"}],"role":"user"}]}}"#
        );
        assert_eq!(
            build_tool_response("c1"),
            r#"{"tool_response":{"functionResponses":[{"id":"c1","response":{"ok":true}}]}}"#
        );
    }
}

#[cfg(test)]
mod parse_tests {
    use super::*;
    use crate::types::{Role, ServerEvent};

    // base64 "AAABAAIAAwA=" -> bytes 00 00 01 00 02 00 03 00 -> i16 LE [0,1,2,3]
    const AUDIO_B64: &str = "AAABAAIAAwA=";

    #[test]
    fn parses_audio_and_output_transcript_binary_and_string_identically() {
        let text = format!(
            r#"{{"serverContent":{{"modelTurn":{{"parts":[{{"inlineData":{{"data":"{AUDIO_B64}"}}}}]}},
               "outputTranscription":{{"text":"Hello","finished":false}}}}}}"#
        );
        let from_string = parse_server_message(text.as_bytes()).unwrap();
        let from_binary = parse_server_message(text.clone().into_bytes().as_slice()).unwrap();
        assert_eq!(from_string, from_binary);

        assert!(from_string
            .iter()
            .any(|e| matches!(e, ServerEvent::OutputAudio(s) if s == &vec![0, 1, 2, 3])));
        assert!(
            from_string.iter().any(|e| matches!(e,
            ServerEvent::Transcript { role: Role::Model, text, final_: false } if text == "Hello"))
        );
    }

    #[test]
    fn thought_part_audio_is_not_dropped_mirrors_sdk_data_accessor() {
        // The SDK's `LiveServerMessage.data` concatenates every part's
        // `inlineData` unconditionally — it does NOT branch on `thought`
        // (only `.text` does). A `thought: true` part carrying `inlineData`
        // must still surface as `OutputAudio`; real model speech must never
        // be dropped because of the `thought` flag.
        let text = format!(
            r#"{{"serverContent":{{"modelTurn":{{"parts":[
                {{"text":"<ctrl95>emotion_model happy<ctrl95>","thought":true}},
                {{"inlineData":{{"data":"{AUDIO_B64}"}},"thought":true}}
            ]}}}}}}"#
        );
        let evs = parse_server_message(text.as_bytes()).unwrap();
        assert_eq!(evs, vec![ServerEvent::OutputAudio(vec![0, 1, 2, 3])]);
    }

    #[test]
    fn output_transcription_finished_maps_to_final_model_transcript() {
        let text = r#"{"serverContent":{"outputTranscription":{"text":"Bye","finished":true}}}"#;
        let evs = parse_server_message(text.as_bytes()).unwrap();
        assert_eq!(
            evs,
            vec![ServerEvent::Transcript {
                role: Role::Model,
                text: "Bye".into(),
                final_: true
            }]
        );
    }

    #[test]
    fn input_transcription_maps_to_user_transcript() {
        let text = r#"{"serverContent":{"inputTranscription":{"text":"Hi","finished":false}}}"#;
        let evs = parse_server_message(text.as_bytes()).unwrap();
        assert_eq!(
            evs,
            vec![ServerEvent::Transcript {
                role: Role::User,
                text: "Hi".into(),
                final_: false
            }]
        );
    }

    #[test]
    fn parses_tool_call_generic() {
        let text = r#"{"toolCall":{"functionCalls":[
            {"name":"end_call","id":"fc-1","args":{"disposition":"appointment"}}
        ]}}"#;
        let evs = parse_server_message(text.as_bytes()).unwrap();
        assert_eq!(
            evs,
            vec![ServerEvent::ToolCall {
                name: "end_call".into(),
                id: "fc-1".into(),
                args: serde_json::json!({"disposition": "appointment"}),
            }]
        );
    }

    #[test]
    fn parses_resumption_turn_interrupt_setup_goaway() {
        assert_eq!(
            parse_server_message(br#"{"sessionResumptionUpdate":{"newHandle":"HANDLE-123"}}"#)
                .unwrap(),
            vec![ServerEvent::ResumptionHandle("HANDLE-123".into())]
        );
        assert_eq!(
            parse_server_message(br#"{"serverContent":{"turnComplete":true}}"#).unwrap(),
            vec![ServerEvent::TurnComplete]
        );
        assert_eq!(
            parse_server_message(br#"{"serverContent":{"interrupted":true}}"#).unwrap(),
            vec![ServerEvent::Interrupted]
        );
        assert_eq!(
            parse_server_message(br#"{"setupComplete":{}}"#).unwrap(),
            vec![ServerEvent::SetupComplete]
        );
        assert_eq!(
            parse_server_message(br#"{"goAway":{}}"#).unwrap(),
            vec![ServerEvent::GoAway]
        );
    }

    #[test]
    fn malformed_json_errors_without_panicking() {
        assert!(parse_server_message(b"not json").is_err());
    }

    #[test]
    fn empty_object_yields_no_events() {
        assert_eq!(
            parse_server_message(b"{}").unwrap(),
            Vec::<ServerEvent>::new()
        );
    }
}
