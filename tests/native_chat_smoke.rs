//! Cargo/Nextest entry point for the opt-in native driver, plus offline wrapper tests.

use std::env;
use std::ffi::OsStr;
use std::fmt;
use std::io;
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

const REPLY_TIMEOUT_SECONDS: &str = "150";
const DRIVER_TIMEOUT: Duration = Duration::from_secs(300);

#[derive(Debug, PartialEq, Eq)]
enum SmokeFailure {
    OptInRequired,
    UnsupportedPlatform,
    Spawn(io::ErrorKind),
    Wait(io::ErrorKind),
    Stop(io::ErrorKind),
    Exit(Option<i32>),
    Timeout,
}

impl fmt::Display for SmokeFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OptInRequired => write!(
                f,
                "live smoke refused: set WIESEL_UI_LIVE=1 explicitly; no driver was invoked"
            ),
            Self::UnsupportedPlatform => write!(f, "native live smoke requires macOS"),
            Self::Spawn(kind) => write!(f, "could not start native smoke driver ({kind:?})"),
            Self::Wait(kind) => write!(f, "could not observe native smoke driver ({kind:?})"),
            Self::Stop(kind) => write!(f, "could not stop/reap native smoke driver ({kind:?})"),
            Self::Exit(Some(1)) => write!(
                f,
                "native smoke reply/assertion failed (exit 1); inspect Wiesel's status bar; a request may have been charged, do not retry blindly"
            ),
            Self::Exit(Some(2)) => write!(
                f,
                "native smoke permission/setup/usage failed (exit 2); check the bundle, runner Accessibility authorization, login/model setup and empty chat/draft; see README.md"
            ),
            Self::Exit(code) => write!(
                f,
                "native smoke driver exited unexpectedly ({code:?}); submission outcome may be unknown, do not retry blindly"
            ),
            Self::Timeout => write!(
                f,
                "native smoke driver exceeded its process deadline and was stopped; Wiesel was not stopped or reset; a request may have been charged, do not retry blindly"
            ),
        }
    }
}

fn require_live_opt_in(value: Option<&OsStr>) -> Result<(), SmokeFailure> {
    if value == Some(OsStr::new("1")) {
        Ok(())
    } else {
        Err(SmokeFailure::OptInRequired)
    }
}

fn driver_command(root: &Path, driver: &Path, bundle: Option<&OsStr>) -> Command {
    let mut command = Command::new("/bin/bash");
    command
        .current_dir(root)
        .arg(driver)
        .args(["--live", "--timeout", REPLY_TIMEOUT_SECONDS]);
    if let Some(bundle) = bundle {
        command.arg("--app").arg(bundle);
    }
    // Never relay subprocess output: even unexpected compiler/driver diagnostics
    // must not disclose caller-supplied text, paths, or credentials in test logs.
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    command
}

fn run_live_smoke(
    opt_in: Option<&OsStr>,
    root: &Path,
    driver: &Path,
    bundle: Option<&OsStr>,
    timeout: Duration,
) -> Result<(), SmokeFailure> {
    require_live_opt_in(opt_in)?;
    if !cfg!(target_os = "macos") {
        return Err(SmokeFailure::UnsupportedPlatform);
    }
    let started = Instant::now();
    let mut child = driver_command(root, driver, bundle)
        .spawn()
        .map_err(|error| SmokeFailure::Spawn(error.kind()))?;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                return if status.success() {
                    Ok(())
                } else {
                    Err(SmokeFailure::Exit(status.code()))
                };
            }
            Ok(None) if started.elapsed() >= timeout => {
                // Stop only our driver, never the app; there is no reset or retry.
                child
                    .kill()
                    .map_err(|error| SmokeFailure::Stop(error.kind()))?;
                child
                    .wait()
                    .map_err(|error| SmokeFailure::Stop(error.kind()))?;
                return Err(SmokeFailure::Timeout);
            }
            Ok(None) => thread::sleep(Duration::from_millis(25)),
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(SmokeFailure::Wait(error.kind()));
            }
        }
    }
}

#[test]
#[ignore = "one potentially billable native request; also requires WIESEL_UI_LIVE=1 and manual setup"]
fn native_chat_completed_reply() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let opt_in = env::var_os("WIESEL_UI_LIVE");
    let bundle = env::var_os("WIESEL_UI_APP");
    if let Err(error) = run_live_smoke(
        opt_in.as_deref(),
        root,
        &root.join("scripts/ui-smoke.sh"),
        bundle.as_deref(),
        DRIVER_TIMEOUT,
    ) {
        panic!("{error}; subprocess output suppressed; no automatic retry");
    }
}

#[cfg(test)]
mod wrapper_tests {
    use super::*;

    #[test]
    fn require_live_opt_in_rejects_missing_value() {
        assert_eq!(require_live_opt_in(None), Err(SmokeFailure::OptInRequired));
    }

    #[test]
    fn require_live_opt_in_rejects_values_other_than_exact_one() {
        for value in ["", "0", "true", "yes", "01", "1 "] {
            assert_eq!(
                require_live_opt_in(Some(OsStr::new(value))),
                Err(SmokeFailure::OptInRequired)
            );
        }
    }

    #[test]
    fn require_live_opt_in_accepts_exact_one() {
        assert_eq!(require_live_opt_in(Some(OsStr::new("1"))), Ok(()));
    }

    // Fake drivers are local shell scripts, never the real Swift runner. Each
    // fixture has its own directory; tests do not mutate process environment.
    #[cfg(target_os = "macos")]
    mod fake_driver {
        use super::*;
        use std::fs;
        use std::path::PathBuf;
        use std::sync::atomic::{AtomicU64, Ordering};

        struct Fixture {
            root: PathBuf,
            driver: PathBuf,
        }

        impl Fixture {
            fn new(script: &str) -> Self {
                static NEXT_ID: AtomicU64 = AtomicU64::new(0);
                let root = env::temp_dir().join(format!(
                    "wiesel native smoke {} {}",
                    std::process::id(),
                    NEXT_ID.fetch_add(1, Ordering::Relaxed)
                ));
                fs::create_dir(&root).expect("create fake driver fixture");
                let driver = root.join("fake driver.sh");
                fs::write(&driver, script).expect("write fake driver");
                Self { root, driver }
            }

            fn run(&self, bundle: Option<&OsStr>) -> Result<(), SmokeFailure> {
                run_live_smoke(
                    Some(OsStr::new("1")),
                    &self.root,
                    &self.driver,
                    bundle,
                    Duration::from_secs(5),
                )
            }
        }

        impl Drop for Fixture {
            fn drop(&mut self) {
                fs::remove_dir_all(&self.root).expect("remove fake driver fixture");
            }
        }

        #[test]
        fn run_live_smoke_does_not_invoke_driver_without_opt_in() {
            let fixture = Fixture::new("touch invoked; exit 0");
            let result = run_live_smoke(
                None,
                &fixture.root,
                &fixture.driver,
                None,
                Duration::from_secs(5),
            );
            assert_eq!(result, Err(SmokeFailure::OptInRequired));
            assert!(!fixture.root.join("invoked").exists());
        }

        #[test]
        fn run_live_smoke_uses_root_and_default_live_arguments() {
            let fixture = Fixture::new(
                r#"[[ "$PWD" -ef "${BASH_SOURCE[0]%/*}" ]] &&
[[ "$#" == 3 && "$1" == --live && "$2" == --timeout && "$3" == 150 ]]
"#,
            );
            assert_eq!(fixture.run(None), Ok(()));
        }

        #[test]
        fn run_live_smoke_passes_bundle_as_one_argument_without_shell_expansion() {
            let fixture = Fixture::new(
                r#"[[ "$#" == 5 && "$4" == --app && "$5" == '/bundle with spaces/$(touch injected).app' ]]
"#,
            );
            assert_eq!(
                fixture.run(Some(OsStr::new(
                    "/bundle with spaces/$(touch injected).app"
                ))),
                Ok(())
            );
        }

        #[test]
        fn run_live_smoke_reports_assertion_failure_without_subprocess_text() {
            let fixture = Fixture::new(
                "echo 'sensitive prompt reply credential'; echo 'sensitive prompt reply credential' >&2; exit 1",
            );
            let error = fixture.run(None).expect_err("fake assertion failure");
            assert_eq!(error, SmokeFailure::Exit(Some(1)));
            assert!(!error.to_string().contains("sensitive"));
        }

        #[test]
        fn run_live_smoke_does_not_retry_a_failed_driver() {
            let fixture = Fixture::new("echo invoked >> invocations; exit 1");
            assert_eq!(fixture.run(None), Err(SmokeFailure::Exit(Some(1))));
            assert_eq!(
                fs::read_to_string(fixture.root.join("invocations"))
                    .expect("read fake invocation count"),
                "invoked\n"
            );
        }

        #[test]
        fn run_live_smoke_reports_spawn_failure() {
            let fixture = Fixture::new("exit 0");
            assert_eq!(
                run_live_smoke(
                    Some(OsStr::new("1")),
                    &fixture.root.join("missing directory"),
                    &fixture.driver,
                    None,
                    Duration::from_secs(5),
                ),
                Err(SmokeFailure::Spawn(io::ErrorKind::NotFound))
            );
        }

        #[test]
        fn run_live_smoke_reports_setup_failure_as_failure_not_skip() {
            let fixture = Fixture::new("exit 2");
            assert_eq!(fixture.run(None), Err(SmokeFailure::Exit(Some(2))));
        }

        #[test]
        fn run_live_smoke_reports_unexpected_exit() {
            let fixture = Fixture::new("exit 7");
            assert_eq!(fixture.run(None), Err(SmokeFailure::Exit(Some(7))));
        }

        #[test]
        fn run_live_smoke_stops_driver_on_process_deadline() {
            let fixture = Fixture::new("while :; do :; done");
            assert_eq!(
                run_live_smoke(
                    Some(OsStr::new("1")),
                    &fixture.root,
                    &fixture.driver,
                    None,
                    Duration::from_millis(50),
                ),
                Err(SmokeFailure::Timeout)
            );
        }

        #[test]
        fn run_live_smoke_reports_missing_driver_as_failure() {
            let fixture = Fixture::new("exit 0");
            fs::remove_file(&fixture.driver).expect("remove fake driver");
            assert_eq!(fixture.run(None), Err(SmokeFailure::Exit(Some(127))));
        }
    }
}
