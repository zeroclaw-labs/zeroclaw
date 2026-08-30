//! Wire types for the Gemini Live subset (config in, server messages out).
//!
//! This module is data-only: enums/structs and the trivial mappings that
//! don't require any I/O (model id / API version). Setup serialization and
//! server-message parsing live in [`crate::wire`] (Tasks 3-5); those consume
//! [`SetupConfig`] and produce [`ServerEvent`].

use serde::Serialize;

/// Which Gemini Live model variant a session talks to. Each variant pins its
/// own model id and API version — the two vary together (native-audio is
/// only served under `v1alpha`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Model {
    /// Cascaded ASR + text LLM + TTS pipeline. Serves under `v1beta`.
    HalfCascade,
    /// End-to-end native-audio model. Serves under `v1alpha`; supports
    /// affective dialog and proactive audio.
    NativeAudio,
}

impl Model {
    /// The bare model id (without the `models/` prefix the wire path needs).
    pub fn model_id(&self) -> &'static str {
        match self {
            Model::HalfCascade => "gemini-3.1-flash-live-preview",
            Model::NativeAudio => "gemini-2.5-flash-native-audio-latest",
        }
    }

    /// The `GenerativeService` API version this model is served under.
    pub fn api_version(&self) -> &'static str {
        match self {
            Model::HalfCascade => "v1beta",
            Model::NativeAudio => "v1alpha",
        }
    }

    /// Whether this is the native-audio (end-to-end) model, as opposed to
    /// the cascaded half-cascade pipeline.
    pub fn is_native(&self) -> bool {
        matches!(self, Model::NativeAudio)
    }
}

/// Who a transcript line or affect annotation belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    User,
    Model,
}

/// An emotion label from a native-audio affective frame (e.g. `<ctrl95>` /
/// `emotion_model` annotations). Opaque text as reported by the model; the
/// affective parser (Task 5) is responsible for extracting it.
#[derive(Clone, Debug, PartialEq)]
pub struct AffectLabel(pub String);

/// A WebSocket close frame's code + reason, as reported by the transport.
#[derive(Clone, Debug)]
pub struct CloseReason {
    pub code: u16,
    pub reason: String,
}

/// One parsed server-side event from the Gemini Live `BidiGenerateContent`
/// stream. A single server text/binary frame can yield zero or more of
/// these (see `wire::parse_server_message`, Task 4).
#[derive(Clone, Debug, PartialEq)]
pub enum ServerEvent {
    /// The `setup` handshake completed; the session may now send audio.
    SetupComplete,
    /// 24kHz PCM16 output audio samples (one `inlineData` part).
    OutputAudio(Vec<i16>),
    /// An input or output transcription delta.
    Transcript {
        role: Role,
        text: String,
        final_: bool,
    },
    /// An affective-dialog emotion annotation (native-audio only).
    Affect { role: Role, label: AffectLabel },
    /// The model's turn was interrupted by callee speech (barge-in).
    Interrupted,
    /// The model's turn finished.
    TurnComplete,
    /// A function/tool call the model wants the client to execute.
    ToolCall {
        name: String,
        id: String,
        args: serde_json::Value,
    },
    /// A `sessionResumptionUpdate` from the server. Per Google's
    /// session-management guidance, handle retention is conditioned on BOTH
    /// fields: `resumable: false` means the stored handle is no longer valid
    /// and must be dropped (`new_handle` is absent in that case); `resumable:
    /// true` with `new_handle: Some(_)` means a fresh handle should replace
    /// the stored one. `resumable: true` with `new_handle: None` changes
    /// nothing (keep whatever handle is already stored).
    ResumptionUpdate {
        new_handle: Option<String>,
        resumable: bool,
    },
    /// The server is about to close the connection (session limit, etc);
    /// reconnect using the latest resumption handle.
    GoAway,
}

/// One `setup.tools[].functionDeclarations[]` entry. The caller owns the
/// full set of functions exposed to the model for a session — the crate has
/// no built-in tool (not even `end_call`): kutsu's caller assembles a
/// single-entry list for that tool; other callers (e.g. the broker channel)
/// assemble their own set.
#[derive(Clone, Debug)]
pub struct FunctionDecl {
    /// The function name the model will call it by (`toolCall.functionCalls[].name`).
    pub name: String,
    /// Human-readable description shown to the model.
    pub description: String,
    /// JSON Schema for the function's parameters (`{}`-shaped object schema
    /// for a no-argument function).
    pub parameters: serde_json::Value,
}

/// Everything needed to serialize a Gemini Live `setup` message for one
/// session, short of the literal wire structure (which lives in
/// `wire::build_setup`, Task 3). The crate does not assemble prompts or
/// know about scenarios/gender/language directives — `system_instruction`
/// arrives here fully built by the caller.
///
/// Every field `wire::build_setup` needs beyond this struct is either a
/// wire-format constant (`responseModalities`, transcription flags) or
/// derived from `model.is_native()` (activity-detection thresholds,
/// `thinkingConfig`, `proactivity`, `NON_BLOCKING` tool behavior, whether
/// `languageCode`/`language` applies).
#[derive(Clone, Debug)]
pub struct SetupConfig {
    /// Which model/API-version this session targets. Selects the endpoint
    /// api-version and, unless `model_id_override` is set, the wire model id.
    pub model: Model,
    /// Explicit `models/<id>` to send on the wire, pinning a specific Gemini
    /// model without changing the api-version (which stays derived from
    /// `model`). `None` falls back to `model`'s default id. An id incompatible
    /// with the selected api-version is the provider's to reject — Gemini
    /// returns a setup error rather than this crate guessing compatibility.
    pub model_id_override: Option<String>,
    /// Prebuilt voice name (`generationConfig.speechConfig.voiceConfig`).
    pub voice: String,
    /// BCP-47 language tag for `speechConfig.languageCode`. Only meaningful
    /// (and only sent) on half-cascade; native-audio ignores the structured
    /// field, so callers should pass `None` there.
    pub language: Option<String>,
    /// The fully-assembled system prompt text (scenario + gender + closing
    /// + language directives already applied by the caller).
    pub system_instruction: String,
    /// `generationConfig.temperature`.
    pub temperature: f32,
    /// The exact set of functions exposed to the model as
    /// `setup.tools[].functionDeclarations`. The caller controls this list
    /// completely — no tool is added implicitly.
    pub functions: Vec<FunctionDecl>,
    /// A prior session's resumption handle to request a warm resume, or
    /// `None` for a fresh session.
    pub resume_handle: Option<String>,
}

impl SetupConfig {
    /// Convenience accessor for the native-audio flag that gates several
    /// wire fields (see the struct-level doc comment).
    pub fn is_native(&self) -> bool {
        self.model.is_native()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_audio_model_id_and_version() {
        assert_eq!(
            Model::NativeAudio.model_id(),
            "gemini-2.5-flash-native-audio-latest"
        );
        assert_eq!(Model::NativeAudio.api_version(), "v1alpha");
        assert!(Model::NativeAudio.is_native());
    }

    #[test]
    fn half_cascade_model_id_and_version() {
        assert_eq!(
            Model::HalfCascade.model_id(),
            "gemini-3.1-flash-live-preview"
        );
        assert_eq!(Model::HalfCascade.api_version(), "v1beta");
        assert!(!Model::HalfCascade.is_native());
    }

    #[test]
    fn role_serializes_snake_case() {
        assert_eq!(serde_json::to_string(&Role::User).unwrap(), "\"user\"");
        assert_eq!(serde_json::to_string(&Role::Model).unwrap(), "\"model\"");
    }

    #[test]
    fn setup_config_is_native_follows_model() {
        let cfg = SetupConfig {
            model: Model::NativeAudio,
            model_id_override: None,
            voice: "Autonoe".into(),
            language: None,
            system_instruction: "Be nice.".into(),
            temperature: 0.8,
            functions: Vec::new(),
            resume_handle: None,
        };
        assert!(cfg.is_native());
    }
}
