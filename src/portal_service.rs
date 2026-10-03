//! The D-Bus side of the open/save dialog: `org.freedesktop.impl.portal.FileChooser`,
//! served to xdg-desktop-portal, one dialog process per call.
//!
//! Each call decodes its request ([`crate::portal::Request`]), starts the
//! dialog program with that request as JSON on stdin, and answers with
//! what it writes on stdout. While the dialog is up, an
//! `org.freedesktop.impl.portal.Request` object sits at the call's handle
//! so the frontend can `Close` it — an application that gives up on its
//! dialog, or exits, gets the window taken away rather than left behind.
//!
//! Nothing here waits on the dialog except the call that started it, and
//! that wait ends when the user answers, when `Close` arrives, or when
//! the dialog dies; no other call ever queues behind it. A dialog that
//! dies answers "other", never "cancelled": the user did not cancel it.

use crate::portal::{response, Answer, Request};
use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::Notify;
use zbus::object_server::ObjectServer;
use zbus::zvariant::{OwnedObjectPath, OwnedValue};

/// The name the portal frontend finds this backend by — the `DBusName`
/// of the `.portal` file.
pub const BUS_NAME: &str = "org.freedesktop.impl.portal.desktop.hyprforge";
/// Where every portal backend serves its interfaces.
pub const OBJECT_PATH: &str = "/org/freedesktop/portal/desktop";

/// How a dialog is started: a program and the arguments before the
/// request. The real one is this binary with `dialog`; a test's is a
/// stand-in script.
#[derive(Debug, Clone)]
pub struct DialogCommand {
    pub program: PathBuf,
    pub args: Vec<String>,
}

impl DialogCommand {
    /// This executable, asked for one dialog.
    pub fn this_binary() -> std::io::Result<DialogCommand> {
        Ok(DialogCommand { program: std::env::current_exe()?, args: vec!["dialog".to_string()] })
    }
}

/// The FileChooser interface.
pub struct FileChooser {
    dialog: DialogCommand,
}

impl FileChooser {
    pub fn new(dialog: DialogCommand) -> FileChooser {
        FileChooser { dialog }
    }

    async fn run(
        &self,
        server: &ObjectServer,
        method: &str,
        handle: OwnedObjectPath,
        app_id: &str,
        title: &str,
        options: HashMap<String, OwnedValue>,
    ) -> (u32, HashMap<String, OwnedValue>) {
        tracing::debug!(method, ?options, "a file chooser request");
        let request = match Request::decode(method, app_id, title, &options) {
            Ok(request) => request,
            Err(why) => {
                tracing::warn!(method, reason = %why.0, "a file chooser request could not be read");
                return (response::OTHER, HashMap::new());
            }
        };
        let close = Arc::new(Notify::new());
        // Registered before the dialog starts, so a `Close` that arrives
        // the moment the call does still finds something to close.
        let registered = server.at(&handle, PortalRequest { close: close.clone() }).await;
        if let Err(e) = &registered {
            tracing::warn!(error = %e, "the request object could not be exported; Close will not reach this dialog");
        }
        let answer = run_dialog(&self.dialog, &request, close).await;
        if registered.is_ok() {
            let _ = server.remove::<PortalRequest, _>(&handle).await;
        }
        if let Answer::Failed(why) = &answer {
            tracing::warn!(method, reason = %why, "the dialog failed");
        }
        answer.encode()
    }
}

#[zbus::interface(name = "org.freedesktop.impl.portal.FileChooser")]
impl FileChooser {
    #[allow(clippy::too_many_arguments)]
    async fn open_file(
        &self,
        #[zbus(object_server)] server: &ObjectServer,
        handle: OwnedObjectPath,
        app_id: String,
        _parent_window: String,
        title: String,
        options: HashMap<String, OwnedValue>,
    ) -> (u32, HashMap<String, OwnedValue>) {
        self.run(server, "OpenFile", handle, &app_id, &title, options).await
    }

    #[allow(clippy::too_many_arguments)]
    async fn save_file(
        &self,
        #[zbus(object_server)] server: &ObjectServer,
        handle: OwnedObjectPath,
        app_id: String,
        _parent_window: String,
        title: String,
        options: HashMap<String, OwnedValue>,
    ) -> (u32, HashMap<String, OwnedValue>) {
        self.run(server, "SaveFile", handle, &app_id, &title, options).await
    }

    #[allow(clippy::too_many_arguments)]
    async fn save_files(
        &self,
        #[zbus(object_server)] server: &ObjectServer,
        handle: OwnedObjectPath,
        app_id: String,
        _parent_window: String,
        title: String,
        options: HashMap<String, OwnedValue>,
    ) -> (u32, HashMap<String, OwnedValue>) {
        self.run(server, "SaveFiles", handle, &app_id, &title, options).await
    }
}

/// The `org.freedesktop.impl.portal.Request` object for one call.
struct PortalRequest {
    close: Arc<Notify>,
}

#[zbus::interface(name = "org.freedesktop.impl.portal.Request")]
impl PortalRequest {
    async fn close(&self) {
        // `notify_one` keeps the permit if nothing is waiting yet, so a
        // Close before the dialog has started is not lost.
        self.close.notify_one();
    }
}

/// Starts the dialog, hands it the request, and waits for its answer —
/// or for `close`, which takes the window away and answers "cancelled".
pub async fn run_dialog(command: &DialogCommand, request: &Request, close: Arc<Notify>) -> Answer {
    let json = match serde_json::to_vec(request) {
        Ok(json) => json,
        Err(e) => return Answer::Failed(format!("the request could not be written: {e}")),
    };
    let mut child = match tokio::process::Command::new(&command.program)
        .args(&command.args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        // The dialog's own warnings go to this service's log.
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
    {
        Ok(child) => child,
        Err(e) => return Answer::Failed(format!("the dialog could not start: {e}")),
    };
    if let Some(mut stdin) = child.stdin.take() {
        if let Err(e) = stdin.write_all(&json).await {
            return Answer::Failed(format!("the dialog was not given its request: {e}"));
        }
        // Dropped here, closing it: the dialog reads to the end.
    }
    let Some(mut stdout) = child.stdout.take() else {
        return Answer::Failed("the dialog has no output to answer on".to_string());
    };
    let reading = tokio::spawn(async move {
        let mut out = Vec::new();
        stdout.read_to_end(&mut out).await.map(|_| out)
    });
    tokio::select! {
        status = child.wait() => {
            let out = reading.await.ok().and_then(Result::ok).unwrap_or_default();
            match serde_json::from_slice::<Answer>(&out) {
                Ok(answer) => answer,
                Err(_) => Answer::Failed(match status {
                    Ok(status) => format!("the dialog ended ({status}) without an answer"),
                    Err(e) => format!("the dialog could not be waited for: {e}"),
                }),
            }
        }
        _ = close.notified() => {
            let _ = child.kill().await;
            Answer::Cancelled
        }
    }
}

/// Serves the interface on the session bus until the process is killed.
pub async fn serve(dialog: DialogCommand) -> zbus::Result<()> {
    let _connection = zbus::connection::Builder::session()?
        .name(BUS_NAME)?
        .serve_at(OBJECT_PATH, FileChooser::new(dialog))?
        .build()
        .await?;
    std::future::pending::<()>().await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::portal::Kind;

    fn request() -> Request {
        Request {
            kind: Kind::Open { multiple: false, directory: false },
            app_id: "x".into(),
            title: "t".into(),
            accept_label: None,
            filters: vec![],
            current_filter: None,
            choices: vec![],
            current_folder: None,
        }
    }

    /// A stand-in dialog: a shell script that does what the test says.
    ///
    /// Run *by* `/bin/sh` rather than made executable and run itself:
    /// writing an executable and exec-ing it races every other test that
    /// forks (`ETXTBSY`, see CLAUDE.md), and a script read by a shell is
    /// never executed from the file at all.
    fn script(body: &str) -> (tempfile::TempDir, DialogCommand) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dialog.sh");
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        (dir, DialogCommand { program: "/bin/sh".into(), args: vec![path.display().to_string()] })
    }

    #[tokio::test]
    async fn the_dialogs_answer_is_the_calls_answer() {
        // Paths travel as their bytes — see `portal::raw_path`. `/a/b.txt`:
        let (_dir, command) = script(r#"cat > /dev/null; echo '{"Chosen":{"paths":[[47,97,47,98,46,116,120,116]],"choices":[],"filter":null}}'"#);
        let answer = run_dialog(&command, &request(), Arc::new(Notify::new())).await;
        assert_eq!(answer, Answer::Chosen { paths: vec!["/a/b.txt".into()], choices: vec![], filter: None });
    }

    #[tokio::test]
    async fn the_dialog_is_handed_the_request_it_was_called_with() {
        let (dir, command) = script(&format!("cat > {}/got.json; echo '\"Cancelled\"'", "$(dirname \"$0\")"));
        run_dialog(&command, &request(), Arc::new(Notify::new())).await;
        let got: Request = serde_json::from_slice(&std::fs::read(dir.path().join("got.json")).unwrap()).unwrap();
        assert_eq!(got, request());
    }

    #[tokio::test]
    async fn a_dialog_that_dies_answers_other_not_cancelled() {
        let (_dir, command) = script("cat > /dev/null; exit 3");
        let answer = run_dialog(&command, &request(), Arc::new(Notify::new())).await;
        assert!(matches!(answer, Answer::Failed(_)), "{answer:?}");
        assert_eq!(answer.encode().0, response::OTHER);
    }

    #[tokio::test]
    async fn close_takes_the_dialog_away_and_cancels() {
        // A dialog nobody answers.
        let (_dir, command) = script("cat > /dev/null; sleep 30");
        let close = Arc::new(Notify::new());
        close.notify_one();
        let started = std::time::Instant::now();
        let answer = run_dialog(&command, &request(), close).await;
        assert_eq!(answer, Answer::Cancelled);
        assert!(started.elapsed() < std::time::Duration::from_secs(5), "it did not wait for the dialog");
    }

    #[tokio::test]
    async fn a_dialog_that_cannot_start_answers_other() {
        let command = DialogCommand { program: "/nonexistent/dialog".into(), args: vec![] };
        let answer = run_dialog(&command, &request(), Arc::new(Notify::new())).await;
        assert!(matches!(answer, Answer::Failed(_)));
    }
}
