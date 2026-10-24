use sauerteig_core::*;

fn floor() -> World {
    let mut world = World::empty();
    for x in -20..20 {
        for z in -20..20 {
            world.set([x, 0, z], Some(Cell::solid(0)));
        }
    }
    world
}

fn player() -> Player {
    let mut player = Player::new(0, "test".into());
    player.position = Vec3::new(0.5, 1.01, 0.5);
    player
}

fn tick(world: &World, player: &mut Player, input: &InputFrame, frames: usize) {
    for _ in 0..frames {
        simulate_player(world, player, input, FIXED_DT);
    }
}

#[test]
fn standing_accelerating_and_ground_friction_are_stable() {
    let world = floor();
    let mut player = player();
    tick(&world, &mut player, &InputFrame::default(), 120);
    assert!(player.grounded);
    assert!((player.position.y - 1.0).abs() < 0.001);
    tick(
        &world,
        &mut player,
        &InputFrame {
            forward: 1.0,
            ..Default::default()
        },
        45,
    );
    assert!(player.position.z < -4.0);
    assert!(player.velocity.z < -8.0);
    tick(&world, &mut player, &InputFrame::default(), 90);
    assert!(player.velocity.length() < 0.001);
    assert!(player.grounded);
}

#[test]
fn jump_has_an_apex_and_returns_to_the_floor() {
    let world = floor();
    let mut player = player();
    tick(
        &world,
        &mut player,
        &InputFrame {
            jump: true,
            ..Default::default()
        },
        1,
    );
    assert!(!player.grounded);
    let mut peak = player.position.y;
    for _ in 0..90 {
        tick(&world, &mut player, &InputFrame::default(), 1);
        peak = peak.max(player.position.y);
    }
    assert!(peak > 2.1 && peak < 2.6, "peak {peak}");
    assert!(player.grounded);
    assert!((player.position.y - 1.0).abs() < 0.001);
}

#[test]
fn wall_collision_does_not_tunnel_or_lose_tangential_movement() {
    let mut world = floor();
    for y in 1..5 {
        for z in -20..20 {
            world.set([3, y, z], Some(Cell::solid(1)));
        }
    }
    let mut player = player();
    tick(
        &world,
        &mut player,
        &InputFrame {
            strafe: 1.0,
            ..Default::default()
        },
        90,
    );
    assert!(
        player.position.x > 2.69 && player.position.x < 2.701,
        "x {}",
        player.position.x
    );
    let before = player.position.z;
    tick(
        &world,
        &mut player,
        &InputFrame {
            strafe: 1.0,
            forward: 1.0,
            ..Default::default()
        },
        30,
    );
    assert!(player.position.z < before - 2.0);
    assert!(player.position.x < 2.701);
}

#[test]
fn half_unit_stairs_are_walkable_without_jumping() {
    let mut world = floor();
    for step in 0..8 {
        let x = 2 + step;
        let level = 1 + step / 2;
        for z in -1..2 {
            for y in 1..level {
                world.set([x, y, z], Some(Cell::solid(0)));
            }
            world.set(
                [x, level, z],
                Some(Cell {
                    material: 0,
                    heights: [if step % 2 == 0 { 4 } else { 8 }; 4],
                    diagonal: false,
                    shape: None,
                    face_materials: None,
                }),
            );
        }
    }
    let mut player = player();
    let input = InputFrame {
        yaw: std::f32::consts::FRAC_PI_2,
        forward: 1.0,
        ..Default::default()
    };
    let mut peak = player.position.y;
    for _ in 0..85 {
        tick(&world, &mut player, &input, 1);
        peak = peak.max(player.position.y);
    }
    assert!(player.position.x > 9.5, "stuck at {:?}", player.position);
    assert!(peak > 4.99, "peak {peak}");
}

#[test]
fn ramps_do_not_behave_like_full_height_blocks() {
    let mut world = floor();
    for step in 0..8 {
        let x = 2 + step;
        let level = 1 + step / 2;
        let low = if step % 2 == 0 { 0 } else { 4 };
        for z in -1..2 {
            for y in 1..level {
                world.set([x, y, z], Some(Cell::solid(0)));
            }
            world.set(
                [x, level, z],
                Some(Cell {
                    material: 3,
                    heights: [low, low + 4, low + 4, low],
                    diagonal: false,
                    shape: None,
                    face_materials: None,
                }),
            );
        }
    }
    let mut player = player();
    let input = InputFrame {
        yaw: std::f32::consts::FRAC_PI_2,
        forward: 1.0,
        ..Default::default()
    };
    let mut peak = player.position.y;
    for _ in 0..85 {
        tick(&world, &mut player, &input, 1);
        peak = peak.max(player.position.y);
        for edit in world.cells().into_iter().filter(|e| e.pos[1] > 0) {
            assert!(
                !player_intersects_cell(&player, edit.pos, edit.cell.unwrap()),
                "penetration at {:?}",
                player.position
            );
        }
    }
    assert!(player.position.x > 9.5, "stuck at {:?}", player.position);
    assert!(peak > 4.95);
}

#[test]
fn jumping_hits_a_ceiling_then_lands() {
    let mut world = floor();
    for x in -1..2 {
        for z in -1..2 {
            world.set(
                [x, 3, z],
                Some(Cell {
                    material: 0,
                    heights: [1, 4, 8, 4],
                    diagonal: false,
                    shape: None,
                    face_materials: None,
                }),
            );
        }
    }
    let mut player = player();
    tick(
        &world,
        &mut player,
        &InputFrame {
            jump: true,
            ..Default::default()
        },
        1,
    );
    let mut peak = player.position.y;
    for _ in 0..60 {
        tick(&world, &mut player, &InputFrame::default(), 1);
        peak = peak.max(player.position.y);
    }
    assert!(peak <= 3.0 - PLAYER_HEIGHT + 0.001);
    assert!(peak > 1.2);
    assert!(player.grounded);
}

#[test]
fn editor_flight_ignores_world_collision() {
    let mut world = floor();
    for y in 1..5 {
        world.set([3, y, 0], Some(Cell::solid(0)));
    }
    let mut player = player();
    player.editing = true;
    tick(
        &world,
        &mut player,
        &InputFrame {
            yaw: std::f32::consts::FRAC_PI_2,
            forward: 1.0,
            ..Default::default()
        },
        30,
    );
    assert!(player.position.x > 8.0);
    assert!(!player.grounded);
    assert!(ray_player(Vec3::new(0.0, 1.5, 0.5), Vec3::X, &player, 20.0).is_none());
}

#[test]
fn removing_the_floor_changes_collision_on_the_next_tick() {
    let mut world = floor();
    let mut player = player();
    tick(&world, &mut player, &InputFrame::default(), 2);
    world.apply_edits(&[CellEdit {
        pos: [0, 0, 0],
        cell: None,
    }]);
    tick(&world, &mut player, &InputFrame::default(), 10);
    assert!(!player.grounded);
    assert!(player.position.y < 0.8);
}

#[test]
fn identical_input_replay_matches_and_invalid_input_stays_finite() {
    let world = floor();
    let mut a = player();
    let mut b = a.clone();
    for seq in 0..80 {
        let input = InputFrame {
            seq,
            yaw: seq as f32 * 0.02,
            forward: 1.0,
            jump: seq == 12,
            ..Default::default()
        };
        simulate_player(&world, &mut a, &input, FIXED_DT);
        simulate_player(&world, &mut b, &input, FIXED_DT);
    }
    assert_eq!(a.position, b.position);
    assert_eq!(a.velocity, b.velocity);
    assert_eq!(a.last_input, 79);
    let bad = InputFrame {
        seq: 80,
        forward: f32::NAN,
        yaw: f32::INFINITY,
        ..Default::default()
    };
    simulate_player(&world, &mut a, &bad, FIXED_DT);
    assert!(a.position.is_finite() && a.velocity.is_finite());
    assert_eq!(a.last_input, 80);
}

#[test]
fn player_hit_distance_uses_feet_height_and_normalized_direction() {
    let player = player();
    let hit = ray_player(Vec3::new(-5.0, 2.0, 0.5), Vec3::X * 4.0, &player, 10.0).unwrap();
    assert!((hit - 5.2).abs() < 0.001);
    assert!(ray_player(Vec3::new(-5.0, 3.0, 0.5), Vec3::X, &player, 10.0).is_none());
    assert!(ray_player(Vec3::new(-5.0, 2.0, 0.5), Vec3::X, &player, 5.0).is_none());
}

#[test]
fn shaped_side_wall_stops_at_the_sloped_boundary_not_the_cell_box() {
    let mut world = floor();
    let mut shape = Shape::solid();
    for corner in [0, 4] {
        shape.vertices[corner][0] = 2;
    }
    for corner in [3, 7] {
        shape.vertices[corner][0] = 6;
    }
    assert!(shape.is_valid());
    let cell = Cell {
        shape: Some(shape),
        ..Cell::solid(1)
    };
    for y in 1..5 {
        world.set([3, y, 0], Some(cell));
    }
    let mut player = player();
    tick(
        &world,
        &mut player,
        &InputFrame {
            strafe: 1.0,
            ..Default::default()
        },
        90,
    );
    assert!(
        (3.03..3.06).contains(&player.position.x),
        "position {:?}",
        player.position
    );
    for y in 1..5 {
        assert!(!player_intersects_cell(&player, [3, y, 0], cell));
    }
}

#[test]
fn a_shaped_sloping_underside_catches_the_head_above_the_cell_bottom() {
    let mut world = floor();
    let mut shape = Shape::solid();
    shape.vertices[1][1] = 4;
    shape.vertices[2][1] = 4;
    assert!(shape.is_valid());
    let cell = Cell {
        shape: Some(shape),
        ..Cell::solid(1)
    };
    world.set([0, 3, 0], Some(cell));
    let mut player = player();
    tick(
        &world,
        &mut player,
        &InputFrame {
            jump: true,
            ..Default::default()
        },
        1,
    );
    let mut peak = player.position.y;
    for _ in 0..90 {
        tick(&world, &mut player, &InputFrame::default(), 1);
        peak = peak.max(player.position.y);
        assert!(
            !player_intersects_cell(&player, [0, 3, 0], cell),
            "head penetrated at {:?}",
            player.position
        );
    }
    // Footprint x=.2..=.8 sees the lowest underside at world y=3.1.
    assert!((1.345..1.351).contains(&peak), "peak {peak}");
    assert!(player.grounded);
}

#[test]
fn explicit_shape_ramps_keep_shared_floor_collision_and_are_walkable() {
    let mut world = floor();
    let mut ramps = Vec::new();
    for step in 0..8 {
        let x = 2 + step;
        let level = 1 + step / 2;
        let low = if step % 2 == 0 { 0 } else { 4 };
        for z in -1..2 {
            for y in 1..level {
                world.set([x, y, z], Some(Cell::solid(0)));
            }
            let legacy = Cell {
                heights: [low, low + 4, low + 4, low],
                ..Cell::solid(3)
            };
            let cell = Cell {
                shape: Some(legacy.as_shape()),
                ..legacy
            };
            assert!(cell.is_valid());
            world.set([x, level, z], Some(cell));
            ramps.push(([x, level, z], cell));
        }
    }
    let mut player = player();
    let input = InputFrame {
        yaw: std::f32::consts::FRAC_PI_2,
        forward: 1.0,
        ..Default::default()
    };
    let mut peak = player.position.y;
    for _ in 0..85 {
        tick(&world, &mut player, &input, 1);
        peak = peak.max(player.position.y);
        for &(pos, cell) in &ramps {
            assert!(
                !player_intersects_cell(&player, pos, cell),
                "penetration at {:?}",
                player.position
            );
        }
    }
    assert!(player.position.x > 9.5, "stuck at {:?}", player.position);
    assert!(peak > 4.95, "peak {peak}");
}
