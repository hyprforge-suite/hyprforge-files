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
    binding_rows, conflict_label, group, plan_binding, BehaviourSetting, BindingRow, Capture, Plan, Setting,
    CONFLICT_POLICIES, GROUPS,
};
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
    /// A write finished: the configuration as it now loads, what the file
    /// now says, and what the load complained about — or why the write
    /// did not happen.
    Saved(Box<Written>),
    Filter(String),
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

        column![
            section_label("Browsing", scale),
            browsing,
            section_label("File operations", scale),
            operations,
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
