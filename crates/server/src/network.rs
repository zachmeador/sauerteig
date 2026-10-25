//! Async transport and save/edit workers. Session lives only on its simulation
//! thread; every interaction with it is a typed command and a reply channel.
use crate::{
    edits::BUSY,
    outbound,
    persistence::{load_world, write_snapshot},
    simulation::{self, Command, Commit, Handle, Health, State as RunState},
    Config, Retired, Session, MAX_PLAYERS, MAX_WORLD_SNAPSHOT_BYTES,
};
use axum::{
    extract::{
        ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade},
        ConnectInfo, State,
    },
    http::StatusCode,
    response::{Html, IntoResponse, Response},
    routing::get,
    Json, Router,
};
use futures_util::{SinkExt, StreamExt};
use sauerteig_core::{ClientMessage, ServerMessage};
use std::{
    collections::VecDeque,
    future::Future,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};
use tokio::{
    net::TcpListener,
    sync::{mpsc, oneshot, watch},
    time::MissedTickBehavior,
};
use tower_http::services::ServeDir;

mod transport;
use transport::{Progress, ProgressListener};
pub(super) const MESSAGE_BYTES: usize = 16 * 1024 * 1024;
pub(super) const OUTBOUND_WRITE_BYTES: usize = MAX_WORLD_SNAPSHOT_BYTES + 32 * 1024;
const STALL_TIMEOUT: Duration = Duration::from_secs(30);
/// How long a prepared edit waits for room in every peer's queue. Past this the
/// player is told the room is busy and the worker moves on, so one lagging peer
/// cannot hold up everyone else's edits.
const COMMIT_WAIT: Duration = Duration::from_secs(2);
const COMMIT_POLL: Duration = Duration::from_millis(25);
/// When the writer can no longer reach a client, the client may simply be gone
/// with commands still in the socket's buffer. The reader keeps taking those (it
/// ends at once when the socket reports the close) but stops this long after the
/// writer did, so a live client cannot hold the connection open.
const WRITER_GONE_GRACE: Duration = Duration::from_secs(2);
struct EditJob {
    id: u64,
    message: ClientMessage,
    done: oneshot::Sender<()>,
}
struct App {
    simulation: Handle,
    health: watch::Receiver<Health>,
    edits: mpsc::Sender<EditJob>,
    config: Config,
    state: watch::Sender<RunState>,
}

async fn health(State(app): State<Arc<App>>) -> Json<Health> {
    Json(app.health.borrow().clone())
}
async fn upgrade(
    ws: WebSocketUpgrade,
    State(app): State<Arc<App>>,
    ConnectInfo(progress): ConnectInfo<Progress>,
) -> Response {
    if *app.state.borrow() != RunState::Running {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "The simulation has stopped.",
        )
            .into_response();
    }
    if app.health.borrow().players >= MAX_PLAYERS {
        return (StatusCode::SERVICE_UNAVAILABLE, "The room is full.").into_response();
    }
    ws.max_message_size(MESSAGE_BYTES)
        .max_frame_size(MESSAGE_BYTES)
        .write_buffer_size(16 * 1024)
        .max_write_buffer_size(OUTBOUND_WRITE_BYTES)
        .on_upgrade(move |socket| connection(socket, app, progress))
}
type Writer = futures_util::stream::SplitSink<WebSocket, Message>;
type Reader = futures_util::stream::SplitStream<WebSocket>;
async fn send_packet(
    socket: &mut Writer,
    progress: &Progress,
    message: Message,
) -> Result<(), String> {
    drain_while_progressing(
        async { socket.send(message).await.map_err(|e| e.to_string()) },
        progress,
        STALL_TIMEOUT,
    )
    .await
}
async fn drain_while_progressing(
    send: impl Future<Output = Result<(), String>>,
    progress: &Progress,
    stall: Duration,
) -> Result<(), String> {
    let started = Instant::now();
    tokio::pin!(send);
    loop {
        let deadline = progress.last_write().max(started) + stall;
        tokio::select! {
            result = &mut send => return result,
            _ = tokio::time::sleep_until(deadline.into()) => {
                if progress.last_write().max(started).elapsed() >= stall {
                    return Err("WebSocket stalled: no bytes drained for 30 seconds.".into());
                }
            }
        }
    }
}
async fn encode_packet(packet: Arc<outbound::Packet>) -> Result<String, String> {
    tokio::task::spawn_blocking(move || packet.encode().map(str::to_owned))
        .await
        .map_err(|e| e.to_string())?
}
/// Frees `value` on a blocking thread, where dropping a large world or history
/// cannot hold up the room. Nothing waits for it.
fn release<T: Send + 'static>(value: T) {
    drop(tokio::task::spawn_blocking(move || drop(value)));
}

/// How a connection is told it is over.
enum Farewell {
    Normal,
    /// The simulation failed; the last saved world is intact.
    Failed(String),
    /// The room would not take this player.
    Refused(String),
}
impl Farewell {
    fn ended(state: RunState) -> Self {
        match state {
            RunState::Failed(message) => Self::Failed(message),
            _ => Self::Normal,
        }
    }
}
/// Sends the reason, if there is one, and a Close frame, without waiting long
/// for a client that is not listening.
async fn say_goodbye<S>(sink: &mut S, farewell: Farewell)
where
    S: futures_util::Sink<Message> + Unpin,
    S::Error: std::fmt::Display,
{
    let goodbye = async {
        let (error, close) = match farewell {
            Farewell::Normal => (None, None),
            Farewell::Failed(message) => (
                Some(message),
                Some((
                    1011,
                    "Simulation failed; last saved world is intact.".to_string(),
                )),
            ),
            Farewell::Refused(message) => (Some(message.clone()), Some((1013, message))),
        };
        if let Some(message) = error {
            let text = serde_json::to_string(&ServerMessage::Error { message })
                .map_err(|e| e.to_string())?;
            sink.send(Message::Text(text.into()))
                .await
                .map_err(|e| e.to_string())?;
        }
        let frame = close.map(|(code, reason)| CloseFrame {
            code,
            reason: reason.into(),
        });
        sink.send(Message::Close(frame))
            .await
            .map_err(|e| e.to_string())?;
        Ok::<(), String>(())
    };
    let _ = tokio::time::timeout(Duration::from_millis(250), goodbye).await;
}

/// Commands a connection has read but not finished. They outlive the socket:
/// whatever a client sent before it went away still applies, in order.
#[derive(Default)]
struct Inbox {
    /// The edit now with the worker.
    pending: Option<oneshot::Receiver<()>>,
    /// Commands read while an edit was pending, in arrival order.
    deferred: VecDeque<(Result<ClientMessage, String>, usize)>,
    deferred_bytes: usize,
}
impl Inbox {
    /// Starts the oldest deferred command once nothing is pending.
    async fn start_deferred(&mut self, app: &App, id: u64) -> Result<bool, String> {
        if self.pending.is_some() {
            return Ok(false);
        }
        let Some((message, bytes)) = self.deferred.pop_front() else {
            return Ok(false);
        };
        self.deferred_bytes -= bytes;
        self.pending = dispatch(app, id, message).await?;
        Ok(true)
    }
    /// Runs what is still owed once the client can no longer be read. Movement
    /// is for a player who is leaving; every other command applies, in order.
    async fn finish(&mut self, app: &App, id: u64) {
        self.deferred
            .retain(|(message, _)| !matches!(message, Ok(ClientMessage::Input { .. })));
        loop {
            if let Some(finished) = self.pending.take() {
                let _ = finished.await;
            }
            let Some((message, _)) = self.deferred.pop_front() else {
                return;
            };
            match dispatch(app, id, message).await {
                Ok(next) => self.pending = next,
                Err(_) => return,
            }
        }
    }
}

/// Reads and dispatches one client's commands until it goes away, the writer
/// can no longer reach it and the grace period passes, or it misbehaves. What was
/// read stays in `inbox`.
async fn read_commands(
    app: &App,
    id: u64,
    reader: &mut Reader,
    welcoming: &AtomicBool,
    writer_gone: &mut watch::Receiver<bool>,
    inbox: &mut Inbox,
) -> Result<(), String> {
    let mut tokens = 240.0_f64;
    let mut token_time = Instant::now();
    let mut give_up: Option<Instant> = None;
    loop {
        if give_up.is_none() && *writer_gone.borrow() {
            give_up = Some(Instant::now() + WRITER_GONE_GRACE);
        }
        if give_up.is_some_and(|deadline| Instant::now() >= deadline) {
            return Ok(());
        }
        if inbox.start_deferred(app, id).await? {
            continue;
        }
        let received = tokio::select! {
            _ = async { if let Some(finished) = &mut inbox.pending { let _ = finished.await; } }, if inbox.pending.is_some() => {
                inbox.pending = None;
                continue;
            }
            _ = writer_gone.changed(), if give_up.is_none() => continue,
            packet = async {
                match give_up {
                    Some(deadline) => tokio::time::timeout_at(deadline.into(), reader.next()).await,
                    None => tokio::time::timeout(Duration::from_secs(60), reader.next()).await,
                }
            } => packet,
        };
        let packet = match received {
            Ok(packet) => packet,
            Err(_) if give_up.is_some() => return Ok(()),
            // A client cannot answer a ping until Welcome drains.
            Err(_) if welcoming.load(Ordering::Acquire) => continue,
            Err(_) => return Err("Peer timed out".into()),
        };
        let Some(Ok(packet)) = packet else {
            return Ok(());
        };
        match packet {
            Message::Text(text) => {
                let now = Instant::now();
                tokens = (tokens + now.duration_since(token_time).as_secs_f64() * 120.0).min(240.0);
                token_time = now;
                if tokens < 1.0 {
                    return Err("Message rate exceeded".into());
                }
                tokens -= 1.0;
                // Include queue/parsed-value overhead, including empty
                // malformed frames, so tiny commands cannot evade the cap.
                let bytes = text.len().saturating_add(1024);
                let message = tokio::task::spawn_blocking(move || {
                    serde_json::from_str::<ClientMessage>(&text)
                        .map_err(|e| format!("Invalid message: {e}"))
                })
                .await
                .map_err(|e| e.to_string())?;
                // Movement is for a player who can no longer be answered.
                if give_up.is_some() && matches!(message, Ok(ClientMessage::Input { .. })) {
                    continue;
                }
                // Movement may pass a pending edit/save/sync, but never a
                // mode change: that barrier drains old-mode inputs.
                let movement = matches!(&message, Ok(ClientMessage::Input { .. }))
                    && !inbox
                        .deferred
                        .iter()
                        .any(|(m, _)| matches!(m, Ok(ClientMessage::SetMode { .. })));
                if inbox.pending.is_none() || movement {
                    let next = dispatch(app, id, message).await?;
                    if next.is_some() {
                        inbox.pending = next;
                    }
                } else {
                    inbox.deferred_bytes += bytes;
                    if inbox.deferred_bytes > MESSAGE_BYTES * 2 {
                        return Err("Pending command byte limit exceeded".into());
                    }
                    inbox.deferred.push_back((message, bytes));
                }
            }
            Message::Close(_) | Message::Binary(_) => return Ok(()),
            // Tungstenite queues pong replies, flushed by the writer.
            Message::Ping(_) | Message::Pong(_) => {}
        }
    }
}

async fn connection(mut socket: WebSocket, app: Arc<App>, progress: Progress) {
    let (events, mut event_rx) = outbound::channel();
    let (snapshots, mut snapshot_rx) = watch::channel(None);
    let mut stopped = app.state.subscribe();
    let state = stopped.borrow().clone();
    if state != RunState::Running {
        say_goodbye(&mut socket, Farewell::ended(state)).await;
        return;
    }
    let joined = app
        .simulation
        .request(|reply| Command::Join {
            events,
            snapshots,
            reply,
        })
        .await;
    let (id, welcome) = match joined {
        Ok(Ok(joined)) => joined,
        Ok(Err(reason)) => {
            say_goodbye(&mut socket, Farewell::Refused(reason)).await;
            return;
        }
        Err(_) => {
            let state = stopped.borrow().clone();
            say_goodbye(&mut socket, Farewell::ended(state)).await;
            return;
        }
    };
    let (mut writer, mut reader) = socket.split();
    let welcoming = AtomicBool::new(true);
    let (writer_done, mut writer_gone) = watch::channel(false);
    {
        let writing = async {
            let sent: Result<(), String> = async {
                let text = tokio::task::spawn_blocking(move || {
                    serde_json::to_string(&welcome.expand()).map_err(|e| e.to_string())
                })
                .await
                .map_err(|e| e.to_string())??;
                send_packet(&mut writer, &progress, Message::Text(text.into())).await?;
                welcoming.store(false, Ordering::Release);
                let mut ping = tokio::time::interval(Duration::from_secs(15));
                loop {
                    tokio::select! {
                        biased;
                        event = event_rx.recv() => {
                            let Some(event) = event else { return Ok(()); };
                            let packet = event.packet.clone();
                            let text = encode_packet(packet).await?;
                            send_packet(&mut writer, &progress, Message::Text(text.into())).await?;
                            drop(event); // Release bytes only when the socket drained them.
                        }
                        changed = snapshot_rx.changed() => {
                            if changed.is_err() { return Ok(()); }
                            let packet = snapshot_rx.borrow_and_update().clone();
                            if let Some(packet) = packet {
                                let text = encode_packet(packet).await?;
                                send_packet(&mut writer, &progress, Message::Text(text.into())).await?;
                            }
                        }
                        _ = ping.tick() => {
                            send_packet(&mut writer, &progress, Message::Ping(Vec::new().into())).await?;
                        }
                    }
                }
            }
            .await;
            if let Err(error) = &sent {
                eprintln!("Player {id} write failed: {error}");
            }
            writer_done.send_replace(true);
            // Commands this player already sent still publish to every queue,
            // this one included. Keep it moving until Leave closes it.
            while event_rx.recv().await.is_some() {}
            // Only the reader decides when the connection is over.
            std::future::pending::<()>().await;
        };
        let reading = async {
            let mut inbox = Inbox::default();
            let outcome = read_commands(
                &app,
                id,
                &mut reader,
                &welcoming,
                &mut writer_gone,
                &mut inbox,
            )
            .await;
            // A client that vanishes still gets what it already sent applied.
            inbox.finish(&app, id).await;
            let _ = app.simulation.send(Command::Leave(id));
            outcome
        };
        // Checking the current value covers shutdown that won before subscribing
        // or during Welcome preparation. Any pending encode/write is cancellable.
        if *stopped.borrow() == RunState::Running {
            tokio::select! {
                result = reading => if let Err(error) = result { eprintln!("Player {id} read failed: {error}"); },
                _ = writing => {},
                _ = stopped.changed() => {},
            }
        }
    }
    let _ = app.simulation.send(Command::Leave(id));
    let state = stopped.borrow().clone();
    say_goodbye(&mut writer, Farewell::ended(state)).await;
}
async fn dispatch(
    app: &App,
    id: u64,
    message: Result<ClientMessage, String>,
) -> Result<Option<oneshot::Receiver<()>>, String> {
    match message {
        Ok(message) if simulation::is_edit(&message) => {
            let (done, finished) = oneshot::channel();
            app.edits
                .send(EditJob { id, message, done })
                .await
                .map_err(|_| "Edit worker stopped".to_string())?;
            Ok(Some(finished))
        }
        Ok(message) => {
            app.simulation
                .request(|reply| Command::Control { id, message, reply })
                .await?;
            Ok(None)
        }
        Err(error) => {
            app.simulation
                .request(|reply| Command::Report {
                    id,
                    request: None,
                    result: Err(error),
                    reply,
                })
                .await?;
            Ok(None)
        }
    }
}
/// Prepares one edit off the simulation thread and offers it to the owner until
/// every peer's queue can take its patch or `COMMIT_WAIT` runs out.
async fn run_edit(
    app: &App,
    stop: &watch::Receiver<RunState>,
    id: u64,
    message: ClientMessage,
) -> Result<(), String> {
    let plan = app
        .simulation
        .request(|reply| Command::Plan { id, message, reply })
        .await??;
    let mut edit = Box::new(
        tokio::task::spawn_blocking(move || plan.prepare())
            .await
            .map_err(|e| format!("Edit preparation failed: {e}"))??,
    );
    let waiting = Instant::now();
    loop {
        if *stop.borrow() != RunState::Running {
            return Err("The session ended before this edit committed.".into());
        }
        match app
            .simulation
            .request(|reply| Command::Commit { id, edit, reply })
            .await?
        {
            Commit::Done { edit, result } => {
                release(edit);
                return result;
            }
            Commit::Wait(unsent) => {
                if waiting.elapsed() >= COMMIT_WAIT {
                    release(unsent);
                    return Err(BUSY.into());
                }
                edit = unsent;
                tokio::time::sleep(COMMIT_POLL).await;
            }
        }
    }
}
async fn edit_worker(
    app: Arc<App>,
    mut jobs: mpsc::Receiver<EditJob>,
    mut retired: mpsc::UnboundedReceiver<Retired>,
) {
    let mut stop = app.state.subscribe();
    loop {
        if *stop.borrow() != RunState::Running {
            break;
        }
        let job = tokio::select! {
            job = jobs.recv() => match job { Some(job) => job, None => break },
            Some(history) = retired.recv() => {
                release(history);
                continue;
            }
            _ = stop.changed() => break,
        };
        // A job whose connection has gone still runs: the client sent it, and
        // only the result has nowhere to go.
        let request = simulation::request_id(&job.message);
        let result = run_edit(&app, &stop, job.id, job.message).await;
        let _ = app
            .simulation
            .request(|reply| Command::Report {
                id: job.id,
                request,
                result,
                reply,
            })
            .await;
        let _ = job.done.send(());
    }
}
async fn save_current(app: &Arc<App>, force: bool) -> Result<(), String> {
    let saved = app
        .simulation
        .request(|reply| Command::Save { force, reply })
        .await?;
    let Some(saved) = saved else {
        return Ok(());
    };
    let revision = saved.revision;
    let path = app.config.world_path.clone();
    let state = app.state.subscribe();
    tokio::task::spawn_blocking(move || {
        write_snapshot(&path, &saved, || {
            !matches!(*state.borrow(), RunState::Failed(_))
        })
    })
    .await
    .map_err(|e| e.to_string())??;
    app.simulation.send(Command::Saved { revision })
}
async fn save_worker(app: Arc<App>) {
    let mut stop = app.state.subscribe();
    let mut timer = tokio::time::interval(Duration::from_secs(1));
    timer.set_missed_tick_behavior(MissedTickBehavior::Skip);
    loop {
        if *stop.borrow() != RunState::Running {
            break;
        }
        tokio::select! {
            _ = stop.changed() => break,
            _ = timer.tick() => {
                if let Err(error) = save_current(&app, false).await {
                    eprintln!("World save failed: {error}");
                    tokio::select! {
                        _ = tokio::time::sleep(Duration::from_secs(4)) => {},
                        _ = stop.changed() => break,
                    }
                }
            }
        }
    }
}
async fn log_worker(app: Arc<App>) {
    let mut stop = app.state.subscribe();
    loop {
        if *stop.borrow() != RunState::Running {
            break;
        }
        tokio::select! {
            _ = stop.changed() => break,
            _ = tokio::time::sleep(Duration::from_secs(10)) => {
                let health = app.health.borrow().clone();
                let t = &health.timing.ticks;
                let work = serde_json::to_string(&health.timing.work).unwrap_or_default();
                eprintln!(
                    "Simulation ticks: p50={:.3} p99={:.3} max={:.3} ms ({} samples, cause={}); work={work}",
                    t.p50_ms, t.p99_ms, t.max_ms, t.samples, health.timing.max_tick_cause
                );
            }
        }
    }
}
/// Serve an already-bound listener. Failure of the owner terminates the room
/// and returns an error to the embedded host's existing visible error path.
pub async fn serve(
    listener: TcpListener,
    config: Config,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> Result<(), String> {
    serve_inner(
        listener,
        config,
        shutdown,
        #[cfg(test)]
        None,
    )
    .await
}
async fn serve_inner(
    listener: TcpListener,
    config: Config,
    shutdown: impl Future<Output = ()> + Send + 'static,
    #[cfg(test)] control: Option<oneshot::Sender<Handle>>,
) -> Result<(), String> {
    let path = config.world_path.clone();
    let editing = config.editing_allowed;
    let session =
        tokio::task::spawn_blocking(move || Session::from_saved(load_world(&path)?, editing))
            .await
            .map_err(|e| e.to_string())??;
    let (state, _) = watch::channel(RunState::Running);
    let (retire, retired) = mpsc::unbounded_channel();
    let (simulation, published_health, thread) = simulation::start(session, state.clone(), retire)?;
    #[cfg(test)]
    if let Some(control) = control {
        let _ = control.send(simulation.clone());
    }
    let (edits, jobs) = mpsc::channel(MAX_PLAYERS);
    let app = Arc::new(App {
        simulation,
        health: published_health,
        edits,
        config,
        state,
    });
    let router = Router::new()
        .route("/health", get(health))
        .route("/ws", get(upgrade));
    let router = if app.config.web_dir.join("index.html").is_file() {
        router.fallback_service(ServeDir::new(&app.config.web_dir))
    } else {
        router.fallback(|| async {
            Html(concat!(
                "<!doctype html><title>Sauerteig server</title><h1>Sauerteig server is running</h1>",
                "<p>The web client has not been built. Build it with the repository's web build ",
                "script, then restart the server. Native clients can connect to /ws now.</p>"
            ))
        })
    }.with_state(app.clone());
    let edit_task = tokio::spawn(edit_worker(app.clone(), jobs, retired));
    let save_task = tokio::spawn(save_worker(app.clone()));
    let log_task = tokio::spawn(log_worker(app.clone()));
    let stop_app = app.clone();
    let result = axum::serve(
        ProgressListener(listener),
        router.into_make_service_with_connect_info::<Progress>(),
    )
    .with_graceful_shutdown(async move {
        let mut stopped = stop_app.state.subscribe();
        if *stopped.borrow() == RunState::Running {
            tokio::select! {_=shutdown=>{},_=stopped.changed()=>{}}
        }
        if *stop_app.state.borrow() == RunState::Running {
            stop_app.state.send_replace(RunState::Stopping);
        }
    })
    .await
    .map_err(|e| e.to_string());
    if *app.state.borrow() == RunState::Running {
        app.state.send_replace(RunState::Stopping);
    }
    let _ = edit_task.await;
    let _ = save_task.await;
    let _ = log_task.await;
    let failure = match &*app.state.borrow() {
        RunState::Failed(error) => Some(error.clone()),
        _ => None,
    };
    let final_result = if let Some(error) = failure {
        Err(error)
    } else {
        match app
            .simulation
            .request(|reply| Command::Stop { reply })
            .await
        {
            Ok(saved) => {
                let path = app.config.world_path.clone();
                tokio::task::spawn_blocking(move || write_snapshot(&path, &saved, || true))
                    .await
                    .map_err(|e| e.to_string())?
            }
            Err(error) => Err(error),
        }
    };
    tokio::task::spawn_blocking(move || thread.join())
        .await
        .map_err(|e| e.to_string())?
        .map_err(|_| "Simulation thread failed".to_string())?;
    final_result?;
    result
}
#[cfg(test)]
mod tests;
