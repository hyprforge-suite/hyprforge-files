//! The host's half of Quick Look: reading what the card shows, at the
//! card's size, without decoding every picture a held arrow key passes.
//!
//! What is read is the preview pane's [`crate::preview::build`], asked
//! for a larger picture — see `hyprforge_files_core`'s
//! `browser/quicklook.rs` for why it is the same pipeline with a bigger
//! edge rather than a second one.
//!
//! # Coalesced, newest only, one at a time
//!
//! The browser asks on every focus move while the card is up, and a held
//! arrow key moves the focus thirty times a second. Building each one
//! would be the CLAUDE.md rule about a loop that does work per signal:
//! a queue of full-size decodes, each one's peak the whole photograph
//! (`hyprforge_image`'s measured 214MB for a 36-megapixel JPEG), every
//! one of them for a picture already scrolled past by the time it
//! finished. So a request is treated as "look", not as work:
//!
//! - a request that follows another within [`SETTLE`] waits that long
//!   first, and gives up if a newer one arrived meanwhile — a held key
//!   builds nothing until it is let go, a single press builds at once;
//! - at most one build runs at a time, so the peak is one picture
//!   however fast the keys come;
//! - a request still waiting for its turn when a newer one arrives gives
//!   up then too.
//!
//! A request that gives up answers nothing at all, rather than "nothing
//! to show": `None` from a build is a real answer the card would draw,
//! and it must not land on an entry that a later request is about to
//! answer properly. Whatever does arrive still passes the browser's own
//! latest-wins check, which is what makes an answer for an entry the
//! arrows have left harmless.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use hyprforge_files_core::preview::Preview;
use hyprforge_files_core::FsBackend;

/// How long a request waits for the arrows to settle, when it follows
/// another closely enough to look like a held key. Longer than a key's
/// repeat interval (25 to 40ms on most desktops), short enough that
/// letting go and seeing the picture read as one gesture.
pub const SETTLE: Duration = Duration::from_millis(120);

/// One window's Quick Look requests. Cloned into every task it starts;
/// the clones share one counter and one turn.
#[derive(Debug, Clone)]
pub struct Coalescer {
    /// The newest request's number.
    latest: Arc<AtomicU64>,
    /// When the newest request arrived — whether the next one is a held
    /// key's or a fresh press.
    last: Arc<Mutex<Option<Instant>>>,
    /// Held by the build in progress.
    turn: Arc<tokio::sync::Mutex<()>>,
}

impl Default for Coalescer {
    fn default() -> Self {
        Coalescer {
            latest: Arc::new(AtomicU64::new(0)),
            last: Arc::new(Mutex::new(None)),
            turn: Arc::new(tokio::sync::Mutex::new(())),
        }
    }
}

/// A request's place in line, taken the moment the host is asked — see
/// [`Coalescer::ticket`].
#[derive(Debug, Clone, Copy)]
pub struct Ticket {
    number: u64,
    /// It followed another closely enough to be a held key's.
    held: bool,
}

impl Coalescer {
    /// Numbers a request, now.
    ///
    /// Separate from [`Coalescer::run`] because the order has to be the
    /// order the browser asked in, and anything asynchronous between the
    /// asking and the numbering can reorder it. Found live: numbered
    /// inside the task, after the window answered with its scale, a
    /// request for an entry already passed could resolve last, take the
    /// newest number, and leave the entry actually on show reading
    /// forever — its own request overtaken by an older one, and the older
    /// one's answer dropped by the browser as stale.
    pub fn ticket(&self) -> Ticket {
        let number = self.latest.fetch_add(1, Ordering::SeqCst) + 1;
        let now = Instant::now();
        // A poisoned lock means a panic elsewhere while holding a
        // timestamp; the timestamp is still fine to read.
        let mut last = self.last.lock().unwrap_or_else(|e| e.into_inner());
        let held = last.is_some_and(|at| now.duration_since(at) < SETTLE);
        *last = Some(now);
        Ticket { number, held }
    }

    /// [`Coalescer::run`] on a ticket taken now.
    pub fn latest<T: Send + 'static>(
        &self,
        work: impl FnOnce() -> T + Send + 'static,
    ) -> impl std::future::Future<Output = Option<T>> + Send + 'static {
        self.run(self.ticket(), work)
    }

    /// Runs `work` on a blocking worker unless a newer ticket is taken
    /// first — `None` when it was overtaken, and when the worker itself
    /// failed.
    pub fn run<T: Send + 'static>(
        &self,
        ticket: Ticket,
        work: impl FnOnce() -> T + Send + 'static,
    ) -> impl std::future::Future<Output = Option<T>> + Send + 'static {
        let latest = self.latest.clone();
        let turn = self.turn.clone();
        async move {
            let current = || latest.load(Ordering::SeqCst) == ticket.number;
            if ticket.held {
                tokio::time::sleep(SETTLE).await;
                if !current() {
                    return None;
                }
            }
            let _turn = turn.lock().await;
            if !current() {
                return None;
            }
            tokio::task::spawn_blocking(work).await.ok()
        }
    }
}

/// What the card shows for `path`, its picture fitted to `edge` physical
/// pixels — `None` when a newer request overtook this one, which the
/// host turns into no message at all.
pub fn build(
    coalescer: &Coalescer,
    ticket: Ticket,
    path: PathBuf,
    mime: Arc<hyprforge_mime::MimeDb>,
    backend: Arc<dyn FsBackend>,
    edge: u32,
) -> impl std::future::Future<Output = Option<(PathBuf, Option<Preview>)>> + Send + 'static {
    coalescer.run(ticket, move || {
        let found = crate::preview::build(&path, &mime, backend.as_ref(), edge);
        (path, found)
    })
}

/// Carries out `Outcome::LoadQuickLook` for a window `window` logical
/// pixels big: asks the window for its output's scale, decodes for the
/// card's picture box at that scale, and sends the answer as `done` —
/// or sends nothing, when a newer request overtook this one.
///
/// Both hosts call this, the Files window and the open/save dialog, so
/// the dialog's card is bounded exactly as the window's is.
pub fn task<M: Send + 'static>(
    coalescer: &Coalescer,
    path: PathBuf,
    mime: Arc<hyprforge_mime::MimeDb>,
    backend: Arc<dyn FsBackend>,
    window: (f32, f32),
    scale: hyprforge_ui::theme::FontScale,
    done: impl Fn(PathBuf, Option<Preview>) -> M + Clone + Send + 'static,
) -> iced::Task<M> {
    // Numbered here, while the outcomes are still in the order the
    // browser asked — see `Coalescer::ticket`.
    let ticket = coalescer.ticket();
    let coalescer = coalescer.clone();
    iced::window::latest()
        .and_then(iced::window::scale_factor)
        .then(move |factor| {
            let edge = hyprforge_files_core::browser::quick_look_edge(window, scale, factor);
            iced::Task::future(build(&coalescer, ticket, path.clone(), mime.clone(), backend.clone(), edge))
        })
        .and_then(move |(path, found)| iced::Task::done(done(path, found)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    /// A key held down: ten requests a key-repeat apart build exactly one
    /// thing — the last — and every other request answers nothing.
    #[tokio::test]
    async fn a_held_arrow_builds_only_where_it_stops() {
        let coalescer = Coalescer::default();
        let built = Arc::new(AtomicUsize::new(0));
        let mut waiting = Vec::new();
        for i in 0..10 {
            let built = built.clone();
            waiting.push(tokio::spawn(coalescer.latest(move || {
                built.fetch_add(1, Ordering::SeqCst);
                std::thread::sleep(Duration::from_millis(50));
                i
            })));
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
        let mut answers = Vec::new();
        for task in waiting {
            answers.push(task.await.unwrap());
        }
        assert_eq!(answers.last(), Some(&Some(9)), "where the key stopped is built");
        // The first press is a fresh one and starts at once; everything
        // the held key passed after it is skipped.
        assert_eq!(answers.iter().flatten().copied().collect::<Vec<_>>(), [0, 9], "{answers:?}");
        assert_eq!(built.load(Ordering::SeqCst), 2);
    }

    /// The newest *ticket* wins, whatever order the work reaches the
    /// coalescer in — the live bug, where an older request that resolved
    /// late overtook the entry on show.
    #[tokio::test]
    async fn the_newest_ticket_wins_even_when_its_work_arrives_first() {
        let coalescer = Coalescer::default();
        let older = coalescer.ticket();
        let newer = coalescer.ticket();
        let newer_answer = tokio::spawn(coalescer.run(newer, || "newer"));
        tokio::time::sleep(Duration::from_millis(10)).await;
        let older_answer = coalescer.run(older, || "older").await;
        assert_eq!(older_answer, None, "the older request was overtaken");
        assert_eq!(newer_answer.await.unwrap(), Some("newer"));
    }

    /// One press, alone, does not wait for a key nobody is holding.
    #[tokio::test]
    async fn a_single_press_builds_at_once() {
        let coalescer = Coalescer::default();
        let started = Instant::now();
        assert_eq!(coalescer.latest(|| 7).await, Some(7));
        assert!(started.elapsed() < SETTLE, "{:?}", started.elapsed());
    }

    /// Never two builds at once, however the requests arrive: the peak is
    /// one picture.
    #[tokio::test]
    async fn builds_never_overlap() {
        let coalescer = Coalescer::default();
        let running = Arc::new(AtomicUsize::new(0));
        let most = Arc::new(AtomicUsize::new(0));
        let mut waiting = Vec::new();
        for _ in 0..3 {
            let (running, most) = (running.clone(), most.clone());
            waiting.push(tokio::spawn(coalescer.latest(move || {
                let now = running.fetch_add(1, Ordering::SeqCst) + 1;
                most.fetch_max(now, Ordering::SeqCst);
                // Longer than the gap between requests, so each new one
                // arrives while the last is still building.
                std::thread::sleep(SETTLE * 3);
                running.fetch_sub(1, Ordering::SeqCst);
            })));
            // Far enough apart that none counts as a held key.
            tokio::time::sleep(SETTLE + Duration::from_millis(20)).await;
        }
        let mut built = 0;
        for task in waiting {
            built += usize::from(task.await.unwrap().is_some());
        }
        assert_eq!(most.load(Ordering::SeqCst), 1, "two builds ran at once");
        // The middle one was still waiting its turn when the last arrived.
        assert_eq!(built, 2);
    }
}
