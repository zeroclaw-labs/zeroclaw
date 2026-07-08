use crate::sop::types::SopRun;

pub fn step_visit_count(run: &SopRun, step_number: u32) -> u32 {
    run.step_results
        .iter()
        .filter(|result| result.step_number == step_number)
        .count()
        .try_into()
        .unwrap_or(u32::MAX)
}

pub fn within_visit_bound(run: &SopRun, step_number: u32, max_visits: u32) -> bool {
    step_visit_count(run, step_number) < max_visits
}
