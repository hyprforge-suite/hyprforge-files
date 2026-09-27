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
(with item counts) and Trash; `hyprforge-look` carries `info`.

**B — the fuzzy path bar.** `~/pr/hy/src → ~/projects/hyprsuite/src`, resolving
as you type. The single best idea in the document and the thing neither Finder
nor Explorer has. Needs a matcher (`hyprforge-emoji` already has a search
function worth looking at for prior art) and a resolution preview.

**C — grid and column views.** Grid exists but draws badges; it needs real
thumbnails, which is the preview crate from the original plan. Column view
(`1c`) is new: cascading panes plus a metadata rail.

*Partly built.* The grid and the list draw real thumbnails — pictures and
SVGs in both, PDFs and videos in the grid — through the shared freedesktop
cache, and a preview pane shows the selected file (`hyprforge-files-core`'s
`preview`, read by the app's `preview.rs`). Column view is not built: its
segment in the view toggle is drawn disabled.

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

**F — Search (`1e`).** Structured chips (`ext:rs`, `modified:<7d`), a scope
rail, saved smart folders. Today's search is a substring match on one
directory.

**G — Transfers (`1f`).** `hyprforge-fileops::ops` already reports progress and
cancels per chunk; this is the popover and the queue window over it.

*Partly built.* The panel shows every running job — the window had always kept
a `Vec` and drawn the first, which only started to matter once extracting,
compressing and rewriting an archive became jobs too. Each row has a bar, a
rate and an estimate, and failures now live in a panel that keeps them rather
than a status line the next job overwrites.

Not built, and each for a reason rather than for lack of time:

- **Pause.** `JobControl` can cancel but not pause, and pausing a job that is
  midway through rewriting an archive is not a state worth being able to sit
  in — the rewrite holds a temporary file beside the original until it
  finishes.
- ~~**A queue.**~~ Built, and it turned out to be a correctness fix rather
  than a scheduling preference — see below.
- **Completed/Failed tabs, retry policy, "Retry as root".** The failures panel
  covers what the Failed tab is for. A retry policy needs a queue; "as root"
  needs privilege escalation this suite does not do.
- **"Queue survives window close · resumes on reconnect".** Jobs are threads
  in this process, and the remote transfers that line is really about need
  phase I's mounts.
- **A Start button on a queued row.** `1f` has one. A job here is queued
  either because the machine is busy, where starting it early gains nothing,
  or because it would rewrite an archive another job is already rewriting —
  where starting it early is the data loss the queue exists to prevent. A
  button that is only sometimes safe is worse than no button.

The queue serialises on *conflict* first and a count second. Two jobs that
rewrite the same archive never run together, whatever else is going on,
because each reads the whole archive and writes a whole new one: the later
rename wins and the earlier edit is silently lost. Everything else runs two at
a time, and a blocked job does not hold up an unrelated one behind it.

**H — the command palette.** `1l`'s idea, inside `1b`.

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
that already works the way it has to: archive jobs report the same `JobEvent`
as a paste, so when phase G builds the popover they appear in it for free.

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

A, D and the thumbnail and preview half of C have since been built;
see each phase above.

The portal open/save dialog is not a phase — it inherits every one of these for
free, because it renders the same view. That is the payoff for the `Mode` seam.

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
