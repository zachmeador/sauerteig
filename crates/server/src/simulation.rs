//! The only owner of Session: a dedicated fixed-rate thread. No network or
//! persistence task can borrow it or hold up its clock with a lock.
use crate::{
    edits::{CommitError, EditPlan, PreparedEdit},
    metrics::Metrics,
    outbound::{Events, Packet},
    persistence::SavedSnapshot,
    Retired, Session, SAVE_DELAY,
};
use sauerteig_core::{ClientMessage, Player, ServerMessage};
use serde::Serialize;
use std::{
    io::Write,
    sync::{mpsc, Arc},
    thread,
    time::{Duration, Instant},
};
use tokio::sync::{mpsc as tokio_mpsc, oneshot, watch};

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum State {
    Running,
    Stopping,
    Failed(String),
}
#[derive(Clone, Serialize)]
pub(crate) struct Health {
    pub ok: bool,
    pub players: usize,
    pub tick: u64,
    pub revision: u64,
    pub timing: crate::metrics::Report,
    pub error: Option<String>,
}
pub(crate) struct Welcome {
    pub id: u64,
    pub saved: SavedSnapshot,
    pub players: Vec<Player>,
    pub editing_allowed: bool,
}
impl Welcome {
    pub fn expand(self) -> ServerMessage {
        let saved = self.saved.expand();
        ServerMessage::Welcome {
            id: self.id,
            revision: saved.revision,
            format_version: saved.format_version,
            bounds: saved.bounds,
            regions: saved.regions,
            cells: saved.cells,
            roots: saved.roots,
            objects: saved.objects,
            volumes: saved.volumes,
            players: self.players,
            editing_allowed: self.editing_allowed,
        }
    }
}
pub(crate) enum Commit {
    Done {
        edit: Box<PreparedEdit>,
        result: Result<(), String>,
    },
    Wait(Box<PreparedEdit>),
}
pub(crate) enum Command {
    Join {
        events: Events,
        snapshots: watch::Sender<Option<Arc<Packet>>>,
        reply: oneshot::Sender<Result<(u64, Welcome), String>>,
    },
    Leave(u64),
    Control {
        id: u64,
        message: ClientMessage,
        reply: oneshot::Sender<()>,
    },
    Plan {
        id: u64,
        message: ClientMessage,
        reply: oneshot::Sender<Result<EditPlan, String>>,
    },
    Commit {
        id: u64,
        edit: Box<PreparedEdit>,
        reply: oneshot::Sender<Commit>,
    },
    Report {
        id: u64,
        request: Option<u64>,
        result: Result<(), String>,
        reply: oneshot::Sender<()>,
    },
    Save {
        force: bool,
        reply: oneshot::Sender<Option<SavedSnapshot>>,
    },
    Saved {
        revision: u64,
    },
    Stop {
        reply: oneshot::Sender<SavedSnapshot>,
    },
    #[cfg(test)]
    Panic,
    /// Leaves `leave` bytes free in a peer's queue behind filler its writer
    /// cannot finish encoding until the returned sender is used or dropped.
    #[cfg(test)]
    Stall {
        id: u64,
        leave: usize,
        reply: oneshot::Sender<Option<mpsc::Sender<()>>>,
    },
    /// Makes a peer's writer fail, as if its client had gone.
    #[cfg(test)]
    Poison {
        id: u64,
        reply: oneshot::Sender<bool>,
    },
}
#[derive(Clone)]
pub(crate) struct Handle {
    sender: mpsc::Sender<Command>,
}
impl Handle {
    pub fn send(&self, command: Command) -> Result<(), String> {
        self.sender.send(command).map_err(|_| {
            "The simulation stopped unexpectedly; the last saved world is intact.".into()
        })
    }
    pub async fn request<T>(
        &self,
        build: impl FnOnce(oneshot::Sender<T>) -> Command,
    ) -> Result<T, String> {
        let (tx, rx) = oneshot::channel();
        self.send(build(tx))?;
        rx.await.map_err(|_| {
            "The simulation stopped unexpectedly; the last saved world is intact.".into()
        })
    }
}
/// `retired` carries history that leaves with its builder to the edit worker,
/// which frees it off this thread.
pub(crate) fn start(
    mut session: Session,
    state: watch::Sender<State>,
    retired: tokio_mpsc::UnboundedSender<Retired>,
) -> Result<(Handle, watch::Receiver<Health>, thread::JoinHandle<()>), String> {
    let (sender, receiver) = mpsc::channel();
    let metrics = Metrics::default();
    let (published, health) = watch::channel(Health {
        ok: true,
        players: 0,
        tick: session.tick,
        revision: session.revision,
        timing: metrics.report(),
        error: None,
    });
    let thread = thread::Builder::new()
        .name("sauerteig-simulation".into())
        .spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                run(&mut session, &receiver, &published, &retired)
            }));
            if let Err(panic) = result {
                let detail = panic.downcast_ref::<&str>().copied()
                    .or_else(|| panic.downcast_ref::<String>().map(String::as_str))
                    .unwrap_or("unknown panic");
                let error = format!("The simulation failed: {detail}. The last saved world is intact; reopen it to continue.");
                fail(&published, &state, error, &mut std::io::stderr());
            }
        }).map_err(|e| e.to_string())?;
    Ok((Handle { sender }, health, thread))
}
/// Publishes a failed simulation, then logs it. The order matters: a log that
/// cannot be written must not leave the room looking healthy, so the state that
/// connections, health, and the embedded host watch changes first.
fn fail(
    published: &watch::Sender<Health>,
    state: &watch::Sender<State>,
    error: String,
    log: &mut impl Write,
) {
    published.send_modify(|h| {
        h.ok = false;
        h.error = Some(error.clone());
    });
    state.send_replace(State::Failed(error.clone()));
    let _ = writeln!(log, "{error}");
}
/// Hands history that left with a builder to the edit worker, so freeing it
/// never costs the simulation a tick.
fn hand_off(session: &mut Session, retired: &tokio_mpsc::UnboundedSender<Retired>) {
    if !session.retired.is_empty() {
        let _ = retired.send(std::mem::take(&mut session.retired));
    }
}
fn run(
    session: &mut Session,
    commands: &mpsc::Receiver<Command>,
    published: &watch::Sender<Health>,
    retired: &tokio_mpsc::UnboundedSender<Retired>,
) {
    let period = Duration::from_secs_f64(1.0 / 60.0);
    let mut next = Instant::now() + period;
    let mut metrics = Metrics::default();
    let mut busy = Duration::ZERO;
    let mut dominant = (Duration::ZERO, "idle");
    loop {
        let now = Instant::now();
        if now >= next {
            let started = Instant::now();
            metrics
                .lateness
                .record(started.saturating_duration_since(next));
            session.step();
            session.reap();
            hand_off(session, retired);
            let elapsed = started.elapsed();
            metrics.record("physics", elapsed);
            if elapsed > dominant.0 {
                dominant = (elapsed, "physics");
            }
            metrics.tick(busy + elapsed, dominant.1);
            dominant = (Duration::ZERO, "idle");
            busy = Duration::ZERO;
            next += period;
            // Do not run a spiral of catch-up steps after an OS scheduling pause.
            if Instant::now() >= next + period {
                next = Instant::now() + period;
            }
            let timing = if session.tick.is_multiple_of(60) {
                metrics.report()
            } else {
                published.borrow().timing.clone()
            };
            published.send_replace(Health {
                ok: true,
                players: session.peers.len(),
                tick: session.tick,
                revision: session.revision,
                timing,
                error: None,
            });
            continue;
        }
        let command = match commands.recv_timeout(next.saturating_duration_since(now)) {
            Ok(c) => c,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(_) => break,
        };
        let started = Instant::now();
        let cause = match command {
            Command::Join {
                events,
                snapshots,
                reply,
            } => {
                let _ = reply.send(session.join(events, snapshots).map_err(str::to_string));
                "join_snapshot"
            }
            Command::Leave(id) => {
                session.remove_peer(id);
                "leave"
            }
            Command::Control { id, message, reply } => {
                if let Err(message) = session.handle_control(id, message) {
                    session.send(id, ServerMessage::Error { message });
                }
                let _ = reply.send(());
                "control"
            }
            Command::Plan { id, message, reply } => {
                let _ = reply.send(session.edit_plan(id, message));
                "edit_view"
            }
            Command::Commit {
                id,
                mut edit,
                reply,
            } => {
                let outcome = match session.commit_prepared(id, &mut edit) {
                    Err(CommitError::Busy) => Commit::Wait(edit),
                    result => {
                        for (phase, duration) in ["edit_checks", "edit_apply", "edit_publish"]
                            .into_iter()
                            .zip(edit.timings)
                        {
                            metrics.record(phase, duration);
                        }
                        Commit::Done {
                            result: result.map_err(CommitError::message),
                            edit,
                        }
                    }
                };
                let _ = reply.send(outcome);
                "edit"
            }
            Command::Report {
                id,
                request,
                result,
                reply,
            } => {
                if let Some(request) = request {
                    session.send(
                        id,
                        ServerMessage::EditResult {
                            request,
                            error: result.err(),
                        },
                    );
                } else if let Err(message) = result {
                    session.send(id, ServerMessage::Error { message });
                }
                let _ = reply.send(());
                "reply"
            }
            Command::Save { force, reply } => {
                let snapshot = (force
                    || (session.dirty && session.last_edit.elapsed() >= SAVE_DELAY))
                    .then(|| session.saved_snapshot());
                let _ = reply.send(snapshot);
                "save_snapshot"
            }
            Command::Saved { revision } => {
                if session.revision == revision {
                    session.dirty = false;
                }
                "save_finished"
            }
            Command::Stop { reply } => {
                let _ = reply.send(session.saved_snapshot());
                break;
            }
            #[cfg(test)]
            Command::Panic => panic!("injected simulation panic"),
            #[cfg(test)]
            Command::Stall { id, leave, reply } => {
                let _ = reply.send(session.stall_peer(id, leave));
                "stall"
            }
            #[cfg(test)]
            Command::Poison { id, reply } => {
                let _ = reply.send(session.poison_peer(id));
                "poison"
            }
        };
        hand_off(session, retired);
        let elapsed = started.elapsed();
        busy += elapsed;
        if elapsed > dominant.0 {
            dominant = (elapsed, cause);
        }
        metrics.record(cause, elapsed);
    }
}
pub(crate) fn is_edit(message: &ClientMessage) -> bool {
    matches!(
        message,
        ClientMessage::Edit { .. }
            | ClientMessage::EditRoots { .. }
            | ClientMessage::EditTransaction { .. }
            | ClientMessage::PlaceObject { .. }
            | ClientMessage::RemoveObject { .. }
            | ClientMessage::Undo
            | ClientMessage::Redo
    )
}
pub(crate) fn request_id(message: &ClientMessage) -> Option<u64> {
    match message {
        ClientMessage::Edit { request, .. }
        | ClientMessage::EditRoots { request, .. }
        | ClientMessage::EditTransaction { request, .. } => Some(*request),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The worst log: writing to it panics, as `eprintln!` does on a broken stderr.
    struct BrokenStderr;
    impl Write for BrokenStderr {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            panic!("failed printing to stderr: Broken pipe")
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    struct FullDisk;
    impl Write for FullDisk {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::ErrorKind::StorageFull.into())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn watchers() -> (
        watch::Sender<Health>,
        watch::Receiver<Health>,
        watch::Sender<State>,
        watch::Receiver<State>,
    ) {
        let (published, health) = watch::channel(Health {
            ok: true,
            players: 0,
            tick: 0,
            revision: 0,
            timing: Metrics::default().report(),
            error: None,
        });
        let (state, running) = watch::channel(State::Running);
        (published, health, state, running)
    }

    #[test]
    fn a_failure_is_published_before_a_broken_log_can_interrupt_it() {
        let (published, health, state, running) = watchers();
        let logged = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            fail(&published, &state, "boom".into(), &mut BrokenStderr)
        }));
        assert!(
            logged.is_err(),
            "the log is the failure being guarded against"
        );
        assert_eq!(*running.borrow(), State::Failed("boom".into()));
        assert!(!health.borrow().ok);
        assert_eq!(health.borrow().error.as_deref(), Some("boom"));
    }

    #[test]
    fn a_log_that_reports_an_error_changes_nothing() {
        let (published, health, state, running) = watchers();
        fail(&published, &state, "boom".into(), &mut FullDisk);
        assert_eq!(*running.borrow(), State::Failed("boom".into()));
        assert!(!health.borrow().ok);
    }
}
