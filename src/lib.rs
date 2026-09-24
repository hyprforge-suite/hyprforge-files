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
//! runs a paste.

pub mod archive_jobs;
pub mod jobs;
pub mod launch;
pub mod system_clipboard;
pub mod tabstrip;
