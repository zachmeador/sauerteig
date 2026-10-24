use sauerteig_core::*;

fn bounds() -> WorldBounds {
    WorldBounds {
        min: [-256, -128, -256],
        max: [512, 256, 512],
    }
}

fn region(origin: [i32; 3], size: u32, cell: Cell) -> CellFragment {
    CellFragment {
        origin,
        size,
        bounds: FineBox {
            min: origin,
            max: origin.map(|value| value + size as i32),
        },
        cell,
    }
}

fn player_at(position: Vec3) -> Player {
    let mut player = Player::new(0, "compact movement".into());
    player.position = position;
    player
}

fn tick(world: &World, player: &mut Player, input: InputFrame, frames: usize) {
    for _ in 0..frames {
        simulate_player(world, player, &input, FIXED_DT);
    }
}

#[test]
fn large_floor_supports_player_outside_legacy_bounds() {
    let floor = region([128 * 8, 96 * 8, 128 * 8], 128, Cell::solid(0));
    let world = World::from_compact_parts(bounds(), &[floor], &[], &[]).unwrap();
    let mut player = player_at(Vec3::new(136.5, 112.01, 136.5));
    tick(&world, &mut player, InputFrame::default(), 120);
    assert!(player.grounded);
    assert!((player.position.y - 112.0).abs() < 0.001);
    assert!((player.position.x - 136.5).abs() < 0.001);
    assert!((player.position.z - 136.5).abs() < 0.001);
    assert_eq!(world.regions().len(), 1);
    assert!(world.cells().is_empty());
}

#[test]
fn local_removal_changes_compact_floor_support_immediately() {
    let floor = region([128 * 8, 96 * 8, 128 * 8], 128, Cell::solid(0));
    let mut world = World::from_compact_parts(bounds(), &[floor], &[], &[]).unwrap();
    let mut player = player_at(Vec3::new(136.5, 112.01, 136.5));
    tick(&world, &mut player, InputFrame::default(), 2);
    world
        .apply_root_edits(&[RootEdit {
            pos: [136, 111, 136],
            fragments: vec![],
        }])
        .unwrap();
    tick(&world, &mut player, InputFrame::default(), 10);
    assert!(!player.grounded);
    assert!(player.position.y < 111.9);
    tick(&world, &mut player, InputFrame::default(), 60);
    assert!(player.grounded);
    assert!((player.position.y - 111.0).abs() < 0.001);
}

#[test]
fn compact_slope_is_walkable_without_coarse_cell_expansion() {
    let slope = region(
        [128 * 8, 96 * 8, 128 * 8],
        128,
        Cell {
            heights: [0, 8, 8, 0],
            ..Cell::solid(0)
        },
    );
    let world =
        World::from_compact_parts(bounds(), std::slice::from_ref(&slope), &[], &[]).unwrap();
    let mut player = player_at(Vec3::new(136.0, 104.31, 136.0));
    tick(&world, &mut player, InputFrame::default(), 2);
    let input = InputFrame {
        yaw: std::f32::consts::FRAC_PI_2,
        forward: 1.0,
        ..Default::default()
    };
    for _ in 0..30 {
        simulate_player(&world, &mut player, &input, FIXED_DT);
        assert!(!player_intersects_fragment(&player, &slope));
    }
    assert!(player.position.x > 139.0, "position {:?}", player.position);
    assert!(player.position.y > 107.0, "position {:?}", player.position);
    assert!(world.cells().is_empty());
}

#[test]
fn compact_ceiling_stops_jump_at_its_actual_underside() {
    let floor = region([128 * 8, 96 * 8, 128 * 8], 128, Cell::solid(0));
    let mut ceiling = region([128 * 8, 114 * 8, 128 * 8], 128, Cell::solid(0));
    ceiling.bounds.max[1] = 115 * 8;
    let world = World::from_compact_parts(bounds(), &[floor, ceiling.clone()], &[], &[]).unwrap();
    let mut player = player_at(Vec3::new(136.5, 112.01, 136.5));
    tick(&world, &mut player, InputFrame::default(), 2);
    tick(
        &world,
        &mut player,
        InputFrame {
            jump: true,
            ..Default::default()
        },
        1,
    );
    let mut peak = player.position.y;
    for _ in 0..90 {
        simulate_player(&world, &mut player, &InputFrame::default(), FIXED_DT);
        peak = peak.max(player.position.y);
        assert!(!player_intersects_fragment(&player, &ceiling));
    }
    assert!((112.24..112.251).contains(&peak), "peak {peak}");
    assert!(player.grounded);
}

#[test]
fn editor_flight_uses_each_dynamic_axis_limit() {
    let world = World::from_compact_parts(bounds(), &[], &[], &[]).unwrap();
    let mut player = player_at(Vec3::new(511.9, 200.0, 400.0));
    player.editing = true;
    tick(
        &world,
        &mut player,
        InputFrame {
            strafe: 1.0,
            ..Default::default()
        },
        30,
    );
    assert!((511.94..511.96).contains(&player.position.x));
    assert!((player.position.y - 200.0).abs() < 0.001);
    assert!((player.position.z - 400.0).abs() < 0.001);
    player.position = Vec3::new(-200.0, -127.9, 400.0);
    tick(
        &world,
        &mut player,
        InputFrame {
            vertical: -1.0,
            ..Default::default()
        },
        30,
    );
    assert!((-127.96..-127.94).contains(&player.position.y));
}

#[test]
fn contact_bias_remains_representable_at_large_coordinates() {
    let far_bounds = WorldBounds {
        min: [0, 0, 0],
        max: [8192, 8192, 8192],
    };
    let floor = region([256 * 8, 4080 * 8, 256 * 8], 128, Cell::solid(0));
    let world =
        World::from_compact_parts(far_bounds, std::slice::from_ref(&floor), &[], &[]).unwrap();
    let mut player = player_at(Vec3::new(264.5, 4096.01, 264.5));
    tick(&world, &mut player, InputFrame::default(), 120);
    assert!(player.grounded);
    assert!(player.position.y > 4096.0);
    assert!((player.position.y - 4096.0).abs() < 0.002);
    assert!(!player_intersects_fragment(&player, &floor));
}

#[test]
fn negative_height_coarse_overrides_use_dynamic_bounds() {
    let cell = CellEdit {
        pos: [136, -10, 136],
        cell: Some(Cell::solid(0)),
    };
    let world = World::from_compact_parts(bounds(), &[], &[cell], &[]).unwrap();
    let mut player = player_at(Vec3::new(136.5, -8.99, 136.5));
    tick(&world, &mut player, InputFrame::default(), 120);
    assert!(player.grounded);
    assert!((player.position.y + 9.0).abs() < 0.001);
}

#[test]
fn flight_in_a_short_world_does_not_panic() {
    let world = World::from_compact_parts(
        WorldBounds {
            min: [0, 0, 0],
            max: [1, 1, 1],
        },
        &[],
        &[],
        &[],
    )
    .unwrap();
    let mut player = player_at(Vec3::splat(0.5));
    player.editing = true;
    tick(
        &world,
        &mut player,
        InputFrame {
            vertical: 1.0,
            ..Default::default()
        },
        2,
    );
    assert!(player.position.is_finite());
    assert!(player.position.y >= 0.0 && player.position.y < 1.0);
}

fn assert_body_inside_bounds(player: &Player, bounds: WorldBounds) {
    let min = player.position - Vec3::new(PLAYER_RADIUS, 0.0, PLAYER_RADIUS);
    let max = player.position + Vec3::new(PLAYER_RADIUS, PLAYER_HEIGHT, PLAYER_RADIUS);
    assert!(min.cmpge(bounds.world_min()).all(), "minimum {min:?}");
    assert!(max.cmple(bounds.world_max()).all(), "maximum {max:?}");
}

#[test]
fn invalid_position_falls_back_inside_shifted_world_bounds() {
    let bounds = WorldBounds {
        min: [128, 96, 128],
        max: [256, 224, 256],
    };
    let world = World::from_compact_parts(bounds, &[], &[], &[]).unwrap();
    let mut player = player_at(Vec3::splat(f32::NAN));
    tick(&world, &mut player, InputFrame::default(), 1);
    assert!(player.position.is_finite());
    assert_body_inside_bounds(&player, bounds);
}

#[test]
fn falling_below_negative_world_returns_to_local_bounds() {
    let bounds = WorldBounds {
        min: [-512, -512, -512],
        max: [-256, -256, -256],
    };
    let world = World::from_compact_parts(bounds, &[], &[], &[]).unwrap();
    let mut player = player_at(Vec3::new(-400.0, -525.0, -400.0));
    tick(&world, &mut player, InputFrame::default(), 1);
    assert_body_inside_bounds(&player, bounds);
    assert_eq!(player.velocity, Vec3::ZERO);
    assert!(!player.grounded);
}

#[test]
fn fallback_in_a_short_world_remains_finite() {
    let bounds = WorldBounds {
        min: [100, 100, 100],
        max: [101, 101, 101],
    };
    let world = World::from_compact_parts(bounds, &[], &[], &[]).unwrap();
    let mut player = player_at(Vec3::splat(f32::NAN));
    player.editing = true;
    tick(&world, &mut player, InputFrame::default(), 1);
    assert!(player.position.is_finite());
    assert!(player.position.cmpge(bounds.world_min()).all());
    assert!(player.position.cmplt(bounds.world_max()).all());
}
