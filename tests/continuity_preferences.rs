use zeroclaw::continuity::{DriftLimits, PreferenceCategory, PreferenceModel};

#[test]
fn preference_update_and_retrieval() {
    let limits = DriftLimits {
        max_session: 5.0,
        max_daily: 10.0,
    };
    let mut model = PreferenceModel::new(limits);
    model
        .update("editor", "vim", 0.8, PreferenceCategory::Technical)
        .unwrap();

    let pref = model.get("editor").unwrap();
    assert_eq!(pref.value, "vim");
    assert!((pref.confidence - 0.8).abs() < f64::EPSILON);
}

#[test]
fn drift_limit_prevents_excessive_updates() {
    let limits = DriftLimits {
        max_session: 0.03,
        max_daily: 1.0,
    };
    let mut model = PreferenceModel::new(limits);
    model
        .update("a", "v1", 0.5, PreferenceCategory::Technical)
        .unwrap();

    let result = model.update("a", "v2", 0.9, PreferenceCategory::Technical);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.contains("drift limit"));
}

#[test]
fn prompt_context_filters_low_confidence() {
    let limits = DriftLimits::default();
    let mut model = PreferenceModel::new(limits);
    model
        .update("style", "concise", 0.9, PreferenceCategory::Communication)
        .unwrap();
    model
        .update("weak_pref", "maybe", 0.1, PreferenceCategory::Technical)
        .unwrap();

    let ctx = model.to_prompt_context();
    assert!(ctx.contains("style"));
    assert!(ctx.contains("concise"));
    assert!(!ctx.contains("weak_pref"));
}

#[test]
fn decay_removes_stale_preferences() {
    let limits = DriftLimits::default();
    let mut model = PreferenceModel::new(limits);
    model
        .update("fresh", "yes", 0.8, PreferenceCategory::Technical)
        .unwrap();

    let removed = model.decay_and_gc(0, 0.9);
    assert_eq!(removed, 1);
    assert!(model.get("fresh").is_none());
}

#[test]
fn deltas_track_value_changes() {
    let limits = DriftLimits {
        max_session: 5.0,
        max_daily: 10.0,
    };
    let mut model = PreferenceModel::new(limits);
    model
        .update("lang", "rust", 0.7, PreferenceCategory::Technical)
        .unwrap();
    model
        .update("lang", "python", 0.8, PreferenceCategory::Technical)
        .unwrap();

    let deltas = model.deltas();
    assert_eq!(deltas.len(), 1);
    assert_eq!(deltas[0].old_value, "rust");
    assert_eq!(deltas[0].new_value, "python");
}
