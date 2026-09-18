//! One local inference at a time, with dictation first in line.
//!
//! Dictation and the meeting worker share the CPU/GPU and the loaded model.
//! Dictation is interactive: a person is waiting for the text. Meeting
//! transcription is background work that can be redone. So:
//!
//! - `acquire_interactive()` raises the preempt flag straight away, then waits
//!   for whoever holds the gate.
//! - `acquire_background()` waits while anyone holds the gate *or* an
//!   interactive caller is waiting for it, so dictation never queues behind a
//!   second window.
//! - `should_preempt()` is what the worker's Whisper abort callback polls. A
//!   background holder that sees `true` aborts its window, drops its guard and
//!   leaves the window `pending`.
//!
//! Guards are plain RAII values (no `MutexGuard` inside), so they are `Send`
//! and cost nothing to hold across a blocking decode.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex, MutexGuard};

pub struct Gate {
    held: Mutex<bool>,
    released: Condvar,
    /// Interactive callers waiting for or holding the gate. Non-zero is the
    /// preempt flag. Only decremented under the `held` lock, so a background
    /// waiter cannot miss the wake-up.
    interactive: AtomicUsize,
}

pub struct InteractiveGuard<'a> {
    gate: &'a Gate,
}

pub struct BackgroundGuard<'a> {
    gate: &'a Gate,
}

impl Gate {
    pub const fn new() -> Self {
        Self {
            held: Mutex::new(false),
            released: Condvar::new(),
            interactive: AtomicUsize::new(0),
        }
    }

    /// A poisoned gate must never wedge dictation: the protected state is a
    /// single bool, so the value is still meaningful after a panic elsewhere.
    fn lock(&self) -> MutexGuard<'_, bool> {
        self.held.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn acquire_interactive(&self) -> InteractiveGuard<'_> {
        self.interactive.fetch_add(1, Ordering::SeqCst);
        let mut held = self.lock();
        while *held {
            held = self.released.wait(held).unwrap_or_else(|e| e.into_inner());
        }
        *held = true;
        InteractiveGuard { gate: self }
    }

    pub fn acquire_background(&self) -> BackgroundGuard<'_> {
        let mut held = self.lock();
        while *held || self.should_preempt() {
            held = self.released.wait(held).unwrap_or_else(|e| e.into_inner());
        }
        *held = true;
        BackgroundGuard { gate: self }
    }

    /// True while an interactive caller is waiting for or holding the gate.
    pub fn should_preempt(&self) -> bool {
        self.interactive.load(Ordering::SeqCst) > 0
    }

    fn release(&self, interactive: bool) {
        let mut held = self.lock();
        *held = false;
        if interactive {
            self.interactive.fetch_sub(1, Ordering::SeqCst);
        }
        drop(held);
        self.released.notify_all();
    }
}

impl Drop for InteractiveGuard<'_> {
    fn drop(&mut self) {
        self.gate.release(true);
    }
}

impl Drop for BackgroundGuard<'_> {
    fn drop(&mut self) {
        self.gate.release(false);
    }
}

static GATE: Gate = Gate::new();

/// Dictation's entry. Blocks the calling thread — call it from a blocking
/// thread, never directly inside an async task.
pub fn acquire_interactive() -> InteractiveGuard<'static> {
    GATE.acquire_interactive()
}

/// The meeting worker's entry: hold the guard for one window, no longer.
#[allow(dead_code)] // scaffold: first used by meetings/worker.rs (WP6)
pub fn acquire_background() -> BackgroundGuard<'static> {
    GATE.acquire_background()
}

/// For the worker's abort callback.
#[allow(dead_code)] // scaffold: first used by meetings/worker.rs (WP6)
pub fn should_preempt() -> bool {
    GATE.should_preempt()
}

#[cfg(test)]
mod tests {
    use super::Gate;
    use std::sync::mpsc;
    use std::thread;
    use std::time::{Duration, Instant};

    const SHORT: Duration = Duration::from_millis(150);
    const LONG: Duration = Duration::from_secs(5);

    fn wait_until(what: &str, cond: impl Fn() -> bool) {
        let deadline = Instant::now() + LONG;
        while !cond() {
            assert!(Instant::now() < deadline, "timed out waiting until {what}");
            thread::sleep(Duration::from_millis(2));
        }
    }

    #[test]
    fn interactive_waits_for_background_holder_and_sets_preempt() {
        let gate = &Gate::new();
        thread::scope(|s| {
            let background = gate.acquire_background();
            assert!(!gate.should_preempt());

            let (acquired_tx, acquired_rx) = mpsc::channel();
            let (release_tx, release_rx) = mpsc::channel::<()>();
            s.spawn(move || {
                let _guard = gate.acquire_interactive();
                acquired_tx.send(()).unwrap();
                release_rx.recv().unwrap();
            });

            // The flag goes up while the interactive caller is still waiting.
            wait_until("preempt is raised", || gate.should_preempt());
            assert!(
                acquired_rx.recv_timeout(SHORT).is_err(),
                "interactive got the gate while background still held it"
            );

            drop(background);
            acquired_rx
                .recv_timeout(LONG)
                .expect("interactive never acquired");
            assert!(
                gate.should_preempt(),
                "preempt must stay up while interactive holds"
            );

            release_tx.send(()).unwrap();
        });
        assert!(!gate.should_preempt());
    }

    #[test]
    fn background_blocks_while_interactive_waits_or_holds() {
        let gate = &Gate::new();
        thread::scope(|s| {
            let first_background = gate.acquire_background();

            let (interactive_tx, interactive_rx) = mpsc::channel();
            let (release_tx, release_rx) = mpsc::channel::<()>();
            s.spawn(move || {
                let _guard = gate.acquire_interactive();
                interactive_tx.send(()).unwrap();
                release_rx.recv().unwrap();
            });
            wait_until("interactive is waiting", || gate.should_preempt());

            // A second background caller arrives while interactive waits.
            let (background_tx, background_rx) = mpsc::channel();
            s.spawn(move || {
                let _guard = gate.acquire_background();
                background_tx.send(()).unwrap();
            });

            // Freeing the gate hands it to the interactive caller, not to the
            // background caller, whichever of them woke first.
            drop(first_background);
            interactive_rx
                .recv_timeout(LONG)
                .expect("interactive never acquired");
            assert!(
                background_rx.recv_timeout(SHORT).is_err(),
                "background jumped ahead of an interactive caller"
            );

            release_tx.send(()).unwrap();
            background_rx
                .recv_timeout(LONG)
                .expect("background never acquired");
        });
    }

    #[test]
    fn background_does_not_start_while_interactive_holds() {
        let gate = &Gate::new();
        thread::scope(|s| {
            let interactive = gate.acquire_interactive();
            let (tx, rx) = mpsc::channel();
            s.spawn(move || {
                let _guard = gate.acquire_background();
                tx.send(()).unwrap();
            });
            assert!(rx.recv_timeout(SHORT).is_err());
            drop(interactive);
            rx.recv_timeout(LONG).expect("background never acquired");
        });
    }

    #[test]
    fn guards_release_on_drop() {
        let gate = &Gate::new();
        for _ in 0..3 {
            let guard = gate.acquire_background();
            assert!(!gate.should_preempt());
            drop(guard);

            let guard = gate.acquire_interactive();
            assert!(gate.should_preempt());
            drop(guard);
            assert!(!gate.should_preempt());
        }
        // Still free: a fresh acquire returns at once instead of deadlocking.
        let _guard = gate.acquire_background();
    }

    #[test]
    fn guards_release_when_the_holder_panics() {
        let gate = &Gate::new();
        thread::scope(|s| {
            let result = s
                .spawn(move || {
                    let _guard = gate.acquire_interactive();
                    panic!("decode blew up");
                })
                .join();
            assert!(result.is_err());
        });
        assert!(!gate.should_preempt());
        let _guard = gate.acquire_background();
    }
}
