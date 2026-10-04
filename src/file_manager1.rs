//! `org.freedesktop.FileManager1`: how other applications ask for a
//! folder to be shown — a browser's "Show in Folder" for a download, an
//! editor's "Reveal in File Manager", a portal's `OpenDirectory`.
//!
//! Served by `hyprforge-files --dbus-service`, which the session bus
//! starts on the first call (the installer's activation file names it).
//! It never draws anything: each call starts a window of this same
//! binary with the matching command line ([`crate::start`]) and answers
//! at once.
//!
//! | Method               | Window started                                 |
//! |----------------------|------------------------------------------------|
//! | `ShowFolders`        | `hyprforge-files PATH…` — a tab per folder     |
//! | `ShowItems`          | `hyprforge-files --select PATH…` — each in its folder, selected |
//! | `ShowItemProperties` | `hyprforge-files --properties PATH…`           |
//!
//! Several items in one folder are one tab with all of them selected;
//! that grouping is the window's ([`crate::start::parse`]), so the
//! command line and the bus cannot come to disagree about it. URIs are
//! decoded here, to paths, so a name that is not UTF-8 reaches the window
//! byte for byte; a URI that is not `file://` (`sftp://`, `smb://`) is
//! left out with a warning, because this window browses local paths — a
//! share is reached by its gvfs mount, which is one.
//!
//! The caller's startup id is handed on as `XDG_ACTIVATION_TOKEN` (and
//! `DESKTOP_STARTUP_ID` for X11), which is how a window started on
//! someone else's behalf is allowed to take focus.
//!
//! # Gone when idle
//!
//! The service exits five minutes after its last call, so it is not a
//! process that sits in the session for nothing. The bus starts it again
//! on the next call; a window it started is not its child in any way that
//! matters — each has its own process group and is reaped on a thread
//! while the service lives, and adopted by init after.

use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Notify;
use zbus::fdo;

/// The name every caller asks for.
pub const BUS_NAME: &str = "org.freedesktop.FileManager1";
pub const OBJECT_PATH: &str = "/org/freedesktop/FileManager1";
pub const INTERFACE: &str = "org.freedesktop.FileManager1";

/// How long the service stays without a call.
pub const IDLE: std::time::Duration = std::time::Duration::from_secs(5 * 60);

/// Which of the three calls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Show {
    Folders,
    Items,
    Properties,
}

/// The window's arguments for one call: its option, then `--`, then the
/// paths. `Err` when not one URI named a local path — said to the
/// caller, so a browser that has another way to show the file uses it.
pub fn invocation(show: Show, uris: &[String]) -> Result<Vec<OsString>, String> {
    let mut args: Vec<OsString> = match show {
        Show::Folders => Vec::new(),
        Show::Items => vec!["--select".into()],
        Show::Properties => vec!["--properties".into()],
    };
    // Every path after `--`, so a path can never be read as an option.
    args.push("--".into());
    let mut any = false;
    for uri in uris {
        if crate::start::is_foreign_uri(uri) {
            // The scheme alone: the rest of a URI is where someone was
            // looking, and the journal is not a record of that.
            let scheme = uri.split_once("://").map_or("", |(s, _)| s);
            tracing::warn!(scheme, "a FileManager1 call named something that is not a local file; left out");
            continue;
        }
        let path: PathBuf = crate::start::path_of(std::ffi::OsStr::new(uri));
        if !path.is_absolute() {
            continue;
        }
        args.push(path.into_os_string());
        any = true;
    }
    if any {
        Ok(args)
    } else {
        Err("none of those is a local file this file manager can show".to_string())
    }
}

/// Starts a window. The real one runs this same binary; a test's
/// records what it was asked.
pub trait Launch: Send + Sync + 'static {
    fn launch(&self, args: Vec<OsString>, startup_id: &str) -> std::io::Result<()>;
}

/// This executable, as a window.
pub struct ThisBinary {
    program: PathBuf,
}

impl ThisBinary {
    pub fn new() -> std::io::Result<ThisBinary> {
        Ok(ThisBinary { program: std::env::current_exe()? })
    }
}

impl Launch for ThisBinary {
    fn launch(&self, args: Vec<OsString>, startup_id: &str) -> std::io::Result<()> {
        use std::os::unix::process::CommandExt;
        let mut command = std::process::Command::new(&self.program);
        command
            .args(args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .process_group(0);
        if !startup_id.is_empty() {
            command.env("XDG_ACTIVATION_TOKEN", startup_id).env("DESKTOP_STARTUP_ID", startup_id);
        }
        let mut child = command.spawn()?;
        // Reaped, never waited for: the window lives as long as someone
        // keeps it open, and this answers its caller now.
        std::thread::spawn(move || {
            let _ = child.wait();
        });
        Ok(())
    }
}

/// The interface.
pub struct FileManager1 {
    launch: Arc<dyn Launch>,
    /// Told on every call, so the idle clock starts again.
    activity: Arc<Notify>,
}

impl FileManager1 {
    pub fn new(launch: Arc<dyn Launch>, activity: Arc<Notify>) -> FileManager1 {
        FileManager1 { launch, activity }
    }

    fn show(&self, show: Show, uris: Vec<String>, startup_id: &str) -> fdo::Result<()> {
        self.activity.notify_one();
        // Counts, never paths — see `invocation`.
        tracing::info!(?show, items = uris.len(), "FileManager1 call");
        let args = invocation(show, &uris).map_err(fdo::Error::InvalidArgs)?;
        self.launch
            .launch(args, startup_id)
            .map_err(|e| fdo::Error::Failed(format!("the file manager window could not start: {e}")))
    }
}

#[zbus::interface(name = "org.freedesktop.FileManager1")]
impl FileManager1 {
    async fn show_folders(&self, uris: Vec<String>, startup_id: String) -> fdo::Result<()> {
        self.show(Show::Folders, uris, &startup_id)
    }

    async fn show_items(&self, uris: Vec<String>, startup_id: String) -> fdo::Result<()> {
        self.show(Show::Items, uris, &startup_id)
    }

    async fn show_item_properties(&self, uris: Vec<String>, startup_id: String) -> fdo::Result<()> {
        self.show(Show::Properties, uris, &startup_id)
    }
}

/// Waits until `activity` has been quiet for `after`.
pub async fn idle(activity: &Notify, after: std::time::Duration) {
    // `notify_one` keeps a permit when nobody is waiting, so a call that
    // lands between two turns of this loop still restarts the clock.
    while tokio::time::timeout(after, activity.notified()).await.is_ok() {}
}

/// Serves the interface on the session bus until it has been idle for
/// [`IDLE`].
pub async fn serve(launch: Arc<dyn Launch>) -> zbus::Result<()> {
    let activity = Arc::new(Notify::new());
    let _connection = zbus::connection::Builder::session()?
        .name(BUS_NAME)?
        .serve_at(OBJECT_PATH, FileManager1::new(launch, activity.clone()))?
        .build()
        .await?;
    idle(&activity, IDLE).await;
    tracing::info!("FileManager1 idle; exiting until the next call");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[derive(Default)]
    struct Recorder {
        calls: Mutex<Vec<(Vec<OsString>, String)>>,
    }

    impl Launch for Recorder {
        fn launch(&self, args: Vec<OsString>, startup_id: &str) -> std::io::Result<()> {
            self.calls.lock().unwrap().push((args, startup_id.to_string()));
            Ok(())
        }
    }

    fn os(words: &[&str]) -> Vec<OsString> {
        words.iter().map(OsString::from).collect()
    }

    #[test]
    fn each_call_starts_the_window_with_its_own_option() {
        let uris = vec!["file:///a/b.png".to_string()];
        assert_eq!(invocation(Show::Folders, &uris).unwrap(), os(&["--", "/a/b.png"]));
        assert_eq!(invocation(Show::Items, &uris).unwrap(), os(&["--select", "--", "/a/b.png"]));
        assert_eq!(invocation(Show::Properties, &uris).unwrap(), os(&["--properties", "--", "/a/b.png"]));
    }

    #[test]
    fn a_file_manager1_uri_with_percent_escapes_resolves_to_a_path() {
        let got = invocation(Show::Items, &["file:///home/a/My%20Files/r%C3%A9sum%C3%A9.pdf".to_string()]).unwrap();
        assert_eq!(got[2], OsString::from("/home/a/My Files/résumé.pdf"));
    }

    /// ShowItems over three folders' worth of items is one window, and
    /// the window makes one tab per folder of them.
    #[test]
    fn show_items_groups_items_by_folder() {
        let uris: Vec<String> =
            ["file:///dl/a.zip", "file:///pics/x.png", "file:///dl/b.pdf"].map(String::from).to_vec();
        let args = invocation(Show::Items, &uris).unwrap();
        let crate::start::Request::Window(start) = crate::start::parse(args, &|_| Some(false)).unwrap() else {
            panic!("a window")
        };
        let tabs: Vec<(PathBuf, Vec<PathBuf>)> = start.tabs.into_iter().map(|t| (t.dir, t.select)).collect();
        assert_eq!(
            tabs,
            [
                (PathBuf::from("/dl"), vec![PathBuf::from("/dl/a.zip"), PathBuf::from("/dl/b.pdf")]),
                (PathBuf::from("/pics"), vec![PathBuf::from("/pics/x.png")]),
            ]
        );
    }

    #[test]
    fn a_uri_that_is_not_a_local_file_is_left_out_and_none_at_all_is_an_error() {
        let got = invocation(Show::Items, &["sftp://host/x".to_string(), "file:///a".to_string()]).unwrap();
        assert_eq!(got, os(&["--select", "--", "/a"]));
        assert!(invocation(Show::Items, &["smb://nas/share/x".to_string()]).is_err());
        assert!(invocation(Show::Items, &[]).is_err());
    }

    /// The interface as a caller sees it, over a real D-Bus connection —
    /// peer to peer on a socket pair, so no bus is joined and nothing
    /// anyone else runs can call it.
    #[tokio::test]
    async fn show_items_over_dbus_starts_a_window_selecting_them() {
        let (ours, theirs) = tokio::net::UnixStream::pair().unwrap();
        let recorder = Arc::new(Recorder::default());
        let activity = Arc::new(Notify::new());
        let launch: Arc<dyn Launch> = recorder.clone();
        let server = zbus::connection::Builder::unix_stream(ours)
            .server(zbus::Guid::generate())
            .unwrap()
            .p2p()
            .serve_at(OBJECT_PATH, FileManager1::new(launch, activity.clone()))
            .unwrap()
            .build();
        let client = zbus::connection::Builder::unix_stream(theirs).p2p().build();
        let (server, client) = tokio::join!(server, client);
        let (_server, client) = (server.unwrap(), client.unwrap());
        client
            .call_method(
                None::<&str>,
                OBJECT_PATH,
                Some(INTERFACE),
                "ShowItems",
                &(vec!["file:///a/b%20c.png"], "token-1"),
            )
            .await
            .unwrap();
        let refused = client
            .call_method(None::<&str>, OBJECT_PATH, Some(INTERFACE), "ShowFolders", &(vec!["smb://x/y"], ""))
            .await;
        assert!(refused.is_err(), "the caller hears that nothing could be shown");
        let calls = recorder.calls.lock().unwrap().clone();
        assert_eq!(calls, [(os(&["--select", "--", "/a/b c.png"]), "token-1".to_string())]);
    }

    /// The packaged activation file starts what this module serves: the
    /// name it owns, and the option that makes the binary serve it.
    #[test]
    fn the_packaged_activation_file_starts_this_service() {
        let file = include_str!("../packaging/hyprforge-files.FileManager1.service");
        assert!(file.lines().any(|l| l == format!("Name={BUS_NAME}")), "{file}");
        assert!(
            file.lines().any(|l| l == format!("Exec=/usr/bin/hyprforge-files {}", crate::start::DBUS_SERVICE)),
            "{file}"
        );
    }

    #[tokio::test]
    async fn the_service_stops_waiting_once_it_has_been_quiet_long_enough() {
        let activity = Notify::new();
        activity.notify_one();
        let quiet = std::time::Duration::from_millis(50);
        let started = std::time::Instant::now();
        tokio::time::timeout(std::time::Duration::from_secs(5), idle(&activity, quiet))
            .await
            .expect("idle returns");
        assert!(started.elapsed() >= quiet, "a call restarted the clock");
    }
}
