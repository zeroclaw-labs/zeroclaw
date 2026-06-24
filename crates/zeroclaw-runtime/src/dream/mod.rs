pub mod engine;
pub mod pending;
pub mod report;

#[cfg(test)]
mod tests {
    use crate::dream::engine::DreamEngine;
    use zeroclaw_config::schema::DreamModeConfig;

    #[test]
    fn dream_engine_is_constructible_via_module_export() {
        let temp = tempfile::tempdir().unwrap();
        let engine = DreamEngine::new(DreamModeConfig::default(), temp.path().to_path_buf());

        let _ = engine;
    }
}
