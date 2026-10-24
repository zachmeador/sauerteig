use sauerteig_core::*;
use std::collections::BTreeSet;

fn close(actual: f32, expected: f32) {
    assert!(
        (actual - expected).abs() < 0.001,
        "actual {actual}, expected {expected}"
    );
}

#[test]
fn negative_coordinates_are_distinct_and_empty_chunks_are_reclaimed() {
    let mut world = World::empty();
    world.set([-1, 3, -9], Some(Cell::solid(0)));
    world.set([7, 3, -9], Some(Cell::solid(2)));
    assert_eq!(World::cell_chunk([-1, 3, -9]), [-1, 0, -2]);
    assert_eq!(world.get([-1, 3, -9]), Some(Cell::solid(0)));
    assert_eq!(world.get([7, 3, -9]), Some(Cell::solid(2)));
    assert_eq!(world.chunk_keys().len(), 2);
    world.set([-1, 3, -9], None);
    assert_eq!(world.chunk_keys(), vec![[0, 0, -2]]);
    world.set([7, 3, -9], None);
    assert!(world.chunk_keys().is_empty());
}

#[test]
fn snapshots_roundtrip_only_occupied_cells_and_reject_bad_geometry() {
    let mut world = World::empty();
    world.set(
        [-8, 1, 9],
        Some(Cell {
            material: 3,
            heights: [0, 4, 8, 4],
            diagonal: false,
            shape: None,
        }),
    );
    world.set([1, 2, 3], Some(Cell::solid(5)));
    let json = serde_json::to_value(&world).unwrap();
    assert_eq!(json.as_array().unwrap().len(), 2);
    let restored: World = serde_json::from_value(json).unwrap();
    assert_eq!(restored.cells(), world.cells());
    let malformed = r#"[{"pos":[0,0,0],"cell":{"material":0,"heights":[9,8,8,8]}}]"#;
    assert!(serde_json::from_str::<World>(malformed).is_err());
    assert!(!World::is_valid_pos([0, -1, 0]));
    assert!(!World::is_valid_pos([64, 0, 0]));
}

#[test]
fn group_edits_are_bounded_and_invalidate_neighbor_chunks() {
    let edits = block_edits([-4, 0, -4], 8, Some(Cell::solid(0)));
    assert_eq!(edits.len(), 512);
    let mut world = World::empty();
    let dirty = world.apply_edits(&edits);
    assert_eq!(world.cells().len(), 512);
    assert_eq!(world.chunk_keys().len(), 4);
    assert_eq!(dirty.len(), dirty.iter().collect::<BTreeSet<_>>().len());
    for key in world.chunk_keys() {
        assert!(dirty.contains(&key));
        for axis in 0..3 {
            for offset in [-1, 1] {
                let mut adjacent = key;
                adjacent[axis] += offset;
                assert!(dirty.contains(&adjacent));
            }
        }
    }
    assert!(world.apply_edits(&edits).is_empty());
    assert_eq!(block_edits([63, 47, 63], 8, Some(Cell::solid(0))).len(), 1);
    assert!(block_edits([0, 0, 0], 3, None).is_empty());
}

#[test]
fn invalid_set_does_not_erase_existing_cell() {
    let mut world = World::empty();
    world.set([0, 0, 0], Some(Cell::solid(0)));
    world.set(
        [0, 0, 0],
        Some(Cell {
            material: 0,
            heights: [0; 4],
            diagonal: false,
            shape: None,
        }),
    );
    world.set([0, 0, 0], Some(Cell::solid(6)));
    assert_eq!(world.get([0, 0, 0]), Some(Cell::solid(0)));
}

#[test]
fn adjoining_solid_cells_have_no_internal_faces_even_across_chunks() {
    let mut world = World::empty();
    world.set([7, 1, 0], Some(Cell::solid(0)));
    assert_eq!(world.mesh_chunk([0, 0, 0]).indices.len(), 36);
    world.set([8, 1, 0], Some(Cell::solid(0)));
    let meshes = [world.mesh_chunk([0, 0, 0]), world.mesh_chunk([1, 0, 0])];
    assert_eq!(meshes.iter().map(|m| m.indices.len()).sum::<usize>(), 60);
    for mesh in meshes {
        for vertex in mesh.vertices {
            close(Vec3::from_array(vertex.normal).length(), 1.0);
            assert!(!(vertex.position[0] == 8.0 && vertex.normal[0].abs() > 0.99));
        }
    }
}

#[test]
fn rays_mesh_and_collision_share_the_deformed_top() {
    let mut world = World::empty();
    let cell = Cell {
        material: 2,
        heights: [0, 8, 8, 0],
        diagonal: false,
        shape: None,
    };
    world.set([0, 0, 0], Some(cell));
    for x in [0.125, 0.375, 0.625, 0.875] {
        let hit = world
            .raycast(Vec3::new(x, 3.0, 0.5), Vec3::NEG_Y, 10.0)
            .unwrap();
        close(hit.point.y, x);
        close(hit.distance, 3.0 - x);
        close(hit.normal.x, -std::f32::consts::FRAC_1_SQRT_2);
        close(hit.normal.y, std::f32::consts::FRAC_1_SQRT_2);
    }
    let mesh = world.mesh_chunk([0, 0, 0]);
    for vertex in mesh.vertices.iter().filter(|v| v.normal[1] > 0.1) {
        close(vertex.position[1], vertex.position[0]);
    }
    let mut player = Player::new(1, "test".into());
    player.position = Vec3::new(0.4, 0.701, 0.5);
    assert!(!player_intersects_cell(&player, [0, 0, 0], cell));
    player.position.y = 0.69;
    assert!(player_intersects_cell(&player, [0, 0, 0], cell));
}

#[test]
fn warped_top_keeps_the_fixed_diagonal_instead_of_bilinear_height() {
    let mut world = World::empty();
    let cell = Cell {
        material: 0,
        heights: [8, 0, 8, 0],
        diagonal: false,
        shape: None,
    };
    world.set([0, 0, 0], Some(cell));
    close(cell.height_at(0.5, 0.5), 1.0);
    let hit = world
        .raycast(Vec3::new(0.5, 2.0, 0.5), Vec3::NEG_Y, 3.0)
        .unwrap();
    close(hit.point.y, 1.0);
    let mut player = Player::new(1, "test".into());
    player.position = Vec3::new(0.5, 0.99, 0.5);
    assert!(player_intersects_cell(&player, [0, 0, 0], cell));
    player.position.y = 1.001;
    assert!(!player_intersects_cell(&player, [0, 0, 0], cell));
}

#[test]
fn alternate_diagonal_matches_height_rays_and_collision() {
    let cell = Cell {
        heights: [0, 8, 0, 8],
        diagonal: true,
        shape: None,
        ..Cell::solid(0)
    };
    let mut world = World::empty();
    world.set([0, 0, 0], Some(cell));
    for (x, z) in [(0.2, 0.3), (0.8, 0.7), (0.5, 0.5)] {
        let hit = world
            .raycast(Vec3::new(x, 2.0, z), Vec3::NEG_Y, 3.0)
            .unwrap();
        close(hit.point.y, cell.height_at(x, z));
    }
    close(cell.height_at(0.5, 0.5), 1.0);
    let mut player = Player::new(1, "test".into());
    // The footprint corners are below 1; the diagonal crosses its interior.
    player.position = Vec3::new(0.5, 0.99, 0.4);
    assert!(player_intersects_cell(&player, [0, 0, 0], cell));
    player.position.y = 1.001;
    assert!(!player_intersects_cell(&player, [0, 0, 0], cell));
}

#[test]
fn touching_opposite_diagonals_hide_only_the_occupied_half() {
    let mut world = World::empty();
    world.set([0, 0, 0], Some(Cell::solid(0)));
    world.set(
        [0, 1, 0],
        Some(Cell {
            heights: [8, 0, 0, 0],
            diagonal: true,
            shape: None,
            ..Cell::solid(0)
        }),
    );
    let inside = world
        .raycast(Vec3::new(0.2, 0.5, 0.2), Vec3::Y, 3.0)
        .unwrap();
    close(inside.point.y, 1.6);
    let uncovered = world
        .raycast(Vec3::new(0.8, 0.5, 0.8), Vec3::Y, 3.0)
        .unwrap();
    close(uncovered.point.y, 1.0);
}

#[test]
fn diagonal_serialization_is_backward_compatible() {
    let old: Cell = serde_json::from_str(r#"{"material":2,"heights":[8,3,1,5]}"#).unwrap();
    assert!(!old.diagonal);
    assert!(!serde_json::to_string(&old).unwrap().contains("diagonal"));
    let changed = Cell {
        diagonal: true,
        shape: None,
        ..old
    };
    let restored: Cell = serde_json::from_str(&serde_json::to_string(&changed).unwrap()).unwrap();
    assert_eq!(restored, changed);
}

#[test]
fn collapsed_half_of_a_cell_has_no_phantom_top_or_bottom() {
    let mut world = World::empty();
    world.set(
        [0, 0, 0],
        Some(Cell {
            material: 0,
            heights: [0, 8, 0, 0],
            diagonal: false,
            shape: None,
        }),
    );
    assert!(world
        .raycast(Vec3::new(0.1, 2.0, 0.9), Vec3::NEG_Y, 3.0)
        .is_none());
    let hit = world
        .raycast(Vec3::new(0.9, 2.0, 0.1), Vec3::NEG_Y, 3.0)
        .unwrap();
    close(hit.point.y, 0.8);
}

#[test]
fn dda_clips_outside_origins_and_respects_distance() {
    let mut world = World::empty();
    world.set([0, 0, 0], Some(Cell::solid(0)));
    let hit = world
        .raycast(Vec3::new(-70.0, 0.5, 0.5), Vec3::X * 2.0, 71.0)
        .unwrap();
    close(hit.distance, 70.0);
    assert_eq!(hit.normal, Vec3::NEG_X);
    assert!(world
        .raycast(Vec3::new(-70.0, 0.5, 0.5), Vec3::X, 69.0)
        .is_none());
    assert!(world.raycast(Vec3::ZERO, Vec3::ZERO, 10.0).is_none());
    assert!(world
        .raycast(Vec3::splat(f32::NAN), Vec3::X, 10.0)
        .is_none());
}

#[test]
fn a_ray_inside_joined_solids_hits_the_visible_exit() {
    let mut world = World::empty();
    world.set([0, 0, 0], Some(Cell::solid(0)));
    world.set([1, 0, 0], Some(Cell::solid(0)));
    let hit = world.raycast(Vec3::splat(0.5), Vec3::X, 5.0).unwrap();
    close(hit.point.x, 2.0);
    close(hit.distance, 1.5);
    assert_eq!(hit.normal, Vec3::X);
}

#[test]
fn partial_neighbors_expose_only_the_step_between_them() {
    let mut world = World::empty();
    world.set(
        [0, 0, 0],
        Some(Cell {
            material: 0,
            heights: [4; 4],
            diagonal: false,
            shape: None,
        }),
    );
    world.set([1, 0, 0], Some(Cell::solid(2)));
    let upper = world
        .raycast(Vec3::new(0.5, 0.75, 0.5), Vec3::X, 5.0)
        .unwrap();
    close(upper.point.x, 1.0);
    let lower = world
        .raycast(Vec3::new(0.5, 0.25, 0.5), Vec3::X, 5.0)
        .unwrap();
    close(lower.point.x, 2.0);
}

#[test]
fn arena_spawns_are_clear_and_supported() {
    let world = World::arena();
    assert!(world.cells().len() > 3000);
    for index in 0..8 {
        let mut player = Player::new(index as u64, "test".into());
        player.position = spawn_point(index);
        assert!(world.cells().iter().all(|e| !player_intersects_cell(
            &player,
            e.pos,
            e.cell.unwrap()
        )));
        let ground = world.raycast(player.position, Vec3::NEG_Y, 0.1).unwrap();
        close(ground.point.y, 1.0);
    }
    let objects = arena_objects();
    assert_eq!(
        objects.iter().map(|o| o.id).collect::<BTreeSet<_>>().len(),
        objects.len()
    );
    assert_eq!(
        objects
            .iter()
            .filter(|o| o.kind == ObjectKind::Spawn)
            .count(),
        8
    );
}

#[test]
fn protocol_shape_is_stable_and_snapshot_carries_world_revision() {
    let message = ClientMessage::Edit {
        request: 42,
        edits: block_edits([0, 0, 0], 1, Some(Cell::solid(0))),
    };
    let value = serde_json::to_value(&message).unwrap();
    assert_eq!(value["type"], "edit");
    assert_eq!(value["request"], 42);
    assert_eq!(value["edits"][0]["pos"], serde_json::json!([0, 0, 0]));
    let snapshot = ServerMessage::Snapshot {
        tick: 2,
        revision: 7,
        players: vec![],
        projectiles: vec![],
    };
    let value = serde_json::to_value(snapshot).unwrap();
    assert_eq!(value["revision"], 7);
    assert_eq!(
        serde_json::to_value(Vec3::new(1.0, 2.0, 3.0)).unwrap(),
        serde_json::json!([1.0, 2.0, 3.0])
    );
}
