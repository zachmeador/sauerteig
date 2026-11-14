//! Exact geometry of a shape on its cube's 0–8 lattice.
//!
//! Everything here is integer arithmetic, so every build and every system
//! classifies the same points and boxes the same way. Triangles come out in
//! the same order, with the same vertices, as `geometry::shape_triangles`, so
//! floating-point queries on them give the same bits as on its triangles.

use super::Source;
use crate::{Shape, SHAPE_FACES};

pub(crate) type P3 = [i32; 3];
pub(crate) type Tri = ([P3; 3], u8);

/// A shape's surviving boundary triangles in lattice coordinates.
#[derive(Clone, Copy)]
pub(crate) struct TriList {
    tris: [Tri; 12],
    len: usize,
}

impl TriList {
    pub(crate) fn as_slice(&self) -> &[Tri] {
        &self.tris[..self.len]
    }
}

fn sub(a: P3, b: P3) -> P3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn cross(a: P3, b: P3) -> P3 {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn dot(a: P3, b: P3) -> i32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn normal(t: [P3; 3]) -> P3 {
    cross(sub(t[1], t[0]), sub(t[2], t[0]))
}

fn quad_triangles(shape: Shape, face: usize, set: bool) -> [[P3; 3]; 2] {
    let p = |i: usize| shape.vertices[i].map(i32::from);
    let [a, b, c, d] = SHAPE_FACES[face];
    if set {
        [[p(a), p(b), p(d)], [p(b), p(c), p(d)]]
    } else {
        [[p(a), p(b), p(c)], [p(a), p(c), p(d)]]
    }
}

/// The boundary `geometry::shape_triangles` emits: zero-area triangles are
/// dropped, then coincident opposite pairs (zero-thickness sheets) cancel.
pub(crate) fn triangles(shape: Shape) -> TriList {
    let mut slots: [Option<Tri>; 12] = [None; 12];
    for face in 0..6 {
        let set = shape.diagonals & (1 << face) != 0;
        for (part, t) in quad_triangles(shape, face, set).into_iter().enumerate() {
            if normal(t) != [0; 3] {
                slots[face * 2 + part] = Some((t, face as u8));
            }
        }
    }
    for i in 0..12 {
        let Some((a, _)) = slots[i] else {
            continue;
        };
        for j in i + 1..12 {
            let Some((b, _)) = slots[j] else {
                continue;
            };
            if a.iter().all(|p| b.contains(p)) && dot(normal(a), normal(b)) < 0 {
                slots[i] = None;
                slots[j] = None;
                break;
            }
        }
    }
    let mut list = TriList {
        tris: [([[0; 3]; 3], 0); 12],
        len: 0,
    };
    for t in slots.into_iter().flatten() {
        list.tris[list.len] = t;
        list.len += 1;
    }
    list
}

pub(crate) fn is_solid(shape: Shape) -> bool {
    shape.vertices == Shape::solid().vertices
}

/// Faces with at least one surviving triangle.
pub(crate) fn visible_faces(shape: Shape) -> [bool; 6] {
    let mut visible = [false; 6];
    for &(_, face) in triangles(shape).as_slice() {
        visible[face as usize] = true;
    }
    visible
}

/// A face's diagonal is meaningless when flipping it leaves every other
/// face's triangles as they were and this face covers the same region either
/// way: it has no triangles at all, or it is a planar convex quad whose
/// triangles all survive under both choices.
fn diagonal_is_meaningless(shape: Shape, face: usize) -> bool {
    let with = |set: bool| {
        let mut s = shape;
        if set {
            s.diagonals |= 1 << face;
        } else {
            s.diagonals &= !(1 << face);
        }
        triangles(s)
    };
    let (clear, set) = (with(false), with(true));
    let others = |list: &TriList| {
        list.as_slice()
            .iter()
            .filter(|t| t.1 as usize != face)
            .copied()
            .collect::<Vec<_>>()
    };
    if others(&clear) != others(&set) {
        return false;
    }
    let own = |list: &TriList| {
        list.as_slice()
            .iter()
            .filter(|t| t.1 as usize == face)
            .map(|t| t.0)
            .collect::<Vec<_>>()
    };
    let (own_clear, own_set) = (own(&clear), own(&set));
    if own_clear.is_empty() && own_set.is_empty() {
        return true;
    }
    let [a, b, c, d] = SHAPE_FACES[face].map(|i| shape.vertices[i].map(i32::from));
    if dot(cross(sub(b, a), sub(c, a)), sub(d, a)) != 0 {
        return false;
    }
    // A strictly convex planar quad: all four corner triangles have area and
    // turn the same way. A collinear corner would leave a T-junction on one
    // side, which the shape rules refuse.
    let (all_clear, all_set) = (
        quad_triangles(shape, face, false),
        quad_triangles(shape, face, true),
    );
    if own_clear != all_clear || own_set != all_set {
        return false;
    }
    let normals: Vec<P3> = all_clear
        .iter()
        .chain(&all_set)
        .map(|t| normal(*t))
        .collect();
    normals
        .iter()
        .all(|&n| n != [0; 3] && normals.iter().all(|&m| dot(n, m) > 0))
}

/// The fixed diagonals of faces whose diagonal means nothing: the ones the
/// importer gives planar faces of Sauerbraten cubes, so imported shapes keep
/// their triangles.
pub(crate) const FIXED_DIAGONALS: u8 = 0b00_1111;

/// Meaningless diagonals take fixed values, so cubes that look the same
/// compare equal.
pub(crate) fn normalize_shape(mut shape: Shape) -> Shape {
    loop {
        let mut changed = false;
        for face in 0..6 {
            let bit = 1 << face;
            if shape.diagonals & bit != FIXED_DIAGONALS & bit
                && diagonal_is_meaningless(shape, face)
            {
                shape.diagonals ^= bit;
                changed = true;
            }
        }
        if !changed {
            return shape;
        }
    }
}

/// A face with no surviving triangle has no surface of its own; it takes the
/// material of the first face that does.
pub(crate) fn normalize_faces(shape: Shape, faces: [u8; 6]) -> [u8; 6] {
    fill_faces(visible_faces(shape), faces)
}

pub(crate) fn fill_faces(visible: [bool; 6], mut faces: [u8; 6]) -> [u8; 6] {
    let Some(first) = visible.iter().position(|&v| v) else {
        return faces;
    };
    let fill = faces[first];
    for (face, material) in faces.iter_mut().enumerate() {
        if !visible[face] {
            *material = fill;
        }
    }
    faces
}

/// The height-field form of a shape, when `Cell { heights, diagonal }`
/// describes exactly this (normalized) shape.
pub(crate) fn heightfield_form(shape: Shape) -> Option<([u8; 4], bool)> {
    let solid = Shape::solid();
    let corners = (0..4).all(|i| {
        shape.vertices[i] == solid.vertices[i]
            && shape.vertices[i + 4][0] == solid.vertices[i + 4][0]
            && shape.vertices[i + 4][2] == solid.vertices[i + 4][2]
    });
    if !corners {
        return None;
    }
    let heights = std::array::from_fn(|i| shape.vertices[i + 4][1]);
    if heights.iter().all(|&h| h == 0) {
        return None;
    }
    let diagonal = shape.diagonals & (1 << 3) != 0;
    let cell = crate::Cell {
        heights,
        diagonal,
        ..crate::Cell::solid(0)
    };
    (normalize_shape(cell.as_shape()) == normalize_shape(shape)).then_some((heights, diagonal))
}

/// Everything the `Shape` rules or height-field rules accept. Shapes with
/// no volume stay refused.
pub(crate) fn shape_allowed(shape: Shape) -> bool {
    if shape.diagonals >= 64 || shape.vertices.iter().flatten().any(|&v| v > 8) {
        return false;
    }
    shape.is_valid() || heightfield_form(shape).is_some()
}

// ---------------------------------------------------------------------------
// Exact predicates in a common integer frame.

pub(crate) type I3 = [i128; 3];

fn sub128(a: I3, b: I3) -> I3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn cross128(a: I3, b: I3) -> I3 {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn dot128(a: I3, b: I3) -> i128 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

/// 2-D orientation in the plane of axes `u`, `v`.
fn orient(a: I3, b: I3, p: I3, u: usize, v: usize) -> i128 {
    (b[u] - a[u]) * (p[v] - a[v]) - (b[v] - a[v]) * (p[u] - a[u])
}

/// Orientation of `p + (ε, ε²)` on the X-Z plane: a fixed symbolic
/// perturbation that puts the vertical probe off every projected edge.
fn orient_perturbed(a: I3, b: I3, p: I3) -> i128 {
    let o = orient(a, b, p, 0, 2);
    if o != 0 {
        o.signum()
    } else if b[2] != a[2] {
        (a[2] - b[2]).signum()
    } else {
        (b[0] - a[0]).signum()
    }
}

fn on_triangle(t: [I3; 3], p: I3) -> bool {
    let n = cross128(sub128(t[1], t[0]), sub128(t[2], t[0]));
    if dot128(n, sub128(p, t[0])) != 0 {
        return false;
    }
    let axis = (0..3).max_by_key(|&a| n[a].abs()).unwrap();
    let (u, v) = ((axis + 1) % 3, (axis + 2) % 3);
    let s = orient(t[0], t[1], t[2], u, v).signum();
    (0..3).all(|i| orient(t[i], t[(i + 1) % 3], p, u, v) * s >= 0)
}

/// Whether a closed (possibly non-manifold) boundary contains `p`, boundary
/// included. Counts signed crossings of a vertical probe, with a symbolic
/// perturbation instead of tolerances.
pub(crate) fn contains(tris: &[[I3; 3]], p: I3) -> bool {
    if tris.iter().any(|&t| on_triangle(t, p)) {
        return true;
    }
    let mut winding = 0;
    for &t in tris {
        let n = cross128(sub128(t[1], t[0]), sub128(t[2], t[0]));
        if n[1] == 0 {
            continue;
        }
        let s = -n[1].signum();
        if (0..3).any(|i| orient_perturbed(t[i], t[(i + 1) % 3], p) != s) {
            continue;
        }
        let f = dot128(n, sub128(p, t[0]));
        if f.signum() * n[1].signum() < 0 {
            winding += n[1].signum();
        }
    }
    winding > 0
}

/// Whether a closed triangle meets the open box `(min, max)`.
pub(crate) fn triangle_meets_open_box(t: [I3; 3], min: I3, max: I3) -> bool {
    // Doubled coordinates centre the box on the origin with integer half sizes.
    let c = std::array::from_fn::<_, 3, _>(|a| min[a] + max[a]);
    let h = std::array::from_fn::<_, 3, _>(|a| max[a] - min[a]);
    let p = t.map(|q| std::array::from_fn::<_, 3, _>(|a| 2 * q[a] - c[a]));
    let separated = |axis: I3| {
        if axis == [0; 3] {
            return false;
        }
        let r = axis[0].abs() * h[0] + axis[1].abs() * h[1] + axis[2].abs() * h[2];
        let d = p.map(|q| dot128(q, axis));
        d.iter().all(|&x| x >= r) || d.iter().all(|&x| x <= -r)
    };
    let edges = [sub128(p[1], p[0]), sub128(p[2], p[1]), sub128(p[0], p[2])];
    let units = [[1, 0, 0], [0, 1, 0], [0, 0, 1]];
    if units.into_iter().any(separated) || separated(cross128(edges[0], edges[1])) {
        return false;
    }
    !edges
        .into_iter()
        .any(|e| units.into_iter().any(|u| separated(cross128(e, u))))
}

/// How a box lies against a closed solid.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Class {
    Inside,
    Outside,
    Crossing,
}

/// A source's triangles in 64ths of a metre: `8 * origin + v * size`.
pub(crate) fn source_triangles_64(source: &Source) -> Vec<[I3; 3]> {
    let origin = source.origin.map(|v| i128::from(v) * 8);
    let size = i128::from(source.size);
    triangles(source.shape)
        .as_slice()
        .iter()
        .map(|(t, _)| t.map(|p| std::array::from_fn(|a| origin[a] + i128::from(p[a]) * size)))
        .collect()
}

pub(crate) fn classify(source: &Source, min: [i32; 3], size: i32) -> Class {
    classify_box(
        source,
        crate::FineBox {
            min,
            max: min.map(|v| v + size),
        },
    )
}

/// Exact: a box whose interior no boundary triangle meets is wholly inside or
/// wholly outside, and its centre, off the boundary, says which.
pub(crate) fn classify_box(source: &Source, b: crate::FineBox) -> Class {
    if is_solid(source.shape) {
        return Class::Inside;
    }
    let tris = source_triangles_64(source);
    let bmin = b.min.map(|v| i128::from(v) * 8);
    let bmax = b.max.map(|v| i128::from(v) * 8);
    if tris.iter().any(|&t| triangle_meets_open_box(t, bmin, bmax)) {
        return Class::Crossing;
    }
    let centre = std::array::from_fn(|a| (bmin[a] + bmax[a]) / 2);
    if contains(&tris, centre) {
        Class::Inside
    } else {
        Class::Outside
    }
}

/// Fixed-point bits below the lattice unit for points from `f32`
/// coordinates: finer than any `f32` spacing at a metre or more from a
/// source's corner, so snapping only moves points nearer than that.
const POINT_BITS: u32 = 36;

/// Whether a source's closed solid contains a world point given in eighths
/// of a metre (`point * 8`, exact for an `f32` point).
pub(crate) fn source_contains_fine(source: &Source, fine: [f64; 3]) -> bool {
    let scale = (1u64 << POINT_BITS) as f64;
    let size = f64::from(source.size);
    let mut p = [0i128; 3];
    for a in 0..3 {
        // Lattice coordinate (fine - origin) * 8 / size, exact in f64 for any
        // point nearer the source than its own size allows.
        let lattice = (fine[a] - f64::from(source.origin[a])) * 8.0 / size;
        p[a] = (lattice * scale).round() as i128;
    }
    let tris: Vec<[I3; 3]> = triangles(source.shape)
        .as_slice()
        .iter()
        .map(|(t, _)| t.map(|q| q.map(|v| i128::from(v) << POINT_BITS)))
        .collect();
    contains(&tris, p)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::shape_triangles;
    use crate::Vec3;

    fn shapes(seed: u32, count: usize) -> Vec<Shape> {
        let mut seed = seed;
        let mut next = || {
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            seed >> 8
        };
        let mut out = Vec::new();
        while out.len() < count {
            let mut shape = Shape::solid();
            for point in &mut shape.vertices {
                for coordinate in point {
                    let inset = (next() % 5) as u8;
                    *coordinate = if *coordinate == 0 { inset } else { 8 - inset };
                }
            }
            shape.diagonals = (next() % 64) as u8;
            if shape_allowed(shape) {
                out.push(shape);
            }
        }
        out
    }

    #[test]
    fn integer_triangles_match_the_float_boundary() {
        for shape in shapes(7, 400) {
            let float: Vec<_> = shape_triangles(shape, Vec3::ZERO, 1.0)
                .into_iter()
                .flatten()
                .collect();
            let exact = triangles(shape);
            assert_eq!(float.len(), exact.as_slice().len());
            for ((t, f), (e, g)) in float.iter().zip(exact.as_slice()) {
                assert_eq!(*f, *g);
                for i in 0..3 {
                    assert_eq!(t[i], Vec3::from_array(e[i].map(|v| v as f32 / 8.0)));
                }
            }
        }
    }

    #[test]
    fn normalizing_never_changes_the_solid_and_is_canonical() {
        for shape in shapes(11, 300) {
            let normal = normalize_shape(shape);
            assert!(shape_allowed(normal));
            assert_eq!(normalize_shape(normal), normal);
            for face in 0..6 {
                let mut flipped = shape;
                flipped.diagonals ^= 1 << face;
                if diagonal_is_meaningless(shape, face) {
                    assert_eq!(normalize_shape(flipped), normal, "{shape:?} face {face}");
                }
            }
            // Points on a grid finer than the lattice classify identically.
            let a: Vec<[I3; 3]> = triangles(shape)
                .as_slice()
                .iter()
                .map(|(t, _)| t.map(|p| p.map(|v| i128::from(v) * 4)))
                .collect();
            let b: Vec<[I3; 3]> = triangles(normal)
                .as_slice()
                .iter()
                .map(|(t, _)| t.map(|p| p.map(|v| i128::from(v) * 4)))
                .collect();
            for x in 0..=32 {
                for y in (0..=32).step_by(3) {
                    for z in (0..=32).step_by(5) {
                        let p = [x, y, z];
                        assert_eq!(contains(&a, p), contains(&b, p));
                    }
                }
            }
        }
    }

    #[test]
    fn exact_containment_agrees_with_the_solid_angle_away_from_the_boundary() {
        for shape in shapes(23, 120) {
            let cell = crate::Cell {
                shape: Some(shape),
                ..crate::Cell::solid(0)
            };
            let tris: Vec<[I3; 3]> = triangles(shape)
                .as_slice()
                .iter()
                .map(|(t, _)| t.map(|p| p.map(|v| i128::from(v) * 64)))
                .collect();
            for i in 0..600u32 {
                let p = [
                    (i * 37 % 509) as i128,
                    (i * 91 % 509) as i128,
                    (i * 53 % 509) as i128,
                ];
                if tris.iter().any(|&t| on_triangle(t, p)) {
                    continue;
                }
                let point = Vec3::from_array(p.map(|v| v as f32 / 512.0));
                if crate::geometry::cell_contains(cell, point) != contains(&tris, p) {
                    // The solid angle is unreliable within rounding of a face.
                    let near = shape_triangles(shape, Vec3::ZERO, 1.0)
                        .into_iter()
                        .flatten()
                        .any(|(t, _)| {
                            let n = (t[1] - t[0]).cross(t[2] - t[0]).normalize();
                            (point - t[0]).dot(n).abs() < 1.0e-4
                        });
                    assert!(near, "{shape:?} at {point:?}");
                }
            }
        }
    }

    #[test]
    fn box_classification_is_exact() {
        let source = |shape| Source {
            origin: [0; 3],
            size: 8,
            shape,
        };
        let half = crate::Cell {
            heights: [4; 4],
            ..crate::Cell::solid(0)
        }
        .as_shape();
        assert_eq!(classify(&source(half), [0, 0, 0], 4), Class::Inside);
        assert_eq!(classify(&source(half), [0, 4, 0], 4), Class::Outside);
        assert_eq!(classify(&source(half), [0, 0, 0], 8), Class::Crossing);
        assert_eq!(classify(&source(half), [0, 3, 0], 1), Class::Inside);
        let ramp = crate::Cell {
            heights: [0, 8, 8, 0],
            ..crate::Cell::solid(0)
        }
        .as_shape();
        assert_eq!(classify(&source(ramp), [4, 4, 0], 4), Class::Crossing);
        assert_eq!(classify(&source(ramp), [4, 0, 0], 4), Class::Inside);
        assert_eq!(classify(&source(ramp), [6, 0, 0], 1), Class::Inside);
        assert_eq!(classify(&source(ramp), [0, 6, 0], 1), Class::Outside);
        // A box touching the slope only along an edge stays outside.
        assert_eq!(classify(&source(ramp), [0, 4, 0], 4), Class::Outside);
    }
}
