//! The window's half of the Properties inspector: the looking and the
//! changing that `hyprforge_files_core::properties` asks for and cannot
//! do itself, because the browser does no I/O.
//!
//! Every function here blocks, and every caller runs it on a blocking
//! worker. None of them waits on another process — `stat`, `chmod`, a
//! directory walk and the MIME database are all this process's own
//! calls — so the bound that matters is the walk's, which is
//! [`MEASURE_MAX_ENTRIES`] entries or [`MEASURE_MAX_TIME`], whichever
//! comes first, and a [`Cancel`] checked at every entry.

use hyprforge_files_core::properties::{
    AppChoice, Apps, Cancel, Facts, Tally, TallyState, MEASURE_MAX_ENTRIES, MEASURE_MAX_TIME,
};
use hyprforge_files_core::users::{effective_uid, name_for_gid, name_for_uid};
use hyprforge_mime::MimeDb;
use std::collections::HashSet;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// How often a running walk reports. Often enough that a large folder's
/// total visibly climbs; rarely enough that a fast disk does not send
/// the window a message per directory.
const REPORT_EVERY: Duration = Duration::from_millis(150);

/// What `stat` and the MIME database say about `path`.
///
/// A link is described by what it points at — its size, its times, its
/// owner — with where it points alongside, because that is the file a
/// person means when they ask about a link. A link pointing nowhere is
/// described as itself.
pub fn facts_of(path: &Path, db: &MimeDb) -> Result<Facts, String> {
    let own = std::fs::symlink_metadata(path).map_err(|e| looking_failed(&e))?;
    let is_link = own.file_type().is_symlink();
    let link_target = if is_link { std::fs::read_link(path).ok() } else { None };
    let meta = if is_link { std::fs::metadata(path).unwrap_or_else(|_| own.clone()) } else { own };

    let found = db.sniff(path);
    let mime = db.canonical(&found.mime).to_string();
    // The XDG data directories themselves: `description_of` adds the
    // `mime/` under each. Measured — handing it `mime_dirs` looked in
    // `/usr/share/mime/mime/` and every file came out described as
    // nothing, falling back to the listing's own word for it.
    let description = hyprforge_mime::types::description_of(&hyprforge_mime::data_dirs(), &mime, None);
    let uid = meta.uid();
    let euid = effective_uid();
    Ok(Facts {
        mime: Some(mime),
        description,
        size: meta.len(),
        // `st_blocks` is always in 512-byte units, whatever the
        // filesystem's own block size — POSIX leaves it unspecified and
        // Linux fixes it at 512.
        on_disk: meta.blocks().saturating_mul(512),
        modified: meta.modified().ok(),
        accessed: meta.accessed().ok(),
        // `statx`'s birth time where the filesystem records it; an
        // `Unsupported` error where it does not, which is "not recorded"
        // rather than a failure.
        created: meta.created().ok(),
        mode: meta.mode() & 0o7777,
        uid,
        gid: meta.gid(),
        owner: name_for_uid(uid),
        group: name_for_gid(meta.gid()),
        inode: meta.ino(),
        links: meta.nlink(),
        link_target,
        // Linux ignores a link's own mode, and `chmod` through a link
        // changes the file it points at — which is not what the boxes
        // beside a link's name would appear to change.
        can_change_mode: !is_link && (euid == 0 || euid == uid),
        tags: crate::tag_io::file_tags(path).unwrap_or_default(),
    })
}

fn looking_failed(e: &std::io::Error) -> String {
    match e.kind() {
        std::io::ErrorKind::NotFound => "It isn't there any more.".to_string(),
        std::io::ErrorKind::PermissionDenied => "You don't have permission to look at this.".to_string(),
        _ => format!("Couldn't look at it: {e}"),
    }
}

/// What can open `path`, by its *name* — the same question opening it
/// asks (`launch.rs` goes by `MimeDb::type_of`), so the default this
/// shows is the one a double-click would use.
pub fn apps_of(path: &Path, db: &MimeDb) -> Apps {
    let Some(mime) = db.type_of(path).map(|m| db.canonical(m).to_string()) else {
        return Apps::default();
    };
    let choices = db
        .candidates(&mime)
        .into_iter()
        .map(|c| AppChoice {
            id: c.app.id.clone(),
            name: c.app.name.clone(),
            made_for: c.made_for,
            is_default: c.is_default,
        })
        .collect();
    let missing_default = db.default_for(&mime).filter(|app| !app.installed).map(|app| app.name.clone());
    Apps { mime: Some(mime), choices, missing_default }
}

/// Sets `path`'s permission bits to `mode` and says what they are now.
pub fn set_mode(path: &Path, mode: u32, db: &MimeDb) -> Result<Facts, String> {
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).map_err(|e| match e.kind() {
        std::io::ErrorKind::PermissionDenied => {
            "You can only change the permissions of your own files.".to_string()
        }
        std::io::ErrorKind::ReadOnlyFilesystem => "This is on a read-only disk.".to_string(),
        std::io::ErrorKind::NotFound => "It isn't there any more.".to_string(),
        _ => format!("Couldn't change the permissions: {e}"),
    })?;
    // Asked again rather than assumed: the filesystem may have stored
    // something other than what was asked for (a FAT disk has no group
    // bits to keep), and the boxes should show what is true.
    facts_of(path, db)
}

/// Makes `app` the default for `mime`, then reads the database again —
/// the window's copy is a snapshot — and says what opens `path` now.
pub fn set_default(path: &Path, mime: &str, app: &str) -> Result<(MimeDb, Apps), String> {
    MimeDb::load()
        .set_default(mime, app)
        .map_err(|e| format!("Couldn't save that choice: {e}"))?;
    let db = MimeDb::load();
    let apps = apps_of(path, &db);
    Ok((db, apps))
}

/// Walks `folders` for their total, calling `report` with a running
/// count every `REPORT_EVERY` and once at the end.
///
/// Returns without a final report when `cancel` is tripped: nobody is
/// waiting for the answer any more.
///
/// - **Links are not followed.** A link to `/` inside a project folder
///   would otherwise count the whole disk, and the link itself is a few
///   bytes of the folder it sits in.
/// - **Other filesystems are not entered**, the way `du -x` does not: a
///   mount point under a folder (a USB stick, `/proc`) is not part of
///   what the folder holds, and some of them never end.
/// - **A file with several hard links counts once**, so a folder of
///   backups made with `cp -al` is not reported at many times its size.
pub fn measure(folders: &[PathBuf], cancel: &Cancel, mut report: impl FnMut(Tally)) {
    let started = Instant::now();
    let mut last_report = started;
    let mut tally = Tally::default();
    let mut seen_links: HashSet<(u64, u64)> = HashSet::new();
    let mut visited: u64 = 0;
    for root in folders {
        let Ok(root_meta) = std::fs::symlink_metadata(root) else {
            tally.unreadable += 1;
            continue;
        };
        let device = root_meta.dev();
        let mut pending = vec![root.clone()];
        while let Some(dir) = pending.pop() {
            let entries = match std::fs::read_dir(&dir) {
                Ok(entries) => entries,
                Err(_) => {
                    tally.unreadable += 1;
                    continue;
                }
            };
            for entry in entries.flatten() {
                if cancel.is_cancelled() {
                    return;
                }
                visited += 1;
                if visited >= MEASURE_MAX_ENTRIES || started.elapsed() >= MEASURE_MAX_TIME {
                    tally.state = TallyState::Bounded;
                    report(tally);
                    return;
                }
                let Ok(meta) = entry.metadata() else { continue };
                if meta.is_dir() {
                    tally.folders += 1;
                    if meta.dev() == device {
                        pending.push(entry.path());
                    }
                    continue;
                }
                tally.files += 1;
                if meta.nlink() > 1 && !seen_links.insert((meta.dev(), meta.ino())) {
                    continue;
                }
                tally.bytes += meta.len();
            }
            if last_report.elapsed() >= REPORT_EVERY {
                last_report = Instant::now();
                report(tally);
            }
        }
    }
    if cancel.is_cancelled() {
        return;
    }
    tally.state = TallyState::Done;
    report(tally);
}

/// [`measure`] as a stream of totals, on a blocking worker — what the
/// window turns into `Message::Tallied`.
pub fn measure_stream(folders: Vec<PathBuf>, cancel: Cancel) -> impl iced::futures::Stream<Item = Tally> {
    iced::stream::channel(4, async move |mut out| {
        use iced::futures::SinkExt;
        let (tx, mut rx) = tokio::sync::mpsc::channel::<Tally>(4);
        let walking = cancel.clone();
        let walk = tokio::task::spawn_blocking(move || {
            // `blocking_send` waits when the window is behind, which
            // slows the walk rather than queueing totals nobody will
            // read — and fails when the window has gone, which stops it.
            measure(&folders, &walking, |tally| {
                if tx.blocking_send(tally).is_err() {
                    walking.cancel();
                }
            });
        });
        while let Some(tally) = rx.recv().await {
            if out.send(tally).await.is_err() {
                cancel.cancel();
                break;
            }
        }
        let _ = walk.await;
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn totals(folders: &[PathBuf], cancel: &Cancel) -> Vec<Tally> {
        let mut seen = Vec::new();
        measure(folders, cancel, |t| seen.push(t));
        seen
    }

    #[test]
    fn a_folder_is_measured_to_the_bottom_and_ends_done() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("a/b")).unwrap();
        std::fs::write(dir.path().join("one"), vec![0u8; 100]).unwrap();
        std::fs::write(dir.path().join("a/b/two"), vec![0u8; 23]).unwrap();
        let seen = totals(&[dir.path().to_path_buf()], &Cancel::new());
        let last = seen.last().copied().unwrap();
        assert_eq!(last.state, TallyState::Done);
        assert_eq!((last.bytes, last.files, last.folders, last.unreadable), (123, 2, 2, 0));
    }

    /// Following a link to somewhere big is how a folder of a few
    /// kilobytes gets reported as the size of the disk.
    #[test]
    fn a_link_is_counted_as_itself_and_never_followed() {
        let dir = tempfile::tempdir().unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        std::fs::write(elsewhere.path().join("big"), vec![0u8; 5000]).unwrap();
        std::os::unix::fs::symlink(elsewhere.path(), dir.path().join("link")).unwrap();
        let last = *totals(&[dir.path().to_path_buf()], &Cancel::new()).last().unwrap();
        assert!(last.bytes < 5000, "{last:?}");
        assert_eq!(last.folders, 0, "the link is not a folder of this one");
    }

    #[test]
    fn a_file_linked_twice_counts_once() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("one"), vec![0u8; 1000]).unwrap();
        std::fs::hard_link(dir.path().join("one"), dir.path().join("two")).unwrap();
        let last = *totals(&[dir.path().to_path_buf()], &Cancel::new()).last().unwrap();
        assert_eq!((last.bytes, last.files), (1000, 2));
    }

    /// A folder that cannot be read is counted as such, never as empty.
    #[test]
    fn an_unreadable_folder_is_counted_as_unreadable() {
        let gone = PathBuf::from("/nonexistent/hyprforge-properties-test");
        let last = *totals(&[gone], &Cancel::new()).last().unwrap();
        assert_eq!(last.unreadable, 1);
        assert_eq!(last.state, TallyState::Done);
    }

    /// A cancelled walk says nothing at all: its answer has nowhere to
    /// go, and a stale total arriving late is the bug the generation
    /// guard exists for.
    #[test]
    fn a_cancelled_walk_reports_nothing() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("one"), b"x").unwrap();
        let cancel = Cancel::new();
        cancel.cancel();
        assert!(totals(&[dir.path().to_path_buf()], &cancel).is_empty());
    }

    #[test]
    fn a_files_facts_come_from_stat_and_its_owner_is_named() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notes.txt");
        std::fs::write(&path, b"hello").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();
        let facts = facts_of(&path, &MimeDb::default()).unwrap();
        assert_eq!(facts.size, 5);
        assert_eq!(facts.mode, 0o640);
        assert_eq!(facts.uid, effective_uid());
        assert!(facts.owner.is_some(), "this process's own user has a name");
        assert!(facts.can_change_mode, "it is ours");
        assert_eq!(facts.link_target, None);
    }

    #[test]
    fn a_link_is_described_by_its_target_and_its_mode_is_not_offered() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("real");
        std::fs::write(&target, b"0123456789").unwrap();
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let facts = facts_of(&link, &MimeDb::default()).unwrap();
        assert_eq!(facts.size, 10, "the target's size");
        assert_eq!(facts.link_target.as_deref(), Some(target.as_path()));
        assert!(!facts.can_change_mode);
    }

    /// Asked of this machine's shared MIME database, because the bug it
    /// guards — looking one `mime/` too deep — passed every test that
    /// built a database by hand.
    #[test]
    fn a_type_is_described_in_words_from_the_installed_database() {
        if !Path::new("/usr/share/mime/text/plain.xml").exists() {
            eprintln!("HYPRFORGE-SKIP: no shared MIME database at /usr/share/mime to describe a type from");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notes.txt");
        std::fs::write(&path, b"hello\n").unwrap();
        let facts = facts_of(&path, &MimeDb::load()).unwrap();
        assert_eq!(facts.mime.as_deref(), Some("text/plain"));
        assert!(facts.description.is_some(), "{facts:?}");
    }

    #[test]
    fn a_path_that_is_gone_says_so() {
        let err = facts_of(Path::new("/nonexistent/hyprforge-x"), &MimeDb::default()).unwrap_err();
        assert!(err.contains("isn't there"), "{err}");
    }

    #[test]
    fn setting_a_mode_reports_what_stat_says_afterwards() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("run.sh");
        std::fs::write(&path, b"#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        let facts = set_mode(&path, 0o744, &MimeDb::default()).unwrap();
        assert_eq!(facts.mode, 0o744);
        assert_eq!(std::fs::metadata(&path).unwrap().mode() & 0o7777, 0o744);
    }

    #[test]
    fn a_name_no_rule_knows_has_no_applications_and_no_type() {
        assert_eq!(apps_of(Path::new("/x/y.qqq"), &MimeDb::default()), Apps::default());
    }
}
