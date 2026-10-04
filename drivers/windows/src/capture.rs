//! Screen capture for `scope.screenshot`/`scope.vision`: monitor
//! enumeration (`EnumDisplayMonitors` + `GetMonitorInfoW`, physical
//! raster from `EnumDisplaySettingsW`), per-display GDI capture
//! (`CreateDCW("DISPLAY")` + `StretchBlt` into a `dmPels`-sized bitmap,
//! `GetDIBits` readout), all behind the `dexter_vision` monitor-crop
//! contract — a region no single display fully covers fails closed.
//!
//! `MONITORINFOEXW::rcMonitor`, `GetWindowRect`/`DwmGetWindowAttribute`
//! and `CreateDCW("DISPLAY")` coordinates all live in the caller's
//! virtual screen space, so `MonitorGeometry.scale = dmPels/rcMonitor`
//! is the honest px-per-unit ratio for the bitmap the same APIs produce.

use dexter_core::{Rect, Window};
use dexter_driver::DriverError;
use dexter_vision::MonitorGeometry;
use std::path::Path;
use windows::core::{w, BOOL, PCWSTR};
use windows::Win32::Foundation::{LPARAM, RECT};
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_EXTENDED_FRAME_BOUNDS};
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleBitmap, CreateCompatibleDC, CreateDCW, DeleteDC, DeleteObject,
    EnumDisplayMonitors, EnumDisplaySettingsW, GetDIBits, GetMonitorInfoW, SelectObject,
    SetStretchBltMode, StretchBlt, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DEVMODEW, DIB_RGB_COLORS,
    ENUM_CURRENT_SETTINGS, HALFTONE, HBITMAP, HDC, HGDIOBJ, HMONITOR, MONITORINFOEXW, SRCCOPY,
};

/// One display: its rect in virtual screen coordinates and the physical
/// raster `EnumDisplaySettingsW` reports — the size the capture bitmap
/// is produced at.
struct Mon {
    geometry: MonitorGeometry,
    physical: (i32, i32),
}

/// Whether a screen capture is actually possible right now: the display
/// device context the capture path needs exists and at least one
/// monitor reports a measurable raster. `capabilities().screenshots`
/// reports exactly this — the claim is never made where a capture would
/// fail (session-0, displayless host).
pub fn available() -> bool {
    unsafe {
        let screen = CreateDCW(w!("DISPLAY"), PCWSTR::null(), PCWSTR::null(), None);
        if screen.is_invalid() {
            return false;
        }
        let _ = DeleteDC(screen);
    }
    monitors()
        .map(|ms| ms.iter().any(|m| m.geometry.scale > 0.0))
        .unwrap_or(false)
}

/// Capture the app's capture window — the largest on-screen window —
/// to a PNG file, mirroring macOS `capture_app_window`. `windows` is the
/// already pid-filtered list for the scoped app (and already narrowed
/// when `scope.window` was set).
pub fn capture_app_window(windows: &[Window], path: &Path) -> Result<(), DriverError> {
    let window = crate::geometry::pick_capture_window(windows)
        .ok_or_else(|| DriverError::NotFound("no capturable window for app".into()))?;
    capture_window_region(window, path).map(|_| ())
}

/// Capture exactly the region `window` occupies — its *visible* frame
/// (`DWMWA_EXTENDED_FRAME_BOUNDS` when the attribute reads; the
/// `GetWindowRect` bounds carry invisible resize borders that would
/// spill off-monitor on edge-snapped windows). Returns the rect
/// actually captured so callers map image pixels back to screen
/// coordinates deterministically.
pub fn capture_window_region(window: &Window, path: &Path) -> Result<Rect, DriverError> {
    let rect = visible_bounds(window);
    let img = crop_monitor_to(&monitors()?, &rect)?;
    img.save(path)
        .map_err(|e| DriverError::Platform(format!("write {}: {e}", path.display())))?;
    Ok(rect)
}

/// Displays as the crop contract sees them: a `MonitorGeometry` per
/// reported monitor, paired with the physical raster to capture at.
/// Fail-closed end to end — no monitors is an error, and a monitor with
/// unmeasurable geometry stays in the list with `scale: 0.0` (which
/// `capture_monitor` can never select).
fn monitors() -> Result<Vec<Mon>, DriverError> {
    let mut out = Vec::new();
    let ok = unsafe {
        EnumDisplayMonitors(
            None,
            None,
            Some(enum_monitors),
            LPARAM(&mut out as *mut _ as isize),
        )
        .as_bool()
    };
    if !ok || out.is_empty() {
        return Err(DriverError::Platform(
            "EnumDisplayMonitors reported none".into(),
        ));
    }
    Ok(out)
}

unsafe extern "system" fn enum_monitors(
    hmon: HMONITOR,
    _hdc: HDC,
    _rect: *mut RECT,
    data: LPARAM,
) -> BOOL {
    unsafe {
        let out = &mut *(data.0 as *mut Vec<Mon>);
        let mut ex = MONITORINFOEXW::default();
        ex.monitorInfo.cbSize = size_of::<MONITORINFOEXW>() as u32;
        if GetMonitorInfoW(hmon, &mut ex.monitorInfo).as_bool() {
            let rc = ex.monitorInfo.rcMonitor;
            let bounds = Rect {
                x: f64::from(rc.left),
                y: f64::from(rc.top),
                w: f64::from(rc.right - rc.left),
                h: f64::from(rc.bottom - rc.top),
            };
            let (pw, ph) = physical_px(&ex.szDevice);
            out.push(Mon {
                geometry: crate::geometry::monitor_geometry(&bounds, (pw, ph)),
                physical: (pw as i32, ph as i32),
            });
        }
        BOOL(1)
    }
}

/// The raster `EnumDisplaySettingsW` reports for this display device —
/// `(0, 0)` when the mode cannot be read, which `monitor_geometry`
/// maps to an unselectable zero-scale geometry.
fn physical_px(device: &[u16; 32]) -> (u32, u32) {
    let mut dm = DEVMODEW {
        dmSize: size_of::<DEVMODEW>() as u16,
        ..Default::default()
    };
    unsafe {
        if EnumDisplaySettingsW(
            PCWSTR::from_raw(device.as_ptr()),
            ENUM_CURRENT_SETTINGS,
            &mut dm,
        )
        .as_bool()
        {
            (dm.dmPelsWidth, dm.dmPelsHeight)
        } else {
            (0, 0)
        }
    }
}

/// The rect a screenshot should claim: DWM's visible frame when the
/// attribute reads — the `GetWindowRect` bounds a window lists carry
/// ~7px invisible resize borders that would spill off-monitor on
/// edge-snapped windows — else the listed bounds as an honest fallback.
fn visible_bounds(window: &Window) -> Rect {
    let mut rc = RECT::default();
    unsafe {
        if DwmGetWindowAttribute(
            crate::win::hwnd_of(window.id),
            DWMWA_EXTENDED_FRAME_BOUNDS,
            &mut rc as *mut _ as *mut _,
            size_of::<RECT>() as u32,
        )
        .is_ok()
        {
            return Rect {
                x: f64::from(rc.left),
                y: f64::from(rc.top),
                w: f64::from(rc.right - rc.left),
                h: f64::from(rc.bottom - rc.top),
            };
        }
    }
    window.bounds
}

/// The per-display crop `dexter_vision` prescribes: pick the single
/// monitor containing the region (spanning/off-screen/degenerate
/// regions fail closed), capture that display's full raster, then crop
/// the region's pixels out of it.
fn crop_monitor_to(mons: &[Mon], region: &Rect) -> Result<image::RgbaImage, DriverError> {
    let geoms: Vec<MonitorGeometry> = mons.iter().map(|m| m.geometry).collect();
    let idx = dexter_vision::capture_monitor(&geoms, region).ok_or_else(|| {
        DriverError::NotFound(format!(
            "region {region:?} not contained in any one display"
        ))
    })?;
    let m = &mons[idx];
    let img = capture_monitor_image(m)?;
    let crop = dexter_vision::monitor_pixel_crop(&m.geometry, region, img.width(), img.height())
        .ok_or_else(|| {
            DriverError::NotFound(format!("crop for {region:?} outside the monitor image"))
        })?;
    Ok(image::imageops::crop_imm(&img, crop.x, crop.y, crop.w, crop.h).to_image())
}

/// `StretchBlt` one display's virtual-space rect into a
/// physical-resolution bitmap, then `GetDIBits` it out as a top-down
/// 32bpp image. Every GDI object is released on every path — nothing is
/// kept between calls.
fn capture_monitor_image(m: &Mon) -> Result<image::RgbaImage, DriverError> {
    let (pw, ph) = m.physical;
    if pw <= 0 || ph <= 0 {
        return Err(DriverError::Platform(
            "monitor has no measurable raster".into(),
        ));
    }
    let (x, y) = (m.geometry.bounds.x as i32, m.geometry.bounds.y as i32);
    let (w, h) = (m.geometry.bounds.w as i32, m.geometry.bounds.h as i32);
    unsafe {
        let screen = CreateDCW(w!("DISPLAY"), PCWSTR::null(), PCWSTR::null(), None);
        if screen.is_invalid() {
            return Err(DriverError::Platform("CreateDCW(DISPLAY) failed".into()));
        }
        let result = capture_from_dc(screen, x, y, w, h, pw, ph);
        let _ = DeleteDC(screen);
        result
    }
}

unsafe fn capture_from_dc(
    screen: HDC,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    pw: i32,
    ph: i32,
) -> Result<image::RgbaImage, DriverError> {
    unsafe {
        let mem = CreateCompatibleDC(Some(screen));
        if mem.is_invalid() {
            return Err(DriverError::Platform("CreateCompatibleDC failed".into()));
        }
        let bmp: HBITMAP = CreateCompatibleBitmap(screen, pw, ph);
        if bmp.is_invalid() {
            let _ = DeleteDC(mem);
            return Err(DriverError::Platform(
                "CreateCompatibleBitmap failed".into(),
            ));
        }
        let old = SelectObject(mem, HGDIOBJ(bmp.0));
        let result = (|| {
            SetStretchBltMode(mem, HALFTONE);
            if !StretchBlt(mem, 0, 0, pw, ph, Some(screen), x, y, w, h, SRCCOPY).as_bool() {
                return Err(DriverError::Platform("StretchBlt failed".into()));
            }
            // Top-down 32bpp readout (negative biHeight = first row
            // written first — matches the image buffer layout).
            let mut bmi = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: pw,
                    biHeight: -ph,
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut buf = vec![0u8; (pw * ph * 4) as usize];
            if GetDIBits(
                mem,
                bmp,
                0,
                ph as u32,
                Some(buf.as_mut_ptr() as *mut _),
                &mut bmi,
                DIB_RGB_COLORS,
            ) == 0
            {
                return Err(DriverError::Platform("GetDIBits failed".into()));
            }
            // GDI emits BGRA with an undefined alpha byte; swap to RGBA
            // and pin alpha opaque for the PNG encoder.
            for px in buf.chunks_exact_mut(4) {
                px.swap(0, 2);
                px[3] = 255;
            }
            image::RgbaImage::from_raw(pw as u32, ph as u32, buf)
                .ok_or_else(|| DriverError::Platform("bitmap buffer size mismatch".into()))
        })();
        SelectObject(mem, old);
        let _ = DeleteObject(HGDIOBJ(bmp.0));
        let _ = DeleteDC(mem);
        result
    }
}
