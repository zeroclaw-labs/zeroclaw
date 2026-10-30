//! Composition for native session families. The installation manifest owns
//! the active format; events own family identity and derivation.
//! Live policies are supplied by the authenticated entrance, never cached here.
use anyhow::Context;
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
    family::EventStorePool,
    installation::{InstallationGuard, NativeBackend, family_identity},
    open_event_store,
};
use zeroclaw_kernel::session::{
    SessionCoordinator,
    engine::{EngineSession, EngineSessionAuthority, EngineSessionSource},
};
mod execution_context;
mod seed;
pub use execution_context::execution_context_evidence;

type FamilyOwner = Arc<tokio::sync::Mutex<Option<Arc<SessionCoordinator>>>>;
type FamilyOwners = HashMap<(std::path::PathBuf, String), FamilyOwner>;

fn owner_is_idle(slot: &FamilyOwner) -> bool {
    Arc::strong_count(slot) == 1
        && slot.try_lock().is_ok_and(|owner| {
            owner
                .as_ref()
                .is_none_or(|owner| Arc::strong_count(owner) == 1)
        })
}

/// Missing historical ownership is not permission to assign every family to
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
    owners: Arc<tokio::sync::Mutex<FamilyOwners>>,
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
            Ok(guard.active().is_ok_and(|(active, _)| active == backend)
                && !check_directory
                    .join("sessions/acp-sessions.db")
                    .try_exists()?)
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
    async fn open_family(
        &self,
        family: String,
        alias: String,
        subject: EventActor,
        authority: Arc<dyn EngineSessionAuthority>,
        initial: ContextSnapshot,
    ) -> anyhow::Result<Arc<dyn SessionExecutionPort>> {
        let (directory, backend) = self.prepare().await?;
        let owner_key = (directory.clone(), family.clone());
        let slot = self
            .owners
            .lock()
            .await
            .entry(owner_key)
            .or_default()
            .clone();
        let mut family_owner = slot.lock().await;
        let stream = self.stream_id(&family).await?;
        let branch = BranchId::parse(format!("branch-{stream}"))?;
        // The identity is bound before physical initialization and every later
        // read/command rechecks the host's current authority.
        zeroclaw_kernel::session::query::SessionAccessPolicy::authorize_actor(
            authority.as_ref(),
            &subject,
            &stream,
            zeroclaw_kernel::session::query::SessionAccess::Write,
        )?;
        if let Some(owner) = family_owner.as_ref().cloned() {
            let record = catalog_record(directory.clone(), stream.clone(), self.stores.clone())
                .await?
                .ok_or_else(|| anyhow::Error::msg("native family missing"))?;
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
        let initialize_family = family.clone();
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
                    zeroclaw_infra::session_events::ownership::FamilyExecutionLease::acquire(
                        &directory,
                        &initialize_family,
                    )?,
                );
                {
                    let installation = InstallationGuard::acquire(&directory.join("sessions"))?;
                    anyhow::ensure!(
                        installation.active()?.0 == backend,
                        "session format changed during creation"
                    );
                    let store = open_event_store(&directory, backend.name())?;
                    seed::FamilySeed {
                        family: &initialize_family,
                        alias: &alias,
                        actor,
                        context: initial,
                    }
                    .publish(store.as_ref(), &initialize_stream)?;
                }
                let store: Arc<dyn SessionEventStore> =
                    Arc::new(stores.open_family(&directory, &initialize_family, Some(lease))?);
                let catalog = zeroclaw_infra::session_events::catalog::SessionCatalog::open(
                    &directory, &stores,
                )?
                .ok_or_else(|| anyhow::Error::msg("native catalog missing"))?;
                let record = catalog
                    .query()
                    .summary(&initialize_stream)?
                    .ok_or_else(|| anyhow::Error::msg("native family missing"))?;
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
            .recover_family(stream.clone(), Arc::new(ExclusiveFamilyRecovery))
            .await?;
        *family_owner = Some(Arc::clone(&owner));
        Ok(Arc::new(
            EngineSession::with_actor(owner, stream, branch, subject, authority)
                .with_source(Arc::new(self.clone())),
        ))
    }
    pub fn stream_id(family: &str) -> Result<SessionStreamId, ContractError> {
        SessionStreamId::parse(format!(
            "family-{}",
            Digest::from_bytes(family.as_bytes()).as_str()
        ))
    }

    async fn read_owner(
        &self,
        directory: std::path::PathBuf,
        family: String,
    ) -> anyhow::Result<Option<Arc<SessionCoordinator>>> {
        let slot = self
            .owners
            .lock()
            .await
            .get(&(directory.clone(), family.clone()))
            .cloned();
        if let Some(slot) = slot
            && let Some(owner) = slot.lock().await.as_ref().cloned()
        {
            return Ok(Some(owner));
        }
        let stores = self.stores.clone();
        let store = tokio::task::spawn_blocking(
            move || -> anyhow::Result<Option<Arc<dyn SessionEventStore>>> {
                let installation = InstallationGuard::acquire(&directory.join("sessions"))?;
                let exists = zeroclaw_infra::session_events::installation::families(
                    &directory,
                    installation.active()?.0,
                )?
                .contains_key(&family);
                drop(installation);
                if !exists {
                    return Ok(None);
                }
                Ok(Some(Arc::new(
                    stores.open_family(&directory, &family, None)?,
                )))
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
    namespace: &str,
) -> anyhow::Result<zeroclaw_infra::session_repository::SessionRepository> {
    zeroclaw_infra::session_events::upgrade::upgrade_installation(
        &config.data_dir,
        NativeBackend::parse(&config.channels.session_backend)?,
        |channel| legacy_session_owner(config, channel),
    )?;
    zeroclaw_infra::session_repository::SessionRepository::for_namespace(
        &config.data_dir,
        namespace,
    )
}
#[async_trait::async_trait]
impl EngineSessionSource for NativeSessionHub {
    async fn stream_id(&self, family: &str) -> Result<SessionStreamId, ContractError> {
        let directory = self.config.read().data_dir.clone();
        let family = family.to_owned();
        tokio::task::spawn_blocking(move || -> anyhow::Result<SessionStreamId> {
            if !directory
                .join("sessions/session-format.json")
                .try_exists()?
            {
                return Ok(Self::stream_id(&family)?);
            }
            let installation = InstallationGuard::acquire(&directory.join("sessions"))?;
            zeroclaw_infra::session_events::installation::families(
                &directory,
                installation.active()?.0,
            )?
            .remove(&family)
            .map_or_else(|| Ok(Self::stream_id(&family)?), Ok)
        })
        .await
        .map_err(|error| ContractError::InvalidRequest(error.to_string()))?
        .map_err(|error| ContractError::InvalidRequest(error.to_string()))
    }
    async fn families(&self) -> anyhow::Result<Vec<String>> {
        let (directory, _) = self.prepare().await?;
        tokio::task::spawn_blocking(move || -> anyhow::Result<_> {
            let installation = InstallationGuard::acquire(&directory.join("sessions"))?;
            Ok(zeroclaw_infra::session_events::installation::families(
                &directory,
                installation.active()?.0,
            )?
            .into_keys()
            .collect())
        })
        .await?
    }
    async fn archive_stale(&self, prefix: &str, before_millis: i64) -> anyhow::Result<usize> {
        let (directory, _) = self.prepare().await?;
        let mut archived = 0;
        for family in self
            .families()
            .await?
            .into_iter()
            .filter(|family| family.starts_with(prefix))
        {
            {
                let mut owners = self.owners.lock().await;
                let key = (directory.clone(), family.clone());
                if owners.get(&key).is_some_and(|owner| !owner_is_idle(owner)) {
                    continue;
                }
                owners.remove(&key);
            }
            let lease_directory = directory.clone();
            let lease_family = family.clone();
            let lease = tokio::task::spawn_blocking(move || {
                zeroclaw_infra::session_events::ownership::FamilyExecutionLease::try_acquire(
                    &lease_directory,
                    &lease_family,
                )
            })
            .await??;
            let Some(lease) = lease else {
                continue;
            };
            let stores = self.stores.clone();
            let store_directory = directory.clone();
            let store = tokio::task::spawn_blocking(move || {
                stores.open_family(&store_directory, &family, Some(Arc::new(lease)))
            })
            .await??;
            let stream = store.root_stream().clone();
            let owner = Arc::new(SessionCoordinator::new(Arc::new(store)));
            let summary = owner
                .query_actor(
                    EventActor::System {
                        component: "family_metadata_reader".into(),
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
                        stream_id: stream.clone(),
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
        family: String,
    ) -> anyhow::Result<Option<zeroclaw_kernel::session::engine::FamilyDescriptor>> {
        let (directory, _) = self.prepare().await?;
        let stream = self.stream_id(&family).await?;
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
        Ok(Some(zeroclaw_kernel::session::engine::FamilyDescriptor {
            stream: record.head.stream_id,
            owning_agent: record
                .owning_agent
                .context("native family owning agent missing")?,
            owner,
        }))
    }
    async fn query(
        &self,
        family: String,
        principal: PrincipalId,
        policy: Arc<dyn zeroclaw_kernel::session::query::SessionAccessPolicy>,
    ) -> anyhow::Result<Option<zeroclaw_kernel::session::engine::FamilyQuery>> {
        let (directory, _) = self.prepare().await?;
        let stream = self.stream_id(&family).await?;
        policy.authorize(
            &principal,
            &stream,
            zeroclaw_kernel::session::query::SessionAccess::Read,
        )?;
        let Some(owner) = self.read_owner(directory, family).await? else {
            return Ok(None);
        };
        let query = owner.query(principal, policy);
        Ok(Some(zeroclaw_kernel::session::engine::FamilyQuery {
            stream,
            query,
        }))
    }
    async fn open_system(
        &self,
        family: String,
        alias: String,
        component: String,
        authority: Arc<dyn EngineSessionAuthority>,
        initial: ContextSnapshot,
    ) -> anyhow::Result<Arc<dyn SessionExecutionPort>> {
        anyhow::ensure!(!component.is_empty(), "system session component missing");
        self.open_family(
            family,
            alias,
            EventActor::System { component },
            authority,
            initial,
        )
        .await
    }

    async fn open(
        &self,
        family: String,
        alias: String,
        principal: PrincipalId,
        authority: Arc<dyn EngineSessionAuthority>,
        initial: ContextSnapshot,
    ) -> anyhow::Result<Arc<dyn SessionExecutionPort>> {
        self.open_family(
            family,
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

struct ExclusiveFamilyRecovery;
impl zeroclaw_kernel::session::recovery::StreamExecutionLossVerifier for ExclusiveFamilyRecovery {
    fn verify_stream_loss(
        &self,
        stream: &SessionStreamId,
        through: &EventCursor,
    ) -> Result<PayloadRecord, zeroclaw_kernel::session::SessionError> {
        PayloadRecord::json(
            "session.evidence/v1",
            &serde_json::json!({
                "source":"exclusive_family_execution_lease","stream":stream,"through":through,
                "reason_key":"prior_family_writer_handles_released","owner_exit_cause":"unknown",
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
    stream: SessionStreamId,
    stores: Arc<zeroclaw_infra::session_events::family::EventStorePool>,
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
        _stream: &SessionStreamId,
        access: zeroclaw_kernel::session::query::SessionAccess,
    ) -> Result<(), zeroclaw_kernel::session::SessionError> {
        if matches!(actor,EventActor::System {component} if component=="family_metadata_reader")
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
        _stream: &SessionStreamId,
        _access: zeroclaw_kernel::session::query::SessionAccess,
    ) -> Result<(), zeroclaw_kernel::session::SessionError> {
        Err(zeroclaw_kernel::session::SessionError::PermissionDenied(
            "metadata reader is a system component".into(),
        ))
    }
}

#[cfg(test)]
mod tests;
