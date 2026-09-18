//! Native macOS CGEventTap-based hotkey listener.
//!
//! Replaces the `rdev` crate on macOS because `rdev::listen` calls
//! `TSMGetInputSourceProperty` from its listener thread, which asserts it is
//! the main thread on macOS 26+ and aborts the process the first time any
//! key event arrives (EXC_BREAKPOINT from `_dispatch_assert_queue_fail`).
//!
//! This implementation reads only the raw keycode and modifier flags from
//! each event, never asks macOS for key names, and therefore never touches
//! TSM APIs.

#![cfg(target_os = "macos")]

use std::os::raw::c_void;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use core_foundation::base::TCFType;
use core_foundation::runloop::{kCFRunLoopCommonModes, CFRunLoop};

use crate::hotkey::{HotkeyEvent, HotkeyMode};

// --- Raw CoreGraphics bindings -------------------------------------------
//
// We only need a small slice of the CGEventTap API; the `core-graphics`
// crate's high-level wrappers don't cover the parts we need (flags-changed
// events, raw keycode field access), so we declare the minimum FFI here.

#[repr(C)]
struct OpaqueCGEvent {
    _private: [u8; 0],
}
type CGEventRef = *mut OpaqueCGEvent;

#[repr(C)]
struct OpaqueCFMachPort {
    _private: [u8; 0],
}
type CFMachPortRef = *mut OpaqueCFMachPort;

#[repr(C)]
struct OpaqueCFRunLoopSource {
    _private: [u8; 0],
}
type CFRunLoopSourceRef = *mut OpaqueCFRunLoopSource;

type CGEventTapCallBack = extern "C" fn(
    proxy: *mut c_void,
    event_type: u32,
    event: CGEventRef,
    user_info: *mut c_void,
) -> CGEventRef;

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGEventTapCreate(
        tap: u32,
        place: u32,
        options: u32,
        events_of_interest: u64,
        callback: CGEventTapCallBack,
        user_info: *mut c_void,
    ) -> CFMachPortRef;

    fn CGEventTapEnable(tap: CFMachPortRef, enable: bool);

    fn CGEventGetFlags(event: CGEventRef) -> u64;

    fn CGEventGetIntegerValueField(event: CGEventRef, field: u32) -> i64;

    fn CGEventSourceFlagsState(state_id: u32) -> u64;

    fn CGEventSourceKeyState(state_id: u32, keycode: u16) -> bool;
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFMachPortCreateRunLoopSource(
        allocator: *mut c_void,
        port: CFMachPortRef,
        order: isize,
    ) -> CFRunLoopSourceRef;

    fn CFRunLoopAddSource(rl: *mut c_void, source: CFRunLoopSourceRef, mode: *const c_void);
}

#[link(name = "IOKit", kind = "framework")]
extern "C" {
    fn IOHIDCheckAccess(request_type: u32) -> u32;
    fn IOHIDRequestAccess(request_type: u32) -> bool;
}

const K_IOHID_REQUEST_TYPE_LISTEN_EVENT: u32 = 1;
const K_IOHID_ACCESS_TYPE_GRANTED: u32 = 0;

/// Whether macOS lets us observe keyboard events from *other* apps. Without
/// this permission a listen-only event tap still gets created, but silently
/// only receives events aimed at our own app — the hotkey then appears to
/// work only while FlowingThoughts is focused.
pub fn input_monitoring_granted() -> bool {
    unsafe { IOHIDCheckAccess(K_IOHID_REQUEST_TYPE_LISTEN_EVENT) == K_IOHID_ACCESS_TYPE_GRANTED }
}

/// Show the system Input Monitoring prompt (and register the app in the
/// System Settings list). Returns the resulting grant state.
pub fn request_input_monitoring() -> bool {
    unsafe { IOHIDRequestAccess(K_IOHID_REQUEST_TYPE_LISTEN_EVENT) }
}

// --- Constants -----------------------------------------------------------

const K_CG_HID_EVENT_TAP: u32 = 0;
const K_CG_HEAD_INSERT_EVENT_TAP: u32 = 0;
const K_CG_EVENT_TAP_OPTION_LISTEN_ONLY: u32 = 1;

const K_CG_EVENT_KEY_DOWN: u32 = 10;
const K_CG_EVENT_KEY_UP: u32 = 11;
const K_CG_EVENT_FLAGS_CHANGED: u32 = 12;
const K_CG_EVENT_TAP_DISABLED_BY_TIMEOUT: u32 = 0xFFFFFFFE;
const K_CG_EVENT_TAP_DISABLED_BY_USER_INPUT: u32 = 0xFFFFFFFF;

const K_CG_KEYBOARD_EVENT_KEYCODE: u32 = 9;

// NSEvent modifier flag mask for the Fn / Globe key.
const NS_EVENT_MODIFIER_FLAG_FUNCTION: u64 = 1 << 23;
// Standard Cocoa modifier masks.
const NS_EVENT_MODIFIER_FLAG_COMMAND: u64 = 1 << 20;
const NS_EVENT_MODIFIER_FLAG_SHIFT: u64 = 1 << 17;

// Virtual keycodes from <HIToolbox/Events.h>.
const KC_SPACE: i64 = 49;

// kCGEventSourceStateCombinedSessionState — the flags/key state the session
// actually sees, queryable at any time regardless of whether our event tap
// received the underlying events.
const K_CG_EVENT_SOURCE_STATE_COMBINED_SESSION: u32 = 0;

fn event_mask() -> u64 {
    (1u64 << K_CG_EVENT_KEY_DOWN) | (1u64 << K_CG_EVENT_KEY_UP) | (1u64 << K_CG_EVENT_FLAGS_CHANGED)
}

// --- Listener state ------------------------------------------------------
//
// The C tap callback needs a way to reach the hotkey mode + sender, so we
// park them in a heap-allocated context struct and pass a raw pointer as
// the `user_info` argument. The context lives as long as the listener
// thread (i.e. the process), so leaking it via Box::into_raw is fine.

struct TapContext {
    mode_state: Arc<Mutex<HotkeyMode>>,
    tx: mpsc::Sender<HotkeyEvent>,
    state: Arc<Mutex<HotkeyFsm>>,
    /// The CFMachPortRef of our event tap, stored as usize once created.
    /// Needed so the callback can re-enable the tap after macOS disables it
    /// (kCGEventTapDisabledByTimeout) — without this the hotkey silently
    /// stops working until the app is restarted.
    tap_port: AtomicUsize,
}

#[derive(Default)]
struct HotkeyFsm {
    recording_active: bool,
    last_start_at: Option<Instant>,
    fn_was_down: bool,
    cmd_was_down: bool,
    shift_was_down: bool,
    space_was_down: bool,
}

impl HotkeyFsm {
    fn start_debounce(&mut self) -> bool {
        let now = Instant::now();
        let ok = self
            .last_start_at
            .map(|last| now.duration_since(last) >= Duration::from_millis(80))
            .unwrap_or(true);
        if ok {
            self.last_start_at = Some(now);
        }
        ok
    }
}

// --- Release failsafe ----------------------------------------------------
//
// The event tap is the only source of RecordStop, and macOS can swallow the
// release event (sleep, screen lock, secure input, tap disabled by timeout).
// When that happens the FSM stays in `recording_active` until the next
// keyboard event — which, if the user walks away or watches a video, is
// minutes or hours later, and the whole ambient capture gets transcribed and
// pasted. So while recording we also poll the *physical* key state via
// CGEventSourceFlagsState/KeyState and synthesize the stop ourselves when
// the hotkey is demonstrably up.

const RELEASE_POLL_INTERVAL: Duration = Duration::from_millis(250);

/// Pure debounce logic for the failsafe poller, split out so it's testable
/// without CoreGraphics.
///
/// Two safety properties:
/// - Fires only after two consecutive polls observe the hotkey up, so a
///   single glitchy read can't kill a live dictation.
/// - Fires only if a poll observed the hotkey *down* earlier in the same
///   recording. On keyboards whose Fn never reaches the session flags state,
///   the query would read "up" throughout a genuine hold — the `saw_held`
///   gate means we never trust a source that can't see the key at all.
#[derive(Default)]
struct ReleaseFailsafe {
    saw_held: bool,
    released_polls: u32,
}

impl ReleaseFailsafe {
    /// Feed one poll observation; returns true when the missed-release stop
    /// should fire.
    fn observe(&mut self, recording: bool, physically_held: bool) -> bool {
        if !recording {
            self.saw_held = false;
            self.released_polls = 0;
            return false;
        }
        if physically_held {
            self.saw_held = true;
            self.released_polls = 0;
            return false;
        }
        if !self.saw_held {
            return false;
        }
        self.released_polls += 1;
        if self.released_polls >= 2 {
            self.saw_held = false;
            self.released_polls = 0;
            return true;
        }
        false
    }
}

fn hotkey_physically_held(mode: HotkeyMode) -> bool {
    let flags = unsafe { CGEventSourceFlagsState(K_CG_EVENT_SOURCE_STATE_COMBINED_SESSION) };
    match mode {
        HotkeyMode::Fn => flags & NS_EVENT_MODIFIER_FLAG_FUNCTION != 0,
        HotkeyMode::CmdShiftSpace => {
            let space_down = unsafe {
                CGEventSourceKeyState(K_CG_EVENT_SOURCE_STATE_COMBINED_SESSION, KC_SPACE as u16)
            };
            flags & NS_EVENT_MODIFIER_FLAG_COMMAND != 0
                && flags & NS_EVENT_MODIFIER_FLAG_SHIFT != 0
                && space_down
        }
    }
}

fn start_release_failsafe(
    mode_state: Arc<Mutex<HotkeyMode>>,
    fsm: Arc<Mutex<HotkeyFsm>>,
    tx: mpsc::Sender<HotkeyEvent>,
) {
    thread::spawn(move || {
        let mut failsafe = ReleaseFailsafe::default();
        loop {
            thread::sleep(RELEASE_POLL_INTERVAL);
            let recording = fsm.lock().map(|g| g.recording_active).unwrap_or(false);
            let mode = mode_state.lock().map(|g| *g).unwrap_or(HotkeyMode::Fn);
            let held = recording && hotkey_physically_held(mode);
            if !failsafe.observe(recording, held) {
                continue;
            }
            // Re-check under the lock — a real release event may have won the
            // race since the poll; only synthesize the stop if we're still
            // stuck in recording.
            if let Ok(mut guard) = fsm.lock() {
                if guard.recording_active {
                    guard.recording_active = false;
                    guard.fn_was_down = false;
                    guard.space_was_down = false;
                    let _ = tx.send(HotkeyEvent::RecordStop);
                    let _ = crate::storage::append_log(
                        "WARN",
                        "Hotkey release failsafe fired — physical key state shows the hotkey is up but no release event was delivered (sleep, screen lock, or dropped tap event)",
                    );
                }
            }
        }
    });
}

extern "C" fn tap_callback(
    _proxy: *mut c_void,
    event_type: u32,
    event: CGEventRef,
    user_info: *mut c_void,
) -> CGEventRef {
    if user_info.is_null() {
        return event;
    }

    // If macOS disabled our tap (timeout or user interrupt), re-enable it
    // immediately — otherwise the hotkey stops working until app restart.
    if event_type == K_CG_EVENT_TAP_DISABLED_BY_TIMEOUT
        || event_type == K_CG_EVENT_TAP_DISABLED_BY_USER_INPUT
    {
        let ctx: &TapContext = unsafe { &*(user_info as *const TapContext) };
        let port = ctx.tap_port.load(Ordering::Acquire);
        if port != 0 {
            unsafe { CGEventTapEnable(port as CFMachPortRef, true) };
            eprintln!("Hotkey event tap was disabled by macOS — re-enabled.");
        }
        return event;
    }
    // SAFETY: we allocated this Box and leaked it; pointer is valid.
    let ctx: &TapContext = unsafe { &*(user_info as *const TapContext) };

    let mode = ctx.mode_state.lock().map(|g| *g).unwrap_or(HotkeyMode::Fn);

    let mut fsm = match ctx.state.lock() {
        Ok(g) => g,
        Err(_) => return event,
    };

    // SAFETY: `event` is a valid CGEventRef provided by the OS for the
    // duration of this callback.
    let flags = unsafe { CGEventGetFlags(event) };
    let keycode = unsafe { CGEventGetIntegerValueField(event, K_CG_KEYBOARD_EVENT_KEYCODE) };

    match mode {
        HotkeyMode::Fn => {
            // macOS sets the secondary-Fn modifier flag on the *navigation*
            // keys (arrows, Home/End, Page Up/Down, forward-delete) and the
            // F-row — not just the physical Fn/Globe key. So reading the flag
            // off an arbitrary keyDown falsely fires on a bare arrow press or a
            // Shift+Option+Arrow selection. The real Fn key announces itself
            // through a flagsChanged event (arrow keys are keyDown/keyUp), so
            // only trust the flag when it arrives on flagsChanged.
            if event_type == K_CG_EVENT_FLAGS_CHANGED {
                let fn_down = (flags & NS_EVENT_MODIFIER_FLAG_FUNCTION) != 0;
                if fn_down && !fsm.fn_was_down {
                    fsm.fn_was_down = true;
                    if !fsm.recording_active && fsm.start_debounce() {
                        fsm.recording_active = true;
                        let _ = ctx.tx.send(HotkeyEvent::RecordStart);
                    }
                } else if !fn_down && fsm.fn_was_down {
                    fsm.fn_was_down = false;
                    if fsm.recording_active {
                        fsm.recording_active = false;
                        let _ = ctx.tx.send(HotkeyEvent::RecordStop);
                    }
                }
            }
        }
        HotkeyMode::CmdShiftSpace => {
            let cmd_down = (flags & NS_EVENT_MODIFIER_FLAG_COMMAND) != 0;
            let shift_down = (flags & NS_EVENT_MODIFIER_FLAG_SHIFT) != 0;
            fsm.cmd_was_down = cmd_down;
            fsm.shift_was_down = shift_down;

            if event_type == K_CG_EVENT_KEY_DOWN && keycode == KC_SPACE {
                fsm.space_was_down = true;
            } else if event_type == K_CG_EVENT_KEY_UP && keycode == KC_SPACE {
                fsm.space_was_down = false;
            }

            let combo = fsm.cmd_was_down && fsm.shift_was_down && fsm.space_was_down;
            if combo && !fsm.recording_active && fsm.start_debounce() {
                fsm.recording_active = true;
                let _ = ctx.tx.send(HotkeyEvent::RecordStart);
            } else if !combo && fsm.recording_active {
                fsm.recording_active = false;
                let _ = ctx.tx.send(HotkeyEvent::RecordStop);
            }
        }
    }

    event
}

pub fn start_listener(mode_state: Arc<Mutex<HotkeyMode>>) -> mpsc::Receiver<HotkeyEvent> {
    let (tx, rx) = mpsc::channel();

    let fsm = Arc::new(Mutex::new(HotkeyFsm::default()));
    start_release_failsafe(mode_state.clone(), fsm.clone(), tx.clone());

    let ctx = Box::new(TapContext {
        mode_state,
        tx,
        state: fsm,
        tap_port: AtomicUsize::new(0),
    });
    // Pass the context pointer through the thread boundary as `usize`.
    // The raw `*mut c_void` isn't `Send`, and wrapping it in a newtype
    // with an unsafe Send impl still trips the closure Send check; going
    // through usize avoids the issue entirely.
    let ctx_addr = Box::into_raw(ctx) as usize;

    thread::spawn(move || {
        let ctx_ptr = ctx_addr as *mut c_void;

        // Without Input Monitoring the tap only sees our own app's events,
        // making the hotkey appear dead outside FlowingThoughts. Ask for it
        // up front so the user gets the system prompt on first launch.
        if !input_monitoring_granted() && !request_input_monitoring() {
            eprintln!(
                "Input Monitoring permission missing — the dictation hotkey will only work while FlowingThoughts is focused. Enable it in System Settings → Privacy & Security → Input Monitoring."
            );
        }

        // SAFETY: CGEventTapCreate requires Accessibility permission. If
        // it's missing we get NULL back and log — no crash.
        let tap = unsafe {
            CGEventTapCreate(
                K_CG_HID_EVENT_TAP,
                K_CG_HEAD_INSERT_EVENT_TAP,
                K_CG_EVENT_TAP_OPTION_LISTEN_ONLY,
                event_mask(),
                tap_callback,
                ctx_ptr,
            )
        };
        if tap.is_null() {
            eprintln!(
                "CGEventTapCreate returned null — grant FlowingThoughts Accessibility permission."
            );
            return;
        }
        // SAFETY: ctx was leaked via Box::into_raw and lives for the process.
        unsafe {
            (*(ctx_ptr as *const TapContext))
                .tap_port
                .store(tap as usize, Ordering::Release);
        }

        let source = unsafe { CFMachPortCreateRunLoopSource(std::ptr::null_mut(), tap, 0) };
        if source.is_null() {
            eprintln!("CFMachPortCreateRunLoopSource returned null");
            return;
        }

        let run_loop = CFRunLoop::get_current();
        unsafe {
            CFRunLoopAddSource(
                run_loop.as_concrete_TypeRef() as *mut c_void,
                source,
                kCFRunLoopCommonModes as *const c_void,
            );
            CGEventTapEnable(tap, true);
        }

        // This blocks forever and drives the callback. Drops out only if the
        // runloop is torn down (process exit).
        CFRunLoop::run_current();
    });

    rx
}

#[cfg(test)]
mod tests {
    use super::ReleaseFailsafe;

    #[test]
    fn failsafe_fires_after_two_released_polls_when_key_was_seen_held() {
        let mut f = ReleaseFailsafe::default();
        assert!(!f.observe(true, true)); // key down, recording
        assert!(!f.observe(true, false)); // first released poll — debounce
        assert!(f.observe(true, false)); // second released poll — fire
    }

    #[test]
    fn failsafe_never_fires_if_key_state_never_showed_held() {
        // Keyboards whose Fn never reaches the session flags state read "up"
        // for the whole recording — the failsafe must not kill the session.
        let mut f = ReleaseFailsafe::default();
        for _ in 0..100 {
            assert!(!f.observe(true, false));
        }
    }

    #[test]
    fn failsafe_debounce_resets_when_key_reads_held_again() {
        let mut f = ReleaseFailsafe::default();
        assert!(!f.observe(true, true));
        assert!(!f.observe(true, false)); // glitchy single read
        assert!(!f.observe(true, true)); // key is actually still down
        assert!(!f.observe(true, false));
        assert!(f.observe(true, false));
    }

    #[test]
    fn failsafe_resets_between_recordings() {
        let mut f = ReleaseFailsafe::default();
        assert!(!f.observe(true, true));
        assert!(!f.observe(false, false)); // recording ended normally
                                           // New recording: needs to see the key held again before it can fire.
        assert!(!f.observe(true, false));
        assert!(!f.observe(true, false));
        assert!(!f.observe(true, false));
    }

    #[test]
    fn failsafe_idle_polls_do_nothing() {
        let mut f = ReleaseFailsafe::default();
        for _ in 0..10 {
            assert!(!f.observe(false, false));
        }
    }
}
