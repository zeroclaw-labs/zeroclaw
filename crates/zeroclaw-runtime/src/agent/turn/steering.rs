//! Mid-turn steering: non-blocking drain of caller-pushed messages between
//! loop iterations (and between wrapper rounds).

/// Drain any steering messages the caller pushed since the last round.
pub fn drain_steering_messages(
    steering_rx: &mut Option<&mut tokio::sync::mpsc::Receiver<String>>,
) -> Vec<String> {
    let Some(rx) = steering_rx.as_deref_mut() else {
        return Vec::new();
    };
    let mut messages = Vec::new();
    loop {
        match rx.try_recv() {
            Ok(message) => messages.push(message),
            Err(
                tokio::sync::mpsc::error::TryRecvError::Empty
                | tokio::sync::mpsc::error::TryRecvError::Disconnected,
            ) => break,
        }
    }
    messages
}
