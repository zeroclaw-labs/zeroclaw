mod store;
mod types;
mod verify;

#[allow(unused_imports)]
pub use store::{
    GoalTrackingSummary, approve, complete, get, list, propose, reject, revert_to_approved,
    set_in_progress, track,
};
pub use types::{Goal, GoalPatch, GoalSource, GoalStatus, VerificationMethod};
pub use verify::verify_goal;
