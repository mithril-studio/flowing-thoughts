//! macOS Accessibility permission helpers.
//!
//! The first time any process calls `AXIsProcessTrustedWithOptions` with the
//! prompt option set, macOS adds that process to
//! System Settings > Privacy & Security > Accessibility, with a toggle the user
//! can flip on. Without this call (or another AX API that triggers TCC), dev
//! builds launched via `tauri dev` never show up in the list.

#![cfg(target_os = "macos")]

use core_foundation::base::TCFType;
use core_foundation::boolean::CFBoolean;
use core_foundation::dictionary::CFDictionary;
use core_foundation::string::CFString;
use core_foundation_sys::base::CFTypeRef;
use core_foundation_sys::dictionary::CFDictionaryRef;

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn AXIsProcessTrustedWithOptions(options: CFDictionaryRef) -> bool;
    static kAXTrustedCheckOptionPrompt: CFTypeRef;
}

/// Ask macOS whether this process is trusted for Accessibility.
///
/// When `prompt` is true and the process isn't trusted yet, macOS registers it
/// in the Accessibility list and surfaces a system dialog linking the user to
/// the settings pane. Safe to call repeatedly.
pub fn is_process_trusted(prompt: bool) -> bool {
    unsafe {
        let key = CFString::wrap_under_get_rule(kAXTrustedCheckOptionPrompt as _);
        let value = CFBoolean::from(prompt);
        let options = CFDictionary::from_CFType_pairs(&[(key, value)]);
        AXIsProcessTrustedWithOptions(options.as_concrete_TypeRef())
    }
}
