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
