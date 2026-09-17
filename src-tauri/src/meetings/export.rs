//! A meeting as Markdown. Scaffolded for WP10, implemented with the session
//! (WP7).
//!
//! `export_markdown` is called by `commands.rs` and its signature is fixed.
//! It only reads, and returns the text: the frontend decides whether that
//! becomes a clipboard copy or a file.
//!
//! - Title, date, duration, participants if any; the summary (overview,
//!   decisions, actions with owner and due date when known, topics) if there
//!   is one; then the transcript of the active run.
//! - Transcript lines: `**Me** [12:34] text`, consecutive segments of one
//!   speaker merged into a paragraph. Edited text wins. Hidden segments are
//!   left out: the ones the pipeline flagged and the user did not bring back,
//!   and the ones the user hid.
//! - `file_name` is `<YYYY-MM-DD> <title>.md`, made safe for a file system.
//!
//! `render` is a pure function of store DTOs, so it tests without a DB.

use rusqlite::Connection;

use super::store::{self, Participant};
use super::types::{
    MeetingDetail, MeetingExport, MeetingSummary, Segment, SummaryItem, SummaryItemKind,
    SummaryStatus,
};

pub fn export_markdown(conn: &Connection, meeting_id: &str) -> Result<MeetingExport, String> {
    let meeting = store::get_meeting(conn, meeting_id)?
        .ok_or_else(|| format!("Meeting '{meeting_id}' not found"))?;
    let participants = store::list_participants(conn, meeting_id)?;
    let summary = store::latest_summary(conn, meeting_id)?;
    let segments = store::list_segments(conn, meeting_id, None)?;
    // The user's own time zone: that is when the meeting was, for them.
    let started = chrono::DateTime::parse_from_rfc3339(&meeting.meeting.started_at)
        .map(|at| at.with_timezone(&chrono::Local).naive_local())
        .map_err(|e| format!("The meeting has no valid start time: {e}"))?;
    Ok(render(&meeting, started, &participants, summary.as_ref(), &segments))
}

/// `65_000` is `01:05`, an hour and a bit is `1:02:03`.
fn timestamp(ms: u64) -> String {
    let seconds = ms / 1_000;
    let (h, m, s) = (seconds / 3_600, seconds / 60 % 60, seconds % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m:02}:{s:02}")
    }
}

/// Keeps letters, digits and the usual punctuation; everything a file system
/// or a shell could trip over becomes a space.
fn safe_file_stem(title: &str) -> String {
    let cleaned: String = title
        .chars()
        .map(|c| if c.is_alphanumeric() || " -_.,()&+'".contains(c) { c } else { ' ' })
        .collect();
    let stem = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    let stem = stem.trim_matches(|c: char| c == '.' || c == ' ');
    let stem: String = stem.chars().take(80).collect();
    if stem.is_empty() {
        "Meeting".to_string()
    } else {
        stem
    }
}

fn participant_label(participant: &Participant) -> Option<String> {
    let name = participant.name.as_deref().map(str::trim).filter(|n| !n.is_empty());
    let email = participant.email.as_deref().map(str::trim).filter(|e| !e.is_empty());
    match (name, email) {
        (Some(name), Some(email)) => Some(format!("{name} <{email}>")),
        (Some(one), None) | (None, Some(one)) => Some(one.to_string()),
        (None, None) => None,
    }
}

fn summary_item_line(item: &SummaryItem) -> String {
    let details: Vec<String> = [("owner", &item.owner), ("due", &item.due_date)]
        .into_iter()
        .filter_map(|(label, value)| {
            let value = value.as_deref().map(str::trim).filter(|v| !v.is_empty())?;
            Some(format!("{label}: {value}"))
        })
        .collect();
    if details.is_empty() {
        format!("- {}\n", item.text.trim())
    } else {
        format!("- {} ({})\n", item.text.trim(), details.join(", "))
    }
}

pub(crate) fn render(
    meeting: &MeetingDetail,
    started: chrono::NaiveDateTime,
    participants: &[Participant],
    summary: Option<&MeetingSummary>,
    segments: &[Segment],
) -> MeetingExport {
    let title = meeting.meeting.title.trim();
    let mut md = format!("# {title}\n\n");
    md.push_str(&format!("- Date: {}\n", started.format("%Y-%m-%d %H:%M")));
    md.push_str(&format!("- Duration: {}\n", timestamp(meeting.meeting.duration_ms)));
    let people: Vec<String> = participants.iter().filter_map(participant_label).collect();
    if !people.is_empty() {
        md.push_str(&format!("- Participants: {}\n", people.join(", ")));
    }

    let has_content = |s: &&MeetingSummary| {
        s.status == SummaryStatus::Done
            && (!s.items.is_empty() || s.overview.as_deref().is_some_and(|o| !o.trim().is_empty()))
    };
    if let Some(summary) = summary.filter(has_content) {
        md.push_str("\n## Summary\n");
        if let Some(overview) = summary.overview.as_deref().map(str::trim).filter(|o| !o.is_empty()) {
            md.push_str(&format!("\n{overview}\n"));
        }
        for (kind, heading) in [
            (SummaryItemKind::Decision, "Decisions"),
            (SummaryItemKind::Action, "Action items"),
            (SummaryItemKind::Topic, "Topics"),
        ] {
            let items: Vec<&SummaryItem> = summary.items.iter().filter(|i| i.kind == kind).collect();
            if items.is_empty() {
                continue;
            }
            md.push_str(&format!("\n### {heading}\n\n"));
            for item in items {
                md.push_str(&summary_item_line(item));
            }
        }
    }

    md.push_str("\n## Transcript\n");
    let mut ordered: Vec<&Segment> =
        segments.iter().filter(|s| !s.hidden && !s.text.trim().is_empty()).collect();
    ordered.sort_by_key(|s| (s.start_ms, s.end_ms));
    if ordered.is_empty() {
        md.push_str("\n_No transcript yet._\n");
    }
    let mut speaker: Option<&str> = None;
    for segment in ordered {
        if speaker == Some(segment.speaker_label.as_str()) {
            md.push(' ');
            md.push_str(segment.text.trim());
        } else {
            if speaker.is_some() {
                md.push('\n');
            }
            md.push_str(&format!(
                "\n**{}** [{}] {}",
                segment.speaker_label,
                timestamp(segment.start_ms),
                segment.text.trim()
            ));
            speaker = Some(&segment.speaker_label);
        }
    }
    if speaker.is_some() {
        md.push('\n');
    }

    MeetingExport {
        file_name: format!("{} {}.md", started.format("%Y-%m-%d"), safe_file_stem(title)),
        markdown: md,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::meetings::types::{
        MeetingLanguage, MeetingListItem, MeetingStatus, ParticipantSource, SuppressedReason,
        TrackKind,
    };

    fn meeting(title: &str) -> MeetingDetail {
        MeetingDetail {
            meeting: MeetingListItem {
                id: "m1".into(),
                title: title.into(),
                status: MeetingStatus::Ready,
                started_at: "2026-09-17T08:00:00+00:00".into(),
                ended_at: None,
                duration_ms: 754_000,
                language: MeetingLanguage::Nl,
                echo_risk: false,
                has_audio: true,
                has_summary: false,
                job: None,
            },
            model: None,
            active_run_id: Some("r1".into()),
            error: None,
            audio_bytes: 0,
            tracks: Vec::new(),
            runs: Vec::new(),
            speakers: Vec::new(),
        }
    }

    fn started() -> chrono::NaiveDateTime {
        chrono::NaiveDate::from_ymd_opt(2026, 9, 17).unwrap().and_hms_opt(10, 0, 0).unwrap()
    }

    fn segment(id: &str, kind: TrackKind, start_ms: u64, text: &str) -> Segment {
        Segment {
            id: id.into(),
            meeting_id: "m1".into(),
            run_id: "r1".into(),
            track_id: kind.as_str().into(),
            track_kind: kind,
            start_ms,
            end_ms: start_ms + 2_000,
            text: text.into(),
            original_text: None,
            lang: Some("nl".into()),
            speaker_id: None,
            speaker_label: if kind == TrackKind::Mic { "Me" } else { "Them" }.into(),
            suppressed_reason: None,
            hidden: false,
        }
    }

    #[test]
    fn a_small_meeting_renders_as_expected() {
        let participants = vec![
            Participant {
                id: "p1".into(),
                meeting_id: "m1".into(),
                person_id: None,
                name: Some("Anna".into()),
                email: Some("anna@example.com".into()),
                source: ParticipantSource::Manual,
            },
            Participant {
                id: "p2".into(),
                meeting_id: "m1".into(),
                person_id: None,
                name: Some("Bob".into()),
                email: None,
                source: ParticipantSource::Manual,
            },
        ];
        let summary = MeetingSummary {
            id: "s1".into(),
            meeting_id: "m1".into(),
            run_id: "r1".into(),
            provider: "openrouter".into(),
            model: "x".into(),
            status: SummaryStatus::Done,
            overview: Some("We planned the release.".into()),
            error: None,
            created_at: "2026-09-17T09:00:00+00:00".into(),
            items: vec![
                SummaryItem {
                    id: "i1".into(),
                    kind: SummaryItemKind::Action,
                    text: "Ship the beta".into(),
                    owner: Some("Anna".into()),
                    due_date: Some("2026-09-24".into()),
                    source_segment_ids: vec!["a".into()],
                },
                SummaryItem {
                    id: "i2".into(),
                    kind: SummaryItemKind::Decision,
                    text: "Release on Thursday".into(),
                    owner: None,
                    due_date: None,
                    source_segment_ids: vec!["b".into()],
                },
            ],
        };
        let mut edited = segment("c", TrackKind::Mic, 9_000, "Donderdag dus.");
        edited.original_text = Some("Donder dag dus.".into());
        let mut flagged = segment("d", TrackKind::Mic, 12_000, "Wanneer brengen we het uit?");
        flagged.suppressed_reason = Some(SuppressedReason::Echo);
        flagged.hidden = true;
        let mut hidden_by_user = segment("e", TrackKind::System, 20_000, "Even iets anders.");
        hidden_by_user.hidden = true;
        let mut brought_back = segment("f", TrackKind::System, 3_725_000, "Tot volgende week.");
        brought_back.suppressed_reason = Some(SuppressedReason::NoSpeech);
        // Out of order on purpose: tracks are decoded one after the other.
        let segments = vec![
            segment("b", TrackKind::System, 5_000, "Wanneer brengen we het uit?"),
            brought_back,
            segment("a", TrackKind::Mic, 1_000, "Goedemorgen."),
            segment("a2", TrackKind::Mic, 3_000, "Zullen we beginnen?"),
            edited,
            flagged,
            hidden_by_user,
        ];

        let export = render(&meeting("Release: planning / Q3"), started(), &participants, Some(&summary), &segments);
        assert_eq!(export.file_name, "2026-09-17 Release planning Q3.md");
        assert_eq!(
            export.markdown,
            "# Release: planning / Q3\n\
             \n\
             - Date: 2026-09-17 10:00\n\
             - Duration: 12:34\n\
             - Participants: Anna <anna@example.com>, Bob\n\
             \n\
             ## Summary\n\
             \n\
             We planned the release.\n\
             \n\
             ### Decisions\n\
             \n\
             - Release on Thursday\n\
             \n\
             ### Action items\n\
             \n\
             - Ship the beta (owner: Anna, due: 2026-09-24)\n\
             \n\
             ## Transcript\n\
             \n\
             **Me** [00:01] Goedemorgen. Zullen we beginnen?\n\
             \n\
             **Them** [00:05] Wanneer brengen we het uit?\n\
             \n\
             **Me** [00:09] Donderdag dus.\n\
             \n\
             **Them** [1:02:05] Tot volgende week.\n"
        );
    }

    #[test]
    fn a_meeting_without_transcript_summary_or_participants_still_exports() {
        let mut failed_summary = MeetingSummary {
            id: "s1".into(),
            meeting_id: "m1".into(),
            run_id: "r1".into(),
            provider: "openrouter".into(),
            model: "x".into(),
            status: SummaryStatus::Failed,
            overview: None,
            error: Some("no key".into()),
            created_at: String::new(),
            items: Vec::new(),
        };
        let export = render(&meeting("  "), started(), &[], Some(&failed_summary), &[]);
        assert_eq!(export.file_name, "2026-09-17 Meeting.md");
        assert!(!export.markdown.contains("## Summary"));
        assert!(!export.markdown.contains("Participants"));
        assert!(export.markdown.ends_with("## Transcript\n\n_No transcript yet._\n"));

        failed_summary.status = SummaryStatus::Done;
        let export = render(&meeting("x"), started(), &[], Some(&failed_summary), &[]);
        assert!(!export.markdown.contains("## Summary"), "an empty summary is left out");
    }

    #[test]
    fn file_names_are_safe() {
        assert_eq!(safe_file_stem("../../etc/passwd"), "etc passwd");
        assert_eq!(safe_file_stem("Wekelijks overleg: café"), "Wekelijks overleg café");
        assert_eq!(safe_file_stem("???"), "Meeting");
        assert_eq!(safe_file_stem(&"a".repeat(200)).len(), 80);
    }
}
