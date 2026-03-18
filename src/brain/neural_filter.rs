//! Neural filter layer using ruvector-nervous-system:
//!
//! - **DentateGyrus** — pattern separation via sparse random projection (384→512 dim).
//! - **HdcMemory** — hyperdimensional computing tag index for fast tag-based lookup.

use anyhow::Result;
use ruvector_nervous_system::{DentateGyrus, HdcMemory, Hypervector};

const SOFT_DEDUP_THRESHOLD: f32 = 0.98;
const SPARSE_DIM: usize = 512;
const ACTIVE_K: usize = 16;
const GYRUS_SEED: u64 = 42;

pub struct NeuralFilter {
    dentate: DentateGyrus,
    pub hdc_tags: HdcMemory,
}

impl NeuralFilter {
    pub fn new(embed_dim: usize) -> Result<Self> {
        let dentate = DentateGyrus::new(embed_dim, SPARSE_DIM, ACTIVE_K, GYRUS_SEED);
        let hdc_tags = HdcMemory::new();
        Ok(Self { dentate, hdc_tags })
    }

    pub fn sparse_code(&self, embedding: &[f32]) -> Vec<f32> {
        self.dentate.encode_dense(embedding)
    }

    pub fn is_suspicious_duplicate(&self, embedding: &[f32], known_codes: &[Vec<f32>]) -> bool {
        let new_code = self.sparse_code(embedding);
        known_codes
            .iter()
            .map(|code| jaccard_f32(&new_code, code))
            .fold(0.0_f32, f32::max)
            > SOFT_DEDUP_THRESHOLD
    }

    pub fn store_tag_signature(&mut self, memory_id: &str, tags: &[&str]) {
        if tags.is_empty() { return; }
        let hvs: Vec<Hypervector> = tags.iter()
            .map(|t| Hypervector::from_seed(tag_seed(t)))
            .collect();
        if let Ok(bundled) = Hypervector::bundle(&hvs) {
            self.hdc_tags.store(memory_id, bundled);
        }
    }

    pub fn find_by_tags(&self, tags: &[&str], k: usize) -> Vec<(String, f32)> {
        if tags.is_empty() || self.hdc_tags.is_empty() {
            return vec![];
        }
        let hvs: Vec<Hypervector> = tags.iter()
            .map(|t| Hypervector::from_seed(tag_seed(t)))
            .collect();
        match Hypervector::bundle(&hvs) {
            Ok(query_hv) => self.hdc_tags.retrieve_top_k(&query_hv, k),
            Err(_) => vec![],
        }
    }

    pub fn hdc_size(&self) -> usize {
        self.hdc_tags.len()
    }
}

fn tag_seed(s: &str) -> u64 {
    let mut hash: u64 = 14695981039346656037;
    for byte in s.as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(1099511628211);
    }
    hash
}

fn jaccard_f32(a: &[f32], b: &[f32]) -> f32 {
    let len = a.len().min(b.len());
    let mut intersection = 0u32;
    let mut union_ = 0u32;
    for i in 0..len {
        let ai = a[i] > 0.0;
        let bi = b[i] > 0.0;
        if ai || bi { union_ += 1; }
        if ai && bi { intersection += 1; }
    }
    if union_ == 0 { 0.0 } else { intersection as f32 / union_ as f32 }
}
