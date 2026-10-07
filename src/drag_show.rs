//! What a drag looks like while it happens: the card that is picked up
//! and follows the pointer, the folder that fills as it gets ready to
//! open, and the listing that fades in when it does.
//!
//! Time, not state: everything here is "how far along is it at this
//! instant", answered from when it started, so the window can redraw on
//! each frame while something moves and stop asking the moment nothing
//! does. The drawing itself is `hyprforge_ui::widgets::drag_card` and
//! `drop_target_style`; the clock that drives them is the window's
//! (`window::frames`, subscribed only while [`App::animating`] says so —
//! see `main.rs`).

use hyprforge_ui::widgets::DragCard;
use iced::animation::{Animation, Easing};
use iced::Point;
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// How long a drag has to rest on a folder before it opens — a
/// spring-loaded folder. Finder's default "medium" delay is about this;
/// much shorter and passing over a folder on the way somewhere else
/// opens it, much longer and the user has let go before it does.
pub const SPRING_DELAY: Duration = Duration::from_millis(700);

/// How long a folder a drag opened takes to fade in.
pub const ARRIVE: Duration = Duration::from_millis(240);

/// How much of the listing is covered when a sprung folder starts to
/// fade in. Not all of it: a flash to a blank card would be its own jolt.
const ARRIVE_COVER: f32 = 0.85;

/// How far the pointer has to move after a folder springs open before
/// another can arm. Without it a drag held still opens the folder under
/// it, then whatever row of the new listing lands under the pointer,
/// and so on down — a cascade nobody asked for.
pub const REST_SLOP: f32 = 8.0;

/// How long picking a card up, and putting it down, take.
const LIFT: Duration = Duration::from_millis(160);

/// Where the card sits relative to the pointer: below and to the right,
/// so the pointer's tip stays on what it is over and the card never
/// hides the folder it is about to land in.
pub const CARD_OFFSET: (f32, f32) = (14.0, 12.0);

/// The card under the pointer.
#[derive(Debug, Clone)]
pub struct Ghost {
    pub card: DragCard,
    /// The pointer, in window coordinates; `None` until it first moves.
    pub at: Option<Point>,
    lift: Animation<bool>,
}

impl Ghost {
    /// Picked up now. Rises with a slight overshoot — up a little past
    /// held and back — which is what reads as "lifted" rather than
    /// "appeared".
    pub fn pick_up(card: DragCard, now: Instant) -> Ghost {
        let lift = Animation::new(false).duration(LIFT).easing(Easing::EaseOutBack).go(true, now);
        Ghost { card, at: None, lift }
    }

    /// Let go — dropped, or abandoned. Sinks and fades where it is,
    /// which over a folder reads as going into it.
    pub fn put_down(&mut self, now: Instant) {
        if self.lift.value() {
            // Settles without overshoot: a card dropped into a folder
            // that bounced on its way in would look like a refusal.
            self.lift = Animation::new(true).duration(LIFT).easing(Easing::EaseInCubic).go(false, now);
        }
    }

    /// How picked up it is: `0.0` gone, `1.0` held, a little over `1.0`
    /// at the top of the pick-up's overshoot.
    pub fn lift(&self, now: Instant) -> f32 {
        self.lift.interpolate(0.0, 1.0, now)
    }

    /// Whether it is still moving on its own, so the window keeps
    /// redrawing.
    pub fn animating(&self, now: Instant) -> bool {
        self.lift.is_animating(now)
    }

    /// Put down, and finished sinking: nothing left to draw.
    pub fn gone(&self, now: Instant) -> bool {
        !self.lift.value() && !self.lift.is_animating(now)
    }

    /// How big it is drawn: a touch small at the start of the pick-up,
    /// growing into place, so it lifts out of the row rather than
    /// fading in over it.
    pub fn scale(&self, now: Instant) -> f32 {
        0.92 + 0.08 * self.lift(now)
    }
}

/// What the card says about `paths`: the first one's name, how many
/// there are, and whether letting go copies.
pub fn card_for(paths: &[PathBuf], copying: bool) -> DragCard {
    let label = paths
        .first()
        .and_then(|p| p.file_name())
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    DragCard { label, count: paths.len(), copying }
}

/// How much of the wait before a folder opens has passed, `0.0`–`1.0`.
pub fn opening(armed: Instant, now: Instant) -> f32 {
    (now.saturating_duration_since(armed).as_secs_f32() / SPRING_DELAY.as_secs_f32()).clamp(0.0, 1.0)
}

/// How covered a folder that just sprang open still is, from
/// [`ARRIVE_COVER`] down to `0.0`, easing out so most of it clears early.
pub fn arrival_cover(started: Instant, now: Instant) -> f32 {
    let t = (now.saturating_duration_since(started).as_secs_f32() / ARRIVE.as_secs_f32()).clamp(0.0, 1.0);
    let eased = 1.0 - (1.0 - t).powi(3);
    ARRIVE_COVER * (1.0 - eased)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_card_rises_past_held_and_settles_there() {
        let t0 = Instant::now();
        let ghost = Ghost::pick_up(card_for(&[PathBuf::from("/a/x.txt")], false), t0);
        assert!(ghost.lift(t0) < 0.1, "it starts flat");
        let peak = (1..20).map(|i| ghost.lift(t0 + LIFT * i / 20)).fold(0.0_f32, f32::max);
        assert!(peak > 1.0, "the overshoot is what makes it read as lifted: {peak}");
        assert!((ghost.lift(t0 + LIFT * 2) - 1.0).abs() < 1e-3);
        assert!(!ghost.animating(t0 + LIFT * 2));
    }

    #[test]
    fn a_card_put_down_sinks_and_then_is_gone() {
        let t0 = Instant::now();
        let mut ghost = Ghost::pick_up(card_for(&[PathBuf::from("/a/x.txt")], false), t0);
        let t1 = t0 + LIFT * 2;
        ghost.put_down(t1);
        assert!(!ghost.gone(t1), "it sinks first");
        let midway = ghost.lift(t1 + LIFT / 2);
        assert!(midway > 0.0 && midway < 1.0, "{midway}");
        assert!(ghost.gone(t1 + LIFT * 2));
    }

    #[test]
    fn the_card_names_the_first_thing_and_counts_the_rest() {
        let card = card_for(&[PathBuf::from("/a/notes.txt"), PathBuf::from("/a/b.png")], true);
        assert_eq!(card, DragCard { label: "notes.txt".into(), count: 2, copying: true });
    }

    #[test]
    fn a_folder_fills_over_the_wait_and_never_past_it() {
        let t0 = Instant::now();
        assert_eq!(opening(t0, t0), 0.0);
        assert!((opening(t0, t0 + SPRING_DELAY / 2) - 0.5).abs() < 1e-3);
        assert_eq!(opening(t0, t0 + SPRING_DELAY * 3), 1.0);
    }

    #[test]
    fn a_sprung_folder_fades_in_from_mostly_covered_to_clear() {
        let t0 = Instant::now();
        assert!((arrival_cover(t0, t0) - ARRIVE_COVER).abs() < 1e-6);
        assert!(arrival_cover(t0, t0 + ARRIVE / 2) < ARRIVE_COVER / 2.0, "most of it clears early");
        assert_eq!(arrival_cover(t0, t0 + ARRIVE), 0.0);
    }
}
