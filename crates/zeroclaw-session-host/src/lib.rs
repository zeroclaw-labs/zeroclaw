//! Composition for native session sources. The installation manifest owns
//! the active format; events own session identity and derivation.
//! Live policies are supplied by the authenticated entrance, never cached here.
use std::{collections::HashMap, sync::Arc};
use zeroclaw_api::{
    principal::PrincipalId,
    session_events::{
        capture::{ContextSnapshot, SessionExecutionPort},
        *,
    },
};
use zeroclaw_config::live::LiveConfigHandle;
use zeroclaw_infra::session_events::{
    catalog::SessionCatalog,
    installation::{InstallationGuard, NativeBackend},
    open_event_store,
    scoped::EventStorePool,
};
use zeroclaw_kernel::session::{
    SessionCoordinator,
    engine::{EngineSession, EngineSessionAuthority, EngineSessionSource},
};
mod seed;

type SessionOwner = Arc<tokio::sync::Mutex<Option<Arc<SessionCoordinator>>>>;
type SessionOwners = HashMap<(std::path::PathBuf, String), SessionOwner>;

fn owner_is_idle(slot: &SessionOwner) -> bool {
    Arc::strong_count(slot) == 1
        && slot.try_lock().is_ok_and(|owner| {
            owner
                .as_ref()
                .is_none_or(|owner| Arc::strong_count(owner) == 1)
        })
}

/// Missing historical ownership is not permission to assign every session_id to
/// the first caller. Preserve uncertainty when configuration cannot resolve it.
pub fn legacy_session_owner(
    config: &zeroclaw_config::schema::Config,
    channel: Option<&str>,
) -> String {
    if let Some(alias) = channel.and_then(|channel| config.agent_for_channel(channel)) {
        return alias.into();
    }
    let mut enabled = config.agents.iter().filter(|(_, agent)| agent.enabled);
    match (enabled.next(), enabled.next()) {
        (Some((alias, _)), None) => alias.clone(),
        _ => "legacy-unattributed".into(),
    }
}

#[derive(Clone)]
pub struct NativeSessionHub {
    config: LiveConfigHandle,
    // Creates process-local execution ownership, not a transcript or policy
    // cache. The hub retains execution owners for its host generation;
    // escaped observations retain ownership after the host begins draining.
    owners: Arc<tokio::sync::Mutex<SessionOwners>>,
    stores: Arc<EventStorePool>,
}
impl NativeSessionHub {
    pub fn new(config: LiveConfigHandle) -> Self {
        Self {
            config,
            owners: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            stores: Arc::new(EventStorePool::default()),
        }
    }
    async fn prepare(&self) -> anyhow::Result<(std::path::PathBuf, NativeBackend)> {
        let (directory, backend) = {
            let config = self.config.read();
            (
                config.data_dir.clone(),
                NativeBackend::parse(&config.channels.session_backend)?,
            )
        };
        let check_directory = directory.clone();
        let ready = tokio::task::spawn_blocking(move || -> anyhow::Result<bool> {
            let guard = InstallationGuard::acquire(&check_directory.join("sessions"))?;
            Ok(guard.active().is_ok_and(|(active, _)| active == backend))
        })
        .await??;
        if !ready {
            let mut owners = self.owners.lock().await;
            anyhow::ensure!(
                owners
                    .iter()
                    .filter(|((path, _), _)| *path == directory)
                    .all(|(_, owner)| owner_is_idle(owner)),
                "session format upgrade requires drained execution and readers"
            );
            owners.retain(|(path, _), _| *path != directory);
            self.stores.release_installation(&directory);
            let upgrade_directory = directory.clone();
            let config = self.config.clone();
            tokio::task::spawn_blocking(move || {
                zeroclaw_infra::session_events::upgrade::upgrade_installation(
                    &upgrade_directory,
                    backend,
                    |channel| legacy_session_owner(&config.read(), channel),
                )
            })
            .await??;
        }
        Ok((directory, backend))
    }

    async fn open_session(
        &self,
        session_id: String,
        creation_source: &str,
        alias: String,
        subject: EventActor,
        authority: Arc<dyn EngineSessionAuthority>,
        initial: ContextSnapshot,
    ) -> anyhow::Result<Arc<dyn SessionExecutionPort>> {
        let (directory, backend) = self.prepare().await?;
        let owner_key = (directory.clone(), session_id.clone());
        let slot = self
            .owners
            .lock()
            .await
            .entry(owner_key)
            .or_default()
            .clone();
        let mut session_owner = slot.lock().await;
        let stream = zeroclaw_api::session_events::SessionId::parse(session_id.to_owned())?;
        let branch = BranchId::parse(format!("branch-{stream}"))?;
        // The identity is bound before physical initialization and every later
        // read/command rechecks the host's current authority.
        zeroclaw_kernel::session::query::SessionAccessPolicy::authorize_actor(
            authority.as_ref(),
            &subject,
            &stream,
            zeroclaw_kernel::session::query::SessionAccess::Write,
        )?;
        if let Some(owner) = session_owner.as_ref().cloned() {
            let record = catalog_record(directory.clone(), stream.clone(), self.stores.clone())
                .await?
                .ok_or_else(|| anyhow::Error::msg("native source missing"))?;
            anyhow::ensure!(
                record.owning_agent.as_deref() == Some(alias.as_str())
                    && !record.closed
                    && !record.deleted,
                "native owning agent differs"
            );
            return Ok(Arc::new(
                EngineSession::with_actor(owner, stream, branch, subject, authority)
                    .with_source(Arc::new(self.clone())),
            ));
        }
        let stores = self.stores.clone();
        let initialize_session_id = session_id.clone();
        let creation_source = creation_source.to_owned();
        let initialize_stream = stream.clone();
        let actor = match &subject {
            EventActor::Principal { principal_id } => {
                match authority.session_owner(&PrincipalId(principal_id.clone())) {
                    Some(principal_id) => EventActor::Principal { principal_id },
                    None => EventActor::System {
                        component: "unscoped_session_creation".into(),
                    },
                }
            }
            EventActor::System { .. } | EventActor::ChannelResponder { .. } => subject.clone(),
        };
        let store =
            tokio::task::spawn_blocking(move || -> anyhow::Result<Arc<dyn SessionEventStore>> {
                let lease = Arc::new(
                    zeroclaw_infra::session_events::ownership::SessionExecutionLease::acquire(
                        &directory,
                        &initialize_session_id,
                    )?,
                );
                {
                    let installation = InstallationGuard::acquire(&directory.join("sessions"))?;
                    anyhow::ensure!(
                        installation.active()?.0 == backend,
                        "session format changed during creation"
                    );
                    let store = open_event_store(&directory, backend.name())?;
                    seed::SessionSeed {
                        source: &creation_source,
                        alias: &alias,
                        actor,
                        context: initial,
                    }
                    .publish(store.as_ref(), &initialize_stream)?;
                }
                let store: Arc<dyn SessionEventStore> = Arc::new(stores.open_session(
                    &directory,
                    &initialize_session_id,
                    Some(lease),
                )?);
                let catalog = zeroclaw_infra::session_events::catalog::SessionCatalog::open(
                    &directory, &stores,
                )?
                .ok_or_else(|| anyhow::Error::msg("native catalog missing"))?;
                let record = catalog
                    .query()
                    .summary(&initialize_stream)?
                    .ok_or_else(|| anyhow::Error::msg("native source missing"))?;
                anyhow::ensure!(
                    record.owning_agent.as_deref() == Some(alias.as_str())
                        && !record.closed
                        && !record.deleted,
                    "native owning agent differs"
                );
                Ok(store)
            })
            .await??;
        let owner = Arc::new(SessionCoordinator::new(store).with_search(Arc::new(
            zeroclaw_infra::session_events::search::FullTextSearch,
        )));
        owner
            .recover_session(stream.clone(), Arc::new(ExclusiveSessionRecovery))
            .await?;
        *session_owner = Some(Arc::clone(&owner));
        Ok(Arc::new(
            EngineSession::with_actor(owner, stream, branch, subject, authority)
                .with_source(Arc::new(self.clone())),
        ))
    }

    async fn read_owner(
        &self,
        directory: std::path::PathBuf,
        session_id: String,
    ) -> anyhow::Result<Option<Arc<SessionCoordinator>>> {
        let slot = self
            .owners
            .lock()
            .await
            .get(&(directory.clone(), session_id.clone()))
            .cloned();
        if let Some(slot) = slot
            && let Some(owner) = slot.lock().await.as_ref().cloned()
        {
            return Ok(Some(owner));
        }
        let stores = self.stores.clone();
        let store = tokio::task::spawn_blocking(
            move || -> anyhow::Result<Option<Arc<dyn SessionEventStore>>> {
                let exists = {
                    let Some(catalog) = SessionCatalog::open(&directory, &stores)? else {
                        return Ok(None);
                    };
                    let stream =
                        zeroclaw_api::session_events::SessionId::parse(session_id.clone())?;
                    catalog
                        .query()
                        .summary(&stream)?
                        .is_some_and(|record| !record.deleted)
                };
                if !exists {
                    return Ok(None);
                }
                Ok(Some(Arc::new(stores.open_session(
                    &directory,
                    &session_id,
                    None,
                )?)))
            },
        )
        .await??;
        Ok(store.map(|store| {
            Arc::new(SessionCoordinator::new(store).with_search(Arc::new(
                zeroclaw_infra::session_events::search::FullTextSearch,
            )))
        }))
    }
}

/// Offline metadata commands use the same configured installation upgrade as
/// serving entrances. The borrowed config is consumed only for this operation.
pub fn open_repository(
    config: &zeroclaw_config::schema::Config,
) -> anyhow::Result<zeroclaw_infra::session_repository::SessionRepository> {
    zeroclaw_infra::session_events::upgrade::upgrade_installation(
        &config.data_dir,
        NativeBackend::parse(&config.channels.session_backend)?,
        |channel| legacy_session_owner(config, channel),
    )?;
    zeroclaw_infra::session_repository::SessionRepository::new(&config.data_dir)
}
#[async_trait::async_trait]
impl EngineSessionSource for NativeSessionHub {
    async fn list(
        &self,
        actor: EventActor,
        policy: Arc<dyn zeroclaw_kernel::session::query::SessionAccessPolicy>,
        limit: usize,
    ) -> anyhow::Result<Vec<zeroclaw_api::session_events::views::SessionSummary>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let (directory, _) = self.prepare().await?;
        let stores = self.stores.clone();
        tokio::task::spawn_blocking(move || -> anyhow::Result<_> {
            use zeroclaw_kernel::session::{SessionError, query::SessionAccess};
            let catalog = SessionCatalog::open(&directory, &stores)?
                .ok_or_else(|| anyhow::Error::msg("native session storage is unavailable"))?;
            let mut visible = Vec::new();
            for (_, summary) in catalog.query().indexed_summaries()? {
                if summary.closed
                    || policy
                        .session_agent_scope()
                        .is_some_and(|alias| summary.owning_agent.as_deref() != Some(alias))
                {
                    continue;
                }
                match policy.authorize_actor(&actor, &summary.head.session_id, SessionAccess::Read)
                {
                    Ok(()) => visible.push(summary),
                    Err(SessionError::PermissionDenied(_)) => continue,
                    Err(error) => return Err(error.into()),
                }
            }
            visible.sort_by(|left, right| {
                right
                    .last_activity
                    .cmp(&left.last_activity)
                    .then_with(|| left.head.session_id.cmp(&right.head.session_id))
            });
            visible.truncate(limit);
            // Revalidate immediately before disclosing the selected records.
            for summary in &visible {
                policy.authorize_actor(&actor, &summary.head.session_id, SessionAccess::Read)?;
            }
            Ok(visible)
        })
        .await?
    }
    async fn sessions(&self) -> anyhow::Result<Vec<String>> {
        let (directory, _) = self.prepare().await?;
        let stores = self.stores.clone();
        tokio::task::spawn_blocking(move || -> anyhow::Result<_> {
            let catalog = SessionCatalog::open(&directory, &stores)?
                .ok_or_else(|| anyhow::Error::msg("native session storage is unavailable"))?;
            Ok(catalog
                .query()
                .index_records()?
                .into_iter()
                .map(|record| record.session_id.to_string())
                .collect())
        })
        .await?
    }
    async fn archive_stale(
        &self,
        creation_source: &str,
        before_millis: i64,
    ) -> anyhow::Result<usize> {
        let (directory, _) = self.prepare().await?;
        let mut archived = 0;
        let stores = self.stores.clone();
        let catalog_directory = directory.clone();
        let creation_source = creation_source.to_owned();
        let sessions = tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<String>> {
            let catalog = SessionCatalog::open(&catalog_directory, &stores)?
                .ok_or_else(|| anyhow::Error::msg("native session storage is unavailable"))?;
            Ok(catalog
                .query()
                .index_records()?
                .into_iter()
                .filter(|record| record.creation_source.as_deref() == Some(&creation_source))
                .map(|record| record.session_id.to_string())
                .collect())
        })
        .await??;
        for session_id in sessions {
            {
                let mut owners = self.owners.lock().await;
                let key = (directory.clone(), session_id.clone());
                if owners.get(&key).is_some_and(|owner| !owner_is_idle(owner)) {
                    continue;
                }
                owners.remove(&key);
            }
            let lease_directory = directory.clone();
            let lease_session_id = session_id.clone();
            let lease = tokio::task::spawn_blocking(move || {
                zeroclaw_infra::session_events::ownership::SessionExecutionLease::try_acquire(
                    &lease_directory,
                    &lease_session_id,
                )
            })
            .await??;
            let Some(lease) = lease else {
                continue;
            };
            let stores = self.stores.clone();
            let store_directory = directory.clone();
            let store = tokio::task::spawn_blocking(move || {
                stores.open_session(&store_directory, &session_id, Some(Arc::new(lease)))
            })
            .await??;
            let stream = store.session_id().clone();
            let owner = Arc::new(SessionCoordinator::new(Arc::new(store)));
            let summary = owner
                .query_actor(
                    EventActor::System {
                        component: "source_metadata_reader".into(),
                    },
                    Arc::new(InternalDescriptorRead),
                )
                .summary(stream.clone())
                .await?;
            if !summary.closed && !summary.running && summary.last_activity < before_millis {
                let event = SessionEvent::typed(
                    stream.clone(),
                    EventData::SessionClosed {
                        reason_key: "session_ttl_expired".into(),
                    },
                    EventActor::System {
                        component: "session_retention".into(),
                    },
                )?;
                owner
                    .append_async(AppendRequest {
                        session_id: stream.clone(),
                        expected_head: summary.head.clone(),
                        idempotency_key: format!(
                            "retention-close:{stream}:{}",
                            summary.head.sequence
                        ),
                        events: vec![event],
                        payloads: vec![],
                        erasure_reference_commitment: None,
                    })
                    .await?;
                archived += 1;
            }
        }
        Ok(archived)
    }
    async fn describe(
        &self,
        session_id: String,
    ) -> anyhow::Result<Option<zeroclaw_kernel::session::engine::SessionDescriptor>> {
        let (directory, _) = self.prepare().await?;
        let stream = zeroclaw_api::session_events::SessionId::parse(session_id.to_owned())?;
        let Some(record) = catalog_record(directory, stream, self.stores.clone()).await? else {
            return Ok(None);
        };
        let owner = record
            .fields
            .get("principal_id")
            .cloned()
            .map(serde_json::from_value)
            .transpose()?
            .flatten();
        let owning_agent = record
            .owning_agent
            .ok_or_else(|| anyhow::Error::msg("native source owning agent missing"))?;
        Ok(Some(zeroclaw_kernel::session::engine::SessionDescriptor {
            stream: record.head.session_id,
            owning_agent,
            owner,
        }))
    }
    async fn query(
        &self,
        session_id: String,
        principal: PrincipalId,
        policy: Arc<dyn zeroclaw_kernel::session::query::SessionAccessPolicy>,
    ) -> anyhow::Result<Option<zeroclaw_kernel::session::engine::SessionReadHandle>> {
        let (directory, _) = self.prepare().await?;
        let stream = zeroclaw_api::session_events::SessionId::parse(session_id.to_owned())?;
        policy.authorize(
            &principal,
            &stream,
            zeroclaw_kernel::session::query::SessionAccess::Read,
        )?;
        let Some(owner) = self.read_owner(directory, session_id).await? else {
            return Ok(None);
        };
        let query = owner.query(principal, policy.clone());
        if let Some(alias) = policy.session_agent_scope() {
            let summary = query.summary(stream.clone()).await?;
            if summary.owning_agent.as_deref() != Some(alias) {
                return Ok(None);
            }
        }
        Ok(Some(zeroclaw_kernel::session::engine::SessionReadHandle {
            stream,
            query,
        }))
    }
    async fn open_system(
        &self,
        session_id: String,
        creation_source: &str,
        alias: String,
        component: String,
        authority: Arc<dyn EngineSessionAuthority>,
        initial: ContextSnapshot,
    ) -> anyhow::Result<Arc<dyn SessionExecutionPort>> {
        anyhow::ensure!(!component.is_empty(), "system session component missing");
        self.open_session(
            session_id,
            creation_source,
            alias,
            EventActor::System { component },
            authority,
            initial,
        )
        .await
    }

    async fn open(
        &self,
        session_id: String,
        creation_source: &str,
        alias: String,
        principal: PrincipalId,
        authority: Arc<dyn EngineSessionAuthority>,
        initial: ContextSnapshot,
    ) -> anyhow::Result<Arc<dyn SessionExecutionPort>> {
        self.open_session(
            session_id,
            creation_source,
            alias,
            EventActor::Principal {
                principal_id: principal.0,
            },
            authority,
            initial,
        )
        .await
    }
}

struct ExclusiveSessionRecovery;
impl zeroclaw_kernel::session::recovery::StreamExecutionLossVerifier for ExclusiveSessionRecovery {
    fn verify_stream_loss(
        &self,
        stream: &SessionId,
        through: &EventCursor,
    ) -> Result<PayloadRecord, zeroclaw_kernel::session::SessionError> {
        PayloadRecord::json(
            "session.evidence/v1",
            &serde_json::json!({
                "source":"exclusive_session_id_execution_lease","stream":stream,"through":through,
                "reason_key":"prior_source_writer_handles_released","owner_exit_cause":"unknown",
            }),
        )
        .map_err(AppendError::from)
        .map_err(Into::into)
    }
}

// This capability exposes only installation metadata to the trusted resource
// resolver. It cannot be supplied by a transport caller or authorize execution.
async fn catalog_record(
    directory: std::path::PathBuf,
    stream: SessionId,
    stores: Arc<zeroclaw_infra::session_events::scoped::EventStorePool>,
) -> anyhow::Result<Option<zeroclaw_infra::session_events::catalog::Session>> {
    tokio::task::spawn_blocking(move || {
        let catalog =
            zeroclaw_infra::session_events::catalog::SessionCatalog::open(&directory, &stores)?
                .ok_or_else(|| anyhow::Error::msg("native catalog missing"))?;
        Ok(catalog
            .query()
            .summary(&stream)?
            .filter(|record| !record.deleted))
    })
    .await?
}

struct InternalDescriptorRead;
impl zeroclaw_kernel::session::query::SessionAccessPolicy for InternalDescriptorRead {
    fn authorize_actor(
        &self,
        actor: &EventActor,
        _stream: &SessionId,
        access: zeroclaw_kernel::session::query::SessionAccess,
    ) -> Result<(), zeroclaw_kernel::session::SessionError> {
        if matches!(actor,EventActor::System {component} if component=="source_metadata_reader")
            && matches!(access, zeroclaw_kernel::session::query::SessionAccess::Read)
        {
            Ok(())
        } else {
            Err(zeroclaw_kernel::session::SessionError::PermissionDenied(
                "internal metadata scope mismatch".into(),
            ))
        }
    }
    fn authorize(
        &self,
        _principal: &PrincipalId,
        _stream: &SessionId,
        _access: zeroclaw_kernel::session::query::SessionAccess,
    ) -> Result<(), zeroclaw_kernel::session::SessionError> {
        Err(zeroclaw_kernel::session::SessionError::PermissionDenied(
            "metadata reader is a system component".into(),
        ))
    }
}

#[cfg(test)]
mod tests;
