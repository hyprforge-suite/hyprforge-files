//! The Preferences sheet's "Terminal & actions" page: which terminal
//! "Open Terminal Here" starts, and the person's own `[[action]]`s.
//!
//! A page of its own, beside Behaviour and Key bindings, because the
//! action list is the largest control in the sheet — a list with an
//! editor under it — and it would bury the switches on Behaviour.
//!
//! Everything here is written to `files-config.toml` the way the rest of
//! the sheet writes it: one line, or one `[[action]]` block, at a time
//! through `hyprforge_files_core::config_edit`, so comments in the file
//! and blocks written by hand survive. What is shown is what the file
//! loads as — a block written by hand is in the list like any other.
//!
//! Checked as it is typed: a command whose program is not installed, a
//! file type that is not one, a quote left open — each is a sentence
//! under the field, and Save stays off until there are none. Writing a
//! block the loader would then refuse would be the sheet handing the
//! person a problem it could have told them about.

use crate::terminal::{Source, Terminal};
use hyprforge_files_core::config::{Config, ConfigProblem};
use hyprforge_files_core::config_edit::Edit;
use hyprforge_files_core::custom::{join_words, split_words, Change, Draft, Selection};
use hyprforge_ui::theme::{spacing, FontScale, BASE_TEXT_SIZE};
use hyprforge_ui::widgets::{
    config_line, hint_text, primary_button, scaled_text, secondary_button, section_label, segmented_choice,
    setting_list, setting_row, toggle,
};
use iced::widget::{column, container, row, text_input, Space};
use iced::{Element, Length};

/// Whether a program is installed. A newtype so the page can still be
/// compared and printed: the real one walks `PATH`, a test's is a list.
#[derive(Clone, Copy)]
pub struct Installed(pub fn(&str) -> bool);

impl PartialEq for Installed {
    /// Every checker is the same checker as far as the sheet's state is
    /// concerned — comparing function pointers says nothing reliable.
    fn eq(&self, _: &Installed) -> bool {
        true
    }
}

impl std::fmt::Debug for Installed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Installed")
    }
}

impl Default for Installed {
    fn default() -> Installed {
        Installed(hyprforge_mime::apps::on_path)
    }
}

/// The page's state.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Launching {
    /// What Automatic finds, once it has been looked for — `Some(None)`
    /// when nothing is installed.
    found: Option<Option<Terminal>>,
    /// The terminal field while a chosen command is being typed.
    terminal_text: Option<String>,
    /// What is wrong with that text, as it stands.
    terminal_problem: Option<String>,
    /// The action being added or changed.
    editor: Option<Editor>,
    /// The action whose removal is waiting for a yes, by id.
    removing: Option<String>,
    /// A write from this page is on its way; when it lands, what it was
    /// for is finished — the editor closes, the field is put away.
    pending: bool,
    installed: Installed,
}

/// The fields of one action, as typed.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Editor {
    /// The block being changed, by id; `None` for a new one.
    id: Option<String>,
    label: String,
    command: String,
    types: String,
    selection: Selection,
    terminal: bool,
    /// What stops it being saved — worked out on every change, not on
    /// every frame, because it walks `PATH`.
    problems: Vec<String>,
}

impl Editor {
    /// The draft these fields say, or what is wrong with them.
    fn draft(&self, installed: Installed) -> Result<Draft, Vec<String>> {
        let command = split_words(&self.command).map_err(|why| vec![why])?;
        let types: Vec<String> = self
            .types
            .split(|c: char| c == ',' || c.is_whitespace())
            .filter(|t| !t.is_empty())
            .map(str::to_string)
            .collect();
        let draft = Draft { label: self.label.trim().to_string(), command, types, selection: self.selection, terminal: self.terminal };
        let problems = draft.problems(&installed.0);
        if problems.is_empty() {
            Ok(draft)
        } else {
            Err(problems)
        }
    }

    fn check(&mut self, installed: Installed) {
        self.problems = self.draft(installed).err().unwrap_or_default();
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Message {
    /// What Automatic would start — looked for off the UI thread when
    /// the sheet opens.
    Found(Option<Terminal>),
    Automatic,
    /// Start typing a terminal of one's own.
    ChooseTerminal,
    TerminalInput(String),
    UseTerminal,
    CancelTerminal,
    Add,
    Edit(String),
    Remove(String),
    RemoveIt,
    KeepIt,
    Label(String),
    Command(String),
    Types(String),
    Selection(Selection),
    InTerminal(bool),
    Save,
    Cancel,
}

impl Launching {
    pub fn new(installed: Installed) -> Launching {
        Launching { installed, ..Launching::default() }
    }

    /// Whether the action editor is open — Escape closes it, rather than
    /// the whole sheet.
    pub fn editing(&self) -> bool {
        self.editor.is_some() || self.terminal_text.is_some() || self.removing.is_some()
    }

    /// Escape: put away whatever is open on the page.
    pub fn cancel(&mut self) {
        self.editor = None;
        self.terminal_text = None;
        self.terminal_problem = None;
        self.removing = None;
    }

    /// A write landed: `ok` when it was made.
    pub fn saved(&mut self, ok: bool) {
        if std::mem::take(&mut self.pending) && ok {
            self.cancel();
        }
    }

    /// Changes the page, and hands back what to write when the change is
    /// one. `writable` is whether the file can take a write now.
    pub fn update(&mut self, message: Message, config: &Config, writable: bool) -> Option<Vec<Edit>> {
        let installed = self.installed;
        let edits = match message {
            Message::Found(terminal) => {
                self.found = Some(terminal);
                None
            }
            Message::Automatic => {
                self.terminal_text = None;
                self.terminal_problem = None;
                // Already Automatic: nothing to take away.
                config.terminal.is_some().then(|| vec![Edit::Terminal(None)])
            }
            Message::ChooseTerminal => {
                let start = config
                    .terminal
                    .clone()
                    .or_else(|| self.found.clone().flatten().map(|t| t.argv))
                    .unwrap_or_default();
                self.terminal_text = Some(join_words(&start));
                self.terminal_problem = terminal_problem(&join_words(&start), installed);
                None
            }
            Message::TerminalInput(text) => {
                self.terminal_problem = terminal_problem(&text, installed);
                self.terminal_text = Some(text);
                None
            }
            Message::UseTerminal => {
                let text = self.terminal_text.clone().unwrap_or_default();
                match (terminal_problem(&text, installed), split_words(&text)) {
                    (None, Ok(words)) if config.terminal.as_ref() == Some(&words) => {
                        self.terminal_text = None;
                        None
                    }
                    (None, Ok(words)) => Some(vec![Edit::Terminal(Some(words))]),
                    (problem, _) => {
                        self.terminal_problem = problem;
                        None
                    }
                }
            }
            Message::CancelTerminal => {
                self.terminal_text = None;
                self.terminal_problem = None;
                None
            }
            Message::Add => {
                let mut editor = Editor::default();
                editor.check(installed);
                self.editor = Some(editor);
                self.removing = None;
                None
            }
            Message::Edit(id) => {
                if let Some(action) = config.actions.iter().find(|a| a.id == id) {
                    let mut editor = Editor {
                        id: Some(action.id.clone()),
                        label: action.label.clone(),
                        command: join_words(&action.command),
                        types: action.types.join(", "),
                        selection: action.selection,
                        terminal: action.terminal,
                        problems: Vec::new(),
                    };
                    editor.check(installed);
                    self.editor = Some(editor);
                    self.removing = None;
                }
                None
            }
            Message::Remove(id) => {
                self.removing = Some(id);
                None
            }
            Message::KeepIt => {
                self.removing = None;
                None
            }
            Message::RemoveIt => self.removing.take().map(|id| vec![Edit::Action(Change::Remove { id })]),
            Message::Label(text) => self.edit(|e| e.label = text),
            Message::Command(text) => self.edit(|e| e.command = text),
            Message::Types(text) => self.edit(|e| e.types = text),
            Message::Selection(selection) => self.edit(|e| e.selection = selection),
            Message::InTerminal(on) => self.edit(|e| e.terminal = on),
            Message::Save => {
                let editor = self.editor.as_ref()?;
                let draft = editor.draft(installed).ok()?;
                Some(vec![Edit::Action(match &editor.id {
                    Some(id) => Change::Replace { id: id.clone(), draft },
                    None => Change::Add(draft),
                })])
            }
            Message::Cancel => {
                self.editor = None;
                None
            }
        };
        let edits = edits.filter(|_| writable);
        if edits.is_some() {
            self.pending = true;
        }
        edits
    }

    fn edit(&mut self, change: impl FnOnce(&mut Editor)) -> Option<Vec<Edit>> {
        let installed = self.installed;
        if let Some(editor) = &mut self.editor {
            change(editor);
            editor.check(installed);
        }
        None
    }

    pub fn view<'a>(
        &'a self,
        config: &'a Config,
        problems: &'a [ConfigProblem],
        writable: bool,
        scale: FontScale,
    ) -> Element<'a, Message> {
        let mut page = column![section_label("Terminal", scale), self.terminal_rows(config, writable, scale)]
            .spacing(spacing::MD);

        page = page.push(section_label("Your actions", scale));
        page = page.push(
            hint_text(
                "Your own commands, in the right-click menus and the command palette (Ctrl+K). \
                 The selected files are added to the end of the command, each as an argument of its \
                 own — it is never run through a shell. From the empty space, the folder in view is \
                 what the action is given.",
                scale,
            )
            .wrapping(iced::widget::text::Wrapping::WordOrGlyph),
        );
        let theirs: Vec<&ConfigProblem> = problems
            .iter()
            .filter(|p| p.message.starts_with("[[action]]") || p.message.starts_with("action:"))
            .collect();
        for problem in theirs {
            page = page.push(warning(problem.message.clone(), scale));
        }
        match &self.editor {
            Some(editor) => page = page.push(self.editor_view(editor, writable, scale)),
            None => {
                let rows = config.actions.iter().enumerate().map(|(i, action)| {
                    let mut hint = join_words(&action.command);
                    if !action.types.is_empty() {
                        hint.push_str(&format!("  \u{b7}  {}", action.types.join(", ")));
                    }
                    hint.push_str(&format!("  \u{b7}  {}", action.selection.label().to_lowercase()));
                    if action.terminal {
                        hint.push_str("  \u{b7}  in a terminal");
                    }
                    let control: Element<'a, Message> = if self.removing.as_deref() == Some(action.id.as_str()) {
                        row![
                            scaled_text("Remove it?", hyprforge_ui::density::META_TEXT_BASE, scale)
                                .color(hyprforge_ui::theme::warning()),
                            primary_button("Remove").on_press_maybe(writable.then_some(Message::RemoveIt)),
                            secondary_button("Keep").on_press(Message::KeepIt),
                        ]
                        .spacing(spacing::SM)
                        .align_y(iced::Alignment::Center)
                        .into()
                    } else {
                        row![
                            secondary_button("Edit").on_press_maybe(writable.then(|| Message::Edit(action.id.clone()))),
                            secondary_button("Remove")
                                .on_press_maybe(writable.then(|| Message::Remove(action.id.clone()))),
                        ]
                        .spacing(spacing::SM)
                        .into()
                    };
                    setting_row(i, action.label.as_str(), Some(config_line(hint, scale).into()), control, scale)
                });
                let rows: Vec<Element<'a, Message>> = rows.collect();
                if rows.is_empty() {
                    page = page.push(hint_text("None yet.", scale));
                } else {
                    page = page.push(setting_list(rows));
                }
                page = page.push(
                    row![primary_button("Add an action").on_press_maybe(writable.then_some(Message::Add))]
                        .spacing(spacing::SM),
                );
            }
        }
        page.into()
    }

    fn terminal_rows<'a>(&'a self, config: &'a Config, writable: bool, scale: FontScale) -> Element<'a, Message> {
        #[derive(Clone, Copy, PartialEq)]
        enum Choice {
            Automatic,
            Chosen,
        }
        let current = if config.terminal.is_some() || self.terminal_text.is_some() { Choice::Chosen } else { Choice::Automatic };
        let choice: Element<'a, Message> = if writable {
            segmented_choice(
                &[Choice::Automatic, Choice::Chosen],
                Some(&current),
                |c| match c {
                    Choice::Automatic => "Automatic".to_string(),
                    Choice::Chosen => "Choose".to_string(),
                },
                |c| match c {
                    Choice::Automatic => Message::Automatic,
                    Choice::Chosen => Message::ChooseTerminal,
                },
                scale,
            )
        } else {
            config_line(if current == Choice::Automatic { "Automatic" } else { "Chosen" }, scale).into()
        };
        let found = match &self.found {
            None => "Looking\u{2026}".to_string(),
            Some(None) => crate::terminal::none_found(),
            Some(Some(terminal)) => format!(
                "Automatic finds {}{}.",
                join_words(&terminal.argv),
                match terminal.source {
                    Source::XdgTerminalExec => " (the desktop's choice)",
                    Source::Env => " (from $TERMINAL)",
                    Source::Known | Source::Configured => "",
                }
            ),
        };
        let hint = match &config.terminal {
            Some(words) if self.terminal_text.is_none() => format!("{} \u{2014} {found}", join_words(words)),
            _ => found,
        };
        let mut rows = vec![setting_row(
            0,
            "Open Terminal Here",
            Some(hint_text(hint, scale).wrapping(iced::widget::text::Wrapping::WordOrGlyph).into()),
            choice,
            scale,
        )];
        if let Some(text) = &self.terminal_text {
            let field = text_input("ghostty", text)
                .on_input(Message::TerminalInput)
                .on_submit(Message::UseTerminal)
                .padding([spacing::XS, spacing::SM])
                .size(scale.apply(BASE_TEXT_SIZE * 0.9))
                .style(hyprforge_ui::widgets::inset_input_style);
            let ok = self.terminal_problem.is_none() && writable;
            let mut under = column![row![
                field,
                primary_button("Use").on_press_maybe(ok.then_some(Message::UseTerminal)),
                secondary_button("Cancel").on_press(Message::CancelTerminal),
            ]
            .spacing(spacing::SM)
            .align_y(iced::Alignment::Center)]
            .spacing(spacing::XS);
            under = under.push(
                hint_text("The program and its options, as words. Files adds where to start, and what to run.", scale)
                    .wrapping(iced::widget::text::Wrapping::WordOrGlyph),
            );
            if let Some(problem) = &self.terminal_problem {
                under = under.push(warning(problem.clone(), scale));
            }
            rows.push(container(under).padding([spacing::XS, spacing::SM]).into());
        }
        setting_list(rows).into()
    }

    fn editor_view<'a>(&'a self, editor: &'a Editor, writable: bool, scale: FontScale) -> Element<'a, Message> {
        let field = |placeholder: &'a str, value: &'a str, on: fn(String) -> Message| {
            text_input(placeholder, value)
                .on_input(on)
                .padding([spacing::XS, spacing::SM])
                .size(scale.apply(BASE_TEXT_SIZE * 0.9))
                .style(hyprforge_ui::widgets::inset_input_style)
                .width(Length::Fill)
        };
        let labelled = |label: &'a str, hint: &'a str, control: Element<'a, Message>| -> Element<'a, Message> {
            column![
                scaled_text(label, hyprforge_ui::density::ROW_TEXT_BASE * 0.9, scale),
                control,
                hint_text(hint, scale).wrapping(iced::widget::text::Wrapping::WordOrGlyph),
            ]
            .spacing(spacing::XS)
            .into()
        };
        let mut card = column![
            labelled("Name", "What the menu calls it.", field("Resize to 50%", &editor.label, Message::Label).into()),
            labelled(
                "Command",
                "The program first, then its options. The selected files are added at the end. \
                 Not a shell: quote a word that has spaces in it.",
                field("magick mogrify -resize 50%", &editor.command, Message::Command).into(),
            ),
            labelled(
                "File types",
                "Offered only on these, like image/* or text/plain, inode/directory for folders. \
                 Empty for anything.",
                field("image/*", &editor.types, Message::Types).into(),
            ),
            labelled(
                "Works on",
                "Greyed in the menu when the selection is another size.",
                segmented_choice(
                    &Selection::ALL,
                    Some(&editor.selection),
                    |s| s.label().to_string(),
                    Message::Selection,
                    scale,
                ),
            ),
            row![
                scaled_text("Run in a terminal", hyprforge_ui::density::ROW_TEXT_BASE * 0.9, scale).width(Length::Fill),
                toggle(editor.terminal, scale).on_toggle(Message::InTerminal),
            ]
            .align_y(iced::Alignment::Center),
        ]
        .spacing(spacing::MD);
        for problem in &editor.problems {
            card = card.push(warning(problem.clone(), scale));
        }
        let ok = editor.problems.is_empty() && writable;
        card = card.push(
            row![
                Space::new().width(Length::Fill),
                secondary_button("Cancel").on_press(Message::Cancel),
                primary_button(if editor.id.is_some() { "Save" } else { "Add" }).on_press_maybe(ok.then_some(Message::Save)),
            ]
            .spacing(spacing::SM),
        );
        container(card)
            .padding(spacing::MD)
            .style(|_t: &iced::Theme| container::Style {
                background: Some(iced::Background::Color(hyprforge_ui::theme::surface::card())),
                border: iced::Border {
                    color: hyprforge_ui::theme::surface::card_border(),
                    width: 1.0,
                    radius: hyprforge_files_core::density::inner_radius().into(),
                },
                ..container::Style::default()
            })
            .into()
    }
}

/// What is wrong with a typed terminal command, if anything.
fn terminal_problem(text: &str, installed: Installed) -> Option<String> {
    match split_words(text) {
        Err(why) => Some(why),
        Ok(words) => match words.first() {
            None => Some("Name the terminal to start, or choose Automatic.".to_string()),
            Some(program) if !(installed.0)(program) => Some(format!("{program} isn't installed, or isn't on your PATH.")),
            Some(_) => None,
        },
    }
}

fn warning<'a>(text: String, scale: FontScale) -> Element<'a, Message> {
    scaled_text(text, hyprforge_ui::density::META_TEXT_BASE, scale)
        .color(hyprforge_ui::theme::warning())
        .wrapping(iced::widget::text::Wrapping::WordOrGlyph)
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use hyprforge_files_core::custom::CustomAction;
    use std::path::Path;

    fn installed(program: &str) -> bool {
        matches!(program, "magick" | "kitty" | "ghostty" | "code")
    }

    fn page() -> Launching {
        Launching::new(Installed(installed))
    }

    fn config(text: &str) -> Config {
        let (config, problems) = hyprforge_files_core::config::parse(text, Path::new("x"));
        assert!(problems.is_empty(), "{problems:?}");
        config
    }

    /// What a page's edits make of `text` — the line the control claims
    /// to write, checked by writing it.
    fn written(text: &str, edits: &[Edit]) -> String {
        hyprforge_files_core::config_edit::apply(text, edits, Path::new("x")).unwrap()
    }

    #[test]
    fn choosing_a_terminal_writes_exactly_its_line() {
        let mut page = page();
        let config = config("");
        page.update(Message::ChooseTerminal, &config, true);
        page.update(Message::TerminalInput("kitty --single-instance".into()), &config, true);
        let edits = page.update(Message::UseTerminal, &config, true).expect("a write");
        assert_eq!(written("", &edits), "[behaviour]\nterminal = [\"kitty\", \"--single-instance\"]\n");
    }

    #[test]
    fn automatic_takes_the_line_away_and_writes_nothing_when_already_automatic() {
        let mut page = page();
        let chosen = config("[behaviour]\nterminal = [\"kitty\"]\n");
        let edits = page.update(Message::Automatic, &chosen, true).expect("a write");
        assert_eq!(written("[behaviour]\nterminal = [\"kitty\"]\n", &edits), "[behaviour]\n");
        assert_eq!(page.update(Message::Automatic, &config(""), true), None);
    }

    #[test]
    fn a_terminal_that_is_not_installed_is_said_and_not_written() {
        let mut page = page();
        let config = config("");
        page.update(Message::ChooseTerminal, &config, true);
        page.update(Message::TerminalInput("urxvt".into()), &config, true);
        assert_eq!(page.update(Message::UseTerminal, &config, true), None);
        assert!(page.terminal_problem.as_deref().is_some_and(|p| p.contains("urxvt")));
    }

    /// The field starts from what the file says — a line written by hand
    /// is what the sheet shows.
    #[test]
    fn a_terminal_written_by_hand_is_where_the_field_starts() {
        let mut page = page();
        page.update(Message::ChooseTerminal, &config("[behaviour]\nterminal = [\"ghostty\", \"--title=x y\"]\n"), true);
        assert_eq!(page.terminal_text.as_deref(), Some("ghostty \"--title=x y\""));
    }

    #[test]
    fn adding_an_action_writes_one_block_with_its_fields() {
        let mut page = page();
        let config = config("");
        page.update(Message::Add, &config, true);
        page.update(Message::Label("Shrink".into()), &config, true);
        page.update(Message::Command("magick mogrify -resize \"50%\"".into()), &config, true);
        page.update(Message::Types("image/png, image/jpeg".into()), &config, true);
        page.update(Message::Selection(Selection::Many), &config, true);
        let edits = page.update(Message::Save, &config, true).expect("a write");
        let out = written("", &edits);
        let loaded = hyprforge_files_core::config::parse(&out, Path::new("x")).0;
        assert_eq!(
            loaded.actions,
            [CustomAction {
                id: "shrink".into(),
                label: "Shrink".into(),
                command: ["magick", "mogrify", "-resize", "50%"].map(String::from).to_vec(),
                types: vec!["image/png".into(), "image/jpeg".into()],
                selection: Selection::Many,
                terminal: false,
            }]
        );
        page.saved(true);
        assert!(!page.editing(), "the editor closes once it is written");
    }

    #[test]
    fn an_action_with_problems_cannot_be_saved_and_says_why() {
        let mut page = page();
        let config = config("");
        page.update(Message::Add, &config, true);
        assert!(!page.editor.as_ref().unwrap().problems.is_empty(), "empty is not saveable");
        page.update(Message::Label("Odd".into()), &config, true);
        page.update(Message::Command("frobnicate".into()), &config, true);
        page.update(Message::Types("pictures".into()), &config, true);
        let problems = page.editor.as_ref().unwrap().problems.clone();
        assert!(problems.iter().any(|p| p.contains("frobnicate")), "{problems:?}");
        assert!(problems.iter().any(|p| p.contains("pictures")), "{problems:?}");
        assert_eq!(page.update(Message::Save, &config, true), None);
        page.update(Message::Command("magick \"unclosed".into()), &config, true);
        assert!(page.editor.as_ref().unwrap().problems[0].contains("never closed"));
    }

    /// A block written by hand opens in the editor as written, and saving
    /// it changes that block by its id.
    #[test]
    fn an_action_written_by_hand_is_edited_in_place() {
        let text = "# mine\n[[action]]\nlabel = \"Code here\"  # handy\ncommand = [\"code\"]\ntypes = [\"inode/directory\"]\n";
        let mut page = page();
        let config = config(text);
        page.update(Message::Edit("code-here".into()), &config, true);
        let editor = page.editor.as_ref().unwrap();
        assert_eq!((editor.label.as_str(), editor.command.as_str(), editor.types.as_str()), ("Code here", "code", "inode/directory"));
        page.update(Message::InTerminal(true), &config, true);
        let edits = page.update(Message::Save, &config, true).unwrap();
        let out = written(text, &edits);
        assert!(out.starts_with("# mine\n") && out.contains("# handy"), "{out}");
        assert!(out.contains("terminal = true"), "{out}");
    }

    #[test]
    fn removing_asks_first_and_then_writes_the_blocks_removal() {
        let text = "[[action]]\nlabel = \"Go\"\ncommand = [\"code\"]\n";
        let mut page = page();
        let config = config(text);
        assert_eq!(page.update(Message::Remove("go".into()), &config, true), None);
        assert_eq!(page.update(Message::KeepIt, &config, true), None);
        page.update(Message::Remove("go".into()), &config, true);
        let edits = page.update(Message::RemoveIt, &config, true).unwrap();
        assert_eq!(written(text, &edits), "");
    }

    #[test]
    fn nothing_is_written_while_the_file_cannot_take_it() {
        let mut page = page();
        let config = config("[behaviour]\nterminal = [\"kitty\"]\n");
        assert_eq!(page.update(Message::Automatic, &config, false), None);
    }
}
