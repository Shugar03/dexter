//! X11 root-window screenshot capture.

use dexter_core::{Rect, Window};
use dexter_driver::DriverError;
use dexter_vision::{MonitorGeometry, PixelCrop};
use std::fs::File;
use std::path::{Path, PathBuf};
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{ConnectionExt, ImageFormat, ImageOrder, VisualClass};
use x11rb::rust_connection::RustConnection;

#[derive(Clone, Copy)]
struct PixelFormat {
    depth: u8,
    bits_per_pixel: u8,
    scanline_pad: u8,
    byte_order: ImageOrder,
    class: VisualClass,
    red_mask: u32,
    green_mask: u32,
    blue_mask: u32,
}

impl PixelFormat {
    fn unsupported(self) -> DriverError {
        DriverError::Unsupported(format!(
            "unsupported X11 pixel format: depth={} bpp={} byte_order={:?} class={:?} masks=({:#x},{:#x},{:#x})",
            self.depth,
            self.bits_per_pixel,
            self.byte_order,
            self.class,
            self.red_mask,
            self.green_mask,
            self.blue_mask
        ))
    }
}

pub(crate) fn wayland_active() -> bool {
    std::env::var_os("WAYLAND_DISPLAY").is_some_and(|value| !value.is_empty())
}

fn connect_x11() -> Result<(RustConnection, usize), DriverError> {
    if wayland_active() {
        return Err(DriverError::Unsupported(
            "Wayland session does not support X11 root capture".into(),
        ));
    }
    x11rb::connect(None)
        .map_err(|error| DriverError::Unsupported(format!("no X display is available: {error}")))
}

pub fn available() -> bool {
    if wayland_active() {
        return (x11_root().is_some() || sway_output_available()) && crate::portal::available();
    }
    x11_root().is_some()
}

fn sway_output_available() -> bool {
    std::env::var_os("SWAYSOCK")
        .filter(|sock| !sock.is_empty())
        .map(PathBuf::from)
        .and_then(|sock| crate::sway::get_outputs(&sock).ok())
        .is_some_and(|outputs| crate::sway::single_output(&outputs).is_ok())
}

fn x11_root() -> Option<(RustConnection, usize)> {
    x11rb::connect(None)
        .ok()
        .filter(|(connection, screen_num)| {
            connection
                .setup()
                .roots
                .get(*screen_num)
                .is_some_and(|screen| screen.width_in_pixels > 0 && screen.height_in_pixels > 0)
        })
}

pub fn capture_app_window(windows: &[Window], path: &Path) -> Result<(), DriverError> {
    let window = crate::geometry::pick_capture_window(windows)
        .ok_or_else(|| DriverError::NotFound("no capturable window for app".into()))?;
    capture_window_region(window, path).map(|_| ())
}

pub fn capture_window_region(window: &Window, path: &Path) -> Result<Rect, DriverError> {
    if wayland_active() {
        return capture_wayland_region(window, path);
    }
    let (connection, screen_num) = connect_x11()?;
    let screen = connection
        .setup()
        .roots
        .get(screen_num)
        .ok_or_else(|| DriverError::Unsupported("X display has no root screen".into()))?;
    let root_w = u32::from(screen.width_in_pixels);
    let root_h = u32::from(screen.height_in_pixels);
    if root_w == 0 || root_h == 0 {
        return Err(DriverError::Unsupported(
            "X display root has no captureable dimensions".into(),
        ));
    }
    let scale = crate::scale::gdk_scale_for_pid(window.pid)?;
    let root_bounds = Rect {
        x: 0.0,
        y: 0.0,
        w: f64::from(root_w) / scale,
        h: f64::from(root_h) / scale,
    };
    let monitor = MonitorGeometry {
        bounds: root_bounds,
        scale,
    };
    if dexter_vision::capture_monitor(std::slice::from_ref(&monitor), &window.bounds).is_none() {
        return Err(DriverError::NotFound(
            "window region is not fully on the X screen".into(),
        ));
    }
    let crop = dexter_vision::monitor_pixel_crop(&monitor, &window.bounds, root_w, root_h)
        .ok_or_else(|| {
            DriverError::NotFound("window region is not fully on the X screen".into())
        })?;
    let image = capture_crop(
        &connection,
        screen.root,
        screen.root_depth,
        screen.root_visual,
        crop,
    )?;
    write_png(path, crop.w, crop.h, &image)?;
    Ok(window.bounds)
}

fn capture_wayland_region(window: &Window, path: &Path) -> Result<Rect, DriverError> {
    let (connection, screen_num) = match x11rb::connect(None) {
        Ok(connection) => connection,
        Err(_) => return capture_native_wayland_region(window, path),
    };
    let screen = connection.setup().roots.get(screen_num).ok_or_else(|| {
        DriverError::Unsupported("Wayland capture needs XWayland: no root screen".into())
    })?;
    let root_w = u32::from(screen.width_in_pixels);
    let root_h = u32::from(screen.height_in_pixels);
    if root_w == 0 || root_h == 0 {
        return Err(DriverError::Unsupported(
            "Wayland capture needs XWayland: X root has no captureable dimensions".into(),
        ));
    }

    let gdk = crate::scale::gdk_scale_for_pid(window.pid)?;
    let candidates = crate::ewmh::wayland_client_candidates(&connection, screen.root, window.pid)?;
    if candidates.is_empty() {
        return capture_native_wayland_region(window, path);
    }
    crate::ewmh::classify_wayland_candidates(window.bounds, gdk, &candidates)?;

    let image = crate::portal::screenshot()?;
    let monitor = wayland_monitor((root_w, root_h), (image.width, image.height), gdk)?;
    let crop_not_found = || {
        DriverError::NotFound(format!(
            "window region {},{} {}x{} is not fully on the Wayland screenshot (logical {}x{} @ {}x)",
            window.bounds.x,
            window.bounds.y,
            window.bounds.w,
            window.bounds.h,
            monitor.bounds.w,
            monitor.bounds.h,
            monitor.scale
        ))
    };
    if dexter_vision::capture_monitor(std::slice::from_ref(&monitor), &window.bounds).is_none() {
        return Err(crop_not_found());
    }
    let crop =
        dexter_vision::monitor_pixel_crop(&monitor, &window.bounds, image.width, image.height)
            .ok_or_else(crop_not_found)?;
    let rgb = crop_rgb(&image, crop)?;
    write_png(path, crop.w, crop.h, &rgb)?;
    Ok(window.bounds)
}

fn capture_native_wayland_region(window: &Window, path: &Path) -> Result<Rect, DriverError> {
    let sock = std::env::var_os("SWAYSOCK")
        .filter(|sock| !sock.is_empty())
        .map(PathBuf::from);
    let frame_name = window.title.as_deref().ok_or_else(|| {
        DriverError::Unsupported("native Wayland capture needs a titled AT-SPI frame".into())
    })?;
    let origin = crate::sway::placement(
        sock.as_deref(),
        window.pid,
        frame_name,
        Rect {
            x: 0.0,
            y: 0.0,
            w: window.bounds.w,
            h: window.bounds.h,
        },
    )?;
    if (origin.x - window.bounds.x).abs() > 1.0 || (origin.y - window.bounds.y).abs() > 1.0 {
        return Err(DriverError::StaleReference(format!(
            "native Wayland placement origin ({},{}) differs from observed window origin ({},{})",
            origin.x, origin.y, window.bounds.x, window.bounds.y
        )));
    }
    if !matches!(crate::scale::gdk_scale_for_pid(window.pid), Ok(1.0)) {
        return Err(DriverError::Unsupported(
            "GDK_SCALE on native Wayland unverified".into(),
        ));
    }
    let sock = sock.as_deref().ok_or_else(|| {
        DriverError::Unsupported(
            "native Wayland placement needs sway IPC (SWAYSOCK unset); other compositors are not supported"
                .into(),
        )
    })?;
    let outputs = crate::sway::get_outputs(sock)?;
    let (bounds, scale) = crate::sway::single_output(&outputs)?;
    let image = crate::portal::screenshot()?;
    let expected_width = bounds.w * f64::from(scale);
    let expected_height = bounds.h * f64::from(scale);
    if expected_width != f64::from(image.width) || expected_height != f64::from(image.height) {
        return Err(DriverError::Unsupported(format!(
            "portal screenshot {}x{} does not match Sway logical output {}x{} at scale {scale}",
            image.width, image.height, bounds.w, bounds.h
        )));
    }
    let monitor = MonitorGeometry {
        bounds,
        scale: f64::from(scale),
    };
    let crop_not_found = || {
        DriverError::NotFound(format!(
            "window region {},{} {}x{} is not fully on the Wayland screenshot (logical output {},{} {}x{} @ {}x)",
            window.bounds.x,
            window.bounds.y,
            window.bounds.w,
            window.bounds.h,
            monitor.bounds.x,
            monitor.bounds.y,
            monitor.bounds.w,
            monitor.bounds.h,
            monitor.scale
        ))
    };
    if dexter_vision::capture_monitor(std::slice::from_ref(&monitor), &window.bounds).is_none() {
        return Err(crop_not_found());
    }
    let crop =
        dexter_vision::monitor_pixel_crop(&monitor, &window.bounds, image.width, image.height)
            .ok_or_else(crop_not_found)?;
    let rgb = crop_rgb(&image, crop)?;
    write_png(path, crop.w, crop.h, &rgb)?;
    Ok(window.bounds)
}

fn wayland_monitor(
    xroot: (u32, u32),
    image: (u32, u32),
    gdk: f64,
) -> Result<MonitorGeometry, DriverError> {
    let (xw, xh) = xroot;
    let (iw, ih) = image;
    let unsupported = || {
        DriverError::Unsupported(format!(
            "portal screenshot {iw}x{ih} does not map onto XWayland root {xw}x{xh} (multi-output or fractional scale)"
        ))
    };
    if xw == 0 || xh == 0 || iw == 0 || ih == 0 || !gdk.is_finite() || gdk <= 0.0 {
        return Err(unsupported());
    }
    if iw % xw != 0 || ih % xh != 0 {
        return Err(unsupported());
    }
    let out_w = iw / xw;
    let out_h = ih / xh;
    if out_w != out_h || !(1..=4).contains(&out_w) {
        return Err(unsupported());
    }
    Ok(MonitorGeometry {
        bounds: Rect {
            x: 0.0,
            y: 0.0,
            w: f64::from(xw) / gdk,
            h: f64::from(xh) / gdk,
        },
        scale: gdk * f64::from(out_w),
    })
}

fn crop_rgb(image: &crate::portal::PortalImage, crop: PixelCrop) -> Result<Vec<u8>, DriverError> {
    let row_stride = usize::try_from(u64::from(image.width) * 3)
        .map_err(|_| DriverError::Platform("portal RGB row size overflows address space".into()))?;
    let capacity = usize::try_from(u64::from(crop.w) * u64::from(crop.h) * 3)
        .map_err(|_| DriverError::Platform("portal crop size overflows address space".into()))?;
    let mut rgb = Vec::with_capacity(capacity);
    for row in crop.y..crop.y + crop.h {
        let start = usize::try_from(u64::from(row) * row_stride as u64 + u64::from(crop.x) * 3)
            .map_err(|_| {
                DriverError::Platform("portal crop offset overflows address space".into())
            })?;
        let end = start
            .checked_add(usize::try_from(u64::from(crop.w) * 3).map_err(|_| {
                DriverError::Platform("portal crop row overflows address space".into())
            })?)
            .ok_or_else(|| {
                DriverError::Platform("portal crop end overflows address space".into())
            })?;
        let row = image.rgb.get(start..end).ok_or_else(|| {
            DriverError::Platform("portal screenshot RGB buffer is truncated".into())
        })?;
        rgb.extend_from_slice(row);
    }
    Ok(rgb)
}

fn capture_crop(
    connection: &RustConnection,
    root: u32,
    root_depth: u8,
    root_visual: u32,
    crop: PixelCrop,
) -> Result<Vec<u8>, DriverError> {
    let x = i16::try_from(crop.x)
        .map_err(|_| DriverError::Unsupported("X11 capture x exceeds protocol range".into()))?;
    let y = i16::try_from(crop.y)
        .map_err(|_| DriverError::Unsupported("X11 capture y exceeds protocol range".into()))?;
    let width = u16::try_from(crop.w)
        .map_err(|_| DriverError::Unsupported("X11 capture width exceeds protocol range".into()))?;
    let height = u16::try_from(crop.h).map_err(|_| {
        DriverError::Unsupported("X11 capture height exceeds protocol range".into())
    })?;
    let setup = connection.setup();
    let visual = setup
        .roots
        .iter()
        .flat_map(|screen| screen.allowed_depths.iter())
        .flat_map(|depth| depth.visuals.iter())
        .find(|visual| visual.visual_id == root_visual)
        .ok_or_else(|| {
            DriverError::Unsupported(format!(
                "unsupported X11 pixel format: root visual {root_visual} is missing"
            ))
        })?;
    let format = setup
        .pixmap_formats
        .iter()
        .find(|format| format.depth == root_depth)
        .ok_or_else(|| {
            DriverError::Unsupported(format!(
                "unsupported X11 pixel format: no pixmap format for depth {root_depth}"
            ))
        })?;
    let reply = connection
        .get_image(ImageFormat::Z_PIXMAP, root, x, y, width, height, u32::MAX)
        .map_err(|error| DriverError::Platform(format!("X11 GetImage: {error}")))?
        .reply()
        .map_err(|error| DriverError::Platform(format!("X11 GetImage: {error}")))?;
    let pixel_format = PixelFormat {
        depth: reply.depth,
        bits_per_pixel: format.bits_per_pixel,
        scanline_pad: format.scanline_pad,
        byte_order: setup.image_byte_order,
        class: visual.class,
        red_mask: visual.red_mask,
        green_mask: visual.green_mask,
        blue_mask: visual.blue_mask,
    };
    if reply.depth != root_depth {
        return Err(pixel_format.unsupported());
    }
    decode_bgrx(&reply.data, crop.w, crop.h, pixel_format)
}

fn component(pixel: u32, mask: u32) -> Option<u8> {
    if mask == 0 {
        return None;
    }
    let shift = mask.trailing_zeros();
    let maximum = mask >> shift;
    if !maximum
        .checked_add(1)
        .is_some_and(|next| maximum & next == 0)
    {
        return None;
    }
    let value = (pixel & mask) >> shift;
    Some(((u64::from(value) * 255 + u64::from(maximum) / 2) / u64::from(maximum)) as u8)
}

fn decode_bgrx(
    data: &[u8],
    width: u32,
    height: u32,
    format: PixelFormat,
) -> Result<Vec<u8>, DriverError> {
    let PixelFormat {
        depth,
        bits_per_pixel,
        scanline_pad,
        byte_order,
        class,
        red_mask,
        green_mask,
        blue_mask,
    } = format;
    let supported_depth = matches!(depth, 24 | 32);
    let supported_bpp = matches!(bits_per_pixel, 24 | 32);
    if !supported_depth
        || !supported_bpp
        || (depth == 32 && bits_per_pixel != 32)
        || scanline_pad == 0
        || byte_order != ImageOrder::LSB_FIRST
        || class != VisualClass::TRUE_COLOR
    {
        return Err(format.unsupported());
    }
    let pixel_bytes = usize::from(bits_per_pixel / 8);
    let row_bits = u64::from(width) * u64::from(bits_per_pixel);
    let pad_bits = u64::from(scanline_pad);
    let row_stride = row_bits.div_ceil(pad_bits) * pad_bits / 8;
    let row_stride = usize::try_from(row_stride)
        .map_err(|_| DriverError::Platform("X11 row size overflows address space".into()))?;
    let required = row_stride
        .checked_mul(height as usize)
        .ok_or_else(|| DriverError::Platform("X11 image size overflows address space".into()))?;
    if data.len() < required {
        return Err(DriverError::Platform(
            "X11 GetImage returned a truncated pixel buffer".into(),
        ));
    }
    if [red_mask, green_mask, blue_mask]
        .into_iter()
        .any(|mask| component(0, mask).is_none())
    {
        return Err(format.unsupported());
    }

    let rgb_len = usize::try_from(u64::from(width) * u64::from(height) * 3)
        .map_err(|_| DriverError::Platform("X11 RGB image size overflows address space".into()))?;
    let mut rgb = Vec::with_capacity(rgb_len);
    for row in 0..height as usize {
        let row_start = row_stride * row;
        for column in 0..width as usize {
            let start = row_start + column * pixel_bytes;
            let mut bytes = [0u8; 4];
            bytes[..pixel_bytes].copy_from_slice(&data[start..start + pixel_bytes]);
            let pixel = u32::from_le_bytes(bytes);
            rgb.push(component(pixel, red_mask).expect("validated red mask"));
            rgb.push(component(pixel, green_mask).expect("validated green mask"));
            rgb.push(component(pixel, blue_mask).expect("validated blue mask"));
        }
    }
    Ok(rgb)
}

fn write_png(path: &Path, width: u32, height: u32, rgb: &[u8]) -> Result<(), DriverError> {
    let file = File::create(path)
        .map_err(|error| DriverError::Platform(format!("write {}: {error}", path.display())))?;
    let mut encoder = png::Encoder::new(file, width, height);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder
        .write_header()
        .map_err(|error| DriverError::Platform(format!("encode PNG header: {error}")))?;
    writer
        .write_image_data(rgb)
        .map_err(|error| DriverError::Platform(format!("encode PNG pixels: {error}")))
}

#[cfg(test)]
mod wayland_monitor_tests {
    use super::wayland_monitor;
    use dexter_driver::DriverError;

    #[test]
    fn derives_physical_output_scale_and_logical_bounds() {
        let monitor = wayland_monitor((640, 360), (1280, 720), 1.0).unwrap();
        assert_eq!(monitor.scale, 2.0);
        assert_eq!((monitor.bounds.w, monitor.bounds.h), (640.0, 360.0));
    }

    #[test]
    fn combines_output_and_gdk_scales() {
        let monitor = wayland_monitor((640, 360), (1280, 720), 2.0).unwrap();
        assert_eq!(monitor.scale, 4.0);
        assert_eq!((monitor.bounds.w, monitor.bounds.h), (320.0, 180.0));
    }

    #[test]
    fn accepts_identity_output_scale() {
        let monitor = wayland_monitor((1280, 720), (1280, 720), 1.0).unwrap();
        assert_eq!(monitor.scale, 1.0);
    }

    #[test]
    fn rejects_non_integer_fractional_and_non_uniform_output_scales() {
        for (xroot, image) in [
            ((1920, 1080), (1280, 720)),
            ((1280, 720), (1920, 1080)),
            ((640, 360), (1280, 1080)),
            ((640, 360), (3200, 1800)),
        ] {
            assert!(
                matches!(
                    wayland_monitor(xroot, image, 1.0),
                    Err(DriverError::Unsupported(_))
                ),
                "xroot={xroot:?} image={image:?}"
            );
        }
    }

    #[test]
    fn rejects_zero_dimensions() {
        for (xroot, image) in [
            ((0, 360), (1280, 720)),
            ((640, 0), (1280, 720)),
            ((640, 360), (0, 720)),
            ((640, 360), (1280, 0)),
        ] {
            assert!(matches!(
                wayland_monitor(xroot, image, 1.0),
                Err(DriverError::Unsupported(_))
            ));
        }
    }
}
