//! On-demand views over canonical Session projections.
use super::*;
use crate::session_events::catalog::{Session, SessionCatalog, SessionDetail};
use zeroclaw_api::session_events::{EventData, context_read};

pub(super) struct RepositoryView(pub SessionCatalog);

impl RepositoryView {
    pub fn record(&self, id: &str) -> Result<Option<Session>> {
        let stream = zeroclaw_api::session_events::SessionId::parse(id)?;
        Ok(self
            .0
            .query()
            .summary(&stream)?
            .filter(|record| !record.deleted))
    }

    pub fn detail(&self, id: &str) -> Result<Option<SessionDetail>> {
        let stream = zeroclaw_api::session_events::SessionId::parse(id)?;
        Ok(self
            .0
            .query()
            .detail(&stream)?
            .filter(|record| !record.deleted))
    }

    pub fn required(&self, id: &str) -> Result<Session> {
        self.record(id)?
            .ok_or_else(|| anyhow::Error::msg(format!("unknown session_uuid: {id}")))
    }

    pub fn create(
        &self,
        id: &str,
        creation_source: &str,
        alias: &str,
        fields: Vec<(&str, serde_json::Value)>,
    ) -> Result<i64> {
        let stream = zeroclaw_api::session_events::SessionId::parse(id)?;
        if let Some(record) = self.0.query().summary(&stream)? {
            anyhow::ensure!(!record.closed && !record.deleted, "session already exists");
            anyhow::ensure!(
                record.owning_agent.as_deref() == Some(alias),
                "session owning agent differs"
            );
            let owner: Option<String> = Self::value(&record, "principal_id", None)?;
            let expected: Option<String> = fields
                .iter()
                .find(|(key, _)| *key == "principal_id")
                .map(|(_, value)| serde_json::from_value(value.clone()))
                .transpose()?
                .flatten();
            anyhow::ensure!(owner == expected, "session principal differs");
            let changed = fields
                .into_iter()
                .filter(|(key, value)| record.fields.get(*key) != Some(value))
                .collect::<Vec<_>>();
            if !changed.is_empty() {
                self.0.commands().change(&record, changed)?;
            }
        } else {
            self.0
                .commands()
                .create(id, creation_source, alias, fields)?;
        }
        // The compatibility return value is the canonical root sequence,
        // rather than a new transport-specific row identifier.
        Ok(1)
    }

    pub fn value<T: serde::de::DeserializeOwned>(
        record: &Session,
        key: &str,
        default: T,
    ) -> Result<T> {
        Ok(record.field(key, default)?)
    }

    pub fn summaries(&self) -> Result<Vec<(String, Session)>> {
        let mut result = self
            .0
            .query()
            .indexed_summaries()?
            .into_iter()
            .filter(|(_, summary)| !summary.deleted)
            .collect::<Vec<_>>();
        result.sort_by(|left, right| {
            right
                .1
                .last_activity
                .cmp(&left.1.last_activity)
                .then_with(|| left.0.cmp(&right.0))
        });
        Ok(result)
    }

    pub fn data(&self, id: &str, record: &SessionDetail) -> Result<SessionTranscript> {
        let mut messages = Vec::new();
        let mut trim_breadcrumb = false;
        let mut context = record.context.clone();
        let mut session_id = record.stream().clone();
        let mut visited = std::collections::HashSet::from([session_id.clone()]);
        while let Some(event) = context.as_ref() {
            let EventData::ContextProjectionCreated { result_ref, .. } = &event.body.data else {
                anyhow::bail!("context source kind mismatch");
            };
            let body: serde_json::Value =
                serde_json::from_slice(&context_read::present(self.0.store(), result_ref)?.body)?;
            let Some(source) = context_read::copy_source(&body)? else {
                break;
            };
            anyhow::ensure!(
                visited.insert(source.cursor.session_id.clone()),
                "copy source cycle"
            );
            session_id = source.cursor.session_id.clone();
            context = Some(context_read::projection_at(self.0.store(), &source)?);
        }
        if let Some(context) = &context {
            for item in context_read::context_refs(self.0.store(), context)? {
                if let Some(message) =
                    context_read::resolve_message(self.0.store(), &session_id, &item)?
                {
                    messages.push(message);
                }
            }
            if let EventData::ContextProjectionCreated { result_ref, .. } = &context.body.data {
                let body: serde_json::Value = serde_json::from_slice(
                    &context_read::present(self.0.store(), result_ref)?.body,
                )?;
                trim_breadcrumb = body
                    .get("history_has_trim_breadcrumb")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false);
            }
        }
        Ok(SessionTranscript {
            session_uuid: id.into(),
            principal_id: Self::value(record, "principal_id", None)?,
            agent_alias: record
                .owning_agent
                .clone()
                .context("session owning agent missing")?,
            workspace_dir: Self::value(record, "workspace_dir", String::new())?,
            interaction_surface: Self::value(record, "interaction_surface", None)?,
            token_count: Self::value(record, "token_count", 0)?,
            created_at: chrono::DateTime::from_timestamp_millis(record.created_at)
                .context("invalid session created_at")?,
            last_activity: chrono::DateTime::from_timestamp_millis(record.last_activity)
                .context("invalid session last_activity")?,
            retained_context: None,
            messages,
            trim_breadcrumb,
        })
    }

    pub fn summary(id: String, record: Session) -> Result<SessionListEntry> {
        Ok(SessionListEntry {
            principal_id: Self::value(&record, "principal_id", None)?,
            workspace_dir: Self::value(&record, "workspace_dir", String::new())?,
            token_count: Self::value(&record, "token_count", 0)?,
            created_at: chrono::DateTime::from_timestamp_millis(record.created_at)
                .context("invalid session created_at")?,
            last_activity: chrono::DateTime::from_timestamp_millis(record.last_activity)
                .context("invalid session last_activity")?,
            session_uuid: id,
            agent_alias: record
                .owning_agent
                .context("session owning agent missing")?,
            message_count: record.message_count,
        })
    }

    pub fn change(&self, id: &str, fields: Vec<(&str, serde_json::Value)>) -> Result<()> {
        self.0.commands().change(&self.required(id)?, fields)?;
        Ok(())
    }

    pub fn delete(&self, id: &str, owner: Option<&str>) -> Result<bool> {
        let Some(record) = self.record(id)? else {
            return Ok(false);
        };
        let principal: Option<String> = Self::value(&record, "principal_id", None)?;
        if owner.is_some_and(|owner| principal.as_deref() != Some(owner)) {
            return Ok(false);
        }
        self.0.commands().close(&record, "session_deleted")?;
        Ok(true)
    }

    pub fn kill(&self, id: &str, owner: Option<&str>) -> Result<SessionKillTransition> {
        let Some(record) = self.record(id)? else {
            return Ok(SessionKillTransition::Missing);
        };
        let principal: Option<String> = Self::value(&record, "principal_id", None)?;
        if owner.is_some_and(|owner| principal.as_deref() != Some(owner)) {
            return Ok(SessionKillTransition::Missing);
        }
        if record.closed {
            return Ok(SessionKillTransition::AlreadyKilled);
        }
        self.0.commands().close(&record, "session_killed")?;
        Ok(SessionKillTransition::Marked)
    }
}
