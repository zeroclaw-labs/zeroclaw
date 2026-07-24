//! Anyhow error-chain rendering helper.

/// Render an `anyhow::Error` with its full `.context()` chain.
/// Uses the alternate Display formatter (`{:#}`) which walks the chain.
/// Plain `{}` only prints the leaf message, losing every context layer.
#[must_use]
pub fn display_chain(err: &anyhow::Error) -> String {
    format!("{err:#}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Context;

    #[test]
    fn display_chain_walks_context() {
        let leaf: anyhow::Result<()> = Err(anyhow::Error::msg("connection refused"));
        let err = leaf
            .context("failed to dial provider")
            .context("processing turn")
            .unwrap_err();
        let s = display_chain(&err);
        assert!(s.contains("processing turn"));
        assert!(s.contains("failed to dial provider"));
        assert!(s.contains("connection refused"));
    }
}
