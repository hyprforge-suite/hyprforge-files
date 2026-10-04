//! Dragging members out of an archive: unpacking them somewhere real
//! while the drag is under way, and handing over their paths only once
//! they are there.
//!
//! A member's path in a listing is `~/x.zip/notes.txt`, which nothing
//! outside this app can open, so a drag out of an archive has to offer
//! real files. Copy already solves the same problem by unpacking first
//! and putting the copies on the clipboard afterwards — but a drag
//! cannot wait like that: it has to start *while the button is still
//! held* (`dnd.rs`), long before a large selection could be unpacked.
//!
//! What makes it possible is the order the protocol does things in. A
//! drag announces only the *types* it offers when it starts; the
//! contents — the `text/uri-list` — are asked for when something accepts
//! the drop, and written into a pipe the receiver reads at its own pace.
//! So the drag starts at once, naming paths that do not exist yet
//! ([`plan`] decides them up front), the unpacking runs beside it
//! ([`unpack`]), and the answer to the receiver's request waits behind a
//! [`Gate`] until the files are on disk. A receiver reading the list
//! afterwards always finds every file it names; a receiver that asks
//! sooner simply waits a little longer for its pipe.
//!
//! **Bounded three ways**, so a drag never turns into a frozen pointer
//! or a disk filling behind somebody's back:
//!
//! - [`BUDGET`] — a selection that unpacks to more than this is
//!   refused, in words, before anything is written. Extract and Copy
//!   are the ways to move that much, and both show progress.
//! - [`DEADLINE`] — unpacking that has not finished by then stops, and
//!   so does the receiver's wait for it.
//! - The drag ending somewhere that did not take it ([`Gate::abandon`])
//!   stops the unpacking and removes what it wrote.
//!
//! **When the copies go.** Not when the receiver says it is finished:
//! `dnd_finished` means it has *read the list*, and a file manager
//! receiving a drop starts a copy job afterwards that can run for
//! minutes. Deleting the source then is deleting files out from under
//! somebody's copy. Nor when this window closes, for the same reason —
//! dropping into another window and closing this one is an ordinary
//! thing to do. So each drag's copies stay in a directory of their own
//! under the user's cache, and [`sweep`] removes old ones: at startup
//! and at the start of every drag out of an archive, anything older than
//! [`KEEP`], and anything past the newest [`KEEP_RECENT`] that is older
//! than [`KEEP_RECENT_FOR`]. A drag that was *not* taken is removed at
//! once, because nobody was handed its paths.
//!
//! On disk rather than in `/tmp`, deliberately: `/tmp` is a RAM-backed
//! tmpfs on most of the machines this suite runs on, and two gigabytes
//! of somebody's archive is not something to hold in memory for a day.

use hyprforge_archive::backend::{Advance, ArchiveBackend, Collision, ExtractRequest, FailureReason, Flow, Progress};
use hyprforge_archive::{ArchiveError, Unlock};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant, SystemTime};

/// The most a drag will unpack: 2 GiB.
///
/// Measured against what it costs the person holding the pointer. A
/// drag has nowhere to show a progress bar — the pointer is busy being
/// the drag — so the work has to be short enough that waiting for the
/// drop to "take" is plausible. Two gigabytes of deflate unpacks in
/// roughly ten seconds on an ordinary disk; past that, Extract and Copy
/// do the same work in the transfers queue, where it can be watched and
/// cancelled.
pub const BUDGET: u64 = 2 * 1024 * 1024 * 1024;

/// How long unpacking for one drag may run, and how long a receiver's
/// request waits for it. Generous against [`BUDGET`], so it only trips
/// on a disk or an archive that has stopped making progress.
pub const DEADLINE: Duration = Duration::from_secs(120);

/// How long a drag's copies are kept for whoever received them.
pub const KEEP: Duration = Duration::from_secs(24 * 60 * 60);

/// How many recent drags keep their copies for the whole of [`KEEP`]…
pub const KEEP_RECENT: usize = 8;

/// …and how long the ones before them are kept: an hour is longer than
/// any copy started from a drop is going to take.
pub const KEEP_RECENT_FOR: Duration = Duration::from_secs(60 * 60);

/// What every drag directory's name starts with, so [`sweep`] can never
/// remove something it did not make even if the root is shared.
const PREFIX: &str = "drag-";

/// Where drags' copies go: `$XDG_CACHE_HOME/hyprforge-files/drags`.
///
/// The cache, because these are copies of something that still exists
/// in the archive and losing them loses nothing.
pub fn root() -> PathBuf {
    let cache = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        // The spec ignores a relative value, and so does this: a
        // relative cache would land somewhere new for every directory
        // the app was started from.
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
        .unwrap_or_else(std::env::temp_dir);
    cache.join("hyprforge-files").join("drags")
}

/// Makes a new, empty directory for one drag under `root`.
///
/// `create_dir`, not `create_dir_all`, for the last step: it fails if
/// the name is taken, which is what makes the directory this drag's
/// alone even with two windows dragging in the same instant.
pub fn make_dir(root: &Path) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(root)?;
    let stamp = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap_or_default().as_nanos();
    for n in 0..100u32 {
        let dir = root.join(format!("{PREFIX}{}-{stamp}-{n}", std::process::id()));
        match std::fs::create_dir(&dir) {
            Ok(()) => return Ok(dir),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::new(std::io::ErrorKind::AlreadyExists, "no free name for a drag's files"))
}

/// Removes old drags' copies from `root` — see the module doc for which
/// — and answers how many went.
///
/// Ages are by each directory's own modification time, which moves
/// whenever unpacking writes into it, so a drag counts from when its
/// files were last put down. A directory whose age cannot be read is
/// left alone: it is not ours to guess about. Never follows a symlink:
/// only real directories named like ours are candidates.
pub fn sweep(root: &Path, now: SystemTime) -> usize {
    let Ok(read) = std::fs::read_dir(root) else {
        return 0; // no drags yet — first run, or nothing to do
    };
    let mut drags: Vec<(PathBuf, Duration)> = read
        .filter_map(Result::ok)
        .filter(|e| e.file_name().to_string_lossy().starts_with(PREFIX))
        .filter_map(|e| {
            let meta = std::fs::symlink_metadata(e.path()).ok()?;
            if !meta.is_dir() {
                return None;
            }
            // A time in the future (a clock that moved back) is age zero:
            // kept, rather than treated as ancient.
            let age = now.duration_since(meta.modified().ok()?).unwrap_or_default();
            Some((e.path(), age))
        })
        .collect();
    drags.sort_by_key(|(_, age)| *age);
    let mut removed = 0;
    for (n, (path, age)) in drags.into_iter().enumerate() {
        if age > KEEP || (n >= KEEP_RECENT && age > KEEP_RECENT_FOR) {
            match std::fs::remove_dir_all(&path) {
                Ok(()) => removed += 1,
                Err(e) => tracing::info!(error = %e, path = %path.display(), "an old drag's files could not be removed"),
            }
        }
    }
    removed
}

/// One extraction a drag needs: members that share a parent inside the
/// archive, unpacked with that parent stripped so each lands at the top
/// of `into` under its own name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Group {
    pub strip: Option<String>,
    pub into: PathBuf,
    pub members: Vec<String>,
}

/// Everything a drag will unpack, and where each dragged member will be.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    pub groups: Vec<Group>,
    /// Where each member lands, in the order they were given — the
    /// paths the drag offers.
    pub lands: Vec<PathBuf>,
}

/// Decides where each member of a drag lands under `dir`, before any of
/// it exists — the drag has to name its paths when it starts.
///
/// Each lands under its own name at the top of `dir`. Two with the same
/// name — possible among a search's results inside an archive, which
/// come from different folders — cannot both be `dir/notes.txt`, so the
/// second goes in `dir/2/`, the third in `dir/3/`: the receiver still
/// gets every file under the name it had, which is the thing a drag is
/// for. Members that share a parent and a destination are unpacked in
/// one call, so a tarball is decompressed once per folder dragged from,
/// not once per file.
pub fn plan(dir: &Path, members: &[String]) -> Plan {
    let mut groups: Vec<Group> = Vec::new();
    let mut lands = Vec::with_capacity(members.len());
    // Names taken so far in each slot: slot 0 is `dir` itself.
    let mut taken: Vec<std::collections::HashSet<String>> = Vec::new();
    for member in members {
        let (parent, name) = match member.rsplit_once('/') {
            Some((parent, name)) => (Some(parent.to_string()), name.to_string()),
            None => (None, member.clone()),
        };
        let slot = (0..)
            .find(|&slot| taken.get(slot).is_none_or(|names: &std::collections::HashSet<String>| !names.contains(&name)))
            .unwrap_or(0);
        if taken.len() <= slot {
            taken.resize_with(slot + 1, Default::default);
        }
        taken[slot].insert(name.clone());
        let into = if slot == 0 { dir.to_path_buf() } else { dir.join((slot + 1).to_string()) };
        lands.push(into.join(&name));
        match groups.iter_mut().find(|g| g.strip == parent && g.into == into) {
            Some(group) => group.members.push(member.clone()),
            None => groups.push(Group { strip: parent, into, members: vec![member.clone()] }),
        }
    }
    Plan { groups, lands }
}

/// Unpacks `plan` out of `archive`, within [`BUDGET`] and `deadline`,
/// and stopping if `gate` is abandoned. `Err` is a sentence for the
/// status bar.
pub fn unpack(
    backend: &dyn ArchiveBackend,
    archive: &Path,
    plan: &Plan,
    unlock: &Unlock,
    gate: &Gate,
    deadline: Instant,
) -> Result<(), String> {
    let mut watch = Watch { gate, budget: BUDGET, deadline, spent: 0, stopped: None };
    for group in &plan.groups {
        let request = ExtractRequest {
            members: group.members.clone(),
            dest: group.into.clone(),
            strip_prefix: group.strip.clone(),
            // The directory was made empty for this drag; nothing can be
            // there to collide with, and if something were it would be
            // this drag's own earlier write.
            collision: Collision::Overwrite,
        };
        let report = match backend.extract_with(archive, &request, unlock, &mut watch) {
            Ok(report) => report,
            Err(ArchiveError::Cancelled) => return Err(watch.why()),
            Err(ArchiveError::PasswordRequired { .. }) => return Err(locked(archive)),
            Err(e) => return Err(format!("They couldn't be unpacked to drag: {e}")),
        };
        watch.spent += report.bytes;
        // A member that would not unpack fails the drag rather than
        // leaving a hole in it: a list of files where one is missing is
        // a drop that half-works, and the receiver would be the one to
        // discover it.
        if let Some(failure) = report.failed.first() {
            if failure.reason == FailureReason::NeedsPassword {
                return Err(locked(archive));
            }
            return Err(format!("{} couldn't be unpacked to drag: {}", failure.member, failure.message));
        }
    }
    Ok(())
}

/// What the status bar says while a drag's members are unpacked — the
/// one place the drag can say anything, since the pointer is busy.
pub fn unpacking_line(items: usize) -> String {
    format!("Unpacking {} to drag\u{2026}", crate::transfers::plural(items, "item", "items"))
}

/// Whether a status line is [`unpacking_line`]'s, so finishing takes
/// down only its own line and never something said since.
pub fn is_unpacking_line(line: &str) -> bool {
    line.starts_with("Unpacking ") && line.ends_with(" to drag\u{2026}")
}

fn locked(archive: &Path) -> String {
    format!(
        "{} is encrypted \u{2014} open a file in it once to unlock it, then drag.",
        archive.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
    )
}

/// Why unpacking stopped early.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stopped {
    TooBig(u64),
    TooSlow,
    Abandoned,
}

/// The progress reporter that is also the three bounds.
struct Watch<'a> {
    gate: &'a Gate,
    budget: u64,
    deadline: Instant,
    /// Bytes written by groups already done.
    spent: u64,
    stopped: Option<Stopped>,
}

impl Watch<'_> {
    fn why(&self) -> String {
        match self.stopped {
            Some(Stopped::TooBig(bytes)) => format!(
                "That's {} to unpack before it can be dropped \u{2014} more than a drag carries. Copy or Extract it instead.",
                human(bytes)
            ),
            Some(Stopped::TooSlow) => "Unpacking for the drag took too long, so it stopped.".to_string(),
            // Nobody to tell: the drag ended without a drop.
            Some(Stopped::Abandoned) | None => String::new(),
        }
    }
}

impl Progress for Watch<'_> {
    fn advance(&mut self, advance: Advance<'_>) -> Flow {
        // Checked on every member, but decided by the first: the total
        // is the extraction's plan, known before anything is written.
        let total = self.spent + advance.bytes_total;
        self.stopped = if total > self.budget {
            Some(Stopped::TooBig(total))
        } else if self.gate.abandoned() {
            Some(Stopped::Abandoned)
        } else if Instant::now() > self.deadline {
            Some(Stopped::TooSlow)
        } else {
            None
        };
        if self.stopped.is_some() {
            Flow::Cancel
        } else {
            Flow::Continue
        }
    }
}

/// "2.4 GiB" — binary units, as the listing's Size column prints them.
fn human(bytes: u64) -> String {
    let b = bytes as f64;
    match bytes {
        0..1_048_576 => format!("{:.0} KiB", b / 1024.0),
        1_048_576..1_073_741_824 => format!("{:.0} MiB", b / 1_048_576.0),
        _ => format!("{:.1} GiB", b / 1_073_741_824.0),
    }
}

/// This window's latest drag out of an archive: where its copies are,
/// and the archive folder it was dragged from.
///
/// Remembered because such a drag comes back to this window, if it is
/// dropped here, as anybody's would — a list of real paths read
/// through the compositor — and those paths are the copies, whose
/// folder is not the one the drag started in. Without this, letting
/// go over the listing it came from (a drag that changed its mind)
/// would add the members back into their own archive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnDrag {
    pub dir: PathBuf,
    pub from: PathBuf,
}

impl OwnDrag {
    /// Whether dropping `paths` into `into` is this drag let go where it
    /// started — which does nothing, as it does for any drag (see
    /// `hyprforge_files_core::drop::plan`), unless Ctrl asked for a
    /// copy beside the original.
    pub fn returned_home(&self, paths: &[PathBuf], into: &Path, ctrl: bool) -> bool {
        !ctrl && into == self.from && !paths.is_empty() && paths.iter().all(|p| p.starts_with(&self.dir))
    }
}

/// Between the unpacking and whoever asks for the drag's files.
///
/// It owns the drag's directory, and it is the one place that decides
/// when that directory goes: on a failure (nothing usable was made), or
/// when the drag ended without a drop and its paths were never handed
/// to anyone. Once they have been, the directory is left for [`sweep`].
pub struct Gate {
    dir: PathBuf,
    state: Mutex<GateState>,
    changed: Condvar,
}

/// What a failure does to the drag itself — cancels it, in `dnd.rs`.
type OnFail = Box<dyn FnOnce() + Send>;

#[derive(Default)]
struct GateState {
    outcome: Option<Result<(), String>>,
    abandoned: bool,
    handed_over: bool,
    on_fail: Option<OnFail>,
}

impl std::fmt::Debug for Gate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Gate").field("dir", &self.dir).finish_non_exhaustive()
    }
}

impl Gate {
    pub fn new(dir: PathBuf) -> Arc<Gate> {
        Arc::new(Gate { dir, state: Mutex::new(GateState::default()), changed: Condvar::new() })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, GateState> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// The unpacking is over, one way or the other. Wakes every waiting
    /// request; on a failure, also cancels the drag (see
    /// [`Self::on_fail`]) and removes what was written.
    pub fn finish(&self, result: Result<(), String>) {
        let (remove, on_fail) = {
            let mut state = self.lock();
            let failed = result.is_err();
            state.outcome = Some(result);
            (failed || state.abandoned, if failed { state.on_fail.take() } else { None })
        };
        self.changed.notify_all();
        if remove {
            self.remove();
        }
        if let Some(cancel) = on_fail {
            cancel();
        }
    }

    /// Waits up to `timeout` for the unpacking, and answers whether the
    /// files are there to hand over. A `true` is a promise that they
    /// were handed to someone, so the directory is kept from then on.
    pub fn wait(&self, timeout: Duration) -> bool {
        let state = self.lock();
        let (mut state, _) = self
            .changed
            .wait_timeout_while(state, timeout, |s| s.outcome.is_none())
            .unwrap_or_else(|p| p.into_inner());
        let ready = matches!(state.outcome, Some(Ok(())));
        if ready {
            state.handed_over = true;
        }
        ready
    }

    /// The drag ended without a drop. Unpacking still running stops at
    /// its next member; files already made are removed, unless a
    /// receiver was handed their paths — then they are left for
    /// [`sweep`], because it may be copying them.
    pub fn abandon(&self) {
        let remove = {
            let mut state = self.lock();
            state.abandoned = true;
            state.outcome.is_some() && !state.handed_over
        };
        if remove {
            self.remove();
        }
    }

    pub fn abandoned(&self) -> bool {
        self.lock().abandoned
    }

    /// What to do if the unpacking fails: called once, then, or at once
    /// if it already has. The drag uses it to end itself, so a refused
    /// selection does not go on looking droppable.
    pub fn on_fail(&self, cancel: impl FnOnce() + Send + 'static) {
        let mut state = self.lock();
        if matches!(state.outcome, Some(Err(_))) {
            drop(state);
            cancel();
        } else {
            state.on_fail = Some(Box::new(cancel));
        }
    }

    fn remove(&self) {
        if let Err(e) = std::fs::remove_dir_all(&self.dir) {
            if e.kind() != std::io::ErrorKind::NotFound {
                tracing::info!(error = %e, "a drag's unpacked files could not be removed");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hyprforge_archive::StdArchives;
    use std::io::Write;

    /// A zip with `docs/guide.txt`, `docs/deep/more.txt`, `readme.md`,
    /// `other/readme.md` and a 3 KiB `big.bin`.
    fn archive(dir: &Path) -> PathBuf {
        let path = dir.join("sample.zip");
        let mut zip = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
        let options = zip::write::SimpleFileOptions::default();
        for (name, body) in [
            ("docs/guide.txt", &b"a guide"[..]),
            ("docs/deep/more.txt", b"more"),
            ("readme.md", b"# top"),
            ("other/readme.md", b"# other"),
            ("big.bin", &[7u8; 3072]),
        ] {
            zip.start_file(name, options).unwrap();
            zip.write_all(body).unwrap();
        }
        zip.finish().unwrap();
        path
    }

    fn deadline() -> Instant {
        Instant::now() + DEADLINE
    }

    #[test]
    fn every_dragged_member_lands_at_the_top_under_its_own_name() {
        let plan = plan(Path::new("/d"), &["docs/guide.txt".into(), "docs/deep".into()]);
        assert_eq!(plan.lands, vec![PathBuf::from("/d/guide.txt"), PathBuf::from("/d/deep")]);
        assert_eq!(plan.groups.len(), 1, "one folder dragged from is one extraction");
        assert_eq!(plan.groups[0].strip.as_deref(), Some("docs"));
    }

    /// Search results inside an archive come from different folders and
    /// can share a name; neither may overwrite the other.
    #[test]
    fn two_members_with_one_name_land_apart_and_keep_the_name() {
        let plan = plan(Path::new("/d"), &["readme.md".into(), "other/readme.md".into()]);
        assert_eq!(plan.lands, vec![PathBuf::from("/d/readme.md"), PathBuf::from("/d/2/readme.md")]);
    }

    #[test]
    fn the_paths_a_drag_names_exist_once_the_gate_opens() {
        let tmp = tempfile::tempdir().unwrap();
        let archive = archive(tmp.path());
        let dir = make_dir(&tmp.path().join("drags")).unwrap();
        let members = vec!["docs/guide.txt".to_string(), "docs/deep".to_string(), "other/readme.md".to_string(), "readme.md".to_string()];
        let plan = plan(&dir, &members);
        let gate = Gate::new(dir.clone());

        let result = unpack(&StdArchives, &archive, &plan, &Unlock::none(), &gate, deadline());
        gate.finish(result.clone());
        assert_eq!(result, Ok(()));
        assert!(gate.wait(Duration::ZERO));
        assert_eq!(std::fs::read(&plan.lands[0]).unwrap(), b"a guide");
        assert_eq!(std::fs::read(plan.lands[1].join("more.txt")).unwrap(), b"more", "a folder brings what is in it");
        assert_eq!(std::fs::read(&plan.lands[2]).unwrap(), b"# other");
        assert_eq!(std::fs::read(&plan.lands[3]).unwrap(), b"# top");
    }

    /// A request that arrives while unpacking is still going waits for
    /// it, rather than being handed paths that are not there yet.
    #[test]
    fn a_receiver_that_asks_early_waits_for_the_files() {
        let gate = Gate::new(PathBuf::from("/nonexistent"));
        let waiter = {
            let gate = Arc::clone(&gate);
            std::thread::spawn(move || gate.wait(Duration::from_secs(10)))
        };
        std::thread::sleep(Duration::from_millis(50));
        gate.finish(Ok(()));
        assert!(waiter.join().unwrap());
    }

    #[test]
    fn a_selection_over_the_budget_is_refused_before_anything_is_written() {
        let tmp = tempfile::tempdir().unwrap();
        let archive = archive(tmp.path());
        let dir = make_dir(&tmp.path().join("drags")).unwrap();
        let plan = plan(&dir, &["big.bin".to_string()]);
        let gate = Gate::new(dir.clone());
        let mut watch = Watch { gate: &gate, budget: 1024, deadline: deadline(), spent: 0, stopped: None };
        let request = ExtractRequest {
            members: plan.groups[0].members.clone(),
            dest: plan.groups[0].into.clone(),
            strip_prefix: None,
            collision: Collision::Overwrite,
        };
        let result = StdArchives.extract_with(&archive, &request, &Unlock::none(), &mut watch);
        assert!(matches!(result, Err(ArchiveError::Cancelled)));
        assert!(!plan.lands[0].exists(), "nothing past the budget is written");
        assert!(watch.why().contains("3 KiB"), "it says how much: {}", watch.why());
        assert!(watch.why().contains("Copy or Extract"), "it says what to do instead");
    }

    #[test]
    fn a_failed_unpack_cancels_the_drag_and_removes_what_it_wrote() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = make_dir(&tmp.path().join("drags")).unwrap();
        std::fs::write(dir.join("half.txt"), b"x").unwrap();
        let gate = Gate::new(dir.clone());
        let cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = Arc::clone(&cancelled);
        gate.on_fail(move || flag.store(true, std::sync::atomic::Ordering::SeqCst));
        gate.finish(Err("no".into()));
        assert!(cancelled.load(std::sync::atomic::Ordering::SeqCst), "a refused drag must not stay droppable");
        assert!(!dir.exists());
        assert!(!gate.wait(Duration::ZERO), "nothing is handed over");
    }

    /// The case `dnd_finished` would get wrong: a receiver that read the
    /// list may still be copying, so a later "cancelled" must not take
    /// the files away.
    #[test]
    fn files_handed_to_a_receiver_are_kept_even_if_the_drag_then_ends() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = make_dir(&tmp.path().join("drags")).unwrap();
        let gate = Gate::new(dir.clone());
        gate.finish(Ok(()));
        assert!(gate.wait(Duration::ZERO));
        gate.abandon();
        assert!(dir.exists());
    }

    #[test]
    fn a_drag_that_was_never_taken_leaves_nothing_behind() {
        let tmp = tempfile::tempdir().unwrap();
        // Abandoned after unpacking finished…
        let dir = make_dir(&tmp.path().join("drags")).unwrap();
        let gate = Gate::new(dir.clone());
        gate.finish(Ok(()));
        gate.abandon();
        assert!(!dir.exists());
        // …and before: the unpacking stops and cleans up when it ends.
        let dir = make_dir(&tmp.path().join("drags")).unwrap();
        let gate = Gate::new(dir.clone());
        gate.abandon();
        let mut watch = Watch { gate: &gate, budget: BUDGET, deadline: deadline(), spent: 0, stopped: None };
        let advance = Advance { member: "x", files_done: 0, files_total: 1, bytes_done: 0, bytes_total: 1 };
        assert_eq!(watch.advance(advance), Flow::Cancel);
        gate.finish(Err(watch.why()));
        assert!(!dir.exists());
    }

    #[test]
    fn an_encrypted_member_without_its_password_is_refused_in_words() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("secret.zip");
        let mut zip = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
        zip.start_file(
            "secret.txt",
            zip::write::SimpleFileOptions::default().with_aes_encryption(zip::AesMode::Aes256, "pw"),
        )
        .unwrap();
        zip.write_all(b"hidden").unwrap();
        zip.finish().unwrap();

        let dir = make_dir(&tmp.path().join("drags")).unwrap();
        let plan = plan(&dir, &["secret.txt".to_string()]);
        let gate = Gate::new(dir);
        let refused = unpack(&StdArchives, &path, &plan, &Unlock::none(), &gate, deadline()).unwrap_err();
        assert!(refused.contains("encrypted") && refused.contains("unlock"), "{refused}");

        let unlocked = Unlock::with(hyprforge_archive::Secret::new("pw".to_string()));
        assert_eq!(unpack(&StdArchives, &path, &plan, &unlocked, &gate, deadline()), Ok(()), "the remembered password opens it");
        assert_eq!(std::fs::read(&plan.lands[0]).unwrap(), b"hidden");
    }

    /// Found live: on this compositor the drag can reach this window's
    /// own device, and the members came back as copies and were added
    /// to their own archive again.
    #[test]
    fn a_drag_let_go_over_the_archive_it_came_from_does_nothing() {
        let own = OwnDrag { dir: PathBuf::from("/c/drag-1"), from: PathBuf::from("/h/x.zip/docs") };
        let copies = [PathBuf::from("/c/drag-1/guide.txt")];
        assert!(own.returned_home(&copies, Path::new("/h/x.zip/docs"), false));
        assert!(!own.returned_home(&copies, Path::new("/h/x.zip/docs"), true), "Ctrl still copies");
        assert!(!own.returned_home(&copies, Path::new("/h/Downloads"), false), "anywhere else is a copy out");
        assert!(
            !own.returned_home(&[PathBuf::from("/elsewhere/guide.txt")], Path::new("/h/x.zip/docs"), false),
            "someone else's files are theirs to drop"
        );
    }

    /// Old drags go, recent ones stay, and nothing that is not a drag
    /// directory is touched.
    #[test]
    fn the_sweep_removes_only_old_drags() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let fresh = make_dir(root).unwrap();
        let stranger = root.join("not-a-drag");
        std::fs::create_dir(&stranger).unwrap();
        let now = SystemTime::now();

        assert_eq!(sweep(root, now), 0);
        assert!(fresh.exists());

        // A day and a minute later, the drag is old; the stranger is not
        // ours whatever its age.
        assert_eq!(sweep(root, now + KEEP + Duration::from_secs(60)), 1);
        assert!(!fresh.exists());
        assert!(stranger.exists());
    }

    #[test]
    fn past_the_recent_few_an_hour_old_drag_goes() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let made: Vec<PathBuf> = (0..KEEP_RECENT + 2).map(|_| make_dir(root).unwrap()).collect();
        let later = SystemTime::now() + KEEP_RECENT_FOR + Duration::from_secs(60);
        assert_eq!(sweep(root, later), 2, "all are equally old, so all but the recent few go");
        assert_eq!(made.iter().filter(|d| d.exists()).count(), KEEP_RECENT);
        assert_eq!(sweep(root, SystemTime::now()), 0, "none of what is left is old yet");
    }
}
