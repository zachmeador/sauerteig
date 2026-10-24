//! HTTP/WebSocket transport and simulation/save task orchestration.

use crate::{
    persistence::{load_world, write_world},
    Config, Session, MAX_PLAYERS, MAX_WORLD_SNAPSHOT_BYTES, SAVE_DELAY,
};
use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        State,
    },
    http::StatusCode,
    response::{Html, IntoResponse, Response},
    routing::get,
    Json, Router,
};
use sauerteig_core::{ClientMessage, ServerMessage};
use std::{
    future::Future,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::{
    net::TcpListener,
    sync::{mpsc, watch},
    time::MissedTickBehavior,
};
use tower_http::services::ServeDir;

pub(super) const MESSAGE_BYTES: usize = 16 * 1024 * 1024;
// Leave framing and pending-write headroom above the native client's message
// limit. Session and save-file validation keep every world snapshot below it.
pub(super) const OUTBOUND_WRITE_BYTES: usize = MAX_WORLD_SNAPSHOT_BYTES + 32 * 1024;

struct App {
    session: Mutex<Session>,
    save_lock: tokio::sync::Mutex<()>,
    config: Config,
    stopped: watch::Sender<bool>,
}

async fn save_current(app: &Arc<App>, force: bool) -> Result<(), String> {
    let _saving = app.save_lock.lock().await;
    let saved = {
        let session = app.session.lock().unwrap();
        if !force && (!session.dirty || session.last_edit.elapsed() < SAVE_DELAY) {
            return Ok(());
        }
        session.saved()
    };
    let revision = saved.revision;
    let path = app.config.world_path.clone();
    tokio::task::spawn_blocking(move || write_world(&path, &saved))
        .await
        .map_err(|e| e.to_string())??;
    let mut session = app.session.lock().unwrap();
    if session.revision == revision {
        session.dirty = false;
    }
    Ok(())
}

async fn health(State(app): State<Arc<App>>) -> Json<serde_json::Value> {
    let session = app.session.lock().unwrap();
    Json(
        serde_json::json!({"ok":true,"players":session.peers.len(),"tick":session.tick,"revision":session.revision}),
    )
}

async fn upgrade(ws: WebSocketUpgrade, State(app): State<Arc<App>>) -> Response {
    if app.session.lock().unwrap().peers.len() >= MAX_PLAYERS {
        return (StatusCode::SERVICE_UNAVAILABLE, "The room is full.").into_response();
    }
    ws.max_message_size(MESSAGE_BYTES)
        .max_frame_size(MESSAGE_BYTES)
        .write_buffer_size(16 * 1024)
        .max_write_buffer_size(OUTBOUND_WRITE_BYTES)
        .on_upgrade(move |socket| connection(socket, app))
}

async fn connection(mut socket: WebSocket, app: Arc<App>) {
    let (event_tx, mut events) = mpsc::channel(64);
    let (snapshot_tx, mut snapshots) = watch::channel(String::new());
    let joined = app.session.lock().unwrap().join(event_tx, snapshot_tx);
    let Ok((id, welcome)) = joined else {
        let _ = send_packet(&mut socket, Message::Close(None)).await;
        return;
    };
    if let Err(error) = send_packet(
        &mut socket,
        Message::Text(serde_json::to_string(&welcome).unwrap().into()),
    )
    .await
    {
        eprintln!("Could not send world snapshot to player {id}: {error}");
        app.session.lock().unwrap().peers.remove(&id);
        return;
    }
    let mut stopped = app.stopped.subscribe();
    let mut tokens: f64 = 240.0;
    let mut token_time = Instant::now();
    let mut last_packet = Instant::now();
    let mut ping = tokio::time::interval(Duration::from_secs(15));
    loop {
        // A delayed Welcome may finish after shutdown was already sent.
        if *stopped.borrow() {
            break;
        }
        tokio::select! {
            biased;
            _ = stopped.changed() => break,
            event = events.recv() => {
                let Some(text) = event else { break; };
                if send_packet(&mut socket, Message::Text(text.into())).await.is_err() { break; }
            }
            message = socket.recv() => {
                let Some(Ok(message)) = message else { break; };
                last_packet = Instant::now();
                match message {
                    Message::Text(text) => {
                        let now = Instant::now();
                        tokens = (tokens + now.duration_since(token_time).as_secs_f64() * 120.0).min(240.0);
                        token_time = now;
                        if tokens < 1.0 { break; }
                        tokens -= 1.0;
                        let result = serde_json::from_str::<ClientMessage>(&text).map_err(|e| format!("Invalid message: {e}"));
                        let mut session = app.session.lock().unwrap();
                        let (edit_request, result) = match result {
                            Ok(message) => {
                                let request = match &message {
                                    ClientMessage::Edit { request, .. }
                                    | ClientMessage::EditRoots { request, .. }
                                    | ClientMessage::EditTransaction { request, .. } => Some(*request),
                                    _ => None,
                                };
                                (request, session.handle(id, message))
                            }
                            Err(error) => (None, Err(error)),
                        };
                        // The patch is queued by handle() before this result. Only
                        // this matching acknowledgment completes a client's edit;
                        // another builder's patch may contain identical geometry.
                        if let Some(request) = edit_request {
                            session.send(id, ServerMessage::EditResult { request, error: result.err() });
                        } else if let Err(message) = result {
                            session.send(id, ServerMessage::Error { message });
                        }
                    }
                    Message::Ping(bytes) => if send_packet(&mut socket, Message::Pong(bytes)).await.is_err() { break; },
                    Message::Close(_) => break,
                    Message::Binary(_) => break,
                    Message::Pong(_) => {}
                }
            }
            changed = snapshots.changed() => {
                if changed.is_err() { break; }
                let text = snapshots.borrow_and_update().clone();
                if !text.is_empty() && send_packet(&mut socket, Message::Text(text.into())).await.is_err() { break; }
            }
            _ = ping.tick() => {
                if last_packet.elapsed() > Duration::from_secs(60) { break; }
                if send_packet(&mut socket, Message::Ping(Vec::new().into())).await.is_err() { break; }
            }
        }
    }
    app.session.lock().unwrap().peers.remove(&id);
    let _ = send_packet(&mut socket, Message::Close(None)).await;
}

async fn send_packet(socket: &mut WebSocket, message: Message) -> Result<(), String> {
    tokio::time::timeout(Duration::from_secs(5), socket.send(message))
        .await
        .map_err(|_| "WebSocket write timed out".to_string())?
        .map_err(|error| error.to_string())
}

/// Serve an already-bound listener. Keeping binding separate makes tests use
/// ephemeral loopback ports and lets the CLI print the actual listening address.
pub async fn serve(
    listener: TcpListener,
    config: Config,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> Result<(), String> {
    let saved = load_world(&config.world_path)?;
    let (stopped, _) = watch::channel(false);
    let app = Arc::new(App {
        session: Mutex::new(Session::new(saved, config.editing_allowed)),
        save_lock: tokio::sync::Mutex::new(()),
        config,
        stopped,
    });
    let router = Router::new()
        .route("/health", get(health))
        .route("/ws", get(upgrade));
    let router = if app.config.web_dir.join("index.html").is_file() {
        router.fallback_service(ServeDir::new(&app.config.web_dir))
    } else {
        router.fallback(|| async { Html("<!doctype html><title>Sauerteig server</title><h1>Sauerteig server is running</h1><p>The web client has not been built. Build it with the repository's web build script, then restart the server. Native clients can connect to this server's /ws endpoint now.</p>") })
    }.with_state(app.clone());
    let tick_app = app.clone();
    let tick_task = tokio::spawn(async move {
        let mut stop = tick_app.stopped.subscribe();
        let mut timer = tokio::time::interval(Duration::from_secs_f64(1.0 / 60.0));
        timer.set_missed_tick_behavior(MissedTickBehavior::Skip);
        loop {
            if *stop.borrow() {
                break;
            }
            tokio::select! {
                _ = stop.changed() => break,
                _ = timer.tick() => tick_app.session.lock().unwrap().step(),
            }
        }
    });
    let save_app = app.clone();
    let save_task = tokio::spawn(async move {
        let mut stop = save_app.stopped.subscribe();
        let mut timer = tokio::time::interval(Duration::from_secs(1));
        timer.set_missed_tick_behavior(MissedTickBehavior::Skip);
        loop {
            if *stop.borrow() {
                break;
            }
            tokio::select! {
                _ = stop.changed() => break,
                _ = timer.tick() => {
                    if let Err(error) = save_current(&save_app, false).await {
                        eprintln!("World save failed: {error}");
                        // Bound retries; simulation continues while persistence is unavailable.
                        tokio::time::sleep(Duration::from_secs(4)).await;
                    }
                }
            }
        }
    });
    let stop_app = app.clone();
    let result = axum::serve(listener, router)
        .with_graceful_shutdown(async move {
            shutdown.await;
            stop_app.stopped.send_replace(true);
        })
        .await
        .map_err(|e| e.to_string());
    app.stopped.send_replace(true);
    let _ = tick_task.await;
    let _ = save_task.await;
    save_current(&app, true).await?;
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::StreamExt;

    #[tokio::test(flavor = "current_thread")]
    async fn connection_opened_after_shutdown_closes_without_another_stop_signal() {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let config = Config {
            world_path: std::env::temp_dir().join(format!(
                "sauerteig-early-stop-{}-{stamp}",
                std::process::id()
            )),
            ..Config::default()
        };
        let (stopped, _) = watch::channel(true);
        let app = Arc::new(App {
            session: Mutex::new(Session::new(load_world(&config.world_path).unwrap(), true)),
            save_lock: tokio::sync::Mutex::new(()),
            config,
            stopped,
        });
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("ws://{}/ws", listener.local_addr().unwrap());
        let router = Router::new()
            .route("/ws", get(upgrade))
            .with_state(app.clone());
        let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let (mut socket, _) = tokio_tungstenite::connect_async(endpoint).await.unwrap();
        // Model shutdown winning before a delayed WebSocket upgrade or Welcome
        // write completes. No second stop notification is ever sent.
        let closed = tokio::time::timeout(Duration::from_secs(2), async {
            while let Some(message) = socket.next().await {
                if matches!(
                    message.unwrap(),
                    tokio_tungstenite::tungstenite::Message::Close(_)
                ) {
                    return;
                }
            }
        })
        .await;
        server.abort();
        closed.expect("a late connection must not keep graceful shutdown waiting");
        assert!(app.session.lock().unwrap().peers.is_empty());
    }
}
