//! A drag that starts in this window and stays in it never becomes a
//! Wayland drag at all.
//!
//! Hyprland 0.56 sends a drag's `enter`, `motion` and `drop` to the
//! first `wl_data_device` a client made, and that is always iced's
//! clipboard's — which ignores them. Measured twice, most recently on
//! 2026-10-06: the clipboard's device was created 45–80ms before this
//! crate's, the drag entered it, and this crate's device heard nothing,
//! not even the selection. So a drag handed to the compositor can never
//! be dropped back onto this window, and moving files between two
//! folders of one window — the commonest drag a file manager sees —
//! did nothing.
//!
//! The answer is not to ask the compositor while there is no need to.
//! A held button gives the window an implicit grab: iced keeps hearing
//! the pointer move, so the window can follow a drag across its own
//! folders by itself, ask its layout which one is under the pointer
//! (the same `drop::HitTest` a Wayland drop uses) and drop through the
//! ordinary paste. The compositor is told only when the pointer reaches
//! the window's edge, because only then can the files be going anywhere
//! else — and the button is still held there, so the press serial
//! `start_drag` needs is still good.
//!
//! The hand-over happens [`EDGE`] pixels *inside* the edge rather than
//! on crossing it. A drag is started against the surface the pointer is
//! over; once the pointer is outside, a compositor is entitled to have
//! moved its focus elsewhere and refuse. Inside the edge it cannot have.
//!
//! This module is the decision and nothing else: the window feeds it
//! positions and acts on what it says.

use iced::{event, keyboard, mouse, Event, Point, Size, Subscription};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

/// How close to the window's edge, in logical pixels, the pointer has
/// to come before the drag is handed to the compositor. Small enough
/// that a drop on a row at the very edge of the window still lands in
/// the app; large enough that one ordinary motion event does not carry
/// the pointer from inside the band to beyond it.
pub const EDGE: f32 = 4.0;

/// What one pointer position means for a drag in progress.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Step {
    /// Still inside: look for a folder under this point.
    Over(Point),
    /// At or past the edge: from now on it is the compositor's drag.
    HandOff,
}

/// Where `at` is, for a window of `window` logical pixels. Past the
/// edge counts as at it: a pointer can arrive there in one motion, and
/// under a grab the window still hears where it went.
pub fn step(at: Point, window: Size) -> Step {
    let inside = at.x >= EDGE && at.y >= EDGE && at.x <= window.width - EDGE && at.y <= window.height - EDGE;
    if inside {
        Step::Over(at)
    } else {
        Step::HandOff
    }
}

/// A drag that has not left the window.
#[derive(Debug, Clone, PartialEq)]
pub struct InnerDrag {
    /// What is being dragged, as the listing names it — real paths, or
    /// an archive's members (`~/x.zip/notes.txt`).
    pub paths: Vec<PathBuf>,
    /// The last position the pointer was seen at. A release carries no
    /// position of its own.
    pub at: Option<Point>,
}

impl InnerDrag {
    pub fn new(paths: Vec<PathBuf>) -> Self {
        Self { paths, at: None }
    }
}

/// What the pointer and keyboard did while a drag is in progress.
#[derive(Debug, Clone, PartialEq)]
pub enum DragEvent {
    Moved(Point),
    /// The left button came up: the drop.
    Released,
    /// The compositor says the pointer left the window — which under a
    /// grab it should not, so this is the case [`EDGE`] exists to make
    /// rare, and it is handed over all the same.
    Left,
    /// Escape: the drag is abandoned and nothing moves.
    Cancelled,
}

/// Whether a drag is in progress, for the event listener — which is a
/// plain function and cannot see the window's state. Off, the listener
/// turns every event away without allocating, so following the pointer
/// costs nothing when nobody is dragging.
static ACTIVE: AtomicBool = AtomicBool::new(false);

/// Tells the listener a drag started (`true`) or ended.
pub fn set_active(active: bool) {
    ACTIVE.store(active, Ordering::Relaxed);
}

/// The pointer and the Escape key while a drag is in progress, and
/// nothing otherwise. Always subscribed rather than only during a drag:
/// a subscription made in response to the drag starting would begin a
/// frame later, and a release in that frame would leave a drag nobody
/// could end.
pub fn events() -> Subscription<DragEvent> {
    event::listen_with(|event, _status, _window| {
        if !ACTIVE.load(Ordering::Relaxed) {
            return None;
        }
        match event {
            Event::Mouse(mouse::Event::CursorMoved { position }) => Some(DragEvent::Moved(position)),
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => Some(DragEvent::Released),
            Event::Mouse(mouse::Event::CursorLeft) => Some(DragEvent::Left),
            Event::Keyboard(keyboard::Event::KeyPressed {
                key: keyboard::Key::Named(keyboard::key::Named::Escape),
                ..
            }) => Some(DragEvent::Cancelled),
            _ => None,
        }
    })
}

/// What letting go of an archive's members over `into` does. They are
/// not files a paste can read, so a drop that lands on a real folder
/// extracts them there; one that lands back inside an archive does
/// nothing that a sentence cannot say better.
#[derive(Debug, Clone, PartialEq)]
pub enum MembersDrop {
    /// Unpack these members of `archive` into `into`, dropping `prefix`
    /// (the folder inside the archive they were dragged from) from the
    /// front of each, so they land loose — what Extract here does.
    Extract { archive: PathBuf, members: Vec<String>, prefix: Option<String>, into: PathBuf },
    /// Let go over the listing it came from, or somewhere inside an
    /// archive: nothing happens.
    Nothing,
    Refused(String),
}

/// See [`MembersDrop`]. `split` is `archive::split` — whether a path
/// goes through an archive, and where — which only a `stat` can answer,
/// so it is the caller's.
pub fn members_drop(
    paths: &[PathBuf],
    into: &std::path::Path,
    split: impl Fn(&std::path::Path) -> Option<(PathBuf, String)>,
) -> MembersDrop {
    let mut archive = None;
    let mut prefix = None;
    let mut members = Vec::new();
    for path in paths {
        let Some((from, member)) = split(path) else { continue };
        if member.is_empty() {
            continue;
        }
        if archive.as_ref().is_some_and(|a| *a != from) {
            continue;
        }
        if prefix.is_none() {
            prefix = Some(match member.rsplit_once('/') {
                Some((dir, _)) => dir.to_string(),
                None => String::new(),
            });
        }
        archive = Some(from);
        members.push(member);
    }
    let Some(archive) = archive else {
        return MembersDrop::Nothing;
    };
    if let Some((into_archive, folder)) = split(into) {
        // Let go over the folder it was picked up in: as anywhere,
        // nothing at all.
        if into_archive == archive && Some(folder.trim_end_matches('/')) == prefix.as_deref() {
            return MembersDrop::Nothing;
        }
        return MembersDrop::Refused(
            "Files from one archive can't be dropped into another — extract them first.".to_string(),
        );
    }
    MembersDrop::Extract {
        archive,
        members,
        prefix: prefix.filter(|p| !p.is_empty()),
        into: into.to_path_buf(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    const WINDOW: Size = Size { width: 800.0, height: 600.0 };

    #[test]
    fn a_drag_across_the_middle_of_the_window_stays_in_it() {
        assert_eq!(step(Point::new(400.0, 300.0), WINDOW), Step::Over(Point::new(400.0, 300.0)));
        assert_eq!(step(Point::new(EDGE, 300.0), WINDOW), Step::Over(Point::new(EDGE, 300.0)));
    }

    #[test]
    fn the_compositor_takes_over_inside_the_edge_not_beyond_it() {
        assert_eq!(step(Point::new(EDGE - 1.0, 300.0), WINDOW), Step::HandOff);
        assert_eq!(step(Point::new(400.0, WINDOW.height - EDGE + 1.0), WINDOW), Step::HandOff);
        assert_eq!(step(Point::new(WINDOW.width - 2.0, 2.0), WINDOW), Step::HandOff);
    }

    #[test]
    fn a_pointer_that_jumped_past_the_edge_in_one_motion_is_handed_over_too() {
        assert_eq!(step(Point::new(-30.0, 300.0), WINDOW), Step::HandOff);
        assert_eq!(step(Point::new(400.0, 900.0), WINDOW), Step::HandOff);
    }

    /// A stand-in for `archive::split` that knows one archive.
    fn split(path: &Path) -> Option<(PathBuf, String)> {
        let s = path.to_str()?;
        let (archive, member) = s.split_once(".zip")?;
        Some((PathBuf::from(format!("{archive}.zip")), member.trim_start_matches('/').to_string()))
    }

    #[test]
    fn members_dropped_on_a_folder_are_extracted_loose_into_it() {
        let paths = [PathBuf::from("/h/x.zip/docs/a.txt"), PathBuf::from("/h/x.zip/docs/b.txt")];
        assert_eq!(
            members_drop(&paths, Path::new("/h/out"), split),
            MembersDrop::Extract {
                archive: PathBuf::from("/h/x.zip"),
                members: vec!["docs/a.txt".into(), "docs/b.txt".into()],
                prefix: Some("docs".into()),
                into: PathBuf::from("/h/out"),
            }
        );
    }

    #[test]
    fn members_from_the_archive_root_have_no_prefix_to_strip() {
        let paths = [PathBuf::from("/h/x.zip/a.txt")];
        let MembersDrop::Extract { prefix, .. } = members_drop(&paths, Path::new("/h"), split) else {
            panic!("a drop on a folder extracts");
        };
        assert_eq!(prefix, None);
    }

    #[test]
    fn members_dropped_inside_an_archive_are_refused_in_words() {
        let paths = [PathBuf::from("/h/x.zip/a.txt")];
        assert!(matches!(members_drop(&paths, Path::new("/h/x.zip/docs"), split), MembersDrop::Refused(_)), "another folder of the same archive");
        assert!(matches!(members_drop(&paths, Path::new("/h/y.zip"), split), MembersDrop::Refused(_)));
    }

    #[test]
    fn members_let_go_where_they_were_picked_up_do_nothing() {
        let paths = [PathBuf::from("/h/x.zip/docs/a.txt")];
        assert_eq!(members_drop(&paths, Path::new("/h/x.zip/docs"), split), MembersDrop::Nothing);
        let root = [PathBuf::from("/h/x.zip/a.txt")];
        assert_eq!(members_drop(&root, Path::new("/h/x.zip"), split), MembersDrop::Nothing);
    }

    #[test]
    fn plain_files_are_not_members() {
        assert_eq!(members_drop(&[PathBuf::from("/h/a.txt")], Path::new("/h/out"), split), MembersDrop::Nothing);
    }
}
