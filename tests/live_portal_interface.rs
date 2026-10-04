//! The open/save dialog's D-Bus interface against the one this machine's
//! xdg-desktop-portal will call.
//!
//! The frontend calls `OpenFile`, `SaveFile` and `SaveFiles` with the
//! signatures in the interface XML it installs, and a method whose
//! arguments differ by one type is a call that fails at the moment an
//! application asks for a dialog — with the GTK dialog never appearing
//! instead either. So this serves the real interface and asks it, over
//! D-Bus's own introspection, what it declares, and compares that with the
//! installed XML: a claim about somebody else's interface, which is what
//! a live test is for.
//!
//! No real bus is joined: the test starts a `dbus-daemon` of its own and
//! kills it after, so this cannot answer anyone's real dialog request.
//! Gated on the XML and on `dbus-daemon` being installed, because those
//! are all it asks — no portal has to be running.

use hyprforge_files::portal_service::{DialogCommand, FileChooser, OBJECT_PATH};
use std::collections::BTreeMap;
use std::path::Path;

const INSTALLED: &str = "/usr/share/dbus-1/interfaces/org.freedesktop.impl.portal.FileChooser.xml";
const INTERFACE: &str = "org.freedesktop.impl.portal.FileChooser";

/// `method name → (in signature, out signature)` for one interface in an
/// introspection document. Enough of XML for D-Bus's own format, which
/// puts each `<method>` and `<arg>` on its own tag.
fn methods_of(xml: &str, interface: &str) -> BTreeMap<String, (String, String)> {
    let start = xml.find(&format!("<interface name=\"{interface}\"")).expect("the interface is in the document");
    let body = &xml[start..];
    let body = &body[..body.find("</interface>").expect("the interface closes")];
    let attr = |tag: &str, name: &str| -> Option<String> {
        let key = format!("{name}=\"");
        let at = tag.find(&key)? + key.len();
        Some(tag[at..at + tag[at..].find('"')?].to_string())
    };
    let mut methods = BTreeMap::new();
    for chunk in body.split("<method ").skip(1) {
        let name = attr(chunk, "name").expect("a method has a name");
        let chunk = &chunk[..chunk.find("</method>").unwrap_or(chunk.len())];
        let (mut ins, mut outs) = (String::new(), String::new());
        for arg in chunk.split("<arg ").skip(1) {
            let arg = &arg[..arg.find('>').unwrap_or(arg.len())];
            let kind = attr(arg, "type").expect("an arg has a type");
            match attr(arg, "direction").as_deref() {
                Some("out") => outs.push_str(&kind),
                _ => ins.push_str(&kind),
            }
        }
        methods.insert(name, (ins, outs));
    }
    methods
}

#[tokio::test]
#[ignore = "reads the installed xdg-desktop-portal interface; run by check.sh"]
async fn the_service_declares_exactly_the_methods_the_portal_calls() {
    if !Path::new(INSTALLED).exists() {
        eprintln!("HYPRFORGE-SKIP: xdg-desktop-portal's FileChooser interface XML is not installed ({INSTALLED})");
        return;
    }
    if std::process::Command::new("dbus-daemon").arg("--version").output().is_err() {
        eprintln!("HYPRFORGE-SKIP: dbus-daemon is not installed, so there is no private bus to serve on");
        return;
    }
    let wanted = methods_of(&std::fs::read_to_string(INSTALLED).unwrap(), INTERFACE);
    assert!(wanted.contains_key("OpenFile"), "the installed XML was read: {wanted:?}");

    // A bus of this test's own: started here, gone when the test is.
    let mut daemon = std::process::Command::new("dbus-daemon")
        .args(["--session", "--nofork", "--nopidfile", "--print-address=1"])
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("dbus-daemon starts");
    let address = {
        use std::io::BufRead;
        let mut line = String::new();
        std::io::BufReader::new(daemon.stdout.take().unwrap()).read_line(&mut line).unwrap();
        line.trim().to_string()
    };
    struct Kill(std::process::Child);
    impl Drop for Kill {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let _daemon = Kill(daemon);

    // The dialog is never started: introspection calls no method.
    let chooser = FileChooser::new(DialogCommand { program: "/bin/false".into(), args: vec![] });
    let _server = zbus::connection::Builder::address(address.as_str())
        .unwrap()
        .serve_at(OBJECT_PATH, chooser)
        .unwrap()
        .build()
        .await
        .unwrap();
    let client = zbus::connection::Builder::address(address.as_str()).unwrap().build().await.unwrap();
    let server_name = _server.unique_name().expect("a bus connection has a name").to_owned();

    let reply = client
        .call_method(Some(server_name), OBJECT_PATH, Some("org.freedesktop.DBus.Introspectable"), "Introspect", &())
        .await
        .unwrap();
    let declared: String = reply.body().deserialize().unwrap();
    let ours = methods_of(&declared, INTERFACE);
    assert_eq!(ours, wanted, "our FileChooser must match the installed interface method for method");
}

/// Overlapping requests must each get their own dialog, however they
/// interleave — found when the real session's service stopped starting
/// dialogs after a few uses: three requests sat open in it, the newest
/// never having started its dialog.
///
/// A, then B while A's dialog is up; A's dialog answers while B's is
/// still up; then C arrives. C's dialog must start, and all three calls
/// must come back.
#[tokio::test]
#[ignore = "starts a private dbus-daemon; run by check.sh"]
async fn a_request_arriving_after_another_finished_still_gets_its_dialog() {
    if std::process::Command::new("dbus-daemon").arg("--version").output().is_err() {
        eprintln!("HYPRFORGE-SKIP: dbus-daemon is not installed, so there is no private bus to serve on");
        return;
    }
    let mut daemon = std::process::Command::new("dbus-daemon")
        .args(["--session", "--nofork", "--nopidfile", "--print-address=1"])
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("dbus-daemon starts");
    let address = {
        use std::io::BufRead;
        let mut line = String::new();
        std::io::BufReader::new(daemon.stdout.take().unwrap()).read_line(&mut line).unwrap();
        line.trim().to_string()
    };
    struct Kill(std::process::Child);
    impl Drop for Kill {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let _daemon = Kill(daemon);

    // A stand-in dialog that notes it started, waits as long as its
    // title says, and cancels. Run by `sh`, never exec'd — see
    // CLAUDE.md on ETXTBSY.
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("dialog.sh");
    std::fs::write(
        &script,
        "#!/bin/sh\nreq=$(cat)\nname=$(printf '%s' \"$req\" | sed -n 's/.*\"title\":\"\\([a-z]*\\)-\\([0-9]*\\)\".*/\\1/p')\nwait=$(printf '%s' \"$req\" | sed -n 's/.*\"title\":\"\\([a-z]*\\)-\\([0-9]*\\)\".*/\\2/p')\ntouch \"$(dirname \"$0\")/started-$name\"\nsleep \"$wait\"\necho '\"Cancelled\"'\n",
    )
    .unwrap();
    let chooser = FileChooser::new(DialogCommand { program: "/bin/sh".into(), args: vec![script.display().to_string()] });
    let server = zbus::connection::Builder::address(address.as_str())
        .unwrap()
        .serve_at(OBJECT_PATH, chooser)
        .unwrap()
        .build()
        .await
        .unwrap();
    let name = server.unique_name().unwrap().to_owned();
    let client = zbus::connection::Builder::address(address.as_str()).unwrap().build().await.unwrap();

    let call = |handle: &'static str, title: &'static str| {
        let client = client.clone();
        let name = name.clone();
        async move {
            let options: std::collections::HashMap<&str, zbus::zvariant::Value> = std::collections::HashMap::new();
            let handle = zbus::zvariant::ObjectPath::try_from(handle).unwrap();
            client
                .call_method(
                    Some(name),
                    OBJECT_PATH,
                    Some(INTERFACE),
                    "OpenFile",
                    &(handle, "test.app", "", title, options),
                )
                .await
                .map(|_| ())
        }
    };
    let started = |n: &str| dir.path().join(format!("started-{n}")).exists();
    let within = |secs: u64| tokio::time::Duration::from_secs(secs);

    let a = tokio::spawn(call("/org/freedesktop/portal/desktop/request/1_1/a", "a-1"));
    let b = tokio::spawn(call("/org/freedesktop/portal/desktop/request/1_1/b", "b-4"));
    // A answers after a second while B's dialog is still up.
    tokio::time::timeout(within(10), a).await.expect("A's call came back").unwrap().unwrap();
    assert!(started("b"), "B's dialog started while A's was up");

    let c = tokio::spawn(call("/org/freedesktop/portal/desktop/request/1_1/c", "c-1"));
    let deadline = tokio::time::Instant::now() + within(5);
    while !started("c") && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
    }
    assert!(started("c"), "C's dialog never started — the service stopped taking requests");
    tokio::time::timeout(within(10), c).await.expect("C's call came back").unwrap().unwrap();
    tokio::time::timeout(within(10), b).await.expect("B's call came back").unwrap().unwrap();
}
