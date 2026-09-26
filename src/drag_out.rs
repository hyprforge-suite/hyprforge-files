//! Dragging files out of the window, into another application.
//!
//! winit, under iced, has no drag *source* on Wayland at all, so this is
//! the `wl_data_device` conversation done by hand. Three facts about the
//! protocol decide its shape, and each one is a way it silently does
//! nothing if got wrong:
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
//!   serial while the button is down. Which is why [`DragOut::attach`]
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
//! Every component runs alone, and this one degrades the same way the
//! clipboard does: on X11, or a compositor with no data device, a drag
//! is a status-bar line and nothing else is lost.

use iced::window::raw_window_handle::{RawDisplayHandle, RawWindowHandle};
use std::io::Write;
use std::sync::{Arc, Mutex, OnceLock};
use wayland_client::backend::{Backend, ObjectId};
use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::wl_data_device::{self, WlDataDevice};
use wayland_client::protocol::wl_data_device_manager::{DndAction, WlDataDeviceManager};
use wayland_client::protocol::wl_data_offer::WlDataOffer;
use wayland_client::protocol::wl_data_source::{self, WlDataSource};
use wayland_client::protocol::wl_pointer::{self, WlPointer};
use wayland_client::protocol::wl_registry::WlRegistry;
use wayland_client::protocol::wl_seat::{self, WlSeat};
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_client::{event_created_child, Connection, Dispatch, Proxy, QueueHandle, WEnum};

/// `BTN_LEFT` from `linux/input-event-codes.h` — the only button a drag
/// starts from.
const BTN_LEFT: u32 = 0x110;

/// The window's drag source. Cheap to hold before it is attached, and
/// inert forever if attaching fails.
#[derive(Default)]
pub struct DragOut {
    /// Filled in once, by the worker thread, when the data device is
    /// ready. Empty means "not yet" or "never" — both of which make a
    /// drag a status line rather than a crash.
    ready: Arc<OnceLock<Ready>>,
    /// Whether [`Self::attach`] has run, so a second call is a no-op.
    attached: OnceLock<()>,
    /// The serial of the left press still being held, if one is.
    press: Arc<Mutex<Option<u32>>>,
}

/// What a drag needs from the worker: the connection it joined and the
/// objects it bound on it.
struct Ready {
    connection: Connection,
    queue: QueueHandle<State>,
    manager: WlDataDeviceManager,
    device: WlDataDevice,
}

impl DragOut {
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
        let (ready, press) = (self.ready.clone(), self.press.clone());
        let spawned = std::thread::Builder::new()
            .name("files-drag-out".into())
            .spawn(move || run_worker(connection, ready, press));
        if let Err(e) = spawned {
            tracing::warn!(error = %e, "the drag worker could not start");
        }
    }

    /// Starts dragging `offers` — `(type, content)` pairs, most specific
    /// first — out of `window`. Must be called while the left button
    /// that began the drag is still held.
    pub fn start(&self, window: &dyn iced::window::Window, offers: Vec<(&'static str, String)>) -> Result<(), String> {
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
fn run_worker(connection: Connection, ready: Arc<OnceLock<Ready>>, press: Arc<Mutex<Option<u32>>>) {
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
    let mut state = State { press, pointer: None };
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
}

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
    fn event(_: &mut Self, _: &WlDataDevice, event: wl_data_device::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        // This device exists to *start* drags. It is told about every
        // clipboard change and every drag over the window as well, and
        // takes none of them — the clipboard is read elsewhere, and
        // dropping onto Files is not something it accepts yet. Each offer
        // is destroyed as soon as it is named, so ignoring them does not
        // leak one per copy anywhere on the desktop.
        match event {
            wl_data_device::Event::Selection { id: Some(offer) } | wl_data_device::Event::Enter { id: Some(offer), .. } => {
                offer.destroy();
            }
            _ => {}
        }
    }

    event_created_child!(State, WlDataDevice, [
        wl_data_device::EVT_DATA_OFFER_OPCODE => (WlDataOffer, ()),
    ]);
}

impl Dispatch<WlDataOffer, ()> for State {
    fn event(_: &mut Self, _: &WlDataOffer, _: <WlDataOffer as Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}

impl Dispatch<WlDataSource, Offer> for State {
    fn event(_: &mut Self, source: &WlDataSource, event: wl_data_source::Event, offer: &Offer, _: &Connection, _: &QueueHandle<Self>) {
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
            wl_data_source::Event::Cancelled | wl_data_source::Event::DndFinished => source.destroy(),
            _ => {}
        }
    }
}
