//! Queries on the tree, with `World`'s meaning. Point, box and ray queries walk
//! the tree and skip empty space; each leaf kind is handled once. Exact pieces
//! are queried analytically: the source's own triangles limited to the leaf's
//! box, and the leaf's faces where the source's solid crosses them, without
//! building cap polygons.
//!
//! Floating-point steps run on each leaf's exact triangles in a fixed order,
//! with the arithmetic of the compact-region queries, so a plain leaf answers
//! as an imported region does. Containment is exact integer arithmetic.
//! Nothing calls a system math function, so every build gives the same bits.

use super::format3::cell_for;
use super::lattice;
use super::{relation, volume_material_of, Node, NodeBox, Octree, Relation, Source};
use crate::{
    CellFragment, CellPos, FineBox, RayHit, RootEdit, Shape, Vec3, VolumeMaterial, MAX_QUERY_ROOTS,
};
use glam::DVec3;
use std::collections::BTreeSet;

const EPS: f32 = 1.0e-5;
/// Node boxes grow by this much, in metres, when a ray decides which nodes to
/// visit: triangle tests accept hits a little past their edges.
const RAY_MARGIN: f64 = 1.0e-4;
const CLIP_EPS: f64 = 1.0e-9;

fn world_point(origin: [i32; 3], size: u32, p: [i32; 3]) -> Vec3 {
    let c = |a: usize| {
        let v = i64::from(origin[a]) * 8 + i64::from(p[a]) * i64::from(size);
        (v as f64 / 64.0) as f32
    };
    Vec3::new(c(0), c(1), c(2))
}

/// The shape a leaf draws its solid from, at its own position and size.
fn leaf_source(node: &Node, at: NodeBox) -> Option<Source> {
    match node {
        Node::Full(..) => Some(Source {
            origin: at.min,
            size: at.size,
            shape: Shape::solid(),
        }),
        Node::Shaped(s) => Some(Source {
            origin: at.min,
            size: at.size,
            shape: s.shape,
        }),
        Node::Piece(p) => Some(p.source),
        _ => None,
    }
}

/// A source's boundary in world coordinates, in `shape_triangles` order.
fn source_triangles(source: &Source) -> Vec<([Vec3; 3], u8)> {
    lattice::triangles(source.shape)
        .as_slice()
        .iter()
        .map(|(t, face)| (t.map(|p| world_point(source.origin, source.size, p)), *face))
        .collect()
}

fn fine_point(point: Vec3) -> [f64; 3] {
    (point.as_dvec3() * 8.0).to_array()
}

fn closed_contains(at: NodeBox, fine: [f64; 3]) -> bool {
    let b = at.fine_box();
    (0..3).all(|a| f64::from(b.min[a]) <= fine[a] && fine[a] <= f64::from(b.max[a]))
}

fn point_epsilon(point: Vec3) -> f32 {
    (point.abs().max_element() * f32::EPSILON * 2.0).max(EPS * 8.0)
}

// ---------------------------------------------------------------------------
// Floating-point steps of the compact-region queries, so equal triangles give
// equal bits.

/// Heights of a triangle clipped to a body's horizontal footprint.
fn height_interval(triangle: [Vec3; 3], min: Vec3, max: Vec3) -> Option<(f32, f32)> {
    let mut polygon = [Vec3::ZERO; 8];
    polygon[..3].copy_from_slice(&triangle);
    let mut len = 3;
    for (axis, plane, sign) in [
        (0, min.x, 1.0),
        (0, max.x, -1.0),
        (2, min.z, 1.0),
        (2, max.z, -1.0),
    ] {
        let mut output = [Vec3::ZERO; 8];
        let mut count = 0;
        for i in 0..len {
            let a = polygon[i];
            let b = polygon[(i + 1) % len];
            let da = (a[axis] - plane) * sign;
            let db = (b[axis] - plane) * sign;
            if da >= 0.0 {
                output[count] = a;
                count += 1;
            }
            if (da > 0.0 && db < 0.0) || (da < 0.0 && db > 0.0) {
                output[count] = a.lerp(b, da / (da - db));
                count += 1;
            }
        }
        polygon = output;
        len = count;
        if len == 0 {
            return None;
        }
    }
    Some(
        polygon[..len]
            .iter()
            .fold((f32::INFINITY, f32::NEG_INFINITY), |(lo, hi), p| {
                (lo.min(p.y), hi.max(p.y))
            }),
    )
}

fn accumulate(
    triangle: [Vec3; 3],
    min: Vec3,
    max: Vec3,
    low: f32,
    high: f32,
    ceiling: bool,
    result: &mut Option<f32>,
) {
    let [a, b, c] = triangle;
    let normal = (b - a).cross(c - a);
    if if ceiling {
        normal.y >= -1.0e-9
    } else {
        normal.y <= 1.0e-9
    } {
        return;
    }
    // Work near the player rather than at a far-away map origin.
    let origin = Vec3::new(min.x, low, min.z);
    let Some((bottom, top)) = height_interval(
        triangle.map(|point| point - origin),
        min - origin,
        max - origin,
    ) else {
        return;
    };
    let height = origin.y + if ceiling { bottom } else { top };
    take(height, low, high, ceiling, result);
}

fn take(height: f32, low: f32, high: f32, ceiling: bool, result: &mut Option<f32>) {
    if height < low || height > high {
        return;
    }
    *result = Some(result.map_or(height, |previous| {
        if ceiling {
            previous.min(height)
        } else {
            previous.max(height)
        }
    }));
}

fn ray_triangle(origin: Vec3, direction: Vec3, [a, b, c]: [Vec3; 3]) -> Option<f64> {
    let [a, b, c] = [a, b, c].map(|p| p.as_dvec3());
    let direction = direction.as_dvec3();
    let edge1 = b - a;
    let edge2 = c - a;
    let p = direction.cross(edge2);
    let det = edge1.dot(p);
    let area = edge1.cross(edge2).length();
    if area < 1.0e-12 || det.abs() < area * 1.0e-12 {
        return None;
    }
    let inv = 1.0 / det;
    let t = origin.as_dvec3() - a;
    let u = t.dot(p) * inv;
    let q = t.cross(edge1);
    let v = direction.dot(q) * inv;
    let tolerance = EPS as f64 / area;
    if u < -tolerance * edge2.length()
        || v < -tolerance * edge1.length()
        || u + v > 1.0 + tolerance * (c - b).length()
    {
        return None;
    }
    let distance = edge2.dot(q) * inv;
    (distance >= -(EPS as f64)).then_some(distance.max(0.0))
}

fn triangle_intersects_box(triangle: [Vec3; 3], min: Vec3, max: Vec3) -> bool {
    let center = (min + max) * 0.5;
    let half = (max - min) * 0.5;
    let t = triangle.map(|p| p - center);
    let edges = [t[1] - t[0], t[2] - t[1], t[0] - t[2]];
    let separated = |axis: Vec3| {
        if axis.length_squared() < 1.0e-14 {
            return false;
        }
        let radius = half.dot(axis.abs());
        let a = t.map(|p| p.dot(axis));
        a.into_iter().fold(f32::INFINITY, f32::min) > radius
            || a.into_iter().fold(f32::NEG_INFINITY, f32::max) < -radius
    };
    for axis in [Vec3::X, Vec3::Y, Vec3::Z, edges[0].cross(edges[1])] {
        if separated(axis) {
            return false;
        }
    }
    for edge in edges {
        for axis in [Vec3::X, Vec3::Y, Vec3::Z] {
            if separated(edge.cross(axis)) {
                return false;
            }
        }
    }
    true
}

/// The slab test `World::raycast_volumes` uses.
fn ray_box_interval(origin: Vec3, direction: Vec3, min: Vec3, max: Vec3) -> Option<(f32, f32)> {
    let mut enter = f32::NEG_INFINITY;
    let mut exit = f32::INFINITY;
    for axis in 0..3 {
        if direction[axis] == 0.0 {
            if origin[axis] < min[axis] || origin[axis] > max[axis] {
                return None;
            }
        } else {
            let a = (min[axis] - origin[axis]) / direction[axis];
            let b = (max[axis] - origin[axis]) / direction[axis];
            enter = enter.max(a.min(b));
            exit = exit.min(a.max(b));
            if enter > exit {
                return None;
            }
        }
    }
    if exit < 0.0 {
        None
    } else {
        Some((enter, exit))
    }
}

/// A box overlaps a source's solid by more than the contact tolerance, in
/// coordinates scaled to the source. Full solids use the full-cell test.
fn source_overlaps_box(source: &Source, min: Vec3, max: Vec3) -> bool {
    let origin = Vec3::from_array(source.origin.map(|v| v as f32 / 8.0));
    let size = source.size as f32 / 8.0;
    let epsilon = EPS / size;
    let min = (min - origin) / size + Vec3::splat(epsilon);
    let max = (max - origin) / size - Vec3::splat(epsilon);
    if min.cmpge(max).any() || max.cmple(Vec3::ZERO).any() || min.cmpge(Vec3::ONE).any() {
        return false;
    }
    if lattice::is_solid(source.shape) {
        let x0 = min.x.max(0.0);
        let x1 = max.x.min(1.0);
        let z0 = min.z.max(0.0);
        let z1 = max.z.min(1.0);
        if x1 - x0 <= epsilon || z1 - z0 <= epsilon {
            return false;
        }
        return 1.0 > epsilon && min.y < 1.0 && max.y > 0.0;
    }
    let triangles = lattice::triangles(source.shape);
    for (t, _) in triangles.as_slice() {
        let triangle = t.map(|p| Vec3::from_array(p.map(|v| v as f32 * 0.125)));
        if triangle_intersects_box(triangle, min, max) {
            return true;
        }
    }
    // No boundary crosses the box: its centre decides, exactly.
    let centre = (min + max) * 0.5;
    let fine = std::array::from_fn(|a| {
        f64::from(source.origin[a]) + f64::from(centre[a]) * f64::from(source.size)
    });
    lattice::source_contains_fine(source, fine)
}

/// Clips triangles to a box, as fragment clipping does, without caps.
fn clip_triangles(triangles: Vec<[DVec3; 3]>, bounds: FineBox) -> Vec<[DVec3; 3]> {
    let mut triangles = triangles;
    let floor = CLIP_EPS * CLIP_EPS;
    for axis in 0..3 {
        for (plane, keep) in [
            (f64::from(bounds.min[axis]) / 8.0, 1.0),
            (f64::from(bounds.max[axis]) / 8.0, -1.0),
        ] {
            let mut result = Vec::with_capacity(triangles.len());
            for triangle in triangles {
                let d = triangle.map(|p| (p[axis] - plane) * keep);
                if d.iter().all(|&v| v >= -CLIP_EPS) {
                    result.push(triangle);
                    continue;
                }
                if d.iter().all(|&v| v <= CLIP_EPS) {
                    continue;
                }
                let mut polygon = Vec::with_capacity(4);
                for i in 0..3 {
                    let j = (i + 1) % 3;
                    if d[i] >= -CLIP_EPS {
                        polygon.push(triangle[i]);
                    }
                    if (d[i] > CLIP_EPS && d[j] < -CLIP_EPS)
                        || (d[i] < -CLIP_EPS && d[j] > CLIP_EPS)
                    {
                        let mut p = triangle[i].lerp(triangle[j], d[i] / (d[i] - d[j]));
                        p[axis] = plane;
                        polygon.push(p);
                    }
                }
                if polygon.len() < 3 {
                    continue;
                }
                for i in 1..polygon.len() - 1 {
                    let t = [polygon[0], polygon[i], polygon[i + 1]];
                    if (t[1] - t[0]).cross(t[2] - t[0]).length_squared() > floor * floor {
                        result.push(t);
                    }
                }
            }
            triangles = result;
        }
    }
    triangles
}

fn segment_meets_rectangle(a: [f64; 2], b: [f64; 2], min: [f64; 2], max: [f64; 2]) -> bool {
    let (mut t0, mut t1) = (0.0f64, 1.0f64);
    for axis in 0..2 {
        let d = b[axis] - a[axis];
        for (bound, sign) in [(min[axis], -1.0), (max[axis], 1.0)] {
            // Inside means sign * (p - bound) <= 0.
            let p = sign * d;
            let q = sign * (bound - a[axis]);
            if p == 0.0 {
                if q < 0.0 {
                    return false;
                }
            } else {
                let r = q / p;
                if p < 0.0 {
                    t0 = t0.max(r);
                } else {
                    t1 = t1.min(r);
                }
            }
        }
    }
    t0 <= t1
}

fn triangle_meets_rectangle(t: [[f64; 2]; 3], min: [f64; 2], max: [f64; 2]) -> bool {
    for axis in 0..2 {
        if t.iter().all(|p| p[axis] < min[axis]) || t.iter().all(|p| p[axis] > max[axis]) {
            return false;
        }
    }
    let corners = [min, [max[0], min[1]], max, [min[0], max[1]]];
    for i in 0..3 {
        let a = t[i];
        let b = t[(i + 1) % 3];
        let c = t[(i + 2) % 3];
        let n = [a[1] - b[1], b[0] - a[0]];
        let side = |p: [f64; 2]| n[0] * (p[0] - a[0]) + n[1] * (p[1] - a[1]);
        let inner = side(c);
        if inner == 0.0 {
            continue;
        }
        if corners.iter().all(|&p| side(p) * inner < 0.0) {
            return false;
        }
    }
    true
}

/// Whether a source's closed solid meets a horizontal rectangle at height `y`
/// (world metres).
fn solid_meets_slice(source: &Source, y: f64, min: [f64; 2], max: [f64; 2]) -> bool {
    for (t, _) in source_triangles(source) {
        let t = t.map(|p| p.as_dvec3());
        let below = t.iter().filter(|p| p.y < y).count();
        let above = t.iter().filter(|p| p.y > y).count();
        if below == 3 || above == 3 {
            continue;
        }
        if below == 0 && above == 0 {
            if triangle_meets_rectangle(t.map(|p| [p.x, p.z]), min, max) {
                return true;
            }
            continue;
        }
        let mut points = Vec::with_capacity(3);
        for i in 0..3 {
            let (a, b) = (t[i], t[(i + 1) % 3]);
            if a.y == y {
                points.push([a.x, a.z]);
            }
            if (a.y < y && b.y > y) || (a.y > y && b.y < y) {
                let s = (y - a.y) / (b.y - a.y);
                points.push([a.x + (b.x - a.x) * s, a.z + (b.z - a.z) * s]);
            }
        }
        for i in 0..points.len() {
            for j in i..points.len() {
                if segment_meets_rectangle(points[i], points[j], min, max) {
                    return true;
                }
            }
        }
    }
    lattice::source_contains_fine(source, [min[0] * 8.0, y * 8.0, min[1] * 8.0])
}

// ---------------------------------------------------------------------------
// Walking the tree.

struct Ray {
    origin: DVec3,
    direction: DVec3,
    limit: f64,
}

impl Ray {
    fn interval(&self, at: NodeBox) -> Option<(f64, f64)> {
        let b = at.fine_box();
        let mut enter = f64::NEG_INFINITY;
        let mut exit = f64::INFINITY;
        for axis in 0..3 {
            let min = f64::from(b.min[axis]) / 8.0 - RAY_MARGIN;
            let max = f64::from(b.max[axis]) / 8.0 + RAY_MARGIN;
            let (o, d) = (self.origin[axis], self.direction[axis]);
            if d == 0.0 {
                if o < min || o > max {
                    return None;
                }
            } else {
                let a = (min - o) / d;
                let c = (max - o) / d;
                enter = enter.max(a.min(c));
                exit = exit.min(a.max(c));
            }
        }
        (enter <= exit && exit >= 0.0 && enter <= self.limit).then_some((enter, exit))
    }
}

/// Visits leaves along a ray front to back, skipping nodes that start past the
/// nearest hit so far.
fn walk_ray(
    node: &Node,
    at: NodeBox,
    ray: &Ray,
    want: &dyn Fn(&Node) -> bool,
    nearest: &mut f64,
    visit: &mut dyn FnMut(NodeBox, &Node, &mut f64),
) {
    match node {
        Node::Branch(children) => {
            let mut order = [(0.0f64, 0usize); 8];
            let mut count = 0;
            for (i, child) in children.iter().enumerate() {
                if child.is_leaf() && !want(child) {
                    continue;
                }
                if let Some((enter, _)) = ray.interval(at.child(i)) {
                    order[count] = (enter, i);
                    count += 1;
                }
            }
            order[..count].sort_by(|a, b| a.0.total_cmp(&b.0));
            for &(enter, i) in &order[..count] {
                if enter > *nearest {
                    break;
                }
                walk_ray(&children[i], at.child(i), ray, want, nearest, visit);
            }
        }
        leaf => {
            if want(leaf) {
                visit(at, leaf, nearest);
            }
        }
    }
}

impl Octree {
    /// Leaves meeting a fine box with positive volume; plain air is skipped.
    pub(crate) fn visit_box(&self, query: FineBox, visit: &mut dyn FnMut(NodeBox, &Node) -> bool) {
        fn walk(
            node: &Node,
            at: NodeBox,
            query: FineBox,
            visit: &mut dyn FnMut(NodeBox, &Node) -> bool,
        ) -> bool {
            if relation(at, query) == Relation::Disjoint {
                return true;
            }
            match node {
                Node::Branch(children) => children
                    .iter()
                    .enumerate()
                    .all(|(i, child)| walk(child, at.child(i), query, visit)),
                leaf if leaf.is_air() => true,
                leaf => visit(at, leaf),
            }
        }
        walk(self.root_node(), self.root_box(), query, visit);
    }

    /// Every leaf, air included, meeting a fine box with positive volume.
    fn visit_all(&self, query: FineBox, visit: &mut dyn FnMut(NodeBox, &Node) -> bool) {
        fn walk(
            node: &Node,
            at: NodeBox,
            query: FineBox,
            visit: &mut dyn FnMut(NodeBox, &Node) -> bool,
        ) -> bool {
            if relation(at, query) == Relation::Disjoint {
                return true;
            }
            match node {
                Node::Branch(children) => children
                    .iter()
                    .enumerate()
                    .all(|(i, child)| walk(child, at.child(i), query, visit)),
                leaf => visit(at, leaf),
            }
        }
        walk(self.root_node(), self.root_box(), query, visit);
    }

    /// The leaf whose half-open box holds a point given in eighths.
    pub(crate) fn leaf_at_fine(&self, fine: [f64; 3]) -> Option<(NodeBox, &Node)> {
        let mut at = self.root_box();
        let b = at.fine_box();
        if (0..3).any(|a| fine[a] < f64::from(b.min[a]) || fine[a] >= f64::from(b.max[a])) {
            return None;
        }
        let mut node = self.root_node();
        while let Node::Branch(children) = node {
            let half = (at.size / 2) as i32;
            let index = (0..3)
                .map(|a| usize::from(fine[a] >= f64::from(at.min[a] + half)) << a)
                .sum();
            node = &children[index];
            at = at.child(index);
        }
        Some((at, node))
    }

    /// Whether a world point lies in solid geometry, boundary included.
    pub fn contains_point(&self, point: Vec3) -> bool {
        point.is_finite() && self.contains_fine(fine_point(point))
    }

    /// The same for a point in eighths of a metre.
    fn contains_fine(&self, fine: [f64; 3]) -> bool {
        fn walk(node: &Node, at: NodeBox, fine: [f64; 3]) -> bool {
            if !closed_contains(at, fine) {
                return false;
            }
            match node {
                Node::Branch(children) => children
                    .iter()
                    .enumerate()
                    .any(|(i, child)| walk(child, at.child(i), fine)),
                Node::Full(..) => true,
                leaf => leaf_source(leaf, at)
                    .is_some_and(|source| lattice::source_contains_fine(&source, fine)),
            }
        }
        walk(self.root_node(), self.root_box(), fine)
    }

    /// Whether a box overlaps solid geometry by more than the contact
    /// tolerance, as `World::intersects_box`.
    pub fn intersects_box(&self, min: Vec3, max: Vec3) -> bool {
        if !min.is_finite() || !max.is_finite() || min.cmpge(max).any() {
            return false;
        }
        let query = FineBox {
            min: (min * 8.0).floor().as_ivec3().to_array(),
            max: (max * 8.0).ceil().as_ivec3().to_array(),
        };
        let Some(query) = query.intersection(self.bounds().fine_box()) else {
            return false;
        };
        let mut hit = false;
        self.visit_box(query, &mut |at, leaf| {
            let Some(source) = leaf_source(leaf, at) else {
                return true;
            };
            let b = at.fine_box();
            let lo = min.max(b.world_min());
            let hi = max.min(b.world_max());
            hit = lo.cmplt(hi).all() && source_overlaps_box(&source, lo, hi);
            !hit
        });
        hit
    }

    /// The highest floor (or lowest ceiling) in `[low, high]` under a body's
    /// footprint `min..max`, from solid geometry.
    pub(crate) fn terrain_support(
        &self,
        min: Vec3,
        max: Vec3,
        low: f32,
        high: f32,
        ceiling: bool,
    ) -> Option<f32> {
        // The interval is closed: a face lying exactly at `low` or `high`
        // counts, so leaves that only touch it in Y are visited too.
        let mut query = FineBox {
            min: (Vec3::new(min.x, low, min.z) * 8.0)
                .floor()
                .as_ivec3()
                .to_array(),
            max: (Vec3::new(max.x, high, max.z) * 8.0)
                .ceil()
                .as_ivec3()
                .to_array(),
        };
        query.min[1] = query.min[1].saturating_sub(1);
        query.max[1] = query.max[1].saturating_add(1);
        let mut result = None;
        self.visit_box(query, &mut |at, leaf| {
            let Some(source) = leaf_source(leaf, at) else {
                return true;
            };
            let triangles = source_triangles(&source);
            if !matches!(leaf, Node::Piece(_)) {
                for (t, _) in triangles {
                    accumulate(t, min, max, low, high, ceiling, &mut result);
                }
                return true;
            }
            let clipped = clip_triangles(
                triangles
                    .iter()
                    .map(|(t, _)| t.map(|p| p.as_dvec3()))
                    .collect(),
                at.fine_box(),
            );
            for t in clipped {
                accumulate(
                    t.map(|p| p.as_vec3()),
                    min,
                    max,
                    low,
                    high,
                    ceiling,
                    &mut result,
                );
            }
            // The leaf's own top or bottom face, where the source's solid
            // crosses it.
            let b = at.fine_box();
            let plane = f64::from(if ceiling { b.min[1] } else { b.max[1] }) / 8.0;
            let rmin = [
                f64::from(min.x).max(f64::from(b.min[0]) / 8.0),
                f64::from(min.z).max(f64::from(b.min[2]) / 8.0),
            ];
            let rmax = [
                f64::from(max.x).min(f64::from(b.max[0]) / 8.0),
                f64::from(max.z).min(f64::from(b.max[2]) / 8.0),
            ];
            if rmin[0] <= rmax[0]
                && rmin[1] <= rmax[1]
                && solid_meets_slice(&source, plane, rmin, rmax)
            {
                let y = plane as f32;
                take(low + (y - low), low, high, ceiling, &mut result);
            }
            true
        });
        result
    }

    /// The first solid hit along a ray, as `World::raycast`: internal joins
    /// between solids are not hits, and a ray from inside solid hits its exit.
    pub fn raycast(&self, origin: Vec3, direction: Vec3, max_distance: f32) -> Option<RayHit> {
        if !origin.is_finite()
            || !direction.is_finite()
            || !max_distance.is_finite()
            || max_distance < 0.0
            || direction.length_squared() < EPS * EPS
        {
            return None;
        }
        let direction = direction.normalize();
        let ray = Ray {
            origin: origin.as_dvec3(),
            direction: direction.as_dvec3(),
            limit: f64::from(max_distance),
        };
        let mut best: Option<RayHit> = None;
        let want = |node: &Node| node.has_geometry();
        let mut visit = |at: NodeBox, leaf: &Node, nearest: &mut f64| {
            let Some(source) = leaf_source(leaf, at) else {
                return;
            };
            let piece = matches!(leaf, Node::Piece(_));
            let consider = |exact: f64, normal: Vec3, face: u8, best: &mut Option<RayHit>| {
                let t = exact as f32;
                if t > max_distance || best.is_some_and(|h| h.distance <= t) {
                    return;
                }
                let point = origin + direction * t;
                let epsilon = point_epsilon(point);
                if piece {
                    let b = at.fine_box();
                    let (lo, hi) = (b.world_min(), b.world_max());
                    if (point + Vec3::splat(epsilon)).cmplt(lo).any()
                        || (point - Vec3::splat(epsilon)).cmpgt(hi).any()
                    {
                        return;
                    }
                }
                // Solid on both sides along the ray is an internal join. The
                // samples come from the unrounded hit, so a grazing ray's
                // samples cannot round back onto the face.
                let hit = ray.origin + ray.direction * exact;
                let offset = ray.direction * f64::from(epsilon);
                if self.contains_fine(((hit - offset) * 8.0).to_array())
                    && self.contains_fine(((hit + offset) * 8.0).to_array())
                {
                    return;
                }
                let b = at.fine_box();
                let inside = (point - normal * epsilon).clamp(
                    b.world_min() + Vec3::splat(epsilon),
                    b.world_max() - Vec3::splat(epsilon),
                );
                let pos = inside.floor().as_ivec3().to_array();
                *best = Some(RayHit {
                    pos,
                    bounds: b.intersection(FineBox::root(pos)).unwrap_or(b),
                    face,
                    normal,
                    point,
                    distance: t,
                });
            };
            for (t, face) in source_triangles(&source) {
                let Some(distance) = ray_triangle(origin, direction, t) else {
                    continue;
                };
                let normal = (t[1] - t[0]).cross(t[2] - t[0]).normalize();
                consider(distance, normal, face, &mut best);
            }
            if piece {
                let b = at.fine_box();
                for face in 0..6u8 {
                    let axis = face as usize / 2;
                    let d = f64::from(direction[axis]);
                    if d == 0.0 {
                        continue;
                    }
                    let plane = f64::from(if face % 2 == 0 {
                        b.min[axis]
                    } else {
                        b.max[axis]
                    });
                    let exact = (plane / 8.0 - f64::from(origin[axis])) / d;
                    if exact < -f64::from(EPS) {
                        continue;
                    }
                    let exact = exact.max(0.0);
                    let point = origin + direction * exact as f32;
                    let mut fine = fine_point(point);
                    fine[axis] = plane;
                    let tolerance = f64::from(point_epsilon(point)) * 8.0;
                    let within = (0..3).filter(|&a| a != axis).all(|a| {
                        f64::from(b.min[a]) - tolerance <= fine[a]
                            && fine[a] <= f64::from(b.max[a]) + tolerance
                    });
                    if !within || !lattice::source_contains_fine(&source, fine) {
                        continue;
                    }
                    let mut normal = Vec3::ZERO;
                    normal[axis] = if face % 2 == 0 { -1.0 } else { 1.0 };
                    consider(exact, normal, face, &mut best);
                }
            }
            if let Some(hit) = &best {
                *nearest = f64::from(hit.distance);
            }
        };
        let mut nearest = f64::INFINITY;
        if ray.interval(self.root_box()).is_some() {
            walk_ray(
                self.root_node(),
                self.root_box(),
                &ray,
                &want,
                &mut nearest,
                &mut visit,
            );
        }
        best
    }

    /// Full effective fragments meeting a box, as `World::fragments_in`.
    pub fn fragments_in(&self, bounds: FineBox) -> Vec<CellFragment> {
        let Some(bounds) = bounds.intersection(self.bounds().fine_box()) else {
            return Vec::new();
        };
        let world = self.bounds().fine_box();
        let mut out = Vec::new();
        self.visit_box(bounds, &mut |at, leaf| {
            if let Some(mut fragment) = fragment_of(at, leaf) {
                fragment.bounds = fragment.bounds.intersection(world).unwrap();
                out.push(fragment);
            }
            true
        });
        out
    }

    /// A metre's content in as few fragments as stay exact.
    pub fn root_fragments(&self, pos: CellPos) -> Vec<CellFragment> {
        if !self.bounds().contains_pos(pos) {
            return Vec::new();
        }
        let root = FineBox::root(pos);
        let mut fragments = Vec::new();
        self.visit_box(root, &mut |at, leaf| {
            let Some(mut fragment) = fragment_of(at, leaf) else {
                return true;
            };
            let clip = at.fine_box().intersection(root).unwrap();
            if clip != at.fine_box() {
                let source = leaf_source(leaf, at).unwrap();
                if lattice::classify_box(&source, clip) == lattice::Class::Outside {
                    return true;
                }
            }
            fragment.bounds = clip;
            fragments.push(fragment);
            true
        });
        let mut fragments = coalesce(crate::canonical_root_fragments(pos, &fragments));
        fragments.sort_by_key(|f| f.bounds);
        fragments
    }

    /// Metres in a box that hold geometry, as `World::try_roots_in`.
    pub fn try_roots_in(&self, bounds: FineBox, max_roots: usize) -> Result<Vec<RootEdit>, String> {
        let Some(bounds) = bounds.intersection(self.bounds().fine_box()) else {
            return Ok(Vec::new());
        };
        let limit = max_roots.min(MAX_QUERY_ROOTS);
        let too_many = || "Selection contains too many metre roots; select a smaller region.";
        let mut positions = BTreeSet::new();
        let mut error = false;
        self.visit_box(bounds, &mut |at, leaf| {
            if !leaf.has_geometry() {
                return true;
            }
            let clip = at.fine_box().intersection(bounds).unwrap();
            let lo = clip.min.map(|v| v.div_euclid(8));
            let hi = clip.max.map(|v| (v - 1).div_euclid(8) + 1);
            let count: i64 = (0..3)
                .map(|a| i64::from(hi[a]) - i64::from(lo[a]))
                .product();
            if count > limit as i64 {
                error = true;
                return false;
            }
            for x in lo[0]..hi[0] {
                for y in lo[1]..hi[1] {
                    for z in lo[2]..hi[2] {
                        positions.insert([x, y, z]);
                        if positions.len() > limit {
                            error = true;
                            return false;
                        }
                    }
                }
            }
            true
        });
        if error {
            return Err(too_many().into());
        }
        Ok(positions
            .into_iter()
            .filter_map(|pos| {
                let fragments = self.root_fragments(pos);
                (!fragments.is_empty()).then_some(RootEdit { pos, fragments })
            })
            .collect())
    }

    // -----------------------------------------------------------------------
    // Cube material.

    /// Water or glass at a point, unless solid geometry occupies it.
    pub fn volume_material_at(&self, point: Vec3) -> Option<VolumeMaterial> {
        if !point.is_finite() {
            return None;
        }
        let bounds = self.bounds();
        if point.cmplt(bounds.world_min()).any() || point.cmpge(bounds.world_max()).any() {
            return None;
        }
        // Solid geometry owns the points it occupies, so a wall inside water
        // is not swimming space.
        let material = self.material_at(point)?;
        (!self.contains_point(point)).then_some(material)
    }

    /// The cube material's water or glass at a point, ignoring geometry.
    fn material_at(&self, point: Vec3) -> Option<VolumeMaterial> {
        let (_, leaf) = self.leaf_at_fine(fine_point(point))?;
        volume_material_of(leaf.cube())
    }

    fn glass_in_box(&self, min: Vec3, max: Vec3, visit: &mut dyn FnMut(NodeBox) -> bool) {
        if !min.is_finite() || !max.is_finite() || !min.cmplt(max).all() {
            return;
        }
        let query = FineBox {
            min: (min * 8.0).floor().as_ivec3().to_array(),
            max: (max * 8.0).ceil().as_ivec3().to_array(),
        };
        self.visit_all(query, &mut |at, leaf| {
            let b = at.fine_box();
            if volume_material_of(leaf.cube()) == Some(VolumeMaterial::Glass)
                && min.cmplt(b.world_max()).all()
                && max.cmpgt(b.world_min()).all()
            {
                return visit(at);
            }
            true
        });
    }

    /// Whether a box overlaps glass, which blocks movement across its whole
    /// cube.
    pub fn intersects_solid_volume(&self, min: Vec3, max: Vec3) -> bool {
        let mut hit = false;
        self.glass_in_box(min, max, &mut |_| {
            hit = true;
            false
        });
        hit
    }

    /// Whether every leaf in a fine box is glass.
    fn all_glass(&self, query: FineBox) -> bool {
        if !self.bounds().fine_box().contains(query) {
            return false;
        }
        let mut all = true;
        self.visit_all(query, &mut |_, leaf| {
            all = volume_material_of(leaf.cube()) == Some(VolumeMaterial::Glass);
            all
        });
        all
    }

    /// Glass floors (or ceilings) in `[low, high]` under a footprint: tops
    /// (or bottoms) of glass cubes not covered by more glass.
    pub(crate) fn glass_support(
        &self,
        min: Vec3,
        max: Vec3,
        low: f32,
        high: f32,
        skin: f32,
        ceiling: bool,
    ) -> Option<f32> {
        let (mut qmin, mut qmax) = (min, max);
        qmin.y = low - skin;
        qmax.y = high + skin;
        let footprint = FineBox {
            min: (min * 8.0).floor().as_ivec3().to_array(),
            max: (max * 8.0).ceil().as_ivec3().to_array(),
        };
        let mut result: Option<f32> = None;
        self.glass_in_box(qmin, qmax, &mut |at| {
            let b = at.fine_box();
            let height = if ceiling {
                b.world_min().y
            } else {
                b.world_max().y
            };
            if height < low - skin || height > high + skin {
                return true;
            }
            let y = if ceiling { b.min[1] - 1 } else { b.max[1] };
            let beyond = FineBox {
                min: [
                    b.min[0].max(footprint.min[0]),
                    y,
                    b.min[2].max(footprint.min[2]),
                ],
                max: [
                    b.max[0].min(footprint.max[0]),
                    y + 1,
                    b.max[2].min(footprint.max[2]),
                ],
            };
            if !beyond.is_empty() && self.all_glass(beyond) {
                return true;
            }
            result = Some(result.map_or(height, |r| {
                if ceiling {
                    r.min(height)
                } else {
                    r.max(height)
                }
            }));
            true
        });
        result
    }

    /// The first water or glass surface along a ray, as
    /// `World::raycast_volumes`: joins between equal materials are skipped,
    /// and the hit's `bounds` is the cube around it.
    pub fn raycast_volumes(
        &self,
        origin: Vec3,
        direction: Vec3,
        max_distance: f32,
    ) -> Option<RayHit> {
        self.raycast_material(origin, direction, max_distance, None)
    }

    /// The first glass surface along a ray.
    pub fn raycast_solid_volumes(
        &self,
        origin: Vec3,
        direction: Vec3,
        max_distance: f32,
    ) -> Option<RayHit> {
        self.raycast_material(origin, direction, max_distance, Some(VolumeMaterial::Glass))
    }

    fn raycast_material(
        &self,
        origin: Vec3,
        direction: Vec3,
        max_distance: f32,
        only: Option<VolumeMaterial>,
    ) -> Option<RayHit> {
        if !origin.is_finite()
            || !direction.is_finite()
            || direction.length_squared() < 1.0e-10
            || !direction.length_squared().is_finite()
            || !max_distance.is_finite()
            || max_distance < 0.0
        {
            return None;
        }
        let direction = direction.normalize();
        let ray = Ray {
            origin: origin.as_dvec3(),
            direction: direction.as_dvec3(),
            limit: f64::from(max_distance),
        };
        let want = |node: &Node| {
            volume_material_of(node.cube()).is_some_and(|m| only.is_none_or(|o| o == m))
        };
        let mut best: Option<RayHit> = None;
        let mut limit = max_distance;
        let mut visit = |at: NodeBox, leaf: &Node, nearest: &mut f64| {
            let material = volume_material_of(leaf.cube()).unwrap();
            let b = at.fine_box();
            let (min, max) = (b.world_min(), b.world_max());
            let Some((enter, exit)) = ray_box_interval(origin, direction, min, max) else {
                return;
            };
            for (distance, entering) in [(enter, true), (exit, false)] {
                if distance < 0.0 || distance > limit {
                    continue;
                }
                // The face this boundary crosses: the slab that set it.
                let mut face = 0;
                let mut best_t = if entering {
                    f32::NEG_INFINITY
                } else {
                    f32::INFINITY
                };
                for axis in 0..3 {
                    if direction[axis] == 0.0 {
                        continue;
                    }
                    let a = (min[axis] - origin[axis]) / direction[axis];
                    let c = (max[axis] - origin[axis]) / direction[axis];
                    let (t, positive) = if entering {
                        (a.min(c), direction[axis] < 0.0)
                    } else {
                        (a.max(c), direction[axis] > 0.0)
                    };
                    if (entering && t > best_t) || (!entering && t < best_t) {
                        best_t = t;
                        face = axis * 2 + usize::from(positive);
                    }
                }
                let point = origin + direction * distance;
                let mut normal = Vec3::ZERO;
                normal[face / 2] = if face % 2 == 0 { -1.0 } else { 1.0 };
                let epsilon = (point.abs().max_element() * f32::EPSILON * 2.0).max(0.001);
                if self.material_at(point + normal * epsilon) == Some(material) {
                    continue;
                }
                best = Some(RayHit {
                    pos: (point - normal * epsilon).floor().as_ivec3().to_array(),
                    bounds: b,
                    face: face as u8,
                    normal,
                    point,
                    distance,
                });
                limit = distance;
                *nearest = f64::from(distance);
            }
        };
        let mut nearest = f64::INFINITY;
        if ray.interval(self.root_box()).is_some() {
            walk_ray(
                self.root_node(),
                self.root_box(),
                &ray,
                &want,
                &mut nearest,
                &mut visit,
            );
        }
        best
    }
}

/// A leaf as a format 3 fragment: its own node as source, or its piece's.
pub(crate) fn fragment_of(at: NodeBox, leaf: &Node) -> Option<CellFragment> {
    let (origin, size, shape, faces) = match leaf {
        Node::Full(f, _) => (at.min, at.size, None, *f),
        Node::Shaped(s) => (at.min, at.size, Some(s.shape), s.faces),
        Node::Piece(p) => (
            p.source.origin,
            p.source.size,
            Some(p.source.shape),
            p.faces,
        ),
        _ => return None,
    };
    Some(CellFragment {
        bounds: at.fine_box(),
        origin,
        size,
        cell: cell_for(shape, faces),
    })
}

/// Merges fragments of one source whose boxes join into a box.
fn coalesce(mut fragments: Vec<CellFragment>) -> Vec<CellFragment> {
    loop {
        let before = fragments.len();
        for axis in 0..3 {
            let u = (axis + 1) % 3;
            let v = (axis + 2) % 3;
            fragments.sort_by(|a, b| {
                let key = |f: &CellFragment| {
                    (
                        f.origin,
                        f.size,
                        f.bounds.min[u],
                        f.bounds.max[u],
                        f.bounds.min[v],
                        f.bounds.max[v],
                        f.bounds.min[axis],
                    )
                };
                key(a).cmp(&key(b))
            });
            let mut merged: Vec<CellFragment> = Vec::with_capacity(fragments.len());
            for fragment in fragments.drain(..) {
                if let Some(last) = merged.last_mut() {
                    if last.origin == fragment.origin
                        && last.size == fragment.size
                        && last.cell == fragment.cell
                        && [u, v].into_iter().all(|a| {
                            last.bounds.min[a] == fragment.bounds.min[a]
                                && last.bounds.max[a] == fragment.bounds.max[a]
                        })
                        && last.bounds.max[axis] == fragment.bounds.min[axis]
                    {
                        last.bounds.max[axis] = fragment.bounds.max[axis];
                        continue;
                    }
                }
                merged.push(fragment);
            }
            fragments = merged;
        }
        if fragments.len() == before {
            return fragments;
        }
    }
}
