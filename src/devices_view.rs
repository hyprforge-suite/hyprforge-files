//! The Files window's drives and network shares: handing the window's
//! one `DeviceHost` state to every tab, carrying out what a tab asks, and
//! the Connect to Server dialog.
//!
//! The decisions are elsewhere — what the sections show and what a click
//! means in `hyprforge_files_core::devices` and the browser, how a drive
//! is mounted in `hyprforge-volumes`, the dialog's phases in
//! `hyprforge_files::connect`. This is the window's wiring between them.

use super::{dialog, App, At, BrowserMessage, Message};
use hyprforge_files::connect::{hint, ConnectDialog, Phase};
use hyprforge_files::devices::{Done, Event};
use hyprforge_files_core::devices::Ask;
use hyprforge_secret::Secret;
use hyprforge_ui::theme::{spacing, FontScale, BASE_TEXT_SIZE};
use hyprforge_ui::widgets::{meta_text, primary_button, scaled_text, secondary_button};
use hyprforge_volumes::{ConnectError, Gvfs, Operation, VolumeError, VolumeId};
use iced::widget::{column, row, text_input, Id, Space};
use iced::{Element, Length, Task};
use std::path::PathBuf;

/// The Connect to Server dialog while it is open.
pub(super) struct Connecting {
    pub(super) dialog: ConnectDialog,
    /// The attempt running, if one is. Aborted when dropped — by Cancel,
    /// or by the dialog closing — and the `gio` it was running dies with
    /// it (`kill_on_drop`): cancelling is real, not a hidden wait.
    attempt: Option<iced::task::Handle>,
}

/// What the dialog's controls say.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum ConnectMessage {
    Address(String),
    User(String),
    Domain(String),
    /// Wrapped from the keystroke on, so a `Debug` of the window's
    /// message — the one thing that might reach a log — is a length.
    Password(Secret<String>),
    Submit,
    Choose(usize),
    Cancel,
    Ended(Result<Option<PathBuf>, ConnectError>),
}

/// The passphrase prompt for an encrypted drive, while it is open.
///
/// The passphrase is a [`Secret`] from the keystroke on, like the
/// Connect dialog's password, and it goes to UDisks2 and nowhere else —
/// never kept past the attempt, never written down.
pub(super) struct DriveUnlock {
    id: VolumeId,
    label: String,
    /// Go there once it is open: a click on the row, not Mount from a
    /// menu.
    open: bool,
    /// The tab that asked, which is where it opens.
    tab: u64,
    passphrase: Secret<String>,
    /// The last attempt's failure — a wrong passphrase is said here,
    /// beside the field to fix it in.
    error: Option<String>,
    /// An attempt is running. UDisks2 cannot be stopped part-way, so
    /// the prompt waits for it rather than pretending to cancel.
    running: bool,
}

/// What the passphrase prompt's controls say.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum DriveUnlockMessage {
    Passphrase(Secret<String>),
    Submit,
    Cancel,
}

fn passphrase_id() -> Id {
    Id::new("unlock-drive-passphrase")
}

/// The address field, so the dialog can put the cursor in it.
fn address_id() -> Id {
    Id::new("connect-to-server-address")
}

fn password_id() -> Id {
    Id::new("connect-to-server-password")
}

impl App {
    /// Hands every pane the window's drives as they now are, and asks
    /// for whatever icons the change needs.
    pub(super) fn broadcast_devices(&mut self) -> Task<Message> {
        let snapshot = self.devices.snapshot();
        let mut tasks = Vec::new();
        for at in self.every_pane() {
            let outcome = self.pane_mut(at).browser.update(BrowserMessage::DevicesChanged(snapshot.clone()));
            tasks.push(self.handle_outcome(at, outcome));
        }
        Task::batch(tasks)
    }

    /// What the watch heard.
    pub(super) fn heard_devices(&mut self, event: Event) -> Task<Message> {
        if !self.devices.heard(event) {
            return Task::none();
        }
        let gone = self.devices.take_gone();
        Task::batch([self.broadcast_devices(), self.leave(gone)])
    }

    /// Sends every pane standing on one of `gone` home. A pane on a drive
    /// that has just been unmounted, ejected or pulled out would be
    /// showing files that are not there; home is where a pin to a
    /// vanished folder would have nowhere better to send it either.
    fn leave(&mut self, gone: Vec<PathBuf>) -> Task<Message> {
        let mut tasks = Vec::new();
        for at in self.every_pane() {
            if gone.iter().any(|g| self.pane(at).browser.current_dir().starts_with(g)) {
                let home = self.home_dir.clone();
                let outcome = self.pane_mut(at).browser.update(BrowserMessage::Navigate(home));
                tasks.push(self.handle_outcome(at, outcome));
            }
        }
        Task::batch(tasks)
    }

    /// A pane asked for a drive to be mounted, unmounted or ejected, or a
    /// share disconnected. A mount opens in the pane that asked.
    pub(super) fn ask_devices(&mut self, at: At, ask: Ask) -> Task<Message> {
        let tab = self.pane(at).id;
        if let Ask::Unlock { id, open } = ask {
            let Some(volume) = self.devices.devices().volume(&id) else { return Task::none() };
            if self.devices.devices().busy.contains_key(&id) {
                return Task::none();
            }
            let label = volume.label.clone();
            self.drive_unlock =
                Some(DriveUnlock { id, label, open, tab, passphrase: Secret::default(), error: None, running: false });
            return iced::widget::operation::focus(passphrase_id());
        }
        match self.devices.ask(ask, tab) {
            Some(work) => Task::batch([self.broadcast_devices(), Task::perform(work, Message::DeviceDone)]),
            None => Task::none(),
        }
    }

    pub(super) fn device_done(&mut self, done: Done) -> Task<Message> {
        let finished = self.devices.done(done);
        let mut tasks = vec![self.broadcast_devices()];
        if let Some((id, again)) = &finished.unlock_ended {
            if let Some(prompt) = self.drive_unlock.as_mut().filter(|p| &p.id == id) {
                if *again {
                    // Asked again, with the refused passphrase gone —
                    // it is not offered back.
                    prompt.running = false;
                    prompt.passphrase = Secret::default();
                    prompt.error = Some(hyprforge_volumes::sentence(
                        Operation::Unlock,
                        &prompt.label,
                        &VolumeError::WrongPassphrase,
                    ));
                    tasks.push(iced::widget::operation::focus(passphrase_id()));
                } else {
                    self.drive_unlock = None;
                }
            }
        }
        if let Some(status) = finished.status {
            self.status = Some(status);
        }
        if let Some((tab, point)) = finished.open {
            if let Some(at) = self.locate(tab) {
                let outcome = self.pane_mut(at).browser.update(BrowserMessage::Navigate(point));
                tasks.push(self.handle_outcome(at, outcome));
            }
        }
        tasks.push(self.leave(finished.left.into_iter().collect()));
        Task::batch(tasks)
    }

    pub(super) fn drive_unlock_update(&mut self, message: DriveUnlockMessage) -> Task<Message> {
        let Some(prompt) = &mut self.drive_unlock else { return Task::none() };
        match message {
            DriveUnlockMessage::Passphrase(secret) => {
                prompt.passphrase = secret;
                prompt.error = None;
                Task::none()
            }
            DriveUnlockMessage::Cancel => {
                if !prompt.running {
                    self.drive_unlock = None;
                }
                Task::none()
            }
            DriveUnlockMessage::Submit => {
                // An empty passphrase is not a guess worth sending.
                if prompt.running || prompt.passphrase.expose().is_empty() {
                    return Task::none();
                }
                let (id, open, tab) = (prompt.id.clone(), prompt.open, prompt.tab);
                let passphrase = prompt.passphrase.clone();
                match self.devices.unlock(id, passphrase, open, tab) {
                    Some(work) => {
                        if let Some(prompt) = &mut self.drive_unlock {
                            prompt.running = true;
                            prompt.error = None;
                        }
                        Task::batch([self.broadcast_devices(), Task::perform(work, Message::DeviceDone)])
                    }
                    None => Task::none(),
                }
            }
        }
    }

    pub(super) fn open_connect(&mut self) -> Task<Message> {
        self.connecting = Some(Connecting { dialog: ConnectDialog::default(), attempt: None });
        iced::widget::operation::focus(address_id())
    }

    pub(super) fn connect_update(&mut self, message: ConnectMessage) -> Task<Message> {
        let Some(open) = &mut self.connecting else { return Task::none() };
        let start = match message {
            ConnectMessage::Address(text) => {
                open.dialog.address = text;
                open.dialog.error = None;
                None
            }
            ConnectMessage::User(text) => {
                open.dialog.user = text;
                None
            }
            ConnectMessage::Domain(text) => {
                open.dialog.domain = text;
                None
            }
            ConnectMessage::Password(secret) => {
                open.dialog.password = secret;
                None
            }
            ConnectMessage::Submit => open.dialog.submit(self.devices.gvfs()),
            ConnectMessage::Choose(choice) => open.dialog.choose(choice),
            ConnectMessage::Cancel => {
                if open.attempt.is_some() {
                    // Stop the attempt, keep the dialog: what was typed
                    // is still worth having.
                    open.attempt = None;
                    open.dialog.cancelled();
                } else {
                    self.connecting = None;
                }
                return Task::none();
            }
            ConnectMessage::Ended(result) => {
                open.attempt = None;
                let ended = open.dialog.ended(result);
                let asks_login = matches!(open.dialog.phase, Phase::Login(_));
                let Some(path) = ended else {
                    return if asks_login { iced::widget::operation::focus(password_id()) } else { Task::none() };
                };
                self.connecting = None;
                // The share list changes by gvfs's own signal too; this
                // look makes the row arrive with the folder rather than a
                // moment after it.
                let shares = self.devices.backends.shares.clone();
                let refresh = Task::perform(async move { shares.shares().await }, |s| Message::Devices(Event::Shares(s)));
                let go = match path {
                    Some(path) => {
                        let at = self.focused();
                        let outcome = self.pane_mut(at).browser.update(BrowserMessage::Navigate(path));
                        self.handle_outcome(at, outcome)
                    }
                    None => {
                        self.status = Some("Connected. This server has no folder to show on its own.".to_string());
                        Task::none()
                    }
                };
                return Task::batch([refresh, go]);
            }
        };
        let Some((uri, answers)) = start else { return Task::none() };
        let shares = self.devices.backends.shares.clone();
        let (task, handle) = Task::perform(async move { shares.connect(&uri, answers).await }, |result| {
            Message::Connect(ConnectMessage::Ended(result))
        })
        .abortable();
        if let Some(open) = &mut self.connecting {
            open.attempt = Some(handle.abort_on_drop());
        }
        task
    }
}

/// The passphrase prompt for an encrypted drive.
pub(super) fn drive_unlock_dialog<'a>(prompt: &'a DriveUnlock, scale: FontScale) -> Element<'a, Message> {
    let size = scale.apply(BASE_TEXT_SIZE);
    let body = if prompt.running {
        "Unlocking\u{2026}".to_string()
    } else {
        "This drive is encrypted. Enter its passphrase to open it.".to_string()
    };
    // `secure`, as every password field here is.
    let mut field = text_input("Passphrase", prompt.passphrase.expose()).id(passphrase_id()).secure(true).size(size).width(Length::Fill);
    if !prompt.running {
        field = field
            .on_input(|t| Message::DriveUnlock(DriveUnlockMessage::Passphrase(Secret::new(t))))
            .on_submit(Message::DriveUnlock(DriveUnlockMessage::Submit));
    }
    let mut extra = column![
        field,
        meta_text("Locked again when it is unmounted or ejected, and never written to disk.", BASE_TEXT_SIZE, scale),
    ]
    .spacing(spacing::SM);
    if let Some(error) = &prompt.error {
        extra = extra.push(
            scaled_text(error.clone(), BASE_TEXT_SIZE, scale).style(|_t: &iced::Theme| iced::widget::text::Style {
                color: Some(hyprforge_ui::theme::error()),
            }),
        );
    }
    let mut cancel = secondary_button("Cancel");
    let mut unlock = primary_button("Unlock");
    if !prompt.running {
        cancel = cancel.on_press(Message::DriveUnlock(DriveUnlockMessage::Cancel));
        if !prompt.passphrase.expose().is_empty() {
            unlock = unlock.on_press(Message::DriveUnlock(DriveUnlockMessage::Submit));
        }
    }
    let buttons = row![Space::new().width(Length::Fill), cancel, unlock].spacing(spacing::SM);
    dialog(format!("Unlock \u{201C}{}\u{201D}", prompt.label), body, Some(extra.into()), buttons.into(), scale)
}

/// The dialog: an address, and — when the server asks — a name and
/// password, or its question's choices.
pub(super) fn connect_dialog<'a>(open: &'a Connecting, gvfs: Option<&Gvfs>, scale: FontScale) -> Element<'a, Message> {
    let d = &open.dialog;
    let size = scale.apply(BASE_TEXT_SIZE);
    let absent = matches!(gvfs, Some(Gvfs::Absent(_)));
    let connecting = matches!(d.phase, Phase::Connecting);

    let mut address = text_input("smb://server/share", &d.address).id(address_id()).size(size).width(Length::Fill);
    if matches!(d.phase, Phase::Address) && !absent {
        address = address
            .on_input(|t| Message::Connect(ConnectMessage::Address(t)))
            .on_submit(Message::Connect(ConnectMessage::Submit));
    }
    let mut extra = column![address].spacing(spacing::SM);

    let body = match &d.phase {
        Phase::Address => hint(gvfs),
        Phase::Connecting => format!("Connecting to {}\u{2026}", d.address.trim()),
        Phase::Login(login) => login.message.clone(),
        Phase::Question(question) => question.message.clone(),
    };

    if let Phase::Login(login) = &d.phase {
        extra = extra.push(
            text_input("Name", &d.user)
                .on_input(|t| Message::Connect(ConnectMessage::User(t)))
                .size(size)
                .width(Length::Fill),
        );
        if login.asks_domain {
            extra = extra.push(
                text_input("Domain", &d.domain)
                    .on_input(|t| Message::Connect(ConnectMessage::Domain(t)))
                    .size(size)
                    .width(Length::Fill),
            );
        }
        // `secure`: a password drawn on screen is one shown to whoever
        // is behind you — the archive prompt's rule.
        extra = extra.push(
            text_input("Password", d.password.expose())
                .id(password_id())
                .secure(true)
                .on_input(|t| Message::Connect(ConnectMessage::Password(Secret::new(t))))
                .on_submit(Message::Connect(ConnectMessage::Submit))
                .size(size)
                .width(Length::Fill),
        );
        extra = extra.push(meta_text(
            "Used for this connection only, and never written to disk.",
            BASE_TEXT_SIZE,
            scale,
        ));
    }

    if let Some(error) = &d.error {
        extra = extra.push(
            scaled_text(error.clone(), BASE_TEXT_SIZE, scale).style(|_t: &iced::Theme| iced::widget::text::Style {
                color: Some(hyprforge_ui::theme::error()),
            }),
        );
    }

    let mut buttons = row![Space::new().width(Length::Fill)].spacing(spacing::SM);
    buttons = buttons.push(
        secondary_button(if connecting { "Stop" } else if absent { "Close" } else { "Cancel" })
            .on_press(Message::Connect(ConnectMessage::Cancel)),
    );
    match &d.phase {
        Phase::Question(question) => {
            for (index, choice) in question.choices.iter().enumerate() {
                buttons = buttons.push(
                    primary_button(choice.clone()).on_press(Message::Connect(ConnectMessage::Choose(index))),
                );
            }
        }
        _ if !absent => {
            let mut connect = primary_button("Connect");
            if d.can_submit() {
                connect = connect.on_press(Message::Connect(ConnectMessage::Submit));
            }
            buttons = buttons.push(connect);
        }
        _ => {}
    }

    dialog("Connect to Server".to_string(), body, Some(extra.into()), buttons.into(), scale)
}

#[cfg(test)]
mod tests {
    use super::super::tests::app_for_test;
    use super::*;
    use hyprforge_files_core::devices::{DeviceMessage, Listing};
    use hyprforge_files_core::Action;
    use hyprforge_volumes::backend::mock::{MockShares, MockVolumes};
    use hyprforge_volumes::{Detach, Operation, Volume, VolumeError, VolumeId, VolumeKind};
    use std::path::Path;
    use std::sync::Arc;

    fn stick(label: &str, mounted: Option<&str>) -> Volume {
        Volume {
            id: VolumeId(format!("/b/{label}")),
            device: PathBuf::from(format!("/dev/{label}")),
            label: label.to_string(),
            size: 1,
            filesystem: Some("vfat".into()),
            mount_point: mounted.map(PathBuf::from),
            kind: VolumeKind::Removable,
            detach: Some(Detach::PowerOff),
            locked: false,
        }
    }

    fn app_with(volumes: Vec<Volume>) -> (App, Arc<MockVolumes>, Arc<MockShares>) {
        let mut app = app_for_test(&["/home/alex", "/home/alex"]);
        let mock = Arc::new(MockVolumes::new(volumes.clone()));
        let shares = Arc::new(MockShares::new(Gvfs::Available { schemes: vec!["smb".into(), "sftp".into()] }, vec![]));
        app.devices = hyprforge_files::devices::DeviceHost::new(
            hyprforge_files::devices::Backends { volumes: mock.clone(), shares: shares.clone() },
            true,
        );
        let _ = app.update(Message::Devices(Event::Gvfs(Gvfs::Available { schemes: vec!["smb".into(), "sftp".into()] })));
        let _ = app.update(Message::Devices(Event::Volumes(Ok(volumes))));
        (app, mock, shares)
    }

    /// Every tab draws the same drives, and a mount started from one is
    /// "Mounting…" in the other too.
    #[test]
    fn a_mount_from_one_tab_shows_in_every_tab() {
        let (mut app, _, _) = app_with(vec![stick("A", None)]);
        let _ = app.update(Message::Browser(BrowserMessage::Device(DeviceMessage::Open(VolumeId("/b/A".into())))));
        for tab in &app.tabs {
            let devices = tab.browser().devices();
            assert!(matches!(devices.volumes, Listing::Listed(ref v) if v.len() == 1));
            assert_eq!(devices.busy.get(&VolumeId("/b/A".into())), Some(&Operation::Mount));
        }
    }

    /// The mount finishes; the tab that clicked goes there and the other
    /// stays where it was.
    #[test]
    fn the_tab_that_clicked_goes_to_the_mounted_drive() {
        let (mut app, _, _) = app_with(vec![stick("A", None)]);
        app.active = 1;
        let _ = app.update(Message::Browser(BrowserMessage::Device(DeviceMessage::Open(VolumeId("/b/A".into())))));
        let _ = app.update(Message::DeviceDone(Done::Volume {
            id: VolumeId("/b/A".into()),
            op: Operation::Mount,
            result: Ok(Some("/run/media/alex/A".into())),
        }));
        assert_eq!(app.tabs[1].browser().current_dir(), Path::new("/run/media/alex/A"));
        assert_eq!(app.tabs[0].browser().current_dir(), Path::new("/home/alex"));
        assert!(app.status.is_none());
    }

    fn locked(label: &str) -> Volume {
        Volume { locked: true, filesystem: None, ..stick(label, None) }
    }

    fn type_passphrase(app: &mut App, passphrase: &str) {
        let _ = app.update(Message::DriveUnlock(DriveUnlockMessage::Passphrase(Secret::new(passphrase.to_string()))));
    }

    /// A click on a locked drive opens the prompt and starts nothing: no
    /// "Unlocking…" until there is a passphrase to unlock with, and an
    /// empty one is not sent.
    #[test]
    fn a_locked_drive_asks_for_its_passphrase_before_anything_starts() {
        let (mut app, mock, _) = app_with(vec![locked("SECRET")]);
        let _ = app.update(Message::Browser(BrowserMessage::Device(DeviceMessage::Open(VolumeId("/b/SECRET".into())))));
        let prompt = app.drive_unlock.as_ref().expect("a prompt opens");
        assert_eq!(prompt.label, "SECRET");
        assert!(prompt.open, "a click goes there once it opens");
        let _ = app.update(Message::DriveUnlock(DriveUnlockMessage::Submit));
        assert!(app.devices.devices().busy.is_empty(), "an empty passphrase is not sent");
        assert!(mock.calls().is_empty());
    }

    /// A wrong passphrase keeps the prompt, says so in it — not on the
    /// status line too — and drops what was typed; the right one closes
    /// it and opens the drive in the tab that clicked.
    #[test]
    fn a_wrong_passphrase_asks_again_and_the_right_one_opens_the_drive() {
        let (mut app, _, _) = app_with(vec![locked("SECRET")]);
        let id = VolumeId("/b/SECRET".into());
        let _ = app.update(Message::Browser(BrowserMessage::Device(DeviceMessage::Open(id.clone()))));
        type_passphrase(&mut app, "wrong");
        let _ = app.update(Message::DriveUnlock(DriveUnlockMessage::Submit));
        assert!(app.drive_unlock.as_ref().unwrap().running);
        assert_eq!(app.devices.devices().busy.get(&id), Some(&Operation::Unlock));
        let _ = app.update(Message::DeviceDone(Done::Volume {
            id: id.clone(),
            op: Operation::Unlock,
            result: Err(VolumeError::WrongPassphrase),
        }));
        let prompt = app.drive_unlock.as_ref().expect("still asking");
        assert!(!prompt.running);
        assert!(prompt.passphrase.expose().is_empty(), "the refused passphrase is not offered back");
        assert!(prompt.error.as_deref().unwrap().contains("didn't unlock"), "{:?}", prompt.error);
        assert!(app.status.is_none());

        type_passphrase(&mut app, "right");
        let _ = app.update(Message::DriveUnlock(DriveUnlockMessage::Submit));
        let _ = app.update(Message::DeviceDone(Done::Volume {
            id,
            op: Operation::Unlock,
            result: Ok(Some("/run/media/alex/SECRET".into())),
        }));
        assert!(app.drive_unlock.is_none());
        assert_eq!(app.tabs[0].browser().current_dir(), Path::new("/run/media/alex/SECRET"));
    }

    /// Escape closes the prompt — but not while UDisks2 is part-way
    /// through, which nothing here can stop.
    #[test]
    fn escape_closes_the_prompt_unless_an_unlock_is_running() {
        let (mut app, _, _) = app_with(vec![locked("SECRET")]);
        let _ = app.update(Message::Browser(BrowserMessage::Device(DeviceMessage::Open(VolumeId("/b/SECRET".into())))));
        type_passphrase(&mut app, "pass");
        let _ = app.update(Message::DriveUnlock(DriveUnlockMessage::Submit));
        let _ = app.update(Message::EscapeInField);
        assert!(app.drive_unlock.is_some(), "an unlock is running");
        let _ = app.update(Message::DeviceDone(Done::Volume {
            id: VolumeId("/b/SECRET".into()),
            op: Operation::Unlock,
            result: Err(VolumeError::WrongPassphrase),
        }));
        let _ = app.update(Message::EscapeInField);
        assert!(app.drive_unlock.is_none());
    }

    #[test]
    fn a_refused_unmount_is_a_sentence_on_the_status_line() {
        let (mut app, _, _) = app_with(vec![stick("A", Some("/run/media/alex/A"))]);
        let _ = app.update(Message::DeviceDone(Done::Volume {
            id: VolumeId("/b/A".into()),
            op: Operation::Unmount,
            result: Err(VolumeError::Busy("target is busy".into())),
        }));
        assert!(app.status.as_deref().unwrap().contains("in use"), "{:?}", app.status);
    }

    /// Ejecting the drive a tab is standing on sends that tab home, and
    /// leaves the others alone.
    #[test]
    fn ejecting_moves_tabs_off_the_drive() {
        let (mut app, _, _) = app_with(vec![stick("A", Some("/run/media/alex/A"))]);
        let _ = app.update(Message::Browser(BrowserMessage::Navigate("/run/media/alex/A/docs".into())));
        let _ = app.update(Message::DeviceDone(Done::Volume {
            id: VolumeId("/b/A".into()),
            op: Operation::Eject,
            result: Ok(None),
        }));
        assert_eq!(app.tabs[0].browser().current_dir(), Path::new("/home/alex"));
        assert!(app.status.as_deref().unwrap().contains("can be removed"));
    }

    /// Pulled out, not ejected: the tab on it still goes home.
    #[test]
    fn a_drive_that_vanishes_takes_its_tabs_home() {
        let (mut app, _, _) = app_with(vec![stick("A", Some("/run/media/alex/A"))]);
        let _ = app.update(Message::Browser(BrowserMessage::Navigate("/run/media/alex/A".into())));
        let _ = app.update(Message::Devices(Event::Volumes(Ok(vec![]))));
        assert_eq!(app.tabs[0].browser().current_dir(), Path::new("/home/alex"));
        assert_eq!(app.tabs[1].browser().current_dir(), Path::new("/home/alex"));
    }

    #[test]
    fn connect_to_server_opens_from_the_window_action() {
        let (mut app, _, _) = app_with(vec![]);
        let _ = app.update(Message::Browser(BrowserMessage::Perform(Action::ConnectToServer)));
        assert!(app.connecting.is_some());
        let _ = app.update(Message::Connect(ConnectMessage::Address("smb://nas/music".into())));
        let _ = app.update(Message::Connect(ConnectMessage::Submit));
        let open = app.connecting.as_ref().unwrap();
        assert_eq!(open.dialog.phase, Phase::Connecting);
        assert!(open.attempt.is_some());
    }

    /// Stop while connecting ends the attempt and keeps what was typed;
    /// a second Cancel closes the dialog.
    #[test]
    fn stopping_an_attempt_keeps_the_dialog_and_cancel_then_closes_it() {
        let (mut app, _, _) = app_with(vec![]);
        let _ = app.open_connect();
        let _ = app.update(Message::Connect(ConnectMessage::Address("smb://nas/music".into())));
        let _ = app.update(Message::Connect(ConnectMessage::Submit));
        let _ = app.update(Message::Connect(ConnectMessage::Cancel));
        let open = app.connecting.as_ref().expect("still open");
        assert_eq!(open.dialog.phase, Phase::Address);
        assert_eq!(open.dialog.address, "smb://nas/music");
        let _ = app.update(Message::Connect(ConnectMessage::Cancel));
        assert!(app.connecting.is_none());
    }

    #[test]
    fn a_connected_share_opens_in_the_active_tab() {
        let (mut app, _, _) = app_with(vec![]);
        let _ = app.open_connect();
        let _ = app.update(Message::Connect(ConnectMessage::Address("sftp://box".into())));
        let _ = app.update(Message::Connect(ConnectMessage::Submit));
        let _ = app.update(Message::Connect(ConnectMessage::Ended(Ok(Some("/run/user/1/gvfs/sftp:host=box".into())))));
        assert!(app.connecting.is_none());
        assert_eq!(app.active_tab().browser().current_dir(), Path::new("/run/user/1/gvfs/sftp:host=box"));
    }
}
