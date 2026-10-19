#![cfg(target_os = "linux")]

use dexter_core::{AppSelector, ElementSource, Observation, ObservationScope, Window};
use dexter_driver::{ComputerDriver, DriverError};
use dexter_linux::LinuxDriver;
use std::io::Read;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::{Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

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
        self.driver.observe(scope).expect("observe GTK fixture")
    }

    fn primary_window(&self) -> Window {
        self.driver
            .windows()
            .expect("list GTK fixture windows")
            .into_iter()
            .filter(|window| window.pid == self.pid)
            .max_by(|left, right| {
                (left.bounds.w * left.bounds.h).total_cmp(&(right.bounds.w * right.bounds.h))
            })
            .unwrap_or_else(|| panic!("no AT-SPI window for fixture pid {}", self.pid))
    }
}

fn require_wayland_portal() {
    assert!(
        std::env::var_os("WAYLAND_DISPLAY").is_some_and(|value| !value.is_empty()),
        "WAYLAND_DISPLAY is required; run this test through run-in-sway.sh"
    );
    assert!(
        std::env::var_os("DISPLAY").is_some_and(|value| !value.is_empty()),
        "DISPLAY is required for the XWayland test path"
    );
    assert!(
        std::env::var_os("DBUS_SESSION_BUS_ADDRESS").is_some_and(|value| !value.is_empty()),
        "DBUS_SESSION_BUS_ADDRESS is required for the portal and AT-SPI"
    );
    let outputs = sway_outputs();
    assert!(
        outputs.iter().any(|output| output["name"] == "HEADLESS-1"),
        "headless Sway output is absent: {outputs:?}"
    );
    let portal = Command::new("gdbus")
        .args([
            "call",
            "--session",
            "--dest",
            "org.freedesktop.portal.Desktop",
            "--object-path",
            "/org/freedesktop/portal/desktop",
            "--method",
            "org.freedesktop.DBus.Properties.Get",
            "org.freedesktop.portal.Screenshot",
            "version",
        ])
        .output()
        .expect("query Screenshot portal version");
    assert!(
        portal.status.success(),
        "Screenshot portal is unavailable: {}",
        String::from_utf8_lossy(&portal.stderr)
    );
}

fn sway_outputs() -> Vec<serde_json::Value> {
    let output = Command::new("swaymsg")
        .args(["-t", "get_outputs", "-r"])
        .output()
        .expect("run swaymsg -t get_outputs -r");
    assert!(
        output.status.success(),
        "swaymsg get_outputs failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("parse sway output JSON")
}

fn output_scale() -> f64 {
    sway_outputs()
        .into_iter()
        .find(|output| output["name"] == "HEADLESS-1")
        .and_then(|output| output["scale"].as_f64())
        .expect("HEADLESS-1 output scale")
}

fn sway_windows_for_pid(pid: i32) -> Vec<serde_json::Value> {
    let output = Command::new("swaymsg")
        .args(["-t", "get_tree", "-r"])
        .output()
        .expect("run swaymsg -t get_tree");
    assert!(
        output.status.success(),
        "swaymsg get_tree failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let tree: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("parse Sway tree JSON");

    fn collect_windows(node: &serde_json::Value, pid: i32, windows: &mut Vec<serde_json::Value>) {
        if node["pid"].as_i64() == Some(i64::from(pid)) && node["rect"].is_object() {
            windows.push(serde_json::json!({
                "id": node.get("id").cloned(),
                "name": node.get("name").cloned(),
                "rect": node.get("rect").cloned(),
                "window_rect": node.get("window_rect").cloned(),
            }));
        }
        for key in ["nodes", "floating_nodes"] {
            if let Some(children) = node[key].as_array() {
                for child in children {
                    collect_windows(child, pid, windows);
                }
            }
        }
    }

    let mut windows = Vec::new();
    collect_windows(&tree, pid, &mut windows);
    windows.sort_by_key(|window| window["id"].as_u64().unwrap_or_default());
    windows
}

fn wait_for_stable_sway_windows(pid: i32, expected_count: usize) {
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut previous: Option<Vec<serde_json::Value>> = None;
    loop {
        let windows = sway_windows_for_pid(pid);
        if windows.len() == expected_count && previous.as_ref() == Some(&windows) {
            return;
        }
        previous = Some(windows.clone());
        assert!(
            Instant::now() < deadline,
            "Sway windows for fixture pid {pid} did not settle to {expected_count} windows: {windows:?}"
        );
        thread::sleep(Duration::from_millis(100));
    }
}

fn expected_fixture_window_count(script_name: &str) -> usize {
    match script_name {
        "act_app.py" => 2,
        "capture_app.py" => 1,
        _ => panic!("unknown GTK fixture script: {script_name}"),
    }
}

fn fixture(backend: &str, script_name: &str, gdk_scale: Option<&str>) -> Fixture {
    require_wayland_portal();
    let gtk3 = Command::new("python3")
        .args([
            "-c",
            "import gi; gi.require_version('Gtk', '3.0'); from gi.repository import Gtk",
        ])
        .output()
        .expect("run Python GTK3 import probe");
    assert!(gtk3.status.success(), "Python GTK3 bindings are required");

    let guard = FIXTURE_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let script = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(script_name);
    assert!(script.is_file(), "fixture is missing: {}", script.display());
    let mut command = Command::new("python3");
    command
        .arg(&script)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .env("GDK_BACKEND", backend)
        .env("NO_AT_BRIDGE", "0")
        .env("GTK_MODULES", "gail:atk-bridge");
    if let Some(scale) = gdk_scale {
        command.env("GDK_SCALE", scale);
    } else {
        command.env_remove("GDK_SCALE");
    }
    let mut child = ChildGuard(command.spawn().expect("spawn GTK fixture"));
    let pid = i32::try_from(child.0.id()).expect("fixture pid fits i32");
    let driver = LinuxDriver::new();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(window) = driver
            .windows()
            .expect("AT-SPI bus must be available")
            .into_iter()
            .find(|window| {
                window.pid == pid
                    && window.bounds.w > 0.0
                    && window.bounds.h > 0.0
                    && window.on_screen
            })
        {
            wait_for_stable_sway_windows(pid, expected_fixture_window_count(script_name));
            return Fixture {
                _child: child,
                driver,
                pid,
                window_id: window.id,
                _guard: guard,
            };
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
            "GTK fixture pid {pid} did not appear on the AT-SPI bus"
        );
        thread::sleep(Duration::from_millis(100));
    }
}

fn assert_scaled_dimensions(png: &[u8], window: &Window, scale: f64) {
    let (width, height) = dexter_vision::png_dimensions(png).expect("valid capture PNG");
    assert!(
        (f64::from(width) - window.bounds.w * scale).abs() <= 1.0,
        "PNG width {width} differs from logical width {} × scale {scale}",
        window.bounds.w
    );
    assert!(
        (f64::from(height) - window.bounds.h * scale).abs() <= 1.0,
        "PNG height {height} differs from logical height {} × scale {scale}",
        window.bounds.h
    );
}

fn assert_xwayland_screenshot_dimensions(gdk: Option<&str>) {
    let fixture = fixture("x11", "act_app.py", gdk);
    let scale = output_scale() * gdk.map_or(1.0, |value| value.parse().expect("GDK_SCALE"));
    let window = fixture.primary_window();
    let observation = fixture.observe(&ObservationScope {
        screenshot: true,
        ..ObservationScope::for_app(AppSelector::Pid(fixture.pid))
    });
    let screenshot = observation.screenshot.expect("observation screenshot path");
    let png = std::fs::read(&screenshot).expect("read portal-captured PNG");
    assert_scaled_dimensions(&png, &window, scale);
    let _ = std::fs::remove_file(screenshot);
}

fn has_probe(observation: &Observation) -> bool {
    observation.elements.iter().any(|element| {
        element.source == ElementSource::Ocr
            && element.name.as_deref().is_some_and(|name| {
                name.to_ascii_uppercase().contains("DEXTER")
                    || name.to_ascii_uppercase().contains("PROBE")
            })
    })
}

#[test]
#[ignore = "needs headless sway + xdg-desktop-portal-wlr; run via drivers/linux/tests/run-in-sway.sh"]
fn xwayland_fixture_screenshot_matches_output_scale() {
    assert_xwayland_screenshot_dimensions(None);
}

#[test]
#[ignore = "needs headless sway + xdg-desktop-portal-wlr; run via drivers/linux/tests/run-in-sway.sh"]
fn xwayland_vision_finds_fixture_text() {
    let fixture = fixture("x11", "capture_app.py", None);
    assert!(
        dexter_vision::platform_provider().is_some(),
        "Tesseract vision provider is required"
    );
    let scope = ObservationScope {
        window: Some(fixture.window_id),
        vision: true,
        ..ObservationScope::for_app(AppSelector::Pid(fixture.pid))
    };
    let deadline = Instant::now() + Duration::from_secs(5);
    let observation = loop {
        let observation = fixture.observe(&scope);
        if has_probe(&observation) || Instant::now() >= deadline {
            break observation;
        }
        thread::sleep(Duration::from_millis(100));
    };
    assert!(
        has_probe(&observation),
        "OCR recognizes the large DEXTER OCR PROBE label"
    );
    let window = observation
        .windows
        .first()
        .expect("window-scoped observation has a window");
    let probe = observation
        .elements
        .iter()
        .filter(|element| element.source == ElementSource::Ocr)
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
}

#[test]
#[ignore = "needs headless sway + xdg-desktop-portal-wlr; run via drivers/linux/tests/run-in-sway.sh"]
fn xwayland_gdk_scale_two_screenshot_matches_output_scale() {
    assert_xwayland_screenshot_dimensions(Some("2"));
}

#[test]
#[ignore = "needs headless sway + xdg-desktop-portal-wlr; run via drivers/linux/tests/run-in-sway.sh"]
fn native_wayland_screenshot_fails_closed_and_vision_adds_no_ocr() {
    let fixture = fixture("wayland", "act_app.py", None);
    let error = fixture
        .driver
        .observe(&ObservationScope {
            screenshot: true,
            ..ObservationScope::for_app(AppSelector::Pid(fixture.pid))
        })
        .expect_err("native Wayland window has no XWayland placement");
    assert!(
        matches!(error, DriverError::Unsupported(ref message) if message.contains("native Wayland")),
        "{error:?}"
    );

    let observation = fixture.observe(&ObservationScope {
        window: Some(fixture.window_id),
        vision: true,
        ..ObservationScope::for_app(AppSelector::Pid(fixture.pid))
    });
    assert!(observation.collection_errors > 0);
    assert!(observation
        .elements
        .iter()
        .all(|element| element.source != ElementSource::Ocr));
}

#[test]
#[ignore = "needs headless sway + xdg-desktop-portal-wlr; run via drivers/linux/tests/run-in-sway.sh"]
fn screenshots_capability_is_available_in_wayland_session() {
    let _fixture = fixture("x11", "act_app.py", None);
    assert!(LinuxDriver::new().capabilities().screenshots);
}
