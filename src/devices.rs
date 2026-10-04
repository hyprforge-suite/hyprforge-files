//! The host's half of drives and network shares: listening to UDisks2
//! and gvfs, keeping the window's one [`Devices`], and carrying out what
//! the browser asks — see `hyprforge_files_core::devices` for the half
//! every tab draws.
//!
//! Both hosts, the Files window and the open/save dialog, keep one
//! [`DeviceHost`] and run [`watch`] as a subscription. The dialog's says
//! `manages_mounts: false` and its menus leave drives alone, so in a
//! file chooser a drive can be opened (mounted on a click — a stick you
//! cannot open is one you cannot save to) but not ejected or
//! disconnected from.
//!
//! # Every signal is "look again"
//!
//! UDisks2 and gvfs announce changes as signals, and plugging in one
//! stick sends dozens. [`watch`] never acts per signal: it waits for a
//! burst to settle (`hyprforge_volumes::settle`) and then lists once.
//! The listing itself is bounded inside `hyprforge-volumes`, so a disk
//! service that stops answering costs a timeout and a sentence, never a
//! frozen sidebar.

use hyprforge_files_core::devices::{Ask, Devices};
use hyprforge_volumes::backend::{ShareBackend, VolumeBackend};
use hyprforge_volumes::settle::{settle, CEILING, QUIET};
use hyprforge_volumes::{sentence, Gvfs, Operation, Share, Volume, VolumeError, VolumeId};
use iced::futures::{SinkExt, Stream};
use std::collections::HashMap;
use std::future::Future;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

/// How long to wait before asking again for a bus that was not there.
/// Long enough not to spin; short enough that starting UDisks2 shows in
/// the sidebar without restarting the window.
const RETRY: Duration = Duration::from_secs(15);

/// The two services a host talks to. Cloned into the subscription.
#[derive(Clone)]
pub struct Backends {
    pub volumes: Arc<dyn VolumeBackend>,
    pub shares: Arc<dyn ShareBackend>,
}

impl Backends {
    /// UDisks2 on the system bus and gvfs through `gio`. Connects on
    /// first use; constructing never fails.
    pub fn system() -> Self {
        Backends {
            volumes: Arc::new(hyprforge_volumes::UDisks2Backend::new()),
            shares: Arc::new(hyprforge_volumes::SystemShares::new()),
        }
    }
}

/// One watch per window: the subscription is keyed on nothing that
/// changes, so iced keeps the one it started.
impl Hash for Backends {
    fn hash<H: Hasher>(&self, state: &mut H) {
        "hyprforge-files devices".hash(state);
    }
}

/// What the watch heard.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    Volumes(Result<Vec<Volume>, VolumeError>),
    Shares(Vec<Share>),
    Gvfs(Gvfs),
}

/// What a piece of work ended in.
#[derive(Debug, Clone, PartialEq)]
pub enum Done {
    Volume { id: VolumeId, op: Operation, result: Result<Option<PathBuf>, VolumeError> },
    Disconnected { path: PathBuf, result: Result<(), String> },
}

/// The work an [`Ask`] became, for the host to run off the UI thread.
pub type Work = Pin<Box<dyn Future<Output = Done> + Send>>;

/// What the host should do now that some work is done.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Finished {
    /// A sentence for the status line — a failure in words.
    pub status: Option<String>,
    /// Go here, in the tab with this id — a drive clicked while it was
    /// not mounted, now mounted.
    pub open: Option<(u64, PathBuf)>,
    /// A mount point that went away: a tab standing inside it should
    /// leave, because what it is showing is no longer there.
    pub left: Option<PathBuf>,
}

/// The window's drives and shares, and what is happening to them.
pub struct DeviceHost {
    pub backends: Backends,
    state: Devices,
    /// The copy the tabs hold, rebuilt only when something changed.
    shared: Arc<Devices>,
    /// Which tab asked to go to a drive once it is mounted.
    opens: HashMap<VolumeId, u64>,
    gvfs: Option<Gvfs>,
    /// Mount points that went away since [`Self::take_gone`] was last
    /// asked — a stick pulled out, a share unmounted by another program.
    gone: Vec<PathBuf>,
}

impl DeviceHost {
    pub fn new(backends: Backends, manages_mounts: bool) -> Self {
        let state = Devices { manages_mounts, ..Devices::default() };
        DeviceHost { backends, shared: Arc::new(state.clone()), state, opens: HashMap::new(), gvfs: None, gone: Vec::new() }
    }

    /// What every tab should be showing.
    pub fn snapshot(&self) -> Arc<Devices> {
        self.shared.clone()
    }

    pub fn devices(&self) -> &Devices {
        &self.state
    }

    /// Whether gvfs is here, once the watch has said.
    pub fn gvfs(&self) -> Option<&Gvfs> {
        self.gvfs.as_ref()
    }

    /// `true` when the tabs need the new [`Self::snapshot`].
    fn publish(&mut self) -> bool {
        if *self.shared == self.state {
            return false;
        }
        self.shared = Arc::new(self.state.clone());
        true
    }

    /// Takes in what the watch heard. `true` when the tabs need telling.
    pub fn heard(&mut self, event: Event) -> bool {
        match event {
            Event::Volumes(result) => {
                // Only a list replacing a list says something went: a
                // UDisks2 that stopped answering has not unmounted
                // anything.
                if let Ok(new) = &result {
                    let before = self.state.volumes().iter().filter_map(|v| v.mount_point.clone());
                    let still: Vec<&PathBuf> = new.iter().filter_map(|v| v.mount_point.as_ref()).collect();
                    let went: Vec<PathBuf> = before.filter(|p| !still.contains(&p)).collect();
                    self.gone.extend(went);
                }
                self.state.listed(result)
            }
            Event::Shares(shares) => {
                self.state.disconnecting.retain(|p| shares.iter().any(|s| &s.path == p));
                let went: Vec<PathBuf> = self
                    .state
                    .shares
                    .iter()
                    .filter(|old| !shares.iter().any(|s| s.path == old.path))
                    .map(|s| s.path.clone())
                    .collect();
                self.gone.extend(went);
                self.state.shares = shares;
            }
            Event::Gvfs(gvfs) => {
                self.gvfs = Some(gvfs);
                return false;
            }
        }
        self.publish()
    }

    /// Mount points that have gone since this was last asked: a tab
    /// standing in one is showing files that are not there any more.
    pub fn take_gone(&mut self) -> Vec<PathBuf> {
        std::mem::take(&mut self.gone)
    }

    /// Starts what `ask` asks, from the tab `tab`. `None` when there is
    /// nothing to start — the drive is already busy, or gone.
    pub fn ask(&mut self, ask: Ask, tab: u64) -> Option<Work> {
        let volumes = self.backends.volumes.clone();
        let work: Work = match ask {
            Ask::Mount { id, open } => {
                if !self.state.begin(&id, Operation::Mount) {
                    return None;
                }
                if open {
                    self.opens.insert(id.clone(), tab);
                }
                Box::pin(async move {
                    let result = volumes.mount(&id).await.map(Some);
                    Done::Volume { id, op: Operation::Mount, result }
                })
            }
            Ask::Unmount(id) => {
                if !self.state.begin(&id, Operation::Unmount) {
                    return None;
                }
                Box::pin(async move {
                    let result = volumes.unmount(&id).await.map(|()| None);
                    Done::Volume { id, op: Operation::Unmount, result }
                })
            }
            Ask::Eject(id) => {
                if !self.state.begin(&id, Operation::Eject) {
                    return None;
                }
                Box::pin(async move {
                    let result = volumes.eject(&id).await.map(|()| None);
                    Done::Volume { id, op: Operation::Eject, result }
                })
            }
            Ask::Disconnect(path) => {
                let share = self.state.shares.iter().find(|s| s.path == path)?.clone();
                if !self.state.disconnecting.insert(path.clone()) {
                    return None;
                }
                let shares = self.backends.shares.clone();
                Box::pin(async move {
                    let result = shares.disconnect(&share).await;
                    Done::Disconnected { path, result }
                })
            }
        };
        self.publish();
        Some(work)
    }

    /// Takes in how some work ended.
    pub fn done(&mut self, done: Done) -> Finished {
        let finished = match done {
            Done::Volume { id, op, result } => {
                let before = self.state.volume(&id).cloned();
                let label = before.as_ref().map_or_else(|| "the drive".to_string(), |v| v.label.clone());
                let opener = self.opens.remove(&id);
                self.state.finish(&id, op, &result);
                match (&result, op) {
                    (Err(e), _) => Finished { status: Some(sentence(op, &label, e)), ..Finished::default() },
                    (Ok(Some(point)), Operation::Mount) => {
                        Finished { open: opener.map(|tab| (tab, point.clone())), ..Finished::default() }
                    }
                    (Ok(_), Operation::Unmount | Operation::Eject) => Finished {
                        left: before.and_then(|v| v.mount_point),
                        // Ejecting is the one that ends with a person
                        // pulling something out, so it is the one that
                        // says it is safe to.
                        status: (op == Operation::Eject).then(|| format!("\u{201C}{label}\u{201D} can be removed.")),
                        ..Finished::default()
                    },
                    (Ok(_), _) => Finished::default(),
                }
            }
            Done::Disconnected { path, result } => {
                self.state.disconnecting.remove(&path);
                let label = self.state.shares.iter().find(|s| s.path == path).map(|s| s.label.clone());
                match result {
                    Ok(()) => {
                        self.state.shares.retain(|s| s.path != path);
                        Finished { left: Some(path), ..Finished::default() }
                    }
                    Err(why) => Finished {
                        status: Some(format!(
                            "Couldn't disconnect \u{201C}{}\u{201D}: {why}",
                            label.unwrap_or_else(|| path.display().to_string())
                        )),
                        ..Finished::default()
                    },
                }
            }
        };
        self.publish();
        finished
    }
}

/// The subscription: gvfs and the shares first, then the drives, then
/// again after every settled burst of signals from either.
pub fn watch(backends: &Backends) -> impl Stream<Item = Event> + use<> {
    let backends = backends.clone();
    iced::stream::channel(8, async move |mut out: iced::futures::channel::mpsc::Sender<Event>| {
        let Backends { volumes, shares } = backends;
        let _ = out.send(Event::Gvfs(shares.gvfs().await)).await;
        let _ = out.send(Event::Shares(shares.shares().await)).await;
        let mut share_signals = Some(shares.watch().await);
        loop {
            let mut volume_signals = match volumes.watch().await {
                Ok(rx) => Some(rx),
                Err(e) => {
                    let _ = out.send(Event::Volumes(Err(e))).await;
                    None
                }
            };
            if volume_signals.is_some() {
                let _ = out.send(Event::Volumes(volumes.volumes().await)).await;
            }
            let retry = tokio::time::sleep(RETRY);
            tokio::pin!(retry);
            loop {
                // Whichever settles first, both are looked at again: a
                // burst the other branch was part-way through is covered
                // by this look, and its tail starts a new one.
                // A watch that ends sets its receiver to `None` and goes
                // round again: the shares' stays quiet from then on, the
                // drives' arms the retry below.
                tokio::select! {
                    alive = settled(&mut volume_signals) => if !alive { continue },
                    alive = settled(&mut share_signals) => if !alive { continue },
                    // No bus, or the watch ended: try again in a while,
                    // without leaving the shares unwatched meanwhile.
                    () = &mut retry, if volume_signals.is_none() => break,
                }
                if volume_signals.is_some() {
                    let _ = out.send(Event::Volumes(volumes.volumes().await)).await;
                }
                let _ = out.send(Event::Shares(shares.shares().await)).await;
            }
        }
    })
}

/// Waits for `signals` to settle. `false` when they have ended for good
/// — and then the channel is dropped, so the next call waits forever
/// rather than spinning on a closed one.
async fn settled(signals: &mut Option<tokio::sync::mpsc::Receiver<()>>) -> bool {
    let Some(rx) = signals else { return std::future::pending().await };
    if settle(rx, QUIET, CEILING).await {
        true
    } else {
        *signals = None;
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hyprforge_volumes::backend::mock::{MockShares, MockVolumes};
    use hyprforge_volumes::{Detach, ShareKind, VolumeKind};

    fn stick(label: &str, mounted: Option<&str>) -> Volume {
        Volume {
            id: VolumeId(format!("/b/{label}")),
            device: PathBuf::from(format!("/dev/{label}")),
            label: label.to_string(),
            size: 1,
            filesystem: Some("vfat".into()),
            mount_point: mounted.map(PathBuf::from),
            kind: VolumeKind::Removable,
            detach: Some(Detach::PowerOff),
            locked: false,
        }
    }

    fn host(volumes: Vec<Volume>) -> (DeviceHost, Arc<MockVolumes>) {
        let mock = Arc::new(MockVolumes::new(volumes.clone()));
        let backends = Backends {
            volumes: mock.clone(),
            shares: Arc::new(MockShares::new(Gvfs::Available { schemes: vec!["smb".into()] }, vec![])),
        };
        let mut host = DeviceHost::new(backends, true);
        host.heard(Event::Volumes(Ok(volumes)));
        (host, mock)
    }

    #[tokio::test]
    async fn a_drive_clicked_while_unmounted_opens_in_the_tab_that_clicked_once_mounted() {
        let (mut host, _) = host(vec![stick("A", None)]);
        let work = host.ask(Ask::Mount { id: VolumeId("/b/A".into()), open: true }, 7).unwrap();
        assert_eq!(host.snapshot().busy.get(&VolumeId("/b/A".into())), Some(&Operation::Mount), "every tab sees it");
        let finished = host.done(work.await);
        assert_eq!(finished.open, Some((7, PathBuf::from("/run/media/mock/A"))));
        assert!(host.snapshot().busy.is_empty());
        assert!(host.snapshot().volume(&VolumeId("/b/A".into())).unwrap().is_mounted());
    }

    #[tokio::test]
    async fn a_second_ask_while_the_first_runs_starts_nothing() {
        let (mut host, mock) = host(vec![stick("A", None)]);
        let first = host.ask(Ask::Mount { id: VolumeId("/b/A".into()), open: true }, 1);
        assert!(first.is_some());
        assert!(host.ask(Ask::Mount { id: VolumeId("/b/A".into()), open: true }, 2).is_none());
        host.done(first.unwrap().await);
        assert_eq!(mock.calls().len(), 1);
    }

    /// The failure that most needs words: something still has a file open
    /// on the stick.
    #[tokio::test]
    async fn a_busy_unmount_is_a_sentence_and_the_drive_stays_mounted() {
        let (mut host, mock) = host(vec![stick("A", Some("/m/A"))]);
        let id = VolumeId("/b/A".into());
        mock.fail(Operation::Unmount, &id, VolumeError::Busy("target is busy".into()));
        let work = host.ask(Ask::Unmount(id.clone()), 1).unwrap();
        let finished = host.done(work.await);
        assert!(finished.status.unwrap().contains("in use"));
        assert_eq!(finished.left, None);
        assert!(host.devices().volume(&id).unwrap().is_mounted());
    }

    #[tokio::test]
    async fn an_eject_says_it_can_be_removed_and_moves_tabs_off_it() {
        let (mut host, _) = host(vec![stick("A", Some("/m/A"))]);
        let work = host.ask(Ask::Eject(VolumeId("/b/A".into())), 1).unwrap();
        let finished = host.done(work.await);
        assert_eq!(finished.left, Some(PathBuf::from("/m/A")));
        assert!(finished.status.unwrap().contains("can be removed"));
        assert!(host.devices().volumes().is_empty());
    }

    #[tokio::test]
    async fn a_policy_refusal_is_said_as_one() {
        let (mut host, mock) = host(vec![stick("A", None)]);
        let id = VolumeId("/b/A".into());
        mock.fail(Operation::Mount, &id, VolumeError::NotAuthorized("no".into()));
        let work = host.ask(Ask::Mount { id, open: true }, 1).unwrap();
        let finished = host.done(work.await);
        assert!(finished.status.unwrap().contains("aren't allowed"));
        assert_eq!(finished.open, None, "nothing to go to");
    }

    /// A stick pulled out, or unmounted by something else, takes the
    /// tabs standing on it with it — and a UDisks2 that stopped
    /// answering does not, because nothing was unmounted.
    #[test]
    fn a_mount_point_that_went_is_reported_once() {
        let (mut host, _) = host(vec![stick("A", Some("/m/A")), stick("B", Some("/m/B"))]);
        host.heard(Event::Volumes(Err(VolumeError::Unavailable)));
        assert!(host.take_gone().is_empty());
        host.heard(Event::Volumes(Ok(vec![stick("A", Some("/m/A")), stick("B", Some("/m/B"))])));
        host.heard(Event::Volumes(Ok(vec![stick("B", Some("/m/B"))])));
        assert_eq!(host.take_gone(), [PathBuf::from("/m/A")]);
        assert!(host.take_gone().is_empty());
    }

    #[test]
    fn hearing_the_same_list_twice_tells_nobody() {
        let (mut host, _) = host(vec![stick("A", None)]);
        assert!(!host.heard(Event::Volumes(Ok(vec![stick("A", None)]))));
        assert!(host.heard(Event::Volumes(Ok(vec![]))));
        assert!(host.heard(Event::Volumes(Err(VolumeError::Unavailable))));
    }

    #[tokio::test]
    async fn a_disconnected_share_leaves_and_its_tabs_go_with_it() {
        let share = Share { label: "x".into(), path: "/g/x".into(), kind: ShareKind::Gvfs { scheme: "smb".into() } };
        let backends = Backends {
            volumes: Arc::new(MockVolumes::new(vec![])),
            shares: Arc::new(MockShares::new(Gvfs::Available { schemes: vec![] }, vec![share.clone()])),
        };
        let mut host = DeviceHost::new(backends, true);
        host.heard(Event::Shares(vec![share]));
        let work = host.ask(Ask::Disconnect("/g/x".into()), 1).unwrap();
        assert!(host.snapshot().disconnecting.contains(&PathBuf::from("/g/x")));
        let finished = host.done(work.await);
        assert_eq!(finished.left, Some(PathBuf::from("/g/x")));
        assert!(host.snapshot().shares.is_empty());
    }

    /// UDisks2 coming back clears the sentence without a restart: the
    /// watch hears it start, and lists again.
    #[tokio::test]
    async fn udisks_starting_later_is_heard_without_a_restart() {
        use iced::futures::StreamExt;
        let mock = Arc::new(MockVolumes::new(vec![stick("A", None)]));
        mock.set_unavailable(true);
        let backends = Backends {
            volumes: mock.clone(),
            shares: Arc::new(MockShares::new(Gvfs::Absent("no".into()), vec![])),
        };
        let mut events = Box::pin(watch(&backends));
        assert!(matches!(events.next().await, Some(Event::Gvfs(_))));
        assert!(matches!(events.next().await, Some(Event::Shares(_))));
        assert!(matches!(events.next().await, Some(Event::Volumes(Err(VolumeError::Unavailable)))));
        mock.set_unavailable(false);
        let next = tokio::time::timeout(Duration::from_secs(5), events.next()).await.unwrap();
        assert!(matches!(&next, Some(Event::Volumes(Ok(v))) if v.len() == 1), "{next:?}");
    }
}
