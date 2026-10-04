//! `hyprforge-files-portal`: Files as the desktop's open/save dialog.
//!
//! Two jobs in one binary, chosen by the first argument:
//!
//! - `dialog` — one dialog window. Reads a [`portal::Request`] as JSON on
//!   stdin, shows the Files browser in `Mode::Dialog`, and writes a
//!   [`portal::Answer`] as JSON on stdout when it closes. Runnable on its
//!   own, which is how it is looked at in the nested compositor.
//! - no argument — the D-Bus service the portal frontend talks to, which
//!   starts one `dialog` per request. See `portal.rs` for the interface.
//!
//! A process per dialog rather than windows in one process: a dialog that
//! crashes takes down that one request and not every application's, two
//! applications can each have a dialog open, and the service holds no
//! window state at all.
//!
//! The dialog is the browser, not a copy of it. What is here is only what
//! a dialog has and the window does not — the name field, the filter and
//! the application's choices, and the two buttons — drawn *around* the
//! browser's own view, never inside it.

use hyprforge_files::host::{build_preview, count_folders, read_dir_task, resolve_icons, thumbnail_stream};
use hyprforge_files::portal::{self, Accept, Answer, Kind, OnScreen, Request};
use hyprforge_files_core::browser::Message as BrowserMessage;
use hyprforge_files_core::keymap::Resolved;
use hyprforge_files_core::{
    sidebar, xdg_user_dirs, Action, Browser, Click, ClickTracker, DialogKind, DirError, Entry, EntryFilter, FsBackend, Mode, Outcome,
    RoutingBackend, Scope,
};
use hyprforge_ui::theme::{app_theme, spacing, FontScale};
use hyprforge_ui::widgets::{dropdown_menu_style, dropdown_style, inset_input_style, primary_button, secondary_button};
use iced::keyboard;
use iced::widget::{column, container, pick_list, row, text, text_input};
use iced::{window, Element, Length, Size, Subscription, Task, Theme};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Distinct from Files' own, so a rule can tell the dialog from the
/// window — and so Hyprland's "a window of fixed size is a dialog"
/// floats this one without floating every Files window too.
const APP_ID: &str = "hyprforge-files-portal";

/// The size the dialog opens at: wide enough for the sidebar, the
/// listing and a preview, and small enough to read as a dialog.
const DIALOG_SIZE: Size = Size::new(960.0, 620.0);
const MIN_SIZE: Size = Size::new(560.0, 380.0);

fn main() {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .init();
    match std::env::args().nth(1).as_deref() {
        Some("dialog") => {
            let answer = run_dialog();
            let json = serde_json::to_string(&answer).unwrap_or_else(|_| "\"Cancelled\"".to_string());
            let mut out = std::io::stdout().lock();
            let _ = writeln!(out, "{json}");
        }
        Some(other) => {
            eprintln!("hyprforge-files-portal: unknown argument {other:?}; run with no argument for the service, or `dialog`");
            std::process::exit(2);
        }
        None => {
            let served = hyprforge_files::portal_service::DialogCommand::this_binary()
                .map_err(|e| e.to_string())
                .and_then(|dialog| {
                    let runtime = tokio::runtime::Runtime::new().map_err(|e| e.to_string())?;
                    runtime.block_on(hyprforge_files::portal_service::serve(dialog)).map_err(|e| e.to_string())
                });
            if let Err(e) = served {
                eprintln!("hyprforge-files-portal: the file chooser service could not start: {e}");
                std::process::exit(1);
            }
        }
    }
}

/// Reads the request, shows the dialog, and returns how it ended.
fn run_dialog() -> Answer {
    let mut raw = String::new();
    if let Err(e) = std::io::stdin().read_to_string(&mut raw) {
        return Answer::Failed(format!("the request could not be read: {e}"));
    }
    let request: Request = match serde_json::from_str(&raw) {
        Ok(request) => request,
        Err(e) => return Answer::Failed(format!("the request is not one this dialog understands: {e}")),
    };
    hyprforge_ui::theme::init(hyprforge_appearance::look::resolve());

    let backend: Arc<dyn FsBackend> = Arc::new(RoutingBackend::default());
    let home = backend.home_dir();
    // Where this application's last dialog was left. A file that cannot
    // be read is said and then left alone: `None` here means nothing is
    // saved over it when this dialog closes.
    let remembered = match portal::Remembered::load(&portal::remembered_path()) {
        Ok(remembered) => Some(remembered),
        Err(why) => {
            tracing::warn!(%why, "the dialog's remembered folders could not be read; they will not be updated");
            None
        }
    };
    let last = remembered.as_ref().and_then(|r| r.folder_for(&request.app_id));
    let start = request.starting_folder(last.as_deref(), &home);
    let (mut config, _problems) = hyprforge_files_core::config::load();
    // The dialog's own short menus — see `MenuConfig::dialog`. The key
    // bindings stay the user's.
    config.menus = hyprforge_files_core::menu::MenuConfig::dialog();
    // Read, never written: the dialog follows the view the user set up in
    // Files, and changing the view in a dialog is not a reason to rewrite
    // the window's settings file under it.
    let prefs = hyprforge_files_core::prefs::load().unwrap_or_default();
    let user_dirs = xdg_user_dirs::load(&hyprforge_paths::config_home(), &home);
    let places = sidebar::build(backend.as_ref(), &user_dirs, &config.sidebar.places);
    let kind = match request.kind {
        Kind::Save { .. } | Kind::SaveFiles { .. } => DialogKind::Save,
        Kind::Open { .. } => DialogKind::Open,
    };
    let (mut browser, first) = Browser::new(Mode::Dialog(kind), prefs.clone(), start, places);
    browser.set_config(Arc::new(config.clone()));

    let answer = Arc::new(std::sync::Mutex::new(Answer::Cancelled));
    let mut dialog = Dialog {
        name: request.suggested_name().unwrap_or_default(),
        filter: request
            .current_filter
            .as_ref()
            .and_then(|current| request.filters.iter().position(|f| f.name == current.name))
            .or_else(|| (!request.filters.is_empty()).then_some(0)),
        choices: request.choices.iter().map(|c| (c.id.clone(), c.initial.clone())).collect(),
        request,
        browser,
        backend,
        mime: Arc::new(hyprforge_mime::MimeDb::load()),
        config: Arc::new(config),
        font_scale: FontScale(hyprforge_ui::theme::active().font_scale),
        width: DIALOG_SIZE.width,
        height: DIALOG_SIZE.height,
        status: None,
        overwrite: None,
        resolving: false,
        resolve_next: None,
        searcher: Default::default(),
        answer: answer.clone(),
        remembered,
        clicks: ClickTracker::new(),
        modifiers: keyboard::Modifiers::default(),
        // Drives and shares, so a stick can be saved to. No Connect to
        // Server: a file chooser does not change the session's mounts.
        devices: hyprforge_files::devices::DeviceHost::new(hyprforge_files::devices::Backends::system(), false),
    };
    dialog.apply_filter();
    let boot = Task::batch([dialog.handle(first), Task::done(Message::Settle)]);
    let boot = std::cell::RefCell::new(Some((dialog, boot)));

    let title = boot.borrow().as_ref().map(|(d, _)| d.request.title.clone()).unwrap_or_default();
    let result = iced::application(
        move || boot.borrow_mut().take().expect("the dialog boots once"),
        Dialog::update,
        Dialog::view,
    )
    .title(move |_: &Dialog| title.clone())
    .theme(|_: &Dialog| -> Theme { app_theme() })
    .subscription(Dialog::subscription)
    .window(window::Settings {
        size: DIALOG_SIZE,
        // Minimum equal to maximum: Hyprland floats a window of fixed size
        // as it maps — it reads as a dialog — and centres it, so the
        // dialog never appears tiled and then jumps. `Settle` lets it be
        // resized once it is up. The same arrangement `hyprforge-media`'s
        // `float.rs` arrived at for a picture opened on its own.
        min_size: Some(DIALOG_SIZE),
        max_size: Some(DIALOG_SIZE),
        platform_specific: window::settings::PlatformSpecific {
            application_id: APP_ID.to_string(),
            ..window::settings::PlatformSpecific::default()
        },
        ..window::Settings::default()
    })
    .run();
    if let Err(e) = result {
        return Answer::Failed(format!("the dialog window could not open: {e}"));
    }
    let chosen = std::mem::replace(&mut *answer.lock().unwrap_or_else(|p| p.into_inner()), Answer::Cancelled);
    chosen
}

struct Dialog {
    request: Request,
    browser: Browser,
    backend: Arc<dyn FsBackend>,
    mime: Arc<hyprforge_mime::MimeDb>,
    config: Arc<hyprforge_files_core::config::Config>,
    font_scale: FontScale,
    width: f32,
    height: f32,
    /// The Save dialog's name field.
    name: String,
    /// Which of the request's filters applies, by index.
    filter: Option<usize>,
    /// `(choice id, value)` for each of the application's choices.
    choices: Vec<(String, String)>,
    status: Option<String>,
    /// A file a Save would replace, waiting for a yes — and the whole
    /// answer a yes gives.
    overwrite: Option<(PathBuf, Vec<PathBuf>)>,
    /// One path-bar resolve out at a time, the newest waiting — the same
    /// coalescing the Files window does.
    resolving: bool,
    resolve_next: Option<hyprforge_files_core::jump::Request>,
    /// Searches below a folder, one walk at a time — the Files window's
    /// arrangement (`hyprforge_files::search_jobs`).
    searcher: hyprforge_files::search_jobs::Searcher,
    /// Where the answer goes when the window closes.
    answer: Arc<std::sync::Mutex<Answer>>,
    /// Each application's last folder — `None` when the file could not
    /// be read, so it is not saved over.
    remembered: Option<portal::Remembered>,
    /// A click arrives from the view bare — no modifiers, no notion of
    /// being the second of a pair — and the window supplies both, as the
    /// Files window does.
    clicks: ClickTracker,
    modifiers: keyboard::Modifiers,
    /// Drives and network shares — the Files window's arrangement
    /// (`hyprforge_files::devices`), without Connect to Server.
    devices: hyprforge_files::devices::DeviceHost,
}

#[derive(Debug, Clone)]
enum Message {
    Browser(BrowserMessage),
    DirLoaded(PathBuf, Result<Vec<Entry>, DirError>),
    PathResolved(String, Vec<hyprforge_files_core::jump::Candidate>),
    Searched(hyprforge_files::search_jobs::Event),
    /// A saved search was written, or could not be.
    SearchesSaved(Result<(), String>),
    KeyPressed(hyprforge_files_core::keymap::KeyPress),
    ModifiersChanged(keyboard::Modifiers),
    EscapeInField,
    NameChanged(String),
    NameSubmitted,
    FilterChosen(Named),
    ChoiceChosen(usize, Named),
    Accept,
    Cancel,
    OverwriteAnswered(bool),
    FolderMade(PathBuf, Result<(), String>),
    Renamed(PathBuf, Result<(), String>),
    Resized(Size),
    /// The window has mapped: let it be resized.
    Settle,
    /// What the drives-and-shares watch heard.
    Devices(hyprforge_files::devices::Event),
    /// A mount the dialog asked for finished.
    DeviceDone(hyprforge_files::devices::Done),
}

/// An entry in a dropdown: shown by name, found again by index.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Named {
    index: usize,
    name: String,
}

impl std::fmt::Display for Named {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.name)
    }
}

impl Dialog {
    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::ModifiersChanged(modifiers) => {
                self.modifiers = modifiers;
                Task::none()
            }
            Message::Browser(inner) => {
                let inner = match inner {
                    BrowserMessage::EntryClicked { index, .. } => {
                        let (ctrl, shift) = (self.modifiers.control(), self.modifiers.shift());
                        if ctrl || shift {
                            self.clicks.reset();
                            BrowserMessage::EntryClicked { index, ctrl, shift }
                        } else {
                            match self.clicks.press(index, std::time::Instant::now()) {
                                Click::Double => BrowserMessage::EntryActivated(index),
                                Click::Single => BrowserMessage::EntryClicked { index, ctrl, shift },
                            }
                        }
                    }
                    // A right press carries no position; the pointer
                    // tracker has it.
                    BrowserMessage::OpenContextMenu { spot, .. } => {
                        self.clicks.reset();
                        BrowserMessage::OpenContextMenu { spot, at: pointer::last() }
                    }
                    other => other,
                };
                let outcome = self.browser.update(inner);
                if outcome.navigates() {
                    self.clicks.reset();
                }
                self.handle(outcome)
            }
            Message::DirLoaded(path, result) => {
                let archive = hyprforge_files_core::archive::split(&path)
                    .and_then(|(archive, _)| hyprforge_archive::Format::by_name(&archive))
                    .map(|format| format.label().to_string());
                if path == self.browser.current_dir() {
                    self.browser.set_archive(archive);
                }
                let outcome = self.browser.update(BrowserMessage::DirLoaded(path, result));
                self.handle(outcome)
            }
            Message::PathResolved(text, candidates) => {
                self.resolving = false;
                let outcome = self.browser.update(BrowserMessage::PathResolved { text: text.clone(), candidates });
                let next = match self.resolve_next.take() {
                    Some(next) if next.text != text => self.resolve(next),
                    _ => Task::none(),
                };
                Task::batch([self.handle(outcome), next])
            }
            Message::Searched(event) => {
                use hyprforge_files::search_jobs::Event;
                use hyprforge_files_core::search::SearchMessage;
                let (message, ended) = match event {
                    Event::Found(run, entries) => (SearchMessage::Found { run, entries }, None),
                    Event::Finished(run, summary) => (SearchMessage::Finished { run, summary }, Some(run)),
                };
                let outcome = self.browser.update(BrowserMessage::Search(message));
                let task = self.handle(outcome);
                let next = match ended.and_then(|run| self.searcher.finished(run)) {
                    Some(start) => self.spawn_search(start),
                    None => Task::none(),
                };
                Task::batch([task, next])
            }
            Message::SearchesSaved(result) => {
                if let Err(why) = result {
                    self.status = Some(why);
                }
                Task::none()
            }
            Message::KeyPressed(press) => self.key(press),
            Message::EscapeInField => {
                let renamed = self.browser.update(BrowserMessage::RenameCancel);
                let pathed = self.browser.update(BrowserMessage::PathCancel);
                let paletted = self.browser.update(BrowserMessage::PaletteCancel);
                let unsaved = self.browser.update(BrowserMessage::Search(
                    hyprforge_files_core::search::SearchMessage::SaveCancel,
                ));
                self.handle(Outcome::Many(vec![renamed, pathed, paletted, unsaved]))
            }
            Message::NameChanged(name) => {
                self.name = name;
                self.overwrite = None;
                Task::none()
            }
            Message::NameSubmitted | Message::Accept => self.accept(),
            Message::FilterChosen(named) => {
                self.filter = Some(named.index);
                self.apply_filter();
                Task::none()
            }
            Message::ChoiceChosen(choice, named) => {
                if let (Some(slot), Some(spec)) = (self.choices.get_mut(choice), self.request.choices.get(choice)) {
                    slot.1 = if spec.options.is_empty() {
                        // A checkbox: its two values, by the spec.
                        if named.index == 1 { "true" } else { "false" }.to_string()
                    } else {
                        spec.options.get(named.index).map(|(id, _)| id.clone()).unwrap_or_default()
                    };
                }
                Task::none()
            }
            Message::Cancel => self.finish(Answer::Cancelled),
            Message::OverwriteAnswered(yes) => match self.overwrite.take() {
                Some((_, answer)) if yes => self.finish_with(answer),
                _ => Task::none(),
            },
            Message::FolderMade(path, result) | Message::Renamed(path, result) => {
                match result {
                    Ok(()) => {
                        self.browser.update(BrowserMessage::AfterListing { path, rename: false });
                    }
                    Err(why) => self.status = Some(why),
                }
                let dir = self.browser.current_dir().to_path_buf();
                self.read(dir)
            }
            Message::Resized(size) => {
                self.width = size.width;
                self.height = size.height;
                Task::none()
            }
            Message::Devices(event) => {
                if !self.devices.heard(event) {
                    return Task::none();
                }
                let mut outcome = self.browser.update(BrowserMessage::DevicesChanged(self.devices.snapshot()));
                // Standing on a stick that was just pulled out: the Files
                // window's rule, home.
                let gone = self.devices.take_gone();
                if gone.iter().any(|g| self.browser.current_dir().starts_with(g)) {
                    let home = self.backend.home_dir();
                    outcome = hyprforge_files_core::Outcome::Many(vec![
                        outcome,
                        self.browser.update(BrowserMessage::Navigate(home)),
                    ]);
                }
                self.handle(outcome)
            }
            Message::DeviceDone(done) => {
                let finished = self.devices.done(done);
                let mut outcome = self.browser.update(BrowserMessage::DevicesChanged(self.devices.snapshot()));
                if let Some(status) = finished.status {
                    self.status = Some(status);
                }
                if let Some((_, point)) = finished.open {
                    outcome = hyprforge_files_core::Outcome::Many(vec![
                        outcome,
                        self.browser.update(BrowserMessage::Navigate(point)),
                    ]);
                }
                self.handle(outcome)
            }
            Message::Settle => window::latest().and_then(|id| {
                Task::batch([
                    window::set_resizable(id, true),
                    window::set_min_size(id, Some(MIN_SIZE)),
                    window::set_max_size(id, None),
                ])
            }),
        }
    }

    /// Carries out what the browser asked for. The reading is the same
    /// as the Files window's (`hyprforge_files::host`); the file
    /// operations a dialog has no business doing are declined, with a
    /// sentence, rather than ignored.
    fn handle(&mut self, outcome: Outcome) -> Task<Message> {
        match outcome {
            Outcome::None => Task::none(),
            Outcome::ReadDir(path) => self.read(path),
            Outcome::Activated(path) => self.activated(path),
            Outcome::CountFolders(folders) => Task::perform(count_folders(self.backend.clone(), folders), |counts| {
                Message::Browser(BrowserMessage::CountsLoaded(counts))
            }),
            // Column view's panes: each its own read, so the nearest folder
            // fills in without waiting on the others. The browser drops an
            // answer for a pane it no longer shows.
            Outcome::ReadColumns(dirs) => Task::batch(dirs.into_iter().map(|dir| {
                Task::perform(read_dir_task(self.backend.clone(), dir.clone()), move |result| {
                    Message::Browser(BrowserMessage::ColumnLoaded(dir.clone(), result))
                })
            })),
            Outcome::RevealFocused => iced::advanced::widget::operate(hyprforge_files_core::reveal::Reveal::new()),
            Outcome::SnapTo { id, y } => {
                iced::widget::operation::snap_to(id, iced::widget::operation::RelativeOffset { x: None, y: Some(y) })
            }
            Outcome::LoadThumbnails(paths) => Task::run(thumbnail_stream(paths), |(path, picture)| {
                Message::Browser(BrowserMessage::ThumbnailLoaded(path, picture))
            }),
            Outcome::LoadPreview(path) => Task::perform(
                build_preview(path, self.mime.clone(), self.backend.clone()),
                |(path, preview)| Message::Browser(BrowserMessage::PreviewLoaded(path, preview)),
            ),
            Outcome::LoadIcons(keys) => Task::perform(resolve_icons(keys, self.mime.clone()), |icons| {
                Message::Browser(BrowserMessage::IconsLoaded(icons))
            }),
            Outcome::ResolvePath(request) => self.resolve(request),
            Outcome::FocusPath { id, select_all } => Task::batch([
                iced::widget::operation::focus(id.clone()),
                if select_all {
                    iced::widget::operation::select_all(id)
                } else {
                    iced::widget::operation::move_cursor_to_end(id)
                },
            ]),
            Outcome::FocusRename { id, select } => Task::batch([
                iced::widget::operation::focus(id.clone()),
                iced::widget::operation::select_range(id, 0, select),
            ]),
            // A Save dialog needs New Folder; GTK's has it too.
            Outcome::CreateFolder(path) => Task::perform(
                async move {
                    let target = path.clone();
                    let result = tokio::task::spawn_blocking(move || hyprforge_files::jobs::create_folder(&target))
                        .await
                        .unwrap_or_else(|e| Err(format!("Making the folder was interrupted: {e}")));
                    (path, result)
                },
                |(path, result)| Message::FolderMade(path, result),
            ),
            Outcome::Rename { from, to } if hyprforge_files_core::archive::split(&from).is_none() => Task::perform(
                async move {
                    let (source, target) = (from, to.clone());
                    let result = tokio::task::spawn_blocking(move || hyprforge_files::jobs::rename(&source, &target))
                        .await
                        .unwrap_or_else(|e| Err(format!("Renaming was interrupted: {e}")));
                    (to, result)
                },
                |(to, result)| Message::Renamed(to, result),
            ),
            Outcome::Notice(text) => {
                self.status = Some(text);
                Task::none()
            }
            Outcome::CopyText(text) => iced::clipboard::write(text),
            Outcome::Many(all) => Task::batch(all.into_iter().map(|o| self.handle(o)).collect::<Vec<_>>()),
            // The view's own settings follow Files' and are not written
            // from here — see `run_dialog`.
            Outcome::OpenContextMenuAtPointer(spot) => {
                let outcome = self.browser.update(BrowserMessage::OpenContextMenu { spot, at: pointer::last() });
                self.handle(outcome)
            }
            Outcome::PrefsChanged(_) | Outcome::Pins(_) | Outcome::Window(_) => Task::none(),
            // Only a click on a drive reaches here — the dialog's menus
            // offer no drive actions — and it mounts, so the stick can
            // be opened and saved to.
            Outcome::Devices(ask) => match self.devices.ask(ask, 0) {
                Some(work) => {
                    let outcome = self.browser.update(BrowserMessage::DevicesChanged(self.devices.snapshot()));
                    Task::batch([self.handle(outcome), Task::perform(work, Message::DeviceDone)])
                }
                None => Task::none(),
            },
            // A search somebody chose to save is saved from here too, by
            // the same read-modify-write the window uses, so a search
            // saved in a file chooser is in Files' sidebar next time.
            Outcome::Search(hyprforge_files_core::search::Ask::Smart(change)) => {
                // The browser has already applied it to its own list.
                Task::perform(
                    async move {
                        tokio::task::spawn_blocking(move || {
                            hyprforge_files_core::prefs::update(|p| {
                                p.searches = hyprforge_files_core::search::apply_smart_change(&p.searches, &change)
                            })
                            .map(|_| ())
                            .map_err(|e| e.to_string())
                        })
                        .await
                        .unwrap_or_else(|e| Err(format!("Saving the search was interrupted: {e}")))
                    },
                    Message::SearchesSaved,
                )
            }
            Outcome::Search(ask) => match self.searcher.ask(ask) {
                Some(start) => self.spawn_search(start),
                None => Task::none(),
            },
            _ => {
                self.status = Some("That isn't something this dialog does — open Files for it.".to_string());
                Task::none()
            }
        }
    }

    fn spawn_search(&self, (request, cancel): hyprforge_files::search_jobs::Start) -> Task<Message> {
        Task::run(hyprforge_files::search_jobs::stream(self.backend.clone(), request, cancel), Message::Searched)
    }

    fn read(&self, path: PathBuf) -> Task<Message> {
        Task::perform(read_dir_task(self.backend.clone(), path.clone()), move |result| {
            Message::DirLoaded(path.clone(), result)
        })
    }

    fn resolve(&mut self, request: hyprforge_files_core::jump::Request) -> Task<Message> {
        if self.resolving {
            self.resolve_next = Some(request);
            return Task::none();
        }
        self.resolving = true;
        let backend = self.backend.clone();
        let text = request.text.clone();
        Task::perform(
            async move {
                tokio::task::spawn_blocking(move || request.run(backend.as_ref())).await.unwrap_or_default()
            },
            move |candidates| Message::PathResolved(text.clone(), candidates),
        )
    }

    /// A double click or Enter on a file. In an Open dialog that is the
    /// choice; in a Save dialog it names the file to save over, in the
    /// name field, for accept to ask about.
    fn activated(&mut self, path: PathBuf) -> Task<Message> {
        match &self.request.kind {
            Kind::Open { directory: false, .. } => self.accept(),
            Kind::Save { .. } => {
                if let Some(name) = path.file_name() {
                    self.name = name.to_string_lossy().into_owned();
                }
                Task::none()
            }
            _ => Task::none(),
        }
    }

    fn key(&mut self, press: hyprforge_files_core::keymap::KeyPress) -> Task<Message> {
        use hyprforge_files_core::keymap::Key;
        if self.overwrite.is_some() {
            return match press.key {
                Key::Escape => self.update(Message::OverwriteAnswered(false)),
                _ => Task::none(),
            };
        }
        if press.key == Key::Tab && self.browser.editing_path() {
            let outcome = self.browser.update(BrowserMessage::PathComplete);
            return self.handle(outcome);
        }
        // Escape with nothing to back out of closes the dialog, the way
        // every dialog does. With a search typed, it clears the search.
        if press.key == Key::Escape && self.browser.search_query().is_empty() && !self.browser.menu_open() {
            return self.finish(Answer::Cancelled);
        }
        match self.config.keymap.resolve(&press) {
            Some(Resolved::Action(action)) if action.scope() == Scope::Window => Task::none(),
            // Enter on a file in an Open dialog is accept; on a folder it
            // goes in, which `Open` already does.
            Some(Resolved::Action(Action::Open)) => {
                let outcome = self.browser.perform(Action::Open);
                self.handle(outcome)
            }
            Some(Resolved::Action(action)) => {
                let outcome = self.browser.perform(action);
                self.handle(outcome)
            }
            Some(Resolved::Text(c)) => {
                let outcome = self.browser.update(BrowserMessage::TypeToSearch(c));
                self.handle(outcome)
            }
            None => Task::none(),
        }
    }

    fn accept(&mut self) -> Task<Message> {
        let folder = self.browser.current_dir().to_path_buf();
        let real = hyprforge_files_core::archive::split(&folder).is_none() && folder != sidebar::trash_path();
        let focused = self.browser.selection().focused().map(Path::to_path_buf);
        let mut selected: Vec<(PathBuf, bool)> = self
            .browser
            .rows()
            .into_iter()
            .filter(|e| self.browser.selection().is_selected(&e.path))
            .map(|e| (e.path.clone(), e.is_dir))
            .collect();
        selected.sort_by_key(|(p, _)| Some(p) != focused.as_ref());
        let screen = OnScreen { folder: &folder, real, selected: &selected, name: &self.name };
        let exists = |p: &Path| std::fs::metadata(p).ok().map(|m| m.is_dir());
        match portal::accept(&self.request.kind, &screen, exists) {
            Accept::Answer(paths) => self.finish_with(paths),
            Accept::Enter(dir) => {
                if matches!(self.request.kind, Kind::Save { .. }) {
                    self.name.clear();
                }
                let outcome = self.browser.update(BrowserMessage::Navigate(dir));
                self.handle(outcome)
            }
            Accept::Overwrite { clash, answer } => {
                self.overwrite = Some((clash, answer));
                Task::none()
            }
            Accept::Refuse(why) => {
                self.status = Some(why);
                Task::none()
            }
            Accept::Nothing => Task::none(),
        }
    }

    fn finish_with(&mut self, paths: Vec<PathBuf>) -> Task<Message> {
        let filter = self.filter.and_then(|i| self.request.filters.get(i)).cloned();
        self.finish(Answer::Chosen { paths, choices: self.choices.clone(), filter })
    }

    fn finish(&mut self, answer: Answer) -> Task<Message> {
        // Remembered only when something was chosen: a cancelled dialog
        // was not left anywhere on purpose.
        //
        // Read again here rather than written from the copy loaded when
        // this dialog opened: two applications can each have a dialog up,
        // and the one closing second would otherwise put back what the
        // first had just changed. The file still is not written over if it
        // cannot be read now.
        if let (Answer::Chosen { .. }, Some(_)) = (&answer, self.remembered.as_ref()) {
            let folder = self.browser.current_dir().to_path_buf();
            if hyprforge_files_core::archive::split(&folder).is_none() {
                let path = portal::remembered_path();
                match portal::Remembered::load(&path) {
                    Ok(mut fresh) => {
                        fresh.remember(&self.request.app_id, &folder);
                        if let Err(e) = fresh.save(&path) {
                            tracing::warn!(error = %e, "the dialog's last folder could not be saved");
                        }
                    }
                    Err(why) => tracing::warn!(%why, "the dialog's remembered folders could not be read; not saved over"),
                }
            }
        }
        *self.answer.lock().unwrap_or_else(|p| p.into_inner()) = answer;
        iced::exit()
    }

    /// Hands the chosen filter to the browser, as a predicate over a
    /// file's name — its type by name only, never by reading it, since
    /// this runs on every refresh of the listing.
    fn apply_filter(&mut self) {
        let Some(filter) = self.filter.and_then(|i| self.request.filters.get(i)).cloned() else {
            self.browser.set_entry_filter(None);
            return;
        };
        let mime = self.mime.clone();
        self.browser.set_entry_filter(Some(EntryFilter::new(move |entry: &Entry| {
            let kind = mime.type_of(Path::new(&entry.name));
            filter.accepts(&entry.name, kind, |child, parent| mime.is_subclass_of(child, parent))
        })));
    }

    fn view(&self) -> Element<'_, Message> {
        let scale = self.font_scale;
        let browser = self.browser.view(scale, self.width).map(Message::Browser);
        let mut bar = row![].spacing(spacing::SM).align_y(iced::Alignment::Center);
        if matches!(self.request.kind, Kind::Save { .. }) {
            bar = bar.push(text("Name"));
            bar = bar.push(
                text_input("File name", &self.name)
                    .on_input(Message::NameChanged)
                    .on_submit(Message::NameSubmitted)
                    .style(inset_input_style)
                    .width(Length::Fill),
            );
        } else {
            bar = bar.push(
                text(self.status.clone().unwrap_or_default())
                    .color(hyprforge_ui::theme::text_dim())
                    .width(Length::Fill),
            );
        }
        if !self.request.filters.is_empty() {
            let options: Vec<Named> = self
                .request
                .filters
                .iter()
                .enumerate()
                .map(|(index, f)| Named { index, name: f.name.clone() })
                .collect();
            let chosen = self.filter.and_then(|i| options.get(i).cloned());
            bar = bar.push(
                pick_list(options, chosen, Message::FilterChosen)
                    .style(dropdown_style)
                    .menu_style(dropdown_menu_style),
            );
        }
        for (index, choice) in self.request.choices.iter().enumerate() {
            let options: Vec<Named> = if choice.options.is_empty() {
                vec![
                    Named { index: 0, name: format!("{}: off", choice.label) },
                    Named { index: 1, name: format!("{}: on", choice.label) },
                ]
            } else {
                choice.options.iter().enumerate().map(|(i, (_, label))| Named { index: i, name: label.clone() }).collect()
            };
            let current = self.choices.get(index).map(|(_, v)| v.as_str()).unwrap_or_default();
            let chosen = if choice.options.is_empty() {
                options.get(usize::from(current == "true")).cloned()
            } else {
                choice.options.iter().position(|(id, _)| id == current).and_then(|i| options.get(i).cloned())
            };
            bar = bar.push(
                pick_list(options, chosen, move |named| Message::ChoiceChosen(index, named))
                    .style(dropdown_style)
                    .menu_style(dropdown_menu_style),
            );
        }
        let accept_label = self.request.accept_label.clone().unwrap_or_else(|| {
            match self.request.kind {
                Kind::Open { .. } => "Open",
                Kind::Save { .. } | Kind::SaveFiles { .. } => "Save",
            }
            .to_string()
        });
        bar = bar.push(secondary_button("Cancel").on_press(Message::Cancel));
        bar = bar.push(primary_button(accept_label).on_press(Message::Accept));

        let mut page = column![browser];
        if let Some((path, _)) = &self.overwrite {
            let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            page = page.push(
                container(
                    row![
                        text(format!("\u{201c}{name}\u{201d} already exists. Replace it?")).width(Length::Fill),
                        secondary_button("Keep it").on_press(Message::OverwriteAnswered(false)),
                        primary_button("Replace").on_press(Message::OverwriteAnswered(true)),
                    ]
                    .spacing(spacing::SM)
                    .align_y(iced::Alignment::Center),
                )
                .padding(spacing::SM),
            );
        } else if let (Some(status), Kind::Save { .. }) = (&self.status, &self.request.kind) {
            page = page.push(container(text(status.clone()).color(hyprforge_ui::theme::text_dim())).padding([0, spacing::SM as u16]));
        }
        page = page.push(container(bar).padding(spacing::SM));
        let window: Element<'_, Message> = container(page)
            .width(Length::Fill)
            .height(Length::Fill)
            .style(|_t: &Theme| container::Style {
                background: Some(iced::Background::Color(hyprforge_ui::theme::surface::root())),
                ..container::Style::default()
            })
            .into();
        match self.browser.menu_overlay(scale, (self.width, self.height)) {
            Some(overlay) => iced::widget::stack![window, overlay.map(Message::Browser)].into(),
            None => window,
        }
    }

    fn subscription(&self) -> Subscription<Message> {
        Subscription::batch([
            keyboard::listen().filter_map(|event| {
                if let keyboard::Event::ModifiersChanged(modifiers) = &event {
                    return Some(Message::ModifiersChanged(*modifiers));
                }
                hyprforge_ui::keys::key_press(&event).map(Message::KeyPressed)
            }),
            iced::event::listen_with(field_escape),
            window::resize_events().map(|(_, size)| Message::Resized(size)),
            pointer::track(),
            Subscription::run_with(self.devices.backends.clone(), hyprforge_files::devices::watch)
                .map(Message::Devices),
        ])
    }
}

/// Escape that a text field took — see the Files window's own
/// `field_escape` for why this is listened for.
fn field_escape(event: iced::Event, status: iced::event::Status, _window: window::Id) -> Option<Message> {
    match (event, status) {
        (
            iced::Event::Keyboard(keyboard::Event::KeyPressed { key: keyboard::Key::Named(keyboard::key::Named::Escape), .. }),
            iced::event::Status::Captured,
        ) => Some(Message::EscapeInField),
        _ => None,
    }
}

/// Where the pointer last was, recorded without a message per mouse move
/// — the Files window's own arrangement, for the same reason: a context
/// menu opens at the pointer and a right press carries no position, and
/// a message per move would rebuild the dialog on every one. One dialog
/// per process, so process-wide atomics are this window's.
mod pointer {
    use iced::{event, mouse, Event, Subscription};
    use std::sync::atomic::{AtomicU32, Ordering};

    static X: AtomicU32 = AtomicU32::new(0);
    static Y: AtomicU32 = AtomicU32::new(0);

    pub fn track<Message: 'static + Send>() -> Subscription<Message> {
        event::listen_with(|event, _status, _window| {
            if let Event::Mouse(mouse::Event::CursorMoved { position }) = event {
                X.store(position.x.to_bits(), Ordering::Relaxed);
                Y.store(position.y.to_bits(), Ordering::Relaxed);
            }
            None
        })
    }

    pub fn last() -> (f32, f32) {
        (f32::from_bits(X.load(Ordering::Relaxed)), f32::from_bits(Y.load(Ordering::Relaxed)))
    }
}
