//! A paste into a folder of an elevated pane, carried out by the
//! administrator helper — see [`crate::admin`].
//!
//! It reports the same [`JobEvent`]s an ordinary paste does
//! ([`crate::jobs`]), so the transfers popover, its progress bar and its
//! cancel button work on it without knowing who did the copying. Two
//! things differ, both because the work happens in another process:
//!
//! - **A conflict is asked about before an item is sent, not during.**
//!   The helper is handed what to do about collisions with the request;
//!   it cannot stop and ask. So each item's destination is looked at
//!   first, and when something is there and the answer is "ask", the
//!   question goes up before the copy starts. Whatever is decided for
//!   the item also decides any collision deeper inside a folder it
//!   merges into.
//! - **Nothing is remembered for Undo.** Undo runs as this user, who
//!   cannot take back what root put down, and an Undo that fails is
//!   worse than none offered.

use crate::admin::{Body, OnCollision, Op, WirePath};
use crate::admin_client::AdminBackend;
use crate::jobs::{Items, JobControl, JobEvent, JobId, JobSummary};
use hyprforge_files_core::clipboard::PasteStep;
use hyprforge_files_core::config::OnConflict;
use hyprforge_fileops::{Collision, CollisionDecision, CollisionPolicy, OpKind, Progress};
use iced::futures::channel::mpsc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// Starts `steps` through the helper on a worker thread.
pub fn start(
    job: JobId,
    steps: Vec<PasteStep>,
    on_conflict: OnConflict,
    admin: Arc<AdminBackend>,
) -> (JobControl, mpsc::UnboundedReceiver<JobEvent>) {
    let (events, receiver) = mpsc::unbounded();
    let (decisions, answers) = std::sync::mpsc::channel();
    let cancel = Arc::new(AtomicBool::new(false));
    let control = JobControl::new(decisions, cancel.clone());
    std::thread::Builder::new()
        .name(format!("files-admin-job-{job}"))
        .spawn(move || {
            let summary = run(job, steps, on_conflict, &admin, &answers, &cancel, &events);
            let _ = events.unbounded_send(JobEvent::Finished { job, summary });
        })
        .expect("spawning a thread only fails when the process is out of resources");
    (control, receiver)
}

/// What the helper is told to do about a collision, for a decision made
/// here. `None` for Cancel, which is not sent at all.
fn on_collision(policy: CollisionPolicy) -> Option<OnCollision> {
    match policy {
        CollisionPolicy::Skip => Some(OnCollision::Skip),
        CollisionPolicy::Replace => Some(OnCollision::Replace),
        CollisionPolicy::KeepBoth => Some(OnCollision::KeepBoth),
        CollisionPolicy::Cancel => None,
    }
}

fn run(
    job: JobId,
    steps: Vec<PasteStep>,
    on_conflict: OnConflict,
    admin: &AdminBackend,
    answers: &std::sync::mpsc::Receiver<CollisionDecision>,
    cancel: &AtomicBool,
    events: &mpsc::UnboundedSender<JobEvent>,
) -> JobSummary {
    let mut summary = JobSummary::default();
    let mut for_the_rest: Option<CollisionPolicy> = match on_conflict {
        OnConflict::Ask => None,
        OnConflict::KeepBoth => Some(CollisionPolicy::KeepBoth),
        OnConflict::Skip => Some(CollisionPolicy::Skip),
        OnConflict::Replace => Some(CollisionPolicy::Replace),
    };
    let of = steps.len();
    for (done, step) in steps.into_iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            summary.cancelled = true;
            break;
        }
        // Is something already there? Asked through the helper too: a
        // destination this user cannot list is the usual reason to be
        // here at all.
        let taken = matches!(
            admin.ask(Op::Stat { path: WirePath::from(step.dest.as_path()) }, |_, _| {}),
            Ok(Body::Entry { .. })
        );
        let policy = if step.duplicate {
            CollisionPolicy::KeepBoth
        } else if !taken {
            // Nothing there now; should something appear meanwhile, it is
            // left alone rather than replaced unasked.
            CollisionPolicy::Skip
        } else if let Some(policy) = for_the_rest {
            policy
        } else {
            let collision = Collision { source: step.source.clone(), dest: step.dest.clone() };
            let _ = events.unbounded_send(JobEvent::Collision { job, collision });
            match answers.recv() {
                Ok(decision) => {
                    if decision.apply_to_rest {
                        for_the_rest = Some(decision.policy);
                    }
                    decision.policy
                }
                Err(_) => CollisionPolicy::Cancel,
            }
        };
        let Some(on_collision) = on_collision(policy) else {
            summary.cancelled = true;
            break;
        };
        let (from, to) = (WirePath::from(step.source.as_path()), WirePath::from(step.dest.as_path()));
        let op = match step.kind {
            OpKind::Copy => Op::Copy { from, to, on_collision },
            OpKind::Move => Op::Move { from, to, on_collision },
        };
        let current = step.source.clone();
        let answer = admin.ask_cancellable(
            op,
            |bytes_done, bytes_total| {
                let progress =
                    Progress { bytes_done, bytes_total, entries_done: 0, entries_total: None, current: current.clone() };
                let _ = events.unbounded_send(JobEvent::Progress { job, progress, items: Items { done, of } });
            },
            Some(cancel),
        );
        match answer {
            Ok(Body::Report { report }) => {
                summary.failed.extend(report.failed.into_iter().map(|(_, why)| why));
                if let Some(why) = report.source_removal_failed {
                    summary.failed.push(why);
                }
                if report.cancelled {
                    summary.cancelled = true;
                    break;
                }
                if report.succeeded > 0 {
                    summary.done += 1;
                } else if report.skipped > 0 {
                    summary.skipped += 1;
                }
            }
            Ok(Body::Refused { why } | Body::Failed { why, .. }) => summary.failed.push(why),
            Ok(other) => summary.failed.push(format!("The administrator helper gave an answer that doesn't fit: {other:?}")),
            // No helper, no password, or it went away: nothing after
            // this item would fare any better.
            Err(e) => {
                summary.failed.push(e.to_string());
                break;
            }
        }
    }
    summary
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cancel_answer_sends_nothing_to_the_helper() {
        assert_eq!(on_collision(CollisionPolicy::Cancel), None);
        assert_eq!(on_collision(CollisionPolicy::KeepBoth), Some(OnCollision::KeepBoth));
    }
}
