//! The tab strip, and the one shape iced cannot express.
//!
//! An active tab's outline runs up its left side, across the top, down
//! its right side — and stops. That missing bottom edge is the whole
//! trick: the stroke spills into the pane below instead of enclosing a
//! box, which is what makes the thing a tab rather than a floating pill.
//!
//! iced's [`Border`](iced::Border) carries a single `width` for all four
//! sides, so there is no way to ask a container for three of them. The
//! shape is drawn instead: a canvas paints the fill and the three-sided
//! stroke, and the tab's actual contents — icon, label, close — sit on
//! top of it in a [`stack`](iced::widget::stack()).
//!
//! # How an inactive tab recedes
//!
//! By subtraction, not by tinting. No fill, no outline, dimmer text, and
//! four pixels shorter. What is left is a label floating on the window's
//! own surface, so the strip reads as one continuous piece of chrome
//! with a single card lifted out of it.
//!
//! The radius and padding are still declared for an inactive tab even
//! though nothing paints them, so that the moment one is hovered or
//! activated a background appears in exactly the right shape with no
//! reflow. And its identity mark keeps full saturation while the text
//! dims — an inactive tab should still tell you at a glance what it is.

use hyprforge_ui::theme::FontScale;
use iced::widget::canvas;
use iced::{mouse, Element, Length, Point, Rectangle, Renderer, Size, Theme};

/// Clear space above the tallest tab, between it and the window's own
/// edge.
///
/// Without it the active tab's top corners land against the window
/// frame and the two rounded edges read as one shape — the tab stops
/// looking like something sitting *in* the window and starts looking
/// like part of its border. The gap is what separates the two.
///
/// It is also why the strip is taller than the tallest tab rather than
/// exactly as tall: this space is the difference, so the two cannot
/// drift apart when either number changes.
///
/// Raised from 12: at that height the gap read as a hairline rather than
/// as deliberate space, and the strip looked crowded against the top of
/// the window — the tabs need enough room above them to read as sitting
/// *in* the window rather than clinging to its edge.
pub const STRIP_TOP_PAD: f32 = 20.0;

/// The strip's own height: the tallest tab, plus the clear space above
/// it. Tabs sit on the strip's bottom edge — see [`STRIP_ALIGNMENT`].
pub const STRIP_HEIGHT: f32 = ACTIVE_HEIGHT + STRIP_TOP_PAD;

/// An active tab's height; four taller than an inactive one, which is
/// one of the four things that mark it.
pub const ACTIVE_HEIGHT: f32 = 32.0;

/// An inactive tab's height.
pub const INACTIVE_HEIGHT: f32 = 28.0;

/// Tabs are pinned to the **bottom** of the strip, not centred in it.
///
/// This is the mechanic the whole strip depends on. Bottom-aligned, tabs
/// of different heights share a baseline with the pane below and grow
/// upward, so the active one reads as rising out of the content. Centred,
/// they would float in the middle of the strip with a gap underneath and
/// the taller one would grow in both directions, which reads as a
/// toolbar of pills.
pub const STRIP_ALIGNMENT: iced::Alignment = iced::Alignment::End;

/// Every tab is this wide, whatever its label says.
///
/// Fixed rather than sized to the name, and that is a real decision
/// rather than a simplification. A tab that fits its label makes the
/// whole strip reflow whenever you navigate — every other tab slides
/// sideways because *this* one is now in a directory with a longer name,
/// and the one you were about to click is no longer under the pointer. A
/// fixed width means a tab stays where it was put, which is what makes
/// the strip usable as a set of targets.
///
/// The cost is that a long name truncates. That is the right trade: the
/// tail of a directory name is the part that distinguishes it, but a tab
/// is a *place marker* rather than a label, and the window title and the
/// path bar both say the full name.
pub const TAB_WIDTH: f32 = 150.0;

/// The corner radius on a tab's two top corners. The bottom two stay
/// square: a tab meets the pane, it does not sit on it.
///
/// Public because the host paints the tab's *fill* while this module
/// draws its *outline*, and a fill with a different radius from the
/// outline over it is visible as a hairline of background in each
/// corner. One constant, so they cannot drift.
pub const TAB_RADIUS: f32 = 8.0;

/// How much of the accent the active tab's outline carries.
///
/// A fifth. At full strength the outline competes with the selected row
/// in the listing, and this is chrome — it should say "this one" quietly.
/// It is still the only accent-coloured stroke anywhere in the chrome,
/// which is what makes it legible at this weight.
const OUTLINE_ALPHA: f32 = 0.2;

/// The three-sided outline, drawn **over** the tab's contents.
///
/// Over rather than under because it is only a 1px stroke at the very
/// edges — the fill is the button's own background, so nothing here
/// covers a glyph — and because a canvas drawn *under* would have to be
/// the stack's first child, which is what decides the stack's size.
///
/// Both dimensions are fixed, so this and the button beneath it are
/// laid out to exactly the same box and the stroke lands on the tab's
/// real edges rather than on wherever a fill-width canvas ended up.
pub fn outline<'a, Message: 'a>(active: bool, width: f32, height: f32) -> Element<'a, Message> {
    canvas(TabShape { active }).width(Length::Fixed(width)).height(Length::Fixed(height)).into()
}

struct TabShape {
    active: bool,
}

impl<Message> canvas::Program<Message, Theme, Renderer> for TabShape {
    type State = ();

    fn draw(
        &self,
        _state: &Self::State,
        renderer: &Renderer,
        theme: &Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        let mut frame = canvas::Frame::new(renderer, bounds.size());
        if !self.active {
            // Nothing at all. An inactive tab is unpainted, not painted
            // faintly — see this module's own doc.
            return vec![frame.into_geometry()];
        }

        let w = bounds.width;
        let h = bounds.height;
        let r = TAB_RADIUS.min(w / 2.0).min(h);

        // Up the left, across the top, down the right — and stop. The
        // missing bottom edge is the whole point; see the module doc.
        // Inset by half the stroke width so the line sits inside the
        // tab's bounds rather than half outside them.
        let inset = 0.5;
        let (w, h) = (w - inset, h);
        let outline = canvas::Path::new(|b| {
            b.move_to(Point::new(inset, h));
            b.line_to(Point::new(inset, r));
            b.quadratic_curve_to(Point::new(inset, inset), Point::new(r, inset));
            b.line_to(Point::new(w - r, inset));
            b.quadratic_curve_to(Point::new(w, inset), Point::new(w, r));
            b.line_to(Point::new(w, h));
        });
        let accent = theme.extended_palette().primary.weak.color;
        frame.stroke(
            &outline,
            canvas::Stroke {
                style: canvas::Style::Solid(iced::Color { a: OUTLINE_ALPHA, ..accent }),
                width: 1.0,
                ..canvas::Stroke::default()
            },
        );

        vec![frame.into_geometry()]
    }
}

/// A tab's identity mark: the same folder shape the sidebar draws.
///
/// `icon::folder_mark` and not a second rounded rectangle here — the
/// two were built independently and had already drifted (11x9 against
/// the shared 0.8 aspect, a 0.2 corner against 0.17), so the folder in
/// a tab was not the folder in the sidebar. The mark keeps full
/// saturation even on an inactive tab whose text has dimmed, because
/// that is exactly when a glance needs it.
///
/// There was a `remote` flag here that drew a circle instead — round
/// means host, square means folder. Nothing ever passed `true`: no tab
/// carries a field that could say so. Worth having when there is a
/// remote place to mark; it is not code until then.
pub fn identity_mark<'a, Message: 'a>(color: iced::Color, scale: FontScale) -> Element<'a, Message> {
    hyprforge_files_core::icon::folder_mark(color, MARK_FOLDER, scale)
}

/// The tab mark's width; its height follows from `icon`'s own folder
/// aspect ratio.
const MARK_FOLDER: f32 = 11.0;

/// The size a tab's canvas reports for a given state, so the strip and
/// the shape cannot disagree about how tall a tab is.
pub fn height(active: bool) -> f32 {
    if active {
        ACTIVE_HEIGHT
    } else {
        INACTIVE_HEIGHT
    }
}

/// The `+` button's hit area — as tall as an inactive tab so it shares
/// their baseline, and present but unpainted, the same treatment.
pub fn new_tab_size() -> Size {
    Size::new(26.0, INACTIVE_HEIGHT)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The four-pixel difference is load-bearing: it is one of the four
    /// things that mark the active tab, and with the strip bottom-aligned
    /// it is what makes that tab rise out of the pane rather than float
    /// in the strip.
    #[test]
    fn an_active_tab_stands_taller_than_an_inactive_one() {
        assert!(height(true) > height(false));
        assert_eq!(height(true) - height(false), 4.0);
    }

    /// Both fit inside the strip with room above — a tab as tall as its
    /// strip has nowhere to rise from, and its top corners would meet
    /// the window frame.
    #[test]
    fn every_tab_fits_inside_the_strip() {
        assert!(height(true) < STRIP_HEIGHT);
        assert!(height(false) < STRIP_HEIGHT);
    }

    /// The clear space above the active tab is exactly `STRIP_TOP_PAD`,
    /// by construction rather than by coincidence. Pinned because the
    /// obvious edit — nudging `STRIP_HEIGHT` to "make room" — would
    /// silently make the two disagree.
    #[test]
    fn the_space_above_the_tallest_tab_is_the_padding_and_nothing_else() {
        assert_eq!(STRIP_HEIGHT - height(true), STRIP_TOP_PAD);
    }

    /// The `+` shares the inactive tabs' baseline rather than being
    /// centred on its own, which is what keeps the strip reading as one
    /// row instead of a row with a floating button at the end.
    #[test]
    fn the_new_tab_button_matches_an_inactive_tab_in_height() {
        assert_eq!(new_tab_size().height, height(false));
    }

    /// A tab is wider than the `+` beside it — the two are different
    /// kinds of target and should not read as a row of equal cells.
    ///
    /// The property that actually matters here, that a tab's width does
    /// not depend on its label, is not assertable: it is enforced by
    /// `TAB_WIDTH` being a constant rather than a function of the name.
    /// An earlier version of this test asserted `TAB_WIDTH > 0.0`, which
    /// clippy correctly rejected as a constant assertion — a test that
    /// cannot fail is not a test.
    #[test]
    fn a_tab_is_a_bigger_target_than_the_new_tab_button() {
        assert!(TAB_WIDTH > new_tab_size().width);
    }
}
