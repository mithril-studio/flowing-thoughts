import { useCallback, useEffect, useState } from "react";
import * as meetingsApi from "../lib/meetingsApi";
import MeetingDetail from "../components/meetings/MeetingDetail";
import MeetingList from "../components/meetings/MeetingList";
import RecordingBar from "../components/meetings/RecordingBar";
import { ErrorBanner } from "../components/meetings/ui";
import type { AppSettings } from "../types/settings";
import type { MeetingListItem, PermissionStatus, RecordingStatus } from "../types/meetings";

interface MeetingsProps {
  settings: AppSettings;
}

const IDLE: RecordingStatus = {
  phase: "idle",
  meeting_id: null,
  started_at: null,
  elapsed_ms: 0,
  tracks: [],
  system_audio_silent: false,
  echo_risk: false,
};

/**
 * Record a call, then read its transcript. All state is the backend's: this
 * page renders what the commands return and what `meeting-state`,
 * `meeting-job-progress` and `meeting-updated` report.
 */
export default function Meetings({ settings }: MeetingsProps) {
  const [meetings, setMeetings] = useState<MeetingListItem[]>([]);
  const [loaded, setLoaded] = useState(false);
  const [activeId, setActiveId] = useState<string | null>(null);
  const [recording, setRecording] = useState<RecordingStatus>(IDLE);
  const [supported, setSupported] = useState<boolean | null>(null);
  const [permission, setPermission] = useState<PermissionStatus | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const refreshList = useCallback(async () => {
    try {
      setMeetings(await meetingsApi.listMeetings());
    } catch (e) {
      setError(String(e));
    } finally {
      setLoaded(true);
    }
  }, []);

  useEffect(() => {
    let current = true;
    meetingsApi
      .meetingsSupported()
      .then((ok) => current && setSupported(ok))
      .catch(() => current && setSupported(false));
    meetingsApi
      .getMeetingRecordingStatus()
      .then((status) => current && setRecording(status))
      .catch(() => {
        // Idle is the safe reading; the next meeting-state event corrects it.
      });
    void refreshList();

    const unlistenState = meetingsApi.onMeetingState(setRecording);
    const unlistenProgress = meetingsApi.onMeetingJobProgress((progress) => {
      setMeetings((prev) =>
        prev.map((m) => (m.id === progress.meeting_id ? { ...m, job: progress } : m)),
      );
    });
    const unlistenUpdated = meetingsApi.onMeetingUpdated((update) => {
      if (update.change === "deleted") {
        setMeetings((prev) => prev.filter((m) => m.id !== update.meeting_id));
        setActiveId((id) => (id === update.meeting_id ? null : id));
      } else {
        void refreshList();
      }
    });
    return () => {
      current = false;
      void unlistenState.then((fn) => fn());
      void unlistenProgress.then((fn) => fn());
      void unlistenUpdated.then((fn) => fn());
    };
  }, [refreshList]);

  // macOS reports a denial as silence, so ask again whenever a recording starts.
  const recordingActive = recording.phase !== "idle";
  useEffect(() => {
    let current = true;
    meetingsApi
      .checkSystemAudioPermission()
      .then((status) => current && setPermission(status))
      .catch(() => {
        // Unknown never blocks a meeting.
      });
    return () => {
      current = false;
    };
  }, [recordingActive]);

  const control = async (command: () => Promise<RecordingStatus>) => {
    setBusy(true);
    setError(null);
    try {
      setRecording(await command());
      // Starting adds a row and stopping changes one's state.
      void refreshList();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  const deleteMeeting = async (meetingId: string) => {
    try {
      await meetingsApi.deleteMeeting(meetingId);
      setMeetings((prev) => prev.filter((m) => m.id !== meetingId));
      setActiveId((id) => (id === meetingId ? null : id));
    } catch (e) {
      setError(String(e));
    }
  };

  const openSystemAudioSettings = () => {
    meetingsApi.openSystemAudioSettings().catch((e) => setError(String(e)));
  };

  const unsupported = supported === false;

  return (
    <div className="flex h-full flex-col">
      {(recordingActive || error) && (
        <div className="space-y-2 px-4 pt-3">
          {recordingActive && (
            <RecordingBar
              status={recording}
              permission={permission}
              busy={busy}
              onPause={() => void control(meetingsApi.pauseMeeting)}
              onResume={() => void control(meetingsApi.resumeMeeting)}
              onStop={() => void control(meetingsApi.stopMeeting)}
              onOpenSystemAudioSettings={openSystemAudioSettings}
            />
          )}
          {error && <ErrorBanner message={error} onDismiss={() => setError(null)} />}
        </div>
      )}

      <div className="min-h-0 flex-1">
        {activeId ? (
          <MeetingDetail
            key={activeId}
            meetingId={activeId}
            settings={settings.meetings}
            onBack={() => setActiveId(null)}
            onDelete={deleteMeeting}
          />
        ) : (
          <div className="flex h-full flex-col">
            <div className="space-y-3 px-4 py-3">
              <div>
                <h2 className="text-sm font-semibold text-zinc-900 dark:text-zinc-100">Meetings</h2>
                <p className="mt-0.5 text-xs text-zinc-500">
                  Records your microphone and the call, then transcribes on this Mac.
                </p>
              </div>
              {!recordingActive && (
                <button
                  type="button"
                  onClick={() => void control(() => meetingsApi.startMeeting())}
                  disabled={busy || supported !== true}
                  className="w-full rounded-lg bg-emerald-600 px-3 py-2 text-sm font-medium text-white transition-colors hover:bg-emerald-500 disabled:cursor-not-allowed disabled:opacity-50 dark:bg-emerald-600 dark:hover:bg-emerald-500"
                >
                  Start meeting
                </button>
              )}
              {unsupported && (
                <p className="rounded-xl border border-amber-200 dark:border-amber-900/60 bg-amber-50 dark:bg-amber-950/30 p-3 text-xs text-amber-800 dark:text-amber-300">
                  Meetings need macOS 14.4 or later, because that is when macOS started letting
                  apps record system audio. Dictation keeps working on this Mac.
                </p>
              )}
            </div>
            <div className="min-h-0 flex-1 overflow-y-auto px-4 pb-4">
              {loaded && (
                <MeetingList meetings={meetings} onOpen={setActiveId} onDelete={deleteMeeting} />
              )}
            </div>
          </div>
        )}
      </div>
    </div>
  );
}
