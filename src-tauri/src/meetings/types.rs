//! Shared meeting types: the vocabulary every package under `meetings/`
//! agrees on. Owned by WP1 (scaffold).
//!
//! Three groups:
//! 1. String enums. Their `as_str()` is what the v3 schema stores in its
//!    `status` / `kind` / `source` columns and what the frontend receives.
//! 2. Seams between packages that are built in parallel: `AudioSource`,
//!    `SampleSink`, `ChunkLedger`, `TrackAudio`.
//! 3. DTOs for the frontend, mirrored by hand in `src/types/meetings.ts`.
//!    Field names are snake_case on both sides, like the settings types.
//!
//! Changing anything here changes a contract between packages: keep
//! additions backwards compatible and update `src/types/meetings.ts` with it.

// Scaffold: most of this is first used by WP2–WP10. Remove when they land.
#![allow(dead_code)]

/// Sample rate of everything downstream of the recorder: chunk files,
/// `SampleSink`, `TrackAudio` and the decoder.
pub const TARGET_SAMPLE_RATE: u32 = 16_000;

/// A fieldless enum stored as text. Generates `as_str`, `parse`, `ALL` and
/// serde impls that use the same strings, so the DB, the wire format and the
/// TypeScript unions cannot drift apart.
macro_rules! string_enum {
    ($(#[$meta:meta])* $name:ident { $($(#[$vmeta:meta])* $variant:ident => $text:literal),+ $(,)? }) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum $name {
            $($(#[$vmeta])* $variant),+
        }

        impl $name {
            pub const ALL: &'static [$name] = &[$($name::$variant),+];

            pub fn as_str(self) -> &'static str {
                match self {
                    $($name::$variant => $text),+
                }
            }

            pub fn parse(value: &str) -> Option<Self> {
                match value {
                    $($text => Some($name::$variant),)+
                    _ => None,
                }
            }
        }

        impl serde::Serialize for $name {
            fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.serialize_str(self.as_str())
            }
        }

        impl<'de> serde::Deserialize<'de> for $name {
            fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let value = <String as serde::Deserialize>::deserialize(deserializer)?;
                $name::parse(&value).ok_or_else(|| {
                    serde::de::Error::custom(format!(
                        "unknown {} '{value}'",
                        stringify!($name)
                    ))
                })
            }
        }
    };
}

string_enum! {
    /// One recorded track. v1 labels segments from it: mic is "Me", system
    /// is "Them".
    TrackKind {
        Mic => "mic",
        System => "system",
    }
}

string_enum! {
    /// `meetings.status`: the one lifecycle the list and detail views show.
    /// Session (WP7) moves it up to `queued`; the worker (WP6) from there on.
    MeetingStatus {
        Recording => "recording",
        Paused => "paused",
        /// The app quit or crashed mid-recording. Launch recovery repairs the
        /// chunks and moves the meeting on to `queued`.
        Interrupted => "interrupted",
        /// Recorded, waiting for the worker.
        Queued => "queued",
        Transcribing => "transcribing",
        /// The active run is complete.
        Ready => "ready",
        /// Transcription failed; `meetings.error` says why. Audio is intact.
        Failed => "failed",
    }
}

string_enum! {
    /// `meeting_audio_chunks.status`.
    ChunkStatus {
        /// Being written. Found at launch, it becomes `recovered`.
        Open => "open",
        Closed => "closed",
        /// Closed by launch recovery: `n_frames` = file length / 2.
        Recovered => "recovered",
        /// The user deleted the audio; the row stays for the timeline.
        Deleted => "deleted",
    }
}

string_enum! {
    /// `transcript_runs.status`.
    RunStatus {
        Queued => "queued",
        Running => "running",
        Done => "done",
        Failed => "failed",
        Cancelled => "cancelled",
    }
}

string_enum! {
    /// `transcript_windows.status`. A preempted window simply stays `pending`.
    WindowStatus {
        Pending => "pending",
        Done => "done",
        Failed => "failed",
    }
}

string_enum! {
    /// `jobs.kind`.
    JobKind {
        Transcribe => "transcribe",
    }
}

string_enum! {
    /// `jobs.status`. At launch every `running` job goes back to `queued`.
    JobStatus {
        Queued => "queued",
        Running => "running",
        Done => "done",
        Failed => "failed",
        Cancelled => "cancelled",
    }
}

string_enum! {
    /// Why a segment is hidden by default. Suspect segments are flagged,
    /// never deleted.
    SuppressedReason {
        NoSpeech => "no_speech",
        OutsideVad => "outside_vad",
        Repeat => "repeat",
        PromptEcho => "prompt_echo",
        /// The mic picked up the speakers: duplicates a system-track segment.
        Echo => "echo",
    }
}

string_enum! {
    /// `speakers.source`. v1 only seeds `track` speakers ("Me", "Them").
    SpeakerSource {
        Track => "track",
        Diarization => "diarization",
        Manual => "manual",
    }
}

string_enum! {
    /// `participants.source`.
    ParticipantSource {
        Calendar => "calendar",
        Manual => "manual",
    }
}

string_enum! {
    /// `summaries.status`.
    SummaryStatus {
        Pending => "pending",
        Done => "done",
        Failed => "failed",
    }
}

string_enum! {
    /// `summary_items.kind`.
    SummaryItemKind {
        Decision => "decision",
        Action => "action",
        Topic => "topic",
    }
}

string_enum! {
    /// Requested transcription language, per meeting and per run.
    MeetingLanguage {
        Auto => "auto",
        Nl => "nl",
        En => "en",
    }
}

string_enum! {
    /// Live recorder phase, as opposed to the persisted `MeetingStatus`.
    RecordingPhase {
        Idle => "idle",
        Starting => "starting",
        Recording => "recording",
        Paused => "paused",
        Stopping => "stopping",
    }
}

string_enum! {
    /// System Audio Recording permission, as far as it can be known. macOS
    /// reports a denial as silence, so `unknown` is common and never blocks.
    PermissionState {
        Granted => "granted",
        Denied => "denied",
        Unknown => "unknown",
        /// macOS older than 14.4: no process taps.
        Unsupported => "unsupported",
    }
}

string_enum! {
    /// What changed, in a `meeting-updated` event.
    MeetingChange {
        Created => "created",
        Renamed => "renamed",
        Status => "status",
        Transcript => "transcript",
        Segment => "segment",
        AudioDeleted => "audio_deleted",
        Summary => "summary",
        Deleted => "deleted",
    }
}

// ---------------------------------------------------------------------------
// Capture seam (WP5 implements sources, WP4 consumes them)
// ---------------------------------------------------------------------------

/// Native format of a capture source. It can change mid-meeting (AirPods
/// connect, output device switch); every `AudioFrames` carries the format it
/// was captured in and a `Discontinuity::FormatChanged` announces the switch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceFormat {
    pub sample_rate: u32,
    pub channels: u16,
}

/// One callback's worth of audio, borrowed from the driver's buffer.
#[derive(Debug, Clone, Copy)]
pub struct AudioFrames<'a> {
    /// Interleaved f32, `channels` samples per frame.
    pub samples: &'a [f32],
    pub format: SourceFormat,
    /// Mach host time of the first frame, in nanoseconds. Both tracks share
    /// this clock; it is what aligns them on one timeline.
    pub host_time_ns: u64,
}

impl AudioFrames<'_> {
    pub fn frame_count(&self) -> usize {
        match self.format.channels {
            0 => 0,
            channels => self.samples.len() / channels as usize,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Discontinuity {
    /// The source rebuilt itself on a new device or format. Frames from here
    /// on use `format`; the recorder reconfigures its resampler.
    FormatChanged { format: SourceFormat },
    /// The source itself lost frames (driver overload, xrun).
    Dropped { frames: u64 },
    /// No callbacks for a while (device vanished, sleep). The next frames'
    /// `host_time_ns` says how long the gap was.
    Stalled,
}

/// Receives audio from an `AudioSource`. The recorder (WP4) implements it.
///
/// `on_frames` runs on the real-time audio thread: no allocation, locks, I/O
/// or logging. Copy into a lock-free ring (`rtrb`) and return.
pub trait AudioSourceHandler: Send {
    fn on_frames(&mut self, frames: AudioFrames<'_>);
    fn on_discontinuity(&mut self, discontinuity: Discontinuity, host_time_ns: u64);
}

/// A capture source: the microphone, the system-audio tap, or a fake in tests.
///
/// `Send` because the session keeps sources in managed state. An
/// implementation that owns a `!Send` handle (`cpal::Stream`) parks it on a
/// thread of its own and talks to it over a channel.
pub trait AudioSource: Send {
    fn kind(&self) -> TrackKind;
    /// Human-readable device name for `meeting_tracks.device_name`.
    fn device_name(&self) -> Option<String>;
    /// The current native format. `None` before `start`.
    fn format(&self) -> Option<SourceFormat>;
    /// Starts delivering to `handler` and returns the initial format. A
    /// source that cannot start (no permission, no device) returns `Err`; the
    /// meeting then continues without that track.
    fn start(&mut self, handler: Box<dyn AudioSourceHandler>) -> Result<SourceFormat, String>;
    /// Stops delivery and drops the handler. Idempotent.
    fn stop(&mut self) -> Result<(), String>;
}

// ---------------------------------------------------------------------------
// Recording seam (WP4)
// ---------------------------------------------------------------------------

/// What the recorder writes into, after resampling: 16 kHz mono f32. The chunk
/// writer implements it for real; tests use a `Vec`-backed fake.
pub trait SampleSink: Send {
    /// Starts a contiguous run whose first sample was captured at
    /// `anchor_host_ns`. Called at the start and again after every gap the
    /// timeline decides to record (over 100 ms); closes any open chunk.
    fn begin(&mut self, anchor_host_ns: u64) -> Result<(), String>;
    /// Appends samples contiguous with the previous write.
    fn write(&mut self, samples: &[f32]) -> Result<(), String>;
    /// Flushes and closes what is open. Idempotent.
    fn finish(&mut self) -> Result<(), String>;
}

/// A `meeting_audio_chunks` row, as plain data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChunkRecord {
    pub id: String,
    pub track_id: String,
    pub seq: u32,
    /// Relative to the meetings audio root: `<meeting_id>/<track>/<seq>.pcm`.
    pub path: String,
    pub status: ChunkStatus,
    pub anchor_host_ns: u64,
    /// Position of the first frame on the meeting timeline.
    pub start_ms: u64,
    pub n_frames: u64,
}

/// Where the chunk writer records its chunks. It lets the recorder (WP4) be
/// built and tested without the store (WP2); the session (WP7) implements it
/// on top of `store`.
pub trait ChunkLedger: Send {
    /// A chunk file was created: insert its row with status `open`.
    fn chunk_opened(&mut self, chunk: &ChunkRecord) -> Result<(), String>;
    /// The file was fsynced and closed with `n_frames` frames in it.
    fn chunk_closed(&mut self, chunk_id: &str, n_frames: u64) -> Result<(), String>;
}

/// One track's audio on the meeting timeline, 16 kHz mono. It lets long-form
/// decoding (WP3) be built and tested without the chunk files; WP4 implements
/// it over a track's chunks.
pub trait TrackAudio {
    /// End of the last chunk on the meeting timeline.
    fn duration_ms(&self) -> u64;
    /// Samples for `[start_ms, start_ms + len_ms)`. Gaps between chunks and
    /// deleted audio read as silence; past the end the result is shorter.
    fn read(&mut self, start_ms: u64, len_ms: u64) -> Result<Vec<f32>, String>;
}

// ---------------------------------------------------------------------------
// DTOs (mirrored in src/types/meetings.ts)
// ---------------------------------------------------------------------------

/// A row in the meetings list.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct MeetingListItem {
    pub id: String,
    pub title: String,
    pub status: MeetingStatus,
    pub started_at: String,
    pub ended_at: Option<String>,
    /// Recorded time, pauses excluded.
    pub duration_ms: u64,
    pub language: MeetingLanguage,
    /// Recorded over the built-in speakers: expect echo in the mic track.
    pub echo_risk: bool,
    pub has_audio: bool,
    pub has_summary: bool,
    /// The meeting's unfinished job, if any.
    pub job: Option<JobProgress>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct MeetingDetail {
    #[serde(flatten)]
    pub meeting: MeetingListItem,
    /// Model of the active run, or the one the next run will use.
    pub model: Option<String>,
    /// The run whose segments are shown. `None` until one has started.
    pub active_run_id: Option<String>,
    pub error: Option<String>,
    /// Bytes of audio still on disk.
    pub audio_bytes: u64,
    pub tracks: Vec<MeetingTrack>,
    pub runs: Vec<TranscriptRun>,
    pub speakers: Vec<Speaker>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct MeetingTrack {
    pub id: String,
    pub kind: TrackKind,
    pub device_name: Option<String>,
    pub duration_ms: u64,
    pub has_audio: bool,
    /// Frames lost to ring-buffer overflow while recording.
    pub overflow_frames: u64,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TranscriptRun {
    pub id: String,
    pub model: String,
    pub language: MeetingLanguage,
    pub status: RunStatus,
    pub error: Option<String>,
    pub created_at: String,
    pub finished_at: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Speaker {
    pub id: String,
    /// "Me", "Them", later "Speaker 2" or the assigned participant's name.
    pub label: String,
    pub source: SpeakerSource,
    pub track_id: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Segment {
    pub id: String,
    pub meeting_id: String,
    pub run_id: String,
    pub track_id: String,
    pub track_kind: TrackKind,
    pub start_ms: u64,
    pub end_ms: u64,
    /// What to show: the user's edit if there is one, else the decoded text.
    pub text: String,
    /// The decoded text, present only when `text` is an edit.
    pub original_text: Option<String>,
    pub lang: Option<String>,
    pub speaker_id: Option<String>,
    pub speaker_label: String,
    /// Why the pipeline flagged this segment, if it did.
    pub suppressed_reason: Option<SuppressedReason>,
    /// Whether to hide it by default: the user's choice if they made one,
    /// else `suppressed_reason.is_some()`.
    pub hidden: bool,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct JobProgress {
    pub job_id: String,
    pub meeting_id: String,
    pub run_id: Option<String>,
    pub kind: JobKind,
    pub status: JobStatus,
    /// Windows decoded, out of `total`. `total` is 0 until planning is done.
    pub done: u32,
    pub total: u32,
    pub error: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PermissionStatus {
    pub state: PermissionState,
    /// Extra context for the Settings row, e.g. why the state is `unknown`.
    pub detail: Option<String>,
}

/// Live recorder state: the payload of `meeting-state` and the result of
/// `get_meeting_recording_status`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RecordingStatus {
    pub phase: RecordingPhase,
    pub meeting_id: Option<String>,
    pub started_at: Option<String>,
    /// Recorded time so far, pauses excluded. The UI ticks on from here.
    pub elapsed_ms: u64,
    /// Tracks that are actually capturing. Mic-only when the tap is
    /// unavailable or was denied.
    pub tracks: Vec<TrackKind>,
    /// The watchdog saw only zeros on the system track: probably denied.
    pub system_audio_silent: bool,
    pub echo_risk: bool,
}

impl RecordingStatus {
    pub fn idle() -> Self {
        Self {
            phase: RecordingPhase::Idle,
            meeting_id: None,
            started_at: None,
            elapsed_ms: 0,
            tracks: Vec::new(),
            system_audio_silent: false,
            echo_risk: false,
        }
    }
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct StartMeetingOptions {
    /// Defaults to a title made from the start time.
    pub title: Option<String>,
    /// Defaults to `settings.meetings.language`.
    pub language: Option<MeetingLanguage>,
}

/// "Re-transcribe as…": always a new run, the old one is kept.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct RetranscribeOptions {
    /// Defaults to `settings.meetings.model`.
    pub model: Option<String>,
    /// Defaults to the meeting's language.
    pub language: Option<MeetingLanguage>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct MeetingSummary {
    pub id: String,
    pub meeting_id: String,
    pub run_id: String,
    pub provider: String,
    pub model: String,
    pub status: SummaryStatus,
    pub overview: Option<String>,
    pub error: Option<String>,
    pub created_at: String,
    pub items: Vec<SummaryItem>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SummaryItem {
    pub id: String,
    pub kind: SummaryItemKind,
    pub text: String,
    /// Unknown stays `None`; the model must not invent one.
    pub owner: Option<String>,
    pub due_date: Option<String>,
    /// Transcript segments this item was drawn from. Never empty.
    pub source_segment_ids: Vec<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct MeetingExport {
    /// Suggested file name, e.g. `2026-09-17 Standup.md`.
    pub file_name: String,
    pub markdown: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn string_enums_round_trip_through_str_and_json() {
        for status in MeetingStatus::ALL {
            assert_eq!(MeetingStatus::parse(status.as_str()), Some(*status));
            let json = serde_json::to_string(status).unwrap();
            assert_eq!(json, format!("\"{}\"", status.as_str()));
            assert_eq!(serde_json::from_str::<MeetingStatus>(&json).unwrap(), *status);
        }
        assert_eq!(SuppressedReason::PromptEcho.as_str(), "prompt_echo");
        assert_eq!(SuppressedReason::parse("outside_vad"), Some(SuppressedReason::OutsideVad));
        assert_eq!(TrackKind::parse("speaker"), None);
        assert!(serde_json::from_str::<TrackKind>("\"speaker\"").is_err());
    }

    #[test]
    fn meeting_detail_flattens_the_list_item() {
        let detail = MeetingDetail {
            meeting: MeetingListItem {
                id: "m1".into(),
                title: "Standup".into(),
                status: MeetingStatus::Ready,
                started_at: "2026-09-17T10:00:00+00:00".into(),
                ended_at: None,
                duration_ms: 1_000,
                language: MeetingLanguage::Auto,
                echo_risk: false,
                has_audio: true,
                has_summary: false,
                job: None,
            },
            model: None,
            active_run_id: None,
            error: None,
            audio_bytes: 0,
            tracks: Vec::new(),
            runs: Vec::new(),
            speakers: Vec::new(),
        };
        let json = serde_json::to_value(&detail).unwrap();
        assert_eq!(json["id"], "m1");
        assert_eq!(json["status"], "ready");
        assert!(json.get("meeting").is_none());
    }

    #[test]
    fn options_accept_an_empty_object() {
        let options: StartMeetingOptions = serde_json::from_str("{}").unwrap();
        assert!(options.title.is_none() && options.language.is_none());
        let options: RetranscribeOptions = serde_json::from_str(r#"{"language":"nl"}"#).unwrap();
        assert_eq!(options.language, Some(MeetingLanguage::Nl));
    }

    #[test]
    fn frame_count_divides_by_channels() {
        let samples = [0.0_f32; 12];
        let frames = AudioFrames {
            samples: &samples,
            format: SourceFormat { sample_rate: 48_000, channels: 2 },
            host_time_ns: 0,
        };
        assert_eq!(frames.frame_count(), 6);
    }
}
