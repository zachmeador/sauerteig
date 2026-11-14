use super::format3::subtree;
use super::*;
use crate::{Cell, CellEdit, CellFragment, MaterialVolume, RootEdit, Vec3, World};

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

/// A world loaded from format 3 as the server and clients load one.
pub(crate) fn world_of(f: &Format3) -> World {
    let mut world = World::from_compact_parts(f.bounds, &f.regions, &f.cells, &f.roots).unwrap();
    world.replace_volumes(&f.volumes).unwrap();
    world
}

/// The checks the loader from before the octree made on format 3: at most
/// 400,000 valid regions inside the bounds that do not overlap, valid cells
/// and roots, each metre once, and valid boxes. Session 1 opened every
/// library copy's output in that loader itself.
pub(crate) fn old_loader_accepts(f: &Format3) -> Result<(), String> {
    let fine = f.bounds.fine_box();
    if !f.bounds.is_valid() || f.regions.len() > 400_000 {
        return Err("bounds or region count".into());
    }
    let mut boxes: Vec<FineBox> = Vec::with_capacity(f.regions.len());
    for region in &f.regions {
        if !region.is_valid() || !fine.contains(region.bounds) {
            return Err(format!("invalid region {region:?}"));
        }
        boxes.push(region.bounds);
    }
    boxes.sort_by_key(|b| b.min[0]);
    for (i, a) in boxes.iter().enumerate() {
        for b in &boxes[i + 1..] {
            if b.min[0] >= a.max[0] {
                break;
            }
            if a.intersection(*b).is_some() {
                return Err(format!("regions overlap: {a:?} {b:?}"));
            }
        }
    }
    let mut seen = std::collections::BTreeSet::new();
    for cell in &f.cells {
        if !f.bounds.contains_pos(cell.pos)
            || cell.cell.is_some_and(|c| !c.is_valid())
            || !seen.insert(cell.pos)
        {
            return Err(format!("invalid cell {cell:?}"));
        }
    }
    for root in &f.roots {
        if !seen.insert(root.pos) || root.fragments.len() > 512 {
            return Err(format!("invalid root {:?}", root.pos));
        }
    }
    crate::validate_root_edits(&f.roots)?;
    crate::validate_material_volumes(&f.volumes, fine)
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
        // The loader from before the octree would open the result.
        old_loader_accepts(&written).unwrap_or_else(|e| panic!("seed {seed}: {e}"));
        let reopened = world_of(&written);
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
        // A world loaded as the server loads one, its boxes written as cube
        // material, holds the same tree, and so does the world reopened from
        // the output.
        assert_eq!(world_of(&f3).digest(), tree.digest(), "seed {seed}");
        assert_eq!(reopened.digest(), tree.digest());
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
    let reopened = world_of(&tree.to_format3());
    for x in 0..2 {
        assert!(reopened.intersects_box(
            Vec3::new(x as f32 + 0.2, 0.2, 0.2),
            Vec3::new(x as f32 + 0.8, 1.8, 1.8)
        ));
    }
}

// ---------------------------------------------------------------------------
// Queries.

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
    assert_eq!(world_of(&f3).digest(), tree.digest());
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
    let origin = Vec3::new(-1.0, 1.5, 0.5);
    let hit = world.raycast_volumes(origin, Vec3::X, 10.0).unwrap();
    assert!((hit.distance - 1.0).abs() < 1.0e-6);
    assert_eq!(hit.face, 0);
    assert_eq!(hit.pos, [0, 1, 0]);
    // The hit's bounds are the cube around it, inside the water's box.
    assert!(world.volumes()[0].bounds.contains(hit.bounds));
    // From inside the water the ray reaches the glass, skipping the joins
    // between water cubes.
    let inside = Vec3::new(0.5, 1.5, 0.5);
    let hit = world.raycast_volumes(inside, Vec3::X, 10.0).unwrap();
    assert!((hit.distance - 2.5).abs() < 1.0e-6);
    // Water ends where glass begins; the glass's face is reported.
    assert_eq!(hit.face, 0);
    let glass = world.raycast_solid_volumes(inside, Vec3::X, 10.0).unwrap();
    assert!((glass.distance - 2.5).abs() < 1.0e-6);
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

/// The tree's own geometry inside a clip box, built from its leaves without
/// the queries under test.
fn stored_content(tree: &Octree, clip: FineBox) -> [u8; 16] {
    let mut copy = Octree::with_root(tree.bounds(), tree.root_box()).unwrap();
    tree.for_each_leaf(|at, leaf| {
        let source = match leaf.geometry {
            LeafGeometry::Empty => return,
            LeafGeometry::Full => Source {
                origin: at.min,
                size: at.size,
                shape: Shape::solid(),
            },
            LeafGeometry::Shaped(shape) => Source {
                origin: at.min,
                size: at.size,
                shape,
            },
            LeafGeometry::Piece(source) => source,
        };
        if let Some(b) = at.fine_box().intersection(clip) {
            copy.insert_source(b, source, leaf.faces);
        }
    });
    copy.digest()
}

#[test]
fn fragment_queries_hold_the_trees_content() {
    for seed in 0..30 {
        let mut rng = Rng::new(7000 + seed);
        let f3 = random_format3(&mut rng, seed % 3 != 0);
        let tree = Octree::from_format3(&f3).unwrap();
        let (bounds, root) = (tree.bounds(), tree.root_box());
        for _ in 0..40 {
            let pos = [
                rng.range(bounds.min[0], bounds.max[0]),
                rng.range(bounds.min[1], bounds.max[1]),
                rng.range(bounds.min[2], bounds.max[2]),
            ];
            let metre = FineBox::root(pos);
            let b = tree.root_fragments(pos);
            assert!(b.iter().all(|f| metre.contains(f.bounds) && f.is_valid()));
            assert_eq!(
                content(bounds, root, metre, &b),
                stored_content(&tree, metre),
                "seed {seed} root {pos:?}: {b:?}"
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
            if let Some(cell) = tree.get(pos) {
                assert_eq!(
                    content(bounds, root, metre, &[CellFragment::from_cell(pos, cell)]),
                    stored_content(&tree, metre)
                );
            }
            // A box of a few metres: every metre holding geometry, with its
            // content.
            let mut query = metre;
            for a in 0..3 {
                query.max[a] += 8 * rng.range(0, 3);
            }
            let roots = tree.try_roots_in(query, 64).unwrap();
            let mut expected = Vec::new();
            for x in query.min[0] / 8..query.max[0] / 8 {
                for y in query.min[1] / 8..query.max[1] / 8 {
                    for z in query.min[2] / 8..query.max[2] / 8 {
                        if !tree.root_fragments([x, y, z]).is_empty() {
                            expected.push([x, y, z]);
                        }
                    }
                }
            }
            expected.sort();
            assert_eq!(roots.iter().map(|r| r.pos).collect::<Vec<_>>(), expected);
            assert_eq!(
                content(bounds, root, query, &tree.fragments_in(query)),
                stored_content(&tree, query)
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
        old_loader_accepts(&written).unwrap();
        assert_eq!(
            Octree::from_format3(&written).unwrap().digest(),
            tree.digest()
        );
        assert_eq!(world_of(&written).digest(), tree.digest());
        assert_eq!(world_of(&f3).digest(), tree.digest());
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

// ---------------------------------------------------------------------------
// `World` on the tree: writes, undo, the water and glass boxes, meshes and
// format 3 accounting.

/// A random edit of a few metres, as the editor sends them: empty metres,
/// whole cells, fragments of a metre, or a neighbour's content copied in,
/// which makes groups that can merge.
fn random_root_edit(rng: &mut Rng, world: &World) -> Vec<RootEdit> {
    let bounds = world.bounds();
    let mut positions = std::collections::BTreeSet::new();
    let mut edits = Vec::new();
    for _ in 0..rng.range(1, 5) {
        let pos = [
            rng.range(bounds.min[0], bounds.max[0]),
            rng.range(bounds.min[1], bounds.max[1]),
            rng.range(bounds.min[2], bounds.max[2]),
        ];
        if !positions.insert(pos) {
            continue;
        }
        let root = FineBox::root(pos);
        let fragments = match rng.below(4) {
            0 => Vec::new(),
            1 => vec![CellFragment::from_cell(pos, random_cell(rng))],
            2 => {
                let mut neighbour = pos;
                neighbour[rng.below(3) as usize] += if rng.chance(50) { 1 } else { -1 };
                if !world.contains_pos(neighbour) {
                    continue;
                }
                let offset: [i32; 3] = std::array::from_fn(|a| (pos[a] - neighbour[a]) * 8);
                world
                    .root_fragments(neighbour)
                    .into_iter()
                    .filter(|f| f.size == 8 && f.origin == f.bounds.min)
                    .map(|f| CellFragment {
                        bounds: FineBox {
                            min: std::array::from_fn(|a| f.bounds.min[a] + offset[a]),
                            max: std::array::from_fn(|a| f.bounds.max[a] + offset[a]),
                        },
                        origin: std::array::from_fn(|a| f.origin[a] + offset[a]),
                        ..f
                    })
                    .collect()
            }
            _ => {
                let mut fragments: Vec<CellFragment> = Vec::new();
                for _ in 0..rng.range(1, 4) {
                    let mut b = root;
                    for a in 0..3 {
                        let lo = rng.range(root.min[a], root.max[a]);
                        b.min[a] = lo;
                        b.max[a] = rng.range(lo + 1, root.max[a] + 1);
                    }
                    if fragments.iter().any(|f| f.bounds.intersection(b).is_some()) {
                        continue;
                    }
                    fragments.push(CellFragment {
                        bounds: b,
                        origin: root.min,
                        size: 8,
                        cell: random_cell(rng),
                    });
                }
                fragments
            }
        };
        edits.push(RootEdit { pos, fragments });
    }
    edits
}

/// What an undo writes back: each metre's content before the edit.
fn before_of(world: &World, edits: &[RootEdit]) -> Vec<RootEdit> {
    edits
        .iter()
        .map(|e| RootEdit {
            pos: e.pos,
            fragments: world.root_fragments(e.pos),
        })
        .collect()
}

#[test]
fn world_edits_and_their_undo_restore_identical_trees() {
    let mut undone = 0;
    for seed in 0..40 {
        let mut rng = Rng::new(11_000 + seed);
        let f3 = random_format3(&mut rng, seed % 4 != 0);
        let original = world_of(&f3);
        let mut world = original.clone();
        let mut history = Vec::new();
        for _ in 0..12 {
            if rng.chance(25) {
                let mut edits = Vec::new();
                let fine = world.bounds().fine_box();
                for _ in 0..rng.range(1, 3) {
                    let mut b = fine;
                    for a in 0..3 {
                        let lo = rng.range(fine.min[a], fine.max[a] - 1);
                        b.min[a] = lo;
                        b.max[a] = rng.range(lo + 1, (lo + 24).min(fine.max[a]) + 1);
                    }
                    let material = [
                        None,
                        Some(VolumeMaterial::Water),
                        Some(VolumeMaterial::Glass),
                    ][rng.below(3) as usize];
                    edits.push(crate::VolumeEdit {
                        bounds: b,
                        material,
                    });
                }
                let before = world.volumes().to_vec();
                if world.apply_volume_edits(&edits).is_ok() {
                    history.push((Vec::new(), Some(before)));
                }
                continue;
            }
            let edits = random_root_edit(&mut rng, &world);
            let before = before_of(&world, &edits);
            world
                .apply_root_edits(&edits)
                .unwrap_or_else(|e| panic!("seed {seed}: {e}"));
            assert_canonical(world.tree.root_node(), world.tree.root_box());
            history.push((before, None));
        }
        let edited = world.digest();
        for (roots, volumes) in history.iter().rev() {
            if let Some(volumes) = volumes {
                world.replace_volumes(volumes).unwrap();
            } else {
                world.apply_root_edits(roots).unwrap();
            }
            assert_canonical(world.tree.root_node(), world.tree.root_box());
            undone += 1;
        }
        assert_eq!(world.digest(), original.digest(), "seed {seed}");
        assert_eq!(world.volumes(), original.volumes(), "seed {seed}");
        assert_ne!(edited, [0; 16]);
    }
    assert!(undone > 300);
}

#[test]
fn rewriting_a_metre_inside_a_larger_shape_keeps_its_leaf() {
    // A two-metre slab, half a cube thick: its metres lie wholly inside or
    // outside it, so no piece shows which shape they belong to.
    let slab = Cell {
        heights: [4; 4],
        ..Cell::solid(2)
    };
    let region = CellFragment {
        bounds: FineBox {
            min: [0, 0, 0],
            max: [16, 16, 16],
        },
        origin: [0, 0, 0],
        size: 16,
        cell: slab,
    };
    let original = World::from_compact_parts(WorldBounds::default(), &[region], &[], &[]).unwrap();
    assert_eq!(original.stats().shaped_leaves, 1);
    let mut world = original.clone();
    let pos = [1, 0, 0];
    let before = world.root_fragments(pos);
    // Writing a metre's own content back changes nothing.
    assert!(world
        .apply_root_edits(&[RootEdit {
            pos,
            fragments: before.clone()
        }])
        .unwrap()
        .is_empty());
    assert_eq!(world.digest(), original.digest());
    // Carving it and writing it back restores the slab's leaf.
    world
        .apply_root_edits(&[RootEdit {
            pos,
            fragments: vec![],
        }])
        .unwrap();
    assert_ne!(world.digest(), original.digest());
    world
        .apply_root_edits(&[RootEdit {
            pos,
            fragments: before,
        }])
        .unwrap();
    assert_eq!(world.digest(), original.digest());
}

#[test]
fn water_and_glass_variants_act() {
    assert_eq!(
        volume_material_of(MAT_WATER | 2),
        Some(VolumeMaterial::Water)
    );
    assert_eq!(
        volume_material_of(MAT_GLASS | 1),
        Some(VolumeMaterial::Glass)
    );
    assert_eq!(volume_material_of(2 << 2), None);
    let bounds = small_bounds();
    let tree = Octree::new(bounds).unwrap();
    let mut builder = OctreeBuilder::new(bounds, tree.root_box()).unwrap();
    let at = NodeBox {
        min: tree.root_box().min,
        size: 16,
    };
    builder.push(at, Leaf::empty(MAT_WATER | 3)).unwrap();
    let tree = builder.finish().unwrap();
    let inside = at.fine_box().world_min() + Vec3::splat(1.0);
    assert_eq!(tree.volume_material_at(inside), Some(VolumeMaterial::Water));
    // Format 3 writes the variant as plain water.
    assert_eq!(tree.to_format3().volumes.len(), 1);
}

#[test]
fn replacing_boxes_writes_only_those_that_differ() {
    let water = |min: [i32; 3], max: [i32; 3]| MaterialVolume {
        bounds: FineBox { min, max },
        material: VolumeMaterial::Water,
    };
    let glass = |min: [i32; 3], max: [i32; 3]| MaterialVolume {
        bounds: FineBox { min, max },
        material: VolumeMaterial::Glass,
    };
    let mut world = World::empty();
    let a = water([0, 8, 0], [24, 16, 8]);
    let b = glass([40, 8, 0], [41, 40, 48]);
    let written = world.replace_volumes(&[a, b]).unwrap();
    assert_eq!(written.len(), 2);
    let loaded = world.volumes().to_vec();
    // An unchanged list writes nothing.
    assert!(world.replace_volumes(&loaded).unwrap().is_empty());
    // A new box next to the others writes only itself; the loaded boxes stay
    // as they were, so their glass keeps its edge bands.
    let c = water([-40, 8, 0], [-32, 16, 8]);
    let written = world.replace_volumes(&[a, b, c]).unwrap();
    assert_eq!(written, vec![c.bounds]);
    assert!(world.volumes().contains(&a) && world.volumes().contains(&b));
    // Removing a box clears its cube material.
    let written = world.replace_volumes(&[b, c]).unwrap();
    assert_eq!(written, vec![a.bounds]);
    assert_eq!(world.volume_material_at(Vec3::new(1.0, 1.5, 0.5)), None);
    assert_eq!(
        world.volume_material_at(Vec3::new(-4.5, 1.5, 0.5)),
        Some(VolumeMaterial::Water)
    );
    // Geometry edits keep the boxes.
    world.set([-5, 1, 0], Some(Cell::solid(1)));
    assert_eq!(world.volumes().len(), 2);
}

#[test]
fn a_thin_pane_across_a_large_world_is_refused_before_it_is_built() {
    let bounds = WorldBounds {
        min: [-4096, 0, -4096],
        max: [4096, 64, 4096],
    };
    let mut world = World::from_compact_parts(bounds, &[], &[], &[]).unwrap();
    let pane = MaterialVolume {
        bounds: FineBox {
            min: [-32765, 3, -32765],
            max: [32765, 4, 32765],
        },
        material: VolumeMaterial::Glass,
    };
    let before = world.digest();
    assert!(world.replace_volumes(&[pane]).is_err());
    assert!(world
        .prepare_volume_edits(&[crate::VolumeEdit {
            bounds: pane.bounds,
            material: Some(VolumeMaterial::Glass),
        }])
        .is_err());
    assert_eq!(world.digest(), before);
    assert!(world.volumes().is_empty());
    // A one-metre floor 64 m across is 4,096 leaves, and goes in.
    let floor = MaterialVolume {
        bounds: FineBox {
            min: [0, 8, 0],
            max: [512, 16, 512],
        },
        material: VolumeMaterial::Glass,
    };
    world.replace_volumes(&[floor]).unwrap();
    assert_eq!(world.stats().leaves(), 4096);
    // The same floor across the whole world would be 67 million.
    let whole = MaterialVolume {
        bounds: FineBox {
            min: [-32768, 8, -32768],
            max: [32768, 16, 32768],
        },
        material: VolumeMaterial::Glass,
    };
    assert!(world.replace_volumes(&[whole]).is_err());
}

/// Every unit's mesh, by key.
pub(crate) fn all_meshes(
    world: &World,
) -> std::collections::BTreeMap<crate::ChunkKey, Vec<[u32; 7]>> {
    world
        .chunk_keys()
        .into_iter()
        .map(|key| {
            let mesh = world.mesh_chunk(key);
            let vertices = mesh
                .vertices
                .iter()
                .map(|v| {
                    let p = v.position.map(f32::to_bits);
                    let n = v.normal.map(f32::to_bits);
                    [p[0], p[1], p[2], n[0], n[1], n[2], u32::from(v.material)]
                })
                .collect();
            (key, vertices)
        })
        .filter(|(_, vertices): &(_, Vec<_>)| !vertices.is_empty())
        .collect()
}

#[test]
fn dirty_units_are_the_only_meshes_an_edit_changes() {
    let fallbacks = super::mesh::cap_fallbacks();
    let mut pieces = 0;
    for seed in 0..25 {
        let mut rng = Rng::new(13_000 + seed);
        let f3 = random_format3(&mut rng, seed % 3 != 0);
        let mut world = world_of(&f3);
        for _ in 0..6 {
            pieces += world.stats().piece_leaves;
            let mut meshes = all_meshes(&world);
            let edits = random_root_edit(&mut rng, &world);
            let dirty = world.apply_root_edits(&edits).unwrap();
            for key in &dirty {
                meshes.remove(key);
                let mesh = world.mesh_chunk(*key);
                if !mesh.vertices.is_empty() {
                    meshes.insert(*key, all_meshes_of(&world, *key));
                }
            }
            assert_eq!(meshes, all_meshes(&world), "seed {seed}");
        }
    }
    let fallbacks = super::mesh::cap_fallbacks() - fallbacks;
    println!("pieces meshed {pieces}, drawn without caps {fallbacks}");
    assert_eq!(fallbacks, 0);
}

/// A world 128 m wide holding leaves of 64 and 32 m, full and shaped, smaller
/// regions on and off their grid, and water and glass boxes.
fn large_leaf_world(rng: &mut Rng) -> Format3 {
    let bounds = WorldBounds {
        min: [0, 0, 0],
        max: [128, 64, 128],
    };
    let fine = bounds.fine_box();
    let mut out = Format3 {
        bounds,
        ..Default::default()
    };
    let mut taken: Vec<FineBox> = Vec::new();
    for index in 0..rng.range(6, 16) {
        let size: u32 = if index == 0 {
            512
        } else {
            [512, 256, 256, 256, 128, 64, 16, 8][rng.below(8) as usize]
        };
        let s = size as i32;
        let mut origin: [i32; 3] = std::array::from_fn(|a| {
            fine.min[a] + rng.range(0, ((fine.max[a] - fine.min[a]) / s).max(1)) * s
        });
        let mut clip = FineBox {
            min: origin,
            max: origin.map(|v| v + s),
        };
        if size <= 128 && rng.chance(40) {
            for v in &mut origin {
                *v += rng.range(-(s - 1).min(7), s.min(8));
            }
            for (a, &start) in origin.iter().enumerate() {
                let lo = rng.range(start, start + s);
                clip.min[a] = lo;
                clip.max[a] = rng.range(lo + 1, start + s + 1);
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
    for _ in 0..rng.range(1, 6) {
        let b = random_box(rng, fine);
        if !out
            .volumes
            .iter()
            .any(|v| v.bounds.intersection(b).is_some())
        {
            out.volumes.push(random_volume(rng, b));
        }
    }
    out
}

/// A box inside `fine`: whole metres up to 16 m on a side, or eighths up to
/// two metres.
fn random_box(rng: &mut Rng, fine: FineBox) -> FineBox {
    let (step, most) = if rng.chance(50) { (8, 16) } else { (1, 16) };
    let mut b = fine;
    for a in 0..3 {
        let cells = (fine.max[a] - fine.min[a]) / step;
        let lo = rng.range(0, cells - 1);
        let hi = rng.range(lo + 1, (lo + most).min(cells) + 1);
        b.min[a] = fine.min[a] + lo * step;
        b.max[a] = fine.min[a] + hi * step;
    }
    b
}

fn random_volume(rng: &mut Rng, bounds: FineBox) -> MaterialVolume {
    MaterialVolume {
        bounds,
        material: if rng.chance(50) {
            VolumeMaterial::Water
        } else {
            VolumeMaterial::Glass
        },
    }
}

/// The boxes with one removed and one added, still disjoint.
fn replaced_boxes(rng: &mut Rng, world: &World) -> Vec<MaterialVolume> {
    let mut boxes = world.volumes().to_vec();
    if !boxes.is_empty() && rng.chance(60) {
        boxes.remove(rng.below(boxes.len() as u64) as usize);
    }
    let b = random_box(rng, world.bounds().fine_box());
    if !boxes.iter().any(|v| v.bounds.intersection(b).is_some()) {
        boxes.push(random_volume(rng, b));
    }
    boxes
}

#[test]
fn dirty_units_cover_box_and_root_writes_beside_large_leaves() {
    let fallbacks = super::mesh::cap_fallbacks();
    let mut steps = [0; 4];
    let mut large = 0;
    let mut pieces = 0;
    for seed in 0..10 {
        let mut rng = Rng::new(19_000 + seed);
        let mut world = world_of(&large_leaf_world(&mut rng));
        world.tree.for_each_leaf(|at, leaf| {
            large += usize::from(at.size >= 256 && leaf.geometry != LeafGeometry::Empty);
        });
        let mut meshes = all_meshes(&world);
        for _ in 0..8 {
            let before = world.clone();
            let kind = rng.below(4) as usize;
            let dirty = match kind {
                0 => {
                    let edits = random_root_edit(&mut rng, &world);
                    let dirty = world.apply_root_edits(&edits).unwrap();
                    assert_eq!(dirty, world.changed_chunks(&before), "seed {seed}");
                    dirty
                }
                1 => {
                    let fine = world.bounds().fine_box();
                    let edits: Vec<_> = (0..rng.range(1, 3))
                        .map(|_| crate::VolumeEdit {
                            bounds: random_box(&mut rng, fine),
                            material: [
                                None,
                                Some(VolumeMaterial::Water),
                                Some(VolumeMaterial::Glass),
                            ][rng.below(3) as usize],
                        })
                        .collect();
                    world.apply_volume_edits(&edits).unwrap();
                    world.changed_chunks(&before)
                }
                2 => {
                    let boxes = replaced_boxes(&mut rng, &world);
                    world.replace_volumes(&boxes).unwrap();
                    world.changed_chunks(&before)
                }
                _ => {
                    let boxes = replaced_boxes(&mut rng, &world);
                    let edits = random_root_edit(&mut rng, &world);
                    world.apply_patch(&edits, Some(&boxes)).unwrap().opaque
                }
            };
            if world.digest() != before.digest() {
                steps[kind] += 1;
            }
            for key in &dirty {
                meshes.remove(key);
                let mesh = all_meshes_of(&world, *key);
                if !mesh.is_empty() {
                    meshes.insert(*key, mesh);
                }
            }
            assert_eq!(meshes, all_meshes(&world), "seed {seed}, step kind {kind}");
            pieces += world.stats().piece_leaves;
        }
    }
    let fallbacks = super::mesh::cap_fallbacks() - fallbacks;
    println!(
        "changing steps by kind {steps:?}, leaves of 32 m or more {large}, pieces meshed {pieces}, drawn without caps {fallbacks}"
    );
    assert!(steps.iter().all(|&n| n >= 5), "{steps:?}");
    assert!(large >= 10, "{large}");
    assert_eq!(fallbacks, 0);
}

#[test]
fn splitting_a_shaped_leaf_into_pieces_rebuilds_the_unit_beside_it() {
    // A 64 m wedge, full on its -X side, and a metre cube against that side
    // at the top, in the unit beside the wedge's. The wedge covers the cube's
    // face exactly; its pieces are judged conservatively and do not.
    let bounds = WorldBounds {
        min: [-16, 0, 0],
        max: [64, 64, 64],
    };
    let wedge = CellFragment {
        bounds: FineBox {
            min: [0, 0, 0],
            max: [512, 512, 512],
        },
        origin: [0, 0, 0],
        size: 512,
        cell: Cell {
            heights: [8, 0, 0, 8],
            ..Cell::solid(1)
        },
    };
    let cube = CellEdit {
        pos: [-1, 62, 10],
        cell: Some(Cell::solid(2)),
    };
    let mut world = world_of(&Format3 {
        bounds,
        regions: vec![wedge],
        cells: vec![cube],
        ..Default::default()
    });
    let mut meshes = all_meshes(&world);
    let before = world.clone();
    // Water over the wedge's far quarter splits it into pieces; no surface
    // changes.
    world
        .apply_volume_edits(&[crate::VolumeEdit {
            bounds: FineBox {
                min: [384, 0, 0],
                max: [512, 512, 512],
            },
            material: Some(VolumeMaterial::Water),
        }])
        .unwrap();
    assert!(world.stats().piece_leaves > 0);
    let dirty = world.changed_chunks(&before);
    assert!(dirty.contains(&world.tree.unit_key([-8, 496, 80])));
    for key in &dirty {
        meshes.remove(key);
        let mesh = all_meshes_of(&world, *key);
        if !mesh.is_empty() {
            meshes.insert(*key, mesh);
        }
    }
    assert_eq!(meshes, all_meshes(&world));
}

/// A tree's content rebuilt on another root, so trees on different roots
/// compare by digest.
fn content_on(tree: &Octree, root: NodeBox) -> [u8; 16] {
    let mut copy = Octree::with_root(tree.bounds(), root).unwrap();
    tree.for_each_leaf(|at, leaf| {
        let source = match leaf.geometry {
            LeafGeometry::Empty => return,
            LeafGeometry::Full => Source {
                origin: at.min,
                size: at.size,
                shape: Shape::solid(),
            },
            LeafGeometry::Shaped(shape) => Source {
                origin: at.min,
                size: at.size,
                shape,
            },
            LeafGeometry::Piece(source) => source,
        };
        copy.insert_source(at.fine_box(), source, leaf.faces);
    });
    copy.digest()
}

#[test]
fn pieces_keep_their_grid_on_reload_when_their_whole_region_is_cut() {
    // A 64 m slab a quarter of its cube thick, on a map grid 24 m off the
    // bounds' minimum corner, and a two-metre shape elsewhere.
    let bounds = WorldBounds {
        min: [-65, -40, -64],
        max: [64, 88, 64],
    };
    let mut slab = Shape::solid();
    for vertex in &mut slab.vertices[4..] {
        vertex[1] = 2;
    }
    let slab = Cell {
        shape: Some(slab),
        ..Cell::solid(1)
    };
    let wedge = Cell {
        heights: [8, 8, 0, 0],
        ..Cell::solid(2)
    };
    let regions = [
        CellFragment {
            bounds: FineBox {
                min: [-512, -320, -512],
                max: [0, 192, 0],
            },
            origin: [-512, -320, -512],
            size: 512,
            cell: slab,
        },
        CellFragment {
            bounds: FineBox {
                min: [16, 0, 16],
                max: [32, 16, 32],
            },
            origin: [16, 0, 16],
            size: 16,
            cell: wedge,
        },
    ];
    let mut world = World::from_compact_parts(bounds, &regions, &[], &[]).unwrap();
    assert_eq!(world.tree.root_box().min, [-1024, -320, -512]);
    // Cutting a metre out of the slab leaves pieces of it 32 m on a side,
    // larger than any whole region left, which are 16 m.
    world
        .apply_root_edits(&[RootEdit {
            pos: [-30, -38, -30],
            fragments: Vec::new(),
        }])
        .unwrap();
    let saved = world.tree.to_format3();
    let side = |r: &CellFragment| r.bounds.max[0] - r.bounds.min[0];
    let whole = saved
        .regions
        .iter()
        .filter(|r| r.source_bounds() == Some(r.bounds))
        .map(side)
        .max();
    let piece = saved
        .regions
        .iter()
        .filter(|r| r.source_bounds() != Some(r.bounds))
        .map(side)
        .max();
    assert_eq!((whole, piece), (Some(128), Some(256)));
    let size = |f: &Format3| {
        (
            f.regions.len() + f.cells.len(),
            serde_json::to_vec(&f.regions).unwrap().len()
                + serde_json::to_vec(&f.cells).unwrap().len(),
        )
    };
    let reopened = world_of(&saved);
    let again = reopened.tree.to_format3();
    assert_eq!(size(&again), size(&saved));
    let root = reopened.tree.root_box();
    assert_eq!(
        content_on(&reopened.tree, root),
        content_on(&world.tree, root)
    );
    // The root still moves, keeping the pieces' grid, until shared-sandbox
    // 2e stores it.
    assert_eq!(root.min, [-768, -320, -512]);
    assert_eq!(again.regions.len(), saved.regions.len());
}

pub(crate) fn all_meshes_of(world: &World, key: crate::ChunkKey) -> Vec<[u32; 7]> {
    world
        .mesh_chunk(key)
        .vertices
        .iter()
        .map(|v| {
            let p = v.position.map(f32::to_bits);
            let n = v.normal.map(f32::to_bits);
            [p[0], p[1], p[2], n[0], n[1], n[2], u32::from(v.material)]
        })
        .collect()
}

fn triangles(world: &World) -> Vec<[Vec3; 3]> {
    let mut out = Vec::new();
    for key in world.chunk_keys() {
        let mesh = world.mesh_chunk(key);
        for t in mesh.indices.as_chunks::<3>().0 {
            out.push(t.map(|i| Vec3::from_array(mesh.vertices[i as usize].position)));
        }
    }
    out
}

#[test]
fn hidden_faces_are_left_out_and_partly_covered_faces_are_drawn_whole() {
    let mut world = World::empty();
    // A two-metre cube with a one-metre cube on its +X side, above the
    // root's floor.
    world
        .apply_root_edits(
            &(0..8)
                .map(|i| {
                    let pos = [i & 1, 2 + (i >> 1 & 1), i >> 2 & 1];
                    RootEdit {
                        pos,
                        fragments: vec![CellFragment::from_cell(pos, Cell::solid(1))],
                    }
                })
                .chain([RootEdit {
                    pos: [2, 2, 0],
                    fragments: vec![CellFragment::from_cell([2, 2, 0], Cell::solid(1))],
                }])
                .collect::<Vec<_>>(),
        )
        .unwrap();
    assert_eq!(world.stats().leaves(), 2);
    let tris = triangles(&world);
    // The small cube's -X face lies wholly against the large one and is
    // left out; the large cube's +X face is partly covered and drawn whole.
    let on_plane = |t: &[Vec3; 3]| t.iter().all(|p| p.x == 2.0);
    let facing = |t: &[Vec3; 3]| (t[1] - t[0]).cross(t[2] - t[0]).x;
    assert_eq!(
        tris.iter()
            .filter(|t| on_plane(t) && facing(t) < 0.0)
            .count(),
        0
    );
    assert_eq!(
        tris.iter()
            .filter(|t| on_plane(t) && facing(t) > 0.0)
            .count(),
        2
    );
    assert_eq!(tris.len(), 6 * 2 + 5 * 2);
    // Faces on the root's floor are left out.
    let mut floor = World::empty();
    floor.set([0, 0, 0], Some(Cell::solid(2)));
    assert_eq!(triangles(&floor).len(), 5 * 2);
    // Two wedges meeting side to side cover each other's triangular faces.
    let mut wedges = World::empty();
    let wedge = Cell {
        heights: [0, 8, 8, 0],
        ..Cell::solid(3)
    };
    wedges.set([0, 1, 0], Some(wedge));
    wedges.set([0, 1, 1], Some(wedge));
    let alone = {
        let mut one = World::empty();
        one.set([0, 1, 0], Some(wedge));
        triangles(&one).len()
    };
    assert_eq!(triangles(&wedges).len(), 2 * alone - 2);
}

#[test]
fn caps_where_pieces_of_one_source_meet_cancel() {
    // A four-metre ramp with one metre carved from it: the pieces' cut faces
    // between each other are left out; only those facing the hole are drawn.
    let ramp = Cell {
        heights: [0, 8, 8, 0],
        ..Cell::solid(1)
    };
    let region = CellFragment {
        bounds: FineBox {
            min: [0, 8, 0],
            max: [32, 40, 32],
        },
        origin: [0, 8, 0],
        size: 32,
        cell: ramp,
    };
    let mut world = World::from_compact_parts(
        WorldBounds::default(),
        std::slice::from_ref(&region),
        &[],
        &[],
    )
    .unwrap();
    let whole = triangles(&world).len();
    world
        .apply_root_edits(&[RootEdit {
            pos: [2, 2, 1],
            fragments: vec![],
        }])
        .unwrap();
    assert!(world.stats().piece_leaves > 0);
    let tris = triangles(&world);
    // No triangle lies on a plane between pieces away from the hole.
    let hole = FineBox::root([2, 2, 1]);
    for t in &tris {
        for axis in 0..3 {
            let plane = t[0][axis];
            if t.iter().all(|p| p[axis] == plane) && (plane * 8.0).fract() == 0.0 {
                let p = (plane * 8.0) as i32;
                let inside_ramp = p > region.bounds.min[axis] && p < region.bounds.max[axis];
                if inside_ramp {
                    // A cut face drawn whole may reach past the hole it faces.
                    let touches_hole = (0..3).all(|a| {
                        let lo = t.iter().map(|p| p[a] * 8.0).fold(f32::MAX, f32::min);
                        let hi = t.iter().map(|p| p[a] * 8.0).fold(f32::MIN, f32::max);
                        lo <= hole.max[a] as f32 + 0.01 && hi >= hole.min[a] as f32 - 0.01
                    });
                    assert!(touches_hole, "a cap between pieces is drawn: {t:?}");
                }
            }
        }
    }
    assert!(tris.len() > whole);
}

#[test]
fn a_leaf_larger_than_a_unit_is_drawn_whole_by_the_unit_at_its_corner() {
    let size = (MESH_UNIT_SIZE * 8 * 4) as u32;
    let region = CellFragment {
        bounds: FineBox {
            min: [0, 8, 0],
            max: [size as i32, 8 + size as i32, size as i32],
        },
        origin: [0, 8, 0],
        size,
        cell: Cell::solid(1),
    };
    let metres = size as i32 / 8;
    let bounds = WorldBounds {
        min: [0, 0, 0],
        max: [metres, metres + 2, metres],
    };
    // The root sits on the region's grid below the bounds.
    let world = World::from_compact_parts(bounds, &[region], &[], &[]).unwrap();
    let keys = world.chunk_keys();
    assert_eq!(keys.len(), 1);
    let mesh = world.mesh_chunk(keys[0]);
    let far = mesh
        .vertices
        .iter()
        .map(|v| v.position[0])
        .fold(f32::MIN, f32::max);
    assert_eq!(far, size as f32 / 8.0);
}

#[test]
fn piece_changes_are_the_difference_of_the_written_parts() {
    let pieces = |world: &World| -> Vec<Format3Piece> {
        let mut out: Vec<_> = world
            .regions()
            .into_iter()
            .map(Format3Piece::Region)
            .collect();
        out.extend(world.cells().into_iter().map(Format3Piece::Cell));
        out
    };
    let key = |p: &Format3Piece| format!("{p:?}");
    for seed in 0..25 {
        let mut rng = Rng::new(17_000 + seed);
        let f3 = random_format3(&mut rng, seed % 2 == 0);
        let mut world = world_of(&f3);
        for _ in 0..5 {
            let before = world.clone();
            if rng.chance(30) {
                let fine = world.bounds().fine_box();
                let mut b = fine;
                for a in 0..3 {
                    let lo = rng.range(fine.min[a], fine.max[a] - 1);
                    b.min[a] = lo;
                    b.max[a] = rng.range(lo + 1, fine.max[a] + 1);
                }
                let _ = world.apply_volume_edits(&[crate::VolumeEdit {
                    bounds: b,
                    material: Some(VolumeMaterial::Water),
                }]);
            } else {
                let edits = random_root_edit(&mut rng, &world);
                world.apply_root_edits(&edits).unwrap();
            }
            let changes = world.piece_changes(&before);
            let mut old: Vec<_> = pieces(&before).iter().map(key).collect();
            let mut new: Vec<_> = pieces(&world).iter().map(key).collect();
            for p in &changes.removed {
                let at = old
                    .iter()
                    .position(|o| *o == key(p))
                    .expect("a removed piece");
                old.swap_remove(at);
            }
            for p in &changes.added {
                let at = new
                    .iter()
                    .position(|o| *o == key(p))
                    .expect("an added piece");
                new.swap_remove(at);
            }
            old.sort();
            new.sort();
            assert_eq!(old, new, "seed {seed}");
        }
    }
}

fn quads(world: &World) -> Vec<(crate::ChunkKey, crate::TransparentMesh)> {
    world
        .transparent_keys()
        .into_iter()
        .map(|key| (key, world.transparent_chunk(key)))
        .collect()
}

/// Every unit's water and glass, by key, as bits.
pub(crate) fn all_transparent(
    world: &World,
) -> std::collections::BTreeMap<crate::ChunkKey, Vec<[u32; 11]>> {
    world
        .transparent_keys()
        .into_iter()
        .map(|key| (key, transparent_bits(world, key)))
        .filter(|(_, v)| !v.is_empty())
        .collect()
}

pub(crate) fn transparent_bits(world: &World, key: crate::ChunkKey) -> Vec<[u32; 11]> {
    world
        .transparent_chunk(key)
        .vertices
        .iter()
        .map(|v| {
            let p = v.position.map(f32::to_bits);
            let n = v.normal.map(f32::to_bits);
            let uv = v.uv.map(f32::to_bits);
            let s = v.size.map(f32::to_bits);
            [
                p[0],
                p[1],
                p[2],
                n[0] ^ n[1].rotate_left(7) ^ n[2].rotate_left(13),
                uv[0],
                uv[1],
                s[0],
                s[1],
                u32::from(v.material),
                u32::from(v.edges),
                0,
            ]
        })
        .collect()
}

/// Fills whole metres with solid cubes.
fn solid(world: &mut World, min: [i32; 3], max: [i32; 3], material: u8) {
    world.tree.fill_solid(
        FineBox {
            min: min.map(|v| v * 8),
            max: max.map(|v| v * 8),
        },
        [material; 6],
    );
}

fn box_of(min: [i32; 3], max: [i32; 3]) -> FineBox {
    FineBox { min, max }
}

fn water(world: &mut World, bounds: FineBox, material: Option<VolumeMaterial>) {
    world
        .apply_volume_edits(&[crate::VolumeEdit { bounds, material }])
        .unwrap();
}

/// The area of a mesh's quads, by outward normal.
fn area_facing(mesh: &crate::TransparentMesh, normal: [f32; 3]) -> f32 {
    mesh.vertices
        .as_chunks::<4>()
        .0
        .iter()
        .filter(|q| q[0].normal == normal)
        .map(|q| q[0].size[0] * q[0].size[1])
        .sum()
}

#[test]
fn water_draws_its_surface_once_merged_and_not_against_solid() {
    let mut world = World::empty();
    solid(&mut world, [0, 0, 0], [8, 1, 8], 1);
    // Two neighbouring boxes of water one metre up: the faces between them
    // go, and each side's faces merge into one rectangle.
    water(
        &mut world,
        box_of([0, 8, 0], [32, 24, 64]),
        Some(VolumeMaterial::Water),
    );
    water(
        &mut world,
        box_of([32, 8, 0], [64, 24, 64]),
        Some(VolumeMaterial::Water),
    );
    let all = quads(&world);
    let total: usize = all.iter().map(|(_, m)| m.quads()).sum();
    // Top, four sides; the floor covers the bottom.
    assert_eq!(total, 5, "{all:?}");
    for (_, mesh) in &all {
        assert!(mesh.vertices.iter().all(|v| v.material == 0));
    }
    let top: f32 = all
        .iter()
        .map(|(_, m)| area_facing(m, [0.0, 1.0, 0.0]))
        .sum();
    assert_eq!(top, 8.0 * 8.0);
    // Without the floor the bottom shows too.
    let mut open = World::empty();
    water(
        &mut open,
        box_of([0, 8, 0], [64, 24, 64]),
        Some(VolumeMaterial::Water),
    );
    let n: usize = quads(&open).iter().map(|(_, m)| m.quads()).sum();
    assert_eq!(n, 6);
}

#[test]
fn a_water_glass_interface_keeps_both_faces_and_quads_face_outward() {
    let mut world = World::empty();
    water(
        &mut world,
        box_of([0, 8, 0], [8, 16, 8]),
        Some(VolumeMaterial::Glass),
    );
    water(
        &mut world,
        box_of([8, 8, 0], [16, 16, 8]),
        Some(VolumeMaterial::Water),
    );
    let all = quads(&world);
    let total: usize = all.iter().map(|(_, m)| m.quads()).sum();
    assert_eq!(total, 12);
    for (_, mesh) in &all {
        for q in mesh.vertices.as_chunks::<4>().0 {
            let p = |i: usize| Vec3::from_array(q[i].position);
            let geometric = (p(1) - p(0)).cross(p(2) - p(0)).normalize();
            assert_eq!(geometric, Vec3::from_array(q[0].normal));
            assert_eq!(q[0].size, q[2].size);
            assert_eq!(q[0].uv, [0.0, 0.0]);
            assert_eq!(q[2].uv, q[0].size);
        }
    }
}

#[test]
fn glass_edge_bands_follow_the_surface_and_not_the_seams_of_the_merge() {
    // An L of glass one eighth thick: a long low box and a tall one beside
    // it. The merge makes a wide rectangle of the low box and the foot of the
    // tall one, and a square on top.
    let mut world = World::empty();
    water(
        &mut world,
        box_of([0, 8, 0], [16, 16, 1]),
        Some(VolumeMaterial::Glass),
    );
    water(
        &mut world,
        box_of([16, 8, 0], [24, 24, 1]),
        Some(VolumeMaterial::Glass),
    );
    let mesh = world.transparent_chunk(world.transparent_keys()[0]);
    let mut front: Vec<_> = mesh
        .vertices
        .as_chunks::<4>()
        .0
        .iter()
        .filter(|q| q[0].normal == [0.0, 0.0, 1.0])
        .map(|q| (q[0].position, q[2].position, q[0].edges))
        .collect();
    front.sort_by(|a, b| a.0[1].total_cmp(&b.0[1]));
    use crate::{EDGE_U_MAX, EDGE_U_MIN, EDGE_V_MAX, EDGE_V_MIN};
    assert_eq!(
        front,
        [
            // The wide rectangle ends everywhere: the square above it
            // covers only part of its top.
            (
                [0.0, 1.0, 0.125],
                [3.0, 2.0, 0.125],
                EDGE_U_MIN | EDGE_V_MIN | EDGE_U_MAX | EDGE_V_MAX
            ),
            // The square's bottom edge lies wholly on the wide rectangle,
            // so it carries no band.
            (
                [2.0, 2.0, 0.125],
                [3.0, 3.0, 0.125],
                EDGE_U_MIN | EDGE_U_MAX | EDGE_V_MAX
            ),
        ]
    );
}

#[test]
fn a_surface_across_a_unit_boundary_has_no_edge_band_there() {
    let unit = MESH_UNIT_SIZE * 8;
    let mut world = World::empty();
    // Straddle a boundary of the root's units inside the world.
    let root = world.tree.root_box().min[0];
    let bounds = world.bounds().fine_box();
    let mid = (0..)
        .map(|k| root + unit * k)
        .find(|&x| x > bounds.min[0] + 64 && x < bounds.max[0] - 64)
        .unwrap();
    water(
        &mut world,
        box_of([mid - 16, 8, 0], [mid + 16, 24, 1]),
        Some(VolumeMaterial::Glass),
    );
    let keys = world.transparent_keys();
    assert!(keys.len() >= 2, "{keys:?}");
    let mut seams = 0;
    for key in keys {
        for q in world.transparent_chunk(key).vertices.as_chunks::<4>().0 {
            if q[0].normal != [0.0, 0.0, 1.0] {
                continue;
            }
            let xs = [q[0].position[0], q[2].position[0]];
            let left = xs[1] == mid as f32 / 8.0;
            let right = xs[0] == mid as f32 / 8.0;
            if left {
                assert_eq!(q[0].edges & crate::EDGE_U_MAX, 0);
                seams += 1;
            }
            if right {
                assert_eq!(q[0].edges & crate::EDGE_U_MIN, 0);
                seams += 1;
            }
        }
    }
    assert_eq!(seams, 2);
}

#[test]
fn a_shaped_leaf_of_water_draws_its_box_and_a_full_one_draws_nothing() {
    let mut world = World::empty();
    solid(&mut world, [0, 0, 0], [4, 4, 4], 2);
    water(
        &mut world,
        box_of([0, 0, 0], [32, 32, 32]),
        Some(VolumeMaterial::Water),
    );
    assert!(world.transparent_keys().is_empty());
    assert!(world.chunk_keys().len() == 1);
    // Carving one metre out of the solid exposes water faces inside it.
    let mut carved = world.clone();
    carved.apply_edits(&[CellEdit {
        pos: [1, 1, 1],
        cell: None,
    }]);
    let n: usize = quads(&carved).iter().map(|(_, m)| m.quads()).sum();
    // The hole is surrounded by solid: its water faces are against it.
    assert_eq!(n, 0);
    // Opening it to the air shows the water at the opening only.
    carved.apply_edits(&[CellEdit {
        pos: [1, 4, 1],
        cell: None,
    }]);
    let _ = quads(&carved);
}

#[test]
fn transparent_units_are_the_only_ones_an_edit_changes() {
    let mut steps = 0;
    for seed in 0..25 {
        let mut rng = Rng::new(23_000 + seed);
        let mut world = world_of(&large_leaf_world(&mut rng));
        let mut meshes = all_transparent(&world);
        for _ in 0..8 {
            let before = world.clone();
            let changes = match rng.below(3) {
                0 => {
                    let edits = random_root_edit(&mut rng, &world);
                    world.apply_root_edits(&edits).unwrap();
                    world.mesh_changes(&before)
                }
                1 => {
                    let fine = world.bounds().fine_box();
                    let edits: Vec<_> = (0..rng.range(1, 3))
                        .map(|_| crate::VolumeEdit {
                            bounds: random_box(&mut rng, fine),
                            material: [
                                None,
                                Some(VolumeMaterial::Water),
                                Some(VolumeMaterial::Glass),
                            ][rng.below(3) as usize],
                        })
                        .collect();
                    world.apply_volume_edits(&edits).unwrap();
                    world.mesh_changes(&before)
                }
                _ => {
                    let boxes = replaced_boxes(&mut rng, &world);
                    let edits = random_root_edit(&mut rng, &world);
                    world.apply_patch(&edits, Some(&boxes)).unwrap()
                }
            };
            steps += usize::from(world.digest() != before.digest());
            for key in &changes.transparent {
                meshes.remove(key);
                let bits = transparent_bits(&world, *key);
                if !bits.is_empty() {
                    meshes.insert(*key, bits);
                }
            }
            assert_eq!(meshes, all_transparent(&world), "seed {seed}");
        }
    }
    assert!(steps >= 100, "{steps}");
}

/// The area of every opaque quad, by axis and side of its normal, to compare
/// surfaces whatever the tessellation.
fn opaque_area(world: &World) -> [f64; 6] {
    let mut area = [0.0; 6];
    for t in triangles(world) {
        let n = (t[1] - t[0]).cross(t[2] - t[0]);
        let k = (0..3)
            .find(|&a| n[a].abs() > 1.0e-3)
            .map_or(0, |a| a * 2 + usize::from(n[a] > 0.0));
        area[k] += f64::from(n.length()) * 0.5;
    }
    area
}

/// A world holding one solid leaf 64 m on a side, which a unit of 16 m is
/// smaller than.
fn world_with_a_large_leaf() -> World {
    let size = (MESH_UNIT_SIZE * 8 * 4) as u32;
    let region = CellFragment {
        bounds: FineBox {
            min: [0, 8, 0],
            max: [size as i32, 8 + size as i32, size as i32],
        },
        origin: [0, 8, 0],
        size,
        cell: Cell::solid(1),
    };
    let metres = size as i32 / 8;
    let bounds = WorldBounds {
        min: [0, 0, 0],
        max: [metres, metres + 8, metres],
    };
    World::from_compact_parts(bounds, &[region], &[], &[]).unwrap()
}

#[test]
fn a_water_edit_over_a_large_solid_leaf_rebuilds_what_it_must_without_shadows() {
    let mut world = world_with_a_large_leaf();
    assert_eq!(world.stats().leaves(), 1);
    let area = opaque_area(&world);
    let before = world.clone();
    // Water through the top of the leaf: inside the solid it is hidden, and
    // above it, in the air, its surface shows.
    water(
        &mut world,
        box_of([100, 500, 100], [140, 560, 140]),
        Some(VolumeMaterial::Water),
    );
    assert!(world.stats().leaves() > 1, "the leaf splits");
    let changes = world.mesh_changes(&before);
    // The leaf's faces now belong to several units, which must be built
    // again, but no solid shape moved: no shadow can change.
    assert!(changes.opaque.len() > 1, "{changes:?}");
    assert!(changes.shadows.is_empty(), "{changes:?}");
    assert!(!changes.transparent.is_empty());
    assert_eq!(opaque_area(&world), area);
    // The units named are all that differ.
    let mut meshes = all_meshes(&before);
    for key in &changes.opaque {
        meshes.remove(key);
        let mesh = all_meshes_of(&world, *key);
        if !mesh.is_empty() {
            meshes.insert(*key, mesh);
        }
    }
    assert_eq!(meshes, all_meshes(&world));
    // Undoing it joins the leaf again, and moves no shadow either.
    let edited = world.clone();
    water(&mut world, box_of([100, 500, 100], [140, 560, 140]), None);
    assert_eq!(world.digest(), before.digest());
    let undo = world.mesh_changes(&edited);
    assert!(undo.shadows.is_empty(), "{undo:?}");
    assert!(undo.opaque.len() > 1);
    // The water above the leaf is the whole of what it draws.
    let all = quads(&edited);
    assert!(all.iter().any(|(_, m)| m.quads() > 0));
    for (_, mesh) in &all {
        assert!(mesh.vertices.iter().all(|v| v.position[1] >= 65.0));
    }
}

#[test]
fn shadows_follow_solid_shapes_and_not_surface_materials() {
    let size = (MESH_UNIT_SIZE * 8 * 4) as u32 as i32;
    let world = world_with_a_large_leaf();
    // Painting a metre changes meshes but no shadow.
    let mut painted = world.clone();
    painted
        .apply_root_edits(&[RootEdit {
            pos: [10, 10, 10],
            fragments: vec![CellFragment::from_cell([10, 10, 10], Cell::solid(3))],
        }])
        .unwrap();
    let changes = painted.mesh_changes(&world);
    assert!(!changes.opaque.is_empty());
    assert!(changes.shadows.is_empty(), "{changes:?}");
    // Carving it away changes a shadow, in a box around the metre.
    let mut carved = world.clone();
    carved
        .apply_root_edits(&[RootEdit {
            pos: [10, 10, 10],
            fragments: Vec::new(),
        }])
        .unwrap();
    let changes = carved.mesh_changes(&world);
    let metre = box_of([80, 80, 80], [88, 88, 88]);
    assert!(!changes.shadows.is_empty());
    assert!(
        changes
            .shadows
            .iter()
            .any(|b| (0..3).all(|a| b.min[a] <= metre.min[a] && b.max[a] >= metre.max[a])),
        "{changes:?}"
    );
    for b in &changes.shadows {
        assert!(
            (0..3).all(|a| b.max[a] - b.min[a] <= 8 + 2),
            "{b:?} in {size}"
        );
    }
    // Restoring it is the reverse.
    let restored = world.mesh_changes(&carved);
    assert!(!restored.shadows.is_empty());
}

#[test]
fn a_pane_set_into_a_wall_has_no_edge_band_where_it_enters_the_wall() {
    use crate::{EDGE_U_MAX, EDGE_U_MIN, EDGE_V_MAX, EDGE_V_MIN};
    let mut world = World::empty();
    solid(&mut world, [0, 0, 0], [8, 3, 1], 1);
    // Glass through the wall, a little way out of each side.
    water(
        &mut world,
        box_of([16, 8, -4], [48, 16, 12]),
        Some(VolumeMaterial::Glass),
    );
    let mut sides = Vec::new();
    for key in world.transparent_keys() {
        for q in world.transparent_chunk(key).vertices.as_chunks::<4>().0 {
            if q[0].normal == [-1.0, 0.0, 0.0] {
                sides.push((q[0].position[2], q[2].position[2], q[0].edges));
            }
        }
    }
    sides.sort_by(|a, b| a.0.total_cmp(&b.0));
    // For the -X face, u runs along Y and v along Z. Each piece ends in the
    // air and goes on into the wall.
    assert_eq!(
        sides,
        [
            (-0.5, 0.0, EDGE_U_MIN | EDGE_U_MAX | EDGE_V_MIN),
            (1.0, 1.5, EDGE_U_MIN | EDGE_U_MAX | EDGE_V_MAX),
        ]
    );
}
