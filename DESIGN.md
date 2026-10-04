# Getting the file manager to the design

The source is `Hyprfiles Explorer Mockups` — twelve artboards: three candidate
shells (`1a` conservative, `1b` compositor, `1l` command-first) and nine
supporting screens drawn in `1b`'s chrome.

**Assumed shell: `1b`.** The mockup itself settles this — everything from `1c`
on is already drawn in `1b` chrome "so the supporting screens read as one
product". Say otherwise and the re-skin is cheap, because everything below the
view layer is shell-agnostic.

`1l` is not a third shell so much as one idea — *every action is a command* —
wearing a whole window. That idea is a **command palette**, and a palette fits
inside `1b` on `Ctrl+K` without costing the familiar layout anything. Building
`1l` as the actual shell would contradict the stated bar for this app ("as
friendly as Windows File Explorer or Mac Finder"): no persistent sidebar is
excellent for one person and hostile to anyone opening a file manager expecting
one.

## What already exists and does not change

None of this is design-dependent, and none of it is thrown away:

- `hyprforge-files-core`: the model, the `FsBackend` seam and its mock, natural
  sort, filtering, remembered prefs.
- The **`Mode` seam** — the app and the portal dialog render one view, enforced
  by the type rather than by convention. This is what makes the open/save
  dialog look like the real browser, and it survives any re-skin.
- `hyprforge-fileops`: the freedesktop trash spec including the
  cross-filesystem case, and copy/move/rename with per-chunk cancellation.
- Packaging, the `.desktop` entry, `xdg-open` launching, installer wiring.

What changes is **the view layer and what feeds it**.

## The three decisions, settled

Resolved with the user before any of this was written:

1. **Add the colour roles we need** — to `hyprforge-look`, not as hex codes here.
2. **Match the theme we already have.** The suite's palette does not move.
3. **Git is not a concern right now.** Deferred out of every phase below; see
   "Deferred" at the end for what that removes from the mockup.

### 1. The colour roles — settled: add what is missing

The design reserves colour so that "colour always means something": cyan =
remote, green = git-clean, orange = git-dirty, red = danger, purple =
selection. Mapped onto `hyprforge_look::Theme`:

| design | role | in Theme? |
|---|---|---|
| four greys `#21222c → #282a36 → #343746 → #44475a` | elevations | yes — `surfaces.{root,sidebar,card,row}` |
| `#f8f8f2` / `#6272a4` | text / muted | yes — `surfaces.{text,text_dim}` |
| `#bd93f9` purple | selection | yes — `accent` |
| `#50fa7b` green | git clean | yes — `success` |
| `#ffb86c` orange | git dirty | yes — `warning` |
| `#ff5555` red | danger | yes — `error` |
| **`#8be9fd` cyan** | **remote** | **no** |
| **`#ff79c6` pink** | **tags/accent** | **no** |

CLAUDE.md forbids colour constants in an app — the Settings app and lock screen
once drifted to different accents, and that rule is why. So a missing role is a
**`hyprforge-look` change**, never a hex code in this crate.

**What gets added: one role, `info`.** It is the "this is somewhere else" colour
— a remote host, a mounted share, a network location — and it is the only one
of the two that any near-term phase actually needs.

Pink is deliberately *not* added yet. The only thing that wants it is tags, and
tags are their own unplanned subsystem (where does a tag live? an xattr, a
sidecar index, a database? what happens when the file moves?) exactly like git
is. Adding a colour role for a feature nobody has designed is the same mistake
as adding a trait seam with nothing behind it — the role lands when tags do,
and `accent` is there if they turn out not to need their own colour at all.

### 2. Theme fidelity — settled: the suite's palette does not move

Measured:

| | mockup | suite default |
|---|---|---|
| foreground | `#f8f8f2` | `#f8f8f2` ✓ |
| accent | `#bd93f9` | `#bd93f9` ✓ |
| error | `#ff5555` | `#ff5555` ✓ |
| warning | `#ffb86c` | `#f5b942` |
| success | `#50fa7b` | `#3ecf8e` |
| background | `#21222c` | `#16161e` |

**We implement the structure and take every colour from the Theme.** The file
manager will therefore look *close to* but not pixel-identical to the mockup —
its greens and oranges are the suite's, not the mockup's — and it will match
the lock screen, the greeter and the Settings app, which is the entire reason
the suite has one Theme at all.

This matters when reviewing against the mockup: a green that is `#3ecf8e`
rather than `#50fa7b` is **correct**, not a bug. What would be a bug is the
file manager's green differing from the Settings app's.

### 3. Git — settled: deferred

Git status appears in `1a` (a Git column, `M`/`A` per row), `1b` (a branch card
in the sidebar), `1c` (`M3` on a folder), `1e` (a `git:dirty` search chip) and
`1g` (a Git tab in Properties). None of it is in the current plan.

It is a real subsystem — reading status for a whole directory cheaply enough to
run on every navigation, without shelling out per file — and it is **out of
scope for every phase below.**

What that removes from the mockup, so nobody builds it by reflex: the Git
column and its `M`/`A` row badges (`1a`), the branch card at the foot of the
sidebar (`1b`), the `M3` overlay on a folder (`1c`), the `git:dirty` search
chip (`1e`), and the Git tab in Properties (`1g`). Everything else on those
artboards stands.

When it does land: one `git status --porcelain=v2` per directory behind a
backend trait with a mock, bounded and cached, per the shape CLAUDE.md lays out
for anything that talks to a program we do not control. Not per file, ever.

## Density, and a tension worth naming

The design specifies a 40px header, 28px rows, a 15px type ramp, 6px radii
inside chrome and 12px on the window.

Those cannot be constants. Pillar 7 puts font scaling in the shared layer, and
the Theme already carries `font_size`, `font_scale` and `rounding` (the last
from Hyprland's own `decoration:rounding`). So the design's numbers become the
**ratios at 100% scale**: row height derived from font metrics so a user at
125% gets proportionally taller rows rather than clipped text, and radii taken
from the Theme with the design's inner/outer distinction preserved as a
relationship rather than two magic numbers.

### Where these pieces live now

When Settings adopted the same design, the parts of this pass that are not
about files moved to the shared UI crate, and Files takes them back through
re-exports, so none of its call sites changed: the type ramp, the radius
ladder and the bar, field and row heights (`hyprforge-ui/src/density.rs`,
with Files' grid, menu and sidebar sizes left in
`hyprforge-files-core/src/density.rs`); the drawn marks
(`hyprforge-ui/src/glyph.rs`); and `selectable_row_style`, the inset field
and search field, the segmented control, `spaced_caps` and the `Tint` roles
(`hyprforge-ui/src/widgets/`). A change to any of them changes both apps,
which is the point.

## Phases

Each is independently shippable and leaves the app working.

**A — the `1b` shell.** Floating chrome, titlebar tabs, the density pass, and
the sidebar as far as its content actually exists today: **Places** (the XDG
directories, already built), **Pinned** with item counts, and **Trash**.

The mockup's sidebar also carries Tags, Remote and a git card. All three are
blocked on subsystems that do not exist — tags, network shares (phase I) and
git (deferred) — so Phase A builds the sidebar as a list of *sections* with
those three simply absent, rather than as three hardcoded blocks with two of
them empty. An empty "Tags" heading with nothing under it is worse than no
heading: it reads as broken rather than as not-yet.

`hyprforge-look` gains the `info` role in this phase, because the Remote
section is the first thing that will want it and the role should exist before
the first consumer rather than arrive with it.

*Built.* Titlebar tabs, the density pass, and a sidebar of Places, Pinned
(with item counts) and Trash; `hyprforge-look` carries `info`. Devices and
Remote arrived with phase I, the second drawn in `info` as planned.

**B — the fuzzy path bar.** `~/pr/hy/src → ~/projects/hyprsuite/src`, resolving
as you type. The single best idea in the document and the thing neither Finder
nor Explorer has. Needs a matcher (`hyprforge-emoji` already has a search
function worth looking at for prior art) and a resolution preview.

*Built.* Ctrl+L, or a click on the field's empty space, turns the crumbs into
text in place. Each segment is matched against the folder the one before it
landed on (`hyprforge-files-core/src/jump.rs`). The tiers are modelled on the
emoji search: exact, prefix, a later word's start, substring, then letters
in order but only from the name's start, so `hy` never finds `why`. Ties go,
in order, to:

1. somewhere you have been or the sidebar offers
2. a folder over a file
3. the shorter path

The answers hang under the field, the top one heading the list as
`~/pr/hy → ~/projects/hyprsuite`. Up/Down move through them, Tab completes
to one and Enter goes there. A file means its folder, with it selected.

Three rules a typist would otherwise trip on:

- **A path that exists as typed is always the first answer.** That is also
  how `~/x.zip/bin` still works: matching never descends into an archive,
  because listing one means decompressing it.
- **Enter before the answer arrives** goes where that answer says, not
  where the previous text's answer pointed.
- **A burst of typing costs two resolves**, the one running and the
  newest, never one per letter.

The panel is not Files' own. `hyprforge-ui`'s `anchored` and `suggestions`
draw it, so Settings' search can share it. See
`crates/hyprforge-ui/DESIGN-SYSTEM.md`.

Measured on this machine, warm cache, through the real `RoutingBackend`:

| Query | Time |
|---|---|
| `~/do/pr/hyf` | 11ms |
| `~/d/p/h/t/d` (five levels) | 4.5ms |
| `~/.c/hy` | 15ms |
| `~/Documents/Projects/hyprforge/crates/` (literal) | 0.4ms |
| `/u/s/ic` | **53ms** |

`/u/s/ic` is the outlier because `FsBackend::read_dir` stats every entry of
`/usr/bin` to answer "is it a folder". That runs off the UI thread with one
resolve per tab in flight, so it costs latency, not jank. The fix, if it is
ever wanted, is a `d_type`-only name listing on the backend trait. It was
not made here, because that trait lives in a published crate and nothing
under the home directory needed it.

**C — grid and column views.** Grid exists but draws badges; it needs real
thumbnails, which is the preview crate from the original plan. Column view
(`1c`) is new: cascading panes plus a metadata rail.

*Built.* The grid and the list draw real thumbnails — pictures and
SVGs in both, PDFs and videos in the grid — through the shared freedesktop
cache, and a preview pane shows the selected file (`hyprforge-files-core`'s
`preview`, read by the app's `preview.rs`).

**Zoom — built** (asked for by the person using it: Ctrl+scroll did
nothing). Ctrl+wheel over the listing, or Ctrl+= / Ctrl+- / Ctrl+0,
draws it larger or smaller in seven steps from 75% to 200%, per view
and remembered in `files.toml`'s `[zoom]` — the grid zoomed in for
pictures and the list out for rows don't undo each other, the way
Nautilus and Dolphin keep it. Only the listing scales: the zoom
multiplies the `FontScale` it is drawn at, and rows, icons, names and
grid cells all follow that already, so one multiplication sizes them
together and the header, sidebar and status bar stay put. Ctrl+wheel
needed a widget of its own (`hyprforge-ui`'s `wheel_zoom`): the listing
is a scrollable, which takes the wheel before a `mouse_area` around it
ever hears it, so the wrapper looks first and passes a plain wheel
through; a touchpad's pixels are added up so a swipe is a few steps,
not all of them. Not yet: thumbnails are decoded for the unzoomed
size, so at 200% a picture is drawn larger than it was decoded.

**Quick Look — built** (it was on the vision doc's deferred list, not a
mockup artboard). Space shows the keyboard's focused entry on a card over
the dimmed window (`hyprforge-files-core/src/browser/quicklook.rs`, the
card through `hyprforge-ui`'s new `scrim`):

- **The pane's pipeline at a bigger size, not a second one.** The card is
  a `Preview` from the same `preview::build`, asked with its own
  `Outcome::LoadQuickLook` and decoded for `quick_look_edge`: the card's
  picture box in physical pixels, from the window's size and the output's
  scale factor (asked of the window per request). A picture is drawn
  `ScaleDown`, so a small icon is not smeared across the card. Text is
  the pane's excerpt, monospace at the listing's size, never wrapped,
  scrollable both ways; a folder or archive is its first names; anything
  else is its icon and the facts.
- **Its own slot, the pane's rule.** The card shows the *focus*, the pane
  the *selection*; an answer for any entry but the one on show is dropped.
  While the card is up the pane asks for nothing — two full-size decodes
  of one photograph at once is the peak to avoid — and catches up when it
  closes.
- **A held arrow decodes nothing it passes.** The browser asks on every
  move; the host (`src/quicklook.rs`) numbers each request the moment it
  is asked, makes one that follows another within 120ms wait that long,
  runs only the newest and only one at a time. Found live: numbered
  inside the async task instead, an older request could take the newest
  number and leave the entry on show reading forever.
- **Keys.** Arrows move (column view's Left and Right step through the
  folder rather than leaving it), Enter closes and opens, Space or Escape
  closes, and any other key closes the card and then does its job, as the
  transfers popover does. A window key (a tab switch) closes it first.
- **Space and type-to-search.** Space is the one bare text key that may be
  bound (`hyprforge-keys`' `would_swallow_typing` now exempts it): no
  search begins with a space, and one typed between two words of a query
  typed at the listing still reaches it (`Keymap::resolve_typing` with
  `Browser::typing_under_way`). Any action or click ends "typing", after
  which Space is Quick Look again — on the results.
- **In the open/save dialog too.** Browser scope and in `DIALOG_ACTIONS`:
  looking before choosing is a file chooser's whole job, and the dialog
  already reads previews through `host.rs`. Escape there closes the card,
  not the dialog.
- **Inside an archive** nothing is asked: a member has no path another
  program can read, so the card shows what the listing knows.

Measured in a nested Hyprland on a 36-megapixel JPEG (debug build): the
card's decode peaked at 107MB over the window's resident size — the one
full-size decode `hyprforge-image` makes before shrinking, now never two
at once — and kept about 42MB more resident afterwards, the 1336-pixel
picture with the renderer's copy of it.

Not built: **playing video.** The card shows a video's frame, length and
size. Playing it means libmpv, a player thread and a `shader` primitive
(`hyprforge-media/src/film.rs`) in a card that closes on any key — not
cheap, and a frame answers "which video is this". No 3D models either:
an STL previews as its icon and facts. And the vision doc's "shared
component multiple apps call into" is not this yet: the card lives in
`hyprforge-files-core` and its readers in this crate, so Files and its
open/save dialog share it and Media does not. Media's viewer already is
the large view of a file; the piece worth sharing later is the readers.

Column view (`hyprforge-files-core`'s `columns`) puts a pane for each
folder on the way here to the left of the folder in view, from home when
you are under it and from `/` otherwise, as many as the window has room
for — the path bar still names every level, so a pane that does not fit
is never the only way back. Three choices worth knowing:

- **The folder in view is still the one listing.** Selection, rename,
  drag, the menus and the preview pane all act on it exactly as in the
  list; the panes to its left are the way here, drawn like the listing's
  rows but not selectable. A click in one goes there. That makes column
  view a way of *drawing* the browser rather than a second browser with
  its own idea of where you are, and the open/save dialog gets it free.
- **The metadata rail is the preview pane.** The mockup's rail and the
  pane show the same things about the same selection; two would compete.
- **The trail is not the accent.** The folder you went into from each
  pane sits on the raised row surface. The accent means *selected*, and a
  highlighted folder that Delete would not touch would be a lie about what
  the next key does.

Left and Right move between levels — out with the folder you left
selected, in with the first row selected — and Up and Down stay in the
folder in view. A pane already read is kept as the folders change around
it, so going one level deeper reads one folder; F5 reads them all again.
Going up now selects the folder you came out of in every view, which
column view needed for Left and the others were missing anyway.

Every view now keeps the keyboard's row in sight, which column view made
pressing — Left lands on a folder that can be anywhere in a long
listing. The view tags the one focused row, and a widget operation
(`hyprforge-files-core`'s `reveal`) finds it in its scrollable and
scrolls only as far as it takes, so walking down a screenful never moves
the list. The browser cannot do this itself: it knows which row, and only
the layout knows where.

**D — Trash, properly (`1k`).** Arguably the highest value-per-line here: the
data is already parsed and the current view is knowingly wrong to a user. Origin-location column, per-item age, restore
in place, Empty with a confirm. The host already maps stored names back to
original ones; this is the UI that was always going to be needed.

*Built.* The Trash lists each item under the name it had, with an Original
Location column and its deletion date in the date column; Restore puts the
selection back, and Empty Trash always asks first.

**E — Properties (`1g`) and Preferences (`1h`).** Properties is an inspector
docked right with General/Permissions/Open-with tabs. Preferences is
behaviour and keybinds only — appearance belongs to the Settings app, which
the mockup says explicitly and which matches the suite's layering.

*Built.* Both are window actions — Alt+Enter and Ctrl+, — because the
open/save dialog ignores window actions, and that is how both stay out
of a file chooser without the browser's view learning which host it is
in (the `Mode` test is untouched).

**Properties** (`hyprforge-files-core`'s `properties`, the host half in
the app's `properties.rs`) is in every right-click menu but the
sidebar's. With nothing selected it describes the folder in view.

- **One docked panel at a time.** It takes the preview pane's slot.
  Two panels would leave the listing a strip and show the same facts
  twice. Closing it brings the pane back as it was, and asking for the
  preview while it is open swaps them back. Unlike the pane it never
  steps aside on a narrow window: it was asked for, so the listing
  gives way down to its floor.
- **General.** Where, kind (the MIME database's own words), type,
  size and on-disk size, modified, accessed, created, owner:group,
  inode, a link's target. A folder's size is walked off the UI thread.
  The walk does not follow links, stays on one filesystem and counts
  hard links once. It stops at a million entries or a minute and says
  "at least". It is cancelled the moment the selection moves on.
- **Several selected.** A total, the kinds ("3 Rust source · 1 TOML")
  and Compress. Bulk rename is F2 with them selected — see "Bulk
  rename" below; the inspector does not offer a button for it.
- **Permissions.** Nine boxes, live for one file you own. A change goes
  through the host and comes back as what `stat` says. Setuid, setgid
  and sticky are shown, never offered, and an edit never touches them.
- **Open with.** What opens the type, by name, as a double-click would.
  The default is marked, and any other can be made the default. "Other
  applications…" opens the window's chooser.
- **The Trash and archives.** Inside an archive nothing is asked of the
  disk and nothing changes. The Trash is described but read-only, and
  opens nothing.

Left out: the Git tab (Git is deferred), tags (deferred) and the
checksum, which reads the whole file to answer a question nobody asked
by opening Properties.

**Preferences** is a sheet over the window (the app's `preferences.rs`,
decisions in `hyprforge-files-core`'s `preferences`). It has two pages.

- **Behaviour.** Hidden files, folders first, preview pane, view and
  sidebar go to `files.toml` and reach every tab at once. Paste
  conflicts and confirm-before-trash/delete go to `files-config.toml`.
  The number-valued settings (progress delay, undo depth) stay in the
  file. Single-click opening is not offered, because it does not exist.
- **Key bindings.** Every action under a heading, with its keys as caps.
  Change, Add, Clear, and Reset for a line the file holds. A key another
  action holds is named and asked about first; taking it rewrites that
  action's line without the key, so the file never holds a clash. A
  bare letter is refused, because it types into search.

`files-config.toml` is written through `toml_edit`, one entry at a time.
Comments, order and untouched lines survive. A file that will not parse
is reported and never written. A missing one is created with a line
saying what wrote it.

Checked in the nested compositor, with a virtual pointer for the clicks:

- each tab of the inspector
- a group-write tick reaching the disk (`754` to `774`) and the
  listing's column
- making Vim the default writing the scratch `mimeapps.list`
- a folder walked
- a three-item summary
- a Behaviour switch written to the file
- Ctrl+R taken from Refresh for the palette, and both reset

**F — Search (`1e`).** Structured chips (`ext:rs`, `modified:<7d`), a scope
rail, saved smart folders. Today's search is a substring match on one
directory.

*Built.* The search box understands filters, looks below the folder, and
saves to the sidebar. A bare word in this folder is still exactly the old
substring match — `query.rs` keeps the text as typed until a filter
appears, and a test holds it there.

- **The query** (`hyprforge-files-core/src/query.rs`, pure): free text
  plus `ext:`, `kind:` (the listing's own `EntryKind`, plus `file` and
  `folder`), `size:` (binary units, as the Size column prints them),
  `modified:` (`<7d`, `>1y`, `today`, `yesterday`), `name:` (a substring
  or a glob) and `is:hidden`/`is:link`, any of them negated with `-`.
  Relative times are pinned once per pass (`Matcher`), so `today` cannot
  draw its line between two rows.
- **What it does not understand is said.** An unknown key is reported
  beside the field *and* searched as literal text — a file can be called
  `colour:red`. A known key with a bad value (`size:huge`) is reported
  and not applied. Only letters-colon-something is a filter, so `12:30`
  and `Re:` stay text. No `git:` chip: git is deferred, and `git:` is an
  unknown key until it is not.
- **Chips.** A finished filter (one followed by a space) leaves the
  field and becomes a removable chip inside it — `hyprforge-ui`'s
  `token_field` and `removable_chip`, neutral rather than tinted,
  because a chip there is something the person typed, not a state.
- **The scope rail**, a strip under the header only while searching:
  *This folder* (instant, the loaded listing), *Subfolders* and *Home*.
  The mockup's rail runs down the side; a column a sidebar wide for
  three buttons and a status line costs the results too much, so it lies
  flat. The Trash offers only *This folder*: a trashed folder's contents
  are stored names nothing can restore one at a time.
- **The walk** (`hyprforge-files-core/src/search.rs`) goes through the
  `FsBackend`, breadth first so the nearest results come first, and is
  bounded four ways — 15 s, 100,000 folders, 5,000 results, 48 levels —
  each its own ending the rail names, with a suggestion. It never opens
  an archive met on the way (listing one means decompressing it, and the
  archive index cache holds four), skips dotfolders unless dotfiles are
  showing or the query says `is:hidden`, symlinked folders, the Trash's
  storage, and other filesystems (by device, like `find -xdev`). Folders
  it could not read and folders on other drives are counted aloud. A
  search *started* inside an archive walks its members through the
  archive backend, whose index is already read.
- **The host** (`hyprforge-files/src/search_jobs.rs`, shared by the
  window and the open/save dialog): one walk per tab, the newest
  winning — a keystroke cancels the running walk and waits for it to
  stop, so a burst of typing costs two walks. Results come back in
  batches (at most every 120 ms or 250 results). The stream has its own
  deadline two seconds past the walk's, because a `read_dir` stuck on a
  dead mount cannot be cancelled; past it the window is told and the
  worker is abandoned. Closing a tab cancels its walk.
- **Results are the listing while shown**, so selection, Trash, Copy,
  drag and the menus act on each result's own path. They take the
  Trash's Origin column as *Folder*, written from where the search
  started (`tree/proj/src` rather than its full path). *Show in Folder* (Ctrl+Alt+O, and at the head
  of a result's menu) goes there with it selected. A re-read of the
  folder (a paste, a rename, F5) runs the search again rather than
  leaving stale results; Escape or going anywhere ends it.
- **Saved searches** keep the query as text, the folder and whether it
  looks below, in `files.toml`'s `searches` (older files parse). They are
  a *Saved Searches* sidebar section after Pinned, run when clicked, and
  light instead of the folder they search. The rail offers *Save
  search…* with a suggested name, or *Saved as "…" · Remove* when the
  search on screen is one.

Found on the way: a rename went to `current_dir.join(name)`, which for a
result from below would have *moved* it into the folder in view. It
renames in place now, with a test.

Measured on this machine (release build, warm cache, through the real
`RoutingBackend`; counts only):

| Walk from `~` | Folders | Found | Time | Peak memory |
|---|---|---|---|---|
| no dotfolders, nothing matches | 17,865 | 0 | 0.80–0.83 s (1.4 s cold) | 8 MB |
| no dotfolders, `ext:rs` | 17,865 | 588 | 0.80 s, first batch at 72 ms | |
| dotfolders too, unbounded | 109,892 | 1,175,818 | 22.7 s | 29 MB |
| dotfolders, default budget | 100,000 | 0 | 10.4 s — stops at the folder limit | 19 MB |
| everything matches, default budget | 592 | 5,000 | 61 ms — stops at the result limit | 5 MB |

Three folders under `~` were on other filesystems (btrfs subvolumes:
build directories) and were counted rather than entered. In the nested
compositor, a debug build searching all of `~` reported "0 matches in
17,865 folders · 3 folders on other drives not searched" within eight
seconds.

*Built since* — the three pieces first left out:

- **The grid says where a result is.** Among results, each cell carries
  a line of meta text under the name: the list's Folder text, cut from
  the front at whole folder names (`…/files/src`), because the folder a
  result is *in* says more than the one every result shares. iced 0.14
  has no text ellipsis and no measuring before layout, so the cut is by
  characters — `density::GRID_FOLDER_CHARS`, the cell's inner width
  over an average character — and the line is clipped besides. The cell
  stays its fixed 132px: the icon shrinks from 56 to 48 among results
  to make room, rather than the name losing its second line (the thing
  a grid is for is recognising names) or the grid changing height when
  a search starts. A test sums the cell's contents both ways.
- **Ctrl+Shift+F moves the search to the rail's next scope** — This
  folder, Subfolders, Home, and round, skipping what the rail is not
  showing (Home when you are home; everything but This folder in the
  Trash). An action like any other (`next-search-scope`), so it is in
  the palette and rebindable in Preferences, and enabled only while
  searching, when the rail is there to show where it went. Ctrl and a
  letter, because iced's text field types an Alt+letter's text into
  itself and never lets it reach the keymap; Ctrl+letter it leaves
  alone, so the key works with the field focused.
- **`content:` reads what files say, with no index**
  (`hyprforge-files-core/src/content.rs`). It is a predicate the walk
  answers *last*, for whatever the name, size and date filters let
  through, so `ext:rs content:TODO` opens only Rust files. The listing
  cannot answer it, so even This folder walks — one level deep. It
  inherits every bound of the walk, whose clock and cancel flag are now
  asked before each file read as well as each folder, and has its own:
  only regular files (a link or a pipe is refused by `lstat` before any
  open, and the open is non-blocking besides), never an archive (a zip
  on the way is not opened; a walk inside one opens nothing and counts
  every member), a NUL in the first 8 KiB is a binary, nothing over
  4 MiB is opened or read past (a log that grows while being read is cut
  there), and 1 GiB in all, after which the walk ends with its own
  reason, "Stopped after reading 1.0 GiB". Matching streams 64 KiB at a
  time through one buffer per walk, keeping a needle's length of
  overlap so a match across two reads is found; `memchr2` finds the
  first letter in either case, so ASCII case is ignored without a
  lowercased copy. Non-ASCII is compared exactly — folding `ß` to `ss`
  in a stream is a Unicode table and a change of length. A file it
  could not read matches no content filter, negated or not:
  `-content:TODO` is files that were read and do not say it. The rail
  counts both halves: "6 files read · not read: 1 too large, 1 binary,
  1 archive". The Trash, which the walk never enters, says content is
  not searched there rather than quietly matching names.

Measured (release build, through the real `RoutingBackend`, peak memory
from the process's `VmHWM`; counts and times only — the measuring
program printed no names or contents). "First run" is the first search
of a tree after building, partly cold; the rest are warm:

| Search | Folders | Files read | Bytes read | Time | Peak memory |
|---|---|---|---|---|---|
| this monorepo, `content:` (nothing matches) | 170 | 574 | 9 MiB | 15 ms | 4.4 MB (4.3 MB name-only) |
| `~/Documents/Projects`, name only | 11,233 | — | — | 0.80 s | 7.3 MB |
| `~/Documents/Projects`, `content:`, default budget | 5,824 | 42,623 | 1 GiB — the read limit | 4.9 s first run, 0.65 s warm | 7.4 MB |
| `~/Documents/Projects`, `content:`, unbounded | 11,233 | 70,641 | 2.0 GiB | 4.9 s first run, 1.5 s warm | 7.4 MB |
| `~/Documents/Projects`, `ext:rs content:unwrap` | 11,233 | 601 | 18 MiB | 0.52 s | 7.7 MB |
| `~`, `content:`, default budget | 4,793 | 43,704 | 1 GiB — the read limit | 1.6 s first run, 0.63 s warm | 7.5 MB |
| `~`, `content:`, unbounded | 17,867 | 105,690 | 2.6 GiB | 3.6 s | 7.6 MB |

Content search costs no memory a name search does not (one 64 KiB
buffer), and on a source tree it is the folder walk that costs, not
the reading. The read limit is what bounds a search of everything: a
whole home directory is 2.6 GiB of text under the size cap (and 50,603
binaries, 2,615 files over 4 MiB). The instrument was checked against
`grep -rli` on the monorepo (269 files say `unwrap`, both ways, and
`ext:rs content:"fn main"` found the 23 grep finds). In the nested
compositor, a debug build searching `~` for `content:todo` stopped at
the read limit within fifteen seconds with 1,728 matches, saying so.

Not built:

- **Folder sizes in results** stay blank: counting a few thousand
  folders scattered across a tree is the second pass the listing bounds
  at 400, and nobody sorts search results by item count.
- **An index.** `content:` reads on every search, and a search of all
  of home reads until the 1 GiB limit — under two seconds here, but on a
  spinning disk the time limit is what ends it. An index would answer
  instantly and would be a daemon, a database and a staleness problem;
  the bounded read is enough for "this project", which is what it is
  for.
- **Case folding beyond ASCII** and **text in UTF-16** (whose NULs make
  it a binary to the sniff), both of which `grep` also leaves out by
  default.

**G — Transfers (`1f`).** `hyprforge-fileops::ops` already reports progress and
cancels per chunk; this is the popover and the queue window over it.

*Built.* A control at the far end of the tab strip, the popover it opens,
and a queue view behind the popover's "Show all"
(`src/transfers_view.rs`, over `hyprforge_files::transfers`). Archive jobs
report the same `JobEvent` as a paste, so extracting, compressing and
rewriting appear in all three beside copies.

- **The control** says what is going on in as few words as hold it —
  `Copying · 42%`, `3 transfers · 18%`, a short bar beside it — and only
  once work has run for `progress-after-ms`, as the old panel did. Idle,
  it stays as a quiet `Transfers` while the session has history, or
  `2 transfers failed` in the error colour while failures are unread.
- **The popover** lists every running job (title, percentage, bar, then
  `item 2 of 4 · 1.6 GiB of 2.0 GiB · 18.2 MB/s · 4s left · preview.png`)
  and every waiting one, each saying *why* it waits: a slot,
  "the other change to “notes.zip”", or "the other job putting “big”
  there". Cancel per row, Cancel all, Show all. It closes on any click
  outside it, a right click included, and on any key — Escape just
  closes; any other key closes it and still does what it does, because
  Ctrl+K's palette opened *under* the popover the first time and the
  letters typed for it went to the search box.
- **The queue view** is a card over the window, not a second window:
  running, waiting, and every job this session that was on screen or did
  not simply work, each with Show (go to what it put down) and, where it
  is safe, Retry. Failed rows list their reasons once each and say what
  cannot be retried and why. Clear finished; Close; Escape. It keeps the
  keyboard, as the other cards do.
- **Failures** still sit under the listing until dismissed, now with
  Details beside Dismiss; dismissing marks them read and the queue view
  keeps them. Closing the popover or the view never hides one.
- **The overall bar never goes back for no reason.** A job stays in the
  batch, counted whole, until the window is idle, so one finishing does
  not drop the bar; bytes weigh the batch when every job knows its size,
  jobs otherwise, and the number is held at its high-water mark while
  the batch is unchanged. More work joining is the one time it resets.
- **A job's own bar no longer empties between items.** Found live:
  `fileops` runs one operation per pasted item and each counts from
  zero, so copying a big folder and then a small one filled, emptied and
  refilled the bar — the old panel did this too. Each item is now an
  equal share, filled by its own fraction (`JobEvent::Progress` carries
  `Items`), and the estimate is shown only on the last item.
- **Progress is coalesced at the source**, one report per 100ms per job
  however many files go by; `jobs.rs` has a test that copies 3,000 small
  files and bounds the report count by elapsed time.
- **History is bounded**: 50 jobs, the oldest that has nothing left to
  say going first, and each job's reasons held as 64 distinct sentences
  and a count.

Decisions, and what changed since "partly built":

- **Retry is offered where running the work again is the original
  request** — and only there. A paste or restore item that landed
  *nothing* (`fileops` removes a failed file's partial copy, and a move
  whose copy failed keeps its source whole) is retried exactly, through
  the queue, with conflicts asked as usual. An item that *partly* landed
  is not: its first collision would be with its own half-copy, and
  `fileops` has no "merge, skip what is there" answer — Skip on a folder
  skips the subtree, Replace deletes what arrived — so the row says so
  and points at pasting again. An archive edit or compression writes a
  whole new file and renames it into place, so a failed one changed
  nothing and is retried; a retried compression refuses if something now
  sits at its name. A failed extraction is not retried: it unpacks member
  by member, and a retry would make a second folder beside the half-full
  first. DESIGN.md said a retry *policy* needed a queue; the queue made
  a retry *button* safe, and an automatic policy is still not built — a
  failure here is usually a permission or a full disk, which a second
  attempt a second later meets again.
- **The queue serialises on the same destination path too**, not only
  the same archive. Found live, pasting one folder twice quickly: both
  duplicates ran at once, each Keep Both picked the first free `big.2`,
  and the later one failed with "File exists" partway through. Only the
  same *path* waits; different things into one folder still run
  together.
- **The control is in the tab strip, not the status bar** `1b` draws.
  The status bar is `hyprforge-files-core`'s and the open/save dialog
  renders it, and a dialog has no jobs.
- **A card, not the mockup's "full queue window".** On Hyprland a second
  toplevel is tiled, and opening a list would rearrange the person's
  layout. The jobs are threads of this window, so a window that outlived
  it would have nothing to show.
- **Shared pieces** went to `hyprforge-ui`: `progress_line` (the bar, in
  the foreground colour — iced's default fills with the accent, which
  means selected) and `popover_card`.
- **Keys.** Ctrl+Shift+Y opens and closes the popover (Firefox's key
  for its downloads), Ctrl+Shift+J the queue view (Chrome's, with the
  Shift that keeps it off plain Ctrl+J, which this crate's config tests
  use as a key nothing holds). Both are window actions — `transfers`
  and `transfer-queue` in `files-config.toml` — so the open/save dialog
  has neither, and both are in the palette as Show Transfers and Show
  Transfer Queue. Each key also closes what it opened: the popover lets
  any other key through after closing, which would have reopened it,
  and the queue view keeps the keyboard, which would have swallowed it.

Still not built, each for its reason:

- **Pause.** `JobControl` can cancel but not pause, and pausing a job that is
  midway through rewriting an archive is not a state worth being able to sit
  in — the rewrite holds a temporary file beside the original until it
  finishes.
- **Completed/Failed tabs.** The queue view's one list, newest first,
  with a coloured outcome per row, is short enough for a session; tabs
  would hide a failure behind a click.
- **"Retry as root".** Privilege escalation is not something this suite
  does.
- **"Queue survives window close · resumes on reconnect".** Jobs are threads
  in this process, and the remote transfers that line is really about need
  phase I's mounts.
- **A Start button on a queued row.** `1f` has one. A job here is queued
  because the machine is busy, where starting it early gains nothing, or
  because it would write what another job is writing — where starting it
  early is the data loss the queue exists to prevent. A button that is
  only sometimes safe is worse than no button.

Found and left alone, because it is `hyprforge-fileops`' and published:
a file that cannot be *read* is reported as "you don't have permission to
write to" its source path.

Clicked through in the nested compositor (2026-10-03), with a virtual
pointer and a virtual keyboard, over copies of a scratch tree — a 24 GiB
and a 16 GiB sparse file and 3,000 small ones — into three folders at
once, the third read-only: the strip control, the popover with two
running and one waiting ("Waiting for a running job to finish"), a row's
Cancel, Cancel all, Show all, the queue view's Show (it went to the
folder with what landed selected), Retry after making the folder
writable (it finished the copy), Clear finished, Close, both keys and
both palette commands. All of it did what it says. Two things were
fixed on the way:

- **A one-file copy read "5.9 GiB of 0 B"** and its bar could not fill:
  `fileops` counted the size of every file under a folder it walked, and
  never the root's own when the root *is* a file. Fixed there, with a
  test; until that is released the row says only what is done rather
  than a total smaller than it.
- **A cancel that arrived after a file's last chunk** left the whole
  file on disk and the window saying it stopped one item earlier than it
  had — so Show and Undo did not know about the file either. Cancel at
  23.9 of 24 GiB found it. A cancelled item whose one file landed whole
  now counts as placed; a folder cut short still does not.

Seen and not changed: Cancel on a 10 GiB half-copy took one to three
seconds to show, under heavy writeback (the rate had fallen to
90 MiB/s). Not measured further, so not changed — the likely cost is
removing the partial file, which the cancel waits for. And when a job
finishes, the rows below it move up, so a second click where a Cancel
was can land on the next job's; noted, not redesigned.

The queue serialises on *conflict* first and a count second. Two jobs that
rewrite the same archive never run together, whatever else is going on,
because each reads the whole archive and writes a whole new one: the later
rename wins and the earlier edit is silently lost. Two that put something at
the same path never run together either (above). Everything else runs two at
a time, and a blocked job does not hold up an unrelated one behind it.

**H — the command palette.** `1l`'s idea, inside `1b`.

*Built.* Ctrl+K opens a field near the top of the window with every
command under it, narrowed as you type (`hyprforge-files-core/src/palette.rs`).

- **Ranking:** a command's initials first (`nf` is New Folder), then the
  path bar's matcher against its name.
- **What is offered:** only what would do something there and then.
  Keystroke-only actions such as focus moves are left out.
- **Running a command:** it goes through the same `perform` a key or a menu
  does, so the palette is one more way of asking, not a second
  implementation.
- **Drawing:** the shared `suggestions` panel hung from the field, so it
  reads as the path bar's sibling. Hosts draw it through `menu_overlay`, as
  they draw menus.
- **The dialog:** it has the palette too, limited to
  `menu::DIALOG_ACTIONS`, the one list its menus also come from.

Checked in the nested compositor:

- the card sits below the window's chrome rather than over the path bar
  (the first screenshot had it covering the path bar)
- the selected row's key hint is legible on the accent (the first
  screenshot had it dim on purple; the fix is in the shared widget)
- Enter runs the command and closes the palette

**I — Devices, mounts, archives (`1j`)** and the terminal drawer (`1i`).
Archives and network shares were already phases 6 and 7 of the original plan;
the drawer is new and is the most cuttable thing here.

*Archives are done, ahead of this order* — see `hyprforge-archive` and
`hyprforge-files-core::archive`. Two deliberate departures from `1j`, and one
what was owed:

- The mockup labels the pane **read-only**. It is not: members can be added,
  renamed and deleted, and a file opened out of an archive is watched so an
  edit can be offered back. Decided with the user; the rest of `1j` is
  followed.
- The mockup's buttons are **Extract here** and **Extract to…**. What shipped
  is *Extract* (into a new folder named after the archive, so a tarbomb cannot
  scatter two hundred files) and *Extract Here* (into the folder in view).
  There is no destination picker: inside an archive "here" means the folder
  the archive lives in, which is the only real folder on screen, and a picker
  that almost always answers that is a dialog charging for something nobody
  chose.
- ~~**Still owed from `1j`:**~~ Built: the `Packed` column (per-member
  compressed size, from `hyprforge_archive::Member::compressed`, shown only
  inside an archive) and the summary line —
  `zstd · 3 entries · 20.3 MB → 7.0 MB`.

`1f` draws an extraction sitting in the transfers queue beside a copy, and
that works the way it has to: archive jobs report the same `JobEvent` as a
paste, so they appear in phase G's popover and queue view with no code of
their own.

*Devices and network shares: built.* Two sidebar sections between the
user's own lists and the Trash, **Devices** and **Remote** — the
second is the "Remote" phase A left absent, in the `info` role it
reserved. The talking is a new library crate, `hyprforge-volumes`,
built in the D-Bus module order CLAUDE.md lays down: plain data, pure
decisions, backend traits with mocks, then the clients, and a read-only
`check.sh` tier against the real UDisks2.

- **Which drives.** UDisks2's own hints decide, the way GNOME's volume
  monitor reads them: a filesystem (or a locked encrypted container),
  not `HintIgnore`, and not `HintSystem` unless it is mounted under
  `/run/media`. A disk image shows only when this user attached it
  (`SetupByUID`) — snap's and flatpak's loop devices never do. Named by
  the hint, then the label, then the size in the decimal units drives
  are sold in ("1.0 TB Volume").
- **Mounted or not.** An unmounted drive is a row like any other; a
  click mounts it and goes there in the tab that clicked. No root:
  UDisks2 mounts for the session user under polkit, interaction left on
  so a policy that wants a password can ask through the session's agent
  (the mount's bound, two minutes, allows for typing it). Mounted, the
  row is a place — a drop target, a folder's menu — with an eject mark.
- **What is happening, and what went wrong.** The row says
  *Mounting…*, *Unmounting…*, *Ejecting…* in every tab at once (the
  window owns one state and hands every tab the same copy), and a second
  click while it runs asks nothing. A refusal is a sentence on the status
  line, one per kind: something has a file open on it; the system's
  policy said no; the password prompt was cancelled; it is not there any
  more; it is encrypted. An unmount that runs past five minutes says not
  to unplug yet — that one must never sound like success. Eject says
  "can be removed" when it is.
- **Eject** unmounts everything on the same drive first, then powers a
  stick off, opens a tray, or detaches a disk image.
- **Plugged and unplugged.** Every signal UDisks2 sends from under its
  root, and the bus's word when UDisks2 itself starts or stops, means
  *look again*. A burst — dozens for one stick — settles for 300 ms
  (never longer than two seconds) and is one listing. A tab standing on
  a drive that is unmounted, ejected or pulled out goes home.
- **UDisks2 not running** is a sentence in the Devices section, never an
  empty list, and the watch tries again every fifteen seconds, so
  starting it clears the sentence without a restart.
- **Remote** lists what is mounted: gvfs's shares, read from its FUSE
  directory's names, and the kernel's network filesystems (`cifs`,
  `nfs4`, `fuse.sshfs`, …) from `/proc/self/mountinfo`, which reports a
  change to whoever polls it — so a share mounted by another program
  arrives with nothing polled. gvfs's mount tracker signal covers its
  own. **Disconnect** is `gio mount -u` for gvfs, `fusermount3 -u` for
  a FUSE mount and `umount` for the rest, whose refusal is passed on in
  its own words.
- **Connect to Server** (a window action, so in the palette too) is a
  dialog over the window. The mechanism is gvfs's `gio mount`, decided
  after looking at this machine: gvfs is running for any GTK program,
  has the SMB, SFTP and FTP backends, and every share it mounts is a
  real directory under `$XDG_RUNTIME_DIR/gvfs` — so a mounted share is
  browsed with exactly the code a home folder is. `gio`'s command line
  is gvfs's published interface; its D-Bus protocol is private. The
  hint names only the schemes this machine's gvfs can reach (here:
  no `dav://`, whose backend is packaged separately), an address it
  cannot reach is refused with that reason, and gvfs missing is said in
  the dialog rather than the row vanishing.
- **Credentials.** `gio mount` asks for a name and password by printing
  a prompt and reading a line. The runner reads those prompts (`gio`
  started with `LC_ALL=C.UTF-8`, so they are the English words it
  matches) and answers each from what the dialog was given; a prompt it
  has no answer for stops the attempt and the dialog asks. A second
  prompt after an answer means it was refused: the attempt stops rather
  than spend a server's lockout attempts in a loop, and the dialog says
  so with the password cleared. A server's question (an unknown host
  key) comes back as its choices, as buttons. The password is a
  `hyprforge_secret::Secret` from the keystroke on, and the only place
  it is read out is the line written to `gio`'s standard input.
- **Bounded and cancellable.** An attempt is bounded at a minute; Stop
  aborts it, and the `gio` it was running dies with the future
  (`kill_on_drop`).
- **The open/save dialog** has both sections' drives and shares — a
  stick you cannot open is one you cannot save to — and mounts a drive
  that is clicked; it has no drive menu, no eject mark and no Connect to
  Server, which are the session's mounts and not a file chooser's
  business. That is data the host sets (`Devices::manages_mounts`), so
  the view still cannot tell which host it is in.

Checked live, in a nested Hyprland, against a 32 MB FAT image attached
with `udisksctl loop-setup` — no drive of the user's was mounted,
unmounted or ejected: a click mounting it and opening it; Unmount with
a shell standing in it, refused with "is in use"; the eject mark
unmounting and detaching it, the tab going home and "can be removed";
attaching it again, the row arriving without a click; unmounting it
from outside, the tab standing on it going home; `localtest:///` (gvfs's
test backend) connected through the dialog, opened, and disconnected
from its row; mounted and unmounted with `gio` outside the window, the
row following; `dav://` refused as unreachable and `smb://127.0.0.1`
failing with the server's own "Connection refused"; the open/save
dialog mounting the image on a click, with no eject mark. Not checked
live: a password prompt (no server here wants one — the conversation is
tested against a stand-in `gio` that asks as the real one does), a
polkit password, UDisks2 stopped, and a kernel network mount.

Left out, each for its reason:

- **Unlocking an encrypted drive.** It shows, marked *Locked*, and says
  so when clicked. Unlocking is a passphrase prompt and a second
  device to clean up after; worth doing, and its own piece.
- **Browsing the network for servers** (gvfs's `network://`). A list of
  whatever answered a broadcast is a different feature from "connect
  to this address".
- **Remembered servers.** A connection lasts as long as gvfs keeps it;
  nothing is written down, so there is no saved password to protect.
- **Formatting, partitioning, safely removing a whole multi-drive
  enclosure.** Disks utility work, not a file manager's.
- **The terminal drawer (`1i`)** — still deferred; see below.

## Bulk rename — built

F2 with more than one thing selected used to do nothing; it opens a
sheet over the window now, as Preferences does. Three layers, each
tested on its own:

- **The rules** — `hyprforge-files-core/src/bulk_rename.rs`, pure. Four
  modes: find & replace (literal by default, case-insensitive unless
  Match case is on, or a regular expression whose replacement can name
  its groups), add text (before the name, or at its end — before the
  extension, or after it when the extension is included), numbering
  from a template (`{n}`, `{n:3}` zero-padded, `{name}`, `{{`/`}}`) with
  a start and a step, counted in the order the listing is drawn, and
  change case (lower, UPPER, Title, Sentence). The extension is "the
  last dot", the same split the single rename's selection uses, and is
  left alone unless Include the extension is on. Each row is flagged
  when its new name is empty, has a `/` or NUL, is `.`/`..`, is over
  255 bytes, would newly start with a dot, lands on something in the
  folder that is not moving, or is what another row gets too. Apply is
  refused while any row is flagged or the rules themselves do not read
  (a bad pattern or template, a start that is not a number) — said in
  words, never a panic. A pattern's compiled size is capped at 1 MB and
  the `regex` crate matches in linear time, so nothing typed can hang a
  preview that is rebuilt on every keystroke; 10,000 rows rebuild well
  inside a frame (a test bounds it).
- **The batch** — `hyprforge-fileops/src/batch.rs`. `plan` orders the
  renames so none lands on a name still taken, and breaks a swap or a
  longer cycle by stepping one member aside to a hidden temporary name
  in the same folder (`.<name>.renaming`). `apply` checks the whole set
  against the disk first — every source there, every target free or
  about to be freed — so the usual failure moves nothing; then every
  rename is `renameat2(RENAME_NOREPLACE)`, so something appearing
  between the check and the rename is refused by the kernel rather than
  overwritten. If one fails anyway, the ones already done are put back
  newest first, and the failure names everything that could not be put
  back with where it is now — a thing left under a temporary name is
  never silent. A filesystem without the flag gets a check and a plain
  rename; a change of case alone on one that ignores case is let
  through as the same file.
- **The sheet** — `src/bulk_rename.rs` (state, view) and
  `src/bulk_rename_window.rs` (what the window does with it). A live
  table of old → new for every item, drawn with `hyprforge-ui`'s new
  shared `change_row`; unchanged rows dimmed rather than hidden; past
  300 rows it says how many more there are and how many of those have
  problems. It has the keyboard: Escape backs out, Tab and Shift+Tab go
  round its own fields, Ctrl+1–4 pick the mode, Enter applies.

What happens on Apply:

- **On a disk**, the batch runs on a blocking thread and the sheet
  waits, frozen. Done: the sheet closes, the new names are selected
  when the listing comes back, and one undo record (`RenamedAll`)
  offers Ctrl+Z, which runs the reverse as a batch of its own — a swap
  is undone by swapping again, and the undo refuses before touching
  anything if the folder has moved on. Refused before anything moved:
  the sheet stays open with the reason. Anything else: the sheet closes
  and the status bar says exactly what was and was not renamed.
- **Inside an archive**, the same plan becomes the edits of *one*
  rewrite, queued like every archive edit so nothing else rewriting the
  same archive runs beside it. The temporary-name step matters there
  too: `hyprforge-archive` applies renames to its table one after
  another, and a swap applied naively puts both members under one name.
  Only items in the folder in view can be renamed this way — a search's
  results from deeper in an archive have neighbours the listing never
  read, and an archive's table, unlike a disk, would accept two members
  of one name. No undo, as for every archive edit (see `undo.rs`).

Departures from the brief, each for its reason:

- **A disk batch is not a queued job.** Measured: 5,000 renames in a
  rotation — the worst shape, every one through the cycle — took 185 ms
  on this machine's btrfs home in a debug build, the check included. A
  progress bar for a fifth of a second is noise, and Cancel halfway would
  have to put everything back, which is what a failure already does.
  Nothing a paste is placing at the same moment can be written over:
  every rename refuses to replace. A slow network mount is where this
  would change, and phase I has since brought them (a gvfs share is a
  FUSE directory, each rename a round trip to the server) — not yet
  measured, so a batch on a share still runs unqueued and the sheet
  waits; making it a job is owed if that wait turns out long.
- **"Would become hidden" stops the rename** rather than warning. It is
  almost always a stray `.` in Add text, and the one file someone really
  means to hide can be renamed by hand.
- **No Rename button in the Properties inspector** for several items, nor
  in the open/save dialog: the dialog says F2 on several "isn't
  something this dialog does", as for every window-only command.
- **No regex toggles from the keyboard.** iced's toggles and segmented
  buttons take no focus; the modes have Ctrl+1–4, the switches need the
  pointer.

Checked in the nested compositor (headless output, scratch folder):
numbering six files `Holiday {n:03}` in the listing's order, the same
template cut to `Holiday` flagging all six rows as duplicates with Apply
off, the batch landing with all six selected under their new names and
"Renamed 6 items · Undo", Ctrl+Z restoring every name with its
contents, and inside a zip a find & replace of two members as one
rewrite, both left selected.

## Deferred, and what that costs

Named so nothing here is mistaken for an oversight:

- **Git** — decided above. Removes five pieces of five artboards.
- **Tags** — no design exists for where a tag is stored or what happens when
  the file moves. Removes the Tags sidebar section and the pink role.
- **The terminal drawer (`1i`)** — the most cuttable thing in the document, and
  the one with a good alternative: opening a terminal at the current directory
  is one keybind and does not need a drawer.

## The order I would actually build in

A (the shell) → D (Trash) → B (the fuzzy path bar) → C (grid, columns,
previews) → the rest.

D comes second rather than fourth because it is the only place the app is
currently *wrong* rather than merely unfinished: it shows a stored filename
where the user expects the name their file had. Everything else is honest
about being incomplete.

A, D, B, H, C, E, F, G and I (but for the terminal drawer) have since
been built; see each phase above.

The portal open/save dialog is not a phase — it inherits every one of these for
free, because it renders the same view. That is the payoff for the `Mode` seam.

## The open/save dialog — built

`hyprforge-files-portal`, a second binary of this crate, is the desktop's
file chooser:

- **The D-Bus half.** `portal_service.rs` serves
  `org.freedesktop.impl.portal.FileChooser`. Each `OpenFile`, `SaveFile`
  or `SaveFiles` starts one `hyprforge-files-portal dialog` process, hands
  it the request as JSON and answers with what it writes back.
  - **Close.** A `Request` object at the call's handle takes the window
    away on `Close`.
  - **Failures.** A dialog that dies answers "other", never "cancelled".
- **The request model.** `portal.rs` decodes requests from the signatures
  read off the GTK backend installed here, and from the interface XML
  xdg-desktop-portal ships. It covers filters (globs, MIME types and their
  subclasses), choices and the starting folder, and encodes the answer as
  `file://` URIs.
- **The dialog window** is `Browser` in `Mode::Dialog`, untouched.
  - **Around it, never inside it:** a name field, the application's
    filters and choices, and the two buttons.
  - **Filters are a listing input** (`EntryFilter`) like the search box,
    not a mode, so the `Mode` test still holds.
  - **The accept decision is one tested function** (`portal::accept`):
    which files Open returns, when Save asks before overwriting, what
    Enter on a folder does.
- **Shared reading.** The listing, counts, thumbnails, previews and icons
  moved from the window into `host.rs`, so the dialog shows a folder
  exactly as Files does.
- **Floating.** It maps at a fixed size, which Hyprland floats as a
  dialog, then becomes resizable — the arrangement `hyprforge-media`
  arrived at.
- **Opt-in.** Installing it chooses it for nothing. The `.portal` file
  has no `UseIn` line, which would have made it the default on a system
  without a `hyprland-portals.conf`. The user names it in their own
  config; see the README.

Checked:

- End to end in a nested Hyprland, over a private session bus driven by
  `busctl`:
  - An Open call returned the picture's URI and the filter in use.
  - A Save onto an existing name asked before replacing.
  - `Close` took the window away and answered "cancelled".
- `check.sh` has a tier that serves the interface on a bus of its own and
  compares it, method by method, with the installed XML. It was made to
  fail once on purpose.

Two things found on the way:

- **`gdbus call` sends garbage for a `b'…'` byte-string argument.**
  `dbus-monitor` showed 9 bytes of what looks like a pointer where
  `/tmp/xyz` should be, so it is the wrong tool for testing anything that
  takes a path; use `busctl`.
- **A path that is not UTF-8 did not survive the JSON hand-off to the
  dialog**, so paths cross as their bytes (`portal::raw_path`).

Since added:

- **Right-click menus of its own** (`MenuConfig::dialog`), only what a
  dialog does. The window's would have offered Trash, Cut and Extract,
  every one declined; a test holds the list to what the host carries out.
- **Each application's last folder** is remembered in `files-portal.toml`,
  most recent first and capped at 64. A file that will not parse is
  reported and never saved over.

Both were checked in the nested compositor: the menu at the pointer, and
a second dialog from the same application opening where the first was
left.

Not yet:

- attaching to the window that asked (`parent_window` needs xdg-foreign,
  which winit cannot import)

## Dropping onto Files — built, and blocked on Hyprland

`hyprforge-files-core::drop` decides where a drop lands and what it does:

- **Where.** A widget operation asks iced's layout which folder row,
  sidebar place or listing background holds the point, through scrolling.
- **What.** Another application's files are copied, never moved. A drag
  between this window's folders moves on one filesystem and copies across
  two, with Ctrl and Shift to say otherwise. The Trash trashes. Letting go
  where the drag started does nothing.
- **The receiving half** is `dnd.rs`, the device that already started
  drags. A drop goes through the ordinary paste, so conflicts, progress,
  undo and archives behave as a paste does.

It never fires on Hyprland 0.56. Measured with `WAYLAND_DEBUG=client`,
filtered to data-device lines:

- The compositor sent the drag's `enter`, `motion` and `leave` to the
  first `wl_data_device` this client created, which is iced's clipboard's
  (smithay-clipboard). That device ignores every drag event, so letting
  go became a `leave`.
- Hyprland's `dataDeviceForClient` returns the first device a client
  made, and its `updateDrag` sends to that one device. wlroots sends to
  all of them.
- iced connects its clipboard before any of this crate's code runs, so
  this device can never be first.

A drag that starts in Files and ends over it does reach the source:
`cancelled`, then a pointer `enter` at the drop position. But no
`dnd_drop_performed` comes first, so a release cannot be told from Escape.
Moving files on that ambiguity was not built. It waits on Hyprland
sending drags to every device a client has, or on iced's clipboard
accepting them.

## Dragging members out of an archive — built

A member's path is `~/x.zip/notes.txt`, which nothing outside this app
can open, and a drag has to start while the button is still held — far
too soon to unpack a large selection first, the way Copy does. What
makes it possible is the order the protocol does things in: a drag
announces only *types* when it starts, and the `text/uri-list` itself is
asked for when something accepts the drop, written into a pipe the
receiver reads at its own pace. So (`src/drag_out.rs`, wired in
`dnd.rs` and the window's `start_archive_drag`):

- **The drag starts at once**, naming where the members *will* be: a
  directory of its own per drag under `$XDG_CACHE_HOME/hyprforge-files/drags`,
  each member at its top under its own name (two of one name, from a
  search inside the archive, go in `2/`). Making that directory is the
  only thing done on the UI thread.
- **They are unpacked beside it**, one extraction per folder dragged
  from, through `hyprforge_archive` (which pins the archive, so the bytes
  are the file that was dragged from). The status bar says
  "Unpacking 3 items to drag…" — the pointer is busy being the drag.
- **The answer waits for the files.** The receiver's request is
  answered on a thread of its own, which waits on a gate until the
  unpacking is over: it gets every path once they all exist, or an empty
  list. Never a list of files that are not there yet.
- **Bounded.** Past 2 GiB unpacked it is refused before anything is
  written — "That's 2.1 GiB to unpack before it can be dropped — more
  than a drag carries. Copy or Extract it instead." — because a drag
  has nowhere to show progress, and Copy and Extract do the same work in
  the transfers queue where it can be watched. Two minutes is the most
  the unpacking may run and the most a receiver waits.
- **Locked archives** use the password already given this session, the
  one Copy uses; with none, the drag is refused in words ("… is
  encrypted — open a file in it once to unlock it, then drag."). A drag
  has no room for a prompt.
- **A refusal ends the drag** rather than leaving it looking droppable:
  destroying the data source is the protocol's way to cancel a drag in
  flight.

**When the copies go** is the one real decision. Not on `dnd_finished`:
that means the receiver has *read the list*, and a file manager
receiving a drop starts its copy afterwards, which can run for minutes.
Not when this window closes, for the same reason — dropping into
another window and closing this one is ordinary. So:

- a drag that was not taken (`cancelled`, and no receiver was ever
  handed the list) is removed at once, as is anything a refused or
  failed unpack wrote;
- one that was handed over is left, and swept: at startup and at the
  start of every drag out of an archive, anything older than a day goes,
  and past the eight most recent anything older than an hour.

On disk under the cache rather than in `/tmp`, which is a RAM-backed
tmpfs on most of the machines this suite runs on.

A drag let go over the archive listing it came from does nothing, as any
drag let go where it started does. Such a drop arrives as the copies'
paths, through the compositor like anyone else's (the members' own
paths are nothing a paste could read), so the window remembers its
latest archive drag to recognise it — `OwnDrag::returned_home`. Found
live: without it the members were added back into their own archive.

Checked in the nested compositor, into a GTK4 window whose only drop
handling is `Gtk.DropTarget` for a `Gdk.FileList` — someone else's
implementation — recording each path and whether it existed when the
drop arrived:

- a folder (`docs`, two members deep) arrived with both files in it;
- a 5 MB member arrived whole;
- a 1 GiB member: the drop arrived four seconds after the button came
  up, with the file complete — the receiver had waited for it;
- a 2.1 GiB member was refused in words and nothing was dropped;
- an encrypted zip's member, with no password given, was refused in
  words and nothing was dropped;
- dropped on a GTK window that takes no drops: `cancelled`, and the
  drag's directory was gone.

Not checked live: unlocking with a remembered password, because typing
it means opening a member, and opening one launches the desktop's
editor, which can attach to the real session over D-Bus. The unit test
unpacks the encrypted member with the password and without.

**One thing the rig taught.** The first run dropped a member back onto
Files and it *arrived* — `enter`, `drop`, `dnd_drop_performed` on this
crate's own data device — which looked like Hyprland delivering to every
device after all. It was the rig: with no keyboard in the nested
compositor, iced's clipboard had made no data device, so this crate's
was the client's first. With a virtual keyboard present the clipboard's
device was first again, the drag entered *it*, and the source got
`cancelled`, exactly as "Dropping onto Files" above says.

## What to verify, not assume

- Screenshot the real window against the mockup at the same size, and compare
  a centre pixel of each surface against the Theme value. That is how the
  `web-colors` drift was found and it is the way to settle "does this look
  right".
- Every new colour comes from the Theme. A grep for a hex literal in this
  crate should return nothing.
- The `Mode` test must keep passing through the whole re-skin: if `view()`
  starts branching on App vs Dialog, the open/save dialog has quietly stopped
  being the same browser.
