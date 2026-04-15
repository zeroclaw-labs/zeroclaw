// Phone Assistant Module (v3.0 Section B)
//
// Integrates the phone assistant with the Brain Layer:
// - caller_match: incoming number → ontology Object lookup
// - context_inject: build system prompt from compiled_truth + timeline
// - post_call: record call metadata, timeline evidence, ontology Action
//
// MoA-native phone features (whisper directing, multi-thread, SOS, phishing
// detection) are separate concerns handled by platform-specific code.
// This module focuses on the brain↔phone bidirectional connection.

pub mod caller_match;
pub mod context_inject;
pub mod post_call;
