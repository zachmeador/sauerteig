//! Opaque world meshes, one per mesh unit.
//!
//! A mesh unit is a node of the tree's grid of one fixed size. Each leaf's
//! faces belong to the unit holding the leaf's minimum corner, so a leaf larger
//! than a unit is drawn whole by that one unit and never split at unit
//! boundaries.
//!
//! A face lying on its leaf's own box is left out when the solids beyond it
//! cover it completely, judged as polygon coverage on the shared plane; a
//! partly covered face is drawn whole, as Sauerbraten does. Coincident caps
//! where pieces of one source meet cover each other, so they cancel. Faces on
//! the root's outer boundary are left out. Water and glass are not meshed here.
//!
//! Plain leaves are judged exactly, in integer 64ths of a metre. Pieces are
//! judged conservatively: when in doubt a face is drawn.

use super::lattice::{self, Class};
use super::query::clip_triangles;
use super::{Node, NodeBox, Octree, Source};
use crate::{ChunkKey, FineBox, Mesh, Shape, Vec3, Vertex, MATERIAL_COLORS};
use glam::DVec3;

/// The side of a mesh unit in metres. Measured on the_cube1m: 16 m units give
/// 749 units, 32 m units 335 and 64 m units 218, against the 2,392 compact
/// batches they replace, so 16 m is the nearest of the sizes allowed.
pub const MESH_UNIT_SIZE: i32 = 16;
const UNIT: u32 = MESH_UNIT_SIZE as u32 * 8;
const CLIP_EPS: f64 = 1.0e-9;

/// A point in 64ths of a metre on a face plane, as a fraction `x / d`,
/// `y / d` with `d > 0`.
#[derive(Clone, Copy)]
struct Q {
    x: i128,
    y: i128,
    d: i128,
}

type P2 = [i128; 2];

fn cross(o: P2, a: P2, b: P2) -> i128 {
    (a[0] - o[0]) * (b[1] - o[1]) - (a[1] - o[1]) * (b[0] - o[0])
}

/// The side of the line `a`–`b` a fraction lies on, times its positive
/// denominator.
fn side(a: P2, b: P2, q: Q) -> i128 {
    (b[0] - a[0]) * (q.y - a[1] * q.d) - (b[1] - a[1]) * (q.x - a[0] * q.d)
}

/// Whether a closed convex polygon, listed counter-clockwise, holds a point.
fn convex_holds(polygon: &[P2], q: Q) -> bool {
    (0..polygon.len()).all(|i| side(polygon[i], polygon[(i + 1) % polygon.len()], q) >= 0)
}

fn counter_clockwise(t: [P2; 3]) -> Option<[P2; 3]> {
    match cross(t[0], t[1], t[2]).signum() {
        1 => Some(t),
        -1 => Some([t[0], t[2], t[1]]),
        _ => None,
    }
}

/// The convex hull of points, counter-clockwise, without collinear points.
fn hull(mut points: Vec<P2>) -> Vec<P2> {
    points.sort_unstable();
    points.dedup();
    if points.len() < 3 {
        return points;
    }
    let mut lower: Vec<P2> = Vec::new();
    for &p in &points {
        while lower.len() >= 2 && cross(lower[lower.len() - 2], lower[lower.len() - 1], p) <= 0 {
            lower.pop();
        }
        lower.push(p);
    }
    let mut upper: Vec<P2> = Vec::new();
    for &p in points.iter().rev() {
        while upper.len() >= 2 && cross(upper[upper.len() - 2], upper[upper.len() - 1], p) <= 0 {
            upper.pop();
        }
        upper.push(p);
    }
    lower.pop();
    upper.pop();
    lower.extend(upper);
    lower
}

fn area2(polygon: &[P2]) -> i128 {
    (0..polygon.len())
        .map(|i| {
            let (a, b) = (polygon[i], polygon[(i + 1) % polygon.len()]);
            a[0] * b[1] - b[0] * a[1]
        })
        .sum()
}

/// An axis-aligned rectangle on a face plane, in 64ths.
#[derive(Clone, Copy)]
struct Rect {
    min: P2,
    max: P2,
}

impl Rect {
    fn corners(self) -> [P2; 4] {
        [
            self.min,
            [self.max[0], self.min[1]],
            self.max,
            [self.min[0], self.max[1]],
        ]
    }
    fn holds(self, p: P2) -> bool {
        (0..2).all(|a| self.min[a] <= p[a] && p[a] <= self.max[a])
    }
}

/// Whether the interiors of a triangle and a rectangle meet: no edge normal
/// of either separates them.
fn interiors_meet(t: [P2; 3], r: Rect) -> bool {
    let corners = r.corners();
    let mut axes = vec![[1, 0], [0, 1]];
    for i in 0..3 {
        let (a, b) = (t[i], t[(i + 1) % 3]);
        axes.push([a[1] - b[1], b[0] - a[0]]);
    }
    axes.iter().all(|n| {
        let project = |p: &P2| n[0] * p[0] + n[1] * p[1];
        let (t_lo, t_hi) = t
            .iter()
            .map(project)
            .fold((i128::MAX, i128::MIN), |(lo, hi), v| (lo.min(v), hi.max(v)));
        let (r_lo, r_hi) = corners
            .iter()
            .map(project)
            .fold((i128::MAX, i128::MIN), |(lo, hi), v| (lo.min(v), hi.max(v)));
        t_hi > r_lo && r_hi > t_lo
    })
}

/// Points whose convex hull is the intersection of a triangle (counter-
/// clockwise) and a rectangle.
fn intersection_points(t: [P2; 3], r: Rect) -> Vec<Q> {
    let whole = |p: P2| Q {
        x: p[0],
        y: p[1],
        d: 1,
    };
    let mut points: Vec<Q> = t
        .iter()
        .filter(|p| r.holds(**p))
        .map(|&p| whole(p))
        .collect();
    points.extend(
        r.corners()
            .into_iter()
            .filter(|&c| convex_holds(&t, whole(c)))
            .map(whole),
    );
    for i in 0..3 {
        let (p, q) = (t[i], t[(i + 1) % 3]);
        for axis in 0..2 {
            let other = 1 - axis;
            for line in [r.min[axis], r.max[axis]] {
                if (p[axis] - line) * (q[axis] - line) >= 0 {
                    continue;
                }
                let mut d = q[axis] - p[axis];
                let mut n = p[other] * d + (q[other] - p[other]) * (line - p[axis]);
                if d < 0 {
                    d = -d;
                    n = -n;
                }
                if r.min[other] * d <= n && n <= r.max[other] * d {
                    let mut point = [0; 2];
                    point[axis] = line * d;
                    point[other] = n;
                    points.push(Q {
                        x: point[0],
                        y: point[1],
                        d,
                    });
                }
            }
        }
    }
    points
}

/// Whether the union of a neighbour's faces on a plane, given as counter-
/// clockwise triangles that do not overlap, holds every point.
fn faces_hold(faces: &[[P2; 3]], points: &[Q]) -> bool {
    if points.is_empty() {
        return true;
    }
    if faces
        .iter()
        .any(|t| points.iter().all(|&q| convex_holds(t, q)))
    {
        return true;
    }
    if faces.len() < 2 {
        return false;
    }
    let outline = hull(faces.iter().flatten().copied().collect());
    let total: i128 = faces.iter().map(|t| area2(t)).sum();
    outline.len() >= 3
        && total == area2(&outline)
        && points.iter().all(|&q| convex_holds(&outline, q))
}

/// One face of a leaf's box: its axis, its side and the plane it lies on.
#[derive(Clone, Copy)]
struct Face {
    axis: usize,
    positive: bool,
    /// The plane, in eighths.
    plane: i32,
    /// The face's square on the plane, in eighths.
    square: FineBox,
}

impl Face {
    fn of(at: NodeBox, k: usize) -> Self {
        let axis = k / 2;
        let positive = k % 2 == 1;
        let b = at.fine_box();
        Self {
            axis,
            positive,
            plane: if positive { b.max[axis] } else { b.min[axis] },
            square: b,
        }
    }
    fn axes(self) -> (usize, usize) {
        ((self.axis + 1) % 3, (self.axis + 2) % 3)
    }
    /// The one-eighth slab beyond the face.
    fn beyond(self) -> FineBox {
        let mut slab = self.square;
        if self.positive {
            slab.min[self.axis] = self.plane;
            slab.max[self.axis] = self.plane + 1;
        } else {
            slab.min[self.axis] = self.plane - 1;
            slab.max[self.axis] = self.plane;
        }
        slab
    }
    /// A neighbour's footprint on the face's square, in 64ths.
    fn footprint(self, nb: NodeBox) -> Rect {
        let (u, v) = self.axes();
        let b = nb.fine_box();
        let s = self.square;
        Rect {
            min: [
                i128::from(b.min[u].max(s.min[u])) * 8,
                i128::from(b.min[v].max(s.min[v])) * 8,
            ],
            max: [
                i128::from(b.max[u].min(s.max[u])) * 8,
                i128::from(b.max[v].min(s.max[v])) * 8,
            ],
        }
    }
    /// The part of the neighbour's slab beyond the face over a footprint.
    fn slab_over(self, nb: NodeBox) -> FineBox {
        let (u, v) = self.axes();
        let mut slab = self.beyond();
        let b = nb.fine_box();
        for a in [u, v] {
            slab.min[a] = slab.min[a].max(b.min[a]);
            slab.max[a] = slab.max[a].min(b.max[a]);
        }
        slab
    }
}

/// A shape's triangles lying on its own box face `k`, as counter-clockwise
/// triangles in 64ths on that plane.
fn faces_on(shape: Shape, at: NodeBox, k: usize) -> Vec<[P2; 3]> {
    let axis = k / 2;
    let level = if k % 2 == 1 { 8 } else { 0 };
    let (u, v) = ((axis + 1) % 3, (axis + 2) % 3);
    let size = i128::from(at.size);
    let point = |p: [i32; 3]| {
        [
            i128::from(at.min[u]) * 8 + i128::from(p[u]) * size,
            i128::from(at.min[v]) * 8 + i128::from(p[v]) * size,
        ]
    };
    lattice::triangles(shape)
        .as_slice()
        .iter()
        .filter(|(t, _)| t.iter().all(|p| p[axis] == level))
        .filter_map(|(t, _)| counter_clockwise(t.map(point)))
        .collect()
}

/// Which box face a lattice triangle lies on, if any.
fn lattice_face(t: [[i32; 3]; 3]) -> Option<usize> {
    (0..3).find_map(|axis| {
        [0, 8]
            .into_iter()
            .position(|level| t.iter().all(|p| p[axis] == level))
            .map(|side| axis * 2 + side)
    })
}

/// What the solids beyond a face are asked to cover.
enum Region<'a> {
    /// The whole face square of a full leaf.
    Square,
    /// An exact triangle of a plain shape, counter-clockwise in 64ths.
    Exact([P2; 3]),
    /// A piece's triangle, known by its bounding rectangle in eighths. A cap
    /// is where the piece's source continues beyond the face.
    Piece {
        min: [f64; 2],
        max: [f64; 2],
        cap: bool,
        source: &'a Source,
    },
}

impl Octree {
    /// The side of a mesh unit in eighths: `MESH_UNIT_SIZE`, or the root when
    /// the root is smaller.
    fn unit_size(&self) -> u32 {
        UNIT.min(self.root_box().size)
    }

    /// The key of the mesh unit holding a point of the grid: its minimum
    /// corner in eighths of a metre.
    pub(crate) fn unit_key(&self, min: [i32; 3]) -> ChunkKey {
        let root = self.root_box().min;
        let size = self.unit_size() as i32;
        std::array::from_fn(|a| root[a] + (min[a] - root[a]).div_euclid(size) * size)
    }

    /// Every mesh unit that owns a leaf with solid geometry.
    pub(crate) fn mesh_keys(&self) -> Vec<ChunkKey> {
        let mut keys = std::collections::BTreeSet::new();
        self.for_each_leaf(|at, leaf| {
            if leaf.geometry != super::LeafGeometry::Empty {
                keys.insert(self.unit_key(at.min));
            }
        });
        keys.into_iter().collect()
    }

    /// The opaque mesh of one unit.
    pub(crate) fn mesh_unit(&self, key: ChunkKey) -> Mesh {
        let mut mesh = Mesh::default();
        let unit = NodeBox {
            min: key,
            size: self.unit_size(),
        };
        if !self.is_node(unit) {
            return mesh;
        }
        let mut owned = Vec::new();
        collect_owned(self.root_node(), self.root_box(), unit, &mut owned);
        for (at, node) in owned {
            self.emit_leaf(at, node, &mut mesh);
        }
        mesh
    }

    fn emit_leaf(&self, at: NodeBox, node: &Node, mesh: &mut Mesh) {
        match node {
            Node::Full(faces, _) => {
                for (k, &material) in faces.iter().enumerate() {
                    if self.covered(at, k, &Region::Square) {
                        continue;
                    }
                    for (t, face) in lattice::triangles(Shape::solid()).as_slice() {
                        if *face as usize == k {
                            append_triangle(mesh, t.map(|p| lattice_point(at, p)), material);
                        }
                    }
                }
            }
            Node::Shaped(s) => {
                let size = i128::from(at.size);
                for (t, face) in lattice::triangles(s.shape).as_slice() {
                    if let Some(k) = lattice_face(*t) {
                        let (u, v) = ((k / 2 + 1) % 3, (k / 2 + 2) % 3);
                        let flat = t.map(|p| {
                            [
                                i128::from(at.min[u]) * 8 + i128::from(p[u]) * size,
                                i128::from(at.min[v]) * 8 + i128::from(p[v]) * size,
                            ]
                        });
                        if let Some(flat) = counter_clockwise(flat) {
                            if self.covered(at, k, &Region::Exact(flat)) {
                                continue;
                            }
                        }
                    }
                    append_triangle(
                        mesh,
                        t.map(|p| lattice_point(at, p)),
                        s.faces[*face as usize],
                    );
                }
            }
            Node::Piece(p) => {
                let b = at.fine_box();
                for (t, face, cap) in piece_triangles(&p.source, b) {
                    if let Some(k) = box_face(t, b) {
                        let (u, v) = ((k / 2 + 1) % 3, (k / 2 + 2) % 3);
                        let fine = t.map(|q| q * 8.0);
                        let min = [
                            fine.iter().map(|q| q[u]).fold(f64::INFINITY, f64::min),
                            fine.iter().map(|q| q[v]).fold(f64::INFINITY, f64::min),
                        ];
                        let max = [
                            fine.iter().map(|q| q[u]).fold(f64::NEG_INFINITY, f64::max),
                            fine.iter().map(|q| q[v]).fold(f64::NEG_INFINITY, f64::max),
                        ];
                        let region = Region::Piece {
                            min,
                            max,
                            cap,
                            source: &p.source,
                        };
                        if self.covered(at, k, &region) {
                            continue;
                        }
                    }
                    append_triangle(mesh, t.map(|q| q.as_vec3()), p.faces[face as usize]);
                }
            }
            _ => {}
        }
    }

    /// Whether the solids beyond face `k` of a leaf cover a region of it.
    fn covered(&self, at: NodeBox, k: usize, region: &Region) -> bool {
        let face = Face::of(at, k);
        let root = self.root_box().fine_box();
        let edge = if face.positive {
            root.max[face.axis]
        } else {
            root.min[face.axis]
        };
        if face.plane == edge {
            return true;
        }
        let beyond = face.beyond();
        // Usually one leaf lies beyond the whole face: find it with one
        // descent before listing every neighbour.
        let mut centre = [0.0; 3];
        for (a, c) in centre.iter_mut().enumerate() {
            *c = (f64::from(beyond.min[a]) + f64::from(beyond.max[a])) * 0.5;
        }
        if let Some((nb, neighbour)) = self.leaf_at_fine(centre) {
            if nb.fine_box().contains(beyond) {
                return self.neighbour_covers(face, k, region, nb, neighbour);
            }
        }
        let mut covered = true;
        self.visit_all(beyond, &mut |nb, neighbour| {
            covered = self.neighbour_covers(face, k, region, nb, neighbour);
            covered
        });
        covered
    }

    /// Whether one neighbour beyond a face covers the part of a region
    /// over its footprint.
    fn neighbour_covers(
        &self,
        face: Face,
        k: usize,
        region: &Region,
        nb: NodeBox,
        neighbour: &Node,
    ) -> bool {
        let r = face.footprint(nb);
        let meets = match region {
            Region::Square => true,
            Region::Exact(t) => interiors_meet(*t, r),
            Region::Piece { min, max, .. } => {
                (0..2).all(|a| max[a] * 8.0 > r.min[a] as f64 && min[a] * 8.0 < r.max[a] as f64)
            }
        };
        if !meets {
            return true;
        }
        match neighbour {
            Node::Full(..) => true,
            Node::Empty(_) => match region {
                Region::Piece {
                    cap: true, source, ..
                } => lattice::classify(source, nb.min, nb.size as i32) == Class::Outside,
                _ => false,
            },
            Node::Piece(other) => {
                let same = matches!(region, Region::Piece { cap: true, source, .. } if **source == other.source);
                same || lattice::classify_box(&other.source, face.slab_over(nb)) == Class::Inside
            }
            Node::Shaped(other) => {
                let faces = faces_on(other.shape, nb, k ^ 1);
                let points = match region {
                    Region::Exact(t) => intersection_points(*t, r),
                    _ => r
                        .corners()
                        .into_iter()
                        .map(|c| Q {
                            x: c[0],
                            y: c[1],
                            d: 1,
                        })
                        .collect(),
                };
                faces_hold(&faces, &points)
            }
            Node::Branch(_) => unreachable!("only leaves are visited"),
        }
    }
}

/// The leaves with solid geometry whose minimum corner lies in a unit.
fn collect_owned<'a>(
    node: &'a Node,
    at: NodeBox,
    unit: NodeBox,
    out: &mut Vec<(NodeBox, &'a Node)>,
) {
    if at.size > unit.size {
        match node {
            Node::Branch(children) => {
                let index = at.child_index(unit);
                collect_owned(&children[index], at.child(index), unit, out);
            }
            leaf => {
                if at.min == unit.min && leaf.has_geometry() {
                    out.push((at, leaf));
                }
            }
        }
        return;
    }
    match node {
        Node::Branch(children) => {
            for (i, child) in children.iter().enumerate() {
                collect_owned(child, at.child(i), unit, out);
            }
        }
        leaf => {
            if leaf.has_geometry() {
                out.push((at, leaf));
            }
        }
    }
}

fn lattice_point(at: NodeBox, p: [i32; 3]) -> Vec3 {
    let c = |a: usize| {
        let v = i64::from(at.min[a]) * 8 + i64::from(p[a]) * i64::from(at.size);
        (v as f64 / 64.0) as f32
    };
    Vec3::new(c(0), c(1), c(2))
}

/// Which face of a leaf's box a piece's triangle lies on, facing out.
fn box_face(t: [DVec3; 3], b: FineBox) -> Option<usize> {
    let normal = (t[1] - t[0]).cross(t[2] - t[0]);
    (0..6).find(|&k| {
        let axis = k / 2;
        let plane = f64::from(if k % 2 == 1 { b.max[axis] } else { b.min[axis] }) / 8.0;
        let outward = if k % 2 == 1 { 1.0 } else { -1.0 };
        t.iter().all(|p| (p[axis] - plane).abs() < CLIP_EPS) && normal[axis] * outward > 0.0
    })
}

pub(crate) fn append_triangle(mesh: &mut Mesh, triangle: [Vec3; 3], material: u8) {
    let cross = (triangle[1] - triangle[0]).cross(triangle[2] - triangle[0]);
    if cross.length_squared() <= 1.0e-16 {
        return;
    }
    let normal = cross.normalize().to_array();
    let start = mesh.vertices.len() as u32;
    mesh.vertices.extend(triangle.map(|position| Vertex {
        position: position.to_array(),
        normal,
        color: MATERIAL_COLORS[material as usize],
        material,
    }));
    mesh.indices.extend([start, start + 1, start + 2]);
}

// ---------------------------------------------------------------------------
// A piece's boundary: its source's triangles clipped to the leaf's box, with
// caps where the source's solid crosses the box. The caps follow the cut
// loops the clipped triangles leave on each plane, as subdivision did before
// the tree.

type DTriangle = ([DVec3; 3], u8, bool);

std::thread_local! {
    /// Pieces this thread drew without caps because a cap could not be built.
    static CAP_FALLBACKS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// How many pieces this thread has drawn without caps because a cap could
/// not be built.
#[cfg(test)]
pub(crate) fn cap_fallbacks() -> usize {
    CAP_FALLBACKS.with(std::cell::Cell::get)
}

/// A piece's triangles in metres, each with its logical face and whether it
/// is a cap. Falls back to the clipped source without caps if a cap cannot be
/// built.
pub(crate) fn piece_triangles(source: &Source, bounds: FineBox) -> Vec<DTriangle> {
    let origin = DVec3::from_array(source.origin.map(|v| f64::from(v) / 8.0));
    let scale = f64::from(source.size) / 64.0;
    let mut triangles: Vec<DTriangle> = lattice::triangles(source.shape)
        .as_slice()
        .iter()
        .map(|(t, face)| {
            (
                t.map(|p| origin + DVec3::from_array(p.map(f64::from)) * scale),
                *face,
                false,
            )
        })
        .collect();
    let plain = triangles.clone();
    for axis in 0..3 {
        for (plane, keep_sign, face) in [
            (f64::from(bounds.min[axis]) / 8.0, 1.0, (axis * 2) as u8),
            (
                f64::from(bounds.max[axis]) / 8.0,
                -1.0,
                (axis * 2 + 1) as u8,
            ),
        ] {
            match clip_plane(triangles, axis, plane, keep_sign, face) {
                Ok(next) => triangles = next,
                Err(_) => {
                    CAP_FALLBACKS.with(|count| count.set(count.get() + 1));
                    return plain
                        .iter()
                        .flat_map(|&(t, face, _)| {
                            clip_triangles(vec![t], bounds)
                                .into_iter()
                                .map(move |t| (t, face, false))
                        })
                        .collect();
                }
            }
        }
    }
    triangles
}

fn same_point(a: DVec3, b: DVec3) -> bool {
    a.distance_squared(b) < CLIP_EPS * CLIP_EPS
}

fn clip_plane(
    triangles: Vec<DTriangle>,
    axis: usize,
    plane: f64,
    keep_sign: f64,
    cap_face: u8,
) -> Result<Vec<DTriangle>, String> {
    let mut result = Vec::new();
    let mut edges = Vec::new();
    for (triangle, face, cap) in triangles {
        let distances = triangle.map(|p| (p[axis] - plane) * keep_sign);
        if distances.iter().all(|&d| d >= -CLIP_EPS) {
            for i in 0..3 {
                let a = triangle[i];
                let b = triangle[(i + 1) % 3];
                if (a[axis] - plane).abs() < CLIP_EPS
                    && (b[axis] - plane).abs() < CLIP_EPS
                    && !same_point(a, b)
                {
                    edges.push((b, a));
                }
            }
            result.push((triangle, face, cap));
            continue;
        }
        if distances.iter().all(|&d| d <= CLIP_EPS) {
            continue;
        }
        let mut polygon = Vec::with_capacity(4);
        for i in 0..3 {
            let j = (i + 1) % 3;
            let a = triangle[i];
            let b = triangle[j];
            let da = distances[i];
            let db = distances[j];
            if da >= -CLIP_EPS {
                polygon.push(a);
            }
            if (da > CLIP_EPS && db < -CLIP_EPS) || (da < -CLIP_EPS && db > CLIP_EPS) {
                let mut p = a.lerp(b, da / (da - db));
                p[axis] = plane;
                polygon.push(p);
            }
        }
        if polygon.len() < 3 {
            continue;
        }
        for i in 0..polygon.len() {
            let a = polygon[i];
            let b = polygon[(i + 1) % polygon.len()];
            if (a[axis] - plane).abs() < CLIP_EPS
                && (b[axis] - plane).abs() < CLIP_EPS
                && !same_point(a, b)
            {
                edges.push((b, a));
            }
        }
        for i in 1..polygon.len() - 1 {
            let t = [polygon[0], polygon[i], polygon[i + 1]];
            if (t[1] - t[0]).cross(t[2] - t[0]).length_squared()
                > (CLIP_EPS * CLIP_EPS) * (CLIP_EPS * CLIP_EPS)
            {
                result.push((t, face, cap));
            }
        }
    }
    // Split collinear segments at every endpoint before cancelling opposite
    // copies: the triangulation can put a T junction on a cut plane.
    let endpoints: Vec<_> = edges.iter().flat_map(|&(a, b)| [a, b]).collect();
    let mut pieces = Vec::new();
    for (a, b) in edges {
        let d = b - a;
        let length = d.length_squared();
        let mut cuts = vec![0.0, 1.0];
        for &p in &endpoints {
            let t = (p - a).dot(d) / length;
            if t > CLIP_EPS && t < 1.0 - CLIP_EPS && same_point(a + d * t, p) {
                cuts.push(t);
            }
        }
        cuts.sort_by(f64::total_cmp);
        cuts.dedup_by(|a, b| (*a - *b).abs() < CLIP_EPS);
        for pair in cuts.windows(2) {
            pieces.push((a + d * pair[0], a + d * pair[1]));
        }
    }
    let mut edges: Vec<(DVec3, DVec3)> = Vec::new();
    for (a, b) in pieces {
        if let Some(index) = edges
            .iter()
            .position(|&(c, d)| same_point(a, d) && same_point(b, c))
        {
            edges.swap_remove(index);
        } else {
            edges.push((a, b));
        }
    }
    if edges.is_empty() {
        return Ok(result);
    }
    let u = (axis + 1) % 3;
    let v = (axis + 2) % 3;
    let mut loops: Vec<Vec<DVec3>> = Vec::new();
    while let Some((start, mut end)) = edges.pop() {
        let mut points = vec![start];
        let mut previous = start;
        let mut guard = 0;
        while !same_point(end, start) {
            points.push(end);
            guard += 1;
            if guard > 256 {
                return Err("A cap exceeded its boundary budget.".into());
            }
            let incoming = end - previous;
            let chosen = edges
                .iter()
                .enumerate()
                .filter(|(_, e)| same_point(e.0, end))
                .max_by(|(_, a), (_, b)| {
                    let turn = |to: DVec3| {
                        let outgoing = to - end;
                        let cross = incoming[u] * outgoing[v] - incoming[v] * outgoing[u];
                        libm::atan2(
                            -keep_sign * cross,
                            incoming[u] * outgoing[u] + incoming[v] * outgoing[v],
                        )
                    };
                    turn(a.1).total_cmp(&turn(b.1))
                })
                .map(|(index, _)| index)
                .ok_or_else(|| "A cap boundary did not close.".to_string())?;
            let next = edges.swap_remove(chosen);
            previous = end;
            end = next.1;
        }
        if points.len() >= 3 {
            loops.push(points);
        }
    }
    let area = |points: &[DVec3]| {
        points
            .iter()
            .zip(points.iter().cycle().skip(1))
            .take(points.len())
            .map(|(a, b)| a[u] * b[v] - b[u] * a[v])
            .sum::<f64>()
            * 0.5
    };
    let areas: Vec<_> = loops.iter().map(|p| area(p)).collect();
    let outer: Vec<_> = (0..loops.len())
        .filter(|&i| areas[i] * (-keep_sign) > CLIP_EPS * CLIP_EPS)
        .collect();
    let holes: Vec<_> = (0..loops.len())
        .filter(|&i| areas[i] * (-keep_sign) < -CLIP_EPS * CLIP_EPS)
        .collect();
    if outer.is_empty() {
        return Err("A cap has no exterior boundary.".into());
    }
    let contains = |ring: &[DVec3], point: DVec3| {
        let mut inside = false;
        for i in 0..ring.len() {
            let a = ring[i];
            let b = ring[(i + 1) % ring.len()];
            if (a[v] > point[v]) != (b[v] > point[v])
                && point[u] < (b[u] - a[u]) * (point[v] - a[v]) / (b[v] - a[v]) + a[u]
            {
                inside = !inside;
            }
        }
        inside
    };
    let mut assignments = vec![Vec::new(); loops.len()];
    for h in holes {
        let owner = outer
            .iter()
            .copied()
            .filter(|&o| contains(&loops[o], loops[h][0]))
            .min_by(|&a, &b| areas[a].abs().total_cmp(&areas[b].abs()))
            .ok_or_else(|| "A cap hole has no exterior.".to_string())?;
        assignments[owner].push(h);
    }
    for o in outer {
        let mut points = loops[o].clone();
        let mut hole_indices = Vec::new();
        let mut expected_area = areas[o].abs();
        for &h in &assignments[o] {
            hole_indices.push(points.len());
            points.extend_from_slice(&loops[h]);
            expected_area -= areas[h].abs();
        }
        let coordinates: Vec<_> = points.iter().flat_map(|p| [p[u], p[v]]).collect();
        let indices = earcutr::earcut(&coordinates, &hole_indices, 2)
            .map_err(|e| format!("A cap could not be triangulated: {e:?}"))?;
        let mut actual_area = 0.0;
        for ids in indices.as_chunks::<3>().0 {
            let mut t = [points[ids[0]], points[ids[1]], points[ids[2]]];
            let cross = (t[1] - t[0]).cross(t[2] - t[0]);
            actual_area += cross[axis].abs() * 0.5;
            if cross[axis] * (-keep_sign) < 0.0 {
                t.swap(1, 2);
            }
            result.push((t, cap_face, true));
        }
        if (actual_area - expected_area).abs() > 1.0e-8_f64.max(expected_area * 1.0e-7) {
            return Err("A cap's triangles did not keep its area.".into());
        }
    }
    Ok(result)
}
