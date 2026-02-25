use super::*;

#[test]
fn narrative_dedup_merges_similar_episodes() {
    let mut store = NarrativeStore::new(100);
    let ep1 = Episode {
        summary: "learned rust".into(),
        timestamp: 1000,
        significance: 0.8,
        verified: true,
        tags: vec!["rust".into(), "learning".into()],
        emotional_tag: None,
        valence_score: None,
    };
    let ep2 = Episode {
        summary: "learned rust".into(),
        timestamp: 2000,
        significance: 0.6,
        verified: false,
        tags: vec!["rust".into(), "learning".into(), "advanced".into()],
        emotional_tag: None,
        valence_score: None,
    };
    store.append(ep1);
    store.append(ep2);
    assert_eq!(store.episodes().len(), 1);
    assert!(store.episodes()[0].tags.contains(&"advanced".into()));
    assert!(store.episodes()[0].verified);
}

#[test]
fn preference_drift_limit_enforced() {
    let limits = DriftLimits {
        max_daily: 0.10,
        max_session: 0.05,
    };
    let mut model = PreferenceModel::new(limits);

    for i in 0..3 {
        let _ = model.update(
            &format!("pref_{}", i),
            "value",
            0.5,
            PreferenceCategory::Technical,
        );
    }

    let result = model.update("overflow_pref", "value", 0.5, PreferenceCategory::Technical);
    let is_rejected = result.is_err()
        || model
            .get("overflow_pref")
            .map_or(true, |p| p.value != "value");

    assert!(
        is_rejected || model.session_drift() <= 0.05 + 0.001,
        "drift limit must be enforced: session_drift={}",
        model.session_drift()
    );
}

#[test]
fn guard_detects_whiplash() {
    let mut guard = ContinuityGuard::new(DriftLimits::default());
    assert!(!guard.is_conservative());

    guard.record_drift(0.015);
    guard.record_drift(0.012);
    guard.record_drift(0.010);

    assert!(
        guard.is_conservative(),
        "guard should enter conservative mode after drift samples exceeding threshold"
    );
}

#[test]
fn idcore_immutability_enforced() {
    let guard = ContinuityGuard::new(DriftLimits::default());
    let original = IdentityCore {
        name: "zeroclaw".into(),
        constitution_hash: "abc123".into(),
        creation_epoch: 1000,
        immutable_values: vec!["honesty".into()],
    };

    let changed_name = IdentityCore {
        name: "altered".into(),
        ..original.clone()
    };
    assert!(guard
        .validate_core_immutability(&original, &changed_name)
        .is_err());

    let changed_hash = IdentityCore {
        constitution_hash: "xyz789".into(),
        ..original.clone()
    };
    assert!(guard
        .validate_core_immutability(&original, &changed_hash)
        .is_err());

    let changed_epoch = IdentityCore {
        creation_epoch: 9999,
        ..original.clone()
    };
    assert!(guard
        .validate_core_immutability(&original, &changed_epoch)
        .is_err());

    assert!(guard
        .validate_core_immutability(&original, &original)
        .is_ok());
}

#[test]
fn preference_update_no_change_is_noop() {
    let mut model = PreferenceModel::new(DriftLimits::default());
    model
        .update("theme", "dark", 0.9, PreferenceCategory::Aesthetic)
        .unwrap();
    let drift_after_first = model.session_drift();

    model
        .update("theme", "dark", 0.9, PreferenceCategory::Aesthetic)
        .unwrap();
    let drift_after_second = model.session_drift();

    assert!(
        (drift_after_first - drift_after_second).abs() < f64::EPSILON,
        "updating same value should not accumulate drift"
    );
}

#[test]
fn verify_recent_marks_unverified_episode() {
    let mut store = NarrativeStore::new(100);
    store.append(Episode {
        summary: "did something".into(),
        timestamp: 1000,
        significance: 0.5,
        verified: false,
        tags: vec!["action".into()],
        emotional_tag: None,
        valence_score: None,
    });
    assert!(!store.episodes()[0].verified);
    store.verify_recent(&["shell".into()]);
    assert!(store.episodes()[0].verified);
    assert!(store.episodes()[0].tags.contains(&"tool:shell".into()));
}

#[test]
fn verify_recent_no_tools_is_noop() {
    let mut store = NarrativeStore::new(100);
    store.append(Episode {
        summary: "did something".into(),
        timestamp: 1000,
        significance: 0.5,
        verified: false,
        tags: vec![],
        emotional_tag: None,
        valence_score: None,
    });
    store.verify_recent(&[]);
    assert!(!store.episodes()[0].verified);
}

#[test]
fn extract_and_fulfill_commitments() {
    let response = "I will fix the bug.\nDone fixing.";
    let mut commits = commitments::extract_commitments(response);
    assert_eq!(commits.len(), 1);
    assert!(!commits[0].fulfilled);
    commitments::check_fulfillment(&mut commits, &["fix".into()]);
    assert!(commits[0].fulfilled);
}

#[test]
fn identity_from_soul_populates_core() {
    let soul = crate::soul::model::SoulModel {
        name: "test_agent".into(),
        values: vec!["integrity".into()],
        ..Default::default()
    };
    let constitution = crate::soul::constitution::Constitution::default_laws();
    let id = identity::identity_from_soul(&soul, &constitution, Vec::new(), Vec::new(), 3);
    assert_eq!(id.core.name, "test_agent");
    assert_eq!(id.session_count, 3);
    assert!(!id.core.constitution_hash.is_empty());
}

#[test]
fn tool_extraction_records_affinity() {
    let mut model = PreferenceModel::new(DriftLimits::default());
    let _ = extraction::extract_tool_preference(&mut model, "file_read", true);
    assert!(model.get("tool_affinity:file_read").is_some());
    let _ = extraction::extract_tool_preference(&mut model, "shell", false);
    assert!(model.get("tool_affinity:shell").is_none());
}

#[test]
fn channel_extraction_records_preference() {
    let mut model = PreferenceModel::new(DriftLimits::default());
    extraction::extract_channel_preference(&mut model, "discord").unwrap();
    let pref = model.get("preferred_channel").unwrap();
    assert_eq!(pref.value, "discord");
}

#[test]
fn episode_emotional_tag_survives_serde() {
    let ep = Episode {
        summary: "breakthrough moment".into(),
        timestamp: 5000,
        significance: 0.9,
        verified: true,
        tags: vec!["milestone".into()],
        emotional_tag: Some("breakthrough".into()),
        valence_score: Some(0.85),
    };
    let json = serde_json::to_string(&ep).unwrap();
    let restored: Episode = serde_json::from_str(&json).unwrap();
    assert_eq!(restored.emotional_tag.as_deref(), Some("breakthrough"));
    assert!((restored.valence_score.unwrap() - 0.85).abs() < f64::EPSILON);
}

#[test]
fn preference_evolution_tracked_on_update() {
    let limits = DriftLimits {
        max_daily: 1.0,
        max_session: 1.0,
    };
    let mut model = PreferenceModel::new(limits);
    model
        .update("theme", "dark", 0.9, PreferenceCategory::Aesthetic)
        .unwrap();
    model
        .update("theme", "light", 0.8, PreferenceCategory::Aesthetic)
        .unwrap();
    let pref = model.get("theme").unwrap();
    assert_eq!(pref.evolution_history.len(), 1);
    assert_eq!(pref.evolution_history[0].value, "dark");
}

#[test]
fn preference_delta_logged_per_update() {
    let limits = DriftLimits {
        max_daily: 1.0,
        max_session: 1.0,
    };
    let mut model = PreferenceModel::new(limits);
    model
        .update("lang", "rust", 0.9, PreferenceCategory::Technical)
        .unwrap();
    model
        .update("lang", "python", 0.7, PreferenceCategory::Technical)
        .unwrap();
    let deltas = model.deltas();
    assert_eq!(deltas.len(), 1);
    assert_eq!(deltas[0].old_value, "rust");
    assert_eq!(deltas[0].new_value, "python");
}

#[test]
fn identity_checksum_deterministic() {
    let soul = crate::soul::model::SoulModel {
        name: "test_agent".into(),
        values: vec!["integrity".into()],
        ..Default::default()
    };
    let constitution = crate::soul::constitution::Constitution::default_laws();
    let id = identity::identity_from_soul(&soul, &constitution, Vec::new(), Vec::new(), 3);
    let hash1 = identity::compute_identity_checksum(&id);
    let hash2 = identity::compute_identity_checksum(&id);
    assert_eq!(hash1, hash2);
    assert_eq!(hash1.len(), 64);
}

#[test]
fn identity_checksum_changes_on_mutation() {
    let soul = crate::soul::model::SoulModel {
        name: "test_agent".into(),
        values: vec!["integrity".into()],
        ..Default::default()
    };
    let constitution = crate::soul::constitution::Constitution::default_laws();
    let id1 = identity::identity_from_soul(&soul, &constitution, Vec::new(), Vec::new(), 3);
    let id2 = identity::identity_from_soul(&soul, &constitution, Vec::new(), Vec::new(), 4);
    assert_ne!(
        identity::compute_identity_checksum(&id1),
        identity::compute_identity_checksum(&id2)
    );
}

#[test]
fn pruning_returns_removed_items() {
    let mut prefs = vec![
        Preference {
            key: "high".into(),
            value: "v".into(),
            confidence: 0.8,
            category: PreferenceCategory::Technical,
            last_updated: 1000,
            reasoning: None,
            evolution_history: vec![],
        },
        Preference {
            key: "low".into(),
            value: "v".into(),
            confidence: 0.05,
            category: PreferenceCategory::Technical,
            last_updated: 1000,
            reasoning: None,
            evolution_history: vec![],
        },
    ];
    let pruned = guard::prune_low_confidence(&mut prefs, 0.1);
    assert_eq!(prefs.len(), 1);
    assert_eq!(pruned.len(), 1);
    assert_eq!(pruned[0].key, "low");
}

#[test]
fn evolution_log_jsonl_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let deltas = vec![types::PreferenceDelta {
        key: "theme".into(),
        old_value: "dark".into(),
        new_value: "light".into(),
        old_confidence: 0.9,
        new_confidence: 0.8,
        drift_amount: 0.01,
        reasoning: None,
        timestamp: 12345,
    }];
    persistence::save_evolution_log(dir.path(), &deltas).unwrap();
    let loaded = persistence::load_evolution_log(dir.path()).unwrap();
    assert_eq!(loaded.len(), 1);
    assert_eq!(loaded[0].key, "theme");
}

#[test]
fn pruning_archive_jsonl_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let pruned = vec![Preference {
        key: "old".into(),
        value: "v".into(),
        confidence: 0.01,
        category: PreferenceCategory::Technical,
        last_updated: 1000,
        reasoning: None,
        evolution_history: vec![],
    }];
    persistence::save_pruning_archive(dir.path(), &pruned).unwrap();
    let path = dir.path().join("pruned.jsonl");
    let content = std::fs::read_to_string(&path).unwrap();
    assert!(!content.is_empty());
    let restored: Preference = serde_json::from_str(content.trim()).unwrap();
    assert_eq!(restored.key, "old");
}

#[test]
fn narrative_emotional_queries() {
    let mut store = NarrativeStore::new(100);
    store.append(Episode {
        summary: "happy event".into(),
        timestamp: 1000,
        significance: 0.8,
        verified: true,
        tags: vec!["joy".into()],
        emotional_tag: Some("positive".into()),
        valence_score: Some(0.9),
    });
    store.append(Episode {
        summary: "sad event".into(),
        timestamp: 2000,
        significance: 0.6,
        verified: true,
        tags: vec!["loss".into()],
        emotional_tag: Some("negative".into()),
        valence_score: Some(-0.7),
    });
    store.append(Episode {
        summary: "neutral event".into(),
        timestamp: 3000,
        significance: 0.3,
        verified: false,
        tags: vec!["routine".into()],
        emotional_tag: None,
        valence_score: None,
    });
    assert_eq!(store.episodes_by_emotion("positive").len(), 1);
    assert_eq!(store.episodes_by_emotion("negative").len(), 1);
    assert_eq!(store.high_valence_episodes(0.5).len(), 2);
}
