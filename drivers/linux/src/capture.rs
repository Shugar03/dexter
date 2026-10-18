//! X11 root-window screenshot capture.

use dexter_core::{Rect, Window};
use dexter_driver::DriverError;
use dexter_vision::{MonitorGeometry, PixelCrop};
use std::fs::File;
use std::path::Path;
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
        return false;
    }
    x11rb::connect(None).is_ok_and(|(connection, screen_num)| {
        connection
            .setup()
            .roots
            .get(screen_num)
            .is_some_and(|screen| screen.width_in_pixels > 0 && screen.height_in_pixels > 0)
    })
}

pub fn capture_app_window(windows: &[Window], path: &Path) -> Result<(), DriverError> {
    let window = crate::geometry::pick_capture_window(windows)
        .ok_or_else(|| DriverError::NotFound("no capturable window for app".into()))?;
    capture_window_region(window, path).map(|_| ())
}

pub fn capture_window_region(window: &Window, path: &Path) -> Result<Rect, DriverError> {
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
    let root_bounds = Rect {
        x: 0.0,
        y: 0.0,
        w: f64::from(root_w),
        h: f64::from(root_h),
    };
    let monitor = MonitorGeometry {
        bounds: root_bounds,
        scale: 1.0,
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
