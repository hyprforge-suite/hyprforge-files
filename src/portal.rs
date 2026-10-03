//! The open/save dialog as the desktop's file chooser: what a request
//! from xdg-desktop-portal asks for, and how the answer goes back.
//!
//! The portal's backend interface is `org.freedesktop.impl.portal.FileChooser`
//! — `OpenFile`, `SaveFile` and `SaveFiles`, each `osssa{sv} → ua{sv}`,
//! read off the GTK backend installed here with `busctl introspect` and
//! checked against the interface XML xdg-desktop-portal installs. Everything
//! in this module is decoding and encoding, with no window and no bus, so
//! every shape the frontend can send is a test.
//!
//! Two things about the wire format are easy to get wrong:
//!
//! - **Paths arrive as bytes.** `current_folder`, `current_file` and
//!   `files` are `ay`, NUL-terminated, because a path need not be UTF-8.
//!   They are decoded as bytes into an `OsString`, never through a
//!   `String`, which would refuse — or worse, mangle — a name a
//!   filesystem holds without complaint.
//! - **The answer is URIs, not paths.** `uris` is `as` of `file://` URIs,
//!   percent-encoded the way Files already encodes them for the
//!   clipboard and for a drag (`clipboard::file_uri`), so a name with a
//!   space or a `#` reaches the application intact.

use hyprforge_files_core::clipboard::file_uri;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::ffi::OsString;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use zbus::zvariant::{OwnedValue, Value};

/// The backend interface this serves.
pub const INTERFACE: &str = "org.freedesktop.impl.portal.FileChooser";

/// Paths as their bytes when the request and the answer cross between
/// the service and the dialog as JSON.
///
/// JSON strings are UTF-8 and a Linux path need not be, so serde's own
/// `PathBuf` refuses one that is not — and the request never reached the
/// dialog at all. Found by sending one: the service answered "failed"
/// in under a second, with no window. An array of byte values carries
/// any path.
mod raw_path {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use std::ffi::OsString;
    use std::os::unix::ffi::{OsStrExt, OsStringExt};
    use std::path::PathBuf;

    fn to_bytes(path: &std::path::Path) -> &[u8] {
        path.as_os_str().as_bytes()
    }

    fn from_bytes(bytes: Vec<u8>) -> PathBuf {
        PathBuf::from(OsString::from_vec(bytes))
    }

    pub mod option {
        use super::*;
        pub fn serialize<S: Serializer>(path: &Option<PathBuf>, s: S) -> Result<S::Ok, S::Error> {
            path.as_deref().map(to_bytes).serialize(s)
        }
        pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<PathBuf>, D::Error> {
            Ok(Option::<Vec<u8>>::deserialize(d)?.map(from_bytes))
        }
    }

    pub mod list {
        use super::*;
        pub fn serialize<S: Serializer>(paths: &[PathBuf], s: S) -> Result<S::Ok, S::Error> {
            paths.iter().map(|p| to_bytes(p)).collect::<Vec<_>>().serialize(s)
        }
        pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<PathBuf>, D::Error> {
            Ok(Vec::<Vec<u8>>::deserialize(d)?.into_iter().map(from_bytes).collect())
        }
    }
}

/// The response codes of `org.freedesktop.impl.portal.Request`.
pub mod response {
    pub const SUCCESS: u32 = 0;
    /// The user dismissed the dialog.
    pub const CANCELLED: u32 = 1;
    /// Anything else — the dialog could not be shown, or failed.
    pub const OTHER: u32 = 2;
}

/// Which of the three methods was called, with what only it takes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Kind {
    Open {
        multiple: bool,
        /// Folders, not files.
        directory: bool,
    },
    Save {
        /// The name to suggest.
        current_name: Option<String>,
        /// The file being saved, if it already exists somewhere.
        #[serde(with = "raw_path::option")]
        current_file: Option<PathBuf>,
    },
    /// Save several files into one folder the user picks.
    SaveFiles {
        #[serde(with = "raw_path::list")]
        files: Vec<PathBuf>,
    },
}

/// One rule of a filter: the portal's `(u, s)`, 0 for a glob, 1 for a
/// MIME type.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Rule {
    Glob(String),
    Mime(String),
}

/// A named filter, `(sa(us))` on the wire.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Filter {
    pub name: String,
    pub rules: Vec<Rule>,
}

impl Filter {
    /// Whether a file called `name`, of MIME type `mime` if one is known,
    /// passes. `is_subclass(child, parent)` is the MIME database's — a
    /// filter for `text/plain` accepts a C source file because
    /// `text/x-csrc` is a kind of `text/plain`, which is how GTK reads
    /// the same filter.
    ///
    /// Globs are case-sensitive, as GTK's are on Linux: `*.png` does not
    /// match `SHOT.PNG`. Being looser than the dialog an application was
    /// tested with would offer it files it did not ask for.
    pub fn accepts(&self, name: &str, mime: Option<&str>, is_subclass: impl Fn(&str, &str) -> bool) -> bool {
        self.rules.iter().any(|rule| match rule {
            Rule::Glob(pattern) => glob_matches(pattern, name),
            Rule::Mime(wanted) => mime.is_some_and(|have| mime_matches(wanted, have, &is_subclass)),
        })
    }
}

/// `type/*` matches any subtype; anything else matches itself and every
/// type the database says is a kind of it.
fn mime_matches(wanted: &str, have: &str, is_subclass: &impl Fn(&str, &str) -> bool) -> bool {
    if let Some(major) = wanted.strip_suffix("/*") {
        return have.split('/').next() == Some(major);
    }
    wanted == have || is_subclass(have, wanted)
}

/// `fnmatch` without flags: `*`, `?` and `[...]` (with `!` or `^` to
/// negate, and ranges). A malformed bracket matches itself literally.
pub fn glob_matches(pattern: &str, name: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let n: Vec<char> = name.chars().collect();
    glob_from(&p, &n)
}

fn glob_from(p: &[char], n: &[char]) -> bool {
    match p.first() {
        None => n.is_empty(),
        Some('*') => (0..=n.len()).any(|skip| glob_from(&p[1..], &n[skip..])),
        Some('?') => !n.is_empty() && glob_from(&p[1..], &n[1..]),
        Some('[') => match (bracket(&p[1..]), n.first()) {
            (Some((set_len, matches)), Some(&c)) => matches(c) && glob_from(&p[1 + set_len..], &n[1..]),
            (Some(_), None) => false,
            (None, _) => n.first() == Some(&'[') && glob_from(&p[1..], &n[1..]),
        },
        Some(&c) => n.first() == Some(&c) && glob_from(&p[1..], &n[1..]),
    }
}

/// A bracket expression after its `[`: how many pattern characters it
/// took including the `]`, and the test. `None` if it never closes.
#[allow(clippy::type_complexity)]
fn bracket(p: &[char]) -> Option<(usize, Box<dyn Fn(char) -> bool + '_>)> {
    let negate = matches!(p.first(), Some('!' | '^'));
    let start = usize::from(negate);
    // A `]` first in the set is a member, not the end.
    let close = p.iter().skip(start + 1).position(|&c| c == ']')? + start + 1;
    let set = &p[start..close];
    let test = move |c: char| {
        let mut i = 0;
        let mut hit = false;
        while i < set.len() {
            if i + 2 < set.len() && set[i + 1] == '-' {
                hit |= (set[i]..=set[i + 2]).contains(&c);
                i += 3;
            } else {
                hit |= set[i] == c;
                i += 1;
            }
        }
        hit != negate
    };
    Some((close + 1, Box::new(test)))
}

/// A combo box the application wants shown — `(ssa(ss)s)`: its id, its
/// label, its options as `(id, label)`, and which is chosen to begin
/// with. No options at all means a checkbox, whose values are `"true"`
/// and `"false"`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Choice {
    pub id: String,
    pub label: String,
    pub options: Vec<(String, String)>,
    pub initial: String,
}

/// Everything one call asked for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Request {
    pub kind: Kind,
    /// The application asking — what its last folder is remembered under.
    pub app_id: String,
    pub title: String,
    /// The accept button's label, with GTK's `_` mnemonic marker removed.
    pub accept_label: Option<String>,
    pub filters: Vec<Filter>,
    pub current_filter: Option<Filter>,
    pub choices: Vec<Choice>,
    #[serde(with = "raw_path::option")]
    pub current_folder: Option<PathBuf>,
}

/// Why a request could not be read. A sentence, returned as response 2.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BadRequest(pub String);

impl Request {
    /// Reads one call's arguments. `method` is the D-Bus member name.
    pub fn decode(
        method: &str,
        app_id: &str,
        title: &str,
        options: &HashMap<String, OwnedValue>,
    ) -> Result<Request, BadRequest> {
        let get = |key: &str| options.get(key).map(|v| Value::from(v.try_clone().expect("an owned value without fds")));
        let flag = |key: &str| -> Result<bool, BadRequest> {
            match get(key) {
                None => Ok(false),
                Some(v) => bool::try_from(v).map_err(|_| BadRequest(format!("{key} is not a boolean"))),
            }
        };
        let text = |key: &str| -> Result<Option<String>, BadRequest> {
            match get(key) {
                None => Ok(None),
                Some(v) => String::try_from(v).map(Some).map_err(|_| BadRequest(format!("{key} is not a string"))),
            }
        };
        let path = |key: &str| -> Result<Option<PathBuf>, BadRequest> {
            match get(key) {
                None => Ok(None),
                Some(v) => bytes_of(v).map(|b| Some(path_from_bytes(b))).ok_or_else(|| BadRequest(format!("{key} is not a byte path"))),
            }
        };
        let kind = match method {
            "OpenFile" => Kind::Open { multiple: flag("multiple")?, directory: flag("directory")? },
            "SaveFile" => Kind::Save { current_name: text("current_name")?, current_file: path("current_file")? },
            "SaveFiles" => {
                let files = match get("files") {
                    None => Vec::new(),
                    Some(v) => {
                        let list: Vec<Value> = Vec::try_from(v).map_err(|_| BadRequest("files is not a list".to_string()))?;
                        list.into_iter()
                            .map(|v| bytes_of(v).map(path_from_bytes).ok_or_else(|| BadRequest("a file in files is not a byte path".to_string())))
                            .collect::<Result<_, _>>()?
                    }
                };
                Kind::SaveFiles { files }
            }
            other => return Err(BadRequest(format!("{other} is not a FileChooser method"))),
        };
        let filters = match get("filters") {
            None => Vec::new(),
            Some(v) => {
                let raw: Vec<(String, Vec<(u32, String)>)> =
                    v.try_into().map_err(|_| BadRequest("filters is not a(sa(us))".to_string()))?;
                raw.into_iter().map(filter_from).collect()
            }
        };
        let current_filter = match get("current_filter") {
            None => None,
            Some(v) => {
                let raw: (String, Vec<(u32, String)>) =
                    v.try_into().map_err(|_| BadRequest("current_filter is not (sa(us))".to_string()))?;
                Some(filter_from(raw))
            }
        };
        let choices = match get("choices") {
            None => Vec::new(),
            Some(v) => {
                let raw: Vec<WireChoice> =
                    v.try_into().map_err(|_| BadRequest("choices is not a(ssa(ss)s)".to_string()))?;
                raw.into_iter()
                    .map(|(id, label, options, initial)| Choice { id, label, options, initial })
                    .collect()
            }
        };
        Ok(Request {
            kind,
            app_id: app_id.to_string(),
            title: title.to_string(),
            accept_label: text("accept_label")?.map(|l| strip_mnemonic(&l)),
            filters,
            current_filter,
            choices,
            current_folder: path("current_folder")?,
        })
    }

    /// Where the dialog opens, best first: the folder asked for, the
    /// folder of the file being saved, the folder this application's
    /// dialog was last left in, then home.
    pub fn starting_folder(&self, remembered: Option<&Path>, home: &Path) -> PathBuf {
        let of_file = match &self.kind {
            Kind::Save { current_file: Some(file), .. } => file.parent().map(Path::to_path_buf),
            _ => None,
        };
        self.current_folder
            .clone()
            .or(of_file)
            .or_else(|| remembered.map(Path::to_path_buf))
            .unwrap_or_else(|| home.to_path_buf())
    }

    /// The name a Save dialog suggests: the one asked for, or the name of
    /// the file being saved.
    pub fn suggested_name(&self) -> Option<String> {
        match &self.kind {
            Kind::Save { current_name: Some(name), .. } => Some(name.clone()),
            Kind::Save { current_file: Some(file), .. } => file.file_name().map(|n| n.to_string_lossy().into_owned()),
            _ => None,
        }
    }
}

/// A choice as it arrives: `(ssa(ss)s)`.
type WireChoice = (String, String, Vec<(String, String)>, String);

fn filter_from((name, rules): (String, Vec<(u32, String)>)) -> Filter {
    Filter {
        name,
        rules: rules
            .into_iter()
            .filter_map(|(kind, text)| match kind {
                0 => Some(Rule::Glob(text)),
                1 => Some(Rule::Mime(text)),
                // A rule kind this version of the spec does not know is
                // left out rather than guessed at.
                _ => None,
            })
            .collect(),
    }
}

/// An `ay` value's bytes.
fn bytes_of(value: Value<'_>) -> Option<Vec<u8>> {
    Vec::<u8>::try_from(value).ok()
}

/// A NUL-terminated byte path, as the portal sends it, without the NUL.
pub fn path_from_bytes(mut bytes: Vec<u8>) -> PathBuf {
    while bytes.last() == Some(&0) {
        bytes.pop();
    }
    PathBuf::from(OsString::from_vec(bytes))
}

/// GTK's mnemonic marker out of a label: `_Open` reads "Open", `__`
/// reads "_".
fn strip_mnemonic(label: &str) -> String {
    let mut out = String::with_capacity(label.len());
    let mut chars = label.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '_' {
            if chars.peek() == Some(&'_') {
                out.push('_');
                chars.next();
            }
            continue;
        }
        out.push(c);
    }
    out
}

/// What pressing the accept button — or Enter — does, given what is on
/// screen. Pure: whether a path exists is asked through `exists`, which
/// answers `Some(is_dir)` or `None`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Accept {
    /// Done: answer with these.
    Answer(Vec<PathBuf>),
    /// Go into this folder instead — a folder chosen in a dialog that
    /// wants files is somewhere to look, not an answer.
    Enter(PathBuf),
    /// Saving over this file needs a yes first.
    Overwrite(PathBuf),
    /// Say why not, and stay open.
    Refuse(String),
    /// Nothing chosen yet; do nothing.
    Nothing,
}

/// What is on screen when accept is pressed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OnScreen<'a> {
    /// The folder in view.
    pub folder: &'a Path,
    /// Whether it is a real folder something can be saved into or
    /// answered with — not a place inside an archive, nor the Trash.
    pub real: bool,
    /// The selection, as `(path, is_dir)`, focused one first.
    pub selected: &'a [(PathBuf, bool)],
    /// The Save dialog's name field.
    pub name: &'a str,
}

/// Decides what accept does — see [`Accept`].
pub fn accept(kind: &Kind, screen: &OnScreen<'_>, exists: impl Fn(&Path) -> Option<bool>) -> Accept {
    let folders: Vec<&PathBuf> = screen.selected.iter().filter(|(_, d)| *d).map(|(p, _)| p).collect();
    let files: Vec<&PathBuf> = screen.selected.iter().filter(|(_, d)| !*d).map(|(p, _)| p).collect();
    let not_real = || Accept::Refuse("Choose a folder on this computer — not a place inside an archive or the Trash.".to_string());
    match kind {
        Kind::Open { directory: true, .. } => {
            if !screen.real {
                return not_real();
            }
            // The one folder selected, or — nothing selected — the folder
            // in view, which is what "Open" on an open folder means.
            match folders.as_slice() {
                [one] if files.is_empty() => Accept::Answer(vec![(*one).clone()]),
                _ => Accept::Answer(vec![screen.folder.to_path_buf()]),
            }
        }
        Kind::Open { multiple, .. } => {
            if files.is_empty() {
                return match folders.first() {
                    Some(folder) => Accept::Enter((*folder).clone()),
                    None => Accept::Nothing,
                };
            }
            // A file inside an archive has no path the application could
            // open; it would have to be unpacked first, and an open dialog
            // that silently unpacks is not one anyone asked for.
            if !screen.real {
                return Accept::Refuse("Files inside an archive can't be opened from here — extract them first.".to_string());
            }
            let chosen: Vec<PathBuf> = if *multiple { files.into_iter().cloned().collect() } else { vec![files[0].clone()] };
            Accept::Answer(chosen)
        }
        Kind::Save { .. } => {
            let name = screen.name.trim();
            if name.is_empty() {
                return Accept::Nothing;
            }
            if !screen.real {
                return not_real();
            }
            // A name with a slash is a path from here, the way GTK takes
            // `sub/report.pdf`; an absolute one is taken as it is.
            let target = screen.folder.join(name);
            match exists(&target) {
                Some(true) => Accept::Enter(target),
                Some(false) => Accept::Overwrite(target),
                None => Accept::Answer(vec![target]),
            }
        }
        Kind::SaveFiles { files } => {
            if !screen.real {
                return not_real();
            }
            let into = match folders.as_slice() {
                [one] => (*one).clone(),
                _ => screen.folder.to_path_buf(),
            };
            let targets: Vec<PathBuf> =
                files.iter().filter_map(|f| f.file_name()).map(|n| into.join(n)).collect();
            match targets.iter().find(|t| exists(t).is_some()) {
                Some(clash) => Accept::Overwrite(clash.clone()),
                None => Accept::Answer(targets),
            }
        }
    }
}

/// How a dialog ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Answer {
    Chosen {
        #[serde(with = "raw_path::list")]
        paths: Vec<PathBuf>,
        /// `(choice id, option id)` for every choice shown.
        choices: Vec<(String, String)>,
        filter: Option<Filter>,
    },
    Cancelled,
    /// The dialog could not do what was asked. A sentence for the log.
    Failed(String),
}

impl Answer {
    /// The `(response, results)` pair the method returns.
    pub fn encode(&self) -> (u32, HashMap<String, OwnedValue>) {
        let mut results = HashMap::new();
        let code = match self {
            Answer::Chosen { paths, choices, filter } => {
                let uris: Vec<String> = paths.iter().map(|p| file_uri(p)).collect();
                results.insert("uris".to_string(), owned(Value::from(uris)));
                if !choices.is_empty() {
                    results.insert("choices".to_string(), owned(Value::from(choices.clone())));
                }
                if let Some(filter) = filter {
                    let rules: Vec<(u32, String)> = filter
                        .rules
                        .iter()
                        .map(|r| match r {
                            Rule::Glob(g) => (0, g.clone()),
                            Rule::Mime(m) => (1, m.clone()),
                        })
                        .collect();
                    results.insert("current_filter".to_string(), owned(Value::from((filter.name.clone(), rules))));
                }
                response::SUCCESS
            }
            Answer::Cancelled => response::CANCELLED,
            Answer::Failed(_) => response::OTHER,
        };
        (code, results)
    }
}

fn owned(value: Value<'_>) -> OwnedValue {
    value.try_to_owned().expect("a value with no file descriptors")
}

/// A path as the portal's `ay`: its bytes and a trailing NUL. For tests
/// that build a request the way the frontend would.
pub fn path_to_bytes(path: &Path) -> Vec<u8> {
    let mut bytes = path.as_os_str().as_bytes().to_vec();
    bytes.push(0);
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts(pairs: Vec<(&str, Value<'static>)>) -> HashMap<String, OwnedValue> {
        pairs.into_iter().map(|(k, v)| (k.to_string(), owned(v))).collect()
    }

    #[test]
    fn an_open_request_reads_every_option_the_spec_gives_it() {
        let filters = vec![
            ("Images".to_string(), vec![(0u32, "*.png".to_string()), (1u32, "image/jpeg".to_string())]),
            ("All".to_string(), vec![(0u32, "*".to_string())]),
        ];
        let choices = vec![(
            "encoding".to_string(),
            "Encoding".to_string(),
            vec![("utf8".to_string(), "UTF-8".to_string())],
            "utf8".to_string(),
        )];
        let options = opts(vec![
            ("multiple", Value::from(true)),
            ("directory", Value::from(false)),
            ("accept_label", Value::from("_Open")),
            ("filters", Value::from(filters)),
            ("current_filter", Value::from(("All".to_string(), vec![(0u32, "*".to_string())]))),
            ("choices", Value::from(choices)),
            ("current_folder", Value::from(path_to_bytes(Path::new("/home/a/Pictures")))),
        ]);
        let request = Request::decode("OpenFile", "org.gnome.Loupe", "Open Image", &options).unwrap();
        assert_eq!(request.kind, Kind::Open { multiple: true, directory: false });
        assert_eq!(request.accept_label.as_deref(), Some("Open"));
        assert_eq!(request.filters.len(), 2);
        assert_eq!(request.filters[0].rules, vec![Rule::Glob("*.png".into()), Rule::Mime("image/jpeg".into())]);
        assert_eq!(request.current_filter.as_ref().map(|f| f.name.as_str()), Some("All"));
        assert_eq!(request.choices[0].initial, "utf8");
        assert_eq!(request.current_folder, Some(PathBuf::from("/home/a/Pictures")));
    }

    /// The options as they really arrive: serialised into a D-Bus
    /// message and read back out of it, the way the service receives
    /// them — not values built by hand, which is how a decoding that read
    /// the wrong bytes passed every other test here.
    fn over_the_wire(pairs: Vec<(&str, Value<'static>)>) -> HashMap<String, OwnedValue> {
        let sent: HashMap<String, Value<'static>> = pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect();
        let message = zbus::message::Message::method_call("/org/freedesktop/portal/desktop", "OpenFile")
            .unwrap()
            .destination("org.freedesktop.impl.portal.desktop.hyprforge")
            .unwrap()
            .interface(INTERFACE)
            .unwrap()
            .build(&(sent,))
            .unwrap();
        let (received,): (HashMap<String, OwnedValue>,) = message.body().deserialize().unwrap();
        received
    }

    #[test]
    fn a_folder_sent_over_dbus_is_the_folder_that_was_sent() {
        let options = over_the_wire(vec![("current_folder", Value::from(path_to_bytes(Path::new("/home/a/Pictures"))))]);
        let request = Request::decode("OpenFile", "x", "t", &options).unwrap();
        assert_eq!(request.current_folder, Some(PathBuf::from("/home/a/Pictures")));
    }

    #[test]
    fn a_folder_sent_beside_filters_is_still_the_folder_that_was_sent() {
        let filters = vec![("Images".to_string(), vec![(1u32, "image/*".to_string())])];
        let options = over_the_wire(vec![
            ("current_folder", Value::from(path_to_bytes(Path::new("/home/a/Pictures")))),
            ("filters", Value::from(filters)),
        ]);
        let request = Request::decode("OpenFile", "x", "t", &options).unwrap();
        assert_eq!(request.current_folder, Some(PathBuf::from("/home/a/Pictures")));
        assert_eq!(request.filters.len(), 1);
    }

    #[test]
    fn a_folder_that_is_not_utf8_arrives_intact() {
        // `\xff` is a valid byte in a Linux file name and invalid UTF-8.
        let raw = b"/tmp/caf\xff\0".to_vec();
        let options = opts(vec![("current_folder", Value::from(raw))]);
        let request = Request::decode("OpenFile", "x", "t", &options).unwrap();
        assert_eq!(request.current_folder.unwrap().as_os_str().as_bytes(), b"/tmp/caf\xff");
    }

    #[test]
    fn a_path_that_is_not_utf8_survives_the_trip_to_the_dialog_and_back() {
        let odd = PathBuf::from(OsString::from_vec(b"/tmp/caf\xff".to_vec()));
        let mut request = Request::decode("OpenFile", "x", "t", &HashMap::new()).unwrap();
        request.current_folder = Some(odd.clone());
        let json = serde_json::to_string(&request).expect("a path that is not UTF-8 still serialises");
        assert_eq!(serde_json::from_str::<Request>(&json).unwrap().current_folder, Some(odd.clone()));
        let answer = Answer::Chosen { paths: vec![odd.join("x")], choices: vec![], filter: None };
        let back: Answer = serde_json::from_str(&serde_json::to_string(&answer).unwrap()).unwrap();
        assert_eq!(back, answer);
    }

    #[test]
    fn save_files_reads_its_names_in_order() {
        let files: Vec<Vec<u8>> = vec![path_to_bytes(Path::new("a.txt")), path_to_bytes(Path::new("b.txt"))];
        let options = opts(vec![("files", Value::from(files))]);
        let request = Request::decode("SaveFiles", "x", "t", &options).unwrap();
        assert_eq!(request.kind, Kind::SaveFiles { files: vec![PathBuf::from("a.txt"), PathBuf::from("b.txt")] });
    }

    #[test]
    fn an_option_of_the_wrong_type_is_refused_not_ignored() {
        let options = opts(vec![("multiple", Value::from("yes"))]);
        assert!(Request::decode("OpenFile", "x", "t", &options).is_err());
        assert!(Request::decode("DeleteFile", "x", "t", &HashMap::new()).is_err());
    }

    #[test]
    fn the_dialog_opens_where_it_was_asked_then_by_the_file_then_where_it_was_left() {
        let home = Path::new("/home/a");
        let left = Path::new("/home/a/last");
        let mut request = Request::decode("SaveFile", "x", "t", &HashMap::new()).unwrap();
        assert_eq!(request.starting_folder(None, home), home);
        assert_eq!(request.starting_folder(Some(left), home), left);
        request.kind = Kind::Save { current_name: None, current_file: Some(PathBuf::from("/srv/doc.odt")) };
        assert_eq!(request.starting_folder(Some(left), home), Path::new("/srv"));
        assert_eq!(request.suggested_name().as_deref(), Some("doc.odt"));
        request.current_folder = Some(PathBuf::from("/asked"));
        assert_eq!(request.starting_folder(Some(left), home), Path::new("/asked"));
    }

    #[test]
    fn globs_match_the_way_fnmatch_does_and_are_case_sensitive() {
        assert!(glob_matches("*.png", "shot.png"));
        assert!(!glob_matches("*.png", "SHOT.PNG"), "case-sensitive, as GTK on Linux");
        assert!(glob_matches("IMG_????.jpg", "IMG_0042.jpg"));
        assert!(glob_matches("*.[ch]", "main.c"));
        assert!(glob_matches("*.[!o]", "main.c"));
        assert!(!glob_matches("*.[!o]", "main.o"));
        assert!(glob_matches("report[0-9].txt", "report7.txt"));
        assert!(glob_matches("a[b", "a[b"), "an unclosed bracket is literal");
        assert!(glob_matches("*", ""));
    }

    #[test]
    fn a_mime_filter_takes_subtypes_and_kinds_of_the_type() {
        let filter = Filter { name: "Text".into(), rules: vec![Rule::Mime("text/plain".into())] };
        let db = |child: &str, parent: &str| child == "text/x-csrc" && parent == "text/plain";
        assert!(filter.accepts("main.c", Some("text/x-csrc"), db));
        assert!(!filter.accepts("shot.png", Some("image/png"), db));
        assert!(!filter.accepts("unknown", None, db), "no type known is no match for a type rule");
        let images = Filter { name: "Images".into(), rules: vec![Rule::Mime("image/*".into())] };
        assert!(images.accepts("x.webp", Some("image/webp"), |_, _| false));
    }

    #[test]
    fn an_answer_is_file_uris_that_survive_spaces_and_hashes() {
        let answer = Answer::Chosen { paths: vec![PathBuf::from("/home/a/my notes #1.txt")], choices: vec![], filter: None };
        let (code, results) = answer.encode();
        assert_eq!(code, response::SUCCESS);
        let uris: Vec<String> = Value::from(results["uris"].try_clone().unwrap()).try_into().unwrap();
        assert_eq!(uris, vec![file_uri(Path::new("/home/a/my notes #1.txt"))]);
        assert!(uris[0].starts_with("file:///") && !uris[0].contains(' ') && !uris[0].contains('#'));
    }

    #[test]
    fn cancelling_answers_one_and_nothing_else() {
        let (code, results) = Answer::Cancelled.encode();
        assert_eq!(code, response::CANCELLED);
        assert!(results.is_empty());
    }

    fn screen<'a>(folder: &'a Path, selected: &'a [(PathBuf, bool)], name: &'a str) -> OnScreen<'a> {
        OnScreen { folder, real: true, selected, name }
    }

    #[test]
    fn opening_with_a_folder_selected_goes_into_it() {
        let selected = [(PathBuf::from("/d/sub"), true)];
        let kind = Kind::Open { multiple: false, directory: false };
        assert_eq!(accept(&kind, &screen(Path::new("/d"), &selected, ""), |_| None), Accept::Enter(PathBuf::from("/d/sub")));
    }

    #[test]
    fn opening_one_file_answers_the_first_even_with_more_selected() {
        let selected = [(PathBuf::from("/d/a"), false), (PathBuf::from("/d/b"), false)];
        let one = Kind::Open { multiple: false, directory: false };
        let many = Kind::Open { multiple: true, directory: false };
        assert_eq!(accept(&one, &screen(Path::new("/d"), &selected, ""), |_| None), Accept::Answer(vec![PathBuf::from("/d/a")]));
        assert_eq!(accept(&many, &screen(Path::new("/d"), &selected, ""), |_| None).clone(), Accept::Answer(vec![PathBuf::from("/d/a"), PathBuf::from("/d/b")]));
    }

    #[test]
    fn choosing_a_folder_with_none_selected_chooses_the_one_in_view() {
        let kind = Kind::Open { multiple: false, directory: true };
        assert_eq!(accept(&kind, &screen(Path::new("/d"), &[], ""), |_| None), Accept::Answer(vec![PathBuf::from("/d")]));
    }

    #[test]
    fn saving_over_a_file_asks_and_saving_onto_a_folder_goes_in() {
        let kind = Kind::Save { current_name: None, current_file: None };
        let exists = |p: &Path| match p.to_str() {
            Some("/d/taken.txt") => Some(false),
            Some("/d/sub") => Some(true),
            _ => None,
        };
        assert_eq!(accept(&kind, &screen(Path::new("/d"), &[], "taken.txt"), exists), Accept::Overwrite(PathBuf::from("/d/taken.txt")));
        assert_eq!(accept(&kind, &screen(Path::new("/d"), &[], "sub"), exists), Accept::Enter(PathBuf::from("/d/sub")));
        assert_eq!(accept(&kind, &screen(Path::new("/d"), &[], "new.txt"), exists), Accept::Answer(vec![PathBuf::from("/d/new.txt")]));
        assert_eq!(accept(&kind, &screen(Path::new("/d"), &[], "  "), exists), Accept::Nothing);
    }

    #[test]
    fn nothing_is_saved_into_an_archive_or_the_trash() {
        let kind = Kind::Save { current_name: None, current_file: None };
        let mut on = screen(Path::new("/d/x.zip"), &[], "a.txt");
        on.real = false;
        assert!(matches!(accept(&kind, &on, |_| None), Accept::Refuse(_)));
    }

    #[test]
    fn saving_several_files_lands_them_in_the_folder_in_order() {
        let kind = Kind::SaveFiles { files: vec![PathBuf::from("a.txt"), PathBuf::from("b.txt")] };
        assert_eq!(
            accept(&kind, &screen(Path::new("/d"), &[], ""), |_| None),
            Accept::Answer(vec![PathBuf::from("/d/a.txt"), PathBuf::from("/d/b.txt")])
        );
    }

    #[test]
    fn mnemonics_are_taken_out_of_the_accept_label() {
        assert_eq!(strip_mnemonic("_Save"), "Save");
        assert_eq!(strip_mnemonic("Save __As"), "Save _As");
    }
}
