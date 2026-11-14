use sauerteig_core::{
    Cell, CellEdit, CellFragment, FineBox, MaterialVolume, RootEdit, VolumeMaterial, World,
    WorldBounds,
};

#[test]
fn authored_snapshot_and_edit_view_keep_all_layers_consistent_after_mutation() {
    let region = CellFragment {
        bounds: FineBox {
            min: [0; 3],
            max: [32; 3],
        },
        origin: [0; 3],
        size: 32,
        cell: Cell::solid(1),
    };
    let detail = RootEdit {
        pos: [11, 0, 10],
        fragments: vec![CellFragment {
            bounds: FineBox {
                min: [88, 0, 80],
                max: [89, 1, 81],
            },
            origin: [88, 0, 80],
            size: 1,
            cell: Cell::solid(2),
        }],
    };
    let mut world = World::from_compact_parts(
        WorldBounds::default(),
        std::slice::from_ref(&region),
        &[CellEdit {
            pos: [10, 0, 10],
            cell: Some(Cell::solid(3)),
        }],
        &[
            detail.clone(),
            RootEdit {
                pos: [1, 1, 1],
                fragments: vec![],
            },
        ],
    )
    .unwrap();
    let water = MaterialVolume {
        bounds: FineBox {
            min: [80, 8, 80],
            max: [88, 16, 88],
        },
        material: VolumeMaterial::Water,
    };
    world.replace_volumes(&[water]).unwrap();
    let snapshot = world.snapshot();
    let view = world.clone();
    let cells = snapshot.cells();
    let roots = snapshot.root_overrides();
    world.set([10, 0, 10], None);
    world
        .apply_root_edits(&[
            RootEdit {
                pos: detail.pos,
                fragments: vec![],
            },
            RootEdit {
                pos: [2, 2, 2],
                fragments: vec![],
            },
        ])
        .unwrap();
    world.replace_volumes(&[]).unwrap();
    // The snapshot keeps the world as it was when taken.
    assert_eq!(snapshot.cells(), cells);
    assert_eq!(snapshot.root_overrides(), roots);
    assert_eq!(snapshot.volumes(), &[water]);
    assert_eq!(view.get([10, 0, 10]), Some(Cell::solid(3)));
    assert_eq!(
        view.root_fragments(detail.pos),
        sauerteig_core::canonical_root_fragments(detail.pos, &detail.fragments)
    );
    assert!(!view.root_fragments([2, 2, 2]).is_empty());
    assert!(world.root_fragments([2, 2, 2]).is_empty());
    let mut reopened = World::from_compact_parts(
        snapshot.bounds(),
        snapshot.regions(),
        &snapshot.cells(),
        &snapshot.root_overrides(),
    )
    .unwrap();
    reopened.replace_volumes(snapshot.volumes()).unwrap();
    assert_eq!(reopened.digest(), view.digest());
    assert_ne!(reopened.digest(), world.digest());
    assert_eq!(reopened.root_overrides(), view.root_overrides());
    assert_eq!(reopened.cells(), view.cells());
    assert_eq!(reopened.volumes(), view.volumes());
    // The region keeps its content around the hole.
    assert!(view.intersects_box(
        sauerteig_core::Vec3::splat(0.1),
        sauerteig_core::Vec3::splat(0.9)
    ));
    assert!(!view.intersects_box(
        sauerteig_core::Vec3::splat(1.1),
        sauerteig_core::Vec3::splat(1.9)
    ));
}
