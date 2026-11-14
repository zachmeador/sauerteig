use super::format3::subtree;
use super::*;
use crate::{
    Cell, CellEdit, CellFragment, InputFrame, MaterialVolume, MovementProfile, Player, RayHit,
    RootEdit, Vec3, World, FIXED_DT,
};

pub(crate) struct Rng(u64);

impl Rng {
    pub(crate) fn new(seed: u64) -> Self {
        Self(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1)
    }
    pub(crate) fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }
    pub(crate) fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }
    pub(crate) fn range(&mut self, lo: i32, hi: i32) -> i32 {
        lo + self.below((hi - lo).max(1) as u64) as i32
    }
    pub(crate) fn chance(&mut self, percent: u64) -> bool {
        self.below(100) < percent
    }
    pub(crate) fn unit(&mut self) -> f32 {
        (self.next() >> 40) as f32 / (1u64 << 24) as f32
    }
    fn shuffle<T>(&mut self, items: &mut [T]) {
        for i in (1..items.len()).rev() {
            items.swap(i, self.below(i as u64 + 1) as usize);
        }
    }
}

pub(crate) fn random_shape(rng: &mut Rng) -> Shape {
    loop {
        let mut shape = Shape::solid();
        let depth = 1 + rng.below(4) as u8;
        for point in &mut shape.vertices {
            for coordinate in point {
                let inset = rng.below(u64::from(depth)) as u8 * if depth > 2 { 2 } else { 4 };
                *coordinate = if *coordinate == 0 { inset } else { 8 - inset };
            }
        }
        shape.diagonals = rng.below(64) as u8;
        if shape.is_valid() && !lattice::is_solid(shape) {
            return shape;
        }
    }
}

pub(crate) fn random_cell(rng: &mut Rng) -> Cell {
    let material = rng.below(8) as u8;
    let mut cell = Cell::solid(material);
    match rng.below(4) {
        0 => {}
        1 => {
            cell.heights = std::array::from_fn(|_| rng.below(9) as u8);
            if cell.heights == [0; 4] {
                cell.heights[0] = 8;
            }
            cell.diagonal = rng.chance(50);
        }
        _ => cell.shape = Some(random_shape(rng)),
    }
    if rng.chance(30) {
        for face in 0..6 {
            cell.set_face_material(face, rng.below(8) as u8);
        }
    }
    assert!(cell.is_valid());
    cell
}

fn small_bounds() -> WorldBounds {
    WorldBounds {
        min: [-4, -2, -4],
        max: [4, 6, 4],
    }
}

fn random_node(rng: &mut Rng, tree: &Octree, max_level: u32) -> NodeBox {
    let root = tree.root_box();
    let level = 1 + rng.below(u64::from(max_level)) as u32;
    let size = root.size >> level.min(root.size.trailing_zeros());
    let cells = (root.size / size) as i32;
    NodeBox {
        min: std::array::from_fn(|a| root.min[a] + rng.range(0, cells) * size as i32),
        size,
    }
}

fn random_leaf(rng: &mut Rng, at: NodeBox) -> Leaf {
    let faces = std::array::from_fn(|_| rng.below(3) as u8);
    let cube = [MAT_AIR, MAT_AIR, MAT_WATER, MAT_GLASS][rng.below(4) as usize];
    let geometry = match rng.below(5) {
        0 => LeafGeometry::Empty,
        1 => LeafGeometry::Full,
        2 => LeafGeometry::Shaped(random_shape(rng)),
        _ => {
            // A larger, offset source cube around this node.
            let size = (at.size * (1 << rng.below(3))).clamp(at.size, 64.max(at.size));
            let origin =
                std::array::from_fn(|a| at.min[a] - rng.range(0, (size - at.size) as i32 + 1));
            LeafGeometry::Piece(Source {
                origin,
                size,
                shape: random_shape(rng),
            })
        }
    };
    Leaf {
        geometry,
        faces,
        cube_material: cube,
    }
}

fn random_tree(rng: &mut Rng, leaves: usize) -> (Octree, Vec<(NodeBox, Leaf)>) {
    let tree = Octree::new(small_bounds()).unwrap();
    let mut chosen: Vec<(NodeBox, Leaf)> = Vec::new();
    for _ in 0..leaves * 4 {
        if chosen.len() == leaves {
            break;
        }
        let at = random_node(rng, &tree, 6);
        if chosen
            .iter()
            .any(|(other, _)| other.contains(at) || at.contains(*other))
        {
            continue;
        }
        chosen.push((at, random_leaf(rng, at)));
    }
    let mut builder = OctreeBuilder::new(tree.bounds(), tree.root_box()).unwrap();
    for &(at, leaf) in &chosen {
        builder.push(at, leaf).unwrap();
    }
    (builder.finish().unwrap(), chosen)
}

fn assert_canonical(node: &Node, at: NodeBox) {
    if let Node::Branch(children) = node {
        assert!(
            merge(children, at).is_none(),
            "a branch at {at:?} could still merge"
        );
        assert!(
            !children.iter().all(Node::is_air),
            "an all-air branch at {at:?}"
        );
        for (i, child) in children.iter().enumerate() {
            assert_canonical(child, at.child(i));
        }
    }
    if let Node::Piece(p) = node {
        assert_eq!(
            lattice::classify(&p.source, at.min, at.size as i32),
            lattice::Class::Crossing
        );
        assert!(p.source.origin != at.min || p.source.size != at.size);
    }
    if let Node::Shaped(s) = node {
        assert!(!lattice::is_solid(s.shape));
        assert_eq!(lattice::normalize_shape(s.shape), s.shape);
    }
}

#[test]
fn leaves_in_any_order_build_the_same_tree() {
    for seed in 0..40 {
        let mut rng = Rng::new(seed);
        let (tree, mut leaves) = random_tree(&mut rng, 30);
        assert_canonical(tree.root_node(), tree.root_box());
        rng.shuffle(&mut leaves);
        let mut builder = OctreeBuilder::new(tree.bounds(), tree.root_box()).unwrap();
        for &(at, leaf) in &leaves {
            builder.push(at, leaf).unwrap();
        }
        let shuffled = builder.finish().unwrap();
        assert_eq!(shuffled.digest(), tree.digest());
        assert!(shuffled.root_node() == tree.root_node());
        // Writing the same leaves one by one into an empty tree agrees too.
        let mut written = Octree::with_root(tree.bounds(), tree.root_box()).unwrap();
        for &(at, leaf) in &leaves {
            let node = make_leaf(at, leaf).unwrap();
            written.write(at, &node, Parts::All).unwrap();
        }
        assert_eq!(written.digest(), tree.digest(), "seed {seed}");
        if let Some(&(at, leaf)) = leaves.first() {
            let mut builder = OctreeBuilder::new(tree.bounds(), tree.root_box()).unwrap();
            builder.push(at, leaf).unwrap();
            builder.push(at, leaf).unwrap();
            assert!(builder.finish().is_err());
            if at.size > 1 {
                let mut builder = OctreeBuilder::new(tree.bounds(), tree.root_box()).unwrap();
                builder.push(at, Leaf::full([0; 6])).unwrap();
                builder.push(at.child(3), Leaf::full([1; 6])).unwrap();
                assert!(builder.finish().is_err());
            }
        }
    }
}

#[test]
fn canonical_form_merges_full_empty_and_pieces_but_not_equal_shapes() {
    let bounds = small_bounds();
    let tree = Octree::new(bounds).unwrap();
    let root = tree.root_box();
    let parent = NodeBox {
        min: root.min,
        size: 16,
    };
    let build = |leaves: &[(NodeBox, Leaf)]| {
        let mut builder = OctreeBuilder::new(bounds, root).unwrap();
        for &(at, leaf) in leaves {
            builder.push(at, leaf).unwrap();
        }
        builder.finish().unwrap()
    };
    // Equal full children merge; so do equal water cubes.
    let full: Vec<_> = (0..8)
        .map(|i| (parent.child(i), Leaf::full([2; 6])))
        .collect();
    assert_eq!(build(&full).leaves(), vec![(parent, Leaf::full([2; 6]))]);
    let water: Vec<_> = (0..8)
        .map(|i| (parent.child(i), Leaf::empty(MAT_WATER)))
        .collect();
    assert_eq!(
        build(&water).leaves(),
        vec![(parent, Leaf::empty(MAT_WATER))]
    );
    // Different surface materials keep them apart.
    let mut mixed = full.clone();
    mixed[3].1.faces[4] = 1;
    assert_eq!(build(&mixed).leaves().len(), 8);
    // Eight equal wedges are a sawtooth, not a wedge.
    let wedge = Cell {
        heights: [0, 8, 8, 0],
        ..Cell::solid(0)
    }
    .as_shape();
    let shaped = |shape| Leaf {
        geometry: LeafGeometry::Shaped(shape),
        faces: [0; 6],
        cube_material: MAT_AIR,
    };
    let wedges: Vec<_> = (0..8).map(|i| (parent.child(i), shaped(wedge))).collect();
    assert_eq!(build(&wedges).leaves().len(), 8);
    // The eight pieces of one wedge become the wedge again; pieces wholly
    // inside or outside are full or empty leaves.
    let source = Source {
        origin: parent.min,
        size: parent.size,
        shape: wedge,
    };
    let piece = |source, faces, cube_material| Leaf {
        geometry: LeafGeometry::Piece(source),
        faces,
        cube_material,
    };
    let pieces: Vec<_> = (0..8)
        .map(|i| (parent.child(i), piece(source, [5; 6], MAT_AIR)))
        .collect();
    assert_eq!(
        build(&pieces).leaves(),
        vec![(
            parent,
            Leaf {
                faces: [5; 6],
                ..shaped(lattice::normalize_shape(wedge))
            }
        )]
    );
    let fine = build(&pieces[..7]);
    let kinds: Vec<_> = fine.leaves().iter().map(|(_, l)| l.geometry).collect();
    assert!(kinds.contains(&LeafGeometry::Full));
    assert!(kinds.iter().any(|g| matches!(g, LeafGeometry::Piece(_))));
    // At a coarser level the pieces merge into a piece of the larger source.
    let big = Source {
        origin: root.min,
        size: 64,
        shape: wedge,
    };
    let quarter: Vec<_> = (0..8)
        .map(|i| (parent.child(i), piece(big, [1; 6], MAT_WATER)))
        .collect();
    let leaves = build(&quarter).leaves();
    assert_eq!(leaves.len(), 1);
    assert_eq!(leaves[0].0, parent);
}

#[test]
fn meaningless_fields_are_normalized() {
    let bounds = small_bounds();
    let root = Octree::new(bounds).unwrap().root_box();
    let at = NodeBox {
        min: root.min,
        size: 8,
    };
    let shaped = |shape, faces| Leaf {
        geometry: LeafGeometry::Shaped(shape),
        faces,
        cube_material: MAT_AIR,
    };
    // A ramp's planar faces have no diagonal; a solid shape is a full leaf.
    let ramp = Cell {
        heights: [0, 8, 8, 0],
        diagonal: true,
        ..Cell::solid(0)
    };
    let a = make_leaf(at, shaped(ramp.as_shape(), [1; 6])).unwrap();
    let flat = Cell {
        diagonal: false,
        ..ramp
    };
    let b = make_leaf(at, shaped(flat.as_shape(), [1; 6])).unwrap();
    assert!(a == b);
    let mut solid = Shape::solid();
    solid.diagonals = 0b10_1010;
    assert!(make_leaf(at, shaped(solid, [3; 6])).unwrap() == Node::Full([3; 6], MAT_AIR));
    // An empty leaf carries no surface materials.
    let empty = make_leaf(
        at,
        Leaf {
            geometry: LeafGeometry::Empty,
            faces: [4; 6],
            cube_material: MAT_GLASS,
        },
    )
    .unwrap();
    assert!(empty == Node::Empty(MAT_GLASS));
    assert_eq!(empty.leaf().unwrap().faces, [0; 6]);
    // A face without surface takes the first visible face's material.
    let wedge = Shape {
        vertices: {
            let mut v = Shape::solid().vertices;
            v[4][0] = 8;
            v[7][0] = 8;
            v
        },
        diagonals: 0,
    };
    assert!(wedge.is_valid());
    let visible = lattice::visible_faces(wedge);
    let hidden = visible.iter().position(|v| !v).expect("a collapsed face");
    let mut faces = [2; 6];
    faces[hidden] = 7;
    let node = make_leaf(at, shaped(wedge, faces)).unwrap();
    assert_eq!(node.faces(), Some([2; 6]));
    // Shapes with no volume stay refused.
    let mut flat = Shape::solid();
    for p in &mut flat.vertices {
        p[1] = 0;
    }
    assert!(make_leaf(at, shaped(flat, [0; 6])).is_err());
    // Pinched height fields stay legal, as today.
    let pinched = Cell {
        heights: [0, 8, 0, 8],
        ..Cell::solid(0)
    };
    assert!(!pinched.as_shape().is_valid());
    assert!(make_leaf(at, shaped(pinched.as_shape(), [0; 6])).is_ok());
}

#[test]
fn cloning_is_constant_and_writes_copy_only_their_path() {
    let mut rng = Rng::new(99);
    let (tree, _) = random_tree(&mut rng, 40);
    let before = tree.digest();
    let mut edited = tree.clone();
    let at = NodeBox {
        min: tree.root_box().min,
        size: 1,
    };
    edited
        .write(at, &Node::Full([6; 6], MAT_AIR), Parts::All)
        .unwrap();
    assert_eq!(tree.digest(), before);
    assert_ne!(edited.digest(), before);
    // Untouched octants still share their nodes.
    if let (Node::Branch(a), Node::Branch(b)) = (tree.root_node(), edited.root_node()) {
        for i in 1..8 {
            if let (Node::Branch(x), Node::Branch(y)) = (&a[i], &b[i]) {
                assert!(Arc::ptr_eq(x, y));
            }
        }
    }
}

/// Random writes, then undo in reverse, restore the identical tree; redo
/// restores the edited tree.
#[test]
fn random_edits_and_undo_restore_identical_trees() {
    for seed in 0..60 {
        let mut rng = Rng::new(1000 + seed);
        let (mut tree, _) = random_tree(&mut rng, 25);
        let original = tree.clone();
        let mut history = Vec::new();
        let mut writes = Vec::new();
        for _ in 0..20 {
            let at = random_node(&mut rng, &tree, 7);
            let content = match rng.below(5) {
                // Copy a sibling, which makes groups that can merge.
                0 if at.size < tree.root_box().size => {
                    let s = (at.size * 2) as i32;
                    let root = tree.root_box().min;
                    let parent = NodeBox {
                        min: std::array::from_fn(|a| {
                            root[a] + (at.min[a] - root[a]).div_euclid(s) * s
                        }),
                        size: at.size * 2,
                    };
                    let sibling = parent.child(rng.below(8) as usize);
                    match tree.node_at(sibling) {
                        Node::Piece(p) => restrict(p.source, p.faces, p.cube, at),
                        Node::Shaped(s) => restrict(
                            Source {
                                origin: sibling.min,
                                size: sibling.size,
                                shape: s.shape,
                            },
                            s.faces,
                            s.cube,
                            at,
                        ),
                        Node::Branch(_) => AIR,
                        leaf => leaf,
                    }
                }
                1 => {
                    let mut distinct: Vec<(NodeBox, Leaf)> = Vec::new();
                    for _ in 0..3 {
                        let size = (at.size >> rng.below(3)).max(1);
                        let cells = (at.size / size) as i32;
                        let nb = NodeBox {
                            min: std::array::from_fn(|a| {
                                at.min[a] + rng.range(0, cells) * size as i32
                            }),
                            size,
                        };
                        if !distinct
                            .iter()
                            .any(|(o, _)| o.contains(nb) || nb.contains(*o))
                        {
                            distinct.push((nb, random_leaf(&mut rng, nb)));
                        }
                    }
                    subtree(tree.bounds(), tree.root_box(), at, &distinct).unwrap()
                }
                _ => make_leaf(at, random_leaf(&mut rng, at)).unwrap(),
            };
            let parts =
                [Parts::All, Parts::Shape, Parts::Surface, Parts::Cube][rng.below(4) as usize];
            if let Some(replaced) = tree.write(at, &content, parts).unwrap() {
                history.push(replaced);
                writes.push((at, content, parts));
            }
            assert_canonical(tree.root_node(), tree.root_box());
        }
        let edited = tree.clone();
        for replaced in history.iter().rev() {
            tree.write(replaced.at, &replaced.content, Parts::All)
                .unwrap();
            assert_canonical(tree.root_node(), tree.root_box());
        }
        assert_eq!(tree.digest(), original.digest(), "seed {seed}");
        assert!(tree.root_node() == original.root_node());
        for (at, content, parts) in &writes {
            tree.write(*at, content, *parts).unwrap();
        }
        assert_eq!(tree.digest(), edited.digest(), "redo, seed {seed}");
    }
}

#[test]
fn an_edit_beside_a_group_that_merges_undoes_exactly() {
    let bounds = small_bounds();
    let mut tree = Octree::new(bounds).unwrap();
    let root = tree.root_box();
    let parent = NodeBox {
        min: [root.min[0] + 16, root.min[1], root.min[2]],
        size: 16,
    };
    let wedge = Cell {
        heights: [0, 8, 8, 0],
        ..Cell::solid(0)
    }
    .as_shape();
    let shaped = make_leaf(
        parent,
        Leaf {
            geometry: LeafGeometry::Shaped(wedge),
            faces: [2; 6],
            cube_material: MAT_AIR,
        },
    )
    .unwrap();
    tree.write(parent, &shaped, Parts::All).unwrap();
    let start = tree.clone();
    // Carving one eighth splits the wedge; restoring it merges it back.
    let corner = parent.child(1);
    let carve = tree.write(corner, &AIR, Parts::Shape).unwrap().unwrap();
    assert_eq!(carve.at, parent);
    assert!(tree.leaves().len() > 1);
    let piece = start.node_at(corner);
    let restore = tree.write(corner, &piece, Parts::All).unwrap().unwrap();
    assert_eq!(tree.digest(), start.digest());
    tree.write(restore.at, &restore.content, Parts::All)
        .unwrap();
    tree.write(carve.at, &carve.content, Parts::All).unwrap();
    assert_eq!(tree.digest(), start.digest());
    // Completing seven equal full cubes merges them; undo splits them again.
    let mut cubes = Octree::new(bounds).unwrap();
    for i in 1..8 {
        cubes
            .write(parent.child(i), &Node::Full([3; 6], MAT_AIR), Parts::All)
            .unwrap();
    }
    let seven = cubes.clone();
    let fill = cubes
        .write(parent.child(0), &Node::Full([3; 6], MAT_AIR), Parts::All)
        .unwrap()
        .unwrap();
    assert_eq!(cubes.leaves(), vec![(parent, Leaf::full([3; 6]))]);
    cubes.write(fill.at, &fill.content, Parts::All).unwrap();
    assert_eq!(cubes.digest(), seven.digest());
    // Writing what is already there changes nothing.
    assert!(cubes
        .write(parent.child(3), &Node::Full([3; 6], MAT_AIR), Parts::All)
        .unwrap()
        .is_none());
}

#[test]
fn writes_stay_inside_the_world_bounds() {
    let bounds = WorldBounds {
        min: [0, 0, 0],
        max: [3, 3, 3],
    };
    let mut tree = Octree::new(bounds).unwrap();
    assert_eq!(tree.root_box().size, 32);
    let whole = tree.root_box();
    tree.write(whole, &Node::Full([1; 6], MAT_WATER), Parts::All)
        .unwrap();
    let mut volume = 0;
    tree.for_each_leaf(|at, leaf| {
        assert!(bounds.fine_box().contains(at.fine_box()));
        assert_eq!(leaf.geometry, LeafGeometry::Full);
        volume += (at.size as i64).pow(3);
    });
    assert_eq!(volume, 24 * 24 * 24);
    assert!(!tree.intersects_box(Vec3::new(3.1, 0.1, 0.1), Vec3::new(3.5, 0.5, 0.5)));
}

// ---------------------------------------------------------------------------
// Conversion.

fn random_format3(rng: &mut Rng, regions: bool) -> Format3 {
    let bounds = WorldBounds {
        min: [-6, -3, -6],
        max: [6 + rng.range(0, 6), 9, 6],
    };
    let fine = bounds.fine_box();
    let mut out = Format3 {
        bounds,
        ..Default::default()
    };
    if regions {
        // Format 3 cannot store the root, so it is derived from the largest
        // whole region on reload. Keep that region on the bounds' grid, where
        // every aligned leaf derives the same root again.
        let mut taken: Vec<FineBox> = Vec::new();
        for index in 0..rng.range(1, 40) {
            let (size, misaligned) = if index == 0 {
                (64, false)
            } else {
                let misaligned = rng.chance(30);
                (
                    1u32 << rng.below(if misaligned { 6 } else { 7 }),
                    misaligned,
                )
            };
            let s = size as i32;
            let mut origin: [i32; 3] = std::array::from_fn(|a| {
                let cells = ((fine.max[a] - fine.min[a]) / s).max(1);
                fine.min[a] + rng.range(0, cells) * s
            });
            if misaligned {
                for v in &mut origin {
                    *v += rng.range(-(s - 1).min(7), s.min(8));
                }
            }
            let source = FineBox {
                min: origin,
                max: origin.map(|v| v + s),
            };
            let mut clip = source;
            if index > 0 && rng.chance(40) {
                for a in 0..3 {
                    let lo = rng.range(source.min[a], source.max[a]);
                    clip.min[a] = lo;
                    clip.max[a] = rng.range(lo + 1, source.max[a] + 1);
                }
            }
            let Some(clip) = clip.intersection(fine) else {
                continue;
            };
            if taken.iter().any(|t| t.intersection(clip).is_some()) {
                continue;
            }
            taken.push(clip);
            out.regions.push(CellFragment {
                bounds: clip,
                origin,
                size,
                cell: random_cell(rng),
            });
        }
    }
    let mut positions = std::collections::BTreeSet::new();
    let position = |rng: &mut Rng| {
        [
            rng.range(bounds.min[0], bounds.max[0]),
            rng.range(bounds.min[1], bounds.max[1]),
            rng.range(bounds.min[2], bounds.max[2]),
        ]
    };
    for _ in 0..rng.range(0, 14) {
        let pos = position(rng);
        if positions.insert(pos) {
            out.cells.push(CellEdit {
                pos,
                cell: (!regions || rng.chance(80)).then(|| random_cell(rng)),
            });
        }
    }
    for _ in 0..rng.range(0, 10) {
        let pos = position(rng);
        if !positions.insert(pos) {
            continue;
        }
        let root = FineBox::root(pos);
        let mut fragments: Vec<CellFragment> = Vec::new();
        for _ in 0..rng.range(0, 5) {
            let mut b = root;
            for a in 0..3 {
                let lo = rng.range(root.min[a], root.max[a]);
                b.min[a] = lo;
                b.max[a] = rng.range(lo + 1, root.max[a] + 1);
            }
            if fragments.iter().any(|f| f.bounds.intersection(b).is_some()) {
                continue;
            }
            let extent = (0..3).map(|a| b.max[a] - b.min[a]).max().unwrap();
            let size = ((extent as u32).next_power_of_two() << rng.below(3)).min(64);
            let s = size as i32;
            let origin =
                std::array::from_fn(|a| b.max[a] - s + rng.range(0, s - (b.max[a] - b.min[a]) + 1));
            fragments.push(CellFragment {
                bounds: b,
                origin,
                size,
                cell: random_cell(rng),
            });
        }
        if fragments.is_empty() && !regions {
            continue;
        }
        out.roots.push(RootEdit { pos, fragments });
    }
    let mut volumes: Vec<MaterialVolume> = Vec::new();
    for _ in 0..rng.range(0, 8) {
        let mut b = fine;
        for a in 0..3 {
            let lo = rng.range(fine.min[a], fine.max[a] - 1);
            b.min[a] = lo;
            b.max[a] = rng.range(lo + 1, (lo + 40).min(fine.max[a]) + 1);
        }
        if volumes.iter().any(|v| v.bounds.intersection(b).is_some()) {
            continue;
        }
        volumes.push(MaterialVolume {
            bounds: b,
            material: if rng.chance(50) {
                VolumeMaterial::Water
            } else {
                VolumeMaterial::Glass
            },
        });
    }
    out.volumes = volumes;
    out
}

fn old_world(f: &Format3) -> World {
    let mut world = World::from_compact_parts(f.bounds, &f.regions, &f.cells, &f.roots).unwrap();
    world.replace_volumes(&f.volumes).unwrap();
    world
}

#[test]
fn random_worlds_convert_to_format3_and_back_with_equal_digests() {
    for seed in 0..80 {
        let mut rng = Rng::new(5000 + seed);
        let f3 = random_format3(&mut rng, seed % 4 != 0);
        let tree = Octree::from_format3(&f3).unwrap_or_else(|e| panic!("seed {seed}: {e}"));
        assert_canonical(tree.root_node(), tree.root_box());
        let written = tree.to_format3();
        assert!(written.roots.is_empty());
        // The loader from before the octree opens the result.
        let reopened = old_world(&written);
        let again = Octree::from_format3(&written).unwrap();
        assert_eq!(again.digest(), tree.digest(), "seed {seed}");
        // Input order does not matter.
        let mut shuffled = f3.clone();
        rng.shuffle(&mut shuffled.regions);
        rng.shuffle(&mut shuffled.cells);
        rng.shuffle(&mut shuffled.roots);
        rng.shuffle(&mut shuffled.volumes);
        assert_eq!(
            Octree::from_format3(&shuffled).unwrap().digest(),
            tree.digest()
        );
        // Today's world normalizes roots its own way; its snapshot holds the
        // same content, and so does the world reopened from the output.
        let world = old_world(&f3);
        assert_eq!(
            Octree::from_world(&world).unwrap().digest(),
            tree.digest(),
            "seed {seed}"
        );
        assert_eq!(
            Octree::from_world(&reopened).unwrap().digest(),
            tree.digest()
        );
    }
}

#[test]
fn conversion_validates_like_the_old_loader() {
    let bounds = WorldBounds::default();
    let region = |origin: [i32; 3], size: u32| CellFragment {
        bounds: FineBox {
            min: origin,
            max: origin.map(|v| v + size as i32),
        },
        origin,
        size,
        cell: Cell::solid(1),
    };
    let parts = |regions: Vec<CellFragment>| Format3 {
        bounds,
        regions,
        ..Default::default()
    };
    assert!(Octree::from_format3(&parts(vec![region([0; 3], 8), region([0; 3], 8)])).is_err());
    assert!(Octree::from_format3(&parts(vec![region([0; 3], 8), region([4, 0, 0], 8)])).is_err());
    assert!(Octree::from_format3(&parts(vec![region([8192 * 8, 0, 0], 8)])).is_err());
    let mut bad = region([0; 3], 8);
    bad.cell.material = 9;
    assert!(Octree::from_format3(&parts(vec![bad])).is_err());
    let cell = |material| CellEdit {
        pos: [0; 3],
        cell: Some(Cell::solid(material)),
    };
    let repeated = Format3 {
        bounds,
        cells: vec![cell(0), cell(1)],
        ..Default::default()
    };
    assert!(Octree::from_format3(&repeated).is_err());
    let volume = |min: [i32; 3], max: [i32; 3], material| MaterialVolume {
        bounds: FineBox { min, max },
        material,
    };
    let overlapping_volumes = Format3 {
        bounds,
        volumes: vec![
            volume([0; 3], [8; 3], VolumeMaterial::Water),
            volume([4; 3], [12; 3], VolumeMaterial::Glass),
        ],
        ..Default::default()
    };
    assert!(Octree::from_format3(&overlapping_volumes).is_err());
    // A misaligned slab across a large world would explode into pieces; the
    // conversion limit refuses it instead of exhausting memory.
    let huge = Format3 {
        bounds: WorldBounds {
            min: [-4096, 0, -4096],
            max: [4096, 64, 4096],
        },
        volumes: vec![volume(
            [-32767, 3, -32767],
            [32767, 4, 32767],
            VolumeMaterial::Glass,
        )],
        ..Default::default()
    };
    assert!(Octree::from_format3(&huge).is_err());
}

#[test]
fn imported_roots_follow_the_largest_region() {
    // A map whose octree sits 24 m off a multiple of 64 m keeps its leaves.
    let bounds = WorldBounds {
        min: [-64, -40, -64],
        max: [64, 88, 64],
    };
    let region = CellFragment {
        bounds: FineBox {
            min: [-512, -320, -512],
            max: [0, 192, 0],
        },
        origin: [-512, -320, -512],
        size: 512,
        cell: Cell::solid(0),
    };
    let tree = Octree::from_format3(&Format3 {
        bounds,
        regions: vec![region.clone()],
        ..Default::default()
    })
    .unwrap();
    assert_eq!(tree.root_box().min, [-512, -320, -512]);
    assert_eq!(tree.leaves().len(), 1);
    assert_eq!(tree.regions(), vec![region.clone()]);
    // Bounds that start off the map's grid, as after an entity widened them,
    // put the root on the region's grid below them.
    let wide = WorldBounds {
        min: [-65, -40, -64],
        max: [64, 88, 64],
    };
    let tree = Octree::from_format3(&Format3 {
        bounds: wide,
        regions: vec![region],
        ..Default::default()
    })
    .unwrap();
    assert_eq!(tree.root_box().min[0], -1024);
    assert_eq!(tree.leaves().len(), 1);
}

#[test]
fn format3_writer_uses_cells_heights_and_boxes() {
    let mut world = World::empty();
    world.set([0, 0, 0], Some(Cell::solid(3)));
    world.set(
        [1, 0, 0],
        Some(Cell {
            heights: [2, 8, 6, 3],
            diagonal: true,
            ..Cell::solid(1)
        }),
    );
    let mut shape = Shape::solid();
    shape.vertices[4][0] = 2;
    world.set(
        [2, 0, 0],
        Some(Cell {
            shape: Some(shape),
            ..Cell::solid(2)
        }),
    );
    world
        .replace_volumes(&[MaterialVolume {
            bounds: FineBox {
                min: [0, 8, 0],
                max: [24, 16, 8],
            },
            material: VolumeMaterial::Water,
        }])
        .unwrap();
    let tree = Octree::from_world(&world).unwrap();
    let f3 = tree.to_format3();
    assert!(f3.regions.is_empty());
    assert_eq!(f3.volumes, world.volumes());
    assert_eq!(tree.get([1, 0, 0]), world.get([1, 0, 0]));
    // The shape's meaningless diagonals take their fixed values.
    let cells = world.cells();
    assert_eq!(f3.cells[..2], cells[..2]);
    let normal = Cell {
        shape: Some(lattice::normalize_shape(shape)),
        ..cells[2].cell.unwrap()
    };
    assert_eq!(f3.cells[2].cell, Some(normal));
    assert_eq!(Octree::from_format3(&f3).unwrap().digest(), tree.digest());
    // Eight equal cells merge into a two-metre leaf, written as a region.
    let mut cubes = World::empty();
    for x in 0..2 {
        for y in 0..2 {
            for z in 0..2 {
                cubes.set([x, y, z], Some(Cell::solid(4)));
            }
        }
    }
    let tree = Octree::from_world(&cubes).unwrap();
    assert_eq!(tree.leaves().len(), 1);
    assert_eq!(tree.region_count(), 1);
    assert!(tree.cells().is_empty());
    assert_eq!(tree.get([1, 1, 1]), Some(Cell::solid(4)));
    let reopened = old_world(&tree.to_format3());
    for x in 0..2 {
        assert!(reopened.intersects_box(
            Vec3::new(x as f32 + 0.2, 0.2, 0.2),
            Vec3::new(x as f32 + 0.8, 1.8, 1.8)
        ));
    }
}

// ---------------------------------------------------------------------------
// Queries against today's world.

fn edge_box(world: &World, min: Vec3, max: Vec3) -> bool {
    let d = Vec3::splat(2.0e-4);
    world.intersects_box(min - d, max + d) != world.intersects_box(min + d, max - d)
}

fn compare_queries(world: &World, tree: &Octree, rng: &mut Rng, queries: usize) -> [usize; 4] {
    let bounds = world.bounds();
    let lo = bounds.world_min();
    let span = bounds.world_max() - lo;
    let point = |rng: &mut Rng| lo + Vec3::new(rng.unit(), rng.unit(), rng.unit()) * span;
    let body = crate::physics::Body::new(0.3, 1.75);
    let mut mismatches = [0; 4];
    for _ in 0..queries {
        // Box overlap.
        let c = point(rng);
        let h = Vec3::new(rng.unit(), rng.unit(), rng.unit()) * 0.6 + Vec3::splat(0.01);
        let (min, max) = (c - h, c + h);
        if world.intersects_box(min, max) != tree.intersects_box(min, max) {
            assert!(edge_box(world, min, max), "box {min:?} {max:?}");
            mismatches[0] += 1;
        }
        assert_eq!(
            world.intersects_solid_volume(min, max),
            tree.intersects_solid_volume(min, max)
        );
        // Point material. Today's terrain test, a box shrunk by its contact
        // tolerance, misses solids near the origin and reports water inside
        // them; the tree tests the point.
        let p = point(rng);
        let (old, new) = (world.volume_material_at(p), tree.volume_material_at(p));
        if old != new {
            let e = Vec3::splat(1.0e-3);
            let inside = new.is_none() && tree.contains_point(p);
            assert!(inside || edge_box(world, p - e, p + e), "material at {p:?}");
            mismatches[1] += 1;
        }
        // Rays.
        let d = Vec3::new(rng.unit() - 0.5, rng.unit() - 0.5, rng.unit() - 0.5);
        let o = point(rng);
        let a = world.raycast(o, d, 30.0);
        let b = tree.raycast(o, d, 30.0);
        match (a, b) {
            (None, None) => {}
            (Some(a), Some(b)) if (a.distance - b.distance).abs() < 1.0e-3 => {
                if a.face != b.face || a.pos != b.pos {
                    mismatches[2] += 1;
                }
            }
            (a, b) => {
                // Rays grazing an edge or a face within tolerance may differ,
                // and today's regions can hold zero-thickness sheets where a
                // clip box only touches its source's solid.
                let e = Vec3::splat(2.0e-3);
                let near = |h: Option<RayHit>| {
                    h.is_some_and(|h| edge_box(world, h.point - e, h.point + e))
                };
                let sheet = a.is_some_and(|h| {
                    !world.intersects_box(h.point - e, h.point + e)
                        && b.is_none_or(|b| b.distance > h.distance)
                });
                assert!(
                    near(a) || near(b) || sheet,
                    "ray {o:?} {d:?}: {a:?} vs {b:?}"
                );
                mismatches[2] += 1;
            }
        }
        let a = world.raycast_volumes(o, d, 30.0);
        let b = tree.raycast_volumes(o, d, 30.0);
        assert_eq!(
            a.map(|h| h.distance),
            b.map(|h| h.distance),
            "volume ray {o:?} {d:?}"
        );
        let a = world.raycast_solid_volumes(o, d, 30.0);
        let b = tree.raycast_solid_volumes(o, d, 30.0);
        assert_eq!(a.map(|h| h.distance), b.map(|h| h.distance));
        // Support and ceiling where a body can stand: inside solid, today's
        // separate cells and regions report their internal faces, which
        // merged leaves no longer have.
        use crate::physics::Space;
        let feet = point(rng);
        if !world.free_at(feet, body) {
            continue;
        }
        let s = world.support_between(feet, feet.y - 0.6, feet.y, body);
        let t = tree.support_between(feet, feet.y - 0.6, feet.y, body);
        if s != t {
            assert!(
                s.zip(t).is_some_and(|(s, t)| (s - t).abs() < 1.0e-4),
                "support at {feet:?}: {s:?} vs {t:?}"
            );
            mismatches[3] += 1;
        }
        let s = world.first_ceiling(feet, feet.y + 1.75, feet.y + 2.3, body);
        let t = tree.first_ceiling(feet, feet.y + 1.75, feet.y + 2.3, body);
        if s != t {
            assert!(
                s.zip(t).is_some_and(|(s, t)| (s - t).abs() < 1.0e-4),
                "ceiling at {feet:?}: {s:?} vs {t:?}"
            );
            mismatches[3] += 1;
        }
    }
    mismatches
}

#[test]
fn queries_agree_with_todays_world_on_random_worlds() {
    let mut totals = [0; 4];
    for seed in 0..40 {
        let mut rng = Rng::new(9000 + seed);
        let f3 = random_format3(&mut rng, seed % 3 != 0);
        let world = old_world(&f3);
        let tree = Octree::from_format3(&f3).unwrap();
        let m = compare_queries(&world, &tree, &mut rng, 300);
        for i in 0..4 {
            totals[i] += m[i];
        }
    }
    eprintln!("random worlds: box, material, ray, support mismatches {totals:?}");
    // Differences are confined to grazing contacts and height-field sides.
    assert!(totals.iter().all(|&m| m < 60), "{totals:?}");
}

#[test]
fn arena_queries_agree_with_todays_world() {
    let world = World::arena();
    let tree = Octree::from_world(&world).unwrap();
    let mut rng = Rng::new(77);
    let m = compare_queries(&world, &tree, &mut rng, 4000);
    eprintln!("arena: box, material, ray, support mismatches {m:?}");
    assert!(m.iter().all(|&m| m < 60), "{m:?}");
}

#[test]
fn empty_override_is_a_hole_and_large_coordinates_keep_detail() {
    let bounds = WorldBounds {
        min: [-2048, 0, -2048],
        max: [2048, 4096, 2048],
    };
    let region = CellFragment {
        bounds: FineBox {
            min: [-16384, 0, -16384],
            max: [16384, 32768, 16384],
        },
        origin: [-16384, 0, -16384],
        size: 32768,
        cell: Cell::solid(1),
    };
    let f3 = Format3 {
        bounds,
        regions: vec![region],
        roots: vec![RootEdit {
            pos: [1000, 2000, 1000],
            fragments: vec![],
        }],
        ..Default::default()
    };
    let tree = Octree::from_format3(&f3).unwrap();
    assert!(!tree.intersects_box(
        Vec3::new(1000.1, 2000.1, 1000.1),
        Vec3::new(1000.9, 2000.9, 1000.9)
    ));
    assert!(tree.intersects_box(
        Vec3::new(1001.1, 2000.1, 1000.1),
        Vec3::new(1001.9, 2000.9, 1000.9)
    ));
    let hit = tree
        .raycast(Vec3::new(1000.5, 2000.5, 1000.5), Vec3::X, 4.0)
        .unwrap();
    assert!((hit.distance - 0.5).abs() < 0.001);
    assert_eq!(hit.face, 0);
    assert_eq!(hit.pos, [1001, 2000, 1000]);
    assert!(tree.root_fragments([1000, 2000, 1000]).is_empty());
    // The hole costs a few leaves per level, not a world of fragments.
    assert!(tree.stats().leaves() < 200, "{:?}", tree.stats());
    let world = old_world(&f3);
    assert_eq!(Octree::from_world(&world).unwrap().digest(), tree.digest());
    // A whole-world selection is bounded by what is stored.
    assert_eq!(
        tree.fragments_in(bounds.fine_box()).len(),
        tree.stats().leaves()
    );
}

#[test]
fn volume_rays_skip_joins_and_report_the_cube_around_a_hit() {
    let mut world = World::empty();
    world
        .replace_volumes(&[
            MaterialVolume {
                bounds: FineBox {
                    min: [0, 8, 0],
                    max: [24, 16, 8],
                },
                material: VolumeMaterial::Water,
            },
            MaterialVolume {
                bounds: FineBox {
                    min: [24, 8, 0],
                    max: [32, 16, 8],
                },
                material: VolumeMaterial::Glass,
            },
        ])
        .unwrap();
    let tree = Octree::from_world(&world).unwrap();
    let origin = Vec3::new(-1.0, 1.5, 0.5);
    let a = world.raycast_volumes(origin, Vec3::X, 10.0).unwrap();
    let b = tree.raycast_volumes(origin, Vec3::X, 10.0).unwrap();
    assert_eq!(a.distance, b.distance);
    assert_eq!(a.face, b.face);
    assert_eq!(a.pos, b.pos);
    assert!(a.bounds.contains(b.bounds));
    // From inside the water the ray reaches the glass, skipping the joins
    // between water cubes.
    let inside = Vec3::new(0.5, 1.5, 0.5);
    let a = world.raycast_volumes(inside, Vec3::X, 10.0).unwrap();
    let b = tree.raycast_volumes(inside, Vec3::X, 10.0).unwrap();
    assert_eq!(a.distance, b.distance);
    assert_eq!(a.face, b.face);
    let glass = tree.raycast_solid_volumes(inside, Vec3::X, 10.0).unwrap();
    assert!((glass.distance - 2.5).abs() < 1.0e-6);
}

// ---------------------------------------------------------------------------
// Movement on the tree.

#[derive(Default)]
struct Track {
    ticks: usize,
    exact: usize,
    largest: f32,
}

fn run_both(world: &World, start: Vec3, script: &[(InputFrame, usize)], track: &mut Track) {
    let tree = Octree::from_world(world).unwrap();
    let profile = MovementProfile::ARENA;
    let mut a = Player::new(1, "old".into());
    a.position = start;
    let mut b = a.clone();
    let mut seq = 0;
    for &(input, ticks) in script {
        for _ in 0..ticks {
            seq += 1;
            let input = InputFrame { seq, ..input };
            crate::simulate_player(world, &mut a, &input, FIXED_DT);
            crate::physics::simulate_in(&tree, &mut b, &input, FIXED_DT, &profile);
            track.ticks += 1;
            if a.position == b.position && a.velocity == b.velocity {
                track.exact += 1;
            }
            let difference = (a.position - b.position)
                .abs()
                .max_element()
                .max((a.velocity - b.velocity).abs().max_element());
            track.largest = track.largest.max(difference);
            assert_eq!(a.grounded, b.grounded, "tick {seq} at {:?}", a.position);
            assert!(difference <= 2.0e-3, "tick {seq}: {a:?} vs {b:?}");
        }
    }
}

#[test]
fn movement_on_the_tree_stays_within_the_golden_tolerance() {
    let input = InputFrame::default;
    let strafe = |v| InputFrame {
        strafe: v,
        ..input()
    };
    let forward = |v| InputFrame {
        forward: v,
        ..input()
    };
    let jump = InputFrame {
        jump: true,
        ..input()
    };
    let mut floor = World::empty();
    for x in -20..20 {
        for z in -20..20 {
            floor.set([x, 0, z], Some(Cell::solid(0)));
        }
    }
    let mut track = Track::default();
    let start = Vec3::new(0.5, 1.01, 0.5);
    run_both(
        &floor,
        start,
        &[
            (input(), 20),
            (forward(1.0), 45),
            (strafe(-1.0), 30),
            (jump, 1),
            (input(), 70),
        ],
        &mut track,
    );
    for explicit in [false, true] {
        let mut ramps = floor.clone();
        for step in 0..8 {
            let x = 2 + step;
            let level = 1 + step / 2;
            let low = if step % 2 == 0 { 0 } else { 4 };
            for z in -1..2 {
                for y in 1..level {
                    ramps.set([x, y, z], Some(Cell::solid(0)));
                }
                let legacy = Cell {
                    heights: [low, low + 4, low + 4, low],
                    ..Cell::solid(3)
                };
                let cell = if explicit {
                    Cell {
                        shape: Some(legacy.as_shape()),
                        ..legacy
                    }
                } else {
                    legacy
                };
                ramps.set([x, level, z], Some(cell));
            }
        }
        run_both(
            &ramps,
            start,
            &[
                (strafe(1.0), 85),
                (input(), 20),
                (strafe(-1.0), 95),
                (input(), 30),
            ],
            &mut track,
        );
    }
    let mut ceiling = floor.clone();
    for x in -1..2 {
        for z in -1..2 {
            ceiling.set(
                [x, 3, z],
                Some(Cell {
                    heights: [1, 4, 8, 4],
                    ..Cell::solid(0)
                }),
            );
        }
    }
    run_both(&ceiling, start, &[(jump, 1), (input(), 70)], &mut track);
    let mut wall = floor.clone();
    for y in 1..5 {
        for z in -20..20 {
            wall.set([3, y, z], Some(Cell::solid(1)));
        }
        wall.set([2, y, -3], Some(Cell::solid(1)));
    }
    let diagonal = InputFrame {
        strafe: 1.0,
        forward: 1.0,
        ..input()
    };
    run_both(
        &wall,
        start,
        &[(strafe(1.0), 90), (diagonal, 30), (strafe(1.0), 30)],
        &mut track,
    );
    let mut pool = floor.clone();
    let water = |min: [i32; 3], max: [i32; 3]| MaterialVolume {
        bounds: FineBox { min, max },
        material: VolumeMaterial::Water,
    };
    pool.replace_volumes(&[
        water([-64, 8, -64], [0, 32, 64]),
        water([0, 8, -64], [64, 13, 64]),
    ])
    .unwrap();
    run_both(
        &pool,
        Vec3::new(-6.0, 6.0, 0.5),
        &[
            (input(), 40),
            (jump, 40),
            (strafe(1.0), 90),
            (jump, 12),
            (forward(-1.0), 40),
        ],
        &mut track,
    );
    let mut glass = World::empty();
    let pane = |min: [i32; 3], max: [i32; 3]| MaterialVolume {
        bounds: FineBox { min, max },
        material: VolumeMaterial::Glass,
    };
    glass
        .replace_volumes(&[
            pane([-64, 0, -64], [64, 8, 64]),
            pane([16, 8, -16], [17, 48, 16]),
            pane([-8, 32, -8], [8, 33, 8]),
        ])
        .unwrap();
    run_both(
        &glass,
        Vec3::new(0.0, 3.0, 0.0),
        &[(input(), 60), (jump, 30), (strafe(1.0), 120)],
        &mut track,
    );
    let arena = World::arena();
    run_both(
        &arena,
        Vec3::new(-14.5, 1.01, -17.5),
        &[(forward(-1.0), 100)],
        &mut track,
    );
    run_both(
        &arena,
        Vec3::new(15.0, 1.01, 16.5),
        &[(forward(1.0), 100)],
        &mut track,
    );
    run_both(
        &arena,
        Vec3::new(-14.0, 5.01, 0.0),
        &[(strafe(1.0), 190)],
        &mut track,
    );
    eprintln!(
        "movement on the tree: {} of {} ticks bit for bit, largest difference {}",
        track.exact, track.ticks, track.largest
    );
}

/// Builds the content of fragments inside a clip box as a tree, so outputs
/// that split the same solid differently compare equal.
fn content(
    bounds: WorldBounds,
    root: NodeBox,
    clip: FineBox,
    fragments: &[CellFragment],
) -> [u8; 16] {
    let mut tree = Octree::with_root(bounds, root).unwrap();
    for f in fragments {
        let Some(b) = f.bounds.intersection(clip) else {
            continue;
        };
        let source = Source {
            origin: f.origin,
            size: f.size,
            shape: if f.cell.is_full() {
                Shape::solid()
            } else {
                f.cell.as_shape()
            },
        };
        let faces = std::array::from_fn(|i| f.cell.material_for_face(i as u8));
        tree.insert_source(b, source, faces);
    }
    tree.digest()
}

#[test]
fn fragment_queries_hold_todays_content() {
    for seed in 0..30 {
        let mut rng = Rng::new(7000 + seed);
        let f3 = random_format3(&mut rng, seed % 3 != 0);
        let world = old_world(&f3);
        let tree = Octree::from_format3(&f3).unwrap();
        let (bounds, root) = (tree.bounds(), tree.root_box());
        for _ in 0..40 {
            let pos = [
                rng.range(bounds.min[0], bounds.max[0]),
                rng.range(bounds.min[1], bounds.max[1]),
                rng.range(bounds.min[2], bounds.max[2]),
            ];
            let metre = FineBox::root(pos);
            let a = world.root_fragments(pos);
            let b = tree.root_fragments(pos);
            assert!(b.iter().all(|f| metre.contains(f.bounds) && f.is_valid()));
            assert_eq!(
                content(bounds, root, metre, &a),
                content(bounds, root, metre, &b),
                "seed {seed} root {pos:?}: {a:?} vs {b:?}"
            );
            // Writing a metre's fragments back changes nothing.
            let mut again = tree.clone();
            again.clear_geometry(metre);
            for f in &b {
                let source = super::checked_source(Source {
                    origin: f.origin,
                    size: f.size,
                    shape: if f.cell.is_full() {
                        Shape::solid()
                    } else {
                        f.cell.as_shape()
                    },
                })
                .unwrap();
                let faces = std::array::from_fn(|i| f.cell.material_for_face(i as u8));
                again.insert_source(f.bounds, source, faces);
            }
            assert_eq!(again.digest(), tree.digest(), "seed {seed} rewrite {pos:?}");
            let get = tree.get(pos);
            if let Some(cell) = world.get(pos) {
                assert_eq!(get.map(|c| c.is_full()), Some(cell.is_full()));
            }
            // A box of a few metres: the same metres, with the same content.
            let mut query = metre;
            for a in 0..3 {
                query.max[a] += 8 * rng.range(0, 3);
            }
            let old = world.try_roots_in(query, 64).unwrap();
            let new = tree.try_roots_in(query, 64).unwrap();
            assert_eq!(
                old.iter().map(|r| r.pos).collect::<Vec<_>>(),
                new.iter().map(|r| r.pos).collect::<Vec<_>>()
            );
            let fragments_old = world.fragments_in(query);
            let fragments_new = tree.fragments_in(query);
            assert_eq!(
                content(bounds, root, query, &fragments_old),
                content(bounds, root, query, &fragments_new)
            );
        }
        // Regions and cells hold the whole world.
        let whole = bounds.fine_box();
        let mut parts = tree.regions();
        parts.extend(
            tree.cells()
                .into_iter()
                .map(|c| CellFragment::from_cell(c.pos, c.cell.unwrap())),
        );
        assert_eq!(content(bounds, root, whole, &parts), {
            let mut geometry = tree.clone();
            geometry.set_cube_material(whole, MAT_AIR);
            geometry.digest()
        });
        assert_eq!(tree.region_count(), tree.regions().len());
        let detailed = tree.detailed_roots();
        assert!(detailed
            .iter()
            .all(|r| r.fragments == tree.root_fragments(r.pos)));
    }
}

#[test]
fn every_kind_of_saved_part_converts_and_round_trips() {
    let bounds = WorldBounds::default();
    let wedge = Cell {
        heights: [0, 8, 8, 0],
        ..Cell::solid(2)
    };
    let region = |origin: [i32; 3], size: u32, clip: Option<FineBox>, cell| CellFragment {
        bounds: clip.unwrap_or(FineBox {
            min: origin,
            max: origin.map(|v| v + size as i32),
        }),
        origin,
        size,
        cell,
    };
    let mut cases = vec![
        // A whole aligned region, and the arena.
        Format3 {
            bounds,
            regions: vec![region([0, 0, 0], 64, None, Cell::solid(1))],
            ..Default::default()
        },
        Octree::from_world(&World::arena()).unwrap().to_format3(),
    ];
    // A partial region, a misaligned one, and roots that override them:
    // detailed, single-cell and empty.
    let partial = FineBox {
        min: [8, 0, 8],
        max: [40, 13, 30],
    };
    cases.push(Format3 {
        bounds,
        regions: vec![
            // The largest whole region sits on the bounds' grid and anchors
            // the root there; the misaligned one becomes exact pieces.
            region([128, 0, 128], 64, None, Cell::solid(1)),
            region([0, 0, 0], 64, Some(partial), wedge),
            region([67, 3, 5], 16, None, wedge),
        ],
        cells: vec![
            CellEdit {
                pos: [2, 0, 2],
                cell: None,
            },
            CellEdit {
                pos: [3, 0, 2],
                cell: Some(Cell::solid(4)),
            },
        ],
        roots: vec![
            RootEdit {
                pos: [1, 1, 1],
                fragments: vec![],
            },
            RootEdit {
                pos: [2, 1, 1],
                fragments: vec![
                    region(
                        [0, 0, 0],
                        64,
                        Some(FineBox {
                            min: [16, 8, 8],
                            max: [20, 16, 16],
                        }),
                        wedge,
                    ),
                    region(
                        [20, 8, 8],
                        1,
                        Some(FineBox {
                            min: [20, 8, 8],
                            max: [21, 9, 9],
                        }),
                        Cell::solid(5),
                    ),
                ],
            },
        ],
        volumes: vec![
            MaterialVolume {
                bounds: FineBox {
                    min: [3, 1, 2],
                    max: [45, 9, 33],
                },
                material: VolumeMaterial::Water,
            },
            MaterialVolume {
                bounds: FineBox {
                    min: [70, 0, 0],
                    max: [71, 30, 40],
                },
                material: VolumeMaterial::Glass,
            },
        ],
    });
    let mut pieces = 0;
    for f3 in cases {
        let tree = Octree::from_format3(&f3).unwrap();
        pieces += tree.stats().piece_leaves;
        assert_eq!(tree.root_box().min, f3.bounds.fine_box().min);
        assert_canonical(tree.root_node(), tree.root_box());
        let written = tree.to_format3();
        let reopened = old_world(&written);
        assert_eq!(
            Octree::from_format3(&written).unwrap().digest(),
            tree.digest()
        );
        assert_eq!(
            Octree::from_world(&reopened).unwrap().digest(),
            tree.digest()
        );
        assert_eq!(
            Octree::from_world(&old_world(&f3)).unwrap().digest(),
            tree.digest()
        );
    }
    assert!(pieces > 0);
}

#[test]
fn height_field_and_shape_forms_build_the_same_tree() {
    for heights in [
        [0, 4, 4, 0],
        [4, 8, 8, 4],
        [1, 8, 3, 6],
        [0, 8, 0, 8],
        [2; 4],
    ] {
        for diagonal in [false, true] {
            let legacy = Cell {
                heights,
                diagonal,
                ..Cell::solid(3)
            };
            let explicit = Cell {
                shape: Some(legacy.as_shape()),
                ..legacy
            };
            let tree = |cell: Cell| {
                let mut world = World::empty();
                world.set([0, 0, 0], Some(cell));
                Octree::from_world(&world).unwrap()
            };
            if explicit.is_valid() {
                assert_eq!(tree(legacy).digest(), tree(explicit).digest());
            }
            // The writer gives the height-field form back.
            let written = tree(legacy).to_format3();
            assert_eq!(written.cells[0].cell.unwrap().shape, None);
            assert_eq!(written.cells[0].cell.unwrap().heights, heights);
        }
    }
}

#[test]
fn leaves_in_octree_order_skip_the_sort() {
    let mut rng = Rng::new(4242);
    let (tree, _) = random_tree(&mut rng, 40);
    let mut builder = OctreeBuilder::new(tree.bounds(), tree.root_box()).unwrap();
    for (at, leaf) in tree.leaves() {
        builder.push(at, leaf).unwrap();
    }
    assert!(builder.ordered);
    assert_eq!(builder.finish().unwrap().digest(), tree.digest());
}

#[test]
fn the_digest_encoding_is_stable() {
    // Changing how trees hash changes this value; do it only on purpose.
    let tree = Octree::from_world(&World::arena()).unwrap();
    let hex: String = tree.digest().iter().map(|b| format!("{b:02x}")).collect();
    assert_eq!(hex, "b9eba07a8acb839e6932ca18fe99869c");
}
