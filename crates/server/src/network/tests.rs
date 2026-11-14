use super::*;
use crate::{
    bots::Bot,
    persistence::{write_world, SavedWorld},
};
use sauerteig_core::{Cell, CellEdit, WorldBounds, WORLD_FORMAT_VERSION};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

struct TestRoom {
    directory: std::path::PathBuf,
    endpoint: String,
    stop: oneshot::Sender<()>,
    server: tokio::task::JoinHandle<Result<(), String>>,
    simulation: Handle,
}
impl TestRoom {
    async fn start(cells: usize) -> Self {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let nonce = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let directory = std::env::temp_dir().join(format!(
            "sauerteig-owner-{}-{stamp}-{nonce}",
            std::process::id()
        ));
        let saved = SavedWorld {
            revision: 0,
            format_version: WORLD_FORMAT_VERSION,
            bounds: WorldBounds {
                min: [0, 0, 0],
                max: [128, 128, 128],
            },
            regions: vec![],
            cells: (0..cells)
                .map(|i| CellEdit {
                    pos: [(i % 64) as i32, (i / 4096) as i32, (i / 64 % 64) as i32],
                    cell: Some(Cell::solid(0)),
                })
                .collect(),
            roots: vec![],
            volumes: vec![],
            objects: vec![],
            next_object: 1,
        };
        write_world(&directory.join("world.json"), &saved).unwrap();
        let config = Config {
            world_path: directory.join("world.json"),
            ..Config::default()
        };
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("ws://{}/ws", listener.local_addr().unwrap());
        let (stop, stopped) = oneshot::channel();
        let (control, ready) = oneshot::channel();
        let server = tokio::spawn(serve_inner(
            listener,
            config,
            async {
                let _ = stopped.await;
            },
            Some(control),
        ));
        let simulation = ready.await.unwrap();
        Self {
            directory,
            endpoint,
            stop,
            server,
            simulation,
        }
    }
    async fn finish(self) {
        let directory = self.directory.clone();
        self.finish_keeping_files().await;
        std::fs::remove_dir_all(directory).unwrap();
    }
    async fn finish_keeping_files(self) {
        let _ = self.stop.send(());
        tokio::time::timeout(Duration::from_secs(10), self.server)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    }
    async fn health(&self) -> serde_json::Value {
        // A real published /health read, rather than a private Session lookup.
        let address = self
            .endpoint
            .trim_start_matches("ws://")
            .trim_end_matches("/ws");
        let mut socket = tokio::net::TcpStream::connect(address).await.unwrap();
        socket
            .write_all(b"GET /health HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
            .await
            .unwrap();
        let mut bytes = Vec::new();
        socket.read_to_end(&mut bytes).await.unwrap();
        let response = String::from_utf8(bytes).unwrap();
        let json = response.split("\r\n\r\n").nth(1).unwrap();
        serde_json::from_str(json).unwrap()
    }
    async fn tick(&self) -> u64 {
        self.health().await["tick"].as_u64().unwrap()
    }
}
async fn small_edit(bot: &mut Bot, request: u64) {
    bot.send(ClientMessage::Edit {
        request,
        edits: vec![CellEdit {
            pos: [100, 20, 100],
            cell: Some(Cell::solid((request % 8) as u8)),
        }],
    })
    .await
    .unwrap();
    let result = bot
        .until(|m| matches!(m,ServerMessage::EditResult {request:r,..} if *r==request))
        .await
        .unwrap();
    assert!(
        matches!(result, ServerMessage::EditResult { error: None, .. }),
        "{result:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn progressing_slow_join_keeps_ticks_and_receives_more_than_64_edits_in_order() {
    let room = TestRoom::start(32_768).await;
    let mut builder = Bot::join(&room.endpoint).await.unwrap();
    builder.edit_mode().await.unwrap();
    let proxy = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let slow_endpoint = format!("ws://{}/ws", proxy.local_addr().unwrap());
    let address = room
        .endpoint
        .trim_start_matches("ws://")
        .trim_end_matches("/ws")
        .to_owned();
    let proxy_task = tokio::spawn(async move {
        let (mut downstream, _) = proxy.accept().await.unwrap();
        let mut upstream = tokio::net::TcpStream::connect(address).await.unwrap();
        let (mut dr, mut dw) = downstream.split();
        let (mut ur, mut uw) = upstream.split();
        tokio::select! {
            _=tokio::io::copy(&mut dr,&mut uw)=>{},
            _=async {
                let mut buffer=[0;16*1024];
                loop {let n=ur.read(&mut buffer).await.unwrap_or(0);if n==0 {break;}
                    if dw.write_all(&buffer[..n]).await.is_err() {break;}
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            }=>{},
        }
    });
    let started = Instant::now();
    let joining = tokio::spawn(async move { Bot::join(&slow_endpoint).await });
    // Wait for the simulation's peer admission, before sending revisions.
    tokio::time::timeout(Duration::from_secs(2), async {
        while room.health().await["players"] != 2 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let measuring = Instant::now();
    let tick = room.tick().await;
    for request in 1..=80 {
        small_edit(&mut builder, request).await;
    }
    let mut joiner = tokio::time::timeout(Duration::from_secs(25), joining)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(
        started.elapsed() > Duration::from_secs(5),
        "transfer must exceed the old whole-message timeout"
    );
    let elapsed = measuring.elapsed().as_secs_f64();
    let advanced = room.tick().await - tick;
    assert!(
        advanced as f64 >= elapsed * 50.0,
        "{advanced} ticks in {elapsed:.2}s while transferring"
    );
    assert_eq!(joiner.revision, 0, "Welcome precedes edits made in flight");
    for expected in 1..=80 {
        let patch = joiner
            .until(|m| matches!(m, ServerMessage::WorldPatch { .. }))
            .await
            .unwrap();
        assert!(matches!(patch,ServerMessage::WorldPatch {revision,..} if revision==expected));
    }
    joiner.socket.close(None).await.unwrap();
    builder.socket.close(None).await.unwrap();
    proxy_task.abort();
    room.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn simulation_panic_closes_connections_reports_failure_and_preserves_save() {
    let room = TestRoom::start(256).await;
    let original = std::fs::read(room.directory.join("world.json")).unwrap();
    let mut bot = Bot::join(&room.endpoint).await.unwrap();
    bot.edit_mode().await.unwrap();
    small_edit(&mut bot, 1).await;
    room.simulation.send(Command::Panic).unwrap();
    let error = tokio::time::timeout(
        Duration::from_secs(2),
        bot.until(|m| matches!(m, ServerMessage::Error { .. })),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(
        matches!(error,ServerMessage::Error {message} if message.contains("simulation failed") && message.contains("last saved world"))
    );
    tokio::time::timeout(Duration::from_secs(2), async {
        while let Some(packet) = bot.socket.next().await {
            if matches!(
                packet,
                Ok(tokio_tungstenite::tungstenite::Message::Close(_))
            ) {
                return;
            }
        }
    })
    .await
    .unwrap();
    let result = tokio::time::timeout(Duration::from_secs(2), room.server)
        .await
        .unwrap()
        .unwrap();
    assert!(result.unwrap_err().contains("simulation failed"));
    assert_eq!(
        std::fs::read(room.directory.join("world.json")).unwrap(),
        original
    );
    std::fs::remove_dir_all(room.directory).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn large_save_serialization_does_not_own_the_simulation_clock() {
    let room = TestRoom::start(32_768).await;
    let snapshot = room
        .simulation
        .request(|reply| Command::Save { force: true, reply })
        .await
        .unwrap()
        .unwrap();
    let tick = room.tick().await;
    let started = Instant::now();
    let task = tokio::task::spawn_blocking(move || {
        struct SlowWriter {
            bytes: usize,
            next_pause: usize,
        }
        impl std::io::Write for SlowWriter {
            fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
                self.bytes += b.len();
                if self.bytes >= self.next_pause {
                    std::thread::sleep(Duration::from_millis(20));
                    self.next_pause = self.bytes + 32 * 1024;
                }
                Ok(b.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let saved = snapshot.expand();
        let mut writer = SlowWriter {
            bytes: 0,
            next_pause: 32 * 1024,
        };
        serde_json::to_writer(&mut writer, &saved).unwrap();
        assert!(writer.bytes > 1024 * 1024);
    });
    task.await.unwrap();
    let elapsed = started.elapsed().as_secs_f64();
    let ticks = room.tick().await - tick;
    assert!(elapsed > 1.0);
    assert!(
        ticks as f64 >= elapsed * 50.0,
        "{ticks} ticks in {elapsed:.2}s during serialization"
    );
    room.finish().await;
}

#[tokio::test]
async fn write_deadline_tracks_progress_instead_of_total_transfer_time() {
    let progress = Progress::new();
    let started = Instant::now();
    drain_while_progressing(
        async {
            for _ in 0..6 {
                tokio::time::sleep(Duration::from_millis(30)).await;
                progress.wrote();
            }
            Ok(())
        },
        &progress,
        Duration::from_millis(90),
    )
    .await
    .unwrap();
    assert!(started.elapsed() > Duration::from_millis(90));
    let error =
        drain_while_progressing(std::future::pending(), &progress, Duration::from_millis(40))
            .await
            .unwrap_err();
    assert!(error.contains("stalled"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failed_owner_cancels_the_atomic_save_replacement() {
    let room = TestRoom::start(64).await;
    let original = std::fs::read(room.directory.join("world.json")).unwrap();
    let mut snapshot = room
        .simulation
        .request(|reply| Command::Save { force: true, reply })
        .await
        .unwrap()
        .unwrap();
    snapshot.revision += 1;
    let path = room.directory.join("world.json");
    assert!(crate::persistence::write_snapshot(&path, &snapshot, || false).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), original);
    assert!(!path.with_extension("json.tmp").exists());
    room.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_builders_movement_continues_during_their_own_paste_and_sync_waits() {
    let room = TestRoom::start(256).await;
    let mut bot = Bot::join(&room.endpoint).await.unwrap();
    bot.edit_mode().await.unwrap();
    bot.send(ClientMessage::EditRoots {
        request: 1,
        base_revision: 0,
        roots: crate::bots::largest_blueprint(bot.bounds, 1),
    })
    .await
    .unwrap();
    bot.send(ClientMessage::Save).await.unwrap();
    bot.send(ClientMessage::Sync { request: 2 }).await.unwrap();
    bot.send(ClientMessage::Input {
        input: sauerteig_core::InputFrame {
            seq: 1,
            forward: 1.0,
            ..Default::default()
        },
    })
    .await
    .unwrap();
    bot.send(ClientMessage::SetMode {
        request: 2,
        editing: false,
    })
    .await
    .unwrap();
    bot.send(ClientMessage::Input {
        input: sauerteig_core::InputFrame {
            seq: 2,
            forward: 1.0,
            ..Default::default()
        },
    })
    .await
    .unwrap();
    let mut moved_before_result = false;
    loop {
        match tokio::time::timeout(Duration::from_secs(30), bot.receive())
            .await
            .unwrap()
            .unwrap()
        {
            ServerMessage::Snapshot { players, .. } => {
                moved_before_result |= players.iter().any(|p| p.id == bot.id && p.last_input == 1);
                assert!(
                    !players.iter().any(|p| p.id == bot.id && p.last_input == 2),
                    "new-mode input overtook its mode change"
                );
            }
            ServerMessage::EditResult { request: 1, error } => {
                assert!(error.is_none(), "{error:?}");
                break;
            }
            ServerMessage::Synced { .. } => {
                panic!("Sync acknowledged an edit still being prepared")
            }
            _ => {}
        }
    }
    assert!(
        moved_before_result,
        "movement cannot wait for the player's own geometry compiler"
    );
    bot.until(|m| matches!(m, ServerMessage::Synced { request: 2 }))
        .await
        .unwrap();
    bot.until(|m| {
        matches!(
            m,
            ServerMessage::ModeChanged {
                request: 2,
                editing: false
            }
        )
    })
    .await
    .unwrap();
    let id = bot.id;
    bot.until(|m|matches!(m,ServerMessage::Snapshot {players,..} if players.iter().any(|p|p.id==id && p.last_input==2 && !p.editing))).await.unwrap();
    bot.socket.close(None).await.unwrap();
    room.finish().await;
}

async fn wait_for_revision(room: &TestRoom, revision: u64) {
    let mut health = serde_json::Value::Null;
    let reached = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            health = room.health().await;
            if health["revision"].as_u64() == Some(revision) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    assert!(
        reached.is_ok(),
        "revision {revision} was never reached; the room reported {health}"
    );
}
async fn wait_for_players(room: &TestRoom, players: u64) {
    tokio::time::timeout(Duration::from_secs(30), async {
        while room.health().await["players"].as_u64() != Some(players) {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("{players} players never remained"));
}
fn blueprint(bot: &Bot, roots: usize) -> Vec<sauerteig_core::RootEdit> {
    crate::bots::largest_blueprint(bot.bounds, 1)
        .into_iter()
        .take(roots)
        .collect()
}
async fn saved(room: &TestRoom) -> SavedWorld {
    room.simulation
        .request(|reply| Command::Save { force: true, reply })
        .await
        .unwrap()
        .unwrap()
        .expand()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_full_queue_answers_the_big_edit_busy_without_holding_anyone_else_or_dropping_the_laggard(
) {
    let room = TestRoom::start(256).await;
    let mut builder = Bot::join(&room.endpoint).await.unwrap();
    builder.edit_mode().await.unwrap();
    let mut neighbour = Bot::join(&room.endpoint).await.unwrap();
    neighbour.edit_mode().await.unwrap();
    let mut laggard = Bot::join(&room.endpoint).await.unwrap();
    // The laggard's socket has stopped draining: its queue is full but for one
    // mebibyte, and the writer is stuck behind the filler.
    let release = room
        .simulation
        .request(|reply| Command::Stall {
            id: laggard.id,
            leave: 1 << 20,
            reply,
        })
        .await
        .unwrap()
        .expect("the laggard is in the room");
    // About 3 MiB to publish: it cannot fit until the laggard drains.
    let roots = blueprint(&builder, 1024);
    let sent = Instant::now();
    builder
        .send(ClientMessage::EditRoots {
            request: 1,
            base_revision: 0,
            roots,
        })
        .await
        .unwrap();
    let (busy, small) = tokio::join!(
        async {
            let result = builder
                .until(|m| matches!(m, ServerMessage::EditResult { request: 1, .. }))
                .await
                .unwrap();
            (result, sent.elapsed())
        },
        async {
            // This one fits, and waits in the worker's one line behind the big one.
            tokio::time::sleep(Duration::from_millis(300)).await;
            neighbour
                .send(ClientMessage::Edit {
                    request: 2,
                    edits: vec![CellEdit {
                        pos: [100, 20, 100],
                        cell: Some(Cell::solid(1)),
                    }],
                })
                .await
                .unwrap();
            neighbour
                .send(ClientMessage::Sync { request: 3 })
                .await
                .unwrap();
            let result = neighbour
                .until(|m| matches!(m, ServerMessage::EditResult { request: 2, .. }))
                .await
                .unwrap();
            neighbour
                .until(|m| matches!(m, ServerMessage::Synced { request: 3 }))
                .await
                .unwrap();
            (result, sent.elapsed())
        }
    );
    let (busy, waited) = busy;
    match busy {
        ServerMessage::EditResult {
            error: Some(error), ..
        } => assert!(error.contains("try again in a moment"), "{error}"),
        other => panic!("expected the busy answer, got {other:?}"),
    }
    assert!(
        waited >= Duration::from_millis(1900),
        "the edit gives the laggard time to catch up first: {waited:?}"
    );
    assert!(
        waited < Duration::from_secs(8),
        "the wait is bounded: {waited:?}"
    );
    let (small, finished) = small;
    assert!(
        matches!(small, ServerMessage::EditResult { error: None, .. }),
        "{small:?}"
    );
    assert!(
        finished < Duration::from_secs(10),
        "a small edit and its Sync wait only for the bounded one: {finished:?}"
    );
    // Only the small edit committed, and the laggard still holds its place.
    wait_for_revision(&room, 1).await;
    assert_eq!(room.health().await["players"], 3);
    // Once its socket drains again it receives every revision it is owed, in order.
    drop(release);
    let patch = laggard
        .until(|m| matches!(m, ServerMessage::WorldPatch { .. }))
        .await
        .unwrap();
    assert!(matches!(
        patch,
        ServerMessage::WorldPatch { revision: 1, .. }
    ));
    for bot in [&mut builder, &mut neighbour, &mut laggard] {
        bot.socket.close(None).await.unwrap();
    }
    room.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_edit_in_flight_when_its_client_leaves_without_sync_still_commits() {
    let room = TestRoom::start(256).await;
    let mut builder = Bot::join(&room.endpoint).await.unwrap();
    builder.edit_mode().await.unwrap();
    let roots = blueprint(&builder, 2048);
    builder
        .send(ClientMessage::EditRoots {
            request: 1,
            base_revision: 0,
            roots,
        })
        .await
        .unwrap();
    // No Sync and no wait for the answer: the close arrives while the edit is
    // still being prepared.
    builder.socket.close(None).await.unwrap();
    drop(builder);
    wait_for_revision(&room, 1).await;
    assert_eq!(saved(&room).await.roots.len(), 2048);
    // The connection lingers only until its work is done.
    wait_for_players(&room, 0).await;
    room.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn commands_read_before_the_socket_ends_all_apply_in_order() {
    let room = TestRoom::start(256).await;
    let mut builder = Bot::join(&room.endpoint).await.unwrap();
    builder.edit_mode().await.unwrap();
    let roots = blueprint(&builder, 1024);
    builder
        .send(ClientMessage::EditRoots {
            request: 1,
            base_revision: 0,
            roots,
        })
        .await
        .unwrap();
    // Read while the edit is pending, so the server must hold it back: the
    // undo has nothing to undo until the edit has committed.
    builder.send(ClientMessage::Undo).await.unwrap();
    // The stream ends with no close frame and no answer awaited. A half close
    // keeps what was sent from being lost to a reset, unlike dropping the socket.
    if let tokio_tungstenite::MaybeTlsStream::Plain(tcp) = builder.socket.get_mut() {
        tcp.shutdown().await.unwrap();
    }
    wait_for_revision(&room, 2).await;
    assert!(
        saved(&room).await.roots.is_empty(),
        "the undo was sent after the edit, so it applies after it"
    );
    wait_for_players(&room, 0).await;
    drop(builder);
    room.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn commands_arriving_after_the_writer_fails_are_still_read_for_a_moment() {
    let room = TestRoom::start(256).await;
    let mut builder = Bot::join(&room.endpoint).await.unwrap();
    builder.edit_mode().await.unwrap();
    // The writer fails, as it does when a client has gone, but the socket still
    // delivers what the client had already sent.
    assert!(room
        .simulation
        .request(|reply| Command::Poison {
            id: builder.id,
            reply
        })
        .await
        .unwrap());
    tokio::time::sleep(Duration::from_millis(200)).await;
    let roots = blueprint(&builder, 1024);
    builder
        .send(ClientMessage::EditRoots {
            request: 1,
            base_revision: 0,
            roots,
        })
        .await
        .unwrap();
    builder.send(ClientMessage::Undo).await.unwrap();
    wait_for_revision(&room, 2).await;
    assert!(saved(&room).await.roots.is_empty());
    // The grace is short: the connection ends once its commands have.
    wait_for_players(&room, 0).await;
    room.finish().await;
}

#[tokio::test]
async fn a_refused_connection_hears_why_before_the_close_frame() {
    async fn goodbye(farewell: Farewell) -> Vec<Message> {
        let sent = Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut sink = Box::pin(futures_util::sink::unfold((), {
            let sent = sent.clone();
            move |(), message: Message| {
                let sent = sent.clone();
                async move {
                    sent.lock().unwrap().push(message);
                    Ok::<(), std::convert::Infallible>(())
                }
            }
        }));
        say_goodbye(&mut sink, farewell).await;
        let sent = sent.lock().unwrap().clone();
        sent
    }
    let reason = "This room is full (16 players).";
    match &goodbye(Farewell::Refused(reason.into())).await[..] {
        [Message::Text(text), Message::Close(Some(frame))] => {
            assert!(matches!(
                serde_json::from_str::<ServerMessage>(text.as_str()).unwrap(),
                ServerMessage::Error { message } if message == reason
            ));
            assert_eq!(frame.code, 1013);
            assert_eq!(frame.reason.as_str(), reason);
        }
        other => panic!("a full room must say why before it closes: {other:?}"),
    }
    match &goodbye(Farewell::Failed("broken".into())).await[..] {
        [Message::Text(_), Message::Close(Some(frame))] => assert_eq!(frame.code, 1011),
        other => panic!("{other:?}"),
    }
    assert!(matches!(
        &goodbye(Farewell::Normal).await[..],
        [Message::Close(None)]
    ));
}

fn modified(path: &std::path::Path) -> std::time::SystemTime {
    std::fs::metadata(path).unwrap().modified().unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_session_without_changes_leaves_the_world_file_untouched() {
    let room = TestRoom::start(64).await;
    let path = room.directory.join("world.json");
    let (bytes, stamp) = (std::fs::read(&path).unwrap(), modified(&path));
    // Long enough for the save worker's first poll to have looked.
    tokio::time::sleep(Duration::from_millis(1500)).await;
    room.finish_keeping_files().await;
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    assert_eq!(modified(&path), stamp);
    std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_edit_is_saved_by_the_final_save_when_the_server_stops_at_once() {
    let room = TestRoom::start(64).await;
    let path = room.directory.join("world.json");
    let stamp = modified(&path);
    let mut bot = Bot::join(&room.endpoint).await.unwrap();
    bot.edit_mode().await.unwrap();
    small_edit(&mut bot, 1).await;
    tokio::time::sleep(Duration::from_millis(30)).await;
    let revision = room.health().await["revision"].as_u64().unwrap();
    assert!(revision > 0);
    room.finish_keeping_files().await;
    assert_ne!(modified(&path), stamp);
    let saved = crate::persistence::load_world(&path).unwrap();
    assert_eq!(saved.revision, revision);
    std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_new_world_is_saved_once_when_the_server_starts() {
    for stop_at_once in [true, false] {
        let directory = std::env::temp_dir().join(format!(
            "sauerteig-first-save-{}-{stop_at_once}",
            std::process::id()
        ));
        let path = directory.join("world.json");
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let (stop, stopped) = oneshot::channel();
        let server = tokio::spawn(serve_inner(
            listener,
            Config {
                world_path: path.clone(),
                ..Config::default()
            },
            async {
                let _ = stopped.await;
            },
            None,
        ));
        if !stop_at_once {
            let deadline = Instant::now() + Duration::from_secs(5);
            while !path.is_file() {
                assert!(Instant::now() < deadline, "no first save while running");
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        }
        let _ = stop.send(());
        server.await.unwrap().unwrap();
        crate::persistence::load_world(&path).unwrap();
        assert!(path.is_file());
        std::fs::remove_dir_all(&directory).unwrap();
    }
}
