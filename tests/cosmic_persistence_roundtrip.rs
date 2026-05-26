use zeroclaw::cosmic::{
    gather_snapshot, BeliefSource, CausalGraph, ConsolidationEngine, CosmicMemoryGraph,
    CosmicPersistence, DriftDetector, EmotionalModulator, GlobalWorkspace, NormativeEngine,
    SelfModel, SensoryThalamus, WorldModel,
};

#[test]
fn persistence_roundtrip_keeps_beliefs() {
    let dir = tempfile::tempdir().expect("tempdir");
    let persistence = CosmicPersistence::new(dir.path());

    let mut sm = SelfModel::new(500);
    sm.update_belief("confidence", 0.75, 0.9_f32, BeliefSource::Observed);
    sm.update_belief("competence", 0.6, 0.8_f32, BeliefSource::Observed);

    let mut wm = WorldModel::new(500);
    wm.update_belief(
        "world:turn_success_rate",
        0.85,
        0.9_f32,
        BeliefSource::Observed,
    );

    let modulator = EmotionalModulator::new();
    let drift = DriftDetector::new(50, 0.1);
    let thalamus = SensoryThalamus::new(0.3, 100);
    let workspace = GlobalWorkspace::new(0.3, 5, 50);
    let consolidation = ConsolidationEngine::new(0.5);
    let normative = NormativeEngine::new(100, 100);
    let causal = CausalGraph::new(100);
    let graph = CosmicMemoryGraph::new(100);

    let snapshot = gather_snapshot(
        &modulator,
        &drift,
        &thalamus,
        &workspace,
        &sm,
        &wm,
        &consolidation,
        &normative,
        &causal,
        &graph,
    );
    persistence.save_all(&snapshot).expect("save_all");

    let loaded = persistence.load_all().expect("load_all");

    assert!(loaded.modules.contains_key("self_model"));
    assert!(loaded.modules.contains_key("world_model"));
    assert!(loaded.modules.contains_key("modulation"));
    assert!(loaded.modules.contains_key("consolidation"));
    assert!(loaded.modules.contains_key("normative"));
    assert!(loaded.modules.contains_key("causal"));

    let sm2 = SelfModel::restore(&loaded.modules["self_model"], 500).expect("restore self_model");
    let wm2 =
        WorldModel::restore(&loaded.modules["world_model"], 500).expect("restore world_model");

    let b = sm2.get_belief("confidence").expect("confidence belief");
    assert!((b.value - 0.75).abs() < 1e-6);

    let b2 = sm2.get_belief("competence").expect("competence belief");
    assert!((b2.value - 0.6).abs() < 1e-6);

    let wb = wm2
        .get_belief("world:turn_success_rate")
        .expect("world belief");
    assert!((wb.value - 0.85).abs() < 1e-6);
}

#[test]
fn atomic_write_leaves_no_tmp_files() {
    let dir = tempfile::tempdir().expect("tempdir");
    let persistence = CosmicPersistence::new(dir.path());

    let sm = SelfModel::new(500);
    let wm = WorldModel::new(500);
    let modulator = EmotionalModulator::new();
    let drift = DriftDetector::new(50, 0.1);
    let thalamus = SensoryThalamus::new(0.3, 100);
    let workspace = GlobalWorkspace::new(0.3, 5, 50);
    let consolidation = ConsolidationEngine::new(0.5);
    let normative = NormativeEngine::new(100, 100);
    let causal = CausalGraph::new(100);
    let graph = CosmicMemoryGraph::new(100);

    let snapshot = gather_snapshot(
        &modulator,
        &drift,
        &thalamus,
        &workspace,
        &sm,
        &wm,
        &consolidation,
        &normative,
        &causal,
        &graph,
    );
    persistence.save_all(&snapshot).expect("save_all");

    let entries: Vec<String> = std::fs::read_dir(dir.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();

    for name in &entries {
        assert!(!name.ends_with(".tmp"), "tmp file left behind: {name}");
    }
}

#[test]
fn emergency_save_preserves_mutated_state() {
    let dir = tempfile::tempdir().expect("tempdir");
    let persistence = CosmicPersistence::new(dir.path());

    let mut sm = SelfModel::new(500);
    sm.update_belief("confidence", 0.75, 0.9_f32, BeliefSource::Observed);

    let mut consolidation = ConsolidationEngine::new(0.65);
    consolidation.add_entry(zeroclaw::cosmic::MemoryEntry {
        id: "e1".into(),
        content: "test memory entry data".into(),
        category: "general".into(),
        importance: 0.8,
        access_count: 3,
        created_at: chrono::Utc::now(),
        last_accessed: chrono::Utc::now(),
    });

    let modulator = EmotionalModulator::new();
    let drift = DriftDetector::new(50, 0.1);
    let thalamus = SensoryThalamus::new(0.3, 100);
    let workspace = GlobalWorkspace::new(0.3, 5, 50);
    let wm = WorldModel::new(500);
    let normative = NormativeEngine::new(100, 100);
    let causal = CausalGraph::new(100);
    let graph = CosmicMemoryGraph::new(100);

    let snapshot = gather_snapshot(
        &modulator,
        &drift,
        &thalamus,
        &workspace,
        &sm,
        &wm,
        &consolidation,
        &normative,
        &causal,
        &graph,
    );
    persistence.save_all(&snapshot).expect("save_all");

    let loaded = persistence.load_all().expect("load_all");
    let sm2 = SelfModel::restore(&loaded.modules["self_model"], 500).expect("restore");
    assert!((sm2.get_belief("confidence").unwrap().value - 0.75).abs() < 1e-6);

    let ce2 = ConsolidationEngine::restore(&loaded.modules["consolidation"]).expect("restore");
    assert_eq!(ce2.entry_count(), 1);
}

#[test]
fn encrypted_persistence_roundtrip() {
    let dir = tempfile::tempdir().expect("tempdir");
    let key_dir = tempfile::tempdir().expect("key_dir");

    let store = zeroclaw::security::SecretStore::new(key_dir.path(), true);
    let persistence = CosmicPersistence::new(dir.path()).with_encryption(store);

    let mut sm = SelfModel::new(500);
    sm.update_belief("secret_val", 0.42, 0.9_f32, BeliefSource::Observed);

    let modulator = EmotionalModulator::new();
    let drift = DriftDetector::new(50, 0.1);
    let thalamus = SensoryThalamus::new(0.3, 100);
    let workspace = GlobalWorkspace::new(0.3, 5, 50);
    let wm = WorldModel::new(500);
    let consolidation = ConsolidationEngine::new(0.5);
    let normative = NormativeEngine::new(100, 100);
    let causal = CausalGraph::new(100);
    let graph = CosmicMemoryGraph::new(100);

    let snapshot = gather_snapshot(
        &modulator,
        &drift,
        &thalamus,
        &workspace,
        &sm,
        &wm,
        &consolidation,
        &normative,
        &causal,
        &graph,
    );
    persistence.save_all(&snapshot).expect("save_all");

    let loaded = persistence.load_all().expect("load_all");
    let sm2 = SelfModel::restore(&loaded.modules["self_model"], 500).expect("restore");
    assert!((sm2.get_belief("secret_val").unwrap().value - 0.42).abs() < 1e-6);
}
