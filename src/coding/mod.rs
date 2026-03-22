//! Multi-model code review and AI-assisted coding pipeline.
//!
//! Implements the "AI collaboration" coding pattern where multiple
//! models review each other's work:
//!
//! 1. **Producer** (e.g. Claude Opus) writes code
//! 2. **Gatekeeper** (e.g. Gemini) reviews architecture alignment
//! 3. **Validator** (e.g. Claude) evaluates the gatekeeper's findings
//! 4. Pipeline merges into a consensus report
//!
//! ## Architecture
//!
//! ```text
//! Code diff ─┬─▸ GeminiReviewer ─▸ ReviewReport ─┐
//!            │                                    │
//!            └─▸ ClaudeReviewer (sees Gemini's) ─▸├─▸ ConsensusReport
//!                                                 │
//!            ┌────────────────────────────────────┘
//!            └─▸ merge findings + consensus verdict
//! ```
//!
//! ## Usage
//!
//! The pipeline is used in two contexts:
//! - **GitHub Actions**: via the `gemini-pr-review.yml` workflow
//! - **MoA app**: via the coding category's review session manager
//!
//! ## Extension
//!
//! Add new reviewers by implementing [`CodeReviewer`] and registering
//! them in [`ReviewPipeline::from_config`].

pub mod auto_fix;
pub mod pipeline;
pub mod reviewers;
pub mod sandbox_bridge;
pub mod traits;

#[allow(unused_imports)]
pub use auto_fix::{generate_fix_plan, FixInstruction, FixPlan};
#[allow(unused_imports)]
pub use pipeline::{PipelineConfig, ReviewPipeline};
#[allow(unused_imports)]
pub use reviewers::{ClaudeReviewer, GeminiReviewer};
#[allow(unused_imports)]
pub use sandbox_bridge::{
    ReviewFixAction, ReviewFixPlan, SandboxReviewBridge, SandboxReviewBridgeConfig,
};
#[allow(unused_imports)]
pub use traits::{
    CodeReviewer, ConsensusReport, ReviewContext, ReviewFinding, ReviewReport, ReviewVerdict,
    Severity,
};
