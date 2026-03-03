//! Terminal lifecycle helpers.
//!
//! With `panic = "abort"` in release builds, Drop-based cleanup is unreliable.
//! Panic hooks and signal handlers are used to restore terminal state.

use std::sync::Once;

use tokio_util::sync::CancellationToken;

fn restore_terminal_state() {
    let _ = ratatui::crossterm::terminal::disable_raw_mode();
    let _ = ratatui::crossterm::execute!(
        std::io::stdout(),
        ratatui::crossterm::terminal::LeaveAlternateScreen,
        ratatui::crossterm::cursor::Show
    );
}

/// Install a panic hook that restores terminal mode before abort/panic output.
pub fn install_panic_hook() {
    static PANIC_HOOK_ONCE: Once = Once::new();
    PANIC_HOOK_ONCE.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |panic_info| {
            restore_terminal_state();
            previous(panic_info);
        }));
    });
}

/// Install async signal handlers that restore terminal mode and cancel session work.
pub async fn install_signal_handlers(cancel: CancellationToken) {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};

        let mut sigterm = match signal(SignalKind::terminate()) {
            Ok(stream) => stream,
            Err(error) => {
                tracing::warn!("Failed to install SIGTERM handler: {error}");
                return;
            }
        };
        let mut sighup = match signal(SignalKind::hangup()) {
            Ok(stream) => stream,
            Err(error) => {
                tracing::warn!("Failed to install SIGHUP handler: {error}");
                return;
            }
        };

        tokio::spawn(async move {
            tokio::select! {
                _ = sigterm.recv() => {},
                _ = sighup.recv() => {},
            }
            restore_terminal_state();
            cancel.cancel();
        });
    }

    #[cfg(not(unix))]
    {
        let _ = cancel;
    }
}
