//! What the command line asks the window to open.
//!
//! ```text
//! hyprforge-files                         home
//! hyprforge-files DIR… FILE…              a tab per folder; a file's folder, with it selected
//! hyprforge-files --select PATH…          each path's folder, with the paths selected
//! hyprforge-files --properties PATH…      the same, with Properties open on them
//! hyprforge-files --dbus-service          org.freedesktop.FileManager1, no window
//! ```
//!
//! `--select` and `--properties` are what `org.freedesktop.FileManager1`
//! asks for (`ShowItems`, `ShowItemProperties` — see
//! [`crate::file_manager1`]), and what the photo viewer's "Show in
//! Files" runs. Several paths in one folder are one tab with all of them
//! selected; paths in different folders are a tab each, in the order
//! they were first named — a browser's "show the five downloads" should
//! not open five windows, or five tabs onto one folder.
//!
//! Every path may be a plain path or a `file://` URI, because the
//! desktop entry's `%U` and every FileManager1 caller hand URIs.
//!
//! A path that does not exist is deliberately **not** checked here and
//! **not** replaced with home: it is handed to the browser as the folder
//! to open, and the ordinary read fails with the browser's own "doesn't
//! exist" — the actionable sentence already built, not a second copy of
//! it invented here.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

/// The option that runs the D-Bus service instead of a window.
pub const DBUS_SERVICE: &str = "--dbus-service";

/// What this process was started to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    Window(Start),
    /// Serve `org.freedesktop.FileManager1` — see [`crate::file_manager1`].
    DbusService,
    Help,
}

/// The tabs a window opens with.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Start {
    /// In order; the first is the one shown. Empty means nothing was
    /// named — home, or whatever the window does when left to itself.
    pub tabs: Vec<StartTab>,
    /// Open Properties on the first tab's selection.
    pub properties: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartTab {
    pub dir: PathBuf,
    /// Selected once the folder is listed. Empty selects nothing.
    pub select: Vec<PathBuf>,
}

/// One line of help, for `--help` and for an option this does not know.
pub const USAGE: &str = "usage: hyprforge-files [PATH…] [--select PATH…] [--properties PATH…] | --dbus-service";

/// Reads the arguments (without the program's name). `is_dir` answers
/// whether a path is a folder: `Some(true)`, `Some(false)` for anything
/// else that exists, `None` when it cannot be found out — which is
/// treated as a folder to open, for the reason in the module doc.
pub fn parse(
    args: impl IntoIterator<Item = OsString>,
    is_dir: &dyn Fn(&Path) -> Option<bool>,
) -> Result<Request, String> {
    #[derive(PartialEq)]
    enum Mode {
        Open,
        Select,
    }
    let args: Vec<OsString> = args.into_iter().collect();
    if args.first().is_some_and(|a| a == DBUS_SERVICE) {
        return if args.len() == 1 {
            Ok(Request::DbusService)
        } else {
            Err(format!("{DBUS_SERVICE} takes nothing else\n{USAGE}"))
        };
    }
    let mut start = Start::default();
    let mut mode = Mode::Open;
    let mut options_done = false;
    for arg in args {
        if !options_done {
            match arg.to_str() {
                Some("--select") => {
                    mode = Mode::Select;
                    continue;
                }
                Some("--properties") => {
                    mode = Mode::Select;
                    start.properties = true;
                    continue;
                }
                Some("--help" | "-h") => return Ok(Request::Help),
                Some("--") => {
                    options_done = true;
                    continue;
                }
                Some(option) if option.starts_with("--") => {
                    return Err(format!("{option} isn't an option this knows\n{USAGE}"));
                }
                _ => {}
            }
        }
        let path = absolute(path_of(&arg));
        let (dir, select) = match (&mode, is_dir(&path)) {
            // Shown *in* its folder, whatever it is.
            (Mode::Select, _) | (Mode::Open, Some(false)) => match path.parent() {
                Some(parent) => (parent.to_path_buf(), Some(path)),
                // The root has no folder to be shown in; it is opened.
                None => (path, None),
            },
            (Mode::Open, _) => (path, None),
        };
        match start.tabs.iter_mut().find(|t| t.dir == dir) {
            Some(tab) => tab.select.extend(select.filter(|p| !tab.select.contains(p))),
            None => start.tabs.push(StartTab { dir, select: select.into_iter().collect() }),
        }
    }
    Ok(Request::Window(start))
}

/// Plain path, or a `file://` URI — percent-decoded, with an optional
/// `localhost` authority stripped (`file://localhost/home/x` is valid
/// per RFC 8089; a bare `file:///home/x` is what most tools emit). Not a
/// general URI parser: nothing here depends on one, and a local path's
/// URI never needs more. Bytes that are not UTF-8 are kept as they are.
pub fn path_of(arg: &OsStr) -> PathBuf {
    let Some(rest) = arg.to_str().and_then(|a| a.strip_prefix("file://")) else {
        return PathBuf::from(arg);
    };
    let rest = rest.strip_prefix("localhost").unwrap_or(rest);
    // `hyprforge_fileops::percent` decodes over raw bytes rather than
    // `str`, so a filename that is not valid UTF-8 survives the trip.
    // A `%` that is not a valid escape makes it refuse; the argument is
    // then taken literally, the only remaining honest reading of it.
    let Ok(decoded) = hyprforge_fileops::percent::decode_path(rest) else {
        return PathBuf::from(arg);
    };
    if decoded.is_absolute() {
        decoded
    } else {
        // No leading slash means no `///` form was used — still an
        // absolute local path per the scheme, so one is added rather
        // than resolving it against whatever the working directory is.
        Path::new("/").join(decoded)
    }
}

/// Whether `arg` is a URI this window cannot open — `sftp://`, `smb://`
/// — rather than a path. A FileManager1 caller can hand those; this
/// window browses local paths (gvfs mounts included, by their path).
pub fn is_foreign_uri(arg: &str) -> bool {
    match arg.split_once("://") {
        Some((scheme, _)) => scheme != "file" && !scheme.is_empty() && !scheme.contains('/'),
        None => false,
    }
}

/// A relative path, against the directory this was started in — which
/// is what a person typing `hyprforge-files .` means, and which stops
/// meaning anything the moment the browser navigates.
fn absolute(path: PathBuf) -> PathBuf {
    if path.is_absolute() {
        return path;
    }
    std::path::absolute(&path).unwrap_or(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Folders end in `/` in these tests; everything else is a file; a
    /// name with `missing` in it does not exist.
    fn fake(path: &Path) -> Option<bool> {
        let text = path.to_string_lossy();
        if text.contains("missing") {
            None
        } else {
            Some(text.ends_with("dir") || text == "/" || text.ends_with("Documents"))
        }
    }

    fn start(args: &[&str]) -> Start {
        match parse(args.iter().map(OsString::from), &fake).unwrap() {
            Request::Window(start) => start,
            other => panic!("{other:?}"),
        }
    }

    fn tab(dir: &str, select: &[&str]) -> StartTab {
        StartTab { dir: dir.into(), select: select.iter().map(PathBuf::from).collect() }
    }

    #[test]
    fn nothing_named_is_no_tabs_for_the_window_to_fill() {
        assert_eq!(start(&[]), Start::default());
    }

    #[test]
    fn a_plain_path_resolves_unchanged() {
        assert_eq!(path_of(OsStr::new("/home/alex/Documents")), PathBuf::from("/home/alex/Documents"));
    }

    #[test]
    fn a_file_uri_decodes_to_the_same_plain_path() {
        assert_eq!(path_of(OsStr::new("file:///home/alex/Documents")), PathBuf::from("/home/alex/Documents"));
        assert_eq!(
            path_of(OsStr::new("file://localhost/home/alex/Documents")),
            PathBuf::from("/home/alex/Documents")
        );
    }

    #[test]
    fn a_uri_with_percent_escapes_resolves_to_the_path_it_names() {
        assert_eq!(path_of(OsStr::new("file:///home/alex/My%20Documents")), PathBuf::from("/home/alex/My Documents"));
        assert_eq!(path_of(OsStr::new("file:///tmp/caf%C3%A9%2Bx%25.png")), PathBuf::from("/tmp/café+x%.png"));
        use std::os::unix::ffi::OsStrExt;
        assert_eq!(path_of(OsStr::new("file:///tmp/%FF.bin")).as_os_str().as_bytes(), b"/tmp/\xff.bin");
    }

    #[test]
    fn a_folder_opens_and_a_file_opens_its_folder_with_it_selected() {
        assert_eq!(start(&["/home/alex/Documents"]).tabs, [tab("/home/alex/Documents", &[])]);
        assert_eq!(start(&["/home/alex/notes.txt"]).tabs, [tab("/home/alex", &["/home/alex/notes.txt"])]);
    }

    /// The design choice the module doc calls out: a path that is not
    /// there reaches the browser as itself, which says so.
    #[test]
    fn a_nonexistent_start_path_is_not_short_circuited() {
        assert_eq!(start(&["/home/alex/missing"]).tabs, [tab("/home/alex/missing", &[])]);
    }

    #[test]
    fn several_paths_open_a_tab_each_in_the_order_named() {
        assert_eq!(start(&["/a/dir", "/b/dir"]).tabs, [tab("/a/dir", &[]), tab("/b/dir", &[])]);
    }

    /// ShowItems with five downloads and a picture elsewhere: two tabs,
    /// not six, each with its own items selected.
    #[test]
    fn selected_items_are_grouped_into_a_tab_per_folder() {
        let got = start(&["--select", "/dl/a.zip", "/pics/x.png", "file:///dl/b%20c.pdf", "/dl/a.zip"]);
        assert_eq!(got.tabs, [tab("/dl", &["/dl/a.zip", "/dl/b c.pdf"]), tab("/pics", &["/pics/x.png"])]);
        assert!(!got.properties);
    }

    /// Showing a folder means showing it in *its* folder, selected — not
    /// going into it, which is ShowFolders.
    #[test]
    fn a_selected_folder_is_shown_in_its_parent() {
        assert_eq!(start(&["--select", "/a/dir"]).tabs, [tab("/a", &["/a/dir"])]);
        assert_eq!(start(&["--select", "/"]).tabs, [tab("/", &[])], "the root has nowhere else to be");
    }

    #[test]
    fn properties_selects_and_asks_for_the_inspector() {
        let got = start(&["--properties", "/a/notes.txt"]);
        assert_eq!(got.tabs, [tab("/a", &["/a/notes.txt"])]);
        assert!(got.properties);
    }

    #[test]
    fn a_double_dash_ends_the_options() {
        assert_eq!(start(&["--select", "--", "/a/--select"]).tabs, [tab("/a", &["/a/--select"])]);
    }

    #[test]
    fn an_unknown_option_is_refused_with_the_usage() {
        let err = parse([OsString::from("--frobnicate")], &fake).unwrap_err();
        assert!(err.contains("--frobnicate") && err.contains("usage"), "{err}");
    }

    #[test]
    fn the_service_is_its_own_request_and_takes_nothing_else() {
        assert_eq!(parse([OsString::from(DBUS_SERVICE)], &fake), Ok(Request::DbusService));
        assert!(parse([OsString::from(DBUS_SERVICE), OsString::from("/a")], &fake).is_err());
    }

    #[test]
    fn a_relative_path_is_taken_from_where_this_was_started() {
        let got = start(&["some/dir"]);
        assert!(got.tabs[0].dir.is_absolute(), "{got:?}");
        assert!(got.tabs[0].dir.ends_with("some/dir"));
    }

    #[test]
    fn only_a_non_file_scheme_is_a_foreign_uri() {
        assert!(is_foreign_uri("sftp://host/x") && is_foreign_uri("smb://nas/share"));
        assert!(!is_foreign_uri("file:///x") && !is_foreign_uri("/x") && !is_foreign_uri("/odd/a://b"));
    }
}
