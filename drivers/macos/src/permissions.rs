//! macOS permission state. Dexter is fail-closed: it reports what it can
//! prove, never assumes access.

use crate::ffi;
use core_foundation::base::TCFType;
use core_foundation::boolean::CFBoolean;
use core_foundation::dictionary::CFDictionary;
use core_foundation::string::CFString;

/// Whether this process is trusted for Accessibility (required to read AX
/// trees of other apps and to send input).
pub fn accessibility_trusted() -> bool {
    unsafe { ffi::AXIsProcessTrusted() != 0 }
}

/// Ask macOS to show the Accessibility grant prompt, then report trust.
/// The prompt is a system dialog; the user may deny it.
pub fn request_accessibility() -> bool {
    let options = unsafe {
        let key = CFString::wrap_under_get_rule(ffi::kAXTrustedCheckOptionPrompt);
        CFDictionary::from_CFType_pairs(&[(key.as_CFType(), CFBoolean::true_value().as_CFType())])
    };
    unsafe { ffi::AXIsProcessTrustedWithOptions(options.as_concrete_TypeRef()) != 0 }
}

/// Whether screen capture is allowed (window titles, pixel capture).
pub fn screen_capture_allowed() -> bool {
    unsafe { ffi::CGPreflightScreenCaptureAccess() }
}

/// Ask macOS to show the Screen Recording prompt. Returns current state.
pub fn request_screen_capture() -> bool {
    unsafe { ffi::CGRequestScreenCaptureAccess() }
}
