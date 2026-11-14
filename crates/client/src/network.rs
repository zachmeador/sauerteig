//! The client's connection to a world server, kept on its own thread.
//!
//! A thread running a current-thread tokio runtime owns the WebSocket. It
//! sends what the frame thread hands it at once, counts the bytes that arrive,
//! decodes messages (the Welcome into a plain [`Welcome`] structure, which
//! needs far less memory than the tagged [`ServerMessage`]), and passes them
//! on through a bounded queue that the frame thread empties once per tick.
//!
//! The queue keeps every message in order except snapshots: only the newest
//! snapshot is kept, in its place in the order. When the frame thread stalls,
//! the queue fills and the thread stops reading, so the backlog waits in the
//! connection instead of in memory.
//!
//! Two limits end a connection. About 5 s without a byte from the server
//! (counted from the end of the handshake, and only while the thread is
//! waiting to read) means the server is gone, because snapshots arrive at
//! 20 Hz. Connecting and the handshake have their own, longer limit, since a
//! host's embedded server answers the upgrade only after loading the world.
//! The caller reconnects after either.
use futures_util::{SinkExt, StreamExt};
use sauerteig_core::{
    CellEdit, CellFragment, MaterialVolume, Player, RootEdit, ServerMessage, WorldBounds,
    WorldObject,
};
use serde::Deserialize;
use std::{
    collections::VecDeque,
    io,
    pin::Pin,
    sync::{
        atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering},
        Arc, Mutex, MutexGuard,
    },
    task::{Context, Poll},
    thread::JoinHandle,
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    net::TcpStream,
    sync::{mpsc, watch, Notify},
};
use tokio_tungstenite::{
    client_async_tls_with_config,
    tungstenite::{
        client::IntoClientRequest, handshake::client::Request, protocol::WebSocketConfig,
        Error as WsError, Message,
    },
    Connector, MaybeTlsStream, WebSocketStream,
};

/// Without any bytes from the server for this long, the connection ends.
pub const SILENCE_LIMIT: Duration = Duration::from_secs(5);
/// Connecting, the encryption handshake and the WebSocket upgrade together.
pub const CONNECT_LIMIT: Duration = Duration::from_secs(30);
/// Today's Welcome is one message, so this is the largest world that joins.
const MAX_MESSAGE_BYTES: usize = 64 * 1024 * 1024;
/// Messages waiting for the frame thread before the connection stops reading.
const QUEUE_LIMIT: usize = 64;
/// How long a closing connection waits to send its Close frame.
const CLOSE_GRACE: Duration = Duration::from_millis(250);

#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub silence: Duration,
    pub connect: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            silence: SILENCE_LIMIT,
            connect: CONNECT_LIMIT,
        }
    }
}

/// The server's Welcome as plain data. It has the fields of
/// [`ServerMessage::Welcome`]; reading it directly avoids the buffered copy
/// that serde makes of a whole tagged message.
#[derive(Debug, Deserialize)]
pub struct Welcome {
    pub id: u64,
    pub revision: u64,
    #[serde(default = "legacy_format_version")]
    pub format_version: u32,
    #[serde(default)]
    pub bounds: WorldBounds,
    #[serde(default)]
    pub regions: Vec<CellFragment>,
    pub cells: Vec<CellEdit>,
    #[serde(default)]
    pub roots: Vec<RootEdit>,
    pub objects: Vec<WorldObject>,
    #[serde(default)]
    pub volumes: Vec<MaterialVolume>,
    pub players: Vec<Player>,
    pub editing_allowed: bool,
}

fn legacy_format_version() -> u32 {
    1
}

#[derive(Debug)]
pub enum Event {
    /// The upgrade finished; the caller says Hello next.
    Opened,
    Welcome(Box<Welcome>),
    /// Any other message. Never a Welcome.
    Message(ServerMessage),
    /// The connection ended and nothing follows. `error` is `None` after a
    /// normal close.
    Closed {
        error: Option<String>,
    },
}

enum Ending {
    /// The caller dropped the connection.
    Stopped,
    Closed,
    Failed(String),
}

#[derive(Default)]
struct Inbox {
    events: VecDeque<Event>,
    /// The newest snapshot, and how many queued events precede it.
    snapshot: Option<(usize, ServerMessage)>,
}

struct Shared {
    inbox: Mutex<Inbox>,
    /// The frame thread emptied the queue.
    space: Notify,
    bytes: AtomicU64,
    started: Instant,
    /// Milliseconds after `started` at which the silence clock last restarted.
    quiet_since: AtomicU64,
    /// The thread is waiting for the server to send something.
    waiting: AtomicBool,
    /// 0 unknown, 1 off, 2 on.
    nodelay: AtomicU8,
}

impl Shared {
    fn new() -> Self {
        Self {
            inbox: Mutex::default(),
            space: Notify::new(),
            bytes: AtomicU64::new(0),
            started: Instant::now(),
            quiet_since: AtomicU64::new(0),
            waiting: AtomicBool::new(false),
            nodelay: AtomicU8::new(0),
        }
    }

    fn inbox(&self) -> MutexGuard<'_, Inbox> {
        self.inbox
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn restart_silence_clock(&self) {
        self.quiet_since
            .store(self.started.elapsed().as_millis() as u64, Ordering::Relaxed);
    }

    fn quiet_for(&self) -> Duration {
        let now = self.started.elapsed().as_millis() as u64;
        Duration::from_millis(now.saturating_sub(self.quiet_since.load(Ordering::Relaxed)))
    }

    fn received(&self, bytes: usize) {
        self.bytes.fetch_add(bytes as u64, Ordering::Relaxed);
        self.restart_silence_clock();
    }

    /// Queues an event behind the ones already waiting, once there is room.
    /// Returns false if the connection was dropped first.
    async fn push(&self, event: Event, stop: &watch::Receiver<bool>) -> bool {
        let mut event = Some(event);
        loop {
            let wake = self.space.notified();
            {
                let mut inbox = self.inbox();
                if inbox.events.len() < QUEUE_LIMIT {
                    inbox.events.extend(event.take());
                    return true;
                }
            }
            let mut stop = stop.clone();
            tokio::select! {
                _ = wake => {}
                _ = stopped(&mut stop) => return false,
            }
        }
    }

    /// Replaces any older waiting snapshot. It never waits for room.
    fn replace_snapshot(&self, snapshot: ServerMessage) {
        let mut inbox = self.inbox();
        let ahead = inbox.events.len();
        inbox.snapshot = Some((ahead, snapshot));
    }

    fn drain(&self) -> Vec<Event> {
        let mut inbox = self.inbox();
        let mut events: Vec<Event> = inbox.events.drain(..).collect();
        if let Some((ahead, snapshot)) = inbox.snapshot.take() {
            events.insert(ahead.min(events.len()), Event::Message(snapshot));
        }
        drop(inbox);
        self.space.notify_one();
        events
    }
}

async fn stopped(stop: &mut watch::Receiver<bool>) {
    // A dropped sender also means nobody is listening any more.
    let _ = stop.wait_for(|stop| *stop).await;
}

/// The TCP stream, which counts what arrives before any encryption.
struct Counted {
    inner: TcpStream,
    shared: Arc<Shared>,
}

impl AsyncRead for Counted {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let before = buf.filled().len();
        let result = Pin::new(&mut self.inner).poll_read(cx, buf);
        if matches!(result, Poll::Ready(Ok(()))) {
            let read = buf.filled().len() - before;
            if read > 0 {
                self.shared.received(read);
            }
        }
        result
    }
}

impl AsyncWrite for Counted {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write(cx, buf)
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

/// One connection to a server. Dropping it closes the socket and waits for
/// its thread, so the server sees the player leave before the caller goes on.
pub struct Connection {
    shared: Arc<Shared>,
    outgoing: mpsc::UnboundedSender<String>,
    stop: watch::Sender<bool>,
    thread: Option<JoinHandle<()>>,
}

impl Connection {
    /// Starts connecting to `endpoint`, a `ws://` or `wss://` address.
    pub fn open(endpoint: &str) -> Result<Self, String> {
        Self::open_with(endpoint, Limits::default(), None)
    }

    /// Like [`Connection::open`], with other limits and, for `wss://`, other
    /// trusted certificates than the built-in ones.
    pub(crate) fn open_with(
        endpoint: &str,
        limits: Limits,
        tls: Option<Arc<rustls::ClientConfig>>,
    ) -> Result<Self, String> {
        let request = endpoint
            .into_client_request()
            .map_err(|error| format!("Not a server address: {error}"))?;
        if !matches!(request.uri().scheme_str(), Some("ws" | "wss")) {
            return Err("A server address starts with ws:// or wss://".into());
        }
        let shared = Arc::new(Shared::new());
        let (outgoing, queue) = mpsc::unbounded_channel();
        let (stop, stop_receiver) = watch::channel(false);
        let thread = {
            let shared = shared.clone();
            std::thread::Builder::new()
                .name("network".into())
                .spawn(move || run(request, limits, tls, shared, queue, stop_receiver))
                .map_err(|error| format!("Could not start the network thread: {error}"))?
        };
        Ok(Self {
            shared,
            outgoing,
            stop,
            thread: Some(thread),
        })
    }

    /// Hands a message to the network thread, which writes it at once.
    pub fn send(&self, text: String) {
        let _ = self.outgoing.send(text);
    }

    /// Everything that arrived since the last call, in order.
    pub fn drain(&self) -> Vec<Event> {
        self.shared.drain()
    }

    /// Bytes read from the server's socket so far.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn bytes_received(&self) -> u64 {
        self.shared.bytes.load(Ordering::Relaxed)
    }

    #[cfg_attr(not(test), allow(dead_code))]
    /// Whether Nagle's algorithm is off on this connection's socket, once
    /// the socket exists.
    pub fn tcp_nodelay(&self) -> Option<bool> {
        match self.shared.nodelay.load(Ordering::Relaxed) {
            0 => None,
            state => Some(state == 2),
        }
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        let _ = self.stop.send(true);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn run(
    request: Request,
    limits: Limits,
    tls: Option<Arc<rustls::ClientConfig>>,
    shared: Arc<Shared>,
    mut outgoing: mpsc::UnboundedReceiver<String>,
    stop: watch::Receiver<bool>,
) {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            shared.inbox().events.push_back(Event::Closed {
                error: Some(format!("Could not start the network runtime: {error}")),
            });
            return;
        }
    };
    runtime.block_on(async {
        let error = match session(request, limits, tls, &shared, &mut outgoing, &stop).await {
            Ending::Stopped => return,
            Ending::Closed => None,
            Ending::Failed(error) => Some(error),
        };
        shared.push(Event::Closed { error }, &stop).await;
    });
}

async fn session(
    request: Request,
    limits: Limits,
    tls: Option<Arc<rustls::ClientConfig>>,
    shared: &Arc<Shared>,
    outgoing: &mut mpsc::UnboundedReceiver<String>,
    stop: &watch::Receiver<bool>,
) -> Ending {
    let mut stop_signal = stop.clone();
    let socket = tokio::select! {
        _ = stopped(&mut stop_signal) => return Ending::Stopped,
        found = tokio::time::timeout(limits.connect, open_socket(request, tls, shared)) => {
            match found {
                Ok(Ok(socket)) => socket,
                Ok(Err(error)) => return Ending::Failed(error),
                Err(_) => {
                    return Ending::Failed(format!(
                        "Connecting took longer than {:?}",
                        limits.connect
                    ))
                }
            }
        }
    };
    shared.restart_silence_clock();
    if !shared.push(Event::Opened, stop).await {
        return Ending::Stopped;
    }
    let (mut sink, mut source) = socket.split();
    let reader = async {
        loop {
            shared.restart_silence_clock();
            shared.waiting.store(true, Ordering::Relaxed);
            let next = source.next().await;
            shared.waiting.store(false, Ordering::Relaxed);
            match next {
                Some(Ok(Message::Text(text))) => match decode(text.as_str()) {
                    Ok(Event::Message(snapshot @ ServerMessage::Snapshot { .. })) => {
                        shared.replace_snapshot(snapshot)
                    }
                    Ok(event) => {
                        if !shared.push(event, stop).await {
                            return Ending::Stopped;
                        }
                    }
                    Err(error) => log::warn!("Invalid server message: {error}"),
                },
                Some(Ok(Message::Close(_))) | None => return Ending::Closed,
                Some(Err(WsError::ConnectionClosed | WsError::AlreadyClosed)) => {
                    return Ending::Closed
                }
                Some(Err(error)) => return Ending::Failed(describe(&error)),
                Some(Ok(_)) => {}
            }
        }
    };
    let writer = async {
        while let Some(text) = outgoing.recv().await {
            if let Err(error) = sink.send(Message::Text(text.into())).await {
                return Ending::Failed(describe(&error));
            }
        }
        Ending::Stopped
    };
    let watchdog = async {
        let period =
            (limits.silence / 4).clamp(Duration::from_millis(10), Duration::from_millis(250));
        loop {
            tokio::time::sleep(period).await;
            if shared.waiting.load(Ordering::Relaxed) && shared.quiet_for() > limits.silence {
                return Ending::Failed(format!("No data from the server for {:?}", limits.silence));
            }
        }
    };
    let mut stop_signal = stop.clone();
    let ending = tokio::select! {
        ending = reader => ending,
        ending = writer => ending,
        ending = watchdog => ending,
        _ = stopped(&mut stop_signal) => Ending::Stopped,
    };
    if matches!(ending, Ending::Stopped) {
        let _ = tokio::time::timeout(CLOSE_GRACE, sink.send(Message::Close(None))).await;
    }
    ending
}

async fn open_socket(
    request: Request,
    tls: Option<Arc<rustls::ClientConfig>>,
    shared: &Arc<Shared>,
) -> Result<WebSocketStream<MaybeTlsStream<Counted>>, String> {
    let uri = request.uri().clone();
    let host = uri.host().ok_or("The server address has no host")?;
    let secure = uri.scheme_str() == Some("wss");
    let port = uri.port_u16().unwrap_or(if secure { 443 } else { 80 });
    let tcp = TcpStream::connect(format!("{host}:{port}"))
        .await
        .map_err(|error| format!("Could not reach {host}:{port}: {error}"))?;
    // Inputs are small and frequent; Nagle's algorithm would hold them back.
    if let Err(error) = tcp.set_nodelay(true) {
        log::warn!("Could not turn off Nagle's algorithm: {error}");
    }
    let nodelay = tcp.nodelay().unwrap_or(false);
    shared
        .nodelay
        .store(if nodelay { 2 } else { 1 }, Ordering::Relaxed);
    let stream = Counted {
        inner: tcp,
        shared: shared.clone(),
    };
    let connector = if secure {
        Connector::Rustls(tls.unwrap_or_else(default_tls))
    } else {
        Connector::Plain
    };
    let config = WebSocketConfig::default()
        .max_message_size(Some(MAX_MESSAGE_BYTES))
        .max_frame_size(Some(MAX_MESSAGE_BYTES));
    let (socket, _) = client_async_tls_with_config(request, stream, Some(config), Some(connector))
        .await
        .map_err(|error| describe(&error))?;
    Ok(socket)
}

/// Trusts the public web's certificate authorities, with rustls's `ring`
/// provider chosen here so that no process-wide default is needed.
fn default_tls() -> Arc<rustls::ClientConfig> {
    let roots = rustls::RootCertStore {
        roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
    };
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let config = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .expect("ring supports the default protocol versions")
        .with_root_certificates(roots)
        .with_no_client_auth();
    Arc::new(config)
}

fn describe(error: &WsError) -> String {
    match error {
        WsError::Http(response) => format!(
            "The server refused the connection (HTTP {})",
            response.status().as_u16()
        ),
        other => other.to_string(),
    }
}

/// The prefix the server's serializer gives a Welcome.
const WELCOME_PREFIX: &str = r#"{"type":"welcome""#;

fn decode(text: &str) -> Result<Event, serde_json::Error> {
    if text.starts_with(WELCOME_PREFIX) {
        return serde_json::from_str(text).map(|welcome| Event::Welcome(Box::new(welcome)));
    }
    Ok(match serde_json::from_str::<ServerMessage>(text)? {
        ServerMessage::Welcome {
            id,
            revision,
            format_version,
            bounds,
            regions,
            cells,
            roots,
            objects,
            volumes,
            players,
            editing_allowed,
        } => Event::Welcome(Box::new(Welcome {
            id,
            revision,
            format_version,
            bounds,
            regions,
            cells,
            roots,
            objects,
            volumes,
            players,
            editing_allowed,
        })),
        message => Event::Message(message),
    })
}

#[cfg(test)]
pub(crate) mod testing {
    use super::*;
    use sauerteig_core::ClientMessage;

    /// A blocking player for tests that drive a hosted server.
    pub struct Client {
        pub connection: Connection,
        backlog: VecDeque<Event>,
    }

    impl Client {
        /// Connects, says Hello and returns with the Welcome.
        pub fn join(endpoint: &str) -> (Self, Welcome) {
            let mut client = Self {
                connection: Connection::open(endpoint).unwrap(),
                backlog: VecDeque::new(),
            };
            let Event::Welcome(welcome) = client.wait(|event| matches!(event, Event::Welcome(_)))
            else {
                unreachable!()
            };
            (client, *welcome)
        }

        pub fn send(&self, message: ClientMessage) {
            self.connection
                .send(serde_json::to_string(&message).unwrap());
        }

        /// The next event for which `wanted` is true, skipping the others.
        /// Panics when the connection ends first or nothing comes in 10 s.
        pub fn wait(&mut self, wanted: impl Fn(&Event) -> bool) -> Event {
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                while let Some(event) = self.backlog.pop_front() {
                    if wanted(&event) {
                        return event;
                    }
                    match event {
                        Event::Opened => self.send(ClientMessage::Hello {
                            name: "test".into(),
                        }),
                        Event::Closed { error } => panic!("The connection ended: {error:?}"),
                        _ => {}
                    }
                }
                self.backlog.extend(self.connection.drain());
                assert!(Instant::now() < deadline, "server response timed out");
                if self.backlog.is_empty() {
                    std::thread::sleep(Duration::from_millis(2));
                }
            }
        }

        /// The next message that is not a Welcome and satisfies `wanted`.
        pub fn message(&mut self, wanted: impl Fn(&ServerMessage) -> bool) -> ServerMessage {
            let Event::Message(message) =
                self.wait(|event| matches!(event, Event::Message(message) if wanted(message)))
            else {
                unreachable!()
            };
            message
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::{SinkExt, StreamExt};
    use std::{net::SocketAddr, sync::atomic::AtomicUsize};
    use tokio::net::TcpListener;
    use tokio_tungstenite::{accept_async, accept_async_with_config};

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap()
    }

    fn listen(runtime: &tokio::runtime::Runtime) -> (TcpListener, SocketAddr) {
        let listener = runtime.block_on(TcpListener::bind("127.0.0.1:0")).unwrap();
        let address = listener.local_addr().unwrap();
        (listener, address)
    }

    fn endpoint(address: SocketAddr) -> String {
        format!("ws://{address}/ws")
    }

    fn fast() -> Limits {
        Limits {
            silence: Duration::from_millis(400),
            connect: Duration::from_secs(5),
        }
    }

    fn notice(text: &str) -> Message {
        Message::Text(
            serde_json::json!({"type": "notice", "message": text})
                .to_string()
                .into(),
        )
    }

    fn patch(revision: u64) -> Message {
        Message::Text(
            serde_json::json!({"type": "world_patch", "revision": revision, "edits": []})
                .to_string()
                .into(),
        )
    }

    fn snapshot(tick: u64) -> Message {
        Message::Text(
            serde_json::json!({
                "type": "snapshot", "tick": tick, "revision": 0,
                "players": [], "projectiles": []
            })
            .to_string()
            .into(),
        )
    }

    /// Collects events until `done` returns true for one. Panics after 10 s.
    fn collect(connection: &Connection, mut done: impl FnMut(&Event) -> bool) -> Vec<Event> {
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut all = Vec::new();
        loop {
            for event in connection.drain() {
                let finished = done(&event);
                all.push(event);
                if finished {
                    return all;
                }
            }
            assert!(Instant::now() < deadline, "no such event: {all:?}");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn wait_until(what: &str, mut condition: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !condition() {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn is_opened(event: &Event) -> bool {
        matches!(event, Event::Opened)
    }

    fn is_closed(event: &Event) -> bool {
        matches!(event, Event::Closed { .. })
    }

    #[test]
    fn connecting_delivers_opened_then_messages_in_order() {
        let runtime = runtime();
        let (listener, address) = listen(&runtime);
        runtime.spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            let mut socket = accept_async(tcp).await.unwrap();
            for revision in 1..=50 {
                socket.send(patch(revision)).await.unwrap();
            }
            std::future::pending::<()>().await;
        });
        let connection = Connection::open(&endpoint(address)).unwrap();
        let mut revisions = Vec::new();
        let events = collect(&connection, |event| {
            if let Event::Message(ServerMessage::WorldPatch { revision, .. }) = event {
                revisions.push(*revision);
            }
            revisions.len() == 50
        });
        assert!(is_opened(&events[0]));
        assert_eq!(revisions, (1..=50).collect::<Vec<_>>());
    }

    #[test]
    fn a_welcome_is_decoded_into_plain_data_whatever_its_key_order() {
        let welcome = serde_json::json!({
            "type": "welcome", "id": 7, "revision": 3, "format_version": 2,
            "cells": [], "objects": [], "players": [], "editing_allowed": true
        });
        let Event::Welcome(plain) = decode(&welcome.to_string()).unwrap() else {
            panic!("a welcome")
        };
        assert_eq!((plain.id, plain.revision, plain.format_version), (7, 3, 2));
        assert!(plain.editing_allowed);
        let shuffled = r#"{"id":9,"revision":1,"cells":[],"objects":[],"players":[],"editing_allowed":false,"type":"welcome"}"#;
        let Event::Welcome(plain) = decode(shuffled).unwrap() else {
            panic!("a welcome")
        };
        assert_eq!((plain.id, plain.format_version), (9, 1));
        assert!(matches!(
            decode(patch(4).into_text().unwrap().as_str()).unwrap(),
            Event::Message(ServerMessage::WorldPatch { revision: 4, .. })
        ));
        assert!(decode("not json").is_err());
    }

    #[test]
    fn only_the_newest_snapshot_waits_and_it_keeps_its_place() {
        let runtime = runtime();
        let (listener, address) = listen(&runtime);
        let sent = Arc::new(AtomicBool::new(false));
        let done = sent.clone();
        runtime.spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            let mut socket = accept_async(tcp).await.unwrap();
            let mut tick = 0;
            for revision in 1..=3 {
                socket.send(patch(revision)).await.unwrap();
                for _ in 0..100 {
                    tick += 1;
                    socket.send(snapshot(tick)).await.unwrap();
                }
            }
            socket.send(notice("last")).await.unwrap();
            done.store(true, Ordering::SeqCst);
            std::future::pending::<()>().await;
        });
        let connection = Connection::open(&endpoint(address)).unwrap();
        wait_until("the server", || sent.load(Ordering::SeqCst));
        // Let the network thread read all of it before the frame thread looks.
        std::thread::sleep(Duration::from_millis(400));
        let events = connection.drain();
        let shape: Vec<String> = events
            .iter()
            .map(|event| match event {
                Event::Opened => "opened".into(),
                Event::Message(ServerMessage::WorldPatch { revision, .. }) => {
                    format!("patch {revision}")
                }
                Event::Message(ServerMessage::Snapshot { tick, .. }) => format!("snapshot {tick}"),
                Event::Message(ServerMessage::Notice { message }) => message.clone(),
                other => format!("{other:?}"),
            })
            .collect();
        assert_eq!(
            shape,
            [
                "opened",
                "patch 1",
                "patch 2",
                "patch 3",
                "snapshot 300",
                "last"
            ]
            .map(String::from)
        );
    }

    #[test]
    fn a_stalled_frame_thread_backs_up_into_the_connection_without_losing_messages() {
        const COUNT: usize = 3000;
        let runtime = runtime();
        let (listener, address) = listen(&runtime);
        let finished = Arc::new(AtomicBool::new(false));
        let heard = Arc::new(AtomicBool::new(false));
        let (done, ping) = (finished.clone(), heard.clone());
        runtime.spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            let (mut sink, mut source) = accept_async(tcp).await.unwrap().split();
            tokio::spawn(async move {
                while let Some(Ok(message)) = source.next().await {
                    if message.is_text() {
                        ping.store(true, Ordering::SeqCst);
                    }
                }
            });
            let padding = "x".repeat(8 * 1024);
            for number in 0..COUNT {
                sink.send(notice(&format!("{number}:{padding}")))
                    .await
                    .unwrap();
            }
            done.store(true, Ordering::SeqCst);
            std::future::pending::<()>().await;
        });
        let connection = Connection::open(&endpoint(address)).unwrap();
        // The frame thread does not look for a while.
        std::thread::sleep(Duration::from_millis(600));
        assert!(
            !finished.load(Ordering::SeqCst),
            "the server should still be waiting to write"
        );
        // Sending needs no reads.
        connection.send("{}".into());
        wait_until("the server to hear the player", || {
            heard.load(Ordering::SeqCst)
        });
        let first = connection.drain();
        assert!(
            first.len() <= QUEUE_LIMIT + 1,
            "{} events queued",
            first.len()
        );
        let mut numbers = Vec::new();
        let take = |events: Vec<Event>, numbers: &mut Vec<usize>| {
            for event in events {
                if let Event::Message(ServerMessage::Notice { message }) = event {
                    numbers.push(message.split(':').next().unwrap().parse::<usize>().unwrap());
                }
            }
        };
        take(first, &mut numbers);
        let deadline = Instant::now() + Duration::from_secs(20);
        while numbers.len() < COUNT {
            take(connection.drain(), &mut numbers);
            assert!(Instant::now() < deadline, "only {} arrived", numbers.len());
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(numbers, (0..COUNT).collect::<Vec<_>>());
    }

    #[test]
    fn messages_are_sent_at_once_while_nothing_is_read() {
        let runtime = runtime();
        let (listener, address) = listen(&runtime);
        let received = Arc::new(Mutex::new(Vec::<(String, Instant)>::new()));
        let log = received.clone();
        runtime.spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            let mut socket = accept_async(tcp).await.unwrap();
            while let Some(Ok(message)) = socket.next().await {
                if let Message::Text(text) = message {
                    log.lock().unwrap().push((text.to_string(), Instant::now()));
                }
            }
        });
        let connection = Connection::open(&endpoint(address)).unwrap();
        wait_until("the connection", || connection.tcp_nodelay().is_some());
        let sent = Instant::now();
        for text in ["one", "two", "three"] {
            connection.send(text.into());
        }
        wait_until("the messages", || received.lock().unwrap().len() == 3);
        let log = received.lock().unwrap();
        let texts: Vec<&str> = log.iter().map(|(text, _)| text.as_str()).collect();
        assert_eq!(texts, ["one", "two", "three"]);
        assert!(log[2].1 - sent < Duration::from_millis(200));
    }

    #[test]
    fn a_closed_connection_reports_it_and_a_new_one_reaches_the_server_again() {
        let runtime = runtime();
        let (listener, address) = listen(&runtime);
        let accepted = Arc::new(AtomicUsize::new(0));
        let count = accepted.clone();
        runtime.spawn(async move {
            loop {
                let (tcp, _) = listener.accept().await.unwrap();
                let number = count.fetch_add(1, Ordering::SeqCst) + 1;
                let mut socket = accept_async(tcp).await.unwrap();
                socket
                    .send(notice(&format!("hello {number}")))
                    .await
                    .unwrap();
                if number == 1 {
                    socket.close(None).await.unwrap();
                }
                tokio::spawn(async move { while socket.next().await.is_some() {} });
            }
        });
        let connection = Connection::open(&endpoint(address)).unwrap();
        let events = collect(&connection, is_closed);
        assert!(matches!(
            events.as_slice(),
            [
                Event::Opened,
                Event::Message(ServerMessage::Notice { .. }),
                Event::Closed { error: None }
            ]
        ));
        drop(connection);
        let again = Connection::open(&endpoint(address)).unwrap();
        let events = collect(&again, |event| {
            matches!(event, Event::Message(ServerMessage::Notice { .. }))
        });
        assert!(
            matches!(&events[1], Event::Message(ServerMessage::Notice { message }) if message == "hello 2")
        );
        assert_eq!(accepted.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn a_server_that_goes_quiet_ends_the_connection() {
        let runtime = runtime();
        let (listener, address) = listen(&runtime);
        runtime.spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            let mut socket = accept_async(tcp).await.unwrap();
            socket.send(notice("then silence")).await.unwrap();
            std::future::pending::<()>().await;
        });
        let started = Instant::now();
        let connection = Connection::open_with(&endpoint(address), fast(), None).unwrap();
        let events = collect(&connection, is_closed);
        let Some(Event::Closed { error: Some(error) }) = events.last() else {
            panic!("closed with an error: {events:?}")
        };
        assert!(error.contains("No data"), "{error}");
        let waited = started.elapsed();
        assert!(waited >= Duration::from_millis(400) && waited < Duration::from_secs(3));
    }

    #[test]
    fn a_server_that_keeps_talking_is_not_silent() {
        let runtime = runtime();
        let (listener, address) = listen(&runtime);
        let talking = Arc::new(AtomicBool::new(true));
        let flag = talking.clone();
        runtime.spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            let mut socket = accept_async(tcp).await.unwrap();
            for _ in 0..15 {
                socket.send(notice("still here")).await.unwrap();
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            flag.store(false, Ordering::SeqCst);
            std::future::pending::<()>().await;
        });
        let connection = Connection::open_with(&endpoint(address), fast(), None).unwrap();
        let mut notices = 0;
        let events = collect(&connection, |event| {
            notices += matches!(event, Event::Message(_)) as usize;
            is_closed(event)
        });
        assert!(
            !talking.load(Ordering::SeqCst),
            "ended while the server was talking: {events:?}"
        );
        assert_eq!(notices, 15);
    }

    #[test]
    fn a_slow_upgrade_is_not_silence() {
        let runtime = runtime();
        let (listener, address) = listen(&runtime);
        runtime.spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            // Longer than the silence limit, like a host loading its world.
            tokio::time::sleep(Duration::from_millis(1500)).await;
            let mut socket = accept_async(tcp).await.unwrap();
            socket.send(notice("loaded")).await.unwrap();
            std::future::pending::<()>().await;
        });
        let connection = Connection::open_with(&endpoint(address), fast(), None).unwrap();
        let events = collect(&connection, |event| {
            matches!(event, Event::Message(ServerMessage::Notice { .. }))
        });
        assert!(is_opened(&events[0]), "{events:?}");
        assert!(!events.iter().any(is_closed));
    }

    #[test]
    fn an_upgrade_that_never_comes_ends_at_the_connect_limit() {
        let runtime = runtime();
        let (listener, address) = listen(&runtime);
        runtime.spawn(async move {
            let (_tcp, _) = listener.accept().await.unwrap();
            std::future::pending::<()>().await;
        });
        let limits = Limits {
            silence: Duration::from_millis(200),
            connect: Duration::from_millis(900),
        };
        let started = Instant::now();
        let connection = Connection::open_with(&endpoint(address), limits, None).unwrap();
        let events = collect(&connection, is_closed);
        let Some(Event::Closed { error: Some(error) }) = events.last() else {
            panic!("closed with an error: {events:?}")
        };
        assert!(error.contains("Connecting"), "{error}");
        let waited = started.elapsed();
        assert!(waited >= Duration::from_millis(900) && waited < Duration::from_secs(4));
    }

    #[test]
    fn a_refused_connection_and_a_bad_address_say_why() {
        let runtime = runtime();
        let (listener, address) = listen(&runtime);
        drop(listener);
        let connection = Connection::open(&endpoint(address)).unwrap();
        let events = collect(&connection, is_closed);
        assert!(matches!(
            events.last(),
            Some(Event::Closed { error: Some(_) })
        ));
        assert!(Connection::open("http://example.com").is_err());
        assert!(Connection::open("not an address").is_err());
    }

    #[test]
    fn dropping_the_connection_closes_the_socket_before_it_returns() {
        let runtime = runtime();
        let (listener, address) = listen(&runtime);
        let ended = Arc::new(AtomicBool::new(false));
        let flag = ended.clone();
        runtime.spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            let mut socket = accept_async(tcp).await.unwrap();
            while let Some(Ok(_)) = socket.next().await {}
            flag.store(true, Ordering::SeqCst);
        });
        let connection = Connection::open(&endpoint(address)).unwrap();
        collect(&connection, is_opened);
        let started = Instant::now();
        drop(connection);
        assert!(started.elapsed() < Duration::from_secs(1));
        wait_until("the server to see the close", || {
            ended.load(Ordering::SeqCst)
        });
    }

    #[test]
    fn dropping_a_connection_that_is_still_connecting_does_not_wait_for_the_limit() {
        let runtime = runtime();
        let (listener, address) = listen(&runtime);
        runtime.spawn(async move {
            let (_tcp, _) = listener.accept().await.unwrap();
            std::future::pending::<()>().await;
        });
        let connection = Connection::open(&endpoint(address)).unwrap();
        std::thread::sleep(Duration::from_millis(100));
        let started = Instant::now();
        drop(connection);
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn a_message_beyond_the_old_frame_limit_arrives_and_its_bytes_are_counted() {
        let runtime = runtime();
        let (listener, address) = listen(&runtime);
        const SIZE: usize = 20 * 1024 * 1024;
        runtime.spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            let mut socket = accept_async(tcp).await.unwrap();
            socket.send(notice(&"x".repeat(SIZE))).await.unwrap();
            std::future::pending::<()>().await;
        });
        let connection = Connection::open(&endpoint(address)).unwrap();
        let events = collect(&connection, |event| {
            matches!(event, Event::Message(ServerMessage::Notice { .. }))
        });
        let Some(Event::Message(ServerMessage::Notice { message })) = events.last() else {
            unreachable!()
        };
        assert_eq!(message.len(), SIZE);
        assert!(connection.bytes_received() > SIZE as u64);
    }

    #[test]
    fn the_clients_socket_has_nagles_algorithm_off() {
        let runtime = runtime();
        let (listener, address) = listen(&runtime);
        runtime.spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            let _socket = accept_async(tcp).await.unwrap();
            std::future::pending::<()>().await;
        });
        let connection = Connection::open(&endpoint(address)).unwrap();
        collect(&connection, is_opened);
        assert_eq!(connection.tcp_nodelay(), Some(true));
    }

    struct Authority {
        certificate: rcgen::Certificate,
        key: rcgen::KeyPair,
    }

    fn authority() -> Authority {
        let key = rcgen::KeyPair::generate().unwrap();
        let mut params = rcgen::CertificateParams::new(Vec::new()).unwrap();
        params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        params.key_usages = vec![
            rcgen::KeyUsagePurpose::KeyCertSign,
            rcgen::KeyUsagePurpose::CrlSign,
        ];
        let certificate = params.self_signed(&key).unwrap();
        Authority { certificate, key }
    }

    fn tls_server(authority: &Authority) -> tokio_rustls::TlsAcceptor {
        let key = rcgen::KeyPair::generate().unwrap();
        let params = rcgen::CertificateParams::new(vec!["localhost".to_string()]).unwrap();
        let leaf = params
            .signed_by(&key, &authority.certificate, &authority.key)
            .unwrap();
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let config = rustls::ServerConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(
                vec![leaf.der().clone()],
                rustls::pki_types::PrivateKeyDer::try_from(key.serialize_der()).unwrap(),
            )
            .unwrap();
        tokio_rustls::TlsAcceptor::from(Arc::new(config))
    }

    fn trusting(authority: &Authority) -> Arc<rustls::ClientConfig> {
        let mut roots = rustls::RootCertStore::empty();
        roots.add(authority.certificate.der().clone()).unwrap();
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        Arc::new(
            rustls::ClientConfig::builder_with_provider(provider)
                .with_safe_default_protocol_versions()
                .unwrap()
                .with_root_certificates(roots)
                .with_no_client_auth(),
        )
    }

    #[test]
    fn a_wss_handshake_succeeds_against_a_server_with_a_trusted_certificate() {
        let runtime = runtime();
        let (listener, address) = listen(&runtime);
        let authority = authority();
        let acceptor = tls_server(&authority);
        runtime.spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            let tls = acceptor.accept(tcp).await.unwrap();
            let mut socket = accept_async_with_config(tls, None).await.unwrap();
            socket.send(notice("over tls")).await.unwrap();
            let _ = socket.next().await;
        });
        let connection = Connection::open_with(
            &format!("wss://localhost:{}/ws", address.port()),
            Limits::default(),
            Some(trusting(&authority)),
        )
        .unwrap();
        let events = collect(&connection, |event| {
            matches!(event, Event::Message(ServerMessage::Notice { .. }))
        });
        assert!(is_opened(&events[0]));
        assert!(matches!(
            events.last(),
            Some(Event::Message(ServerMessage::Notice { message })) if message == "over tls"
        ));
        assert_eq!(connection.tcp_nodelay(), Some(true));
    }

    #[test]
    fn a_wss_address_never_stays_connecting_when_it_cannot_be_trusted_or_is_plain() {
        let runtime = runtime();
        let authority = authority();
        let acceptor = tls_server(&authority);
        let (listener, address) = listen(&runtime);
        runtime.spawn(async move {
            loop {
                let (tcp, _) = listener.accept().await.unwrap();
                let acceptor = acceptor.clone();
                tokio::spawn(async move {
                    let _ = acceptor.accept(tcp).await;
                });
            }
        });
        // The built-in roots do not know the test authority. This also checks
        // that the default configuration builds without a process-wide
        // crypto provider.
        let connection =
            Connection::open(&format!("wss://localhost:{}/ws", address.port())).unwrap();
        let events = collect(&connection, is_closed);
        assert!(matches!(
            events.last(),
            Some(Event::Closed { error: Some(_) })
        ));
        assert!(!events.iter().any(is_opened));

        let (listener, address) = listen(&runtime);
        runtime.spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            let _ = accept_async(tcp).await;
        });
        let connection =
            Connection::open(&format!("wss://127.0.0.1:{}/ws", address.port())).unwrap();
        let events = collect(&connection, is_closed);
        assert!(matches!(
            events.last(),
            Some(Event::Closed { error: Some(_) })
        ));
        assert!(!events.iter().any(is_opened));
    }

    /// Joins a running server and reports how much memory the join took.
    /// Start a server on a big world, then run, for example:
    /// `JOIN_ENDPOINT=ws://127.0.0.1:3000/ws /usr/bin/time -l target/release/deps/sauerteig_client-... measure_join --ignored --nocapture`
    #[test]
    #[ignore = "needs a running server; see the doc comment"]
    fn measure_join() {
        let endpoint = std::env::var("JOIN_ENDPOINT").expect("JOIN_ENDPOINT");
        let start = Instant::now();
        let (client, welcome) = testing::Client::join(&endpoint);
        let welcome_at = start.elapsed();
        let mut world = sauerteig_core::World::from_compact_parts(
            welcome.bounds,
            &welcome.regions,
            &welcome.cells,
            &welcome.roots,
        )
        .unwrap();
        world.replace_volumes(&welcome.volumes).unwrap();
        println!(
            "welcome after {welcome_at:?}, world built after {:?}, {} bytes read",
            start.elapsed(),
            client.connection.bytes_received()
        );
    }
}
