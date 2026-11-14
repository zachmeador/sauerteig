//! Fixed 80 ms delays in each direction over real localhost WebSockets.
//! This proves protocol/simulation behavior under injected latency, not WAN
//! conditions, packet loss tolerance, rendering performance, or combat feel.

use futures_util::{SinkExt, StreamExt};
use sauerteig_core::{
    Cell, CellEdit, ClientMessage, InputFrame, ObjectKind, Player, ServerMessage, Vec3, World,
    WorldObject,
};
use sauerteig_server::{serve, Config};
use std::{collections::BTreeSet, path::PathBuf, time::Duration};
use tokio::{
    net::TcpListener,
    sync::{mpsc, oneshot},
    task::JoinHandle,
    time::{sleep_until, Instant},
};
use tokio_tungstenite::tungstenite::Message;

const ONE_WAY: Duration = Duration::from_millis(80);

struct TemporaryWorld(PathBuf);

impl TemporaryWorld {
    fn new() -> Self {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = std::env::temp_dir().join(format!(
            "sauerteig-delayed-assets-{}-{stamp}",
            std::process::id()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        Self(directory)
    }
}

impl Drop for TemporaryWorld {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Timestamp at enqueue/arrival, so an 80 ms delay never becomes an 80 ms
/// per-message throttle. Network reads continue while delivery is delayed.
struct DelayedClient {
    outgoing: mpsc::UnboundedSender<(Instant, ClientMessage)>,
    incoming: mpsc::UnboundedReceiver<ServerMessage>,
    workers: Vec<JoinHandle<()>>,
}

impl DelayedClient {
    async fn connect(url: &str) -> Self {
        let (socket, _) = tokio_tungstenite::connect_async(url).await.unwrap();
        let (mut sink, mut stream) = socket.split();
        let (outgoing, mut pending) = mpsc::unbounded_channel::<(Instant, ClientMessage)>();
        let (arrived, mut delivery) = mpsc::unbounded_channel::<(Instant, ServerMessage)>();
        let (delivered, incoming) = mpsc::unbounded_channel();
        let transmit = tokio::spawn(async move {
            while let Some((deadline, message)) = pending.recv().await {
                sleep_until(deadline).await;
                sink.send(Message::Text(
                    serde_json::to_string(&message).unwrap().into(),
                ))
                .await
                .expect("client stays connected while transmitting");
            }
        });
        let receive = tokio::spawn(async move {
            while let Some(frame) = stream.next().await {
                if let Message::Text(text) = frame.expect("valid WebSocket frame") {
                    let message = serde_json::from_str(&text).unwrap();
                    if arrived.send((Instant::now() + ONE_WAY, message)).is_err() {
                        break;
                    }
                }
            }
        });
        let delay = tokio::spawn(async move {
            while let Some((deadline, message)) = delivery.recv().await {
                sleep_until(deadline).await;
                if delivered.send(message).is_err() {
                    break;
                }
            }
        });
        Self {
            outgoing,
            incoming,
            workers: vec![transmit, receive, delay],
        }
    }

    fn send(&self, message: ClientMessage) {
        self.outgoing
            .send((Instant::now() + ONE_WAY, message))
            .expect("outgoing delay queue open");
    }

    async fn receive(&mut self) -> ServerMessage {
        tokio::time::timeout(Duration::from_secs(5), self.incoming.recv())
            .await
            .expect("response before timeout")
            .expect("client stays connected while receiving")
    }
}

impl Drop for DelayedClient {
    fn drop(&mut self) {
        for worker in &self.workers {
            worker.abort();
        }
    }
}

#[derive(Default)]
struct Observed {
    id: u64,
    revision: u64,
    world: World,
    objects: Vec<WorldObject>,
    players: Vec<Player>,
    revisions: Vec<u64>,
    shot_owners: BTreeSet<u64>,
    damaged: BTreeSet<u64>,
    saw_score_and_respawn: bool,
    saw_rock_removed: bool,
    edit_acknowledged: bool,
}

impl Observed {
    fn apply(&mut self, message: &ServerMessage) {
        match message {
            ServerMessage::Welcome {
                id,
                revision,
                cells,
                roots,
                objects,
                players,
                ..
            } => {
                self.id = *id;
                self.revision = *revision;
                self.world = World::from_parts(cells, roots).unwrap();
                self.objects = objects.clone();
                self.players = players.clone();
            }
            ServerMessage::WorldPatch { revision, edits } => {
                self.world.apply_edits(edits);
                self.revision = *revision;
                self.revisions.push(*revision);
            }
            ServerMessage::RootPatch { revision, roots } => {
                self.world.apply_root_edits(roots).unwrap();
                self.revision = *revision;
                self.revisions.push(*revision);
            }
            ServerMessage::Objects { revision, objects } => {
                self.saw_rock_removed |= self.objects.iter().any(|o| o.kind == ObjectKind::Rock)
                    && !objects.iter().any(|o| o.kind == ObjectKind::Rock);
                self.objects = objects.clone();
                self.revision = *revision;
                self.revisions.push(*revision);
            }
            ServerMessage::Snapshot { players, .. } => {
                self.damaged.extend(
                    players
                        .iter()
                        .filter(|p| !p.editing && p.health < 100)
                        .map(|p| p.id),
                );
                self.saw_score_and_respawn |= players.iter().any(|p| p.score > 0)
                    && players
                        .iter()
                        .any(|p| self.damaged.contains(&p.id) && p.health == 100);
                self.players = players.clone();
            }
            ServerMessage::Shots { shots } => {
                self.shot_owners.extend(shots.iter().map(|s| s.owner));
            }
            ServerMessage::EditResult { request, error } => {
                assert!(error.is_none(), "edit {request} rejected: {error:?}");
                self.edit_acknowledged |= *request == 1;
            }
            ServerMessage::Error { message } => panic!("server rejected request: {message}"),
            _ => {}
        }
    }
}

async fn joined(url: &str) -> (DelayedClient, Observed) {
    let mut client = DelayedClient::connect(url).await;
    let mut state = Observed::default();
    loop {
        let message = client.receive().await;
        state.apply(&message);
        if matches!(message, ServerMessage::Welcome { .. }) {
            return (client, state);
        }
    }
}

fn assert_objects(actual: &[WorldObject], expected: &[WorldObject]) {
    assert_eq!(
        serde_json::to_value(actual).unwrap(),
        serde_json::to_value(expected).unwrap()
    );
}

#[tokio::test]
async fn textured_geometry_and_props_survive_combat_delayed_clients_and_reload() {
    let temporary = TemporaryWorld::new();
    let config = Config {
        world_path: temporary.0.join("world.json"),
        editing_allowed: true,
    };
    let mut expected_world = World::empty();
    for x in -16..16 {
        for z in -16..16 {
            expected_world.set([x, 0, z], Some(Cell::solid(0)));
        }
    }
    // ID 1 starts at +Z looking toward ID 2. Both are fighters; ID 2 fires
    // away from ID 1 so damage/respawn can be checked deterministically.
    let mut expected_objects: Vec<_> = [0.5, 6.5]
        .into_iter()
        .enumerate()
        .map(|(i, z)| WorldObject {
            id: i as u32 + 1,
            kind: ObjectKind::Spawn,
            position: Vec3::new(0.5, 1.0, z),
            rotation: 0.0,
        })
        .collect();
    std::fs::write(
        &config.world_path,
        serde_json::to_vec(&serde_json::json!({
            "revision": 0,
            "cells": expected_world.cells(),
            "objects": expected_objects,
        }))
        .unwrap(),
    )
    .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}/ws", listener.local_addr().unwrap());
    let (stop, stopped) = oneshot::channel();
    let server = tokio::spawn(serve(listener, config.clone(), async {
        let _ = stopped.await;
    }));
    let (mut attacker, mut attack_state) = joined(&url).await;
    let (mut target, mut target_state) = joined(&url).await;
    let (mut builder, mut build_state) = joined(&url).await;
    let mode_sent = Instant::now();
    builder.send(ClientMessage::SetMode {
        request: 1,
        editing: true,
    });
    loop {
        let message = builder.receive().await;
        build_state.apply(&message);
        if matches!(
            message,
            ServerMessage::ModeChanged {
                request: 1,
                editing: true
            }
        ) {
            break;
        }
    }
    assert!(
        mode_sent.elapsed() >= ONE_WAY * 2,
        "both delay queues are active"
    );

    let edits = vec![
        CellEdit {
            pos: [-8, 1, -8],
            cell: Some(Cell::solid(6)),
        },
        CellEdit {
            pos: [-7, 1, -8],
            cell: Some(Cell::solid(7)),
        },
    ];
    expected_world.apply_edits(&edits);
    let kinds = [
        ObjectKind::WallLamp,
        ObjectKind::HangingLamp,
        ObjectKind::UtilityBox,
        ObjectKind::Pipe,
        ObjectKind::Rock,
    ];
    for (i, kind) in kinds.into_iter().enumerate() {
        expected_objects.push(WorldObject {
            id: i as u32 + 3,
            kind,
            position: Vec3::new(6.0 + i as f32 * 2.0, 1.0, 9.0),
            rotation: i as f32 * 0.25,
        });
    }
    let mut commands = vec![ClientMessage::Edit { request: 1, edits }];
    commands.extend(
        expected_objects[2..]
            .iter()
            .map(|object| ClientMessage::PlaceObject {
                kind: object.kind,
                position: object.position,
                rotation: object.rotation,
            }),
    );
    commands.push(ClientMessage::RemoveObject { id: 7 });
    let rock = expected_objects.last_mut().unwrap();
    rock.id = 8;
    commands.push(ClientMessage::PlaceObject {
        kind: rock.kind,
        position: rock.position,
        rotation: rock.rotation,
    });
    let mut commands = commands.into_iter();
    let mut input_ticks = tokio::time::interval(Duration::from_secs_f64(1.0 / 30.0));
    let mut edit_ticks = tokio::time::interval(Duration::from_millis(100));
    input_ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    edit_ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let started = Instant::now();
    let deadline = started + Duration::from_secs(8);
    let mut seq = 0;
    loop {
        tokio::select! {
            _ = sleep_until(deadline) => panic!("delayed clients did not converge and observe combat"),
            _ = input_ticks.tick() => {
                seq += 1;
                for client in [&attacker, &target] {
                    client.send(ClientMessage::Input { input: InputFrame {
                        seq,
                        fire: true,
                        ..Default::default()
                    }});
                }
            }
            _ = edit_ticks.tick(), if commands.len() > 0 => {
                builder.send(commands.next().unwrap());
            }
            message = attacker.incoming.recv() => attack_state.apply(&message.expect("attacker connected")),
            message = target.incoming.recv() => target_state.apply(&message.expect("target connected")),
            message = builder.incoming.recv() => build_state.apply(&message.expect("builder connected")),
        }
        let all_observed = [&attack_state, &target_state, &build_state]
            .into_iter()
            .all(|state| {
                state.revision == 8
                    && state.saw_score_and_respawn
                    && state.shot_owners.contains(&attack_state.id)
                    && state.shot_owners.contains(&target_state.id)
                    && state.players.len() == 3
                    && state
                        .players
                        .iter()
                        .filter(|p| !p.editing)
                        .all(|p| p.last_input >= 30)
            });
        if all_observed && started.elapsed() >= Duration::from_secs(2) {
            break;
        }
    }
    assert!(build_state.edit_acknowledged);
    for state in [&attack_state, &target_state, &build_state] {
        assert_eq!(state.world.cells(), expected_world.cells());
        assert_objects(&state.objects, &expected_objects);
        assert_eq!(state.revisions, (1..=8).collect::<Vec<_>>());
        assert!(state.saw_rock_removed);
        assert!(state.damaged.contains(&target_state.id));
        assert_eq!(state.players.iter().filter(|p| p.editing).count(), 1);
        assert!(
            state
                .players
                .iter()
                .find(|p| p.id == build_state.id)
                .unwrap()
                .editing
        );
    }

    builder.send(ClientMessage::Save);
    loop {
        let message = builder.receive().await;
        build_state.apply(&message);
        if matches!(message, ServerMessage::Notice { message } if message == "Save requested.") {
            break;
        }
    }
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let saved: serde_json::Value =
                serde_json::from_slice(&std::fs::read(&config.world_path).unwrap()).unwrap();
            if saved["revision"] == 8 {
                let cells: Vec<CellEdit> = serde_json::from_value(saved["cells"].clone()).unwrap();
                assert_eq!(cells, expected_world.cells());
                let objects: Vec<WorldObject> =
                    serde_json::from_value(saved["objects"].clone()).unwrap();
                assert_objects(&objects, &expected_objects);
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("explicit save persisted before shutdown");
    drop((attacker, target, builder));
    stop.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .unwrap()
        .unwrap()
        .unwrap();

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}/ws", listener.local_addr().unwrap());
    let (stop, stopped) = oneshot::channel();
    let server = tokio::spawn(serve(listener, config, async {
        let _ = stopped.await;
    }));
    let (rejoined, restored) = joined(&url).await;
    assert_eq!(restored.revision, 8);
    assert_eq!(restored.world.cells(), expected_world.cells());
    assert_objects(&restored.objects, &expected_objects);
    drop(rejoined);
    stop.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    eprintln!("80 ms each direction: two fighters + builder, revisions 1..8 converged, damage/score/respawn observed, save/restart/rejoin matched");
}
