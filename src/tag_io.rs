//! The host's half of tags: reading and writing the `user.xdg.tags`
//! attribute — see `hyprforge_files_core::tags` for the format, the
//! index and why a tag lives on the file. Shared by the window and the
//! open/save dialog, whose sidebar lists tags too. Blocking, every call:
//! run off the UI thread.

use hyprforge_files_core::{Entry, FsBackend};
use std::path::{Path, PathBuf};

fn display_name(path: &Path) -> String {
    path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| path.display().to_string())
}

/// A file's tags, from the file — see `hyprforge_files_core::tags`.
/// `Err` is a sentence: a filesystem with no extended attributes says
/// it cannot hold tags.
pub fn file_tags(path: &Path) -> Result<Vec<String>, String> {
    match hyprforge_fileops::xattr::get(path, hyprforge_files_core::tags::ATTRIBUTE) {
        Ok(Some(value)) => Ok(hyprforge_files_core::tags::parse(&value)),
        Ok(None) => Ok(Vec::new()),
        Err(e) if hyprforge_fileops::xattr::unsupported(&e) => {
            Err(format!("{} is on a drive that can't hold tags.", display_name(path)))
        }
        Err(e) => Err(format!("{}'s tags couldn't be read: {e}", display_name(path))),
    }
}

/// Writes each file's new tags — the attribute set, or removed when the
/// last tag goes. The files written, and a sentence for each not.
pub fn write_tags(writes: Vec<(PathBuf, Vec<String>)>) -> (Vec<PathBuf>, Vec<String>) {
    use hyprforge_files_core::tags;
    let mut written = Vec::new();
    let mut failed = Vec::new();
    for (path, now) in writes {
        let result = match tags::format(&now) {
            Some(value) => hyprforge_fileops::xattr::set(&path, tags::ATTRIBUTE, &value),
            None => hyprforge_fileops::xattr::remove(&path, tags::ATTRIBUTE),
        };
        match result {
            Ok(()) => written.push(path),
            Err(e) if hyprforge_fileops::xattr::unsupported(&e) => {
                failed.push(format!("{} is on a drive that can't hold tags.", display_name(&path)))
            }
            Err(e) => failed.push(format!("Couldn't tag {}: {e}", display_name(&path))),
        }
    }
    (written, failed)
}

/// A tag's view: each file the index lists, if it is there and still
/// has the tag on it, and the ones that are not — to be forgotten.
pub fn read_tagged(backend: &dyn FsBackend, tag: &str, paths: &[PathBuf]) -> (Vec<Entry>, Vec<PathBuf>) {
    let mut found = Vec::new();
    let mut forgotten = Vec::new();
    for path in paths {
        let still = file_tags(path).is_ok_and(|tags| tags.iter().any(|t| t == tag));
        match backend.stat(path) {
            Ok(mut entry) if still => {
                entry.origin = path.parent().map(Path::to_path_buf);
                found.push(entry);
            }
            _ => forgotten.push(path.clone()),
        }
    }
    (found, forgotten)
}


#[cfg(test)]
mod tests {
    use super::*;
    use hyprforge_files_core::StdBackend;

    /// A file in a fresh directory, or `None` (said) where the directory's
    /// filesystem holds no extended attributes.
    fn tagged_dir() -> Option<(tempfile::TempDir, PathBuf)> {
        let dir = tempfile::tempdir().ok()?;
        let file = dir.path().join("report.pdf");
        std::fs::write(&file, "x").ok()?;
        if let Err(e) = hyprforge_fileops::xattr::set(&file, "user.hyprforge.probe", b"1") {
            eprintln!("HYPRFORGE-SKIP: the temporary directory holds no user extended attributes ({e})");
            return None;
        }
        Some((dir, file))
    }

    #[test]
    fn tags_written_are_the_tags_read_back_and_the_last_one_off_removes_the_attribute() {
        let Some((_dir, file)) = tagged_dir() else { return };
        let (written, failed) = write_tags(vec![(file.clone(), vec!["work".into(), "q3".into()])]);
        assert!(failed.is_empty(), "{failed:?}");
        assert_eq!(written, std::slice::from_ref(&file));
        assert_eq!(file_tags(&file).unwrap(), ["work", "q3"]);
        assert_eq!(
            hyprforge_fileops::xattr::get(&file, hyprforge_files_core::tags::ATTRIBUTE).unwrap().as_deref(),
            Some(&b"work,q3"[..]),
            "the spelling Dolphin reads"
        );
        write_tags(vec![(file.clone(), Vec::new())]);
        assert_eq!(hyprforge_fileops::xattr::get(&file, hyprforge_files_core::tags::ATTRIBUTE).unwrap(), None);
    }

    /// The index is only an index: a file whose tag was taken off
    /// elsewhere, or that is gone, is not shown as having it.
    #[test]
    fn a_tag_view_shows_what_still_has_the_tag_and_names_the_rest() {
        let Some((dir, file)) = tagged_dir() else { return };
        write_tags(vec![(file.clone(), vec!["work".into()])]);
        let untagged = dir.path().join("plain.txt");
        std::fs::write(&untagged, "y").unwrap();
        let gone = dir.path().join("gone.txt");
        let (found, forgotten) = read_tagged(&StdBackend, "work", &[file.clone(), untagged.clone(), gone.clone()]);
        assert_eq!(found.iter().map(|e| e.path.clone()).collect::<Vec<_>>(), [file]);
        assert_eq!(found[0].origin.as_deref(), Some(dir.path()), "a Folder column, as Starred's");
        assert_eq!(forgotten, [untagged, gone]);
    }
}
