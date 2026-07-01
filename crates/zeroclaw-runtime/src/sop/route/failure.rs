use super::NextStep;
use crate::sop::step_contract::StepFailure;

pub fn route_failure(policy: &StepFailure, retries_consumed: u32, max_retries: u32) -> NextStep {
    match policy {
        StepFailure::Fail => NextStep::Fail("step failed".into()),
        StepFailure::Retry { max } => {
            let retry_limit = (*max).min(max_retries);
            if retries_consumed < retry_limit {
                NextStep::Retry
            } else {
                NextStep::Fail("step retry limit reached".into())
            }
        }
        StepFailure::Goto { step } => NextStep::Step(*step),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_respects_global_limit() {
        let policy = StepFailure::Retry { max: 10 };

        assert_eq!(route_failure(&policy, 0, 2), NextStep::Retry);
        assert_eq!(route_failure(&policy, 1, 2), NextStep::Retry);
        assert_eq!(
            route_failure(&policy, 2, 2),
            NextStep::Fail("step retry limit reached".into())
        );
    }
}
