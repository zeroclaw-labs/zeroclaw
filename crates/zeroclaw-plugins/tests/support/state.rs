use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use zeroclaw_plugins::instance::PluginInstanceScope;
use zeroclaw_plugins::services::{
    PluginStateBackend, PluginStateError, PluginStateKey, PluginStateService, PluginStateValue,
};

type StateRows = HashMap<(String, String), (u64, Vec<u8>)>;

#[derive(Clone, Default)]
struct TestStateBackend {
    rows: Arc<Mutex<StateRows>>,
}

#[async_trait]
impl PluginStateBackend for TestStateBackend {
    async fn get(
        &self,
        scope: &PluginInstanceScope,
        key: &PluginStateKey,
    ) -> Result<Option<PluginStateValue>, PluginStateError> {
        let owner = scope
            .id()
            .config_entry_key()
            .map_err(|_| PluginStateError::Unavailable)?;
        let rows = self
            .rows
            .lock()
            .map_err(|_| PluginStateError::Unavailable)?;
        Ok(rows
            .get(&(owner, key.as_str().to_string()))
            .map(|(revision, value)| PluginStateValue::new(*revision, value.clone())))
    }

    async fn put(
        &self,
        scope: &PluginInstanceScope,
        key: &PluginStateKey,
        value: &[u8],
        expected_revision: Option<u64>,
    ) -> Result<u64, PluginStateError> {
        let owner = scope
            .id()
            .config_entry_key()
            .map_err(|_| PluginStateError::Unavailable)?;
        let row_key = (owner, key.as_str().to_string());
        let mut rows = self
            .rows
            .lock()
            .map_err(|_| PluginStateError::Unavailable)?;
        let current = rows.get(&row_key).map(|(revision, _)| *revision);
        if current != expected_revision {
            return Err(PluginStateError::Conflict);
        }
        let revision = current
            .unwrap_or(0)
            .checked_add(1)
            .ok_or(PluginStateError::Unavailable)?;
        rows.insert(row_key, (revision, value.to_vec()));
        Ok(revision)
    }

    async fn delete(
        &self,
        scope: &PluginInstanceScope,
        key: &PluginStateKey,
        expected_revision: u64,
    ) -> Result<(), PluginStateError> {
        let owner = scope
            .id()
            .config_entry_key()
            .map_err(|_| PluginStateError::Unavailable)?;
        let row_key = (owner, key.as_str().to_string());
        let mut rows = self
            .rows
            .lock()
            .map_err(|_| PluginStateError::Unavailable)?;
        let current = rows
            .get(&row_key)
            .map(|(revision, _)| *revision)
            .ok_or(PluginStateError::NotFound)?;
        if current != expected_revision {
            return Err(PluginStateError::Conflict);
        }
        rows.remove(&row_key);
        Ok(())
    }
}

pub fn state_service() -> PluginStateService {
    PluginStateService::new(TestStateBackend::default())
}
