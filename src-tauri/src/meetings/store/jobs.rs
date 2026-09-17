//! `jobs`: the SQLite-backed queue the `meeting-worker` thread drains.
//!
//! Order is `priority` (higher first), then age. The worker's writes
//! (`heartbeat_job`, `set_job_progress`, `finish_job`, `fail_job`) only touch
//! a `running` job and say whether they did, so a job cancelled from the UI
//! connection is never resurrected by the worker finishing its window.

use std::collections::HashMap;

use rusqlite::{params, Connection, Row};

use super::super::types::{JobKind, JobProgress, JobStatus};
use super::{enum_col, execute, new_id, now, query_all, query_opt, u32_col};

#[derive(Debug, Clone)]
pub struct NewJob {
    pub kind: JobKind,
    pub meeting_id: String,
    pub run_id: Option<String>,
    /// Higher runs first. 0 is the default.
    pub priority: i64,
    /// `None` is `{}`.
    pub payload_json: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobRow {
    pub id: String,
    pub kind: JobKind,
    pub meeting_id: String,
    pub run_id: Option<String>,
    pub status: JobStatus,
    pub priority: i64,
    /// Times the job was claimed.
    pub attempts: u32,
    pub progress_done: u32,
    pub progress_total: u32,
    pub payload_json: String,
    pub error: Option<String>,
    pub created_at: String,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    /// The worker's heartbeat.
    pub updated_at: String,
}

impl JobRow {
    /// The shape the frontend and `meeting-job-progress` use.
    pub fn progress(&self) -> JobProgress {
        JobProgress {
            job_id: self.id.clone(),
            meeting_id: self.meeting_id.clone(),
            run_id: self.run_id.clone(),
            kind: self.kind,
            status: self.status,
            done: self.progress_done,
            total: self.progress_total,
            error: self.error.clone(),
        }
    }
}

const JOB_COLUMNS: &str = "id, kind, meeting_id, run_id, status, priority, attempts, progress_done,
     progress_total, payload_json, error, created_at, started_at, finished_at, updated_at";

/// Claim order. `rowid` settles two jobs created in the same instant.
const QUEUE_ORDER: &str = "ORDER BY priority DESC, created_at ASC, rowid ASC";

fn job_from_row(row: &Row<'_>) -> rusqlite::Result<JobRow> {
    Ok(JobRow {
        id: row.get(0)?,
        kind: enum_col(row, 1, JobKind::parse)?,
        meeting_id: row.get(2)?,
        run_id: row.get(3)?,
        status: enum_col(row, 4, JobStatus::parse)?,
        priority: row.get(5)?,
        attempts: u32_col(row, 6)?,
        progress_done: u32_col(row, 7)?,
        progress_total: u32_col(row, 8)?,
        payload_json: row.get(9)?,
        error: row.get(10)?,
        created_at: row.get(11)?,
        started_at: row.get(12)?,
        finished_at: row.get(13)?,
        updated_at: row.get(14)?,
    })
}

/// Enqueues a job. Returns the `queued` row.
pub fn insert_job(conn: &Connection, job: &NewJob) -> Result<JobRow, String> {
    let row = query_opt(
        conn,
        "insert job",
        &format!(
            "INSERT INTO jobs (id, kind, meeting_id, run_id, status, priority, payload_json,
                               created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8)
             RETURNING {JOB_COLUMNS}"
        ),
        params![
            new_id(),
            job.kind.as_str(),
            job.meeting_id,
            job.run_id,
            JobStatus::Queued.as_str(),
            job.priority,
            job.payload_json.as_deref().unwrap_or("{}"),
            now(),
        ],
        job_from_row,
    )?;
    row.ok_or_else(|| "Inserting a job returned no row".to_string())
}

pub fn get_job(conn: &Connection, job_id: &str) -> Result<Option<JobRow>, String> {
    query_opt(
        conn,
        "read job",
        &format!("SELECT {JOB_COLUMNS} FROM jobs WHERE id = ?1"),
        params![job_id],
        job_from_row,
    )
}

/// The job `claim_next_job` would take, without taking it.
#[cfg(test)] // the worker claims (`claim_next_job`); only the tests peek
pub fn next_queued_job(conn: &Connection) -> Result<Option<JobRow>, String> {
    query_opt(
        conn,
        "read next queued job",
        &format!("SELECT {JOB_COLUMNS} FROM jobs WHERE status = 'queued' {QUEUE_ORDER} LIMIT 1"),
        [],
        job_from_row,
    )
}

/// Takes the next `queued` job and makes it `running`, in one statement: two
/// claimers can never get the same job. `None` when the queue is empty.
pub fn claim_next_job(conn: &Connection) -> Result<Option<JobRow>, String> {
    query_opt(
        conn,
        "claim next job",
        &format!(
            "UPDATE jobs
                SET status = 'running', attempts = attempts + 1, error = NULL,
                    started_at = ?1, finished_at = NULL, updated_at = ?1
              WHERE id = (SELECT id FROM jobs WHERE status = 'queued' {QUEUE_ORDER} LIMIT 1)
             RETURNING {JOB_COLUMNS}"
        ),
        params![now()],
        job_from_row,
    )
}

/// "Still working on it." `false` when the job is no longer `running`
/// (cancelled or deleted meanwhile): the worker should drop it.
pub fn heartbeat_job(conn: &Connection, job_id: &str) -> Result<bool, String> {
    let changed = execute(
        conn,
        "heartbeat job",
        "UPDATE jobs SET updated_at = ?2 WHERE id = ?1 AND status = 'running'",
        params![job_id, now()],
    )?;
    Ok(changed > 0)
}

/// Windows decoded out of the total; doubles as a heartbeat. `false` when the
/// job is no longer `running`.
pub fn set_job_progress(
    conn: &Connection,
    job_id: &str,
    done: u32,
    total: u32,
) -> Result<bool, String> {
    let changed = execute(
        conn,
        "set job progress",
        "UPDATE jobs SET progress_done = ?2, progress_total = ?3, updated_at = ?4
          WHERE id = ?1 AND status = 'running'",
        params![job_id, done as i64, total as i64, now()],
    )?;
    Ok(changed > 0)
}

fn settle_job(
    conn: &Connection,
    job_id: &str,
    status: JobStatus,
    error: Option<&str>,
) -> Result<bool, String> {
    let changed = execute(
        conn,
        "settle job",
        "UPDATE jobs SET status = ?2, error = ?3, finished_at = ?4, updated_at = ?4
          WHERE id = ?1 AND status = 'running'",
        params![job_id, status.as_str(), error, now()],
    )?;
    Ok(changed > 0)
}

/// `running` to `done`. `false` when the job was no longer `running`.
pub fn finish_job(conn: &Connection, job_id: &str) -> Result<bool, String> {
    settle_job(conn, job_id, JobStatus::Done, None)
}

/// `running` to `failed`. `false` when the job was no longer `running`.
pub fn fail_job(conn: &Connection, job_id: &str, error: &str) -> Result<bool, String> {
    settle_job(conn, job_id, JobStatus::Failed, Some(error))
}

/// Cancels the meeting's `queued` and `running` jobs, before its audio or the
/// meeting itself is deleted. Returns how many were cancelled.
pub fn cancel_unfinished_jobs(conn: &Connection, meeting_id: &str) -> Result<usize, String> {
    execute(
        conn,
        "cancel jobs",
        "UPDATE jobs SET status = 'cancelled', finished_at = ?2, updated_at = ?2
          WHERE meeting_id = ?1 AND status IN ('queued', 'running')",
        params![meeting_id, now()],
    )
}

/// Launch repair: nothing can be running when the app has just started, so
/// every `running` job goes back to `queued`. It keeps its place in the queue
/// (`created_at` is untouched) and its progress; the windows that are `done`
/// stay done. Returns how many were requeued.
pub fn requeue_running_jobs(conn: &Connection) -> Result<usize, String> {
    execute(
        conn,
        "requeue running jobs",
        "UPDATE jobs SET status = 'queued', started_at = NULL, updated_at = ?1
          WHERE status = 'running'",
        params![now()],
    )
}

/// Every job of the meeting, newest first.
pub fn list_jobs(conn: &Connection, meeting_id: &str) -> Result<Vec<JobRow>, String> {
    query_all(
        conn,
        "list jobs",
        &format!(
            "SELECT {JOB_COLUMNS} FROM jobs WHERE meeting_id = ?1
              ORDER BY created_at DESC, rowid DESC"
        ),
        params![meeting_id],
        job_from_row,
    )
}

const UNFINISHED_ORDER: &str =
    "ORDER BY (status = 'running') DESC, priority DESC, created_at ASC, rowid ASC";

/// The meeting's unfinished job, if any: the running one, else the next in
/// line.
pub fn job_progress(conn: &Connection, meeting_id: &str) -> Result<Option<JobProgress>, String> {
    let job = query_opt(
        conn,
        "read job progress",
        &format!(
            "SELECT {JOB_COLUMNS} FROM jobs
              WHERE meeting_id = ?1 AND status IN ('queued', 'running') {UNFINISHED_ORDER} LIMIT 1"
        ),
        params![meeting_id],
        job_from_row,
    )?;
    Ok(job.map(|job| job.progress()))
}

/// `job_progress` for every meeting at once, keyed by meeting id: the meetings
/// list needs one query, not one per row.
pub(super) fn unfinished_jobs(conn: &Connection) -> Result<HashMap<String, JobProgress>, String> {
    let jobs = query_all(
        conn,
        "list unfinished jobs",
        &format!(
            "SELECT {JOB_COLUMNS} FROM jobs WHERE status IN ('queued', 'running') {UNFINISHED_ORDER}"
        ),
        [],
        job_from_row,
    )?;
    let mut by_meeting = HashMap::new();
    for job in jobs {
        by_meeting.entry(job.meeting_id.clone()).or_insert_with(|| job.progress());
    }
    Ok(by_meeting)
}
