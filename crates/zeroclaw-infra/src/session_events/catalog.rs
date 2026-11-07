//! Transport-neutral session metadata projected from the active event store.
//! The installation lock serializes conditional metadata changes and cutover.
use super::installation::{InstallationGuard, creation_source};
use super::*;
use std::collections::BTreeMap;
use zeroclaw_api::session_events::{
    BranchId, CaptureMode, EventActor, EventData, RangeDisposition, SessionId, SourceRange,
    StoredPayloadRef,
};

fn string_field(fields: &BTreeMap<String, serde_json::Value>, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| fields.get(*key).and_then(serde_json::Value::as_str))
        .map(str::to_owned)
}

fn bounded_preview(value: &str) -> String {
    let mut chars = value.chars();
    let truncated: String = chars.by_ref().take(256).collect();
    if chars.next().is_some() {
        format!("{truncated}…")
    } else {
        truncated
    }
}

fn residual_metadata(fields: &BTreeMap<String, serde_json::Value>) -> serde_json::Value {
    const INDEXED: &[&str] = &[
        "creation_source",
        "title",
        "cwd",
        "workspace_dir",
        "working_directory",
        "git_remote",
        "git.remote",
        "git_branch",
        "git.branch",
        "git_commit",
        "git.commit",
        "provider",
        "provider_ref",
        "model_provider",
        "model",
        "model_name",
        "category",
        "session_category",
        "project",
        "project_id",
    ];
    serde_json::Value::Object(
        fields
            .iter()
            .filter(|(key, _)| !INDEXED.contains(&key.as_str()))
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect(),
    )
}

pub struct SessionCatalog {
    guard: InstallationGuard,
    store: Arc<dyn SessionEventStore>,
}

use zeroclaw_api::session_events::catalog::SessionReducer;
pub use zeroclaw_api::session_events::catalog::{Session, SessionDetail, SessionIndexRecord};

/// Common read surface for all protocol entrances.
pub struct SessionQuery<'a> {
    catalog: &'a SessionCatalog,
}

/// Common metadata/lifecycle command surface for all protocol entrances.
pub struct SessionCommand<'a> {
    catalog: &'a SessionCatalog,
}

/// A copy creates only a new identity and a frozen source coordinate. The
/// source session ID is carried by the cursor, never repeated independently.
pub struct SessionCopy<'a> {
    pub session_id: &'a SessionId,
    pub source: &'a zeroclaw_api::session_events::context_read::CopySource,
    pub creation_source: &'a str,
    pub owning_agent: &'a str,
}

impl SessionCatalog {
    pub fn open(
        data_dir: &Path,
        stores: &super::scoped::EventStorePool,
    ) -> Result<Option<Self>, AppendError> {
        let guard = InstallationGuard::acquire(&data_dir.join("sessions"))?;
        if guard.phase().is_none() {
            guard.require_legacy().map_err(jsonl::storage)?;
            return Ok(None);
        }
        let store = stores.open_installation(&guard)?;
        let mut catalog = Self { guard, store };
        if catalog.cleanup_temporary()? {
            let directory =
                catalog.guard.directory().parent().ok_or_else(|| {
                    AppendError::Corrupt("session directory parent missing".into())
                })?;
            stores.release_installation(directory);
            catalog.store = stores.open_installation(&catalog.guard)?;
        }
        let streams = {
            let (backend, _) = catalog.guard.active()?;
            let directory =
                catalog.guard.directory().parent().ok_or_else(|| {
                    AppendError::Corrupt("session directory parent missing".into())
                })?;
            installation::streams(directory, backend)?
        };
        let (backend, _) = catalog.guard.active()?;
        index::synchronize(
            catalog
                .guard
                .directory()
                .parent()
                .ok_or_else(|| AppendError::Corrupt("session directory parent missing".into()))?,
            backend,
            catalog.store.as_ref(),
            &streams,
            |stream| catalog.query().index_record(stream),
        )?;
        Ok(Some(catalog))
    }

    pub fn store(&self) -> &dyn SessionEventStore {
        self.store.as_ref()
    }

    pub fn query(&self) -> SessionQuery<'_> {
        SessionQuery { catalog: self }
    }

    pub fn commands(&self) -> SessionCommand<'_> {
        SessionCommand { catalog: self }
    }

    fn refresh_index(&self) -> Result<(), AppendError> {
        let (backend, _) = self.guard.active()?;
        let directory = self
            .guard
            .directory()
            .parent()
            .ok_or_else(|| AppendError::Corrupt("session directory parent missing".into()))?;
        let streams = installation::streams(directory, backend)?;
        index::synchronize(
            directory,
            backend,
            self.store.as_ref(),
            &streams,
            |stream| self.query().index_record(stream),
        )
    }

    fn cleanup_temporary(&self) -> Result<bool, AppendError> {
        let directory = self
            .guard
            .directory()
            .parent()
            .ok_or_else(|| AppendError::Corrupt("session directory parent missing".into()))?;
        let (backend, _) = self.guard.active()?;
        let mut discarded = false;
        for stream in super::temporary::candidates(directory, backend)? {
            let Some(record) = self.query().summary(&stream).map_err(upgrade::storage)? else {
                continue;
            };
            if record
                .fields
                .get("session_kind")
                .and_then(serde_json::Value::as_str)
                != Some("temporary")
            {
                continue;
            }
            let live = record
                .fields
                .get("temporary_owner_id")
                .and_then(serde_json::Value::as_str)
                .map(|owner| super::temporary::owner_is_live(directory, owner))
                .transpose()?
                .unwrap_or(false);
            if !live || record.closed {
                super::temporary::discard(directory, backend, &stream)?;
                discarded = true;
            }
        }
        Ok(discarded)
    }
}

impl SessionQuery<'_> {
    pub(crate) fn index_record(
        &self,
        stream: &SessionId,
    ) -> Result<Option<SessionIndexRecord>, ReadError> {
        if self.temporary_root(stream)?.is_some() {
            return Ok(None);
        }
        self.index_record_at(stream, None, None, &mut std::collections::HashSet::new())
    }

    /// A child's lifetime follows its canonical parent request. Protocol
    /// metadata and user-provided parent labels cannot extend that lifetime.
    fn temporary_root(&self, stream: &SessionId) -> Result<Option<SessionId>, ReadError> {
        let mut current = stream.clone();
        let mut visited = std::collections::HashSet::new();
        loop {
            if !visited.insert(current.clone()) {
                return Err(ReadError::Corrupt("cyclic session derivation".into()));
            }
            let Some(detail) = self.project_at(&current, false, None, None)? else {
                return Ok(None);
            };
            if detail
                .fields
                .get("session_kind")
                .and_then(serde_json::Value::as_str)
                == Some("temporary")
            {
                return Ok(Some(current));
            }
            let root = self
                .catalog
                .store
                .read_page(&current, &EventCursor::beginning(current.clone()), None, 1)?
                .events
                .into_iter()
                .next()
                .ok_or_else(|| ReadError::Gap("session root missing".into()))?;
            if !matches!(root.body.data, EventData::DerivedSessionStarted { .. }) {
                return Ok(None);
            }
            let request = self
                .catalog
                .store
                .read_event(
                    root.causation_id.as_ref().ok_or_else(|| {
                        ReadError::Corrupt("derived session cause missing".into())
                    })?,
                )?
                .ok_or_else(|| ReadError::Gap("derived session parent request missing".into()))?;
            current = request.session_id;
        }
    }

    fn index_record_at(
        &self,
        stream: &SessionId,
        through: Option<&EventCursor>,
        branch: Option<&BranchId>,
        visited: &mut std::collections::HashSet<SessionId>,
    ) -> Result<Option<SessionIndexRecord>, ReadError> {
        if !visited.insert(stream.clone()) {
            return Err(ReadError::Corrupt("copy source cycle".into()));
        }
        let Some(detail) = self.project_at(stream, false, through, branch)? else {
            return Ok(None);
        };
        let session = &detail.session;
        let mut first_user_message_id = None;
        let mut first_user_preview = None;
        let mut parent_session_id = None;
        let mut parent_agent = None;
        let mut derived_child = false;
        let mut inherited_message_count = 0;
        if let Some(value) = session.fields.get("parent_session_id") {
            parent_session_id = serde_json::from_value(value.clone()).ok();
        }
        let mut indexed_cwd = None;
        let mut indexed_provider = None;
        let mut indexed_model = None;
        let head = through.cloned().unwrap_or(self.catalog.store.head(stream)?);
        let lineage = branch
            .map(|branch| self.branch_lineage(stream, &head, branch))
            .transpose()?;
        let mut cursor = EventCursor::beginning(stream.clone());
        while cursor.sequence < head.sequence {
            let page = self
                .catalog
                .store
                .read_page(stream, &cursor, Some(&head), 256)?;
            for event in &page.events {
                if lineage.as_ref().is_some_and(|lineage| {
                    event.branch_id.is_some()
                        && !zeroclaw_api::session_events::lineage::includes(lineage, event)
                }) {
                    continue;
                }
                match &event.body.data {
                    EventData::ContextProjectionCreated {
                        reason_key,
                        result_ref,
                        ..
                    } if reason_key == "copy_on_write_seed" => {
                        let body = self.metadata_value(result_ref)?;
                        let source =
                            zeroclaw_api::session_events::context_read::copy_source(&body)?
                                .ok_or_else(|| {
                                    ReadError::Corrupt("copy source coordinate missing".into())
                                })?;
                        parent_session_id = Some(source.cursor.session_id.clone());
                        if let Some(source) = self.index_record_at(
                            &source.cursor.session_id,
                            Some(&source.cursor),
                            Some(&source.branch_id),
                            visited,
                        )? {
                            inherited_message_count = source.message_count;
                            if first_user_message_id.is_none() {
                                first_user_message_id = source.first_user_message_id;
                                first_user_preview = source.first_user_preview;
                            }
                        }
                    }
                    EventData::UserContentRecorded { content_ref, .. }
                        if first_user_message_id.is_none() =>
                    {
                        if let PayloadRead::Present(payload) =
                            self.catalog.store.read_payload(&content_ref.payload_id)?
                        {
                            let content: zeroclaw_api::session_events::ContentPayload =
                                serde_json::from_slice(&payload.body)
                                    .map_err(|error| ReadError::Corrupt(error.to_string()))?;
                            first_user_message_id = Some(content.message_id.clone());
                            first_user_preview = Some(self.preview_content(&content)?);
                        }
                    }
                    EventData::ContextProjectionCreated {
                        reason_key,
                        result_ref,
                        ..
                    } if first_user_message_id.is_none()
                        && (reason_key == "host_seed" || reason_key == "legacy_import") =>
                    {
                        let body = self.metadata_value(result_ref)?;
                        let rows = body
                            .get(if reason_key == "host_seed" {
                                "seed_messages"
                            } else {
                                "legacy_messages"
                            })
                            .and_then(serde_json::Value::as_array)
                            .ok_or_else(|| ReadError::Corrupt("seed identities missing".into()))?;
                        if let Some(user) = rows.iter().find(|row| row["role"] == "user") {
                            if reason_key == "legacy_import" {
                                let reference: StoredPayloadRef =
                                    serde_json::from_value(user["content_ref"].clone())
                                        .map_err(|error| ReadError::Corrupt(error.to_string()))?;
                                let content: zeroclaw_api::session_events::ContentPayload =
                                    serde_json::from_slice(
                                        &zeroclaw_api::session_events::context_read::present(
                                            self.catalog.store(),
                                            &reference,
                                        )?
                                        .body,
                                    )
                                    .map_err(|error| ReadError::Corrupt(error.to_string()))?;
                                first_user_message_id = Some(content.message_id.clone());
                                first_user_preview = Some(self.preview_content(&content)?);
                            } else {
                                let reference = zeroclaw_api::session_events::MessageRef {
                                    message_id: serde_json::from_value(user["message_id"].clone())
                                        .map_err(|error| ReadError::Corrupt(error.to_string()))?,
                                    source: zeroclaw_api::session_events::ContextMessageSource::Conversation {
                                        payload_ref: serde_json::from_value(body["messages_ref"].clone())
                                            .map_err(|error| ReadError::Corrupt(error.to_string()))?,
                                        item_index: serde_json::from_value(user["source_index"].clone())
                                            .map_err(|error| ReadError::Corrupt(error.to_string()))?,
                                    },
                                };
                                first_user_message_id = Some(reference.message_id.clone());
                                match zeroclaw_api::session_events::context_read::resolve_message(
                                    self.catalog.store(),
                                    stream,
                                    &reference,
                                ) {
                                    Ok(Some(
                                        zeroclaw_api::model_provider::ConversationMessage::Chat(
                                            chat,
                                        ),
                                    )) => {
                                        first_user_preview = Some(bounded_preview(&chat.content));
                                    }
                                    // Erasure retains the seed's stable message identity.
                                    Err(ReadError::OriginalPayloadErased(_)) | Ok(None) => {}
                                    Ok(Some(_)) => {
                                        return Err(ReadError::Corrupt(
                                            "user seed is not a chat message".into(),
                                        ));
                                    }
                                    Err(error) => return Err(error),
                                }
                            }
                        }
                    }
                    EventData::TurnAccepted { ingress_ref, .. } => {
                        if let PayloadRead::Present(payload) =
                            self.catalog.store.read_payload(&ingress_ref.payload_id)?
                        {
                            let value: serde_json::Value = serde_json::from_slice(&payload.body)
                                .map_err(|error| ReadError::Corrupt(error.to_string()))?;
                            indexed_cwd = value
                                .get("execution_workspace")
                                .or_else(|| value.get("cwd"))
                                .and_then(serde_json::Value::as_str)
                                .map(str::to_owned);
                        }
                    }
                    EventData::ProviderRequestPrepared {
                        route_attribution_ref,
                        ..
                    } => {
                        if let PayloadRead::Present(payload) = self
                            .catalog
                            .store
                            .read_payload(&route_attribution_ref.payload_id)?
                        {
                            let value: serde_json::Value = serde_json::from_slice(&payload.body)
                                .map_err(|error| ReadError::Corrupt(error.to_string()))?;
                            let dispatch = value.get("actual_dispatch").unwrap_or(&value);
                            indexed_provider = dispatch
                                .get("provider_ref")
                                .or_else(|| dispatch.get("provider"))
                                .and_then(serde_json::Value::as_str)
                                .map(str::to_owned);
                            indexed_model = dispatch
                                .get("model")
                                .and_then(serde_json::Value::as_str)
                                .map(str::to_owned);
                        }
                    }
                    EventData::DerivedSessionStarted { .. } => {
                        derived_child = true;
                        if let Some(causation) = &event.causation_id
                            && let Some(parent) = self.catalog.store.read_event(causation)?
                        {
                            parent_session_id = Some(parent.session_id.clone());
                            if let Some(parent_session) = self.project(&parent.session_id, false)? {
                                parent_agent = parent_session.owning_agent.clone();
                            }
                        }
                    }
                    _ => {}
                }
            }
            if let Some(last) = page.events.last() {
                cursor.sequence = last.sequence;
            }
            if !page.has_more {
                break;
            }
        }
        let mut metadata = residual_metadata(&session.fields);
        if derived_child {
            metadata["session_kind"] = serde_json::json!("child_agent");
        }
        Ok(Some(SessionIndexRecord {
            session_id: stream.clone(),
            creation_source: session.creation_source().map(str::to_owned),
            title: session
                .fields
                .get("title")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned),
            created_at: session.created_at,
            last_activity: session.last_activity,
            first_user_message_id,
            first_user_preview,
            cwd: indexed_cwd.or_else(|| {
                string_field(
                    &session.fields,
                    &["cwd", "workspace_dir", "working_directory"],
                )
            }),
            git_remote: string_field(&session.fields, &["git_remote", "git.remote"]),
            git_branch: string_field(&session.fields, &["git_branch", "git.branch"]),
            git_commit: string_field(&session.fields, &["git_commit", "git.commit"]),
            provider: indexed_provider.or_else(|| {
                string_field(
                    &session.fields,
                    &["provider", "provider_ref", "model_provider"],
                )
            }),
            model: indexed_model
                .or_else(|| string_field(&session.fields, &["model", "model_name"])),
            category: string_field(&session.fields, &["category", "session_category"]),
            project: string_field(&session.fields, &["project", "project_id"]),
            parent_session_id,
            owning_agent: session
                .owning_agent
                .clone()
                .or_else(|| parent_agent.clone()),
            parent_agent,
            closed: session.closed,
            deleted: session.deleted,
            head_sequence: session.head.sequence,
            message_count: session
                .message_count
                .saturating_add(inherited_message_count),
            running: session.running,
            metadata,
        }))
    }

    /// Read the rebuildable discovery projection without replaying every
    /// session. Full history remains available through `detail`/`summary`.
    pub fn index_records(&self) -> Result<Vec<SessionIndexRecord>, ReadError> {
        self.catalog
            .refresh_index()
            .map_err(|error| ReadError::StorageUnavailable(error.to_string()))?;
        let (backend, _) = self
            .catalog
            .guard
            .active()
            .map_err(|error| ReadError::StorageUnavailable(error.to_string()))?;
        index::read_records(
            self.catalog
                .guard
                .directory()
                .parent()
                .ok_or_else(|| ReadError::Corrupt("session directory parent missing".into()))?,
            backend,
        )
    }

    /// Build the lightweight catalog summaries directly from the durable
    /// discovery index. Full event replay remains reserved for detail and
    /// restore operations.
    pub fn indexed_summaries(
        &self,
    ) -> Result<Vec<(String, zeroclaw_api::session_events::catalog::Session)>, ReadError> {
        let mut result = Vec::new();
        for record in self.index_records()? {
            let key = record.session_id.as_str().to_owned();
            let mut fields = match record.metadata {
                serde_json::Value::Object(values) => values.into_iter().collect(),
                _ => BTreeMap::new(),
            };
            if let Some(source) = &record.creation_source {
                fields.insert(
                    "creation_source".into(),
                    serde_json::Value::String(source.clone()),
                );
            }
            if let Some(title) = &record.title {
                fields.insert("title".into(), serde_json::Value::String(title.clone()));
            }
            if let Some(cwd) = &record.cwd {
                fields.insert(
                    "workspace_dir".into(),
                    serde_json::Value::String(cwd.clone()),
                );
            }
            for (key, value) in [
                ("git_remote", record.git_remote),
                ("git_branch", record.git_branch),
                ("git_commit", record.git_commit),
                ("provider", record.provider),
                ("model", record.model),
                ("category", record.category),
                ("project", record.project),
            ] {
                if let Some(value) = value {
                    fields.insert(key.into(), serde_json::Value::String(value));
                }
            }
            result.push((
                key,
                zeroclaw_api::session_events::catalog::Session {
                    owning_agent: record.owning_agent,
                    fields,
                    closed: record.closed,
                    deleted: record.deleted,
                    created_at: record.created_at,
                    last_activity: record.last_activity,
                    head: EventCursor {
                        session_id: record.session_id,
                        sequence: record.head_sequence,
                    },
                    message_count: record.message_count,
                    running: record.running,
                },
            ));
        }
        Ok(result)
    }

    fn preview_content(
        &self,
        content: &zeroclaw_api::session_events::ContentPayload,
    ) -> Result<String, ReadError> {
        let mut preview = String::new();
        for part in &content.parts {
            if let zeroclaw_api::session_events::RecordedContentPart::Text { text_ref } = part
                && let PayloadRead::Present(payload) =
                    self.catalog.store.read_payload(&text_ref.payload_id)?
            {
                preview.push_str(
                    std::str::from_utf8(&payload.body)
                        .map_err(|error| ReadError::Corrupt(error.to_string()))?,
                );
            }
        }
        Ok(bounded_preview(&preview))
    }

    pub fn detail(&self, stream: &SessionId) -> Result<Option<SessionDetail>, ReadError> {
        self.project(stream, true)
    }

    pub fn summary(&self, stream: &SessionId) -> Result<Option<Session>, ReadError> {
        Ok(self.project(stream, false)?.map(|detail| detail.session))
    }

    fn project(
        &self,
        stream: &SessionId,
        include_context: bool,
    ) -> Result<Option<SessionDetail>, ReadError> {
        let Some(mut detail) = self.project_at(stream, include_context, None, None)? else {
            return Ok(None);
        };
        if detail.session.owning_agent.is_none() {
            let mut current = stream.clone();
            let mut visited = std::collections::HashSet::new();
            loop {
                if !visited.insert(current.clone()) {
                    return Err(ReadError::Corrupt("cyclic session derivation".into()));
                }
                let root = self
                    .catalog
                    .store
                    .read_page(&current, &EventCursor::beginning(current.clone()), None, 1)?
                    .events
                    .into_iter()
                    .next()
                    .ok_or_else(|| ReadError::Gap("derived session root missing".into()))?;
                if !matches!(root.body.data, EventData::DerivedSessionStarted { .. }) {
                    return Err(ReadError::Corrupt("session owning agent missing".into()));
                }
                let request = self
                    .catalog
                    .store
                    .read_event(root.causation_id.as_ref().ok_or_else(|| {
                        ReadError::Corrupt("derived session cause missing".into())
                    })?)?
                    .ok_or_else(|| {
                        ReadError::Gap("derived session parent request missing".into())
                    })?;
                if !matches!(
                    request.body.data,
                    EventData::ChildInvocationRequested { .. }
                ) {
                    return Err(ReadError::Corrupt(
                        "derived session parent kind invalid".into(),
                    ));
                }
                current = request.session_id;
                let parent = self
                    .project_at(&current, false, None, None)?
                    .ok_or_else(|| ReadError::Gap("derived session parent missing".into()))?;
                if let Some(agent) = parent.session.owning_agent {
                    detail.session.owning_agent = Some(agent);
                    if !detail.session.fields.contains_key("principal_id")
                        && let Some(principal) = parent.session.fields.get("principal_id")
                    {
                        detail
                            .session
                            .fields
                            .insert("principal_id".into(), principal.clone());
                    }
                    break;
                }
            }
        }
        Ok(Some(detail))
    }

    fn project_at(
        &self,
        stream: &SessionId,
        include_context: bool,
        through: Option<&EventCursor>,
        branch: Option<&BranchId>,
    ) -> Result<Option<SessionDetail>, ReadError> {
        let actual = self.catalog.store.head(stream)?;
        let head = through.cloned().unwrap_or(actual.clone());
        if head.session_id != *stream || head.sequence > actual.sequence {
            return Err(ReadError::Gap(
                "session projection prefix unavailable".into(),
            ));
        }
        if head.sequence == 0 {
            return Ok(None);
        }
        let lineage = branch
            .map(|branch| self.branch_lineage(stream, &head, branch))
            .transpose()?;
        let mut reducer = SessionReducer::new(stream.clone(), include_context);
        let mut after = EventCursor::beginning(stream.clone());
        loop {
            let page = self
                .catalog
                .store
                .read_page(stream, &after, Some(&head), 256)?;
            for event in &page.events {
                let include = lineage.as_ref().is_none_or(|lineage| {
                    event.sequence == 1
                        || event.branch_id.is_none()
                        || zeroclaw_api::session_events::lineage::includes(lineage, event)
                });
                let seed_count = if include {
                    if let EventData::ContextProjectionCreated {
                        reason_key,
                        result_ref,
                        ..
                    } = &event.body.data
                        && (reason_key == "host_seed" || reason_key == "legacy_import")
                    {
                        let payload = zeroclaw_api::session_events::context_read::present(
                            self.catalog.store(),
                            result_ref,
                        )?;
                        let body: serde_json::Value = serde_json::from_slice(&payload.body)
                            .map_err(|error| ReadError::Corrupt(error.to_string()))?;
                        Some(
                            body.get(if reason_key == "host_seed" {
                                "seed_messages"
                            } else {
                                "legacy_messages"
                            })
                            .and_then(serde_json::Value::as_array)
                            .ok_or_else(|| ReadError::Corrupt("seed identities missing".into()))?
                            .len(),
                        )
                    } else {
                        None
                    }
                } else {
                    None
                };
                reducer.apply_scoped(event, seed_count, include)?;
                if include && let EventData::TurnAccepted { ingress_ref, .. } = &event.body.data {
                    let value = self.metadata_value(ingress_ref)?;
                    reducer.observe_ingress(&value)?;
                }
            }
            if !page.has_more {
                break;
            }
            after.sequence = page
                .events
                .last()
                .ok_or_else(|| ReadError::Gap("empty catalog page".into()))?
                .sequence;
        }
        let pending: Vec<_> = reducer
            .pending_metadata()
            .map(|(field, _, reference)| (field.to_owned(), reference.clone()))
            .collect();
        for (field, reference) in pending {
            let value = self.metadata_value(&reference)?;
            reducer.resolve_metadata(&field, value)?;
        }
        reducer.finish()
    }

    fn branch_lineage(
        &self,
        session_id: &SessionId,
        through: &EventCursor,
        branch_id: &BranchId,
    ) -> Result<BTreeMap<BranchId, u64>, ReadError> {
        let mut origins = Vec::new();
        let mut after = EventCursor::beginning(session_id.clone());
        loop {
            let page = self
                .catalog
                .store
                .read_page(session_id, &after, Some(through), 256)?;
            if let Some(last) = page.events.last() {
                after.sequence = last.sequence;
            }
            origins.extend(page.events.into_iter().filter(|event| {
                matches!(
                    event.body.data,
                    EventData::SessionStarted { .. }
                        | EventData::DerivedSessionStarted { .. }
                        | EventData::BranchCreated { .. }
                )
            }));
            if !page.has_more {
                break;
            }
        }
        zeroclaw_api::session_events::lineage::frozen_lineage(
            &origins,
            session_id,
            branch_id,
            through.sequence,
        )
    }

    fn metadata_value(&self, reference: &StoredPayloadRef) -> Result<serde_json::Value, ReadError> {
        let value = match self.catalog.store.read_payload(&reference.payload_id)? {
            PayloadRead::Present(value) if StoredPayloadRef::from(&value) == *reference => value,
            PayloadRead::Present(_) => {
                return Err(ReadError::Corrupt("metadata reference mismatch".into()));
            }
            PayloadRead::ErasurePending | PayloadRead::Redacted
                if zeroclaw_api::session_events::payloads::content_leaf(&reference.codec) =>
            {
                return Ok(serde_json::Value::Null);
            }
            _ => return Err(ReadError::Corrupt("structural metadata unavailable".into())),
        };
        if value.codec == "session.text/v1" {
            Ok(serde_json::Value::String(
                String::from_utf8(value.body)
                    .map_err(|error| ReadError::Corrupt(error.to_string()))?,
            ))
        } else {
            serde_json::from_slice(&value.body)
                .map_err(|error| ReadError::Corrupt(error.to_string()))
        }
    }
}

pub(crate) fn metadata_payload(
    field: &str,
    value: serde_json::Value,
) -> Result<PayloadRecord, AppendError> {
    let structural = matches!(
        field,
        "creation_source"
            | "principal_id"
            | "created_at"
            | "last_activity"
            | "owning_agent"
            | "latest_context_input_tokens"
            | "interaction_surface"
            | "session_kind"
            | "temporary_owner_id"
    );
    let codec = if structural {
        "session.authority/v1"
    } else {
        "session.bytes/v1"
    };
    Ok(PayloadRecord::json(codec, &value)?)
}

impl SessionCommand<'_> {
    /// Create a separately listed session whose initial context refers to a
    /// frozen point in another session. The event log remains independent;
    /// readers can resolve the source lazily and later writes stay local.
    pub fn create_copy(&self, request: SessionCopy<'_>) -> Result<(), AppendError> {
        self.copy(request, false)
    }

    pub fn create_temporary(&self, request: SessionCopy<'_>) -> Result<(), AppendError> {
        self.copy(request, true)
    }

    fn copy(&self, request: SessionCopy<'_>, temporary: bool) -> Result<(), AppendError> {
        let SessionCopy {
            session_id,
            source: coordinate,
            creation_source: source,
            owning_agent: alias,
        } = request;
        let source_cursor = &coordinate.cursor;
        let source_session_id = &coordinate.cursor.session_id;
        let source_branch_id = &coordinate.branch_id;
        if source_cursor.session_id != *source_session_id || source_cursor.sequence == 0 {
            return Err(AppendError::InvalidReference(
                "copy source cursor is invalid".into(),
            ));
        }
        let source_head = self
            .catalog
            .store
            .head(source_session_id)
            .map_err(upgrade::storage)?;
        if source_cursor.sequence > source_head.sequence {
            return Err(AppendError::InvalidReference(
                "copy source cursor is beyond the source head".into(),
            ));
        }
        let mut history = Vec::new();
        let mut after = EventCursor::beginning(source_session_id.clone());
        loop {
            let page = self
                .catalog
                .store
                .read_page(source_session_id, &after, Some(source_cursor), 256)
                .map_err(upgrade::storage)?;
            if let Some(last) = page.events.last() {
                after.sequence = last.sequence;
            }
            history.extend(page.events);
            if !page.has_more {
                break;
            }
        }
        let lineage = zeroclaw_api::session_events::lineage::frozen_lineage(
            &history,
            source_session_id,
            source_branch_id,
            source_cursor.sequence,
        )
        .map_err(upgrade::storage)?;
        if !history.iter().any(|event| {
            event.body.kind() == zeroclaw_api::session_events::EventKind::ContextProjectionCreated
                && zeroclaw_api::session_events::lineage::includes(&lineage, event)
        }) {
            return Err(AppendError::InvalidReference(
                "copy source context missing".into(),
            ));
        }
        let source_metadata = self
            .catalog
            .query()
            .project_at(
                source_session_id,
                false,
                Some(source_cursor),
                Some(source_branch_id),
            )
            .map_err(upgrade::storage)?
            .ok_or_else(|| AppendError::InvalidReference("copy source session missing".into()))?;
        let source_is_temporary = self
            .catalog
            .query()
            .temporary_root(source_session_id)
            .map_err(upgrade::storage)?
            .is_some();
        if source_is_temporary {
            return Err(AppendError::InvalidReference(
                "copy source must outlive the new session".into(),
            ));
        }
        let mut fields = source_metadata
            .fields
            .iter()
            .filter(|(key, _)| {
                matches!(
                    key.as_str(),
                    "principal_id"
                        | "workspace_dir"
                        | "cwd"
                        | "interaction_surface"
                        | "title"
                        | "git_remote"
                        | "git_branch"
                        | "git_commit"
                        | "provider"
                        | "model"
                        | "category"
                        | "project"
                        | "latest_context_input_tokens"
                )
            })
            .map(|(key, value)| (key.as_str(), value.clone()))
            .collect::<Vec<_>>();
        fields.push((
            "session_kind",
            serde_json::json!(if temporary { "temporary" } else { "copy" }),
        ));
        if temporary {
            let directory =
                self.catalog.guard.directory().parent().ok_or_else(|| {
                    AppendError::Corrupt("session directory parent missing".into())
                })?;
            let owner = super::temporary::retain_process(directory)?;
            fields.push(("temporary_owner_id", serde_json::json!(owner)));
        }
        let body = PayloadRecord::json(
            "session.context/v1",
            &serde_json::json!({
                "copy_source_session_id": source_session_id,
                "copy_source_sequence": source_cursor.sequence,
                "copy_source_branch_id": source_branch_id,
                "history_has_trim_breadcrumb": false
            }),
        )?;
        let mut context = SessionEvent::typed(
            session_id.clone(),
            EventData::ContextProjectionCreated {
                source_ranges: vec![SourceRange {
                    session_id: source_session_id.clone(),
                    from_seq: 1,
                    to_seq: source_cursor.sequence,
                    disposition: RangeDisposition::Included,
                }],
                summary_ref: None,
                budget: 0,
                reason_key: "copy_on_write_seed".into(),
                algorithm_version: "session-copy/v1".into(),
                result_ref: StoredPayloadRef::from(&body),
                summary_source_refs: vec![],
                transform_started_event_id: None,
            },
            EventActor::System {
                component: "session_catalog".into(),
            },
        )?;
        context.branch_id = Some(BranchId::parse(format!("branch-{session_id}"))?);
        self.create_with_context(session_id.as_str(), source, alias, fields, (context, body))
    }

    pub fn create(
        &self,
        family: &str,
        source: &str,
        alias: &str,
        fields: Vec<(&str, serde_json::Value)>,
    ) -> Result<(), AppendError> {
        let stream = SessionId::parse(family.to_owned())?;
        let body = PayloadRecord::json(
            "session.context/v1",
            &serde_json::json!({"message_refs":[],"history_has_trim_breadcrumb":false}),
        )?;
        let mut context = SessionEvent::typed(
            stream.clone(),
            EventData::ContextProjectionCreated {
                source_ranges: vec![],
                summary_ref: None,
                budget: 0,
                reason_key: "initial_context".into(),
                algorithm_version: "native-entrance/v1".into(),
                result_ref: StoredPayloadRef::from(&body),
                summary_source_refs: vec![],
                transform_started_event_id: None,
            },
            EventActor::default(),
        )?;
        context.branch_id = Some(BranchId::parse(format!("branch-{stream}"))?);
        self.create_with_context(family, source, alias, fields, (context, body))
    }

    fn create_with_context(
        &self,
        session_id: &str,
        source: &str,
        alias: &str,
        fields: Vec<(&str, serde_json::Value)>,
        context: (SessionEvent, PayloadRecord),
    ) -> Result<(), AppendError> {
        let stream = SessionId::parse(session_id.to_owned())?;
        if self
            .catalog
            .store
            .head(&stream)
            .map_err(upgrade::storage)?
            .sequence
            != 0
        {
            return Err(AppendError::InvalidTransition(
                "session already exists".into(),
            ));
        }
        let branch = BranchId::parse(format!("branch-{stream}"))?;
        let mut root = SessionEvent::typed(
            stream.clone(),
            EventData::SessionStarted {
                owning_agent: alias.into(),
                capture_mode: CaptureMode::Native,
            },
            EventActor::System {
                component: "session_catalog".into(),
            },
        )?;
        root.branch_id = Some(branch.clone());
        let (source, source_payload) = creation_source(stream.clone(), source)?;
        let (context, body) = context;
        let mut events = vec![root, source];
        let mut payloads = vec![source_payload, body];
        Self::metadata_events(&stream, fields, &mut events, &mut payloads)?;
        events.push(context);
        self.append(
            &stream,
            EventCursor::beginning(stream.clone()),
            events,
            payloads,
        )
    }

    pub fn change(
        &self,
        record: &Session,
        fields: Vec<(&str, serde_json::Value)>,
    ) -> Result<(), AppendError> {
        if fields
            .iter()
            .any(|(field, _)| matches!(*field, "session_kind" | "temporary_owner_id"))
        {
            return Err(AppendError::InvalidReference(
                "session lifetime is immutable".into(),
            ));
        }
        let mut events = Vec::new();
        let mut payloads = Vec::new();
        Self::metadata_events(record.stream(), fields, &mut events, &mut payloads)?;
        self.append(record.stream(), record.head.clone(), events, payloads)
    }

    fn metadata_events(
        stream: &SessionId,
        fields: Vec<(&str, serde_json::Value)>,
        events: &mut Vec<SessionEvent>,
        payloads: &mut Vec<PayloadRecord>,
    ) -> Result<(), AppendError> {
        for (field, value) in fields {
            if field == "creation_source" {
                return Err(AppendError::InvalidReference(
                    "session creation identity is immutable".into(),
                ));
            }
            let body = metadata_payload(field, value)?;
            events.push(SessionEvent::typed(
                stream.clone(),
                EventData::SessionMetadataChanged {
                    field: field.into(),
                    new_value_ref: StoredPayloadRef::from(&body),
                },
                EventActor::System {
                    component: "session_catalog".into(),
                },
            )?);
            payloads.push(body);
        }
        Ok(())
    }

    pub fn close(&self, record: &Session, reason: &str) -> Result<(), AppendError> {
        let event = SessionEvent::typed(
            record.stream().clone(),
            EventData::SessionClosed {
                reason_key: reason.into(),
            },
            EventActor::System {
                component: "session_catalog".into(),
            },
        )?;
        self.append(record.stream(), record.head.clone(), vec![event], vec![])
    }

    fn append(
        &self,
        stream: &SessionId,
        head: EventCursor,
        events: Vec<SessionEvent>,
        payloads: Vec<PayloadRecord>,
    ) -> Result<(), AppendError> {
        if events.is_empty() {
            return Ok(());
        }
        let closed = events
            .iter()
            .any(|event| matches!(event.body.data, EventData::SessionClosed { .. }));
        self.catalog.store.append_batch(AppendRequest {
            session_id: stream.clone(),
            expected_head: head,
            idempotency_key: format!("catalog:{}", EventId::new()),
            events,
            payloads,
            erasure_reference_commitment: None,
        })?;
        let (backend, _) = self.catalog.guard.active()?;
        let directory = self
            .catalog
            .guard
            .directory()
            .parent()
            .ok_or_else(|| AppendError::Corrupt("session directory parent missing".into()))?;
        if closed {
            super::temporary::discard_if_closed(
                directory,
                backend,
                self.catalog.store.as_ref(),
                stream,
            )?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session_events::installation::NativeBackend;
    use crate::session_events::scoped::EventStorePool;

    #[test]
    fn copy_and_temporary_sessions_use_independent_ids_and_index_metadata() {
        let directory = tempfile::tempdir().expect("directory");
        crate::session_events::upgrade::upgrade_installation(
            directory.path(),
            NativeBackend::Sqlite,
            |_| "fixture".into(),
        )
        .expect("installation");
        let pool = EventStorePool::default();
        let catalog = SessionCatalog::open(directory.path(), &pool)
            .expect("catalog")
            .expect("native catalog");
        let source_id = SessionId::parse("source-session").expect("source id");
        catalog
            .commands()
            .create("source-session", "rpc", "agent", vec![])
            .expect("source");
        let source = catalog
            .query()
            .summary(&source_id)
            .expect("source summary")
            .expect("source exists");
        let copied_id = SessionId::parse("copied-session").expect("copy id");
        assert!(
            catalog
                .commands()
                .create_copy(SessionCopy {
                    session_id: &copied_id,
                    source: &zeroclaw_api::session_events::context_read::CopySource {
                        cursor: source.head.clone(),
                        branch_id: BranchId::new(),
                    },
                    creation_source: "rpc",
                    owning_agent: "agent",
                })
                .is_err()
        );
        assert_eq!(
            catalog
                .store()
                .head(&copied_id)
                .expect("uncreated copy")
                .sequence,
            0
        );
        catalog
            .commands()
            .create_copy(SessionCopy {
                session_id: &copied_id,
                source: &zeroclaw_api::session_events::context_read::CopySource {
                    cursor: source.head.clone(),
                    branch_id: BranchId::parse(format!("branch-{source_id}")).expect("branch"),
                },
                creation_source: "rpc",
                owning_agent: "agent",
            })
            .expect("copy");
        let copied_history = catalog
            .store()
            .read_page(
                &copied_id,
                &EventCursor::beginning(copied_id.clone()),
                None,
                256,
            )
            .expect("copy events");
        assert_eq!(
            copied_history
                .events
                .iter()
                .filter(|event| {
                    event.body.kind()
                        == zeroclaw_api::session_events::EventKind::ContextProjectionCreated
                })
                .count(),
            1
        );
        let temporary_id = SessionId::parse("temporary-session").expect("temporary id");
        catalog
            .commands()
            .create_temporary(SessionCopy {
                session_id: &temporary_id,
                source: &zeroclaw_api::session_events::context_read::CopySource {
                    cursor: source.head.clone(),
                    branch_id: BranchId::parse(format!("branch-{source_id}")).expect("branch"),
                },
                creation_source: "rpc",
                owning_agent: "agent",
            })
            .expect("temporary");
        let rows = catalog.query().indexed_summaries().expect("index");
        assert_eq!(rows.len(), 2);
        let copied = rows
            .iter()
            .find(|(id, _)| id == copied_id.as_str())
            .expect("copy row");
        assert_eq!(
            copied.1.fields.get("session_kind").and_then(|v| v.as_str()),
            Some("copy")
        );
        assert!(!copied.1.fields.contains_key("copy_source_session_id"));
        assert!(!copied.1.fields.contains_key("copy_source_sequence"));
        assert!(!copied.1.fields.contains_key("copy_source_branch_id"));
        let index_rows = catalog.query().index_records().expect("index rows");
        assert_eq!(
            index_rows
                .iter()
                .find(|row| row.session_id == copied_id)
                .expect("copy index")
                .parent_session_id
                .as_ref(),
            Some(&source_id)
        );
        assert!(!rows.iter().any(|(id, _)| id == temporary_id.as_str()));
        let temporary = catalog
            .query()
            .summary(&temporary_id)
            .expect("temporary")
            .expect("exists");
        assert_eq!(
            temporary
                .fields
                .get("session_kind")
                .and_then(|v| v.as_str()),
            Some("temporary")
        );
        assert!(!temporary.closed);
    }
}
