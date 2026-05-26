use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum LayerDecision {
    Read,
    Write,
    Defer,
    Reject,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CognitiveLayerOutput {
    pub layer: String,
    pub decision: LayerDecision,
    pub reasoning_summary: String,
    pub confidence: f64,
    pub source_id: String,
}

pub trait CognitiveLayer {
    fn name(&self) -> &'static str;
    fn process(&self, input: &str) -> CognitiveLayerOutput;
}

#[cfg(test)]
mod tests {
    use super::*;

    struct DummyLayer;

    impl CognitiveLayer for DummyLayer {
        fn name(&self) -> &'static str {
            "dummy"
        }

        fn process(&self, input: &str) -> CognitiveLayerOutput {
            CognitiveLayerOutput {
                layer: self.name().to_string(),
                decision: if input.contains("reject") {
                    LayerDecision::Reject
                } else {
                    LayerDecision::Read
                },
                reasoning_summary: format!("processed: {}", &input[..input.len().min(20)]),
                confidence: 0.8,
                source_id: "dummy_layer".to_string(),
            }
        }
    }

    #[test]
    fn trait_contract_works() {
        let layer = DummyLayer;
        assert_eq!(layer.name(), "dummy");
        let output = layer.process("hello world");
        assert_eq!(output.layer, "dummy");
        assert_eq!(output.decision, LayerDecision::Read);
        assert_eq!(output.confidence, 0.8);
    }

    #[test]
    fn trait_reject_decision() {
        let layer = DummyLayer;
        let output = layer.process("please reject this");
        assert_eq!(output.decision, LayerDecision::Reject);
    }
}
