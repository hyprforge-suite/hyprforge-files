//! The undo history: what Ctrl+Z would take back next, and what waits
//! behind it.
//!
//! The notice under the listing ("Moved 3 items to the Trash · Undo") is
//! gone after a few seconds, and with it any way of seeing what undo is
//! holding. Explorer and Dolphin keep that entirely hidden until you
//! press the key and find out; this shows it.
//!
//! Only the newest row has an Undo button. That is not a shortcut taken:
//! each undo checks that the folder still looks the way the record left
//! it (`jobs::undo`), and taking an older record back first would check
//! against a state the newer one has since changed. The rows behind it
//! are history, and say so.

use super::{App, BrowserMessage, Message};
use hyprforge_files_core::action::Action;
use hyprforge_ui::theme::{spacing, FontScale, BASE_TEXT_SIZE};
use hyprforge_ui::widgets::{divider, meta_text, popover_card, scaled_text, secondary_button};
use iced::widget::{column, container, mouse_area, opaque, row, scrollable, Space};
use iced::{Element, Length, Padding};

/// How wide the popover is, at 100% — the transfers popover's width, so
/// the window's two popovers read as one kind of thing.
const WIDTH: f32 = 380.0;
/// How far above the window's bottom edge it hangs, at 100%: clear of
/// the status bar and the notice row whose History button opens it.
const LIFT: f32 = 76.0;

/// The layer over the window while the history is open.
pub(crate) fn overlay(app: &App, scale: FontScale) -> Option<Element<'_, Message>> {
    app.undo_history_open.then(|| popover(app, scale))
}

fn popover(app: &App, scale: FontScale) -> Element<'_, Message> {
    let mut list = column![padded(scaled_text("Undo history", BASE_TEXT_SIZE, scale))].spacing(spacing::XS);
    list = list.push(divider());

    let mut rows = column![].spacing(spacing::XS);
    for (i, done) in app.undo.newest_first().enumerate() {
        let text = scaled_text(done.describe(), BASE_TEXT_SIZE, scale).width(Length::Fill);
        let line: Element<'_, Message> = if i == 0 {
            // The action, not its key, for the reason the notice gives:
            // Ctrl+Z may have been rebound.
            row![text, secondary_button("Undo").on_press(Message::Browser(BrowserMessage::Perform(Action::Undo)))]
                .spacing(spacing::SM)
                .align_y(iced::Alignment::Center)
                .into()
        } else {
            row![text.color(hyprforge_ui::theme::text_dim())].into()
        };
        rows = rows.push(padded(line));
    }
    if app.undo.is_empty() {
        rows = rows.push(padded(meta_text("Nothing to undo yet.", BASE_TEXT_SIZE, scale)));
    }
    list = list.push(scrollable(rows).height(Length::Shrink));

    if app.undo.len() > 1 {
        list = list.push(divider());
        list = list.push(padded(meta_text(
            "Undo works newest first: each one checks the folder is as it was left.",
            hyprforge_files_core::density::META_TEXT_BASE,
            scale,
        )));
    }

    let card = opaque(popover_card(list.padding([spacing::SM, 0.0])).width(Length::Fixed(scale.apply(WIDTH))));
    let placed = container(card)
        .width(Length::Fill)
        .height(Length::Fill)
        .align_x(iced::alignment::Horizontal::Right)
        .align_y(iced::alignment::Vertical::Bottom)
        .padding(Padding { bottom: scale.apply(LIFT), right: spacing::MD, ..Padding::ZERO });
    // Any click outside closes it and takes the click, as the transfers
    // popover's backdrop does.
    let backdrop = mouse_area(Space::new().width(Length::Fill).height(Length::Fill))
        .on_press(Message::UndoHistory(false))
        .on_right_press(Message::UndoHistory(false));
    iced::widget::stack![backdrop, placed].into()
}

fn padded<'a>(content: impl Into<Element<'a, Message>>) -> Element<'a, Message> {
    container(content).width(Length::Fill).padding([0.0, spacing::MD]).into()
}
