//! Copying and moving files without freezing the window.
//!
//! A paste becomes a *job*: a worker thread drives one
//! `hyprforge_fileops::ops::Operation` per pasted item, a bounded step at
//! a time, and reports back through a stream the window turns into
//! messages with `Task::run`. The window keeps painting, and can cancel
//! between any two steps — mid-file included.
//!
//! A conflict — something already at a destination — pauses the job.
//! The worker sends [`JobEvent::Collision`] and blocks until the window
//! answers through [`JobControl::answer`]. That wait has no timeout, and
//! it is the one wait in this suite that should not: it is waiting on a
//! *person*, not on another process. If the window goes away instead,
//! the answer channel closes and the job cancels, so the thread cannot
//! outlive the process's interest in it.
//!
//! What is decided without asking, in order:
//!
//! 1. A copy into its own folder collides with itself, and is always
//!    kept as a numbered duplicate — see
//!    `hyprforge_files_core::clipboard::PasteStep::duplicate`.
//! 2. An "apply to the rest" answer, or `[behaviour] on-conflict` when it
//!    is anything but `ask`, answers every later conflict in the job —
//!    across items, not only within one, which is what "the rest" means
//!    to the person who ticked it.

use hyprforge_files_core::clipboard::PasteStep;
use hyprforge_files_core::config::OnConflict;
use hyprforge_fileops::{Collision, CollisionDecision, CollisionPolicy, Operation, Progress, StepOutcome};
use iced::futures::channel::mpsc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

pub type JobId = u64;

/// What a job reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobEvent {
    /// `progress` is the *current item's*: `fileops` runs one
    /// operation per pasted item, and each starts its counts at zero.
    /// `items` says where that item is in the job, which is what lets a
    /// job of several items draw one bar that does not fall back to
    /// empty each time it moves on to the next — see
    /// `crate::transfers::job_fraction`.
    Progress { job: JobId, progress: Progress, items: Items },
    /// Paused: something is in the way. Answer with [`JobControl::answer`].
    Collision { job: JobId, collision: Collision },
    Finished { job: JobId, summary: JobSummary },
}

/// Which of a job's items a progress report is about: `done` items are
/// finished, of `of` in all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Items {
    pub done: usize,
    pub of: usize,
}

impl Default for Items {
    /// A job of one item that has not finished it.
    fn default() -> Self {
        Items { done: 0, of: 1 }
    }
}

/// How a job went, item by item.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct JobSummary {
    /// Items placed.
    pub done: usize,
    /// Each placed item: where it came from, and where it actually landed
    /// — which a Keep Both answer can make different from where it was
    /// sent. What an undo takes back.
    pub placed: Vec<(std::path::PathBuf, std::path::PathBuf)>,
    /// Items left alone because of a Skip answer.
    pub skipped: usize,
    /// One sentence per thing that went wrong — already the actionable
    /// wording `OpsError`'s `Display` produces.
    pub failed: Vec<String>,
    pub cancelled: bool,
    /// The archive this job stopped on for want of a password.
    ///
    /// Carried as a field rather than left to be recognised in
    /// `failed` by its wording: "did this need a password" is a
    /// question the window acts on (it opens a prompt), and deciding it
    /// by matching a sentence would break the moment that sentence was
    /// reworded — which is exactly the sort of thing a message is
    /// allowed to do. Always `None` for a paste; see
    /// `crate::archive_jobs`.
    pub needs_password: Option<std::path::PathBuf>,
    /// The items that failed *whole* — nothing of them landed — and so
    /// can be run again exactly as they were asked for.
    ///
    /// Only those. An item that partly landed (a folder with 998 of its
    /// 1000 files across) cannot be retried by running it again: its
    /// first collision is with its own half-copy, and `fileops` has no
    /// "merge into the folder, skip what is already there" answer —
    /// Skip on a folder skips the whole subtree, and Replace deletes
    /// what did arrive. An item that landed nothing has neither problem:
    /// a failed file's partial copy is removed by `fileops` before it
    /// reports, and a move whose copy failed keeps its source whole, so
    /// running the step again is the original request, and anything
    /// that appeared at the destination meanwhile is asked about as it
    /// would have been the first time. Always empty for an archive job.
    pub retry: Vec<PasteStep>,
    /// Items that failed after part of them had landed — the ones
    /// `retry` leaves out, counted so the window can say why they are
    /// not offered again rather than leave them silently missing from
    /// the Retry button's count.
    pub partly: usize,
}

impl JobSummary {
    /// Whether every item arrived, so a cut's clipboard can be emptied.
    pub fn complete(&self) -> bool {
        self.failed.is_empty() && !self.cancelled && self.skipped == 0
    }
}

/// The window's handle on a running job.
#[derive(Debug, Clone)]
pub struct JobControl {
    decisions: std::sync::mpsc::Sender<CollisionDecision>,
    cancel: Arc<AtomicBool>,
}

impl JobControl {
    /// Builds one over a decision channel and a cancel flag.
    ///
    /// For a job that runs somewhere other than [`start`] — see
    /// [`crate::archive_jobs`], which reports the same events but does
    /// its work through a different library.
    pub fn new(
        decisions: std::sync::mpsc::Sender<CollisionDecision>,
        cancel: Arc<AtomicBool>,
    ) -> JobControl {
        JobControl { decisions, cancel }
    }

    /// Answers the collision the job is paused on.
    pub fn answer(&self, decision: CollisionDecision) {
        // A job that already finished has dropped its receiver; an answer
        // to nothing is nothing to report.
        let _ = self.decisions.send(decision);
    }

    /// Stops the job at its next step. A paused job is answered with
    /// Cancel so it does not sit waiting for a decision nobody will make.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
        self.answer(CollisionDecision { policy: CollisionPolicy::Cancel, apply_to_rest: true });
    }
}

/// How often progress is reported, at most. A copy of small files makes
/// thousands of steps a second, and a message per step would rebuild the
/// window thousands of times a second to move a number nobody can read
/// that fast.
const PROGRESS_EVERY: Duration = Duration::from_millis(100);

/// Starts `steps` on a worker thread.
pub fn start(
    job: JobId,
    steps: Vec<PasteStep>,
    on_conflict: OnConflict,
) -> (JobControl, mpsc::UnboundedReceiver<JobEvent>) {
    let (events, receiver) = mpsc::unbounded();
    let (decisions, answers) = std::sync::mpsc::channel();
    let cancel = Arc::new(AtomicBool::new(false));
    let control = JobControl { decisions, cancel: cancel.clone() };
    std::thread::Builder::new()
        .name(format!("files-job-{job}"))
        .spawn(move || {
            let summary = run(job, steps, on_conflict, &answers, &cancel, &events);
            let _ = events.unbounded_send(JobEvent::Finished { job, summary });
        })
        .expect("spawning a thread only fails when the process is out of resources");
    (control, receiver)
}

fn run(
    job: JobId,
    steps: Vec<PasteStep>,
    on_conflict: OnConflict,
    answers: &std::sync::mpsc::Receiver<CollisionDecision>,
    cancel: &AtomicBool,
    events: &mpsc::UnboundedSender<JobEvent>,
) -> JobSummary {
    let mut summary = JobSummary::default();
    let mut for_the_rest: Option<CollisionPolicy> = match on_conflict {
        OnConflict::Ask => None,
        OnConflict::KeepBoth => Some(CollisionPolicy::KeepBoth),
        OnConflict::Skip => Some(CollisionPolicy::Skip),
        OnConflict::Replace => Some(CollisionPolicy::Replace),
    };
    let mut last_progress: Option<Instant> = None;

    let of = steps.len();
    'items: for (done, step) in steps.into_iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            summary.cancelled = true;
            break;
        }
        let mut op = Operation::real(step.kind, &step.source, &step.dest);
        loop {
            if cancel.load(Ordering::Relaxed) {
                op.request_cancel();
            }
            match op.step() {
                StepOutcome::Progress(progress) => {
                    if last_progress.is_none_or(|at| at.elapsed() >= PROGRESS_EVERY) {
                        last_progress = Some(Instant::now());
                        let _ = events.unbounded_send(JobEvent::Progress { job, progress, items: Items { done, of } });
                    }
                }
                StepOutcome::Collision(collision) => {
                    let policy = if step.duplicate && collision.dest == step.dest {
                        CollisionPolicy::KeepBoth
                    } else if let Some(policy) = for_the_rest {
                        policy
                    } else {
                        let _ = events.unbounded_send(JobEvent::Collision { job, collision });
                        match answers.recv() {
                            Ok(decision) => {
                                if decision.apply_to_rest {
                                    for_the_rest = Some(decision.policy);
                                }
                                decision.policy
                            }
                            // The window is gone; nobody will answer.
                            Err(_) => CollisionPolicy::Cancel,
                        }
                    };
                    op.resolve(CollisionDecision { policy, apply_to_rest: false });
                }
                StepOutcome::Done(report) => {
                    // Before `failed` is drained: whether this item can be
                    // run again is decided by what it left behind. See
                    // `JobSummary::retry`.
                    if !report.cancelled
                        && report.succeeded.is_empty()
                        && !report.failed.is_empty()
                        && report.source_removal_failed.is_none()
                    {
                        summary.retry.push(step.clone());
                    } else if !report.failed.is_empty() && !report.cancelled {
                        summary.partly += 1;
                    }
                    summary.failed.extend(report.failed.into_iter().map(|(_, message)| message));
                    if let Some(message) = report.source_removal_failed {
                        summary.failed.push(message);
                    }
                    if report.cancelled {
                        summary.cancelled = true;
                        break 'items;
                    }
                    if report.succeeded.is_empty() && !report.skipped.is_empty() {
                        summary.skipped += 1;
                    } else if !report.succeeded.is_empty() {
                        summary.done += 1;
                        summary.placed.push((step.source.clone(), report.dest));
                    }
                    continue 'items;
                }
            }
        }
    }
    summary
}

/// Renames `from` to `to` in one go — a rename is a single `rename(2)`
/// in the same folder, too quick to be worth a job.
///
/// Through `Operation` rather than `std::fs::rename`, because
/// `std::fs::rename` replaces whatever is at `to` without a word. The
/// name was checked against the listing when Enter was pressed, but
/// something can appear in between; here that is a collision, answered
/// Cancel, and reported — never an overwrite.
pub fn rename(from: &std::path::Path, to: &std::path::Path) -> Result<(), String> {
    let mut op = Operation::real(hyprforge_fileops::OpKind::Move, from, to);
    loop {
        match op.step() {
            StepOutcome::Progress(_) => {}
            StepOutcome::Collision(_) => {
                op.resolve(CollisionDecision { policy: CollisionPolicy::Cancel, apply_to_rest: true });
            }
            StepOutcome::Done(report) => {
                if let Some((_, message)) = report.failed.into_iter().next() {
                    return Err(message);
                }
                if report.cancelled {
                    let name = to.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    return Err(format!("Something called \"{name}\" appeared there first."));
                }
                return Ok(());
            }
        }
    }
}

/// Makes a new, empty folder. `create_dir` and not `create_dir_all`, so
/// a folder that appeared under the same name in the meantime is an
/// error rather than silently "created" again.
pub fn create_folder(path: &std::path::Path) -> Result<(), String> {
    std::fs::create_dir(path).map_err(|e| {
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        match e.kind() {
            std::io::ErrorKind::AlreadyExists => format!("\"{name}\" already exists."),
            std::io::ErrorKind::PermissionDenied => {
                "You don't have permission to make a folder here.".to_string()
            }
            _ => format!("Couldn't make \"{name}\": {e}"),
        }
    })
}

/// Takes back one recorded action. Returns the folders whose listings
/// changed, and a sentence for each part that could not be undone.
///
/// Every branch checks that the world still matches the record before
/// touching anything, and refuses rather than guesses when it does not:
/// a file renamed again since, a folder that has things in it now, a
/// name that is taken again. Undoing a copy moves the copies to the
/// Trash and never deletes them.
pub fn undo(done: hyprforge_files_core::undo::Undoable) -> (Vec<std::path::PathBuf>, Vec<String>) {
    use hyprforge_files_core::undo::Undoable;
    use std::path::{Path, PathBuf};

    let mut dirs: Vec<PathBuf> = Vec::new();
    let mut errors = Vec::new();
    let touched = |dirs: &mut Vec<PathBuf>, path: &Path| {
        if let Some(parent) = path.parent() {
            if !dirs.iter().any(|d| d == parent) {
                dirs.push(parent.to_path_buf());
            }
        }
    };
    let exists = |path: &Path| std::fs::symlink_metadata(path).is_ok();
    let name = |path: &Path| {
        path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
    };

    // "Put `now` back at `was`" — shared by rename and move.
    let put_back = |dirs: &mut Vec<PathBuf>, errors: &mut Vec<String>, was: &Path, now: &Path| {
        if !exists(now) {
            errors.push(format!("\u{201C}{}\u{201D} isn't there any more.", name(now)));
        } else if exists(was) {
            errors.push(format!("Something called \u{201C}{}\u{201D} is back where it was.", name(was)));
        } else {
            match rename(now, was) {
                Ok(()) => {
                    touched(dirs, now);
                    touched(dirs, was);
                }
                Err(e) => errors.push(e),
            }
        }
    };

    match done {
        Undoable::Trashed(items) => {
            for (stored, original, record) in items {
                let item = hyprforge_fileops::TrashedItem {
                    original_path: original.clone(),
                    deleted_at: String::new(),
                    trashed_file: stored.clone(),
                    info_file: record,
                };
                match hyprforge_fileops::restore(&item) {
                    Ok(()) => {
                        dirs.push(stored.parent().map(Path::to_path_buf).unwrap_or_default());
                        touched(&mut dirs, &original);
                    }
                    Err(e) => errors.push(e.to_string()),
                }
            }
        }
        Undoable::Renamed { from, to } => put_back(&mut dirs, &mut errors, &from, &to),
        Undoable::Moved(items) => {
            for (was, now) in items {
                put_back(&mut dirs, &mut errors, &was, &now);
            }
        }
        Undoable::Copied(copies) => {
            for copy in copies {
                if !exists(&copy) {
                    continue;
                }
                match hyprforge_fileops::trash(&copy) {
                    Ok(_) => touched(&mut dirs, &copy),
                    Err(e) => errors.push(e.to_string()),
                }
            }
        }
        // Both go to the Trash rather than being deleted, the rule
        // `Copied` above already follows: an undo that loses a file is
        // worse than no undo, and an extracted folder may well have
        // things in it by now that the extraction did not put there.
        Undoable::Extracted(into) | Undoable::Compressed(into) => {
            if exists(&into) {
                match hyprforge_fileops::trash(&into) {
                    Ok(_) => touched(&mut dirs, &into),
                    Err(e) => errors.push(e.to_string()),
                }
            }
        }
        Undoable::MadeFolder(path) => match std::fs::remove_dir(&path) {
            Ok(()) => touched(&mut dirs, &path),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) if e.kind() == std::io::ErrorKind::DirectoryNotEmpty => errors.push(format!(
                "\u{201C}{}\u{201D} has things in it now, so it was left where it is.",
                name(&path)
            )),
            Err(e) => errors.push(format!("{}: {e}", name(&path))),
        },
    }
    (dirs, errors)
}

#[cfg(test)]
mod tests {
    use super::*;
    use hyprforge_fileops::OpKind;
    use iced::futures::channel::mpsc::TryRecvError;
    use std::fs;
    use std::path::Path;

    /// Collects a job's events, answering each collision with `answer`.
    /// Bounded: a job that never finishes fails the test instead of
    /// hanging it.
    fn drive(
        control: &JobControl,
        mut events: mpsc::UnboundedReceiver<JobEvent>,
        mut answer: impl FnMut(&Collision) -> CollisionDecision,
    ) -> (JobSummary, usize) {
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut collisions = 0;
        while Instant::now() < deadline {
            match events.try_recv() {
                Ok(JobEvent::Finished { summary, .. }) => return (summary, collisions),
                Ok(JobEvent::Collision { collision, .. }) => {
                    collisions += 1;
                    control.answer(answer(&collision));
                }
                Ok(JobEvent::Progress { .. }) => {}
                Err(TryRecvError::Closed) => panic!("the job ended without finishing"),
                Err(TryRecvError::Empty) => std::thread::sleep(Duration::from_millis(5)),
            }
        }
        panic!("the job did not finish in time");
    }

    fn never(_: &Collision) -> CollisionDecision {
        panic!("no collision was expected");
    }

    fn step(source: &Path, dest: &Path, kind: OpKind) -> PasteStep {
        PasteStep { source: source.into(), dest: dest.into(), kind, duplicate: false }
    }

    #[test]
    fn a_copy_lands_and_the_original_stays() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.txt");
        fs::write(&a, "hello").unwrap();
        let b = dir.path().join("b.txt");
        let (control, events) = start(1, vec![step(&a, &b, OpKind::Copy)], OnConflict::Ask);
        let (summary, _) = drive(&control, events, never);
        assert_eq!(summary, JobSummary { done: 1, placed: vec![(a.clone(), b.clone())], ..JobSummary::default() });
        assert!(summary.complete());
        assert_eq!(fs::read_to_string(&b).unwrap(), "hello");
        assert!(a.exists());
    }

    #[test]
    fn a_move_takes_a_whole_folder() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        fs::create_dir_all(src.join("deep")).unwrap();
        fs::write(src.join("deep").join("f.txt"), "x").unwrap();
        let dest = dir.path().join("elsewhere");
        let (control, events) = start(1, vec![step(&src, &dest, OpKind::Move)], OnConflict::Ask);
        let (summary, _) = drive(&control, events, never);
        assert!(summary.complete(), "{summary:?}");
        assert!(dest.join("deep").join("f.txt").exists());
        assert!(!src.exists());
    }

    /// A copy into its own folder makes a second copy without asking —
    /// "replace this file with itself?" would be absurd.
    #[test]
    fn a_duplicate_is_kept_beside_the_original_without_asking() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.txt");
        fs::write(&a, "hello").unwrap();
        let dup = PasteStep { source: a.clone(), dest: a.clone(), kind: OpKind::Copy, duplicate: true };
        let (control, events) = start(1, vec![dup], OnConflict::Ask);
        let (summary, collisions) = drive(&control, events, never);
        assert_eq!(collisions, 0);
        assert!(summary.complete(), "{summary:?}");
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 2, "the original and one copy");
        assert_ne!(summary.placed[0].1, a, "the copy is recorded where it really is");
        assert!(summary.placed[0].1.exists());
    }

    /// A conflict pauses the job and the answer is carried out.
    #[test]
    fn a_conflict_is_asked_about_and_the_answer_is_carried_out() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.txt");
        let b = dir.path().join("b.txt");
        fs::write(&a, "new").unwrap();
        fs::write(&b, "old").unwrap();
        let (control, events) = start(1, vec![step(&a, &b, OpKind::Copy)], OnConflict::Ask);
        let (summary, collisions) = drive(&control, events, |c| {
            assert_eq!(c.dest, b);
            CollisionDecision { policy: CollisionPolicy::Replace, apply_to_rest: false }
        });
        assert_eq!(collisions, 1);
        assert!(summary.complete(), "{summary:?}");
        assert_eq!(fs::read_to_string(&b).unwrap(), "new");
    }

    /// "Apply to the rest" reaches the *next item*, not only the rest of
    /// the item it was ticked on.
    #[test]
    fn apply_to_the_rest_answers_every_later_item_too() {
        let dir = tempfile::tempdir().unwrap();
        let from = dir.path().join("from");
        let to = dir.path().join("to");
        fs::create_dir_all(&from).unwrap();
        fs::create_dir_all(&to).unwrap();
        let mut steps = Vec::new();
        for name in ["a", "b", "c"] {
            fs::write(from.join(name), "new").unwrap();
            fs::write(to.join(name), "old").unwrap();
            steps.push(step(&from.join(name), &to.join(name), OpKind::Copy));
        }
        let (control, events) = start(1, steps, OnConflict::Ask);
        let (summary, collisions) = drive(&control, events, |_| CollisionDecision {
            policy: CollisionPolicy::Skip,
            apply_to_rest: true,
        });
        assert_eq!(collisions, 1, "asked once, for the first");
        assert_eq!(summary.skipped, 3);
        assert!(!summary.complete());
        assert_eq!(fs::read_to_string(to.join("c")).unwrap(), "old");
    }

    /// With a configured policy nobody is asked at all.
    #[test]
    fn a_configured_policy_never_asks() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.txt");
        let b = dir.path().join("b.txt");
        fs::write(&a, "new").unwrap();
        fs::write(&b, "old").unwrap();
        let (control, events) = start(1, vec![step(&a, &b, OpKind::Copy)], OnConflict::KeepBoth);
        let (summary, collisions) = drive(&control, events, never);
        assert_eq!(collisions, 0);
        assert!(summary.complete());
        assert_eq!(fs::read_to_string(&b).unwrap(), "old", "the existing file is untouched");
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 3);
    }

    /// Cancelling a paused job answers its question, so the worker does
    /// not sit waiting forever, and nothing after it runs.
    #[test]
    fn cancelling_a_paused_job_ends_it() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.txt");
        let b = dir.path().join("b.txt");
        let c = dir.path().join("c.txt");
        fs::write(&a, "new").unwrap();
        fs::write(&b, "old").unwrap();
        let steps = vec![step(&a, &b, OpKind::Copy), step(&a, &c, OpKind::Copy)];
        let (control, events) = start(1, steps, OnConflict::Ask);
        let canceller = control.clone();
        let (summary, _) = drive(&control, events, move |_| {
            canceller.cancel();
            // `cancel` already answered; this second answer goes nowhere.
            CollisionDecision { policy: CollisionPolicy::Cancel, apply_to_rest: false }
        });
        assert!(summary.cancelled);
        assert!(!c.exists(), "the item after the cancel never ran");
        assert_eq!(fs::read_to_string(&b).unwrap(), "old");
    }

    /// If the window goes away mid-question, the job cancels rather
    /// than leaving a thread blocked for the life of the process.
    #[test]
    fn a_job_whose_window_went_away_cancels() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.txt");
        let b = dir.path().join("b.txt");
        fs::write(&a, "new").unwrap();
        fs::write(&b, "old").unwrap();
        let (control, mut events) = start(1, vec![step(&a, &b, OpKind::Copy)], OnConflict::Ask);
        drop(control);
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            assert!(Instant::now() < deadline, "the job did not end");
            match events.try_recv() {
                Ok(JobEvent::Finished { summary, .. }) => {
                    assert!(summary.cancelled);
                    break;
                }
                Ok(_) => {}
                Err(TryRecvError::Closed) => panic!("ended without finishing"),
                Err(TryRecvError::Empty) => std::thread::sleep(Duration::from_millis(5)),
            }
        }
        assert_eq!(fs::read_to_string(&b).unwrap(), "old");
    }

    // --- undo ---------------------------------------------------------------

    use hyprforge_files_core::undo::Undoable;

    #[test]
    fn undoing_a_rename_renames_it_back() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = (dir.path().join("a.txt"), dir.path().join("b.txt"));
        fs::write(&b, "x").unwrap();
        let (dirs, errors) = undo(Undoable::Renamed { from: a.clone(), to: b.clone() });
        assert!(errors.is_empty(), "{errors:?}");
        assert!(a.exists() && !b.exists());
        assert_eq!(dirs, [dir.path().to_path_buf()]);
    }

    /// Renamed again since: the record no longer describes the world,
    /// and nothing is touched.
    #[test]
    fn an_undo_that_no_longer_fits_is_refused_and_touches_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = (dir.path().join("a.txt"), dir.path().join("b.txt"));
        let (errors_gone, errors_taken);
        {
            let (_, errors) = undo(Undoable::Renamed { from: a.clone(), to: b.clone() });
            errors_gone = errors;
        }
        fs::write(&a, "someone else's").unwrap();
        fs::write(&b, "mine").unwrap();
        {
            let (_, errors) = undo(Undoable::Renamed { from: a.clone(), to: b.clone() });
            errors_taken = errors;
        }
        assert!(errors_gone[0].contains("isn't there"), "{errors_gone:?}");
        assert!(errors_taken[0].contains("is back where it was"), "{errors_taken:?}");
        assert_eq!(fs::read_to_string(&a).unwrap(), "someone else's");
        assert_eq!(fs::read_to_string(&b).unwrap(), "mine");
    }

    #[test]
    fn undoing_a_move_moves_everything_back() {
        let dir = tempfile::tempdir().unwrap();
        let there = dir.path().join("there");
        fs::create_dir(&there).unwrap();
        fs::write(there.join("a"), "1").unwrap();
        fs::write(there.join("b"), "2").unwrap();
        let moved = vec![
            (dir.path().join("a"), there.join("a")),
            (dir.path().join("b"), there.join("b")),
        ];
        let (_, errors) = undo(Undoable::Moved(moved));
        assert!(errors.is_empty(), "{errors:?}");
        assert!(dir.path().join("a").exists() && dir.path().join("b").exists());
    }

    /// A made folder goes only while it is still empty.
    #[test]
    fn undoing_a_new_folder_removes_it_only_if_it_is_still_empty() {
        let dir = tempfile::tempdir().unwrap();
        let empty = dir.path().join("New folder");
        fs::create_dir(&empty).unwrap();
        let (_, errors) = undo(Undoable::MadeFolder(empty.clone()));
        assert!(errors.is_empty() && !empty.exists());

        let used = dir.path().join("New folder 2");
        fs::create_dir(&used).unwrap();
        fs::write(used.join("keep.txt"), "x").unwrap();
        let (_, errors) = undo(Undoable::MadeFolder(used.clone()));
        assert!(errors[0].contains("has things in it"), "{errors:?}");
        assert!(used.join("keep.txt").exists());
    }

    /// Undoing a trash puts the item back — driven through a trash made
    /// in a temporary directory, never the real one.
    #[test]
    fn undoing_a_trash_puts_the_item_back() {
        let dir = tempfile::tempdir().unwrap();
        let home_trash = dir.path().join("Trash");
        let original = dir.path().join("docs").join("notes.txt");
        fs::create_dir_all(original.parent().unwrap()).unwrap();
        fs::write(&original, "hello").unwrap();
        let item = hyprforge_fileops::trash_into(
            &hyprforge_fileops::fs::mock::MockFilesystem::new(),
            &home_trash,
            &original,
        )
        .unwrap();
        assert!(!original.exists());

        let (_, errors) = undo(Undoable::Trashed(vec![(
            item.trashed_file.clone(),
            item.original_path.clone(),
            item.info_file.clone(),
        )]));
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(fs::read_to_string(&original).unwrap(), "hello");
        assert!(!item.info_file.exists(), "the record went with it");
    }

    #[test]
    fn a_rename_renames() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.txt");
        fs::write(&a, "x").unwrap();
        rename(&a, &dir.path().join("b.txt")).unwrap();
        assert!(dir.path().join("b.txt").exists());
        assert!(!a.exists());
    }

    /// The case `std::fs::rename` gets wrong: it would replace `b.txt`
    /// without a word.
    #[test]
    fn a_rename_onto_an_existing_name_is_refused_and_overwrites_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.txt");
        let b = dir.path().join("b.txt");
        fs::write(&a, "mine").unwrap();
        fs::write(&b, "theirs").unwrap();
        let err = rename(&a, &b).unwrap_err();
        assert!(err.contains("b.txt"), "{err}");
        assert_eq!(fs::read_to_string(&b).unwrap(), "theirs");
        assert!(a.exists());
    }

    #[test]
    fn a_new_folder_is_made_once() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("New folder");
        create_folder(&path).unwrap();
        assert!(path.is_dir());
        assert!(create_folder(&path).unwrap_err().contains("already exists"));
    }

    /// The item that failed whole is offered again; the one that landed
    /// is not — running it again would only collide with itself.
    #[test]
    fn only_an_item_that_landed_nothing_is_offered_again() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("gone.txt");
        let a = dir.path().join("a.txt");
        fs::write(&a, "x").unwrap();
        let failing = step(&missing, &dir.path().join("x.txt"), OpKind::Copy);
        let steps = vec![failing.clone(), step(&a, &dir.path().join("b.txt"), OpKind::Copy)];
        let (control, events) = start(1, steps, OnConflict::Ask);
        let (summary, _) = drive(&control, events, never);
        assert_eq!(summary.retry, [failing]);
    }

    /// A folder that got partway across is a failure with no retry: its
    /// half-copy is in the way of running it again, and neither answer
    /// fileops has for a folder (skip all of it, or delete what arrived)
    /// finishes the job. The landed half stays, and nothing offers to
    /// touch it.
    #[test]
    fn a_folder_that_partly_landed_is_not_offered_again() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        fs::create_dir(&src).unwrap();
        fs::write(src.join("fine.txt"), "x").unwrap();
        let locked = src.join("locked.txt");
        fs::write(&locked, "secret").unwrap();
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
        // Root reads anything, and then there is nothing to fail.
        if fs::read(&locked).is_ok() {
            eprintln!("HYPRFORGE-SKIP: running as a user that can read a mode-000 file");
            return;
        }
        let (control, events) =
            start(1, vec![step(&src, &dir.path().join("dest"), OpKind::Copy)], OnConflict::Ask);
        let (summary, _) = drive(&control, events, never);
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(summary.failed.len(), 1, "{summary:?}");
        assert!(summary.retry.is_empty(), "{summary:?}");
        assert_eq!(summary.partly, 1);
        assert!(dir.path().join("dest").join("fine.txt").exists());
    }

    /// Thousands of small files make thousands of steps a second, and
    /// each progress report is a message the window rebuilds itself for.
    /// The worker coalesces them to one per `PROGRESS_EVERY`, so the
    /// count is bounded by how long the job ran, not by how many files
    /// it had. Measured as a count of messages because that is the
    /// resource: a redraw per file is what this prevents.
    #[test]
    fn a_copy_of_thousands_of_small_files_reports_at_a_bounded_rate() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("many");
        fs::create_dir(&src).unwrap();
        for n in 0..3000 {
            fs::write(src.join(format!("{n}.txt")), "x").unwrap();
        }
        let started = Instant::now();
        let (_control, mut events) =
            start(1, vec![step(&src, &dir.path().join("copy"), OpKind::Copy)], OnConflict::Ask);
        let mut reports = 0usize;
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            assert!(Instant::now() < deadline, "the copy did not finish");
            match events.try_recv() {
                Ok(JobEvent::Progress { .. }) => reports += 1,
                Ok(JobEvent::Finished { summary, .. }) => {
                    assert!(summary.complete(), "{summary:?}");
                    break;
                }
                Ok(JobEvent::Collision { .. }) => panic!("nothing was in the way"),
                Err(TryRecvError::Closed) => panic!("ended without finishing"),
                Err(TryRecvError::Empty) => std::thread::sleep(Duration::from_millis(2)),
            }
        }
        let ran = started.elapsed();
        let bound = (ran.as_millis() / PROGRESS_EVERY.as_millis()) as usize + 2;
        assert!(reports <= bound, "{reports} reports in {ran:?} — more than one per {PROGRESS_EVERY:?}");
        assert!(reports < 3000, "a report per file is the thing this prevents");
    }

    #[test]
    fn a_failure_is_reported_in_words_and_the_rest_still_runs() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("gone.txt");
        let a = dir.path().join("a.txt");
        fs::write(&a, "x").unwrap();
        let steps = vec![
            step(&missing, &dir.path().join("x.txt"), OpKind::Copy),
            step(&a, &dir.path().join("b.txt"), OpKind::Copy),
        ];
        let (control, events) = start(1, steps, OnConflict::Ask);
        let (summary, _) = drive(&control, events, never);
        assert_eq!(summary.done, 1);
        assert_eq!(summary.failed.len(), 1, "{summary:?}");
        assert!(!summary.complete());
    }
}
