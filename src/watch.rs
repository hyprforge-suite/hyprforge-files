//! Live folder updates: a folder on screen re-reads itself when
//! something other than this window changes it.
//!
//! The window runs [`watch`] as a subscription keyed on [`Watched`] — the
//! distinct folders its tabs are showing — so iced starts a new watch
//! whenever a tab navigates, opens or closes, and drops the old one (and
//! with it the inotify descriptor) in the same step. Nothing here keeps a
//! list of tabs; the key *is* the list.
//!
//! # Every event is "look again"
//!
//! Unpacking a tarball into a folder on screen is thousands of `CREATE`s
//! and `CLOSE_WRITE`s in a second. Re-reading per event would be the
//! video player's mistake that CLAUDE.md records — work per signal,
//! falling behind a sender that signals faster than the work — so an
//! event only marks its folder dirty. The first event of a burst starts a
//! wait; the burst is drained as it arrives, until it has been quiet for
//! [`QUIET`] or [`CEILING`] has passed since it began, and then each
//! dirty folder is sent once. A copy that never stops still shows up
//! every [`CEILING`], rather than never.
//!
//! # What is not watched by the kernel
//!
//! - **A folder inside an archive.** It is not a folder on disk; the
//!   archive's own rewrite already re-reads what it changed. Skipped.
//! - **A folder on a network share.** inotify reports changes made
//!   *through this machine's* kernel, and the whole point of a share is
//!   that someone else is changing it — a watch there would answer every
//!   question with silence, which reads as "nothing changed". So a share
//!   is polled every [`Watched::poll_every`] instead, while it is shown,
//!   and not otherwise. A share is recognised two ways: one the sidebar
//!   knows about (`Devices::share_holding`, handed in by the window as
//!   [`Watched::shares`]), and anything whose `statfs` says NFS, SMB,
//!   9p, Ceph, AFS or FUSE — the last because gvfs, sshfs and rclone are
//!   all FUSE, and a local FUSE filesystem polled is merely a little
//!   wasteful where a remote one watched is wrong.
//! - **A folder the kernel will not watch.** `add_watch` fails with
//!   `ENOSPC` when `max_user_watches` is spent — an IDE indexing a large
//!   tree can do that. That is logged once per process, and the folder is
//!   polled; it never fails the folder, because the listing itself is
//!   fine. A folder that is simply gone (`ENOENT`) is not polled — its
//!   tab already says it does not exist.
//!
//! # Nothing here blocks a thread anyone waits on
//!
//! The events are read through tokio's reactor (inotify's `stream`
//! feature), never by a blocking `read`. Setting up does block — a
//! `statfs` or an `add_watch` on a hard-mounted NFS share whose server
//! has gone away waits for it to come back — so it runs on the blocking
//! pool under [`SET_UP_WITHIN`], and a set-up that does not finish in
//! time leaves every folder polled rather than leaving the window
//! without updates.

use iced::futures::{SinkExt, Stream, StreamExt};
use inotify::{EventMask, Inotify, WatchDescriptor, WatchMask};
use std::collections::{BTreeSet, HashMap};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// How long a burst must go quiet before its folders are re-read. A
/// quarter of a second: long enough that a `cp -r` reads as one change,
/// short enough that a saved file still looks instant.
pub const QUIET: Duration = Duration::from_millis(250);

/// The longest a burst may hold back a re-read, however long it goes on.
pub const CEILING: Duration = Duration::from_secs(1);

/// The bound on setting a watch up — see the module doc.
pub const SET_UP_WITHIN: Duration = Duration::from_secs(2);

/// What a change to a folder's listing looks like. Not `MODIFY`: a file
/// being written fires one per `write`, and its size in the listing can
/// wait for `CLOSE_WRITE`. `DELETE_SELF` and `MOVE_SELF` so a folder that
/// is removed or renamed while shown is re-read, and says so.
fn mask() -> WatchMask {
    WatchMask::CREATE
        | WatchMask::DELETE
        | WatchMask::MOVED_FROM
        | WatchMask::MOVED_TO
        | WatchMask::CLOSE_WRITE
        | WatchMask::ATTRIB
        | WatchMask::DELETE_SELF
        | WatchMask::MOVE_SELF
}

/// What the window wants watched: the subscription's key.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Watched {
    /// Every distinct folder a tab is showing, archives and shares
    /// included — sorting them out is this module's job.
    pub dirs: BTreeSet<PathBuf>,
    /// The ones the window already knows are on a network share. Polled
    /// without asking the kernel anything about them.
    pub shares: BTreeSet<PathBuf>,
    /// How often a polled folder is looked at again.
    pub poll_every: Duration,
}

/// How one folder is kept up to date.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum How {
    /// An inotify watch.
    Kernel,
    /// Re-read every [`Watched::poll_every`].
    Poll,
    /// Not at all — an archive, or a folder that is gone.
    Skip,
}

/// How `dir` should be kept up to date, before any watch is tried.
/// Blocking: it `stat`s and `statfs`es.
pub fn classify(dir: &Path, shares: &BTreeSet<PathBuf>) -> How {
    if shares.contains(dir) {
        return How::Poll;
    }
    // `split` stats only a component whose *name* could be an archive,
    // so for an ordinary folder this costs nothing.
    if hyprforge_files_core::archive::split(dir).is_some() {
        return How::Skip;
    }
    if is_remote(dir) {
        return How::Poll;
    }
    How::Kernel
}

/// What a failed `add_watch` turns into. A folder that is not there (or
/// is not a folder) is skipped — its tab already says so, and polling it
/// would re-read an error every few seconds. Anything else is the kernel
/// declining to watch a folder that is fine, and the folder is polled.
pub fn after_refusal(error: &io::Error) -> How {
    match error.raw_os_error() {
        // `EACCES` too: a folder this user cannot read cannot be listed
        // either, and its tab already says that.
        Some(libc::ENOENT) | Some(libc::ENOTDIR) | Some(libc::EACCES) => How::Skip,
        _ => How::Poll,
    }
}

/// `statfs`'s answer for filesystems a kernel watch cannot see changes
/// on. The numbers are `linux/magic.h`'s, checked against the copy
/// installed on the machine this was written on.
pub fn is_remote_magic(f_type: i64) -> bool {
    const NFS: i64 = 0x6969;
    const SMB: i64 = 0x517B;
    const CIFS: i64 = 0xFF53_4D42;
    const SMB2: i64 = 0xFE53_4D42;
    const FUSE: i64 = 0x6573_5546;
    const V9FS: i64 = 0x0102_1997;
    const CEPH: i64 = 0x00c3_6400;
    const AFS: i64 = 0x5346_414F;
    // `f_type` is signed on some ABIs, and the SMB magics have the top
    // bit set — compared as the 32 bits the kernel actually writes.
    let f_type = f_type & 0xFFFF_FFFF;
    matches!(f_type, NFS | SMB | CIFS | SMB2 | FUSE | V9FS | CEPH | AFS)
}

/// Whether `dir` is on a network (or FUSE) filesystem. `false` when it
/// cannot be asked — a folder that will not `statfs` will not take a
/// watch either, and [`after_refusal`] decides what happens then.
pub fn is_remote(dir: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let Ok(c_path) = std::ffi::CString::new(dir.as_os_str().as_bytes()) else { return false };
    let mut buf = std::mem::MaybeUninit::<libc::statfs>::zeroed();
    // SAFETY: `c_path` is a NUL-terminated string that outlives the call,
    // and `buf` is a properly sized, writable `statfs` the kernel fills
    // in; it is read only when the call says it succeeded.
    let ok = unsafe { libc::statfs(c_path.as_ptr(), buf.as_mut_ptr()) } == 0;
    if !ok {
        return false;
    }
    // SAFETY: `statfs` returned 0, so it wrote the whole struct.
    let stat = unsafe { buf.assume_init() };
    #[allow(clippy::unnecessary_cast)] // `f_type`'s type differs by ABI.
    is_remote_magic(stat.f_type as i64)
}

/// Said once per process, however many times the window navigates: a
/// spent watch limit stays spent, and a log line per folder change would
/// only bury the first one.
static REFUSED_LOGGED: AtomicBool = AtomicBool::new(false);

fn refused_once(why: &str, error: &dyn std::fmt::Display) {
    if !REFUSED_LOGGED.swap(true, Ordering::Relaxed) {
        tracing::warn!("{why}: {error} — such folders are looked at again every few seconds instead of watched");
    }
}

/// What setting up came to: the kernel's watch, what each of its
/// descriptors is, and what is left to poll.
struct SetUp {
    inotify: Option<Inotify>,
    watching: HashMap<WatchDescriptor, PathBuf>,
    polled: Vec<PathBuf>,
}

/// Classifies every folder and adds a kernel watch for each that takes
/// one. Blocking — see the module doc.
fn set_up(watched: &Watched) -> SetUp {
    let mut polled = Vec::new();
    let mut kernel = Vec::new();
    for dir in &watched.dirs {
        match classify(dir, &watched.shares) {
            How::Kernel => kernel.push(dir.clone()),
            How::Poll => polled.push(dir.clone()),
            How::Skip => {}
        }
    }
    let mut watching = HashMap::new();
    if kernel.is_empty() {
        return SetUp { inotify: None, watching, polled };
    }
    let inotify = match Inotify::init() {
        Ok(inotify) => inotify,
        // `EMFILE`: `max_user_instances` is spent. The same fallback as a
        // spent watch limit, for the same reason.
        Err(e) => {
            refused_once("couldn't start watching folders", &e);
            polled.extend(kernel);
            return SetUp { inotify: None, watching, polled };
        }
    };
    let mut watches = inotify.watches();
    for dir in kernel {
        match watches.add(&dir, mask()) {
            Ok(wd) => {
                watching.insert(wd, dir);
            }
            Err(e) => match after_refusal(&e) {
                How::Skip => {}
                _ => {
                    let why = if e.raw_os_error() == Some(libc::ENOSPC) {
                        "the inotify watch limit is spent (fs.inotify.max_user_watches)"
                    } else {
                        "a folder couldn't be watched"
                    };
                    refused_once(why, &e);
                    polled.push(dir);
                }
            },
        }
    }
    SetUp { inotify: Some(inotify), watching, polled }
}

/// Waits for the next item, then drains the burst it starts: until
/// nothing has arrived for `quiet`, or `ceiling` has passed since the
/// first. Everything that arrived, in order; `None` when the stream has
/// ended with nothing pending.
pub async fn burst<S, T>(stream: &mut S, quiet: Duration, ceiling: Duration) -> Option<Vec<T>>
where
    S: Stream<Item = T> + Unpin,
{
    let first = stream.next().await?;
    let mut items = vec![first];
    let started = tokio::time::Instant::now();
    loop {
        let left = ceiling.saturating_sub(started.elapsed());
        if left.is_zero() {
            break;
        }
        match tokio::time::timeout(quiet.min(left), stream.next()).await {
            Ok(Some(item)) => items.push(item),
            // Quiet for long enough, or the stream ended mid-burst —
            // either way what arrived is worth one look.
            Err(_) | Ok(None) => break,
        }
    }
    Some(items)
}

/// The folders a burst of events touched. A queue overflow means events
/// were lost, so every watched folder might have changed.
fn dirty(events: &[io::Result<inotify::EventOwned>], watching: &HashMap<WatchDescriptor, PathBuf>) -> BTreeSet<PathBuf> {
    let mut dirty = BTreeSet::new();
    for event in events.iter().flatten() {
        if event.mask.contains(EventMask::Q_OVERFLOW) {
            dirty.extend(watching.values().cloned());
        } else if let Some(dir) = watching.get(&event.wd) {
            dirty.insert(dir.clone());
        }
    }
    dirty
}

/// The subscription: one folder path per re-read the window should do.
pub fn watch(watched: &Watched) -> impl Stream<Item = PathBuf> + use<> {
    let watched = watched.clone();
    iced::stream::channel(8, async move |mut out: iced::futures::channel::mpsc::Sender<PathBuf>| {
        let setting_up = {
            let watched = watched.clone();
            tokio::task::spawn_blocking(move || set_up(&watched))
        };
        let SetUp { inotify, watching, mut polled } =
            match tokio::time::timeout(SET_UP_WITHIN, setting_up).await {
                Ok(Ok(set_up)) => set_up,
                // Too slow (a share that has stopped answering), or the
                // set-up panicked: every folder is polled. The blocking
                // thread is left to finish on its own; its watch, if it
                // ever makes one, is dropped with it.
                Ok(Err(_)) | Err(_) => {
                    refused_once("setting up folder watches took too long", &"a filesystem isn't answering");
                    SetUp { inotify: None, watching: HashMap::new(), polled: watched.dirs.iter().cloned().collect() }
                }
            };
        let mut events = inotify.and_then(|inotify| {
            // 4KiB holds a few dozen events with names; a burst bigger
            // than that is simply read in several goes.
            inotify.into_event_stream(vec![0u8; 4096]).map_err(|e| refused_once("couldn't read folder watches", &e)).ok()
        });
        if events.is_none() {
            // Whatever the kernel was going to watch is now polled.
            polled.extend(watching.values().cloned());
        }
        polled.sort();
        polled.dedup();
        let mut tick = tokio::time::interval_at(tokio::time::Instant::now() + watched.poll_every, watched.poll_every);
        // A slow re-read must not be followed by a catch-up burst of
        // ticks: the next look is a whole interval after this one.
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                got = async {
                    match events.as_mut() {
                        Some(stream) => burst(stream, QUIET, CEILING).await,
                        None => std::future::pending().await,
                    }
                } => {
                    let Some(got) = got else {
                        // The descriptor closed under us — nothing more
                        // will arrive, so fall back to looking.
                        events = None;
                        polled.extend(watching.values().cloned());
                        continue;
                    };
                    for dir in dirty(&got, &watching) {
                        if out.send(dir).await.is_err() {
                            return;
                        }
                    }
                }
                _ = tick.tick(), if !polled.is_empty() => {
                    for dir in &polled {
                        if out.send(dir.clone()).await.is_err() {
                            return;
                        }
                    }
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced::futures::channel::mpsc;

    fn watched(dirs: &[&Path], shares: &[&Path], poll_every: Duration) -> Watched {
        Watched {
            dirs: dirs.iter().map(|d| d.to_path_buf()).collect(),
            shares: shares.iter().map(|d| d.to_path_buf()).collect(),
            poll_every,
        }
    }

    /// Everything the stream sends within `window`.
    async fn collect_for(stream: &mut (impl Stream<Item = PathBuf> + Unpin), window: Duration) -> Vec<PathBuf> {
        let mut got = Vec::new();
        let until = tokio::time::Instant::now() + window;
        while let Ok(Some(path)) = tokio::time::timeout_at(until, stream.next()).await {
            got.push(path);
        }
        got
    }

    #[tokio::test(start_paused = true)]
    async fn a_burst_is_drained_into_one_look_after_it_goes_quiet() {
        let (mut tx, mut rx) = mpsc::channel::<u32>(4);
        let sender = tokio::spawn(async move {
            for i in 0..40 {
                let _ = tx.send(i).await;
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            // Held open, so the end of the burst is quiet, not closed.
            tokio::time::sleep(Duration::from_secs(60)).await;
        });
        let started = tokio::time::Instant::now();
        let got = burst(&mut rx, QUIET, Duration::from_secs(5)).await.unwrap();
        assert_eq!(got.len(), 40, "every event was drained, none left queued");
        let waited = started.elapsed();
        assert!(waited >= Duration::from_millis(390) + QUIET - Duration::from_millis(20), "{waited:?}");
        assert!(waited < Duration::from_millis(390) + QUIET + Duration::from_millis(50), "{waited:?}");
        sender.abort();
    }

    /// A copy that never stops must still show up, not wait for a quiet
    /// that never comes.
    #[tokio::test(start_paused = true)]
    async fn a_burst_that_never_goes_quiet_is_looked_at_by_the_ceiling() {
        let (mut tx, mut rx) = mpsc::channel::<()>(4);
        let sender = tokio::spawn(async move {
            loop {
                let _ = tx.send(()).await;
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        });
        let started = tokio::time::Instant::now();
        burst(&mut rx, QUIET, CEILING).await.unwrap();
        assert!(started.elapsed() <= CEILING + Duration::from_millis(60), "{:?}", started.elapsed());
        sender.abort();
    }

    /// The property the debounce exists for, against the real kernel:
    /// two hundred files landing in a folder on screen is one re-read of
    /// it, not two hundred — and it arrives promptly.
    #[tokio::test]
    async fn a_burst_of_writes_produces_one_re_read() {
        let dir = tempfile::tempdir().unwrap();
        let mut stream = Box::pin(watch(&watched(&[dir.path()], &[], Duration::from_secs(3600))));
        // Let the set-up finish before anything is written, or the
        // first files would land before the watch exists.
        assert!(collect_for(&mut stream, Duration::from_millis(300)).await.is_empty(), "nothing yet");

        let started = std::time::Instant::now();
        for i in 0..200 {
            std::fs::write(dir.path().join(format!("f{i}")), b"x").unwrap();
        }
        let first = tokio::time::timeout(Duration::from_secs(5), stream.next()).await.unwrap().unwrap();
        let latency = started.elapsed();
        assert_eq!(first, dir.path());
        let rest = collect_for(&mut stream, QUIET * 3).await;
        assert!(rest.is_empty(), "one re-read for the whole burst, got {} more", rest.len());
        // From the first write to the re-read: the writes themselves,
        // then one quiet period. Printed so a run can be read for it.
        eprintln!("burst of 200 writes to re-read: {latency:?}");
        assert!(latency < QUIET + Duration::from_millis(750), "{latency:?}");
    }

    #[test]
    fn an_archive_is_never_watched() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("photos.zip");
        std::fs::write(&archive, b"PK").unwrap();
        let none = BTreeSet::new();
        assert_eq!(classify(&archive, &none), How::Skip, "the archive's own root");
        assert_eq!(classify(&archive.join("2024/june"), &none), How::Skip, "a folder inside it");
        // A *folder* named like an archive is a folder.
        let folder = dir.path().join("backup.zip");
        std::fs::create_dir(&folder).unwrap();
        assert_eq!(classify(&folder, &none), How::Kernel);
    }

    /// No kernel watch is asked for a folder the sidebar knows is on a
    /// share: the folder is looked at again on the interval, while it is
    /// shown, whether or not anything changed.
    #[tokio::test]
    async fn a_share_falls_back_to_polling() {
        let share = tempfile::tempdir().unwrap();
        let every = Duration::from_millis(150);
        let mut stream = Box::pin(watch(&watched(&[share.path()], &[share.path()], every)));
        let got = collect_for(&mut stream, every * 4 + every / 2).await;
        assert!((3..=5).contains(&got.len()), "about one look per interval, got {}", got.len());
        assert!(got.iter().all(|p| p == share.path()));
        assert_eq!(classify(share.path(), &[share.path().to_path_buf()].into()), How::Poll);
    }

    #[test]
    fn a_spent_watch_limit_is_polled_and_a_gone_folder_is_not() {
        assert_eq!(after_refusal(&io::Error::from_raw_os_error(libc::ENOSPC)), How::Poll);
        assert_eq!(after_refusal(&io::Error::from_raw_os_error(libc::EMFILE)), How::Poll);
        assert_eq!(after_refusal(&io::Error::from_raw_os_error(libc::ENOENT)), How::Skip);
    }

    /// A folder deleted from under the watch is still in the set the
    /// window asked for; nothing panics and nothing is polled for it.
    #[tokio::test]
    async fn a_gone_folder_is_neither_watched_nor_polled() {
        let dir = tempfile::tempdir().unwrap();
        let gone = dir.path().join("gone");
        let mut stream = Box::pin(watch(&watched(&[&gone], &[], Duration::from_millis(50))));
        assert!(collect_for(&mut stream, Duration::from_millis(300)).await.is_empty());
    }

    #[test]
    fn network_filesystems_are_recognised_by_their_magic() {
        assert!(is_remote_magic(0x6969), "NFS");
        assert!(is_remote_magic(0xFE53_4D42), "SMB2");
        // The same bits arriving sign-extended, as a 32-bit `f_type` does.
        assert!(is_remote_magic(0xFF53_4D42_u32 as i32 as i64), "CIFS");
        assert!(is_remote_magic(0x6573_5546), "FUSE");
        assert!(!is_remote_magic(0x9123_683E), "btrfs");
        assert!(!is_remote_magic(0xEF53), "ext4");
        assert!(!is_remote_magic(0x0102_1994), "tmpfs");
    }

    #[test]
    fn a_local_folder_is_not_remote() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!is_remote(dir.path()));
        assert_eq!(classify(dir.path(), &BTreeSet::new()), How::Kernel);
    }
}
