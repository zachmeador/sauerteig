//! Atomic geometry/object editing over the public WebSocket protocol.

use futures_util::{SinkExt, StreamExt};
use sauerteig_core::{
    Cell, CellEdit, CellFragment, ClientMessage, FineBox, MaterialVolume, ObjectEdit, ObjectKind,
    ObjectPlacement, RootEdit, ServerMessage, Vec3, VolumeEdit, VolumeMaterial, World, WorldBounds,
    WorldObject, WORLD_FORMAT_VERSION,
};
use sauerteig_server::{serve, Config};
use std::{
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};
use tokio::{net::TcpListener, sync::oneshot, task::JoinHandle};
use tokio_tungstenite::{tungstenite::Message, MaybeTlsStream, WebSocketStream};

type Socket = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

struct TemporaryWorld(PathBuf);
static NEXT_TEMPORARY_WORLD: AtomicU64 = AtomicU64::new(0);

impl TemporaryWorld {
    fn new() -> Self {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let sequence = NEXT_TEMPORARY_WORLD.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "sauerteig-object-transactions-{}-{stamp}-{sequence}",
            std::process::id(),
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

async fn start(config: Config) -> (String, oneshot::Sender<()>, JoinHandle<Result<(), String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}/ws", listener.local_addr().unwrap());
    let (stop, stopped) = oneshot::channel();
    let server = tokio::spawn(serve(listener, config, async {
        let _ = stopped.await;
    }));
    (url, stop, server)
}

async fn stop(stop: oneshot::Sender<()>, server: JoinHandle<Result<(), String>>) {
    stop.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

async fn send(socket: &mut Socket, message: ClientMessage) {
    socket
        .send(Message::Text(
            serde_json::to_string(&message).unwrap().into(),
        ))
        .await
        .unwrap();
}

/// Ignore only simulation traffic. All world events remain visible to assertions
/// so a split patch, an unexpected mutation, or an early result fails the test.
async fn event(socket: &mut Socket) -> ServerMessage {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Message::Text(text) = socket.next().await.expect("open connection").unwrap() {
                let message: ServerMessage = serde_json::from_str(&text).unwrap();
                if !matches!(
                    message,
                    ServerMessage::Snapshot { .. } | ServerMessage::Shots { .. }
                ) {
                    return message;
                }
            }
        }
    })
    .await
    .expect("server response before timeout")
}

async fn join(url: &str) -> (Socket, u64, World, Vec<WorldObject>) {
    let (mut socket, _) = tokio_tungstenite::connect_async(url).await.unwrap();
    match event(&mut socket).await {
        ServerMessage::Welcome {
            revision,
            bounds,
            regions,
            cells,
            roots,
            objects,
            volumes,
            ..
        } => {
            let mut world = World::from_compact_parts(bounds, &regions, &cells, &roots).unwrap();
            world.replace_volumes(&volumes).unwrap();
            (socket, revision, world, objects)
        }
        other => panic!("expected welcome, got {other:?}"),
    }
}

async fn edit_mode(socket: &mut Socket) {
    send(
        socket,
        ClientMessage::SetMode {
            request: 1,
            editing: true,
        },
    )
    .await;
    assert!(matches!(
        event(socket).await,
        ServerMessage::ModeChanged {
            request: 1,
            editing: true
        }
    ));
}

fn root(pos: [i32; 3], cell: Option<Cell>) -> RootEdit {
    let origin = pos.map(|v| v * 8);
    RootEdit {
        pos,
        fragments: cell
            .into_iter()
            .map(|cell| CellFragment {
                bounds: FineBox {
                    min: origin,
                    max: origin.map(|v| v + 8),
                },
                origin,
                size: 8,
                cell,
            })
            .collect(),
    }
}

async fn transaction_patch(
    socket: &mut Socket,
    revision: u64,
    expected_roots: &[RootEdit],
    expected_objects: &[WorldObject],
) {
    match event(socket).await {
        ServerMessage::TransactionPatch {
            revision: actual,
            roots,
            objects,
            volumes,
        } => {
            assert!(
                volumes.is_none(),
                "geometry/object edits must not retransmit the volume layer"
            );
            assert_eq!(actual, revision);
            assert_eq!(roots, expected_roots);
            assert_eq!(objects, expected_objects);
        }
        other => panic!("expected one atomic transaction patch, got {other:?}"),
    }
}

async fn result(socket: &mut Socket, request: u64, accepted: bool) {
    match event(socket).await {
        ServerMessage::EditResult {
            request: actual,
            error,
        } => {
            assert_eq!(actual, request);
            assert_eq!(error.is_none(), accepted, "unexpected result: {error:?}");
            if !accepted {
                assert!(error.unwrap().contains("changed after"));
            }
        }
        other => panic!("expected transaction result after its patch, got {other:?}"),
    }
}

async fn history_conflict(socket: &mut Socket) {
    send(socket, ClientMessage::Undo).await;
    match event(socket).await {
        ServerMessage::Error { message } => assert!(message.contains("later edit")),
        other => panic!("expected history conflict without any patch, got {other:?}"),
    }
}

async fn material_patch(
    socket: &mut Socket,
    revision: u64,
    expected_roots: &[RootEdit],
    expected_objects: &[WorldObject],
    expected_volumes: &[MaterialVolume],
) {
    match event(socket).await {
        ServerMessage::TransactionPatch {
            revision: actual,
            roots,
            objects,
            volumes: Some(volumes),
        } => {
            assert_eq!(actual, revision);
            assert_eq!(roots, expected_roots);
            assert_eq!(objects, expected_objects);
            assert_eq!(volumes, expected_volumes);
        }
        other => panic!("expected atomic material transaction, got {other:?}"),
    }
}

#[tokio::test]
async fn material_geometry_and_objects_share_multiplayer_history_and_saved_reopen() {
    let temporary = TemporaryWorld::new();
    let config = Config {
        world_path: temporary.0.join("world.json"),
        web_dir: temporary.0.join("web"),
        editing_allowed: true,
    };
    std::fs::write(
        &config.world_path,
        br#"{"revision":0,"cells":[],"objects":[]}"#,
    )
    .unwrap();
    let (url, shutdown, server) = start(config.clone()).await;
    let (mut first, revision, original, _) = join(&url).await;
    assert_eq!(revision, 0);
    assert!(original.volumes().is_empty());
    let (mut second, _, _, _) = join(&url).await;
    edit_mode(&mut first).await;
    edit_mode(&mut second).await;
    let geometry = vec![root([8, 1, 8], Some(Cell::solid(2)))];
    let removal = vec![root([8, 1, 8], None)];
    let lamp = ObjectPlacement {
        kind: ObjectKind::Light,
        position: Vec3::new(8.5, 3.5, 8.5),
        rotation: 0.5,
    };
    let objects = vec![WorldObject {
        id: 1,
        kind: lamp.kind,
        position: lamp.position,
        rotation: lamp.rotation,
    }];
    let water = MaterialVolume {
        bounds: FineBox {
            min: [64, 8, 64],
            max: [80, 32, 80],
        },
        material: VolumeMaterial::Water,
    };
    let glass = MaterialVolume {
        bounds: FineBox {
            min: [80, 8, 64],
            max: [81, 32, 80],
        },
        material: VolumeMaterial::Glass,
    };
    let volumes = vec![water, glass];
    send(
        &mut first,
        ClientMessage::EditTransaction {
            request: 1,
            base_revision: 0,
            roots: geometry.clone(),
            objects: vec![ObjectEdit::Create { object: lamp }],
            volumes: volumes
                .iter()
                .map(|volume| VolumeEdit {
                    bounds: volume.bounds,
                    material: Some(volume.material),
                })
                .collect(),
        },
    )
    .await;
    material_patch(&mut first, 1, &geometry, &objects, &volumes).await;
    result(&mut first, 1, true).await;
    material_patch(&mut second, 1, &geometry, &objects, &volumes).await;
    send(&mut first, ClientMessage::Undo).await;
    material_patch(&mut first, 2, &removal, &[], &[]).await;
    material_patch(&mut second, 2, &removal, &[], &[]).await;
    send(&mut first, ClientMessage::Redo).await;
    material_patch(&mut first, 3, &geometry, &objects, &volumes).await;
    material_patch(&mut second, 3, &geometry, &objects, &volumes).await;

    send(
        &mut second,
        ClientMessage::EditTransaction {
            request: 1,
            base_revision: 3,
            roots: vec![],
            objects: vec![],
            volumes: vec![VolumeEdit {
                bounds: glass.bounds,
                material: None,
            }],
        },
    )
    .await;
    material_patch(&mut second, 4, &[], &objects, &[water]).await;
    result(&mut second, 1, true).await;
    material_patch(&mut first, 4, &[], &objects, &[water]).await;
    history_conflict(&mut first).await;
    send(&mut second, ClientMessage::Undo).await;
    material_patch(&mut second, 5, &[], &objects, &volumes).await;
    material_patch(&mut first, 5, &[], &objects, &volumes).await;
    // Restoring the same layer restores undo ownership, but stale transactions
    // still cannot replace it or partially commit their geometry.
    send(
        &mut first,
        ClientMessage::EditTransaction {
            request: 2,
            base_revision: 3,
            roots: vec![root([9, 1, 9], Some(Cell::solid(1)))],
            objects: vec![],
            volumes: vec![VolumeEdit {
                bounds: water.bounds,
                material: None,
            }],
        },
    )
    .await;
    result(&mut first, 2, false).await;
    send(&mut first, ClientMessage::Undo).await;
    material_patch(&mut first, 6, &removal, &[], &[]).await;
    material_patch(&mut second, 6, &removal, &[], &[]).await;
    send(&mut first, ClientMessage::Redo).await;
    material_patch(&mut first, 7, &geometry, &objects, &volumes).await;
    material_patch(&mut second, 7, &geometry, &objects, &volumes).await;
    first.close(None).await.unwrap();
    second.close(None).await.unwrap();
    stop(shutdown, server).await;

    let saved: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&config.world_path).unwrap()).unwrap();
    assert_eq!(saved["format_version"], 3);
    assert_eq!(saved["volumes"].as_array().unwrap().len(), 2);
    let (url, shutdown, server) = start(config).await;
    let (mut rejoined, revision, restored, restored_objects) = join(&url).await;
    assert_eq!(revision, 7);
    assert_eq!(restored.volumes(), volumes);
    assert_eq!(restored_objects, objects);
    assert_eq!(restored.get([8, 1, 8]), Some(Cell::solid(2)));
    assert!(restored.get([9, 1, 9]).is_none());
    assert_eq!(restored.volume_material_at(Vec3::new(8.5, 1.5, 8.5)), None);
    assert_eq!(
        restored.volume_material_at(Vec3::new(9.5, 2.0, 8.5)),
        Some(VolumeMaterial::Water)
    );
    assert!(restored.intersects_solid_volume(Vec3::new(10.01, 2.0, 8.5), Vec3::new(10.1, 2.5, 8.9)));
    rejoined.close(None).await.unwrap();
    stop(shutdown, server).await;
}

#[tokio::test]
async fn mixed_edits_are_atomic_conflict_safe_and_persist_through_rejoin() {
    let temporary = TemporaryWorld::new();
    let config = Config {
        world_path: temporary.0.join("world.json"),
        web_dir: temporary.0.join("web"),
        editing_allowed: true,
    };
    let mut expected_world = World::empty();
    for x in -16..16 {
        for z in -16..16 {
            expected_world.set([x, 0, z], Some(Cell::solid(0)));
        }
    }
    let initial_objects = vec![WorldObject {
        id: 1,
        kind: ObjectKind::Spawn,
        position: Vec3::new(0.5, 1.0, 0.5),
        rotation: 0.0,
    }];
    std::fs::write(
        &config.world_path,
        serde_json::to_vec(&serde_json::json!({
            "revision": 0,
            "cells": expected_world.cells(),
            "objects": initial_objects,
        }))
        .unwrap(),
    )
    .unwrap();

    let (url, shutdown, server) = start(config.clone()).await;
    let (mut first, revision, _, objects) = join(&url).await;
    assert_eq!(revision, 0);
    assert_eq!(objects, initial_objects);
    let (mut second, _, _, _) = join(&url).await;
    edit_mode(&mut first).await;
    edit_mode(&mut second).await;

    let geometry = vec![root([8, 1, 8], Some(Cell::solid(2)))];
    let removal = vec![root([8, 1, 8], None)];
    let lamp = ObjectPlacement {
        kind: ObjectKind::WallLamp,
        position: Vec3::new(8.5, 2.0, 8.5),
        rotation: 0.75,
    };
    let mut fixtures = initial_objects.clone();
    fixtures.push(WorldObject {
        id: 2,
        kind: lamp.kind,
        position: lamp.position,
        rotation: lamp.rotation,
    });
    send(
        &mut first,
        ClientMessage::EditTransaction {
            volumes: Vec::new(),
            request: 1,
            base_revision: 0,
            roots: geometry.clone(),
            objects: vec![ObjectEdit::Create {
                object: lamp.clone(),
            }],
        },
    )
    .await;
    transaction_patch(&mut first, 1, &geometry, &fixtures).await;
    result(&mut first, 1, true).await;
    transaction_patch(&mut second, 1, &geometry, &fixtures).await;

    send(&mut first, ClientMessage::Undo).await;
    transaction_patch(&mut first, 2, &removal, &initial_objects).await;
    transaction_patch(&mut second, 2, &removal, &initial_objects).await;
    send(&mut first, ClientMessage::Redo).await;
    transaction_patch(&mut first, 3, &geometry, &fixtures).await;
    transaction_patch(&mut second, 3, &geometry, &fixtures).await;

    let moved_lamp = ObjectPlacement {
        position: Vec3::new(9.5, 2.5, 8.5),
        rotation: 1.5,
        ..lamp.clone()
    };
    let mut moved = fixtures.clone();
    moved[1].position = moved_lamp.position;
    moved[1].rotation = moved_lamp.rotation;
    send(
        &mut second,
        ClientMessage::EditTransaction {
            volumes: Vec::new(),
            request: 1,
            base_revision: 3,
            roots: vec![],
            objects: vec![ObjectEdit::Update {
                id: 2,
                object: moved_lamp,
            }],
        },
    )
    .await;
    transaction_patch(&mut second, 4, &[], &moved).await;
    result(&mut second, 1, true).await;
    transaction_patch(&mut first, 4, &[], &moved).await;
    history_conflict(&mut first).await;

    // The stale object prevents the accompanying geometry from being applied.
    send(
        &mut first,
        ClientMessage::EditTransaction {
            volumes: Vec::new(),
            request: 2,
            base_revision: 3,
            roots: vec![root([9, 1, 8], Some(Cell::solid(5)))],
            objects: vec![ObjectEdit::Update {
                id: 2,
                object: lamp.clone(),
            }],
        },
    )
    .await;
    result(&mut first, 2, false).await;
    let (mut observer, revision, observed_world, observed_objects) = join(&url).await;
    expected_world.apply_root_edits(&geometry).unwrap();
    assert_eq!(revision, 4);
    assert_eq!(observed_world.cells(), expected_world.cells());
    assert_eq!(observed_objects, moved);
    observer.close(None).await.unwrap();

    // Undoing the other builder's move restores the original ownership lineage,
    // allowing the first builder to undo and redo the whole pasted fixture.
    send(&mut second, ClientMessage::Undo).await;
    transaction_patch(&mut second, 5, &[], &fixtures).await;
    transaction_patch(&mut first, 5, &[], &fixtures).await;
    send(&mut first, ClientMessage::Undo).await;
    transaction_patch(&mut first, 6, &removal, &initial_objects).await;
    transaction_patch(&mut second, 6, &removal, &initial_objects).await;
    send(&mut first, ClientMessage::Redo).await;
    transaction_patch(&mut first, 7, &geometry, &fixtures).await;
    transaction_patch(&mut second, 7, &geometry, &fixtures).await;

    // A later delete is also protected, and undo restores exactly the same ID.
    send(
        &mut second,
        ClientMessage::EditTransaction {
            volumes: Vec::new(),
            request: 2,
            base_revision: 7,
            roots: vec![],
            objects: vec![ObjectEdit::Delete { id: 2 }],
        },
    )
    .await;
    transaction_patch(&mut second, 8, &[], &initial_objects).await;
    result(&mut second, 2, true).await;
    transaction_patch(&mut first, 8, &[], &initial_objects).await;
    history_conflict(&mut first).await;
    send(&mut second, ClientMessage::Undo).await;
    transaction_patch(&mut second, 9, &[], &fixtures).await;
    transaction_patch(&mut first, 9, &[], &fixtures).await;

    send(&mut first, ClientMessage::Save).await;
    assert!(matches!(event(&mut first).await,
        ServerMessage::Notice { message } if message == "Save requested."));
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let saved: serde_json::Value =
                serde_json::from_slice(&std::fs::read(&config.world_path).unwrap()).unwrap();
            if saved["revision"] == 9 {
                let cells: Vec<CellEdit> = serde_json::from_value(saved["cells"].clone()).unwrap();
                let objects: Vec<WorldObject> =
                    serde_json::from_value(saved["objects"].clone()).unwrap();
                assert_eq!(cells, expected_world.cells());
                assert_eq!(objects, fixtures);
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("explicit save persisted before shutdown");
    first.close(None).await.unwrap();
    second.close(None).await.unwrap();
    stop(shutdown, server).await;

    let (url, shutdown, server) = start(config).await;
    let (mut rejoined, revision, restored_world, restored_objects) = join(&url).await;
    assert_eq!(revision, 9);
    assert_eq!(restored_world.cells(), expected_world.cells());
    assert_eq!(
        restored_world.detailed_roots(),
        expected_world.detailed_roots()
    );
    assert_eq!(restored_objects, fixtures);
    rejoined.close(None).await.unwrap();
    stop(shutdown, server).await;
}

#[tokio::test]
async fn compact_base_empty_override_and_objects_survive_save_and_rejoin() {
    let temporary = TemporaryWorld::new();
    let config = Config {
        world_path: temporary.0.join("world.json"),
        web_dir: temporary.0.join("web"),
        editing_allowed: true,
    };
    let bounds = WorldBounds {
        min: [-256, 0, -256],
        max: [256, 128, 256],
    };
    let regions = vec![CellFragment {
        origin: [1024, 0, 1024],
        size: 128,
        bounds: FineBox {
            min: [1024, 0, 1024],
            max: [1152, 128, 1152],
        },
        cell: Cell::solid(2),
    }];
    let mut objects = vec![WorldObject {
        id: 1,
        kind: ObjectKind::Spawn,
        position: Vec3::new(128.5, 16.0, 128.5),
        rotation: 0.0,
    }];
    std::fs::write(
        &config.world_path,
        serde_json::to_vec(&serde_json::json!({
            "format_version": WORLD_FORMAT_VERSION,
            "revision": 0,
            "bounds": bounds,
            "regions": regions,
            "cells": [],
            "roots": [],
            "objects": objects,
        }))
        .unwrap(),
    )
    .unwrap();
    let (url, shutdown, server) = start(config.clone()).await;
    let (mut builder, revision, original, _) = join(&url).await;
    assert_eq!(revision, 0);
    assert_eq!(original.bounds(), bounds);
    assert_eq!(original.region_count(), 1);
    assert!(!original.root_fragments([136, 15, 136]).is_empty());
    edit_mode(&mut builder).await;

    let empty_override = vec![root([136, 15, 136], None)];
    let lamp = ObjectPlacement {
        kind: ObjectKind::Light,
        position: Vec3::new(136.5, 16.5, 136.5),
        rotation: 1.25,
    };
    objects.push(WorldObject {
        id: 2,
        kind: lamp.kind,
        position: lamp.position,
        rotation: lamp.rotation,
    });
    send(
        &mut builder,
        ClientMessage::EditTransaction {
            volumes: Vec::new(),
            request: 1,
            base_revision: 0,
            roots: empty_override.clone(),
            objects: vec![ObjectEdit::Create { object: lamp }],
        },
    )
    .await;
    transaction_patch(&mut builder, 1, &empty_override, &objects).await;
    result(&mut builder, 1, true).await;
    send(&mut builder, ClientMessage::Save).await;
    assert!(matches!(event(&mut builder).await,
        ServerMessage::Notice { message } if message == "Save requested."));
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let saved: serde_json::Value =
                serde_json::from_slice(&std::fs::read(&config.world_path).unwrap()).unwrap();
            if saved["revision"] == 1 {
                assert_eq!(saved["format_version"], WORLD_FORMAT_VERSION);
                assert_eq!(saved["regions"].as_array().unwrap().len(), 1);
                assert_eq!(saved["cells"].as_array().unwrap().len(), 0);
                let roots: Vec<RootEdit> = serde_json::from_value(saved["roots"].clone()).unwrap();
                assert_eq!(roots, empty_override);
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("compact save persisted without expanding its base");
    builder.close(None).await.unwrap();
    stop(shutdown, server).await;

    let (url, shutdown, server) = start(config).await;
    let (mut rejoined, revision, restored, restored_objects) = join(&url).await;
    assert_eq!(revision, 1);
    assert_eq!(restored.bounds(), bounds);
    assert_eq!(restored.regions(), regions);
    assert_eq!(restored.root_overrides(), empty_override);
    assert!(restored.root_fragments([136, 15, 136]).is_empty());
    assert!(!restored.root_fragments([137, 15, 136]).is_empty());
    assert_eq!(restored_objects, objects);
    rejoined.close(None).await.unwrap();
    stop(shutdown, server).await;
}
