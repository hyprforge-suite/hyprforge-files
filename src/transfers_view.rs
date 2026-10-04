//! Transfers — mockup `1f` — drawn: the control in the tab strip, the
//! popover it opens, and the queue view behind "Show all".
//!
//! The work itself is the window's queue (`App::enqueue`, `pump`,
//! `begin` in `main.rs`) and the jobs it starts; what is decided about
//! that work without a window — the overall bar, the session's history,
//! why something waits — is `hyprforge_files::transfers`. This module is
//! the part that needs `App` and `Message`: reading the one into the
//! other, and the few answers a click on it gives.
//!
//! **Where it lives.** At the far end of the tab strip, not in the
//! status bar the mockup's `1b` draws. The status bar is
//! `hyprforge-files-core`'s, rendered by the open/save dialog too, and a
//! dialog has no jobs; the strip is this window's own chrome, and the
//! work it reports belongs to the window rather than to whichever tab
//! is showing — the same reason a browser's downloads button sits there.
//!
//! **A popover, then a view in the window — not a second window.**
//! The mockup draws a "full queue window". On Hyprland a second toplevel
//! is tiled: opening it would rearrange the person's layout to show a
//! list, and closing it would rearrange it back. Every other question
//! this window asks — the conflict, the password, the chooser — is a
//! card over the window for that reason, and the queue view is one more.
//! The jobs are threads of this process in any case, so a window that
//! outlived this one would have nothing to show.
//!
//! **What closing hides.** Nothing that matters. The popover closes on
//! any click outside it and on Escape, and the queue view on Escape and
//! its own button — but a failure is never only in either. It stays in
//! the panel under the listing until it is dismissed, and in the queue
//! view's history after that, until "Clear finished".

use super::{
    App, BrowserMessage, JobKind, Message, Queued, QueuedWork, RunningJob,
};
use hyprforge_files::archive_jobs::Work;
use hyprforge_files::jobs::{JobId, JobSummary};
use hyprforge_files::transfers::{self, Batch, Finished, History, Outcome, Reasons, Retry, Waiting};
use hyprforge_files_core::clipboard::PasteStep;
use hyprforge_fileops::Progress;
use hyprforge_ui::theme::{spacing, FontScale, BASE_TEXT_SIZE};
use hyprforge_ui::widgets::{
    divider, meta_text, popover_card, primary_button, progress_line, scaled_text, secondary_button,
    section_label, status_dot, Tint,
};
use iced::widget::{button, column, container, mouse_area, opaque, row, scrollable, Space};
use iced::{Background, Border, Color, Element, Length, Padding, Task, Theme};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Everything the window keeps about transfers beyond the queue itself.
#[derive(Debug, Default)]
pub(crate) struct Transfers {
    pub history: History<RetryWork>,
    pub batch: Batch,
    pub open: Panel,
}

impl Transfers {
    pub fn is_open(&self) -> bool {
        self.open != Panel::Closed
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Panel {
    #[default]
    Closed,
    Popover,
    Queue,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum TransfersMessage {
    /// The strip's control: open the popover, or close it.
    Toggle,
    /// "Show all": the queue view.
    ShowQueue,
    Close,
    Retry(JobId),
    /// Go to where a finished job put something, and select it.
    Show(PathBuf),
    ClearFinished,
}

/// A failed job's work, kept so it can be asked for again.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum RetryWork {
    /// The items of a paste or a restore that landed nothing — see
    /// `JobSummary::retry` for why only those.
    Paste {
        kind: JobKind,
        steps: Vec<PasteStep>,
        /// So a retried drop still leaves the clipboard alone.
        from_drop: bool,
    },
    Archive(Work),
}

/// Whether a failed run of `work` can simply be run again.
///
/// An edit and a compression both write a whole new archive beside the
/// old one and rename it into place (`hyprforge_archive`'s
/// `rewrite_in_place`), so a failure leaves the disk as it was and
/// running it again is the original request. An extraction is not:
/// it unpacks member by member into a folder, and one that stopped
/// partway has put some of them down — a retry would make a second
/// folder beside the half-full first one, or collide with it.
pub(crate) fn retryable(work: &Work) -> bool {
    matches!(work, Work::Edit { .. } | Work::Compress { .. })
}

/// Adds a finished job to the history.
///
/// Everything that was ever on screen, and everything that did not
/// simply work — but not a quick job that did: a paste of three small
/// files finishes before its progress would be shown (`[behaviour]
/// progress-after-ms`), is visible in the listing already, and an entry
/// for it would make the strip's control appear for something nobody
/// saw happen.
pub(crate) fn record(
    transfers: &mut Transfers,
    finished: &RunningJob,
    summary: &JobSummary,
    from_drop: bool,
    shown_after: Duration,
) {
    let outcome = Outcome::of(summary);
    if outcome == Outcome::Done && finished.started.elapsed() < shown_after {
        return;
    }
    let retry = match (&finished.kind, outcome) {
        (_, Outcome::Done) => None,
        (JobKind::Copy | JobKind::Move, _) if !summary.retry.is_empty() => Some(Retry {
            items: summary.retry.len(),
            work: RetryWork::Paste { kind: finished.kind.clone(), steps: summary.retry.clone(), from_drop },
        }),
        // A restore's records go with its steps: each retried item still
        // has to lose its `.trashinfo` once it is back.
        (JobKind::Restore { records }, _) if !summary.retry.is_empty() => {
            let records = records
                .iter()
                .filter(|(stored, _)| summary.retry.iter().any(|s| &s.source == stored))
                .cloned()
                .collect();
            Some(Retry {
                items: summary.retry.len(),
                work: RetryWork::Paste {
                    kind: JobKind::Restore { records },
                    steps: summary.retry.clone(),
                    from_drop,
                },
            })
        }
        (JobKind::Archive { .. }, Outcome::Failed) if !summary.cancelled => {
            finished.retry.clone().map(|work| Retry { items: 1, work: RetryWork::Archive(work) })
        }
        _ => None,
    };
    // Said whenever something failed that Retry does not cover — with
    // or without a Retry beside it, because a button that retries one
    // item of two reads as covering both.
    let note = match (&finished.kind, outcome) {
        (JobKind::Archive { .. }, Outcome::Failed) if retry.is_none() => {
            Some("What was unpacked before it stopped is still there.".to_string())
        }
        (JobKind::Archive { .. }, _) => None,
        (_, Outcome::Failed) if summary.partly > 0 => Some(format!(
            "{} partly arrived, so running {} again would collide with what did. Paste {} again to choose what happens.",
            transfers::plural(summary.partly, "item", "items"),
            if summary.partly == 1 { "it" } else { "them" },
            if summary.partly == 1 { "it" } else { "them" },
        )),
        _ => None,
    };
    transfers.history.push(Finished {
        id: finished.id,
        done_word: capitalised(finished.kind.done()),
        subject: finished.subject.clone(),
        outcome,
        reasons: Reasons::new(&summary.failed),
        // What it put down — or, for an edit, which puts nothing new
        // down, the archive it changed.
        show: summary.placed.first().map(|(_, landed)| landed.clone()).or_else(|| finished.archive.clone()),
        retry,
        note,
        seen: outcome != Outcome::Failed,
    });
}

/// "copied" → "Copied". The words are a fixed handful, so this only
/// ever meets ASCII.
fn capitalised(word: &'static str) -> &'static str {
    match word {
        "copied" => "Copied",
        "moved" => "Moved",
        "restored" => "Restored",
        "extracted" => "Extracted",
        "compressed" => "Compressed",
        _ => "Updated",
    }
}

impl App {
    pub(crate) fn transfers_update(&mut self, message: TransfersMessage) -> Task<Message> {
        match message {
            TransfersMessage::Toggle => {
                self.transfers.open = match self.transfers.open {
                    Panel::Popover => Panel::Closed,
                    _ => Panel::Popover,
                };
                Task::none()
            }
            TransfersMessage::ShowQueue => {
                self.transfers.open = Panel::Queue;
                Task::none()
            }
            TransfersMessage::Close => {
                self.transfers.open = Panel::Closed;
                Task::none()
            }
            TransfersMessage::ClearFinished => {
                self.transfers.history.clear();
                Task::none()
            }
            TransfersMessage::Show(path) => {
                self.transfers.open = Panel::Closed;
                let Some(folder) = path.parent().map(Path::to_path_buf) else {
                    return Task::none();
                };
                let go = self.update(Message::Browser(BrowserMessage::Navigate(folder)));
                // After the navigation, which sets its own selection when
                // it goes up a level and would replace this one.
                let active = self.active;
                self.tabs[active]
                    .browser
                    .update(BrowserMessage::AfterListing { path, rename: false });
                go
            }
            TransfersMessage::Retry(id) => match self.transfers.history.take_retry(id) {
                Some(work) => self.retry(work),
                None => Task::none(),
            },
        }
    }

    /// Queues failed work again, through the same queue as the first
    /// time — so it waits its turn, and an archive edit still never
    /// runs beside another edit of the same archive.
    fn retry(&mut self, work: RetryWork) -> Task<Message> {
        match work {
            RetryWork::Paste { kind, steps, from_drop } => {
                let mut dirs: Vec<PathBuf> = Vec::new();
                let mut touch = |path: &Path| {
                    if let Some(parent) = path.parent() {
                        if !dirs.iter().any(|d| d == parent) {
                            dirs.push(parent.to_path_buf());
                        }
                    }
                };
                for step in &steps {
                    touch(&step.dest);
                    if kind != JobKind::Copy {
                        touch(&step.source);
                    }
                }
                let id = self.next_job_id;
                self.next_job_id += 1;
                if from_drop {
                    self.from_drops.insert(id);
                }
                self.enqueue(Queued { id, kind, dirs, what: QueuedWork::Paste { steps } })
            }
            RetryWork::Archive(work) => {
                // A compression names a file that did not exist when it
                // was asked for. If something is there now, writing over
                // it would be the retry destroying a file the first
                // attempt never touched.
                if let Work::Compress { dest, .. } = &work {
                    if std::fs::symlink_metadata(dest).is_ok() {
                        self.status = Some(format!(
                            "Something called \u{201C}{}\u{201D} is there now, so it was left alone.",
                            dest.file_name().map(|n| n.to_string_lossy()).unwrap_or_default()
                        ));
                        return Task::none();
                    }
                }
                self.start_archive_job(work)
            }
        }
    }
}

// --- the control in the strip ----------------------------------------------

/// How wide the strip control's own bar is, at 100%.
const INDICATOR_BAR_WIDTH: f32 = 56.0;

/// The strip's control, or `None` when there is nothing to report and
/// nothing to look back at.
///
/// Running work appears only once it has run for `progress-after-ms`,
/// as the old panel did: a paste of three small files finishes before
/// the control could be read, and a control that flashes up and
/// vanishes reads as something going wrong.
pub(crate) fn indicator(app: &App, scale: FontScale) -> Option<Element<'_, Message>> {
    let after = Duration::from_millis(app.config.behaviour.progress_after_ms);
    let active = app.jobs.iter().any(|j| j.started.elapsed() >= after) || !app.queued.is_empty();
    let failed = app.transfers.history.unseen_failures().count();
    if !active && app.transfers.history.is_empty() && !app.transfers.is_open() {
        return None;
    }

    let (label, tint): (String, Option<Tint>) = if active || !app.jobs.is_empty() {
        let total = app.jobs.len() + app.queued.len();
        let single = (total == 1).then(|| {
            app.jobs
                .first()
                .map(|j| j.kind.doing())
                .or_else(|| app.queued.front().map(|q| q.kind.doing()))
        });
        let fraction = app.transfers.batch.fraction();
        (transfers::indicator_label(app.jobs.len(), app.queued.len(), single.flatten(), fraction), None)
    } else if failed > 0 {
        (format!("{} failed", transfers::plural(failed, "transfer", "transfers")), Some(Tint::Error))
    } else {
        ("Transfers".to_string(), Some(Tint::Dim))
    };

    let mut inside = row![].spacing(spacing::SM).align_y(iced::Alignment::Center);
    if let (true, Some(fraction)) = (!app.jobs.is_empty(), app.transfers.batch.fraction()) {
        inside = inside.push(
            container(progress_line(fraction, scale)).width(Length::Fixed(scale.apply(INDICATOR_BAR_WIDTH))),
        );
    }
    let text = scaled_text(label, hyprforge_files_core::density::META_TEXT_BASE, scale);
    inside = inside.push(match tint {
        Some(tint) => text.color(tint.iced()),
        None => text.color(hyprforge_ui::theme::text()),
    });

    let open = app.transfers.open == Panel::Popover;
    let height = scale.apply(hyprforge_files::tabstrip::INACTIVE_HEIGHT - INDICATOR_INSET);
    let control = button(container(inside).center_y(Length::Fill))
        .height(Length::Fixed(height))
        .padding([0.0, spacing::SM])
        .on_press(Message::Transfers(TransfersMessage::Toggle))
        .style(move |_t: &Theme, status| button::Style {
            // "On is filled": open, it has the row step under it, the
            // same fill as a hovered one — never the accent, which means
            // selected.
            background: (open || matches!(status, button::Status::Hovered))
                .then(|| Background::Color(hyprforge_ui::theme::surface::row())),
            text_color: hyprforge_ui::theme::text(),
            border: Border { radius: hyprforge_ui::density::inner_radius().into(), ..Border::default() },
            ..button::Style::default()
        });
    // Lifted off the strip's bottom edge, where the tabs meet the pane:
    // flush with it, the control read as one more tab.
    Some(container(control).padding(Padding { bottom: scale.apply(INDICATOR_INSET), ..Padding::ZERO }).into())
}

/// How far the control sits above the strip's bottom edge, at 100%.
const INDICATOR_INSET: f32 = 4.0;

// --- the popover and the queue view ----------------------------------------

/// How wide the popover is, at 100% — the mockup's 372, rounded.
const POPOVER_WIDTH: f32 = 380.0;
/// How wide the queue view's card may get, at 100%.
const QUEUE_WIDTH: f32 = 640.0;
/// Waiting rows the popover lists before the rest become a count.
/// Running rows are never cut: there are at most `App::AT_ONCE`.
const POPOVER_WAITING: usize = 4;
/// Finished rows the popover lists under nothing running.
const POPOVER_FINISHED: usize = 3;
/// Failure sentences a finished row lists before the rest become a count.
const REASONS_SHOWN: usize = 8;

/// The layer over the window while the popover or the queue view is
/// open.
pub(crate) fn overlay(app: &App, scale: FontScale) -> Option<Element<'_, Message>> {
    match app.transfers.open {
        Panel::Closed => None,
        Panel::Popover => Some(popover(app, scale)),
        Panel::Queue => Some(queue_view(app, scale)),
    }
}

fn popover(app: &App, scale: FontScale) -> Element<'_, Message> {
    let mut list = column![header(app, scale)].spacing(spacing::XS);
    list = list.push(divider());

    for job in &app.jobs {
        list = list.push(job_row(job, scale));
    }
    for waiting in app.queued.iter().take(POPOVER_WAITING) {
        list = list.push(queued_row(app, waiting, scale));
    }
    if app.queued.len() > POPOVER_WAITING {
        list = list.push(padded(meta_text(
            format!("and {} more waiting", app.queued.len() - POPOVER_WAITING),
            BASE_TEXT_SIZE,
            scale,
        )));
    }
    if app.jobs.is_empty() && app.queued.is_empty() {
        list = list.push(padded(meta_text("Nothing is running.", BASE_TEXT_SIZE, scale)));
        for entry in app.transfers.history.iter().take(POPOVER_FINISHED) {
            list = list.push(finished_row(entry, false, scale));
        }
    }

    list = list.push(divider());
    let finished = app.transfers.history.len();
    let more = if finished > 0 {
        format!("Show all \u{00B7} {} finished", finished)
    } else {
        "Show all".to_string()
    };
    list = list.push(padded(
        row![
            Space::new().width(Length::Fill),
            secondary_button(more).on_press(Message::Transfers(TransfersMessage::ShowQueue)),
        ]
        .align_y(iced::Alignment::Center),
    ));

    let card = opaque(
        popover_card(list.padding([spacing::SM, 0.0])).width(Length::Fixed(scale.apply(POPOVER_WIDTH))),
    );
    // Hung from the strip's far end, where the control is: the strip's
    // height is known, and the control is always its last element.
    let placed = container(card)
        .width(Length::Fill)
        .height(Length::Fill)
        .align_x(iced::alignment::Horizontal::Right)
        .padding(Padding {
            top: scale.apply(hyprforge_files::tabstrip::STRIP_HEIGHT) + spacing::XS,
            right: spacing::MD,
            ..Padding::ZERO
        });
    // Any click outside closes it — which includes a click on the
    // control itself, so the control still toggles — and takes the
    // click, as the context menu's own backdrop does. A right click
    // too: let through, it would open a context menu underneath.
    let backdrop = mouse_area(Space::new().width(Length::Fill).height(Length::Fill))
        .on_press(Message::Transfers(TransfersMessage::Close))
        .on_right_press(Message::Transfers(TransfersMessage::Close));
    iced::widget::stack![backdrop, placed].into()
}

/// "Transfers · 2 active · 1 queued", and Cancel all while anything is.
fn header(app: &App, scale: FontScale) -> Element<'_, Message> {
    let mut counts = Vec::new();
    if !app.jobs.is_empty() {
        counts.push(format!("{} active", app.jobs.len()));
    }
    if !app.queued.is_empty() {
        counts.push(format!("{} queued", app.queued.len()));
    }
    let mut line = row![scaled_text("Transfers", BASE_TEXT_SIZE, scale).color(hyprforge_ui::theme::text())]
        .spacing(spacing::SM)
        .align_y(iced::Alignment::Center);
    if !counts.is_empty() {
        line = line.push(meta_text(counts.join(" \u{00B7} "), BASE_TEXT_SIZE, scale));
    }
    line = line.push(Space::new().width(Length::Fill));
    if !app.jobs.is_empty() || !app.queued.is_empty() {
        line = line.push(secondary_button("Cancel all").on_press(Message::CancelAllJobs));
    }
    padded(line)
}

/// The queue view: everything running, waiting and finished this
/// session, on a card over the window.
fn queue_view(app: &App, scale: FontScale) -> Element<'_, Message> {
    let finished = app.transfers.history.len();
    let mut counts = vec![
        format!("{} running", app.jobs.len()),
        format!("{} waiting", app.queued.len()),
        format!("{finished} finished"),
    ];
    counts.retain(|c| !c.starts_with("0 "));

    let mut body = column![].spacing(spacing::XS);
    if !app.jobs.is_empty() {
        body = body.push(padded(section_label("Running", scale)));
        for job in &app.jobs {
            body = body.push(job_row(job, scale));
        }
    }
    if !app.queued.is_empty() {
        body = body.push(padded(section_label("Waiting", scale)));
        for waiting in &app.queued {
            body = body.push(queued_row(app, waiting, scale));
        }
    }
    if finished > 0 {
        body = body.push(padded(section_label("Finished this session", scale)));
        for entry in app.transfers.history.iter() {
            body = body.push(finished_row(entry, true, scale));
        }
    }
    if app.jobs.is_empty() && app.queued.is_empty() && finished == 0 {
        body = body.push(padded(meta_text(
            "Nothing has been copied, moved or unpacked that took long enough to list.",
            BASE_TEXT_SIZE,
            scale,
        )));
    }

    let mut footer = row![].spacing(spacing::SM).align_y(iced::Alignment::Center);
    if !app.jobs.is_empty() || !app.queued.is_empty() {
        footer = footer.push(secondary_button("Cancel all").on_press(Message::CancelAllJobs));
    }
    footer = footer.push(Space::new().width(Length::Fill));
    if finished > 0 {
        footer = footer.push(
            secondary_button("Clear finished").on_press(Message::Transfers(TransfersMessage::ClearFinished)),
        );
    }
    footer = footer.push(primary_button("Close").on_press(Message::Transfers(TransfersMessage::Close)));

    let mut title = row![scaled_text("Transfers", 17.0, scale)].spacing(spacing::SM).align_y(iced::Alignment::Center);
    if !counts.is_empty() {
        title = title.push(meta_text(counts.join(" \u{00B7} "), BASE_TEXT_SIZE, scale));
    }

    let card = popover_card(
        column![
            padded(title),
            divider(),
            // The list scrolls and the title and buttons do not: a
            // session's worth of history must not push Close off the
            // bottom of the window. Bounded by the window's own height,
            // which the window knows and the layout cannot be asked —
            // a scrollable that is merely `Shrink` grows to its content.
            container(scrollable(body).height(Length::Shrink)).max_height(list_room(app, scale)),
            divider(),
            padded(footer),
        ]
        .spacing(spacing::SM)
        .padding([spacing::MD, 0.0]),
    )
    .max_width(scale.apply(QUEUE_WIDTH));

    // The dialogs' scrim and placement, so the queue view reads as one
    // more card over the window rather than a new kind of thing.
    opaque(
        container(card)
            .center_x(Length::Fill)
            .center_y(Length::Fill)
            .padding(spacing::XL)
            .style(|_t: &Theme| container::Style {
                background: Some(Background::Color(Color {
                    a: super::SCRIM_ALPHA,
                    ..hyprforge_ui::theme::surface::root()
                })),
                ..container::Style::default()
            }),
    )
}

/// How tall the queue view's list may be: the window, less the scrim's
/// margin and the card's title and buttons.
fn list_room(app: &App, scale: FontScale) -> f32 {
    let window = app.last_window_size.1 as f32;
    (window - 2.0 * spacing::XL - scale.apply(QUEUE_CHROME)).max(scale.apply(QUEUE_CHROME))
}

/// The queue view's title, buttons, dividers and padding, at 100% —
/// what [`list_room`] keeps clear.
const QUEUE_CHROME: f32 = 150.0;

// --- rows -------------------------------------------------------------------

fn padded<'a>(content: impl Into<Element<'a, Message>>) -> Element<'a, Message> {
    container(content).width(Length::Fill).padding([0.0, spacing::MD]).into()
}

/// One running job: what, how far, how fast, how much longer, and a
/// way to stop it. The mockup's row, less its Pause — see DESIGN.md.
fn job_row<'a>(job: &RunningJob, scale: FontScale) -> Element<'a, Message> {
    let title = format!("{} {}", job.kind.doing(), job.subject);
    let fraction = job.fraction();
    let mut top = row![scaled_text(title, BASE_TEXT_SIZE, scale)
        .color(hyprforge_ui::theme::text())
        .wrapping(iced::widget::text::Wrapping::None)
        .width(Length::Fill)]
    .spacing(spacing::SM)
    .align_y(iced::Alignment::Center);
    if let Some(fraction) = fraction {
        top = top.push(meta_text(format!("{}%", transfers::percent(fraction)), BASE_TEXT_SIZE, scale));
    }
    top = top.push(secondary_button("Cancel").on_press(Message::CancelJob(job.id)));

    let mut rows = column![top].spacing(spacing::XS);
    // A bar only where there is a fraction. While the tree is still
    // being walked there is none, and "counting…" below says so.
    if let Some(fraction) = fraction {
        rows = rows.push(progress_line(fraction, scale));
    }
    rows = rows.push(meta_text(job_detail(job), BASE_TEXT_SIZE * 0.9, scale));
    container(rows).width(Length::Fill).padding([spacing::XS, spacing::MD]).into()
}

/// "412 of 1,338 · 1.2 GB of 3 GB · 18.2 MB/s · 4s left · preview.png".
fn job_detail(job: &RunningJob) -> String {
    if job.conflict.is_some() {
        return "Waiting for your answer".to_string();
    }
    let mut parts = Vec::new();
    // A job of several items says which one it is on, and the counts
    // after it are that item's: fileops counts each from zero.
    let several = job.items.of > 1;
    if several {
        parts.push(format!("item {} of {}", (job.items.done + 1).min(job.items.of), job.items.of));
    }
    match &job.progress {
        Some(Progress { entries_done, entries_total: Some(total), bytes_done, bytes_total, .. }) => {
            if !several {
                parts.push(format!("{entries_done} of {total}"));
            }
            // A total below what is already done is no total. fileops
            // up to 0.1.5 said 0 for a copy of one file (fixed in the
            // monorepo's copy, found live), which read "5.9 GiB of
            // 0 B"; against a published copy that predates the fix,
            // only what is done is said.
            match bytes_total.filter(|t| *t >= *bytes_done && *t > 0) {
                Some(bytes_total) => parts.push(format!(
                    "{} of {}",
                    hyprforge_files_core::human_readable_size(*bytes_done),
                    hyprforge_files_core::human_readable_size(bytes_total)
                )),
                None if *bytes_done > 0 => parts.push(hyprforge_files_core::human_readable_size(*bytes_done)),
                None => {}
            }
        }
        _ => parts.push("counting\u{2026}".to_string()),
    }
    if let Some(rate) = job.rate.per_second() {
        parts.push(rate);
    }
    // An estimate from the current item's bytes is the job's only on its
    // last item; before that it would promise "4s left" with whole
    // folders still to come.
    let last_item = job.items.done + 1 >= job.items.of;
    if let Some(left) = job.progress.as_ref().filter(|_| last_item).and_then(|p| job.rate.remaining(p)) {
        parts.push(left);
    }
    // The file it is on — the mockup's `preview.png`. Its name only: the
    // folder is in the title already, and a full path would wrap.
    if let Some(name) = job.progress.as_ref().and_then(|p| p.current.file_name()) {
        parts.push(name.to_string_lossy().into_owned());
    }
    parts.join(" \u{00B7} ")
}

/// Why `queued` has not started.
pub(crate) fn why_waiting(app: &App, queued: &Queued) -> Waiting {
    if let Some(archive) = queued.archive() {
        if app.jobs.iter().any(|running| running.archive.as_deref() == Some(archive)) {
            let name = archive.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            return Waiting::Behind(name);
        }
    }
    for dest in queued.lands_at() {
        if app.jobs.iter().any(|running| running.lands_at.iter().any(|d| d == dest)) {
            let name = dest.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            return Waiting::SameName(name);
        }
    }
    Waiting::Busy
}

/// One waiting job: what it will do, why it has not, and a way to call
/// it off. No Start button — see DESIGN.md: a job waits either because
/// starting it early gains nothing or because starting it early is the
/// data loss the queue exists to prevent.
fn queued_row<'a>(app: &App, queued: &Queued, scale: FontScale) -> Element<'a, Message> {
    let title = format!("{} {}", queued.kind.doing(), queued.subject());
    container(
        column![
            row![
                scaled_text(title, BASE_TEXT_SIZE, scale)
                    .color(hyprforge_ui::theme::text())
                    .wrapping(iced::widget::text::Wrapping::None)
                    .width(Length::Fill),
                meta_text("queued", BASE_TEXT_SIZE, scale),
                secondary_button("Cancel").on_press(Message::CancelJob(queued.id)),
            ]
            .spacing(spacing::SM)
            .align_y(iced::Alignment::Center),
            meta_text(why_waiting(app, queued).words(), BASE_TEXT_SIZE * 0.9, scale),
        ]
        .spacing(spacing::XS),
    )
    .width(Length::Fill)
    .padding([spacing::XS, spacing::MD])
    .into()
}

/// One finished job. In the queue view (`full`), a failure lists its
/// reasons and says what can be done about it.
fn finished_row<'a>(entry: &Finished<RetryWork>, full: bool, scale: FontScale) -> Element<'a, Message> {
    let (tint, word) = match entry.outcome {
        Outcome::Done => (Tint::Success, "done"),
        Outcome::Failed => (Tint::Error, "failed"),
        Outcome::Cancelled => (Tint::Dim, "cancelled"),
    };
    let mut top = row![
        status_dot(tint, scale),
        scaled_text(entry.title(), BASE_TEXT_SIZE, scale)
            .color(hyprforge_ui::theme::text())
            .wrapping(iced::widget::text::Wrapping::None)
            .width(Length::Fill),
        meta_text(word, BASE_TEXT_SIZE, scale).color(tint.iced()),
    ]
    .spacing(spacing::SM)
    .align_y(iced::Alignment::Center);
    if full {
        if let Some(path) = &entry.show {
            top = top.push(
                secondary_button("Show").on_press(Message::Transfers(TransfersMessage::Show(path.clone()))),
            );
        }
        if let Some(retry) = &entry.retry {
            // The count is said whenever Retry would not cover every
            // failure, so the button never claims more than it does.
            let label = match (retry.items, entry.note.is_some()) {
                (1, false) => "Retry".to_string(),
                (n, _) => format!("Retry {}", transfers::plural(n, "item", "items")),
            };
            top = top.push(secondary_button(label).on_press(Message::Transfers(TransfersMessage::Retry(entry.id))));
        }
    }
    let mut rows = column![top].spacing(spacing::XS);
    if full && entry.outcome == Outcome::Failed {
        rows = rows.push(meta_text(entry.headline(), BASE_TEXT_SIZE * 0.9, scale));
        let (shown, more) = entry.reasons.shown(REASONS_SHOWN);
        for reason in shown {
            rows = rows.push(meta_text(reason.clone(), BASE_TEXT_SIZE * 0.9, scale));
        }
        if more > 0 {
            rows = rows.push(meta_text(format!("and {more} more"), BASE_TEXT_SIZE * 0.9, scale));
        }
        if let Some(note) = &entry.note {
            rows = rows.push(meta_text(note.clone(), BASE_TEXT_SIZE * 0.9, scale));
        }
    }
    container(rows).width(Length::Fill).padding([spacing::XS, spacing::MD]).into()
}

// --- the failure panel -------------------------------------------------------

/// What went wrong, under the listing, until it is read.
///
/// Mockup `1f` gives failures a tab of their own, and the reason is
/// visible in what this replaced: they were joined into one sentence in
/// the status bar, which the *next* job's report then overwrote. A
/// paste of four hundred files that could not write three of them said
/// so for as long as it took to start anything else, and the three
/// names were gone.
///
/// So it stays until dismissed — a failure is the one outcome that has
/// to outlast the thing that produced it — and dismissing only marks it
/// read: the queue view still lists it, reasons and all, behind
/// "Details".
pub(crate) fn failures_panel<'a>(transfers: &Transfers, scale: FontScale) -> Option<Element<'a, Message>> {
    let unseen: Vec<&Finished<RetryWork>> = transfers.history.unseen_failures().collect();
    if unseen.is_empty() {
        return None;
    }
    let total: usize = unseen.iter().map(|f| f.reasons.total()).sum();
    let headline = match unseen.as_slice() {
        [only] => only.headline(),
        _ => format!("{} couldn't be finished", transfers::plural(total, "item", "items")),
    };
    let mut panel = column![row![
        status_dot(Tint::Error, scale),
        scaled_text(headline, BASE_TEXT_SIZE, scale).width(Length::Fill),
        secondary_button("Details").on_press(Message::Transfers(TransfersMessage::ShowQueue)),
        secondary_button("Dismiss").on_press(Message::DismissFailures),
    ]
    .spacing(spacing::SM)
    .align_y(iced::Alignment::Center)]
    .spacing(spacing::XS);

    // Each reason once, across every unread job — a permission failure
    // over a whole tree is the same sentence per file, and forty
    // identical lines say no more than one does.
    let mut seen: Vec<&str> = Vec::new();
    let mut distinct_more = 0usize;
    for entry in &unseen {
        let (shown, more) = entry.reasons.shown(usize::MAX);
        distinct_more += more;
        for reason in shown {
            if !seen.contains(&reason.as_str()) {
                seen.push(reason);
            }
        }
    }
    for reason in seen.iter().take(REASONS_SHOWN) {
        panel = panel.push(meta_text((*reason).to_string(), BASE_TEXT_SIZE, scale));
    }
    let more = seen.len().saturating_sub(REASONS_SHOWN) + distinct_more;
    if more > 0 {
        panel = panel.push(meta_text(format!("and {more} more"), BASE_TEXT_SIZE, scale));
    }
    Some(container(panel).width(Length::Fill).padding([spacing::XS, spacing::MD]).into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::app_for_test;
    use crate::{ArchiveUndo, JobEvent, Rate};
    use hyprforge_fileops::OpKind;
    use std::time::Instant;

    fn running(app: &mut App, id: JobId, kind: JobKind, started: Instant) {
        let (control, _events) =
            hyprforge_files::jobs::start(id, vec![], hyprforge_files_core::config::OnConflict::Ask);
        app.transfers.batch.join(id);
        app.transfers.batch.started(id);
        app.jobs.push(RunningJob {
            id,
            control,
            kind,
            started,
            dirs: vec![],
            archive: None,
            progress: None,
            rate: Rate::default(),
            conflict: None,
            apply_to_rest: false,
            subject: "\u{201C}a.txt\u{201D} to b".to_string(),
            retry: None,
            items: Default::default(),
            lands_at: Vec::new(),
        });
    }

    /// Long enough ago that `progress-after-ms` has passed.
    fn a_while_ago() -> Instant {
        Instant::now() - Duration::from_secs(5)
    }

    fn finish(app: &mut App, id: JobId, summary: JobSummary) {
        let _ = app.update(Message::Job(JobEvent::Finished { job: id, summary }));
    }

    fn step(source: &Path, dest: &Path) -> PasteStep {
        PasteStep { source: source.into(), dest: dest.into(), kind: OpKind::Copy, duplicate: false }
    }

    fn bytes(done: u64, total: u64) -> Progress {
        Progress {
            bytes_done: done,
            bytes_total: Some(total),
            entries_done: 0,
            entries_total: Some(1),
            current: PathBuf::from("/x/f"),
        }
    }

    /// A quick paste that worked is visible in the listing already; an
    /// entry for it would make the strip's control appear for something
    /// nobody saw happen.
    #[test]
    fn a_quick_job_that_worked_leaves_no_trace() {
        let mut app = app_for_test(&["/dir"]);
        running(&mut app, 1, JobKind::Copy, Instant::now());
        finish(&mut app, 1, JobSummary { done: 1, ..JobSummary::default() });
        assert!(app.transfers.history.is_empty());
        assert!(indicator(&app, FontScale::default()).is_none());
    }

    #[test]
    fn a_job_that_was_on_screen_is_remembered_once_it_finishes() {
        let mut app = app_for_test(&["/dir"]);
        running(&mut app, 1, JobKind::Copy, a_while_ago());
        finish(&mut app, 1, JobSummary { done: 1, ..JobSummary::default() });
        let entry = app.transfers.history.iter().next().expect("recorded");
        assert_eq!(entry.outcome, Outcome::Done);
        assert_eq!(entry.title(), "Copied \u{201C}a.txt\u{201D} to b");
        assert!(indicator(&app, FontScale::default()).is_some(), "the queue view stays reachable");
    }

    /// Retry queues exactly the items that landed nothing — and only
    /// once, however many times the button is pressed.
    #[test]
    fn a_retry_queues_exactly_the_failed_items_once() {
        let dir = tempfile::tempdir().unwrap();
        let failed = step(&dir.path().join("gone.txt"), &dir.path().join("to").join("gone.txt"));
        let mut app = app_for_test(&["/dir"]);
        running(&mut app, 1, JobKind::Copy, a_while_ago());
        finish(
            &mut app,
            1,
            JobSummary {
                done: 3,
                failed: vec!["gone.txt does not exist".into()],
                retry: vec![failed.clone()],
                ..JobSummary::default()
            },
        );
        let entry = app.transfers.history.iter().next().unwrap();
        assert_eq!(entry.retry.as_ref().map(|r| r.items), Some(1));

        let _ = app.update(Message::Transfers(TransfersMessage::Retry(1)));
        let _ = app.update(Message::Transfers(TransfersMessage::Retry(1)));
        let started: Vec<JobId> =
            app.jobs.iter().map(|j| j.id).chain(app.queued.iter().map(|q| q.id)).collect();
        assert_eq!(started.len(), 1, "one retry, not two");
        let retried = app.jobs.iter().find(|j| j.id == started[0]).unwrap();
        assert_eq!(retried.kind, JobKind::Copy);
        assert_eq!(retried.dirs, [dir.path().join("to")]);
    }

    /// The whole road, on a real disk: a copy that failed because its
    /// file was missing, the file put back, Retry pressed — and the copy
    /// lands, through the queue and a real worker thread.
    #[test]
    fn a_retry_after_the_cause_is_fixed_finishes_the_copy() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("report.txt");
        let dest = dir.path().join("to").join("report.txt");
        std::fs::create_dir(dir.path().join("to")).unwrap();
        let mut app = app_for_test(&["/dir"]);
        running(&mut app, 1, JobKind::Copy, a_while_ago());
        finish(
            &mut app,
            1,
            JobSummary {
                failed: vec!["report.txt does not exist".into()],
                retry: vec![step(&source, &dest)],
                ..JobSummary::default()
            },
        );
        std::fs::write(&source, "back again").unwrap();
        let _ = app.update(Message::Transfers(TransfersMessage::Retry(1)));
        let deadline = Instant::now() + Duration::from_secs(10);
        while std::fs::read_to_string(&dest).ok().as_deref() != Some("back again") {
            assert!(Instant::now() < deadline, "the retried copy never landed");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// A folder that got partway across has no retry, and says why
    /// rather than leaving a failed row with no way forward.
    #[test]
    fn a_failure_that_cannot_be_retried_says_why() {
        let mut app = app_for_test(&["/dir"]);
        running(&mut app, 1, JobKind::Copy, a_while_ago());
        finish(
            &mut app,
            1,
            JobSummary {
                done: 1,
                failed: vec!["locked.txt: permission denied".into()],
                partly: 1,
                ..JobSummary::default()
            },
        );
        let entry = app.transfers.history.iter().next().unwrap();
        assert!(entry.retry.is_none());
        assert!(entry.note.as_deref().is_some_and(|n| n.contains("Paste it again")), "{:?}", entry.note);
    }

    /// An edit writes a whole new archive and renames it into place, so
    /// a failed one changed nothing and can run again; an extraction
    /// cannot.
    #[test]
    fn a_failed_archive_rewrite_is_offered_again_and_an_extraction_is_not() {
        let edit = Work::Edit { archive: "/nowhere/notes.zip".into(), edits: vec![] };
        let extract = Work::Extract { archives: vec!["/nowhere/notes.zip".into()], into: None };
        assert!(retryable(&edit));
        assert!(retryable(&Work::Compress {
            sources: vec![],
            dest: "/nowhere/new.zip".into(),
            format: hyprforge_archive::Format::Zip,
        }));
        assert!(!retryable(&extract));

        let mut app = app_for_test(&["/dir"]);
        running(&mut app, 1, JobKind::Archive { doing: "Updating", undo: ArchiveUndo::Nothing }, a_while_ago());
        app.jobs[0].retry = Some(edit.clone());
        finish(&mut app, 1, JobSummary { failed: vec!["disk full".into()], ..JobSummary::default() });
        let entry = app.transfers.history.iter().next().unwrap();
        assert_eq!(entry.retry.as_ref().map(|r| &r.work), Some(&RetryWork::Archive(edit)));
    }

    /// A compression names a file that was not there. If one is now, a
    /// retry would write over something the first attempt never touched.
    #[test]
    fn a_retried_compression_never_writes_over_a_file_that_appeared() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("new.zip");
        std::fs::write(&dest, "someone else's").unwrap();
        let mut app = app_for_test(&["/dir"]);
        let _ = app.retry(RetryWork::Archive(Work::Compress {
            sources: vec![],
            dest: dest.clone(),
            format: hyprforge_archive::Format::Zip,
        }));
        assert!(app.jobs.is_empty() && app.queued.is_empty());
        assert!(app.status.as_deref().is_some_and(|s| s.contains("new.zip")));
        assert_eq!(std::fs::read_to_string(&dest).unwrap(), "someone else's");
    }

    /// Closing the popover hides the list, never a failure: that stays
    /// under the listing until it is dismissed.
    #[test]
    fn closing_the_popover_never_hides_a_failure() {
        let mut app = app_for_test(&["/dir"]);
        running(&mut app, 1, JobKind::Copy, a_while_ago());
        let _ = app.update(Message::Transfers(TransfersMessage::Toggle));
        assert_eq!(app.transfers.open, Panel::Popover);
        finish(&mut app, 1, JobSummary { failed: vec!["disk full".into()], ..JobSummary::default() });
        let escape = hyprforge_files_core::keymap::KeyPress {
            key: hyprforge_files_core::keymap::Key::Escape,
            mods: Default::default(),
            text: None,
        };
        let _ = app.update(Message::KeyPressed(escape));
        assert_eq!(app.transfers.open, Panel::Closed, "Escape closes it");
        assert!(failures_panel(&app.transfers, FontScale::default()).is_some());
    }

    /// Found live: Ctrl+K with the popover open put the palette under
    /// it, and the letters typed for the palette went to the search box.
    /// A key that is not Escape closes the popover and still does its
    /// own thing; the queue view, a card, keeps the keyboard.
    #[test]
    fn a_key_closes_the_popover_and_still_acts_but_the_queue_view_keeps_it() {
        let ctrl_k = hyprforge_files_core::keymap::KeyPress {
            key: hyprforge_files_core::keymap::Key::Char('k'),
            mods: hyprforge_files_core::keymap::Modifiers { ctrl: true, ..Default::default() },
            text: None,
        };
        let mut app = app_for_test(&["/dir"]);
        let _ = app.update(Message::Transfers(TransfersMessage::Toggle));
        let _ = app.update(Message::KeyPressed(ctrl_k));
        assert!(!app.transfers.is_open());
        assert!(app.tabs[0].browser.menu_overlay(FontScale::default(), (900.0, 600.0)).is_some(), "the palette opened");

        let mut app = app_for_test(&["/dir"]);
        let _ = app.update(Message::Transfers(TransfersMessage::ShowQueue));
        let _ = app.update(Message::KeyPressed(ctrl_k));
        assert_eq!(app.transfers.open, Panel::Queue);
        assert!(app.tabs[0].browser.menu_overlay(FontScale::default(), (900.0, 600.0)).is_none());
    }

    #[test]
    fn the_strip_control_toggles_the_popover() {
        let mut app = app_for_test(&["/dir"]);
        let _ = app.update(Message::Transfers(TransfersMessage::Toggle));
        assert!(app.transfers.is_open());
        let _ = app.update(Message::Transfers(TransfersMessage::Toggle));
        assert!(!app.transfers.is_open());
    }

    /// The keys open what the clicks open, and the same key closes it
    /// again — through both panels' own handling of a key, which would
    /// otherwise reopen the popover (it lets a key act after closing)
    /// or ignore the key (the queue view keeps the keyboard).
    #[test]
    fn each_transfers_key_opens_its_panel_and_the_same_key_closes_it() {
        let press = |key: char| hyprforge_files_core::keymap::KeyPress {
            key: hyprforge_files_core::keymap::Key::Char(key),
            mods: hyprforge_files_core::keymap::Modifiers { ctrl: true, shift: true, ..Default::default() },
            text: None,
        };
        let mut app = app_for_test(&["/dir"]);
        let _ = app.update(Message::KeyPressed(press('y')));
        assert_eq!(app.transfers.open, Panel::Popover);
        let _ = app.update(Message::KeyPressed(press('y')));
        assert_eq!(app.transfers.open, Panel::Closed, "the popover's key closes it and does not reopen it");

        let _ = app.update(Message::KeyPressed(press('j')));
        assert_eq!(app.transfers.open, Panel::Queue);
        let _ = app.update(Message::KeyPressed(press('j')));
        assert_eq!(app.transfers.open, Panel::Closed, "the queue view lets its own key out");

        // From the popover, the queue key goes on to the view rather
        // than only closing the popover.
        let _ = app.update(Message::KeyPressed(press('y')));
        let _ = app.update(Message::KeyPressed(press('j')));
        assert_eq!(app.transfers.open, Panel::Queue);
    }

    /// The overall bar, driven through the window's own loop: one job
    /// finishing does not drop it, and the batch starts again at zero
    /// once everything is done.
    #[test]
    fn the_overall_bar_holds_as_jobs_finish_and_resets_when_idle() {
        let mut app = app_for_test(&["/dir"]);
        running(&mut app, 1, JobKind::Copy, a_while_ago());
        running(&mut app, 2, JobKind::Copy, a_while_ago());
        let _ = app.update(Message::Job(JobEvent::Progress { job: 1, progress: bytes(90, 100), items: Default::default() }));
        let _ = app.update(Message::Job(JobEvent::Progress { job: 2, progress: bytes(10, 100), items: Default::default() }));
        let before = app.transfers.batch.fraction().unwrap();
        finish(&mut app, 1, JobSummary { done: 1, ..JobSummary::default() });
        assert!(app.transfers.batch.fraction().unwrap() >= before);
        finish(&mut app, 2, JobSummary { done: 1, ..JobSummary::default() });
        assert_eq!(app.transfers.batch.fraction(), None, "idle: the next batch starts fresh");
    }

    /// A job waiting on an archive another job is rewriting says which,
    /// rather than the "busy" a slot shortage would give.
    #[test]
    fn a_queued_edit_says_which_archive_it_is_waiting_for() {
        let mut app = app_for_test(&["/dir"]);
        let archive = PathBuf::from("/home/a/notes.zip");
        running(&mut app, 1, JobKind::Archive { doing: "Updating", undo: ArchiveUndo::Nothing }, a_while_ago());
        app.jobs[0].archive = Some(archive.clone());
        let waiting = Queued {
            id: 2,
            kind: JobKind::Archive { doing: "Updating", undo: ArchiveUndo::Nothing },
            dirs: vec![],
            what: QueuedWork::Archive {
                work: Work::Edit { archive: archive.clone(), edits: vec![] },
                unlock: hyprforge_archive::Unlock::none(),
            },
        };
        assert_eq!(why_waiting(&app, &waiting), Waiting::Behind("notes.zip".into()));
        let unrelated =
            Queued { id: 3, kind: JobKind::Copy, dirs: vec![], what: QueuedWork::Paste { steps: vec![] } };
        assert_eq!(why_waiting(&app, &unrelated), Waiting::Busy);
    }

    /// Found live: pasting one folder twice ran both duplicates at once,
    /// both chose `big.2`, and the second failed with "File exists". A
    /// second paste to the same path waits for the first, and says so;
    /// a paste of something else into the same folder does not wait.
    #[test]
    fn a_second_paste_to_the_same_path_waits_for_the_first() {
        let mut app = app_for_test(&["/dir"]);
        running(&mut app, 1, JobKind::Copy, a_while_ago());
        app.jobs[0].lands_at = vec![PathBuf::from("/dir/big")];
        let paste = |id, name: &str| Queued {
            id,
            kind: JobKind::Copy,
            dirs: vec![],
            what: QueuedWork::Paste {
                steps: vec![PasteStep {
                    source: PathBuf::from("/dir").join(name),
                    dest: PathBuf::from("/dir").join(name),
                    kind: OpKind::Copy,
                    duplicate: true,
                }],
            },
        };
        assert!(!app.may_start(&paste(2, "big")));
        assert_eq!(why_waiting(&app, &paste(2, "big")), Waiting::SameName("big".into()));
        assert!(app.may_start(&paste(3, "small")), "a different name into the same folder goes now");
    }

    /// Cancelling work before it starts takes it out of the bar: it was
    /// never going to be done, so counting it would hold the bar short.
    #[test]
    fn cancelling_queued_work_takes_it_out_of_the_overall_bar() {
        let mut app = app_for_test(&["/dir"]);
        running(&mut app, 1, JobKind::Copy, a_while_ago());
        running(&mut app, 2, JobKind::Copy, a_while_ago());
        app.transfers.batch.join(3);
        app.queued.push_back(Queued {
            id: 3,
            kind: JobKind::Copy,
            dirs: vec![],
            what: QueuedWork::Paste { steps: vec![] },
        });
        let _ = app.update(Message::Job(JobEvent::Progress { job: 1, progress: bytes(50, 100), items: Default::default() }));
        let _ = app.update(Message::Job(JobEvent::Progress { job: 2, progress: bytes(50, 100), items: Default::default() }));
        let with_queued = app.transfers.batch.fraction().unwrap();
        let _ = app.update(Message::CancelJob(3));
        assert!(app.transfers.batch.fraction().unwrap() > with_queued);
    }

    /// "Show" goes to the folder something landed in.
    #[test]
    fn show_goes_to_where_it_landed() {
        let mut app = app_for_test(&["/dir"]);
        let _ = app.update(Message::Transfers(TransfersMessage::ShowQueue));
        let _ = app.update(Message::Transfers(TransfersMessage::Show(PathBuf::from("/dir/sub/copy.txt"))));
        assert!(!app.transfers.is_open());
        assert_eq!(app.tabs[0].browser.current_dir(), Path::new("/dir/sub"));
    }
}
