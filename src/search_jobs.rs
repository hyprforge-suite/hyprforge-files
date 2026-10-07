//! Running a search below a folder for a window — the host's half of
//! [`hyprforge_files_core::search`].
//!
//! Both hosts, the Files window and the open/save dialog, keep one
//! [`Searcher`] per browser and hand every [`Ask`] to it. It holds the
//! rule the browser relies on: **one walk per browser, and the newest
//! wins.** A new request cancels the walk that is running and waits for
//! it to wind down — one folder's read, at most — before starting, so a
//! burst of typing costs the walk that was running and the one for where
//! the typing stopped, never a thread per keystroke. The path bar's
//! resolve works the same way (`Tab::resolve_next`), and both follow
//! CLAUDE.md's rule from the video player: treat a signal as "look
//! again", never as a job of its own.
//!
//! # Every wait is bounded
//!
//! The walk checks its own clock between folders, so it normally ends by
//! itself within [`Budget::time`](hyprforge_files_core::search::Budget).
//! What it cannot do is interrupt a `read_dir` that never returns — a
//! network mount that stopped answering. So the stream that carries its
//! results has a deadline of its own, a little past the walk's: when it
//! passes, the walk is told to stop, the window is told the search ran
//! out of time, and the stuck worker is left to finish on its own,
//! holding nothing the window needs. Folders on another filesystem are
//! not entered at all ([`elsewhere`]), which is the commoner way to meet
//! such a mount in the first place.

use hyprforge_files_core::content::{self, Opened, Refused};
use hyprforge_files_core::search::{walk, Ask, End, Request, Summary};
use hyprforge_files_core::{Entry, FsBackend};
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// How long past the walk's own time limit the window waits for it.
const GRACE: Duration = Duration::from_secs(2);

/// What a running search reports, for a host to turn into the browser's
/// [`hyprforge_files_core::search::SearchMessage`].
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    Found(u64, Vec<Entry>),
    Finished(u64, Summary),
}

/// One browser's searches: the walk running, and the newest asked for
/// while it was.
#[derive(Debug, Default)]
pub struct Searcher {
    running: Option<(u64, Arc<AtomicBool>)>,
    next: Option<Request>,
}

/// A walk to start now: the request and the flag that cancels it.
pub type Start = (Request, Arc<AtomicBool>);

impl Searcher {
    /// What to do about `ask`. `Some` is a walk to start now; anything
    /// else waits for the running one to finish, or needs no walk.
    pub fn ask(&mut self, ask: Ask) -> Option<Start> {
        match ask {
            Ask::Run(request) => match &self.running {
                Some((_, cancel)) => {
                    cancel.store(true, Ordering::Relaxed);
                    // Only the newest: the one this replaces was never
                    // started, and nobody is waiting for its answer.
                    self.next = Some(request);
                    None
                }
                None => Some(self.start(request)),
            },
            Ask::Stop => {
                if let Some((_, cancel)) = &self.running {
                    cancel.store(true, Ordering::Relaxed);
                }
                self.next = None;
                None
            }
            // Saved searches are the window's list, not a walk.
            Ask::Smart(_) => None,
        }
    }

    /// The walk numbered `run` has ended. Starts the one that was waiting
    /// for it, if any.
    pub fn finished(&mut self, run: u64) -> Option<Start> {
        if self.running.as_ref().is_some_and(|(id, _)| *id == run) {
            self.running = None;
        }
        if self.running.is_some() {
            return None;
        }
        self.next.take().map(|request| self.start(request))
    }

    /// Whether a walk is going — for tests, and for a host deciding
    /// whether it has anything to cancel.
    pub fn busy(&self) -> bool {
        self.running.is_some()
    }

    fn start(&mut self, request: Request) -> Start {
        let cancel = Arc::new(AtomicBool::new(false));
        self.running = Some((request.run, cancel.clone()));
        (request, cancel)
    }
}

/// A closed tab's walk stops with it: nobody will read what it finds,
/// and it would otherwise run to its time limit on a machine the user is
/// doing something else with.
impl Drop for Searcher {
    fn drop(&mut self) {
        if let Some((_, cancel)) = &self.running {
            cancel.store(true, Ordering::Relaxed);
        }
    }
}

/// Whether `dir` is on another filesystem than `root` — measured by the
/// device a `stat` reports, the way `find -xdev` does. A path the kernel
/// cannot `stat` (a member inside an archive, which only the archive
/// backend can list) is never "elsewhere": the walk is already wherever
/// the backend says it is.
pub fn elsewhere(root: &Path) -> impl Fn(&Path) -> bool + Send + 'static {
    let device = std::fs::metadata(root).map(|m| m.dev()).ok();
    move |dir: &Path| match device {
        Some(device) => std::fs::symlink_metadata(dir).is_ok_and(|m| m.dev() != device),
        None => false,
    }
}

/// How a walk from `root` reads a file for `content:` — see
/// [`hyprforge_files_core::content`]. A root that is a folder on disk
/// reads files on disk; any other root is inside an archive (the same
/// test [`elsewhere`] makes), where no member is a file to open, and
/// every one is counted as such rather than reported unreadable.
pub fn opener(root: &Path) -> fn(&Path) -> Result<Opened, Refused> {
    if std::fs::metadata(root).is_ok_and(|m| m.is_dir()) {
        content::open_regular
    } else {
        content::in_archive
    }
}

/// Runs one search on a blocking worker and streams what it finds — see
/// the module doc for how it ends.
pub fn stream(
    backend: Arc<dyn FsBackend>,
    request: Request,
    cancel: Arc<AtomicBool>,
) -> impl iced::futures::Stream<Item = Event> {
    let run = request.run;
    let allowed = request.budget.time;
    let deadline = tokio::time::Instant::now() + allowed + GRACE;
    iced::stream::channel(4, async move |mut out| {
        use iced::futures::SinkExt;
        let (found, mut batches) = tokio::sync::mpsc::unbounded_channel::<Vec<Entry>>();
        let fence = elsewhere(&request.root);
        let open = opener(&request.root);
        let worker_cancel = cancel.clone();
        let worker = tokio::task::spawn_blocking(move || {
            let tags_of = |path: &std::path::Path| crate::tag_io::file_tags(path).unwrap_or_default();
            walk(backend.as_ref(), &request, &worker_cancel, &fence, &open, &tags_of, &mut |batch| found.send(batch).is_ok())
        });
        let mut count = 0;
        loop {
            match tokio::time::timeout_at(deadline, batches.recv()).await {
                Ok(Some(batch)) => {
                    count += batch.len();
                    // A closed channel means the window moved on.
                    if out.send(Event::Found(run, batch)).await.is_err() {
                        cancel.store(true, Ordering::Relaxed);
                        return;
                    }
                }
                // The walk dropped its sender: it has ended, and its
                // summary is ready.
                Ok(None) => {
                    let summary = worker.await.unwrap_or_else(|_| Summary { found: count, ..Summary::stopped() });
                    let _ = out.send(Event::Finished(run, summary)).await;
                    return;
                }
                Err(_) => {
                    // Stuck in a read that will not return. Told to stop,
                    // and not waited for.
                    cancel.store(true, Ordering::Relaxed);
                    // "Stopped after" the time it was allowed, which is
                    // what the status line's reader means by it.
                    let summary = Summary { end: End::TimeLimit, found: count, elapsed: allowed, ..Summary::stopped() };
                    let _ = out.send(Event::Finished(run, summary)).await;
                    return;
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use hyprforge_files_core::backend::mock::MockBackend;
    use hyprforge_files_core::search::Budget;
    use iced::futures::StreamExt;
    use std::path::PathBuf;

    fn request(run: u64) -> Request {
        Request {
            run,
            root: PathBuf::from("/r"),
            matcher: hyprforge_files_core::query::parse("ext:rs").matcher_now(),
            show_hidden: false,
            skip: Vec::new(),
            budget: Budget::default(),
        }
    }

    /// The property the browser leans on: a burst of requests is the one
    /// running and the newest, never one walk per keystroke.
    #[test]
    fn a_burst_of_requests_runs_the_first_and_the_last() {
        let mut searcher = Searcher::default();
        let (first, cancel) = searcher.ask(Ask::Run(request(1))).expect("nothing running: starts");
        assert_eq!(first.run, 1);
        assert!(searcher.ask(Ask::Run(request(2))).is_none());
        assert!(searcher.ask(Ask::Run(request(3))).is_none());
        assert!(cancel.load(Ordering::Relaxed), "the running walk is told to stop");
        let (next, _) = searcher.finished(1).expect("the newest starts when it ends");
        assert_eq!(next.run, 3, "two was replaced before it began");
        assert!(searcher.finished(3).is_none());
        assert!(!searcher.busy());
    }

    #[test]
    fn stop_cancels_and_forgets_what_was_waiting() {
        let mut searcher = Searcher::default();
        let (_, cancel) = searcher.ask(Ask::Run(request(1))).unwrap();
        searcher.ask(Ask::Run(request(2)));
        searcher.ask(Ask::Stop);
        assert!(cancel.load(Ordering::Relaxed));
        assert!(searcher.finished(1).is_none(), "nothing starts after a stop");
    }

    /// A late report for a walk already replaced must not clear the one
    /// that replaced it.
    #[test]
    fn a_report_for_an_old_walk_leaves_the_running_one_alone() {
        let mut searcher = Searcher::default();
        searcher.ask(Ask::Run(request(1)));
        searcher.finished(1);
        searcher.ask(Ask::Run(request(2)));
        assert!(searcher.finished(1).is_none());
        assert!(searcher.busy());
    }

    #[test]
    fn a_path_that_cannot_be_stated_is_never_elsewhere() {
        let fence = elsewhere(Path::new("/"));
        assert!(!fence(Path::new("/no/such/place/at/all")));
        let unknown_root = elsewhere(Path::new("/no/such/root.zip/inner"));
        assert!(!unknown_root(Path::new("/proc")), "an archive's walk is never fenced");
    }

    #[test]
    fn proc_is_another_filesystem_from_the_root() {
        // `/proc` is its own filesystem on every Linux system this runs
        // on; a fence that cannot see that cannot see a network mount.
        assert!(elsewhere(Path::new("/"))(Path::new("/proc")));
    }

    /// A walk from a real folder reads real files; one from inside an
    /// archive opens nothing, and says so.
    #[test]
    fn contents_are_read_on_disk_and_never_inside_an_archive() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.txt");
        std::fs::write(&file, "x").unwrap();
        assert!(opener(dir.path())(&file).is_ok());
        let inside = dir.path().join("a.zip").join("inner");
        assert_eq!(opener(&inside)(&file).err(), Some(Refused::InArchive));
    }

    #[tokio::test]
    async fn the_stream_sends_every_result_then_how_it_ended() {
        let backend = MockBackend::new();
        let r = Path::new("/r");
        backend.seed("/r", vec![MockBackend::file(r, "a.rs", 1), MockBackend::file(r, "b.txt", 1)]);
        let events: Vec<Event> =
            stream(Arc::new(backend), request(7), Arc::new(AtomicBool::new(false))).collect().await;
        let Some(Event::Finished(7, summary)) = events.last() else { panic!("{events:?}") };
        assert_eq!(summary.end, End::Complete);
        assert_eq!(summary.found, 1);
        assert!(matches!(&events[0], Event::Found(7, batch) if batch.len() == 1));
    }
}
