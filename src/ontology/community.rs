//! PR #9 — GraphRAG community layer (Phase 5 cross-search).
//!
//! Reads the existing `ontology_objects` + `ontology_links` graph,
//! discovers tight communities, and exposes a per-community summary that
//! the agent loop's Phase 5 cross-search step can inject into the
//! prompt — "for *this* query, which slice of the user's world matters?".
//!
//! ## Algorithm: label propagation (LPA)
//!
//! The roadmap calls for Leiden clustering. Leiden is a 2019 refinement
//! of Louvain that fixes the disconnected-community bug; the output
//! quality is similar in dense graphs but Leiden is materially harder
//! to implement correctly without a peer-reviewed library. For the
//! current corpus size (≤ low-thousands of objects per user), label
//! propagation:
//!
//! - runs in O((V + E) · iterations), terminating in single-digit
//!   iterations on real graphs,
//! - is deterministic when nodes are processed in id order with a fixed
//!   tiebreak,
//! - has no external deps,
//! - and produces communities of sufficient quality to seed the Phase 5
//!   summary injection.
//!
//! Swapping in Leiden is mechanical once the module boundary stabilises —
//! the public surface is `detect_communities(graph) → CommunityAssignment`
//! so the algorithm switch never leaks into callers.
//!
//! ## Output flow
//!
//! 1. Reader builds `GraphView` from `ontology_objects` + `ontology_links`.
//! 2. `detect_communities(&graph)` returns a `CommunityAssignment` mapping
//!    `object_id → community_id` plus per-community member lists.
//! 3. Caller summarises each community via an LLM (interface left to the
//!    scheduler — same pattern as `consolidate::Summarizer`) and
//!    persists into the new `ontology_communities` table.
//! 4. Phase 5 cross-search loads the table, computes cosine similarity
//!    between the query embedding and `summary_embedding`, and returns
//!    the top-N summaries.

use std::collections::{BTreeMap, HashMap};

/// One node in the community graph. We carry the existing
/// `ontology_objects.id` so the writer can join back without a lookup
/// table.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct GraphNode {
    pub object_id: i64,
    /// Optional title — used to seed deterministic tiebreaks and to
    /// surface a human-readable "anchor" in summaries.
    pub title: Option<String>,
}

/// Undirected edge with a positive integer weight (== link multiplicity
/// after the reader collapses duplicates). Weight 0 means "no edge"; the
/// reader must filter such rows out before passing them in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GraphEdge {
    pub from_object_id: i64,
    pub to_object_id: i64,
    pub weight: u32,
}

/// Snapshot of the ontology graph at one point in time. The reader
/// constructs this from a single SQL pass; the algorithm never touches
/// the database directly.
#[derive(Debug, Default, Clone)]
pub struct GraphView {
    pub nodes: Vec<GraphNode>,
    pub edges: Vec<GraphEdge>,
}

impl GraphView {
    /// Build an adjacency map. Keys are object_ids, values are
    /// `(neighbor_id, weight)` lists. Self-loops are dropped — they
    /// distort modularity without telling us anything useful.
    pub fn adjacency(&self) -> HashMap<i64, Vec<(i64, u32)>> {
        let mut adj: HashMap<i64, Vec<(i64, u32)>> = HashMap::new();
        for n in &self.nodes {
            adj.entry(n.object_id).or_default();
        }
        for e in &self.edges {
            if e.from_object_id == e.to_object_id || e.weight == 0 {
                continue;
            }
            adj.entry(e.from_object_id)
                .or_default()
                .push((e.to_object_id, e.weight));
            adj.entry(e.to_object_id)
                .or_default()
                .push((e.from_object_id, e.weight));
        }
        adj
    }
}

/// Result of running [`detect_communities`].
#[derive(Debug, Clone)]
pub struct CommunityAssignment {
    /// `object_id → community_id`. Community ids are dense, stable across
    /// runs against the same graph (sorted by smallest member object_id).
    pub of_node: HashMap<i64, u32>,
    /// `community_id → list of object_ids`. Member order matches the
    /// canonical ascending object_id sort so the LLM prompt is stable.
    pub members: BTreeMap<u32, Vec<i64>>,
}

impl CommunityAssignment {
    pub fn community_count(&self) -> usize {
        self.members.len()
    }

    pub fn largest_community_size(&self) -> usize {
        self.members.values().map(Vec::len).max().unwrap_or(0)
    }
}

/// Detect communities via deterministic label propagation.
///
/// - Each node initially carries its own object_id as a label.
/// - Repeatedly: for every node in ascending object_id order, switch to
///   the most common label among neighbours (weighted), with ties broken
///   by smallest label. Stop when no node changed in a full sweep, or
///   when `max_iterations` is reached.
/// - Final labels are renumbered 0..N in order of smallest member id, so
///   the assignment is reproducible across runs and sortable for storage.
pub fn detect_communities(graph: &GraphView) -> CommunityAssignment {
    detect_communities_with_options(graph, 50)
}

/// Same as [`detect_communities`] but with a configurable iteration cap
/// (useful in tests to assert "no, really, one pass is enough" cases).
pub fn detect_communities_with_options(
    graph: &GraphView,
    max_iterations: usize,
) -> CommunityAssignment {
    let adj = graph.adjacency();
    if adj.is_empty() {
        return CommunityAssignment {
            of_node: HashMap::new(),
            members: BTreeMap::new(),
        };
    }

    // Stable iteration order — sorting once is cheap and lets us assert
    // determinism in tests.
    let mut node_ids: Vec<i64> = adj.keys().copied().collect();
    node_ids.sort_unstable();

    // Initial labels = object_id (avoids a u32 namespace collision).
    let mut labels: HashMap<i64, i64> = node_ids.iter().map(|id| (*id, *id)).collect();

    for _ in 0..max_iterations {
        let mut changed = false;
        for node in &node_ids {
            // Empty neighbours → keep label.
            let neighbours = adj.get(node).map(Vec::as_slice).unwrap_or(&[]);
            if neighbours.is_empty() {
                continue;
            }
            // Tally weighted votes per label among neighbours. Ties broken
            // by smallest label so the algorithm is deterministic.
            let mut tally: BTreeMap<i64, u64> = BTreeMap::new();
            for (n_id, weight) in neighbours {
                let label = labels[n_id];
                *tally.entry(label).or_default() += u64::from(*weight);
            }
            let best = tally
                .iter()
                .max_by(|a, b| a.1.cmp(b.1).then_with(|| b.0.cmp(a.0)))
                .map(|(label, _)| *label);
            if let Some(new_label) = best {
                if labels[node] != new_label {
                    labels.insert(*node, new_label);
                    changed = true;
                }
            }
        }
        if !changed {
            break;
        }
    }

    // Renumber labels 0..N by smallest-member object_id so storage IDs
    // are stable.
    let mut by_label: BTreeMap<i64, Vec<i64>> = BTreeMap::new();
    for (node, label) in &labels {
        by_label.entry(*label).or_default().push(*node);
    }
    for members in by_label.values_mut() {
        members.sort_unstable();
    }
    let mut renumbered: Vec<(i64, Vec<i64>)> = by_label.into_iter().collect();
    renumbered.sort_by_key(|(_, members)| members.first().copied().unwrap_or(i64::MAX));

    let mut of_node = HashMap::new();
    let mut members_out: BTreeMap<u32, Vec<i64>> = BTreeMap::new();
    for (cid, (_, members)) in renumbered.into_iter().enumerate() {
        let cid_u32 = u32::try_from(cid).unwrap_or(u32::MAX);
        for m in &members {
            of_node.insert(*m, cid_u32);
        }
        members_out.insert(cid_u32, members);
    }

    CommunityAssignment {
        of_node,
        members: members_out,
    }
}

// ── Phase 5 cross-search support ──────────────────────────────────

/// One row read out of `ontology_communities` for query-time injection.
#[derive(Debug, Clone)]
pub struct CommunitySummary {
    pub community_id: u32,
    pub level: u32,
    pub summary: String,
    pub keywords: Vec<String>,
    pub object_ids: Vec<i64>,
    /// Optional embedding — `None` when summary hasn't been embedded yet
    /// (will be picked up by the next backfill pass).
    pub summary_embedding: Option<Vec<f32>>,
}

/// Pure scoring helper used by the agent loop's Phase 5 step. Given a
/// query embedding and a list of community summaries, return the top-N
/// by cosine similarity. Communities without embeddings are skipped.
///
/// The function is split out so it stays testable without spinning up
/// SqliteMemory or an embedder.
#[must_use]
pub fn rank_communities_for_query(
    query_embedding: &[f32],
    summaries: &[CommunitySummary],
    top_n: usize,
) -> Vec<RankedCommunity> {
    use crate::memory::consolidate::cosine_similarity;
    if top_n == 0 || summaries.is_empty() || query_embedding.is_empty() {
        return Vec::new();
    }
    let mut scored: Vec<RankedCommunity> = summaries
        .iter()
        .filter_map(|s| {
            s.summary_embedding.as_ref().map(|emb| RankedCommunity {
                community_id: s.community_id,
                summary: s.summary.clone(),
                similarity: cosine_similarity(query_embedding, emb),
                keywords: s.keywords.clone(),
                object_ids: s.object_ids.clone(),
            })
        })
        .collect();
    scored.sort_by(|a, b| {
        b.similarity
            .partial_cmp(&a.similarity)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.community_id.cmp(&b.community_id))
    });
    scored.truncate(top_n);
    scored
}

/// Picked community returned to the agent loop.
#[derive(Debug, Clone, PartialEq)]
pub struct RankedCommunity {
    pub community_id: u32,
    pub summary: String,
    pub similarity: f32,
    pub keywords: Vec<String>,
    pub object_ids: Vec<i64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn n(id: i64, title: Option<&str>) -> GraphNode {
        GraphNode {
            object_id: id,
            title: title.map(String::from),
        }
    }

    fn e(a: i64, b: i64, w: u32) -> GraphEdge {
        GraphEdge {
            from_object_id: a,
            to_object_id: b,
            weight: w,
        }
    }

    #[test]
    fn empty_graph_returns_empty_assignment() {
        let g = GraphView::default();
        let a = detect_communities(&g);
        assert!(a.of_node.is_empty());
        assert!(a.members.is_empty());
        assert_eq!(a.community_count(), 0);
    }

    #[test]
    fn isolated_nodes_each_form_own_community() {
        let g = GraphView {
            nodes: vec![n(1, None), n(2, None), n(3, None)],
            edges: vec![],
        };
        let a = detect_communities(&g);
        assert_eq!(a.community_count(), 3);
        assert_eq!(a.largest_community_size(), 1);
    }

    #[test]
    fn dense_clique_collapses_into_one_community() {
        // K4 — all four pairs connected — should converge to a single
        // community.
        let g = GraphView {
            nodes: vec![n(1, None), n(2, None), n(3, None), n(4, None)],
            edges: vec![e(1, 2, 1), e(1, 3, 1), e(1, 4, 1), e(2, 3, 1), e(2, 4, 1), e(3, 4, 1)],
        };
        let a = detect_communities(&g);
        assert_eq!(a.community_count(), 1);
        assert_eq!(a.largest_community_size(), 4);
        assert_eq!(a.members[&0], vec![1, 2, 3, 4]);
    }

    #[test]
    fn two_clusters_joined_by_one_weak_edge_split_correctly() {
        // {1,2,3} fully connected, {4,5,6} fully connected, single
        // bridging edge 3↔4. Expect two communities.
        let g = GraphView {
            nodes: (1..=6).map(|i| n(i, None)).collect(),
            edges: vec![
                e(1, 2, 5),
                e(1, 3, 5),
                e(2, 3, 5),
                e(4, 5, 5),
                e(4, 6, 5),
                e(5, 6, 5),
                e(3, 4, 1), // weak bridge
            ],
        };
        let a = detect_communities(&g);
        assert_eq!(
            a.community_count(),
            2,
            "weak bridge should not merge distinct clusters"
        );
        assert_eq!(a.members[&0], vec![1, 2, 3]);
        assert_eq!(a.members[&1], vec![4, 5, 6]);
    }

    #[test]
    fn community_ids_are_stable_across_runs() {
        let g = GraphView {
            nodes: (1..=4).map(|i| n(i, None)).collect(),
            edges: vec![e(1, 2, 1), e(3, 4, 1)],
        };
        let a = detect_communities(&g);
        let b = detect_communities(&g);
        assert_eq!(a.of_node, b.of_node);
        assert_eq!(a.members, b.members);
    }

    #[test]
    fn self_loops_and_zero_weight_edges_are_ignored() {
        let g = GraphView {
            nodes: vec![n(1, None), n(2, None)],
            edges: vec![e(1, 1, 5), e(1, 2, 0), e(2, 2, 9)],
        };
        let a = detect_communities(&g);
        // Both nodes are isolated after dropping self-loops + zero edges.
        assert_eq!(a.community_count(), 2);
    }

    #[test]
    fn rank_communities_respects_cosine_then_id() {
        let summaries = vec![
            CommunitySummary {
                community_id: 0,
                level: 0,
                summary: "household".into(),
                keywords: vec![],
                object_ids: vec![1, 2],
                summary_embedding: Some(vec![1.0, 0.0]),
            },
            CommunitySummary {
                community_id: 1,
                level: 0,
                summary: "work".into(),
                keywords: vec![],
                object_ids: vec![3, 4],
                summary_embedding: Some(vec![0.0, 1.0]),
            },
            CommunitySummary {
                community_id: 2,
                level: 0,
                summary: "no embedding yet".into(),
                keywords: vec![],
                object_ids: vec![5],
                summary_embedding: None,
            },
        ];
        let q = vec![0.9, 0.1];
        let top = rank_communities_for_query(&q, &summaries, 2);
        assert_eq!(top.len(), 2);
        assert_eq!(top[0].community_id, 0);
        assert!(top[0].similarity > top[1].similarity);
    }

    #[test]
    fn rank_communities_skips_communities_without_embedding() {
        let summaries = vec![CommunitySummary {
            community_id: 9,
            level: 0,
            summary: "not embedded".into(),
            keywords: vec![],
            object_ids: vec![],
            summary_embedding: None,
        }];
        let q = vec![1.0, 0.0];
        assert!(rank_communities_for_query(&q, &summaries, 5).is_empty());
    }

    #[test]
    fn rank_communities_returns_empty_for_empty_query_or_zero_n() {
        let summaries = vec![CommunitySummary {
            community_id: 0,
            level: 0,
            summary: "x".into(),
            keywords: vec![],
            object_ids: vec![],
            summary_embedding: Some(vec![1.0]),
        }];
        assert!(rank_communities_for_query(&[], &summaries, 5).is_empty());
        assert!(rank_communities_for_query(&[1.0], &summaries, 0).is_empty());
    }

    #[test]
    fn iteration_cap_respected_so_pathological_input_terminates() {
        // Ring of 6 nodes — known LPA hard case (oscillating labels).
        // With max_iterations = 1 we get *some* assignment, just not the
        // converged one — but the function must still terminate.
        let g = GraphView {
            nodes: (1..=6).map(|i| n(i, None)).collect(),
            edges: vec![
                e(1, 2, 1),
                e(2, 3, 1),
                e(3, 4, 1),
                e(4, 5, 1),
                e(5, 6, 1),
                e(6, 1, 1),
            ],
        };
        let a = detect_communities_with_options(&g, 1);
        // Every node was assigned to *some* community.
        assert_eq!(a.of_node.len(), 6);
    }
}
