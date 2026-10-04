//! The tabs the window had open, kept so the next window opens on them:
//! `files-session.toml`.
//!
//! ```toml
//! tabs = ["/home/me/Projects/hyprforge", "/home/me/Downloads"]
//! active = 1
//!
//! [[split]]
//! tab = 1
//! second = "/home/me/Pictures"
//! focused = 1
//! ```
//!
//! A split tab is its `tabs` entry — the left pane's folder — and a
//! `[[split]]` naming its right pane and which of the two had the
//! keyboard. Kept beside the list rather than inside it, so a file from
//! before split view (no `[[split]]` at all) reads as it always did, and
//! a Files from before split view reading a newer file ignores the table
//! it does not know and restores the left panes — never refuses the
//! file. An unsplit session writes no `[[split]]`, so the file reads as
//! it did then too.
//!
//! Its own file rather than a field of `files.toml` — see
//! `hyprforge_paths::files_session_toml_path` for why — and written
//! whole, atomically, a moment after the tabs stop changing, the way the
//! window size is.
//!
//! The rules for reading it are the suite's usual three, and they stay
//! apart:
//!
//! - **Missing** is first run: one tab at home, nothing said.
//! - **Present and unreadable** is said, and the window opens on one tab
//!   at home rather than refusing to start. It is not written over until
//!   the tabs next change, which is the person's own doing.
//! - **A folder in it that is gone** — a stick unplugged, a project
//!   deleted — is skipped, and the rest are restored. A tab that opened
//!   on "this folder doesn't exist" would be a tab the person has to
//!   close before they can do anything.
//!
//! And a path given on the command line wins outright. `hyprforge-files
//! ~/Downloads` is someone asking for Downloads, and reopening five
//! other tabs around it is an answer to a question they did not ask.

use hyprforge_files_core::backend::FsBackend;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

/// How long checking that the saved folders still exist may hold up the
/// window opening. A folder on a share whose server has gone away does
/// not answer a `stat`; it is skipped, not waited for.
pub const CHECK_WITHIN: Duration = Duration::from_millis(500);

/// What is kept.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Session {
    /// Each tab's folder, left to right — a split tab's left pane's.
    pub tabs: Vec<PathBuf>,
    /// Which of them was in front.
    pub active: usize,
    /// The tabs that were split, in tab order — see the module doc.
    #[serde(rename = "split", skip_serializing_if = "Vec::is_empty")]
    pub splits: Vec<Split>,
}

/// One split tab's second pane.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Split {
    /// Which of [`Session::tabs`] this is.
    pub tab: usize,
    /// The right pane's folder.
    pub second: PathBuf,
    /// Which pane had the keyboard: 0 the left, 1 the right. Anything
    /// else, hand-written, is read as the left.
    #[serde(default)]
    pub focused: usize,
}

impl Session {
    /// The split of tab `tab`, if it was split.
    pub fn split_of(&self, tab: usize) -> Option<&Split> {
        self.splits.iter().find(|s| s.tab == tab)
    }

    /// Every folder the session names: each tab's, then each second
    /// pane's — what has to be checked before any of them is reopened.
    pub fn folders(&self) -> Vec<PathBuf> {
        self.tabs.iter().cloned().chain(self.splits.iter().map(|s| s.second.clone())).collect()
    }
}

impl Session {
    /// The same tabs without the ones `exists` says are gone, with the
    /// one in front kept in front when it survives, and the nearest one
    /// to its left otherwise. `None` when nothing is left to restore.
    ///
    /// A split tab is gone only when both its panes are: one gone leaves
    /// the tab unsplit, at the folder that is still there — the same
    /// rule as a whole tab, one pane down.
    pub fn without_gone(&self, mut exists: impl FnMut(&Path) -> bool) -> Option<Session> {
        let mut tabs = Vec::new();
        let mut splits = Vec::new();
        let mut active = 0;
        for (i, dir) in self.tabs.iter().enumerate() {
            let first = exists(dir).then(|| dir.clone());
            let split = self.split_of(i);
            let second = split.filter(|s| exists(&s.second)).map(|s| s.second.clone());
            let (kept, second) = match (first, second) {
                (Some(first), second) => (first, second),
                (None, Some(second)) => (second, None),
                (None, None) => continue,
            };
            if i <= self.active {
                active = tabs.len();
            }
            if let Some(second) = second {
                let focused = split.map_or(0, |s| s.focused).min(1);
                splits.push(Split { tab: tabs.len(), second, focused });
            }
            tabs.push(kept);
        }
        (!tabs.is_empty()).then_some(Session { tabs, active, splits })
    }
}

/// Reads the session at `path`: `Ok(None)` when there is none yet,
/// `Err` with a sentence when there is one that cannot be read.
pub fn load_from(path: &Path) -> Result<Option<Session>, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("{} couldn't be read, so last time's tabs weren't reopened: {e}", path.display())),
    };
    toml::from_str(&text)
        .map(Some)
        .map_err(|e| format!("{} isn't a session Files can read, so last time's tabs weren't reopened: {e}", path.display()))
}

/// Writes `session` to `path`, atomically. Blocking.
pub fn save_to(path: &Path, session: &Session) -> Result<(), String> {
    let text = toml::to_string(session).expect("a session is plain data and always serialises");
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("couldn't save your tabs to {}: {e}", path.display()))?;
    }
    hyprforge_paths::write_atomic(path, &text).map_err(|e| format!("couldn't save your tabs to {}: {e}", path.display()))
}

/// What is saved for a tab showing `dir`. A tab inside an archive is
/// kept as the folder holding the archive: opening an archive is
/// something done on purpose — one with a password would ask for it
/// before the window had finished appearing — and the folder it is in is
/// where the person was. Blocking: it `stat`s whatever is named like an
/// archive.
pub fn kept_as(dir: &Path) -> PathBuf {
    match hyprforge_files_core::archive::split(dir) {
        Some((archive, _)) => archive.parent().map(Path::to_path_buf).unwrap_or(archive),
        None => dir.to_path_buf(),
    }
}

/// The tabs to open with: last time's, when they are wanted and some of
/// them are still there, else `None` — which means one tab, at the start
/// folder, as before there were sessions.
///
/// `given` is whether a path was on the command line; that wins. A
/// session that could not be read is reported through the second value
/// and otherwise treated as none.
pub fn starting_tabs(
    given: bool,
    restore: bool,
    path: &Path,
    backend: Arc<dyn FsBackend>,
) -> (Option<Session>, Option<String>) {
    if given || !restore {
        return (None, None);
    }
    let session = match load_from(path) {
        Ok(Some(session)) => session,
        Ok(None) => return (None, None),
        Err(why) => return (None, Some(why)),
    };
    let present = present(&session.folders(), backend, CHECK_WITHIN);
    (session.without_gone(|dir| present.contains(dir)), None)
}

/// Which of `dirs` exist, as far as can be told within `bound`. Asked on
/// a thread of its own, one folder after another, and whatever has not
/// answered by the deadline counts as gone — the window opens on time
/// with the tabs that answered.
pub fn present(dirs: &[PathBuf], backend: Arc<dyn FsBackend>, bound: Duration) -> std::collections::HashSet<PathBuf> {
    let (tx, rx) = std::sync::mpsc::channel();
    let dirs = dirs.to_vec();
    std::thread::spawn(move || {
        for dir in dirs {
            // A folder — not a file that has taken the name since.
            let here = backend.stat(&dir).is_ok_and(|entry| entry.is_dir);
            if tx.send((dir, here)).is_err() {
                return;
            }
        }
    });
    let deadline = std::time::Instant::now() + bound;
    let mut found = std::collections::HashSet::new();
    while let Some(left) = deadline.checked_duration_since(std::time::Instant::now()) {
        match rx.recv_timeout(left) {
            Ok((dir, true)) => {
                found.insert(dir);
            }
            Ok((_, false)) => {}
            // Every folder answered, or the deadline passed.
            Err(_) => break,
        }
    }
    found
}

/// The window's half: what was last seen, and the generation of the
/// save waiting for the tabs to settle — the same guard the window size
/// uses, so a run of navigations writes once, after the last.
#[derive(Debug, Clone, Default)]
pub struct Keeper {
    /// Where to write. `None` writes nowhere — a window under test must
    /// never touch the real file.
    path: Option<PathBuf>,
    seen: Session,
    generation: u64,
}

impl Keeper {
    pub fn new(path: Option<PathBuf>, now: Session) -> Keeper {
        Keeper { path, seen: now, generation: 0 }
    }

    /// The tabs are now `now`. The generation to arm a save with, when
    /// they changed and keeping them is `wanted`. Never for no tabs at
    /// all: that is the window closing, and the session worth keeping is
    /// the one it had a moment ago.
    pub fn changed(&mut self, now: Session, wanted: bool) -> Option<u64> {
        if now == self.seen || now.tabs.is_empty() {
            return None;
        }
        self.seen = now;
        if !wanted || self.path.is_none() {
            return None;
        }
        self.generation += 1;
        Some(self.generation)
    }

    /// What to write now that the save armed with `generation` is due —
    /// `None` when a later change has armed its own.
    pub fn settled(&self, generation: u64) -> Option<(PathBuf, Session)> {
        if generation != self.generation {
            return None;
        }
        Some((self.path.clone()?, self.seen.clone()))
    }

    /// What to write right now, whatever is pending — the last tab
    /// closing, which ends the process before any timer could fire.
    pub fn now(&self) -> Option<(PathBuf, Session)> {
        Some((self.path.clone()?, self.seen.clone())).filter(|(_, s)| !s.tabs.is_empty())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hyprforge_files_core::backend::StdBackend;

    fn session(tabs: &[&str], active: usize) -> Session {
        Session { tabs: tabs.iter().map(PathBuf::from).collect(), active, splits: Vec::new() }
    }

    fn split(tab: usize, second: &str, focused: usize) -> Split {
        Split { tab, second: PathBuf::from(second), focused }
    }

    #[test]
    fn a_run_of_navigations_is_saved_once_after_the_last() {
        let mut keeper = Keeper::new(Some(PathBuf::from("/x/files-session.toml")), session(&["/a"], 0));
        assert_eq!(keeper.changed(session(&["/a"], 0), true), None, "nothing changed");
        let first = keeper.changed(session(&["/b"], 0), true).unwrap();
        let second = keeper.changed(session(&["/b", "/c"], 1), true).unwrap();
        assert_eq!(keeper.settled(first), None, "superseded");
        let (_, saved) = keeper.settled(second).unwrap();
        assert_eq!(saved, session(&["/b", "/c"], 1));
    }

    #[test]
    fn switching_tabs_is_a_change_worth_keeping() {
        let mut keeper = Keeper::new(Some(PathBuf::from("/s")), session(&["/a", "/b"], 0));
        assert!(keeper.changed(session(&["/a", "/b"], 1), true).is_some());
    }

    #[test]
    fn nothing_is_written_when_restoring_is_off_or_for_a_window_with_no_tabs() {
        let mut keeper = Keeper::new(Some(PathBuf::from("/s")), session(&["/a"], 0));
        assert_eq!(keeper.changed(session(&["/b"], 0), false), None);
        assert_eq!(keeper.changed(Session::default(), true), None, "the window closing");
        assert_eq!(keeper.now().unwrap().1, session(&["/b"], 0), "what it had a moment ago");
        let mut untested = Keeper::new(None, session(&["/a"], 0));
        assert_eq!(untested.changed(session(&["/b"], 0), true), None, "no path, no write");
    }

    #[test]
    fn restoring_skips_a_folder_that_is_gone() {
        let restored = session(&["/a", "/gone", "/c"], 2).without_gone(|d| d != Path::new("/gone")).unwrap();
        assert_eq!(restored, session(&["/a", "/c"], 1), "the tab in front stays in front");
    }

    #[test]
    fn when_the_tab_in_front_is_gone_its_left_neighbour_is_in_front() {
        let restored = session(&["/a", "/b", "/gone", "/d"], 2).without_gone(|d| d != Path::new("/gone")).unwrap();
        assert_eq!(restored, session(&["/a", "/b", "/d"], 1));
        let restored = session(&["/gone", "/b"], 0).without_gone(|d| d != Path::new("/gone")).unwrap();
        assert_eq!(restored, session(&["/b"], 0));
    }

    #[test]
    fn a_session_whose_every_folder_is_gone_restores_nothing() {
        assert_eq!(session(&["/x", "/y"], 0).without_gone(|_| false), None);
        assert_eq!(Session::default().without_gone(|_| true), None);
    }

    #[test]
    fn a_hand_edited_active_past_the_end_lands_on_the_last_tab() {
        let restored = session(&["/a", "/b"], 9).without_gone(|_| true).unwrap();
        assert_eq!(restored.active, 1);
    }

    #[test]
    fn a_missing_session_is_first_run_and_an_unreadable_one_is_said() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("files-session.toml");
        assert_eq!(load_from(&path), Ok(None));
        std::fs::write(&path, "tabs = \"not a list\"\n").unwrap();
        let why = load_from(&path).unwrap_err();
        assert!(why.contains("files-session.toml"), "{why}");
    }

    #[test]
    fn what_is_saved_is_what_comes_back() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("deeper/files-session.toml");
        let saved = session(&["/home/me", "/home/me/Downloads"], 1);
        save_to(&path, &saved).unwrap();
        assert_eq!(load_from(&path), Ok(Some(saved)));
    }

    #[test]
    fn a_path_on_the_command_line_wins_over_the_saved_session() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("files-session.toml");
        save_to(&path, &Session { tabs: vec![dir.path().to_path_buf()], active: 0, splits: Vec::new() }).unwrap();
        let backend: Arc<dyn FsBackend> = Arc::new(StdBackend);
        assert_eq!(starting_tabs(true, true, &path, backend.clone()), (None, None), "argv wins");
        assert_eq!(starting_tabs(false, false, &path, backend.clone()), (None, None), "switched off");
        let (restored, said) = starting_tabs(false, true, &path, backend);
        assert_eq!(said, None);
        assert_eq!(restored.unwrap().tabs, vec![dir.path().to_path_buf()], "and otherwise it is restored");
    }

    #[test]
    fn restoring_checks_the_real_disk_and_a_file_is_not_a_folder() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("note.txt");
        std::fs::write(&file, "x").unwrap();
        let tabs = vec![dir.path().to_path_buf(), file.clone(), dir.path().join("gone")];
        let found = present(&tabs, Arc::new(StdBackend), Duration::from_secs(5));
        assert_eq!(found, [dir.path().to_path_buf()].into());
    }

    #[test]
    fn a_tab_inside_an_archive_is_kept_as_the_folder_holding_it() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("photos.zip");
        std::fs::write(&archive, b"PK").unwrap();
        assert_eq!(kept_as(&archive.join("2024")), dir.path());
        assert_eq!(kept_as(&archive), dir.path());
        assert_eq!(kept_as(dir.path()), dir.path());
    }

    /// Both panes, and which had the keyboard, come back.
    #[test]
    fn a_split_tab_round_trips_through_the_session_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("files-session.toml");
        let saved = Session { splits: vec![split(1, "/home/me/Pictures", 1)], ..session(&["/home/me", "/home/me/Downloads"], 1) };
        save_to(&path, &saved).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("[[split]]"), "{text}");
        assert_eq!(load_from(&path), Ok(Some(saved)));
    }

    /// A file written before split view existed reads as it always did,
    /// and an unsplit session is still written the way it was then — so a
    /// Files from before split view reads it too.
    #[test]
    fn a_session_file_from_before_split_view_still_loads() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("files-session.toml");
        std::fs::write(&path, "tabs = [\"/a\", \"/b\"]\nactive = 1\n").unwrap();
        assert_eq!(load_from(&path), Ok(Some(session(&["/a", "/b"], 1))));
        save_to(&path, &session(&["/a"], 0)).unwrap();
        assert!(!std::fs::read_to_string(&path).unwrap().contains("split"), "nothing new for an unsplit session");
    }

    /// A newer file's `[[split]]` is ignored by a reader that does not
    /// know it — the shape of the old struct, which had no field for it.
    #[test]
    fn an_older_reader_ignores_the_split_rather_than_refusing_the_file() {
        #[derive(Deserialize, Debug, PartialEq)]
        struct Before {
            tabs: Vec<PathBuf>,
            active: usize,
        }
        let text = toml::to_string(&Session { splits: vec![split(0, "/c", 0)], ..session(&["/a"], 0) }).unwrap();
        let before: Before = toml::from_str(&text).unwrap();
        assert_eq!(before, Before { tabs: vec![PathBuf::from("/a")], active: 0 });
    }

    /// One pane's folder gone leaves the tab unsplit at the other; both
    /// gone takes the tab, and the splits after it keep their own tabs.
    #[test]
    fn a_split_whose_pane_is_gone_restores_unsplit() {
        let saved = Session {
            splits: vec![split(0, "/gone2", 0), split(1, "/b2", 1), split(2, "/c2", 1), split(3, "/gone4", 0)],
            ..session(&["/a", "/gone", "/c", "/gone3"], 2)
        };
        let restored = saved.without_gone(|d| !d.to_string_lossy().starts_with("/gone")).unwrap();
        assert_eq!(restored.tabs, vec![PathBuf::from("/a"), PathBuf::from("/b2"), PathBuf::from("/c")]);
        assert_eq!(restored.splits, vec![split(2, "/c2", 1)], "the third tab, still split, still focused right");
        assert_eq!(restored.active, 2);
        assert_eq!(restored.folders().len(), 4);
    }
}
