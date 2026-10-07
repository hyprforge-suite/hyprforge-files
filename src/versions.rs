//! The host's half of Previous Versions: finding a file's copies in
//! snapper's snapshots — see `hyprforge_files_core::snapshots` for what
//! a version is and why identical copies collapse.
//!
//! Blocking, and bounded twice: at most [`MOST`] snapshots are looked
//! in, newest first, and the search stops after [`TIME`] with what it
//! has. A machine keeping a year of hourly snapshots has thousands, and
//! a `stat` in each is still a `stat` too many to wait on whole.

use hyprforge_files_core::snapshots::{config_for, parse_info, versions, Config, Found, Unreadable, Version};
use std::path::Path;
use std::time::{Duration, Instant};

/// Where snapper's configs are.
pub const CONFIGS: &str = "/etc/snapper/configs";

/// The most snapshots looked in for one file.
pub const MOST: usize = 500;

/// The longest one search takes.
pub const TIME: Duration = Duration::from_secs(5);

/// Every snapper config on this machine. Unreadable or absent is none.
pub fn configs(dir: &Path) -> Vec<Config> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    entries
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_str()?.to_string();
            let text = std::fs::read_to_string(e.path()).ok()?;
            Config::parse(&name, &text)
        })
        .collect()
}

/// `path`'s versions, newest first — see the module doc for the bounds.
pub fn find(path: &Path, configs: &[Config]) -> Result<Vec<Version>, Unreadable> {
    let (config, relative) = config_for(path, configs).ok_or(Unreadable::NotSnapshotted)?;
    let root = config.snapshots();
    let entries = std::fs::read_dir(&root).map_err(|e| match e.kind() {
        std::io::ErrorKind::PermissionDenied => Unreadable::NotAllowed,
        _ => Unreadable::Failed(format!("{} couldn't be read: {e}", root.display())),
    })?;
    let mut numbers: Vec<u32> =
        entries.flatten().filter_map(|e| e.file_name().to_str()?.parse::<u32>().ok()).collect();
    numbers.sort_unstable_by(|a, b| b.cmp(a));
    numbers.truncate(MOST);
    let started = Instant::now();
    let mut found = Vec::new();
    for num in numbers {
        if started.elapsed() > TIME {
            break;
        }
        let copy = config.copy_in(num, &relative);
        let Ok(meta) = std::fs::symlink_metadata(&copy) else { continue };
        let info_path = root.join(num.to_string()).join("info.xml");
        let Some(info) = std::fs::read_to_string(&info_path).ok().and_then(|x| parse_info(&x)) else { continue };
        found.push(Found { info, path: copy, modified: meta.modified().ok(), size: meta.len() });
    }
    let now = std::fs::symlink_metadata(path).ok().map(|m| (m.modified().ok(), m.len()));
    Ok(versions(found, now))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// A snapper layout in a temporary directory: `/home` is `root/home`,
    /// with snapshots 1–3 holding three copies of `alex/a.txt`.
    fn layout() -> (tempfile::TempDir, Config) {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let config = Config { name: "home".into(), subvolume: home.clone() };
        for (num, text) in [(1, "one"), (2, "one"), (3, "two")] {
            let copy = config.copy_in(num, Path::new("alex/a.txt"));
            std::fs::create_dir_all(copy.parent().unwrap()).unwrap();
            std::fs::write(&copy, text).unwrap();
            // The same modified time for 1 and 2, as an unchanged file has.
            let when = std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(if num < 3 { 100 } else { 200 });
            std::fs::File::options().write(true).open(&copy).unwrap().set_modified(when).unwrap();
            std::fs::write(
                config.snapshots().join(num.to_string()).join("info.xml"),
                format!("<snapshot><num>{num}</num><date>2026-10-0{num} 10:00:00</date><description>timeline</description></snapshot>"),
            )
            .unwrap();
        }
        std::fs::create_dir_all(home.join("alex")).unwrap();
        std::fs::write(home.join("alex/a.txt"), "three, now").unwrap();
        (dir, config)
    }

    #[test]
    fn a_files_copies_come_back_as_the_versions_there_were() {
        let (_dir, config) = layout();
        let path = config.subvolume.join("alex/a.txt");
        let got = find(&path, std::slice::from_ref(&config)).unwrap();
        assert_eq!(got.iter().map(|v| (v.newest.info.num, v.snapshots)).collect::<Vec<_>>(), [(3, 1), (2, 2)]);
        assert_eq!(got[0].newest.path, config.copy_in(3, Path::new("alex/a.txt")));
    }

    #[test]
    fn a_path_no_config_covers_says_so() {
        let (_dir, config) = layout();
        assert_eq!(find(Path::new("/elsewhere/x"), &[config]), Err(Unreadable::NotSnapshotted));
    }

    #[test]
    fn the_configs_folder_is_read_for_each_subvolume() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("home"), "SUBVOLUME=\"/home\"\n").unwrap();
        std::fs::write(dir.path().join("root"), "SUBVOLUME=\"/\"\n").unwrap();
        let mut got: Vec<PathBuf> = configs(dir.path()).into_iter().map(|c| c.subvolume).collect();
        got.sort();
        assert_eq!(got, [PathBuf::from("/"), PathBuf::from("/home")]);
        assert!(configs(Path::new("/nowhere")).is_empty());
    }
}
