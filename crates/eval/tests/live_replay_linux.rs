#![cfg(target_os = "linux")]

use dexter_core::{
    Action, AppSelector, ExpectedState, MouseButton, SemanticTarget, Target, VerificationStatus,
};
use dexter_driver::ComputerDriver;
use dexter_engine::{Engine, RunConfig, Step};
use dexter_eval::recording::{
    record, replay_with, RecordedStep, Recording, ReplayOptions, StepResult,
};
use dexter_linux::LinuxDriver;
use dexter_policy::{fingerprint, ActionContext, Policy};
use dexter_sim::SimDriver;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

static FIXTURE_LOCK: Mutex<()> = Mutex::new(());

struct Fixture {
    child: ChildGuard,
    pid: i32,
    _guard: MutexGuard<'static, ()>,
}

struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            let _ = self.0.kill();
        }
        let _ = self.0.wait();
    }
}

fn start_fixture() -> Fixture {
    assert!(
        std::env::var_os("DISPLAY").is_some(),
        "live replay requires DISPLAY (run with DISPLAY=:0)"
    );
    assert!(
        std::env::var_os("DBUS_SESSION_BUS_ADDRESS").is_some(),
        "live replay requires a D-Bus session"
    );
    let gtk3 = Command::new("python3")
        .args([
            "-c",
            "import gi; gi.require_version('Gtk', '3.0'); from gi.repository import Gtk",
        ])
        .status()
        .expect("python3 must be installed");
    assert!(gtk3.success(), "Python GTK3 bindings are required");

    let guard = FIXTURE_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    LinuxDriver::new()
        .windows()
        .expect("an accessible AT-SPI bus is required");

    let script = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../drivers/linux/tests/fixtures/act_app.py"
    );
    let child = Command::new("python3")
        .arg(script)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .env("NO_AT_BRIDGE", "0")
        .env("GTK_MODULES", "gail:atk-bridge")
        .spawn()
        .expect("spawn GTK act fixture");
    let pid = child.id() as i32;
    let mut fixture = Fixture {
        child: ChildGuard(child),
        pid,
        _guard: guard,
    };
    let driver = LinuxDriver::new();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if driver
            .windows()
            .is_ok_and(|windows| windows.iter().any(|window| window.pid == fixture.pid))
        {
            break;
        }
        if let Some(status) = fixture.child.0.try_wait().expect("poll fixture process") {
            panic!("GTK act fixture exited before registering: {status}");
        }
        assert!(
            Instant::now() < deadline,
            "GTK fixture pid {} did not appear on the a11y bus",
            fixture.pid
        );
        thread::sleep(Duration::from_millis(100));
    }
    fixture
}

fn app() -> AppSelector {
    AppSelector::Name("python3".into())
}

fn semantic(role: &str, name: &str) -> Target {
    Target::Semantic(SemanticTarget {
        role: Some(role.into()),
        name: Some(name.into()),
        ..SemanticTarget::default()
    })
}

fn click(name: &str) -> Action {
    Action::Click {
        target: semantic("button", name),
        button: MouseButton::Left,
    }
}

fn policy() -> Policy {
    Policy::from_toml(
        r#"
        [defaults]
        mutating = "allow"

        [[rule]]
        action = "click"
        decision = "require_approval"
        reason = "click approval required"
        "#,
    )
    .expect("live replay policy parses")
}

fn config() -> RunConfig {
    RunConfig {
        app: Some(app()),
        max_attempts: 1,
        ..RunConfig::default()
    }
}

fn journal_path() -> PathBuf {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock after epoch")
        .as_nanos();
    std::env::temp_dir().join(format!(
        "dexter-live-replay-{}-{stamp}.jsonl",
        std::process::id()
    ))
}

fn completed_recording() -> Recording {
    let fixture = start_fixture();
    let done_when = ExpectedState::TextPresent {
        text: "Hello".into(),
    };
    let first_action = click("Greet");
    let grant = fingerprint(
        &first_action,
        &ActionContext {
            app: Some(app()),
            target_hint: None,
        },
    );
    let path = journal_path();
    let mut engine = Engine::new(LinuxDriver::new(), policy(), Duration::from_secs(60));
    engine
        .set_journal_sink(&path)
        .expect("attach live JSONL journal sink");
    engine.pre_grant_approval(&grant);
    for action in [
        first_action,
        Action::SetValue {
            target: semantic("text_area", "Name"),
            value: "Replay Name".into(),
        },
    ] {
        let status = engine.run_step(
            &Step {
                note: None,
                action,
                expect: None,
                max_attempts: Some(1),
                app: None,
            },
            &config(),
        );
        assert!(status.done(), "recording action failed: {status:?}");
    }
    let recording = record(
        "live-act-fixture",
        "greet and enter a name",
        "completed",
        &done_when,
        &engine.events(),
    )
    .expect("record Linux driver journal");
    assert_eq!(recording.driver.as_deref(), Some("linux"));
    assert!(recording.steps[0].granted);
    assert!(!recording.steps[1].granted);
    drop(engine);
    drop(fixture);
    std::fs::remove_file(path).expect("remove temporary live journal");
    recording
}

#[test]
#[ignore = "requires DISPLAY=:0, GTK3, an AT-SPI bus, and a D-Bus session"]
fn live_linux_journal_replays_only_with_matching_opt_ins() {
    let recording = completed_recording();
    let fixture = start_fixture();
    let no_honor = replay_with(
        &recording,
        LinuxDriver::new(),
        policy(),
        config(),
        &ReplayOptions::default(),
    );
    assert_eq!(no_honor.diverged_at, Some(0));
    assert_eq!(no_honor.replayed.first(), Some(&StepResult::NeedsApproval));
    let grant = recording.steps[0]
        .fingerprint
        .as_deref()
        .expect("first action records fingerprint");
    assert!(no_honor
        .divergence
        .as_ref()
        .expect("approval divergence")
        .reason
        .contains(grant));

    let honored = replay_with(
        &recording,
        LinuxDriver::new(),
        policy(),
        config(),
        &ReplayOptions {
            honor_recorded_grants: true,
            ..ReplayOptions::default()
        },
    );
    assert!(
        honored.faithful(&recording),
        "recorded grant replay diverged: {honored:?}"
    );
    assert_eq!(honored.done_when, VerificationStatus::Verified);

    let sim_mismatch = replay_with(
        &recording,
        SimDriver::new(Vec::new()),
        policy(),
        config(),
        &ReplayOptions::default(),
    );
    assert_eq!(sim_mismatch.diverged_at, Some(0));
    assert!(sim_mismatch.replayed.is_empty());
    assert!(sim_mismatch
        .divergence
        .as_ref()
        .expect("driver mismatch")
        .reason
        .contains("driver mismatch: recorded by linux, replaying on sim"));

    let locked_action = click("Locked");
    let locked_fingerprint = fingerprint(
        &locked_action,
        &ActionContext {
            app: Some(app()),
            target_hint: None,
        },
    );
    let locked_recording = Recording {
        scenario: "live-act-fixture".into(),
        goal: "click the disabled Locked button".into(),
        outcome: "failed".into(),
        done_when: ExpectedState::TextPresent {
            text: "never present".into(),
        },
        driver: Some("linux".into()),
        steps: vec![RecordedStep {
            action: locked_action,
            app: Some(app()),
            observes_before: 0,
            result: StepResult::Done,
            fingerprint: Some(locked_fingerprint.clone()),
            granted: false,
        }],
        trailing_observes: 0,
    };
    let locked_report = replay_with(
        &locked_recording,
        LinuxDriver::new(),
        policy(),
        config(),
        &ReplayOptions {
            grants: vec![locked_fingerprint],
            ..ReplayOptions::default()
        },
    );
    assert_eq!(locked_report.diverged_at, Some(0));
    assert!(
        locked_report
            .divergence
            .as_ref()
            .expect("disabled target divergence")
            .reason
            .contains("disabled"),
        "expected driver's disabled-target reason, got {locked_report:?}"
    );
    drop(fixture);
}
