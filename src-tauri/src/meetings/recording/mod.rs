//! OWNER: WP4 (recording). From captured frames to chunk files on disk.
//! Nothing in the scaffold calls into this module yet; WP5 feeds it and WP7
//! drives it.
//!
//! Data flow per track:
//!
//! ```text
//! AudioSource --on_frames--> rtrb ring (~5 s) --writer thread--> resample
//!   --> timeline --> SampleSink (chunk_writer) --> <seq>.pcm + ChunkLedger
//! ```
//!
//! This module must provide:
//!
//! - A recorder per track that implements `types::AudioSourceHandler`. Its
//!   `on_frames` only copies into the ring; overflow is counted, never
//!   blocked on, and recorded as a gap.
//! - `pause` / `resume` (paused audio is dropped and shows up as a gap) and a
//!   `stop` that drains the ring and finishes the sink.
//! - `meetings_root()`: `~/Library/Application Support/FlowingThoughts/meetings`,
//!   and the layout under it, `<meeting_id>/<track>/<seq>.pcm`, where
//!   `<track>` is `TrackKind::as_str()`. Chunk rows store the path relative
//!   to the root.
//! - `delete_meeting_audio(meeting_id)` and `audio_bytes(meeting_id)` for the
//!   session (WP7) and the store's DTOs.
//! - A `types::TrackAudio` implementation over a track's `ChunkRecord`s, for
//!   long-form decoding. Gaps and deleted chunks read as silence.
//!
//! Where it lives:
//!
//! | File | What |
//! |---|---|
//! | `recorder.rs` | `start_track`: the ring, the writer thread, pause/resume/stop, the error state |
//! | `resample.rs` | `StreamResampler`: any format to 16 kHz mono |
//! | `timeline.rs` | `Timeline`: runs, gaps, drift, chunk anchors (pure) |
//! | `chunk_writer.rs` | `ChunkWriter`: the real `SampleSink`, `<seq>.pcm` files |
//! | `sidecar.rs` | `track.json`: the chunk list next to the files, for recovery without the DB |
//! | `recovery.rs` | repairing chunks a crash or quit left open |
//! | `reader.rs` | `ChunkTrackAudio`: the `TrackAudio` over chunk files |
//!
//! Format: raw `s16le`, 16 kHz (`types::TARGET_SAMPLE_RATE`), mono. No header
//! to finalize. The writer calls `write()` at least once a second and
//! `fsync`s when a chunk closes, because the app exits through `_exit(0)`.
//!
//! Tests need no hardware: fake `AudioSource`, fake `SampleSink`, fake
//! `ChunkLedger`, a temp directory.

// Nothing calls into this module until WP5 (capture) and WP7 (session) land.
// Remove once they do.
#![allow(dead_code)]

pub mod chunk_writer;
pub mod reader;
pub mod recorder;
pub mod recovery;
pub mod resample;
pub mod sidecar;
pub mod timeline;

use std::fs;
use std::path::{Path, PathBuf};

use super::types::{TrackKind, TARGET_SAMPLE_RATE};

// Unused for the same reason as the `dead_code` allow above.
#[allow(unused_imports)]
pub use chunk_writer::{ChunkWriter, ChunkWriterConfig};
#[allow(unused_imports)]
pub use reader::{AudioSpan, ChunkTrackAudio};
#[allow(unused_imports)]
pub use recorder::{
    record_to_disk, start_track, RecorderConfig, RecorderHandler, RecorderStatus, TrackRecorder,
};

/// Length of a full chunk file.
pub const CHUNK_SECONDS: u64 = 60;
pub const CHUNK_FRAMES: u64 = CHUNK_SECONDS * TARGET_SAMPLE_RATE as u64;
/// s16le mono.
pub const BYTES_PER_FRAME: u64 = 2;
pub const CHUNK_EXTENSION: &str = "pcm";

/// `~/Library/Application Support/FlowingThoughts/meetings`. Everything below
/// takes the root as a parameter so tests can point it at a temp directory.
pub fn meetings_root() -> Result<PathBuf, String> {
    let home = std::env::var("HOME").map_err(|_| "HOME environment variable not set".to_string())?;
    Ok(PathBuf::from(home)
        .join("Library")
        .join("Application Support")
        .join("FlowingThoughts")
        .join("meetings"))
}

/// Meeting ids become directory names, and `delete_meeting_audio` removes
/// that directory: refuse anything that could point somewhere else.
fn validate_meeting_id(meeting_id: &str) -> Result<(), String> {
    let ok = !meeting_id.is_empty()
        && meeting_id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if ok {
        Ok(())
    } else {
        Err(format!("Invalid meeting id '{meeting_id}'"))
    }
}

pub fn meeting_dir(root: &Path, meeting_id: &str) -> Result<PathBuf, String> {
    validate_meeting_id(meeting_id)?;
    Ok(root.join(meeting_id))
}

pub fn track_dir(root: &Path, meeting_id: &str, kind: TrackKind) -> Result<PathBuf, String> {
    Ok(meeting_dir(root, meeting_id)?.join(kind.as_str()))
}

/// `000042.pcm`
pub fn chunk_file_name(seq: u32) -> String {
    format!("{seq:06}.{CHUNK_EXTENSION}")
}

/// What `ChunkRecord::path` holds: `<meeting_id>/<track>/<seq:06>.pcm`,
/// relative to the meetings root, always with forward slashes.
pub fn chunk_rel_path(meeting_id: &str, kind: TrackKind, seq: u32) -> String {
    format!("{meeting_id}/{}/{}", kind.as_str(), chunk_file_name(seq))
}

/// Removes a meeting's audio directory (chunk files and sidecars). A meeting
/// without audio on disk is fine. The caller marks the chunk rows `deleted`.
pub fn delete_meeting_audio(meeting_id: &str) -> Result<(), String> {
    delete_meeting_audio_in(&meetings_root()?, meeting_id)
}

pub fn delete_meeting_audio_in(root: &Path, meeting_id: &str) -> Result<(), String> {
    let dir = meeting_dir(root, meeting_id)?;
    match fs::remove_dir_all(&dir) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("Failed to delete meeting audio: {e}")),
    }
}

/// Bytes of chunk audio still on disk for a meeting; 0 when there is none.
pub fn audio_bytes(meeting_id: &str) -> u64 {
    meetings_root().map(|root| audio_bytes_in(&root, meeting_id)).unwrap_or(0)
}

pub fn audio_bytes_in(root: &Path, meeting_id: &str) -> u64 {
    let Ok(dir) = meeting_dir(root, meeting_id) else {
        return 0;
    };
    let mut total = 0;
    for track in fs::read_dir(dir).into_iter().flatten().flatten() {
        for file in fs::read_dir(track.path()).into_iter().flatten().flatten() {
            let path = file.path();
            if path.extension().and_then(|e| e.to_str()) == Some(CHUNK_EXTENSION) {
                total += file.metadata().map(|m| m.len()).unwrap_or(0);
            }
        }
    }
    total
}

/// Never from the audio callback. Silent in tests, which must not write to
/// the user's real log file.
pub(crate) fn log(level: &str, message: &str) {
    #[cfg(not(test))]
    let _ = crate::storage::append_log(level, message);
    #[cfg(test)]
    let _ = (level, message);
}

#[cfg(test)]
pub(crate) mod test_support {
    use std::path::PathBuf;

    /// A fresh directory under the system temp dir, removed on drop.
    pub struct TempDir(pub PathBuf);

    impl TempDir {
        pub fn new(label: &str) -> Self {
            let path = std::env::temp_dir()
                .join(format!("flowing-thoughts-{label}-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        pub fn path(&self) -> &std::path::Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::TempDir;
    use super::*;

    #[test]
    fn layout_is_meeting_track_seq() {
        assert_eq!(chunk_file_name(7), "000007.pcm");
        assert_eq!(chunk_rel_path("m-1", TrackKind::System, 12), "m-1/system/000012.pcm");
        assert_eq!(CHUNK_FRAMES, 960_000);
        let root = Path::new("/root");
        assert_eq!(track_dir(root, "m-1", TrackKind::Mic).unwrap(), Path::new("/root/m-1/mic"));
        assert!(meetings_root().unwrap().ends_with("FlowingThoughts/meetings"));
    }

    #[test]
    fn meeting_ids_cannot_escape_the_root() {
        for bad in ["", "..", "../other", "a/b", ".", "a b"] {
            assert!(meeting_dir(Path::new("/root"), bad).is_err(), "{bad:?}");
            assert!(delete_meeting_audio_in(Path::new("/root"), bad).is_err(), "{bad:?}");
            assert_eq!(audio_bytes_in(Path::new("/root"), bad), 0);
        }
        assert!(meeting_dir(Path::new("/root"), &uuid::Uuid::new_v4().to_string()).is_ok());
    }

    #[test]
    fn audio_bytes_counts_pcm_only_and_delete_removes_the_meeting() {
        let tmp = TempDir::new("paths");
        let mic = track_dir(tmp.path(), "m1", TrackKind::Mic).unwrap();
        let system = track_dir(tmp.path(), "m1", TrackKind::System).unwrap();
        fs::create_dir_all(&mic).unwrap();
        fs::create_dir_all(&system).unwrap();
        fs::write(mic.join(chunk_file_name(0)), vec![0u8; 1_000]).unwrap();
        fs::write(mic.join(chunk_file_name(1)), vec![0u8; 24]).unwrap();
        fs::write(system.join(chunk_file_name(0)), vec![0u8; 500]).unwrap();
        fs::write(mic.join("track.json"), b"{}").unwrap();
        fs::create_dir_all(tmp.path().join("m2").join("mic")).unwrap();

        assert_eq!(audio_bytes_in(tmp.path(), "m1"), 1_524);
        assert_eq!(audio_bytes_in(tmp.path(), "m2"), 0);
        assert_eq!(audio_bytes_in(tmp.path(), "missing"), 0);

        delete_meeting_audio_in(tmp.path(), "m1").unwrap();
        assert!(!tmp.path().join("m1").exists());
        assert!(tmp.path().join("m2").exists(), "other meetings are untouched");
        delete_meeting_audio_in(tmp.path(), "m1").unwrap();
    }
}
