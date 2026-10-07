//! The "Tags…" sheet: which tags the selection has, and what to change.
//!
//! Opened on a selection with each file's tags read from the file
//! itself (`user.xdg.tags`, see `hyprforge_files_core::tags`), so a tag
//! put on in Dolphin shows here too. Every tag the window knows of is
//! listed — the selection's own, and the index's — each marked by how
//! many of the selected files have it:
//!
//! - **all** of them: a tick. Clicking takes it off every one.
//! - **some**: a dash. Clicking puts it on every one; nothing is taken
//!   off by a click on a dash, because "some" is the one state a person
//!   could not get back to by clicking again.
//! - **none**: empty. Clicking puts it on every one.
//!
//! A tag left at "some" is not touched: each file keeps whatever it had.
//! Nothing is written until Done, and then only to files whose tags
//! actually change.

use hyprforge_files_core::tags::{self, TagChange};
use std::path::PathBuf;

/// How many of the selection have a tag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mark {
    All,
    Some,
    None,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TagSheet {
    /// The selection, and each file's tags as read when the sheet opened.
    pub files: Vec<(PathBuf, Vec<String>)>,
    /// Each tag listed, and what it is set to now.
    pub choices: Vec<(String, Mark)>,
    /// The "new tag" field.
    pub typed: String,
    /// Why the typed tag could not be added, said under the field.
    pub problem: Option<&'static str>,
}

impl TagSheet {
    /// The sheet for `files`, listing their tags first and then every
    /// other tag `known` — the index's — so one already in use is a click
    /// away rather than retyped.
    pub fn open(files: Vec<(PathBuf, Vec<String>)>, known: impl IntoIterator<Item = String>) -> TagSheet {
        let per_file: Vec<Vec<String>> = files.iter().map(|(_, t)| t.clone()).collect();
        let mut choices: Vec<(String, Mark)> = tags::shared(&per_file)
            .into_iter()
            .map(|(tag, all)| (tag, if all { Mark::All } else { Mark::Some }))
            .collect();
        let mut others: Vec<String> = known.into_iter().filter(|k| !choices.iter().any(|(t, _)| t == k)).collect();
        others.sort();
        others.dedup();
        choices.extend(others.into_iter().map(|t| (t, Mark::None)));
        TagSheet { files, choices, typed: String::new(), problem: None }
    }

    /// A click on a tag — see the module doc for what each mark becomes.
    pub fn toggle(&mut self, index: usize) {
        if let Some((_, mark)) = self.choices.get_mut(index) {
            *mark = match mark {
                Mark::All => Mark::None,
                Mark::Some | Mark::None => Mark::All,
            };
        }
    }

    /// Enter in the field: the typed tag, on every file. One already
    /// listed is ticked rather than listed twice.
    pub fn add_typed(&mut self) {
        match tags::clean(&self.typed) {
            Ok(tag) => {
                match self.choices.iter_mut().find(|(t, _)| *t == tag) {
                    Some((_, mark)) => *mark = Mark::All,
                    None => self.choices.push((tag, Mark::All)),
                }
                self.typed.clear();
                self.problem = None;
            }
            Err(why) => self.problem = Some(why),
        }
    }

    /// Each file whose tags change, with the tags it will have.
    pub fn writes(&self) -> Vec<(PathBuf, Vec<String>)> {
        self.files
            .iter()
            .filter_map(|(path, had)| {
                let mut now = had.clone();
                for (tag, mark) in &self.choices {
                    now = match mark {
                        Mark::All => tags::with(&now, tag),
                        Mark::None => tags::without(&now, tag),
                        Mark::Some => now,
                    };
                }
                (now != *had).then(|| (path.clone(), now))
            })
            .collect()
    }

    /// What the index learns once `written` — the files whose writes
    /// succeeded — have their new tags.
    pub fn index_changes(&self, written: &[PathBuf]) -> Vec<TagChange> {
        let mut out = Vec::new();
        for (tag, mark) in &self.choices {
            let paths = written.to_vec();
            match mark {
                // Every file in the selection, not only those that were
                // written: one that already had the tag set elsewhere
                // joins the index too, which is how a tag from Dolphin
                // comes to be listed.
                Mark::All => out.push(TagChange::Add {
                    tag: tag.clone(),
                    paths: self.files.iter().map(|(p, _)| p.clone()).filter(|p| written.contains(p) || self.had(p, tag)).collect(),
                }),
                Mark::None if !paths.is_empty() => out.push(TagChange::Remove { tag: tag.clone(), paths }),
                _ => {}
            }
        }
        out
    }

    fn had(&self, path: &PathBuf, tag: &str) -> bool {
        self.files.iter().any(|(p, t)| p == path && t.iter().any(|x| x == tag))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sheet() -> TagSheet {
        TagSheet::open(
            vec![
                (PathBuf::from("/a"), vec!["work".to_string(), "draft".to_string()]),
                (PathBuf::from("/b"), vec!["work".to_string()]),
            ],
            ["holiday".to_string(), "work".to_string()],
        )
    }

    #[test]
    fn the_selection_s_tags_come_first_marked_by_how_many_have_them() {
        let s = sheet();
        assert_eq!(
            s.choices,
            [("work".into(), Mark::All), ("draft".into(), Mark::Some), ("holiday".into(), Mark::None)]
        );
    }

    #[test]
    fn nothing_changed_writes_nothing() {
        assert!(sheet().writes().is_empty());
    }

    /// A dash becomes a tick, never a blank: "some" cannot be clicked
    /// back to, so a click must not lose it.
    #[test]
    fn a_click_on_a_dash_puts_it_on_every_file() {
        let mut s = sheet();
        s.toggle(1);
        assert_eq!(s.choices[1].1, Mark::All);
        assert_eq!(s.writes(), [(PathBuf::from("/b"), vec!["work".to_string(), "draft".to_string()])]);
    }

    #[test]
    fn a_click_on_a_tick_takes_it_off_every_file() {
        let mut s = sheet();
        s.toggle(0);
        let writes = s.writes();
        assert_eq!(writes.len(), 2);
        assert!(writes.iter().all(|(_, t)| !t.contains(&"work".to_string())));
        assert!(writes[0].1.contains(&"draft".to_string()), "a tag left at a dash is not touched");
    }

    #[test]
    fn a_typed_tag_goes_on_every_file_and_a_bad_one_is_said() {
        let mut s = sheet();
        s.typed = "  invoices ".into();
        s.add_typed();
        assert_eq!(s.choices.last(), Some(&("invoices".to_string(), Mark::All)));
        assert!(s.typed.is_empty());
        s.typed = "a,b".into();
        s.add_typed();
        assert!(s.problem.is_some());
        assert_eq!(s.typed, "a,b", "kept, to be corrected");
    }

    #[test]
    fn the_index_learns_only_what_was_written_or_already_true() {
        let mut s = sheet();
        s.toggle(2); // holiday on both
        let changes = s.index_changes(&[PathBuf::from("/a")]); // only /a's write worked
        let holiday = changes.iter().find(|c| matches!(c, TagChange::Add { tag, .. } if tag == "holiday")).unwrap();
        assert_eq!(holiday, &TagChange::Add { tag: "holiday".into(), paths: vec![PathBuf::from("/a")] });
        let work = changes.iter().find(|c| matches!(c, TagChange::Add { tag, .. } if tag == "work")).unwrap();
        assert!(matches!(work, TagChange::Add { paths, .. } if paths.len() == 2), "a tag both had already is indexed for both");
    }
}
