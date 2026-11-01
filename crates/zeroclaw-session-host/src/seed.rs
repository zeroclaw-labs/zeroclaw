use super::*;

pub(super) struct FamilySeed<'a> {
    pub family: &'a str,
    pub alias: &'a str,
    pub actor: EventActor,
    pub context: ContextSnapshot,
}

impl FamilySeed<'_> {
    pub fn publish(
        self,
        store: &dyn SessionEventStore,
        stream: &SessionStreamId,
    ) -> anyhow::Result<()> {
        if store.head(stream)?.sequence != 0 {
            return Ok(());
        }
        let branch = BranchId::parse(format!("branch-{stream}"))?;
        let mut root = SessionEvent::typed(
            stream.clone(),
            EventData::SessionStarted {
                session_id: SessionId::parse(format!("session-{stream}"))?,
                owning_agent: self.alias.into(),
                capture_mode: CaptureMode::Native,
            },
            self.actor.clone(),
        )?;
        root.branch_id = Some(branch.clone());
        let messages = &self.context.messages;
        let conversation = PayloadRecord::json("session.conversation/v1", &messages)?;
        // The immutable seed creates IDs; later projections resolve its content
        // by source index instead of copying another transcript.
        let seeds: Vec<_> = messages.iter().enumerate().filter_map(|(source_index, message)| {
            match message {
                zeroclaw_api::model_provider::ConversationMessage::Chat(chat) if chat.role != "system" => {
                    Some(serde_json::json!({"source_index":source_index,"message_id":MessageId::new(),"role":chat.role}))
                }
                _ => None,
            }
        }).collect();
        let seeded = !seeds.is_empty();
        let context = PayloadRecord::json(
            "session.context/v1",
            &serde_json::json!({
                "messages_ref":StoredPayloadRef::from(&conversation),
                "seed_messages":seeds,
                "history_has_trim_breadcrumb":self.context.history_has_trim_breadcrumb,
            }),
        )?;
        let mut projection = SessionEvent::typed(
            stream.clone(),
            EventData::ContextProjectionCreated {
                source_ranges: vec![],
                summary_ref: None,
                budget: 0,
                reason_key: if seeded {
                    "host_seed"
                } else {
                    "initial_context"
                }
                .into(),
                algorithm_version: "native-entrance/v1".into(),
                result_ref: StoredPayloadRef::from(&context),
                summary_source_refs: vec![],
                transform_started_event_id: None,
            },
            self.actor.clone(),
        )?;
        projection.branch_id = Some(branch);
        let (identity, identity_payload) = family_identity(stream.clone(), self.family)?;
        let mut events = vec![root, identity];
        let mut payloads = vec![identity_payload];
        if seeded {
            let evidence = PayloadRecord::json(
                "session.evidence/v1",
                &serde_json::json!({"source_kind":"host_supplied_seed","source_message_count":messages.len()}),
            )?;
            events.push(SessionEvent::typed(
                stream.clone(),
                EventData::LegacyHistoryImported {
                    source_kind: "host_supplied_seed".into(),
                    import_receipt_ref: StoredPayloadRef::from(&evidence),
                    completeness: "context_only".into(),
                    known_gaps: vec!["prior_execution_not_observed".into()],
                },
                self.actor,
            )?);
            payloads.push(evidence);
        }
        events.push(projection);
        payloads.extend([conversation, context]);
        store.append_batch(AppendRequest {
            stream_id: stream.clone(),
            expected_head: EventCursor::beginning(stream.clone()),
            idempotency_key: format!("native-family-create:{stream}"),
            events,
            payloads,
            erasure_reference_commitment: None,
        })?;
        Ok(())
    }
}
