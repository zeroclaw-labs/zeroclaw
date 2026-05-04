//! HMAC-SHA256 tool execution receipts for hallucination detection.
//!
//! When enabled, every tool execution produces a cryptographic receipt that
//! proves the tool actually ran. The LLM cannot forge valid receipts because
//! it doesn't know the ephemeral session key.
//!
//! Based on: Basu, A. (2026). "Tool Receipts, Not Zero-Knowledge Proofs:
//! Practical Hallucination Detection for AI Agents." arXiv:2603.10060

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use hmac::{Hmac, Mac};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

const RECEIPT_PREFIX: &str = "zc-receipt";

/// Generates and verifies HMAC-SHA256 receipts for tool executions.
/// Each session gets an ephemeral key — never exposed to the LLM.
#[derive(Clone)]
pub struct ReceiptGenerator {
    key: Vec<u8>,
}

impl Default for ReceiptGenerator {
    fn default() -> Self {
        Self::new()
    }
}

impl ReceiptGenerator {
    /// Create a new generator with a random 256-bit ephemeral key.
    pub fn new() -> Self {
        use ring::rand::{SecureRandom, SystemRandom};
        let mut key = vec![0u8; 32];
        SystemRandom::new()
            .fill(&mut key)
            .expect("system RNG failed");
        Self { key }
    }

    /// Create a generator with a known key (for testing).
    #[cfg(test)]
    pub fn with_key(key: Vec<u8>) -> Self {
        Self { key }
    }

    /// Generate a receipt for a tool execution.
    ///
    /// The receipt encodes: tool_name | args_hash | result_hash | timestamp
    /// into an HMAC-SHA256 digest, formatted as `zc-receipt-{timestamp}-{hash}`.
    pub fn generate(
        &self,
        tool_name: &str,
        args: &serde_json::Value,
        result: &str,
        timestamp: u64,
    ) -> String {
        let digest = self.compute_hmac(tool_name, args, result, timestamp);
        format!(
            "{RECEIPT_PREFIX}-{timestamp}-{}",
            URL_SAFE_NO_PAD.encode(digest)
        )
    }

    /// Generate a receipt using the current wall-clock time.
    pub fn generate_now(&self, tool_name: &str, args: &serde_json::Value, result: &str) -> String {
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        self.generate(tool_name, args, result, timestamp)
    }

    /// Verify a receipt against the expected tool execution parameters.
    ///
    /// Parses the timestamp from the receipt string, recomputes the HMAC,
    /// and compares. Returns `false` for malformed, tampered, or fabricated receipts.
    pub fn verify(
        &self,
        receipt: &str,
        tool_name: &str,
        args: &serde_json::Value,
        result: &str,
    ) -> bool {
        let Some((timestamp, provided_hash)) = parse_receipt(receipt) else {
            return false;
        };
        let Ok(provided_bytes) = URL_SAFE_NO_PAD.decode(provided_hash) else {
            return false;
        };
        let mut mac = HmacSha256::new_from_slice(&self.key).expect("HMAC accepts any key length");
        mac.update(tool_name.as_bytes());
        mac.update(b"|");
        mac.update(args.to_string().as_bytes());
        mac.update(b"|");
        mac.update(result.as_bytes());
        mac.update(b"|");
        mac.update(timestamp.to_string().as_bytes());
        mac.verify_slice(&provided_bytes).is_ok()
    }

    fn compute_hmac(
        &self,
        tool_name: &str,
        args: &serde_json::Value,
        result: &str,
        timestamp: u64,
    ) -> Vec<u8> {
        let mut mac = HmacSha256::new_from_slice(&self.key).expect("HMAC accepts any key length");
        mac.update(tool_name.as_bytes());
        mac.update(b"|");
        mac.update(args.to_string().as_bytes());
        mac.update(b"|");
        mac.update(result.as_bytes());
        mac.update(b"|");
        mac.update(timestamp.to_string().as_bytes());
        mac.finalize().into_bytes().to_vec()
    }
}

/// Per-turn receipt forwarding scope, used to thread the generator and
/// the per-turn collector through delegate sub-loops without changing the
/// `Tool` trait signature. Mirrors the pattern used by
/// `TOOL_LOOP_COST_TRACKING_CONTEXT`.
#[derive(Clone)]
pub struct ReceiptScope {
    pub generator: ReceiptGenerator,
    pub collector: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
}

tokio::task_local! {
    /// Set by the orchestrator when `[agent.tool_receipts] enabled = true`.
    /// `DelegateTool` reads this to forward receipts into sub-agent tool loops
    /// so subagent tool calls land in the same per-turn collector.
    pub static TOOL_LOOP_RECEIPT_CONTEXT: Option<ReceiptScope>;
}

/// Parse a receipt string into (timestamp, hash).
/// Expected format: `zc-receipt-{timestamp}-{base64url_hash}`
fn parse_receipt(receipt: &str) -> Option<(u64, &str)> {
    let rest = receipt.strip_prefix("zc-receipt-")?;
    let dash_pos = rest.find('-')?;
    let timestamp: u64 = rest[..dash_pos].parse().ok()?;
    let hash = &rest[dash_pos + 1..];
    if hash.is_empty() {
        return None;
    }
    Some((timestamp, hash))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_key() -> Vec<u8> {
        vec![42u8; 32]
    }

    fn test_args() -> serde_json::Value {
        serde_json::json!({"command": "date"})
    }

    #[test]
    fn receipt_generation_deterministic() {
        let receipt_gen = ReceiptGenerator::with_key(test_key());
        let r1 = receipt_gen.generate("shell", &test_args(), "Mon Mar 27", 1_711_547_700);
        let r2 = receipt_gen.generate("shell", &test_args(), "Mon Mar 27", 1_711_547_700);
        assert_eq!(r1, r2);
    }

    #[test]
    fn receipt_format_parseable() {
        let receipt_gen = ReceiptGenerator::with_key(test_key());
        let receipt = receipt_gen.generate("shell", &test_args(), "output", 1_711_547_700);
        assert!(receipt.starts_with("zc-receipt-1711547700-"));
        let (ts, hash) = parse_receipt(&receipt).unwrap();
        assert_eq!(ts, 1_711_547_700);
        assert!(!hash.is_empty());
    }

    #[test]
    fn receipt_verification_succeeds() {
        let receipt_gen = ReceiptGenerator::with_key(test_key());
        let args = test_args();
        let receipt = receipt_gen.generate("shell", &args, "output", 1_711_547_700);
        assert!(receipt_gen.verify(&receipt, "shell", &args, "output"));
    }

    #[test]
    fn receipt_verification_fails_tampered_result() {
        let receipt_gen = ReceiptGenerator::with_key(test_key());
        let args = test_args();
        let receipt = receipt_gen.generate("shell", &args, "real output", 1_711_547_700);
        assert!(!receipt_gen.verify(&receipt, "shell", &args, "fake output"));
    }

    #[test]
    fn receipt_verification_fails_tampered_name() {
        let receipt_gen = ReceiptGenerator::with_key(test_key());
        let args = test_args();
        let receipt = receipt_gen.generate("shell", &args, "output", 1_711_547_700);
        assert!(!receipt_gen.verify(&receipt, "web_search", &args, "output"));
    }

    #[test]
    fn receipt_verification_fails_wrong_key() {
        let receipt_gen1 = ReceiptGenerator::with_key(vec![1u8; 32]);
        let receipt_gen2 = ReceiptGenerator::with_key(vec![2u8; 32]);
        let args = test_args();
        let receipt = receipt_gen1.generate("shell", &args, "output", 1_711_547_700);
        assert!(!receipt_gen2.verify(&receipt, "shell", &args, "output"));
    }

    #[test]
    fn fabricated_receipt_fails_verification() {
        let receipt_gen = ReceiptGenerator::with_key(test_key());
        let args = test_args();
        let fake = "zc-receipt-1_711_547_700-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
        assert!(!receipt_gen.verify(fake, "shell", &args, "output"));
    }

    #[test]
    fn malformed_receipt_fails_verification() {
        let receipt_gen = ReceiptGenerator::with_key(test_key());
        let args = test_args();
        assert!(!receipt_gen.verify("not-a-receipt", "shell", &args, "output"));
        assert!(!receipt_gen.verify("zc-receipt-", "shell", &args, "output"));
        assert!(!receipt_gen.verify("zc-receipt-abc-hash", "shell", &args, "output"));
        assert!(!receipt_gen.verify("zc-receipt-123-", "shell", &args, "output"));
    }

    #[test]
    fn receipt_from_different_tool_fails() {
        let receipt_gen = ReceiptGenerator::with_key(test_key());
        let args_a = serde_json::json!({"query": "rust"});
        let args_b = serde_json::json!({"path": "/tmp"});
        let receipt = receipt_gen.generate("web_search", &args_a, "results", 1_711_547_700);
        assert!(!receipt_gen.verify(&receipt, "file_read", &args_b, "results"));
    }

    #[test]
    fn receipt_with_modified_args_fails() {
        let receipt_gen = ReceiptGenerator::with_key(test_key());
        let args_real = serde_json::json!({"command": "date"});
        let args_fake = serde_json::json!({"command": "rm -rf /"});
        let receipt = receipt_gen.generate("shell", &args_real, "Mon Mar 27", 1_711_547_700);
        assert!(!receipt_gen.verify(&receipt, "shell", &args_fake, "Mon Mar 27"));
    }

    #[test]
    fn generate_now_produces_valid_receipt() {
        let receipt_gen = ReceiptGenerator::with_key(test_key());
        let args = test_args();
        let receipt = receipt_gen.generate_now("shell", &args, "output");
        assert!(receipt.starts_with("zc-receipt-"));
        assert!(receipt_gen.verify(&receipt, "shell", &args, "output"));
    }

    #[test]
    fn new_generates_random_key() {
        let receipt_gen1 = ReceiptGenerator::new();
        let receipt_gen2 = ReceiptGenerator::new();
        let args = test_args();
        let r1 = receipt_gen1.generate("shell", &args, "out", 100);
        let r2 = receipt_gen2.generate("shell", &args, "out", 100);
        // Different keys → different receipts (probabilistically)
        assert_ne!(r1, r2);
    }
}
