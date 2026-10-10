//! What the transfers popover and the queue view know, apart from how
//! they are drawn: the session's finished jobs, the one number the
//! window chrome shows for all the running ones, and the words for why
//! a queued job is waiting.
//!
//! None of it starts, stops or schedules anything. The jobs are
//! [`crate::jobs`] and [`crate::archive_jobs`], the queue is the
//! window's, and this is the record they leave and the summary of what
//! they are doing — kept apart from the window so each decision below
//! has a test that does not need one.

use crate::archive_jobs::Work;
use crate::jobs::{Items, JobId, JobSummary};
use hyprforge_files_core::clipboard::PasteStep;
use hyprforge_fileops::Progress;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};

// --- how a finished job went -----------------------------------------------

/// How a job ended, as one word the queue view can colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Done,
    /// Something could not be done. Wins over `Cancelled`: a job that
    /// failed on three files and was then stopped still has three
    /// sentences somebody needs to read.
    Failed,
    Cancelled,
}

impl Outcome {
    pub fn of(summary: &JobSummary) -> Outcome {
        if !summary.failed.is_empty() {
            Outcome::Failed
        } else if summary.cancelled {
            Outcome::Cancelled
        } else {
            Outcome::Done
        }
    }
}

/// A finished job's failure sentences, each once, and how many there
/// were.
///
/// Bounded twice, because a permission-denied sweep over a mounted
/// share fails on every one of ten thousand files and the history keeps
/// the record for the whole session: identical sentences collapse into
/// one, and past [`Reasons::KEPT`] distinct ones only the count is kept.
/// The panel shows fewer still; this is the cap on what is *held*.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Reasons {
    kept: Vec<String>,
    /// Distinct sentences, including those not kept.
    distinct: usize,
    /// Every failure, repeats included — the number the headline says.
    total: usize,
}

impl Reasons {
    pub const KEPT: usize = 64;

    pub fn new(failed: &[String]) -> Reasons {
        let mut seen = std::collections::HashSet::new();
        let mut kept = Vec::new();
        for reason in failed {
            if seen.insert(reason.as_str()) && kept.len() < Self::KEPT {
                kept.push(reason.clone());
            }
        }
        Reasons { kept, distinct: seen.len(), total: failed.len() }
    }

    /// The first `at_most` distinct sentences, and how many distinct
    /// ones are left over after them.
    pub fn shown(&self, at_most: usize) -> (&[String], usize) {
        let shown = &self.kept[..self.kept.len().min(at_most)];
        (shown, self.distinct - shown.len())
    }

    pub fn total(&self) -> usize {
        self.total
    }

    pub fn is_empty(&self) -> bool {
        self.total == 0
    }
}

/// What it would take to do a failed job's work again — offered only
/// where doing it again is safe. `R` is the window's own description of
/// a piece of work; this module only holds it.
#[derive(Debug, Clone, PartialEq)]
pub struct Retry<R> {
    /// How many items it covers, for the button's label.
    pub items: usize,
    pub work: R,
}

/// One job that has finished this session.
#[derive(Debug, Clone, PartialEq)]
pub struct Finished<R> {
    pub id: JobId,
    /// "Copied", "Extracted" — capitalised, the start of [`Self::title`].
    pub done_word: &'static str,
    /// "3 items to Downloads", "release.tar.zst" — see [`paste_subject`].
    pub subject: String,
    pub outcome: Outcome,
    pub reasons: Reasons,
    /// Something the job put down, for "Show": the window goes to its
    /// folder and selects it.
    pub show: Option<PathBuf>,
    pub retry: Option<Retry<R>>,
    /// The same again through the administrator helper, for the items
    /// that were refused for permission — offered beside [`Self::retry`]
    /// when the helper is installed and the job did not already run as
    /// administrator.
    pub admin_retry: Option<Retry<R>>,
    /// Why a failure has no retry, when it has none — said, because a
    /// failed row with no way forward reads as an omission otherwise.
    pub note: Option<String>,
    /// Whether its failures have been read — dismissed from the window's
    /// failure panel, or opened in the queue view. Kept in the history
    /// either way; this only says whether they still need saying.
    pub seen: bool,
}

impl<R> Finished<R> {
    pub fn title(&self) -> String {
        format!("{} {}", self.done_word, self.subject)
    }

    /// The failure panel's line: "3 items couldn't be copied".
    pub fn headline(&self) -> String {
        format!(
            "{} couldn't be {}",
            plural(self.reasons.total(), "item", "items"),
            self.done_word.to_lowercase()
        )
    }
}

/// The session's finished jobs, newest first.
///
/// A record, not a log: bounded at [`History::KEPT`], and what goes
/// first when it is full is the oldest entry that has nothing left to
/// say — a job that worked, or a failure already read. An unread
/// failure outlasts a newer success, because a failure is the one
/// outcome that has to outlast the thing that produced it (see the
/// failure panel in `main.rs`); only when *every* entry is an unread
/// failure does the oldest of them go, because a bound that can be
/// argued past is not one.
#[derive(Debug, Clone)]
pub struct History<R> {
    entries: VecDeque<Finished<R>>,
}

impl<R> Default for History<R> {
    fn default() -> Self {
        History { entries: VecDeque::new() }
    }
}

impl<R> History<R> {
    pub const KEPT: usize = 50;

    pub fn push(&mut self, finished: Finished<R>) {
        self.entries.push_front(finished);
        while self.entries.len() > Self::KEPT {
            let quiet = self
                .entries
                .iter()
                .rposition(|f| f.outcome != Outcome::Failed || f.seen)
                .unwrap_or(self.entries.len() - 1);
            self.entries.remove(quiet);
        }
    }

    /// Newest first.
    pub fn iter(&self) -> impl Iterator<Item = &Finished<R>> {
        self.entries.iter()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Failures nobody has read yet, newest first.
    pub fn unseen_failures(&self) -> impl Iterator<Item = &Finished<R>> {
        self.entries.iter().filter(|f| f.outcome == Outcome::Failed && !f.seen)
    }

    /// Marks every failure read. They stay in the history.
    pub fn acknowledge(&mut self) {
        for entry in &mut self.entries {
            entry.seen = true;
        }
    }

    /// Forgets everything — "Clear finished", asked for in the one place
    /// that lists what is being cleared.
    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// Takes job `id`'s retry, once. A second press of the same button
    /// finds nothing, so a double click cannot queue the work twice.
    pub fn take_retry(&mut self, id: JobId) -> Option<R> {
        let entry = self.entries.iter_mut().find(|f| f.id == id)?;
        entry.seen = true;
        // Either button spends both: the two cover the same failed
        // items, and pressing one then the other would queue them twice.
        entry.admin_retry = None;
        entry.retry.take().map(|retry| retry.work)
    }

    /// Takes job `id`'s retry as administrator, once — and its ordinary
    /// retry with it, for the reason [`Self::take_retry`] gives.
    pub fn take_admin_retry(&mut self, id: JobId) -> Option<R> {
        let entry = self.entries.iter_mut().find(|f| f.id == id)?;
        entry.seen = true;
        entry.retry = None;
        entry.admin_retry.take().map(|retry| retry.work)
    }
}

// --- the one number the chrome shows ---------------------------------------

/// Overall progress across every job since the window was last idle.
///
/// The chrome shows one bar for all of them, and it must not go
/// backwards for no reason. Three things would make a naive sum do it,
/// and each is handled here:
///
/// - **A job finishing.** If finished jobs left the sum, the bar would
///   drop every time one ended — two jobs at 90% and 10% read 50%, then
///   the first finishes and it reads 10%. So a job stays in the batch,
///   counted whole, until the window is idle again.
/// - **Units.** Bytes are the honest measure when every job knows its
///   total; a queued job, or one still walking its tree, does not. Then
///   the batch falls back to counting jobs, each weighted by its own
///   fraction — and moving between the two can step down. So the
///   number shown is held at its high-water mark while the batch's
///   membership is the same.
/// - **More work.** A job joining *is* a reason to go back: the same
///   bar now stands for more. That is the one time the mark resets.
#[derive(Debug, Clone, Default)]
pub struct Batch {
    members: Vec<(JobId, Share)>,
    shown: Option<f32>,
}

#[derive(Debug, Clone, PartialEq)]
enum Share {
    Waiting,
    Running {
        /// `(done, total)` for the whole job, when it is known — see
        /// [`job_bytes`].
        bytes: Option<(u64, u64)>,
        fraction: Option<f32>,
    },
    /// Done, with its whole byte total if that was ever known.
    Finished(Option<u64>),
}

impl Batch {
    /// Work asked for — queued or started.
    pub fn join(&mut self, id: JobId) {
        if !self.members.iter().any(|(m, _)| *m == id) {
            self.members.push((id, Share::Waiting));
            self.shown = None;
            self.settle();
        }
    }

    /// Work called off before it started: it was never going to be
    /// done, so it leaves rather than being counted finished.
    pub fn leave(&mut self, id: JobId) {
        let before = self.members.len();
        self.members.retain(|(m, _)| *m != id);
        if self.members.len() != before {
            self.shown = None;
            self.settle();
        }
    }

    pub fn progress(&mut self, id: JobId, progress: &Progress, items: Items) {
        if let Some((_, share)) = self.members.iter_mut().find(|(m, _)| *m == id) {
            *share = Share::Running {
                bytes: job_bytes(progress, items),
                fraction: job_fraction(progress, items),
            };
            self.settle();
        }
    }

    pub fn started(&mut self, id: JobId) {
        if let Some((_, share)) = self.members.iter_mut().find(|(m, _)| *m == id) {
            if *share == Share::Waiting {
                *share = Share::Running { bytes: None, fraction: None };
            }
        }
    }

    pub fn finish(&mut self, id: JobId) {
        if let Some((_, share)) = self.members.iter_mut().find(|(m, _)| *m == id) {
            let bytes = match share {
                Share::Running { bytes: Some((_, total)), .. } => Some(*total),
                _ => None,
            };
            *share = Share::Finished(bytes);
            self.settle();
        }
    }

    /// Nothing running and nothing queued: the next job starts a new
    /// batch, at zero.
    pub fn clear(&mut self) {
        self.members.clear();
        self.shown = None;
    }

    /// The fraction to draw, or `None` when there is none yet — a batch
    /// of jobs that are all still counting has no honest number.
    pub fn fraction(&self) -> Option<f32> {
        self.shown
    }

    fn settle(&mut self) {
        let Some(now) = self.compute() else {
            return;
        };
        self.shown = Some(self.shown.map_or(now, |was| was.max(now)));
    }

    fn compute(&self) -> Option<f32> {
        if self.members.is_empty() {
            return None;
        }
        // Bytes, when every member can say how many.
        let mut done = 0u64;
        let mut total = 0u64;
        let mut all_bytes = true;
        for (_, share) in &self.members {
            match share {
                Share::Finished(Some(t)) => {
                    done += t;
                    total += t;
                }
                // Finished without ever reporting a size: it weighed
                // nothing in bytes, and it is done.
                Share::Finished(None) => {}
                Share::Running { bytes: Some((d, t)), .. } => {
                    done += (*d).min(*t);
                    total += t;
                }
                _ => all_bytes = false,
            }
        }
        if all_bytes && total > 0 {
            return Some((done as f64 / total as f64) as f32);
        }
        // Otherwise, jobs: each counts one, filled by its own fraction.
        let mut any = false;
        let mut sum = 0.0f64;
        for (_, share) in &self.members {
            sum += match share {
                Share::Finished(_) => {
                    any = true;
                    1.0
                }
                Share::Running { fraction: Some(f), .. } => {
                    any = true;
                    *f as f64
                }
                _ => 0.0,
            };
        }
        any.then(|| (sum / self.members.len() as f64) as f32)
    }
}

/// A whole job's fraction, from its current item's report.
///
/// `fileops` runs one operation per pasted item and each counts from
/// zero, so a job copying a large folder and then a small one would
/// draw a bar that fills, empties and fills again — the old panel did.
/// Instead each item is an equal share, filled by its own fraction:
/// `(items done + this item's fraction) / items`. At the moment one item
/// ends and the next begins, both sides of that are the same number, so
/// the bar never steps back. Weighting by bytes would be better and
/// cannot be done honestly: the next item's size is not known until
/// fileops has walked it.
///
/// `None` only while there is nothing at all to go on — the first item
/// still being counted.
pub fn job_fraction(progress: &Progress, items: Items) -> Option<f32> {
    let of = items.of.max(1);
    let here = own_fraction(progress);
    if here.is_none() && items.done == 0 {
        return None;
    }
    Some(((items.done as f32 + here.unwrap_or(0.0)) / of as f32).clamp(0.0, 1.0))
}

/// A whole job's `(bytes done, bytes total)` — known only for a job of
/// one item, whose current item *is* the job.
pub fn job_bytes(progress: &Progress, items: Items) -> Option<(u64, u64)> {
    (items.of <= 1).then_some(())?;
    progress.bytes_total.map(|total| (progress.bytes_done, total))
}

/// One item's own fraction: bytes where it knows them, entries
/// otherwise, `None` while its totals are still being counted.
pub fn own_fraction(progress: &Progress) -> Option<f32> {
    if let Some(total) = progress.bytes_total.filter(|t| *t > 0) {
        return Some((progress.bytes_done as f64 / total as f64).clamp(0.0, 1.0) as f32);
    }
    let total = progress.entries_total.filter(|t| *t > 0)?;
    Some((progress.entries_done as f64 / total as f64).clamp(0.0, 1.0) as f32)
}

// --- why a queued job is waiting -------------------------------------------

/// Why something in the queue has not started.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Waiting {
    /// As many jobs as run at once already are.
    Busy,
    /// Another job is rewriting the same archive, and two rewrites of
    /// one file at once lose one of them. Carries the archive's name.
    Behind(String),
    /// Another job is putting something at the same path, and only one
    /// of them can find it free. Carries the name.
    SameName(String),
}

impl Waiting {
    pub fn words(&self) -> String {
        match self {
            Waiting::Busy => "Waiting for a running job to finish".to_string(),
            Waiting::Behind(name) => {
                format!("Waiting for the other change to \u{201C}{name}\u{201D} to finish")
            }
            Waiting::SameName(name) => {
                format!("Waiting for the other job putting \u{201C}{name}\u{201D} there to finish")
            }
        }
    }
}

// --- the words for a job ---------------------------------------------------

/// What a paste is about, without its verb: "\u{201C}report.pdf\u{201D}
/// to Documents", "3 items to Documents".
pub fn paste_subject(steps: &[PasteStep]) -> String {
    let what = match steps {
        [one] => quoted_name(&one.source),
        many => plural(many.len(), "item", "items"),
    };
    let into: Vec<&Path> = steps.iter().filter_map(|s| s.dest.parent()).collect();
    match into.first() {
        Some(first) if into.iter().all(|p| p == first) => format!("{what} to {}", folder_name(first)),
        _ => what,
    }
}

/// What a finished copy or move did, said whole: what, from where, to
/// where, and how long it took — "Moved \u{201C}report.pdf\u{201D} from
/// Downloads to Documents in 1.2s". The line a notification (or the
/// window's undo strip) carries, which has to make sense read away from
/// the window that did it.
///
/// `placed` is each item's source and where it landed. Several items
/// from several folders say how many folders rather than naming one.
pub fn finished_line(verb: &str, placed: &[(PathBuf, PathBuf)], took: std::time::Duration) -> String {
    let what = match placed {
        [(from, _)] => quoted_name(from),
        many => plural(many.len(), "item", "items"),
    };
    let side = |paths: Vec<&Path>| -> Option<String> {
        let first = *paths.first()?;
        if paths.iter().all(|p| *p == first) {
            Some(folder_name(first))
        } else {
            let mut distinct = paths.clone();
            distinct.sort();
            distinct.dedup();
            Some(plural(distinct.len(), "folder", "folders"))
        }
    };
    let from = side(placed.iter().filter_map(|(f, _)| f.parent()).collect());
    let to = side(placed.iter().filter_map(|(_, t)| t.parent()).collect());
    let mut line = format!("{verb} {what}");
    if let Some(from) = from {
        line.push_str(&format!(" from {from}"));
    }
    if let Some(to) = to {
        line.push_str(&format!(" to {to}"));
    }
    format!("{line} in {}", took_words(took))
}

/// How long something took, as a person would say it: "<0.1s",
/// "0.3s", "12s", "2m 05s".
pub fn took_words(took: std::time::Duration) -> String {
    let secs = took.as_secs_f64();
    // A move on one disk is a rename, done before the clock moves:
    // "0.0s" reads as nothing having happened.
    if secs < 0.05 {
        "<0.1s".to_string()
    } else if secs < 10.0 {
        format!("{secs:.1}s")
    } else if secs < 60.0 {
        format!("{}s", secs.round() as u64)
    } else {
        let whole = secs.round() as u64;
        format!("{}m {:02}s", whole / 60, whole % 60)
    }
}

/// A restore's subject: where things go back to is each one's own
/// folder, so the subject names where they come from instead.
pub fn restore_subject(steps: &[PasteStep]) -> String {
    let what = match steps {
        [one] => quoted_name(&one.dest),
        many => plural(many.len(), "item", "items"),
    };
    format!("{what} from the Trash")
}

pub fn archive_subject(work: &Work) -> String {
    match work {
        Work::Extract { archives, .. } => match archives.as_slice() {
            [one] => quoted_name(one),
            many => plural(many.len(), "archive", "archives"),
        },
        Work::ExtractMembers { archive, members, .. } => {
            format!("{} from {}", plural(members.len(), "item", "items"), quoted_name(archive))
        }
        Work::Compress { dest, .. } => quoted_name(dest),
        Work::Edit { archive, .. } => quoted_name(archive),
    }
}

/// The chrome's label: what is going on, in as few words as hold it.
pub fn indicator_label(running: usize, queued: usize, single: Option<&str>, fraction: Option<f32>) -> String {
    let what = match (running + queued, single) {
        (1, Some(doing)) => doing.to_string(),
        (n, _) => plural(n, "transfer", "transfers"),
    };
    match fraction {
        Some(f) => format!("{what} \u{00B7} {}%", percent(f)),
        None => what,
    }
}

/// A fraction as a whole percentage that never claims 100 early: a job
/// at 99.6% says 99, because "100%" beside a bar still moving reads as
/// stuck.
pub fn percent(fraction: f32) -> u32 {
    let p = (fraction.clamp(0.0, 1.0) * 100.0).floor() as u32;
    if fraction < 1.0 { p.min(99) } else { 100 }
}

fn quoted_name(path: &Path) -> String {
    format!("\u{201C}{}\u{201D}", folder_name(path))
}

fn folder_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

pub fn plural(n: usize, one: &str, many: &str) -> String {
    if n == 1 {
        format!("1 {one}")
    } else {
        format!("{n} {many}")
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    #[test]
    fn a_finished_move_says_what_from_where_to_where_and_how_long() {
        let placed = [(PathBuf::from("/h/Downloads/report.pdf"), PathBuf::from("/h/Documents/report.pdf"))];
        assert_eq!(
            finished_line("Moved", &placed, Duration::from_millis(1234)),
            "Moved \u{201C}report.pdf\u{201D} from Downloads to Documents in 1.2s"
        );
    }

    #[test]
    fn several_items_from_several_folders_count_the_folders() {
        let placed = [
            (PathBuf::from("/h/a/1.txt"), PathBuf::from("/h/out/1.txt")),
            (PathBuf::from("/h/b/2.txt"), PathBuf::from("/h/out/2.txt")),
            (PathBuf::from("/h/b/3.txt"), PathBuf::from("/h/out/3.txt")),
        ];
        assert_eq!(finished_line("Copied", &placed, Duration::from_secs(42)), "Copied 3 items from 2 folders to out in 42s");
    }

    #[test]
    fn a_long_job_says_minutes() {
        assert_eq!(took_words(Duration::from_secs(125)), "2m 05s");
        assert_eq!(took_words(Duration::from_millis(300)), "0.3s");
        assert_eq!(took_words(Duration::from_millis(2)), "<0.1s", "never \"0.0s\"");
    }

    use super::*;
    use hyprforge_fileops::OpKind;

    fn bytes(done: u64, total: u64) -> Progress {
        Progress {
            bytes_done: done,
            bytes_total: Some(total),
            entries_done: 0,
            entries_total: Some(1),
            current: PathBuf::new(),
        }
    }

    fn counting() -> Progress {
        Progress { bytes_done: 0, bytes_total: None, entries_done: 0, entries_total: None, current: PathBuf::new() }
    }

    fn finished(id: JobId, outcome: Outcome) -> Finished<()> {
        Finished {
            id,
            done_word: "Copied",
            subject: format!("job {id}"),
            outcome,
            reasons: Reasons::default(),
            show: None,
            retry: None,
            admin_retry: None,
            note: None,
            seen: false,
        }
    }

    // --- the batch ---------------------------------------------------------

    /// The bug a sum over *running* jobs has: one finishing drops the bar.
    #[test]
    fn a_job_finishing_never_moves_the_bar_back() {
        let mut batch = Batch::default();
        batch.join(1);
        batch.join(2);
        batch.progress(1, &bytes(90, 100), Items::default());
        batch.progress(2, &bytes(10, 100), Items::default());
        assert_eq!(batch.fraction(), Some(0.5));
        batch.finish(1);
        assert!(batch.fraction().unwrap() >= 0.5, "{:?}", batch.fraction());
        batch.progress(2, &bytes(50, 100), Items::default());
        assert_eq!(batch.fraction(), Some(0.75), "bytes: 150 of 200");
    }

    /// Bytes, not jobs, when every job knows its size: a 1GB copy at
    /// half is most of the work, beside a 1KB one that is done.
    #[test]
    fn bytes_weigh_the_batch_when_every_job_knows_them() {
        let mut batch = Batch::default();
        batch.join(1);
        batch.join(2);
        batch.progress(1, &bytes(500, 1000), Items::default());
        batch.progress(2, &bytes(1, 1), Items::default());
        batch.finish(2);
        let f = batch.fraction().unwrap();
        assert!((f - 501.0 / 1001.0).abs() < 1e-4, "{f}");
    }

    /// A queued job has no size yet, so the batch counts jobs — and
    /// when it starts and reports bytes, the mixed measure must not
    /// step backwards.
    #[test]
    fn changing_measure_holds_the_bar_rather_than_dropping_it() {
        let mut batch = Batch::default();
        batch.join(1);
        batch.join(2);
        batch.progress(1, &bytes(80, 100), Items::default());
        batch.finish(1);
        // Jobs: one done of two.
        assert_eq!(batch.fraction(), Some(0.5));
        batch.started(2);
        // Now bytes: 100 + 0 of 100 + 10_000 — under 1%. Held.
        batch.progress(2, &bytes(0, 10_000), Items::default());
        assert_eq!(batch.fraction(), Some(0.5));
        batch.progress(2, &bytes(9_900, 10_000), Items::default());
        assert!(batch.fraction().unwrap() > 0.98);
    }

    /// The one honest reason to go back: more work.
    #[test]
    fn a_job_joining_is_the_one_time_the_bar_may_go_back() {
        let mut batch = Batch::default();
        batch.join(1);
        batch.progress(1, &bytes(90, 100), Items::default());
        batch.join(2);
        assert_eq!(batch.fraction(), Some(0.45), "two jobs, one 90% along");
    }

    /// While every job is still counting its tree there is no fraction,
    /// and none is invented.
    #[test]
    fn a_batch_still_counting_has_no_fraction() {
        let mut batch = Batch::default();
        batch.join(1);
        batch.started(1);
        batch.progress(1, &counting(), Items::default());
        assert_eq!(batch.fraction(), None);
    }

    #[test]
    fn cancelling_queued_work_takes_it_out_of_the_sum() {
        let mut batch = Batch::default();
        batch.join(1);
        batch.join(2);
        batch.progress(1, &bytes(50, 100), Items::default());
        batch.leave(2);
        assert_eq!(batch.fraction(), Some(0.5));
    }

    /// The bug the old panel had, found live: a paste of a large folder
    /// and then a small one filled the bar, emptied it, and filled it
    /// again, because each item's report counts from zero.
    #[test]
    fn a_job_of_several_items_never_empties_its_bar_between_them() {
        let first_done = job_fraction(&bytes(100, 100), Items { done: 0, of: 2 }).unwrap();
        let second_starts = job_fraction(&bytes(0, 5), Items { done: 1, of: 2 }).unwrap();
        assert_eq!(first_done, 0.5);
        assert_eq!(second_starts, 0.5, "the two sides of the boundary are one number");
        assert_eq!(job_fraction(&bytes(5, 5), Items { done: 1, of: 2 }), Some(1.0));
        // Counting the second item's tree says nothing new, but the
        // first is still done.
        assert_eq!(job_fraction(&counting(), Items { done: 1, of: 2 }), Some(0.5));
        assert_eq!(job_fraction(&counting(), Items { done: 0, of: 2 }), None);
    }

    /// A job's byte total is only the job's when it has one item.
    #[test]
    fn only_a_single_item_job_claims_a_byte_total() {
        assert_eq!(job_bytes(&bytes(5, 10), Items::default()), Some((5, 10)));
        assert_eq!(job_bytes(&bytes(5, 10), Items { done: 1, of: 2 }), None);
    }

    // --- the history -------------------------------------------------------

    /// The history is bounded, and what it gives up first is what has
    /// nothing left to say.
    #[test]
    fn a_full_history_drops_old_successes_before_an_unread_failure() {
        let mut history = History::default();
        history.push(finished(0, Outcome::Failed));
        for id in 1..=History::<()>::KEPT as JobId + 10 {
            history.push(finished(id, Outcome::Done));
        }
        assert_eq!(history.len(), History::<()>::KEPT);
        assert!(history.iter().any(|f| f.id == 0), "the unread failure is still there");
        assert_eq!(history.unseen_failures().count(), 1);
    }

    #[test]
    fn a_history_of_nothing_but_unread_failures_is_still_bounded() {
        let mut history = History::default();
        for id in 0..History::<()>::KEPT as JobId * 3 {
            history.push(finished(id, Outcome::Failed));
        }
        assert_eq!(history.len(), History::<()>::KEPT);
        assert_eq!(history.iter().next().unwrap().id, History::<()>::KEPT as JobId * 3 - 1, "newest first");
    }

    /// Reading a failure does not delete it — the queue view still
    /// lists it with its reasons.
    #[test]
    fn dismissing_failures_keeps_them_in_the_history() {
        let mut history = History::default();
        history.push(finished(1, Outcome::Failed));
        history.acknowledge();
        assert_eq!(history.unseen_failures().count(), 0);
        assert_eq!(history.len(), 1);
    }

    #[test]
    fn a_retry_can_be_taken_only_once() {
        let mut history = History::default();
        let mut entry = finished(1, Outcome::Failed);
        entry.retry = Some(Retry { items: 1, work: () });
        history.push(entry);
        assert_eq!(history.take_retry(1), Some(()));
        assert_eq!(history.take_retry(1), None, "a double click queues nothing twice");
    }

    /// Retry and Retry as administrator cover the same failed items, so
    /// pressing one spends the other: pressing both must not queue them
    /// twice.
    #[test]
    fn either_retry_spends_both() {
        let mut history = History::default();
        for id in [1, 2] {
            let mut entry = finished(id, Outcome::Failed);
            entry.retry = Some(Retry { items: 1, work: () });
            entry.admin_retry = Some(Retry { items: 1, work: () });
            history.push(entry);
        }
        assert_eq!(history.take_admin_retry(1), Some(()));
        assert_eq!(history.take_retry(1), None);
        assert_eq!(history.take_retry(2), Some(()));
        assert_eq!(history.take_admin_retry(2), None);
    }

    // --- reasons -----------------------------------------------------------

    /// Ten thousand identical sentences are one sentence and a count,
    /// and ten thousand distinct ones are a bounded list and a count —
    /// held for the whole session, so held small.
    #[test]
    fn reasons_are_held_bounded_and_once_each() {
        let same = vec!["permission denied".to_string(); 10_000];
        let reasons = Reasons::new(&same);
        assert_eq!(reasons.shown(8), (&["permission denied".to_string()][..], 0));
        assert_eq!(reasons.total(), 10_000);

        let distinct: Vec<String> = (0..10_000).map(|n| format!("{n}: denied")).collect();
        let reasons = Reasons::new(&distinct);
        assert_eq!(reasons.kept.len(), Reasons::KEPT);
        let (shown, more) = reasons.shown(8);
        assert_eq!(shown.len(), 8);
        assert_eq!(more, 10_000 - 8);
    }

    #[test]
    fn a_failure_wins_over_a_cancel_in_the_outcome() {
        let summary = JobSummary { failed: vec!["x".into()], cancelled: true, ..JobSummary::default() };
        assert_eq!(Outcome::of(&summary), Outcome::Failed);
        let summary = JobSummary { cancelled: true, ..JobSummary::default() };
        assert_eq!(Outcome::of(&summary), Outcome::Cancelled);
    }

    // --- words -------------------------------------------------------------

    fn paste(source: &str, dest: &str) -> PasteStep {
        PasteStep { source: source.into(), dest: dest.into(), kind: OpKind::Copy, duplicate: false }
    }

    #[test]
    fn a_paste_is_named_by_what_and_where() {
        assert_eq!(
            paste_subject(&[paste("/a/report.pdf", "/home/me/Documents/report.pdf")]),
            "\u{201C}report.pdf\u{201D} to Documents"
        );
        assert_eq!(
            paste_subject(&[paste("/a/x", "/home/me/Pictures/x"), paste("/a/y", "/home/me/Pictures/y")]),
            "2 items to Pictures"
        );
    }

    #[test]
    fn the_chrome_says_one_jobs_verb_and_counts_several() {
        assert_eq!(indicator_label(1, 0, Some("Copying"), Some(0.42)), "Copying \u{00B7} 42%");
        assert_eq!(indicator_label(2, 1, Some("Copying"), None), "3 transfers");
    }

    /// "100%" beside a bar still moving reads as a hang.
    #[test]
    fn a_percentage_never_says_100_before_it_is() {
        assert_eq!(percent(0.996), 99);
        assert_eq!(percent(1.0), 100);
        assert_eq!(percent(0.0), 0);
    }

    #[test]
    fn a_queued_job_says_which_archive_it_is_behind() {
        assert!(Waiting::Behind("notes.zip".into()).words().contains("notes.zip"));
        assert!(Waiting::Busy.words().contains("running"));
    }
}
