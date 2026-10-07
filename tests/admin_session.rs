//! The window's end of the administrator helper against the real
//! helper binary, run unprivileged — no pkexec, so nothing here asks
//! for a password or runs as root.

use hyprforge_files::admin::{Body, OnCollision, Op, WirePath};
use hyprforge_files::admin_client::{AdminError, Session};
use std::process::Command;

fn helper() -> Session {
    Session::spawn(Command::new(env!("CARGO_BIN_EXE_hyprforge-files-admin"))).expect("the helper starts")
}

#[test]
fn a_session_lists_a_folder_and_copies_into_it_through_the_real_helper() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), "hello").unwrap();
    let mut session = helper();
    let to = dir.path().join("b.txt");
    let copied = session
        .ask(
            Op::Copy {
                from: WirePath::from(dir.path().join("a.txt").as_path()),
                to: WirePath::from(to.as_path()),
                on_collision: OnCollision::Skip,
            },
            |_, _| {},
        )
        .unwrap();
    assert!(matches!(copied, Body::Report { ref report } if report.succeeded == 1), "{copied:?}");
    let listed = session.ask(Op::ReadDir { path: WirePath::from(dir.path()) }, |_, _| {}).unwrap();
    let Body::Entries { entries } = listed else { panic!("{listed:?}") };
    let mut names: Vec<_> = entries.iter().map(|e| e.name.clone()).collect();
    names.sort();
    assert_eq!(names, ["a.txt", "b.txt"]);
}

#[test]
fn a_refusal_comes_back_as_an_answer_not_a_lost_session() {
    let mut session = helper();
    let refused = session.ask(Op::Delete { path: WirePath::Text("/usr".into()) }, |_, _| {}).unwrap();
    assert!(matches!(refused, Body::Refused { .. }), "{refused:?}");
    assert!(!session.is_lost(), "a refusal leaves the session usable");
}

/// pkexec's exit status 126 is a dismissed prompt — someone pressed
/// Cancel — and has to read as that, not as a crash.
#[test]
fn a_dismissed_password_prompt_is_its_own_outcome() {
    let mut session = Session::spawn({
        let mut c = Command::new("sh");
        c.args(["-c", "exit 126"]);
        c
    })
    .unwrap();
    let got = session.ask(Op::ReadDir { path: WirePath::Text("/".into()) }, |_, _| {});
    assert_eq!(got, Err(AdminError::Declined));
    assert!(session.is_lost());
    assert_eq!(session.ask(Op::ReadDir { path: WirePath::Text("/".into()) }, |_, _| {}), Err(AdminError::Declined));
}
