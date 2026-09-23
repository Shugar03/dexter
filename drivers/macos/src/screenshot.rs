//! Screen capture via xcap (ScreenCaptureKit on macOS). Requires Screen
//! Recording permission — checked explicitly, never assumed.
//!
//! xcap's window list is not 1:1 with CGWindowList — in some contexts it
//! only reports a few capturable windows. Fallback: capture the monitor and
//! crop to the window bounds we already know from CGWindowList.

use crate::permissions;
use dexter_core::Window;
use dexter_driver::DriverError;
use std::path::Path;

/// Capture the app's frontmost window to a PNG file.
/// `windows` is the CGWindowList for the app (already pid-filtered).
pub fn capture_app_window(
    pid: i32,
    windows: &[Window],
    path: &Path,
) -> Result<String, DriverError> {
    if !permissions::screen_capture_allowed() {
        return Err(DriverError::PermissionDenied(
            "screen recording not granted — enable it in System Settings > Privacy & Security"
                .into(),
        ));
    }
    if let Some(title) = try_direct_window(pid, path)? {
        return Ok(title);
    }
    crop_from_monitor(windows, path)
}

/// Prefer the capturable window xcap reports for this pid.
fn try_direct_window(pid: i32, path: &Path) -> Result<Option<String>, DriverError> {
    let windows = xcap::Window::all()
        .map_err(|e| DriverError::Platform(format!("window enumeration: {e}")))?;
    let mut candidates: Vec<_> = windows
        .into_iter()
        .filter(|w| w.pid().map(|p| p as i32 == pid).unwrap_or(false))
        .filter(|w| !w.is_minimized().unwrap_or(false))
        .collect();
    candidates.sort_by_key(|w| {
        let focused = w.is_focused().unwrap_or(false);
        let area = w
            .width()
            .ok()
            .zip(w.height().ok())
            .map(|(w, h)| w as u64 * h as u64)
            .unwrap_or(0);
        std::cmp::Reverse((focused as u8, area))
    });
    let Some(window) = candidates.into_iter().next() else {
        return Ok(None);
    };
    let title = window.title().unwrap_or_default();
    let img = window
        .capture_image()
        .map_err(|e| DriverError::Platform(format!("capture: {e}")))?;
    img.save(path)
        .map_err(|e| DriverError::Platform(format!("save {}: {e}", path.display())))?;
    Ok(Some(title))
}

/// Pick the app's most capturable window: largest layer-0 on-screen window,
/// falling back to off-screen layer-0 windows (other Spaces). This is the
/// same region `crop_from_monitor` captures, so callers can map image pixels
/// back to `window.bounds` deterministically.
pub fn pick_capture_window(windows: &[Window]) -> Option<&Window> {
    let area = |w: &Window| w.bounds.w * w.bounds.h;
    windows
        .iter()
        .filter(|w| w.layer == 0 && w.on_screen && area(w) > 1.0)
        .max_by(|a, b| {
            area(a)
                .partial_cmp(&area(b))
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .or_else(|| {
            windows
                .iter()
                .filter(|w| w.layer == 0 && area(w) > 1.0)
                .max_by(|a, b| {
                    area(a)
                        .partial_cmp(&area(b))
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
        })
}

/// Capture exactly `window.bounds` — deterministic coverage, unlike
/// `capture_app_window` which lets xcap pick the focused window. Vision OCR
/// uses this so the token→element mapping knows which rect the image covers.
pub fn capture_window_region(window: &Window, path: &Path) -> Result<(), DriverError> {
    if !permissions::screen_capture_allowed() {
        return Err(DriverError::PermissionDenied(
            "screen recording not granted — enable it in System Settings > Privacy & Security"
                .into(),
        ));
    }
    crop_monitor_to(&window.bounds, path)
}

/// Capture the primary monitor and crop to `bounds` (points — the image is
/// in physical pixels, so multiply by the scale factor).
fn crop_monitor_to(bounds: &dexter_core::Rect, path: &Path) -> Result<(), DriverError> {
    let monitor = xcap::Monitor::all()
        .map_err(|e| DriverError::Platform(format!("monitor enumeration: {e}")))?
        .into_iter()
        .find(|m| m.is_primary().unwrap_or(false))
        .or_else(|| xcap::Monitor::all().ok()?.into_iter().next())
        .ok_or_else(|| DriverError::Platform("no monitor".into()))?;

    let img = monitor
        .capture_image()
        .map_err(|e| DriverError::Platform(format!("capture: {e}")))?;
    let scale = monitor.scale_factor().unwrap_or(1.0) as f64;

    let x = (bounds.x * scale).max(0.0) as u32;
    let y = (bounds.y * scale).max(0.0) as u32;
    let w = (bounds.w * scale) as u32;
    let h = (bounds.h * scale) as u32;
    let w = w.min(img.width().saturating_sub(x));
    let h = h.min(img.height().saturating_sub(y));
    if w == 0 || h == 0 {
        return Err(DriverError::NotFound(
            "window bounds outside captured monitor".into(),
        ));
    }
    image::imageops::crop_imm(&img, x, y, w, h)
        .to_image()
        .save(path)
        .map_err(|e| DriverError::Platform(format!("save {}: {e}", path.display())))
}

/// Fallback: capture the primary monitor and crop to the app's main
/// layer-0 window (bounds come from CGWindowList, in points — the image is
/// in physical pixels, so multiply by the scale factor). On-screen flags
/// can be absent for windows in other Spaces, so we prefer but don't
/// require them.
fn crop_from_monitor(windows: &[Window], path: &Path) -> Result<String, DriverError> {
    let target = pick_capture_window(windows)
        .ok_or_else(|| DriverError::NotFound("no capturable window bounds".into()))?;
    crop_monitor_to(&target.bounds, path)?;
    Ok(target.title.clone().unwrap_or_default())
}
