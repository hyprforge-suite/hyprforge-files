//! One Files window for "Show in folder", not one per click.
//!
//! A browser's "Show in folder", an editor's "Reveal in file manager",
//! `hyprforge-files ~/Downloads` from a launcher — each starts this
//! binary, and each used to open a window of its own. With
//! `[behaviour] open-in = "tab"` (the default) a new process first asks
//! the Files window used most recently to open the folders as tabs, and
//! brings that window forward; only when no window answers does it open
//! its own.
//!
//! **How the windows are found.** Every window serves a socket at
//! `$XDG_RUNTIME_DIR/hyprforge-files/<pid>.sock`, one JSON line in and
//! `ok` out — the shape `hyprforge-settings`' `ipc.rs` uses. Which one
//! is "used most recently" is the compositor's to say: `hyprctl clients`
//! gives each window's `focusHistoryID`, matched to a socket by pid. So
//! this is Hyprland's; off Hyprland, or with `hyprctl` silent, every
//! launch opens a window as before.
//!
//! **Nothing waits without a bound.** The `hyprctl` call is
//! `hyprforge_process`'s, the socket has a read and a write timeout, and
//! a window that does not answer in time is treated as none at all — the
//! new process opens its own, so a wedged window costs a second window,
//! never a launch that never happens. A socket whose window has gone
//! (`ECONNREFUSED`) is removed by whoever finds it.
//!
//! **Bringing it forward.** `hl.dsp.focus({ window = "pid:N" })`, checked
//! live against Hyprland 0.56: it focuses the window, switching to its
//! workspace, whatever `misc:focus_on_activate` says.

use crate::admin::WirePath;
use crate::start::{Start, StartTab};
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// How long a window gets to take a request.
const ANSWER: Duration = Duration::from_secs(2);

/// The folder every window's socket is in. `None` without
/// `$XDG_RUNTIME_DIR`, which a session always sets — and without which
/// there is nowhere private to put a socket, so no window serves one and
/// every launch opens its own.
pub fn dir() -> Option<PathBuf> {
    std::env::var_os("XDG_RUNTIME_DIR").map(|d| PathBuf::from(d).join("hyprforge-files"))
}

/// This process's socket, in `dir`.
fn socket_in(dir: &Path) -> PathBuf {
    dir.join(format!("{}.sock", std::process::id()))
}

/// A [`Start`] on the wire. Paths travel the way the admin helper's do:
/// text when they are UTF-8, bytes when not.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Wire {
    pub tabs: Vec<WireTab>,
    pub properties: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireTab {
    pub dir: WirePath,
    pub select: Vec<WirePath>,
}

impl From<&Start> for Wire {
    fn from(start: &Start) -> Wire {
        Wire {
            tabs: start
                .tabs
                .iter()
                .map(|t| WireTab {
                    dir: WirePath::from(t.dir.as_path()),
                    select: t.select.iter().map(|p| WirePath::from(p.as_path())).collect(),
                })
                .collect(),
            properties: start.properties,
        }
    }
}

impl Wire {
    pub fn to_start(&self) -> Start {
        Start {
            tabs: self
                .tabs
                .iter()
                .map(|t| StartTab { dir: t.dir.to_path(), select: t.select.iter().map(WirePath::to_path).collect() })
                .collect(),
            properties: self.properties,
            new_window: false,
            workspace: None,
        }
    }
}

/// The pids with a socket in `dir`, other than this process's.
fn listening(dir: &Path) -> Vec<u32> {
    let own = std::process::id();
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    entries
        .flatten()
        .filter_map(|e| e.file_name().to_str()?.strip_suffix(".sock")?.parse::<u32>().ok())
        .filter(|pid| *pid != own)
        .collect()
}

/// Of `windows` — `(pid, focusHistoryID)` for every Files window the
/// compositor lists — the one used most recently that is `listening`.
/// `focusHistoryID` 0 is the window with focus now, 1 the one before.
pub fn choose(windows: &[(u32, i64)], listening: &[u32]) -> Option<u32> {
    windows
        .iter()
        .filter(|(pid, history)| listening.contains(pid) && *history >= 0)
        .min_by_key(|(_, history)| *history)
        .map(|(pid, _)| *pid)
}

/// Every Files window Hyprland knows of, as `(pid, focusHistoryID)`.
/// `None` off Hyprland, or when `hyprctl` does not answer.
fn windows() -> Option<Vec<(u32, i64)>> {
    std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE")?;
    let out = hyprforge_process::output(
        std::process::Command::new("hyprctl").args(["-j", "clients"]),
        hyprforge_process::TIMEOUT,
    )
    .ok()?;
    if !out.status.success() {
        return None;
    }
    let clients: Vec<serde_json::Value> = serde_json::from_slice(&out.stdout).ok()?;
    Some(
        clients
            .iter()
            .filter(|c| c["class"].as_str() == Some("hyprforge-files"))
            .filter_map(|c| Some((u32::try_from(c["pid"].as_u64()?).ok()?, c["focusHistoryID"].as_i64()?)))
            .collect(),
    )
}

/// Asks window `pid` to open `start`. `Ok(())` once it said yes.
fn ask(dir: &Path, pid: u32, start: &Start) -> std::io::Result<()> {
    let path = dir.join(format!("{pid}.sock"));
    let mut stream = match UnixStream::connect(&path) {
        Ok(stream) => stream,
        Err(e) => {
            // Its window has gone and left the socket: tidied here.
            if e.kind() == std::io::ErrorKind::ConnectionRefused {
                let _ = std::fs::remove_file(&path);
            }
            return Err(e);
        }
    };
    stream.set_read_timeout(Some(ANSWER))?;
    stream.set_write_timeout(Some(ANSWER))?;
    let mut line = serde_json::to_vec(&Wire::from(start)).map_err(std::io::Error::other)?;
    line.push(b'\n');
    stream.write_all(&line)?;
    let mut answer = String::new();
    BufReader::new(stream).read_line(&mut answer)?;
    if answer.trim() == "ok" {
        Ok(())
    } else {
        Err(std::io::Error::other(format!("the window answered {:?}", answer.trim())))
    }
}

/// Hands `start` to the Files window used most recently and brings it
/// forward. `true` when one took it — this process then has nothing to
/// do. `false` means open a window here, for any reason at all.
pub fn hand_off(start: &Start) -> bool {
    let Some(dir) = dir() else { return false };
    let Some(windows) = windows() else { return false };
    let mut candidates = listening(&dir);
    while let Some(pid) = choose(&windows, &candidates) {
        match ask(&dir, pid, start) {
            Ok(()) => {
                let focus = format!("hl.dsp.focus({{ window = \"pid:{pid}\" }})");
                let _ = hyprforge_process::output(
                    std::process::Command::new("hyprctl").args(["dispatch", &focus]),
                    hyprforge_process::TIMEOUT,
                );
                return true;
            }
            Err(e) => {
                tracing::info!(pid, error = %e, "a Files window did not take the request");
                candidates.retain(|p| *p != pid);
            }
        }
    }
    false
}

/// Puts this process's window on `workspace` once Hyprland has mapped
/// it — `--workspace`. Blocking: waits, bounded, for the window to show
/// up in `hyprctl clients`, because a dispatch aimed at a pid with no
/// window yet does nothing and still says "ok" (found in Media's
/// `float.rs`). Follows it there: the window was opened to be used.
///
/// The window appears where the person is first and then moves. A rule
/// placing it at map time would need it started by Hyprland
/// (`exec_cmd` with rules), through a shell, with a path quoted twice
/// over; one moment on the wrong workspace is the smaller cost.
pub fn move_own_window(workspace: &str) -> Result<(), String> {
    let pid = std::process::id();
    let started = std::time::Instant::now();
    loop {
        if windows().is_some_and(|w| w.iter().any(|(p, _)| *p == pid)) {
            break;
        }
        if started.elapsed() > Duration::from_secs(3) {
            return Err("the window never appeared in hyprctl clients".to_string());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let call = format!(
        "hl.dsp.window.move({{ window = \"pid:{pid}\", workspace = {}, follow = true }})",
        lua_string(workspace)
    );
    let out = hyprforge_process::output(
        std::process::Command::new("hyprctl").args(["dispatch", &call]),
        hyprforge_process::TIMEOUT,
    )
    .map_err(|e| format!("hyprctl dispatch: {e}"))?;
    let said = String::from_utf8_lossy(&out.stdout);
    if said.trim() == "ok" {
        Ok(())
    } else {
        Err(format!("hyprctl dispatch {call}: {}", said.trim()))
    }
}

/// `text` as a Lua string literal — a long bracket whose level no `]=…]`
/// inside it can close. A workspace is a number or a short name, but a
/// name is free text, and an unescaped quote in a dispatch is a parse
/// error that fails the whole call.
fn lua_string(text: &str) -> String {
    let mut level = 0;
    while text.contains(&format!("]{}]", "=".repeat(level))) {
        level += 1;
    }
    let eq = "=".repeat(level);
    format!("[{eq}[{text}]{eq}]")
}

/// Hyprland's workspaces, as the picker offers them: `(what
/// `--workspace` takes, what the button says)`, ordinary ones by number,
/// then named ones, then the next empty number. `None` off Hyprland.
pub fn workspaces() -> Option<Vec<(String, String)>> {
    std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE")?;
    let out = hyprforge_process::output(
        std::process::Command::new("hyprctl").args(["-j", "workspaces"]),
        hyprforge_process::TIMEOUT,
    )
    .ok()?;
    let list: Vec<serde_json::Value> = serde_json::from_slice(&out.stdout).ok()?;
    Some(offered(list.iter().filter_map(|w| Some((w["id"].as_i64()?, w["name"].as_str()?.to_string())))))
}

/// See [`workspaces`]: the order and the labels, from `(id, name)`.
/// Special workspaces (negative ids) are not somewhere to put a window
/// of files.
pub fn offered(list: impl Iterator<Item = (i64, String)>) -> Vec<(String, String)> {
    let mut numbered: Vec<i64> = Vec::new();
    let mut named: Vec<String> = Vec::new();
    for (id, name) in list.filter(|(id, _)| *id > 0) {
        if name == id.to_string() {
            numbered.push(id);
        } else {
            named.push(name);
        }
    }
    numbered.sort_unstable();
    named.sort();
    let next = numbered.iter().max().map_or(1, |m| m + 1);
    numbered
        .into_iter()
        .map(|id| (id.to_string(), id.to_string()))
        .chain(named.into_iter().map(|n| (format!("name:{n}"), n)))
        .chain([(next.to_string(), format!("{next} (empty)"))])
        .collect()
}

/// This window's socket, as a stream of what other launches ask it to
/// open. Answered `ok` as soon as each is read: opening the tabs is the
/// window's, and the asker only needs to know someone has it.
pub fn serve() -> impl iced::futures::Stream<Item = Start> {
    serve_in(dir())
}

/// [`serve`], with its socket in `dir` — the seam a test uses to keep
/// out of the real runtime folder.
fn serve_in(dir: Option<PathBuf>) -> impl iced::futures::Stream<Item = Start> {
    iced::stream::channel(8, async move |mut out: iced::futures::channel::mpsc::Sender<Start>| {
        use iced::futures::SinkExt;
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
        let Some(dir) = dir else { return };
        // Owner-only: what is asked through it opens folders in this
        // person's window.
        {
            use std::os::unix::fs::DirBuilderExt;
            let _ = std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&dir);
        }
        if let Err(e) = std::fs::create_dir_all(&dir) {
            tracing::warn!(error = %e, "no folder for the window's socket; other launches will open their own");
            return;
        }
        let path = socket_in(&dir);
        let _ = std::fs::remove_file(&path);
        let listener = match tokio::net::UnixListener::bind(&path) {
            Ok(listener) => listener,
            Err(e) => {
                tracing::warn!(error = %e, "the window's socket could not be opened");
                return;
            }
        };
        loop {
            let Ok((stream, _)) = listener.accept().await else { continue };
            let (read, mut write) = stream.into_split();
            let mut line = String::new();
            let mut reader = tokio::io::BufReader::new(read);
            let read = tokio::time::timeout(ANSWER, reader.read_line(&mut line)).await;
            let Ok(Ok(_)) = read else { continue };
            let Ok(wire) = serde_json::from_str::<Wire>(&line) else {
                let _ = write.write_all(b"not a request\n").await;
                continue;
            };
            let _ = write.write_all(b"ok\n").await;
            if out.send(wire.to_start()).await.is_err() {
                return;
            }
        }
    })
}

/// Takes this window's socket away, as the window closes.
pub fn withdraw() {
    if let Some(dir) = dir() {
        let _ = std::fs::remove_file(socket_in(&dir));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_window_used_last_takes_it() {
        let windows = [(10, 2), (11, 0), (12, 1)];
        assert_eq!(choose(&windows, &[10, 11, 12]), Some(11));
    }

    #[test]
    fn a_window_with_no_socket_is_passed_over_for_the_next() {
        let windows = [(10, 2), (11, 0), (12, 1)];
        assert_eq!(choose(&windows, &[10, 12]), Some(12), "11 has focus but no socket — an older Files");
        assert_eq!(choose(&windows, &[]), None);
    }

    /// A socket whose window Hyprland does not list — gone, or on another
    /// session — is never chosen, whatever its file says.
    #[test]
    fn a_socket_with_no_window_is_never_chosen() {
        assert_eq!(choose(&[(10, 0)], &[99]), None);
    }

    #[test]
    fn a_request_crosses_the_wire_whole() {
        let start = Start {
            tabs: vec![StartTab { dir: PathBuf::from("/h/Downloads"), select: vec![PathBuf::from("/h/Downloads/a.pdf")] }],
            properties: true,
            ..Start::default()
        };
        let wire: Wire = serde_json::from_str(&serde_json::to_string(&Wire::from(&start)).unwrap()).unwrap();
        assert_eq!(wire.to_start(), start);
    }

    #[test]
    fn the_picker_lists_numbers_then_names_then_the_next_empty_one() {
        let got = offered([(3, "3".into()), (1, "1".into()), (5, "web".into()), (-98, "special:magic".into())].into_iter());
        assert_eq!(
            got,
            [
                ("1".into(), "1".into()),
                ("3".into(), "3".into()),
                ("name:web".into(), "web".into()),
                ("4".into(), "4 (empty)".into()),
            ]
        );
    }

    #[test]
    fn a_workspace_name_cannot_break_out_of_its_string() {
        assert_eq!(lua_string("3"), "[[3]]");
        assert_eq!(lua_string("name:a]]b"), "[=[name:a]]b]=]");
    }

    /// The whole round trip through a real socket: a window serving, a
    /// launch asking, the window hearing what was asked.
    #[tokio::test]
    async fn a_window_hears_what_another_launch_asks() {
        use iced::futures::StreamExt;
        let runtime = tempfile::tempdir().unwrap();
        let mut heard = Box::pin(serve_in(Some(runtime.path().to_path_buf())));
        let start = Start { tabs: vec![StartTab { dir: PathBuf::from("/x"), select: Vec::new() }], ..Start::default() };
        let path = socket_in(runtime.path());
        let asked = tokio::task::spawn_blocking({
            let start = start.clone();
            move || {
                for _ in 0..50 {
                    if path.exists() {
                        // Ask "ourselves" by pid: the path is all `ask` needs.
                        let mut stream = UnixStream::connect(&path).unwrap();
                        let mut line = serde_json::to_vec(&Wire::from(&start)).unwrap();
                        line.push(b'\n');
                        stream.write_all(&line).unwrap();
                        let mut answer = String::new();
                        BufReader::new(stream).read_line(&mut answer).unwrap();
                        return answer;
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
                String::new()
            }
        });
        let got = tokio::time::timeout(Duration::from_secs(5), heard.next()).await.unwrap();
        assert_eq!(got, Some(start));
        assert_eq!(asked.await.unwrap().trim(), "ok");
    }
}
