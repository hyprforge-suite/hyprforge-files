//! Opening a file with whatever the desktop thinks should open it.
//!
//! This is the app's job and deliberately not `hyprforge-files-core`'s:
//! the portal's open/save dialog browses the same tree with the same
//! view, but double-clicking there *chooses* a file and returns it to
//! whichever application asked. A dialog that launched a media player
//! when you picked an attachment would be a bug, so the two hosts
//! interpret the same `Activated` outcome differently and only this one
//! launches anything.
//!
//! The desktop's own opener rather than reading `mimeapps.list`
//! ourselves. It is the entry point every other application already
//! uses, which means the user's existing "always open .md in this
//! editor" choice is honoured without this suite reimplementing the
//! lookup and then disagreeing with the rest of the desktop about it.
//! Reimplementing it would also mean owning the whole desktop-entry
//! spec — `Exec` field codes, `TryExec`, terminal applications — to end
//! up where the opener already is.
//!
//! # Why `gio open` first, and `xdg-open` only after
//!
//! Both read the same `mimeapps.list`. They disagree about one earlier
//! step: what type the file *is*.
//!
//! On a desktop `xdg-open` does not recognise — Hyprland is one — it
//! asks `xdg-mime query filetype`, which falls back to `file
//! --mime-type` when the `mimetype` tool is absent. `file` reads
//! content and never the name, so `model.stl` is `application/octet-
//! stream`, `part.3mf` is `application/zip` and `scene.blend` is
//! `application/zstd`. Nothing has a default for those, and `xdg-open`
//! answers a lookup that found nothing by working down a built-in list
//! of *browsers* — so a double-clicked STL opens in Firefox, silently
//! and successfully. Measured on this machine: every `.stl`, `.3mf`,
//! `.uf2`, `.blend` and `.sh` in a real Downloads folder went to the
//! browser that way.
//!
//! `gio` uses the shared MIME database's filename rules, so the same
//! files come back as `model/stl` and `model/3mf` and reach the
//! application the user actually chose. It ships with glib2 and is
//! therefore present almost everywhere, but "almost" is why `xdg-open`
//! stays as the fallback rather than being replaced.
//!
//! What this must never become is a *third* opinion about which
//! application opens a file. Both of these read the user's own
//! `mimeapps.list`; the day this file starts consulting anything else,
//! the rule above has been broken.
//!
//! # Why this does not wait
//!
//! CLAUDE.md is emphatic that nothing here may wait on another process
//! without a bound, and the bound is usually
//! `hyprforge_process::output(.., TIMEOUT)`. That is the wrong tool
//! here, and the distinction is worth stating because reaching for it
//! would look correct: `output()` waits for a child to *exit* and hands
//! back what it printed. The child here is a text editor the user is
//! about to spend an hour in. There is nothing to collect and nothing
//! to wait for — the useful part is over the moment the process exists.
//!
//! So this spawns and never waits at all, which is the strongest form
//! of "bounded": no wait can be too long if there is no wait. What that
//! costs is that a program which fails *after* exec — a missing shared
//! library, a corrupt desktop entry — cannot be reported here, only a
//! failure to start at all. That is a real gap and it is the reason the
//! failure path below says what was attempted rather than pretending to
//! know why nothing appeared.

use std::path::Path;

/// What became of one attempt to open a file.
///
/// `Spawned` deliberately does **not** mean "it worked" — see this
/// module's own doc. It means a process was created; whether a window
/// ever appears is beyond what this can observe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Opened {
    Spawned,
    /// No opener is installed at all — neither `gio` (glib2) nor
    /// `xdg-open` (xdg-utils), each a dependency of essentially every
    /// desktop, so this is rare — but "rare" is not "impossible", and a
    /// file manager that silently does nothing when you double-click is
    /// indistinguishable from one that is broken. The UI says this
    /// sentence instead.
    NoOpener,
    /// It exists and could not be started.
    Failed(String),
    /// A *particular* application was chosen and `gio` is not installed
    /// to launch it. Distinct from [`Opened::NoOpener`]: opening the
    /// file normally still works here, it is only the choice that
    /// cannot be honoured, and the sentence has to say that rather than
    /// claiming nothing can be opened at all.
    NoChooser,
}

impl Opened {
    /// The sentence a user should see, or `None` when there is nothing
    /// to say because it worked.
    ///
    /// Every variant that is not `Spawned` has to name what to do about
    /// it: "no dead ends" (vision pillar 3) means an error in this suite
    /// is an actionable message in the window, never a log line the user
    /// is expected to go and find.
    pub fn message(&self, path: &Path) -> Option<String> {
        let name = path.file_name().unwrap_or(path.as_os_str()).to_string_lossy();
        match self {
            Opened::Spawned => None,
            Opened::NoOpener => Some(format!(
                "Couldn't open {name} because no desktop opener is installed. \
                 Install glib2 (for gio) or xdg-utils, or open it from an \
                 application directly."
            )),
            Opened::Failed(why) => Some(format!("Couldn't open {name}: {why}")),
            Opened::NoChooser => Some(format!(
                "Couldn't open {name} with that application because gio isn't \
                 installed. Install glib2, or open it with the usual application."
            )),
        }
    }
}

/// The openers tried in order, each with the arguments that precede
/// the path — see this module's doc for why `gio` leads.
const OPENERS: [&[&str]; 2] = [&["gio", "open"], &["xdg-open"]];

/// Hands `path` to the desktop's own opener.
pub fn open(path: &Path) -> Opened {
    open_first_of(&OPENERS, path)
}

/// [`open`], over a given list of openers, so a test can exercise the
/// fallback without either real one.
///
/// An opener that is not installed is not a failure: the next one is
/// tried, and only a list with nothing installed in it is `NoOpener`.
/// An opener that *is* installed and fails to start is reported as
/// itself — it is a broken installation, not a missing one, and trying
/// the next opener would bury it.
pub fn open_first_of(openers: &[&[&str]], path: &Path) -> Opened {
    let mut outcome = Opened::NoOpener;
    for opener in openers {
        let (program, args) = opener.split_first().expect("an opener names a program");
        outcome = open_with_args(program, args, path);
        if outcome != Opened::NoOpener {
            return outcome;
        }
    }
    outcome
}

/// [`open`], with arguments before the path — `gio` needs `open`. The
/// seam a test points at something that is not a real opener is
/// [`open_first_of`], which is the whole sequence.
fn open_with_args(opener: &str, args: &[&str], path: &Path) -> Opened {
    match std::process::Command::new(opener)
        .args(args)
        .arg(path)
        // The child's output goes nowhere rather than inheriting this
        // window's. Same reasoning as `hyprforge-trayd`'s
        // `spawn_settings`: a GUI started from here prints its own
        // renderer chatter, and inheriting it buries anything this app
        // actually logged. The child keeps its own logging.
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(_child) => {
            // The handle is dropped without being waited on, which on
            // Unix leaves the child a zombie until this process exits.
            // For a file manager that is a handful of entries in the
            // process table over a session, not a leak worth a reaper
            // thread — but it is deliberate rather than overlooked, and
            // if this ever becomes a long-lived daemon that opens
            // thousands of files, it is the thing to revisit.
            Opened::Spawned
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Opened::NoOpener,
        Err(e) => Opened::Failed(e.to_string()),
    }
}

/// Opens `path` with one chosen application, named by its desktop
/// entry.
///
/// `gio launch` rather than reading the entry's `Exec=` and running it:
/// the field codes (`%f`, `%U`, `%c`), `TryExec`, `Terminal=true` and
/// D-Bus activation are the desktop entry specification, and this file's
/// whole position is that the suite does not reimplement it — see the
/// module doc. There is no `xdg-open` equivalent to fall back to, since
/// `xdg-open` takes a file and not an application, so a machine without
/// `gio` cannot honour a *choice* of application. It is told so, rather
/// than being quietly given the default instead.
pub fn open_with_app(entry: &Path, path: &Path) -> Opened {
    match std::process::Command::new("gio")
        .arg("launch")
        .arg(entry)
        .arg(path)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(_child) => Opened::Spawned,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Opened::NoChooser,
        Err(e) => Opened::Failed(e.to_string()),
    }
}

/// What a file is, for the person's own actions' `types` — see
/// `hyprforge_files_core::custom::TypeOf`: its type by name, then every
/// type that one is a kind of, from the shared MIME database.
///
/// By name, never by reading the file: this runs as a menu opens, on the
/// UI thread, once per selected row, and a name is what an action's
/// author means by "pictures" anyway. The database is the window's copy;
/// a later reload (after a default application is set) changes what
/// opens a type, never what a type is, so a tab keeping the copy it was
/// given is no staler than it needs to be.
pub fn type_of(mime: std::sync::Arc<hyprforge_mime::MimeDb>) -> hyprforge_files_core::custom::TypeOf {
    hyprforge_files_core::custom::TypeOf::new(move |path| {
        let Some(kind) = mime.type_of(path) else { return Vec::new() };
        let kind = mime.canonical(kind).to_string();
        let mut all = mime.lookup().types.ancestors(&kind);
        all.insert(0, kind);
        all
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// The sibling-binary-absent case CLAUDE.md requires every component
    /// to survive: a missing opener is a state with a message, never a
    /// crash and never silence.
    #[test]
    fn a_missing_opener_is_reported_rather_than_failing_silently() {
        let openers: [&[&str]; 1] = [&["xdg-open-does-not-exist-xyz"]];
        let outcome = open_first_of(&openers, Path::new("/tmp/x.txt"));
        assert_eq!(outcome, Opened::NoOpener);

        let message = outcome.message(Path::new("/tmp/x.txt")).expect("a failure must say something");
        assert!(message.contains("x.txt"), "the message names the file the user clicked");
        assert!(message.contains("xdg-utils"), "and names a package that fixes it");
    }

    /// Success is silent. A file manager that popped up "opened!" every
    /// time you double-clicked would be unusable.
    #[test]
    fn opening_something_successfully_says_nothing_at_all() {
        // `true` ignores its argument and exits 0 — a stand-in for an
        // opener that starts fine, without launching a real application
        // on the machine running the tests.
        let openers: [&[&str]; 1] = [&["true"]];
        let outcome = open_first_of(&openers, Path::new("/tmp/x.txt"));
        assert_eq!(outcome, Opened::Spawned);
        assert_eq!(outcome.message(Path::new("/tmp/x.txt")), None);
    }

    /// `gio` is tried first and `xdg-open` only if it is absent — see
    /// the module doc. A missing opener must fall through rather than
    /// being reported, or a machine without `gio` would stop opening
    /// anything at all.
    #[test]
    fn a_missing_opener_falls_through_to_the_next_one() {
        let openers: [&[&str]; 2] = [&["gio-does-not-exist-xyz", "open"], &["true"]];
        assert_eq!(open_first_of(&openers, Path::new("/tmp/x.txt")), Opened::Spawned);

        let none: [&[&str]; 2] = [&["gio-does-not-exist-xyz", "open"], &["xdg-open-does-not-exist-xyz"]];
        assert_eq!(
            open_first_of(&none, Path::new("/tmp/x.txt")),
            Opened::NoOpener,
            "with nothing installed the user still gets the sentence"
        );
    }

    /// The first opener that is actually there is the one used; the
    /// fallback is for absence, not for a second opinion.
    #[test]
    fn the_first_installed_opener_is_the_one_used() {
        let openers: [&[&str]; 2] = [&["true"], &["xdg-open-does-not-exist-xyz"]];
        assert_eq!(open_first_of(&openers, Path::new("/tmp/x.txt")), Opened::Spawned);
    }

    /// Choosing an application needs gio, and a machine without it is
    /// told what it can still do rather than that nothing works.
    #[test]
    fn a_chosen_application_without_gio_says_what_is_missing() {
        let message = Opened::NoChooser.message(Path::new("/tmp/part.stl")).unwrap();
        assert!(message.contains("part.stl"));
        assert!(message.contains("glib2"), "names the package");
        assert!(message.contains("usual application"), "and what still works");
    }

    /// `open` hands a path to `gio open`, which turns it into a file://
    /// URI and asks that *scheme's* handler before it asks what the
    /// file is. A desktop entry claiming the scheme therefore becomes
    /// the opener of everything — measured: double-clicking a PNG with
    /// the viewer as its default re-raised this window instead. The
    /// user's defaults are only reachable while nothing claims it.
    #[test]
    fn the_desktop_entry_never_claims_every_file_on_the_machine() {
        let entry = include_str!("../packaging/hyprforge-files.desktop");
        let types = entry
            .lines()
            .find_map(|line| line.strip_prefix("MimeType="))
            .expect("the entry declares its types");
        let types: Vec<&str> = types.split(';').filter(|t| !t.is_empty()).collect();
        assert!(!types.contains(&"x-scheme-handler/file"), "claims: {types:?}");
        assert!(types.contains(&"inode/directory"), "a folder must still reach Files");
    }

    /// A path with no file name at all (the filesystem root) must still
    /// produce a sentence rather than panicking on an `unwrap` of
    /// `file_name()` — which returns `None` for `/`.
    #[test]
    fn a_path_with_no_file_name_still_produces_a_message() {
        let outcome = Opened::NoOpener;
        let message = outcome.message(&PathBuf::from("/")).expect("still a message");
        assert!(!message.is_empty());
    }
}
