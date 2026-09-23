//! Raw externs for APIs the safe crates don't expose.
//! AXValue decoding (position/size attributes) and process trust live in
//! HIServices (reachable through the ApplicationServices umbrella); screen
//! capture preflight lives in CoreGraphics.

use core_foundation::base::CFTypeRef;
use core_foundation::dictionary::CFDictionaryRef;
use core_foundation::string::CFStringRef;
use std::ffi::c_void;

pub type CfBoolean = u8;

/// `CGRect` — origin + size, all `f64`.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct CGRect {
    pub origin: CGPoint,
    pub size: CGSize,
}

/// `CGPoint` / `CGSize` are `repr(C)` pairs of `f64`.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct CGPoint {
    pub x: f64,
    pub y: f64,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct CGSize {
    pub width: f64,
    pub height: f64,
}

/// AXValueType values we decode.
pub const K_AX_VALUE_CG_POINT_TYPE: i32 = 1;
pub const K_AX_VALUE_CG_SIZE_TYPE: i32 = 2;

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    pub fn AXIsProcessTrusted() -> CfBoolean;
    pub fn AXIsProcessTrustedWithOptions(options: CFDictionaryRef) -> CfBoolean;
    pub static kAXTrustedCheckOptionPrompt: CFStringRef;
    pub fn AXValueGetType(value: CFTypeRef) -> i32;
    pub fn AXValueGetValue(value: CFTypeRef, theType: i32, valuePtr: *mut c_void) -> CfBoolean;
}

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    pub fn CGPreflightScreenCaptureAccess() -> bool;
    pub fn CGRequestScreenCaptureAccess() -> bool;
    /// Decode a kCGWindowBounds dictionary into a CGRect.
    pub fn CGRectMakeWithDictionaryRepresentation(dict: CFDictionaryRef, rect: *mut CGRect)
        -> bool;
}
