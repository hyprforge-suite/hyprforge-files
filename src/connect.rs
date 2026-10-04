//! The Connect to Server dialog, as data: what has been typed, what the
//! server asked for, and what one attempt is sent with.
//!
//! An attempt is one run of `gio mount` (see `hyprforge_volumes::gvfs`).
//! When the server wants a name and password, or asks a question, the
//! attempt stops and the dialog asks the person — then the *next* attempt
//! runs with the answers. So the dialog moves between four phases, and
//! every transition is a function here a test can call without a window.
//!
//! The password lives in a [`Secret`] from the moment it is typed: the
//! window's `Message` carries one, this struct holds one, and the only
//! place it is read out is the line written to `gio`'s standard input.
//! It is kept for one attempt's retry and dropped — zeroed — with the
//! dialog.

use hyprforge_secret::Secret;
use hyprforge_volumes::{gvfs, Answers, ConnectError, Gvfs, Login, Question};
use std::path::PathBuf;

/// Where the dialog is.
#[derive(Debug, Clone, PartialEq)]
pub enum Phase {
    /// Typing an address.
    Address,
    /// An attempt is running.
    Connecting,
    /// The server wants a name and password.
    Login(Login),
    /// The server asked something.
    Question(Question),
}

/// The dialog's state.
#[derive(Debug, Clone)]
pub struct ConnectDialog {
    pub address: String,
    pub user: String,
    pub domain: String,
    pub password: Secret<String>,
    pub phase: Phase,
    /// What went wrong last, in words.
    pub error: Option<String>,
    /// The URI the running or last attempt was for.
    uri: Option<String>,
    /// A choice made for the server's question, carried into every
    /// later attempt of the same connection.
    choice: Option<usize>,
    /// The phase to go back to if an attempt is cancelled.
    before: Option<Phase>,
}

impl Default for ConnectDialog {
    fn default() -> Self {
        ConnectDialog {
            address: String::new(),
            user: String::new(),
            domain: String::new(),
            password: Secret::default(),
            phase: Phase::Address,
            error: None,
            uri: None,
            choice: None,
            before: None,
        }
    }
}

impl ConnectDialog {
    /// The first attempt, from the address typed. `Err` (and the dialog
    /// says why) when it is not an address gvfs can reach.
    pub fn submit(&mut self, gvfs: Option<&Gvfs>) -> Option<(String, Answers)> {
        let schemes = match gvfs {
            Some(Gvfs::Available { schemes }) => schemes.clone(),
            Some(Gvfs::Absent(why)) => {
                self.error = Some(why.clone());
                return None;
            }
            // Not known yet: let gio be the judge of the scheme.
            None => {
                let scheme = self.address.trim().split_once(':').map(|(s, _)| s.to_ascii_lowercase());
                scheme.into_iter().collect()
            }
        };
        match self.phase {
            Phase::Address => match gvfs::address(&self.address, &schemes) {
                Ok(uri) => {
                    self.uri = Some(uri);
                    self.choice = None;
                }
                Err(e) => {
                    self.error = Some(e.to_string());
                    return None;
                }
            },
            // Answering the server: the same address again.
            Phase::Login(_) | Phase::Question(_) => {}
            Phase::Connecting => return None,
        }
        let uri = self.uri.clone()?;
        let answers = self.answers();
        self.before = Some(std::mem::replace(&mut self.phase, Phase::Connecting));
        self.error = None;
        Some((uri, answers))
    }

    /// Picks one of the server's choices and tries again with it.
    pub fn choose(&mut self, choice: usize) -> Option<(String, Answers)> {
        if !matches!(self.phase, Phase::Question(_)) {
            return None;
        }
        self.choice = Some(choice);
        self.submit(None)
    }

    fn answers(&self) -> Answers {
        let asked_login = matches!(self.phase, Phase::Login(_));
        Answers {
            user: (asked_login && !self.user.trim().is_empty()).then(|| self.user.trim().to_string()),
            domain: (asked_login && !self.domain.trim().is_empty()).then(|| self.domain.trim().to_string()),
            password: asked_login.then(|| self.password.clone()),
            choice: self.choice,
        }
    }

    /// An attempt was cancelled: back to where it was started from.
    pub fn cancelled(&mut self) {
        if let Some(before) = self.before.take() {
            self.phase = before;
        }
    }

    /// How an attempt ended. `Some(Ok(path))` closes the dialog — with
    /// somewhere to go when gvfs gave one; `None` keeps it open, asking
    /// for what the server wants or saying what went wrong.
    pub fn ended(&mut self, result: Result<Option<PathBuf>, ConnectError>) -> Option<Option<PathBuf>> {
        self.before = None;
        match result {
            Ok(path) => Some(path),
            Err(ConnectError::NeedsLogin(login)) => {
                if self.user.is_empty() {
                    self.user = login.user.clone().unwrap_or_default();
                }
                if self.domain.is_empty() {
                    self.domain = login.domain.clone().unwrap_or_default();
                }
                if login.retry {
                    // The refused password is not offered back.
                    self.password = Secret::default();
                    self.error = Some("That didn't work — check the name and password.".to_string());
                }
                self.phase = Phase::Login(login);
                None
            }
            Err(ConnectError::Question(question)) => {
                self.phase = Phase::Question(question);
                None
            }
            Err(e) => {
                // Back to the address, which is the thing to change for
                // every other failure — and the server's words with it.
                self.phase = Phase::Address;
                self.password = Secret::default();
                self.error = Some(e.to_string());
                None
            }
        }
    }

    /// Whether the Connect button can be pressed.
    pub fn can_submit(&self) -> bool {
        match &self.phase {
            Phase::Address => !self.address.trim().is_empty(),
            Phase::Login(_) => !self.password.expose().is_empty(),
            Phase::Question(_) | Phase::Connecting => false,
        }
    }
}

/// The hint under the address field: what this machine can reach.
pub fn hint(gvfs: Option<&Gvfs>) -> String {
    match gvfs {
        Some(Gvfs::Available { schemes }) => {
            let offered = gvfs::offered(schemes);
            if offered.is_empty() {
                "gvfs is here, but none of the usual network backends are installed.".to_string()
            } else {
                format!(
                    "This machine can reach {} addresses. For example smb://server/share or sftp://user@host.",
                    offered.iter().map(|s| format!("{s}://")).collect::<Vec<_>>().join(", ")
                )
            }
        }
        Some(Gvfs::Absent(why)) => why.clone(),
        None => "For example smb://server/share or sftp://user@host.".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gvfs() -> Gvfs {
        Gvfs::Available { schemes: vec!["sftp".into(), "smb".into(), "ssh".into()] }
    }

    fn login(retry: bool) -> Login {
        Login { message: "Password required".into(), user: Some("apost".into()), domain: Some("WORKGROUP".into()), asks_domain: true, retry }
    }

    #[test]
    fn a_first_attempt_carries_no_answers_at_all() {
        let mut d = ConnectDialog { address: "smb://nas/music".into(), ..Default::default() };
        let (uri, answers) = d.submit(Some(&gvfs())).unwrap();
        assert_eq!(uri, "smb://nas/music");
        assert_eq!(answers, Answers::default());
        assert_eq!(d.phase, Phase::Connecting);
    }

    #[test]
    fn something_that_is_not_an_address_is_said_and_not_tried() {
        let mut d = ConnectDialog { address: "nas".into(), ..Default::default() };
        assert!(d.submit(Some(&gvfs())).is_none());
        assert!(d.error.as_deref().unwrap().contains("isn't a server address"));
        assert_eq!(d.phase, Phase::Address);
    }

    /// The feature is never hidden: with gvfs missing the dialog opens
    /// and says so.
    #[test]
    fn gvfs_missing_is_said_rather_than_tried() {
        let mut d = ConnectDialog { address: "smb://nas/x".into(), ..Default::default() };
        assert!(d.submit(Some(&Gvfs::Absent("gvfs isn't installed".into()))).is_none());
        assert_eq!(d.error.as_deref(), Some("gvfs isn't installed"));
        assert_eq!(hint(Some(&Gvfs::Absent("gvfs isn't installed".into()))), "gvfs isn't installed");
    }

    #[test]
    fn a_login_request_fills_in_gios_defaults_and_the_next_attempt_answers() {
        let mut d = ConnectDialog { address: "smb://nas/music".into(), ..Default::default() };
        d.submit(Some(&gvfs()));
        assert_eq!(d.ended(Err(ConnectError::NeedsLogin(login(false)))), None);
        assert_eq!(d.user, "apost");
        assert!(!d.can_submit(), "no password typed yet");
        d.password = Secret::new("pw".to_string());
        let (uri, answers) = d.submit(Some(&gvfs())).unwrap();
        assert_eq!(uri, "smb://nas/music");
        assert_eq!(answers.user.as_deref(), Some("apost"));
        assert_eq!(answers.password.as_ref().map(|p| p.expose().as_str()), Some("pw"));
    }

    #[test]
    fn a_refused_password_is_cleared_and_said() {
        let mut d = ConnectDialog { address: "smb://nas/music".into(), password: Secret::new("bad".to_string()), ..Default::default() };
        d.submit(Some(&gvfs()));
        d.ended(Err(ConnectError::NeedsLogin(login(true))));
        assert!(d.password.expose().is_empty());
        assert!(d.error.as_deref().unwrap().contains("didn't work"));
    }

    #[test]
    fn a_question_is_answered_by_choosing_and_the_choice_sticks() {
        let mut d = ConnectDialog { address: "sftp://box".into(), ..Default::default() };
        d.submit(Some(&gvfs()));
        d.ended(Err(ConnectError::Question(Question { message: "Unknown host".into(), choices: vec!["Log In Anyway".into(), "Cancel".into()] })));
        let (_, answers) = d.choose(0).unwrap();
        assert_eq!(answers.choice, Some(0));
        // The host key accepted, the server now wants a password.
        d.ended(Err(ConnectError::NeedsLogin(login(false))));
        d.password = Secret::new("pw".to_string());
        let (_, answers) = d.submit(Some(&gvfs())).unwrap();
        assert_eq!(answers.choice, Some(0), "the question will be asked again, and answered the same");
    }

    #[test]
    fn a_failure_goes_back_to_the_address_with_the_servers_words() {
        let mut d = ConnectDialog { address: "smb://nas/x".into(), ..Default::default() };
        d.submit(Some(&gvfs()));
        d.ended(Err(ConnectError::Failed("Connection refused".into())));
        assert_eq!(d.phase, Phase::Address);
        assert_eq!(d.error.as_deref(), Some("Connection refused"));
    }

    #[test]
    fn cancelling_goes_back_to_where_the_attempt_started() {
        let mut d = ConnectDialog { address: "smb://nas/x".into(), ..Default::default() };
        d.submit(Some(&gvfs()));
        d.cancelled();
        assert_eq!(d.phase, Phase::Address);
        assert!(d.can_submit());
    }

    #[test]
    fn success_closes_with_somewhere_to_go() {
        let mut d = ConnectDialog { address: "smb://nas/x".into(), ..Default::default() };
        d.submit(Some(&gvfs()));
        assert_eq!(d.ended(Ok(Some("/run/user/1/gvfs/x".into()))), Some(Some(PathBuf::from("/run/user/1/gvfs/x"))));
    }

    #[test]
    fn the_hint_names_only_what_this_machine_can_reach() {
        let h = hint(Some(&gvfs()));
        assert!(h.contains("smb://") && h.contains("sftp://"), "{h}");
        assert!(!h.contains("dav://"), "{h}");
    }

    /// The dialog's `Debug` — which a `?dialog` in a log line would use
    /// — carries the password's length, never the password.
    #[test]
    fn the_dialog_never_prints_its_password() {
        let d = ConnectDialog { password: Secret::new("hunter2".to_string()), ..Default::default() };
        assert!(!format!("{d:?}").contains("hunter2"));
    }
}
