//! `hyprforge-files-admin`: the one part of this suite that runs as root.
//!
//! DESIGN.md said privilege escalation was not something this suite
//! does, and for an app with no reason to touch the system that stays
//! true. A file manager has one: `/etc`, `/usr/share`, another user's
//! folder — places a person sometimes has to go, and where every other
//! file manager offers a way in. So there is one, and it is as narrow as
//! it can be made:
//!
//! - **A separate program**, run through `pkexec`, so the window — with
//!   its renderer, its fonts, its D-Bus and its decoders of untrusted
//!   files — never runs as root. polkit asks for the password, not this
//!   suite.
//! - **Eight operations and nothing else**: list, stat, count, copy,
//!   move, rename, make a folder, delete. No shell, no "run this", no
//!   reading a file's contents back. Each is the same code the
//!   unprivileged paths use (`hyprforge-listing`, `hyprforge-fileops`),
//!   so an elevated copy behaves exactly like an ordinary one.
//! - **Every path is a full path** with no `..` in it, checked before
//!   anything is touched; and `/` and the folders directly under it are
//!   never moved, renamed or deleted, whoever asks. Each refusal is a
//!   sentence, because the person who sees it typed nothing wrong.
//! - **No Trash.** A root-owned file put in this user's Trash could not
//!   be restored by them, so deleting is deleting, and the window asks
//!   first.
//! - **It leaves.** When the window closes its end, or after five
//!   minutes with nothing asked, the helper exits — a root process does
//!   not outlive the reason for it.
//!
//! It speaks one JSON object per line over standard input and output.
//! A request is `{"id":1,"op":"read_dir","path":"/etc"}`; every reply
//! carries the id it answers. A copy or move sends `progress` lines
//! before its report, and can be cancelled mid-way with `cancel`.
//!
//! Paths travel as text when they are UTF-8 and as bytes when they are
//! not: a name on disk is bytes, and a listing that failed because one
//! name in `/usr/share` was Latin-1 would be a listing nobody could use.

use hyprforge_fileops::{CollisionDecision, CollisionPolicy, OpKind, Operation, StepOutcome};
use hyprforge_files_core::{Entry, EntryKind, EntrySize, FsBackend, ItemCount, StdBackend};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Component, Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, TryRecvError};
use std::time::{Duration, Instant, UNIX_EPOCH};

/// The helper's installed name. The polkit action names its full path,
/// which is what lets `auth_admin_keep` apply to it and nothing else.
pub const PROGRAM: &str = "hyprforge-files-admin";

/// How long the helper waits for its next request before it exits.
pub const IDLE: Duration = Duration::from_secs(5 * 60);

/// How often a copy or move says how far it has got. Often enough for a
/// progress bar to move; rarely enough that a million small files are
/// not a million lines.
const PROGRESS_EVERY: Duration = Duration::from_millis(200);

/// A path on the wire: text when it can be, bytes when it cannot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum WirePath {
    Text(String),
    Bytes(Vec<u8>),
}

impl From<&Path> for WirePath {
    fn from(path: &Path) -> Self {
        match path.to_str() {
            Some(text) => WirePath::Text(text.to_string()),
            None => WirePath::Bytes(path.as_os_str().as_bytes().to_vec()),
        }
    }
}

impl WirePath {
    pub fn to_path(&self) -> PathBuf {
        match self {
            WirePath::Text(text) => PathBuf::from(text),
            WirePath::Bytes(bytes) => PathBuf::from(std::ffi::OsString::from_vec(bytes.clone())),
        }
    }
}

/// What to do when a copy or move finds something already there. There
/// is no asking from inside the helper — the window decides before it
/// sends, the way a paste's conflict dialog decides before it goes on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OnCollision {
    Skip,
    Replace,
    KeepBoth,
}

/// Everything the helper can be asked. Anything else is refused.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Op {
    ReadDir { path: WirePath },
    Stat { path: WirePath },
    CountChildren { path: WirePath },
    Copy { from: WirePath, to: WirePath, on_collision: OnCollision },
    Move { from: WirePath, to: WirePath, on_collision: OnCollision },
    Rename { from: WirePath, to: WirePath },
    Mkdir { path: WirePath },
    Delete { path: WirePath },
    /// Stops the copy or move in progress. Answered by that request's
    /// own report, marked cancelled.
    Cancel,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Request {
    pub id: u64,
    #[serde(flatten)]
    pub op: Op,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reply {
    pub id: u64,
    #[serde(flatten)]
    pub body: Body,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reply", rename_all = "snake_case")]
pub enum Body {
    Entries { entries: Vec<WireEntry> },
    Entry { entry: WireEntry },
    Count { count: usize },
    /// A copy or move is under way. Not the answer — that is `report`.
    Progress { bytes_done: u64, bytes_total: Option<u64> },
    Report { report: WireReport },
    /// A rename, a new folder or a delete went through.
    Done,
    /// Never attempted: the request broke one of the rules in the module
    /// doc. A sentence.
    Refused { why: String },
    /// Attempted, and the system said no.
    Failed { kind: FailKind, why: String },
}

/// The failures a window acts on rather than only shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailKind {
    NotFound,
    /// Something is already at the name asked for.
    Exists,
    /// Even root was refused: a read-only filesystem, an immutable file.
    PermissionDenied,
    Other,
}

/// A listing entry, as `hyprforge-listing` describes one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireEntry {
    pub name: String,
    pub path: WirePath,
    pub is_dir: bool,
    pub size: WireSize,
    /// Seconds and nanoseconds since the epoch; negative for before it.
    pub modified: Option<(i64, u32)>,
    pub is_symlink: bool,
    pub link_broken: bool,
    pub hidden: bool,
    pub mode: u32,
    pub uid: u32,
    pub owner: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WireSize {
    Bytes(u64),
    Items(usize),
    Uncounted,
    Unreadable,
}

impl From<&Entry> for WireEntry {
    fn from(e: &Entry) -> Self {
        WireEntry {
            name: e.name.clone(),
            path: WirePath::from(e.path.as_path()),
            is_dir: e.is_dir,
            size: match e.size {
                EntrySize::Bytes(n) => WireSize::Bytes(n),
                EntrySize::Items(ItemCount::Known(n)) => WireSize::Items(n),
                EntrySize::Items(ItemCount::Pending) => WireSize::Uncounted,
                EntrySize::Items(ItemCount::Unreadable) => WireSize::Unreadable,
            },
            modified: e.modified.map(|t| match t.duration_since(UNIX_EPOCH) {
                Ok(d) => (d.as_secs() as i64, d.subsec_nanos()),
                Err(before) => (-(before.duration().as_secs() as i64), before.duration().subsec_nanos()),
            }),
            is_symlink: e.is_symlink,
            link_broken: e.link_broken,
            hidden: e.hidden,
            mode: e.mode,
            uid: e.uid,
            owner: e.owner.clone(),
        }
    }
}

impl WireEntry {
    pub fn to_entry(&self) -> Entry {
        Entry {
            name: self.name.clone(),
            path: self.path.to_path(),
            is_dir: self.is_dir,
            size: match self.size {
                WireSize::Bytes(n) => EntrySize::Bytes(n),
                WireSize::Items(n) => EntrySize::Items(ItemCount::Known(n)),
                WireSize::Uncounted => EntrySize::UNCOUNTED,
                WireSize::Unreadable => EntrySize::Items(ItemCount::Unreadable),
            },
            modified: self.modified.map(|(secs, nanos)| {
                let d = Duration::new(secs.unsigned_abs(), nanos);
                if secs >= 0 {
                    UNIX_EPOCH + d
                } else {
                    UNIX_EPOCH - d
                }
            }),
            is_symlink: self.is_symlink,
            link_broken: self.link_broken,
            hidden: self.hidden,
            kind: EntryKind::classify(self.is_dir, &self.name),
            mode: self.mode,
            uid: self.uid,
            owner: self.owner.clone(),
            origin: None,
            packed: None,
        }
    }
}

/// What a copy or move did — `hyprforge_fileops::Report`, less what
/// only the process that ran it could use.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireReport {
    pub succeeded: usize,
    pub skipped: usize,
    pub failed: Vec<(WirePath, String)>,
    pub cancelled: bool,
    pub source_removal_failed: Option<String>,
    pub dest: WirePath,
}

/// Whether the helper may look at `path`: a full path, with no `..`.
///
/// Checked on the path as written, not after resolving it: `..` is
/// refused outright rather than resolved, because a path that needs it
/// was built by something that should have resolved it already, and the
/// helper does not guess what was meant.
pub fn check(path: &Path) -> Result<(), String> {
    if !path.is_absolute() {
        return Err(format!("“{}” isn't a full path — it has to start from /.", path.display()));
    }
    if path.components().any(|c| matches!(c, Component::ParentDir | Component::CurDir)) {
        return Err(format!("“{}” goes through “..”, which isn't accepted here.", path.display()));
    }
    Ok(())
}

/// Whether the helper may change `path` — move, rename or delete it, or
/// make it. [`check`], and also never `/` or a folder directly under it:
/// no slip of a hand should be able to delete `/usr`.
pub fn check_changeable(path: &Path) -> Result<(), String> {
    check(path)?;
    let depth = path.components().filter(|c| matches!(c, Component::Normal(_))).count();
    if depth < 2 {
        return Err(format!(
            "{} is one of the system's own top-level folders, and isn't changed from here.",
            path.display()
        ));
    }
    Ok(())
}

/// Checks a request against the rules before anything is touched.
pub fn vet(op: &Op) -> Result<(), String> {
    match op {
        Op::ReadDir { path } | Op::Stat { path } | Op::CountChildren { path } => check(&path.to_path()),
        Op::Copy { from, to, .. } => {
            check(&from.to_path())?;
            check_changeable(&to.to_path())
        }
        Op::Move { from, to, .. } | Op::Rename { from, to } => {
            check_changeable(&from.to_path())?;
            check_changeable(&to.to_path())
        }
        Op::Mkdir { path } | Op::Delete { path } => check_changeable(&path.to_path()),
        Op::Cancel => Ok(()),
    }
}

/// One line read from the window: a request, or the id of one that
/// could not be understood (`0` when even that was unreadable).
enum Incoming {
    Request(Request),
    Garbled(u64),
}

/// Serves requests from `input` until it closes or `idle` passes with
/// nothing asked. The whole of the helper's `main`.
///
/// Reading is on a thread of its own, so a `cancel` can arrive while a
/// copy is running; requests that arrive meanwhile wait their turn.
pub fn serve(input: impl Read + Send + 'static, mut output: impl Write, idle: Duration) -> io::Result<()> {
    let (tx, rx) = mpsc::channel();
    std::thread::Builder::new().name("admin-read".into()).spawn(move || {
        for line in BufReader::new(input).lines() {
            let Ok(line) = line else { break };
            if line.trim().is_empty() {
                continue;
            }
            let incoming = match serde_json::from_str::<Request>(&line) {
                Ok(request) => Incoming::Request(request),
                Err(_) => Incoming::Garbled(
                    serde_json::from_str::<serde_json::Value>(&line)
                        .ok()
                        .and_then(|v| v.get("id").and_then(|id| id.as_u64()))
                        .unwrap_or(0),
                ),
            };
            if tx.send(incoming).is_err() {
                break;
            }
        }
    })?;
    let mut waiting = VecDeque::new();
    loop {
        let next = match waiting.pop_front() {
            Some(next) => next,
            None => match rx.recv_timeout(idle) {
                Ok(next) => next,
                Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => return Ok(()),
            },
        };
        match next {
            Incoming::Garbled(id) => send(
                &mut output,
                id,
                Body::Refused { why: "That isn't a request this helper understands.".to_string() },
            )?,
            Incoming::Request(Request { id, op }) => {
                if let Err(why) = vet(&op) {
                    send(&mut output, id, Body::Refused { why })?;
                    continue;
                }
                answer(id, op, &mut output, &rx, &mut waiting)?;
            }
        }
    }
}

fn send(output: &mut impl Write, id: u64, body: Body) -> io::Result<()> {
    let mut line = serde_json::to_vec(&Reply { id, body }).map_err(io::Error::other)?;
    line.push(b'\n');
    output.write_all(&line)?;
    output.flush()
}

fn failed(e: &io::Error, path: &Path) -> Body {
    let kind = match e.kind() {
        io::ErrorKind::NotFound => FailKind::NotFound,
        io::ErrorKind::AlreadyExists => FailKind::Exists,
        io::ErrorKind::PermissionDenied | io::ErrorKind::ReadOnlyFilesystem => FailKind::PermissionDenied,
        _ => FailKind::Other,
    };
    let why = match kind {
        FailKind::NotFound => format!("{} doesn't exist.", path.display()),
        FailKind::Exists => format!("{} already exists.", path.display()),
        FailKind::PermissionDenied => format!("{} can't be changed, even as administrator: {e}", path.display()),
        FailKind::Other => format!("{}: {e}", path.display()),
    };
    Body::Failed { kind, why }
}

fn answer(
    id: u64,
    op: Op,
    output: &mut impl Write,
    rx: &Receiver<Incoming>,
    waiting: &mut VecDeque<Incoming>,
) -> io::Result<()> {
    let body = match op {
        Op::ReadDir { path } => match StdBackend.read_dir(&path.to_path()) {
            Ok(entries) => Body::Entries { entries: entries.iter().map(WireEntry::from).collect() },
            Err(e) => Body::Failed { kind: FailKind::Other, why: e.to_string() },
        },
        Op::Stat { path } => match StdBackend.stat(&path.to_path()) {
            Ok(entry) => Body::Entry { entry: WireEntry::from(&entry) },
            Err(e) => Body::Failed { kind: FailKind::Other, why: e.to_string() },
        },
        Op::CountChildren { path } => match StdBackend.count_children(&path.to_path()) {
            Ok(count) => Body::Count { count },
            Err(e) => Body::Failed { kind: FailKind::Other, why: e.to_string() },
        },
        Op::Copy { from, to, on_collision } => {
            transfer(id, OpKind::Copy, &from.to_path(), &to.to_path(), on_collision, output, rx, waiting)?
        }
        Op::Move { from, to, on_collision } => {
            transfer(id, OpKind::Move, &from.to_path(), &to.to_path(), on_collision, output, rx, waiting)?
        }
        Op::Rename { from, to } => {
            let (from, to) = (from.to_path(), to.to_path());
            match hyprforge_fileops::batch::rename_noreplace(&from, &to) {
                Ok(()) => Body::Done,
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => failed(&e, &to),
                Err(e) => failed(&e, &from),
            }
        }
        Op::Mkdir { path } => {
            let path = path.to_path();
            match std::fs::create_dir(&path) {
                Ok(()) => Body::Done,
                Err(e) => failed(&e, &path),
            }
        }
        Op::Delete { path } => {
            let path = path.to_path();
            match hyprforge_fileops::delete_permanently(&path) {
                Ok(()) => Body::Done,
                Err(e) => failed(&e, &path),
            }
        }
        // Nothing is running to cancel: it finished first.
        Op::Cancel => return Ok(()),
    };
    send(output, id, body)
}

/// A copy or move, step by step, saying how far it has got and listening
/// for `cancel` between steps.
#[allow(clippy::too_many_arguments)]
fn transfer(
    id: u64,
    kind: OpKind,
    from: &Path,
    to: &Path,
    on_collision: OnCollision,
    output: &mut impl Write,
    rx: &Receiver<Incoming>,
    waiting: &mut VecDeque<Incoming>,
) -> io::Result<Body> {
    let mut op = Operation::real(kind, from, to);
    let policy = match on_collision {
        OnCollision::Skip => CollisionPolicy::Skip,
        OnCollision::Replace => CollisionPolicy::Replace,
        OnCollision::KeepBoth => CollisionPolicy::KeepBoth,
    };
    let mut said = Instant::now();
    loop {
        loop {
            match rx.try_recv() {
                Ok(Incoming::Request(Request { op: Op::Cancel, .. })) => op.request_cancel(),
                Ok(other) => waiting.push_back(other),
                // Disconnected: the window went away mid-copy. The copy
                // finishes all the same — half a tree copied into a
                // system folder is worse than a whole one nobody watched
                // arrive — and then `serve` finds nothing left to read.
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
            }
        }
        match op.step() {
            StepOutcome::Progress(p) => {
                if said.elapsed() >= PROGRESS_EVERY {
                    said = Instant::now();
                    send(output, id, Body::Progress { bytes_done: p.bytes_done, bytes_total: p.bytes_total })?;
                }
            }
            StepOutcome::Collision(_) => op.resolve(CollisionDecision { policy, apply_to_rest: true }),
            StepOutcome::Done(report) => {
                return Ok(Body::Report {
                    report: WireReport {
                        succeeded: report.succeeded.len(),
                        skipped: report.skipped.len(),
                        failed: report.failed.iter().map(|(p, why)| (WirePath::from(p.as_path()), why.clone())).collect(),
                        cancelled: report.cancelled,
                        source_removal_failed: report.source_removal_failed,
                        dest: WirePath::from(report.dest.as_path()),
                    },
                })
            }
        }
    }
}

/// `OsStr` bytes, for building a non-UTF-8 name in a test.
#[cfg(test)]
pub(crate) fn os(bytes: &[u8]) -> &std::ffi::OsStr {
    std::ffi::OsStr::from_bytes(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Runs the server over `requests` (one per line) and returns every
    /// reply it wrote.
    fn run(requests: &[Request]) -> Vec<Reply> {
        let input: Vec<u8> = requests
            .iter()
            .flat_map(|r| {
                let mut line = serde_json::to_vec(r).unwrap();
                line.push(b'\n');
                line
            })
            .collect();
        let mut out = Vec::new();
        serve(io::Cursor::new(input), &mut out, Duration::from_secs(5)).unwrap();
        out.split(|b| *b == b'\n').filter(|l| !l.is_empty()).map(|l| serde_json::from_slice(l).unwrap()).collect()
    }

    fn p(path: &Path) -> WirePath {
        WirePath::from(path)
    }

    #[test]
    fn a_relative_path_is_refused_before_anything_is_touched() {
        assert!(check(Path::new("etc/passwd")).is_err());
        assert!(vet(&Op::Delete { path: WirePath::Text("tmp/x".into()) }).is_err());
    }

    #[test]
    fn a_path_through_dot_dot_is_refused_rather_than_resolved() {
        assert!(check(Path::new("/home/a/../../etc")).is_err());
        assert!(check(Path::new("/home/./a")).is_ok(), "std drops an inner `.` while parsing — nothing to refuse");
    }

    #[test]
    fn the_root_and_its_own_folders_are_never_changed() {
        for path in ["/", "/usr", "/etc", "/home"] {
            assert!(check_changeable(Path::new(path)).is_err(), "{path}");
            assert!(vet(&Op::Delete { path: WirePath::Text(path.into()) }).is_err(), "{path}");
        }
        assert!(check_changeable(Path::new("/etc/hosts")).is_ok());
        assert!(vet(&Op::ReadDir { path: WirePath::Text("/".into()) }).is_ok(), "looking is not changing");
    }

    #[test]
    fn a_copy_may_read_from_anywhere_but_only_write_below_the_top_level() {
        let to_usr = Op::Copy { from: WirePath::Text("/tmp/x".into()), to: WirePath::Text("/usr".into()), on_collision: OnCollision::Skip };
        assert!(vet(&to_usr).is_err());
        let from_root = Op::Copy { from: WirePath::Text("/etc".into()), to: WirePath::Text("/tmp/etc-copy".into()), on_collision: OnCollision::Skip };
        assert!(vet(&from_root).is_ok());
    }

    #[test]
    fn an_unknown_request_is_refused_in_words_under_its_own_id() {
        let mut out = Vec::new();
        serve(io::Cursor::new(b"{\"id\":7,\"op\":\"exec\",\"cmd\":\"sh\"}\nnot json\n".to_vec()), &mut out, Duration::from_secs(5)).unwrap();
        let replies: Vec<Reply> =
            out.split(|b| *b == b'\n').filter(|l| !l.is_empty()).map(|l| serde_json::from_slice(l).unwrap()).collect();
        assert_eq!(replies.len(), 2);
        assert_eq!(replies[0].id, 7);
        assert!(matches!(replies[0].body, Body::Refused { .. }));
        assert_eq!(replies[1].id, 0);
    }

    #[test]
    fn a_listing_comes_back_as_the_entries_the_ordinary_backend_sees() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "hello").unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        let replies = run(&[Request { id: 1, op: Op::ReadDir { path: p(dir.path()) } }]);
        let Body::Entries { entries } = &replies[0].body else { panic!("{:?}", replies[0]) };
        let mut over_wire: Vec<Entry> = entries.iter().map(WireEntry::to_entry).collect();
        let mut direct = StdBackend.read_dir(dir.path()).unwrap();
        over_wire.sort_by(|a, b| a.name.cmp(&b.name));
        direct.sort_by(|a, b| a.name.cmp(&b.name));
        assert_eq!(over_wire, direct, "the wire loses nothing a listing shows");
    }

    #[test]
    fn a_name_that_is_not_utf8_survives_the_wire() {
        let dir = tempfile::tempdir().unwrap();
        let odd = dir.path().join(os(b"caf\xe9.txt"));
        std::fs::write(&odd, "x").unwrap();
        let replies = run(&[Request { id: 1, op: Op::Stat { path: p(&odd) } }]);
        let Body::Entry { entry } = &replies[0].body else { panic!("{:?}", replies[0]) };
        assert_eq!(entry.to_entry().path, odd);
    }

    #[test]
    fn a_copy_reports_what_it_did_and_lands_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let (from, to) = (dir.path().join("a.txt"), dir.path().join("b.txt"));
        std::fs::write(&from, "hello").unwrap();
        let replies = run(&[Request { id: 3, op: Op::Copy { from: p(&from), to: p(&to), on_collision: OnCollision::Skip } }]);
        let Some(Reply { id: 3, body: Body::Report { report } }) = replies.last() else { panic!("{replies:?}") };
        assert_eq!(report.succeeded, 1);
        assert!(report.failed.is_empty());
        assert_eq!(std::fs::read_to_string(&to).unwrap(), "hello");
    }

    #[test]
    fn a_rename_onto_a_name_that_is_taken_says_so_and_replaces_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = (dir.path().join("a"), dir.path().join("b"));
        std::fs::write(&a, "a").unwrap();
        std::fs::write(&b, "b").unwrap();
        let replies = run(&[Request { id: 1, op: Op::Rename { from: p(&a), to: p(&b) } }]);
        assert!(matches!(replies[0].body, Body::Failed { kind: FailKind::Exists, .. }), "{replies:?}");
        assert_eq!(std::fs::read_to_string(&b).unwrap(), "b");
    }

    #[test]
    fn make_delete_and_their_failures_name_what_they_were_about() {
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("new");
        let replies = run(&[
            Request { id: 1, op: Op::Mkdir { path: p(&folder) } },
            Request { id: 2, op: Op::Mkdir { path: p(&folder) } },
            Request { id: 3, op: Op::Delete { path: p(&folder) } },
            Request { id: 4, op: Op::Delete { path: p(&folder) } },
        ]);
        assert_eq!(replies.iter().map(|r| r.id).collect::<Vec<_>>(), [1, 2, 3, 4], "one answer each, in order");
        assert_eq!(replies[0].body, Body::Done);
        assert!(matches!(replies[1].body, Body::Failed { kind: FailKind::Exists, .. }));
        assert_eq!(replies[2].body, Body::Done);
        assert!(matches!(replies[3].body, Body::Failed { kind: FailKind::NotFound, .. }));
        assert!(!folder.exists());
    }

    #[test]
    fn the_helper_leaves_when_nothing_is_asked() {
        // A reader that never answers and never closes: only the idle
        // bound can end this.
        struct Silent;
        impl Read for Silent {
            fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
                std::thread::sleep(Duration::from_secs(3600));
                Ok(0)
            }
        }
        let started = Instant::now();
        serve(Silent, Vec::new(), Duration::from_millis(50)).unwrap();
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn requests_and_replies_keep_their_spelling() {
        let request = Request { id: 1, op: Op::ReadDir { path: WirePath::Text("/etc".into()) } };
        assert_eq!(serde_json::to_string(&request).unwrap(), r#"{"id":1,"op":"read_dir","path":"/etc"}"#);
        let reply = Reply { id: 1, body: Body::Failed { kind: FailKind::NotFound, why: "gone".into() } };
        assert_eq!(serde_json::to_string(&reply).unwrap(), r#"{"id":1,"reply":"failed","kind":"not_found","why":"gone"}"#);
    }
}
