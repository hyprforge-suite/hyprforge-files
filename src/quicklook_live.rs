//! Quick Look's live half: a video that plays, a model that turns and an
//! animated GIF that moves, in the card, through the same panes Media's
//! viewer uses (`hyprforge_viewer`).
//!
//! The card itself is `hyprforge_files_core`'s, and for everything else
//! it shows a still — a photograph, a video's frame, a PDF's page. For a
//! video or a model the window fills the card's middle with a live pane
//! instead, and keeps the card's header and key hint around it
//! (`Browser::quick_look_parts`, `quick_look_card`).
//!
//! **When it starts.** Once the card's own answer for the entry has come
//! (`Browser::quick_look_settled`): that answer already waits out a held
//! arrow key, so stepping through a folder of fifty videos starts one
//! player — the one stopped on — not fifty.
//!
//! **An animation** shows the card's own still until its frames are
//! decoded, and keeps showing it if the GIF turns out to have one frame.
//! The preview pane beside the listing never animates: it is
//! `hyprforge_files_core`'s, shared with the open/save dialog and drawn
//! without the GPU film, and two films on screen at once would share the
//! one texture iced keeps per kind of primitive.
//!
//! **When it stops.** The moment the card is closed or moves on: the
//! pane is dropped, and dropping a `VideoPane` stops its player and its
//! thread. Nothing plays behind a closed card.

use hyprforge_viewer::animation::Animation;
use hyprforge_viewer::model_pane::{Loaded, ModelPane};
use hyprforge_viewer::video::VideoPane;
use std::path::{Path, PathBuf};

/// What can be shown live.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Video,
    Model,
    Animation,
}

/// Whether `path`, of MIME type `mime` (if known), is shown live — a
/// video by its type, a model by what `hyprforge_mesh` can read, a GIF
/// by its type (whether it moves is only known once it is decoded).
pub fn kind_of(path: &Path, mime: Option<&str>) -> Option<Kind> {
    if hyprforge_mesh::detect(path).is_some() {
        return Some(Kind::Model);
    }
    match mime {
        Some(m) if m.starts_with("video/") => Some(Kind::Video),
        Some("image/gif") => Some(Kind::Animation),
        _ => None,
    }
}

/// The pane in the card.
#[derive(Debug)]
pub enum Live {
    Video(VideoPane),
    /// Read off the disk meanwhile; then the pane, or why not.
    Model { path: PathBuf, pane: Option<ModelPane>, failed: Option<String> },
    /// Decoded off the UI thread meanwhile; then the frames, with the
    /// first as a still to draw under the film for iced's software
    /// renderer. `None` while decoding, and for a GIF that does not move:
    /// the card's own still shows either way.
    Animation { path: PathBuf, playing: Option<(Animation, iced::widget::image::Handle)> },
}

impl Live {
    pub fn path(&self) -> &Path {
        match self {
            Live::Video(pane) => pane.path(),
            Live::Model { path, .. } | Live::Animation { path, .. } => path,
        }
    }

    /// An animation decoded for this card arrived.
    pub fn animation_loaded(&mut self, for_path: &Path, animation: Option<Animation>) {
        if let Live::Animation { path, playing } = self {
            if path.as_path() == for_path {
                *playing = animation.map(|animation| {
                    let frame = animation.program().frame;
                    let still = iced::widget::image::Handle::from_rgba(frame.width, frame.height, frame.pixels.clone());
                    (animation, still)
                });
            }
        }
    }

    /// The animation playing in the card, if one is.
    pub fn animation(&self) -> Option<&Animation> {
        match self {
            Live::Animation { playing: Some((animation, _)), .. } => Some(animation),
            _ => None,
        }
    }

    pub fn animation_mut(&mut self) -> Option<&mut Animation> {
        match self {
            Live::Animation { playing: Some((animation, _)), .. } => Some(animation),
            _ => None,
        }
    }

    /// A model read for this card arrived; `generation` is the window's.
    pub fn model_loaded(&mut self, for_path: &Path, result: Result<Loaded, String>, generation: u64) {
        if let Live::Model { path, pane, failed } = self {
            if path.as_path() != for_path {
                return;
            }
            match result {
                Ok(loaded) => {
                    *pane = Some(ModelPane::new(
                        path.clone(),
                        &loaded,
                        generation,
                        hyprforge_mesh::style::Projection::Perspective,
                    ))
                }
                Err(e) => *failed = Some(e),
            }
        }
    }
}

/// What to do about the live pane, given what the card shows now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// Leave it as it is.
    Keep,
    /// Drop it — the card closed, or moved on.
    Stop,
    /// Start one for this, of this kind.
    Start(PathBuf, Kind),
}

/// The decision, pure: `live` is the pane's path if there is one,
/// `showing` the card's entry, `settled` whether the card has its answer
/// for it, and `kind` what that entry is.
pub fn step(live: Option<&Path>, showing: Option<&Path>, settled: bool, kind: Option<Kind>) -> Step {
    match (live, showing) {
        (Some(live), Some(showing)) if live == showing => Step::Keep,
        (Some(_), _) => Step::Stop,
        (None, Some(showing)) if settled => match kind {
            Some(kind) => Step::Start(showing.to_path_buf(), kind),
            None => Step::Keep,
        },
        (None, _) => Step::Keep,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frames(n: usize) -> Option<Animation> {
        let frame = || hyprforge_video::Frame { width: 1, height: 1, pixels: vec![0, 0, 0, 255] };
        Animation::over((0..n).map(|_| (frame(), std::time::Duration::from_millis(50))).collect(), [0; 3])
    }

    /// An answer for the GIF the card has moved off is not shown on the
    /// one it moved to.
    #[test]
    fn an_animation_for_another_entry_is_ignored() {
        let mut live = Live::Animation { path: "/a/one.gif".into(), playing: None };
        live.animation_loaded(Path::new("/a/two.gif"), frames(2));
        assert!(live.animation().is_none());
        live.animation_loaded(Path::new("/a/one.gif"), frames(2));
        assert!(live.animation().is_some());
    }

    #[test]
    fn a_video_a_model_or_a_gif_is_live_and_a_photo_is_not() {
        assert_eq!(kind_of(Path::new("/a/clip.mkv"), Some("video/x-matroska")), Some(Kind::Video));
        assert_eq!(kind_of(Path::new("/a/part.stl"), Some("model/stl")), Some(Kind::Model));
        assert_eq!(kind_of(Path::new("/a/photo.jpg"), Some("image/jpeg")), None);
        assert_eq!(kind_of(Path::new("/a/dance.gif"), Some("image/gif")), Some(Kind::Animation));
        assert_eq!(kind_of(Path::new("/a/notes"), None), None);
    }

    /// A held arrow key passes entries before their answers come; only
    /// the one stopped on, once answered, starts a player.
    #[test]
    fn nothing_starts_until_the_card_has_settled() {
        let clip = Path::new("/a/clip.mkv");
        assert_eq!(step(None, Some(clip), false, Some(Kind::Video)), Step::Keep);
        assert_eq!(step(None, Some(clip), true, Some(Kind::Video)), Step::Start(clip.to_path_buf(), Kind::Video));
    }

    #[test]
    fn closing_the_card_or_moving_on_stops_what_plays() {
        let clip = Path::new("/a/clip.mkv");
        assert_eq!(step(Some(clip), None, false, None), Step::Stop, "closed");
        assert_eq!(step(Some(clip), Some(Path::new("/a/next.mkv")), false, Some(Kind::Video)), Step::Stop, "moved on");
        assert_eq!(step(Some(clip), Some(clip), true, Some(Kind::Video)), Step::Keep, "still on it");
    }
}
