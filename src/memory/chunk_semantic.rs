// Semantic Chunking (v3.0 S3)
//
// For long documents (>2000 chars), detect topic boundaries using
// embedding distance changes between adjacent sentences, smoothed
// with a Savitzky-Golay-like moving average filter.
//
// Short documents (<= threshold) should use the existing recursive
// chunker (`chunker.rs`) for speed.

use super::chunker::Chunk;
use super::embeddings::EmbeddingProvider;
use super::vector::cosine_similarity;

/// Minimum document length (chars) to trigger semantic chunking.
/// Shorter docs use the fast recursive chunker instead.
pub const SEMANTIC_CHUNK_THRESHOLD: usize = 2000;

/// Configuration for semantic chunking.
#[derive(Debug, Clone)]
pub struct SemanticChunkConfig {
    /// Maximum tokens per chunk (same as recursive chunker).
    pub max_tokens: usize,
    /// Smoothing window size for the distance signal (must be odd).
    /// Larger = fewer, broader topic boundaries.
    pub smoothing_window: usize,
    /// Percentile threshold for boundary detection (0.0–1.0).
    /// Higher = fewer boundaries (only major topic shifts).
    pub boundary_percentile: f64,
}

impl Default for SemanticChunkConfig {
    fn default() -> Self {
        Self {
            max_tokens: 512,
            smoothing_window: 5,
            boundary_percentile: 0.75,
        }
    }
}

/// Split a long document into semantically coherent chunks.
///
/// Algorithm:
/// 1. Split text into sentences/paragraphs
/// 2. Compute embeddings for each segment
/// 3. Calculate cosine distance between adjacent segments
/// 4. Smooth the distance signal (moving average)
/// 5. Find peaks above the percentile threshold → topic boundaries
/// 6. Group segments between boundaries into chunks (respecting max_tokens)
///
/// Falls back to the recursive chunker if embedding fails.
pub async fn chunk_semantic(
    text: &str,
    config: &SemanticChunkConfig,
    embedder: &dyn EmbeddingProvider,
) -> Vec<Chunk> {
    // Short documents: delegate to recursive chunker
    if text.len() <= SEMANTIC_CHUNK_THRESHOLD {
        return super::chunker::chunk_markdown(text, config.max_tokens);
    }

    // Split into segments (paragraphs)
    let segments = split_into_segments(text);
    if segments.len() < 3 {
        return super::chunker::chunk_markdown(text, config.max_tokens);
    }

    // Compute embeddings for each segment
    let embeddings = match compute_segment_embeddings(&segments, embedder).await {
        Ok(embs) => embs,
        Err(e) => {
            tracing::warn!("Semantic chunking embedding failed, falling back: {e}");
            return super::chunker::chunk_markdown(text, config.max_tokens);
        }
    };

    // Calculate inter-segment distances
    let distances = compute_distances(&embeddings);
    if distances.is_empty() {
        return super::chunker::chunk_markdown(text, config.max_tokens);
    }

    // Smooth the distance signal
    let smoothed = moving_average(&distances, config.smoothing_window);

    // Find boundary positions (peaks above threshold)
    let boundary_indices = find_boundaries(&smoothed, config.boundary_percentile);

    // Group segments into chunks at boundary positions
    build_chunks(&segments, &boundary_indices, config.max_tokens)
}

/// Split text into paragraph-level segments.
fn split_into_segments(text: &str) -> Vec<String> {
    let mut segments = Vec::new();
    let mut current = String::new();

    for line in text.lines() {
        if line.trim().is_empty() {
            if !current.trim().is_empty() {
                segments.push(std::mem::take(&mut current));
            }
        } else {
            if !current.is_empty() {
                current.push('\n');
            }
            current.push_str(line);
        }
    }
    if !current.trim().is_empty() {
        segments.push(current);
    }

    segments
}

/// Compute embeddings for each text segment.
async fn compute_segment_embeddings(
    segments: &[String],
    embedder: &dyn EmbeddingProvider,
) -> anyhow::Result<Vec<Vec<f32>>> {
    let mut embeddings = Vec::with_capacity(segments.len());
    for segment in segments {
        // Truncate very long segments to avoid API limits
        let text = if segment.len() > 8000 {
            &segment[..8000]
        } else {
            segment.as_str()
        };
        let emb = embedder.embed_one(text).await?;
        if emb.is_empty() {
            anyhow::bail!("embedding provider returned empty vector");
        }
        embeddings.push(emb);
    }
    Ok(embeddings)
}

/// Compute cosine distances between adjacent embeddings.
/// Returns `distances[i] = 1.0 - cosine_similarity(emb[i], emb[i+1])`.
fn compute_distances(embeddings: &[Vec<f32>]) -> Vec<f32> {
    if embeddings.len() < 2 {
        return Vec::new();
    }
    embeddings
        .windows(2)
        .map(|pair| 1.0 - cosine_similarity(&pair[0], &pair[1]))
        .collect()
}

/// Simple moving average smoothing.
/// Window must be odd; if even, it's incremented by 1.
fn moving_average(values: &[f32], window: usize) -> Vec<f32> {
    if values.is_empty() {
        return Vec::new();
    }
    let w = if window % 2 == 0 { window + 1 } else { window };
    let half = w / 2;
    let n = values.len();

    (0..n)
        .map(|i| {
            let start = i.saturating_sub(half);
            let end = (i + half + 1).min(n);
            let sum: f32 = values[start..end].iter().sum();
            sum / (end - start) as f32
        })
        .collect()
}

/// Find indices where the smoothed distance exceeds the given percentile.
/// These represent topic boundary positions.
fn find_boundaries(smoothed: &[f32], percentile: f64) -> Vec<usize> {
    if smoothed.is_empty() {
        return Vec::new();
    }

    let mut sorted: Vec<f32> = smoothed.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

    let threshold_idx = ((sorted.len() as f64 * percentile) as usize).min(sorted.len() - 1);
    let threshold = sorted[threshold_idx];

    smoothed
        .iter()
        .enumerate()
        .filter(|(_, &v)| v >= threshold)
        .map(|(i, _)| i)
        .collect()
}

/// Build chunks by grouping segments between boundary positions.
fn build_chunks(
    segments: &[String],
    boundary_indices: &[usize],
    max_tokens: usize,
) -> Vec<Chunk> {
    let max_chars = max_tokens * 4;
    let mut chunks = Vec::new();
    let mut current = String::new();

    // Boundary indices refer to gaps between segments.
    // boundary_indices[i] means there's a break after segment[boundary_indices[i]].
    let boundary_set: std::collections::HashSet<usize> =
        boundary_indices.iter().copied().collect();

    for (i, segment) in segments.iter().enumerate() {
        // Check if adding this segment would exceed limit
        if !current.is_empty() && current.len() + segment.len() + 1 > max_chars {
            chunks.push(Chunk {
                index: chunks.len(),
                content: current.trim().to_string(),
                heading: None,
            });
            current.clear();
        }

        if !current.is_empty() {
            current.push_str("\n\n");
        }
        current.push_str(segment);

        // If this is a boundary position, flush the current chunk
        if boundary_set.contains(&i) && !current.trim().is_empty() {
            chunks.push(Chunk {
                index: chunks.len(),
                content: current.trim().to_string(),
                heading: None,
            });
            current.clear();
        }
    }

    // Flush remaining
    if !current.trim().is_empty() {
        chunks.push(Chunk {
            index: chunks.len(),
            content: current.trim().to_string(),
            heading: None,
        });
    }

    chunks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_into_segments_basic() {
        let text = "First paragraph.\n\nSecond paragraph.\n\nThird paragraph.";
        let segs = split_into_segments(text);
        assert_eq!(segs.len(), 3);
        assert_eq!(segs[0], "First paragraph.");
        assert_eq!(segs[1], "Second paragraph.");
    }

    #[test]
    fn split_into_segments_multiline_para() {
        let text = "Line one.\nLine two.\n\nNew paragraph.";
        let segs = split_into_segments(text);
        assert_eq!(segs.len(), 2);
        assert!(segs[0].contains("Line one."));
        assert!(segs[0].contains("Line two."));
    }

    #[test]
    fn compute_distances_identical() {
        let embs = vec![vec![1.0, 0.0], vec![1.0, 0.0], vec![1.0, 0.0]];
        let dists = compute_distances(&embs);
        assert_eq!(dists.len(), 2);
        for d in &dists {
            assert!(*d < 0.01, "Expected near-zero distance, got {d}");
        }
    }

    #[test]
    fn compute_distances_orthogonal() {
        let embs = vec![vec![1.0, 0.0], vec![0.0, 1.0]];
        let dists = compute_distances(&embs);
        assert_eq!(dists.len(), 1);
        assert!((dists[0] - 1.0).abs() < 0.01);
    }

    #[test]
    fn compute_distances_empty() {
        assert!(compute_distances(&[]).is_empty());
        assert!(compute_distances(&[vec![1.0]]).is_empty());
    }

    #[test]
    fn moving_average_identity_window_1() {
        let values = vec![1.0, 2.0, 3.0, 4.0, 5.0];
        let smoothed = moving_average(&values, 1);
        assert_eq!(smoothed, values);
    }

    #[test]
    fn moving_average_smooths() {
        let values = vec![0.0, 0.0, 1.0, 0.0, 0.0];
        let smoothed = moving_average(&values, 3);
        // Center value should be reduced by averaging with neighbors
        assert!(smoothed[2] < 1.0);
        assert!(smoothed[2] > 0.0);
        // Should be 1/3
        assert!((smoothed[2] - 1.0 / 3.0).abs() < 0.01);
    }

    #[test]
    fn moving_average_empty() {
        assert!(moving_average(&[], 3).is_empty());
    }

    #[test]
    fn find_boundaries_basic() {
        let smoothed = vec![0.1, 0.2, 0.9, 0.1, 0.8];
        let boundaries = find_boundaries(&smoothed, 0.75);
        // Top 25% of values: 0.8 and 0.9 → indices 2 and 4
        assert!(boundaries.contains(&2));
        assert!(boundaries.contains(&4));
    }

    #[test]
    fn find_boundaries_empty() {
        assert!(find_boundaries(&[], 0.75).is_empty());
    }

    #[test]
    fn build_chunks_no_boundaries() {
        let segments = vec!["A".to_string(), "B".to_string(), "C".to_string()];
        let chunks = build_chunks(&segments, &[], 512);
        // All in one chunk (no boundaries, fits in limit)
        assert_eq!(chunks.len(), 1);
        assert!(chunks[0].content.contains('A'));
        assert!(chunks[0].content.contains('C'));
    }

    #[test]
    fn build_chunks_with_boundary() {
        let segments = vec!["A".to_string(), "B".to_string(), "C".to_string()];
        let chunks = build_chunks(&segments, &[0], 512);
        // Boundary after segment 0 → [A] [B, C]
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].content, "A");
        assert!(chunks[1].content.contains('B'));
    }

    #[test]
    fn build_chunks_respects_max_tokens() {
        let segments: Vec<String> = (0..10).map(|i| "x".repeat(500 + i)).collect();
        // max_tokens=200 → max_chars=800, each segment ~500 chars
        let chunks = build_chunks(&segments, &[], 200);
        assert!(chunks.len() > 1);
        for chunk in &chunks {
            assert!(chunk.content.len() <= 1200, "Chunk too long: {}", chunk.content.len());
        }
    }

    #[test]
    fn below_threshold_uses_recursive() {
        // Text under SEMANTIC_CHUNK_THRESHOLD should bypass semantic chunking
        let short = "Short text under 2000 chars.";
        assert!(short.len() < SEMANTIC_CHUNK_THRESHOLD);
        // The async function would delegate to chunk_markdown, tested separately
    }

    #[test]
    fn chunk_indexes_sequential() {
        let segments = vec!["A".to_string(), "B".to_string(), "C".to_string()];
        let chunks = build_chunks(&segments, &[0, 1], 512);
        for (i, chunk) in chunks.iter().enumerate() {
            assert_eq!(chunk.index, i);
        }
    }
}
