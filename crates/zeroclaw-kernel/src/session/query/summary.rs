//! Disposable session-list metadata, derived from one fixed event prefix.
use super::*;
use zeroclaw_api::session_events::ModelResolution;

impl SessionQuery {
    pub async fn summary(&self, stream: SessionId) -> Result<SessionSummary, SessionError> {
        use zeroclaw_api::session_events::catalog::SessionReducer;
        let mut reducer = SessionReducer::new(stream.clone(), false);
        let mut after = EventCursor::beginning(stream.clone());
        let mut through = None;
        let mut has_copy_seed = false;
        loop {
            let page = self
                .read_page(stream.clone(), after, through.clone(), 256)
                .await?;
            through = Some(page.through.clone());
            for event in &page.events {
                has_copy_seed |= matches!(&event.body.data,
                    EventData::ContextProjectionCreated { reason_key, .. } if reason_key == "copy_on_write_seed");
                let seed_count = if let EventData::ContextProjectionCreated { reason_key, .. } =
                    &event.body.data
                    && (reason_key == "host_seed" || reason_key == "legacy_import")
                {
                    let PayloadRead::Present(record) = self
                        .read_payload(
                            stream.clone(),
                            PayloadSelector {
                                event_sequence: event.sequence,
                                reference_path: vec![0],
                            },
                        )
                        .await?
                    else {
                        return Err(ReadError::Corrupt("seed identities unavailable".into()).into());
                    };
                    let value: serde_json::Value = serde_json::from_slice(&record.body)
                        .map_err(|error| ReadError::Corrupt(error.to_string()))?;
                    Some(
                        value
                            .get(if reason_key == "host_seed" {
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
                };
                reducer.apply(event, seed_count)?;
                if let EventData::TurnAccepted { ingress_ref, .. } = &event.body.data {
                    let reference_index = event
                        .body
                        .data
                        .stored_refs()
                        .map_err(AppendError::from)?
                        .iter()
                        .position(|reference| reference == ingress_ref)
                        .ok_or_else(|| ReadError::Corrupt("ingress reference missing".into()))?;
                    let PayloadRead::Present(record) = self
                        .read_payload(
                            stream.clone(),
                            PayloadSelector {
                                event_sequence: event.sequence,
                                reference_path: vec![reference_index],
                            },
                        )
                        .await?
                    else {
                        return Err(ReadError::Corrupt("ingress unavailable".into()).into());
                    };
                    let value = serde_json::from_slice(&record.body)
                        .map_err(|error| ReadError::Corrupt(error.to_string()))?;
                    reducer.observe_ingress(&value)?;
                }
            }
            after = page
                .events
                .last()
                .map_or(page.after.clone(), |event| EventCursor {
                    session_id: stream.clone(),
                    sequence: event.sequence,
                });
            if !page.has_more {
                break;
            }
        }
        let pending: Vec<_> = reducer
            .pending_metadata()
            .map(|(field, sequence, reference)| (field.to_owned(), sequence, reference.clone()))
            .collect();
        for (field, sequence, reference) in pending {
            let value = match self
                .read_payload(
                    stream.clone(),
                    PayloadSelector {
                        event_sequence: sequence,
                        reference_path: vec![0],
                    },
                )
                .await?
            {
                PayloadRead::Present(record) if record.codec == "session.text/v1" => {
                    serde_json::Value::String(
                        String::from_utf8(record.body)
                            .map_err(|error| ReadError::Corrupt(error.to_string()))?,
                    )
                }
                PayloadRead::Present(record) => serde_json::from_slice(&record.body)
                    .map_err(|error| ReadError::Corrupt(error.to_string()))?,
                PayloadRead::ErasurePending | PayloadRead::Redacted
                    if zeroclaw_api::session_events::payloads::content_leaf(&reference.codec) =>
                {
                    serde_json::Value::Null
                }
                _ => {
                    return Err(ReadError::Corrupt("structural metadata unavailable".into()).into());
                }
            };
            reducer.resolve_metadata(&field, value)?;
        }
        self.policy
            .authorize_actor(&self.actor, &stream, SessionAccess::Read)?;
        let mut summary = reducer
            .finish()?
            .ok_or_else(|| ReadError::Gap("session root missing".into()))?
            .session;
        if has_copy_seed {
            summary.message_count = self
                .message_window(stream, through, None, Some(0))
                .await?
                .next_cursor
                .map_or(0, |cursor| cursor.before);
        }
        Ok(summary)
    }
}

impl SessionQuery {
    /// Rejected selections are observations, never the effective override.
    pub async fn model_selection(
        &self,
        mut stream: SessionId,
    ) -> Result<Option<zeroclaw_api::session_events::capture::ModelOverrideSelection>, SessionError>
    {
        let mut frozen = None;
        let mut selected_branch = self.branch.clone();
        let mut visited = std::collections::HashSet::new();
        loop {
            if !visited.insert(stream.clone()) {
                return Err(ReadError::Corrupt("copy source cycle".into()).into());
            }
            let mut after = EventCursor::beginning(stream.clone());
            let mut through = frozen.take();
            let mut history = Vec::new();
            loop {
                let page = self
                    .read_page(stream.clone(), after.clone(), through.clone(), 256)
                    .await?;
                through = Some(page.through.clone());
                if let Some(last) = page.events.last() {
                    after.sequence = last.sequence;
                }
                history.extend(page.events);
                if !page.has_more {
                    break;
                }
            }
            let branch = selected_branch
                .clone()
                .or_else(|| {
                    history
                        .iter()
                        .find(|event| {
                            matches!(
                                event.body.kind(),
                                EventKind::SessionStarted | EventKind::DerivedSessionStarted
                            )
                        })
                        .and_then(|event| event.branch_id.clone())
                })
                .ok_or_else(|| ReadError::Gap("model selection branch absent".into()))?;
            let lineage = zeroclaw_api::session_events::lineage::frozen_lineage(
                &history,
                &stream,
                &branch,
                after.sequence,
            )?;
            let applied = history
                .iter()
                .rev()
                .find(|event| {
                    zeroclaw_api::session_events::lineage::includes(&lineage, event)
                        && matches!(
                            event.body.data,
                            EventData::SessionModelOverrideChanged {
                                resolution: ModelResolution::Applied { .. }
                            }
                        )
                })
                .map(|event| {
                    event
                        .causation_id
                        .clone()
                        .ok_or_else(|| ReadError::Corrupt("model selection origin absent".into()))
                })
                .transpose()?;
            let Some(cause) = applied else {
                let seed = history.iter().rev().find(|event| {
                zeroclaw_api::session_events::lineage::includes(&lineage, event)
                    && matches!(&event.body.data, EventData::ContextProjectionCreated { reason_key, .. } if reason_key == "copy_on_write_seed")
            });
                if let Some(seed) = seed
                    && let EventData::ContextProjectionCreated { result_ref, .. } = &seed.body.data
                {
                    self.policy
                        .authorize_actor(&self.actor, &stream, SessionAccess::Read)?;
                    let store = self.coordinator.store.clone();
                    let reference = result_ref.clone();
                    let body = self
                        .coordinator
                        .blocking(move || {
                            let payload = zeroclaw_api::session_events::context_read::present(
                                store.as_ref(),
                                &reference,
                            )?;
                            let body = serde_json::from_slice(&payload.body)
                                .map_err(|error| ReadError::Corrupt(error.to_string()))?;
                            Ok(zeroclaw_api::session_events::context_read::copy_source(
                                &body,
                            )?)
                        })
                        .await?;
                    self.policy
                        .authorize_actor(&self.actor, &stream, SessionAccess::Read)?;
                    if let Some(source) = body {
                        stream = source.cursor.session_id.clone();
                        selected_branch = Some(source.branch_id);
                        frozen = Some(source.cursor);
                        continue;
                    }
                }
                return Ok(None);
            };
            self.policy
                .authorize_actor(&self.actor, &stream, SessionAccess::Read)?;
            let store = self.coordinator.store.clone();
            let control = self
                .coordinator
                .blocking(move || {
                    Ok(store
                        .read_event(&cause)?
                        .ok_or_else(|| ReadError::Gap("model selection input absent".into()))?)
                })
                .await?;
            self.policy
                .authorize_actor(&self.actor, &stream, SessionAccess::Read)?;
            if control.session_id != stream
                || control.body.kind() != EventKind::ControlInputRecorded
            {
                return Err(
                    ReadError::Corrupt("model selection input scope mismatch".into()).into(),
                );
            }
            let PayloadRead::Present(value) = self
                .read_payload(
                    stream,
                    PayloadSelector {
                        event_sequence: control.sequence,
                        reference_path: vec![0],
                    },
                )
                .await?
            else {
                return Err(SessionError::StaleTarget(
                    "model selection input erased".into(),
                ));
            };
            let value: serde_json::Value = serde_json::from_slice(&value.body)
                .map_err(|error| ReadError::Corrupt(error.to_string()))?;
            return Ok(Some(
                serde_json::from_value(
                    value
                        .get("selection")
                        .cloned()
                        .ok_or_else(|| ReadError::Corrupt("model selection missing".into()))?,
                )
                .map_err(|error| ReadError::Corrupt(error.to_string()))?,
            ));
        }
    }
}
