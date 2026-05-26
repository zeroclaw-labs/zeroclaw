use num_complex::Complex64;
use rand::Rng;
use serde::{Deserialize, Serialize};
use std::f64::consts::FRAC_1_SQRT_2;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuantumState {
    pub amplitudes: Vec<Complex64>,
    pub phase: f64,
    pub num_qubits: usize,
}

impl QuantumState {
    pub fn new(num_qubits: usize) -> Self {
        let dim = 1 << num_qubits;
        let mut amplitudes = vec![Complex64::new(0.0, 0.0); dim];
        amplitudes[0] = Complex64::new(1.0, 0.0);
        Self {
            amplitudes,
            phase: 0.0,
            num_qubits,
        }
    }

    pub fn from_amplitudes(amplitudes: Vec<Complex64>) -> Self {
        let num_qubits = amplitudes.len().trailing_zeros() as usize;
        Self {
            amplitudes,
            phase: 0.0,
            num_qubits,
        }
    }

    pub fn dimension(&self) -> usize {
        self.amplitudes.len()
    }

    pub fn probabilities(&self) -> Vec<f64> {
        self.amplitudes.iter().map(|a| a.norm_sqr()).collect()
    }

    pub fn normalize(&mut self) {
        let norm: f64 = self
            .amplitudes
            .iter()
            .map(|a| a.norm_sqr())
            .sum::<f64>()
            .sqrt();
        if norm > 1e-15 {
            for a in &mut self.amplitudes {
                *a /= norm;
            }
        }
    }

    pub fn is_normalized(&self) -> bool {
        let total: f64 = self.amplitudes.iter().map(|a| a.norm_sqr()).sum();
        (total - 1.0).abs() < 1e-10
    }

    pub fn coherence(&self) -> f64 {
        let dim = self.amplitudes.len();
        if dim <= 1 {
            return 0.0;
        }
        let mut off_diagonal_sum = 0.0;
        for i in 0..dim {
            for j in 0..dim {
                if i != j {
                    off_diagonal_sum += (self.amplitudes[i] * self.amplitudes[j].conj()).norm();
                }
            }
        }
        off_diagonal_sum / (dim * (dim - 1)) as f64
    }

    pub fn measure<R: Rng + ?Sized>(&self, rng: &mut R) -> usize {
        let probs = self.probabilities();
        let r: f64 = rng.random();
        let mut cumulative = 0.0;
        for (i, p) in probs.iter().enumerate() {
            cumulative += p;
            if r < cumulative {
                return i;
            }
        }
        probs.len() - 1
    }

    pub fn measure_and_collapse<R: Rng + ?Sized>(&mut self, rng: &mut R) -> usize {
        let outcome = self.measure(rng);
        for (i, a) in self.amplitudes.iter_mut().enumerate() {
            if i == outcome {
                *a = Complex64::new(1.0, 0.0);
            } else {
                *a = Complex64::new(0.0, 0.0);
            }
        }
        outcome
    }

    pub fn apply_decoherence(&mut self, rate: f64) {
        let decay = (-rate).exp();
        for a in &mut self.amplitudes {
            let prob = a.norm_sqr();
            let phase = a.arg();
            let decayed_mag = prob.sqrt() * decay + (1.0 - decay) * prob.sqrt();
            *a = Complex64::from_polar(decayed_mag, phase * decay);
        }
        self.normalize();
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Qubit {
    pub alpha: Complex64,
    pub beta: Complex64,
}

impl Qubit {
    pub fn zero() -> Self {
        Self {
            alpha: Complex64::new(1.0, 0.0),
            beta: Complex64::new(0.0, 0.0),
        }
    }

    pub fn one() -> Self {
        Self {
            alpha: Complex64::new(0.0, 0.0),
            beta: Complex64::new(1.0, 0.0),
        }
    }

    pub fn plus() -> Self {
        Self {
            alpha: Complex64::new(FRAC_1_SQRT_2, 0.0),
            beta: Complex64::new(FRAC_1_SQRT_2, 0.0),
        }
    }

    pub fn minus() -> Self {
        Self {
            alpha: Complex64::new(FRAC_1_SQRT_2, 0.0),
            beta: Complex64::new(-FRAC_1_SQRT_2, 0.0),
        }
    }

    pub fn from_angles(theta: f64, phi: f64) -> Self {
        Self {
            alpha: Complex64::new((theta / 2.0).cos(), 0.0),
            beta: Complex64::from_polar((theta / 2.0).sin(), phi),
        }
    }

    pub fn probability_zero(&self) -> f64 {
        self.alpha.norm_sqr()
    }

    pub fn probability_one(&self) -> f64 {
        self.beta.norm_sqr()
    }

    pub fn is_normalized(&self) -> bool {
        (self.alpha.norm_sqr() + self.beta.norm_sqr() - 1.0).abs() < 1e-10
    }

    pub fn to_state(&self) -> QuantumState {
        QuantumState {
            amplitudes: vec![self.alpha, self.beta],
            phase: 0.0,
            num_qubits: 1,
        }
    }

    pub fn bloch_coords(&self) -> (f64, f64, f64) {
        let theta = 2.0 * self.alpha.norm().acos();
        let phi = self.beta.arg() - self.alpha.arg();
        let x = theta.sin() * phi.cos();
        let y = theta.sin() * phi.sin();
        let z = theta.cos();
        (x, y, z)
    }

    pub fn measure<R: Rng + ?Sized>(&self, rng: &mut R) -> bool {
        rng.random::<f64>() < self.probability_one()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuantumRegister {
    pub state: QuantumState,
}

impl QuantumRegister {
    pub fn new(num_qubits: usize) -> Self {
        Self {
            state: QuantumState::new(num_qubits),
        }
    }

    pub fn num_qubits(&self) -> usize {
        self.state.num_qubits
    }

    pub fn from_qubits(qubits: &[Qubit]) -> Self {
        let n = qubits.len();
        let dim = 1 << n;
        let mut amplitudes = vec![Complex64::new(0.0, 0.0); dim];

        for (i, amp_slot) in amplitudes.iter_mut().enumerate().take(dim) {
            let mut amp = Complex64::new(1.0, 0.0);
            for (q, qubit) in qubits.iter().enumerate() {
                if (i >> (n - 1 - q)) & 1 == 0 {
                    amp *= qubit.alpha;
                } else {
                    amp *= qubit.beta;
                }
            }
            *amp_slot = amp;
        }

        Self {
            state: QuantumState::from_amplitudes(amplitudes),
        }
    }

    pub fn measure_all<R: Rng + ?Sized>(&mut self, rng: &mut R) -> Vec<bool> {
        let outcome = self.state.measure_and_collapse(rng);
        let n = self.num_qubits();
        (0..n).map(|q| (outcome >> (n - 1 - q)) & 1 == 1).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qubit_zero_normalized() {
        let q = Qubit::zero();
        assert!(q.is_normalized());
        assert!((q.probability_zero() - 1.0).abs() < 1e-10);
    }

    #[test]
    fn qubit_plus_equal_superposition() {
        let q = Qubit::plus();
        assert!(q.is_normalized());
        assert!((q.probability_zero() - 0.5).abs() < 1e-10);
        assert!((q.probability_one() - 0.5).abs() < 1e-10);
    }

    #[test]
    fn quantum_state_normalization() {
        let mut state =
            QuantumState::from_amplitudes(vec![Complex64::new(3.0, 0.0), Complex64::new(4.0, 0.0)]);
        state.normalize();
        assert!(state.is_normalized());
    }

    #[test]
    fn register_from_qubits_tensor_product() {
        let reg = QuantumRegister::from_qubits(&[Qubit::zero(), Qubit::one()]);
        let probs = reg.state.probabilities();
        assert!((probs[0] - 0.0).abs() < 1e-10); // |00⟩
        assert!((probs[1] - 1.0).abs() < 1e-10); // |01⟩
        assert!((probs[2] - 0.0).abs() < 1e-10); // |10⟩
        assert!((probs[3] - 0.0).abs() < 1e-10); // |11⟩
    }

    #[test]
    fn measurement_born_rule_statistics() {
        let state = QuantumState::from_amplitudes(vec![
            Complex64::new(FRAC_1_SQRT_2, 0.0),
            Complex64::new(FRAC_1_SQRT_2, 0.0),
        ]);
        let mut rng = rand::rng();
        let mut counts = [0u32; 2];
        for _ in 0..1000 {
            counts[state.measure(&mut rng)] += 1;
        }
        let p0 = f64::from(counts[0]) / 1000.0;
        assert!((p0 - 0.5).abs() < 0.1);
    }

    #[test]
    fn bloch_sphere_zero_state() {
        let q = Qubit::zero();
        let (x, y, z) = q.bloch_coords();
        assert!(x.abs() < 1e-10);
        assert!(y.abs() < 1e-10);
        assert!((z - 1.0).abs() < 1e-10);
    }

    #[test]
    fn decoherence_reduces_coherence() {
        let mut state = QuantumState::from_amplitudes(vec![
            Complex64::new(FRAC_1_SQRT_2, 0.0),
            Complex64::new(FRAC_1_SQRT_2, 0.0),
        ]);
        let initial_coherence = state.coherence();
        state.apply_decoherence(0.5);
        assert!(state.coherence() <= initial_coherence + 1e-10);
    }
}
