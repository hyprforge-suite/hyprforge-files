//! The Preferences sheet: behaviour and key bindings, over the window
//! (mockup `1h`).
//!
//! What each setting means and what a binding would do is decided in
//! `hyprforge_files_core::preferences`, where it is tested without a
//! window. This is the sheet itself — its state, what its buttons ask
//! the window to do ([`Effect`]), and how it is drawn.
//!
//! A sheet over the window rather than a second window: `iced` 0.14's
//! single-window `application` is what this app is built on, and a
//! settings page that is modal to the one window it configures is what
//! the mockup draws anyway.
//!
//! Appearance is deliberately absent — see the core module's doc — and
//! the sheet says where it lives instead, because "where do I change the
//! colours" is the first thing someone opening Preferences looks for.

use hyprforge_files_core::action::Action;
use hyprforge_files_core::config::{Config, ConfigProblem};
use hyprforge_files_core::config_edit::{Edit, EditError, FileState};
use hyprforge_files_core::keymap::Combo;
use hyprforge_files_core::preferences::{
    binding_rows, cap_label, conflict_label, group, plan_binding, source_label, BehaviourSetting, BindingRow, Capture,
    Plan, Setting, SidebarSetting, ThumbnailSetting, CONFLICT_POLICIES, GRID_NAME_LINES, GROUPS, THUMBNAIL_CAPS,
    THUMBNAIL_SOURCES,
};
use hyprforge_files_core::preferences::{open_in_label, typing_label, OPEN_IN_CHOICES, TYPING_CHOICES};
use hyprforge_files_core::preferences::{seconds_label, watch_network_choices};
use hyprforge_files_core::prefs::{Prefs, SidebarPref, ViewMode};
use hyprforge_ui::theme::{spacing, surface, FontScale, BASE_TEXT_SIZE};
use hyprforge_ui::widgets::{
    config_line, hint_text, keycap, meta_text, page_header, primary_button, scaled_text, secondary_button,
    section_label, segmented_choice, selectable_row_style, setting_list, setting_row, toggle,
};
use iced::widget::{button, column, container, row, scrollable, text_input, Space};
use iced::{Background, Border, Color, Element, Length};
use std::collections::BTreeSet;
use std::path::PathBuf;

pub mod launching;

/// The sheet's two pages.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Page {
    #[default]
    Behaviour,
    Keys,
    /// The terminal, and the person's own actions — see [`launching`].
    Launching,
}

/// A key that belongs to another action, waiting for a yes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Taking {
    pub action: Action,
    pub holder: Action,
    pub combo: Combo,
    pub edits: Vec<Edit>,
}

/// The sheet, while it is open.
#[derive(Debug, Clone, PartialEq)]
pub struct Preferences {
    page: Page,
    /// `files-config.toml`, for the sheet to name.
    path: PathBuf,
    /// What the file said when last read — `None` until it has been.
    file: Option<FileState>,
    /// What the last load of the file complained about.
    problems: Vec<ConfigProblem>,
    /// The action waiting for a key press, and whether that press
    /// replaces its keys or joins them.
    capturing: Option<(Action, Capture)>,
    taking: Option<Taking>,
    /// Why the last key pressed could not be bound, or the last write
    /// failed — said where it happened, not in the window's status line
    /// behind the sheet.
    note: Option<String>,
    /// A write on its way. Everything that writes is off meanwhile: two
    /// edits racing would each read the file before the other wrote it.
    saving: bool,
    /// Narrows the key bindings list.
    filter: String,
    /// What the last Clear Recent did, said beside its button. `None`
    /// while one is running, too: [`Self::clearing`] says that.
    cleared: Option<String>,
    /// A Clear Recent on its way.
    clearing: bool,
    /// The "Terminal & actions" page.
    launching: launching::Launching,
}

/// What a write came back with: the configuration as it now loads, what
/// the file now says, and what the load complained about — or why the
/// write did not happen.
pub type Written = Result<(Config, FileState, Vec<ConfigProblem>), EditError>;

/// What the sheet is told.
#[derive(Debug, Clone, PartialEq)]
pub enum Message {
    Show(Page),
    Close,
    /// The file, read when the sheet opened.
    Read(FileState, Vec<ConfigProblem>),
    Capture(Action, Capture),
    CancelCapture,
    /// A key pressed while capturing — the window hands it over.
    Captured(Combo),
    TakeIt,
    KeepIt,
    Clear(Action),
    Reset(Action),
    Browsing(Setting),
    Behaviour(BehaviourSetting),
    /// One of `[thumbnails]`' settings.
    Thumbnails(ThumbnailSetting),
    /// A write finished: the configuration as it now loads, what the file
    /// now says, and what the load complained about — or why the write
    /// did not happen.
    Saved(Box<Written>),
    Filter(String),
    /// One of `[sidebar]`'s Recent and Starred switches.
    Sidebar(SidebarSetting),
    /// The Clear Recent button.
    ClearRecent,
    /// What Clear Recent did, in a sentence — the window's answer.
    RecentCleared(String),
    /// Escape: puts away what is open on a page, else closes the sheet.
    Escape,
    Launching(launching::Message),
}

/// What the window should do after the sheet has updated.
#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    None,
    Close,
    /// Write these to `files-config.toml`, then reload it and answer with
    /// [`Message::Saved`].
    Write(Vec<Edit>),
    /// Apply this to every tab and save it to `files.toml`.
    Adopt(Setting),
    /// Forget what Files recorded in Recent, and answer with
    /// [`Message::RecentCleared`].
    ClearRecent,
}

/// "1 line", "2 lines".
fn lines_label(n: u8) -> String {
    if n == 1 {
        "1 line".to_string()
    } else {
        format!("{n} lines")
    }
}

/// The line under a thumbnail source: what it costs, and for other
/// programs' thumbnailers, which ones this machine has — a switch for
/// "things installed elsewhere" means nothing until it names them.
fn source_hint<'a>(
    source: hyprforge_files_core::preview::Source,
    scale: FontScale,
) -> Option<Element<'a, Message>> {
    use hyprforge_files_core::preview::Source;
    let text = match source {
        Source::Picture | Source::Svg => "Photos, drawings and SVGs, in the list as well as the grid.".to_string(),
        Source::Pdf => "The first page, in the grid. Uses poppler's pdftoppm.".to_string(),
        Source::Video => "A frame from the start, in the grid. Uses ffmpeg.".to_string(),
        Source::Model => "STL, 3MF and OBJ, drawn in the grid.".to_string(),
        Source::System => {
            let found: Vec<&str> =
                crate::preview::system_thumbnailers().all().iter().map(|t| t.name.as_str()).collect();
            if found.is_empty() {
                "None are installed.".to_string()
            } else {
                format!("For any other type, in the grid. Found: {}.", found.join(", "))
            }
        }
    };
    Some(hint_text(text, scale).wrapping(iced::widget::text::Wrapping::WordOrGlyph).into())
}

impl Preferences {
    pub fn new(path: PathBuf, page: Page) -> Preferences {
        Preferences {
            page,
            path,
            file: None,
            problems: Vec::new(),
            capturing: None,
            taking: None,
            note: None,
            saving: false,
            filter: String::new(),
            cleared: None,
            clearing: false,
            launching: launching::Launching::new(launching::Installed::default()),
        }
    }

    /// Whether the next key press is a binding being captured, rather
    /// than something for the sheet or the window.
    pub fn capturing(&self) -> bool {
        self.capturing.is_some()
    }

    pub fn page(&self) -> Page {
        self.page
    }

    /// Whether edits to `files-config.toml` are possible right now.
    fn writable(&self) -> bool {
        !self.saving && self.file.as_ref().is_some_and(|f| f.refused.is_none())
    }

    pub fn update(&mut self, message: Message, config: &Config) -> Effect {
        match message {
            Message::Show(page) => {
                self.page = page;
                self.capturing = None;
                self.taking = None;
                Effect::None
            }
            Message::Close => Effect::Close,
            Message::Escape if self.page == Page::Launching && self.launching.editing() => {
                self.launching.cancel();
                Effect::None
            }
            Message::Escape => Effect::Close,
            Message::Launching(message) => match self.launching.update(message, config, self.writable()) {
                Some(edits) => self.write(edits),
                None => Effect::None,
            },
            Message::Read(file, problems) => {
                self.file = Some(file);
                self.problems = problems;
                Effect::None
            }
            Message::Capture(action, how) => {
                if !self.writable() {
                    return Effect::None;
                }
                self.capturing = Some((action, how));
                self.taking = None;
                self.note = None;
                Effect::None
            }
            Message::CancelCapture => {
                self.capturing = None;
                Effect::None
            }
            Message::Captured(combo) => {
                let Some((action, how)) = self.capturing.take() else { return Effect::None };
                match plan_binding(&config.keymap, action, combo, how) {
                    Plan::Bind(edits) => self.write(edits),
                    Plan::Already => Effect::None,
                    Plan::Refused(why) => {
                        self.note = Some(why);
                        Effect::None
                    }
                    Plan::Taken { holder, edits } => {
                        self.taking = Some(Taking { action, holder, combo, edits });
                        Effect::None
                    }
                }
            }
            Message::TakeIt => match self.taking.take() {
                Some(taking) => self.write(taking.edits),
                None => Effect::None,
            },
            Message::KeepIt => {
                self.taking = None;
                Effect::None
            }
            Message::Clear(action) => self.write(vec![hyprforge_files_core::preferences::clear(action)]),
            Message::Reset(action) => self.write(vec![hyprforge_files_core::preferences::reset(action)]),
            Message::Browsing(setting) => Effect::Adopt(setting),
            Message::Behaviour(setting) => {
                if setting.holds_in(&config.behaviour) {
                    return Effect::None;
                }
                self.write(vec![setting.edit()])
            }
            Message::Thumbnails(setting) => {
                if setting.holds_in(&config.thumbnails) {
                    return Effect::None;
                }
                self.write(vec![setting.edit()])
            }
            Message::Saved(result) => {
                let result = *result;
                self.saving = false;
                self.launching.saved(result.is_ok());
                match result {
                    Ok((_, file, problems)) => {
                        self.file = Some(file);
                        self.problems = problems;
                        self.note = None;
                    }
                    Err(why) => self.note = Some(why.to_string()),
                }
                Effect::None
            }
            Message::Filter(text) => {
                self.filter = text;
                Effect::None
            }
            Message::Sidebar(setting) => {
                if setting.holds_in(&config.sidebar) {
                    return Effect::None;
                }
                self.write(vec![setting.edit()])
            }
            // Not a `files-config.toml` write, so not held back by one:
            // it edits another file entirely. Held back only by itself.
            Message::ClearRecent => {
                if self.clearing {
                    return Effect::None;
                }
                self.clearing = true;
                self.cleared = None;
                Effect::ClearRecent
            }
            Message::RecentCleared(said) => {
                self.clearing = false;
                self.cleared = Some(said);
                Effect::None
            }
        }
    }

    fn write(&mut self, edits: Vec<Edit>) -> Effect {
        if !self.writable() {
            return Effect::None;
        }
        self.saving = true;
        self.note = None;
        Effect::Write(edits)
    }

    /// Draws the sheet, over the whole window.
    pub fn view<'a>(&'a self, config: &'a Config, prefs: &'a Prefs, scale: FontScale) -> Element<'a, Message> {
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let shown_path = hyprforge_files_core::format::tilde_path(&self.path, home.as_deref());

        let nav_item = |page: Page, label: &'static str| -> Element<'a, Message> {
            button(scaled_text(label, BASE_TEXT_SIZE, scale))
                .width(Length::Fill)
                .padding([spacing::XS, spacing::SM])
                .on_press(Message::Show(page))
                .style(move |t: &iced::Theme, status| selectable_row_style(t, status, self.page == page))
                .into()
        };
        let nav = column![
            nav_item(Page::Behaviour, "Behaviour"),
            nav_item(Page::Keys, "Key bindings"),
            nav_item(Page::Launching, "Terminal & actions"),
            Space::new().height(Length::Fill),
            hint_text("Colours, fonts and the accent come from the Settings app, so every Hyprforge window matches.", scale)
                .wrapping(iced::widget::text::Wrapping::WordOrGlyph),
        ]
        .spacing(spacing::XS)
        .width(Length::Fixed(scale.apply(170.0)))
        .height(Length::Fill);

        let page: Element<'a, Message> = match self.page {
            Page::Behaviour => self.behaviour(config, prefs, scale),
            Page::Keys => self.keys(config, scale),
            Page::Launching => {
                self.launching.view(config, &self.problems, self.writable(), scale).map(Message::Launching)
            }
        };

        let mut body = column![
            row![
                page_header(
                    match self.page {
                        Page::Behaviour => "Behaviour",
                        Page::Keys => "Key bindings",
                        Page::Launching => "Terminal & actions",
                    },
                    Some(shown_path),
                    scale,
                ),
                Space::new().width(Length::Fill),
                secondary_button("Close").on_press(Message::Close),
            ]
            .align_y(iced::Alignment::Center),
        ]
        .spacing(spacing::MD);
        if let Some(Some(refused)) = self.file.as_ref().map(|f| f.refused.as_ref()) {
            body = body.push(warning(refused.to_string(), scale));
        }
        if let Some(note) = &self.note {
            body = body.push(warning(note.clone(), scale));
        }
        body = body.push(scrollable(container(page).padding([0.0, spacing::MD])).height(Length::Fill));

        let card = container(
            row![nav, container(body).width(Length::Fill).height(Length::Fill)]
                .spacing(spacing::LG)
                .height(Length::Fill),
        )
        .padding(spacing::LG)
        .max_width(scale.apply(900.0))
        .height(Length::Fill)
        .style(|_t: &iced::Theme| container::Style {
            background: Some(Background::Color(surface::sidebar())),
            border: Border {
                color: surface::card_border(),
                width: 1.0,
                radius: hyprforge_files_core::density::outer_radius().into(),
            },
            ..container::Style::default()
        });

        iced::widget::opaque(
            container(card)
                .center_x(Length::Fill)
                .padding(spacing::XL)
                .width(Length::Fill)
                .height(Length::Fill)
                // Dimmed with the window's own root colour, as every
                // dialog here is: the window is still there, and plainly
                // not what the keyboard is on.
                .style(|_t: &iced::Theme| container::Style {
                    background: Some(Background::Color(Color { a: 0.6, ..surface::root() })),
                    ..container::Style::default()
                }),
        )
    }

    fn behaviour<'a>(&'a self, config: &'a Config, prefs: &'a Prefs, scale: FontScale) -> Element<'a, Message> {
        let switch = |on: bool, make: fn(bool) -> Message| -> Element<'a, Message> {
            toggle(on, scale).on_toggle(make).into()
        };
        let writable = self.writable();
        let config_switch = |on: bool, make: fn(bool) -> Message| -> Element<'a, Message> {
            let mut t = toggle(on, scale);
            if writable {
                t = t.on_toggle(make);
            }
            t.into()
        };
        let browsing = setting_list([
            setting_row(
                0,
                "Show hidden files",
                Some(hint_text("Names starting with a dot. The status bar's switch and Ctrl+H change it too.", scale).into()),
                switch(prefs.show_hidden, |on| Message::Browsing(Setting::ShowHidden(on))),
                scale,
            ),
            setting_row(
                1,
                "Folders before files",
                None,
                switch(prefs.directories_first, |on| Message::Browsing(Setting::DirectoriesFirst(on))),
                scale,
            ),
            setting_row(
                2,
                "Preview pane",
                Some(hint_text("Beside the listing. Properties takes its place while it is open.", scale).into()),
                switch(prefs.preview_pane, |on| Message::Browsing(Setting::PreviewPane(on))),
                scale,
            ),
            setting_row(
                3,
                "View",
                Some(hint_text("Every tab. Files also remembers the one you last picked in the bar.", scale).into()),
                segmented_choice(
                    &[ViewMode::List, ViewMode::Grid, ViewMode::Columns],
                    Some(&prefs.view_mode),
                    |mode| match mode {
                        ViewMode::List => "List".to_string(),
                        ViewMode::Grid => "Grid".to_string(),
                        ViewMode::Columns => "Columns".to_string(),
                    },
                    |mode| Message::Browsing(Setting::View(mode)),
                    scale,
                ),
                scale,
            ),
            setting_row(
                4,
                "Sidebar",
                Some(hint_text("Automatic folds it to a rail in a narrow window.", scale).into()),
                segmented_choice(
                    &[SidebarPref::Auto, SidebarPref::Shown, SidebarPref::Hidden],
                    Some(&prefs.sidebar),
                    |pref| match pref {
                        SidebarPref::Auto => "Automatic".to_string(),
                        SidebarPref::Shown => "Shown".to_string(),
                        SidebarPref::Hidden => "Hidden".to_string(),
                    },
                    |pref| Message::Browsing(Setting::Sidebar(pref)),
                    scale,
                ),
                scale,
            ),
            setting_row(
                5,
                "Typing in a folder",
                Some(hint_text("Jump moves to the first name you type; Ctrl+F searches either way.", scale).into()),
                if writable {
                    segmented_choice(
                        &TYPING_CHOICES,
                        Some(&config.behaviour.typing),
                        |t| typing_label(*t).to_string(),
                        |t| Message::Behaviour(BehaviourSetting::Typing(t)),
                        scale,
                    )
                } else {
                    config_line(typing_label(config.behaviour.typing), scale).into()
                },
                scale,
            ),
            setting_row(
                6,
                "Folders from other apps",
                Some(hint_text("Show in folder, and opening a folder with Files, use the window you used last.", scale).into()),
                if writable {
                    segmented_choice(
                        &OPEN_IN_CHOICES,
                        Some(&config.behaviour.open_in),
                        |o| open_in_label(*o).to_string(),
                        |o| Message::Behaviour(BehaviourSetting::OpenIn(o)),
                        scale,
                    )
                } else {
                    config_line(open_in_label(config.behaviour.open_in), scale).into()
                },
                scale,
            ),
        ]);

        let behaviour = &config.behaviour;
        let conflict: Element<'a, Message> = if writable {
            segmented_choice(
                &CONFLICT_POLICIES,
                Some(&behaviour.on_conflict),
                |p| conflict_label(*p).to_string(),
                |p| Message::Behaviour(BehaviourSetting::OnConflict(p)),
                scale,
            )
        } else {
            config_line(conflict_label(behaviour.on_conflict), scale).into()
        };
        let operations = setting_list([
            setting_row(
                0,
                "When a pasted name is taken",
                Some(hint_text("Replace overwrites what is there; it is never the default.", scale).into()),
                conflict,
                scale,
            ),
            setting_row(
                1,
                "Ask before moving to the Trash",
                Some(hint_text("Off by default: the Trash is the undo.", scale).into()),
                config_switch(behaviour.confirm_trash, |on| Message::Behaviour(BehaviourSetting::ConfirmTrash(on))),
                scale,
            ),
            setting_row(
                2,
                "Ask before deleting for good",
                None,
                config_switch(behaviour.confirm_delete, |on| Message::Behaviour(BehaviourSetting::ConfirmDelete(on))),
                scale,
            ),
        ]);

        // Recent and Starred: whether each has a row at the top of
        // Places, and a way to forget what Files itself put in Recent.
        // Only Files' own entries — the list is the desktop's, and what
        // a browser or an editor recorded there is theirs to keep.
        let sidebar_config = &config.sidebar;
        let mut clear = secondary_button(if self.clearing { "Clearing\u{2026}" } else { "Clear Recent" });
        if !self.clearing {
            clear = clear.on_press(Message::ClearRecent);
        }
        let clear_hint = self.cleared.clone().unwrap_or_else(|| {
            "Forgets the files Files opened. What other applications opened stays in their Recent.".to_string()
        });
        let sidebar = setting_list([
            setting_row(
                0,
                "Recent in the sidebar",
                Some(hint_text("What was opened lately, by Files and every other application.", scale).into()),
                config_switch(sidebar_config.show_recent, |on| Message::Sidebar(SidebarSetting::ShowRecent(on))),
                scale,
            ),
            setting_row(
                1,
                "Starred in the sidebar",
                Some(hint_text("Star a file or folder from its menu.", scale).into()),
                config_switch(sidebar_config.show_starred, |on| Message::Sidebar(SidebarSetting::ShowStarred(on))),
                scale,
            ),
            setting_row(2, "Files' recent history", Some(hint_text(clear_hint, scale).into()), clear, scale),
        ]);

        column![
            section_label("Browsing", scale),
            browsing,
            section_label("Sidebar", scale),
            sidebar,
            section_label("File operations", scale),
            operations,
            section_label("Tabs and live updates", scale),
            self.live(config, prefs, scale),
            section_label("Thumbnails and names", scale),
            self.thumbnails(config, scale),
            hint_text(
                "Browsing is remembered in files.toml. File operations and key bindings are written to \
                 files-config.toml one line at a time, so anything you wrote there by hand stays; its \
                 other settings are there to edit.",
                scale,
            )
            .wrapping(iced::widget::text::Wrapping::WordOrGlyph),
        ]
        .spacing(spacing::MD)
        .into()
    }

    /// Reopening tabs (`files.toml`, like Browsing) and watching folders
    /// (`files-config.toml`, like File operations) — one section because
    /// both are about what the window shows without being asked.
    fn live<'a>(&'a self, config: &'a Config, prefs: &'a Prefs, scale: FontScale) -> Element<'a, Message> {
        let writable = self.writable();
        let behaviour = &config.behaviour;
        let mut watch = toggle(behaviour.watch, scale);
        if writable {
            watch = watch.on_toggle(|on| Message::Behaviour(BehaviourSetting::Watch(on)));
        }
        let every: Element<'a, Message> = if writable {
            segmented_choice(
                &watch_network_choices(behaviour.watch_network_every),
                Some(&behaviour.watch_network_every),
                |s| seconds_label(*s),
                |s| Message::Behaviour(BehaviourSetting::WatchNetworkEvery(s)),
                scale,
            )
        } else {
            config_line(seconds_label(behaviour.watch_network_every), scale).into()
        };
        setting_list([
            setting_row(
                0,
                "Reopen last time's tabs",
                Some(hint_text("Not when Files is started on a folder. A folder that is gone is skipped.", scale).into()),
                toggle(prefs.restore_tabs, scale).on_toggle(|on| Message::Browsing(Setting::RestoreTabs(on))),
                scale,
            ),
            // The toggle only: which pane Copy to Other Pane goes to needs
            // no setting, because a split has exactly one other pane.
            setting_row(
                1,
                "Open new tabs split",
                Some(hint_text("Two folders side by side. F3 splits or unsplits any tab.", scale).into()),
                toggle(prefs.split_new_tabs, scale).on_toggle(|on| Message::Browsing(Setting::SplitNewTabs(on))),
                scale,
            ),
            setting_row(
                2,
                "Update folders as they change",
                Some(hint_text("Off, a folder shows what other programs did only when you press F5.", scale).into()),
                watch,
                scale,
            ),
            setting_row(
                3,
                "Check network folders every",
                Some(
                    hint_text("A share can't say when someone else changes it, so it is asked while it is shown.", scale)
                        .into(),
                ),
                every,
                scale,
            ),
        ])
        .into()
    }

    /// `[thumbnails]` and the grid's name length: which kinds of file are
    /// drawn as a picture of themselves, the size past which none is, and
    /// how much of a long name a grid cell shows.
    fn thumbnails<'a>(&'a self, config: &'a Config, scale: FontScale) -> Element<'a, Message> {
        let writable = self.writable();
        let lines = config.behaviour.grid_name_lines;
        let names: Element<'a, Message> = if writable {
            segmented_choice(
                &GRID_NAME_LINES,
                Some(&lines),
                |n| lines_label(*n),
                |n| Message::Behaviour(BehaviourSetting::GridNameLines(n)),
                scale,
            )
        } else {
            config_line(lines_label(lines), scale).into()
        };
        let mut rows = vec![setting_row(
            0,
            "Names in the grid",
            Some(hint_text("Longer names end in \u{2026}; the selected one is shown whole.", scale).into()),
            names,
            scale,
        )];
        for (i, source) in THUMBNAIL_SOURCES.into_iter().enumerate() {
            let on = config.thumbnails.allows(source);
            let mut t = toggle(on, scale);
            if writable {
                t = t.on_toggle(move |on| Message::Thumbnails(ThumbnailSetting::Source(source, on)));
            }
            rows.push(setting_row(i + 1, source_label(source), source_hint(source, scale), t, scale));
        }
        let cap = config.thumbnails.max_file_mb;
        let caps: Element<'a, Message> = if writable {
            segmented_choice(
                &THUMBNAIL_CAPS,
                THUMBNAIL_CAPS.contains(&cap).then_some(&cap),
                |mb| cap_label(*mb),
                |mb| Message::Thumbnails(ThumbnailSetting::MaxFileMb(mb)),
                scale,
            )
        } else {
            config_line(cap_label(cap), scale).into()
        };
        rows.push(setting_row(
            THUMBNAIL_SOURCES.len() + 1,
            "Skip files larger than",
            Some(hint_text("A bigger file keeps its icon.", scale).into()),
            caps,
            scale,
        ));
        setting_list(rows).into()
    }

    fn keys<'a>(&'a self, config: &'a Config, scale: FontScale) -> Element<'a, Message> {
        let empty = BTreeSet::new();
        let customised = self.file.as_ref().map_or(&empty, |f| &f.customised);
        let rows = binding_rows(&config.keymap, customised);
        let filter = self.filter.trim().to_lowercase();
        let shown = |row: &BindingRow| {
            filter.is_empty()
                || row.action.label().to_lowercase().contains(&filter)
                || row.keys.iter().any(|k| k.to_string().to_lowercase().contains(&filter))
        };

        let mut list = column![text_input("Find a command or a key", &self.filter)
            .on_input(Message::Filter)
            .padding([spacing::XS, spacing::SM])
            .size(scale.apply(BASE_TEXT_SIZE * 0.9))
            .style(hyprforge_ui::widgets::inset_input_style)]
        .spacing(spacing::MD);

        let key_problems: Vec<&ConfigProblem> =
            self.problems.iter().filter(|p| p.message.starts_with("[keys]")).collect();
        if !key_problems.is_empty() {
            let mut lines = column![].spacing(2.0);
            for problem in key_problems {
                lines = lines.push(
                    scaled_text(problem.message.clone(), hyprforge_ui::density::META_TEXT_BASE, scale)
                        .color(hyprforge_ui::theme::warning())
                        .wrapping(iced::widget::text::Wrapping::WordOrGlyph),
                );
            }
            list = list.push(lines);
        }

        for heading in GROUPS {
            let in_group: Vec<&BindingRow> = rows.iter().filter(|r| group(r.action) == heading && shown(r)).collect();
            if in_group.is_empty() {
                continue;
            }
            let lines = in_group.into_iter().enumerate().map(|(i, r)| self.binding_line(i, r, scale));
            list = list.push(column![section_label(heading, scale), setting_list(lines)].spacing(spacing::SM));
        }
        list.into()
    }

    fn binding_line<'a>(&'a self, index: usize, row_data: &BindingRow, scale: FontScale) -> Element<'a, Message> {
        let action = row_data.action;
        let writable = self.writable();
        let quiet = |label: &'static str, message: Message| -> Element<'a, Message> {
            let mut b = button(meta_text(label, hyprforge_ui::density::META_TEXT_BASE, scale))
                .padding([2.0, spacing::XS])
                .style(|t: &iced::Theme, status| selectable_row_style(t, status, false));
            if writable {
                b = b.on_press(message);
            }
            b.into()
        };

        let capturing_here = self.capturing.is_some_and(|(a, _)| a == action);
        let mut control = row![].spacing(spacing::SM).align_y(iced::Alignment::Center);
        if capturing_here {
            control = control
                .push(scaled_text("Press a key\u{2026}", hyprforge_ui::density::META_TEXT_BASE, scale))
                .push(secondary_button("Cancel").on_press(Message::CancelCapture));
        } else {
            let mut caps = row![].spacing(spacing::XS).align_y(iced::Alignment::Center);
            if row_data.keys.is_empty() {
                caps = caps.push(meta_text("No key", hyprforge_ui::density::META_TEXT_BASE, scale));
            }
            for (n, combo) in row_data.keys.iter().enumerate() {
                if n > 0 {
                    caps = caps.push(meta_text("or", hyprforge_ui::density::META_TEXT_BASE, scale));
                }
                let mut chord = row![].spacing(2.0);
                for part in combo.to_string().split('+') {
                    chord = chord.push(keycap(part.to_string(), scale));
                }
                caps = caps.push(chord);
            }
            control = control.push(caps);
            control = control.push(quiet("Change", Message::Capture(action, Capture::Replace)));
            if !row_data.keys.is_empty() {
                control = control.push(quiet("Add", Message::Capture(action, Capture::Add)));
                control = control.push(quiet("Clear", Message::Clear(action)));
            }
            if row_data.customised {
                control = control.push(quiet("Reset", Message::Reset(action)));
            }
        }

        let hint: Option<Element<'a, Message>> = if row_data.customised && !row_data.is_default {
            Some(hint_text("Your own binding", scale).into())
        } else if hyprforge_files_core::preferences::only_in_the_window(action) {
            Some(hint_text("Not in the open/save dialog", scale).into())
        } else {
            None
        };
        let line = setting_row(index, row_data.action.label(), hint, control, scale);
        // The question about a taken key gets a line of its own under the
        // row. Squeezed in as the row's hint it shared the width with the
        // keycaps and buttons, and its own buttons wrapped a word a line.
        match &self.taking {
            Some(taking) if taking.action == action => column![
                line,
                container(
                    row![
                        scaled_text(
                            format!("{} is {}'s key. Use it here instead?", taking.combo, taking.holder.label()),
                            hyprforge_ui::density::META_TEXT_BASE,
                            scale,
                        )
                        .color(hyprforge_ui::theme::warning())
                        .width(Length::Fill),
                        primary_button("Use it here").on_press(Message::TakeIt),
                        secondary_button("Keep it there").on_press(Message::KeepIt),
                    ]
                    .spacing(spacing::SM)
                    .align_y(iced::Alignment::Center),
                )
                .padding([spacing::XS, scale.apply(14.0)]),
            ]
            .into(),
            _ => line,
        }
    }
}

/// A sentence the sheet needs seen: in the warning colour, wrapped.
fn warning<'a>(text: String, scale: FontScale) -> Element<'a, Message> {
    scaled_text(text, hyprforge_ui::density::META_TEXT_BASE, scale)
        .color(hyprforge_ui::theme::warning())
        .wrapping(iced::widget::text::Wrapping::WordOrGlyph)
        .into()
}

/// Reads `path` for the sheet: what it names and whether it can be
/// written, and what loading it complains about. Blocking.
pub fn read(path: &std::path::Path) -> (FileState, Vec<ConfigProblem>) {
    let state = hyprforge_files_core::config_edit::read_state(path);
    let (_, problems) = hyprforge_files_core::config::load_from(path);
    (state, problems)
}

/// Writes `edits` to `path` and loads the result. Blocking.
pub fn write(
    path: &std::path::Path,
    edits: &[Edit],
) -> Result<(Config, FileState, Vec<ConfigProblem>), EditError> {
    hyprforge_files_core::config_edit::apply_to_file(path, edits)?;
    let (config, problems) = hyprforge_files_core::config::load_from(path);
    Ok((config, hyprforge_files_core::config_edit::read_state(path), problems))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn combo(text: &str) -> Combo {
        Combo::parse(text).unwrap()
    }

    fn ready() -> Preferences {
        let mut sheet = Preferences::new(PathBuf::from("/tmp/files-config.toml"), Page::Keys);
        sheet.update(Message::Read(FileState::default(), Vec::new()), &Config::default());
        sheet
    }

    #[test]
    fn a_captured_free_key_is_written_at_once() {
        let mut sheet = ready();
        let config = Config::default();
        sheet.update(Message::Capture(Action::Properties, Capture::Replace), &config);
        assert!(sheet.capturing());
        let effect = sheet.update(Message::Captured(combo("Ctrl+I")), &config);
        assert!(matches!(effect, Effect::Write(_)), "{effect:?}");
        assert!(!sheet.capturing());
    }

    /// A clash is asked about, and nothing is written until it is
    /// answered — "keep it there" writes nothing at all.
    #[test]
    fn a_key_another_action_holds_is_asked_about_before_anything_is_written() {
        let mut sheet = ready();
        let config = Config::default();
        sheet.update(Message::Capture(Action::Rename, Capture::Replace), &config);
        assert_eq!(sheet.update(Message::Captured(combo("Ctrl+R")), &config), Effect::None);
        assert_eq!(sheet.taking.as_ref().map(|t| t.holder), Some(Action::Refresh));
        assert_eq!(sheet.update(Message::KeepIt, &config), Effect::None);

        sheet.update(Message::Capture(Action::Rename, Capture::Replace), &config);
        sheet.update(Message::Captured(combo("Ctrl+R")), &config);
        assert!(matches!(sheet.update(Message::TakeIt, &config), Effect::Write(edits) if edits.len() == 2));
    }

    #[test]
    fn a_refused_key_is_explained_in_the_sheet() {
        let mut sheet = ready();
        let config = Config::default();
        sheet.update(Message::Capture(Action::Rename, Capture::Replace), &config);
        assert_eq!(sheet.update(Message::Captured(combo("N")), &config), Effect::None);
        assert!(sheet.note.as_deref().is_some_and(|n| n.contains("search")), "{:?}", sheet.note);
    }

    /// The 37-binds rule, as the sheet sees it: a file that will not
    /// parse is never written over, so nothing in the sheet offers to.
    #[test]
    fn nothing_is_written_while_the_file_will_not_parse() {
        let mut sheet = Preferences::new(PathBuf::from("/tmp/x.toml"), Page::Keys);
        let broken = FileState {
            customised: BTreeSet::new(),
            refused: Some(EditError::Unparseable { path: "x".into(), why: "line 1".into() }),
        };
        let config = Config::default();
        sheet.update(Message::Read(broken, Vec::new()), &config);
        sheet.update(Message::Capture(Action::Rename, Capture::Replace), &config);
        assert!(!sheet.capturing());
        assert_eq!(sheet.update(Message::Clear(Action::Rename), &config), Effect::None);
        assert_eq!(
            sheet.update(Message::Behaviour(BehaviourSetting::ConfirmTrash(true)), &config),
            Effect::None
        );
        // The listing's own settings live in another file and still work.
        assert!(matches!(
            sheet.update(Message::Browsing(Setting::ShowHidden(true)), &config),
            Effect::Adopt(_)
        ));
    }

    #[test]
    fn nothing_is_written_before_the_file_has_been_read() {
        let mut sheet = Preferences::new(PathBuf::from("/tmp/x.toml"), Page::Keys);
        assert_eq!(sheet.update(Message::Clear(Action::Rename), &Config::default()), Effect::None);
    }

    #[test]
    fn a_second_write_waits_for_the_first() {
        let mut sheet = ready();
        let config = Config::default();
        assert!(matches!(sheet.update(Message::Clear(Action::Rename), &config), Effect::Write(_)));
        assert_eq!(sheet.update(Message::Clear(Action::Copy), &config), Effect::None);
        sheet.update(Message::Saved(Box::new(Ok((config.clone(), FileState::default(), Vec::new())))), &config);
        assert!(matches!(sheet.update(Message::Clear(Action::Copy), &config), Effect::Write(_)));
    }

    #[test]
    fn the_thumbnail_and_grid_name_controls_write_their_own_edit() {
        use hyprforge_files_core::config_edit::BehaviourValue;
        use hyprforge_files_core::preview::Source;
        let config = Config::default();
        let mut sheet = ready();
        let effect = sheet.update(Message::Thumbnails(ThumbnailSetting::Source(Source::Video, false)), &config);
        assert_eq!(effect, Effect::Write(vec![Edit::Thumbnails("videos", BehaviourValue::Switch(false))]));
        let mut sheet = ready();
        let effect = sheet.update(Message::Thumbnails(ThumbnailSetting::MaxFileMb(50)), &config);
        assert_eq!(effect, Effect::Write(vec![Edit::Thumbnails("max-file-mb", BehaviourValue::Number(50))]));
        let mut sheet = ready();
        let effect = sheet.update(Message::Behaviour(BehaviourSetting::GridNameLines(3)), &config);
        assert_eq!(effect, Effect::Write(vec![Edit::Behaviour("grid-name-lines", BehaviourValue::Number(3))]));
        // Already so: nothing to write.
        let mut sheet = ready();
        assert_eq!(sheet.update(Message::Thumbnails(ThumbnailSetting::Source(Source::Pdf, true)), &config), Effect::None);
        assert_eq!(sheet.update(Message::Behaviour(BehaviourSetting::GridNameLines(2)), &config), Effect::None);
    }

    #[test]
    fn choosing_the_behaviour_already_in_force_writes_nothing() {
        let mut sheet = ready();
        let config = Config::default();
        assert_eq!(
            sheet.update(
                Message::Behaviour(BehaviourSetting::OnConflict(hyprforge_files_core::config::OnConflict::Ask)),
                &config
            ),
            Effect::None
        );
    }

    /// Live updates go to `files-config.toml` a line at a time; reopening
    /// tabs is the window's own state, in `files.toml`, like the toolbar's.
    #[test]
    fn the_live_update_switches_write_their_line_and_restoring_tabs_is_adopted() {
        let mut sheet = ready();
        let config = Config::default();
        assert_eq!(
            sheet.update(Message::Behaviour(BehaviourSetting::WatchNetworkEvery(10)), &config),
            Effect::Write(vec![Edit::Behaviour(
                "watch-network-every",
                hyprforge_files_core::config_edit::BehaviourValue::Number(10)
            )])
        );
        let mut sheet = ready();
        assert_eq!(sheet.update(Message::Behaviour(BehaviourSetting::Watch(true)), &config), Effect::None, "already on");
        assert_eq!(
            sheet.update(Message::Browsing(Setting::RestoreTabs(false)), &config),
            Effect::Adopt(Setting::RestoreTabs(false))
        );
    }

    /// Opening new tabs split is the window's own state, in `files.toml`
    /// beside restoring tabs: every tab adopts it, and nothing is written
    /// to `files-config.toml`. That the switch writes exactly its line of
    /// `files.toml` is `hyprforge_files_core::preferences`'s test.
    #[test]
    fn opening_new_tabs_split_is_adopted_by_the_window_like_restoring_tabs() {
        let mut sheet = ready();
        assert_eq!(
            sheet.update(Message::Browsing(Setting::SplitNewTabs(true)), &Config::default()),
            Effect::Adopt(Setting::SplitNewTabs(true))
        );
    }

    /// Through the real file: a hand-written comment and line survive
    /// the sheet's edit, and the edit is the one line it claims.
    #[test]
    fn switching_live_updates_off_keeps_what_was_written_by_hand() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("files-config.toml");
        std::fs::write(&path, "[behaviour]\n# slow link\nwatch-network-every = 30\n").unwrap();
        let (config, _, problems) = write(&path, &[BehaviourSetting::Watch(false).edit()]).unwrap();
        assert!(problems.is_empty(), "{problems:?}");
        assert!(!config.behaviour.watch);
        assert_eq!(config.behaviour.watch_network_every, 30);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "[behaviour]\n# slow link\nwatch-network-every = 30\nwatch = false\n"
        );
    }

    /// Each sidebar switch writes exactly its own `[sidebar]` line, and
    /// through the real file the person's other lines stay as written.
    #[test]
    fn each_sidebar_switch_writes_exactly_its_line() {
        use hyprforge_files_core::config_edit::BehaviourValue;
        let mut sheet = ready();
        let config = Config::default();
        assert_eq!(
            sheet.update(Message::Sidebar(SidebarSetting::ShowRecent(false)), &config),
            Effect::Write(vec![Edit::Sidebar("show-recent", BehaviourValue::Switch(false))])
        );
        let mut sheet = ready();
        assert_eq!(
            sheet.update(Message::Sidebar(SidebarSetting::ShowStarred(false)), &config),
            Effect::Write(vec![Edit::Sidebar("show-starred", BehaviourValue::Switch(false))])
        );

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("files-config.toml");
        let mine = "# mine\n[sidebar]\nshow-trash = false\n";
        std::fs::write(&path, mine).unwrap();
        let (config, _, problems) = write(&path, &[SidebarSetting::ShowRecent(false).edit()]).unwrap();
        assert!(problems.is_empty(), "{problems:?}");
        assert!(!config.sidebar.show_recent && !config.sidebar.show_trash && config.sidebar.show_starred);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), format!("{mine}show-recent = false\n"));
    }

    /// A line written by hand is what the sheet shows: choosing the value
    /// it already says writes nothing.
    #[test]
    fn a_hand_written_sidebar_line_shows_in_the_sheet() {
        let mut sheet = ready();
        let (config, _) =
            hyprforge_files_core::config::parse("[sidebar]\nshow-starred = false\n", std::path::Path::new("x"));
        assert!(!config.sidebar.show_starred);
        assert_eq!(sheet.update(Message::Sidebar(SidebarSetting::ShowStarred(false)), &config), Effect::None);
    }

    /// Clear Recent asks the window once, says what it did, and is not a
    /// `files-config.toml` write — so a file that will not parse does not
    /// stop it.
    #[test]
    fn clear_recent_asks_once_and_says_what_it_did() {
        let mut sheet = Preferences::new(PathBuf::from("/tmp/x.toml"), Page::Behaviour);
        let config = Config::default();
        assert_eq!(sheet.update(Message::ClearRecent, &config), Effect::ClearRecent);
        assert_eq!(sheet.update(Message::ClearRecent, &config), Effect::None, "one at a time");
        sheet.update(Message::RecentCleared("Cleared 2 files".into()), &config);
        assert_eq!(sheet.cleared.as_deref(), Some("Cleared 2 files"));
        assert_eq!(sheet.update(Message::ClearRecent, &config), Effect::ClearRecent);
    }

    #[test]
    fn writing_and_reading_back_goes_through_the_real_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("files-config.toml");
        std::fs::write(&path, "# mine\n[keys]\ntrash = \"Delete\"\n").unwrap();
        let (config, state, problems) =
            write(&path, &[hyprforge_files_core::preferences::clear(Action::Refresh)]).unwrap();
        assert!(problems.is_empty(), "{problems:?}");
        assert!(config.keymap.combos_for(Action::Refresh).is_empty());
        assert!(state.customised.contains("refresh") && state.customised.contains("trash"));
        assert!(std::fs::read_to_string(&path).unwrap().starts_with("# mine\n"));
    }
}
