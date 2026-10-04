# hyprforge-files

A file manager for Hyprland.

Tabs in the titlebar, a sidebar of places and pins, list, grid and column views,
a path bar that understands `~/do/pr/hyf` as `~/Documents/Projects/hyprforge`,
a Ctrl+K palette of every command,
a search that understands filters (`ext:rs`, `size:>10M`, `modified:<7d`)
as chips, looks below the folder or across home, and saves to the
sidebar, multi-select and history — and the freedesktop trash done
properly, including the part that bites: a file cannot be renamed across
filesystems, so a file on another one needs a trash directory of its own,
which is not an exotic case on a btrfs laptop where subvolumes report
different device numbers.

It also browses **archives as folders**. `~/Downloads/x.tar.xz/bin` is a
path this browser navigates to — zip, tar over gzip/bzip2/xz/zstd, and
7z, all pure Rust so a machine with no p7zip still opens a `.7z`. They
can be edited in place too: add, rename and delete, each rewrite going
to a temporary file beside the original and renamed over it, so an
interrupted edit never leaves a truncated archive.

A **preview pane** beside the listing shows the selected file — a
picture, the first lines of a text file, a folder's or an archive's
contents, a PDF's first page, a video's frame or a song's cover — and
the grid draws real thumbnails of pictures, SVGs, PDFs and videos,
the costly ones kept in the freedesktop thumbnail cache every other
file manager and viewer already shares. Files can be copied and pasted with other
applications, and dragged out of the window into them.

**Drives and servers** are in the sidebar. Devices lists removable
drives and disk images, mounted or not: a click on one that is not
mounted mounts it — over UDisks2, with no root, polkit deciding — and
goes there, and the eject mark or its menu unmounts, ejects or powers it
off, saying in words why when it cannot (something still has a file
open on it; the system's policy said no). Plugging a stick in or pulling
it out shows at once. Remote lists the network shares that are mounted,
gvfs's and the kernel's, and **Connect to Server** mounts an `smb://`,
`sftp://` or `ftp://` address through gvfs, asking for a name and
password when the server wants one. UDisks2 not running, or gvfs not
installed, is said in the sidebar or the dialog rather than hidden.

Copies, moves and archive work run in the background through a
**queue**: a control in the tab strip shows overall progress, its
popover lists each job with its rate and estimate and why a waiting one
waits, and a queue view keeps the session's finished jobs, their
failures, and a Retry wherever running the work again is safe.

**Properties** (Alt+Enter, or the bottom of any right-click menu) docks
in the preview pane's place: General, with a folder's size walked in the
background; Permissions, editable for a file you own; and Open with,
where any application for the type can be made its default. **Preferences**
(Ctrl+,) covers behaviour and every key binding — rebind, clear or reset,
with a key another command holds named before it is taken — and writes
`files-config.toml` one line at a time, so anything written there by
hand stays. Colours and fonts are the Settings app's.

It can also be **every application's Open and Save dialog**, through
xdg-desktop-portal — the same browser, path bar and previews, not a
smaller copy. See "The open/save dialog" below.

Part of [Hyprforge](https://github.com/hyprforge-suite/hyprforge), a suite of
native Hyprland desktop applications. It appears there as a
submodule at `crates/hyprforge-files`; this repository is where its code
lives, and pull requests here are welcome.

## What is here and what is next door

The browsing view is **not** in this crate. It lives in
`hyprforge-files-core`, because the portal's open/save dialog has to
render the same one — enforced by the type rather than by convention:
`view()` takes a private `ViewModel` with no `mode` field, so it cannot
branch on which host it is in.

What this crate owns is the window around it, and the things a dialog
deliberately does not have: launching what you double-click, file
operations, the transfers popover and queue view, the archive jobs, the system clipboard,
drag and drop with other applications, and connecting to a server
(`connect.rs`). Reading what the listing,
the preview pane and the thumbnails show is `host.rs`, shared with the
dialog — the second binary here, `hyprforge-files-portal` — and so is
watching drives and shares (`devices.rs`): a stick can be opened, and
saved to, from a file chooser too, though not ejected from one. The
talking to UDisks2 and gvfs is `hyprforge-volumes`'.

## Installing

```
cargo install --path .
```

Arch users can build the `hyprforge-files` package from the monorepo's
`packaging/arch` instead, which also installs the desktop entry.

Nothing else in the suite is required. A missing `appearance.toml` is
first-run, not an error, and the app draws correctly themed on a machine
where the Settings app has never been installed.

`pdftoppm` (poppler) and `ffmpeg` are optional too: without them a PDF,
a video or a song previews as the details the listing already knows and
a sentence naming what to install, and a PDF or a video gets no
thumbnail.

So are UDisks2 and gvfs. Without UDisks2 the Devices section says it
isn't running; without gvfs (and `gvfs-smb` for Windows shares) the
Connect to Server dialog says what to install. Network shares mounted
some other way — an fstab line, `sshfs` — are listed either way.

## The open/save dialog

`hyprforge-files-portal` serves xdg-desktop-portal's
`org.freedesktop.impl.portal.FileChooser`. Each request starts one
dialog window, which answers and exits; an application that gives up on
its dialog takes the window with it. Each application's dialog opens
where it was last left, remembered in `files-portal.toml` beside Files'
own settings.

Installing it switches nothing. To make it every application's dialog,
put this in `~/.config/xdg-desktop-portal/hyprland-portals.conf`:

```
[preferred]
default=hyprland;gtk
org.freedesktop.impl.portal.FileChooser=hyprforge
```

and run `systemctl --user restart xdg-desktop-portal`. Keep the
`default=` line: the portal reads only the first `portals.conf` it finds.
Removing the last line puts the previous dialog back.

The package installs the `.portal` descriptor where xdg-desktop-portal
looks for one (`/usr/share/xdg-desktop-portal/portals`, the only place
it does) and a D-Bus activation file, so the service starts when an
application asks and at no other time.

## Licence

MIT. See `LICENSE`.
