use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenContext {
    pub used: usize,
    pub budget: usize,
    pub window_size: usize,
    pub source_id: String,
    pub timestamp: u64,
    pub confidence: f64,
}

impl TokenContext {
    pub fn new(budget: usize, window_size: usize) -> Self {
        Self {
            used: 0,
            budget,
            window_size,
            source_id: String::new(),
            timestamp: 0,
            confidence: 1.0,
        }
    }

    pub fn consume(&mut self, tokens: usize) {
        self.used = self.used.saturating_add(tokens).min(self.budget);
    }

    pub fn remaining(&self) -> usize {
        self.budget.saturating_sub(self.used)
    }

    pub fn utilization(&self) -> f64 {
        if self.budget == 0 {
            return 0.0;
        }
        self.used as f64 / self.budget as f64
    }

    pub fn reset(&mut self) {
        self.used = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn budget_tracking() {
        let mut ctx = TokenContext::new(1000, 4096);
        ctx.consume(300);
        assert_eq!(ctx.remaining(), 700);
        assert_eq!(ctx.used, 300);
        ctx.consume(800);
        assert_eq!(ctx.used, 1000);
        assert_eq!(ctx.remaining(), 0);
    }

    #[test]
    fn utilization_percentage() {
        let mut ctx = TokenContext::new(100, 4096);
        ctx.consume(50);
        assert!((ctx.utilization() - 0.5).abs() < f64::EPSILON);
    }

    #[test]
    fn reset_clears_usage() {
        let mut ctx = TokenContext::new(1000, 4096);
        ctx.consume(500);
        ctx.reset();
        assert_eq!(ctx.used, 0);
        assert_eq!(ctx.remaining(), 1000);
    }
}
