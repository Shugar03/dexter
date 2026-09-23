//! Window enumeration via CGWindowListCopyWindowInfo.
//!
//! Window *titles* only appear when this process has Screen Recording
//! permission; owner, pid, bounds and layer are always present. Missing
//! titles stay `None` — never faked.

use core_foundation::array::CFArray;
use core_foundation::base::{CFType, TCFType};
use core_foundation::boolean::CFBoolean;
use core_foundation::dictionary::CFDictionary;
use core_foundation::number::CFNumber;
use core_foundation::string::CFString;
use core_foundation::ConcreteCFType;
use core_graphics::window::{
    kCGNullWindowID, kCGWindowListExcludeDesktopElements, CGWindowListCopyWindowInfo,
};
use dexter_core::{Rect, Window};
use dexter_driver::DriverError;

type Dict = CFDictionary<CFString, CFType>;

fn get<T: ConcreteCFType>(dict: &Dict, key: &'static str) -> Option<T> {
    dict.find(CFString::from_static_string(key))
        .and_then(|v| v.downcast::<T>())
}

fn num(dict: &Dict, key: &'static str) -> Option<f64> {
    get::<CFNumber>(dict, key).and_then(|n| n.to_f64())
}

fn int(dict: &Dict, key: &'static str) -> Option<i64> {
    get::<CFNumber>(dict, key).and_then(|n| n.to_i64())
}

fn string(dict: &Dict, key: &'static str) -> Option<String> {
    get::<CFString>(dict, key).map(|s| s.to_string())
}

fn boolean(dict: &Dict, key: &'static str) -> Option<bool> {
    get::<CFBoolean>(dict, key)
        .map(|b| b == CFBoolean::true_value())
        .or_else(|| num(dict, key).map(|n| n != 0.0))
}

fn bounds(dict: &Dict) -> Option<Rect> {
    let v = dict.find(CFString::from_static_string("kCGWindowBounds"))?;
    let mut rect = crate::ffi::CGRect::default();
    let ok = unsafe {
        crate::ffi::CGRectMakeWithDictionaryRepresentation(
            v.as_CFTypeRef() as core_foundation::dictionary::CFDictionaryRef,
            &mut rect,
        )
    };
    ok.then_some(Rect {
        x: rect.origin.x,
        y: rect.origin.y,
        w: rect.size.width,
        h: rect.size.height,
    })
}

/// All windows known to the window server (layer 0 = normal app windows;
/// other layers kept so callers can inspect overlays/menus if they want).
pub fn list_windows() -> Result<Vec<Window>, DriverError> {
    let raw =
        unsafe { CGWindowListCopyWindowInfo(kCGWindowListExcludeDesktopElements, kCGNullWindowID) };
    if raw.is_null() {
        return Err(DriverError::Platform(
            "CGWindowListCopyWindowInfo returned null".into(),
        ));
    }
    let list: CFArray<Dict> = unsafe { CFArray::wrap_under_create_rule(raw) };
    let mut out = Vec::with_capacity(list.len() as usize);
    for dict in list.iter() {
        let dict: &Dict = &dict;
        let (Some(id), Some(pid), Some(app), Some(rect)) = (
            int(dict, "kCGWindowNumber"),
            int(dict, "kCGWindowOwnerPID"),
            string(dict, "kCGWindowOwnerName"),
            bounds(dict),
        ) else {
            continue;
        };
        out.push(Window {
            id: id as u32,
            pid: pid as i32,
            app,
            title: string(dict, "kCGWindowName"),
            bounds: rect,
            on_screen: boolean(dict, "kCGWindowIsOnscreen").unwrap_or(false),
            layer: int(dict, "kCGWindowLayer").unwrap_or(0) as i32,
        });
    }
    Ok(out)
}
