use crate::network::{MESSAGE_BYTES, OUTBOUND_WRITE_BYTES};
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
use std::{path::PathBuf, time::Duration};
use tokio::{
    net::TcpListener,
    sync::{mpsc, watch},
};

fn room() -> Session {
    let mut world = World::empty();
    for x in -8..8 {
        for z in -8..8 {
            world.set([x, 0, z], Some(Cell::solid(0)));
        }
    }
    Session::new(
        SavedWorld {
            revision: 0,
            cells: world.cells(),
            roots: Vec::new(),
            objects: Vec::new(),
        },
        true,
    )
}

fn peer(session: &mut Session, editing: bool) -> (u64, mpsc::Receiver<String>) {
    let (events, receiver) = mpsc::channel(256);
    let (snapshots, _) = watch::channel(String::new());
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
            revision: 0,
            cells: vec![],
            roots: vec![RootEdit {
                pos,
                fragments: fragments.clone(),
            }],
            objects: vec![],
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
fn outbound_limit_covers_a_full_bounded_world() {
    use sauerteig_core::{Shape, WORLD_HEIGHT, WORLD_MAX, WORLD_MIN};
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
    let cell_count = (WORLD_MAX - WORLD_MIN).pow(2) as usize * WORLD_HEIGHT as usize;
    let welcome_budget = cell_count * 256 + MAX_OBJECTS * 256 + MAX_PLAYERS * 1024 + 1024;
    assert!(welcome_budget + 16 * 1024 + 14 < OUTBOUND_WRITE_BYTES);
    let detailed_budget = MAX_WORLD_FRAGMENTS * 320 + MAX_OBJECTS * 256 + MAX_PLAYERS * 1024 + 1024;
    assert!(detailed_budget + 16 * 1024 + 14 < OUTBOUND_WRITE_BYTES);
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
    let message: ServerMessage = serde_json::from_str(&events.try_recv().unwrap()).unwrap();
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
    let message: ServerMessage = serde_json::from_str(&events.try_recv().unwrap()).unwrap();
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
    let message: ServerMessage = serde_json::from_str(&events.try_recv().unwrap()).unwrap();
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
        Some(slope)
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
async fn websocket_large_arena_joins_and_edit_results_correlate_with_requests() {
    use futures_util::StreamExt;
    let dir = temporary_directory("large-arena-edit-results");
    let config = Config {
        world_path: dir.join("world.json"),
        web_dir: dir.join("web"),
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
        web_dir: dir.join("web"),
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
        assert_eq!(World::from_cells(&cells).get([30, 8, 30]), Some(slope));
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
        web_dir: dir.join("web"),
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
