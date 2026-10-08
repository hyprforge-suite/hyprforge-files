//! The file manager application.
//!
//! A library as well as a binary, the same arrangement `hyprforge-tray`
//! uses: the window in `main.rs` is a thin composition root, and
//! everything it is made of lives here where a test can reach it
//! without opening a window or talking to a compositor.
//!
//! The browsing view itself is **not** here — it is in
//! `hyprforge-files-core`, because the portal's open/save dialog has to
//! render the same one. What this crate owns is the window around it
//! and the things a dialog deliberately does not have: launching what
//! you double-click, and file operations — including the ones that go
//! through an archive, which `archive_jobs` runs the same way `jobs`
//! runs a paste — and the parts that need the window's own connection
//! or a disk: reading what the preview pane and the thumbnails show
//! (`preview`) and what Quick Look shows (`quicklook`), the Wayland
//! clipboard (`system_clipboard`) and dragging files out to other
//! applications and taking drops from them (`dnd`, with `drag_out`
//! unpacking what is dragged out of an archive).
//! The Properties inspector's looking and changing (`properties`) and
//! the Preferences sheet (`preferences`) are here too: both are the
//! window's, and the open/save dialog has neither.
//! So are keeping a folder on screen up to date as other programs
//! change it (`watch`) and reopening last time's tabs (`session`).

pub mod admin;
pub mod admin_client;
pub mod admin_jobs;
pub mod archive_jobs;
pub mod bulk_rename;
pub mod connect;
pub mod devices;
pub mod dnd;
pub mod drag_show;
pub mod drag_out;
pub mod file_manager1;
pub mod handoff;
pub mod host;
pub mod inner_drag;
pub mod jobs;
pub mod launch;
pub mod notify;
pub mod portal;
pub mod portal_service;
pub mod preferences;
pub mod preview;
pub mod properties;
pub mod quicklook;
pub mod quicklook_live;
pub mod search_jobs;
pub mod session;
pub mod start;
pub mod system_clipboard;
pub mod tag_io;
pub mod tags_sheet;
pub mod tabstrip;
pub mod terminal;
pub mod transfers;
pub mod versions;
pub mod watch;
