//! The window's end of `hyprforge-files-admin` — see [`crate::admin`].
//!
//! One [`Session`] per window, started the first time something needs
//! it: `pkexec` asks for the password then, and polkit's
//! `auth_admin_keep` means a second request within a few minutes does
//! not ask again. Every call blocks, so the window makes them from a
//! blocking thread, and every wait is bounded:
//!
//! - the first answer waits [`AUTH_WAIT`], because a person is typing a
//!   password — the same two minutes UDisks2 gives a mount prompt;
//! - a listing or a `stat` after that waits [`READ_WAIT`];
//! - a change waits [`CHANGE_WAIT`] *between lines*, and a copy sends a
//!   progress line every fraction of a second, so a long copy is never
//!   cut off for being long — only for going quiet.
//!
//! Dismissing the password prompt is its own outcome
//! ([`AdminError::Declined`]), never an error dump: pkexec's exit status
//! says which it was, and someone who pressed Cancel did nothing wrong.

use crate::admin::{Body, Op, Reply, Request, WireEntry, WirePath};
use hyprforge_files_core::{Entry, FilesError, FsBackend, StdBackend};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// How long the first answer may take: someone is reading a prompt and
/// typing a password.
pub const AUTH_WAIT: Duration = Duration::from_secs(120);

/// How long a listing, a count or a `stat` may take, once authorised.
pub const READ_WAIT: Duration = Duration::from_secs(10);

/// How long a change may go without a line. A copy says how far it has
/// got several times a second; a delete of a large tree says nothing
/// until it is done, which is what this has to allow for.
pub const CHANGE_WAIT: Duration = Duration::from_secs(120);

/// Why a request got no answer.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AdminError {
    /// The password prompt was dismissed.
    #[error("Administrator access wasn't given.")]
    Declined,
    /// polkit said no without asking — this account may not administer
    /// the machine.
    #[error("This account isn't allowed administrator access.")]
    NotAllowed,
    /// `pkexec` or the helper is not installed.
    #[error("{0}")]
    Unavailable(String),
    #[error("The administrator helper stopped answering.")]
    TimedOut,
    /// The helper exited, or its pipe broke, mid-conversation.
    #[error("The administrator helper stopped: {0}")]
    Lost(String),
}

/// Where the helper is: beside this binary, which is how a debug build
/// finds its own, else where the package puts it. pkexec runs whatever
/// path it is given, and the polkit action names `/usr/bin`'s — so a
/// copy anywhere else still works, under polkit's generic "run a
/// program as administrator" prompt instead of this one's.
pub fn helper_path() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join(crate::admin::PROGRAM)))
        .filter(|p| p.exists())
        .unwrap_or_else(|| Path::new("/usr/bin").join(crate::admin::PROGRAM))
}

/// Stops the copy or move a [`Session`] is running, from another thread.
#[derive(Clone)]
pub struct Canceller(Arc<Mutex<ChildStdin>>);

impl Canceller {
    pub fn cancel(&self) {
        let mut stdin = self.0.lock().unwrap_or_else(|p| p.into_inner());
        let _ = write_request(&mut stdin, &Request { id: 0, op: Op::Cancel });
    }
}

/// A running helper.
pub struct Session {
    /// `Option` only so `Drop` can hand it to a reaper.
    child: Option<Child>,
    stdin: Arc<Mutex<ChildStdin>>,
    replies: Receiver<Reply>,
    next_id: u64,
    /// Whether anything has been answered yet — until then, a wait is a
    /// password prompt's.
    authorised: bool,
    /// Set once the helper has gone; every later request fails at once
    /// rather than waiting on a pipe nobody reads.
    lost: Option<AdminError>,
}

impl Session {
    /// Starts the helper through `pkexec`. Returns at once: the prompt
    /// appears, and is answered, during the first [`Session::ask`].
    pub fn start() -> Result<Session, AdminError> {
        let helper = helper_path();
        if !helper.exists() {
            return Err(AdminError::Unavailable(format!(
                "Administrator access needs {}, which isn't installed.",
                crate::admin::PROGRAM
            )));
        }
        let mut command = Command::new("pkexec");
        command.arg(helper);
        Session::spawn(command).map_err(|e| match e {
            AdminError::Unavailable(_) => {
                AdminError::Unavailable("Administrator access needs pkexec (polkit), which isn't installed.".to_string())
            }
            other => other,
        })
    }

    /// Starts `command` as the helper — the seam the tests use to run it
    /// unprivileged, with no pkexec in between.
    pub fn spawn(mut command: Command) -> Result<Session, AdminError> {
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| AdminError::Unavailable(e.to_string()))?;
        let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
            let _ = child.kill();
            let _ = child.wait();
            return Err(AdminError::Lost("its pipes could not be opened".to_string()));
        };
        let (tx, replies) = mpsc::channel();
        let read = std::thread::Builder::new().name("admin-replies".into()).spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                match serde_json::from_str::<Reply>(&line) {
                    Ok(reply) => {
                        if tx.send(reply).is_err() {
                            break;
                        }
                    }
                    Err(e) => tracing::warn!(error = %e, "an administrator helper reply could not be read"),
                }
            }
        });
        if let Err(e) = read {
            let _ = child.kill();
            let _ = child.wait();
            return Err(AdminError::Lost(e.to_string()));
        }
        Ok(Session {
            child: Some(child),
            stdin: Arc::new(Mutex::new(stdin)),
            replies,
            next_id: 1,
            authorised: false,
            lost: None,
        })
    }

    /// Whether this session is past use — the helper exited, or timed
    /// out. The window starts a new one rather than asking this again.
    pub fn is_lost(&self) -> bool {
        self.lost.is_some()
    }

    pub fn canceller(&self) -> Canceller {
        Canceller(self.stdin.clone())
    }

    /// Asks one thing and waits for its answer, handing each progress
    /// line to `progress` as it comes.
    pub fn ask(&mut self, op: Op, progress: impl FnMut(u64, Option<u64>)) -> Result<Body, AdminError> {
        self.ask_cancellable(op, progress, None)
    }

    /// [`Session::ask`], sending `cancel` to the helper the first time
    /// `stop` is found set — looked at on every progress line, so a copy
    /// stops within a fraction of a second of being asked to.
    pub fn ask_cancellable(
        &mut self,
        op: Op,
        mut progress: impl FnMut(u64, Option<u64>),
        stop: Option<&std::sync::atomic::AtomicBool>,
    ) -> Result<Body, AdminError> {
        let mut cancelled = false;
        if let Some(lost) = &self.lost {
            return Err(lost.clone());
        }
        let changes = !matches!(op, Op::ReadDir { .. } | Op::Stat { .. } | Op::CountChildren { .. });
        let id = self.next_id;
        self.next_id += 1;
        {
            let mut stdin = self.stdin.lock().unwrap_or_else(|p| p.into_inner());
            if let Err(e) = write_request(&mut stdin, &Request { id, op }) {
                drop(stdin);
                return Err(self.lose(Some(e.to_string())));
            }
        }
        loop {
            let wait = match (self.authorised, changes) {
                (false, _) => AUTH_WAIT,
                (true, false) => READ_WAIT,
                (true, true) => CHANGE_WAIT,
            };
            match self.replies.recv_timeout(wait) {
                Ok(Reply { id: answered, body }) if answered == id => {
                    self.authorised = true;
                    match body {
                        Body::Progress { bytes_done, bytes_total } => {
                            progress(bytes_done, bytes_total);
                            if !cancelled && stop.is_some_and(|s| s.load(std::sync::atomic::Ordering::Relaxed)) {
                                cancelled = true;
                                self.canceller().cancel();
                            }
                        }
                        body => return Ok(body),
                    }
                }
                // An answer to something given up on earlier.
                Ok(_) => self.authorised = true,
                Err(RecvTimeoutError::Timeout) => {
                    self.lost = Some(AdminError::TimedOut);
                    return Err(AdminError::TimedOut);
                }
                Err(RecvTimeoutError::Disconnected) => return Err(self.lose(None)),
            }
        }
    }

    /// The helper's output ended: decides why from how it exited, and
    /// remembers it.
    fn lose(&mut self, broken: Option<String>) -> AdminError {
        let status = self.child.as_mut().and_then(|child| wait_bounded(child, Duration::from_secs(2)));
        let why = match status.and_then(|s| s.code()) {
            // pkexec's own codes: 126 when the prompt was dismissed, 127
            // when authorisation could not be had at all.
            Some(126) if !self.authorised => AdminError::Declined,
            Some(127) if !self.authorised => AdminError::NotAllowed,
            Some(code) => AdminError::Lost(broken.unwrap_or_else(|| format!("it exited with status {code}"))),
            None => AdminError::Lost(broken.unwrap_or_else(|| "it exited without saying why".to_string())),
        };
        self.lost = Some(why.clone());
        why
    }
}

impl Drop for Session {
    /// Closing its input — which dropping `stdin` does, unless a
    /// [`Canceller`] still holds it — is the helper's signal to leave.
    /// It cannot be killed from here, being root's, and one finishing a
    /// copy finishes it first; so `pkexec` is reaped on a thread of its
    /// own, for fifteen minutes and no longer.
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = std::thread::Builder::new().name("admin-reap".into()).spawn(move || {
                let _ = wait_bounded(&mut child, REAP_WAIT);
            });
        }
    }
}

/// How long a closed session's helper is waited for, to be reaped.
const REAP_WAIT: Duration = Duration::from_secs(15 * 60);

fn write_request(stdin: &mut ChildStdin, request: &Request) -> std::io::Result<()> {
    let mut line = serde_json::to_vec(request).map_err(std::io::Error::other)?;
    line.push(b'\n');
    stdin.write_all(&line)?;
    stdin.flush()
}

/// `Child::wait`, given up on after `limit` — a helper that closed its
/// output and is still running is one this window will not wait on.
fn wait_bounded(child: &mut Child, limit: Duration) -> Option<std::process::ExitStatus> {
    let deadline = std::time::Instant::now() + limit;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Some(status),
            Ok(None) if std::time::Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            _ => return None,
        }
    }
}

/// One window's helper, shared by everything that needs it — and an
/// [`FsBackend`] over it, so an elevated pane's listings, counts and
/// path completion go through the helper by being handed this instead
/// of the ordinary backend, with nothing above them changed.
///
/// The session starts on first use and starts again after it is lost;
/// one request at a time goes to it, which is all the helper serves.
#[derive(Default)]
pub struct AdminBackend {
    session: Mutex<Option<Session>>,
}

impl AdminBackend {
    /// `None` when there is no helper to run — so the window offers no
    /// way in rather than one that fails.
    pub fn installed() -> Option<AdminBackend> {
        helper_path().exists().then(AdminBackend::default)
    }

    /// Asks the helper one thing, starting it first if need be — which
    /// is when the password prompt appears. Waits [`CHANGE_WAIT`] at
    /// most for a request already running (a long copy) to finish, and
    /// says so rather than queueing forever.
    pub fn ask(&self, op: Op, progress: impl FnMut(u64, Option<u64>)) -> Result<Body, AdminError> {
        self.ask_cancellable(op, progress, None)
    }

    /// [`AdminBackend::ask`], stoppable — see [`Session::ask_cancellable`].
    pub fn ask_cancellable(
        &self,
        op: Op,
        progress: impl FnMut(u64, Option<u64>),
        stop: Option<&std::sync::atomic::AtomicBool>,
    ) -> Result<Body, AdminError> {
        let deadline = std::time::Instant::now() + CHANGE_WAIT;
        let mut guard = loop {
            match self.session.try_lock() {
                Ok(guard) => break guard,
                Err(std::sync::TryLockError::Poisoned(p)) => break p.into_inner(),
                Err(std::sync::TryLockError::WouldBlock) if std::time::Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(20))
                }
                Err(std::sync::TryLockError::WouldBlock) => return Err(AdminError::TimedOut),
            }
        };
        if guard.as_ref().is_none_or(Session::is_lost) {
            *guard = Some(Session::start()?);
        }
        let Some(session) = guard.as_mut() else { return Err(AdminError::Lost("it did not start".to_string())) };
        session.ask_cancellable(op, progress, stop)
    }

    fn files_error(path: &Path, body: Result<Body, AdminError>) -> FilesError {
        let message = match body {
            Ok(Body::Failed { kind: crate::admin::FailKind::NotFound, .. }) => {
                return FilesError::NotFound { path: path.to_path_buf() }
            }
            Ok(Body::Failed { why, .. } | Body::Refused { why }) => why,
            Ok(other) => format!("The administrator helper gave an answer that doesn't fit: {other:?}"),
            Err(e) => e.to_string(),
        };
        FilesError::Elsewhere { path: path.to_path_buf(), message }
    }
}

impl FsBackend for AdminBackend {
    fn read_dir(&self, path: &Path) -> Result<Vec<Entry>, FilesError> {
        match self.ask(Op::ReadDir { path: WirePath::from(path) }, |_, _| {}) {
            Ok(Body::Entries { entries }) => Ok(entries.iter().map(WireEntry::to_entry).collect()),
            other => Err(Self::files_error(path, other)),
        }
    }

    fn stat(&self, path: &Path) -> Result<Entry, FilesError> {
        match self.ask(Op::Stat { path: WirePath::from(path) }, |_, _| {}) {
            Ok(Body::Entry { entry }) => Ok(entry.to_entry()),
            other => Err(Self::files_error(path, other)),
        }
    }

    fn count_children(&self, path: &Path) -> Result<usize, FilesError> {
        match self.ask(Op::CountChildren { path: WirePath::from(path) }, |_, _| {}) {
            Ok(Body::Count { count }) => Ok(count),
            other => Err(Self::files_error(path, other)),
        }
    }

    fn home_dir(&self) -> PathBuf {
        StdBackend.home_dir()
    }

    /// The user's own resolution: it says whether two spellings are one
    /// place, and a path this user cannot resolve is compared as written.
    fn canonicalize(&self, path: &Path) -> Result<PathBuf, FilesError> {
        StdBackend.canonicalize(path)
    }
}
