//! Processes this app instance launched and still holds OS handles for.
//!
//! A process is owned only through the handle this instance's own spawn
//! returned (see [`crate::daemon::Supervisor`]): the supervisor's Unix process
//! group, or the Windows Job Object it was created in, reaches the daemon it
//! runs. Nothing is recorded across app runs, so a daemon left by an earlier
//! run, or one that was already listening when this instance started, is
//! external. It is never adopted, whatever its PID, executable path, or
//! anything it reports about itself, and quitting never signals it.
//!
//! A supervisor is recorded the moment it is spawned, under the same lock
//! Quit takes to seal the registry, and before it reports readiness. Quit
//! therefore holds every handle there is to hold, and stops each tree before
//! it returns, whether or not that tree had finished starting.

use crate::daemon::Supervisor;
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

/// How long quitting waits for an owned supervisor to stop its daemon before
/// forcing the tree down. The supervisor allows its daemon 10 seconds to stop
/// and then drains the daemon's output.
pub const QUIT_GRACE: Duration = Duration::from_secs(15);

/// Supervisor trees this app instance launched, ready or still starting.
#[derive(Debug, Default)]
pub struct OwnedProcesses {
    registry: Mutex<Registry>,
}

#[derive(Debug, Default)]
struct Registry {
    next_id: u64,
    /// Oldest first, each with the ID its launch uses to find it again.
    launched: Vec<(u64, Supervisor)>,
    /// Set when quitting starts; nothing is spawned after it.
    closing: bool,
}

/// The owned-process registry shared between startup and exit.
pub type SharedOwnedProcesses = Arc<OwnedProcesses>;

fn quitting() -> std::io::Error {
    std::io::Error::other("the app is quitting, so it does not start a daemon")
}

impl OwnedProcesses {
    fn registry(&self) -> MutexGuard<'_, Registry> {
        self.registry.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Whether this instance owns any process.
    pub fn is_empty(&self) -> bool {
        self.registry().launched.is_empty()
    }

    /// Launch the desktop supervisor for `binary` and keep it as owned. It is
    /// refused once quitting has started. A launch whose readiness fails is
    /// stopped and forgotten; one that Quit stopped while it was starting
    /// reports so.
    pub fn launch(&self, binary: &Path, port: u16) -> std::io::Result<()> {
        if self.registry().closing {
            return Err(quitting());
        }
        crate::daemon::ensure_desktop_supervisor_capability(binary)?;
        let (id, stdout) = {
            let mut registry = self.registry();
            if registry.closing {
                return Err(quitting());
            }
            let (child, stdout) = crate::daemon::spawn_supervisor(binary, port)?;
            let id = registry.next_id;
            registry.next_id += 1;
            registry.launched.push((id, child));
            (id, stdout)
        };
        let frame = crate::daemon::read_readiness(stdout);
        let ready = crate::daemon::validate_readiness_frame(frame, || self.peek_exit(id));
        let mut registry = self.registry();
        let Some(index) = registry.launched.iter().position(|(entry, _)| *entry == id) else {
            return Err(std::io::Error::other(
                "the app quit while the daemon was starting; it was stopped",
            ));
        };
        match ready {
            Ok(()) => Ok(()),
            Err(startup_error) => {
                // Stop it while still holding the registry: a Quit waiting
                // for the lock must not see it gone until it is stopped.
                let (_, mut child) = registry.launched.remove(index);
                let stopped = crate::daemon::terminate_supervisor_tree(
                    &mut child,
                    crate::daemon::STARTUP_CLEANUP_GRACE,
                );
                drop(registry);
                Err(crate::daemon::attach_cleanup_error(startup_error, stopped))
            }
        }
    }

    /// The exit status of a recorded supervisor, without reaping it on Unix.
    fn peek_exit(&self, id: u64) -> std::io::Result<Option<std::process::ExitStatus>> {
        let mut registry = self.registry();
        match registry.launched.iter_mut().find(|(entry, _)| *entry == id) {
            Some((_, child)) => crate::daemon::peek_supervisor_exit(child),
            None => Ok(None),
        }
    }

    /// Seal the registry and stop every owned tree through its handle, newest
    /// first, so a gateway launched after its core stops before the core.
    /// Trees still starting are stopped too, so nothing this instance launched
    /// is left running when this returns. Every handle is released either way;
    /// the errors are returned.
    ///
    /// Every stop of a recorded tree, here or after a failed readiness check,
    /// happens while the registry lock is held, so the registry is empty only
    /// once the trees taken from it have stopped.
    pub fn quit(&self, grace: Duration) -> Vec<std::io::Error> {
        let mut registry = self.registry();
        registry.closing = true;
        let launched = std::mem::take(&mut registry.launched);
        let errors = launched
            .into_iter()
            .rev()
            .filter_map(|(_, mut child)| {
                crate::daemon::terminate_supervisor_tree(&mut child, grace).err()
            })
            .collect();
        drop(registry);
        errors
    }
}

#[cfg(all(test, unix))]
impl OwnedProcesses {
    fn launched_ids(&self) -> Vec<u32> {
        self.registry()
            .launched
            .iter()
            .map(|(_, child)| child.id())
            .collect()
    }

    /// Take the handles out, as an app crash loses them.
    fn lose_handles(&self) -> Vec<Supervisor> {
        std::mem::take(&mut self.registry().launched)
            .into_iter()
            .map(|(_, child)| child)
            .collect()
    }
}

/// Whether an exit request should be declined so the app keeps running in the
/// tray. Closing the last window (no exit code) keeps the app, and the
/// processes it owns, running; an explicit exit such as tray Quit proceeds.
pub fn keep_running_on_exit_request(code: Option<i32>) -> bool {
    code.is_none()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closing_the_last_window_keeps_running_but_quit_exits() {
        assert!(keep_running_on_exit_request(None));
        assert!(!keep_running_on_exit_request(Some(0)));
    }

    #[test]
    fn a_new_registry_owns_nothing() {
        let owned = OwnedProcesses::default();
        assert!(owned.is_empty());
        assert!(owned.quit(Duration::from_millis(10)).is_empty());
    }

    #[cfg(unix)]
    mod process_trees {
        use super::super::*;
        use std::fs;
        use std::os::unix::fs::PermissionsExt;
        use std::path::PathBuf;
        use std::process::{Child, Command, Stdio};
        use std::time::Instant;

        /// Test fixtures stop quickly; the production grace is not needed.
        const TEST_GRACE: Duration = Duration::from_secs(5);

        struct Fixture {
            dir: PathBuf,
        }

        impl Fixture {
            fn new(label: &str) -> Self {
                let unique = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .expect("system clock should be after the Unix epoch")
                    .as_nanos();
                let dir = std::env::temp_dir()
                    .join(format!("zc-own-{label}-{}-{unique}", std::process::id()));
                fs::create_dir(&dir).expect("create fixture directory");
                Self { dir }
            }

            fn literal(path: &Path) -> String {
                path.to_string_lossy().replace('\'', "'\\''")
            }

            /// A supervisor that answers the capability probe, starts a
            /// descendant in its own process group, reports READY, and on
            /// SIGTERM logs `tag`, stops the descendant, and exits.
            fn supervisor(&self, tag: &str) -> PathBuf {
                let dir = Self::literal(&self.dir);
                self.script(
                    tag,
                    &format!(
                        "#!/bin/sh\n\
                         if [ \"${{1:-}}\" = service ] && [ \"${{2:-}}\" = run-desktop-daemon ] && [ \"${{3:-}}\" = --help ]; then exit 0; fi\n\
                         sleep 30 &\n\
                         child=$!\n\
                         printf '%s' \"$child\" > '{dir}/descendant.'\"$$\"\n\
                         trap 'printf \"%s\\n\" {tag} >> '\\''{dir}/stop-order'\\''; kill \"$child\" 2>/dev/null; wait \"$child\" 2>/dev/null; exit 0' TERM\n\
                         printf '%s\\n' READY\n\
                         wait \"$child\"\n"
                    ),
                )
            }

            /// A supervisor that reports READY and exits at once, leaving a
            /// SIGTERM-ignoring descendant in its process group. Started as
            /// `daemon` (the way a user runs it), it stays up instead.
            fn exiting_supervisor(&self, tag: &str) -> PathBuf {
                let dir = Self::literal(&self.dir);
                self.script(
                    tag,
                    &format!(
                        "#!/bin/sh\n\
                         if [ \"${{1:-}}\" = service ] && [ \"${{2:-}}\" = run-desktop-daemon ] && [ \"${{3:-}}\" = --help ]; then exit 0; fi\n\
                         if [ \"${{1:-}}\" = daemon ]; then\n\
                         sleep 30 &\n\
                         printf '%s' \"$!\" > '{dir}/descendant.'\"$$\"\n\
                         printf '%s\\n' READY\n\
                         wait\n\
                         exit 0\n\
                         fi\n\
                         trap '' TERM\n\
                         sleep 30 &\n\
                         printf '%s' \"$!\" > '{dir}/descendant.'\"$$\"\n\
                         printf '%s\\n' READY\n\
                         exit 0\n"
                    ),
                )
            }

            /// A supervisor that records its PID once started and reports
            /// READY only after [`Fixture::release`], so a test can act while
            /// the launch is in progress.
            fn gated_supervisor(&self, tag: &str) -> PathBuf {
                let dir = Self::literal(&self.dir);
                self.script(
                    tag,
                    &format!(
                        "#!/bin/sh\n\
                         if [ \"${{1:-}}\" = service ] && [ \"${{2:-}}\" = run-desktop-daemon ] && [ \"${{3:-}}\" = --help ]; then exit 0; fi\n\
                         trap 'printf \"%s\\n\" {tag} >> '\\''{dir}/stop-order'\\''; exit 0' TERM\n\
                         printf '%s' \"$$\" > '{dir}/started'\n\
                         while [ ! -f '{dir}/release' ]; do sleep 0.05; done\n\
                         printf '%s\\n' READY\n\
                         while :; do sleep 1; done\n"
                    ),
                )
            }

            /// A supervisor that reports an invalid readiness line, so the
            /// launch stops it, and that ignores SIGTERM (recording that it
            /// arrived) until [`Fixture::stop`], so that stop takes the whole
            /// startup cleanup grace and ends with SIGKILL.
            fn slow_failing_supervisor(&self, tag: &str) -> PathBuf {
                let dir = Self::literal(&self.dir);
                self.script(
                    tag,
                    &format!(
                        "#!/bin/sh\n\
                         if [ \"${{1:-}}\" = service ] && [ \"${{2:-}}\" = run-desktop-daemon ] && [ \"${{3:-}}\" = --help ]; then exit 0; fi\n\
                         printf '%s' \"$$\" > '{dir}/started'\n\
                         term='{dir}/term-received'\n\
                         trap 'printf x > \"$term\"' TERM\n\
                         printf '%s\\n' INVALID\n\
                         while [ ! -f '{dir}/stop' ]; do sleep 0.1; done\n"
                    ),
                )
            }

            fn stop(&self) {
                let _ = fs::write(self.dir.join("stop"), b"");
            }

            fn release(&self) {
                fs::write(self.dir.join("release"), b"").expect("release the gated fixture");
            }

            /// A supervisor that answers the capability probe but reports an
            /// invalid readiness line, so the app cannot verify the launch.
            fn unverifiable_supervisor(&self, tag: &str) -> PathBuf {
                let dir = Self::literal(&self.dir);
                self.script(
                    tag,
                    &format!(
                        "#!/bin/sh\n\
                         if [ \"${{1:-}}\" = service ] && [ \"${{2:-}}\" = run-desktop-daemon ] && [ \"${{3:-}}\" = --help ]; then exit 0; fi\n\
                         sleep 30 &\n\
                         printf '%s' \"$!\" > '{dir}/descendant.'\"$$\"\n\
                         printf '%s\\n' INVALID\n\
                         wait\n"
                    ),
                )
            }

            fn script(&self, tag: &str, body: &str) -> PathBuf {
                let path = self.dir.join(tag);
                fs::write(&path, body).expect("write fixture script");
                fs::set_permissions(&path, fs::Permissions::from_mode(0o700))
                    .expect("make fixture executable");
                // The first exec of a new file can take seconds on a loaded
                // macOS host; run the capability probe once, untimed, so the
                // launch's timed probe is not measuring that.
                let probe = Command::new(&path)
                    .args(["service", "run-desktop-daemon", "--help"])
                    .status()
                    .expect("warm fixture script");
                assert!(probe.success(), "fixture must answer the capability probe");
                path
            }

            fn descendant_of(&self, supervisor_pid: u32) -> i32 {
                fs::read_to_string(self.dir.join(format!("descendant.{supervisor_pid}")))
                    .expect("fixture should record its descendant")
                    .parse()
                    .expect("descendant pid should be numeric")
            }

            fn stop_order(&self) -> String {
                fs::read_to_string(self.dir.join("stop-order")).unwrap_or_default()
            }

            /// Start `binary` the way a user starts an external daemon: not
            /// through the registry, in its own process group.
            fn start_external(&self, binary: &Path) -> Child {
                use std::os::unix::process::CommandExt;
                let mut child = Command::new(binary)
                    .args(["daemon"])
                    .stdin(Stdio::null())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::null())
                    .process_group(0)
                    .spawn()
                    .expect("start external daemon");
                let mut line = String::new();
                std::io::BufRead::read_line(
                    &mut std::io::BufReader::new(child.stdout.take().expect("stdout")),
                    &mut line,
                )
                .expect("read external readiness");
                assert_eq!(line.trim(), "READY");
                child
            }
        }

        impl Drop for Fixture {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.dir);
            }
        }

        fn alive(pid: i32) -> bool {
            // SAFETY: signal 0 only checks that `pid` exists.
            unsafe { libc::kill(pid, 0) == 0 }
        }

        fn wait_gone(pid: i32) -> bool {
            let deadline = Instant::now() + Duration::from_secs(3);
            while Instant::now() < deadline {
                if !alive(pid) {
                    return true;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            false
        }

        /// Stop a process tree the test started outside the registry.
        fn stop_external(mut child: Child) {
            let group = i32::try_from(child.id()).expect("pid fits in pid_t");
            // SAFETY: the test launched this process group and has not reaped
            // its leader, so the group ID still names only that tree.
            unsafe { libc::kill(-group, libc::SIGKILL) };
            let _ = child.wait();
        }

        #[test]
        fn quit_stops_the_tree_this_instance_launched() {
            let fixture = Fixture::new("owned");
            let binary = fixture.supervisor("core");
            let owned = OwnedProcesses::default();
            owned.launch(&binary, 0).expect("launch owned daemon");

            let supervisor = owned.launched_ids()[0];
            let descendant = fixture.descendant_of(supervisor);
            assert!(owned.quit(TEST_GRACE).is_empty());

            assert!(owned.is_empty());
            assert!(wait_gone(descendant), "owned descendant survived Quit");
            assert!(!alive(i32::try_from(supervisor).expect("pid")));
            assert_eq!(fixture.stop_order(), "core\n");
        }

        #[test]
        fn external_daemon_from_the_same_executable_survives_quit() {
            let fixture = Fixture::new("external");
            let binary = fixture.supervisor("core");
            let external = fixture.start_external(&binary);
            let external_descendant = fixture.descendant_of(external.id());
            let owned = OwnedProcesses::default();
            owned.launch(&binary, 0).expect("launch owned daemon");

            assert!(owned.quit(TEST_GRACE).is_empty());

            let external_pid = i32::try_from(external.id()).expect("pid");
            assert!(alive(external_pid), "Quit stopped an external daemon");
            assert!(alive(external_descendant));
            stop_external(external);
        }

        #[test]
        fn relaunched_app_owns_nothing_from_an_earlier_run() {
            let fixture = Fixture::new("relaunch");
            let binary = fixture.supervisor("core");
            // The earlier run launched a daemon and ended without Quit (a
            // crash): its handle is gone, the tree keeps running.
            let earlier_run = OwnedProcesses::default();
            earlier_run
                .launch(&binary, 0)
                .expect("launch in earlier run");
            let leftover = earlier_run
                .lose_handles()
                .pop()
                .expect("earlier run launched one");
            let leftover_pid = i32::try_from(leftover.id()).expect("pid");

            // The relaunched instance starts with an empty registry and never
            // adopts the leftover, even though it runs the same executable.
            let relaunched = OwnedProcesses::default();
            assert!(relaunched.is_empty());
            assert!(relaunched.quit(TEST_GRACE).is_empty());
            assert!(
                alive(leftover_pid),
                "a relaunch stopped an earlier run's daemon"
            );
            stop_external(leftover);
        }

        #[test]
        fn exited_owned_supervisor_keeps_its_pid_reserved_until_quit() {
            let fixture = Fixture::new("reserved");
            let binary = fixture.exiting_supervisor("core");
            let owned = OwnedProcesses::default();
            owned.launch(&binary, 0).expect("launch owned daemon");
            let supervisor = owned.launched_ids()[0];
            let supervisor_pid = i32::try_from(supervisor).expect("pid");
            let descendant = fixture.descendant_of(supervisor);

            let deadline = Instant::now() + Duration::from_secs(3);
            while !crate::daemon::supervisor_exited(supervisor).expect("peek supervisor") {
                assert!(Instant::now() < deadline, "fixture supervisor did not exit");
                std::thread::sleep(Duration::from_millis(20));
            }
            // The exited supervisor stays unreaped, so the OS cannot hand its
            // PID, or its process group ID, to another process before Quit.
            assert!(alive(supervisor_pid), "exited supervisor was reaped early");
            let external = fixture.start_external(&binary);
            let external_descendant = fixture.descendant_of(external.id());

            assert!(owned.quit(TEST_GRACE).is_empty());
            assert!(
                wait_gone(descendant),
                "SIGTERM-ignoring descendant of the owned group survived Quit"
            );
            assert!(
                alive(external_descendant),
                "Quit reached an external daemon from the same executable"
            );
            stop_external(external);
        }

        #[test]
        fn unverifiable_launch_is_never_owned() {
            let fixture = Fixture::new("unverified");
            let binary = fixture.unverifiable_supervisor("core");
            let owned = OwnedProcesses::default();
            let error = owned
                .launch(&binary, 0)
                .expect_err("an invalid readiness line must fail the launch");
            assert!(error.to_string().contains("invalid readiness response"));
            assert!(owned.is_empty());
            // The failed launch settled, so Quit does not wait for it.
            let started = Instant::now();
            assert!(owned.quit(TEST_GRACE).is_empty());
            assert!(started.elapsed() < Duration::from_secs(1));
        }

        #[test]
        fn quit_stops_the_newest_tree_first() {
            let fixture = Fixture::new("order");
            let core = fixture.supervisor("core");
            let gateway = fixture.supervisor("gateway");
            let owned = OwnedProcesses::default();
            owned.launch(&core, 0).expect("launch core");
            owned.launch(&gateway, 0).expect("launch gateway");

            assert!(owned.quit(TEST_GRACE).is_empty());
            assert_eq!(fixture.stop_order(), "gateway\ncore\n");
        }

        /// Wait until the gated fixture has started and recorded its PID.
        fn started_pid(fixture: &Fixture) -> i32 {
            let path = fixture.dir.join("started");
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                if let Some(pid) = fs::read_to_string(&path)
                    .ok()
                    .and_then(|text| text.trim().parse().ok())
                {
                    return pid;
                }
                assert!(Instant::now() < deadline, "gated fixture did not start");
                std::thread::sleep(Duration::from_millis(20));
            }
        }

        #[test]
        fn quit_stops_a_launch_that_is_still_starting() {
            let fixture = Fixture::new("starting");
            let binary = fixture.gated_supervisor("core");
            let owned = std::sync::Arc::new(OwnedProcesses::default());

            let launching = std::sync::Arc::clone(&owned);
            let launch = std::thread::spawn(move || launching.launch(&binary, 0));
            let supervisor = started_pid(&fixture);

            // Quit while the supervisor is spawned but has not reported READY:
            // it is already recorded, so Quit stops it before returning.
            assert!(owned.quit(TEST_GRACE).is_empty());
            assert!(
                !alive(supervisor),
                "the starting supervisor outlived the Quit that returned"
            );
            assert_eq!(fixture.stop_order(), "core\n");

            fixture.release();
            let error = launch
                .join()
                .expect("launch thread")
                .expect_err("the launch learns Quit stopped it");
            assert!(error.to_string().contains("quit"), "{error}");
            assert!(owned.is_empty());
        }

        /// The app exits as soon as its Exit callback returns from Quit. Run
        /// exactly that in a child process and check, from outside, that the
        /// supervisor it was starting did not survive it.
        #[test]
        fn nothing_launched_survives_an_app_exit_right_after_quit() {
            let fixture = Fixture::new("exit");
            let binary = fixture.gated_supervisor("core");
            let status = Command::new(std::env::current_exe().expect("test binary"))
                .args([
                    "--exact",
                    "ownership::tests::process_trees::quit_then_exit_helper",
                    "--ignored",
                    "--test-threads=1",
                ])
                .env(EXIT_HELPER_BINARY_ENV, &binary)
                .env(EXIT_HELPER_DIR_ENV, &fixture.dir)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .expect("run the app stand-in");
            assert!(status.success(), "app stand-in failed: {status}");
            let supervisor = started_pid(&fixture);
            assert!(
                wait_gone(supervisor),
                "a supervisor launched before Quit survived the app's exit"
            );
        }

        const EXIT_HELPER_BINARY_ENV: &str = "ZEROCLAW_DESKTOP_TEST_EXIT_BINARY";
        const EXIT_HELPER_DIR_ENV: &str = "ZEROCLAW_DESKTOP_TEST_EXIT_DIR";

        /// The app stand-in: start a launch that waits for READY, Quit while
        /// it waits, and exit the process immediately, as the Exit callback
        /// does.
        #[test]
        #[ignore = "subprocess helper for nothing_launched_survives_an_app_exit_right_after_quit"]
        fn quit_then_exit_helper() {
            let (Some(binary), Some(dir)) = (
                std::env::var_os(EXIT_HELPER_BINARY_ENV),
                std::env::var_os(EXIT_HELPER_DIR_ENV),
            ) else {
                return;
            };
            let fixture = Fixture {
                dir: PathBuf::from(dir),
            };
            let owned = std::sync::Arc::new(OwnedProcesses::default());
            let launching = std::sync::Arc::clone(&owned);
            let binary = PathBuf::from(binary);
            std::thread::spawn(move || launching.launch(&binary, 0));
            let _ = started_pid(&fixture);
            let errors = owned.quit(TEST_GRACE);
            // Leave the directory for the parent test to inspect.
            std::mem::forget(fixture);
            std::process::exit(if errors.is_empty() { 0 } else { 1 });
        }

        /// A launch whose readiness failed is being stopped (SIGTERM sent,
        /// startup cleanup grace running) when the app Quits and exits at
        /// once. Quit must not return before that stop finishes, or the app's
        /// exit kills the thread doing it and the supervisor survives.
        #[test]
        fn a_failed_start_still_cleaning_up_does_not_survive_an_app_exit() {
            let fixture = Fixture::new("failstop");
            let binary = fixture.slow_failing_supervisor("core");
            let status = Command::new(std::env::current_exe().expect("test binary"))
                .args([
                    "--exact",
                    "ownership::tests::process_trees::quit_during_failed_start_cleanup_helper",
                    "--ignored",
                    "--test-threads=1",
                ])
                .env(EXIT_HELPER_BINARY_ENV, &binary)
                .env(EXIT_HELPER_DIR_ENV, &fixture.dir)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .expect("run the app stand-in");
            let supervisor = started_pid(&fixture);
            let survived = !wait_gone(supervisor);
            if survived {
                // Let it end on its own before the fixture directory goes.
                fixture.stop();
                wait_gone(supervisor);
            }
            assert!(status.success(), "app stand-in failed: {status}");
            assert!(
                !survived,
                "a supervisor whose failed start was still being stopped survived the app's exit"
            );
        }

        /// The app stand-in: start a launch that fails readiness, wait until
        /// its cleanup has sent SIGTERM, then Quit and exit immediately.
        #[test]
        #[ignore = "subprocess helper for a_failed_start_still_cleaning_up_does_not_survive_an_app_exit"]
        fn quit_during_failed_start_cleanup_helper() {
            let (Some(binary), Some(dir)) = (
                std::env::var_os(EXIT_HELPER_BINARY_ENV),
                std::env::var_os(EXIT_HELPER_DIR_ENV),
            ) else {
                return;
            };
            let fixture = Fixture {
                dir: PathBuf::from(dir),
            };
            let owned = std::sync::Arc::new(OwnedProcesses::default());
            let launching = std::sync::Arc::clone(&owned);
            let binary = PathBuf::from(binary);
            std::thread::spawn(move || launching.launch(&binary, 0));
            let term = fixture.dir.join("term-received");
            let deadline = Instant::now() + Duration::from_secs(5);
            while !term.exists() {
                if Instant::now() >= deadline {
                    std::process::exit(2);
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            let errors = owned.quit(TEST_GRACE);
            std::mem::forget(fixture);
            std::process::exit(if errors.is_empty() { 0 } else { 1 });
        }

        #[test]
        fn a_launch_after_quit_began_is_refused() {
            let fixture = Fixture::new("sealed");
            let binary = fixture.supervisor("core");
            let owned = OwnedProcesses::default();
            assert!(owned.quit(TEST_GRACE).is_empty());

            let error = owned
                .launch(&binary, 0)
                .expect_err("no launch is admitted once quitting started");
            assert!(error.to_string().contains("quitting"));
            assert!(owned.is_empty());
        }
    }
}
