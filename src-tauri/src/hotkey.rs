use rdev::{listen, Event, EventType, Key};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
pub enum HotkeyEvent {
    RecordStart,
    RecordStop,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HotkeyMode {
    CmdShiftSpace,
    Fn,
}

impl HotkeyMode {
    pub fn parse(value: &str) -> Self {
        match value.to_ascii_lowercase().trim() {
            "fn" => Self::Fn,
            "cmd+shift+space" => Self::CmdShiftSpace,
            "cmd_shift_space" => Self::CmdShiftSpace,
            _ => Self::CmdShiftSpace,
        }
    }

    pub fn from_preset(preset: &str) -> Self {
        Self::parse(preset)
    }
}

pub fn mode_from_env() -> Option<HotkeyMode> {
    std::env::var("OVW_HOTKEY")
        .ok()
        .map(|raw| HotkeyMode::parse(&raw))
}

#[cfg(test)]
pub fn mode_to_preset(mode: HotkeyMode) -> &'static str {
    match mode {
        HotkeyMode::CmdShiftSpace => "cmd_shift_space",
        HotkeyMode::Fn => "fn",
    }
}

/// Spawn a background thread that listens for global hotkeys.
/// Default hotkey mode: hold Cmd+Shift+Space to record.
/// Optional override for development: set `OVW_HOTKEY=fn`.
pub fn start_listener(mode_state: Arc<Mutex<HotkeyMode>>) -> mpsc::Receiver<HotkeyEvent> {
    let (tx, rx) = mpsc::channel();

    thread::spawn(move || {
        let mut cmd_down = false;
        let mut shift_down = false;
        let mut space_down = false;
        let mut recording_active = false;
        let mut last_start_at: Option<Instant> = None;
        let start_debounce = Duration::from_millis(80);

        if let Err(e) = listen(move |event: Event| {
            let mode = mode_state
                .lock()
                .map(|guard| *guard)
                .unwrap_or(HotkeyMode::CmdShiftSpace);
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

#[cfg(test)]
mod tests {
    use super::{mode_to_preset, HotkeyMode};

    #[test]
    fn hotkey_mode_parse_supports_fn_override() {
        assert_eq!(HotkeyMode::parse("fn"), HotkeyMode::Fn);
        assert_eq!(HotkeyMode::parse("FN"), HotkeyMode::Fn);
    }

    #[test]
    fn hotkey_mode_parse_defaults_to_cmd_shift_space() {
        assert_eq!(HotkeyMode::parse("cmd+shift+space"), HotkeyMode::CmdShiftSpace);
        assert_eq!(HotkeyMode::parse("cmd_shift_space"), HotkeyMode::CmdShiftSpace);
        assert_eq!(HotkeyMode::parse("anything-else"), HotkeyMode::CmdShiftSpace);
        assert_eq!(HotkeyMode::parse(""), HotkeyMode::CmdShiftSpace);
    }

    #[test]
    fn hotkey_mode_preset_mapping_round_trips() {
        assert_eq!(mode_to_preset(HotkeyMode::Fn), "fn");
        assert_eq!(mode_to_preset(HotkeyMode::CmdShiftSpace), "cmd_shift_space");
    }
}
