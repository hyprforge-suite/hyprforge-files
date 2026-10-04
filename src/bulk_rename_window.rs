//! The window's half of bulk rename: opening the sheet, carrying out
//! what it asks for, and hearing how that went.
//!
//! A module of its own rather than more arms in `App::update`, so the
//! whole feature can be read in two files — this and
//! `hyprforge_files::bulk_rename` — and so `main.rs` only gains the
//! hooks that route to it.
//!
//! Two roads, by where the items are:
//!
//! - **On a disk**, the renames run on a blocking thread through
//!   `hyprforge_fileops::batch` — all or none, every one refusing to
//!   replace anything — and the sheet waits, frozen, for the answer.
//!   Why not the job queue: see `hyprforge_files::bulk_rename::run`.
//! - **Inside an archive**, every rename is one edit of a single rewrite,
//!   queued like any other archive edit so that nothing else rewriting
//!   the same archive can run beside it and lose one of the two changes.
//!   No undo, for the reason `hyprforge_files_core::undo` gives for every
//!   archive edit.

use super::{App, At, Message};
use hyprforge_files::bulk_rename::{self, BulkRename, Effect};
use hyprforge_files_core::browser::Message as BrowserMessage;
use hyprforge_files_core::bulk_rename::Request;
use iced::Task;
use std::path::PathBuf;

/// What a key the sheet hears means to it: Escape backs out, Tab and
/// Shift+Tab move between its fields, Ctrl+1 to Ctrl+4 pick the mode —
/// the mode buttons are not focusable in iced, so without these the
/// sheet would need the mouse. Everything else is swallowed: nothing
/// reaches the listing behind the sheet.
pub(super) fn sheet_key(press: &hyprforge_files_core::keymap::KeyPress) -> Option<bulk_rename::Message> {
    use hyprforge_files_core::bulk_rename::MODES;
    use hyprforge_files_core::keymap::Key;
    let mods = press.mods;
    match press.key {
        Key::Escape => Some(bulk_rename::Message::Cancel),
        Key::Tab if !mods.ctrl && !mods.alt => Some(bulk_rename::Message::NextField(!mods.shift)),
        Key::Char(c @ '1'..='4') if mods.ctrl && !mods.alt && !mods.shift => {
            let index = c as usize - '1' as usize;
            MODES.get(index).map(|(mode, _)| bulk_rename::Message::Mode(*mode))
        }
        _ => None,
    }
}

impl App {
    /// Opens the sheet over `request`, for the pane at `at`.
    pub(super) fn open_bulk_rename(&mut self, at: At, request: Request) -> Task<Message> {
        let (sheet, field) = BulkRename::new(request);
        self.bulk_rename = Some((self.pane(at).id, sheet));
        iced::widget::operation::focus(field)
    }

    pub(super) fn bulk_rename_message(&mut self, message: bulk_rename::Message) -> Task<Message> {
        let Some((tab_id, sheet)) = &mut self.bulk_rename else { return Task::none() };
        let tab_id = *tab_id;
        match sheet.update(message) {
            Effect::None => Task::none(),
            Effect::Close => {
                self.bulk_rename = None;
                Task::none()
            }
            Effect::Focus(id) => iced::widget::operation::focus(id),
            Effect::Apply(renames) => self.apply_bulk_rename(tab_id, renames),
        }
    }

    fn apply_bulk_rename(&mut self, tab_id: u64, renames: Vec<(PathBuf, PathBuf)>) -> Task<Message> {
        let in_archive = renames.first().is_some_and(|(from, _)| hyprforge_files_core::archive::split(from).is_some());
        if in_archive {
            let neighbours = self.bulk_rename.as_ref().map(|(_, s)| s.request().neighbours.clone()).unwrap_or_default();
            self.bulk_rename = None;
            let Some((archive, edits)) = bulk_rename::archive_edits(&renames, &neighbours) else {
                self.status = Some("Those names can't be used inside this archive.".to_string());
                return Task::none();
            };
            if let Some(at) = self.locate(tab_id) {
                let renamed = renames.into_iter().map(|(_, to)| to).collect();
                self.pane_mut(at).browser.update(BrowserMessage::SelectWhenListed(renamed));
            }
            return self.start_archive_job(hyprforge_files::archive_jobs::Work::Edit { archive, edits });
        }
        Task::perform(
            async move {
                tokio::task::spawn_blocking(move || bulk_rename::run(renames))
                    .await
                    .unwrap_or_else(|e| Err(bulk_rename::interrupted(e.to_string())))
            },
            move |result| Message::BulkRenamed(tab_id, result),
        )
    }

    /// A disk rename's answer. Done: close, select the new names, offer
    /// the undo. Refused with nothing changed: keep the sheet, say why,
    /// let the rules be changed. Anything else: close, and say in the
    /// status bar exactly where things are.
    pub(super) fn bulk_renamed(
        &mut self,
        tab_id: u64,
        result: Result<Vec<(PathBuf, PathBuf)>, Box<hyprforge_fileops::batch::Failure>>,
    ) -> Task<Message> {
        let done = match result {
            Ok(done) => {
                self.bulk_rename = None;
                done
            }
            Err(failure) => {
                let why = bulk_rename::describe_failure(&failure);
                match &mut self.bulk_rename {
                    Some((_, sheet)) if failure.nothing_changed() && !bulk_rename::was_interrupted(&failure) => {
                        sheet.failed(why)
                    }
                    _ => {
                        self.bulk_rename = None;
                        self.status = Some(why);
                    }
                }
                failure.renamed
            }
        };
        let mut tasks = Vec::new();
        if !done.is_empty() {
            tasks.push(self.record(hyprforge_files_core::undo::Undoable::RenamedAll(done.clone())));
        }
        if let Some(at) = self.locate(tab_id) {
            let renamed: Vec<PathBuf> = done.into_iter().map(|(_, to)| to).collect();
            if !renamed.is_empty() {
                self.pane_mut(at).browser.update(BrowserMessage::SelectWhenListed(renamed));
            }
            let dir = self.pane(at).browser.current_dir().to_path_buf();
            tasks.push(self.spawn_read_dir(at, dir));
        }
        Task::batch(tasks)
    }
}

#[cfg(test)]
mod tests {
    use super::sheet_key;
    use hyprforge_files::bulk_rename::Message;
    use hyprforge_files_core::bulk_rename::Mode;
    use hyprforge_files_core::keymap::{Combo, KeyPress};

    fn press(binding: &str) -> KeyPress {
        let combo = Combo::parse(binding).unwrap();
        KeyPress { key: combo.key, mods: combo.mods, text: None }
    }

    /// The sheet works without a mouse: modes by number, fields by Tab.
    #[test]
    fn the_sheet_can_be_driven_from_the_keyboard() {
        assert_eq!(sheet_key(&press("Ctrl+3")), Some(Message::Mode(Mode::Number)));
        assert_eq!(sheet_key(&press("Tab")), Some(Message::NextField(true)));
        assert_eq!(sheet_key(&press("Shift+Tab")), Some(Message::NextField(false)));
        assert_eq!(sheet_key(&press("Escape")), Some(Message::Cancel));
    }

    /// Everything else stops at the sheet — Delete must not trash the
    /// selection hidden behind it.
    #[test]
    fn other_keys_go_nowhere() {
        assert_eq!(sheet_key(&press("Delete")), None);
        assert_eq!(sheet_key(&press("Ctrl+5")), None);
    }
}
