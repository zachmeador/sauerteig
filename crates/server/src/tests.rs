use crate::edits::CommitError;
use crate::network::{MESSAGE_BYTES, OUTBOUND_WRITE_BYTES};
use crate::outbound::FOLLOW_UP_BYTES;
use crate::persistence::{load_world, write_world, SavedWorld};
use crate::{
    legacy_root, serve, Config, Session, EDIT_BURST, MAX_EDITS, MAX_HISTORY_COMPLEXITY,
    MAX_INPUT_BACKLOG, MAX_OBJECTS, MAX_PLAYERS, MAX_WORLD_FRAGMENTS,
};
use sauerteig_core::{
    simulate_player, spawn_point, Cell, CellEdit, CellFragment, CellPos, ClientMessage, FineBox,
    InputFrame, ObjectKind, RootEdit, ServerMessage, Vec3, World, WorldObject, FIXED_DT, WORLD_MAX,
    WORLD_MIN,
};
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};
use tokio::{
    net::TcpListener,
    sync::{mpsc, watch},
};

/// A cell as a world stores it: a planar face's diagonal takes its fixed
/// value.
fn stored(pos: CellPos, cell: Cell) -> Option<Cell> {
    let stored = World::from_cells(&[CellEdit {
        pos,
        cell: Some(cell),
    }])
    .get(pos);
    assert!(stored.is_some_and(|c| c.as_shape().vertices == cell.as_shape().vertices));
    stored
}

fn room() -> Session {
    let mut world = World::empty();
    for x in -8..8 {
        for z in -8..8 {
            world.set([x, 0, z], Some(Cell::solid(0)));
        }
    }
    Session::new(
        SavedWorld {
            volumes: Vec::new(),
            format_version: 2,
            bounds: Default::default(),
            regions: world.regions(),
            revision: 0,
            cells: world.cells(),
            roots: Vec::new(),
            objects: Vec::new(),
            next_object: 0,
        },
        true,
    )
}

fn volume_edit(
    min: [i32; 3],
    max: [i32; 3],
    material: Option<sauerteig_core::VolumeMaterial>,
) -> sauerteig_core::VolumeEdit {
    sauerteig_core::VolumeEdit {
        bounds: FineBox { min, max },
        material,
    }
}

fn volume_transaction(
    session: &mut Session,
    id: u64,
    base_revision: u64,
    roots: Vec<RootEdit>,
    objects: Vec<sauerteig_core::ObjectEdit>,
    volumes: Vec<sauerteig_core::VolumeEdit>,
) -> Result<(), String> {
    session.handle(
        id,
        ClientMessage::EditTransaction {
            request: 1,
            base_revision,
            roots,
            objects,
            volumes,
        },
    )
}

#[test]
fn volume_layer_history_preserves_multiplayer_ownership_and_stale_clear_clocks() {
    use sauerteig_core::VolumeMaterial::{Glass, Water};
    let mut session = room();
    let (a, mut events) = peer(&mut session, true);
    let (b, _events) = peer(&mut session, true);
    volume_transaction(
        &mut session,
        a,
        0,
        vec![],
        vec![],
        vec![volume_edit([160, 32, 160], [176, 48, 176], Some(Water))],
    )
    .unwrap();
    assert_eq!(session.revision, 1);
    match events.try_recv().unwrap().packet.message.clone() {
        ServerMessage::TransactionPatch {
            volumes: Some(volumes),
            roots,
            objects,
            ..
        } => {
            assert_eq!(volumes, session.world.volumes());
            assert!(roots.is_empty() && objects.is_empty());
        }
        other => panic!("expected atomic material patch, got {other:?}"),
    }
    let water = session.world.volumes().to_vec();
    volume_transaction(
        &mut session,
        b,
        1,
        vec![],
        vec![],
        vec![volume_edit([240, 32, 160], [248, 48, 176], Some(Glass))],
    )
    .unwrap();
    let combined = session.world.volumes().to_vec();
    assert!(session.undo(a).unwrap_err().contains("later edit"));
    session.undo(b).unwrap();
    assert_eq!(session.world.volumes(), water);
    session.undo(a).unwrap();
    assert!(session.world.volumes().is_empty());
    assert!(volume_transaction(
        &mut session,
        a,
        0,
        vec![],
        vec![],
        vec![volume_edit([160, 32, 160], [176, 48, 176], None)]
    )
    .unwrap_err()
    .contains("changed after"));
    session.redo(a).unwrap();
    assert_eq!(session.world.volumes(), water);
    session.redo(b).unwrap();
    assert_eq!(session.world.volumes(), combined);
}

#[test]
fn invalid_material_object_and_fighter_changes_leave_mixed_transactions_atomic() {
    use sauerteig_core::{ObjectEdit, ObjectPlacement, VolumeMaterial::Glass};
    let mut session = room();
    let (builder, _events) = peer(&mut session, true);
    let (fighter, _events) = peer(&mut session, false);
    session.peers.get_mut(&fighter).unwrap().player.position = Vec3::new(20.5, 4.0, 20.5);
    let root = fine_root([30, 4, 20], [1, 2, 3], 1);
    let before = serde_json::to_vec(&session.saved()).unwrap();
    let invalid_object = ObjectEdit::Create {
        object: ObjectPlacement {
            kind: ObjectKind::Tree,
            position: Vec3::splat(f32::NAN),
            rotation: 0.0,
        },
    };
    assert!(volume_transaction(
        &mut session,
        builder,
        0,
        vec![root.clone()],
        vec![invalid_object],
        vec![volume_edit([240, 32, 160], [248, 48, 176], Some(Glass))]
    )
    .is_err());
    assert_eq!(serde_json::to_vec(&session.saved()).unwrap(), before);
    assert!(volume_transaction(
        &mut session,
        builder,
        0,
        vec![root.clone()],
        vec![],
        vec![volume_edit([160, 32, 160], [176, 48, 176], Some(Glass))]
    )
    .unwrap_err()
    .contains("inside a player"));
    assert_eq!(serde_json::to_vec(&session.saved()).unwrap(), before);
    assert!(volume_transaction(
        &mut session,
        builder,
        0,
        vec![root],
        vec![],
        vec![volume_edit([0, -1, 0], [8, 8, 8], None)]
    )
    .is_err());
    assert_eq!(serde_json::to_vec(&session.saved()).unwrap(), before);
    assert!(session.peers[&builder].undo.is_empty());
}

#[test]
fn volume_replacements_obey_combined_wire_budget_before_any_mutation() {
    use sauerteig_core::VolumeMaterial::Water;
    let mut session = room();
    let (builder, _events) = peer(&mut session, true);
    session.geometry_bytes = crate::MAX_GEOMETRY_BYTES - 1;
    let before = session.world.volumes().to_vec();
    assert!(volume_transaction(
        &mut session,
        builder,
        0,
        vec![],
        vec![],
        vec![volume_edit([160, 32, 160], [176, 48, 176], Some(Water))]
    )
    .unwrap_err()
    .contains("snapshot limit"));
    assert_eq!(session.revision, 0);
    assert_eq!(session.world.volumes(), before);
    assert!(session.peers[&builder].undo.is_empty());
}

#[test]
fn water_does_not_block_combat_but_glass_stops_shots_and_blast_visibility() {
    use sauerteig_core::{
        MaterialVolume,
        VolumeMaterial::{Glass, Water},
    };
    let mut session = room();
    let (shooter, _events) = peer(&mut session, false);
    let (target, _events) = peer(&mut session, false);
    session.peers.get_mut(&shooter).unwrap().player.position = Vec3::new(0.0, 1.0, 0.0);
    session.peers.get_mut(&target).unwrap().player.position = Vec3::new(6.0, 1.0, 0.0);
    let water = MaterialVolume {
        bounds: FineBox {
            min: [8, 8, -8],
            max: [16, 40, 8],
        },
        material: Water,
    };
    session.world.replace_volumes(&[water]).unwrap();
    let origin = session.peers[&shooter].player.eye();
    assert_eq!(
        session.trace(origin, Vec3::X, 10.0, shooter).1,
        Some(target)
    );
    session
        .world
        .replace_volumes(&[
            water,
            MaterialVolume {
                bounds: FineBox {
                    min: [24, 8, -8],
                    max: [25, 40, 8],
                },
                material: Glass,
            },
        ])
        .unwrap();
    let (distance, hit, collided) = session.trace(origin, Vec3::X, 10.0, shooter);
    assert_eq!(distance, 3.0);
    assert_eq!(hit, None);
    assert!(collided);
    let center = Vec3::new(2.9, 2.0, 0.0);
    session.explode(center, shooter, None);
    assert_eq!(session.peers[&target].player.health, 100);
    session.world.replace_volumes(&[water]).unwrap();
    session.explode(center, shooter, None);
    assert!(session.peers[&target].player.health < 100);
}

fn peer(
    session: &mut Session,
    editing: bool,
) -> (u64, mpsc::UnboundedReceiver<crate::outbound::Queued>) {
    let (events, receiver) = crate::outbound::channel();
    let (snapshots, _) = watch::channel(None);
    let (id, _) = session.join(events, snapshots).unwrap();
    session.peers.get_mut(&id).unwrap().player.editing = editing;
    (id, receiver)
}

fn edit(material: u8) -> Vec<CellEdit> {
    vec![CellEdit {
        pos: [20, 4, 20],
        cell: Some(Cell::solid(material)),
    }]
}

fn fine_root(pos: CellPos, offset: [i32; 3], material: u8) -> RootEdit {
    let origin = std::array::from_fn(|i| pos[i] * 8 + offset[i]);
    RootEdit {
        pos,
        fragments: vec![CellFragment {
            bounds: FineBox {
                min: origin,
                max: origin.map(|v| v + 1),
            },
            origin,
            size: 1,
            cell: Cell::solid(material),
        }],
    }
}

fn assert_root(world: &World, root: &RootEdit) {
    assert_eq!(
        world.root_fragments(root.pos),
        sauerteig_core::canonical_root_fragments(root.pos, &root.fragments),
    );
}

#[test]
fn sparse_history_preserves_legacy_ownership_and_stale_empty_roots() {
    let mut session = room();
    let (a, _a) = peer(&mut session, true);
    let (b, _b) = peer(&mut session, true);
    let detail = fine_root([20, 4, 20], [1, 2, 3], 2);
    session.edit_roots(a, 0, vec![detail.clone()]).unwrap();
    session.edit(b, edit(1)).unwrap();
    assert!(session.undo(a).unwrap_err().contains("later edit"));
    session.undo(b).unwrap();
    assert_root(&session.world, &detail);
    session.undo(a).unwrap();
    assert!(session.world.root_fragments(detail.pos).is_empty());
    assert!(!session.cell_revisions.contains_key(&detail.pos));
    // Clearing restored the old ownership lineage, but did not rewind the
    // stale-edit clock. Even a stale clear of the now-empty root is rejected.
    assert!(session
        .edit_roots(
            a,
            0,
            vec![RootEdit {
                pos: detail.pos,
                fragments: vec![]
            }]
        )
        .unwrap_err()
        .contains("changed after"));
    assert_eq!(session.revision, 4);
    // A shared boundary or an empty gap inside a selection's bounding box
    // does not make an unrelated root conflict with this old snapshot.
    let unrelated = fine_root([21, 4, 20], [0, 2, 3], 1);
    session.edit_roots(b, 0, vec![unrelated.clone()]).unwrap();
    assert_root(&session.world, &unrelated);
    session.redo(a).unwrap();
    assert_root(&session.world, &detail);
}

#[test]
fn fine_pastes_and_redo_check_exact_fighter_overlap_atomically() {
    let mut session = room();
    let (builder, _a) = peer(&mut session, true);
    let (fighter, _b) = peer(&mut session, false);
    let detail = fine_root([2, 1, 2], [3, 1, 3], 2);
    session
        .edit_roots(builder, 0, vec![detail.clone()])
        .unwrap();
    session.undo(builder).unwrap();
    session.peers.get_mut(&fighter).unwrap().player.position = Vec3::new(2.5, 1.0, 2.5);
    assert!(session
        .redo(builder)
        .unwrap_err()
        .contains("inside a player"));
    assert_eq!(session.peers[&builder].redo.len(), 1);
    let safe = fine_root([20, 4, 20], [1, 1, 1], 1);
    assert!(session
        .edit_roots(builder, 2, vec![safe.clone(), detail])
        .unwrap_err()
        .contains("inside a player"));
    assert_eq!(session.revision, 2);
    assert!(session.world.root_fragments(safe.pos).is_empty());
    // This tiny fragment shares the fighter's root, but is outside the body.
    let beside = fine_root([2, 1, 2], [7, 1, 3], 1);
    session
        .edit_roots(builder, 2, vec![beside.clone()])
        .unwrap();
    assert_root(&session.world, &beside);
    assert!(!session.blocked(&session.peers[&fighter].player));
}

#[test]
fn large_root_pastes_are_atomic_and_history_is_bounded_by_geometry() {
    let mut session = room();
    let (builder, _events) = peer(&mut session, true);
    let mut roots = Vec::new();
    for x in WORLD_MIN..WORLD_MAX {
        for z in WORLD_MIN..WORLD_MAX {
            roots.push(legacy_root(CellEdit {
                pos: [x, 8, z],
                cell: Some(Cell::solid(0)),
            }));
        }
    }
    assert_eq!(roots.len(), MAX_EDITS);
    for revision in 0..9 {
        for root in &mut roots {
            root.fragments[0].cell.material = (revision % 2) as u8;
        }
        // Test storage capacity independently of the wall-clock rate limit.
        session.peers.get_mut(&builder).unwrap().edit_tokens = EDIT_BURST;
        session
            .edit_roots(builder, revision, roots.clone())
            .unwrap();
        assert_eq!(
            session.revision,
            revision + 1,
            "one revision per whole paste"
        );
    }
    let history = &session.peers[&builder].undo;
    assert_eq!(
        history.len(),
        8,
        "large operations spend the history budget"
    );
    assert!(history.iter().map(|e| e.complexity).sum::<usize>() <= MAX_HISTORY_COMPLEXITY);
    let revision = session.revision;
    let mut invalid = roots;
    invalid.last_mut().unwrap().fragments[0].size = 3;
    assert!(session.edit_roots(builder, revision, invalid).is_err());
    assert_eq!(session.revision, revision);
}

#[test]
fn legacy_snapshots_decode_without_detailed_roots() {
    let saved: SavedWorld = serde_json::from_value(serde_json::json!({
        "revision": 0, "cells": [], "objects": [],
    }))
    .unwrap();
    assert!(saved.roots.is_empty());
    let welcome: ServerMessage = serde_json::from_value(serde_json::json!({
        "type": "welcome", "id": 1, "revision": 0, "cells": [],
        "objects": [], "players": [], "editing_allowed": true,
    }))
    .unwrap();
    assert!(matches!(welcome, ServerMessage::Welcome { roots, .. } if roots.is_empty()));
}

#[test]
fn empty_clipped_fragments_do_not_create_revisions_or_claim_ownership() {
    let mut session = room();
    let (builder, _events) = peer(&mut session, true);
    let mut shape = sauerteig_core::Shape::solid();
    for vertex in &mut shape.vertices {
        vertex[0] /= 2;
    }
    let pos = [20, 4, 20];
    let origin = pos.map(|v| v * 8);
    let mut min = origin;
    min[0] += 6;
    let empty = RootEdit {
        pos,
        fragments: vec![CellFragment {
            bounds: FineBox {
                min,
                max: origin.map(|v| v + 8),
            },
            origin,
            size: 8,
            cell: Cell {
                shape: Some(shape),
                ..Cell::solid(1)
            },
        }],
    };
    session.edit_roots(builder, 0, vec![empty.clone()]).unwrap();
    assert_eq!(session.revision, 0);
    assert!(session.peers[&builder].undo.is_empty());
    let actual = fine_root([21, 4, 20], [1, 1, 1], 2);
    session.edit_roots(builder, 0, vec![empty, actual]).unwrap();
    assert_eq!(session.revision, 1);
    assert!(!session.cell_revisions.contains_key(&pos));
    assert!(!session.root_changed_at.contains_key(&pos));
    assert_eq!(session.peers[&builder].undo.back().unwrap().before.len(), 1);
    session.undo(builder).unwrap();
    assert!(session.world.detailed_roots().is_empty());
}

#[test]
fn precoalescing_saves_and_replacements_count_the_stored_geometry() {
    let pos = [20, 4, 20];
    let mut fragments = Vec::new();
    for x in 0..8 {
        for y in 0..8 {
            for z in 0..8 {
                fragments.extend(fine_root(pos, [x, y, z], 0).fragments);
            }
        }
    }
    assert_eq!(fragments.len(), 512);
    let dir = temporary_directory("precoalescing-save");
    let path = dir.join("world.json");
    write_world(
        &path,
        &SavedWorld {
            volumes: Vec::new(),
            format_version: 2,
            bounds: Default::default(),
            regions: Vec::new(),
            revision: 0,
            cells: vec![],
            roots: vec![RootEdit {
                pos,
                fragments: fragments.clone(),
            }],
            objects: vec![],
            next_object: 0,
        },
    )
    .unwrap();
    let mut session = Session::new(load_world(&path).unwrap(), true);
    assert_eq!(session.geometry_fragments, 1);
    assert_eq!(session.world.get(pos), Some(Cell::solid(0)));
    assert!(session.world.detailed_roots().is_empty());
    let (builder, _events) = peer(&mut session, true);
    for fragment in &mut fragments {
        fragment.cell.material = 2;
    }
    // Model an otherwise full room in the capacity counter: replacing its
    // one affected root with 512 coalescible pieces must spend no new space.
    session.geometry_fragments = MAX_WORLD_FRAGMENTS;
    session
        .edit_roots(builder, 0, vec![RootEdit { pos, fragments }])
        .unwrap();
    assert_eq!(session.geometry_fragments, MAX_WORLD_FRAGMENTS);
    assert_eq!(session.world.get(pos), Some(Cell::solid(2)));
    session.undo(builder).unwrap();
    assert_eq!(session.geometry_fragments, MAX_WORLD_FRAGMENTS);
    assert_eq!(session.world.get(pos), Some(Cell::solid(0)));
    // Restore the fixture's real occupancy and remove the loaded root.
    session.geometry_fragments = 1;
    session
        .edit(builder, vec![CellEdit { pos, cell: None }])
        .unwrap();
    assert_eq!(session.geometry_fragments, 0);
    session.undo(builder).unwrap();
    assert_eq!(session.geometry_fragments, 1);
    assert_eq!(session.world.get(pos), Some(Cell::solid(0)));
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn outbound_limit_covers_the_supported_snapshot_budget() {
    use sauerteig_core::{Shape, WORLD_HEIGHT, WORLD_MIN};
    let widest_cell = CellEdit {
        pos: [WORLD_MIN, WORLD_HEIGHT - 1, WORLD_MIN],
        cell: Some(Cell {
            shape: Some(Shape {
                diagonals: 63,
                ..Shape::solid()
            }),
            diagonal: true,
            ..Cell::solid(5)
        }),
    };
    assert!(serde_json::to_vec(&widest_cell).unwrap().len() < 256);
    let wide_root = legacy_root(widest_cell.clone());
    assert!(serde_json::to_vec(&wide_root).unwrap().len() < 320);
    let envelope = MAX_OBJECTS * 256 + MAX_PLAYERS * 1024 + 1024;
    assert!(envelope < crate::SNAPSHOT_ENVELOPE_BYTES);
    assert_eq!(
        crate::MAX_GEOMETRY_BYTES + crate::SNAPSHOT_ENVELOPE_BYTES,
        64 * 1024 * 1024
    );
    const {
        assert!(crate::MAX_WORLD_SNAPSHOT_BYTES + 16 * 1024 + 14 < OUTBOUND_WRITE_BYTES);
    }
    let request = ClientMessage::Edit {
        request: u64::MAX,
        edits: vec![widest_cell; MAX_EDITS],
    };
    assert!(serde_json::to_vec(&request).unwrap().len() < MESSAGE_BYTES);
}

#[test]
fn undo_never_erases_an_intervening_builders_work() {
    let mut session = room();
    let (a, _a) = peer(&mut session, true);
    let (b, _b) = peer(&mut session, true);
    session.edit(a, edit(0)).unwrap();
    session.edit(b, edit(1)).unwrap();
    assert!(session.undo(a).unwrap_err().contains("later edit"));
    session.edit(a, edit(2)).unwrap();
    session.undo(a).unwrap();
    assert_eq!(session.world.get([20, 4, 20]), Some(Cell::solid(1)));
    // A's earlier edit must not become undoable just because A undid a
    // subsequent overwrite; B still owns the restored value.
    assert!(session.undo(a).unwrap_err().contains("later edit"));
    session.undo(b).unwrap();
    session.undo(a).unwrap();
    assert_eq!(session.world.get([20, 4, 20]), None);
    assert_eq!(session.revision, 6);
}

#[test]
fn overlapping_history_can_be_undone_and_redone_repeatedly() {
    let mut session = room();
    let (a, _a) = peer(&mut session, true);
    session.edit(a, edit(0)).unwrap();
    session.edit(a, edit(2)).unwrap();
    for _ in 0..3 {
        session.undo(a).unwrap();
        session.undo(a).unwrap();
        assert_eq!(session.world.get([20, 4, 20]), None);
        session.redo(a).unwrap();
        assert_eq!(session.world.get([20, 4, 20]), Some(Cell::solid(0)));
        session.redo(a).unwrap();
        assert_eq!(session.world.get([20, 4, 20]), Some(Cell::solid(2)));
    }
    assert_eq!(session.revision, 14);
    session.undo(a).unwrap();
    session.edit(a, edit(1)).unwrap();
    assert_eq!(session.redo(a).unwrap_err(), "Nothing to redo.");
}

#[test]
fn redo_checks_ownership_even_when_another_builder_restored_the_same_value() {
    let mut session = room();
    let (a, _a) = peer(&mut session, true);
    let (b, _b) = peer(&mut session, true);
    session.edit(a, edit(0)).unwrap();
    session.edit(a, edit(2)).unwrap();
    session.undo(a).unwrap();
    session.edit(b, edit(1)).unwrap();
    session.edit(b, edit(0)).unwrap();
    assert!(session.redo(a).unwrap_err().contains("later edit"));
    assert_eq!(session.world.get([20, 4, 20]), Some(Cell::solid(0)));
    session.undo(b).unwrap();
    session.undo(b).unwrap();
    session.redo(a).unwrap();
    assert_eq!(session.world.get([20, 4, 20]), Some(Cell::solid(2)));
}

#[test]
fn redo_is_atomic_around_fighters_and_requires_edit_mode() {
    let mut session = room();
    let (a, _a) = peer(&mut session, true);
    let (b, _b) = peer(&mut session, false);
    session.edit(a, edit(0)).unwrap();
    session.undo(a).unwrap();
    session.peers.get_mut(&b).unwrap().player.position = Vec3::new(20.5, 4.0, 20.5);
    let revision = session.revision;
    assert!(session.redo(a).unwrap_err().contains("inside a player"));
    assert_eq!(session.revision, revision);
    assert_eq!(session.peers[&a].redo.len(), 1);
    session.peers.get_mut(&b).unwrap().player.position = spawn_point(0);
    session.peers.get_mut(&a).unwrap().player.editing = false;
    assert!(session.redo(a).is_err());
    session.peers.get_mut(&a).unwrap().player.editing = true;
    session.redo(a).unwrap();
    assert_eq!(session.world.get([20, 4, 20]), Some(Cell::solid(0)));
}

#[test]
fn edits_are_atomic_and_cannot_embed_a_fighter() {
    let mut session = room();
    let (builder, _a) = peer(&mut session, true);
    let (fighter, _b) = peer(&mut session, false);
    session.peers.get_mut(&fighter).unwrap().player.position = Vec3::new(2.5, 1.0, 2.5);
    let edits = vec![
        CellEdit {
            pos: [20, 4, 20],
            cell: Some(Cell::solid(1)),
        },
        CellEdit {
            pos: [2, 1, 2],
            cell: Some(Cell::solid(1)),
        },
    ];
    assert!(session.edit(builder, edits).is_err());
    assert_eq!(session.revision, 0);
    assert_eq!(session.world.get([20, 4, 20]), None);
    assert!(session.edit(fighter, edit(0)).is_err());
    let mut malformed = edit(0);
    malformed[0].cell.as_mut().unwrap().heights = [9; 4];
    assert!(session.edit(builder, malformed).is_err());
    assert_eq!(session.revision, 0);
}

#[test]
fn input_spam_cannot_advance_the_simulation() {
    let mut normal = room();
    let mut flooded = room();
    let (a, _a) = peer(&mut normal, false);
    let (b, _b) = peer(&mut flooded, false);
    let input = InputFrame {
        seq: 1,
        forward: 1.0,
        ..Default::default()
    };
    normal.handle(a, ClientMessage::Input { input }).unwrap();
    for seq in 1..=32 {
        flooded
            .handle(
                b,
                ClientMessage::Input {
                    input: InputFrame { seq, ..input },
                },
            )
            .unwrap();
    }
    for _ in 0..1000 {
        flooded
            .handle(
                b,
                ClientMessage::Input {
                    input: InputFrame { seq: 32, ..input },
                },
            )
            .unwrap();
    }
    normal.step();
    flooded.step();
    assert_eq!(
        normal.peers[&a].player.position,
        flooded.peers[&b].player.position
    );
    assert_eq!(flooded.peers[&b].player.last_input, 1);
    assert_eq!(flooded.peers[&b].inputs.len(), 31);
    assert_eq!(normal.tick, 1);
    assert!(flooded
        .handle(
            b,
            ClientMessage::Input {
                input: InputFrame {
                    forward: f32::NAN,
                    ..input
                }
            }
        )
        .is_err());
    assert!(flooded
        .handle(
            b,
            ClientMessage::Input {
                input: InputFrame {
                    forward: 2.0,
                    ..input
                }
            }
        )
        .is_err());
}

#[test]
fn burst_inputs_are_consumed_once_in_order_and_idle_ticks_do_not_repeat_motion() {
    let mut session = room();
    let (id, _events) = peer(&mut session, true);
    let mut expected = session.peers[&id].player.clone();
    for seq in 1..=3 {
        session
            .handle(
                id,
                ClientMessage::Input {
                    input: InputFrame {
                        seq,
                        forward: 1.0,
                        ..Default::default()
                    },
                },
            )
            .unwrap();
    }
    // Duplicate and stale packets must not add movement or replace a frame.
    for seq in [3, 2] {
        session
            .handle(
                id,
                ClientMessage::Input {
                    input: InputFrame {
                        seq,
                        forward: -1.0,
                        ..Default::default()
                    },
                },
            )
            .unwrap();
    }
    assert_eq!(session.peers[&id].inputs.len(), 3);
    for seq in 1..=3 {
        let input = InputFrame {
            seq,
            forward: 1.0,
            ..Default::default()
        };
        simulate_player(&session.world, &mut expected, &input, FIXED_DT);
        session.step();
        assert_eq!(session.peers[&id].player.last_input, seq);
        assert_eq!(session.peers[&id].player.position, expected.position);
    }
    session.step();
    assert_eq!(session.peers[&id].player.last_input, 3);
    assert_eq!(session.peers[&id].player.position, expected.position);
}

#[test]
fn missing_inputs_keep_gravity_running_without_acknowledging_new_frames() {
    let mut session = room();
    let (id, _events) = peer(&mut session, false);
    session.peers.get_mut(&id).unwrap().player.position = Vec3::new(0.5, 8.0, 0.5);
    session
        .handle(
            id,
            ClientMessage::Input {
                input: InputFrame {
                    seq: 17,
                    ..Default::default()
                },
            },
        )
        .unwrap();
    session.step();
    let first_height = session.peers[&id].player.position.y;
    session.step();
    assert!(session.peers[&id].player.position.y < first_height);
    assert_eq!(session.peers[&id].player.last_input, 17);
}

#[test]
fn overflowing_input_queue_disconnects_instead_of_skipping_prediction_frames() {
    let mut session = room();
    let (id, mut events) = peer(&mut session, false);
    for seq in 1..=MAX_INPUT_BACKLOG as u32 + 1 {
        session
            .handle(
                id,
                ClientMessage::Input {
                    input: InputFrame {
                        seq,
                        ..Default::default()
                    },
                },
            )
            .unwrap();
    }
    assert!(!session.peers.contains_key(&id));
    let message: ServerMessage = events.try_recv().unwrap().packet.message.clone();
    assert!(matches!(message, ServerMessage::Error { message } if message.contains("backlog")));
}

#[test]
fn mode_acknowledges_request_and_discards_old_mode_inputs() {
    let mut session = room();
    let (id, mut events) = peer(&mut session, false);
    for seq in 1..=3 {
        session
            .handle(
                id,
                ClientMessage::Input {
                    input: InputFrame {
                        seq,
                        forward: 1.0,
                        fire: true,
                        ..Default::default()
                    },
                },
            )
            .unwrap();
    }
    session
        .handle(
            id,
            ClientMessage::SetMode {
                request: 7,
                editing: true,
            },
        )
        .unwrap();
    let p = &session.peers[&id];
    assert!(p.inputs.is_empty());
    assert_eq!(p.player.last_input, 3);
    assert_eq!(p.player.mode_request, 7);
    assert!(p.player.editing);
    assert!(!p.input.fire);
    let message: ServerMessage = events.try_recv().unwrap().packet.message.clone();
    assert!(matches!(
        message,
        ServerMessage::ModeChanged {
            request: 7,
            editing: true
        }
    ));
    session
        .handle(
            id,
            ClientMessage::SetMode {
                request: 7,
                editing: false,
            },
        )
        .unwrap();
    assert!(
        session.peers[&id].player.editing,
        "duplicate request is idempotent"
    );
}

#[test]
fn denied_edit_mode_still_acknowledges_authoritative_mode() {
    let mut session = room();
    session.editing_allowed = false;
    let (id, mut events) = peer(&mut session, false);
    assert!(session
        .handle(
            id,
            ClientMessage::SetMode {
                request: 12,
                editing: true
            }
        )
        .is_err());
    let message: ServerMessage = events.try_recv().unwrap().packet.message.clone();
    assert!(matches!(
        message,
        ServerMessage::ModeChanged {
            request: 12,
            editing: false
        }
    ));
    assert_eq!(session.peers[&id].player.mode_request, 12);
    assert!(!session.peers[&id].player.editing);
}

#[test]
fn authoritative_hitscan_respects_walls_and_scores_a_kill() {
    let mut session = room();
    let (a, _a) = peer(&mut session, false);
    let (b, _b) = peer(&mut session, false);
    session.peers.get_mut(&a).unwrap().player.position = Vec3::new(0.5, 1.0, 4.5);
    session.peers.get_mut(&b).unwrap().player.position = Vec3::new(0.5, 1.0, 0.5);
    session.world.set([0, 2, 2], Some(Cell::solid(0)));
    let mut shots = Vec::new();
    session.fire(a, &mut shots);
    assert_eq!(session.peers[&b].player.health, 100);
    assert!(!shots.last().unwrap().hit);
    session.world.set([0, 2, 2], None);
    for _ in 0..3 {
        session.fire(a, &mut shots);
    }
    assert_eq!(session.peers[&a].player.score, 1);
    assert_eq!(session.peers[&b].player.health, 100);
}

#[test]
fn weapon_cooldowns_are_owned_by_server_ticks() {
    let mut session = room();
    let (a, _a) = peer(&mut session, false);
    let (b, _b) = peer(&mut session, false);
    session.peers.get_mut(&a).unwrap().player.position = Vec3::new(0.5, 1.0, 4.5);
    session.peers.get_mut(&b).unwrap().player.position = Vec3::new(0.5, 1.0, 0.5);
    for seq in 1..=12 {
        session
            .handle(
                a,
                ClientMessage::Input {
                    input: InputFrame {
                        seq,
                        fire: true,
                        ..Default::default()
                    },
                },
            )
            .unwrap();
        session.step();
    }
    assert_eq!(session.peers[&b].player.health, 66);
    session.step();
    assert_eq!(session.peers[&b].player.health, 32);
}

#[test]
fn rockets_hit_and_respawn_a_target() {
    let mut session = room();
    let (a, _a) = peer(&mut session, false);
    let (b, _b) = peer(&mut session, false);
    session.peers.get_mut(&a).unwrap().player.position = Vec3::new(0.5, 1.0, 4.5);
    session.peers.get_mut(&b).unwrap().player.position = Vec3::new(0.5, 1.0, 0.5);
    session.peers.get_mut(&a).unwrap().input.weapon = 1;
    let mut shots = Vec::new();
    session.fire(a, &mut shots);
    assert_eq!(session.rockets.len(), 1);
    for _ in 0..12 {
        session.advance_rockets(&mut shots);
    }
    assert!(session.rockets.is_empty());
    assert_eq!(session.peers[&a].player.score, 1);
    assert_eq!(session.peers[&b].player.health, 100);
}

#[test]
fn persistence_preserves_deformation_objects_and_revision() {
    let mut session = room();
    let (a, _a) = peer(&mut session, true);
    let slope = Cell {
        material: 3,
        heights: [2, 2, 8, 8],
        ..Cell::solid(3)
    };
    session
        .edit(
            a,
            vec![CellEdit {
                pos: [12, 5, 12],
                cell: Some(slope),
            }],
        )
        .unwrap();
    session
        .handle(
            a,
            ClientMessage::PlaceObject {
                kind: ObjectKind::Light,
                position: Vec3::new(1.0, 4.0, 1.0),
                rotation: 0.5,
            },
        )
        .unwrap();
    let dir = temporary_directory("save");
    let path = dir.join("world.json");
    write_world(&path, &session.saved()).unwrap();
    let loaded = load_world(&path).unwrap();
    assert_eq!(loaded.revision, 2);
    assert_eq!(
        World::from_cells(&loaded.cells).get([12, 5, 12]),
        stored([12, 5, 12], slope)
    );
    assert_eq!(loaded.objects.len(), 1);
    assert!(!path.with_extension("json.tmp").exists());
    std::fs::remove_dir_all(dir).unwrap();
}

fn temporary_directory(label: &str) -> PathBuf {
    let time = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path =
        std::env::temp_dir().join(format!("sauerteig-{label}-{}-{time}", std::process::id()));
    std::fs::create_dir_all(&path).unwrap();
    path
}

type ClientSocket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn send_client(socket: &mut ClientSocket, message: ClientMessage) {
    use futures_util::SinkExt;
    socket
        .send(tokio_tungstenite::tungstenite::Message::Text(
            serde_json::to_string(&message).unwrap().into(),
        ))
        .await
        .unwrap();
}

async fn receive_where(
    socket: &mut ClientSocket,
    matches: impl Fn(&ServerMessage) -> bool,
) -> ServerMessage {
    use futures_util::StreamExt;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let message = socket
                .next()
                .await
                .expect("connection open")
                .expect("valid frame");
            if let tokio_tungstenite::tungstenite::Message::Text(text) = message {
                let message: ServerMessage = serde_json::from_str(&text).unwrap();
                if matches(&message) {
                    return message;
                }
            }
        }
    })
    .await
    .expect("server response before timeout")
}

#[tokio::test]
async fn plain_http_root_says_the_server_is_running() {
    let dir = temporary_directory("http-root");
    let config = Config {
        world_path: dir.join("world.json"),
        editing_allowed: true,
    };
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(serve(listener, config, async {
        let _ = stopped.await;
    }));
    let response = tokio::task::spawn_blocking(move || {
        use std::io::{Read, Write};
        let mut stream = std::net::TcpStream::connect(address).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        stream
            .write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
            .unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        response
    })
    .await
    .unwrap();
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    assert!(response.contains("text/plain"), "{response}");
    assert!(response.contains("game connections use /ws"), "{response}");

    stop.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn websocket_large_arena_joins_and_edit_results_correlate_with_requests() {
    use futures_util::StreamExt;
    let dir = temporary_directory("large-arena-edit-results");
    let config = Config {
        world_path: dir.join("world.json"),
        editing_allowed: true,
    };
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(serve(listener, config, async {
        let _ = stopped.await;
    }));
    let (mut first, _) = tokio_tungstenite::connect_async(format!("ws://{address}/ws"))
        .await
        .unwrap();
    let welcome = tokio::time::timeout(Duration::from_secs(5), first.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let tokio_tungstenite::tungstenite::Message::Text(welcome) = welcome else {
        panic!("first frame must contain the complete Welcome");
    };
    assert!(welcome.len() > 256 * 1024, "regress the old outbound cap");
    assert!(welcome.len() < OUTBOUND_WRITE_BYTES);
    assert!(matches!(
        serde_json::from_str::<ServerMessage>(&welcome).unwrap(),
        ServerMessage::Welcome { cells, .. } if cells.len() == World::arena().cells().len()
    ));
    send_client(
        &mut first,
        ClientMessage::SetMode {
            request: 1,
            editing: true,
        },
    )
    .await;
    receive_where(&mut first, |m| {
        matches!(m, ServerMessage::ModeChanged { .. })
    })
    .await;

    // An unrelated failure queued before an edit must not masquerade as
    // that edit's rejection. Changed edits publish their patch first.
    send_client(&mut first, ClientMessage::Undo).await;
    send_client(
        &mut first,
        ClientMessage::Edit {
            request: 9,
            edits: edit(1),
        },
    )
    .await;
    let next_event = |m: &ServerMessage| !matches!(m, ServerMessage::Snapshot { .. });
    assert!(matches!(receive_where(&mut first, next_event).await,
        ServerMessage::Error { message } if message.contains("Nothing to undo")));
    assert!(matches!(
        receive_where(&mut first, next_event).await,
        ServerMessage::WorldPatch { revision: 1, .. }
    ));
    assert!(matches!(
        receive_where(&mut first, next_event).await,
        ServerMessage::EditResult {
            request: 9,
            error: None
        }
    ));

    // A no-op still finishes its exact request, without a new patch.
    send_client(
        &mut first,
        ClientMessage::Edit {
            request: 10,
            edits: edit(1),
        },
    )
    .await;
    assert!(matches!(
        receive_where(&mut first, next_event).await,
        ServerMessage::EditResult {
            request: 10,
            error: None
        }
    ));
    send_client(
        &mut first,
        ClientMessage::Edit {
            request: 11,
            edits: vec![CellEdit {
                pos: [1000, 4, 0],
                cell: Some(Cell::solid(0)),
            }],
        },
    )
    .await;
    assert!(matches!(
        receive_where(&mut first, next_event).await,
        ServerMessage::EditResult {
            request: 11,
            error: Some(_)
        }
    ));

    let (mut second, _) = tokio_tungstenite::connect_async(format!("ws://{address}/ws"))
        .await
        .unwrap();
    assert!(matches!(
        receive_where(&mut second, |m| matches!(m, ServerMessage::Welcome { .. })).await,
        ServerMessage::Welcome { revision: 1, .. }
    ));
    send_client(
        &mut second,
        ClientMessage::SetMode {
            request: 1,
            editing: true,
        },
    )
    .await;
    receive_where(&mut second, |m| {
        matches!(m, ServerMessage::ModeChanged { .. })
    })
    .await;
    send_client(
        &mut second,
        ClientMessage::Edit {
            request: 21,
            edits: edit(2),
        },
    )
    .await;
    assert!(matches!(
        receive_where(&mut first, next_event).await,
        ServerMessage::WorldPatch { revision: 2, .. }
    ));
    // The peer's identical patch does not acknowledge our later request.
    send_client(
        &mut first,
        ClientMessage::Edit {
            request: 12,
            edits: edit(2),
        },
    )
    .await;
    assert!(matches!(
        receive_where(&mut first, next_event).await,
        ServerMessage::EditResult {
            request: 12,
            error: None
        }
    ));
    assert!(matches!(
        receive_where(&mut second, next_event).await,
        ServerMessage::WorldPatch { revision: 2, .. }
    ));
    assert!(matches!(
        receive_where(&mut second, next_event).await,
        ServerMessage::EditResult {
            request: 21,
            error: None
        }
    ));

    first.close(None).await.unwrap();
    second.close(None).await.unwrap();
    stop.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn websocket_clients_share_edits_reject_bad_inputs_and_rejoin_saved_world() {
    use futures_util::SinkExt;
    let dir = temporary_directory("websocket");
    let config = Config {
        world_path: dir.join("world.json"),
        editing_allowed: true,
    };
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let server_config = config.clone();
    let server = tokio::spawn(serve(listener, server_config, async {
        let _ = stopped.await;
    }));
    let (mut first, _) = tokio_tungstenite::connect_async(format!("ws://{address}/ws"))
        .await
        .unwrap();
    receive_where(&mut first, |m| matches!(m, ServerMessage::Welcome { .. })).await;
    send_client(
        &mut first,
        ClientMessage::SetMode {
            request: 1,
            editing: true,
        },
    )
    .await;
    let slope = Cell {
        material: 2,
        heights: [2, 2, 8, 8],
        ..Cell::solid(2)
    };
    send_client(
        &mut first,
        ClientMessage::Edit {
            request: 1,
            edits: vec![CellEdit {
                pos: [30, 8, 30],
                cell: Some(slope),
            }],
        },
    )
    .await;
    receive_where(&mut first, |m| {
        matches!(m, ServerMessage::WorldPatch { revision: 1, .. })
    })
    .await;

    let (mut second, _) = tokio_tungstenite::connect_async(format!("ws://{address}/ws"))
        .await
        .unwrap();
    let welcome = receive_where(&mut second, |m| matches!(m, ServerMessage::Welcome { .. })).await;
    if let ServerMessage::Welcome {
        revision,
        cells,
        players,
        ..
    } = welcome
    {
        assert_eq!(revision, 1);
        assert_eq!(players.len(), 2);
        assert_eq!(
            World::from_cells(&cells).get([30, 8, 30]),
            stored([30, 8, 30], slope)
        );
    } else {
        unreachable!();
    }

    send_client(
        &mut second,
        ClientMessage::SetMode {
            request: 1,
            editing: true,
        },
    )
    .await;
    send_client(
        &mut second,
        ClientMessage::Edit {
            request: 1,
            edits: vec![CellEdit {
                pos: [30, 8, 30],
                cell: Some(Cell::solid(4)),
            }],
        },
    )
    .await;
    receive_where(&mut first, |m| {
        matches!(m, ServerMessage::WorldPatch { revision: 2, .. })
    })
    .await;
    send_client(&mut first, ClientMessage::Undo).await;
    let error = receive_where(&mut first, |m| matches!(m, ServerMessage::Error { .. })).await;
    assert!(matches!(error, ServerMessage::Error { message } if message.contains("later edit")));
    send_client(
        &mut first,
        ClientMessage::Input {
            input: InputFrame {
                seq: 1,
                forward: 2.0,
                ..Default::default()
            },
        },
    )
    .await;
    receive_where(
        &mut first,
        |m| matches!(m, ServerMessage::Error { message } if message.contains("Invalid input")),
    )
    .await;
    first
        .send(tokio_tungstenite::tungstenite::Message::Text(
            "{\"type\":\"input\",\"input\":{\"yaw\":1e999}}".into(),
        ))
        .await
        .unwrap();
    receive_where(
        &mut first,
        |m| matches!(m, ServerMessage::Error { message } if message.contains("Invalid message")),
    )
    .await;
    send_client(&mut first, ClientMessage::Save).await;
    receive_where(&mut first, |m| matches!(m, ServerMessage::Notice { .. })).await;
    first.close(None).await.unwrap();
    second.close(None).await.unwrap();
    stop.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(load_world(&config.world_path).unwrap().revision, 2);

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(serve(listener, config, async {
        let _ = stopped.await;
    }));
    let (mut rejoined, _) = tokio_tungstenite::connect_async(format!("ws://{address}/ws"))
        .await
        .unwrap();
    let welcome = receive_where(&mut rejoined, |m| {
        matches!(m, ServerMessage::Welcome { .. })
    })
    .await;
    if let ServerMessage::Welcome {
        revision, cells, ..
    } = welcome
    {
        assert_eq!(revision, 2);
        assert_eq!(
            World::from_cells(&cells).get([30, 8, 30]),
            Some(Cell::solid(4))
        );
    } else {
        unreachable!();
    }
    rejoined.close(None).await.unwrap();
    stop.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn websocket_combat_produces_authoritative_damage_score_and_respawn() {
    let dir = temporary_directory("combat");
    let config = Config {
        world_path: dir.join("world.json"),
        editing_allowed: true,
    };
    let mut saved = room().saved();
    // Player IDs rotate authored spawn choices, so ID 1 starts at +Z and
    // can shoot straight toward ID 2 with yaw zero.
    saved.objects = vec![
        WorldObject {
            id: 1,
            kind: ObjectKind::Spawn,
            position: Vec3::new(0.5, 1.0, 0.5),
            rotation: 0.0,
        },
        WorldObject {
            id: 2,
            kind: ObjectKind::Spawn,
            position: Vec3::new(0.5, 1.0, 4.5),
            rotation: 0.0,
        },
    ];
    write_world(&config.world_path, &saved).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(serve(listener, config, async {
        let _ = stopped.await;
    }));
    let (mut attacker, _) = tokio_tungstenite::connect_async(format!("ws://{address}/ws"))
        .await
        .unwrap();
    let welcome = receive_where(&mut attacker, |m| {
        matches!(m, ServerMessage::Welcome { .. })
    })
    .await;
    let attacker_id = if let ServerMessage::Welcome { id, .. } = welcome {
        id
    } else {
        unreachable!()
    };
    let (mut target, _) = tokio_tungstenite::connect_async(format!("ws://{address}/ws"))
        .await
        .unwrap();
    let welcome = receive_where(&mut target, |m| matches!(m, ServerMessage::Welcome { .. })).await;
    let target_id = if let ServerMessage::Welcome { id, .. } = welcome {
        id
    } else {
        unreachable!()
    };
    let mut saw_damage = false;
    let mut saw_respawn = false;
    for seq in 1..=12 {
        send_client(
            &mut attacker,
            ClientMessage::Input {
                input: InputFrame {
                    seq,
                    fire: true,
                    ..Default::default()
                },
            },
        )
        .await;
        let snapshot = receive_where(&mut attacker, |m| {
            matches!(m, ServerMessage::Snapshot { .. })
        })
        .await;
        if let ServerMessage::Snapshot {
            players, revision, ..
        } = snapshot
        {
            assert_eq!(revision, 0);
            let victim = players.iter().find(|p| p.id == target_id).unwrap();
            let shooter = players.iter().find(|p| p.id == attacker_id).unwrap();
            saw_damage |= victim.health < 100;
            if shooter.score == 1 {
                assert_eq!(victim.health, 100);
                saw_respawn = true;
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(60)).await;
    }
    assert!(saw_damage, "client observed server-authoritative damage");
    assert!(saw_respawn, "client observed score and healthy respawn");
    attacker.close(None).await.unwrap();
    target.close(None).await.unwrap();
    stop.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    std::fs::remove_dir_all(dir).unwrap();
}

fn object_at(position: Vec3) -> sauerteig_core::ObjectPlacement {
    sauerteig_core::ObjectPlacement {
        kind: ObjectKind::WallLamp,
        position,
        rotation: 0.75,
    }
}

fn transaction(
    session: &mut Session,
    id: u64,
    base_revision: u64,
    roots: Vec<RootEdit>,
    objects: Vec<sauerteig_core::ObjectEdit>,
) -> Result<(), String> {
    session.handle(
        id,
        ClientMessage::EditTransaction {
            volumes: Vec::new(),
            request: 1,
            base_revision,
            roots,
            objects,
        },
    )
}

#[test]
fn object_validation_failures_leave_mixed_transactions_and_ids_untouched() {
    use sauerteig_core::ObjectEdit;
    let mut session = room();
    let (builder, _events) = peer(&mut session, true);
    let root = fine_root([20, 4, 20], [0, 0, 0], 2);
    for position in [
        Vec3::splat(f32::NAN),
        Vec3::splat(f32::INFINITY),
        Vec3::new(100.0, 2.0, 0.0),
    ] {
        assert!(transaction(
            &mut session,
            builder,
            0,
            vec![root.clone()],
            vec![ObjectEdit::Create {
                object: object_at(position)
            }]
        )
        .is_err());
        assert_eq!(session.revision, 0);
        assert_eq!(session.next_object, 1);
        assert!(session.objects.is_empty());
        assert!(session.world.root_fragments(root.pos).is_empty());
        assert!(session.peers[&builder].undo.is_empty());
    }
    let mut invalid_rotation = object_at(Vec3::new(1.0, 2.0, 1.0));
    invalid_rotation.rotation = f32::INFINITY;
    assert!(transaction(
        &mut session,
        builder,
        0,
        vec![root.clone()],
        vec![ObjectEdit::Create {
            object: invalid_rotation
        }]
    )
    .is_err());
    let placement = object_at(Vec3::new(1.0, 2.0, 1.0));
    transaction(
        &mut session,
        builder,
        0,
        vec![],
        vec![ObjectEdit::Create {
            object: placement.clone(),
        }],
    )
    .unwrap();
    assert!(transaction(
        &mut session,
        builder,
        1,
        vec![root.clone()],
        vec![
            ObjectEdit::Delete { id: 1 },
            ObjectEdit::Update {
                id: 1,
                object: placement
            }
        ]
    )
    .unwrap_err()
    .contains("once"));
    assert_eq!(session.objects.len(), 1);
    assert_eq!(session.revision, 1);
    assert!(session.world.root_fragments(root.pos).is_empty());
    session.next_object = u32::MAX - 1;
    assert!(transaction(
        &mut session,
        builder,
        1,
        vec![root.clone()],
        vec![
            ObjectEdit::Create {
                object: object_at(Vec3::new(2.0, 2.0, 1.0))
            },
            ObjectEdit::Create {
                object: object_at(Vec3::new(3.0, 2.0, 1.0))
            }
        ]
    )
    .unwrap_err()
    .contains("IDs"));
    assert_eq!(session.next_object, u32::MAX - 1);
    assert_eq!(session.revision, 1);
    assert!(session.world.root_fragments(root.pos).is_empty());
}

#[test]
fn mixed_history_fighter_checks_and_object_limit_are_atomic() {
    use sauerteig_core::ObjectEdit;
    let mut session = room();
    let (builder, _events) = peer(&mut session, true);
    let (fighter, _fighter_events) = peer(&mut session, false);
    let root = fine_root([2, 1, 2], [3, 1, 3], 2);
    transaction(
        &mut session,
        builder,
        0,
        vec![root.clone()],
        vec![ObjectEdit::Create {
            object: object_at(Vec3::new(20.0, 4.0, 20.0)),
        }],
    )
    .unwrap();
    let original = session.objects.clone();
    session.undo(builder).unwrap();
    session.peers.get_mut(&fighter).unwrap().player.position = Vec3::new(2.5, 1.0, 2.5);
    assert!(session
        .redo(builder)
        .unwrap_err()
        .contains("inside a player"));
    assert_eq!(session.revision, 2);
    assert!(session.objects.is_empty());
    assert!(session.world.root_fragments(root.pos).is_empty());
    session.peers.get_mut(&fighter).unwrap().player.position = Vec3::new(-4.0, 1.0, -4.0);
    session.redo(builder).unwrap();
    assert_eq!(session.objects, original);
    assert_root(&session.world, &root);
    transaction(
        &mut session,
        builder,
        3,
        vec![],
        vec![ObjectEdit::Delete { id: 1 }],
    )
    .unwrap();
    // Another builder may use the freed capacity. Undo must keep both halves
    // unchanged if restoring the deleted object would exceed the room limit.
    for id in 2..=MAX_OBJECTS as u32 + 1 {
        std::sync::Arc::make_mut(&mut session.objects).push(WorldObject {
            id,
            kind: ObjectKind::Light,
            position: Vec3::new(4.0, 2.0, 4.0),
            rotation: 0.0,
        });
    }
    assert!(session.undo(builder).unwrap_err().contains("limit"));
    assert_eq!(session.objects.len(), MAX_OBJECTS);
    assert_eq!(session.revision, 4);
}

#[test]
fn object_ownership_lineage_and_stale_clocks_survive_undo_redo() {
    use sauerteig_core::ObjectEdit;
    let mut session = room();
    let (a, _a) = peer(&mut session, true);
    let (b, _b) = peer(&mut session, true);
    transaction(
        &mut session,
        a,
        0,
        vec![],
        vec![ObjectEdit::Create {
            object: object_at(Vec3::new(1.0, 2.0, 1.0)),
        }],
    )
    .unwrap();
    let original = session.objects.clone();
    transaction(
        &mut session,
        b,
        1,
        vec![],
        vec![ObjectEdit::Update {
            id: 1,
            object: object_at(Vec3::new(2.0, 2.0, 1.0)),
        }],
    )
    .unwrap();
    assert!(session.undo(a).unwrap_err().contains("later edit"));
    session.undo(b).unwrap();
    assert_eq!(session.objects, original);
    assert!(transaction(
        &mut session,
        a,
        1,
        vec![],
        vec![ObjectEdit::Update {
            id: 1,
            object: object_at(Vec3::new(3.0, 2.0, 1.0))
        }]
    )
    .unwrap_err()
    .contains("changed after"));
    session.undo(a).unwrap();
    assert!(session.objects.is_empty());
    session.redo(a).unwrap();
    assert_eq!(session.objects, original);
    session.redo(b).unwrap();
    assert_eq!(session.objects[0].position, Vec3::new(2.0, 2.0, 1.0));
    transaction(
        &mut session,
        a,
        6,
        vec![],
        vec![ObjectEdit::Delete { id: 1 }],
    )
    .unwrap();
    assert!(session.undo(b).unwrap_err().contains("later edit"));
    session.undo(a).unwrap();
    session.undo(b).unwrap();
    assert_eq!(session.objects, original);
}

#[test]
fn deleted_ids_remain_consumed_after_save_and_load() {
    let mut session = room();
    let (builder, _events) = peer(&mut session, true);
    session
        .handle(
            builder,
            ClientMessage::PlaceObject {
                kind: ObjectKind::Light,
                position: Vec3::new(1.0, 2.0, 1.0),
                rotation: -0.25,
            },
        )
        .unwrap();
    session
        .handle(builder, ClientMessage::RemoveObject { id: 1 })
        .unwrap();
    let dir = temporary_directory("consumed-object-ids");
    let path = dir.join("world.json");
    write_world(&path, &session.saved()).unwrap();
    let mut loaded = Session::new(load_world(&path).unwrap(), true);
    let (builder, _events) = peer(&mut loaded, true);
    loaded
        .handle(
            builder,
            ClientMessage::PlaceObject {
                kind: ObjectKind::Light,
                position: Vec3::new(1.0, 2.0, 1.0),
                rotation: -0.25,
            },
        )
        .unwrap();
    assert_eq!(loaded.objects[0].id, 2);
    assert_eq!(loaded.objects[0].rotation, -0.25);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn geometry_conflict_blocks_the_object_half_of_mixed_history() {
    use sauerteig_core::ObjectEdit;
    let mut session = room();
    let (a, _a) = peer(&mut session, true);
    let (b, _b) = peer(&mut session, true);
    let root = fine_root([20, 4, 20], [1, 1, 1], 2);
    transaction(
        &mut session,
        a,
        0,
        vec![root.clone()],
        vec![ObjectEdit::Create {
            object: object_at(Vec3::new(20.0, 5.0, 20.0)),
        }],
    )
    .unwrap();
    let objects = session.objects.clone();
    session.edit(b, edit(3)).unwrap();
    assert!(session.undo(a).unwrap_err().contains("later edit"));
    assert_eq!(session.objects, objects);
    assert_eq!(session.world.get(root.pos), Some(Cell::solid(3)));
    assert_eq!(session.revision, 2);
    session.undo(b).unwrap();
    session.undo(a).unwrap();
    assert!(session.objects.is_empty());
    assert!(session.world.root_fragments(root.pos).is_empty());
    session.edit(b, edit(4)).unwrap();
    assert!(session.redo(a).unwrap_err().contains("later edit"));
    assert!(session.objects.is_empty());
    assert_eq!(session.world.get(root.pos), Some(Cell::solid(4)));
}

#[test]
fn exhausted_revision_can_reload_without_overflowing_on_another_edit() {
    let mut session = room();
    session.revision = u64::MAX - 1;
    let (builder, _events) = peer(&mut session, true);
    session.edit(builder, edit(2)).unwrap();
    assert_eq!(session.revision, u64::MAX);
    let dir = temporary_directory("exhausted-revision");
    let path = dir.join("world.json");
    write_world(&path, &session.saved()).unwrap();
    let mut loaded = Session::new(load_world(&path).unwrap(), true);
    let (builder, _events) = peer(&mut loaded, true);
    assert!(loaded
        .edit(builder, edit(3))
        .unwrap_err()
        .contains("exhausted"));
    assert_eq!(loaded.world.get([20, 4, 20]), Some(Cell::solid(2)));
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn fighters_standing_on_a_negative_y_floor_do_not_fall_out_of_the_world() {
    let mut cells = Vec::new();
    for x in -4..4 {
        for z in -4..4 {
            cells.push(CellEdit {
                pos: [x, -21, z],
                cell: Some(Cell::solid(0)),
            });
        }
    }
    let mut session = Session::new(
        SavedWorld {
            volumes: Vec::new(),
            revision: 0,
            format_version: 2,
            bounds: sauerteig_core::WorldBounds {
                min: [-16, -32, -16],
                max: [16, 16, 16],
            },
            regions: Vec::new(),
            cells,
            roots: Vec::new(),
            objects: vec![WorldObject {
                id: 1,
                kind: ObjectKind::Spawn,
                position: Vec3::new(0.5, -20.0, 0.5),
                rotation: 0.0,
            }],
            next_object: 2,
        },
        true,
    );
    let (fighter, _events) = peer(&mut session, false);
    for _ in 0..120 {
        session.step();
    }
    let player = &session.peers[&fighter].player;
    assert!(player.grounded);
    assert!((player.position.y + 20.0).abs() < 0.01);
    assert_eq!(player.health, 100);
    assert_eq!(player.score, 0);
    session.peers.get_mut(&fighter).unwrap().player.position = Vec3::new(0.5, -41.0, 0.5);
    session.step();
    assert_eq!(session.peers[&fighter].player.score, -1);
    assert!((session.peers[&fighter].player.position.y + 20.0).abs() < 0.01);
}

#[test]
fn snapshot_byte_limit_rejects_mixed_edits_and_history_before_mutation() {
    use sauerteig_core::ObjectEdit;
    let mut session = room();
    let (builder, _events) = peer(&mut session, true);
    let root = fine_root([20, 4, 20], [1, 1, 1], 2);
    let initial_bytes = session.geometry_bytes;
    session.geometry_bytes = crate::MAX_GEOMETRY_BYTES;
    let error = transaction(
        &mut session,
        builder,
        0,
        vec![root.clone()],
        vec![ObjectEdit::Create {
            object: object_at(Vec3::new(20.0, 5.0, 20.0)),
        }],
    )
    .unwrap_err();
    assert!(error.contains("64 MiB snapshot limit"));
    assert_eq!(session.revision, 0);
    assert_eq!(session.next_object, 1);
    assert!(session.objects.is_empty());
    assert!(session.world.root_fragments(root.pos).is_empty());
    assert!(session.peers[&builder].undo.is_empty());
    session.geometry_bytes = initial_bytes;
    transaction(
        &mut session,
        builder,
        0,
        vec![root.clone()],
        vec![ObjectEdit::Create {
            object: object_at(Vec3::new(20.0, 5.0, 20.0)),
        }],
    )
    .unwrap();
    let placed_bytes = session.geometry_bytes;
    assert!(placed_bytes > initial_bytes);
    session.undo(builder).unwrap();
    assert_eq!(session.geometry_bytes, initial_bytes);
    session.redo(builder).unwrap();
    assert_eq!(session.geometry_bytes, placed_bytes);
    transaction(
        &mut session,
        builder,
        3,
        vec![RootEdit {
            pos: root.pos,
            fragments: vec![],
        }],
        vec![ObjectEdit::Delete { id: 1 }],
    )
    .unwrap();
    assert_eq!(session.geometry_bytes, initial_bytes);
    session.geometry_bytes = crate::MAX_GEOMETRY_BYTES;
    assert!(session
        .undo(builder)
        .unwrap_err()
        .contains("64 MiB snapshot limit"));
    assert_eq!(session.revision, 4);
    assert!(session.objects.is_empty());
    assert!(session.world.root_fragments(root.pos).is_empty());
    assert_eq!(session.peers[&builder].undo.len(), 2);
}

#[test]
fn compact_snapshot_accounting_is_small_conservative_and_survives_cuts() {
    let source = CellFragment {
        bounds: FineBox {
            min: [0, 0, 0],
            max: [1024, 1024, 1024],
        },
        origin: [0, 0, 0],
        size: 1024,
        cell: Cell::solid(0),
    };
    let saved = SavedWorld {
        volumes: Vec::new(),
        revision: 0,
        format_version: 2,
        bounds: sauerteig_core::WorldBounds {
            min: [-256, -256, -256],
            max: [256, 256, 256],
        },
        regions: vec![source],
        cells: vec![],
        roots: vec![],
        objects: vec![],
        next_object: 1,
    };
    let mut session = Session::new(saved, true);
    let (builder, _events) = peer(&mut session, true);
    let initial_bytes = session.geometry_bytes;
    assert!(
        initial_bytes < 1024,
        "a 128m cube remains one serialized fragment"
    );
    transaction(
        &mut session,
        builder,
        0,
        vec![RootEdit {
            pos: [20, 20, 20],
            fragments: vec![],
        }],
        vec![],
    )
    .unwrap();
    assert!(session.geometry_bytes > initial_bytes);
    // The cut splits the cube into the leaves around it; the file holds them
    // as regions and cells, and the room counts each by its own JSON.
    let saved = session.saved();
    assert!(saved.roots.is_empty());
    assert!(saved.regions.len() > 1);
    assert_eq!(
        session.geometry_fragments,
        saved.regions.len() + saved.cells.len()
    );
    crate::validate_snapshot_geometry(&saved.regions, &saved.cells, &saved.roots).unwrap();
    let expected = saved
        .regions
        .iter()
        .map(|fragment| crate::json_bytes(fragment) + 1)
        .sum::<usize>()
        + saved
            .cells
            .iter()
            .map(|cell| crate::json_bytes(cell) + 1)
            .sum::<usize>();
    assert_eq!(session.geometry_bytes, expected);
    let (events, _receiver) = crate::outbound::channel();
    let (snapshots, _) = watch::channel(None);
    let (_, welcome) = session.join(events, snapshots).unwrap();
    let welcome = welcome.expand();
    assert!(crate::json_bytes(&welcome) < session.geometry_bytes + crate::SNAPSHOT_ENVELOPE_BYTES);
    assert_eq!(Session::new(saved, true).geometry_bytes, expected);
    // Undoing the cut merges the leaves back into one region.
    session.undo(builder).unwrap();
    assert_eq!(session.geometry_bytes, initial_bytes);
    assert_eq!(session.geometry_fragments, 1);
}

#[test]
fn oversized_save_files_fail_with_the_native_snapshot_limit() {
    let dir = temporary_directory("snapshot-size");
    let path = dir.join("world.json");
    std::fs::create_dir_all(&dir).unwrap();
    let file = std::fs::File::create(&path).unwrap();
    file.set_len(crate::MAX_WORLD_SNAPSHOT_BYTES as u64 + 1)
        .unwrap();
    let error = match load_world(&path) {
        Ok(_) => panic!("oversized world was accepted"),
        Err(error) => error,
    };
    assert!(error.contains("64 MiB snapshot limit"));
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn sync_acknowledges_prior_history_changes_without_requiring_edit_mode() {
    let mut session = room();
    let (builder, mut events) = peer(&mut session, true);
    session.edit(builder, edit(1)).unwrap();
    while events.try_recv().is_ok() {}
    session.handle(builder, ClientMessage::Undo).unwrap();
    session
        .handle(builder, ClientMessage::Sync { request: 42 })
        .unwrap();
    let patch: ServerMessage = events.try_recv().unwrap().packet.message.clone();
    let synced: ServerMessage = events.try_recv().unwrap().packet.message.clone();
    assert!(matches!(
        patch,
        ServerMessage::WorldPatch { revision: 2, .. }
    ));
    assert!(matches!(synced, ServerMessage::Synced { request: 42 }));
    assert!(session.world.get([20, 4, 20]).is_none());
    assert_eq!(session.peers[&builder].redo.len(), 1);
    session.peers.get_mut(&builder).unwrap().player.editing = false;
    session
        .handle(builder, ClientMessage::Sync { request: 43 })
        .unwrap();
    let synced: ServerMessage = events.try_recv().unwrap().packet.message.clone();
    assert!(matches!(synced, ServerMessage::Synced { request: 43 }));
    assert_eq!(session.revision, 2);
}

#[test]
fn a_room_counts_the_pieces_and_bytes_its_save_will_hold() {
    // A fragment of a distant source, half a metre wide: the tree stores it
    // as cubes, and the file writes each as a region.
    let root = RootEdit {
        pos: [4000, 4000, 4000],
        fragments: vec![CellFragment {
            bounds: FineBox {
                min: [32000, 32000, 32000],
                max: [32004, 32008, 32008],
            },
            origin: [0, 0, 0],
            size: 65536,
            cell: Cell::solid(0),
        }],
    };
    let saved = SavedWorld {
        volumes: Vec::new(),
        revision: 0,
        format_version: 2,
        bounds: sauerteig_core::WorldBounds {
            min: [0, 0, 0],
            max: [8192, 8192, 8192],
        },
        regions: vec![],
        cells: vec![],
        roots: vec![root],
        objects: vec![],
        next_object: 1,
    };
    let room = Session::new(saved, true);
    let written = room.saved();
    assert!(written.roots.is_empty());
    assert_eq!(
        room.geometry_fragments,
        written.regions.len() + written.cells.len()
    );
    assert_eq!(
        room.geometry_bytes,
        crate::snapshot_geometry_bytes(&written.regions, &written.cells, &written.roots)
    );
    assert_eq!(
        Session::new(written, true).geometry_bytes,
        room.geometry_bytes
    );
}

#[test]
fn a_full_peer_backlog_defers_publication_without_losing_the_joiner() {
    let mut session = room();
    let (id, mut incoming) = peer(&mut session, true);
    let packet = crate::outbound::Packet::new(ServerMessage::Synced { request: 1 });
    while session.peers[&id].events.available(packet.bytes) {
        assert!(session.peers[&id].events.send(packet.clone()));
    }
    let mut prepared = session
        .edit_plan(
            id,
            ClientMessage::Edit {
                request: 1,
                edits: edit(1),
            },
        )
        .unwrap()
        .prepare()
        .unwrap();
    let (fragments, bytes) = (session.geometry_fragments, session.geometry_bytes);
    assert!(matches!(
        session.commit_prepared(id, &mut prepared),
        Err(CommitError::Busy)
    ));
    // Busy changes nothing, so the same prepared edit can be offered again.
    assert_eq!(session.revision, 0);
    assert!(session.peers.contains_key(&id));
    assert!(session.peers[&id].undo.is_empty());
    assert_eq!(session.peers[&id].edit_tokens, EDIT_BURST);
    assert_eq!(
        (session.geometry_fragments, session.geometry_bytes),
        (fragments, bytes)
    );
    while incoming.try_recv().is_ok() {}
    session.commit_prepared(id, &mut prepared).unwrap();
    assert_eq!(session.revision, 1);
    assert!(session.peers.contains_key(&id));
}

/// A commit that can be rebuilt identically: a builder, whose queue stays open,
/// and the message it publishes next.
struct Scenario {
    session: Session,
    builder: u64,
    _events: mpsc::UnboundedReceiver<crate::outbound::Queued>,
    message: ClientMessage,
}

fn scenario(message: impl FnOnce(&mut Session, u64) -> ClientMessage) -> Scenario {
    let mut session = room();
    let (builder, events) = peer(&mut session, true);
    let message = message(&mut session, builder);
    Scenario {
        session,
        builder,
        _events: events,
        message,
    }
}

/// Every cell of a full 128 by 128 layer, the most a legacy edit may change.
fn cap_erase() -> Scenario {
    let cells: Vec<CellEdit> = (WORLD_MIN..WORLD_MAX)
        .flat_map(|x| {
            (WORLD_MIN..WORLD_MAX).map(move |z| CellEdit {
                pos: [x, 8, z],
                cell: Some(Cell::solid(0)),
            })
        })
        .collect();
    assert_eq!(cells.len(), MAX_EDITS);
    let mut session = Session::new(
        SavedWorld {
            volumes: Vec::new(),
            format_version: 2,
            bounds: Default::default(),
            regions: Vec::new(),
            revision: 0,
            cells: cells.clone(),
            roots: Vec::new(),
            objects: Vec::new(),
            next_object: 0,
        },
        true,
    );
    let (builder, events) = peer(&mut session, true);
    let erase = cells
        .into_iter()
        .map(|cell| CellEdit { cell: None, ..cell })
        .collect();
    Scenario {
        session,
        builder,
        _events: events,
        message: ClientMessage::Edit {
            request: 1,
            edits: erase,
        },
    }
}

/// Commits the scenario three times on identical rooms. With plenty of room the
/// first shows the packet the commit queues. The others leave a watcher, and the
/// builder whose result must still fit, exactly that packet plus the follow-up
/// space, then one byte less. The reservation has to be the packet's own cost:
/// less could drop a peer that was promised room, more would turn away edits
/// that fit. Returns the packet's cost.
fn assert_reservation_is_the_packet(build: impl Fn() -> Scenario) -> usize {
    let published = {
        let Scenario {
            mut session,
            builder,
            _events,
            message,
        } = build();
        let (_watcher, mut seen) = peer(&mut session, false);
        let mut prepared = session
            .edit_plan(builder, message)
            .unwrap()
            .prepare()
            .unwrap();
        session.commit_prepared(builder, &mut prepared).unwrap();
        let patch = seen.try_recv().unwrap().packet.clone();
        assert!(
            patch.encode().unwrap().len() <= patch.bytes,
            "the accounting must cover the real encoding"
        );
        patch.bytes
    };
    for fits in [false, true] {
        let room = published + FOLLOW_UP_BYTES - usize::from(!fits);
        let Scenario {
            mut session,
            builder,
            _events,
            message,
        } = build();
        let (watcher, _seen) = peer(&mut session, false);
        let mut prepared = session
            .edit_plan(builder, message)
            .unwrap()
            .prepare()
            .unwrap();
        assert!(session.fill_backlog(watcher, room) && session.fill_backlog(builder, room));
        let revision = session.revision;
        let result = session.commit_prepared(builder, &mut prepared);
        if fits {
            result.unwrap();
            assert_eq!(session.revision, revision + 1);
            // The result that follows the patch still fits in both queues.
            session.send(
                builder,
                ServerMessage::EditResult {
                    request: 1,
                    error: None,
                },
            );
            assert!(session.peers.contains_key(&builder));
            assert!(session.peers.contains_key(&watcher));
        } else {
            assert!(matches!(result, Err(CommitError::Busy)), "{result:?}");
            assert_eq!(session.revision, revision);
        }
    }
    published
}

#[test]
fn a_legacy_erase_at_the_cap_reserves_exactly_its_world_patch() {
    // Sixteen thousand emptied roots. A legacy patch costs the same per cell
    // edit however empty its root is, so an estimate that follows the roots'
    // fragments falls millions of bytes short here.
    let published = assert_reservation_is_the_packet(cap_erase);
    assert_eq!(published, crate::outbound::cost::world_patch(MAX_EDITS));
}

#[test]
fn every_kind_of_patch_reserves_exactly_its_packet() {
    use sauerteig_core::{ObjectEdit, VolumeMaterial::Water};
    // Detailed roots publish a root patch.
    assert_reservation_is_the_packet(|| {
        scenario(|_, _| ClientMessage::EditRoots {
            request: 1,
            base_revision: 0,
            roots: vec![
                fine_root([20, 4, 20], [1, 2, 3], 2),
                fine_root([21, 4, 20], [0, 2, 3], 1),
            ],
        })
    });
    // An object publishes the whole object list, however long it has grown.
    assert_reservation_is_the_packet(|| {
        scenario(|session, _| {
            std::sync::Arc::make_mut(&mut session.objects).extend((1..=500).map(|id| {
                WorldObject {
                    id,
                    kind: ObjectKind::Light,
                    position: Vec3::new(4.0, 2.0, 4.0),
                    rotation: 0.0,
                }
            }));
            session.next_object = 501;
            ClientMessage::PlaceObject {
                kind: ObjectKind::Light,
                position: Vec3::new(5.0, 2.0, 5.0),
                rotation: 0.0,
            }
        })
    });
    // A transaction carries roots, the object list, and the replacement volume layer.
    assert_reservation_is_the_packet(|| {
        scenario(|_, _| ClientMessage::EditTransaction {
            request: 1,
            base_revision: 0,
            roots: vec![fine_root([20, 4, 20], [1, 2, 3], 2)],
            objects: vec![ObjectEdit::Create {
                object: object_at(Vec3::new(4.0, 2.0, 4.0)),
            }],
            volumes: vec![volume_edit([160, 32, 160], [176, 48, 176], Some(Water))],
        })
    });
    // Undo publishes with the patch kind its entry was made with.
    assert_reservation_is_the_packet(|| {
        let mut scenario = scenario(|_, _| ClientMessage::Undo);
        scenario.session.edit(scenario.builder, edit(1)).unwrap();
        scenario
    });
}

#[test]
fn a_leaving_peers_history_is_retired_not_freed_by_the_thread_that_removes_it() {
    let mut session = room();
    let (a, _a) = peer(&mut session, true);
    let (b, events_b) = peer(&mut session, true);
    let (c, _c) = peer(&mut session, true);
    for (index, builder) in [a, b, c].into_iter().enumerate() {
        for material in [1, 2] {
            let cell = CellEdit {
                pos: [20 + index as i32, 4, 20],
                cell: Some(Cell::solid(material)),
            };
            session.edit(builder, vec![cell]).unwrap();
        }
        session.undo(builder).unwrap();
    }
    let undo = std::sync::Arc::downgrade(&session.peers[&a].undo.back().unwrap().before);
    let redo = std::sync::Arc::downgrade(&session.peers[&a].redo.last().unwrap().before);
    session.remove_peer(a);
    assert!(!session.peers.contains_key(&a));
    assert_eq!(session.retired.len(), 2, "one undo and one redo entry");
    assert!(
        undo.upgrade().is_some() && redo.upgrade().is_some(),
        "removal must not free the history on this thread"
    );
    // A connection that went away is reaped the same way, and so is a peer
    // whose queue overflows.
    drop(events_b);
    session.reap();
    assert!(!session.peers.contains_key(&b));
    assert_eq!(session.retired.len(), 4);
    assert!(session.fill_backlog(c, 0));
    session.send(
        c,
        ServerMessage::Notice {
            message: "x".into(),
        },
    );
    assert!(!session.peers.contains_key(&c));
    assert_eq!(session.retired.len(), 6);
    drop(std::mem::take(&mut session.retired));
    assert!(undo.upgrade().is_none() && redo.upgrade().is_none());
}

#[test]
fn a_save_waits_for_quiet_but_never_longer_than_the_autosave_limit() {
    use crate::{AUTOSAVE_LIMIT, SAVE_DELAY};
    let mut session = room();
    assert!(!session.save_due(), "nothing changed");
    session.changed();
    assert!(!session.save_due(), "a fresh edit waits for quiet");
    session.last_edit = Instant::now() - SAVE_DELAY;
    assert!(session.save_due(), "quiet long enough");

    session.last_edit = Instant::now();
    session.dirty_since = Instant::now() - AUTOSAVE_LIMIT + Duration::from_secs(1);
    session.changed();
    assert!(!session.save_due(), "still young, and edits continue");
    assert!(
        session.dirty_since.elapsed() >= AUTOSAVE_LIMIT - Duration::from_secs(1),
        "later edits do not restart the clock"
    );
    session.dirty_since = Instant::now() - AUTOSAVE_LIMIT;
    session.changed();
    assert!(session.save_due(), "old enough, though editing continues");
    assert!(AUTOSAVE_LIMIT < Duration::from_secs(10));
}

#[test]
fn changes_made_during_a_save_stay_unsaved_and_keep_their_age() {
    let mut session = room();
    session.changed();
    let snapshot = session.begin_save();
    let taken = session.snapshot_at;
    session.changed();
    session.finish_save(snapshot.revision);
    assert!(session.dirty);
    assert_eq!(session.dirty_since, taken);
    let snapshot = session.begin_save();
    session.finish_save(snapshot.revision);
    assert!(!session.dirty);
}

#[test]
fn replacing_a_world_file_leaves_only_the_new_world() {
    let directory = std::env::temp_dir().join(format!("sauerteig-rename-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&directory);
    let path = directory.join("world.json");
    let mut saved = room().saved();
    write_world(&path, &saved).unwrap();
    saved.revision = 7;
    write_world(&path, &saved).unwrap();
    assert_eq!(load_world(&path).unwrap().revision, 7);
    assert!(!path.with_extension("json.tmp").exists());
    std::fs::remove_dir_all(&directory).unwrap();
}

/// Leaves of the water box in `near_guard`: 64 cubes 16 m on a side.
const WATER_LEAVES: usize = 64;

/// A world 128 m square whose water and glass hold `WATER_LEAVES + pane`
/// leaves: 16 m of water across the floor, and above it a glass pane an eighth
/// thick whose eighths are one leaf each. Its geometry is empty.
fn near_guard(pane: usize) -> SavedWorld {
    use sauerteig_core::{MaterialVolume, VolumeMaterial, WorldBounds};
    let glass = |min: [i32; 3], max: [i32; 3]| MaterialVolume {
        bounds: FineBox { min, max },
        material: VolumeMaterial::Glass,
    };
    let rows = (pane / 1024) as i32;
    let rest = (pane % 1024) as i32;
    let mut volumes = vec![MaterialVolume {
        bounds: FineBox {
            min: [0, 0, 0],
            max: [1024, 128, 1024],
        },
        material: VolumeMaterial::Water,
    }];
    if rows > 0 {
        volumes.push(glass([0, 200, 0], [1024, 201, rows]));
    }
    if rest > 0 {
        volumes.push(glass([0, 200, rows], [rest, 201, rows + 1]));
    }
    SavedWorld {
        revision: 0,
        format_version: 3,
        bounds: WorldBounds {
            min: [0, 0, 0],
            max: [128, 32, 128],
        },
        regions: Vec::new(),
        cells: Vec::new(),
        roots: Vec::new(),
        volumes,
        objects: Vec::new(),
        next_object: 0,
    }
}

/// A full cell in one metre, inside `near_guard`'s water.
fn cell_at(pos: CellPos) -> RootEdit {
    RootEdit {
        pos,
        fragments: vec![CellFragment::from_cell(pos, Cell::solid(1))],
    }
}

/// How many leaves an edit adds to a world.
fn leaves_added(world: &World, roots: &[RootEdit]) -> usize {
    let mut probe = world.clone();
    probe.apply_root_edits(roots).unwrap();
    probe.stats().leaves() - world.stats().leaves()
}

/// Joins an editor and builds its world from the Welcome, as a client does.
fn join_and_build(
    session: &mut Session,
) -> (u64, mpsc::UnboundedReceiver<crate::outbound::Queued>, World) {
    let (events, receiver) = crate::outbound::channel();
    let (snapshots, _) = watch::channel(None);
    let (id, welcome) = session.join(events, snapshots).unwrap();
    session.peers.get_mut(&id).unwrap().player.editing = true;
    let ServerMessage::Welcome {
        bounds,
        regions,
        cells,
        roots,
        volumes,
        ..
    } = welcome.expand()
    else {
        panic!("a join answers with a Welcome");
    };
    let mut world = World::from_compact_parts(bounds, &regions, &cells, &roots).unwrap();
    world.replace_volumes(&volumes).unwrap();
    (id, receiver, world)
}

#[test]
fn geometry_inside_water_counts_against_the_world_guard() {
    let guard = sauerteig_core::MAX_WORLD_PIECES;
    let water = Session::new(near_guard(0), true);
    assert_eq!(water.world_stats.leaves(), WATER_LEAVES);
    let (first, second) = (cell_at([4, 4, 4]), cell_at([20, 4, 4]));
    // One cell splits the 16 m of water around it into many leaves, which
    // format 3's piece count does not see.
    let added = leaves_added(&water.world, std::slice::from_ref(&first));
    assert_eq!(
        added,
        leaves_added(&water.world, std::slice::from_ref(&second))
    );
    assert!(added > 20, "{added}");
    let slack = added / 2;
    let mut session = Session::new(near_guard(guard - slack - added - WATER_LEAVES), true);
    assert_eq!(session.world_stats.leaves(), guard - slack - added);
    let (builder, _events) = peer(&mut session, true);

    session.edit_roots(builder, 0, vec![first.clone()]).unwrap();
    assert_eq!(session.world_stats.leaves(), guard - slack);
    assert_eq!(session.geometry_fragments, 1);

    // Saved just under the guard, the world opens again unchanged.
    let dir = temporary_directory("under-guard");
    let path = dir.join("world.json");
    write_world(&path, &session.saved()).unwrap();
    let (saved, world) = crate::persistence::open_world(&path).unwrap();
    let reopened = Session::from_world(saved, world, true).unwrap();
    assert_eq!(reopened.world_stats.leaves(), guard - slack);
    assert_eq!(reopened.world.digest(), session.world.digest());
    std::fs::remove_dir_all(dir).unwrap();

    // The next cell would pass the guard.
    let before = session.world.digest();
    let revision = session.revision;
    assert_eq!(
        session
            .edit_roots(builder, revision, vec![second])
            .unwrap_err(),
        crate::WORLD_GUARD_ERROR
    );
    assert_eq!(session.world.digest(), before);
    assert_eq!(session.revision, revision);

    // Removing detail is accepted.
    session
        .edit_roots(
            builder,
            revision,
            vec![RootEdit {
                pos: first.pos,
                fragments: Vec::new(),
            }],
        )
        .unwrap();
    assert_eq!(session.world_stats.leaves(), guard - slack - added);
}

#[test]
fn a_world_saved_over_the_guard_opens_on_the_server_and_through_a_welcome() {
    let guard = sauerteig_core::MAX_WORLD_PIECES;
    let over = guard + 1000;
    let dir = temporary_directory("over-guard");
    let path = dir.join("world.json");
    write_world(&path, &near_guard(over - WATER_LEAVES)).unwrap();
    let (saved, world) = crate::persistence::open_world(&path).unwrap();
    std::fs::remove_dir_all(dir).unwrap();
    let mut session = Session::from_world(saved, world, true).unwrap();
    assert_eq!(session.world_stats.leaves(), over);
    let (builder, _events, client) = join_and_build(&mut session);
    assert_eq!(client.digest(), session.world.digest());
    assert_eq!(client.volumes(), session.world.volumes());

    // An edit that adds leaves is refused, and one that removes them is
    // accepted, though the world stays over the guard.
    assert_eq!(
        session
            .edit_roots(builder, 0, vec![cell_at([4, 4, 4])])
            .unwrap_err(),
        crate::WORLD_GUARD_ERROR
    );
    volume_transaction(
        &mut session,
        builder,
        0,
        vec![],
        vec![],
        vec![volume_edit([0, 200, 0], [64, 201, 1], None)],
    )
    .unwrap();
    assert_eq!(session.world_stats.leaves(), over - 64);
}

#[test]
fn a_transaction_near_the_guard_gives_the_client_the_servers_world() {
    let guard = sauerteig_core::MAX_WORLD_PIECES;
    let added = leaves_added(
        &Session::new(near_guard(0), true).world,
        &[cell_at([4, 4, 4])],
    );
    assert!(added > 20, "{added}");
    let slack = added / 2;
    let rows = (guard - slack - WATER_LEAVES) / 1024;
    let mut session = Session::new(near_guard(guard - slack - WATER_LEAVES), true);
    assert_eq!(session.world_stats.leaves(), guard - slack);
    let (builder, mut events, mut client) = join_and_build(&mut session);
    type Patch = (Vec<RootEdit>, Option<Vec<sauerteig_core::MaterialVolume>>);
    fn take_patch(events: &mut mpsc::UnboundedReceiver<crate::outbound::Queued>) -> Option<Patch> {
        let mut patch = None;
        while let Ok(queued) = events.try_recv() {
            if let ServerMessage::TransactionPatch { roots, volumes, .. } = &queued.packet.message {
                assert!(patch.is_none(), "one patch an edit");
                patch = Some((roots.clone(), volumes.clone()));
            }
        }
        patch
    }
    fn follow(
        session: &Session,
        events: &mut mpsc::UnboundedReceiver<crate::outbound::Queued>,
        client: &mut World,
    ) {
        let (roots, volumes) = take_patch(events).expect("a patch");
        let chunks = client.apply_patch(&roots, volumes.as_deref()).unwrap();
        assert!(!chunks.is_empty());
        assert_eq!(client.digest(), session.world.digest());
        assert_eq!(client.volumes(), session.world.volumes());
    }
    let row = rows as i32 - 1;
    let cleared = added as i32 + 8;

    // The cell alone would pass the guard; with part of the pane cleared
    // the result stays under it.
    volume_transaction(
        &mut session,
        builder,
        0,
        vec![cell_at([4, 4, 4])],
        vec![],
        vec![volume_edit([0, 200, row], [cleared, 201, row + 1], None)],
    )
    .unwrap();
    assert_eq!(session.world_stats.leaves(), guard - slack - 8);
    follow(&session, &mut events, &mut client);

    // A transaction whose result would pass the guard is refused and sends
    // nothing.
    let revision = session.revision;
    let before = session.world.digest();
    assert_eq!(
        volume_transaction(
            &mut session,
            builder,
            revision,
            vec![cell_at([20, 4, 4])],
            vec![],
            vec![volume_edit([0, 200, row - 1], [1, 201, row], None)],
        )
        .unwrap_err(),
        crate::WORLD_GUARD_ERROR
    );
    assert_eq!(session.world.digest(), before);
    assert!(take_patch(&mut events).is_none());

    // Undoing the first puts the pane back before the cell goes: the boxes
    // alone pass the guard, the result does not, and a client applying the
    // patch in the server's order agrees.
    session.undo(builder).unwrap();
    assert_eq!(session.world_stats.leaves(), guard - slack);
    follow(&session, &mut events, &mut client);
}
