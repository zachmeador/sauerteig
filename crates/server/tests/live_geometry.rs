//! Exercise the public server with real WebSockets. This checks synchronization
//! and simulation behavior; it is not a rendering or frame-rate benchmark.

use futures_util::{SinkExt, StreamExt};
use sauerteig_core::{
    Cell, CellEdit, CellFragment, ClientMessage, FineBox, InputFrame, ObjectKind, RootEdit,
    ServerMessage, Shape, Vec3, World, WorldObject,
};
use sauerteig_server::{serve, Config};
use std::{path::PathBuf, time::Duration};
use tokio::{net::TcpListener, sync::oneshot};
use tokio_tungstenite::{tungstenite::Message, MaybeTlsStream, WebSocketStream};

type Socket = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

struct TemporaryWorld(PathBuf);

impl TemporaryWorld {
    fn new() -> Self {
        let time = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "sauerteig-live-geometry-{}-{time}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}

impl Drop for TemporaryWorld {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

async fn send(socket: &mut Socket, message: ClientMessage) {
    socket
        .send(Message::Text(
            serde_json::to_string(&message).unwrap().into(),
        ))
        .await
        .unwrap();
}

async fn receive(socket: &mut Socket, matches: impl Fn(&ServerMessage) -> bool) -> ServerMessage {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let frame = socket.next().await.expect("open connection").unwrap();
            if let Message::Text(text) = frame {
                let message = serde_json::from_str::<ServerMessage>(&text).unwrap();
                if matches(&message) {
                    return message;
                }
            }
        }
    })
    .await
    .expect("server response before timeout")
}

async fn patch(socket: &mut Socket, revision: u64, edits: &[CellEdit]) {
    let message = receive(
        socket,
        |m| matches!(m, ServerMessage::WorldPatch { revision: actual, .. } if *actual == revision),
    )
    .await;
    if let ServerMessage::WorldPatch { edits: actual, .. } = message {
        assert_eq!(actual, edits);
    }
}

async fn root_patch(socket: &mut Socket, revision: u64, roots: &[RootEdit]) {
    let message = receive(
        socket,
        |m| matches!(m, ServerMessage::RootPatch { revision: actual, .. } if *actual == revision),
    )
    .await;
    if let ServerMessage::RootPatch { roots: actual, .. } = message {
        assert_eq!(actual, canonical_roots(roots));
    }
}

fn canonical_roots(roots: &[RootEdit]) -> Vec<RootEdit> {
    roots
        .iter()
        .map(|root| RootEdit {
            pos: root.pos,
            fragments: sauerteig_core::canonical_root_fragments(root.pos, &root.fragments),
        })
        .collect()
}

fn coarse_root(edit: CellEdit) -> RootEdit {
    let origin = edit.pos.map(|v| v * 8);
    RootEdit {
        pos: edit.pos,
        fragments: edit
            .cell
            .into_iter()
            .map(|cell| CellFragment {
                origin,
                size: 8,
                bounds: FineBox {
                    min: origin,
                    max: origin.map(|v| v + 8),
                },
                cell,
            })
            .collect(),
    }
}

fn assert_world(actual: &World, expected: &World) {
    assert_eq!(actual.cells(), expected.cells());
    assert_eq!(actual.detailed_roots(), expected.detailed_roots());
}

async fn editor(url: &str) -> Socket {
    let (mut socket, _) = tokio_tungstenite::connect_async(url).await.unwrap();
    receive(&mut socket, |m| matches!(m, ServerMessage::Welcome { .. })).await;
    send(
        &mut socket,
        ClientMessage::SetMode {
            request: 1,
            editing: true,
        },
    )
    .await;
    receive(&mut socket, |m| {
        matches!(
            m,
            ServerMessage::ModeChanged {
                request: 1,
                editing: true
            }
        )
    })
    .await;
    socket
}

#[tokio::test]
async fn two_builders_share_a_large_sparse_paste_with_a_live_fighter_and_reload_it() {
    let temporary = TemporaryWorld::new();
    let config = Config {
        world_path: temporary.0.join("world.json"),
        editing_allowed: true,
    };
    let mut expected_world = World::empty();
    for x in -12..12 {
        for z in -12..12 {
            expected_world.set([x, 0, z], Some(Cell::solid(0)));
        }
    }
    std::fs::write(
        &config.world_path,
        serde_json::to_vec(&serde_json::json!({
            "revision": 0,
            "cells": expected_world.cells(),
            "objects": [WorldObject {
                id: 1,
                kind: ObjectKind::Spawn,
                position: Vec3::new(0.5, 1.0, 0.5),
                rotation: 0.0,
            }],
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
    let mut first = editor(&url).await;
    let mut second = editor(&url).await;
    let (mut fighter, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    let welcome = receive(&mut fighter, |m| matches!(m, ServerMessage::Welcome { .. })).await;
    let (fighter_id, initial_position, fighter_world) = match welcome {
        ServerMessage::Welcome {
            id,
            cells,
            roots,
            players,
            ..
        } => {
            assert_eq!(players.len(), 3);
            assert_eq!(players.iter().filter(|p| p.editing).count(), 2);
            let position = players.iter().find(|p| p.id == id).unwrap().position;
            (id, position, World::from_parts(&cells, &roots).unwrap())
        }
        _ => unreachable!(),
    };

    // Send actual movement/fire input while independently consuming patches,
    // snapshots and shot events, just as a playing client does during an edit.
    let (stop_fighter, mut fighter_stopped) = oneshot::channel();
    let (progress, mut observed) = tokio::sync::watch::channel((0_u64, 0_u32, 0_usize));
    let fighter_task = tokio::spawn(async move {
        let (mut sink, mut stream) = fighter.split();
        let mut interval = tokio::time::interval(Duration::from_secs_f64(1.0 / 60.0));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut world = fighter_world;
        let mut revisions = Vec::new();
        let mut max_distance = 0.0_f32;
        let mut seq = 0;
        let mut status = (0, 0, 0);
        loop {
            tokio::select! {
                _ = &mut fighter_stopped => break,
                _ = interval.tick() => {
                    seq += 1;
                    let message = ClientMessage::Input {
                        input: InputFrame {
                            seq,
                            strafe: if (seq / 30) % 2 == 0 { 1.0 } else { -1.0 },
                            fire: true,
                            ..Default::default()
                        },
                    };
                    sink.send(Message::Text(serde_json::to_string(&message).unwrap().into()))
                        .await.unwrap();
                }
                frame = stream.next() => {
                    let frame = frame.expect("fighter remains connected").unwrap();
                    if let Message::Text(text) = frame {
                        match serde_json::from_str::<ServerMessage>(&text).unwrap() {
                            ServerMessage::WorldPatch { revision, edits } => {
                                world.apply_edits(&edits);
                                revisions.push((revision, edits.into_iter().map(coarse_root).collect()));
                                status.0 = revision;
                            }
                            ServerMessage::RootPatch { revision, roots } => {
                                world.apply_root_edits(&roots).unwrap();
                                revisions.push((revision, roots));
                                status.0 = revision;
                            }
                            ServerMessage::Snapshot { players, .. } => {
                                let player = players.iter().find(|p| p.id == fighter_id).unwrap();
                                assert!(!player.editing, "builders must not change the fighter's mode");
                                assert_eq!(player.health, 100);
                                max_distance = max_distance.max(player.position.distance(initial_position));
                                status.1 = player.last_input;
                            }
                            ServerMessage::Shots { shots } if status.0 > 0 => {
                                status.2 += shots.iter().filter(|shot| shot.owner == fighter_id).count();
                            }
                            ServerMessage::Error { message } => panic!("fighter error: {message}"),
                            _ => {}
                        }
                        progress.send_replace(status);
                    }
                }
            }
        }
        sink.close().await.unwrap();
        (world, revisions, max_distance)
    });

    // A side inset and a raised underside require explicit eight-corner shape
    // data; the old four top heights cannot represent this cell.
    let mut shape = Shape::solid();
    for vertex in &mut shape.vertices {
        vertex[0] /= 2;
        if vertex[1] == 0 {
            vertex[1] = 2;
        }
    }
    let shaped = Cell {
        shape: Some(shape),
        ..Cell::solid(2)
    };
    assert!(shaped.is_valid());
    // More than the old 512-cell cap, while preserving coarse geometry. The
    // one detailed root contains an eighth-metre element and a shaped half-
    // metre element separated by empty space; it is never expanded to 512 cells.
    let mut first_edits = Vec::new();
    for x in 6..38 {
        for z in 6..38 {
            first_edits.push(coarse_root(CellEdit {
                pos: [x, 1, z],
                cell: Some(Cell::solid(0)),
            }));
        }
    }
    let origin = [48, 8, 48];
    first_edits[0].fragments = vec![
        CellFragment {
            origin,
            size: 4,
            bounds: FineBox {
                min: origin,
                max: origin.map(|v| v + 4),
            },
            cell: shaped,
        },
        CellFragment {
            origin: origin.map(|v| v + 7),
            size: 1,
            bounds: FineBox {
                min: origin.map(|v| v + 7),
                max: origin.map(|v| v + 8),
            },
            cell: Cell::solid(3),
        },
    ];
    let second_edits = vec![CellEdit {
        pos: [40, 1, 40],
        cell: Some(Cell::solid(1)),
    }];
    send(
        &mut first,
        ClientMessage::EditRoots {
            request: 1,
            base_revision: 0,
            roots: first_edits.clone(),
        },
    )
    .await;
    root_patch(&mut first, 1, &first_edits).await;
    assert!(matches!(
        receive(&mut first, |m| matches!(
            m,
            ServerMessage::EditResult { request: 1, .. }
        ))
        .await,
        ServerMessage::EditResult { error: None, .. }
    ));
    root_patch(&mut second, 1, &first_edits).await;
    send(
        &mut second,
        ClientMessage::Edit {
            request: 1,
            edits: second_edits.clone(),
        },
    )
    .await;
    patch(&mut second, 2, &second_edits).await;
    assert!(matches!(
        receive(&mut second, |m| matches!(
            m,
            ServerMessage::EditResult { request: 1, .. }
        ))
        .await,
        ServerMessage::EditResult { error: None, .. }
    ));
    patch(&mut first, 2, &second_edits).await;
    tokio::time::timeout(
        Duration::from_secs(5),
        observed.wait_for(|s| s.0 == 2 && s.1 >= 12 && s.2 > 0),
    )
    .await
    .unwrap()
    .unwrap();

    let removal: Vec<_> = first_edits
        .iter()
        .map(|r| RootEdit {
            pos: r.pos,
            fragments: vec![],
        })
        .collect();
    send(&mut first, ClientMessage::Undo).await;
    root_patch(&mut first, 3, &removal).await;
    root_patch(&mut second, 3, &removal).await;
    send(&mut first, ClientMessage::Redo).await;
    root_patch(&mut first, 4, &first_edits).await;
    root_patch(&mut second, 4, &first_edits).await;
    tokio::time::timeout(
        Duration::from_secs(5),
        observed.wait_for(|s| s.0 == 4 && s.1 >= 30 && s.2 >= 2),
    )
    .await
    .unwrap()
    .unwrap();

    let snapshot = receive(&mut first, |m| {
        matches!(m, ServerMessage::Snapshot { revision: 4, players, .. }
            if players.iter().any(|p| p.id == fighter_id && p.last_input >= 30))
    })
    .await;
    let position = match snapshot {
        ServerMessage::Snapshot { players, .. } => {
            players
                .iter()
                .find(|p| p.id == fighter_id)
                .unwrap()
                .position
        }
        _ => unreachable!(),
    };
    // Cover the adjacent cells too, so a frame of movement between this
    // snapshot and the request cannot make the overlap check race the test.
    let mut blocking = Vec::new();
    for x in position.x.floor() as i32 - 1..=position.x.floor() as i32 + 1 {
        for z in position.z.floor() as i32 - 1..=position.z.floor() as i32 + 1 {
            blocking.push(CellEdit {
                pos: [x, 1, z],
                cell: Some(Cell::solid(0)),
            });
        }
    }
    send(
        &mut first,
        ClientMessage::EditRoots {
            request: 2,
            base_revision: 4,
            roots: blocking.into_iter().map(coarse_root).collect(),
        },
    )
    .await;
    let rejected = receive(&mut first, |m| {
        matches!(m, ServerMessage::EditResult { request: 2, .. })
    })
    .await;
    assert!(
        matches!(rejected, ServerMessage::EditResult { error: Some(error), .. }
        if error.contains("inside a player"))
    );

    // A rejected fine edit acknowledges only its own request, and cannot use an
    // old snapshot to overwrite a root whose ownership was restored by redo.
    send(
        &mut first,
        ClientMessage::EditRoots {
            request: 3,
            base_revision: 0,
            roots: vec![first_edits[0].clone()],
        },
    )
    .await;
    let stale = receive(&mut first, |m| {
        matches!(m, ServerMessage::EditResult { request: 3, .. })
    })
    .await;
    assert!(
        matches!(stale, ServerMessage::EditResult { error: Some(error), .. }
        if error.contains("changed after"))
    );

    expected_world.apply_root_edits(&first_edits).unwrap();
    expected_world.apply_edits(&second_edits);
    send(&mut first, ClientMessage::Save).await;
    receive(
        &mut first,
        |m| matches!(m, ServerMessage::Notice { message } if message == "Save requested."),
    )
    .await;
    // Check the requested save before shutdown's separate final flush.
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let bytes = std::fs::read(&config.world_path).unwrap();
            let saved: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            if saved["revision"] == 4 {
                let cells: Vec<CellEdit> = serde_json::from_value(saved["cells"].clone()).unwrap();
                let roots: Vec<RootEdit> = serde_json::from_value(saved["roots"].clone()).unwrap();
                assert_world(&World::from_parts(&cells, &roots).unwrap(), &expected_world);
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("explicit save persisted before shutdown");

    stop_fighter.send(()).unwrap();
    let (fighter_world, revisions, max_distance) = fighter_task.await.unwrap();
    assert_world(&fighter_world, &expected_world);
    assert_eq!(
        revisions,
        vec![
            (1, canonical_roots(&first_edits)),
            (2, second_edits.into_iter().map(coarse_root).collect()),
            (3, removal),
            (4, canonical_roots(&first_edits))
        ]
    );
    assert!(
        max_distance > 0.5,
        "fighter moved while the shared world changed"
    );
    first.close(None).await.unwrap();
    second.close(None).await.unwrap();
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
    let (mut rejoined, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    let welcome = receive(&mut rejoined, |m| {
        matches!(m, ServerMessage::Welcome { .. })
    })
    .await;
    if let ServerMessage::Welcome {
        revision,
        cells,
        roots,
        ..
    } = welcome
    {
        assert_eq!(revision, 4, "rejected overlap did not change the revision");
        assert_world(&World::from_parts(&cells, &roots).unwrap(), &expected_world);
        assert_eq!(
            roots.len(),
            1,
            "ordinary roots remain coarse in the snapshot"
        );
        assert_eq!(roots[0].fragments.len(), 2);
        assert_eq!(roots[0].fragments[0].cell.shape, Some(shape));
    }
    rejoined.close(None).await.unwrap();
    stop.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}
