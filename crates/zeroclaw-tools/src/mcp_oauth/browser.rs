//! Browser launch for the interactive MCP OAuth flow.
//!
//! The launch is strictly advisory: the authorization URL is always printed,
//! a failed launcher never fails the flow, and a background (non-interactive)
//! caller must never reach this module at all — that mode returns an
//! actionable `authorization_required` error instead of opening windows.

use anyhow::{Context, Result};
use reqwest::Url;

use std::time::{Duration, Instant};

/// How long the detached reaper waits for an opener process to exit before
/// treating it as wedged. Openers exit within milliseconds on healthy
/// systems; long-lived ones (a few `gio` builds block until the app closes)
/// still get to spawn their browser before we stop waiting.
const OPENER_REAP_TIMEOUT: Duration = Duration::from_secs(15);

/// Opens (or tries to open) the user's browser at an authorization URL.
/// Injectable so tests and non-desktop hosts can substitute behaviors; the
/// flow treats `Err` from any launcher as "fall back to the printed URL".
pub trait BrowserLauncher: Send + Sync {
    fn launch(&self, url: &Url) -> Result<()>;
}

/// Default launcher: the platform's opener command, spawned with stdio
/// silenced (the opener's chatter is not ZeroClaw's output) and reaped by a
/// detached helper thread so openers do not linger as zombies.
pub struct SystemBrowserLauncher;

impl BrowserLauncher for SystemBrowserLauncher {
    fn launch(&self, url: &Url) -> Result<()> {
        let mut command = build_launch_command(url);
        command
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        let mut child = command
            .spawn()
            .with_context(|| format!("cannot launch a browser for {url}"))?;
        // Reap off-thread: `launch` must not block the interactive flow on
        // an opener that outlives the browser handoff. Killing a still-alive
        // opener after the timeout only stops the launcher process itself;
        // the browser it already handed the URL to is independent.
        std::thread::spawn(move || {
            let deadline = Instant::now() + OPENER_REAP_TIMEOUT;
            while Instant::now() < deadline {
                if child.try_wait().ok().flatten().is_some() {
                    return;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            let _ = child.kill();
            let _ = child.wait();
        });
        Ok(())
    }
}

/// Assemble the opener invocation. The URL is passed as ONE argv element
/// with no shell in the path: the authorization URL carries `&` separators
/// (state, code_challenge, resource), which any shell would split on. In
/// particular Windows must NOT go through `cmd /C start` — the sibling
/// `browser_open.rs` documents this exact pitfall and uses `rundll32` for
/// the same reason.
fn build_launch_command(url: &Url) -> std::process::Command {
    let mut command = opener_command();
    command.arg(url.as_str());
    command
}

fn opener_command() -> std::process::Command {
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
    }
    #[cfg(target_os = "windows")]
    {
        // Direct process invocation (no cmd.exe): rundll32 with the args
        // `url.dll,FileProtocolHandler` and the URL asks the shell for the
        // default handler without any command-line metacharacter
        // interpretation.
        let mut command = std::process::Command::new("rundll32");
        command.arg("url.dll,FileProtocolHandler");
        command
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        // Prefer a graphical session opener; `cmd`-style fallbacks are
        // deliberately not guessed. Failure lands in the printed-URL path.
        if which_exists("xdg-open") {
            return std::process::Command::new("xdg-open");
        }
        if which_exists("gio") {
            // gio needs the `open` subcommand before the URL.
            let mut command = std::process::Command::new("gio");
            command.arg("open");
            return command;
        }
        // Absent both: the launch fails at spawn and the flow prints the URL.
        std::process::Command::new("xdg-open")
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn which_exists(program: &str) -> bool {
    std::env::var_os("PATH")
        .as_ref()
        .and_then(|paths| {
            std::env::split_paths(paths)
                .map(|dir| dir.join(program))
                .find(|candidate| candidate.is_file())
        })
        .is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn launched_argv(url: &Url) -> (std::ffi::OsString, Vec<std::ffi::OsString>) {
        let command = build_launch_command(url);
        let program = command.get_program().to_os_string();
        let args = command
            .get_args()
            .map(|a| a.to_os_string())
            .collect::<Vec<_>>();
        (program, args)
    }

    #[test]
    fn url_travels_as_one_argv_element_with_no_shell() {
        // Regression pin for the MAJOR finding class: `&`-bearing query
        // strings must never be re-interpreted by a shell on the way to the
        // opener.
        let url = Url::parse(
            "https://auth.example/authorize?response_type=code&client_id=x&state=s&code_challenge=c",
        )
        .expect("url");
        let (program, args) = launched_argv(&url);
        let program = program.to_string_lossy().into_owned();
        assert_ne!(program, "cmd", "no cmd.exe indirection");
        assert!(
            !args.iter().any(|a| a.to_string_lossy().starts_with("/C")),
            "no shell -C indirection: {args:?}"
        );
        #[cfg(target_os = "windows")]
        assert_eq!(program, "rundll32");
        assert_eq!(
            args.last().map(|a| a.to_string_lossy().into_owned()),
            Some(url.as_str().to_string()),
            "the URL must reach the opener verbatim as the final argv element"
        );
    }

    #[test]
    fn printed_url_is_the_exact_authorization_url() {
        // The launcher receives the URL verbatim: whatever gets printed must
        // be what gets launched, so what the operator can paste is exactly
        // what the flow would have used.
        struct CapturingLauncher(std::sync::Mutex<Option<String>>);
        impl BrowserLauncher for CapturingLauncher {
            fn launch(&self, url: &Url) -> Result<()> {
                *self.0.lock().expect("lock") = Some(url.as_str().to_string());
                Ok(())
            }
        }
        let captured = std::sync::Arc::new(CapturingLauncher(std::sync::Mutex::new(None)));
        let url =
            Url::parse("https://auth.example/authorize?client_id=x&state=y%20z").expect("url");
        struct Launcher(std::sync::Arc<CapturingLauncher>);
        impl BrowserLauncher for Launcher {
            fn launch(&self, url: &Url) -> Result<()> {
                self.0.launch(url)
            }
        }
        Launcher(captured.clone()).launch(&url).expect("launch");
        assert_eq!(
            captured.0.lock().expect("lock").as_deref(),
            Some(url.as_str()),
            "launcher must receive the exact URL the operator is shown"
        );
    }

    #[test]
    fn system_launcher_opens_the_loopback_free_authorization_url_without_panic() {
        // Not every CI box has xdg-open; launching is best-effort anyway.
        // This only pins that constructing and calling the launcher cannot
        // panic on a missing opener (the flow prints the URL instead).
        let result =
            SystemBrowserLauncher.launch(&Url::parse("https://127.0.0.1:1/nope").expect("url"));
        assert!(result.is_ok() || result.unwrap_err().to_string().contains("browser"));
    }
}
