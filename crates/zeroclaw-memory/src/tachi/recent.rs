//! Translate existing host scope into the kernel's pre-limit read predicates.

use super::{
    DEFAULT_AGENT, DEFAULT_NAMESPACE, METADATA_SESSION_ID, METADATA_SUPERSEDED_BY,
    METADATA_ZC_AGENT, METADATA_ZC_NAMESPACE,
};
use portable_kernel::store::scoped_read::{MetadataTextFilter, RecentMemoryQuery, TextMembership};
use portable_kernel::{MemoryEntry, MemoryStore};
use std::collections::HashSet;

// The pinned kernel bounds metadata values per query, not the host allowlist.
const MAX_QUERY_VALUES: usize = 256;

pub(super) fn queries(
    prefix: String,
    limit: usize,
    session_id: Option<&str>,
    namespace: Option<&str>,
    allowed_agents: Option<&[&str]>,
) -> Vec<RecentMemoryQuery> {
    let mut query = RecentMemoryQuery::new(prefix, limit);
    if let Some(session) = session_id {
        query.filters.push(MetadataTextFilter {
            field: METADATA_SESSION_ID.into(),
            values: vec![session.into()],
            membership: TextMembership::AnyOf,
            missing_value: None,
        });
    }
    // Exclude soft-superseded rows before applying the SQL LIMIT so
    // superseded records cannot starve active rows in bounded recent recall.
    query.filters.push(MetadataTextFilter {
        field: METADATA_SUPERSEDED_BY.into(),
        values: vec![String::new()],
        membership: TextMembership::AnyOf,
        missing_value: Some(String::new()),
    });
    let (membership, value) = match namespace {
        Some(namespace) => (TextMembership::AnyOf, namespace),
        None => (TextMembership::NoneOf, crate::SOUL_NAMESPACE),
    };
    query.filters.push(MetadataTextFilter {
        field: METADATA_ZC_NAMESPACE.into(),
        values: vec![value.into()],
        membership,
        missing_value: Some(DEFAULT_NAMESPACE.into()),
    });
    let Some(allowed) = allowed_agents else {
        return vec![query];
    };
    let fixed_values: usize = query.filters.iter().map(|filter| filter.values.len()).sum();
    allowed
        .chunks(MAX_QUERY_VALUES - fixed_values)
        .map(|agents| {
            let mut batch = query.clone();
            batch.filters.push(MetadataTextFilter {
                field: METADATA_ZC_AGENT.into(),
                values: agents.iter().map(|agent| (*agent).to_owned()).collect(),
                membership: TextMembership::AnyOf,
                missing_value: Some(DEFAULT_AGENT.into()),
            });
            batch
        })
        .collect()
}

pub(super) fn read(
    store: &MemoryStore,
    queries: &[RecentMemoryQuery],
    limit: usize,
) -> anyhow::Result<(Vec<MemoryEntry>, Option<String>)> {
    let mut rows: Vec<MemoryEntry> = Vec::new();
    let mut max_saturated_tail_ts: Option<String> = None;
    for query in queries {
        let batch_rows = store
            .list_recent_matching(query)
            .map_err(|e| anyhow::Error::msg(format!("tachi list_recent failed: {e}")))?;
        if query.limit > 0
            && batch_rows.len() >= query.limit
            && let Some(tail) = batch_rows.last()
            && max_saturated_tail_ts
                .as_deref()
                .is_none_or(|prev| tail.timestamp.as_str() > prev)
        {
            max_saturated_tail_ts = Some(tail.timestamp.clone());
        }
        rows.extend(batch_rows);
        // Each batch has the same predicates and limit. Its top `limit` rows
        // suffice for the global top `limit`, ordered by nanosecond-canonical
        // effective `updated_at` DESC with id tie-breaker.
        rows.sort_unstable_by(|a, b| {
            super::TachiMemory::effective_updated_sort_key(b)
                .cmp(&super::TachiMemory::effective_updated_sort_key(a))
                .then_with(|| a.id.cmp(&b.id))
        });
        let mut seen = HashSet::new();
        rows.retain(|row| seen.insert(row.id.clone()));
        rows.truncate(limit);
    }
    Ok((rows, max_saturated_tail_ts))
}
