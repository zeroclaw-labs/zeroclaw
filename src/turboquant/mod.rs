#![allow(
    clippy::many_single_char_names,
    clippy::unreadable_literal,
    clippy::cast_possible_truncation,
    clippy::cast_lossless,
    clippy::manual_midpoint,
    clippy::manual_div_ceil,
    clippy::excessive_precision,
    clippy::needless_range_loop
)]

use rand::RngExt;
use serde::{Deserialize, Serialize};
use std::f64::consts::PI;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LloydMaxCodebook {
    pub bits: u8,
    pub centroids: Vec<f64>,
    pub boundaries: Vec<f64>,
    pub distortion_per_coord: f64,
}

impl LloydMaxCodebook {
    pub fn solve(bits: u8) -> Self {
        let n_levels = 1usize << bits;
        let sigma = 1.0;

        let mut centroids: Vec<f64> = (0..n_levels)
            .map(|i| {
                let t = (i as f64 + 0.5) / n_levels as f64;
                sigma * normal_quantile(t)
            })
            .collect();

        for _ in 0..200 {
            let mut boundaries = Vec::with_capacity(n_levels - 1);
            for i in 0..n_levels - 1 {
                boundaries.push((centroids[i] + centroids[i + 1]) / 2.0);
            }

            let mut new_centroids = Vec::with_capacity(n_levels);
            for i in 0..n_levels {
                let lo = if i == 0 {
                    -6.0 * sigma
                } else {
                    boundaries[i - 1]
                };
                let hi = if i == n_levels - 1 {
                    6.0 * sigma
                } else {
                    boundaries[i]
                };

                let (num, den) = gauss_quadrature_conditional_mean(lo, hi, sigma);
                if den > 1e-15 {
                    new_centroids.push(num / den);
                } else {
                    new_centroids.push(centroids[i]);
                }
            }

            let max_shift = centroids
                .iter()
                .zip(&new_centroids)
                .map(|(a, b)| (a - b).abs())
                .fold(0.0_f64, f64::max);

            centroids = new_centroids;
            if max_shift < 1e-12 {
                break;
            }
        }

        let mut boundaries = Vec::with_capacity(n_levels - 1);
        for i in 0..n_levels - 1 {
            boundaries.push((centroids[i] + centroids[i + 1]) / 2.0);
        }

        let distortion = compute_distortion(&centroids, &boundaries, sigma);

        Self {
            bits,
            centroids,
            boundaries,
            distortion_per_coord: distortion,
        }
    }

    pub fn quantize(&self, value: f64) -> u8 {
        for (i, &b) in self.boundaries.iter().enumerate() {
            if value < b {
                return i as u8;
            }
        }
        self.centroids.len() as u8 - 1
    }

    pub fn dequantize(&self, index: u8) -> f64 {
        self.centroids[index as usize]
    }
}

fn normal_pdf(x: f64, sigma: f64) -> f64 {
    let z = x / sigma;
    (-0.5 * z * z).exp() / (sigma * (2.0 * PI).sqrt())
}

fn normal_quantile(p: f64) -> f64 {
    let p = p.clamp(1e-10, 1.0 - 1e-10);
    let a = [
        -3.969683028665376e1,
        2.209460984245205e2,
        -2.759285104469687e2,
        1.383577518672690e2,
        -3.066479806614716e1,
        2.506628277459239e0,
    ];
    let b = [
        -5.447609879822406e1,
        1.615858368580409e2,
        -1.556989798598866e2,
        6.680131188771972e1,
        -1.328068155288572e1,
    ];
    let c = [
        -7.784894002430293e-3,
        -3.223964580411365e-1,
        -2.400758277161838e0,
        -2.549732539343734e0,
        4.374664141464968e0,
        2.938163982698783e0,
    ];
    let d = [
        7.784695709041462e-3,
        3.224671290700398e-1,
        2.445134137142996e0,
        3.754408661907416e0,
    ];

    let p_low = 0.02425;
    let p_high = 1.0 - p_low;

    if p < p_low {
        let q = (-2.0 * p.ln()).sqrt();
        (((((c[0] * q + c[1]) * q + c[2]) * q + c[3]) * q + c[4]) * q + c[5])
            / ((((d[0] * q + d[1]) * q + d[2]) * q + d[3]) * q + 1.0)
    } else if p <= p_high {
        let q = p - 0.5;
        let r = q * q;
        (((((a[0] * r + a[1]) * r + a[2]) * r + a[3]) * r + a[4]) * r + a[5]) * q
            / (((((b[0] * r + b[1]) * r + b[2]) * r + b[3]) * r + b[4]) * r + 1.0)
    } else {
        let q = (-2.0 * (1.0 - p).ln()).sqrt();
        -(((((c[0] * q + c[1]) * q + c[2]) * q + c[3]) * q + c[4]) * q + c[5])
            / ((((d[0] * q + d[1]) * q + d[2]) * q + d[3]) * q + 1.0)
    }
}

fn gauss_quadrature_conditional_mean(lo: f64, hi: f64, sigma: f64) -> (f64, f64) {
    let n = 32;
    let mid = (lo + hi) / 2.0;
    let half = (hi - lo) / 2.0;
    let mut num = 0.0;
    let mut den = 0.0;

    for i in 0..n {
        let t = (2.0 * (i as f64) + 1.0) / (2.0 * n as f64);
        let cos_val = (PI * t).cos();
        let x = mid + half * cos_val;
        let w = PI / n as f64 * (1.0 - cos_val * cos_val).sqrt();
        let pdf = normal_pdf(x, sigma);
        num += x * pdf * w * half;
        den += pdf * w * half;
    }

    (num, den)
}

fn compute_distortion(centroids: &[f64], boundaries: &[f64], sigma: f64) -> f64 {
    let n = centroids.len();
    let mut total = 0.0;

    for i in 0..n {
        let lo = if i == 0 {
            -6.0 * sigma
        } else {
            boundaries[i - 1]
        };
        let hi = if i == n - 1 {
            6.0 * sigma
        } else {
            boundaries[i]
        };
        let c = centroids[i];

        let steps = 32;
        let mid = (lo + hi) / 2.0;
        let half = (hi - lo) / 2.0;
        let mut integral = 0.0;

        for j in 0..steps {
            let t = (2.0 * (j as f64) + 1.0) / (2.0 * steps as f64);
            let cos_val = (PI * t).cos();
            let x = mid + half * cos_val;
            let w = PI / steps as f64 * (1.0 - cos_val * cos_val).sqrt();
            let pdf = normal_pdf(x, sigma);
            integral += (x - c).powi(2) * pdf * w * half;
        }

        total += integral;
    }

    total
}

pub fn generate_rotation_matrix(d: usize, seed: u64) -> Vec<Vec<f64>> {
    use rand::SeedableRng;
    let mut rng = rand::rngs::StdRng::seed_from_u64(seed);
    let mut matrix: Vec<Vec<f64>> = (0..d)
        .map(|_| (0..d).map(|_| rng.random::<f64>() * 2.0 - 1.0).collect())
        .collect();

    for i in 0..d {
        for j in 0..i {
            let dot: f64 = (0..d).map(|k| matrix[i][k] * matrix[j][k]).sum();
            for k in 0..d {
                matrix[i][k] -= dot * matrix[j][k];
            }
        }
        let norm: f64 = (0..d)
            .map(|k| matrix[i][k] * matrix[i][k])
            .sum::<f64>()
            .sqrt();
        if norm > 1e-10 {
            for k in 0..d {
                matrix[i][k] /= norm;
            }
        }
    }

    matrix
}

fn mat_vec_multiply(matrix: &[Vec<f64>], vec: &[f64]) -> Vec<f64> {
    matrix
        .iter()
        .map(|row| row.iter().zip(vec).map(|(a, b)| a * b).sum())
        .collect()
}

fn mat_transpose_vec_multiply(matrix: &[Vec<f64>], vec: &[f64]) -> Vec<f64> {
    let d = matrix[0].len();
    let mut result = vec![0.0; d];
    for (i, row) in matrix.iter().enumerate() {
        for (j, &val) in row.iter().enumerate() {
            result[j] += val * vec[i];
        }
    }
    result
}

fn dot_product(a: &[f64], b: &[f64]) -> f64 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

fn l2_norm(v: &[f64]) -> f64 {
    v.iter().map(|x| x * x).sum::<f64>().sqrt()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurboQuantMSE {
    dimension: usize,
    bits: u8,
    rotation_seed: u64,
    codebook: LloydMaxCodebook,
    #[serde(skip)]
    rotation_cache: Option<Vec<Vec<f64>>>,
}

impl TurboQuantMSE {
    pub fn new(dimension: usize, bits: u8, seed: u64) -> Self {
        let codebook = LloydMaxCodebook::solve(bits);
        Self {
            dimension,
            bits,
            rotation_seed: seed,
            codebook,
            rotation_cache: None,
        }
    }

    fn rotation(&mut self) -> &Vec<Vec<f64>> {
        if self.rotation_cache.is_none() {
            self.rotation_cache =
                Some(generate_rotation_matrix(self.dimension, self.rotation_seed));
        }
        self.rotation_cache.as_ref().unwrap()
    }

    pub fn quantize(&mut self, x: &[f64]) -> (Vec<u8>, Vec<f64>) {
        let norm = l2_norm(x);
        if norm < 1e-15 {
            return (vec![0; self.dimension], vec![0.0; self.dimension]);
        }

        let normalized: Vec<f64> = x.iter().map(|v| v / norm).collect();
        let rotation = self.rotation().clone();
        let rotated = mat_vec_multiply(&rotation, &normalized);
        let scale = (self.dimension as f64).sqrt();

        let indices: Vec<u8> = rotated
            .iter()
            .map(|&y| self.codebook.quantize(y * scale))
            .collect();

        let reconstructed_rotated: Vec<f64> = indices
            .iter()
            .map(|&idx| self.codebook.dequantize(idx) / scale)
            .collect();

        let reconstructed_normalized =
            mat_transpose_vec_multiply(&rotation, &reconstructed_rotated);
        let reconstructed: Vec<f64> = reconstructed_normalized.iter().map(|v| v * norm).collect();

        (indices, reconstructed)
    }

    pub fn dequantize(&mut self, indices: &[u8], norm: f64) -> Vec<f64> {
        let scale = (self.dimension as f64).sqrt();
        let reconstructed_rotated: Vec<f64> = indices
            .iter()
            .map(|&idx| self.codebook.dequantize(idx) / scale)
            .collect();

        let rotation = self.rotation().clone();
        let reconstructed_normalized =
            mat_transpose_vec_multiply(&rotation, &reconstructed_rotated);
        reconstructed_normalized.iter().map(|v| v * norm).collect()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompressedVector {
    pub mse_indices: Vec<u8>,
    pub qjl_signs: Vec<u8>,
    pub residual_norm: f64,
    pub original_norm: f64,
    pub dimension: usize,
}

impl CompressedVector {
    pub fn storage_bytes(&self) -> usize {
        self.mse_indices.len() + self.qjl_signs.len() + 16 + 4
    }
}

#[derive(Debug, Clone)]
pub struct TurboQuantProd {
    mse: TurboQuantMSE,
    qjl_seed: u64,
    qjl_cache: Option<Vec<Vec<f64>>>,
}

impl TurboQuantProd {
    pub fn new(dimension: usize, bits: u8, mse_seed: u64, qjl_seed: u64) -> Self {
        Self {
            mse: TurboQuantMSE::new(dimension, bits, mse_seed),
            qjl_seed,
            qjl_cache: None,
        }
    }

    fn qjl_matrix(&mut self) -> &Vec<Vec<f64>> {
        if self.qjl_cache.is_none() {
            self.qjl_cache = Some(generate_rotation_matrix(self.mse.dimension, self.qjl_seed));
        }
        self.qjl_cache.as_ref().unwrap()
    }

    pub fn quantize(&mut self, x: &[f64]) -> CompressedVector {
        let original_norm = l2_norm(x);
        let (indices, reconstruction) = self.mse.quantize(x);

        let residual: Vec<f64> = x.iter().zip(&reconstruction).map(|(a, b)| a - b).collect();
        let residual_norm = l2_norm(&residual);

        let qjl = self.qjl_matrix().clone();
        let projected = mat_vec_multiply(&qjl, &residual);

        let mut qjl_signs = vec![0u8; (self.mse.dimension + 7) / 8];
        for (i, &p) in projected.iter().enumerate() {
            if p >= 0.0 {
                qjl_signs[i / 8] |= 1 << (i % 8);
            }
        }

        CompressedVector {
            mse_indices: indices,
            qjl_signs,
            residual_norm,
            original_norm,
            dimension: self.mse.dimension,
        }
    }

    pub fn inner_product(&mut self, query: &[f64], compressed: &CompressedVector) -> f64 {
        let x_mse = self
            .mse
            .dequantize(&compressed.mse_indices, compressed.original_norm);
        let term1 = dot_product(query, &x_mse);

        if compressed.residual_norm < 1e-15 {
            return term1;
        }

        let qjl = self.qjl_matrix().clone();
        let q_projected = mat_vec_multiply(&qjl, query);

        let mut qjl_ip = 0.0;
        for (i, &qp) in q_projected.iter().enumerate() {
            let sign_bit = (compressed.qjl_signs[i / 8] >> (i % 8)) & 1;
            let sign = if sign_bit == 1 { 1.0 } else { -1.0 };
            qjl_ip += qp * sign;
        }

        let m = self.mse.dimension as f64;
        let correction_scale = (PI / 2.0).sqrt() / m;
        let term2 = compressed.residual_norm * correction_scale * qjl_ip;

        term1 + term2
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompressedMemoryEntry {
    pub compressed: CompressedVector,
    pub tick: u64,
    pub domain: String,
    pub metadata: serde_json::Value,
}

#[derive(Debug)]
pub struct CompressedMemoryStore {
    quantizer: TurboQuantProd,
    entries: Vec<CompressedMemoryEntry>,
    capacity: usize,
}

impl CompressedMemoryStore {
    pub fn new(dimension: usize, bits: u8, capacity: usize) -> Self {
        Self {
            quantizer: TurboQuantProd::new(dimension, bits, 42, 137),
            entries: Vec::with_capacity(capacity.min(4096)),
            capacity,
        }
    }

    pub fn store(
        &mut self,
        vector: &[f64],
        tick: u64,
        domain: String,
        metadata: serde_json::Value,
    ) {
        if self.entries.len() >= self.capacity {
            self.entries.remove(0);
        }

        let compressed = self.quantizer.quantize(vector);
        self.entries.push(CompressedMemoryEntry {
            compressed,
            tick,
            domain,
            metadata,
        });
    }

    pub fn attention_scores(&mut self, query: &[f64]) -> Vec<(usize, f64)> {
        self.entries
            .iter()
            .enumerate()
            .map(|(i, entry)| (i, self.quantizer.inner_product(query, &entry.compressed)))
            .collect()
    }

    pub fn top_k_attention(&mut self, query: &[f64], k: usize) -> Vec<(usize, f64)> {
        let mut scores = self.attention_scores(query);
        scores.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        scores.truncate(k);
        scores
    }

    pub fn domain_attention(&mut self, query: &[f64], domain: &str) -> Vec<(usize, f64)> {
        self.entries
            .iter()
            .enumerate()
            .filter(|(_, e)| e.domain == domain)
            .map(|(i, entry)| (i, self.quantizer.inner_product(query, &entry.compressed)))
            .collect()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn memory_bytes(&self) -> usize {
        self.entries
            .iter()
            .map(|e| e.compressed.storage_bytes())
            .sum()
    }

    pub fn compression_ratio(&self, original_element_bytes: usize) -> f64 {
        if self.entries.is_empty() {
            return 0.0;
        }
        let original =
            self.entries.len() * self.entries[0].compressed.dimension * original_element_bytes;
        let compressed = self.memory_bytes();
        if compressed == 0 {
            0.0
        } else {
            original as f64 / compressed as f64
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::SeedableRng;

    #[test]
    fn lloyd_max_codebook_3bit_has_8_centroids() {
        let cb = LloydMaxCodebook::solve(3);
        assert_eq!(cb.centroids.len(), 8);
        assert_eq!(cb.boundaries.len(), 7);
        for i in 0..cb.centroids.len() - 1 {
            assert!(cb.centroids[i] < cb.centroids[i + 1]);
        }
    }

    #[test]
    fn lloyd_max_quantize_dequantize_roundtrip() {
        let cb = LloydMaxCodebook::solve(3);
        let idx = cb.quantize(0.5);
        let val = cb.dequantize(idx);
        assert!((val - 0.5).abs() < 0.5);
    }

    #[test]
    fn rotation_matrix_is_orthogonal() {
        let d = 16;
        let mat = generate_rotation_matrix(d, 42);
        for i in 0..d {
            let norm: f64 = (0..d).map(|k| mat[i][k] * mat[i][k]).sum::<f64>().sqrt();
            assert!((norm - 1.0).abs() < 1e-10, "Row {i} norm = {norm}");
        }
        for i in 0..d {
            for j in i + 1..d {
                let dot: f64 = (0..d).map(|k| mat[i][k] * mat[j][k]).sum();
                assert!(dot.abs() < 1e-10, "Rows {i},{j} dot = {dot}");
            }
        }
    }

    #[test]
    fn mse_quantize_preserves_direction() {
        let d = 32;
        let mut tq = TurboQuantMSE::new(d, 3, 42);
        let x: Vec<f64> = (0..d).map(|i| (i as f64 * 0.1).sin()).collect();

        let (_, reconstruction) = tq.quantize(&x);

        let cos_sim = dot_product(&x, &reconstruction) / (l2_norm(&x) * l2_norm(&reconstruction));
        assert!(cos_sim > 0.8, "Cosine similarity {cos_sim} too low");
    }

    #[test]
    fn turboquant_prod_inner_product_is_unbiased() {
        let d = 64;
        let mut tq = TurboQuantProd::new(d, 3, 42, 137);
        let mut rng = rand::rngs::StdRng::seed_from_u64(999);

        let n_trials = 200;
        let mut total_error = 0.0;

        for _ in 0..n_trials {
            let x: Vec<f64> = (0..d).map(|_| rng.random::<f64>() * 2.0 - 1.0).collect();
            let y: Vec<f64> = (0..d).map(|_| rng.random::<f64>() * 2.0 - 1.0).collect();

            let true_ip = dot_product(&x, &y);
            let compressed = tq.quantize(&x);
            let estimated_ip = tq.inner_product(&y, &compressed);

            total_error += estimated_ip - true_ip;
        }

        let avg_bias = total_error / n_trials as f64;
        assert!(
            avg_bias.abs() < 0.5,
            "Average bias {avg_bias} is too large — should be near zero"
        );
    }

    #[test]
    fn compressed_memory_store_basic_operations() {
        let d = 32;
        let mut store = CompressedMemoryStore::new(d, 3, 100);

        let v1: Vec<f64> = (0..d).map(|i| (i as f64 * 0.1).sin()).collect();
        let v2: Vec<f64> = (0..d).map(|i| (i as f64 * 0.2).cos()).collect();

        store.store(&v1, 1, "experience".to_string(), serde_json::json!({}));
        store.store(&v2, 2, "neuro".to_string(), serde_json::json!({}));

        assert_eq!(store.len(), 2);
        assert!(store.compression_ratio(8) > 1.0);
    }

    #[test]
    fn compressed_memory_store_attention_retrieval() {
        let d = 32;
        let mut store = CompressedMemoryStore::new(d, 3, 100);
        let mut rng = rand::rngs::StdRng::seed_from_u64(42);

        let target: Vec<f64> = (0..d).map(|_| rng.random::<f64>()).collect();
        store.store(&target, 0, "target".to_string(), serde_json::json!({}));

        for i in 1..20 {
            let noise: Vec<f64> = (0..d).map(|_| rng.random::<f64>() * 2.0 - 1.0).collect();
            store.store(&noise, i, "noise".to_string(), serde_json::json!({}));
        }

        let top = store.top_k_attention(&target, 3);
        assert_eq!(top[0].0, 0, "Target should be most similar to itself");
    }

    #[test]
    fn compressed_memory_store_capacity_eviction() {
        let d = 16;
        let mut store = CompressedMemoryStore::new(d, 2, 5);

        for i in 0..10 {
            let v: Vec<f64> = (0..d).map(|j| (i * d + j) as f64).collect();
            store.store(&v, i as u64, "test".to_string(), serde_json::json!({}));
        }

        assert_eq!(store.len(), 5);
    }

    #[test]
    fn compressed_memory_store_domain_filtering() {
        let d = 16;
        let mut store = CompressedMemoryStore::new(d, 3, 100);

        let v1: Vec<f64> = (0..d).map(|i| i as f64).collect();
        let v2: Vec<f64> = (0..d).map(|i| (i as f64) * 2.0).collect();
        let v3: Vec<f64> = (0..d).map(|i| (i as f64) * 3.0).collect();

        store.store(&v1, 1, "experience".to_string(), serde_json::json!({}));
        store.store(&v2, 2, "neuro".to_string(), serde_json::json!({}));
        store.store(&v3, 3, "experience".to_string(), serde_json::json!({}));

        let query: Vec<f64> = (0..d).map(|i| i as f64).collect();
        let exp_scores = store.domain_attention(&query, "experience");
        assert_eq!(exp_scores.len(), 2);

        let neuro_scores = store.domain_attention(&query, "neuro");
        assert_eq!(neuro_scores.len(), 1);
    }
}
