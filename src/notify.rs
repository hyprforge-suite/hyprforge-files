//! What the window has to say, as desktop notifications.
//!
//! A line at the bottom of the window — "Moved 3 items", "Couldn't
//! copy", the undo notice — is easy to miss and gone the moment the
//! window is. The desktop already has a place for news: the
//! notification daemon (this suite's own notifd, or dunst, or mako —
//! anything that answers `org.freedesktop.Notifications`). So when one
//! is running, everything the window would have put on that line goes
//! there instead, and the undo notice comes with its Undo and History
//! buttons as notification actions.
//!
//! Two rules keep it from being worse than the line it replaces:
//!
//! - **One at a time.** Every message replaces the last
//!   (`replaces_id`), as each status line replaced the one before it, so
//!   a burst of them is one notification changing rather than a stack.
//! - **No daemon, no change.** Every component runs alone: with nothing
//!   answering the bus, or a call that fails, the line at the bottom of
//!   the window is used exactly as before. Not knowing yet counts as no
//!   daemon — a message never waits on the bus to be shown somewhere.
//!
//! The bus is talked to on the window's subscription, never from
//! `update`: every call is bounded, and a daemon that stops answering
//! costs a notification, not a frozen window.

use iced::futures::channel::mpsc::{unbounded, UnboundedReceiver, UnboundedSender};
use iced::futures::{SinkExt, Stream, StreamExt};
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

/// How long any one call to the bus may take.
const BUS_WAIT: Duration = Duration::from_secs(2);

/// What the window asks to be shown.
#[derive(Debug, Clone, PartialEq)]
pub enum Note {
    /// A status line.
    Status(String),
    /// Something Undo can take back, for `seconds`, with its buttons.
    Undo { text: String, seconds: u64 },
}

/// What comes back from the bus.
#[derive(Debug, Clone, PartialEq)]
pub enum NoteEvent {
    /// Whether a notification daemon is there to show things. Sent once
    /// at start, and again whenever that changes.
    Available(bool),
    /// A note could not be shown — the window puts it on its own line.
    Failed(Note),
    /// The Undo button on the undo notification was pressed.
    Undo,
    /// Its History button.
    History,
}

/// Requests to the worker. One window per process, so one channel, made
/// on first use — the same arrangement as `dnd`'s events.
static REQUESTS: OnceLock<Requests> = OnceLock::new();

/// The sending half, for [`show`], and the receiving half, kept until
/// the worker takes it.
type Requests = (UnboundedSender<Note>, Mutex<Option<UnboundedReceiver<Note>>>);

fn requests() -> &'static Requests {
    REQUESTS.get_or_init(|| {
        let (tx, rx) = unbounded();
        (tx, Mutex::new(Some(rx)))
    })
}

/// Shows `note`. Returns at once; a failure comes back as
/// [`NoteEvent::Failed`].
pub fn show(note: Note) {
    let _ = requests().0.unbounded_send(note);
}

#[zbus::proxy(
    interface = "org.freedesktop.Notifications",
    default_service = "org.freedesktop.Notifications",
    default_path = "/org/freedesktop/Notifications"
)]
trait Notifications {
    #[allow(clippy::too_many_arguments)]
    fn notify(
        &self,
        app_name: &str,
        replaces_id: u32,
        app_icon: &str,
        summary: &str,
        body: &str,
        actions: &[&str],
        hints: HashMap<&str, zbus::zvariant::Value<'_>>,
        expire_timeout: i32,
    ) -> zbus::Result<u32>;

    #[zbus(signal)]
    fn action_invoked(&self, id: u32, action_key: String) -> zbus::Result<()>;
}

/// The action keys on an undo notification.
const UNDO: &str = "undo";
const HISTORY: &str = "history";

/// The arguments one note is sent with: summary, body, actions (key,
/// label pairs, flattened as the spec has them) and how long it stays,
/// in milliseconds (`-1` for the daemon's own default).
pub fn arguments(note: &Note) -> (String, Vec<&'static str>, i32) {
    match note {
        Note::Status(text) => (text.clone(), Vec::new(), -1),
        Note::Undo { text, seconds } => {
            (text.clone(), vec![UNDO, "Undo", HISTORY, "History"], (*seconds).saturating_mul(1000).min(i32::MAX as u64) as i32)
        }
    }
}

/// The worker, as a subscription's stream: connects, says whether a
/// daemon is there, then shows what it is asked and passes back what
/// is pressed. The first caller gets it; there is only ever one.
pub fn events() -> impl Stream<Item = NoteEvent> {
    let taken = requests().1.lock().unwrap_or_else(|p| p.into_inner()).take();
    iced::stream::channel(16, move |mut out: iced::futures::channel::mpsc::Sender<NoteEvent>| async move {
        let Some(mut asked) = taken else { return };
        let Some(proxy) = connect().await else {
            let _ = out.send(NoteEvent::Available(false)).await;
            // Nothing to show anything on: every note goes back.
            while let Some(note) = asked.next().await {
                let _ = out.send(NoteEvent::Failed(note)).await;
            }
            return;
        };
        let _ = out.send(NoteEvent::Available(true)).await;
        let mut pressed = match tokio::time::timeout(BUS_WAIT, proxy.receive_action_invoked()).await {
            Ok(Ok(stream)) => Some(stream),
            _ => None,
        };
        // The one notification on screen, replaced by each new one; and
        // the undo notification's id, which is the only one with buttons.
        let mut shown: u32 = 0;
        let mut undo_id: Option<u32> = None;
        loop {
            let press = async {
                match pressed.as_mut() {
                    Some(stream) => stream.next().await,
                    None => std::future::pending().await,
                }
            };
            tokio::select! {
                note = asked.next() => {
                    let Some(note) = note else { return };
                    let (summary, actions, expire) = arguments(&note);
                    let mut hints = HashMap::new();
                    // Which application this is, so a daemon can show its
                    // icon and group it — the launcher entry's name.
                    hints.insert("desktop-entry", zbus::zvariant::Value::from("hyprforge-files"));
                    let call = proxy.notify("Files", shown, "system-file-manager", &summary, "", &actions, hints, expire);
                    match tokio::time::timeout(BUS_WAIT, call).await {
                        Ok(Ok(id)) => {
                            shown = id;
                            undo_id = matches!(note, Note::Undo { .. }).then_some(id);
                        }
                        _ => {
                            let _ = out.send(NoteEvent::Failed(note)).await;
                        }
                    }
                }
                Some(signal) = press => {
                    let Ok(args) = signal.args() else { continue };
                    if Some(args.id) != undo_id {
                        continue;
                    }
                    let event = match args.action_key.as_str() {
                        UNDO => NoteEvent::Undo,
                        HISTORY => NoteEvent::History,
                        _ => continue,
                    };
                    let _ = out.send(event).await;
                }
            }
        }
    })
}

/// The session bus, and a proxy on a daemon that is actually there —
/// either running, or one the bus would start when asked.
async fn connect() -> Option<NotificationsProxy<'static>> {
    let bus = tokio::time::timeout(BUS_WAIT, zbus::Connection::session()).await.ok()?.ok()?;
    let dbus = tokio::time::timeout(BUS_WAIT, zbus::fdo::DBusProxy::new(&bus)).await.ok()?.ok()?;
    let name = zbus::names::BusName::try_from("org.freedesktop.Notifications").ok()?;
    let running = tokio::time::timeout(BUS_WAIT, dbus.name_has_owner(name.clone())).await.ok()?.ok()?;
    if !running {
        let activatable = tokio::time::timeout(BUS_WAIT, dbus.list_activatable_names()).await.ok()?.ok()?;
        if !activatable.iter().any(|n| n.as_str() == "org.freedesktop.Notifications") {
            return None;
        }
    }
    tokio::time::timeout(BUS_WAIT, NotificationsProxy::new(&bus)).await.ok()?.ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_status_line_has_no_buttons_and_the_daemons_own_timeout() {
        let (summary, actions, expire) = arguments(&Note::Status("Moved 3 items".into()));
        assert_eq!(summary, "Moved 3 items");
        assert!(actions.is_empty());
        assert_eq!(expire, -1);
    }

    #[test]
    fn the_undo_notice_brings_its_buttons_and_lasts_as_long_as_undo_offers() {
        let (_, actions, expire) = arguments(&Note::Undo { text: "Trashed 2 items".into(), seconds: 6 });
        assert_eq!(actions, [UNDO, "Undo", HISTORY, "History"], "key, label pairs, as the spec flattens them");
        assert_eq!(expire, 6000);
    }
}
