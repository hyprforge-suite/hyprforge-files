//! Building what the preview pane shows for one selected entry.
//!
//! The browser asks (`Outcome::LoadPreview`) and draws; this is the half
//! that reads. Everything here runs on a worker thread, and everything is
//! bounded, because the pane asks again every time the selection moves:
//!
//! - text: the first [`TEXT_BYTES`] of the file, at most [`TEXT_LINES`]
//!   lines of it;
//! - a folder: names only, no `stat` per entry, and no more than
//!   [`FOLDER_SCAN`] of them counted;
//! - pictures: decoded to the pane's width by `hyprforge-image`, the
//!   same budgeted decode the grid uses;
//! - PDFs, video and audio: another program, `pdftoppm` or `ffmpeg`,
//!   through `hyprforge_process::output` with its timeout, asked for a
//!   picture already scaled to the pane — so what comes back is a few
//!   hundred kilobytes of PNG whatever the source was.
//!
//! # The tools are optional
//!
//! A machine without poppler or ffmpeg still gets a preview of those
//! files: the details the listing already knows, and a sentence naming
//! what to install. CLAUDE.md's rule that a sibling missing is a state
//! with a message, not a failure, applies to somebody else's program as
//! much as to ours.

use std::path::Path;
use std::process::Command;

use hyprforge_files_core::preview::{Excerpt, Listing, Picture, Preview};
use hyprforge_files_core::backend::FsBackend;

/// How much of a text file is read.
pub const TEXT_BYTES: usize = 32 * 1024;
/// How many of its lines are shown.
pub const TEXT_LINES: usize = 120;
/// A line longer than this is cut: minified JavaScript is one line of
/// megabytes, and the pane does not wrap.
const LINE_CHARS: usize = 160;
/// How many names a folder's preview shows.
const LISTING_NAMES: usize = 12;
/// How many entries a folder's count reads before saying "at least".
const FOLDER_SCAN: usize = 10_000;

/// Everything the pane can say about `path`, or `None` when it can say
/// nothing the listing does not already.
///
/// `edge` is the picture's widest side in physical pixels.
pub fn build(path: &Path, mime: &hyprforge_mime::MimeDb, backend: &dyn FsBackend, edge: u32) -> Option<Preview> {
    let preview = if path.is_dir() {
        folder(path)
    } else {
        // By contents as well as name: a file called `notes` with no
        // extension is text, and only reading it can say so.
        let kind = mime.sniff(path).mime;
        let kind = mime.canonical(&kind).to_string();
        file(path, &kind, mime, backend, edge)
    };
    preview.filter(|p| !p.is_empty())
}

fn file(path: &Path, kind: &str, mime: &hyprforge_mime::MimeDb, backend: &dyn FsBackend, edge: u32) -> Option<Preview> {
    if hyprforge_image::format::looks_decodable(path) {
        return Some(picture(path, edge));
    }
    if kind == "image/svg+xml" || kind == "image/svg+xml-compressed" {
        return Some(Preview { picture: Some(Picture::from_path(path)), ..Preview::default() });
    }
    if kind == "application/pdf" {
        return Some(pdf(path, edge));
    }
    if kind.starts_with("video/") {
        return Some(media(path, edge, true));
    }
    if kind.starts_with("audio/") {
        return Some(media(path, edge, false));
    }
    if path.file_name().is_some_and(|n| hyprforge_files_core::archive::looks_browsable(&n.to_string_lossy())) {
        return archive(path, backend);
    }
    if kind == "text/plain" || mime.is_subclass_of(kind, "text/plain") {
        return text(path).map(|text| Preview { text: Some(text), ..Preview::default() });
    }
    None
}

/// A picture this build decodes, and how big it really is.
fn picture(path: &Path, edge: u32) -> Preview {
    let picture = hyprforge_image::decode_to_fit(path, &hyprforge_image::Budget::for_edge(edge))
        .map(|d| Picture::Raster(iced::widget::image::Handle::from_rgba(d.size.width, d.size.height, d.pixels)))
        .map_err(|e| tracing::debug!(error = %e, "no preview picture"))
        .ok();
    let mut details = Vec::new();
    if let Ok(measured) = hyprforge_image::measure(path) {
        let (w, h) = measured.display_size();
        details.push(("Dimensions".to_string(), format!("{w} × {h}")));
    }
    Preview { picture, details, ..Preview::default() }
}

/// The start of a text file, or `None` when it turns out not to be text
/// after all — a NUL in the first block is the same test `grep` uses.
pub fn text(path: &Path) -> Option<Excerpt> {
    use std::io::Read;
    let mut head = Vec::with_capacity(TEXT_BYTES);
    std::fs::File::open(path).ok()?.take(TEXT_BYTES as u64 + 1).read_to_end(&mut head).ok()?;
    excerpt(&head)
}

/// [`text`], on bytes already read — the part a test can reach.
pub fn excerpt(head: &[u8]) -> Option<Excerpt> {
    let past_the_end = head.len() > TEXT_BYTES;
    let head = &head[..head.len().min(TEXT_BYTES)];
    if head.contains(&0) {
        return None;
    }
    // Lossy: a cut through the middle of a multi-byte character at the
    // end of the block is expected, and one replacement character there
    // is better than refusing the whole file.
    let decoded = String::from_utf8_lossy(head);
    let mut lines = decoded.lines();
    let shown: Vec<String> = lines
        .by_ref()
        .take(TEXT_LINES)
        .map(|line| {
            let line = line.replace('\t', "    ");
            match line.char_indices().nth(LINE_CHARS) {
                Some((cut, _)) => format!("{}…", &line[..cut]),
                None => line,
            }
        })
        .collect();
    let truncated = past_the_end || lines.next().is_some();
    Some(Excerpt { text: shown.join("\n"), truncated })
}

/// A folder's first names and its count, from names alone.
///
/// `std::fs::read_dir` rather than the browser's backend: the backend
/// `stat`s every entry for the listing's columns, which is the right cost
/// for navigating into a folder and the wrong one for glancing at it.
/// `file_type` comes from the directory entry itself on every filesystem
/// that matters, so telling folders from files costs nothing more.
fn folder(path: &Path) -> Option<Preview> {
    let read = std::fs::read_dir(path).ok()?;
    let mut names: Vec<(String, bool)> = Vec::new();
    let mut total = 0usize;
    for entry in read.flatten() {
        total += 1;
        if total > FOLDER_SCAN {
            break;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        let is_dir = entry.file_type().is_ok_and(|t| t.is_dir());
        names.push((name, is_dir));
    }
    let hidden = total.min(FOLDER_SCAN) - names.len();
    let mut details = Vec::new();
    if hidden > 0 {
        details.push(("Hidden".to_string(), format!("{hidden} more, starting with a dot")));
    }
    if total > FOLDER_SCAN {
        details.push(("Items".to_string(), format!("More than {FOLDER_SCAN}")));
    }
    let shown = names.len();
    Some(Preview { listing: Some(first_names(names, shown)), details, ..Preview::default() })
}

/// An archive's top level, through the backend that already browses one
/// as a folder.
fn archive(path: &Path, backend: &dyn FsBackend) -> Option<Preview> {
    let entries = backend.read_dir(path).ok()?;
    let total = entries.len();
    let names = entries.into_iter().map(|e| (e.name, e.is_dir)).collect();
    Some(Preview { listing: Some(first_names(names, total)), ..Preview::default() })
}

/// Folders first, then by name, cut to what the pane shows.
fn first_names(mut names: Vec<(String, bool)>, total: usize) -> Listing {
    names.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.to_lowercase().cmp(&b.0.to_lowercase())));
    names.truncate(LISTING_NAMES);
    Listing { names, total }
}

/// A PDF's first page and its page count, from poppler's own tools.
fn pdf(path: &Path, edge: u32) -> Preview {
    let page = run(Command::new("pdftoppm")
        .args(["-f", "1", "-l", "1", "-singlefile", "-png", "-scale-to"])
        .arg(edge.to_string())
        .arg(path));
    let mut details = Vec::new();
    match &page {
        Err(Missing) => details.push(install("poppler", "a PDF's first page")),
        Ok(None) => {}
        Ok(Some(_)) => {
            if let Ok(Some(info)) = run(Command::new("pdfinfo").arg(path)) {
                let info = String::from_utf8_lossy(&info);
                for (key, label) in [("Pages:", "Pages"), ("Title:", "Title"), ("Author:", "Author")] {
                    if let Some(value) = info.lines().find_map(|l| l.strip_prefix(key)).map(str::trim) {
                        if !value.is_empty() {
                            details.push((label.to_string(), value.to_string()));
                        }
                    }
                }
            }
        }
    }
    let picture = page.ok().flatten().map(|png| Picture::Raster(iced::widget::image::Handle::from_bytes(png)));
    Preview { picture, details, ..Preview::default() }
}

/// A video's frame, or a song's cover, with what `ffprobe` says about it.
fn media(path: &Path, edge: u32, video: bool) -> Preview {
    let probe = run(Command::new("ffprobe")
        .args(["-v", "error", "-print_format", "json", "-show_entries"])
        .arg("format=duration:format_tags=title,artist,album:stream=codec_type,width,height:stream_disposition=attached_pic")
        .arg(path));
    let info = match probe {
        Err(Missing) => {
            let what = if video { "a video's frame and length" } else { "a song's cover and tags" };
            return Preview { details: vec![install("ffmpeg", what)], ..Preview::default() };
        }
        Ok(None) => return Preview::default(),
        Ok(Some(json)) => Probe::parse(&json),
    };

    let mut details = Vec::new();
    for (label, value) in [("Title", &info.title), ("Artist", &info.artist), ("Album", &info.album)] {
        if let Some(value) = value {
            details.push((label.to_string(), value.clone()));
        }
    }
    if let Some(seconds) = info.duration {
        details.push(("Duration".to_string(), duration(seconds)));
    }
    if let Some((w, h)) = info.size.filter(|_| video) {
        details.push(("Dimensions".to_string(), format!("{w} × {h}")));
    }

    // A tenth of the way in, and never more than half a minute: the first
    // frame of most videos is black, and seeking far into a long one is
    // slow on a spinning disk.
    let at = info.duration.map_or(0.0, |d| (d * 0.1).min(30.0));
    let picture = if video || info.has_cover {
        let mut command = Command::new("ffmpeg");
        command.args(["-v", "error"]);
        if video {
            command.arg("-ss").arg(format!("{at:.2}"));
        }
        command
            .arg("-i")
            .arg(path)
            .args(["-an", "-frames:v", "1", "-vf"])
            .arg(format!("scale={edge}:-2"))
            .args(["-f", "image2pipe", "-c:v", "png", "-"]);
        run(&mut command).ok().flatten().map(|png| Picture::Raster(iced::widget::image::Handle::from_bytes(png)))
    } else {
        None
    };
    Preview { picture, details, ..Preview::default() }
}

/// What `ffprobe` said, reduced to what the pane shows.
#[derive(Debug, Default, PartialEq)]
pub struct Probe {
    pub duration: Option<f64>,
    pub size: Option<(u64, u64)>,
    pub has_cover: bool,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
}

impl Probe {
    pub fn parse(json: &[u8]) -> Probe {
        let Ok(value) = serde_json::from_slice::<serde_json::Value>(json) else {
            return Probe::default();
        };
        let format = &value["format"];
        let tag = |key: &str| {
            // Tag names are case-insensitive by convention and files
            // disagree: MP3s say `title`, some Vorbis files `TITLE`.
            format["tags"].as_object().and_then(|tags| {
                tags.iter()
                    .find(|(k, _)| k.eq_ignore_ascii_case(key))
                    .and_then(|(_, v)| v.as_str())
                    .map(str::trim)
                    .filter(|v| !v.is_empty())
                    .map(String::from)
            })
        };
        let streams = value["streams"].as_array().cloned().unwrap_or_default();
        let is_cover = |s: &serde_json::Value| s["disposition"]["attached_pic"].as_i64() == Some(1);
        let picture_stream = |s: &&serde_json::Value| s["codec_type"] == "video" && !is_cover(s);
        Probe {
            duration: format["duration"].as_str().and_then(|d| d.parse().ok()),
            size: streams
                .iter()
                .find(picture_stream)
                .and_then(|s| Some((s["width"].as_u64()?, s["height"].as_u64()?))),
            has_cover: streams.iter().any(is_cover),
            title: tag("title"),
            artist: tag("artist"),
            album: tag("album"),
        }
    }
}

/// `1:02:03`, `4:05`.
pub fn duration(seconds: f64) -> String {
    let total = seconds.max(0.0).round() as u64;
    let (h, m, s) = (total / 3600, total / 60 % 60, total % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

/// A preview row naming the package that would show more.
fn install(package: &str, what: &str) -> (String, String) {
    ("Preview".to_string(), format!("Install {package} to see {what} here."))
}

/// The program is not installed.
struct Missing;

/// Runs `command` to completion within the usual bound: its standard
/// output when it succeeded, `None` when it ran and failed — a damaged
/// PDF, a video with no frames, a timeout — and [`Missing`] when it is
/// not installed at all, which is the one case worth telling the user.
fn run(command: &mut Command) -> Result<Option<Vec<u8>>, Missing> {
    match hyprforge_process::output(command, hyprforge_process::TIMEOUT) {
        Ok(out) if out.status.success() && !out.stdout.is_empty() => Ok(Some(out.stdout)),
        Ok(_) => Ok(None),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(Missing),
        Err(e) => {
            tracing::debug!(error = %e, "preview helper did not finish");
            Ok(None)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_short_text_file_is_shown_whole_and_says_so() {
        let e = excerpt(b"one\ntwo\n").unwrap();
        assert_eq!(e.text, "one\ntwo");
        assert!(!e.truncated);
    }

    /// Past the line budget, or past the byte budget, the pane says the
    /// file goes on rather than implying it ends where the excerpt does.
    #[test]
    fn a_long_text_file_is_cut_and_says_so() {
        let many: String = (0..TEXT_LINES + 5).map(|i| format!("{i}\n")).collect();
        let e = excerpt(many.as_bytes()).unwrap();
        assert_eq!(e.text.lines().count(), TEXT_LINES);
        assert!(e.truncated);

        let big = vec![b'a'; TEXT_BYTES + 1];
        assert!(excerpt(&big).unwrap().truncated, "more bytes than were read");
    }

    /// A NUL means binary, whatever the name said.
    #[test]
    fn a_file_with_a_nul_in_it_is_not_text() {
        assert_eq!(excerpt(b"PK\x03\x04\x00\x00"), None);
    }

    /// Minified code is one enormous line; it is cut, not drawn.
    #[test]
    fn a_very_long_line_is_cut() {
        let line = "x".repeat(LINE_CHARS * 3);
        let e = excerpt(line.as_bytes()).unwrap();
        assert_eq!(e.text.chars().count(), LINE_CHARS + 1, "the cut plus the ellipsis");
    }

    /// A cut through a multi-byte character at the block's end is one
    /// replacement character, not a refusal.
    #[test]
    fn a_character_cut_in_half_at_the_end_does_not_lose_the_file() {
        let mut bytes = "é".repeat(TEXT_BYTES / 2).into_bytes();
        bytes.truncate(TEXT_BYTES - 1);
        assert!(excerpt(&bytes).is_some());
    }

    #[test]
    fn durations_read_the_way_players_show_them() {
        assert_eq!(duration(662.442), "11:02");
        assert_eq!(duration(3723.0), "1:02:03");
        assert_eq!(duration(4.4), "0:04");
    }

    /// The shape `ffprobe` actually printed for a real 4K video on this
    /// machine — including the cover-art flag every stream carries.
    #[test]
    fn a_videos_probe_gives_its_length_and_its_picture_size() {
        let json = br#"{"streams":[
            {"codec_name":"h264","codec_type":"video","width":3840,"height":2160,"disposition":{"attached_pic":0}},
            {"codec_name":"aac","codec_type":"audio","disposition":{"attached_pic":0}}],
            "format":{"duration":"662.442000","tags":{}}}"#;
        let probe = Probe::parse(json);
        assert_eq!(probe.duration, Some(662.442));
        assert_eq!(probe.size, Some((3840, 2160)));
        assert!(!probe.has_cover);
    }

    /// A song's embedded cover is a video stream too; it is the cover,
    /// not a picture size, and tags are found whatever their case.
    #[test]
    fn a_songs_cover_is_a_cover_and_its_tags_are_read_in_any_case() {
        let json = br#"{"streams":[
            {"codec_type":"audio","disposition":{"attached_pic":0}},
            {"codec_type":"video","width":600,"height":600,"disposition":{"attached_pic":1}}],
            "format":{"duration":"201.5","tags":{"TITLE":"Song","artist":"Band"}}}"#;
        let probe = Probe::parse(json);
        assert!(probe.has_cover);
        assert_eq!(probe.size, None);
        assert_eq!(probe.title.as_deref(), Some("Song"));
        assert_eq!(probe.artist.as_deref(), Some("Band"));
    }

    /// Nonsense from the tool is no information, not a crash.
    #[test]
    fn a_probe_that_is_not_json_is_empty() {
        assert_eq!(Probe::parse(b"not json"), Probe::default());
    }

    /// A folder lists folders first, hides dotfiles from the names but
    /// counts them, and says how many there are in all.
    #[test]
    fn a_folder_lists_folders_first_and_counts_what_it_hides() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("b.txt"), "").unwrap();
        std::fs::write(dir.path().join(".hidden"), "").unwrap();
        std::fs::create_dir(dir.path().join("z-sub")).unwrap();
        let preview = folder(dir.path()).unwrap();
        let listing = preview.listing.unwrap();
        assert_eq!(listing.names, [("z-sub".to_string(), true), ("b.txt".to_string(), false)]);
        assert_eq!(listing.total, 2, "what is shown, not counting the dotfile");
        assert!(preview.details.iter().any(|(l, v)| l == "Hidden" && v.starts_with('1')));
    }

    /// No poppler: the pane says what would show more, rather than
    /// nothing at all.
    #[test]
    fn a_missing_tool_is_named_in_the_preview() {
        let (label, text) = install("poppler", "a PDF's first page");
        assert_eq!(label, "Preview");
        assert!(text.contains("poppler"));
    }
}
