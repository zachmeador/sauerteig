use sauerteig_core::*;

fn bounds(min: [i32; 3], max: [i32; 3]) -> FineBox {
    FineBox { min, max }
}
fn volume(min: [i32; 3], max: [i32; 3], material: VolumeMaterial) -> MaterialVolume {
    MaterialVolume {
        bounds: bounds(min, max),
        material,
    }
}
fn paint(min: [i32; 3], max: [i32; 3], material: Option<VolumeMaterial>) -> VolumeEdit {
    VolumeEdit {
        bounds: bounds(min, max),
        material,
    }
}
fn fine_volume(volumes: &[MaterialVolume]) -> i64 {
    volumes
        .iter()
        .map(|volume| {
            (0..3)
                .map(|a| i64::from(volume.bounds.max[a] - volume.bounds.min[a]))
                .product::<i64>()
        })
        .sum()
}

#[test]
fn ordered_paint_and_clear_keep_exact_sparse_space_and_terrain() {
    let mut world = World::empty();
    world.set([0, 0, 0], Some(Cell::solid(2)));
    let terrain = world.cells();
    world
        .apply_volume_edits(&[
            paint([0, 8, 0], [32, 40, 32], Some(VolumeMaterial::Water)),
            paint([8, 16, 8], [24, 32, 24], Some(VolumeMaterial::Glass)),
            paint([12, 20, 12], [20, 28, 20], None),
        ])
        .unwrap();
    validate_material_volumes(world.volumes(), world.bounds().fine_box()).unwrap();
    assert_eq!(fine_volume(world.volumes()), 32 * 32 * 32 - 8 * 8 * 8);
    assert_eq!(
        world.volume_material_at(Vec3::new(0.5, 1.5, 0.5)),
        Some(VolumeMaterial::Water)
    );
    assert_eq!(
        world.volume_material_at(Vec3::new(1.25, 2.25, 1.25)),
        Some(VolumeMaterial::Glass)
    );
    assert_eq!(world.volume_material_at(Vec3::new(2.0, 3.0, 2.0)), None);
    assert_eq!(world.cells().len(), terrain.len());
    assert_eq!(world.get([0, 0, 0]), Some(Cell::solid(2)));
    world
        .apply_volume_edits(&[paint([0, 8, 0], [32, 40, 32], None)])
        .unwrap();
    assert!(world.volumes().is_empty());
    assert_eq!(world.get([0, 0, 0]), Some(Cell::solid(2)));
}

#[test]
fn invalid_replacement_or_edit_leaves_every_volume_untouched() {
    let mut world = World::empty();
    let water = volume([0, 0, 0], [32, 32, 32], VolumeMaterial::Water);
    world.replace_volumes(&[water]).unwrap();
    for invalid in [
        volume([0, 0, 0], [0, 8, 8], VolumeMaterial::Glass),
        volume([0, -1, 0], [8, 8, 8], VolumeMaterial::Glass),
    ] {
        assert!(world.replace_volumes(&[invalid]).is_err());
        assert!(world
            .apply_volume_edits(&[
                paint([0, 0, 0], [8, 8, 8], None),
                VolumeEdit {
                    bounds: invalid.bounds,
                    material: Some(invalid.material)
                }
            ])
            .is_err());
        assert_eq!(world.volumes(), &[water]);
    }
    assert!(world
        .replace_volumes(&[
            water,
            volume([8, 8, 8], [40, 40, 40], VolumeMaterial::Glass)
        ])
        .is_err());
    assert_eq!(world.volumes(), &[water]);
}

#[test]
fn clipped_selection_can_be_translated_and_rotated_without_losing_volume() {
    let mut world = World::empty();
    world
        .replace_volumes(&[
            volume([0, 8, 0], [24, 24, 16], VolumeMaterial::Water),
            volume([24, 8, 0], [32, 24, 16], VolumeMaterial::Glass),
        ])
        .unwrap();
    let selection = world.volume_regions(bounds([8, 8, 0], [32, 24, 8]));
    assert_eq!(fine_volume(&selection), 24 * 16 * 8);
    assert!(selection
        .iter()
        .all(|volume| bounds([8, 8, 0], [32, 24, 8]).contains(volume.bounds)));
    // A quarter turn about the selection origin followed by a translation.
    let edits: Vec<_> = selection
        .iter()
        .map(|volume| {
            let b = volume.bounds;
            paint(
                [80 - b.max[2], b.min[1], 64 + b.min[0] - 8],
                [80 - b.min[2], b.max[1], 64 + b.max[0] - 8],
                Some(volume.material),
            )
        })
        .collect();
    world.apply_volume_edits(&edits).unwrap();
    let pasted = world.volume_regions(bounds([72, 8, 64], [80, 24, 88]));
    assert_eq!(fine_volume(&selection), fine_volume(&pasted));
    assert_eq!(
        world.volume_material_at(Vec3::new(9.5, 2.0, 8.5)),
        Some(VolumeMaterial::Water)
    );
    assert_eq!(
        world.volume_material_at(Vec3::new(9.5, 2.0, 10.5)),
        Some(VolumeMaterial::Glass)
    );
}

#[test]
fn large_regions_do_not_expand_into_metre_cells_and_adjacent_space_coalesces() {
    let mut world = World::from_compact_parts(
        WorldBounds {
            min: [-4096, 0, -4096],
            max: [4096, 4096, 4096],
        },
        &[],
        &[],
        &[],
    )
    .unwrap();
    let box_ = world.bounds().fine_box();
    world
        .apply_volume_edits(&[VolumeEdit {
            bounds: box_,
            material: Some(VolumeMaterial::Water),
        }])
        .unwrap();
    assert_eq!(world.volumes().len(), 1);
    assert!(world.cells().is_empty());
    assert!(world.chunk_keys().is_empty());
    world
        .apply_volume_edits(&[paint([-8, 8, -8], [8, 24, 8], None)])
        .unwrap();
    assert!(world.volumes().len() <= 6);
    world
        .apply_volume_edits(&[paint([-8, 8, -8], [8, 24, 8], Some(VolumeMaterial::Water))])
        .unwrap();
    assert_eq!(
        world.volumes(),
        &[MaterialVolume {
            bounds: box_,
            material: VolumeMaterial::Water
        }]
    );
}

#[test]
fn full_bounded_layer_validates_and_queries_spatially() {
    let mut world = World::empty();
    let boxes: Vec<_> = (0..MAX_MATERIAL_VOLUMES)
        .map(|id| {
            let x = (id % 32) as i32 * 2;
            let y = ((id / 32) % 32) as i32 * 2;
            let z = (id / 1024) as i32 * 2;
            volume([x, y, z], [x + 1, y + 1, z + 1], VolumeMaterial::Water)
        })
        .collect();
    world.replace_volumes(&boxes).unwrap();
    assert_eq!(world.volumes().len(), MAX_MATERIAL_VOLUMES);
    assert_eq!(
        world
            .volume_regions(bounds([20, 20, 20], [21, 21, 21]))
            .len(),
        1
    );
    let mut excessive = boxes;
    excessive.push(excessive[0]);
    assert!(world.replace_volumes(&excessive).is_err());
    assert_eq!(world.volumes().len(), MAX_MATERIAL_VOLUMES);
}

fn crossing_strips(count: i32, opposite_material: VolumeMaterial) -> Vec<MaterialVolume> {
    let mut volumes = Vec::new();
    for i in 0..count {
        // Gaps prevent coalescing. Each front Y strip meets every rear Z strip
        // over a positive-area rectangle on the shared X plane.
        volumes.push(volume(
            [0, i * 2, 0],
            [8, i * 2 + 1, count * 2],
            VolumeMaterial::Water,
        ));
        volumes.push(volume(
            [8, 0, i * 2],
            [16, count * 2, i * 2 + 1],
            opposite_material,
        ));
    }
    volumes
}

#[test]
fn shared_face_budget_rejects_crossing_strip_mesh_growth_atomically() {
    let mut world = World::from_compact_parts(
        WorldBounds {
            min: [0, 0, 0],
            max: [128, 128, 128],
        },
        &[],
        &[],
        &[],
    )
    .unwrap();
    assert_eq!(256 * 256, MAX_VOLUME_FACE_CONTACTS);
    let accepted = crossing_strips(256, VolumeMaterial::Water);
    world.replace_volumes(&accepted).unwrap();
    assert_eq!(world.volumes().len(), 512);
    let before = world.volumes().to_vec();
    let excessive = crossing_strips(257, VolumeMaterial::Water);
    let error = world.replace_volumes(&excessive).unwrap_err();
    assert!(error.contains("too many shared faces"));
    assert_eq!(world.volumes(), before);
    let edits: Vec<_> = excessive
        .into_iter()
        .map(|volume| VolumeEdit {
            bounds: volume.bounds,
            material: Some(volume.material),
        })
        .collect();
    assert!(world
        .apply_volume_edits(&edits)
        .unwrap_err()
        .contains("too many shared faces"));
    assert_eq!(world.volumes(), before);
    // Unlike equal-material joins, water/glass interfaces are kept by meshing
    // and therefore do not spend this subtraction budget.
    world
        .replace_volumes(&crossing_strips(257, VolumeMaterial::Glass))
        .unwrap();
    assert_eq!(world.volumes().len(), 514);
}

#[test]
fn shared_face_validation_keeps_huge_regions_compact_and_ignores_edge_contacts() {
    let mut world = World::from_compact_parts(
        WorldBounds {
            min: [-4096, 0, -4096],
            max: [4096, 8192, 4096],
        },
        &[],
        &[],
        &[],
    )
    .unwrap();
    let adjacent = [
        volume(
            [-32768, 0, -32768],
            [0, 65536, 32768],
            VolumeMaterial::Water,
        ),
        volume([0, 0, -32768], [32768, 65536, 32768], VolumeMaterial::Water),
    ];
    world.replace_volumes(&adjacent).unwrap();
    assert_eq!(world.volumes().len(), 1);
    assert_eq!(world.volumes()[0].bounds, world.bounds().fine_box());
    assert!(world.cells().is_empty() && world.chunk_keys().is_empty());
    let edge = [
        volume([0, 0, 0], [8, 8, 8], VolumeMaterial::Water),
        volume([8, 8, 0], [16, 16, 8], VolumeMaterial::Water),
        volume([8, 8, 8], [16, 16, 16], VolumeMaterial::Water),
    ];
    validate_material_volumes(&edge, world.bounds().fine_box()).unwrap();
}

#[test]
fn bounded_paste_can_clear_destination_and_apply_a_maximum_layer() {
    let mut world = World::empty();
    let mut edits = vec![paint([0, 0, 0], [64, 64, 32], None); MAX_MATERIAL_VOLUMES];
    edits.extend((0..MAX_MATERIAL_VOLUMES).map(|id| {
        let x = (id % 32) as i32 * 2;
        let y = ((id / 32) % 32) as i32 * 2;
        let z = (id / 1024) as i32 * 2;
        paint(
            [x, y, z],
            [x + 1, y + 1, z + 1],
            Some(VolumeMaterial::Water),
        )
    }));
    assert_eq!(edits.len(), MAX_VOLUME_EDITS);
    world.apply_volume_edits(&edits).unwrap();
    assert_eq!(world.volumes().len(), MAX_MATERIAL_VOLUMES);
    edits.push(edits[0]);
    assert!(world.apply_volume_edits(&edits).is_err());
    assert_eq!(world.volumes().len(), MAX_MATERIAL_VOLUMES);
    // Completely hidden paints can be discarded by index before subtracting
    // every later overlap, even when a request fills the complete edit budget.
    let repeated =
        vec![paint([0, 0, 0], [64, 64, 32], Some(VolumeMaterial::Water)); MAX_VOLUME_EDITS];
    world.apply_volume_edits(&repeated).unwrap();
    assert_eq!(world.volumes().len(), 1);
}

#[test]
fn v3_volume_round_trip_and_legacy_worlds_remain_readable() {
    let mut world = World::empty();
    world
        .replace_volumes(&[volume([0, 8, 0], [8, 16, 8], VolumeMaterial::Glass)])
        .unwrap();
    let json = serde_json::to_value(&world).unwrap();
    assert_eq!(json["format_version"], 3);
    assert_eq!(json["volumes"][0]["material"], "glass");
    let restored: World = serde_json::from_value(json).unwrap();
    assert_eq!(restored.volumes(), world.volumes());
    for legacy in [
        "[]",
        r#"{"format_version":1,"cells":[],"roots":[]}"#,
        r#"{"format_version":2,"cells":[],"roots":[],"regions":[]}"#,
    ] {
        assert!(serde_json::from_str::<World>(legacy)
            .unwrap()
            .volumes()
            .is_empty());
    }
}

#[test]
fn material_picking_is_separate_from_terrain_and_weapons_ignore_water() {
    let mut world = World::empty();
    world
        .replace_volumes(&[
            volume([8, 8, -8], [16, 32, 8], VolumeMaterial::Water),
            volume([24, 8, -8], [25, 32, 8], VolumeMaterial::Glass),
        ])
        .unwrap();
    let origin = Vec3::new(0.0, 2.0, 0.0);
    assert!(world.raycast(origin, Vec3::X, 8.0).is_none());
    let hit = world.raycast_volumes(origin, Vec3::X, 8.0).unwrap();
    assert_eq!(hit.face, 0);
    assert_eq!(hit.normal, -Vec3::X);
    assert_eq!(hit.distance, 1.0);
    assert_eq!(
        world
            .raycast_solid_volumes(origin, Vec3::X, 8.0)
            .unwrap()
            .distance,
        3.0
    );
    assert!(!world.intersects_solid_volume(Vec3::new(1.1, 1.5, -0.1), Vec3::new(1.9, 2.5, 0.1)));
    world.set([1, 2, 0], Some(Cell::solid(1)));
    assert_eq!(world.volume_material_at(Vec3::new(1.5, 2.5, 0.5)), None);
    world.set([1, 2, 0], None);
    assert_eq!(
        world.volume_material_at(Vec3::new(1.5, 2.5, 0.5)),
        Some(VolumeMaterial::Water)
    );
}

#[test]
fn material_rays_skip_partial_shared_faces_and_pick_the_exposed_exit() {
    let mut world = World::empty();
    world
        .replace_volumes(&[
            volume([0, 8, 0], [16, 40, 32], VolumeMaterial::Water),
            volume([16, 8, 0], [32, 24, 16], VolumeMaterial::Water),
        ])
        .unwrap();
    assert_eq!(world.volumes().len(), 2);
    let hit = world
        .raycast_volumes(Vec3::new(1.0, 2.0, 1.0), Vec3::X, 8.0)
        .unwrap();
    assert_eq!(hit.distance, 3.0);
    assert_eq!(hit.face, 1);
    assert_eq!(hit.point.x, 4.0);
    let exposed = world
        .raycast_volumes(Vec3::new(1.0, 4.0, 3.0), Vec3::X, 8.0)
        .unwrap();
    assert_eq!(exposed.distance, 1.0);
    assert_eq!(exposed.point.x, 2.0);
    let inward = world
        .raycast_volumes(Vec3::new(3.0, 2.0, 1.0), -Vec3::X, 8.0)
        .unwrap();
    assert_eq!(inward.distance, 3.0);
    assert_eq!(inward.point.x, 0.0);
}

#[test]
fn overlapping_paint_batches_match_independent_sampled_space() {
    let mut world = World::empty();
    let mut expected = vec![None; 16 * 16 * 16];
    let mut seed = 47_u32;
    let mut next = || {
        seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        seed
    };
    let mut edits = Vec::new();
    for i in 0..80 {
        let min = std::array::from_fn(|_| (next() % 15) as i32);
        let max = std::array::from_fn(|a| min[a] + 1 + (next() % (16 - min[a]) as u32) as i32);
        let material = match i % 3 {
            0 => Some(VolumeMaterial::Water),
            1 => Some(VolumeMaterial::Glass),
            _ => None,
        };
        edits.push(paint(min, max, material));
        for x in min[0]..max[0] {
            for y in min[1]..max[1] {
                for z in min[2]..max[2] {
                    expected[(x + y * 16 + z * 256) as usize] = material;
                }
            }
        }
    }
    world.apply_volume_edits(&edits).unwrap();
    for x in 0..16 {
        for y in 0..16 {
            for z in 0..16 {
                let point = (Vec3::new(x as f32, y as f32, z as f32) + Vec3::splat(0.5)) / 8.0;
                assert_eq!(
                    world.volume_material_at(point),
                    expected[x + y * 16 + z * 256],
                    "sample {x},{y},{z}"
                );
            }
        }
    }
}

#[test]
fn glass_stops_players_supports_feet_and_blocks_jump_ceilings() {
    let mut world = World::empty();
    world
        .replace_volumes(&[
            volume([-64, 0, -64], [64, 8, 64], VolumeMaterial::Glass),
            volume([16, 8, -16], [17, 48, 16], VolumeMaterial::Glass),
            volume([-8, 32, -8], [8, 33, 8], VolumeMaterial::Glass),
        ])
        .unwrap();
    let mut player = Player::new(1, "test".into());
    player.position = Vec3::new(0.0, 3.0, 0.0);
    for _ in 0..90 {
        simulate_player(&world, &mut player, &InputFrame::default(), FIXED_DT);
    }
    assert!(player.grounded);
    assert!((player.position.y - 1.0).abs() < 0.002);
    let jump = InputFrame {
        jump: true,
        ..Default::default()
    };
    let mut highest = player.position.y;
    for _ in 0..30 {
        simulate_player(&world, &mut player, &jump, FIXED_DT);
        highest = highest.max(player.position.y);
    }
    assert!(highest <= 4.0 - PLAYER_HEIGHT + 0.001);
    let right = InputFrame {
        strafe: 1.0,
        ..Default::default()
    };
    for _ in 0..120 {
        simulate_player(&world, &mut player, &right, FIXED_DT);
    }
    assert!(player.position.x > 1.65 && player.position.x <= 2.0 - PLAYER_RADIUS + 0.001);
}

#[test]
fn water_is_passable_slows_motion_and_allows_swimming_up_and_down() {
    let mut world = World::empty();
    world
        .replace_volumes(&[volume(
            [-128, 0, -128],
            [128, 128, 128],
            VolumeMaterial::Water,
        )])
        .unwrap();
    let mut player = Player::new(1, "swimmer".into());
    player.position = Vec3::new(0.0, 4.0, 0.0);
    player.velocity = Vec3::X * 12.0;
    simulate_player(&world, &mut player, &InputFrame::default(), FIXED_DT);
    assert!(player.velocity.x < 12.0 && player.position.x > 0.0);
    assert!(player.velocity.y.abs() < GRAVITY * FIXED_DT);
    let up = InputFrame {
        jump: true,
        ..Default::default()
    };
    for _ in 0..60 {
        simulate_player(&world, &mut player, &up, FIXED_DT);
    }
    assert!(player.position.y > 7.0);
    let high = player.position.y;
    let down = InputFrame {
        vertical: -1.0,
        ..Default::default()
    };
    for _ in 0..60 {
        simulate_player(&world, &mut player, &down, FIXED_DT);
    }
    assert!(player.position.y < high - 2.0);
    assert!(!player.grounded);
}
