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

/// `AXValueType` values we decode — the `CF_ENUM` order in the SDK's
/// `AXValue.h` (Unknown=0, CGPoint, CGSize, CGRect, CFRange, AXError,
/// Illegal). The error slot in a batched
/// `AXUIElementCopyMultipleAttributeValues` result is `AXError`.
pub const K_AX_VALUE_CG_POINT_TYPE: i32 = 1;
pub const K_AX_VALUE_CG_SIZE_TYPE: i32 = 2;
pub const K_AX_VALUE_AX_ERROR_TYPE: i32 = 5;

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    pub fn AXIsProcessTrusted() -> CfBoolean;
    pub fn AXIsProcessTrustedWithOptions(options: CFDictionaryRef) -> CfBoolean;
    pub static kAXTrustedCheckOptionPrompt: CFStringRef;
    /// The `AXValue` CFTypeID — `AXValueGetType` is only defined on
    /// AXValue instances, so callers must check this first.
    pub fn AXValueGetTypeID() -> usize;
    pub fn AXValueGetType(value: CFTypeRef) -> i32;
    pub fn AXValueGetValue(value: CFTypeRef, theType: i32, valuePtr: *mut c_void) -> CfBoolean;
    /// Create an AXValue wrapping a CGPoint/CGSize — needed to *set*
    /// AXPosition/AXSize (window move/resize).
    pub fn AXValueCreate(theType: i32, valuePtr: *const c_void) -> CFTypeRef;
    /// Fetch several attributes in one IPC roundtrip — the documented
    /// fast path for tree walks. `values` is a CFArray parallel to
    /// `attributes`; unfetchable slots arrive as AXValue-wrapped
    /// AXError (type `kAXValueAXErrorType`).
    pub fn AXUIElementCopyMultipleAttributeValues(
        element: CFTypeRef,
        attributes: core_foundation::array::CFArrayRef,
        options: u32,
        values: *mut core_foundation::array::CFArrayRef,
    ) -> i32;
}

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    pub fn CGPreflightScreenCaptureAccess() -> bool;
    pub fn CGRequestScreenCaptureAccess() -> bool;
    /// Decode a kCGWindowBounds dictionary into a CGRect.
    pub fn CGRectMakeWithDictionaryRepresentation(dict: CFDictionaryRef, rect: *mut CGRect)
        -> bool;

    // --- Synthetic input (CGEvent). Only used when the caller explicitly
    // enabled physical input — these can move the real cursor / type into
    // whatever app is frontmost. ---
    pub fn CGEventCreateMouseEvent(
        source: CFTypeRef,
        mouse_type: u32,
        position: CGPoint,
        button: u32,
    ) -> CFTypeRef;
    pub fn CGEventCreateKeyboardEvent(
        source: CFTypeRef,
        virtual_key: u16,
        key_down: bool,
    ) -> CFTypeRef;
    pub fn CGEventKeyboardSetUnicodeString(event: CFTypeRef, length: usize, chars: *const u16);
    pub fn CGEventCreateScrollWheelEvent(
        source: CFTypeRef,
        units: u32,
        wheel_count: u32,
        wheel1: i32,
        wheel2: i32,
    ) -> CFTypeRef;
    pub fn CGEventSetFlags(event: CFTypeRef, flags: u64);
    /// Generic integer field setter — used for `kCGMouseEventClickState`
    /// (multi-click) on mouse events.
    pub fn CGEventSetIntegerValueField(event: CFTypeRef, field: u32, value: i64);
    pub fn CGEventPost(tap_location: u32, event: CFTypeRef);
    pub fn CFRelease(cf: CFTypeRef);
}

/// CGEvent mouse event types.
pub const K_CG_EVENT_LEFT_DOWN: u32 = 1;
pub const K_CG_EVENT_LEFT_UP: u32 = 2;
pub const K_CG_EVENT_RIGHT_DOWN: u32 = 3;
pub const K_CG_EVENT_RIGHT_UP: u32 = 4;
pub const K_CG_EVENT_LEFT_DRAGGED: u32 = 6;
pub const K_CG_EVENT_MIDDLE_DOWN: u32 = 10;
pub const K_CG_EVENT_MIDDLE_UP: u32 = 11;
/// `kCGMouseEventClickState` (`CGEventTypes.h`) — the click-count
/// field on mouse down/up events (1 = single, 2 = double...). Field
/// 23 is a scroll-wheel delta axis — the wrong constant here silently
/// downgraded multi-clicks to repeated singles.
pub const K_CG_MOUSE_EVENT_CLICK_STATE: u32 = 1;
/// CGEventTapLocation — post at the HID level (before session routing).
pub const K_CG_HID_EVENT_TAP: u32 = 0;
/// Mouse buttons for CGEventCreateMouseEvent.
pub const K_CG_MOUSE_LEFT: u32 = 0;
pub const K_CG_MOUSE_RIGHT: u32 = 1;
pub const K_CG_MOUSE_MIDDLE: u32 = 2;
/// Scroll event units: lines.
pub const K_CG_SCROLL_UNIT_LINE: u32 = 1;
/// CGEventFlags.
pub const K_CG_FLAG_SHIFT: u64 = 0x0002_0000;
pub const K_CG_FLAG_CONTROL: u64 = 0x0004_0000;
pub const K_CG_FLAG_ALT: u64 = 0x0008_0000;
pub const K_CG_FLAG_CMD: u64 = 0x0010_0000;

#[cfg(test)]
mod tests {
    use super::*;

    // The AXValueType CF_ENUM order is fixed by the SDK (`AXValue.h`):
    // a wrong constant silently mis-decodes every batched read — the
    // AXError slot (5) was once written as 3, which is CGRect.
    #[test]
    fn ax_value_type_constants_match_the_sdk() {
        assert_eq!(K_AX_VALUE_CG_POINT_TYPE, 1); // kAXValueCGPointType
        assert_eq!(K_AX_VALUE_CG_SIZE_TYPE, 2); // kAXValueCGSizeType
                                                // 3 is kAXValueCGRectType, 4 is kAXValueCFRangeType — neither
                                                // is the error slot.
        assert_eq!(K_AX_VALUE_AX_ERROR_TYPE, 5); // kAXValueAXErrorType
    }

    // CGEvent field ids are fixed by `CGEventTypes.h`: click state is
    // field 1 — writing the count anywhere else degrades multi-clicks
    // to repeated singles the OS happens to merge.
    #[test]
    fn cg_event_field_constants_match_the_sdk() {
        assert_eq!(K_CG_MOUSE_EVENT_CLICK_STATE, 1); // kCGMouseEventClickState
        assert_eq!(K_CG_EVENT_LEFT_DOWN, 1); // kCGEventLeftMouseDown
        assert_eq!(K_CG_EVENT_LEFT_UP, 2); // kCGEventLeftMouseUp
        assert_eq!(K_CG_EVENT_RIGHT_DOWN, 3); // kCGEventRightMouseDown
        assert_eq!(K_CG_EVENT_RIGHT_UP, 4); // kCGEventRightMouseUp
    }
}
