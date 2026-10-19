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
    fixture_with_scale(None)
}

fn fixture_with_scale(gdk_scale: Option<&str>) -> Option<Fixture> {
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
    let mut command = Command::new("python3");
    command
        .arg(script)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .env("NO_AT_BRIDGE", "0")
        .env("GTK_MODULES", "gail:atk-bridge");
    if let Some(scale) = gdk_scale {
        command.env("GDK_SCALE", scale);
    }
    let mut child = ChildGuard(command.spawn().expect("spawn GTK capture fixture"));
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

fn x_device_frame_dimensions(pid: i32) -> (u32, u32) {
    let windows = Command::new("wmctrl")
        .args(["-lpG"])
        .output()
        .expect("run wmctrl for X11 geometry");
    assert!(windows.status.success(), "wmctrl failed");
    let rows = String::from_utf8(windows.stdout).expect("wmctrl output is UTF-8");
    let pid = pid.to_string();
    let row = rows
        .lines()
        .find(|line| {
            let fields = line.split_whitespace().collect::<Vec<_>>();
            fields.get(2).copied() == Some(pid.as_str())
                && line.ends_with(" dexter-capture-fixture")
        })
        .unwrap_or_else(|| panic!("no wmctrl row for fixture pid {pid}: {rows}"));
    let fields = row.split_whitespace().collect::<Vec<_>>();
    let client_width = fields[5].parse::<u32>().expect("X client width");
    let client_height = fields[6].parse::<u32>().expect("X client height");
    let extents = Command::new("xprop")
        .args(["-id", fields[0], "_NET_FRAME_EXTENTS"])
        .output()
        .expect("read X frame extents");
    assert!(extents.status.success(), "xprop failed");
    let extents = String::from_utf8(extents.stdout).expect("xprop output is UTF-8");
    let values = extents
        .split_once('=')
        .expect("_NET_FRAME_EXTENTS property exists")
        .1
        .split(',')
        .map(|value| value.trim().parse::<u32>().expect("frame extent"))
        .collect::<Vec<_>>();
    assert_eq!(values.len(), 4, "_NET_FRAME_EXTENTS={values:?}");
    (
        client_width + values[0] + values[1],
        client_height + values[2] + values[3],
    )
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
fn gdk_scale_two_capture_matches_x_device_frame_geometry() {
    let fixture = fixture_with_scale(Some("2"))
        .expect("GDK_SCALE=2 live GTK fixture requires X11 and a D-Bus session");
    let windows = fixture.driver.windows().expect("list fixture windows");
    let window = windows
        .iter()
        .find(|window| window.pid == fixture.pid)
        .expect("fixture AT-SPI frame");
    let path = temp_png("hidpi");
    dexter_linux::capture::capture_window_region(window, &path).expect("capture scaled frame");
    let png = std::fs::read(&path).expect("read scaled capture");
    let (width, height) = dexter_vision::png_dimensions(&png).expect("valid PNG dimensions");
    let (device_width, device_height) = x_device_frame_dimensions(fixture.pid);
    assert!(
        width.abs_diff(device_width) <= 2,
        "PNG width {width} differs from X device frame width {device_width}"
    );
    assert!(
        height.abs_diff(device_height) <= 2,
        "PNG height {height} differs from X device frame height {device_height}"
    );
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
    let has_probe = |observation: &Observation| {
        observation.elements.iter().any(|element| {
            element.source == ElementSource::Ocr
                && element.name.as_deref().is_some_and(|name| {
                    name.to_ascii_uppercase().contains("DEXTER")
                        || name.to_ascii_uppercase().contains("PROBE")
                })
        })
    };
    let deadline = Instant::now() + Duration::from_secs(3);
    let observation = loop {
        let observation = fixture.observe(&scope);
        if has_probe(&observation) || Instant::now() >= deadline {
            break observation;
        }
        thread::sleep(Duration::from_millis(100));
    };
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
