//! Runtime keybinding overrides.

use std::collections::HashMap;
use std::sync::RwLock;

use super::chord::Chord;

/// `tag -> (variant_name -> chords)`. Sparse at every level: an absent
/// tag means "use compile-time defaults for that whole enum"; an absent
/// variant within a present tag keeps that variant's default chords.
pub type OverrideTable = HashMap<String, HashMap<String, Vec<Chord>>>;

static ACTIVE: RwLock<Option<OverrideTable>> = RwLock::new(None);

/// Look up the override map for one enum's `tag`. Returns a clone so the
/// lock is not held across the rebuild in `resolved_bindings`.
pub fn lookup(tag: &str) -> Option<HashMap<String, Vec<Chord>>> {
    ACTIVE
        .read()
        .ok()
        .and_then(|guard| guard.as_ref().and_then(|t| t.get(tag).cloned()))
}

/// Replace the entire active override table (preset pick / config load).
pub fn set_active(table: OverrideTable) {
    if let Ok(mut guard) = ACTIVE.write() {
        *guard = Some(table);
    }
}

/// Insert or replace a single `tag.variant` row, leaving the rest of the
/// active table intact (capture-modal save). Creates the table / tag
/// bucket on demand.
pub fn set_row(tag: &str, variant: &str, chords: Vec<Chord>) {
    if let Ok(mut guard) = ACTIVE.write() {
        let table = guard.get_or_insert_with(HashMap::new);
        table
            .entry(tag.to_string())
            .or_default()
            .insert(variant.to_string(), chords);
    }
}

/// Drop the active table so a test starts from compile-time defaults.
/// Hold [`TEST_GUARD`] across the call.
#[cfg(test)]
pub(crate) fn reset() {
    if let Ok(mut guard) = ACTIVE.write() {
        *guard = None;
    }
}

/// Serializes every test that installs an override, wherever it lives.
/// `ACTIVE` is process-wide, so two such tests running in parallel would
/// clobber each other's state.
#[cfg(test)]
pub(crate) static TEST_GUARD: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyCode;

    #[test]
    fn set_and_lookup_round_trips() {
        let _g = TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        let mut table = OverrideTable::new();
        let mut dash = HashMap::new();
        dash.insert("refresh".to_string(), vec![Chord::key(KeyCode::F(5))]);
        table.insert("dashboard".to_string(), dash);
        set_active(table);

        let got = lookup("dashboard").expect("tag present");
        assert_eq!(
            got.get("refresh").unwrap(),
            &vec![Chord::key(KeyCode::F(5))]
        );
        assert!(lookup("chat").is_none());
        reset();
        assert!(lookup("dashboard").is_none());
    }

    #[test]
    fn set_row_creates_on_demand() {
        let _g = TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        reset();
        set_row("logs", "toggle_follow", vec![Chord::char('F')]);
        let got = lookup("logs").expect("tag created");
        assert_eq!(got.get("toggle_follow").unwrap(), &vec![Chord::char('F')]);
        reset();
    }
}
