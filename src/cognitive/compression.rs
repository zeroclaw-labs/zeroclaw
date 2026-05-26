use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompressedMemory {
    pub summary: String,
    pub original_count: usize,
    pub fidelity: f64,
    pub source_id: String,
    pub timestamp: u64,
    pub confidence: f64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MemoryCompressor {
    pub compressed: Vec<CompressedMemory>,
}

impl MemoryCompressor {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn compress_batch(&mut self, items: &[String], fidelity: f64) -> CompressedMemory {
        let count = items.len();
        let summary = if count <= 3 {
            items.join("; ")
        } else {
            let first_three: Vec<&str> = items.iter().take(3).map(|s| s.as_str()).collect();
            format!("{} (+{} more)", first_three.join("; "), count - 3)
        };
        let cm = CompressedMemory {
            summary,
            original_count: count,
            fidelity: fidelity.clamp(0.0, 1.0),
            source_id: String::new(),
            timestamp: 0,
            confidence: fidelity.clamp(0.0, 1.0),
        };
        self.compressed.push(cm.clone());
        cm
    }

    pub fn retrieve(&self, index: usize) -> Option<&CompressedMemory> {
        self.compressed.get(index)
    }

    pub fn total_original_count(&self) -> usize {
        self.compressed.iter().map(|c| c.original_count).sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compress_reduces_count() {
        let mut mc = MemoryCompressor::new();
        let items: Vec<String> = (0..10).map(|i| format!("memory_{i}")).collect();
        let cm = mc.compress_batch(&items, 0.7);
        assert_eq!(cm.original_count, 10);
        assert!(cm.summary.contains("+7 more"));
        assert_eq!(mc.compressed.len(), 1);
        assert_eq!(mc.total_original_count(), 10);
    }

    #[test]
    fn small_batch_no_truncation() {
        let mut mc = MemoryCompressor::new();
        let items = vec!["a".into(), "b".into()];
        let cm = mc.compress_batch(&items, 0.9);
        assert_eq!(cm.summary, "a; b");
        assert_eq!(cm.original_count, 2);
    }
}
