//! Pure capture-geometry rules shared by `scope.screenshot` and
//! `scope.vision`: what a monitor contributes to the
//! `dexter_vision::capture_monitor` decision and which window is the
//! capture target. Kept platform-free so the rules are testable
//! anywhere — `capture.rs` (Windows-only) only feeds live
//! `MONITORINFOEXW` / `EnumDisplaySettingsW` readings through them.

use dexter_core::{Rect, Window};
use dexter_vision::MonitorGeometry;

/// `MonitorGeometry` for one display: `bounds` is the monitor rect in
/// the caller's screen-coordinate space (`MONITORINFOEXW::rcMonitor` —
/// the same space `GetWindowRect` reports window bounds in) and
/// `physical_px` the raster `EnumDisplaySettingsW` reports
/// (`dmPelsWidth`/`dmPelsHeight`), which is the size the capture bitmap
/// is produced at. `scale` is therefore the px-per-unit ratio the crop
/// math needs — the same number `GetDpiForMonitor/96` reports wherever
/// Win32 does not virtualize coordinates, and still correct where it
/// does (secondary displays under DPI-unaware or system-aware
/// processes). A monitor that cannot be measured honestly gets
/// `scale: 0.0` — `capture_monitor` never selects a zero-scale
/// geometry, so bad readings fail closed instead of miscropping.
pub fn monitor_geometry(bounds: &Rect, physical_px: (u32, u32)) -> MonitorGeometry {
    let (pw, ph) = physical_px;
    let scale = if pw > 0 && ph > 0 && bounds.w > 0.0 && bounds.h > 0.0 {
        let sx = f64::from(pw) / bounds.w;
        let sy = f64::from(ph) / bounds.h;
        // Both axes must report the same pixels-per-unit ratio — a
        // disagreement means a stale or anamorphic mode readout the
        // crop math cannot honor honestly.
        if (sx - sy).abs() <= 0.02 * sx {
            sx
        } else {
            0.0
        }
    } else {
        0.0
    };
    MonitorGeometry {
        bounds: *bounds,
        scale,
    }
}

/// The window a screenshot/OCR pass should capture: the largest
/// on-screen layer-0 window, else the largest layer-0 window (an
/// off-screen HWND — e.g. minimized — still picks deterministically).
/// The same rule macOS `pick_capture_window` uses, so `scope.screenshot`
/// covers a deterministic region on every platform.
pub fn pick_capture_window(windows: &[Window]) -> Option<&Window> {
    let layer0 = |w: &&Window| w.layer == 0 && w.bounds.w * w.bounds.h > 1.0;
    let largest =
        |a: &&Window, b: &&Window| (a.bounds.w * a.bounds.h).total_cmp(&(b.bounds.w * b.bounds.h));
    windows
        .iter()
        .filter(|w| layer0(w) && w.on_screen)
        .max_by(|a, b| largest(a, b))
        .or_else(|| windows.iter().filter(layer0).max_by(largest))
}

#[cfg(test)]
mod tests {
    use super::*;

    const WIN: Rect = Rect {
        x: 100.0,
        y: 200.0,
        w: 800.0,
        h: 600.0,
    };

    fn win(id: u32, bounds: Rect, on_screen: bool) -> Window {
        Window {
            id,
            pid: 42,
            app: "notepad".into(),
            bundle_id: None,
            title: None,
            bounds,
            on_screen,
            layer: 0,
        }
    }

    #[test]
    fn monitor_geometry_scale_is_physical_px_per_logical_unit() {
        let mon = Rect {
            x: 0.0,
            y: 0.0,
            w: 1920.0,
            h: 1080.0,
        };
        // 4K raster reported to a 200%-scaled (or virtualized) caller:
        // 2px per logical unit on both axes.
        let g = monitor_geometry(&mon, (3840, 2160));
        assert_eq!(g.bounds, mon);
        assert_eq!(g.scale, 2.0);
        // 1:1 desktop.
        assert_eq!(monitor_geometry(&mon, (1920, 1080)).scale, 1.0);
        // Negative origins are fine — a display left of the primary.
        let left = Rect { x: -1920.0, ..mon };
        assert_eq!(monitor_geometry(&left, (1920, 1080)).scale, 1.0);
    }

    #[test]
    fn monitor_geometry_fails_closed_on_unmeasurable_rasters() {
        // Zero physical raster, degenerate bounds, or an axis
        // disagreement (anamorphic/stale mode info) all produce a
        // zero-scale geometry `capture_monitor` can never select.
        for (bounds, phys) in [
            (WIN, (0, 0)),
            (WIN, (0, 600)),
            (Rect { w: 0.0, ..WIN }, (3840, 2160)),
            (WIN, (800, 800)),
        ] {
            assert_eq!(
                monitor_geometry(&bounds, phys).scale,
                0.0,
                "{bounds:?} {phys:?}"
            );
        }
    }

    #[test]
    fn pick_capture_window_prefers_the_largest_on_screen_window() {
        let small_on = Rect {
            w: 100.0,
            h: 100.0,
            ..WIN
        };
        let big_off = Rect {
            w: 1600.0,
            h: 1200.0,
            ..WIN
        };
        let wins = vec![
            win(1, small_on, true),
            win(2, big_off, false),
            win(3, WIN, true),
        ];
        assert_eq!(pick_capture_window(&wins).map(|w| w.id), Some(3));
    }

    #[test]
    fn pick_capture_window_falls_back_to_off_screen_and_rejects_degenerate() {
        let big = Rect {
            w: 1600.0,
            h: 1200.0,
            ..WIN
        };
        // Everything off-screen: largest still wins (deterministic pick).
        let wins = vec![win(1, WIN, false), win(2, big, false)];
        assert_eq!(pick_capture_window(&wins).map(|w| w.id), Some(2));
        // Layered (non-0) and degenerate windows are never picked.
        let zero = Rect { w: 0.0, ..WIN };
        let layered = Window {
            layer: 8,
            ..win(4, big, true)
        };
        let wins = vec![win(1, zero, true), layered];
        assert!(pick_capture_window(&wins).is_none());
        assert!(pick_capture_window(&[]).is_none());
    }
}
