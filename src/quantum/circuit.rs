use rand::Rng;
use serde::{Deserialize, Serialize};
use std::f64::consts::PI;

use super::gates;
use super::state::QuantumState;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum GateOp {
    H(usize),
    X(usize),
    Y(usize),
    Z(usize),
    Rx(usize, f64),
    Ry(usize, f64),
    Rz(usize, f64),
    Cnot(usize, usize),
    Measure(usize),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuantumCircuit {
    pub num_qubits: usize,
    pub ops: Vec<GateOp>,
    pub parameters: Vec<f64>,
}

impl QuantumCircuit {
    pub fn new(num_qubits: usize) -> Self {
        Self {
            num_qubits,
            ops: Vec::new(),
            parameters: Vec::new(),
        }
    }

    pub fn h(&mut self, target: usize) -> &mut Self {
        self.ops.push(GateOp::H(target));
        self
    }

    pub fn x(&mut self, target: usize) -> &mut Self {
        self.ops.push(GateOp::X(target));
        self
    }

    pub fn y(&mut self, target: usize) -> &mut Self {
        self.ops.push(GateOp::Y(target));
        self
    }

    pub fn z(&mut self, target: usize) -> &mut Self {
        self.ops.push(GateOp::Z(target));
        self
    }

    pub fn rx(&mut self, target: usize, param_idx: usize) -> &mut Self {
        self.ops.push(GateOp::Rx(target, param_idx as f64));
        self
    }

    pub fn ry(&mut self, target: usize, param_idx: usize) -> &mut Self {
        self.ops.push(GateOp::Ry(target, param_idx as f64));
        self
    }

    pub fn rz(&mut self, target: usize, param_idx: usize) -> &mut Self {
        self.ops.push(GateOp::Rz(target, param_idx as f64));
        self
    }

    pub fn cnot(&mut self, control: usize, target: usize) -> &mut Self {
        self.ops.push(GateOp::Cnot(control, target));
        self
    }

    pub fn measure(&mut self, target: usize) -> &mut Self {
        self.ops.push(GateOp::Measure(target));
        self
    }

    pub fn set_parameters(&mut self, params: Vec<f64>) {
        self.parameters = params;
    }

    pub fn execute(&self, rng: &mut impl Rng) -> CircuitResult {
        let mut state = QuantumState::new(self.num_qubits);
        let mut measurements = Vec::new();

        for op in &self.ops {
            match op {
                GateOp::H(t) => gates::apply_single_qubit_gate(&mut state, *t, &gates::hadamard()),
                GateOp::X(t) => gates::apply_single_qubit_gate(&mut state, *t, &gates::pauli_x()),
                GateOp::Y(t) => gates::apply_single_qubit_gate(&mut state, *t, &gates::pauli_y()),
                GateOp::Z(t) => gates::apply_single_qubit_gate(&mut state, *t, &gates::pauli_z()),
                GateOp::Rx(t, idx) => {
                    let angle = self.resolve_param(*idx);
                    gates::apply_single_qubit_gate(&mut state, *t, &gates::rx(angle));
                }
                GateOp::Ry(t, idx) => {
                    let angle = self.resolve_param(*idx);
                    gates::apply_single_qubit_gate(&mut state, *t, &gates::ry(angle));
                }
                GateOp::Rz(t, idx) => {
                    let angle = self.resolve_param(*idx);
                    gates::apply_single_qubit_gate(&mut state, *t, &gates::rz(angle));
                }
                GateOp::Cnot(c, t) => gates::apply_cnot(&mut state, *c, *t),
                GateOp::Measure(t) => {
                    let outcome = state.measure_and_collapse(rng);
                    let n = self.num_qubits;
                    let bit = (outcome >> (n - 1 - t)) & 1;
                    measurements.push((*t, bit == 1));
                }
            }
        }

        CircuitResult {
            final_state: state,
            measurements,
        }
    }

    fn resolve_param(&self, idx: f64) -> f64 {
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let i = idx as usize;
        if i < self.parameters.len() {
            self.parameters[i]
        } else {
            0.0
        }
    }

    pub fn gradient_parameter_shift(&self, param_idx: usize, rng: &mut impl Rng) -> f64 {
        let shift = PI / 2.0;

        let mut params_plus = self.parameters.clone();
        params_plus[param_idx] += shift;
        let mut circuit_plus = self.clone();
        circuit_plus.set_parameters(params_plus);
        let result_plus = circuit_plus.execute(rng);
        let expectation_plus = result_plus.expectation_z(0);

        let mut params_minus = self.parameters.clone();
        params_minus[param_idx] -= shift;
        let mut circuit_minus = self.clone();
        circuit_minus.set_parameters(params_minus);
        let result_minus = circuit_minus.execute(rng);
        let expectation_minus = result_minus.expectation_z(0);

        (expectation_plus - expectation_minus) / 2.0
    }
}

#[derive(Debug, Clone)]
pub struct CircuitResult {
    pub final_state: QuantumState,
    pub measurements: Vec<(usize, bool)>,
}

impl CircuitResult {
    pub fn expectation_z(&self, qubit: usize) -> f64 {
        let probs = self.final_state.probabilities();
        let n = self.final_state.num_qubits;
        let bit = n - 1 - qubit;
        let mut expectation = 0.0;
        for (i, p) in probs.iter().enumerate() {
            let eigenvalue = if (i >> bit) & 1 == 0 { 1.0 } else { -1.0 };
            expectation += eigenvalue * p;
        }
        expectation
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn circuit_bell_state() {
        let mut circuit = QuantumCircuit::new(2);
        circuit.h(0).cnot(0, 1);
        let mut rng = rand::rng();
        let result = circuit.execute(&mut rng);
        let probs = result.final_state.probabilities();
        assert!((probs[0] - 0.5).abs() < 1e-10);
        assert!((probs[3] - 0.5).abs() < 1e-10);
    }

    #[test]
    fn variational_circuit_parameterized() {
        let mut circuit = QuantumCircuit::new(1);
        circuit.ry(0, 0);
        circuit.set_parameters(vec![PI / 2.0]);
        let mut rng = rand::rng();
        let result = circuit.execute(&mut rng);
        let probs = result.final_state.probabilities();
        assert!((probs[0] - 0.5).abs() < 1e-10);
        assert!((probs[1] - 0.5).abs() < 1e-10);
    }

    #[test]
    fn expectation_z_on_zero_state() {
        let circuit = QuantumCircuit::new(1);
        let mut rng = rand::rng();
        let result = circuit.execute(&mut rng);
        let exp = result.expectation_z(0);
        assert!((exp - 1.0).abs() < 1e-10);
    }

    #[test]
    fn gradient_nonzero_for_ry() {
        let mut circuit = QuantumCircuit::new(1);
        circuit.ry(0, 0);
        circuit.set_parameters(vec![PI / 4.0]);
        let mut rng = rand::rng();
        let grad = circuit.gradient_parameter_shift(0, &mut rng);
        assert!(grad.abs() > 0.01);
    }
}
