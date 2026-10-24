use sauerteig_core::*;

fn region(origin: [i32; 3], size: u32, cell: Cell) -> CellFragment {
    CellFragment {
        origin,
        size,
        bounds: FineBox {
            min: origin,
            max: origin.map(|v| v + size as i32),
        },
        cell,
    }
}
fn large_world() -> World {
    World::from_compact_parts(
        WorldBounds {
            min: [-2048, 0, -2048],
            max: [2048, 4096, 2048],
        },
        &[region([-16384, 0, -16384], 32768, Cell::solid(1))],
        &[],
        &[],
    )
    .unwrap()
}
#[test]
fn one_large_leaf_remains_compact_and_selection_is_bounded() {
    let world = large_world();
    assert_eq!(world.region_count(), 1);
    assert!(world.cells().is_empty());
    assert!(world.detailed_roots().is_empty());
    assert_eq!(world.chunk_keys().len(), 1);
    assert_eq!(world.mesh_chunk(world.chunk_keys()[0]).indices.len(), 36);
    assert!(world.try_roots_in(world.bounds().fine_box(), 512).is_err());
    assert_eq!(
        world
            .try_roots_in(FineBox::root([1000, 2000, 1000]), 512)
            .unwrap()
            .len(),
        1
    );
    assert!(world.intersects_box(
        Vec3::new(1000.1, 2000.1, 1000.1),
        Vec3::new(1000.9, 2000.9, 1000.9)
    ));
    assert!(!world.intersects_box(
        Vec3::new(3000.1, 2000.1, 1000.1),
        Vec3::new(3000.9, 2000.9, 1000.9)
    ));
}
#[test]
fn empty_override_is_a_real_hole_after_save_and_undo() {
    let mut world = large_world();
    let pos = [1000, 2000, 1000];
    let before = world.root_fragments(pos);
    let owner = world.chunk_keys()[0];
    let dirty = world
        .apply_root_edits(&[RootEdit {
            pos,
            fragments: vec![],
        }])
        .unwrap();
    assert!(dirty.contains(&owner));
    assert_eq!(world.override_count(), 1);
    assert_eq!(world.region_count(), 1);
    assert!(world.root_fragments(pos).is_empty());
    assert!(!world.intersects_box(
        Vec3::new(1000.1, 2000.1, 1000.1),
        Vec3::new(1000.9, 2000.9, 1000.9)
    ));
    assert!(world.intersects_box(
        Vec3::new(1001.1, 2000.1, 1000.1),
        Vec3::new(1001.9, 2000.9, 1000.9)
    ));
    let hit = world
        .raycast(Vec3::new(1000.5, 2000.5, 1000.5), Vec3::X, 4.0)
        .unwrap();
    assert!((hit.distance - 0.5).abs() < 0.001);
    assert_eq!(hit.face, 0);
    assert_eq!(hit.pos, [1001, 2000, 1000]);
    let json = serde_json::to_value(&world).unwrap();
    assert_eq!(json["format_version"], 2);
    assert_eq!(json["regions"].as_array().unwrap().len(), 1);
    assert_eq!(json["roots"].as_array().unwrap().len(), 1);
    let mut restored: World = serde_json::from_value(json).unwrap();
    assert!(restored.root_fragments(pos).is_empty());
    restored
        .apply_root_edits(&[RootEdit {
            pos,
            fragments: before,
        }])
        .unwrap();
    assert!(restored.intersects_box(
        Vec3::new(1000.1, 2000.1, 1000.1),
        Vec3::new(1000.9, 2000.9, 1000.9)
    ));
    // An internal join introduced by local undo cannot become a hit.
    assert!(restored
        .raycast(Vec3::new(1000.5, 2000.5, 1000.5), Vec3::X, 4.0)
        .is_none());
}
#[test]
fn compact_edits_validate_every_replacement_before_mutation() {
    let mut world = large_world();
    let before = serde_json::to_vec(&world).unwrap();
    let invalid = region([8000, 8000, 8000], 8, Cell::solid(9));
    assert!(world
        .apply_root_edits(&[
            RootEdit {
                pos: [1000, 1000, 1000],
                fragments: vec![]
            },
            RootEdit {
                pos: [1001, 1000, 1000],
                fragments: vec![invalid]
            },
        ])
        .is_err());
    assert_eq!(serde_json::to_vec(&world).unwrap(), before);
    assert!(world
        .apply_root_edits(&[RootEdit {
            pos: [3000, 1000, 1000],
            fragments: vec![]
        }])
        .is_err());
    assert_eq!(serde_json::to_vec(&world).unwrap(), before);
}
#[test]
fn overlapping_regions_and_out_of_bounds_data_are_rejected() {
    let a = region([0, 0, 0], 8, Cell::solid(1));
    assert!(World::from_compact_parts(WorldBounds::default(), &[a.clone(), a], &[], &[]).is_err());
    let a = region([8192 * 8, 0, 0], 8, Cell::solid(1));
    assert!(World::from_compact_parts(WorldBounds::default(), &[a], &[], &[]).is_err());
    let json = r#"{"format_version":3,"bounds":{"min":[0,0,0],"max":[8,8,8]},"regions":[],"cells":[],"roots":[]}"#;
    assert!(serde_json::from_str::<World>(json).is_err());
}
#[test]
fn query_index_is_conservative_and_mesh_keys_never_collide_with_local_chunks() {
    let regions: Vec<_> = (0..300)
        .map(|x| region([x * 16, 0, 0], 8, Cell::solid((x % 8) as u8)))
        .collect();
    let mut world = World::from_compact_parts(
        WorldBounds {
            min: [0, 0, 0],
            max: [1024, 16, 16],
        },
        &regions,
        &[],
        &[],
    )
    .unwrap();
    assert_eq!(world.chunk_keys().len(), 3);
    for x in 0..300 {
        let found = world.fragments_in(FineBox::root([x * 2, 0, 0]));
        assert_eq!(found.len(), 1);
        assert_eq!(found[0], regions[x as usize]);
        assert!(world
            .fragments_in(FineBox::root([x * 2 + 1, 0, 0]))
            .is_empty());
    }
    let base_keys = world.chunk_keys();
    world
        .apply_root_edits(&[RootEdit {
            pos: [1, 0, 0],
            fragments: vec![CellFragment::from_cell([1, 0, 0], Cell::solid(2))],
        }])
        .unwrap();
    assert!(base_keys.iter().all(|k| *k != World::cell_chunk([1, 0, 0])));
    assert_eq!(world.chunk_keys().len(), 4);
}
#[test]
fn large_coordinate_rays_keep_eighth_metre_details() {
    let f = region([32768, 32768, 32768], 1, Cell::solid(2));
    let world = World::from_compact_parts(
        WorldBounds {
            min: [4096, 4096, 4096],
            max: [4100, 4100, 4100],
        },
        &[f],
        &[],
        &[],
    )
    .unwrap();
    let hit = world
        .raycast(Vec3::new(4096.0625, 4097.0, 4096.0625), -Vec3::Y, 2.0)
        .unwrap();
    assert_eq!(hit.face, 3);
    assert!((hit.point.y - 4096.125).abs() < 0.0005);
}

#[test]
fn face_materials_survive_coarse_fine_caps_and_compact_meshes() {
    let mut cell = Cell::solid(7);
    for face in 0..6 {
        cell.set_face_material(face, face);
    }
    assert!(cell.is_valid());
    let cases = [region([0, 0, 0], 8, cell), region([0, 0, 0], 64, cell)];
    for f in cases {
        let world = World::from_compact_parts(WorldBounds::default(), &[f], &[], &[]).unwrap();
        let mesh = world.mesh_chunk(world.chunk_keys()[0]);
        for v in mesh.vertices {
            let n = Vec3::from_array(v.normal);
            let axis = (0..3)
                .max_by(|&a, &b| n[a].abs().total_cmp(&n[b].abs()))
                .unwrap();
            let face = axis * 2 + usize::from(n[axis] > 0.0);
            assert_eq!(v.material, face as u8);
        }
    }
    let mut world = World::empty();
    world.set([0, 0, 0], Some(cell));
    let mesh = world.mesh_chunk([0, 0, 0]);
    assert_eq!(
        mesh.vertices
            .iter()
            .map(|v| v.material)
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        6
    );
    let half = CellFragment {
        bounds: FineBox {
            min: [0, 0, 0],
            max: [4, 8, 8],
        },
        ..CellFragment::from_cell([0, 0, 0], cell)
    };
    world
        .apply_root_edits(&[RootEdit {
            pos: [0, 0, 0],
            fragments: vec![half],
        }])
        .unwrap();
    assert!(world
        .mesh_chunk([0, 0, 0])
        .vertices
        .iter()
        .filter(|v| v.position[0] == 0.5 && v.normal[0] > 0.99)
        .all(|v| v.material == 1));
    assert_eq!(
        world.root_fragments([0, 0, 0])[0].cell.face_materials,
        cell.face_materials
    );
}
#[test]
fn old_cells_default_to_fallback_and_bad_face_slots_are_rejected() {
    let mut cell: Cell = serde_json::from_str(r#"{"material":2,"heights":[8,8,8,8]}"#).unwrap();
    assert_eq!(cell.face_materials, None);
    assert!((0..6).all(|f| cell.material_for_face(f) == 2));
    cell.set_face_material(3, 5);
    assert_eq!(cell.material_for_face(3), 5);
    assert_eq!(cell.material_for_face(1), 2);
    cell.set_material(4);
    assert_eq!(cell.face_materials, None);
    cell.face_materials = Some([8; 6]);
    assert!(!cell.is_valid());
}

#[test]
fn compact_noop_keeps_storage_and_other_root_identity_stable() {
    let f = CellFragment::from_cell([0, 0, 0], Cell::solid(1));
    let mut world = World::from_compact_parts(WorldBounds::default(), &[f], &[], &[]).unwrap();
    let before = serde_json::to_vec(&world).unwrap();
    assert!(world
        .apply_root_edits(&[RootEdit {
            pos: [0, 0, 0],
            fragments: world.root_fragments([0, 0, 0])
        }])
        .unwrap()
        .is_empty());
    assert_eq!(serde_json::to_vec(&world).unwrap(), before);
    let mut world = large_world();
    let before = world.root_fragments([2, 2, 2]);
    assert!(world
        .apply_root_edits(&[RootEdit {
            pos: [2, 2, 2],
            fragments: before.clone()
        }])
        .unwrap()
        .is_empty());
    assert_eq!(world.override_count(), 0);
    world
        .apply_root_edits(&[RootEdit {
            pos: [1, 1, 1],
            fragments: vec![],
        }])
        .unwrap();
    assert_eq!(world.root_fragments([2, 2, 2]), before);
}
#[test]
fn huge_triangle_does_not_accept_a_ray_outside_its_edge() {
    let regions = [
        region([0, 0, 0], 32768, Cell::solid(1)),
        CellFragment::from_cell([-1, 4095, 0], Cell::solid(2)),
    ];
    let world = World::from_compact_parts(
        WorldBounds {
            min: [-1, 0, 0],
            max: [4096, 8192, 4096],
        },
        &regions,
        &[],
        &[],
    )
    .unwrap();
    assert!(world
        .raycast(Vec3::new(-0.01, 4097.0, 2048.0), -Vec3::Y, 2.0)
        .is_none());
    let hit = world
        .raycast(Vec3::new(4096.0, 4097.0, 2048.0), -Vec3::Y, 2.0)
        .unwrap();
    assert!(world.contains_pos(hit.pos));
    assert!(FineBox::root(hit.pos).contains(hit.bounds));
}

#[test]
fn large_finite_box_queries_remain_bounded_by_stored_geometry() {
    let world = large_world();
    assert_eq!(
        world
            .fragments_in(FineBox {
                min: [i32::MIN; 3],
                max: [i32::MAX; 3]
            })
            .len(),
        1
    );
    let world = World::from_compact_parts(
        WorldBounds {
            min: [-4096; 3],
            max: [4096; 3],
        },
        &[],
        &[CellEdit {
            pos: [2000, 2000, 2000],
            cell: Some(Cell::solid(1)),
        }],
        &[],
    )
    .unwrap();
    assert!(world.intersects_box(Vec3::splat(-1e30), Vec3::splat(1e30)));
}
#[test]
fn shallow_angle_ray_crosses_a_large_world_boundary() {
    let world = World::from_compact_parts(
        WorldBounds {
            min: [-1, 0, -1],
            max: [4096, 4096, 4096],
        },
        &[region([0; 3], 32768, Cell::solid(1))],
        &[],
        &[],
    )
    .unwrap();
    let hit = world
        .raycast(
            Vec3::new(-0.001, 2048.0, -1.0),
            Vec3::new(0.000001, 0.0, 1.0),
            2000.0,
        )
        .unwrap();
    assert_eq!(hit.face, 0);
    assert!((hit.distance - 1000.0).abs() < 0.001);
}
