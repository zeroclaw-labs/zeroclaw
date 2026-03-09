use num_complex::Complex64;
use std::f64::consts::FRAC_1_SQRT_2;

use super::state::QuantumState;

pub type Gate2x2 = [[Complex64; 2]; 2];

pub fn identity() -> Gate2x2 {
    [
        [Complex64::new(1.0, 0.0), Complex64::new(0.0, 0.0)],
        [Complex64::new(0.0, 0.0), Complex64::new(1.0, 0.0)],
    ]
}

pub fn pauli_x() -> Gate2x2 {
    [
        [Complex64::new(0.0, 0.0), Complex64::new(1.0, 0.0)],
        [Complex64::new(1.0, 0.0), Complex64::new(0.0, 0.0)],
    ]
}

pub fn pauli_y() -> Gate2x2 {
    [
        [Complex64::new(0.0, 0.0), Complex64::new(0.0, -1.0)],
        [Complex64::new(0.0, 1.0), Complex64::new(0.0, 0.0)],
    ]
}

pub fn pauli_z() -> Gate2x2 {
    [
        [Complex64::new(1.0, 0.0), Complex64::new(0.0, 0.0)],
        [Complex64::new(0.0, 0.0), Complex64::new(-1.0, 0.0)],
    ]
}

pub fn hadamard() -> Gate2x2 {
    let h = FRAC_1_SQRT_2;
    [
        [Complex64::new(h, 0.0), Complex64::new(h, 0.0)],
        [Complex64::new(h, 0.0), Complex64::new(-h, 0.0)],
    ]
}

pub fn rx(theta: f64) -> Gate2x2 {
    let c = (theta / 2.0).cos();
    let s = (theta / 2.0).sin();
    [
        [Complex64::new(c, 0.0), Complex64::new(0.0, -s)],
        [Complex64::new(0.0, -s), Complex64::new(c, 0.0)],
    ]
}

pub fn ry(theta: f64) -> Gate2x2 {
    let c = (theta / 2.0).cos();
    let s = (theta / 2.0).sin();
    [
        [Complex64::new(c, 0.0), Complex64::new(-s, 0.0)],
        [Complex64::new(s, 0.0), Complex64::new(c, 0.0)],
    ]
}

pub fn rz(theta: f64) -> Gate2x2 {
    [
        [
            Complex64::from_polar(1.0, -theta / 2.0),
            Complex64::new(0.0, 0.0),
        ],
        [
            Complex64::new(0.0, 0.0),
            Complex64::from_polar(1.0, theta / 2.0),
        ],
    ]
}

pub fn apply_single_qubit_gate(state: &mut QuantumState, target: usize, gate: &Gate2x2) {
    let n = state.num_qubits;
    let dim = state.dimension();
    let mut new_amps = vec![Complex64::new(0.0, 0.0); dim];
    let bit = n - 1 - target;

    for i in 0..dim {
        let b = (i >> bit) & 1;
        let partner = i ^ (1 << bit);

        if b == 0 {
            new_amps[i] +=
                gate[0][0] * state.amplitudes[i] + gate[0][1] * state.amplitudes[partner];
            new_amps[partner] +=
                gate[1][0] * state.amplitudes[i] + gate[1][1] * state.amplitudes[partner];
        }
    }
    state.amplitudes = new_amps;
}

pub fn apply_cnot(state: &mut QuantumState, control: usize, target: usize) {
    let n = state.num_qubits;
    let dim = state.dimension();
    let control_bit = n - 1 - control;
    let target_bit = n - 1 - target;
    let mut new_amps = state.amplitudes.clone();

    for i in 0..dim {
        let c = (i >> control_bit) & 1;
        if c == 1 {
            let flipped = i ^ (1 << target_bit);
            new_amps[i] = state.amplitudes[flipped];
            new_amps[flipped] = state.amplitudes[i];
        }
    }
    state.amplitudes = new_amps;
}

pub fn is_unitary(gate: &Gate2x2) -> bool {
    let adjoint = [
        [gate[0][0].conj(), gate[1][0].conj()],
        [gate[0][1].conj(), gate[1][1].conj()],
    ];

    let product = [
        [
            gate[0][0] * adjoint[0][0] + gate[0][1] * adjoint[1][0],
            gate[0][0] * adjoint[0][1] + gate[0][1] * adjoint[1][1],
        ],
        [
            gate[1][0] * adjoint[0][0] + gate[1][1] * adjoint[1][0],
            gate[1][0] * adjoint[0][1] + gate[1][1] * adjoint[1][1],
        ],
    ];

    let i00 = (product[0][0] - Complex64::new(1.0, 0.0)).norm();
    let i01 = product[0][1].norm();
    let i10 = product[1][0].norm();
    let i11 = (product[1][1] - Complex64::new(1.0, 0.0)).norm();

    i00 < 1e-10 && i01 < 1e-10 && i10 < 1e-10 && i11 < 1e-10
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::PI;

    #[test]
    fn all_gates_are_unitary() {
        assert!(is_unitary(&identity()));
        assert!(is_unitary(&pauli_x()));
        assert!(is_unitary(&pauli_y()));
        assert!(is_unitary(&pauli_z()));
        assert!(is_unitary(&hadamard()));
        assert!(is_unitary(&rx(PI / 4.0)));
        assert!(is_unitary(&ry(PI / 3.0)));
        assert!(is_unitary(&rz(PI / 6.0)));
    }

    #[test]
    fn hadamard_creates_superposition() {
        let mut state = QuantumState::new(1);
        apply_single_qubit_gate(&mut state, 0, &hadamard());
        let probs = state.probabilities();
        assert!((probs[0] - 0.5).abs() < 1e-10);
        assert!((probs[1] - 0.5).abs() < 1e-10);
    }

    #[test]
    fn pauli_x_flips_state() {
        let mut state = QuantumState::new(1);
        apply_single_qubit_gate(&mut state, 0, &pauli_x());
        let probs = state.probabilities();
        assert!((probs[0] - 0.0).abs() < 1e-10);
        assert!((probs[1] - 1.0).abs() < 1e-10);
    }

    #[test]
    fn cnot_creates_bell_state() {
        let mut state = QuantumState::new(2);
        apply_single_qubit_gate(&mut state, 0, &hadamard());
        apply_cnot(&mut state, 0, 1);
        let probs = state.probabilities();
        assert!((probs[0] - 0.5).abs() < 1e-10); // |00⟩
        assert!((probs[1] - 0.0).abs() < 1e-10); // |01⟩
        assert!((probs[2] - 0.0).abs() < 1e-10); // |10⟩
        assert!((probs[3] - 0.5).abs() < 1e-10); // |11⟩
    }

    #[test]
    fn rx_ry_rz_parameterized() {
        for angle in [0.0, PI / 4.0, PI / 2.0, PI, 3.0 * PI / 2.0] {
            assert!(is_unitary(&rx(angle)));
            assert!(is_unitary(&ry(angle)));
            assert!(is_unitary(&rz(angle)));
        }
    }

    #[test]
    fn cnot_no_flip_when_control_zero() {
        let mut state = QuantumState::new(2);
        // |00⟩ → CNOT → |00⟩
        apply_cnot(&mut state, 0, 1);
        let probs = state.probabilities();
        assert!((probs[0] - 1.0).abs() < 1e-10);
    }
}
