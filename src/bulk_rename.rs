//! The bulk rename sheet: F2 with several things selected.
//!
//! What a rule does to a name, and what is wrong with the result, is
//! decided in `hyprforge_files_core::bulk_rename`, where it is tested
//! without a window; the order the renames run in — swaps included — and
//! putting things back when one fails are `hyprforge_fileops::batch`'s.
//! This is the sheet: its state, what its buttons ask the window to do
//! ([`Effect`]), and how it is drawn.
//!
//! A sheet over the window, like Preferences, rather than a second
//! window: Hyprland would tile a second toplevel and rearrange the
//! person's layout to show a form about the window they were already
//! looking at.
//!
//! The preview is worked out again on every keystroke, whole. It is a
//! pure function over a few thousand names at most and measured at a
//! fraction of a frame for ten thousand (the core module's own test), so
//! caching parts of it would buy nothing but a way to show stale rows.

use hyprforge_files_core::bulk_rename::{
    preview, CaseChange, Mode, Place, Preview, Request, Rules, CASES, MODES,
};
use hyprforge_ui::theme::{spacing, surface, FontScale, BASE_TEXT_SIZE};
use hyprforge_ui::widgets::{
    change_row, hint_text, inset_input_style, meta_text, page_header, primary_button, scaled_text,
    secondary_button, segmented_choice, toggle, Change,
};
use iced::widget::{column, container, row, scrollable, text_input, Id, Space};
use iced::{Background, Border, Color, Element, Length};
use std::path::PathBuf;

/// How many preview rows are drawn. iced lays out every row of a
/// `column` on every frame whether it is scrolled into view or not, and
/// the preview is rebuilt on every keystroke; past this the sheet says
/// how many more there are, and how many of *those* have problems, so a
/// clash below the fold is never invisible.
pub const SHOWN_ROWS: usize = 300;

/// The sheet's text fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Field {
    Find,
    ReplaceWith,
    Add,
    Template,
    Start,
    Step,
}

const FIELDS: usize = 6;

impl Field {
    /// The fields `mode` shows, in the order Tab visits them.
    fn of(mode: Mode) -> &'static [Field] {
        match mode {
            Mode::Replace => &[Field::Find, Field::ReplaceWith],
            Mode::Add => &[Field::Add],
            Mode::Number => &[Field::Template, Field::Start, Field::Step],
            Mode::Case => &[],
        }
    }
}

/// The sheet, while it is open.
#[derive(Debug, Clone)]
pub struct BulkRename {
    request: Request,
    rules: Rules,
    preview: Preview,
    /// Each text field's identity, held for the sheet's life so that
    /// focus can be moved to one by name.
    ids: [Id; FIELDS],
    /// The field last typed in or moved to — where Tab moves on from.
    /// iced cannot be asked which field has focus, so this is the
    /// sheet's own record; a field clicked and not typed in leaves it
    /// one behind, and Tab then goes from the last one typed in.
    focused: Field,
    /// A rename is running. Everything is off meanwhile, so the rules
    /// cannot change under the renames they produced.
    applying: bool,
    /// Why the last attempt failed, when it changed nothing and the
    /// sheet stayed open to try again.
    note: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Message {
    Mode(Mode),
    Find(String),
    ReplaceWith(String),
    MatchCase(bool),
    Regex(bool),
    Add(String),
    AddAt(Place),
    Template(String),
    Start(String),
    Step(String),
    Case(CaseChange),
    WholeName(bool),
    /// Tab (`true`) or Shift+Tab: the next or previous field of the
    /// mode showing, round to the first after the last. Only the sheet's
    /// own fields — iced's own `focus_next` walks the whole window, and
    /// would carry the cursor into the search box behind the sheet.
    NextField(bool),
    /// Enter in a field, or the Rename button.
    Apply,
    Cancel,
}

/// What the window has to do about a message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    None,
    Close,
    /// Focus this field — the sheet opened, or the mode changed.
    Focus(Id),
    /// Carry these out, as `(from, to)`, all or none.
    Apply(Vec<(PathBuf, PathBuf)>),
}

impl BulkRename {
    /// The sheet over `request`, and the field to focus.
    pub fn new(request: Request) -> (BulkRename, Id) {
        let rules = Rules::default();
        let preview = preview(&request, &rules);
        let ids = std::array::from_fn(|_| Id::unique());
        let sheet = BulkRename { request, rules, preview, ids, focused: Field::Find, applying: false, note: None };
        let first = sheet.id(Field::Find);
        (sheet, first)
    }

    fn id(&self, field: Field) -> Id {
        self.ids[field as usize].clone()
    }

    /// What the sheet was opened over.
    pub fn request(&self) -> &Request {
        &self.request
    }

    pub fn applying(&self) -> bool {
        self.applying
    }

    /// What the preview says now — for the window's tests.
    pub fn preview(&self) -> &Preview {
        &self.preview
    }

    /// The window's answer to [`Effect::Apply`] failed and changed
    /// nothing: say why, and let the person change the rules and try
    /// again.
    pub fn failed(&mut self, why: String) {
        self.applying = false;
        self.note = Some(why);
    }

    pub fn update(&mut self, message: Message) -> Effect {
        if self.applying {
            return Effect::None;
        }
        let rules = &mut self.rules;
        match message {
            Message::Cancel => return Effect::Close,
            Message::Apply => {
                return match self.preview.renames() {
                    Ok(renames) => {
                        self.applying = true;
                        self.note = None;
                        Effect::Apply(renames)
                    }
                    // The button is off whenever this is an error; Enter
                    // in a field gets here anyway, and says why.
                    Err(why) => {
                        self.note = Some(why);
                        Effect::None
                    }
                };
            }
            Message::Mode(mode) => {
                if rules.mode == mode {
                    return Effect::None;
                }
                rules.mode = mode;
                self.refresh();
                return match Field::of(mode).first() {
                    Some(&first) => {
                        self.focused = first;
                        Effect::Focus(self.id(first))
                    }
                    None => Effect::None,
                };
            }
            Message::NextField(forward) => {
                let fields = Field::of(rules.mode);
                if fields.is_empty() {
                    return Effect::None;
                }
                let at = fields.iter().position(|f| *f == self.focused);
                let next = match (at, forward) {
                    (None, _) => 0,
                    (Some(i), true) => (i + 1) % fields.len(),
                    (Some(i), false) => (i + fields.len() - 1) % fields.len(),
                };
                self.focused = fields[next];
                return Effect::Focus(self.id(self.focused));
            }
            Message::Find(text) => {
                rules.find = text;
                self.focused = Field::Find;
            }
            Message::ReplaceWith(text) => {
                rules.replace_with = text;
                self.focused = Field::ReplaceWith;
            }
            Message::MatchCase(on) => rules.match_case = on,
            Message::Regex(on) => rules.regex = on,
            Message::Add(text) => {
                rules.add = text;
                self.focused = Field::Add;
            }
            Message::AddAt(place) => rules.add_at = place,
            Message::Template(text) => {
                rules.template = text;
                self.focused = Field::Template;
            }
            Message::Start(text) => {
                rules.start = text;
                self.focused = Field::Start;
            }
            Message::Step(text) => {
                rules.step = text;
                self.focused = Field::Step;
            }
            Message::Case(change) => rules.case = change,
            Message::WholeName(on) => rules.whole_name = on,
        }
        self.refresh();
        Effect::None
    }

    fn refresh(&mut self) {
        self.preview = preview(&self.request, &self.rules);
        self.note = None;
    }

    pub fn view(&self, scale: FontScale) -> Element<'_, Message> {
        let count = self.request.items.len();
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let folder = self
            .request
            .folder()
            .map(|f| hyprforge_files_core::format::tilde_path(f, home.as_deref()));

        let modes = segmented_choice(
            &MODES.map(|(mode, _)| mode),
            Some(&self.rules.mode),
            |mode| MODES.iter().find(|(m, _)| m == mode).map(|(_, l)| l.to_string()).unwrap_or_default(),
            Message::Mode,
            scale,
        );

        let mut body = column![
            row![
                page_header(format!("Rename {count} items"), folder, scale),
                Space::new().width(Length::Fill),
            ]
            .align_y(iced::Alignment::Center),
            row![modes, hint_text("Ctrl+1\u{2013}4 to switch \u{00B7} Tab between fields", scale)]
                .spacing(spacing::MD)
                .align_y(iced::Alignment::Center),
            self.controls(scale),
            switch(self.rules.whole_name, "Include the extension", Message::WholeName, scale),
        ]
        .spacing(spacing::MD);

        if let Some(error) = &self.preview.error {
            body = body.push(warning(error.clone(), scale));
        }
        body = body.push(meta_text(self.summary(), BASE_TEXT_SIZE * 0.9, scale));

        let mut rows = column![].spacing(spacing::SM);
        for row in self.preview.rows.iter().take(SHOWN_ROWS) {
            let change = match (row.changed(), &row.problem) {
                (false, _) => Change::Unchanged,
                (true, None) => Change::Changes,
                (true, Some(_)) => Change::Refused,
            };
            rows = rows.push(change_row(
                row.old.as_str(),
                row.new.as_str(),
                change,
                row.problem.as_ref().map(|p| p.describe()),
                scale,
            ));
        }
        if let Some(more) = self.unshown() {
            rows = rows.push(hint_text(more, scale));
        }
        body = body.push(
            container(scrollable(container(rows).padding([0.0, spacing::MD])).height(Length::Fill))
                .height(Length::Fill)
                .padding([spacing::SM, 0.0])
                .style(|_t: &iced::Theme| container::Style {
                    background: Some(Background::Color(surface::card())),
                    border: Border {
                        color: surface::card_border(),
                        width: 1.0,
                        radius: hyprforge_ui::density::inner_radius().into(),
                    },
                    ..container::Style::default()
                }),
        );

        let ready = self.preview.renames();
        let label = match (&ready, self.applying) {
            (_, true) => "Renaming\u{2026}".to_string(),
            (Ok(renames), false) if renames.len() == 1 => "Rename 1 item".to_string(),
            (Ok(renames), false) => format!("Rename {} items", renames.len()),
            (Err(_), false) => "Rename".to_string(),
        };
        let mut confirm = primary_button(label);
        if ready.is_ok() && !self.applying {
            confirm = confirm.on_press(Message::Apply);
        }
        let mut cancel = secondary_button("Cancel");
        if !self.applying {
            cancel = cancel.on_press(Message::Cancel);
        }
        let mut footer = row![].spacing(spacing::SM).align_y(iced::Alignment::Center);
        footer = match &self.note {
            Some(note) => footer.push(container(warning(note.clone(), scale)).width(Length::Fill)),
            None => footer.push(Space::new().width(Length::Fill)),
        };
        body = body.push(footer.push(cancel).push(confirm));

        let card = container(body)
            .padding(spacing::LG)
            .max_width(scale.apply(820.0))
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
                // dialog here is.
                .style(|_t: &iced::Theme| container::Style {
                    background: Some(Background::Color(Color { a: 0.6, ..surface::root() })),
                    ..container::Style::default()
                }),
        )
    }

    /// The fields for the mode showing.
    fn controls(&self, scale: FontScale) -> Element<'_, Message> {
        let rules = &self.rules;
        match rules.mode {
            Mode::Replace => column![
                row![
                    labelled("Find", field(&self.id(Field::Find), "Text to find", &rules.find, Message::Find, scale), scale),
                    labelled(
                        "Replace with",
                        field(&self.id(Field::ReplaceWith), "Nothing, to remove it", &rules.replace_with, Message::ReplaceWith, scale),
                        scale
                    ),
                ]
                .spacing(spacing::MD),
                row![
                    switch(rules.match_case, "Match case", Message::MatchCase, scale),
                    switch(rules.regex, "Regular expression", Message::Regex, scale),
                ]
                .spacing(spacing::LG),
                hint_text(
                    if rules.regex {
                        "$1, $2\u{2026} in the replacement put back what each group matched."
                    } else {
                        "Every match in each name is replaced."
                    },
                    scale
                ),
            ]
            .spacing(spacing::SM)
            .into(),
            Mode::Add => column![
                labelled("Text", field(&self.id(Field::Add), "Text to add", &rules.add, Message::Add, scale), scale),
                segmented_choice(
                    &[Place::Start, Place::End],
                    Some(&rules.add_at),
                    |place| match place {
                        Place::Start => "Before the name".to_string(),
                        Place::End if rules.whole_name => "After the extension".to_string(),
                        Place::End => "Before the extension".to_string(),
                    },
                    Message::AddAt,
                    scale,
                ),
            ]
            .spacing(spacing::SM)
            .into(),
            Mode::Number => column![
                row![
                    container(labelled(
                        "Name",
                        field(&self.id(Field::Template), "Holiday {n:03}", &rules.template, Message::Template, scale),
                        scale
                    ))
                    .width(Length::FillPortion(3)),
                    container(labelled("Start at", field(&self.id(Field::Start), "1", &rules.start, Message::Start, scale), scale))
                        .width(Length::FillPortion(1)),
                    container(labelled("Step", field(&self.id(Field::Step), "1", &rules.step, Message::Step, scale), scale))
                        .width(Length::FillPortion(1)),
                ]
                .spacing(spacing::MD),
                hint_text(
                    "{n} is the number, {n:3} pads it to three digits, {name} is the old name. Counted in the order the folder is sorted.",
                    scale
                ),
            ]
            .spacing(spacing::SM)
            .into(),
            Mode::Case => segmented_choice(
                &CASES.map(|(c, _)| c),
                Some(&rules.case),
                |change| CASES.iter().find(|(c, _)| c == change).map(|(_, l)| l.to_string()).unwrap_or_default(),
                Message::Case,
                scale,
            ),
        }
    }

    /// "8 of 12 will change · 2 problems".
    fn summary(&self) -> String {
        let total = self.preview.rows.len();
        let changes = self.preview.changes();
        let mut said = match changes {
            0 => "Nothing would change".to_string(),
            n if n == total => format!("All {total} will change"),
            n => format!("{n} of {total} will change"),
        };
        match self.preview.problems() {
            0 => {}
            1 => said.push_str(" \u{00B7} 1 problem"),
            n => said.push_str(&format!(" \u{00B7} {n} problems")),
        }
        said
    }

    /// What the rows past [`SHOWN_ROWS`] hold, when there are any.
    fn unshown(&self) -> Option<String> {
        let rest = self.preview.rows.get(SHOWN_ROWS..)?;
        if rest.is_empty() {
            return None;
        }
        let problems = rest.iter().filter(|r| r.problem.is_some()).count();
        Some(match problems {
            0 => format!("\u{2026} and {} more, not shown.", rest.len()),
            n => format!("\u{2026} and {} more, not shown \u{2014} {n} of them with problems.", rest.len()),
        })
    }
}

/// A text field in the sheet's look. Enter applies, from any of them.
fn field<'a>(
    id: &Id,
    placeholder: &str,
    value: &str,
    on_input: fn(String) -> Message,
    scale: FontScale,
) -> Element<'a, Message> {
    text_input(placeholder, value)
        .id(id.clone())
        .on_input(on_input)
        .on_submit(Message::Apply)
        .padding([spacing::XS, spacing::SM])
        .size(scale.apply(BASE_TEXT_SIZE))
        .style(inset_input_style)
        .into()
}

/// A field with its label over it.
fn labelled<'a>(label: &'a str, field: Element<'a, Message>, scale: FontScale) -> Element<'a, Message> {
    column![meta_text(label, BASE_TEXT_SIZE * 0.85, scale), field].spacing(spacing::XS).width(Length::Fill).into()
}

/// A switch with its words beside it.
fn switch<'a>(on: bool, label: &'a str, make: fn(bool) -> Message, scale: FontScale) -> Element<'a, Message> {
    row![toggle(on, scale).on_toggle(make), scaled_text(label, BASE_TEXT_SIZE * 0.9, scale)]
        .spacing(spacing::SM)
        .align_y(iced::Alignment::Center)
        .into()
}

fn warning<'a>(text: String, scale: FontScale) -> Element<'a, Message> {
    scaled_text(text, hyprforge_ui::density::META_TEXT_BASE, scale)
        .color(hyprforge_ui::theme::warning())
        .wrapping(iced::widget::text::Wrapping::WordOrGlyph)
        .into()
}

/// A failed batch, in words: what failed, and — the part that matters —
/// what state it left things in. Every name that is not where it was is
/// named, with where it is now.
pub fn describe_failure(failure: &hyprforge_fileops::batch::Failure) -> String {
    if was_interrupted(failure) {
        return format!(
            "Renaming was interrupted ({}), so what it got through is not known \u{2014} the folder shows what is there now.",
            failure.reason
        );
    }
    let name = |p: &std::path::Path| {
        p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| p.display().to_string())
    };
    let (from, _) = &failure.step;
    let mut said = if failure.before_starting {
        format!("Nothing was renamed: {}.", failure.reason)
    } else if failure.nothing_changed() {
        format!(
            "Nothing was renamed: \u{201C}{}\u{201D} couldn't be renamed ({}), so everything already renamed was put back.",
            name(from),
            failure.reason
        )
    } else {
        format!(
            "Renaming stopped at \u{201C}{}\u{201D} ({}), and not everything could be put back.",
            name(from),
            failure.reason
        )
    };
    for (was, now) in &failure.renamed {
        said.push_str(&format!(" \u{201C}{}\u{201D} is now \u{201C}{}\u{201D}.", name(was), name(now)));
    }
    for (was, now) in &failure.stranded {
        said.push_str(&format!(
            " \u{201C}{}\u{201D} is under the hidden name \u{201C}{}\u{201D}.",
            name(was),
            name(now)
        ));
    }
    if !failure.nothing_changed() {
        said.push_str(" Everything else is as it was.");
    }
    said
}

/// The thread running a batch died before it could say how far it got.
///
/// Not a failure `batch` reports — it never panics by design — but the
/// one case where nobody knows what state the folder is in, which is
/// worth a sentence that says exactly that rather than a guess. Marked
/// by an empty step, which no real rename has.
pub fn interrupted(reason: String) -> Box<hyprforge_fileops::batch::Failure> {
    Box::new(hyprforge_fileops::batch::Failure {
        step: (PathBuf::new(), PathBuf::new()),
        reason,
        before_starting: false,
        renamed: Vec::new(),
        stranded: Vec::new(),
    })
}

/// Whether `failure` is [`interrupted`]'s.
pub fn was_interrupted(failure: &hyprforge_fileops::batch::Failure) -> bool {
    failure.step.0.as_os_str().is_empty()
}

/// Carries out a bulk rename on the disk. Blocking.
///
/// Not a queued job, and that is measured rather than assumed: 5,000
/// renames in a rotation — the worst shape, a cycle through a temporary
/// name — took 185 ms on a btrfs home in a debug build, the check
/// against the disk included. A progress bar that appears for a fifth
/// of a second is noise, and a Cancel halfway would have to put back
/// everything already done anyway, which is what a failure does. It
/// cannot write over anything a paste is placing at the same moment:
/// every rename refuses to replace (see `hyprforge_fileops::batch`).
pub fn run(renames: Vec<(PathBuf, PathBuf)>) -> Result<Vec<(PathBuf, PathBuf)>, Box<hyprforge_fileops::batch::Failure>> {
    hyprforge_fileops::batch::apply(&renames)
}

/// The same renames inside an archive, as the edits one rewrite applies
/// in order — swaps broken by a temporary member name exactly as on a
/// disk, because an archive's table takes renames one after another
/// too, and a swap applied naively collapses both members onto one name.
///
/// `neighbours` are the names the listing holds; a temporary name is
/// chosen clear of them. `None` if a path is not inside an archive, or
/// the renames cannot be planned at all.
pub fn archive_edits(
    renames: &[(PathBuf, PathBuf)],
    neighbours: &[PathBuf],
) -> Option<(PathBuf, Vec<hyprforge_archive::Edit>)> {
    let taken: std::collections::HashSet<&std::path::Path> = neighbours.iter().map(PathBuf::as_path).collect();
    let steps = hyprforge_fileops::batch::plan(renames, |p| taken.contains(p)).ok()?;
    let mut archive = None;
    let mut edits = Vec::with_capacity(steps.len());
    for step in steps {
        let (Some((from_archive, from)), Some((to_archive, to))) = (
            hyprforge_files_core::archive::split(&step.from),
            hyprforge_files_core::archive::split(&step.to),
        ) else {
            return None;
        };
        if from_archive != to_archive || archive.as_ref().is_some_and(|a| a != &from_archive) {
            return None;
        }
        archive = Some(from_archive);
        edits.push(hyprforge_archive::Edit::Rename { from, to });
    }
    Some((archive?, edits))
}

#[cfg(test)]
mod tests {
    use super::*;
    use hyprforge_files_core::bulk_rename::Item;

    fn request(names: &[&str]) -> Request {
        let items: Vec<Item> = names
            .iter()
            .map(|n| Item { path: PathBuf::from("/d").join(n), name: n.to_string(), is_dir: false })
            .collect();
        Request { neighbours: items.iter().map(|i| i.path.clone()).collect(), items }
    }

    #[test]
    fn apply_hands_over_only_what_changes() {
        let (mut sheet, _) = BulkRename::new(request(&["a.txt", "b.txt"]));
        sheet.update(Message::Find("a".into()));
        sheet.update(Message::ReplaceWith("c".into()));
        assert_eq!(
            sheet.update(Message::Apply),
            Effect::Apply(vec![(PathBuf::from("/d/a.txt"), PathBuf::from("/d/c.txt"))])
        );
        assert!(sheet.applying());
    }

    /// Enter in a field with a clash showing does not apply — it says
    /// why, where the person is looking.
    #[test]
    fn apply_is_refused_while_a_row_has_a_problem() {
        let (mut sheet, _) = BulkRename::new(request(&["a.txt", "b.txt"]));
        sheet.update(Message::Mode(Mode::Number));
        sheet.update(Message::Template("same".into()));
        assert_eq!(sheet.update(Message::Apply), Effect::None);
        assert!(!sheet.applying());
        assert_eq!(sheet.note.as_deref(), Some("2 names have problems."));
    }

    /// Nothing may change the rules under a rename that is running.
    #[test]
    fn the_rules_are_frozen_while_renaming() {
        let (mut sheet, _) = BulkRename::new(request(&["a", "b"]));
        sheet.update(Message::Mode(Mode::Case));
        sheet.update(Message::Case(CaseChange::Upper));
        assert!(matches!(sheet.update(Message::Apply), Effect::Apply(_)));
        sheet.update(Message::Case(CaseChange::Lower));
        assert_eq!(sheet.rules.case, CaseChange::Upper);
        assert_eq!(sheet.update(Message::Cancel), Effect::None, "and it cannot be closed under it");
    }

    /// Tab goes round the mode's own fields and nowhere else.
    #[test]
    fn tab_goes_round_the_fields_of_the_mode_showing() {
        let (mut sheet, first) = BulkRename::new(request(&["a"]));
        assert_eq!(first, sheet.id(Field::Find));
        assert_eq!(sheet.update(Message::NextField(true)), Effect::Focus(sheet.id(Field::ReplaceWith)));
        assert_eq!(sheet.update(Message::NextField(true)), Effect::Focus(sheet.id(Field::Find)), "round again");
        assert_eq!(sheet.update(Message::NextField(false)), Effect::Focus(sheet.id(Field::ReplaceWith)));
        sheet.update(Message::Mode(Mode::Number));
        assert_eq!(sheet.update(Message::NextField(true)), Effect::Focus(sheet.id(Field::Start)));
        sheet.update(Message::Mode(Mode::Case));
        assert_eq!(sheet.update(Message::NextField(true)), Effect::None, "case has no fields");
    }

    /// Looking at another mode and coming back loses nothing typed.
    #[test]
    fn switching_modes_keeps_what_was_typed() {
        let (mut sheet, _) = BulkRename::new(request(&["a"]));
        sheet.update(Message::Find("x".into()));
        assert!(matches!(sheet.update(Message::Mode(Mode::Add)), Effect::Focus(_)));
        sheet.update(Message::Mode(Mode::Replace));
        assert_eq!(sheet.rules.find, "x");
    }

    #[test]
    fn a_failure_that_changed_nothing_says_so() {
        let failure = hyprforge_fileops::batch::Failure {
            step: ("/d/c".into(), "/d/c2".into()),
            reason: "you don't have permission to rename things there".into(),
            before_starting: false,
            renamed: vec![],
            stranded: vec![],
        };
        let said = describe_failure(&failure);
        assert!(said.starts_with("Nothing was renamed"), "{said}");
        assert!(said.contains("put back"), "{said}");
    }

    /// The case that must never be silent: something left under a
    /// temporary name is named, with the name it is under.
    #[test]
    fn a_failure_names_everything_not_where_it_was() {
        let failure = hyprforge_fileops::batch::Failure {
            step: ("/d/.a.renaming".into(), "/d/b".into()),
            reason: "it isn't there any more".into(),
            before_starting: false,
            renamed: vec![("/d/x".into(), "/d/y".into())],
            stranded: vec![("/d/a".into(), "/d/.a.renaming".into())],
        };
        let said = describe_failure(&failure);
        assert!(said.contains("\u{201C}x\u{201D} is now \u{201C}y\u{201D}"), "{said}");
        assert!(said.contains("\u{201C}a\u{201D} is under the hidden name \u{201C}.a.renaming\u{201D}"), "{said}");
        assert!(said.ends_with("Everything else is as it was."), "{said}");
    }

    /// A swap inside an archive becomes three edits in one rewrite —
    /// and applied in order to the archive's table they really swap.
    #[test]
    fn a_swap_inside_an_archive_is_one_rewrite_that_really_swaps() {
        // `archive::split` asks the disk whether the archive is a file.
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().join("notes.zip");
        std::fs::write(&base, b"").unwrap();
        let (a, b) = (base.join("a.txt"), base.join("b.txt"));
        let (archive, edits) =
            archive_edits(&[(a.clone(), b.clone()), (b.clone(), a.clone())], &[a.clone(), b.clone()]).unwrap();
        assert_eq!(archive, base);
        assert_eq!(edits.len(), 3);
        let mut members: Vec<(hyprforge_archive::Member, &str)> = vec![
            (hyprforge_archive::Member::file("a.txt", 5), "was a"),
            (hyprforge_archive::Member::file("b.txt", 5), "was b"),
        ];
        for edit in &edits {
            hyprforge_archive::write::apply_to_list(&mut members, edit);
        }
        let find = |name: &str| members.iter().find(|(m, _)| m.path == name).map(|(_, c)| *c);
        assert_eq!(find("a.txt"), Some("was b"));
        assert_eq!(find("b.txt"), Some("was a"));
    }
}
