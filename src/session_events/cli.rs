//! Local CLI composition. The CLI is the authenticated shared operator;
//! remote/channel identities must enter through their own admission owner.
use super::{AppendError, Arc, Path, SessionCoordinator, SessionEventStore, open_event_store};
use zeroclaw_api::principal::PrincipalId;
use zeroclaw_api::session_events::capture::{EngineTurnInput, SessionExecutionPort};
use zeroclaw_api::session_events::{
    AppendRequest, BranchId, CaptureMode, Digest, EventActor, EventCursor, EventData, EventKind,
    PayloadRecord, SessionEvent, SessionId, SessionStreamId, StoredPayloadRef,
};
use zeroclaw_infra::session_events::installation::{
    InstallationGuard, NativeBackend, family_identity,
};
use zeroclaw_kernel::session::engine::{EngineSession, EngineSessionAuthority};
use zeroclaw_kernel::session::query::{SessionAccess, SessionAccessPolicy};
use zeroclaw_kernel::session::{EffectIntent, PermissionResolver, SessionError};
use zeroclaw_runtime::live_config_authority::AgentExecutionCapability;

struct CliAuthority {
    capability: AgentExecutionCapability,
    alias: String,
    stream: SessionStreamId,
    origin: zeroclaw_api::ingress::TurnOrigin,
    parent_alias: Option<String>,
}
impl CliAuthority {
    fn current(&self) -> Result<(), SessionError> {
        if let Some(parent) = &self.parent_alias {
            let handle = self.capability.config_handle();
            let config = handle.read();
            if parent != &self.alias && config.delegate_target_mode(parent, &self.alias).is_none() {
                return Err(SessionError::PermissionDenied(
                    "CLI child target revoked".into(),
                ));
            }
        }
        self.capability
            .admit(&self.alias)
            .and_then(|admission| admission.revalidate())
            .map_err(|error| SessionError::PermissionDenied(error.to_string()))
    }
}
impl SessionAccessPolicy for CliAuthority {
    fn authorize_reasoning(
        &self,
        actor: &EventActor,
        stream: &SessionStreamId,
    ) -> Result<(), SessionError> {
        self.authorize_actor(actor, stream, SessionAccess::Read)
    }
    fn authorize(
        &self,
        principal: &PrincipalId,
        stream: &SessionStreamId,
        access: SessionAccess,
    ) -> Result<(), SessionError> {
        if *principal != PrincipalId::shared_operator()
            || *stream != self.stream
            || matches!(access, SessionAccess::Erase)
        {
            return Err(SessionError::PermissionDenied(
                "local CLI session scope mismatch".into(),
            ));
        }
        self.current()
    }
}
impl PermissionResolver for CliAuthority {
    fn tool_authority_evidence(
        &self,
        stream: &zeroclaw_api::session_events::SessionStreamId,
        name: &str,
    ) -> Result<serde_json::Value, zeroclaw_kernel::session::SessionError> {
        if stream != &self.stream {
            return Err(SessionError::PermissionDenied(
                "CLI tool evidence scope mismatch".into(),
            ));
        }
        self.current()?;
        let handle = self.capability.config_handle();
        let config = handle.read();
        let profile = config
            .risk_profile_for_agent(&self.alias)
            .ok_or_else(|| SessionError::PermissionDenied("agent risk profile missing".into()))?;
        if profile
            .effective_allowed_tools()
            .is_some_and(|tools| !tools.iter().any(|tool| tool == name))
        {
            return Err(zeroclaw_kernel::session::SessionError::PermissionDenied(
                "current tool policy denies action".into(),
            ));
        }
        Ok(
            serde_json::json!({"source":"live_tool_admission","config_revision":config.revision(),"matched_rules":{"allowed_tools":profile.effective_allowed_tools(),"auto_approve":profile.auto_approve.iter().filter(|tool|tool.as_str()==name).collect::<Vec<_>>(),"always_ask":profile.always_ask.iter().filter(|tool|tool.as_str()==name).collect::<Vec<_>>(),"autonomy_level":profile.level}}),
        )
    }

    fn child_authority(
        &self,
        target: &str,
        stream: &SessionStreamId,
    ) -> Result<Arc<dyn EngineSessionAuthority>, SessionError> {
        self.current()?;
        let handle = self.capability.config_handle();
        let config = handle.read();
        if target != self.alias && config.delegate_target_mode(&self.alias, target).is_none() {
            return Err(SessionError::PermissionDenied(
                "CLI child target not reachable".into(),
            ));
        }
        drop(config);
        let child = Self {
            capability: self.capability.clone(),
            alias: target.into(),
            stream: stream.clone(),
            origin: zeroclaw_api::ingress::TurnOrigin::SubTurn,
            parent_alias: Some(self.alias.clone()),
        };
        child.current()?;
        Ok(Arc::new(child))
    }

    fn authorize_context(
        &self,
        stream: &SessionStreamId,
        _: &BranchId,
    ) -> Result<(), SessionError> {
        if stream != &self.stream {
            return Err(SessionError::PermissionDenied(
                "CLI context scope mismatch".into(),
            ));
        }
        self.current()
    }
    fn authorize(&self, intent: &EffectIntent) -> Result<(), SessionError> {
        if intent.stream_id != self.stream
            || !matches!(
                intent.event.body.kind(),
                EventKind::ProviderAttemptStarted
                    | EventKind::ToolEffectStarted
                    | EventKind::ChildInvocationRequested
            )
        {
            return Err(SessionError::PermissionDenied(
                "CLI effect scope mismatch".into(),
            ));
        }
        self.current()
    }
    fn authorize_tool(
        &self,
        intent: &EffectIntent,
        name: &str,
        _arguments: &serde_json::Value,
    ) -> Result<(), SessionError> {
        PermissionResolver::authorize(self, intent)?;
        let handle = self.capability.config_handle();
        let config = handle.read();
        let profile = config
            .risk_profile_for_agent(&self.alias)
            .ok_or_else(|| SessionError::PermissionDenied("agent risk profile missing".into()))?;
        if profile
            .effective_allowed_tools()
            .is_some_and(|allowed| !allowed.iter().any(|tool| tool == name))
        {
            return Err(SessionError::PermissionDenied(
                "current tool allowlist denies action".into(),
            ));
        }
        // Sandbox, command approval and the scoped registry remain owned by
        // the existing engine/tools and execute after this durable boundary.
        Ok(())
    }
}

struct CliSession {
    owner: Arc<SessionCoordinator>,
    authority: Arc<CliAuthority>,
    branch: BranchId,
}

/// Create one native family for an unpersisted CLI conversation. Creation is
/// fenced before publication; no legacy message writer is enabled for it.
pub async fn new_cli_session(
    data_dir: &Path,
    backend: &str,
    alias: &str,
    capability: AgentExecutionCapability,
) -> anyhow::Result<Arc<dyn SessionExecutionPort>> {
    open_cli_session(data_dir, backend, alias, capability, None).await
}

pub async fn open_cli_session(
    data_dir: &Path,
    backend: &str,
    alias: &str,
    capability: AgentExecutionCapability,
    state_file: Option<&Path>,
) -> anyhow::Result<Arc<dyn SessionExecutionPort>> {
    use zeroclaw_infra::session_events::cli_state::{CliStateBinding, CliStateGuard};
    use zeroclaw_infra::session_events::family::FamilyEventStore;
    use zeroclaw_infra::session_events::import::{
        LegacyImport, LegacySnapshot, import_legacy_snapshot,
    };
    let data_dir = data_dir.to_owned();
    let alias = alias.to_owned();
    let persistent = state_file.is_some();
    let state_file = state_file.map(Path::to_owned);
    let backend = match backend {
        "sqlite" => NativeBackend::Sqlite,
        "jsonl" => NativeBackend::Jsonl,
        _ => anyhow::bail!("unsupported native session backend"),
    };
    capability.admit(&alias)?.revalidate()?;
    let session = tokio::task::spawn_blocking(move || -> anyhow::Result<CliSession> {
        std::fs::create_dir_all(&data_dir)?;
        let data_dir = std::fs::canonicalize(&data_dir)?;
        let mut lease = state_file.as_deref().map(CliStateGuard::acquire).transpose()?;
        let stream = if let Some(lease) = &lease {
            SessionStreamId::parse(format!("cli-state-{}", Digest::from_bytes(&zeroclaw_api::session_events::canonical_json(
                &serde_json::json!({"path":lease.path(),"agent":alias}),
            )?).as_str()))?
        } else { SessionStreamId::new() };
        let branch = BranchId::parse(format!("branch-{stream}"))?;
        let session_id = SessionId::parse(format!("session-{stream}"))?;
        let family = format!("cli:{}:{stream}", alias);
        zeroclaw_infra::session_events::upgrade::upgrade_installation(&data_dir, backend, |channel| {
            zeroclaw_session_host::legacy_session_owner(&capability.config_handle().read(), channel)
        })?;
        let published = {
            let installation = InstallationGuard::acquire(&data_dir.join("sessions"))?;
            anyhow::ensure!(installation.active()?.0 == backend, "CLI session backend changed");
            zeroclaw_infra::session_events::installation::families(&data_dir, backend)?.contains_key(&family)
        };
        if let Some(lease) = &mut lease {
            anyhow::ensure!(published || !lease.has_backup()?, "CLI backup has no matching family in this installation");
            lease.bind(CliStateBinding {version:1, data_dir:data_dir.clone(), family:family.clone()})?;
        }
        let legacy = !published && lease.as_ref().is_some_and(|lease| lease.path().exists());
        if legacy {
            let state_path = lease.as_ref().ok_or_else(|| anyhow::Error::msg("CLI legacy import lease missing"))?.path().to_owned();
            let imported = import_legacy_snapshot(&data_dir, LegacyImport {
                family:&family, owning_agent:&alias, target:backend, stream:stream.clone(), branch:branch.clone(),
                session:session_id.clone(), source_kind:"cli-state-json",
            }, || {
                let mut options = std::fs::OpenOptions::new();
                options.read(true);
                #[cfg(unix)] {
                    use std::os::unix::fs::OpenOptionsExt;
                    options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
                }
                let file = options.open(&state_path).map_err(|error| AppendError::StorageUnavailable(error.to_string()))?;
                if !file.metadata().map_err(|error| AppendError::StorageUnavailable(error.to_string()))?.is_file() {
                    return Err(AppendError::StorageUnavailable("CLI source is not a regular file".into()));
                }
                let state: zeroclaw_runtime::agent::history::InteractiveSessionState = serde_json::from_reader(std::io::BufReader::new(file))
                    .map_err(|error| AppendError::Corrupt(error.to_string()))?;
                let crumb = if state.version >= 2 {Some(state.history_has_trim_breadcrumb)} else {
                    state.history.iter().find(|message| message.role != "system")
                        .map(|message| zeroclaw_runtime::agent::history::is_history_trim_breadcrumb_text(&message.content))
                };
                Ok(LegacySnapshot {messages:state.history.into_iter().map(|message| zeroclaw_infra::session_backend::TimestampedMessage { message, created_at: None }).collect(), history_has_trim_breadcrumb:crumb, metadata:vec![]})
            })?;
            anyhow::ensure!(imported == stream, "CLI import identity differs from path binding");
        } else if !published {
            let installation = InstallationGuard::acquire(&data_dir.join("sessions"))?;
            anyhow::ensure!(installation.active()?.0 == backend, "CLI session backend changed");
            let store = open_event_store(&data_dir, match backend {NativeBackend::Sqlite=>"sqlite", NativeBackend::Jsonl=>"jsonl"})?;
            let key = format!("native-family-create:{stream}");
            if store.lookup_receipt(&stream, &key)?.is_none() {
                let mut root = SessionEvent::typed(stream.clone(), EventData::SessionStarted {
                    session_id, owning_agent:alias.clone(), capture_mode:CaptureMode::Native,
                }, EventActor::Principal {principal_id:PrincipalId::shared_operator().0})?;
                root.branch_id=Some(branch.clone());
                let context_payload=PayloadRecord::json("session.context/v1", &serde_json::json!({"messages":[],"history_has_trim_breadcrumb":false}))?;
                let mut context=SessionEvent::typed(stream.clone(), EventData::ContextProjectionCreated {
                    source_ranges:vec![], summary_ref:None, budget:0, reason_key:"initial_context".into(),
                    algorithm_version:"native-cli/v1".into(), result_ref:StoredPayloadRef::from(&context_payload),
                    summary_source_refs:vec![], transform_started_event_id:None,
                }, EventActor::default())?;
                context.branch_id=Some(branch.clone());
                let (identity, identity_payload) = family_identity(stream.clone(), &family)?;
                SessionCoordinator::new(Arc::clone(&store)).append(AppendRequest {
                    stream_id:stream.clone(), expected_head:EventCursor::beginning(stream.clone()), idempotency_key:key.clone(),
                    events:vec![root,identity,context], payloads:vec![context_payload,identity_payload], erasure_reference_commitment:None,
                })?;
            }
            drop(installation);
        }
        if let Some(lease) = &lease {
            zeroclaw_infra::session_events::upgrade::archive_path(lease.path(), "backup")?;
            lease.finish_archive()?;
        }
        let store: Arc<dyn SessionEventStore> = match lease {
            Some(lease) => Arc::new(FamilyEventStore::open_with_cli_lease(&data_dir, &family, Arc::new(lease))?),
            None => Arc::new(FamilyEventStore::open_with_execution_lease(&data_dir, &family, Arc::new(zeroclaw_infra::session_events::ownership::FamilyExecutionLease::acquire(&data_dir,&family)?))?),
        };
        let page=store.read_page(&stream,&EventCursor::beginning(stream.clone()),None,4)?;
        anyhow::ensure!(matches!(page.events.first().map(|event|&event.body.data),Some(EventData::SessionStarted {owning_agent,..}) if owning_agent==&alias),"CLI persisted owning agent differs");
        anyhow::ensure!(store.read_latest_event(&stream, &branch, None, &EventKind::ContextProjectionCreated)?.is_some(),"CLI initial context missing");
        Ok(CliSession {owner:Arc::new(SessionCoordinator::new(store).with_search(Arc::new(zeroclaw_infra::session_events::search::FullTextSearch))),authority:Arc::new(CliAuthority {capability,alias,stream,origin:zeroclaw_api::ingress::TurnOrigin::Interactive,parent_alias:None}),branch})
    }).await??;
    // The exclusive path lease is retained by the store itself. Acquisition
    // proves every prior coordinator/commit worker/observer has released it.
    if persistent {
        session
            .owner
            .recover_family(
                session.authority.stream.clone(),
                Arc::new(CliOwnershipRecovery),
            )
            .await?;
    }
    Ok(Arc::new(EngineSession::new(
        session.owner,
        session.authority.stream.clone(),
        session.branch,
        PrincipalId::shared_operator(),
        session.authority,
    )))
}
struct CliOwnershipRecovery;
impl zeroclaw_kernel::session::recovery::StreamExecutionLossVerifier for CliOwnershipRecovery {
    fn verify_stream_loss(
        &self,
        stream: &SessionStreamId,
        through: &EventCursor,
    ) -> Result<PayloadRecord, SessionError> {
        Ok(PayloadRecord::json(
            "session.evidence/v1",
            &serde_json::json!({
                "source":"exclusive_cli_path_lease","stream":stream,"through":through,
                "reason_key":"prior_cli_writer_handles_released","owner_exit_cause":"unknown",
            }),
        )
        .map_err(AppendError::from)?)
    }
}

impl EngineSessionAuthority for CliAuthority {
    fn execution_binding(&self, input: &EngineTurnInput) -> Result<PayloadRecord, SessionError> {
        if input.agent_alias.as_deref() != Some(self.alias.as_str())
            || input.ingress.origin != self.origin
        {
            return Err(SessionError::PermissionDenied(
                "CLI ingress binding changed".into(),
            ));
        }
        self.current()?;
        Ok(PayloadRecord::json(
            "session.execution-binding/v1",
            &serde_json::json!({
                "config_revision":self.capability.config_handle().revision(),
                "requested_provider_ref":input.provider_ref,"requested_model":input.model,
            }),
        )
        .map_err(AppendError::from)?)
    }
    fn maintenance_binding(&self) -> Result<PayloadRecord, SessionError> {
        self.current()?;
        Ok(PayloadRecord::json("session.execution-binding/v1", &serde_json::json!({
            "config_revision":self.capability.config_handle().revision(),"purpose":"context_maintenance",
        })).map_err(AppendError::from)?)
    }
    fn diagnostic(&self, error: &anyhow::Error) -> Option<String> {
        Some(zeroclaw_runtime::agent::Agent::session_execution_diagnostic(error))
    }
}
