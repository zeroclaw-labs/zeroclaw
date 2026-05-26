mod store;
mod types;
mod verify;

#[allow(unused_imports)]
pub use store::{
    approve, complete, get, list, propose, reject, revert_to_approved, set_in_progress,
};
pub use types::{Goal, GoalSource, GoalStatus, VerificationMethod};
pub use verify::verify_goal;
