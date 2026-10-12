#![cfg(target_os = "linux")]

use dexter_core::{
    Action, AppSelector, ElementSource, MouseButton, Observation, ObservationScope, Target,
};
use dexter_driver::{ActContext, ComputerDriver, DriverError};
use dexter_linux::LinuxDriver;
use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::sync::{Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

static FIXTURE_LOCK: Mutex<()> = Mutex::new(());

struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            let _ = self.0.kill();
        }
        let _ = self.0.wait();
    }
}

struct Fixture {
    _child: ChildGuard,
    driver: LinuxDriver,
    pid: i32,
    window_id: u32,
    _guard: MutexGuard<'static, ()>,
}

impl Fixture {
    fn observe(&self, scope: &ObservationScope) -> Observation {
        self.driver.observe(scope).expect("observe the GTK fixture")
    }
}

fn fixture() -> Option<Fixture> {
    let gtk3 = Command::new("python3")
        .args([
            "-c",
            "import gi; gi.require_version('Gtk', '3.0'); from gi.repository import Gtk",
        ])
        .output()
        .is_ok_and(|output| output.status.success());
    if !gtk3 {
        eprintln!("skipped: python3 GTK3 bindings unavailable");
        return None;
    }

    if std::env::var_os("WAYLAND_DISPLAY").is_some_and(|value| !value.is_empty()) {
        eprintln!("skipped: Wayland session does not support X11 capture");
        return None;
    }
    if !dexter_linux::capture::available() {
        eprintln!("skipped: no usable X display");
        return None;
    }
    if std::env::var_os("DBUS_SESSION_BUS_ADDRESS").is_none() {
        eprintln!("skipped: GTK fixture requires a D-Bus session");
        return None;
    }

    let guard = FIXTURE_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let driver = LinuxDriver::new();
    if let Err(error) = driver.windows() {
        eprintln!("skipped: no a11y bus ({error})");
        return None;
    }

    let script = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/capture_app.py");
    let mut child = ChildGuard(
        Command::new("python3")
            .arg(script)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .env("NO_AT_BRIDGE", "0")
            .env("GTK_MODULES", "gail:atk-bridge")
            .spawn()
            .expect("spawn GTK capture fixture"),
    );
    let pid = i32::try_from(child.0.id()).expect("fixture pid fits i32");
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(window_id) = driver
            .windows()
            .ok()
            .and_then(|windows| windows.into_iter().find(|window| window.pid == pid))
            .map(|window| window.id)
        {
            return Some(Fixture {
                _child: child,
                driver,
                pid,
                window_id,
                _guard: guard,
            });
        }
        if let Some(status) = child.0.try_wait().expect("poll GTK fixture") {
            let mut stderr = String::new();
            if let Some(mut stream) = child.0.stderr.take() {
                let _ = stream.read_to_string(&mut stderr);
            }
            panic!("GTK fixture exited {status}: {stderr}");
        }
        assert!(
            Instant::now() < deadline,
            "GTK capture fixture pid {pid} did not appear on the a11y bus"
        );
        thread::sleep(Duration::from_millis(100));
    }
}

fn temp_png(label: &str) -> std::path::PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock after epoch")
        .as_nanos();
    std::env::temp_dir().join(format!("dexter-{label}-{}-{nonce}.png", std::process::id()))
}

fn assert_dimensions_match_window(png: &[u8], window: &dexter_core::Window) {
    let (width, height) = dexter_vision::png_dimensions(png).expect("valid PNG dimensions");
    assert!(
        (f64::from(width) - window.bounds.w).abs() <= 1.0,
        "PNG width {width} differs from window width {}",
        window.bounds.w
    );
    assert!(
        (f64::from(height) - window.bounds.h).abs() <= 1.0,
        "PNG height {height} differs from window height {}",
        window.bounds.h
    );
}

#[test]
fn app_observation_captures_png_and_honors_explicit_path() {
    let Some(fixture) = fixture() else { return };
    let scope = ObservationScope {
        screenshot: true,
        ..ObservationScope::for_app(AppSelector::Pid(fixture.pid))
    };
    let observation = fixture.observe(&scope);
    let path = observation
        .screenshot
        .as_ref()
        .expect("observation has screenshot");
    assert!(std::path::Path::new(path).is_file());
    assert_eq!(observation.windows.len(), 1);
    assert_dimensions_match_window(
        &std::fs::read(path).expect("read captured PNG"),
        &observation.windows[0],
    );
    let _ = std::fs::remove_file(path);

    let explicit_path = temp_png("capture-explicit");
    let scope = ObservationScope {
        screenshot: true,
        screenshot_path: Some(explicit_path.to_string_lossy().into_owned()),
        ..ObservationScope::for_app(AppSelector::Pid(fixture.pid))
    };
    let observation = fixture.observe(&scope);
    assert_eq!(
        observation.screenshot.as_deref(),
        Some(explicit_path.to_string_lossy().as_ref())
    );
    assert!(explicit_path.is_file());
    assert_dimensions_match_window(
        &std::fs::read(&explicit_path).expect("read explicit PNG"),
        &observation.windows[0],
    );
    let _ = std::fs::remove_file(explicit_path);

    let scope = ObservationScope {
        screenshot: true,
        ..ObservationScope::default()
    };
    let observation = fixture.observe(&scope);
    let path = observation
        .screenshot
        .as_ref()
        .expect("unscoped observation has screenshot");
    assert!(!observation.windows.is_empty());
    assert!(std::path::Path::new(path).is_file());
    let _ = std::fs::remove_file(path);
}

#[test]
fn vision_finds_fixture_text_and_ocr_elements_stay_stale() {
    let Some(fixture) = fixture() else { return };
    if dexter_vision::platform_provider().is_none() {
        eprintln!("skipped: no Tesseract vision provider");
        return;
    }

    let scope = ObservationScope {
        window: Some(fixture.window_id),
        vision: true,
        ..ObservationScope::for_app(AppSelector::Pid(fixture.pid))
    };
    let observation = fixture.observe(&scope);
    let window = observation
        .windows
        .first()
        .expect("window-scoped observation has a window");
    let ocr_elements: Vec<_> = observation
        .elements
        .iter()
        .filter(|element| element.source == ElementSource::Ocr)
        .collect();
    let probe = ocr_elements
        .iter()
        .find(|element| {
            element.name.as_deref().is_some_and(|name| {
                name.to_ascii_uppercase().contains("DEXTER")
                    || name.to_ascii_uppercase().contains("PROBE")
            })
        })
        .expect("OCR recognizes the large DEXTER OCR PROBE label");
    let bounds = probe.bounds.expect("recognized token has bounds");
    assert!(
        bounds.x >= window.bounds.x - 1.0
            && bounds.y >= window.bounds.y - 1.0
            && bounds.x + bounds.w <= window.bounds.x + window.bounds.w + 1.0
            && bounds.y + bounds.h <= window.bounds.y + window.bounds.h + 1.0,
        "OCR bounds {bounds:?} fall outside window {:?}",
        window.bounds
    );

    let error = fixture
        .driver
        .act(
            &Action::Click {
                target: Target::Element {
                    observation: observation.id,
                    element: probe.id,
                },
                button: MouseButton::Left,
            },
            &ActContext {
                app: Some(AppSelector::Pid(fixture.pid)),
                allow_coordinates: false,
            },
        )
        .unwrap_err();
    assert!(
        matches!(error, DriverError::StaleReference(_)),
        "OCR element must not be actionable: {error:?}"
    );
}
