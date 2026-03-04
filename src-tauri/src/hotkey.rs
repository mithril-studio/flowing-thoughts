use rdev::{listen, Event, EventType, Key};
use std::sync::mpsc;
use std::thread;

#[derive(Debug, Clone)]
pub enum HotkeyEvent {
    RecordStart,
    RecordStop,
}

/// Spawn a background thread that listens for fn key press/release.
/// Returns a receiver that emits HotkeyEvents.
/// Requires macOS Accessibility permission.
pub fn start_listener() -> mpsc::Receiver<HotkeyEvent> {
    let (tx, rx) = mpsc::channel();

    thread::spawn(move || {
        let tx = tx;
        if let Err(e) = listen(move |event: Event| {
            match event.event_type {
                EventType::KeyPress(Key::Function) => {
                    let _ = tx.send(HotkeyEvent::RecordStart);
                }
                EventType::KeyRelease(Key::Function) => {
                    let _ = tx.send(HotkeyEvent::RecordStop);
                }
                _ => {}
            }
        }) {
            eprintln!("Failed to listen for hotkey events: {:?}", e);
        }
    });

    rx
}
