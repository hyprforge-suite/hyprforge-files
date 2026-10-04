//! What any window that hosts a [`Browser`](hyprforge_files_core::Browser)
//! does for it: the reading the browser asks for and cannot do itself.
//!
//! The Files window and the open/save dialog are both hosts, and the
//! dialog must show a folder exactly as the window does — the same
//! counts, thumbnails, previews and icons — so these live here, once,
//! rather than in either binary. Every one runs its I/O on a blocking
//! worker and is bounded the way its doc says.

use hyprforge_files_core::{DirError, DirErrorKind, Entry, FsBackend};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// How many folders one listing will count before giving up.
pub const COUNT_BUDGET: usize = 400;

/// Reads one directory off the UI thread, through whichever backend
/// claims it.
///
/// The backend is handed in rather than constructed here. That is what
/// makes everything above this function reachable with the mock: a
/// window's whole update loop — stale generations, tab routing, folder
/// counts — used to be untestable without a real disk, because this one
/// function named `StdBackend` as a concrete type and nothing else in
/// the app held a backend at all.
pub async fn read_dir_task(
    backend: Arc<dyn FsBackend>,
    path: PathBuf,
) -> Result<Vec<Entry>, DirError> {
    match tokio::task::spawn_blocking(move || {
        backend.read_dir(&path).map_err(|e| DirError::from(&e))
    })
    .await
    {
        Ok(result) => result,
        Err(e) => Err(DirError {
            message: format!("Reading this folder was interrupted: {e}"),
            kind: DirErrorKind::Other,
        }),
    }
}

/// The second pass behind a folder's Size cell: how many things are in
/// each of these directories.
///
/// One call per folder, which is why this is a second pass rather than
/// part of the listing — see `Outcome::CountFolders`'s own doc.
///
/// Through the same backend as the listing itself, so the trash's count
/// agrees with the trash's listing rather than counting its storage
/// directory behind its back.
///
/// Bounded by [`COUNT_BUDGET`]. A directory of ten thousand
/// subdirectories would otherwise spend ten thousand reads filling in a
/// column nobody has scrolled to, on a machine the user is trying to do
/// something else with. Past the budget the rest simply stay
/// `ItemCount::Pending`, which the Size cell already renders as blank —
/// the alternative, counting forever, is invisible until it is
/// someone's fan spinning up.
pub async fn count_folders(
    backend: Arc<dyn FsBackend>,
    folders: Vec<PathBuf>,
) -> Vec<(PathBuf, Option<usize>)> {
    tokio::task::spawn_blocking(move || {
        folders
            .into_iter()
            .take(COUNT_BUDGET)
            .map(|path| {
                // `None` rather than `Some(0)` when the read fails: a
                // folder you have no permission to open is not an empty
                // one, and `ItemCount::Unreadable` says so with an em
                // dash where `Known(0)` would say "0 items".
                let count = backend.count_children(&path).ok();
                (path, count)
            })
            .collect()
    })
    .await
    .unwrap_or_default()
}

/// The preview pane's picture, in physical pixels: twice the pane's
/// widest, for a 2x display.
const PREVIEW_EDGE: u32 = (hyprforge_files_core::browser::PREVIEW_WIDTH as u32) * 2;

/// Thumbnails, one at a time, each sent back as soon as it exists.
///
/// One at a time on purpose: a task per picture would put a whole
/// folder's decodes in flight at once, and a decode's peak is the full
/// photograph before it is shrunk — the resource CLAUDE.md says to ask
/// about, not only the result. Sequential keeps the peak at one picture
/// however many there are; streaming keeps the first ones on screen
/// arriving first.
///
/// Each is made at the freedesktop size covering `edge`, and comes back
/// with the size it was made at — the browser keeps the old picture until
/// a bigger one arrives. `allowed` is `[thumbnails]`: which sources are
/// on, and the size cap only the host can apply, since it reads sizes.
pub fn thumbnail_stream(
    paths: Vec<PathBuf>,
    edge: u32,
    mime: Arc<hyprforge_mime::MimeDb>,
    allowed: hyprforge_files_core::config::Thumbnails,
) -> impl iced::futures::Stream<Item = (PathBuf, hyprforge_files_core::preview::Picture, u32)> {
    // The freedesktop cache every other program shares — a thumbnail is
    // made once per version of a file, and a picture another program
    // already thumbnailed costs nothing here. See `preview::thumbnail`.
    let cache = hyprforge_thumbnails::Cache::standard();
    iced::stream::channel(16, async move |mut out| {
        use iced::futures::SinkExt;
        for path in paths {
            let for_task = path.clone();
            let cache = cache.clone();
            let mime = mime.clone();
            let handle = tokio::task::spawn_blocking(move || {
                crate::preview::thumbnail(&for_task, edge, cache.as_ref(), &mime, &allowed)
            })
                .await
                .ok()
                .flatten();
            if let Some((handle, made)) = handle {
                // A closed channel means the window moved on; stop.
                if out.send((path, handle, made)).await.is_err() {
                    return;
                }
            }
        }
    })
}

/// The preview pane's content for `path`, built on a worker thread — see
/// `hyprforge_files::preview` for what it reads and how each part is
/// bounded.
pub async fn build_preview(
    path: PathBuf,
    mime: Arc<hyprforge_mime::MimeDb>,
    backend: Arc<dyn FsBackend>,
) -> (PathBuf, Option<hyprforge_files_core::preview::Preview>) {
    let for_task = path.clone();
    let preview = tokio::task::spawn_blocking(move || {
        crate::preview::build(&for_task, &mime, backend.as_ref(), PREVIEW_EDGE)
    })
    .await
    .ok()
    .flatten();
    (path, preview)
}

/// The size theme icons are looked up at: 32 logical at 2x, the one
/// density whose artwork holds up both shrunk to a list row and grown to
/// a grid cell. Nearly every file-type icon is an SVG, which iced
/// rasterises at whatever size it is drawn, so this chooses the *design*
/// (how much detail the artist drew) rather than the pixels.
const ICON_SIZE: u32 = 32;
const ICON_SCALE: u32 = 2;

/// The configured icon theme's chain, loaded the first time a listing
/// asks for an icon — on a worker thread, since loading asks gsettings
/// which theme is set — and kept. Changing the theme needs a new window,
/// the same as every other GTK-style application.
fn theme_icons() -> &'static hyprforge_icons::Icons {
    static ICONS: std::sync::OnceLock<hyprforge_icons::Icons> = std::sync::OnceLock::new();
    ICONS.get_or_init(hyprforge_icons::Icons::load)
}

/// The icon for one browser key — see `hyprforge_files_core::icon`'s
/// `IconSource` for the four kinds of key.
///
/// A file's type is decided by name alone, as its key is: sniffing
/// contents would mean opening every file in the folder to draw its icon.
fn resolve_icon(key: &str, mime: &hyprforge_mime::MimeDb) -> Option<hyprforge_files_core::preview::Picture> {
    use hyprforge_files_core::icon::IconSource;
    use hyprforge_files_core::preview::Picture;
    let themed = |names: &[&str]| theme_icons().lookup(names, ICON_SIZE, ICON_SCALE).map(|p| Picture::from_path(&p));
    match IconSource::of(key) {
        IconSource::Folder => themed(&["folder", "inode-directory"]),
        // The place's own name first, and a plain folder when the theme
        // has nothing by that name — a theme without `folder-cloud` still
        // draws a folder rather than the coloured mark.
        IconSource::Themed(name) => themed(&[name, "folder"]),
        IconSource::File(path) => user_icon(path).or_else(|| {
            tracing::warn!(path = %path.display(), "[sidebar.icons] names an image that cannot be drawn; using the theme's folder");
            themed(&["folder"])
        }),
        IconSource::Type(sample) => {
            let names = match mime.type_of(Path::new(&sample)) {
                Some(kind) => mime.icon_names(kind),
                // Not a claim that it is binary — only that no rule names
                // it. The theme's own "unknown" keeps the listing one
                // theme's drawing rather than a badge among icons.
                None => vec!["application-octet-stream".to_string(), "unknown".to_string()],
            };
            let names: Vec<&str> = names.iter().map(String::as_str).collect();
            themed(&names)
        }
    }
}

/// An image the user chose for a folder: an SVG as itself, and anything
/// else decoded to icon size by the budgeted decode — a user's file can
/// be a 40-megapixel photograph, and it is drawn at eighteen pixels.
fn user_icon(path: &Path) -> Option<hyprforge_files_core::preview::Picture> {
    use hyprforge_files_core::preview::Picture;
    if !path.is_file() {
        return None;
    }
    if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("svg")) {
        return Some(Picture::from_path(path));
    }
    let decoded = hyprforge_image::decode_to_fit(path, &hyprforge_image::Budget::for_edge(ICON_SIZE * ICON_SCALE)).ok()?;
    Some(Picture::Raster(iced::widget::image::Handle::from_rgba(decoded.size.width, decoded.size.height, decoded.pixels)))
}

/// Icons for each key, found on a worker thread — every lookup stats
/// files, the first one reads the theme's index files, and none of that
/// belongs on the thread drawing the window.
pub async fn resolve_icons(
    keys: Vec<String>,
    mime: Arc<hyprforge_mime::MimeDb>,
) -> Vec<(String, Option<hyprforge_files_core::preview::Picture>)> {
    tokio::task::spawn_blocking(move || {
        keys.into_iter()
            .map(|key| {
                let found = resolve_icon(&key, &mime);
                (key, found)
            })
            .collect()
    })
    .await
    .unwrap_or_default()
}
