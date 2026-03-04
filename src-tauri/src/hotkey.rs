use rdev::{listen, Event, EventType, Key};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
pub enum HotkeyEvent {
    RecordStart,
    RecordStop,
}

#[derive(Debug, Clone, Copy)]
enum HotkeyMode {
    CmdShiftSpace,
    Fn,
}

impl HotkeyMode {
    fn from_env() -> Self {
        match std::env::var("OVW_HOTKEY")
            .unwrap_or_else(|_| "cmd+shift+space".to_string())
            .to_ascii_lowercase()
            .trim()
        {
            "fn" => Self::Fn,
            _ => Self::CmdShiftSpace,
        }
    }
}

/// Spawn a background thread that listens for global hotkeys.
/// Default hotkey mode: hold Cmd+Shift+Space to record.
/// Optional override for development: set `OVW_HOTKEY=fn`.
pub fn start_listener() -> mpsc::Receiver<HotkeyEvent> {
    let (tx, rx) = mpsc::channel();
    let mode = HotkeyMode::from_env();

    thread::spawn(move || {
        let mut cmd_down = false;
        let mut shift_down = false;
        let mut space_down = false;
        let mut recording_active = false;
        let mut last_start_at: Option<Instant> = None;
        let start_debounce = Duration::from_millis(80);

        if let Err(e) = listen(move |event: Event| {
            match mode {
                HotkeyMode::Fn => match event.event_type {
                    EventType::KeyPress(Key::Function) => {
                        if !recording_active {
                            recording_active = true;
                            let _ = tx.send(HotkeyEvent::RecordStart);
                        }
                    }
                    EventType::KeyRelease(Key::Function) => {
                        if recording_active {
                            recording_active = false;
                            let _ = tx.send(HotkeyEvent::RecordStop);
                        }
                    }
                    _ => {}
                },
                HotkeyMode::CmdShiftSpace => match event.event_type {
                    EventType::KeyPress(key) => {
                        if is_cmd_key(key) {
                            cmd_down = true;
                        } else if is_shift_key(key) {
                            shift_down = true;
                        } else if key == Key::Space {
                            space_down = true;
                        }

                        if cmd_down && shift_down && space_down && !recording_active {
                            let now = Instant::now();
                            let can_start = last_start_at
                                .map(|last| now.duration_since(last) >= start_debounce)
                                .unwrap_or(true);
                            if can_start {
                                recording_active = true;
                                last_start_at = Some(now);
                                let _ = tx.send(HotkeyEvent::RecordStart);
                            }
                        }
                    }
                    EventType::KeyRelease(key) => {
                        if is_cmd_key(key) {
                            cmd_down = false;
                        } else if is_shift_key(key) {
                            shift_down = false;
                        } else if key == Key::Space {
                            space_down = false;
                        }

                        let combo_still_held = cmd_down && shift_down && space_down;
                        if recording_active && !combo_still_held {
                            recording_active = false;
                            let _ = tx.send(HotkeyEvent::RecordStop);
                        }
                    }
                    _ => {}
                },
            }
        }) {
            eprintln!("Failed to listen for hotkey events: {:?}", e);
        }
    });

    rx
}

fn is_cmd_key(key: Key) -> bool {
    key == Key::MetaLeft || key == Key::MetaRight
}

fn is_shift_key(key: Key) -> bool {
    key == Key::ShiftLeft || key == Key::ShiftRight
}
