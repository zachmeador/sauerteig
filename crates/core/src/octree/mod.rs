//! One octree of cubes for a world's geometry and cube material, as in
//! Sauerbraten.
//!
//! Node sizes are powers of two on the eighth-metre grid, from one unit to the
//! root. A node is empty space, a leaf, or eight children shared copy-on-write,
//! so cloning a tree takes constant time and a change copies only the path to
//! it. Leaves follow one canonical form, so equal content gives equal trees and
//! digests, and nothing derived is stored.
//!
//! Its queries take `World`'s arguments and give `World`'s results, so `World`
//! can route them through it.

mod diff;
mod digest;
mod format3;
pub(crate) mod lattice;
pub(crate) mod mesh;
#[cfg(test)]
mod private_worlds;
mod query;
#[cfg(test)]
mod tests;

pub(crate) use format3::{faces_of, piece_of, source_of};
pub use format3::{Format3, Format3Piece, PieceChanges};
pub use mesh::MESH_UNIT_SIZE;
pub(crate) use query::fragment_overlaps_box;

use crate::{FineBox, Shape, VolumeMaterial, WorldBounds, MATERIAL_COLORS};
use lattice::Class;
use std::sync::Arc;

/// Sauerbraten's 16-bit cube material: no material.
pub const MAT_AIR: u16 = 0;
/// Sauerbraten's water, without variant or flags.
pub const MAT_WATER: u16 = 1 << 2;
/// Sauerbraten's glass, without variant or flags.
pub const MAT_GLASS: u16 = 3 << 2;
const MATF_VOLUME: u16 = 7 << 2;

/// The material that acts: water and glass, whatever their variant. Clip,
/// death and other flags are stored but inert, and so is lava.
pub fn volume_material_of(cube_material: u16) -> Option<VolumeMaterial> {
    match cube_material & MATF_VOLUME {
        MAT_WATER => Some(VolumeMaterial::Water),
        MAT_GLASS => Some(VolumeMaterial::Glass),
        _ => None,
    }
}

pub fn cube_material_for(material: VolumeMaterial) -> u16 {
    match material {
        VolumeMaterial::Water => MAT_WATER,
        VolumeMaterial::Glass => MAT_GLASS,
    }
}

/// The largest root: twice the widest world, so any world fits whatever
/// grid offset its cubes need.
pub const MAX_ROOT_SIZE: u32 = 1 << 17;

/// Conversion refuses inputs that would expand past this many leaves, so a
/// hostile file cannot exhaust memory before the world's own limits apply.
pub const MAX_CONVERSION_LEAVES: usize = 1 << 22;

/// A shaped cube that pieces are cut from: its minimum corner and power-of-two
/// size in eighths of a metre, and its shape.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Source {
    pub origin: [i32; 3],
    pub size: u32,
    pub shape: Shape,
}

impl Source {
    pub fn fine_box(&self) -> Option<FineBox> {
        let size = i32::try_from(self.size).ok()?;
        let mut max = [0; 3];
        for (a, v) in max.iter_mut().enumerate() {
            *v = self.origin[a].checked_add(size)?;
        }
        Some(FineBox {
            min: self.origin,
            max,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LeafGeometry {
    Empty,
    Full,
    /// A shape on this cube's own lattice.
    Shaped(Shape),
    /// The part of a larger or offset shaped cube that lies in this cube.
    Piece(Source),
}

/// A leaf's content. Surface materials are per logical face (-X, +X, -Y, +Y,
/// -Z, +Z) and mean nothing without solid geometry; the cube material is
/// Sauerbraten's 16-bit value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Leaf {
    pub geometry: LeafGeometry,
    pub faces: [u8; 6],
    pub cube_material: u16,
}

impl Leaf {
    pub fn full(faces: [u8; 6]) -> Self {
        Self {
            geometry: LeafGeometry::Full,
            faces,
            cube_material: MAT_AIR,
        }
    }
    pub fn empty(cube_material: u16) -> Self {
        Self {
            geometry: LeafGeometry::Empty,
            faces: [0; 6],
            cube_material,
        }
    }
}

/// A node of a tree's grid: minimum corner in eighths of a metre and size.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NodeBox {
    pub min: [i32; 3],
    pub size: u32,
}

impl NodeBox {
    pub fn fine_box(self) -> FineBox {
        let size = self.size as i32;
        FineBox {
            min: self.min,
            max: self.min.map(|v| v + size),
        }
    }
    pub(crate) fn child(self, index: usize) -> Self {
        let half = (self.size / 2) as i32;
        Self {
            min: [
                self.min[0] + (index & 1) as i32 * half,
                self.min[1] + (index >> 1 & 1) as i32 * half,
                self.min[2] + (index >> 2 & 1) as i32 * half,
            ],
            size: self.size / 2,
        }
    }
    /// Index of the child that contains a strictly smaller node inside this one.
    pub(crate) fn child_index(self, inner: Self) -> usize {
        let half = (self.size / 2) as i32;
        (0..3)
            .map(|a| usize::from(inner.min[a] >= self.min[a] + half) << a)
            .sum()
    }
    pub(crate) fn contains(self, other: Self) -> bool {
        (0..3).all(|a| {
            self.min[a] <= other.min[a]
                && other.min[a] as i64 + other.size as i64 <= self.min[a] as i64 + self.size as i64
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Relation {
    Disjoint,
    Inside,
    Partial,
}

pub(crate) fn relation(node: NodeBox, target: FineBox) -> Relation {
    let b = node.fine_box();
    if (0..3).any(|a| b.max[a] <= target.min[a] || b.min[a] >= target.max[a]) {
        Relation::Disjoint
    } else if target.contains(b) {
        Relation::Inside
    } else {
        Relation::Partial
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ShapedLeaf {
    pub shape: Shape,
    pub faces: [u8; 6],
    pub cube: u16,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PieceLeaf {
    pub source: Source,
    pub faces: [u8; 6],
    pub cube: u16,
}

#[derive(Clone, Debug)]
pub(crate) enum Node {
    Empty(u16),
    Full([u8; 6], u16),
    Shaped(Arc<ShapedLeaf>),
    Piece(Arc<PieceLeaf>),
    Branch(Arc<[Node; 8]>),
}

impl PartialEq for Node {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Node::Empty(a), Node::Empty(b)) => a == b,
            (Node::Full(f, a), Node::Full(g, b)) => f == g && a == b,
            (Node::Shaped(a), Node::Shaped(b)) => Arc::ptr_eq(a, b) || a == b,
            (Node::Piece(a), Node::Piece(b)) => Arc::ptr_eq(a, b) || a == b,
            (Node::Branch(a), Node::Branch(b)) => Arc::ptr_eq(a, b) || a == b,
            _ => false,
        }
    }
}

pub(crate) const AIR: Node = Node::Empty(MAT_AIR);

impl Node {
    pub(crate) fn is_leaf(&self) -> bool {
        !matches!(self, Node::Branch(_))
    }
    pub(crate) fn is_air(&self) -> bool {
        matches!(self, Node::Empty(MAT_AIR))
    }
    pub(crate) fn has_geometry(&self) -> bool {
        matches!(self, Node::Full(..) | Node::Shaped(_) | Node::Piece(_))
    }
    pub(crate) fn cube(&self) -> u16 {
        match self {
            Node::Empty(c) | Node::Full(_, c) => *c,
            Node::Shaped(s) => s.cube,
            Node::Piece(p) => p.cube,
            Node::Branch(_) => MAT_AIR,
        }
    }
    pub(crate) fn faces(&self) -> Option<[u8; 6]> {
        match self {
            Node::Full(f, _) => Some(*f),
            Node::Shaped(s) => Some(s.faces),
            Node::Piece(p) => Some(p.faces),
            _ => None,
        }
    }
    pub(crate) fn leaf(&self) -> Option<Leaf> {
        let (geometry, faces, cube_material) = match self {
            Node::Empty(c) => (LeafGeometry::Empty, [0; 6], *c),
            Node::Full(f, c) => (LeafGeometry::Full, *f, *c),
            Node::Shaped(s) => (LeafGeometry::Shaped(s.shape), s.faces, s.cube),
            Node::Piece(p) => (LeafGeometry::Piece(p.source), p.faces, p.cube),
            Node::Branch(_) => return None,
        };
        Some(Leaf {
            geometry,
            faces,
            cube_material,
        })
    }
    pub(crate) fn with_cube(&self, cube: u16) -> Node {
        match self {
            Node::Empty(_) => Node::Empty(cube),
            Node::Full(f, _) => Node::Full(*f, cube),
            Node::Shaped(s) => Node::Shaped(Arc::new(ShapedLeaf {
                cube,
                ..(**s).clone()
            })),
            Node::Piece(p) => Node::Piece(Arc::new(PieceLeaf {
                cube,
                ..(**p).clone()
            })),
            Node::Branch(_) => unreachable!("cube material is set per leaf"),
        }
    }
    fn with_faces(&self, faces: [u8; 6]) -> Node {
        match self {
            Node::Empty(c) => Node::Empty(*c),
            Node::Full(_, c) => Node::Full(faces, *c),
            Node::Shaped(s) => Node::Shaped(Arc::new(ShapedLeaf {
                faces: lattice::normalize_faces(s.shape, faces),
                ..(**s).clone()
            })),
            Node::Piece(p) => Node::Piece(Arc::new(PieceLeaf {
                faces: lattice::normalize_faces(p.source.shape, faces),
                ..(**p).clone()
            })),
            Node::Branch(_) => unreachable!("surface materials are set per leaf"),
        }
    }
}

// ---------------------------------------------------------------------------
// Canonical leaves.

fn check_faces(faces: [u8; 6]) -> Result<(), String> {
    if faces.iter().any(|&m| m as usize >= MATERIAL_COLORS.len()) {
        return Err("A leaf names a surface material outside the catalog.".into());
    }
    Ok(())
}

/// The validity and canonical form of shapes already seen; a map uses few.
#[derive(Default)]
pub(crate) struct ShapeCache(std::collections::HashMap<([[u8; 3]; 8], u8), Option<Canonical>>);

#[derive(Clone, Copy)]
pub(crate) struct Canonical {
    pub shape: Shape,
    /// Faces that keep a surface, whose materials mean something.
    pub visible: [bool; 6],
}

impl ShapeCache {
    /// None when the shape has no volume or folds through itself.
    pub(crate) fn canonical(&mut self, shape: Shape) -> Option<Canonical> {
        if lattice::is_solid(shape) && shape.diagonals < 64 {
            // Every face of the full cube is a convex square.
            return Some(Canonical {
                shape: Shape {
                    diagonals: lattice::FIXED_DIAGONALS,
                    ..shape
                },
                visible: [true; 6],
            });
        }
        *self
            .0
            .entry((shape.vertices, shape.diagonals))
            .or_insert_with(|| {
                lattice::shape_allowed(shape).then(|| {
                    let shape = lattice::normalize_shape(shape);
                    Canonical {
                        shape,
                        visible: lattice::visible_faces(shape),
                    }
                })
            })
    }

    /// A piece's source, validated and normalized, with its visible faces.
    pub(crate) fn source(&mut self, source: Source) -> Result<(Source, [bool; 6]), String> {
        if !source.size.is_power_of_two()
            || source.size > crate::MAX_FRAGMENT_SOURCE_SIZE
            || source.fine_box().is_none()
        {
            return Err("A piece's source cube has an invalid size or position.".into());
        }
        let canonical = self.canonical(source.shape).ok_or_else(|| {
            "A piece's source shape has no volume or folds through itself.".to_string()
        })?;
        Ok((
            Source {
                shape: canonical.shape,
                ..source
            },
            canonical.visible,
        ))
    }
}

/// A canonical shape at its own node: the solid shape is a full leaf.
fn plain(shape: Shape, faces: [u8; 6], cube: u16) -> Node {
    if lattice::is_solid(shape) {
        return Node::Full(faces, cube);
    }
    Node::Shaped(Arc::new(ShapedLeaf { shape, faces, cube }))
}

/// A normalized source, with normalized surface materials, restricted to a
/// node inside it: the plain shape at the source's own node, a full or empty
/// leaf when the node lies wholly inside or outside the solid, and an exact
/// piece otherwise.
pub(crate) fn restrict(source: Source, faces: [u8; 6], cube: u16, at: NodeBox) -> Node {
    if source.origin == at.min && source.size == at.size {
        return plain(source.shape, faces, cube);
    }
    match lattice::classify(&source, at.min, at.size as i32) {
        Class::Inside => Node::Full(faces, cube),
        Class::Outside => Node::Empty(cube),
        Class::Crossing => Node::Piece(Arc::new(PieceLeaf {
            source,
            faces,
            cube,
        })),
    }
}

/// Validates and normalizes a leaf for a node.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn make_leaf(at: NodeBox, leaf: Leaf) -> Result<Node, String> {
    make_leaf_with(at, leaf, &mut ShapeCache::default())
}

pub(crate) fn make_leaf_with(
    at: NodeBox,
    leaf: Leaf,
    cache: &mut ShapeCache,
) -> Result<Node, String> {
    match leaf.geometry {
        LeafGeometry::Empty => Ok(Node::Empty(leaf.cube_material)),
        LeafGeometry::Full => {
            check_faces(leaf.faces)?;
            Ok(Node::Full(leaf.faces, leaf.cube_material))
        }
        LeafGeometry::Shaped(shape) => {
            check_faces(leaf.faces)?;
            let canonical = cache.canonical(shape).ok_or_else(|| {
                "A leaf's shape has no volume or folds through itself.".to_string()
            })?;
            let faces = lattice::fill_faces(canonical.visible, leaf.faces);
            Ok(plain(canonical.shape, faces, leaf.cube_material))
        }
        LeafGeometry::Piece(source) => {
            check_faces(leaf.faces)?;
            let (source, visible) = cache.source(source)?;
            if !source.fine_box().is_some_and(|b| b.contains(at.fine_box())) {
                return Err("A piece lies outside its source cube.".into());
            }
            let faces = lattice::fill_faces(visible, leaf.faces);
            Ok(restrict(source, faces, leaf.cube_material, at))
        }
    }
}

#[cfg(test)]
pub(crate) fn checked_source(source: Source) -> Result<Source, String> {
    ShapeCache::default()
        .source(source)
        .map(|(source, _)| source)
}

/// The eight children a leaf splits into.
pub(crate) fn split(node: &Node, at: NodeBox) -> [Node; 8] {
    match node {
        Node::Empty(c) => std::array::from_fn(|_| Node::Empty(*c)),
        Node::Full(f, c) => std::array::from_fn(|_| Node::Full(*f, *c)),
        Node::Shaped(s) => {
            let source = Source {
                origin: at.min,
                size: at.size,
                shape: s.shape,
            };
            std::array::from_fn(|i| restrict(source, s.faces, s.cube, at.child(i)))
        }
        Node::Piece(p) => std::array::from_fn(|i| restrict(p.source, p.faces, p.cube, at.child(i))),
        Node::Branch(children) => (**children).clone(),
    }
}

/// A node's content restricted to a node inside it.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn restrict_node(node: &Node, at: NodeBox, inner: NodeBox) -> Node {
    if at == inner {
        return node.clone();
    }
    match node {
        Node::Branch(children) => {
            let index = at.child_index(inner);
            restrict_node(&children[index], at.child(index), inner)
        }
        Node::Shaped(s) => restrict(
            Source {
                origin: at.min,
                size: at.size,
                shape: s.shape,
            },
            s.faces,
            s.cube,
            inner,
        ),
        Node::Piece(p) => restrict(p.source, p.faces, p.cube, inner),
        leaf => leaf.clone(),
    }
}

/// Eight children merge when they are equal full or empty leaves, or pieces of
/// one source with equal surface and cube materials, counting full and empty
/// children that lie wholly inside or outside that source.
fn merge(children: &[Node; 8], at: NodeBox) -> Option<Node> {
    match &children[0] {
        Node::Empty(c)
            if children
                .iter()
                .all(|n| matches!(n, Node::Empty(d) if d == c)) =>
        {
            return Some(Node::Empty(*c));
        }
        Node::Full(f, c)
            if children
                .iter()
                .all(|n| matches!(n, Node::Full(g, d) if g == f && d == c)) =>
        {
            return Some(Node::Full(*f, *c));
        }
        _ => {}
    }
    let piece = children.iter().find_map(|n| match n {
        Node::Piece(p) => Some(p.clone()),
        _ => None,
    })?;
    // A piece never grows past its source's own cube.
    if !piece.source.fine_box()?.contains(at.fine_box()) {
        return None;
    }
    for (index, child) in children.iter().enumerate() {
        let cb = at.child(index);
        let fits = match child {
            Node::Piece(p) => Arc::ptr_eq(p, &piece) || **p == *piece,
            Node::Full(f, c) => {
                *f == piece.faces
                    && *c == piece.cube
                    && lattice::classify(&piece.source, cb.min, cb.size as i32) == Class::Inside
            }
            Node::Empty(c) => {
                *c == piece.cube
                    && lattice::classify(&piece.source, cb.min, cb.size as i32) == Class::Outside
            }
            _ => false,
        };
        if !fits {
            return None;
        }
    }
    Some(restrict(piece.source, piece.faces, piece.cube, at))
}

pub(crate) fn canonical_branch(children: [Node; 8], at: NodeBox) -> Node {
    merge(&children, at).unwrap_or_else(|| Node::Branch(Arc::new(children)))
}

// ---------------------------------------------------------------------------
// The tree.

#[derive(Clone, Debug)]
pub struct Octree {
    root_box: NodeBox,
    bounds: WorldBounds,
    root: Node,
}

/// Leaves, internal nodes and estimated bytes by part.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TreeStats {
    pub full_leaves: usize,
    pub shaped_leaves: usize,
    pub piece_leaves: usize,
    /// Empty leaves holding a cube material such as water or glass.
    pub material_leaves: usize,
    pub internal_nodes: usize,
    pub branch_bytes: usize,
    pub shaped_bytes: usize,
    pub piece_bytes: usize,
}

impl TreeStats {
    pub fn leaves(&self) -> usize {
        self.full_leaves + self.shaped_leaves + self.piece_leaves + self.material_leaves
    }
    pub fn bytes(&self) -> usize {
        std::mem::size_of::<Octree>() + self.branch_bytes + self.shaped_bytes + self.piece_bytes
    }
}

/// The root a world gets when it has no cubes to align with: its minimum
/// corner at the bounds' minimum, sized to the next power of two.
pub(crate) fn root_for(bounds: WorldBounds, origin: [i32; 3]) -> Result<NodeBox, String> {
    if !bounds.is_valid() {
        return Err("Invalid world bounds (maximum 8192 metres per axis).".into());
    }
    let fine = bounds.fine_box();
    let extent = (0..3)
        .map(|a| i64::from(fine.max[a]) - i64::from(origin[a]))
        .max()
        .unwrap();
    if (0..3).any(|a| origin[a] > fine.min[a]) || extent <= 0 {
        return Err("The tree's root must contain the world bounds.".into());
    }
    let size = (extent as u64).next_power_of_two();
    if size > u64::from(MAX_ROOT_SIZE) {
        return Err("The world needs a root larger than the largest tree.".into());
    }
    Ok(NodeBox {
        min: origin,
        size: size as u32,
    })
}

impl Octree {
    /// An empty tree whose root starts at the bounds' minimum corner.
    pub fn new(bounds: WorldBounds) -> Result<Self, String> {
        let root_box = root_for(bounds, bounds.fine_box().min)?;
        Ok(Self {
            root_box,
            bounds,
            root: AIR,
        })
    }

    /// An empty tree with a given root, which must contain the bounds.
    pub fn with_root(bounds: WorldBounds, root_box: NodeBox) -> Result<Self, String> {
        if !bounds.is_valid() {
            return Err("Invalid world bounds (maximum 8192 metres per axis).".into());
        }
        if !root_box.size.is_power_of_two()
            || root_box.size > MAX_ROOT_SIZE
            || !root_box.fine_box().contains(bounds.fine_box())
        {
            return Err("The tree's root must be a power of two containing the bounds.".into());
        }
        Ok(Self {
            root_box,
            bounds,
            root: AIR,
        })
    }

    pub fn bounds(&self) -> WorldBounds {
        self.bounds
    }

    pub fn root_box(&self) -> NodeBox {
        self.root_box
    }

    /// Whether a box is a node of this tree's grid.
    pub fn is_node(&self, at: NodeBox) -> bool {
        at.size.is_power_of_two()
            && self.root_box.contains(at)
            && (0..3).all(|a| (at.min[a] - self.root_box.min[a]).rem_euclid(at.size as i32) == 0)
    }

    /// A hash of the root, bounds and canonical content.
    pub fn digest(&self) -> [u8; 16] {
        let mut h = digest::Hasher::new();
        h.write(b"sauerteig octree 1");
        for v in self
            .root_box
            .min
            .iter()
            .chain(&self.bounds.min)
            .chain(&self.bounds.max)
        {
            h.write(&v.to_le_bytes());
        }
        h.write(&self.root_box.size.to_le_bytes());
        hash_node(&mut h, &self.root);
        h.finish()
    }

    /// Every stored leaf in octree order; plain air is not stored.
    pub fn for_each_leaf(&self, mut visit: impl FnMut(NodeBox, Leaf)) {
        fn walk(node: &Node, at: NodeBox, visit: &mut impl FnMut(NodeBox, Leaf)) {
            match node {
                Node::Branch(children) => {
                    for (i, child) in children.iter().enumerate() {
                        walk(child, at.child(i), visit);
                    }
                }
                leaf if leaf.is_air() => {}
                leaf => visit(at, leaf.leaf().unwrap()),
            }
        }
        walk(&self.root, self.root_box, &mut visit);
    }

    pub fn leaves(&self) -> Vec<(NodeBox, Leaf)> {
        let mut out = Vec::new();
        self.for_each_leaf(|at, leaf| out.push((at, leaf)));
        out
    }

    pub fn stats(&self) -> TreeStats {
        fn walk(node: &Node, stats: &mut TreeStats) {
            match node {
                Node::Branch(children) => {
                    stats.internal_nodes += 1;
                    stats.branch_bytes +=
                        2 * std::mem::size_of::<usize>() + std::mem::size_of::<[Node; 8]>();
                    for child in children.iter() {
                        walk(child, stats);
                    }
                }
                Node::Empty(MAT_AIR) => {}
                Node::Empty(_) => stats.material_leaves += 1,
                Node::Full(..) => stats.full_leaves += 1,
                Node::Shaped(_) => {
                    stats.shaped_leaves += 1;
                    stats.shaped_bytes +=
                        2 * std::mem::size_of::<usize>() + std::mem::size_of::<ShapedLeaf>();
                }
                Node::Piece(_) => {
                    stats.piece_leaves += 1;
                    stats.piece_bytes +=
                        2 * std::mem::size_of::<usize>() + std::mem::size_of::<PieceLeaf>();
                }
            }
        }
        let mut stats = TreeStats::default();
        walk(&self.root, &mut stats);
        stats
    }

    /// The leaf whose node holds a fine-grid box, if one leaf holds all of it.
    pub fn leaf_containing(&self, at: NodeBox) -> Option<(NodeBox, Leaf)> {
        if !self.root_box.contains(at) {
            return None;
        }
        let mut node = &self.root;
        let mut nb = self.root_box;
        loop {
            match node {
                Node::Branch(children) if nb.size > at.size => {
                    let index = nb.child_index(at);
                    node = &children[index];
                    nb = nb.child(index);
                }
                Node::Branch(_) => return None,
                leaf => return Some((nb, leaf.leaf().unwrap())),
            }
        }
    }

    pub(crate) fn root_node(&self) -> &Node {
        &self.root
    }

    /// The content of a node of the grid as it stands, even inside a larger
    /// leaf.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn node_at(&self, at: NodeBox) -> Node {
        restrict_node(&self.root, self.root_box, at)
    }

    /// Applies an operation to the maximal nodes inside a box, clipped to the
    /// world bounds, and keeps every changed path canonical.
    pub(crate) fn modify(&mut self, target: FineBox, op: &mut dyn FnMut(&Node, NodeBox) -> Node) {
        fn walk(
            node: &mut Node,
            at: NodeBox,
            target: FineBox,
            op: &mut dyn FnMut(&Node, NodeBox) -> Node,
        ) {
            match relation(at, target) {
                Relation::Disjoint => {}
                Relation::Inside => *node = op(node, at),
                Relation::Partial => {
                    let mut children = split(node, at);
                    for (i, child) in children.iter_mut().enumerate() {
                        walk(child, at.child(i), target, op);
                    }
                    *node = canonical_branch(children, at);
                }
            }
        }
        if let Some(target) = target.intersection(self.bounds.fine_box()) {
            walk(&mut self.root, self.root_box, target, op);
        }
    }

    /// Removes solid geometry and surface materials in a box, keeping cube
    /// material.
    pub(crate) fn clear_geometry(&mut self, target: FineBox) {
        self.modify(target, &mut |node, at| {
            map_leaves(node, at, &|leaf| Node::Empty(leaf.cube()))
        });
    }

    /// Fills a box with solid geometry of the given surface materials,
    /// keeping cube material.
    pub(crate) fn fill_solid(&mut self, target: FineBox, faces: [u8; 6]) {
        let solid = Node::Full(faces, MAT_AIR);
        self.modify(target, &mut |node, at| {
            combine(&solid, node, at, Parts::Shape)
        });
    }

    /// Sets the cube material in a box, keeping geometry.
    pub(crate) fn set_cube_material(&mut self, target: FineBox, cube: u16) {
        self.modify(target, &mut |node, at| {
            map_leaves(node, at, &|leaf| leaf.with_cube(cube))
        });
    }

    /// Writes a source's solid and surface materials into a box inside it,
    /// keeping cube material.
    /// The source must have passed validation.
    pub(crate) fn insert_source(&mut self, target: FineBox, source: Source, faces: [u8; 6]) {
        let source = Source {
            shape: lattice::normalize_shape(source.shape),
            ..source
        };
        let faces = lattice::normalize_faces(source.shape, faces);
        self.modify(target, &mut |node, at| {
            combine(
                &restrict(source, faces, MAT_AIR, at),
                node,
                at,
                Parts::Shape,
            )
        });
    }

    /// Writes a subtree at a node. Returns what an undo writes back: the
    /// smallest node that held the changed content, and that content.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn write(
        &mut self,
        at: NodeBox,
        content: &Node,
        parts: Parts,
    ) -> Result<Option<Replaced>, String> {
        if !self.is_node(at) {
            return Err("A write must name a node of the tree's grid.".into());
        }
        let replaced_box = self.replaced_box(at);
        let before = self.node_at(replaced_box);
        let previous = self.root.clone();
        let content = content.clone();
        self.modify(at.fine_box(), &mut |node, nb| {
            combine(&restrict_node(&content, at, nb), node, nb, parts)
        });
        if self.node_at(replaced_box) == before {
            self.root = previous;
            return Ok(None);
        }
        Ok(Some(Replaced {
            at: replaced_box,
            content: before,
        }))
    }

    /// The node itself, or the leaf above it that a write splits.
    #[cfg_attr(not(test), allow(dead_code))]
    fn replaced_box(&self, at: NodeBox) -> NodeBox {
        let mut node = &self.root;
        let mut nb = self.root_box;
        while nb.size > at.size {
            match node {
                Node::Branch(children) => {
                    let index = nb.child_index(at);
                    node = &children[index];
                    nb = nb.child(index);
                }
                _ => return nb,
            }
        }
        at
    }
}

impl Octree {
    /// Merges the nodes around a written box back into a source's own content
    /// wherever every leaf under them is that source restricted. Children that
    /// all lie wholly inside or outside a shape have no piece to show which
    /// shape they belong to, so the canonical merge cannot rebuild it; the
    /// source of a written fragment can. With it, writing back a piece of a
    /// larger shape, as an undo does, restores the shape's leaf.
    pub(crate) fn remerge(&mut self, source: Source, faces: [u8; 6], near: FineBox) {
        let Some(own) = source.fine_box() else {
            return;
        };
        fn walk(
            node: &mut Node,
            at: NodeBox,
            near: FineBox,
            source: &Source,
            own: FineBox,
            faces: [u8; 6],
        ) -> bool {
            if relation(at, near) == Relation::Disjoint {
                return false;
            }
            let Node::Branch(children) = &*node else {
                return false;
            };
            let mut rebuilt: Option<[Node; 8]> = None;
            for i in 0..8 {
                let mut child = children[i].clone();
                if walk(&mut child, at.child(i), near, source, own, faces) {
                    rebuilt.get_or_insert_with(|| (**children).clone())[i] = child;
                }
            }
            let mut changed = false;
            if let Some(children) = rebuilt {
                *node = canonical_branch(children, at);
                changed = true;
            }
            if matches!(node, Node::Branch(_)) && own.contains(at.fine_box()) {
                let candidate = restrict(*source, faces, first_cube(node), at);
                if expands_to(&candidate, at, node) {
                    *node = candidate;
                    changed = true;
                }
            }
            changed
        }
        walk(&mut self.root, self.root_box, near, &source, own, faces);
    }

    /// How many leaves setting the cube material in a box would add,
    /// counted before anything is built and ignoring merges, or `None` past
    /// `limit`.
    pub(crate) fn count_cube_write(
        &self,
        target: FineBox,
        cube: u16,
        limit: usize,
    ) -> Option<usize> {
        match target.intersection(self.bounds.fine_box()) {
            Some(target) => added_leaves(&self.root, self.root_box, target, cube, limit),
            None => Some(0),
        }
    }
}

/// The cube material of a subtree's first leaf.
fn first_cube(node: &Node) -> u16 {
    match node {
        Node::Branch(children) => first_cube(&children[0]),
        leaf => leaf.cube(),
    }
}

/// Whether a subtree holds exactly what a leaf splits into.
fn expands_to(candidate: &Node, at: NodeBox, subtree: &Node) -> bool {
    match subtree {
        Node::Branch(children) => {
            let parts = split(candidate, at);
            (0..8).all(|i| expands_to(&parts[i], at.child(i), &children[i]))
        }
        leaf => leaf == candidate,
    }
}

/// Leaves a cube-material write adds under a node, or `None` past `limit`.
fn added_leaves(
    node: &Node,
    at: NodeBox,
    target: FineBox,
    cube: u16,
    limit: usize,
) -> Option<usize> {
    match relation(at, target) {
        Relation::Disjoint => Some(0),
        Relation::Inside => {
            // Every leaf keeps its place; only plain air becomes a leaf.
            let mut added = 0;
            if cube != MAT_AIR {
                count_air(node, &mut added);
            }
            (added <= limit).then_some(added)
        }
        Relation::Partial => match node {
            Node::Branch(children) => {
                let mut added = 0;
                for (i, child) in children.iter().enumerate() {
                    added += added_leaves(child, at.child(i), target, cube, limit - added)?;
                }
                Some(added)
            }
            leaf => {
                let after = split_leaves(leaf, at, target, cube, limit + 1)?;
                let added = after.saturating_sub(usize::from(!leaf.is_air()));
                (added <= limit).then_some(added)
            }
        },
    }
}

fn count_air(node: &Node, count: &mut usize) {
    match node {
        Node::Branch(children) => children.iter().for_each(|c| count_air(c, count)),
        leaf if leaf.is_air() => *count += 1,
        _ => {}
    }
}

/// How many leaves other than plain air a leaf becomes when a write covers
/// part of it, or `None` past `limit`.
fn split_leaves(
    leaf: &Node,
    at: NodeBox,
    target: FineBox,
    cube: u16,
    limit: usize,
) -> Option<usize> {
    match relation(at, target) {
        Relation::Disjoint => Some(usize::from(!leaf.is_air())),
        Relation::Inside => Some(usize::from(leaf.has_geometry() || cube != MAT_AIR)),
        Relation::Partial => {
            let mut total = 0;
            for (i, child) in split(leaf, at).iter().enumerate() {
                total += split_leaves(child, at.child(i), target, cube, limit - total)?;
                if total > limit {
                    return None;
                }
            }
            Some(total)
        }
    }
}

/// Which parts of the content a write replaces.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) enum Parts {
    /// Everything, for paste, undo and redo.
    All,
    /// Geometry and surface materials, keeping the cube material.
    Shape,
    /// Surface materials only.
    Surface,
    /// The cube material only.
    Cube,
}

#[derive(Clone, Debug)]
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) struct Replaced {
    pub at: NodeBox,
    pub content: Node,
}

/// Applies a leaf function to every leaf of a subtree, canonically.
pub(crate) fn map_leaves(node: &Node, at: NodeBox, f: &dyn Fn(&Node) -> Node) -> Node {
    match node {
        Node::Branch(children) => {
            let mapped = std::array::from_fn(|i| map_leaves(&children[i], at.child(i), f));
            canonical_branch(mapped, at)
        }
        leaf => f(leaf),
    }
}

/// Takes the given parts from `new` and the rest from `old`.
pub(crate) fn combine(new: &Node, old: &Node, at: NodeBox, parts: Parts) -> Node {
    if parts == Parts::All {
        return new.clone();
    }
    if new.is_leaf() && old.is_leaf() {
        return match parts {
            Parts::Shape => new.with_cube(old.cube()),
            Parts::Cube => old.with_cube(new.cube()),
            Parts::Surface => match new.faces() {
                Some(faces) => old.with_faces(faces),
                None => old.clone(),
            },
            Parts::All => unreachable!(),
        };
    }
    let a = split(new, at);
    let b = split(old, at);
    let children = std::array::from_fn(|i| combine(&a[i], &b[i], at.child(i), parts));
    canonical_branch(children, at)
}

fn hash_node(h: &mut digest::Hasher, node: &Node) {
    let shape = |h: &mut digest::Hasher, shape: &Shape| {
        for v in shape.vertices {
            h.write(&v);
        }
        h.write(&[shape.diagonals]);
    };
    match node {
        Node::Empty(c) => {
            h.write(&[0]);
            h.write(&c.to_le_bytes());
        }
        Node::Full(f, c) => {
            h.write(&[1]);
            h.write(f);
            h.write(&c.to_le_bytes());
        }
        Node::Shaped(s) => {
            h.write(&[2]);
            shape(h, &s.shape);
            h.write(&s.faces);
            h.write(&s.cube.to_le_bytes());
        }
        Node::Piece(p) => {
            h.write(&[3]);
            for v in p.source.origin {
                h.write(&v.to_le_bytes());
            }
            h.write(&p.source.size.to_le_bytes());
            shape(h, &p.source.shape);
            h.write(&p.faces);
            h.write(&p.cube.to_le_bytes());
        }
        Node::Branch(children) => {
            h.write(&[4]);
            for child in children.iter() {
                hash_node(h, child);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// The builder.

fn spread(mut v: u64) -> u64 {
    v &= 0x1f_ffff;
    v = (v | v << 32) & 0x001f_0000_0000_ffff;
    v = (v | v << 16) & 0x001f_0000_ff00_00ff;
    v = (v | v << 8) & 0x100f_00f0_0f00_f00f;
    v = (v | v << 4) & 0x10c3_0c30_c30c_30c3;
    v = (v | v << 2) & 0x1249_2492_4924_9249;
    v
}

/// Pre-order key: Morton order of minimum corners, larger nodes first.
fn order_key(root: NodeBox, at: NodeBox) -> u64 {
    let d = (0..3)
        .map(|a| (at.min[a] - root.min[a]) as u64)
        .collect::<Vec<_>>();
    let morton = spread(d[0]) | spread(d[1]) << 1 | spread(d[2]) << 2;
    morton << 5 | u64::from(31 - at.size.trailing_zeros())
}

enum Slot {
    Free,
    Filled(Node),
    Open,
}

struct Frame {
    at: NodeBox,
    slots: [Slot; 8],
}

/// Builds a tree from leaves given in any order; leaves given in octree
/// order (pre-order, children in Morton order) skip the sort. Leaves may not
/// overlap, and anything outside the world bounds is left out.
pub struct OctreeBuilder {
    tree: Octree,
    leaves: Vec<(u64, NodeBox, Node)>,
    ordered: bool,
    limit: usize,
    pub(crate) shapes: ShapeCache,
}

impl OctreeBuilder {
    pub fn new(bounds: WorldBounds, root_box: NodeBox) -> Result<Self, String> {
        Ok(Self {
            tree: Octree::with_root(bounds, root_box)?,
            leaves: Vec::new(),
            ordered: true,
            limit: MAX_CONVERSION_LEAVES,
            shapes: ShapeCache::default(),
        })
    }

    pub fn root_box(&self) -> NodeBox {
        self.tree.root_box
    }

    /// Adds a leaf at a node of the grid.
    pub fn push(&mut self, at: NodeBox, leaf: Leaf) -> Result<(), String> {
        if !self.tree.is_node(at) {
            return Err("A leaf must sit on a node of the tree's grid.".into());
        }
        let node = make_leaf_with(at, leaf, &mut self.shapes)?;
        let bounds = self.tree.bounds.fine_box();
        match relation(at, bounds) {
            Relation::Inside => self.add(at, node),
            Relation::Disjoint => Ok(()),
            Relation::Partial => self.add_inside(at, node, bounds),
        }
    }

    fn add_inside(&mut self, at: NodeBox, node: Node, bounds: FineBox) -> Result<(), String> {
        for (i, child) in split(&node, at).into_iter().enumerate() {
            let cb = at.child(i);
            match relation(cb, bounds) {
                Relation::Inside => self.add(cb, child)?,
                Relation::Disjoint => {}
                Relation::Partial => self.add_inside(cb, child, bounds)?,
            }
        }
        Ok(())
    }

    pub(crate) fn add(&mut self, at: NodeBox, node: Node) -> Result<(), String> {
        if self.leaves.len() >= self.limit {
            return Err("The world expands past the tree's conversion limit.".into());
        }
        let key = order_key(self.tree.root_box, at);
        if self.leaves.last().is_some_and(|last| last.0 > key) {
            self.ordered = false;
        }
        self.leaves.push((key, at, node));
        Ok(())
    }

    pub(crate) fn len(&self) -> usize {
        self.leaves.len()
    }

    pub fn finish(mut self) -> Result<Octree, String> {
        if !self.ordered {
            self.leaves.sort_by_key(|leaf| leaf.0);
        }
        let root_box = self.tree.root_box;
        let overlap = || "Leaves overlap.".to_string();
        let mut root_leaf = None;
        let mut stack = vec![Frame {
            at: root_box,
            slots: std::array::from_fn(|_| Slot::Free),
        }];
        let close = |stack: &mut Vec<Frame>| {
            let frame = stack.pop().unwrap();
            let children = frame.slots.map(|slot| match slot {
                Slot::Filled(node) => node,
                _ => AIR,
            });
            let node = canonical_branch(children, frame.at);
            let parent = stack.last_mut().unwrap();
            let index = parent.at.child_index(frame.at);
            parent.slots[index] = Slot::Filled(node);
        };
        for (_, at, node) in std::mem::take(&mut self.leaves) {
            if at == root_box {
                if root_leaf.is_some()
                    || stack.len() > 1
                    || stack[0].slots.iter().any(|s| !matches!(s, Slot::Free))
                {
                    return Err(overlap());
                }
                root_leaf = Some(node);
                continue;
            }
            if root_leaf.is_some() {
                return Err(overlap());
            }
            loop {
                let top = stack.last().unwrap();
                if top.at.contains(at) && top.at.size > at.size {
                    break;
                }
                if stack.len() == 1 {
                    return Err(overlap());
                }
                close(&mut stack);
            }
            loop {
                let top = stack.last_mut().unwrap();
                let index = top.at.child_index(at);
                let child = top.at.child(index);
                if child == at {
                    if !matches!(top.slots[index], Slot::Free) {
                        return Err(overlap());
                    }
                    top.slots[index] = Slot::Filled(node);
                    break;
                }
                match top.slots[index] {
                    Slot::Free => {
                        top.slots[index] = Slot::Open;
                        stack.push(Frame {
                            at: child,
                            slots: std::array::from_fn(|_| Slot::Free),
                        });
                    }
                    _ => return Err(overlap()),
                }
            }
        }
        while stack.len() > 1 {
            close(&mut stack);
        }
        let frame = stack.pop().unwrap();
        self.tree.root = match root_leaf {
            Some(node) => node,
            None => canonical_branch(
                frame.slots.map(|slot| match slot {
                    Slot::Filled(node) => node,
                    _ => AIR,
                }),
                root_box,
            ),
        };
        Ok(self.tree)
    }
}
