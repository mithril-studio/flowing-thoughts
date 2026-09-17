//! OWNER: WP5 (capture). Core Audio property listeners.
//!
//! Must provide:
//!
//! - Default output and default input device changes, delivered off the HAL
//!   notification thread, so `system_tap.rs` and `mic.rs` can rebuild.
//!   Target from the spike: recovery in under 2 s.
//! - "Is the current output the built-in speakers?", which the session (WP7)
//!   stores as `meetings.echo_risk` and shows as "Use headphones for best
//!   results".
