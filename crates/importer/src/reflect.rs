//! Reflecting a tree across X = 0. Imports made before the importer fixed
//! its handedness are this mirror image of today's, so a re-import is
//! compared with the old copy reflected.

use sauerteig_core::{
    Leaf, LeafGeometry, NodeBox, Octree, OctreeBuilder, Shape, Source, WorldBounds, SHAPE_FACES,
};

/// The lattice slot a corner moves to when X is mirrored.
const MIRROR_SLOT: [usize; 8] = [1, 0, 3, 2, 5, 4, 7, 6];

fn mirror_box(at: NodeBox) -> NodeBox {
    NodeBox {
        min: [-(at.min[0] + at.size as i32), at.min[1], at.min[2]],
        size: at.size,
    }
}

pub(crate) fn mirror_shape(shape: Shape) -> Shape {
    let mut vertices = [[0; 3]; 8];
    for (slot, vertex) in vertices.iter_mut().enumerate() {
        let [x, y, z] = shape.vertices[MIRROR_SLOT[slot]];
        *vertex = [8 - x, y, z];
    }
    let mut diagonals = 0;
    for (face, quad) in SHAPE_FACES.iter().enumerate() {
        let old_face = if face < 2 { 1 - face } else { face };
        let old = SHAPE_FACES[old_face];
        let chosen = if shape.diagonals & (1 << old_face) == 0 {
            [old[0], old[2]]
        } else {
            [old[1], old[3]]
        };
        let chosen = chosen.map(|slot| MIRROR_SLOT[slot]);
        if !(chosen.contains(&quad[0]) && chosen.contains(&quad[2])) {
            diagonals |= 1 << face;
        }
    }
    Shape {
        vertices,
        diagonals,
    }
}

fn mirror_leaf(leaf: Leaf) -> Leaf {
    let geometry = match leaf.geometry {
        LeafGeometry::Empty => LeafGeometry::Empty,
        LeafGeometry::Full => LeafGeometry::Full,
        LeafGeometry::Shaped(shape) => LeafGeometry::Shaped(mirror_shape(shape)),
        LeafGeometry::Piece(source) => {
            let at = mirror_box(NodeBox {
                min: source.origin,
                size: source.size,
            });
            LeafGeometry::Piece(Source {
                origin: at.min,
                size: at.size,
                shape: mirror_shape(source.shape),
            })
        }
    };
    let mut faces = leaf.faces;
    faces.swap(0, 1);
    Leaf {
        geometry,
        faces,
        cube_material: leaf.cube_material,
    }
}

/// The tree's mirror image across X = 0, built on the given root, which must
/// hold the mirrored bounds and have the mirrored leaves on its grid.
pub fn reflect_x(tree: &Octree, root: NodeBox) -> Result<Octree, String> {
    let bounds = tree.bounds();
    let mirrored = WorldBounds {
        min: [-bounds.max[0], bounds.min[1], bounds.min[2]],
        max: [-bounds.min[0], bounds.max[1], bounds.max[2]],
    };
    let mut builder = OctreeBuilder::new(mirrored, root)?;
    let mut failure = None;
    tree.for_each_leaf(|at, leaf| {
        if failure.is_none() {
            if let Err(error) = builder.push(mirror_box(at), mirror_leaf(leaf)) {
                failure = Some(error);
            }
        }
    });
    match failure {
        Some(error) => Err(error),
        None => builder.finish(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{convert, read_ogz, ImportOptions, ImportedWorld};
    use sauerteig_core::{Octree, Vec3};
    use std::path::Path;

    fn load(world: &ImportedWorld) -> Octree {
        Octree::from_parts(
            world.bounds,
            &world.regions,
            &world.cells,
            &world.roots,
            &world.volumes,
        )
        .unwrap()
    }

    fn sample() -> Octree {
        let bounds = WorldBounds {
            min: [0, 0, 0],
            max: [4, 4, 4],
        };
        let root = NodeBox {
            min: [0, 0, 0],
            size: 32,
        };
        let mut builder = OctreeBuilder::new(bounds, root).unwrap();
        let mut warped = Shape::solid();
        warped.vertices[6] = [8, 3, 5];
        warped.vertices[4][1] = 2;
        warped.diagonals = 0b10_1001;
        let faces = [1, 2, 3, 4, 5, 6];
        let leaf = |geometry, faces| Leaf {
            geometry,
            faces,
            cube_material: 4,
        };
        builder
            .push(
                NodeBox {
                    min: [0, 0, 0],
                    size: 8,
                },
                leaf(LeafGeometry::Shaped(warped), faces),
            )
            .unwrap();
        builder
            .push(
                NodeBox {
                    min: [8, 0, 0],
                    size: 8,
                },
                Leaf::full([7; 6]),
            )
            .unwrap();
        let mut wedge = Shape::solid();
        wedge.vertices[4][1] = 0;
        wedge.vertices[5][1] = 0;
        builder
            .push(
                NodeBox {
                    min: [0, 8, 8],
                    size: 8,
                },
                leaf(
                    LeafGeometry::Piece(Source {
                        origin: [0, 0, 0],
                        size: 16,
                        shape: wedge,
                    }),
                    faces,
                ),
            )
            .unwrap();
        builder.finish().unwrap()
    }

    #[test]
    fn reflecting_twice_restores_the_tree_and_once_moves_leaves_across_x() {
        let tree = sample();
        let mirrored_root = mirror_box(tree.root_box());
        let once = reflect_x(&tree, mirrored_root).unwrap();
        assert_ne!(once.digest(), tree.digest());
        let twice = reflect_x(&once, tree.root_box()).unwrap();
        assert_eq!(twice.digest(), tree.digest());
        let mut full = Vec::new();
        once.for_each_leaf(|at, leaf| {
            if matches!(leaf.geometry, LeafGeometry::Full) {
                full.push(at);
            }
        });
        // The full leaf at x = 8..16 now sits at x = -16..-8.
        assert_eq!(
            full,
            [NodeBox {
                min: [-16, 0, 0],
                size: 8
            }]
        );
        let mut faces = Vec::new();
        once.for_each_leaf(|_, leaf| {
            if matches!(leaf.geometry, LeafGeometry::Shaped(_)) {
                faces.push(leaf.faces);
            }
        });
        assert_eq!(faces, [[2, 1, 3, 4, 5, 6]]);
    }

    /// Re-imports an original from `.local/originals/` and compares it with
    /// the library's copy from `.local/library/`, which was imported before
    /// the handedness fix, reflected. Skips what the machine lacks.
    #[test]
    #[ignore = "needs private copies in .local/originals and .local/library"]
    fn private_reimports_equal_the_library_copies_reflected() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.local");
        let mut compared = 0;
        for name in ["complex", "ralith1"] {
            let original = root.join(format!("originals/{name}.ogz"));
            let copy = root.join(format!("library/{name}.json"));
            if !original.exists() || !copy.exists() {
                eprintln!("skipping {name}");
                continue;
            }
            let map = read_ogz(&std::fs::read(original).unwrap()).unwrap();
            let (new, _) = convert(&map, &ImportOptions::default()).unwrap();
            let old: ImportedWorld = serde_json::from_slice(&std::fs::read(copy).unwrap()).unwrap();
            let (new_tree, old_tree) = (load(&new), load(&old));
            let reflected = reflect_x(&old_tree, new_tree.root_box()).unwrap();
            assert_eq!(new_tree.digest(), reflected.digest(), "{name}");
            // Objects mirror too, bit for bit.
            let bits = |p: Vec3| p.to_array().map(f32::to_bits);
            let mut a: Vec<_> = new
                .objects
                .iter()
                .map(|o| (o.id, bits(o.position)))
                .collect();
            let mut b: Vec<_> = old
                .objects
                .iter()
                .map(|o| {
                    (
                        o.id,
                        bits(Vec3::new(-o.position.x, o.position.y, o.position.z)),
                    )
                })
                .collect();
            a.sort_unstable();
            b.sort_unstable();
            assert_eq!(a, b, "{name} objects");
            compared += 1;
        }
        assert!(compared > 0 || !root.join("library").exists());
    }
}
