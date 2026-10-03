//! Extracting and compressing without freezing the window.
//!
//! The same arrangement as [`crate::jobs`] and deliberately the same
//! *events*: a worker thread does the work, reports through an
//! `mpsc` stream the window turns into messages, and stops between any
//! two members when the window asks. Reusing [`JobEvent`] rather than
//! inventing a second progress channel is what lets the existing
//! progress bar, cancel button and status line work here with no change
//! at all — an archive job is a job.
//!
//! What is *not* shared is the collision conversation.
//! `hyprforge_archive` answers a collision from a
//! [`hyprforge_archive::Collision`] chosen before the run starts, where
//! a paste stops and asks per file. That is a real difference in
//! behaviour and it is on purpose: an extraction routinely puts hundreds
//! of files down at once, and a dialog per collision is not a
//! conversation anyone wants to have. The window passes the
//! `[behaviour] on-conflict` setting in, so someone who wants "ask"
//! everywhere still gets the safest of the three rather than a surprise.

use hyprforge_archive::backend::{
    Advance, ArchiveBackend, Collision, ExtractRequest, FailureReason, Flow,
    Progress as ArchiveProgress, Source,
};
use hyprforge_archive::{ArchiveError, Format, StdArchives, Unlock};
use hyprforge_files_core::config::OnConflict;
use hyprforge_fileops::Progress;
use iced::futures::channel::mpsc;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::jobs::{JobControl, JobEvent, JobId, JobSummary};

/// The same rate the paste job reports at, for the same reason.
const PROGRESS_EVERY: Duration = Duration::from_millis(100);

/// One piece of archive work.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Work {
    /// Unpack whole archives, each into `into` — or, when `into` is
    /// `None`, into a new folder beside itself named after it.
    Extract {
        archives: Vec<PathBuf>,
        into: Option<PathBuf>,
    },
    /// Unpack named members of one archive.
    ExtractMembers {
        archive: PathBuf,
        members: Vec<String>,
        /// Dropped from the front of each member's path, so extracting
        /// `docs/guide.txt` out of a folder you are standing in puts
        /// `guide.txt` down rather than `docs/guide.txt`.
        strip_prefix: Option<String>,
        into: PathBuf,
    },
    /// Make a new archive.
    Compress {
        sources: Vec<Source>,
        dest: PathBuf,
        format: Format,
    },
    /// Change one that exists.
    Edit {
        archive: PathBuf,
        edits: Vec<hyprforge_archive::Edit>,
    },
}

impl Work {
    /// The directories a finished job may have changed, so the window
    /// knows what to read again.
    pub fn touches(&self) -> Vec<PathBuf> {
        match self {
            Work::Extract { archives, into } => match into {
                Some(into) => vec![into.clone()],
                None => archives
                    .iter()
                    .filter_map(|a| a.parent().map(Path::to_path_buf))
                    .collect(),
            },
            Work::ExtractMembers { into, .. } => vec![into.clone()],
            Work::Compress { dest, .. } => {
                dest.parent().map(Path::to_path_buf).into_iter().collect()
            }
            // An edited archive is a different file, so both the folder
            // holding it and any listing *inside* it are stale.
            Work::Edit { archive, .. } => {
                let mut dirs = vec![archive.clone()];
                dirs.extend(archive.parent().map(Path::to_path_buf));
                dirs
            }
        }
    }

    /// What taking this back would mean, if anything.
    ///
    /// Worked out before the job runs, because the destination is known
    /// then and the summary does not carry it. `None` for an edit: see
    /// `hyprforge_files_core::undo`'s module doc on why an archive
    /// rewrite has no honest undo.
    pub fn undoes(&self) -> Undoes {
        match self {
            // Only for a single archive extracted into a folder this
            // job made. Extracting several at once, or into a folder
            // that was already there, has no one thing to take back —
            // and trashing a folder somebody chose, with whatever else
            // was in it, is not an undo.
            Work::Extract { archives, into: None } if archives.len() == 1 => Undoes::Extracted,
            Work::Extract { .. } | Work::ExtractMembers { .. } => Undoes::Nothing,
            Work::Compress { .. } => Undoes::Compressed,
            Work::Edit { .. } => Undoes::Nothing,
        }
    }

    /// The archive this work is about, when there is exactly one — so
    /// the caller can look up a password for it.
    ///
    /// `None` for a `Compress` (there is no archive yet) and for an
    /// `Extract` over several at once, where there is no single answer
    /// and each is unlocked as it is reached.
    pub fn archive(&self) -> Option<&Path> {
        match self {
            Work::Extract { archives, .. } => match archives.as_slice() {
                [only] => Some(only),
                _ => None,
            },
            Work::ExtractMembers { archive, .. } | Work::Edit { archive, .. } => Some(archive),
            Work::Compress { .. } => None,
        }
    }

    /// "Extracting", for the progress line.
    pub fn doing(&self) -> &'static str {
        match self {
            Work::Extract { .. } | Work::ExtractMembers { .. } => "Extracting",
            Work::Compress { .. } => "Compressing",
            Work::Edit { .. } => "Updating",
        }
    }
}

/// What taking a piece of work back would mean — the kind only; the
/// path comes from what the job reports it actually made.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Undoes {
    Nothing,
    Extracted,
    Compressed,
}

/// What the archive crate should do about something already at the
/// destination, given how this app is configured.
///
/// `Ask` becomes `Rename` rather than `Overwrite`: this is the one place
/// the setting cannot be honoured literally (see the module doc), and
/// the honest substitute for "ask me" is the answer that loses nothing.
fn collision_for(on_conflict: OnConflict) -> Collision {
    match on_conflict {
        OnConflict::Ask | OnConflict::KeepBoth => Collision::Rename,
        OnConflict::Skip => Collision::Skip,
        OnConflict::Replace => Collision::Overwrite,
    }
}

/// Where `Extract` with no destination puts an archive's contents: a
/// folder beside it named after it, with the archive extensions taken
/// off.
///
/// Into a folder, never loose into the current one — see
/// [`hyprforge_files_core::Action::Extract`]'s own doc on tarbombs. The
/// name is made free here rather than colliding, because the collision
/// policy is about the *files inside*, and having "Extract" quietly
/// merge two releases of the same project into one folder is a different
/// and worse surprise.
pub fn folder_for(archive: &Path) -> PathBuf {
    let parent = archive.parent().unwrap_or(Path::new("."));
    let name = archive
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "archive".to_string());
    // `.tar.gz` loses both halves, `.zip` loses one — the format knows
    // which, so the suffix is taken from there rather than guessed at
    // by counting dots.
    let stem = Format::by_name(archive)
        .map(|format| format!(".{}", format.extension()))
        .and_then(|suffix| {
            name.to_ascii_lowercase()
                .ends_with(&suffix)
                .then(|| name[..name.len() - suffix.len()].to_string())
        })
        .filter(|stem| !stem.is_empty())
        .unwrap_or_else(|| name.clone());

    let first = parent.join(&stem);
    if std::fs::symlink_metadata(&first).is_err() {
        return first;
    }
    for n in 2u32.. {
        let candidate = parent.join(format!("{stem} ({n})"));
        if std::fs::symlink_metadata(&candidate).is_err() {
            return candidate;
        }
    }
    unreachable!("the loop returns as soon as a name is free")
}

/// Starts `work` on a worker thread.
pub fn start(
    job: JobId,
    work: Work,
    on_conflict: OnConflict,
    unlock: Unlock,
) -> (JobControl, mpsc::UnboundedReceiver<JobEvent>) {
    let (events, receiver) = mpsc::unbounded();
    // An archive job never asks a question, so nothing is ever sent down
    // the decision channel — the `JobControl` is held for its cancel
    // flag, which is the half that matters here.
    let (decisions, _answers) = std::sync::mpsc::channel();
    let cancel = Arc::new(AtomicBool::new(false));
    let control = JobControl::new(decisions, cancel.clone());

    std::thread::Builder::new()
        .name(format!("files-archive-{job}"))
        .spawn(move || {
            let summary = run(job, work, on_conflict, &unlock, &cancel, &events);
            let _ = events.unbounded_send(JobEvent::Finished { job, summary });
        })
        .expect("spawning a thread only fails when the process is out of resources");
    (control, receiver)
}

/// Turns the archive crate's progress reports into the window's, and
/// carries the cancel flag the other way.
struct Reporter<'a> {
    job: JobId,
    cancel: &'a AtomicBool,
    events: &'a mpsc::UnboundedSender<JobEvent>,
    last: Option<Instant>,
    /// Members finished in *earlier* pieces of work, so a job over three
    /// archives counts up across all of them rather than restarting at
    /// each.
    offset: u64,
    /// Which archive of several this is — see `JobEvent::Progress`.
    items: crate::jobs::Items,
}

impl ArchiveProgress for Reporter<'_> {
    fn advance(&mut self, advance: Advance<'_>) -> Flow {
        if self.cancel.load(Ordering::Relaxed) {
            return Flow::Cancel;
        }
        if self.last.is_none_or(|at| at.elapsed() >= PROGRESS_EVERY) {
            self.last = Some(Instant::now());
            let _ = self.events.unbounded_send(JobEvent::Progress {
                job: self.job,
                items: self.items,
                progress: Progress {
                    bytes_done: advance.bytes_done,
                    // `None` when the archive never stated its members'
                    // sizes: a total of zero would draw a full bar over
                    // work that has not started. See
                    // `hyprforge_archive::Member::size_known`.
                    bytes_total: (advance.bytes_total > 0).then_some(advance.bytes_total),
                    entries_done: self.offset + advance.files_done as u64,
                    entries_total: (advance.files_total > 0)
                        .then_some(self.offset + advance.files_total as u64),
                    current: PathBuf::from(advance.member),
                },
            });
        }
        Flow::Continue
    }
}

fn run(
    job: JobId,
    work: Work,
    on_conflict: OnConflict,
    unlock: &Unlock,
    cancel: &AtomicBool,
    events: &mpsc::UnboundedSender<JobEvent>,
) -> JobSummary {
    let mut summary = JobSummary::default();
    let mut reporter = Reporter {
        job,
        cancel,
        events,
        last: None,
        offset: 0,
        items: crate::jobs::Items::default(),
    };
    let backend = StdArchives;
    let collision = collision_for(on_conflict);

    match work {
        Work::Extract { archives, into } => {
            let of = archives.len();
            for (done, archive) in archives.into_iter().enumerate() {
                reporter.items = crate::jobs::Items { done, of };
                if cancel.load(Ordering::Relaxed) {
                    summary.cancelled = true;
                    break;
                }
                let dest = into.clone().unwrap_or_else(|| folder_for(&archive));
                let request = ExtractRequest {
                    members: Vec::new(),
                    dest: dest.clone(),
                    strip_prefix: None,
                    collision,
                };
                // One archive failing does not abandon the others — the
                // rule this suite follows everywhere a listing or a
                // batch can go partly wrong. Each reports its own
                // sentence.
                match backend.extract_with(&archive, &request, unlock, &mut reporter) {
                    Ok(report) => absorb(&mut summary, &archive, &dest, report),
                    Err(e) if e.cancelled() => {
                        summary.cancelled = true;
                        break;
                    }
                    Err(ArchiveError::PasswordRequired { .. }) => {
                        // Recorded, not reported as a failure: the
                        // window turns this into a prompt, and calling
                        // it a failure first would put a sentence in
                        // the status bar that the prompt then
                        // contradicts.
                        summary.needs_password = Some(archive.clone());
                        break;
                    }
                    Err(e) => summary.failed.push(e.to_string()),
                }
                reporter.offset = summary.done as u64;
            }
        }
        Work::ExtractMembers {
            archive,
            members,
            strip_prefix,
            into,
        } => {
            let request = ExtractRequest {
                members,
                dest: into.clone(),
                strip_prefix,
                collision,
            };
            match backend.extract_with(&archive, &request, unlock, &mut reporter) {
                Ok(report) => absorb(&mut summary, &archive, &into, report),
                Err(e) if e.cancelled() => summary.cancelled = true,
                Err(ArchiveError::PasswordRequired { .. }) => {
                    summary.needs_password = Some(archive.clone())
                }
                Err(e) => summary.failed.push(e.to_string()),
            }
        }
        Work::Compress {
            sources,
            dest,
            format,
        } => match backend.create(&dest, format, &sources, &mut reporter) {
            Ok(()) => {
                summary.done = sources.len();
                // The new archive is the one thing this job placed, and
                // naming it is what lets an undo take it back.
                if let Some(first) = sources.first() {
                    summary.placed.push((first.path.clone(), dest));
                }
            }
            Err(e) if e.cancelled() => summary.cancelled = true,
            Err(e) => summary.failed.push(e.to_string()),
        },
        Work::Edit { archive, edits } => {
            let count = edits.len();
            match backend.edit_with(&archive, &edits, unlock, &mut reporter) {
                Ok(()) => summary.done = count,
                Err(e) if e.cancelled() => summary.cancelled = true,
                Err(ArchiveError::PasswordRequired { .. }) => {
                    summary.needs_password = Some(archive.clone())
                }
                Err(e) => summary.failed.push(e.to_string()),
            }
        }
    }

    summary
}

/// Folds one extraction's report into the job summary.
///
/// Every part of it, including the parts that did not happen: a member
/// skipped because something was already there, and a member that would
/// not decompress, both have to reach the person. A summary that counted
/// only successes would report "extracted 400 files" for an archive that
/// silently dropped three.
fn absorb(
    summary: &mut JobSummary,
    archive: &Path,
    dest: &Path,
    report: hyprforge_archive::ExtractReport,
) {
    summary.done += report.files;
    summary.skipped += report.skipped.len();
    for failure in report.failed {
        // A member that needed a password is not a failure to report —
        // it is a question, and the window asks it. Recording it as a
        // failure as well would put a sentence in the status bar
        // underneath the prompt that contradicts it.
        if failure.reason == FailureReason::NeedsPassword {
            summary.needs_password.get_or_insert_with(|| archive.to_path_buf());
            continue;
        }
        summary.failed.push(format!(
            "{} couldn't be extracted from {}: {}",
            failure.member,
            archive.display(),
            failure.message
        ));
    }
    // The destination, not each file: an undo of an extraction takes
    // back the folder, and listing four hundred paths to do it would
    // make the undo record larger than the listing it came from.
    summary.placed.push((archive.to_path_buf(), dest.to_path_buf()));
}

/// The format a "Compress…" choice names.
///
/// A short list on purpose: these are the four a person is actually
/// choosing between, and every one of them round-trips through this
/// suite's own reader.
pub const OFFERED: [(&str, Format); 4] = [
    ("zip", Format::Zip),
    (
        "tar.gz",
        Format::Tar(hyprforge_archive::Compression::Gzip),
    ),
    ("tar.xz", Format::Tar(hyprforge_archive::Compression::Xz)),
    ("7z", Format::SevenZ),
];

/// The error a caller reports when an archive path cannot be split — it
/// should not happen, and saying so beats a silent no-op.
pub fn not_in_an_archive(path: &Path) -> ArchiveError {
    ArchiveError::NotAnArchive {
        path: path.to_path_buf(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_archives_own_folder_loses_every_part_of_its_suffix() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            folder_for(&dir.path().join("linux-6.6.tar.gz")),
            dir.path().join("linux-6.6"),
            "`.tar.gz` is two extensions and both come off"
        );
        assert_eq!(
            folder_for(&dir.path().join("notes.zip")),
            dir.path().join("notes")
        );
    }

    #[test]
    fn extracting_twice_does_not_merge_two_archives_into_one_folder() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("release")).unwrap();
        assert_eq!(
            folder_for(&dir.path().join("release.zip")),
            dir.path().join("release (2)"),
            "a second extraction gets its own folder rather than merging"
        );
    }

    /// The setting cannot be honoured literally, so the substitute has
    /// to be the one that cannot lose anything.
    #[test]
    fn asking_about_conflicts_becomes_keeping_both_rather_than_overwriting() {
        assert_eq!(collision_for(OnConflict::Ask), Collision::Rename);
        assert_eq!(collision_for(OnConflict::Replace), Collision::Overwrite);
        assert_eq!(collision_for(OnConflict::Skip), Collision::Skip);
    }

    #[test]
    fn an_edited_archive_makes_both_it_and_its_folder_stale() {
        let work = Work::Edit {
            archive: PathBuf::from("/home/a/sample.zip"),
            edits: Vec::new(),
        };
        let touched = work.touches();
        assert!(touched.contains(&PathBuf::from("/home/a/sample.zip")), "{touched:?}");
        assert!(touched.contains(&PathBuf::from("/home/a")), "{touched:?}");
    }
}
