//! Old transcript APIs exist only for explicit old-schema test fixtures.
use super::*;

impl SessionRepository {
    pub fn load_message_page(
        &self,
        session_uuid: &str,
        limit: usize,
        cursor: Option<&str>,
    ) -> Result<SessionMessagePage> {
        legacy_fixture!(self, load_message_page(session_uuid, limit, cursor));

        let _ = (session_uuid, limit, cursor);

        anyhow::bail!("legacy transcript operations require the session execution port")
    }

    pub fn append_turn(&self, session_uuid: &str, messages: &[ConversationMessage]) -> Result<()> {
        legacy_fixture!(self, append_turn(session_uuid, messages));

        let _ = (session_uuid, messages);

        anyhow::bail!("legacy transcript operations require the session execution port")
    }

    pub fn replace_messages(
        &self,
        session_uuid: &str,
        messages: &[ConversationMessage],
    ) -> Result<()> {
        legacy_fixture!(self, replace_messages(session_uuid, messages));

        let _ = (session_uuid, messages);

        anyhow::bail!("legacy transcript operations require the session execution port")
    }

    pub fn replace_messages_and_breadcrumb(
        &self,
        session_uuid: &str,
        messages: &[ConversationMessage],
        breadcrumb_present: bool,
    ) -> Result<()> {
        legacy_fixture!(
            self,
            replace_messages_and_breadcrumb(session_uuid, messages, breadcrumb_present,)
        );

        let _ = (session_uuid, messages, breadcrumb_present);

        anyhow::bail!("legacy transcript operations require the session execution port")
    }

    pub fn begin_turn_checkpoint(
        &self,
        session_uuid: &str,
        turn_id: &str,
        messages: &[ConversationMessage],
    ) -> Result<()> {
        legacy_fixture!(self, begin_turn_checkpoint(session_uuid, turn_id, messages));

        let _ = (session_uuid, turn_id, messages);

        anyhow::bail!("legacy transcript operations require the session execution port")
    }

    pub fn append_turn_checkpoint(
        &self,
        session_uuid: &str,
        turn_id: &str,
        messages: &[ConversationMessage],
    ) -> Result<i64> {
        legacy_fixture!(
            self,
            append_turn_checkpoint(session_uuid, turn_id, messages)
        );

        let _ = (session_uuid, turn_id, messages);

        anyhow::bail!("legacy transcript operations require the session execution port")
    }

    pub fn persist_retained_context_at_frontier(
        &self,
        session_uuid: &str,
        turn_id: &str,
        retained_messages: &[ConversationMessage],
        breadcrumb: bool,
        frontier: i64,
    ) -> Result<()> {
        legacy_fixture!(
            self,
            persist_retained_context_at_frontier(
                session_uuid,
                turn_id,
                retained_messages,
                breadcrumb,
                frontier,
            )
        );

        let _ = (
            session_uuid,
            turn_id,
            retained_messages,
            breadcrumb,
            frontier,
        );

        anyhow::bail!("legacy transcript operations require the session execution port")
    }

    pub fn persist_retained_context_seed(
        &self,
        session_uuid: &str,
        retained_messages: &[ConversationMessage],
        breadcrumb: bool,
    ) -> Result<()> {
        legacy_fixture!(
            self,
            persist_retained_context_seed(session_uuid, retained_messages, breadcrumb,)
        );

        let _ = (session_uuid, retained_messages, breadcrumb);

        anyhow::bail!("legacy transcript operations require the session execution port")
    }

    pub fn checkpoint_frontier(&self, session_uuid: &str, turn_id: &str) -> Result<i64> {
        legacy_fixture!(self, checkpoint_frontier(session_uuid, turn_id));

        let _ = (session_uuid, turn_id);

        anyhow::bail!("legacy transcript operations require the session execution port")
    }

    pub fn finalize_turn_checkpoint_with_context(
        &self,
        session_uuid: &str,
        turn_id: &str,
        terminal_messages: &[ConversationMessage],
        retained_messages: &[ConversationMessage],
        breadcrumb: bool,
    ) -> Result<()> {
        legacy_fixture!(
            self,
            finalize_turn_checkpoint_with_context(
                session_uuid,
                turn_id,
                terminal_messages,
                retained_messages,
                breadcrumb,
            )
        );

        let _ = (
            session_uuid,
            turn_id,
            terminal_messages,
            retained_messages,
            breadcrumb,
        );

        anyhow::bail!("legacy transcript operations require the session execution port")
    }

    pub fn finalize_turn_checkpoint_with_context_for_owner(
        &self,
        session_uuid: &str,
        turn_id: &str,
        terminal_messages: &[ConversationMessage],
        retained_messages: &[ConversationMessage],
        breadcrumb: bool,
        owner_principal_id: Option<&str>,
    ) -> Result<()> {
        legacy_fixture!(
            self,
            finalize_turn_checkpoint_with_context_for_owner(
                session_uuid,
                turn_id,
                terminal_messages,
                retained_messages,
                breadcrumb,
                owner_principal_id,
            )
        );

        let _ = (
            session_uuid,
            turn_id,
            terminal_messages,
            retained_messages,
            breadcrumb,
            owner_principal_id,
        );

        anyhow::bail!("legacy transcript operations require the session execution port")
    }

    pub fn discard_turn_checkpoint(&self, session_uuid: &str, turn_id: &str) -> Result<()> {
        legacy_fixture!(self, discard_turn_checkpoint(session_uuid, turn_id));

        let _ = (session_uuid, turn_id);

        anyhow::bail!("legacy transcript operations require the session execution port")
    }

    pub fn finalize_turn_checkpoint(
        &self,
        session_uuid: &str,
        turn_id: &str,
        messages: &[ConversationMessage],
    ) -> Result<()> {
        legacy_fixture!(
            self,
            finalize_turn_checkpoint(session_uuid, turn_id, messages)
        );

        let _ = (session_uuid, turn_id, messages);

        anyhow::bail!("legacy transcript operations require the session execution port")
    }

    pub fn recover_turn_checkpoint(
        &self,
        session_uuid: &str,
        interruption_marker: &str,
    ) -> Result<bool> {
        legacy_fixture!(
            self,
            recover_turn_checkpoint(session_uuid, interruption_marker)
        );

        let _ = (session_uuid, interruption_marker);

        anyhow::bail!("legacy transcript operations require the session execution port")
    }

    pub fn recover_turn_checkpoint_for_owner(
        &self,
        session_uuid: &str,
        interruption_marker: &str,
        owner_principal_id: Option<&str>,
    ) -> Result<bool> {
        legacy_fixture!(
            self,
            recover_turn_checkpoint_for_owner(
                session_uuid,
                interruption_marker,
                owner_principal_id,
            )
        );

        let _ = (session_uuid, interruption_marker, owner_principal_id);

        anyhow::bail!("legacy transcript operations require the session execution port")
    }

    pub fn set_trim_breadcrumb(&self, session_uuid: &str, present: bool) -> Result<()> {
        legacy_fixture!(self, set_trim_breadcrumb(session_uuid, present));

        let _ = (session_uuid, present);

        anyhow::bail!("legacy transcript operations require the session execution port")
    }
}
