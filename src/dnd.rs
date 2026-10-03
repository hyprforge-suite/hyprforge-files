//! Drag and drop: dragging files out of the window into another
//! application, and dropping files onto it — from another application,
//! or from this window's own folders.
//!
//! winit, under iced, has no drag source on Wayland at all and reports
//! no drops either, so this is the `wl_data_device` conversation done by
//! hand. Three facts about the protocol decide the dragging half's shape,
//! and each one is a way it silently does nothing if got wrong:
//!
//! - `start_drag` names the surface the drag came from, and that has to
//!   be *the window's own* `wl_surface` — an object on winit's
//!   connection. A second connection of our own (the way
//!   `hyprforge_clipboard` talks data-control) could not name it. So this
//!   joins winit's connection instead, through the `wl_display` pointer
//!   the window hands out, with an event queue of its own: the same thing
//!   iced's own clipboard (smithay-clipboard) already does to that
//!   connection.
//! - It also takes the serial of the button press that is still being
//!   held. winit sees that press and does not share it, so the worker
//!   binds its own `wl_pointer` — a compositor sends a client's button
//!   events to every pointer that client created — and remembers the
//!   serial while the button is down. Which is why [`Dnd::attach`]
//!   runs when the window opens, not when the first drag starts: a
//!   pointer created mid-press never heard the press.
//! - The files are not handed over when the drag starts. They are asked
//!   for, by type, when something accepts the drop, and written to a
//!   pipe the receiver holds — so the worker thread has to be dispatching
//!   for as long as a drag can last, and a drop that is never answered
//!   is a receiver that hangs.
//!
//! Only `copy` is offered. A receiver asked to *move* is entitled to
//! expect the source to delete what it dropped, and deleting someone's
//! files because another program accepted a drop is not a decision this
//! app gets to make on its behalf.
//!
//! Dropping is the same device, listening. Two facts shape that half:
//!
//! - A drag over the window reports positions to the data device and
//!   none to the pointer, so iced never sees the pointer move and no
//!   widget can say what is under it. What this sends the window is a
//!   point, and the window asks its own layout which folder holds it —
//!   see `hyprforge_files_core::drop`.
//! - Another application's files arrive as a `text/uri-list` written
//!   into a pipe, by a program that may never finish writing. The read
//!   is bounded, on a thread of its own, and only a drop that was read
//!   is acted on. A drag that started *here* is never read back at all:
//!   this side already knows the paths, and reading them through the
//!   compositor would only be slower.
//!
//! Only `copy` is accepted from outside, for the reason only `copy` is
//! offered: a move would delete another program's files on its say-so.
//! A drag between this window's own folders may still move — that is
//! decided by `drop::plan` and carried out by this app's own paste, not
//! negotiated with the compositor.
//!
//! Every component runs alone, and this one degrades the same way the
//! clipboard does: on X11, or a compositor with no data device, a drag
//! is a status-bar line and nothing else is lost.

use hyprforge_files_core::clipboard::{clip_from, URI_LIST};
use hyprforge_files_core::drop::DropFrom;
use iced::futures::channel::mpsc::{unbounded, UnboundedReceiver, UnboundedSender};
use iced::window::raw_window_handle::{RawDisplayHandle, RawWindowHandle};
use std::io::{Read, Write};
use std::os::fd::AsFd;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use wayland_client::backend::{Backend, ObjectId};
use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::wl_data_device::{self, WlDataDevice};
use wayland_client::protocol::wl_data_device_manager::{DndAction, WlDataDeviceManager};
use wayland_client::protocol::wl_data_offer::{self, WlDataOffer};
use wayland_client::protocol::wl_data_source::{self, WlDataSource};
use wayland_client::protocol::wl_pointer::{self, WlPointer};
use wayland_client::protocol::wl_registry::WlRegistry;
use wayland_client::protocol::wl_seat::{self, WlSeat};
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_client::{event_created_child, Connection, Dispatch, Proxy, QueueHandle, WEnum};

/// `BTN_LEFT` from `linux/input-event-codes.h` — the only button a drag
/// starts from.
const BTN_LEFT: u32 = 0x110;

/// How long another application gets to hand over a dropped list of
/// files. A list of paths is a few kilobytes; a source that has not
/// written it in this long is not going to, and the drop is reported as
/// failed rather than left hanging.
const DROP_READ_TIMEOUT: Duration = Duration::from_secs(5);

/// What a drag over the window tells it.
#[derive(Debug, Clone, PartialEq)]
pub enum DropEvent {
    /// Something that could be dropped is over the window, here — in
    /// the window's own logical coordinates.
    Over { x: f64, y: f64 },
    /// It left, or the drag ended somewhere else.
    Left,
    /// It was let go, here.
    Dropped { x: f64, y: f64, from: DropFrom },
    /// It was let go, and the files could not be read from whoever
    /// offered them. A sentence for the status bar.
    Failed(String),
}

/// The window's drop events. One window per process — closing its last
/// tab exits — so one channel, made on first use; the same reasoning as
/// the pointer tracker in `main.rs`.
static EVENTS: OnceLock<Channel> = OnceLock::new();

/// The sending half, kept for the worker, and the receiving half, kept
/// until the window's subscription takes it.
type Channel = (UnboundedSender<DropEvent>, Mutex<Option<UnboundedReceiver<DropEvent>>>);

fn events_channel() -> &'static Channel {
    EVENTS.get_or_init(|| {
        let (tx, rx) = unbounded();
        (tx, Mutex::new(Some(rx)))
    })
}

/// The stream of drop events, for a subscription. The first caller gets
/// it; a second (there is none) would get a stream that never yields.
pub fn events() -> impl iced::futures::Stream<Item = DropEvent> {
    let taken = events_channel().1.lock().unwrap_or_else(|p| p.into_inner()).take();
    let (_, empty) = unbounded();
    taken.unwrap_or(empty)
}

/// The window's drag source. Cheap to hold before it is attached, and
/// inert forever if attaching fails.
#[derive(Default)]
pub struct Dnd {
    /// Filled in once, by the worker thread, when the data device is
    /// ready. Empty means "not yet" or "never" — both of which make a
    /// drag a status line rather than a crash.
    ready: Arc<OnceLock<Ready>>,
    /// Whether [`Self::attach`] has run, so a second call is a no-op.
    attached: OnceLock<()>,
    /// The serial of the left press still being held, if one is.
    press: Arc<Mutex<Option<u32>>>,
    /// The paths being dragged out right now, if a drag started here is
    /// under way — which is how a drop onto this window knows it came
    /// from this window.
    outgoing: Arc<Mutex<Option<Vec<PathBuf>>>>,
}

/// What a drag needs from the worker: the connection it joined and the
/// objects it bound on it.
struct Ready {
    connection: Connection,
    queue: QueueHandle<State>,
    manager: WlDataDeviceManager,
    device: WlDataDevice,
}

impl Dnd {
    pub fn new() -> Self {
        Self::default()
    }

    /// Joins the window's Wayland connection and starts the worker that
    /// listens on it. Call once the window exists, before any drag.
    ///
    /// Returns at once: binding the globals needs a round trip, and a
    /// round trip has no timeout, so it happens on the worker's thread
    /// where a compositor that stops answering stalls only this feature
    /// and not the window.
    pub fn attach(&self, window: &dyn iced::window::Window) {
        if self.attached.set(()).is_err() {
            return;
        }
        let display = match window.display_handle().map(|h| h.as_raw()) {
            Ok(RawDisplayHandle::Wayland(h)) => h.display.as_ptr(),
            Ok(_) => {
                tracing::info!("not a Wayland window; dragging files out is unavailable");
                return;
            }
            Err(e) => {
                tracing::warn!(error = %e, "no display handle; dragging files out is unavailable");
                return;
            }
        };
        // SAFETY: the pointer is winit's `wl_display`, which lives as long
        // as its event loop — the whole life of this process, since this
        // app has one window and exits when it closes. The foreign
        // backend never closes a display it did not open.
        let backend = unsafe { Backend::from_foreign_display(display.cast()) };
        let connection = Connection::from_backend(backend);
        let (ready, press, outgoing) = (self.ready.clone(), self.press.clone(), self.outgoing.clone());
        let spawned = std::thread::Builder::new()
            .name("files-dnd".into())
            .spawn(move || run_worker(connection, ready, press, outgoing));
        if let Err(e) = spawned {
            tracing::warn!(error = %e, "the drag worker could not start");
        }
    }

    /// Starts dragging `paths`, offered as `offers` — `(type, content)`
    /// pairs, most specific first — out of `window`. Must be called while
    /// the left button that began the drag is still held.
    pub fn start(
        &self,
        window: &dyn iced::window::Window,
        paths: Vec<PathBuf>,
        offers: Vec<(&'static str, String)>,
    ) -> Result<(), String> {
        let Some(ready) = self.ready.get() else {
            return Err("Dragging out needs a Wayland compositor with drag-and-drop.".to_string());
        };
        let Some(serial) = *self.press.lock().unwrap_or_else(|p| p.into_inner()) else {
            // The button came up before the drag could start — a flick
            // too quick to be a drag after all. Nothing to say.
            return Ok(());
        };
        let surface = match window.window_handle().map(|h| h.as_raw()) {
            Ok(RawWindowHandle::Wayland(h)) => h.surface.as_ptr(),
            _ => return Err("This window has no Wayland surface to drag from.".to_string()),
        };
        // SAFETY: winit's `wl_surface` for this window, which is alive —
        // `window::run` only calls back while the window exists — and is
        // only named here, never destroyed or given a queue of ours.
        let surface = unsafe { ObjectId::from_ptr(WlSurface::interface(), surface.cast()) }
            .ok()
            .and_then(|id| WlSurface::from_id(&ready.connection, id).ok())
            .ok_or_else(|| "The window's surface could not be used for a drag.".to_string())?;

        let offer: Arc<Vec<(String, Vec<u8>)>> =
            Arc::new(offers.into_iter().map(|(mime, text)| (mime.to_string(), text.into_bytes())).collect());
        let source = ready.manager.create_data_source(&ready.queue, Offer(offer.clone()));
        *self.outgoing.lock().unwrap_or_else(|p| p.into_inner()) = Some(paths);
        for (mime, _) in offer.iter() {
            source.offer(mime.clone());
        }
        // Drag actions arrived in version 3; before that a drop is a copy
        // by definition, which is exactly what is on offer anyway.
        if source.version() >= 3 {
            source.set_actions(DndAction::Copy);
        }
        // No icon surface: the compositor draws its own drag cursor, and
        // a picture of the files is a nicety a drag works without.
        ready.device.start_drag(Some(&source), &surface, None, serial);
        ready
            .connection
            .flush()
            .map_err(|e| format!("The drag could not be sent to the compositor: {e}"))
    }
}

/// The worker: bind what a drag needs, publish it, then dispatch this
/// queue's events for the rest of the process's life.
fn run_worker(
    connection: Connection,
    ready: Arc<OnceLock<Ready>>,
    press: Arc<Mutex<Option<u32>>>,
    outgoing: Arc<Mutex<Option<Vec<PathBuf>>>>,
) {
    let (globals, mut queue) = match registry_queue_init::<State>(&connection) {
        Ok(found) => found,
        Err(e) => {
            tracing::warn!(error = %e, "the compositor's globals could not be listed; dragging out is unavailable");
            return;
        }
    };
    let qh = queue.handle();
    // Version 3 for drag actions; the capabilities event that tells the
    // worker a pointer exists has been there since version 1.
    let (Ok(seat), Ok(manager)) = (
        globals.bind::<WlSeat, _, _>(&qh, 1..=7, ()),
        globals.bind::<WlDataDeviceManager, _, _>(&qh, 1..=3, ()),
    ) else {
        tracing::info!("no seat or data device manager; dragging files out is unavailable");
        return;
    };
    let device = manager.get_data_device(&seat, &qh, ());
    let mut state = State {
        press,
        pointer: None,
        outgoing,
        entered: None,
        at: (0.0, 0.0),
        events: events_channel().0.clone(),
        connection: connection.clone(),
    };
    // The capabilities event, and with it this worker's own pointer,
    // arrive on the first dispatch — before any drag could be asked for.
    let _ = ready.set(Ready { connection, queue: qh, manager, device });
    loop {
        if let Err(e) = queue.blocking_dispatch(&mut state) {
            tracing::warn!(error = %e, "the drag worker lost its connection");
            return;
        }
    }
}

/// What a data source carries: every type it offered and the bytes for
/// each, answered from here whenever a receiver asks.
struct Offer(Arc<Vec<(String, Vec<u8>)>>);

struct State {
    press: Arc<Mutex<Option<u32>>>,
    pointer: Option<WlPointer>,
    /// See [`Dnd::outgoing`].
    outgoing: Arc<Mutex<Option<Vec<PathBuf>>>>,
    /// The offer being dragged over the window, if it carries files.
    entered: Option<WlDataOffer>,
    /// Where it was last reported — a drop event carries no position.
    at: (f64, f64),
    events: UnboundedSender<DropEvent>,
    connection: Connection,
}

/// The types an offer said it has, collected as they are announced —
/// which is before the `enter` that says the offer is over the window.
#[derive(Default)]
struct OfferTypes(Mutex<Vec<String>>);

impl Dispatch<WlRegistry, GlobalListContents> for State {
    fn event(_: &mut Self, _: &WlRegistry, _: <WlRegistry as Proxy>::Event, _: &GlobalListContents, _: &Connection, _: &QueueHandle<Self>) {}
}

impl Dispatch<WlSeat, ()> for State {
    fn event(state: &mut Self, seat: &WlSeat, event: wl_seat::Event, _: &(), _: &Connection, qh: &QueueHandle<Self>) {
        if let wl_seat::Event::Capabilities { capabilities: WEnum::Value(caps) } = event {
            let has_pointer = caps.contains(wl_seat::Capability::Pointer);
            match (&state.pointer, has_pointer) {
                (None, true) => state.pointer = Some(seat.get_pointer(qh, ())),
                (Some(_), false) => {
                    if let Some(p) = state.pointer.take() {
                        if p.version() >= 3 {
                            p.release();
                        }
                    }
                }
                _ => {}
            }
        }
    }
}

impl Dispatch<WlPointer, ()> for State {
    fn event(state: &mut Self, _: &WlPointer, event: wl_pointer::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let wl_pointer::Event::Button { serial, button: BTN_LEFT, state: WEnum::Value(pressed), .. } = event {
            let mut press = state.press.lock().unwrap_or_else(|p| p.into_inner());
            *press = (pressed == wl_pointer::ButtonState::Pressed).then_some(serial);
        }
    }
}

impl Dispatch<WlDataDeviceManager, ()> for State {
    fn event(_: &mut Self, _: &WlDataDeviceManager, _: <WlDataDeviceManager as Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}

impl Dispatch<WlDataDevice, ()> for State {
    fn event(state: &mut Self, _: &WlDataDevice, event: wl_data_device::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        match event {
            // Told about every clipboard change on the desktop, and takes
            // none of them — the clipboard is read elsewhere. Destroyed as
            // soon as named, so ignoring them leaks nothing.
            wl_data_device::Event::Selection { id: Some(offer) } => offer.destroy(),
            wl_data_device::Event::Enter { serial, x, y, id, .. } => {
                if let Some(old) = state.entered.take() {
                    old.destroy();
                }
                let Some(offer) = id else {
                    tracing::debug!("a drag entered with no offer");
                    return;
                };
                let types: Vec<String> =
                    offer.data::<OfferTypes>().map(|t| t.0.lock().unwrap_or_else(|p| p.into_inner()).clone()).unwrap_or_default();
                tracing::debug!(?types, x, y, "a drag entered the window");
                let has_files = offer
                    .data::<OfferTypes>()
                    .is_some_and(|t| t.0.lock().unwrap_or_else(|p| p.into_inner()).iter().any(|m| m == URI_LIST));
                if !has_files {
                    // Text, a picture from a browser: nothing a folder can
                    // hold. Saying so is what makes the compositor show a
                    // "no" cursor rather than a promise.
                    offer.accept(serial, None);
                    offer.destroy();
                    return;
                }
                offer.accept(serial, Some(URI_LIST.to_string()));
                if offer.version() >= 3 {
                    offer.set_actions(DndAction::Copy, DndAction::Copy);
                }
                state.entered = Some(offer);
                state.at = (x, y);
                let _ = state.events.unbounded_send(DropEvent::Over { x, y });
            }
            wl_data_device::Event::Motion { x, y, .. } if state.entered.is_some() => {
                state.at = (x, y);
                let _ = state.events.unbounded_send(DropEvent::Over { x, y });
            }
            wl_data_device::Event::Leave => {
                if let Some(offer) = state.entered.take() {
                    offer.destroy();
                    let _ = state.events.unbounded_send(DropEvent::Left);
                }
            }
            wl_data_device::Event::Drop => {
                let Some(offer) = state.entered.take() else {
                    tracing::debug!("a drop with nothing accepted");
                    return;
                };
                let (x, y) = state.at;
                tracing::debug!(x, y, "a drop on the window");
                let mine = state.outgoing.lock().unwrap_or_else(|p| p.into_inner()).clone();
                if let Some(paths) = mine {
                    // Dropped back onto this window: the paths are already
                    // known, so nothing is read through the compositor.
                    finish(&offer);
                    let _ = state.events.unbounded_send(DropEvent::Dropped { x, y, from: DropFrom::Inside(paths) });
                } else {
                    receive_drop(offer, (x, y), state.events.clone(), state.connection.clone());
                }
                let _ = state.connection.flush();
            }
            _ => {}
        }
    }

    event_created_child!(State, WlDataDevice, [
        wl_data_device::EVT_DATA_OFFER_OPCODE => (WlDataOffer, OfferTypes::default()),
    ]);
}

impl Dispatch<WlDataOffer, OfferTypes> for State {
    fn event(_: &mut Self, _: &WlDataOffer, event: <WlDataOffer as Proxy>::Event, types: &OfferTypes, _: &Connection, _: &QueueHandle<Self>) {
        if let wl_data_offer::Event::Offer { mime_type } = event {
            types.0.lock().unwrap_or_else(|p| p.into_inner()).push(mime_type);
        }
    }
}

/// Tells the source the drop is done with, and lets the offer go.
fn finish(offer: &WlDataOffer) {
    // `finish` arrived with drag actions, in version 3; before that the
    // drop simply ends when the offer goes.
    if offer.version() >= 3 {
        offer.finish();
    }
    offer.destroy();
}

/// Reads another application's dropped files, off the worker's thread
/// and bounded — see the module doc.
fn receive_drop(offer: WlDataOffer, (x, y): (f64, f64), events: UnboundedSender<DropEvent>, connection: Connection) {
    let (mut reader, writer) = match std::io::pipe() {
        Ok(pair) => pair,
        Err(e) => {
            finish(&offer);
            let _ = events.unbounded_send(DropEvent::Failed(format!("The dropped files could not be received: {e}")));
            return;
        }
    };
    offer.receive(URI_LIST.to_string(), writer.as_fd());
    // Ours closed, so the read ends when the *source* closes its end.
    drop(writer);
    let _ = connection.flush();
    let spawned = std::thread::Builder::new().name("files-drop-read".into()).spawn(move || {
        let (tx, rx) = std::sync::mpsc::channel();
        // The read itself on a thread that may never come back: a source
        // that never writes holds it forever, and only this waiter, not
        // the drop, has to wait for that.
        let _ = std::thread::Builder::new().name("files-drop-pipe".into()).spawn(move || {
            let mut bytes = Vec::new();
            let _ = tx.send(reader.read_to_end(&mut bytes).map(|_| bytes));
        });
        let event = match rx.recv_timeout(DROP_READ_TIMEOUT) {
            Ok(Ok(bytes)) => match clip_from(URI_LIST, &bytes) {
                Some(clip) => DropEvent::Dropped { x, y, from: DropFrom::Outside(clip.paths) },
                None => DropEvent::Failed("Only files on this computer can be dropped here.".to_string()),
            },
            Ok(Err(e)) => DropEvent::Failed(format!("The dropped files could not be read: {e}")),
            Err(_) => DropEvent::Failed("The application the files came from never handed them over.".to_string()),
        };
        finish(&offer);
        let _ = connection.flush();
        let _ = events.unbounded_send(event);
    });
    if let Err(e) = spawned {
        tracing::warn!(error = %e, "the drop reader could not start");
    }
}

impl Dispatch<WlDataSource, Offer> for State {
    fn event(state: &mut Self, source: &WlDataSource, event: wl_data_source::Event, offer: &Offer, _: &Connection, _: &QueueHandle<Self>) {
        match event {
            wl_data_source::Event::Send { mime_type, fd } => {
                let Some(bytes) = offer.0.iter().find(|(m, _)| *m == mime_type).map(|(_, b)| b.clone()) else {
                    return; // dropping `fd` closes it: an empty answer
                };
                // On a thread of its own: a pipe holds 64KiB, a long
                // selection's URI list can be more, and a receiver that
                // is slow to read must not stop this worker answering
                // everyone else. The write ends, and the pipe closes,
                // when the receiver has it all or goes away.
                let _ = std::thread::Builder::new().name("files-drag-send".into()).spawn(move || {
                    if let Err(e) = std::fs::File::from(fd).write_all(&bytes) {
                        tracing::info!(error = %e, "a drop's receiver stopped reading");
                    }
                });
            }
            // Over either way: dropped nowhere, or dropped and taken.
            wl_data_source::Event::Cancelled | wl_data_source::Event::DndFinished => {
                *state.outgoing.lock().unwrap_or_else(|p| p.into_inner()) = None;
                source.destroy();
            }
            _ => {}
        }
    }
}
