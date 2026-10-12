//! Copied files, across applications — against a *nested* compositor.
//!
//! These tests write to a clipboard, so they must never meet the one the
//! person at this machine is using. They run only when
//! `HYPRFORGE_TEST_NESTED_DISPLAY` names a display, refuse outright if
//! that display is the session's own, and point this process at it
//! before any clipboard call:
//!
//! ```text
//! ./crates/hyprforge-lock/testing/nested.sh
//! HYPRFORGE_TEST_NESTED_DISPLAY=wayland-2 \
//!     cargo test -p hyprforge-files --test nested_clipboard -- --ignored --test-threads=1
//! ```
//!
//! The other application is `wl-clipboard` (`wl-copy`, `wl-paste`), a
//! clipboard implementation this project did not write — which is the
//! point: the formats are checked against somebody else's reading of
//! them, not against this code's own.

use hyprforge_files::system_clipboard::SystemClipboard;
use hyprforge_files_core::clipboard::{ClipVerb, FileClip, FileClipboard};
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const SKIP_MARKER: &str = "HYPRFORGE-SKIP:";

/// Points this process at the nested display, or says why not.
fn nested_display() -> Option<String> {
    let Ok(nested) = std::env::var("HYPRFORGE_TEST_NESTED_DISPLAY") else {
        eprintln!("{SKIP_MARKER} HYPRFORGE_TEST_NESTED_DISPLAY is not set — these tests write a clipboard and only run against a nested compositor");
        return None;
    };
    let session = std::env::var("HYPRFORGE_TEST_SESSION_DISPLAY")
        .or_else(|_| std::env::var("WAYLAND_DISPLAY"))
        .unwrap_or_default();
    assert_ne!(
        nested, session,
        "refusing to write to {nested}: it is the session's own display"
    );
    // Remember the session's display the first time, so a second test in
    // the same process still compares against it after the first one
    // repointed WAYLAND_DISPLAY.
    // SAFETY: `--test-threads=1` is part of how these tests are run
    // (see the module doc), so nothing else reads the environment while
    // this writes it.
    unsafe {
        std::env::set_var("HYPRFORGE_TEST_SESSION_DISPLAY", &session);
        std::env::set_var("WAYLAND_DISPLAY", &nested);
    }
    Some(nested)
}

/// Runs a `wl-clipboard` command against `display`, bounded.
fn wl(display: &str, program: &str, args: &[&str], stdin: Option<&str>) -> String {
    let mut child = Command::new(program)
        .args(args)
        .env("WAYLAND_DISPLAY", display)
        .stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap_or_else(|e| panic!("{program} could not be started: {e}"));
    if let Some(input) = stdin {
        child.stdin.take().unwrap().write_all(input.as_bytes()).unwrap();
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if child.try_wait().unwrap().is_some() {
            break;
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            panic!("{program} {args:?} did not finish in time");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let mut out = String::new();
    use std::io::Read;
    child.stdout.take().unwrap().read_to_string(&mut out).unwrap();
    out
}

fn clip(verb: ClipVerb, paths: &[&str]) -> FileClip {
    FileClip { paths: paths.iter().map(PathBuf::from).collect(), verb }
}

/// Files copied here are offered in every form another file manager or
/// a terminal looks for, and `wl-paste` reads each back as written.
#[test]
#[ignore = "disposable: changes a compositor or the clipboard; tools/hf test disposable"]
fn files_copied_here_are_pasteable_elsewhere() {
    let Some(display) = nested_display() else { return };
    let board = SystemClipboard::new();
    board.set(clip(ClipVerb::Cut, &["/tmp/a b.txt", "/srv/x"])).unwrap();
    // `set` returns once the compositor has the new selection; give the
    // source's thread a moment to be serving before asking it.
    std::thread::sleep(Duration::from_millis(200));

    let types = wl(&display, "wl-paste", &["--list-types"], None);
    for wanted in ["x-special/gnome-copied-files", "text/uri-list", "application/x-kde-cutselection", "text/plain"] {
        assert!(types.lines().any(|t| t == wanted), "{wanted} missing from {types:?}");
    }
    assert_eq!(
        wl(&display, "wl-paste", &["--no-newline", "--type", "x-special/gnome-copied-files"], None),
        "cut\nfile:///tmp/a%20b.txt\nfile:///srv/x"
    );
    assert_eq!(
        wl(&display, "wl-paste", &["--no-newline", "--type", "text/uri-list"], None),
        "file:///tmp/a%20b.txt\r\nfile:///srv/x\r\n"
    );
    assert_eq!(wl(&display, "wl-paste", &["--no-newline", "--type", "text/plain"], None), "/tmp/a b.txt\n/srv/x");
}

/// A URI list another application copied is read back as files to paste.
#[test]
#[ignore = "disposable: changes a compositor or the clipboard; tools/hf test disposable"]
fn files_copied_elsewhere_are_pasteable_here() {
    let Some(display) = nested_display() else { return };
    wl(&display, "wl-copy", &["--type", "text/uri-list"], Some("file:///home/x/r%C3%A9sum%C3%A9.pdf\r\n"));
    std::thread::sleep(Duration::from_millis(200));
    let board = SystemClipboard::new();
    assert_eq!(board.get(), Some(clip(ClipVerb::Copy, &["/home/x/résumé.pdf"])));
}

/// Text copied elsewhere since is not files, even though this app still
/// remembers some: the system clipboard is the truth.
#[test]
#[ignore = "disposable: changes a compositor or the clipboard; tools/hf test disposable"]
fn text_copied_elsewhere_since_means_there_is_nothing_to_paste() {
    let Some(display) = nested_display() else { return };
    let board = SystemClipboard::new();
    board.set(clip(ClipVerb::Copy, &["/tmp/a"])).unwrap();
    std::thread::sleep(Duration::from_millis(200));
    wl(&display, "wl-copy", &[], Some("just some words"));
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(board.get(), None);
}

/// After a move, the clipboard is emptied only if it still holds what
/// this app put there.
#[test]
#[ignore = "disposable: changes a compositor or the clipboard; tools/hf test disposable"]
fn clearing_leaves_someone_elses_copy_alone() {
    let Some(display) = nested_display() else { return };
    let board = SystemClipboard::new();
    board.set(clip(ClipVerb::Cut, &["/tmp/a"])).unwrap();
    std::thread::sleep(Duration::from_millis(200));
    wl(&display, "wl-copy", &[], Some("theirs"));
    std::thread::sleep(Duration::from_millis(200));
    board.clear();
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(wl(&display, "wl-paste", &["--no-newline"], None), "theirs");

    board.set(clip(ClipVerb::Cut, &["/tmp/b"])).unwrap();
    std::thread::sleep(Duration::from_millis(200));
    board.clear();
    std::thread::sleep(Duration::from_millis(200));
    let types = wl(&display, "wl-paste", &["--list-types"], None);
    assert!(!types.contains("x-special/gnome-copied-files"), "still offered: {types:?}");
}
