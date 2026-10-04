//! Running things outside the window: "Open Terminal Here", and the
//! person's own `[[action]]`s (`hyprforge_files_core::custom`).
//!
//! # Which terminal
//!
//! In this order, the first that answers:
//!
//! 1. `[behaviour] terminal` in `files-config.toml` — a choice the person
//!    made. Used as written even when it cannot be found: quietly
//!    starting something else instead would hide that the choice is
//!    broken, so its failure to start is said instead.
//! 2. `xdg-terminal-exec`, the freedesktop proposal for "the user's
//!    terminal", when it is installed.
//! 3. `$TERMINAL`, the older convention, when what it names is installed.
//! 4. The first installed of [`KNOWN`].
//!
//! None of them is a failure of the window: it is a sentence in the
//! status bar naming what to install or set.
//!
//! # How a terminal is told where to start
//!
//! Every process here is started with the folder as its working
//! directory, which is what most terminals start their shell in. Some
//! do not — a terminal that is already running and opens the new window
//! itself never sees this process's directory — so the ones known to
//! take a flag for it are given that too (`Recipe`). Ghostty is also
//! told not to hand the window to an instance already running, because
//! that instance would open it in *its* directory.
//!
//! # Spawned, never waited on — except to hear how an action ended
//!
//! A terminal can stay open all day, so it is spawned and reaped on a
//! thread of its own — the way `hyprforge-media`'s `launch` reaps what it
//! opens — rather than waited for. A custom action is waited for, on the
//! async runtime, because "it failed" is something the person has to be
//! told; that wait holds nothing up and has no bound on purpose: a
//! conversion of a long video is allowed to take as long as it takes,
//! and killing it at some number would be the bug. Each child gets a
//! process group of its own, so closing the terminal Files was started
//! from does not take the programs it started with it.

use hyprforge_files_core::custom::CustomAction;
use std::ffi::OsString;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};

/// Terminals looked for when nothing says which, in order.
pub const KNOWN: [&str; 8] = ["ghostty", "kitty", "foot", "alacritty", "wezterm", "konsole", "gnome-terminal", "xterm"];

/// Where a terminal came from — for the Preferences sheet's "Automatic
/// (found …)", and for a failure to name whose choice it was.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Configured,
    XdgTerminalExec,
    Env,
    Known,
}

/// A terminal to run: a program and the arguments it was given.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Terminal {
    pub argv: Vec<String>,
    pub source: Source,
}

/// The terminal to use — see the module doc for the order. `env` is
/// `$TERMINAL`; `on_path` says whether a program is installed
/// (`hyprforge_mime::apps::on_path` in the window).
pub fn resolve(configured: Option<&[String]>, env: Option<&str>, on_path: &dyn Fn(&str) -> bool) -> Option<Terminal> {
    if let Some(argv) = configured.filter(|a| !a.is_empty()) {
        return Some(Terminal { argv: argv.to_vec(), source: Source::Configured });
    }
    if on_path("xdg-terminal-exec") {
        return Some(Terminal { argv: vec!["xdg-terminal-exec".to_string()], source: Source::XdgTerminalExec });
    }
    // `$TERMINAL` is conventionally a program name, sometimes with a
    // flag or two after it; it is the environment's, not this file's, so
    // it is split on spaces the way every program reading it does.
    let from_env: Vec<String> = env.unwrap_or_default().split_whitespace().map(str::to_string).collect();
    if from_env.first().is_some_and(|program| on_path(program)) {
        return Some(Terminal { argv: from_env, source: Source::Env });
    }
    KNOWN
        .into_iter()
        .find(|t| on_path(t))
        .map(|t| Terminal { argv: vec![t.to_string()], source: Source::Known })
}

/// [`resolve`] against this machine and `files-config.toml`'s choice.
pub fn find(configured: Option<&[String]>) -> Option<Terminal> {
    let env = std::env::var("TERMINAL").ok();
    resolve(configured, env.as_deref(), &hyprforge_mime::apps::on_path)
}

/// What a known terminal needs told, beyond being started in the folder.
struct Recipe {
    /// A subcommand that has to come first (`wezterm start`), unless the
    /// person's own arguments already have it.
    subcommand: Option<&'static str>,
    /// The flags that say where to start, given the folder.
    dir: fn(&Path) -> Vec<OsString>,
    /// What goes before a command for the terminal to run, instead of
    /// a shell. Empty for terminals that take the command as their
    /// remaining arguments.
    exec: &'static [&'static str],
}

fn joined(flag: &str, dir: &Path) -> OsString {
    let mut out = OsString::from(flag);
    out.push(dir.as_os_str());
    out
}

fn recipe(program: &str) -> Recipe {
    let name = Path::new(program).file_name().and_then(|n| n.to_str()).unwrap_or(program);
    match name {
        "ghostty" => Recipe {
            subcommand: None,
            dir: |d| vec![joined("--working-directory=", d), "--gtk-single-instance=false".into()],
            exec: &["-e"],
        },
        "kitty" => Recipe { subcommand: None, dir: |d| vec!["--directory".into(), d.into()], exec: &[] },
        "foot" => Recipe { subcommand: None, dir: |d| vec![joined("--working-directory=", d)], exec: &[] },
        "alacritty" => {
            Recipe { subcommand: None, dir: |d| vec!["--working-directory".into(), d.into()], exec: &["-e"] }
        }
        "wezterm" => Recipe { subcommand: Some("start"), dir: |d| vec!["--cwd".into(), d.into()], exec: &["--"] },
        "konsole" => Recipe { subcommand: None, dir: |d| vec!["--workdir".into(), d.into()], exec: &["-e"] },
        "gnome-terminal" => Recipe { subcommand: None, dir: |d| vec![joined("--working-directory=", d)], exec: &["--"] },
        // It starts the terminal the person chose, in the directory it
        // was started in, and takes a command as its arguments.
        "xdg-terminal-exec" => Recipe { subcommand: None, dir: |_| Vec::new(), exec: &[] },
        // xterm, and anything this does not know: the working directory
        // alone, and the `-e` nearly every terminal accepts.
        _ => Recipe { subcommand: None, dir: |_| Vec::new(), exec: &["-e"] },
    }
}

impl Terminal {
    /// The program and arguments that open this terminal in `dir`, and
    /// run `command` in it when there is one.
    pub fn command(&self, dir: &Path, command: Option<&[OsString]>) -> (OsString, Vec<OsString>) {
        let (program, given) = self.argv.split_first().expect("a terminal is never an empty list");
        let recipe = recipe(program);
        let mut args: Vec<OsString> = Vec::new();
        if let Some(sub) = recipe.subcommand.filter(|s| !given.iter().any(|g| g == s)) {
            args.push(sub.into());
        }
        args.extend(given.iter().map(OsString::from));
        args.extend((recipe.dir)(dir));
        if let Some(command) = command {
            args.extend(recipe.exec.iter().map(OsString::from));
            args.extend(command.iter().cloned());
        }
        (program.into(), args)
    }

    /// The program's name, for a sentence.
    pub fn name(&self) -> &str {
        self.argv.first().map(String::as_str).unwrap_or_default()
    }
}

/// The sentence for no terminal at all.
pub fn none_found() -> String {
    format!(
        "Couldn't open a terminal: none was found. Install one (Files looks for {}), \
         or choose one in Preferences.",
        KNOWN.join(", ")
    )
}

/// Opens `terminal` in `dir`: spawned with `dir` as its working
/// directory, reaped on a thread of its own, never waited for. `Err` is
/// the sentence for the status bar.
pub fn open_in(terminal: &Terminal, dir: &Path) -> Result<(), String> {
    let (program, args) = terminal.command(dir, None);
    let mut command = std::process::Command::new(&program);
    command
        .args(&args)
        .current_dir(dir)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .process_group(0);
    match command.spawn() {
        Ok(mut child) => {
            std::thread::spawn(move || {
                let _ = child.wait();
            });
            Ok(())
        }
        Err(e) => Err(start_failure(terminal.name(), terminal.source == Source::Configured, &e)),
    }
}

fn start_failure(program: &str, configured: bool, e: &std::io::Error) -> String {
    match (e.kind(), configured) {
        (std::io::ErrorKind::NotFound, true) => format!(
            "Couldn't open a terminal: {program}, the one chosen in Preferences, isn't installed. \
             Install it, or set the terminal back to Automatic."
        ),
        (std::io::ErrorKind::NotFound, false) => format!("Couldn't open a terminal: {program} isn't installed."),
        _ => format!("Couldn't open {program}: {e}"),
    }
}

/// How much of what a failing action printed is kept for the sentence:
/// the end of it, where a program says what went wrong.
const STDERR_KEPT: usize = 2048;

/// Runs a custom action on `paths` from `cwd`, and says how it ended:
/// `Ok` when it exited successfully, or the sentence for the status bar.
///
/// `terminal` is used only when the action asks to run in one, and must
/// then be `Some`. Inside a terminal, how the *command* ended is the
/// terminal's business — this hears only whether the terminal started.
pub async fn run_custom(
    action: CustomAction,
    paths: Vec<PathBuf>,
    cwd: PathBuf,
    terminal: Option<Terminal>,
) -> Result<(), String> {
    let argv = action.argv(&paths);
    let label = &action.label;
    let (program, args) = if action.terminal {
        let Some(terminal) = &terminal else {
            return Err(format!("\u{201c}{label}\u{201d} runs in a terminal, and {}", none_found_lower()));
        };
        terminal.command(&cwd, Some(&argv))
    } else {
        let (program, rest) = argv.split_first().expect("a command is never empty");
        (program.clone(), rest.to_vec())
    };
    let mut command = tokio::process::Command::new(&program);
    command
        .args(&args)
        .current_dir(&cwd)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(if action.terminal { std::process::Stdio::null() } else { std::process::Stdio::piped() })
        .process_group(0);
    let mut child = command.spawn().map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => format!(
            "\u{201c}{label}\u{201d} couldn't start: {} isn't installed, or isn't on your PATH.",
            program.to_string_lossy()
        ),
        _ => format!("\u{201c}{label}\u{201d} couldn't start: {e}"),
    })?;
    let mut said = Vec::new();
    if let Some(mut stderr) = child.stderr.take() {
        use tokio::io::AsyncReadExt;
        let mut buf = [0u8; 4096];
        // Read to the end, so a program that writes a lot is never left
        // blocked on a full pipe, keeping only the tail.
        while let Ok(n) = stderr.read(&mut buf).await {
            if n == 0 {
                break;
            }
            said.extend_from_slice(&buf[..n]);
            if said.len() > STDERR_KEPT {
                said.drain(..said.len() - STDERR_KEPT);
            }
        }
    }
    let status = child.wait().await.map_err(|e| format!("\u{201c}{label}\u{201d} couldn't be waited for: {e}"))?;
    if status.success() {
        return Ok(());
    }
    let how = match status.code() {
        Some(code) => format!("exit {code}"),
        None => "stopped by a signal".to_string(),
    };
    let last = String::from_utf8_lossy(&said)
        .lines()
        .rev()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(|l| format!(": {l}"))
        .unwrap_or_default();
    Err(format!("\u{201c}{label}\u{201d} failed ({how}){last}"))
}

fn none_found_lower() -> String {
    format!("no terminal was found. Install one (Files looks for {}), or choose one in Preferences.", KNOWN.join(", "))
}

#[cfg(test)]
mod tests {
    use super::*;
    use hyprforge_files_core::custom::Selection;

    fn installed(list: &'static [&'static str]) -> impl Fn(&str) -> bool {
        move |p| list.contains(&p)
    }

    fn words(list: &[&str]) -> Vec<String> {
        list.iter().map(|w| w.to_string()).collect()
    }

    #[test]
    fn the_terminal_in_the_config_wins_even_when_it_is_not_installed() {
        let chosen = words(&["wezterm"]);
        let found = resolve(Some(&chosen), Some("kitty"), &installed(&["xdg-terminal-exec", "kitty"])).unwrap();
        assert_eq!(found, Terminal { argv: chosen, source: Source::Configured });
    }

    #[test]
    fn xdg_terminal_exec_comes_next() {
        let found = resolve(None, Some("kitty"), &installed(&["xdg-terminal-exec", "kitty"])).unwrap();
        assert_eq!(found.source, Source::XdgTerminalExec);
    }

    #[test]
    fn then_the_terminal_variable_when_what_it_names_is_installed() {
        let found = resolve(None, Some("kitty -1"), &installed(&["kitty", "ghostty"])).unwrap();
        assert_eq!(found, Terminal { argv: words(&["kitty", "-1"]), source: Source::Env });
        let skipped = resolve(None, Some("urxvt"), &installed(&["foot"])).unwrap();
        assert_eq!(skipped, Terminal { argv: words(&["foot"]), source: Source::Known }, "urxvt isn't there");
    }

    #[test]
    fn then_the_first_known_terminal_that_is_installed() {
        let found = resolve(None, None, &installed(&["xterm", "foot", "ghostty"])).unwrap();
        assert_eq!(found.argv, ["ghostty"], "in the known list's order, not the machine's");
        assert_eq!(resolve(None, Some(""), &installed(&[])), None);
    }

    #[test]
    fn no_terminal_is_a_sentence_naming_what_to_do() {
        let said = none_found();
        assert!(said.contains("ghostty") && said.contains("Preferences"), "{said}");
    }

    fn term(argv: &[&str]) -> Terminal {
        Terminal { argv: words(argv), source: Source::Known }
    }

    #[test]
    fn ghostty_is_told_the_folder_and_to_open_a_window_of_its_own() {
        let (program, args) = term(&["ghostty"]).command(Path::new("/w d"), None);
        assert_eq!(program, "ghostty");
        assert_eq!(args, [OsString::from("--working-directory=/w d"), "--gtk-single-instance=false".into()]);
        let (_, args) = term(&["ghostty"]).command(Path::new("/w"), Some(&["htop".into()]));
        assert_eq!(&args[2..], [OsString::from("-e"), "htop".into()]);
    }

    #[test]
    fn each_known_terminal_runs_a_command_its_own_way() {
        let run = [OsString::from("make"), OsString::from("all")];
        let args = |argv: &[&str]| term(argv).command(Path::new("/w"), Some(&run)).1;
        assert_eq!(args(&["kitty"]), ["--directory", "/w", "make", "all"]);
        assert_eq!(args(&["wezterm"]), ["start", "--cwd", "/w", "--", "make", "all"]);
        assert_eq!(args(&["wezterm", "start", "--always-new-process"])[..2], ["start", "--always-new-process"]);
        assert_eq!(args(&["/usr/bin/xterm"]), ["-e", "make", "all"]);
        assert_eq!(args(&["xdg-terminal-exec"]), ["make", "all"]);
        assert_eq!(args(&["foot", "--app-id=x"]), ["--app-id=x", "--working-directory=/w", "make", "all"]);
    }

    fn action(command: &[&str]) -> CustomAction {
        CustomAction {
            id: "t".into(),
            label: "Test".into(),
            command: words(command),
            types: Vec::new(),
            selection: Selection::Any,
            terminal: false,
        }
    }

    #[tokio::test]
    async fn an_action_that_succeeds_says_nothing() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(run_custom(action(&["true"]), vec![], dir.path().into(), None).await, Ok(()));
    }

    /// The selection arrives as the program's own arguments, exactly —
    /// a name full of shell syntax included, with no shell to read it.
    #[tokio::test]
    async fn a_selection_reaches_the_program_as_its_arguments() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("got");
        let evil = dir.path().join("a; touch pwned $(id).txt");
        let script = format!("printf '%s\\n' \"$@\" > {}", out.display());
        let result = run_custom(
            action(&["sh", "-c", &script, "sh"]),
            vec![evil.clone(), "b c".into()],
            dir.path().into(),
            None,
        )
        .await;
        assert_eq!(result, Ok(()));
        let got = std::fs::read_to_string(&out).unwrap();
        assert_eq!(got, format!("{}\nb c\n", evil.display()));
        assert!(!dir.path().join("pwned").exists());
    }

    #[tokio::test]
    async fn an_action_runs_in_the_folder_in_view() {
        let dir = tempfile::tempdir().unwrap();
        let result = run_custom(action(&["sh", "-c", "pwd > here"]), vec![], dir.path().into(), None).await;
        assert_eq!(result, Ok(()));
        let here = std::fs::read_to_string(dir.path().join("here")).unwrap();
        assert_eq!(Path::new(here.trim()).canonicalize().unwrap(), dir.path().canonicalize().unwrap());
    }

    #[tokio::test]
    async fn a_failing_action_says_how_and_the_last_thing_it_printed() {
        let dir = tempfile::tempdir().unwrap();
        let said = run_custom(
            action(&["sh", "-c", "echo first >&2; echo 'no images defined' >&2; exit 3"]),
            vec![],
            dir.path().into(),
            None,
        )
        .await
        .unwrap_err();
        assert_eq!(said, "\u{201c}Test\u{201d} failed (exit 3): no images defined");
    }

    #[tokio::test]
    async fn an_action_whose_program_is_missing_says_so() {
        let dir = tempfile::tempdir().unwrap();
        let said = run_custom(action(&["no-such-program-xyz"]), vec![], dir.path().into(), None).await.unwrap_err();
        assert!(said.contains("no-such-program-xyz") && said.contains("PATH"), "{said}");
    }

    #[tokio::test]
    async fn an_action_for_a_terminal_with_no_terminal_says_so() {
        let dir = tempfile::tempdir().unwrap();
        let mut in_terminal = action(&["htop"]);
        in_terminal.terminal = true;
        let said = run_custom(in_terminal, vec![], dir.path().into(), None).await.unwrap_err();
        assert!(said.contains("terminal") && said.contains("Preferences"), "{said}");
    }

    #[test]
    fn a_chosen_terminal_that_is_missing_names_the_choice() {
        let e = std::io::Error::from(std::io::ErrorKind::NotFound);
        assert!(start_failure("wezterm", true, &e).contains("chosen in Preferences"));
        assert!(!start_failure("wezterm", false, &e).contains("Preferences"));
    }

    /// Opening is spawning: a terminal that exits at once (`true`
    /// stands in) is not waited for, and a missing one is a sentence.
    #[test]
    fn opening_a_terminal_does_not_wait_for_it() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(open_in(&term(&["true"]), dir.path()), Ok(()));
        let missing = Terminal { argv: words(&["no-such-terminal-xyz"]), source: Source::Configured };
        assert!(open_in(&missing, dir.path()).unwrap_err().contains("isn't installed"));
    }
}
