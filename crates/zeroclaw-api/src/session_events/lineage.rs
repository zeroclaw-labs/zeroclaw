//! Frozen branch selection derived only from committed branch origins.
use super::*;
use std::collections::{BTreeMap, HashMap, HashSet};

pub fn frozen_lineage<'a>(
    events: impl IntoIterator<Item = &'a SessionEvent>,
    stream: &SessionStreamId,
    branch: &BranchId,
    through: u64,
) -> Result<BTreeMap<BranchId, u64>, ReadError> {
    let origins: HashMap<_, _> = events
        .into_iter()
        .filter(|e| {
            &e.stream_id == stream
                && matches!(
                    e.body.kind(),
                    EventKind::SessionStarted
                        | EventKind::DerivedSessionStarted
                        | EventKind::BranchCreated
                )
        })
        .filter_map(|e| e.branch_id.as_ref().map(|b| (b.clone(), e)))
        .collect();
    let mut selected = BTreeMap::new();
    let mut seen = HashSet::new();
    let mut current = branch.clone();
    let mut bound = through;
    loop {
        if !seen.insert(current.clone()) {
            return Err(ReadError::Corrupt("cyclic branch lineage".into()));
        }
        let origin = origins
            .get(&current)
            .ok_or_else(|| ReadError::Gap("branch origin missing".into()))?;
        selected.insert(current.clone(), bound);
        match &origin.body.data {
            EventData::BranchCreated {
                parent_branch_id,
                parent_cursor,
            } => {
                if &parent_cursor.stream_id != stream {
                    return Err(ReadError::Corrupt("cross-stream branch lineage".into()));
                }
                bound = bound.min(parent_cursor.sequence);
                current = parent_branch_id.clone();
            }
            _ => return Ok(selected),
        }
    }
}

pub fn includes(lineage: &BTreeMap<BranchId, u64>, event: &SessionEvent) -> bool {
    event
        .branch_id
        .as_ref()
        .and_then(|b| lineage.get(b))
        .is_some_and(|bound| event.sequence <= *bound)
}
