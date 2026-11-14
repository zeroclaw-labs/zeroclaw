//! Runtime facade for the shared tool-protocol detector.
//!
//! Detection is pure parsing and therefore lives in `zeroclaw-tool-call-parser`.
//! The turn runtime keeps this facade so its execution and telemetry modules do
//! not depend on the parser crate's internal module path.

pub(crate) use zeroclaw_tool_call_parser::protocol_detect::*;
