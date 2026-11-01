//! Standard session repository used by protocol entrances. Event storage and
//! projections are shared; protocol namespaces select families, never a database.
use crate::session_events::{catalog::SessionCatalog, family::EventStorePool};
use anyhow::{Context, Result};
use chrono::Utc;
use std::path::{Path, PathBuf};
#[cfg(any(test, feature = "test-util"))]
use zeroclaw_api::model_provider::ConversationMessage;
use zeroclaw_api::plan::PlanEntry;
use zeroclaw_log::{Action, EventOutcome};
mod models;
mod view;
pub use models::*;

macro_rules! legacy_fixture {
    ($repository:expr, $method:ident($($argument:expr),* $(,)?)) => {
        #[cfg(any(test, feature = "test-util"))]
        if let Some(legacy) = $repository.fixture()? {
            return legacy.$method($($argument),*);
        }
    };
}

#[cfg(any(test, feature = "test-util"))]
mod legacy_fixture;

pub struct SessionRepository {
    data_dir: PathBuf,
    namespace: String,
    stores: EventStorePool,
    #[cfg(any(test, feature = "test-util"))]
    legacy_fixture: Option<crate::session_events::upgrade::legacy_acp::LegacyAcpStore>,
}

impl SessionRepository {
    /// Binds a family namespace to the installation's configured native format.
    /// Migration remains owned by installation startup, never this handle.
    pub fn for_namespace(data_dir: &Path, namespace: &str) -> Result<Self> {
        anyhow::ensure!(
            !namespace.contains('/') && !namespace.contains('\\'),
            "invalid session namespace"
        );
        Ok(Self {
            data_dir: data_dir.into(),
            namespace: namespace.into(),
            stores: Default::default(),
            #[cfg(any(test, feature = "test-util"))]
            legacy_fixture: None,
        })
    }

    /// Query and command capabilities share the standard installation catalog.
    /// No protocol-specific connection or metadata state is retained.
    pub fn catalog(&self) -> Result<SessionCatalog> {
        SessionCatalog::open(&self.data_dir, &self.stores)?
            .context("standard session storage has not been initialized")
    }

    #[cfg(any(test, feature = "test-util"))]
    pub fn legacy_fixture(data_dir: &Path) -> Result<Self> {
        let mut repository = Self::for_namespace(data_dir, "acp_")?;
        if !data_dir.join("sessions/session-format.json").try_exists()? {
            repository.legacy_fixture =
                Some(crate::session_events::upgrade::legacy_acp::LegacyAcpStore::new(data_dir)?);
        }
        Ok(repository)
    }

    #[cfg(any(test, feature = "test-util"))]
    fn fixture(
        &self,
    ) -> Result<Option<&crate::session_events::upgrade::legacy_acp::LegacyAcpStore>> {
        Ok(
            if self
                .data_dir
                .join("sessions/session-format.json")
                .try_exists()?
            {
                None
            } else {
                self.legacy_fixture.as_ref()
            },
        )
    }

    fn view(&self) -> Result<view::RepositoryView<'_>> {
        let catalog = self.catalog()?;
        Ok(view::RepositoryView(catalog, &self.namespace))
    }
    pub fn create_session(
        &self,
        session_uuid: &str,
        agent_alias: &str,
        workspace_dir: &str,
        principal_id: Option<&str>,
    ) -> Result<i64> {
        legacy_fixture!(
            self,
            create_session(session_uuid, agent_alias, workspace_dir, principal_id)
        );

        self.create_session_with_interaction_surface(
            session_uuid,
            agent_alias,
            workspace_dir,
            None,
            principal_id,
        )
    }

    pub fn create_session_with_interaction_surface(
        &self,
        session_uuid: &str,
        agent_alias: &str,
        workspace_dir: &str,
        interaction_surface: Option<&str>,
        principal_id: Option<&str>,
    ) -> Result<i64> {
        legacy_fixture!(
            self,
            create_session_with_interaction_surface(
                session_uuid,
                agent_alias,
                workspace_dir,
                interaction_surface,
                principal_id,
            )
        );

        let native = self.view()?;

        native.create(
            session_uuid,
            agent_alias,
            vec![
                ("workspace_dir", serde_json::json!(workspace_dir)),
                (
                    "interaction_surface",
                    serde_json::json!(interaction_surface),
                ),
                ("principal_id", serde_json::json!(principal_id)),
            ],
        )
    }

    pub fn delete_session_owned(
        &self,
        session_uuid: &str,
        owner_principal_id: &str,
    ) -> Result<bool> {
        legacy_fixture!(self, delete_session_owned(session_uuid, owner_principal_id));

        let native = self.view()?;

        native.delete(session_uuid, Some(owner_principal_id))
    }

    // Distinguish a missing session from an existing unscoped session.
    #[allow(clippy::option_option)]
    pub fn session_principal(&self, session_uuid: &str) -> Result<Option<Option<String>>> {
        legacy_fixture!(self, session_principal(session_uuid));

        let native = self.view()?;

        native
            .record(session_uuid)?
            .map(|record| view::RepositoryView::value(&record, "principal_id", None))
            .transpose()
    }

    pub fn session_owner_and_surface(
        &self,
        session_uuid: &str,
    ) -> Result<Option<(Option<String>, Option<String>)>> {
        legacy_fixture!(self, session_owner_and_surface(session_uuid));

        let native = self.view()?;

        native
            .record(session_uuid)?
            .map(|record| {
                Ok((
                    view::RepositoryView::value(&record, "principal_id", None)?,
                    view::RepositoryView::value(&record, "interaction_surface", None)?,
                ))
            })
            .transpose()
    }

    pub fn bind_interaction_surface_if_unset(
        &self,
        session_uuid: &str,
        interaction_surface: &str,
    ) -> Result<String> {
        legacy_fixture!(
            self,
            bind_interaction_surface_if_unset(session_uuid, interaction_surface)
        );

        let native = self.view()?;

        let record = native.required(session_uuid)?;
        let surface: Option<String> =
            view::RepositoryView::value(&record, "interaction_surface", None)?;
        if let Some(surface) = surface {
            return Ok(surface);
        }
        native.0.commands().change(
            &record,
            vec![(
                "interaction_surface",
                serde_json::json!(interaction_surface),
            )],
        )?;
        Ok(interaction_surface.into())
    }

    pub fn load_session(&self, session_uuid: &str) -> Result<Option<SessionTranscript>> {
        legacy_fixture!(self, load_session(session_uuid));

        let native = self.view()?;

        native
            .detail(session_uuid)?
            .map(|record| native.data(session_uuid, &record))
            .transpose()
    }

    pub fn load_session_for_agent(
        &self,
        session_uuid: &str,
        agent_alias: &str,
    ) -> Result<Option<SessionTranscript>> {
        legacy_fixture!(self, load_session_for_agent(session_uuid, agent_alias));

        let native = self.view()?;

        native
            .detail(session_uuid)?
            .filter(|record| record.owning_agent.as_deref() == Some(agent_alias))
            .map(|record| native.data(session_uuid, &record))
            .transpose()
    }

    pub fn classify_session_for_agent(
        &self,
        session_uuid: &str,
        agent_alias: &str,
    ) -> Result<SessionOwnership> {
        legacy_fixture!(self, classify_session_for_agent(session_uuid, agent_alias));

        let native = self.view()?;

        Ok(match native.record(session_uuid)? {
            Some(record) if record.owning_agent.as_deref() == Some(agent_alias) => {
                SessionOwnership::Owned
            }
            Some(_) => SessionOwnership::Foreign,
            None => SessionOwnership::Missing,
        })
    }

    pub fn is_live_session_for_agent(&self, session_uuid: &str, agent_alias: &str) -> Result<bool> {
        legacy_fixture!(self, is_live_session_for_agent(session_uuid, agent_alias));

        let native = self.view()?;

        Ok(native.record(session_uuid)?.is_some_and(|record| {
            record.owning_agent.as_deref() == Some(agent_alias) && !record.closed
        }))
    }

    pub fn list_session_ids(&self) -> Result<Vec<String>> {
        legacy_fixture!(self, list_session_ids());

        let native = self.view()?;

        Ok(native.summaries()?.into_iter().map(|(id, _)| id).collect())
    }

    pub fn load_session_for_restore(&self, session_uuid: &str) -> Result<SessionRestore> {
        legacy_fixture!(self, load_session_for_restore(session_uuid));

        let native = self.view()?;

        Ok(match native.detail(session_uuid)? {
            None => SessionRestore::Missing,
            Some(record) if record.closed => SessionRestore::Killed,
            Some(record) => {
                SessionRestore::Restorable(Box::new(native.data(session_uuid, &record)?))
            }
        })
    }

    pub fn list_sessions(&self) -> Result<Vec<SessionListEntry>> {
        legacy_fixture!(self, list_sessions());

        let native = self.view()?;

        native
            .summaries()?
            .into_iter()
            .filter(|(_, record)| !record.closed)
            .map(|(id, record)| view::RepositoryView::summary(id, record))
            .collect()
    }

    pub fn list_live_sessions_by_agent(&self, agent_alias: &str) -> Result<Vec<SessionListEntry>> {
        legacy_fixture!(self, list_live_sessions_by_agent(agent_alias));

        let native = self.view()?;

        native
            .summaries()?
            .into_iter()
            .filter(|(_, record)| {
                !record.closed && record.owning_agent.as_deref() == Some(agent_alias)
            })
            .map(|(id, record)| view::RepositoryView::summary(id, record))
            .collect()
    }

    pub fn contains_session(&self, session_uuid: &str) -> Result<bool> {
        legacy_fixture!(self, contains_session(session_uuid));

        let native = self.view()?;

        Ok(native.record(session_uuid)?.is_some())
    }

    pub fn set_token_count(&self, session_uuid: &str, token_count: u64) -> Result<()> {
        legacy_fixture!(self, set_token_count(session_uuid, token_count));

        let native = self.view()?;

        native.change(
            session_uuid,
            vec![("token_count", serde_json::json!(token_count))],
        )
    }

    pub fn clear_token_count(&self, session_uuid: &str) -> Result<()> {
        legacy_fixture!(self, clear_token_count(session_uuid));

        let native = self.view()?;

        native.change(session_uuid, vec![("token_count", serde_json::json!(0))])
    }

    pub fn persist_usage_snapshot(
        &self,
        session_uuid: &str,
        input_tokens: Option<u64>,
        accepted: bool,
    ) -> Result<()> {
        legacy_fixture!(
            self,
            persist_usage_snapshot(session_uuid, input_tokens, accepted)
        );

        if !accepted {
            return Ok(());
        }
        match input_tokens {
            Some(v) => self.set_token_count(session_uuid, v),
            None => self.clear_token_count(session_uuid),
        }
    }

    pub fn set_plan(&self, session_uuid: &str, entries: &[PlanEntry]) -> Result<()> {
        legacy_fixture!(self, set_plan(session_uuid, entries));

        let native = self.view()?;

        native.change(session_uuid, vec![("plan", serde_json::json!(entries))])
    }

    pub fn get_plan(&self, session_uuid: &str) -> Result<Vec<PlanEntry>> {
        legacy_fixture!(self, get_plan(session_uuid));

        let native = self.view()?;

        native
            .record(session_uuid)?
            .map(|record| view::RepositoryView::value(&record, "plan", vec![]))
            .transpose()
            .map(Option::unwrap_or_default)
    }

    pub fn append_event(
        &self,
        session_uuid: &str,
        action: Action,
        outcome: EventOutcome,
        payload: Option<&str>,
    ) -> Result<()> {
        legacy_fixture!(self, append_event(session_uuid, action, outcome, payload));

        let native = self.view()?;

        native.change(session_uuid, vec![("protocol_observation", serde_json::json!({"action":action.as_str(),"outcome":outcome.as_str(),"payload":payload}))])
    }

    pub fn delete_session(&self, session_uuid: &str) -> Result<bool> {
        legacy_fixture!(self, delete_session(session_uuid));

        let native = self.view()?;

        native.delete(session_uuid, None)
    }

    pub fn count_live_sessions_by_agent(&self, agent_alias: &str) -> Result<usize> {
        legacy_fixture!(self, count_live_sessions_by_agent(agent_alias));

        let native = self.view()?;

        Ok(native
            .summaries()?
            .iter()
            .filter(|(_, record)| {
                !record.closed && record.owning_agent.as_deref() == Some(agent_alias)
            })
            .count())
    }

    pub fn list_sessions_by_agent(&self, agent_alias: &str) -> Result<Vec<SessionListEntry>> {
        legacy_fixture!(self, list_sessions_by_agent(agent_alias));

        let native = self.view()?;

        native
            .summaries()?
            .into_iter()
            .filter(|(_, record)| record.owning_agent.as_deref() == Some(agent_alias))
            .map(|(id, record)| view::RepositoryView::summary(id, record))
            .collect()
    }

    pub fn projected_message_count(&self, session_uuid: &str) -> Result<Option<usize>> {
        legacy_fixture!(self, projected_message_count(session_uuid));

        let native = self.view()?;

        Ok(native
            .record(session_uuid)?
            .map(|record| record.message_count))
    }

    pub fn delete_sessions_by_agent(&self, agent_alias: &str) -> Result<usize> {
        legacy_fixture!(self, delete_sessions_by_agent(agent_alias));

        let native = self.view()?;

        let records = native.summaries()?;
        let mut changed = 0;
        for (_, record) in records
            .into_iter()
            .filter(|(_, record)| record.owning_agent.as_deref() == Some(agent_alias))
        {
            native.0.commands().close(&record, "session_deleted")?;
            changed += 1;
        }
        Ok(changed)
    }

    pub fn rename_sessions_by_agent(&self, from: &str, to: &str) -> Result<usize> {
        legacy_fixture!(self, rename_sessions_by_agent(from, to));

        let native = self.view()?;

        let records = native.summaries()?;
        let mut changed = 0;
        for (_, record) in records
            .into_iter()
            .filter(|(_, record)| record.owning_agent.as_deref() == Some(from))
        {
            native
                .0
                .commands()
                .change(&record, vec![("owning_agent", serde_json::json!(to))])?;
            changed += 1;
        }
        Ok(changed)
    }

    pub fn mark_session_killed_atomic(&self, session_uuid: &str) -> Result<SessionKillTransition> {
        legacy_fixture!(self, mark_session_killed_atomic(session_uuid));

        self.mark_session_killed_atomic_for_owner(session_uuid, None)
    }

    pub fn mark_session_killed_atomic_for_owner(
        &self,
        session_uuid: &str,
        owner_principal_id: Option<&str>,
    ) -> Result<SessionKillTransition> {
        legacy_fixture!(
            self,
            mark_session_killed_atomic_for_owner(session_uuid, owner_principal_id)
        );

        let native = self.view()?;

        native.kill(session_uuid, owner_principal_id)
    }

    pub fn mark_session_killed(&self, session_uuid: &str) -> Result<bool> {
        legacy_fixture!(self, mark_session_killed(session_uuid));

        Ok(matches!(
            self.mark_session_killed_atomic(session_uuid)?,
            SessionKillTransition::Marked | SessionKillTransition::AlreadyKilled
        ))
    }

    pub fn is_session_killed(&self, session_uuid: &str) -> Result<bool> {
        legacy_fixture!(self, is_session_killed(session_uuid));

        let native = self.view()?;

        Ok(native
            .record(session_uuid)?
            .is_some_and(|record| record.closed))
    }

    pub fn touch_session(&self, session_uuid: &str) -> Result<()> {
        legacy_fixture!(self, touch_session(session_uuid));

        let native = self.view()?;

        if let Some(record) = native.record(session_uuid)? {
            native.0.commands().change(
                &record,
                vec![(
                    "last_activity",
                    serde_json::json!(Utc::now().timestamp_millis()),
                )],
            )?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session_events::{installation::NativeBackend, upgrade::upgrade_installation};

    #[test]
    fn protocol_namespaces_share_the_standard_session_model_and_never_create_acp_database() {
        for backend in [NativeBackend::Sqlite, NativeBackend::Jsonl] {
            let directory = tempfile::tempdir().expect("directory");
            upgrade_installation(directory.path(), backend, |_| "unattributed".into())
                .expect("ready");
            let acp = SessionRepository::for_namespace(directory.path(), "acp_").expect("acp");
            let rpc = SessionRepository::for_namespace(directory.path(), "rpc_").expect("rpc");
            acp.create_session("same", "agent", "/workspace", Some("owner"))
                .expect("create ACP");
            rpc.create_session("same", "agent", "/workspace", Some("owner"))
                .expect("create RPC");
            assert_eq!(acp.list_sessions().expect("ACP list").len(), 1);
            assert_eq!(rpc.list_sessions().expect("RPC list").len(), 1);
            let catalog = acp.catalog().expect("catalog");
            let families = catalog.query().families().expect("families");
            let acp: zeroclaw_api::session_events::catalog::Session = catalog
                .query()
                .summary(&families["acp_same"])
                .expect("summary")
                .expect("ACP session");
            let rpc: zeroclaw_api::session_events::catalog::Session = catalog
                .query()
                .summary(&families["rpc_same"])
                .expect("summary")
                .expect("RPC session");
            assert_eq!(acp.owning_agent, rpc.owning_agent);
            assert_ne!(acp.stream(), rpc.stream());
            assert!(!directory.path().join("sessions/acp-sessions.db").exists());
        }
    }
}
