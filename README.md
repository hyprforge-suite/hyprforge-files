# hyprforge-files

A file manager for Hyprland.

Tabs in the titlebar, a sidebar of places and pins, list and grid views,
search, multi-select and history — and the freedesktop trash done
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

Part of [Hyprforge](https://github.com/hyprforge-suite/hyprforge), a suite of
native Hyprland desktop applications. This repository is a split of the
`crates/hyprforge-files` directory there; development happens in the
monorepo and `sync.sh` keeps this copy in step.

## What is here and what is next door

The browsing view is **not** in this crate. It lives in
`hyprforge-files-core`, because the portal's open/save dialog has to
render the same one — enforced by the type rather than by convention:
`view()` takes a private `ViewModel` with no `mode` field, so it cannot
branch on which host it is in.

What this crate owns is the window around it, and the things a dialog
deliberately does not have: launching what you double-click, file
operations, the transfers panel, the archive jobs, reading what the
preview pane and the thumbnails show, the system clipboard, and dragging
files out to another application.

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

## Licence

MIT. See `LICENSE`.
