//! Shapes on a cube's 0–8 lattice: their boundary triangles and validity.
use crate::{Shape, Vec3, SHAPE_FACES};

const EPS: f32 = 1.0e-5;

pub(crate) fn ray_box_interval(
    origin: Vec3,
    direction: Vec3,
    min: Vec3,
    max: Vec3,
) -> Option<(f32, f32)> {
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

// General cells retain their actual triangulated boundary, including concave
// faces. They are never replaced by their convex hull for queries or drawing.
type BoundaryTriangle = ([Vec3; 3], u8);

pub(crate) fn shape_triangles(
    shape: Shape,
    origin: Vec3,
    size: f32,
) -> [Option<BoundaryTriangle>; 12] {
    let points = shape
        .vertices
        .map(|p| origin + Vec3::from_array(p.map(|v| v as f32)) * (size / 8.0));
    let mut triangles = [None; 12];
    for (face, &[a, b, c, d]) in SHAPE_FACES.iter().enumerate() {
        let indices = if shape.diagonals & (1 << face) == 0 {
            [[a, b, c], [a, c, d]]
        } else {
            [[a, b, d], [b, c, d]]
        };
        for (part, indices) in indices.into_iter().enumerate() {
            let triangle = indices.map(|i| points[i]);
            if (triangle[1] - triangle[0])
                .cross(triangle[2] - triangle[0])
                .length_squared()
                > EPS * EPS * ((size * size) * (size * size))
            {
                triangles[face * 2 + part] = Some((triangle, face as u8));
            }
        }
    }
    // A collapsed half of a ramp has coincident, opposite top/bottom triangles.
    // Remove both; a zero-thickness sheet is not a solid boundary.
    for i in 0..12 {
        let Some((a, _)) = triangles[i] else {
            continue;
        };
        for j in i + 1..12 {
            let Some((b, _)) = triangles[j] else {
                continue;
            };
            if a.iter().all(|p| b.contains(p))
                && (a[1] - a[0])
                    .cross(a[2] - a[0])
                    .dot((b[1] - b[0]).cross(b[2] - b[0]))
                    < 0.0
            {
                triangles[i] = None;
                triangles[j] = None;
                break;
            }
        }
    }
    triangles
}

fn plane_section(triangle: [Vec3; 3], point: Vec3, normal: Vec3) -> Vec<Vec3> {
    let distances = triangle.map(|v| (v - point).dot(normal));
    let mut section = Vec::with_capacity(3);
    for i in 0..3 {
        let j = (i + 1) % 3;
        if distances[i].abs() <= EPS {
            section.push(triangle[i]);
        }
        if (distances[i] > EPS && distances[j] < -EPS)
            || (distances[i] < -EPS && distances[j] > EPS)
        {
            section
                .push(triangle[i].lerp(triangle[j], distances[i] / (distances[i] - distances[j])));
        }
    }
    section
}

fn cross2(a: glam::Vec2, b: glam::Vec2) -> f32 {
    a.x * b.y - a.y * b.x
}

fn point_in_triangle2(p: glam::Vec2, t: [glam::Vec2; 3]) -> bool {
    let d = std::array::from_fn::<_, 3, _>(|i| cross2(t[(i + 1) % 3] - t[i], p - t[i]));
    d.iter().all(|&v| v >= -EPS) || d.iter().all(|&v| v <= EPS)
}

fn invalid_triangle_intersection(a: [Vec3; 3], b: [Vec3; 3]) -> bool {
    let na = (a[1] - a[0]).cross(a[2] - a[0]).normalize();
    let nb = (b[1] - b[0]).cross(b[2] - b[0]).normalize();
    let shared: Vec<_> = a.iter().copied().filter(|p| b.contains(p)).collect();
    let line = na.cross(nb);
    if line.length_squared() > EPS * EPS {
        // Distinct planes containing the same edge cannot intersect elsewhere.
        if shared.len() >= 2 {
            return false;
        }
        let sa = plane_section(a, b[0], nb);
        let sb = plane_section(b, a[0], na);
        if sa.is_empty() || sb.is_empty() {
            return false;
        }
        let axis = line.normalize();
        let interval = |s: &[Vec3]| {
            s.iter()
                .fold((f32::INFINITY, f32::NEG_INFINITY), |(lo, hi), p| {
                    (lo.min(p.dot(axis)), hi.max(p.dot(axis)))
                })
        };
        let (a0, a1) = interval(&sa);
        let (b0, b1) = interval(&sb);
        let lo = a0.max(b0);
        let hi = a1.min(b1);
        if lo > hi + EPS {
            return false;
        }
        return !shared
            .first()
            .is_some_and(|p| (lo - p.dot(axis)).abs() <= EPS && (hi - p.dot(axis)).abs() <= EPS);
    }
    if (b[0] - a[0]).dot(na).abs() > EPS {
        return false;
    }
    let axis = if na.x.abs() >= na.y.abs() && na.x.abs() >= na.z.abs() {
        0
    } else if na.y.abs() >= na.z.abs() {
        1
    } else {
        2
    };
    let project = |p: Vec3| match axis {
        0 => glam::Vec2::new(p.y, p.z),
        1 => glam::Vec2::new(p.x, p.z),
        _ => glam::Vec2::new(p.x, p.y),
    };
    let aa = a.map(project);
    let bb = b.map(project);
    // Strict 2-D SAT detects area overlap, including triangles sharing an edge
    // but folded onto the same side of it.
    let mut area_overlap = true;
    for triangle in [aa, bb] {
        for i in 0..3 {
            let edge = triangle[(i + 1) % 3] - triangle[i];
            let axis = glam::Vec2::new(-edge.y, edge.x).normalize();
            let interval = |t: [glam::Vec2; 3]| {
                t.into_iter()
                    .fold((f32::INFINITY, f32::NEG_INFINITY), |(lo, hi), p| {
                        (lo.min(p.dot(axis)), hi.max(p.dot(axis)))
                    })
            };
            let (a0, a1) = interval(aa);
            let (b0, b1) = interval(bb);
            if a1.min(b1) - a0.max(b0) <= EPS {
                area_overlap = false;
            }
        }
    }
    if area_overlap {
        return true;
    }
    // Boundary contacts are legal only at the vertices/edges these triangles
    // actually share. This also rejects a vertex touching an unrelated face.
    for i in 0..3 {
        if !shared.contains(&a[i]) && point_in_triangle2(aa[i], bb) {
            return true;
        }
        if !shared.contains(&b[i]) && point_in_triangle2(bb[i], aa) {
            return true;
        }
    }
    false
}

pub(crate) fn shape_is_valid(shape: Shape) -> bool {
    if shape.diagonals >= 64 || shape.vertices.iter().flatten().any(|&v| v > 8) {
        return false;
    }
    let triangles: Vec<_> = shape_triangles(shape, Vec3::ZERO, 1.0)
        .into_iter()
        .flatten()
        .map(|(t, _)| t)
        .collect();
    if triangles.len() < 4 {
        return false;
    }
    // Geometric edges must occur twice, with opposite directions. Quantized
    // coordinates let this topology check be exact even after corner collapse.
    let mut edges = std::collections::BTreeMap::new();
    for (index, t) in triangles.iter().enumerate() {
        for i in 0..3 {
            let a = t[i].to_array().map(|v| (v * 8.0) as u8);
            let b = t[(i + 1) % 3].to_array().map(|v| (v * 8.0) as u8);
            let (key, sign) = if a < b { ((a, b), 1_i32) } else { ((b, a), -1) };
            edges
                .entry(key)
                .or_insert_with(Vec::new)
                .push((index, sign));
        }
    }
    if edges
        .values()
        .any(|uses| uses.len() != 2 || uses[0].1 + uses[1].1 != 0)
    {
        return false;
    }
    for i in 0..triangles.len() {
        for j in i + 1..triangles.len() {
            if invalid_triangle_intersection(triangles[i], triangles[j]) {
                return false;
            }
        }
    }
    // Test each connected boundary separately: a positive component must not
    // conceal another inverted component in a total signed-volume sum.
    let mut visited = vec![false; triangles.len()];
    for start in 0..triangles.len() {
        if visited[start] {
            continue;
        }
        let mut pending = vec![start];
        let mut volume6 = 0.0;
        while let Some(i) = pending.pop() {
            if visited[i] {
                continue;
            }
            visited[i] = true;
            let [a, b, c] = triangles[i];
            volume6 += a.dot(b.cross(c));
            for uses in edges.values() {
                if uses[0].0 == i {
                    pending.push(uses[1].0);
                }
                if uses[1].0 == i {
                    pending.push(uses[0].0);
                }
            }
        }
        if volume6 <= EPS {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod shape_tests {
    use super::*;
    use crate::{Cell, CellEdit, CellFragment, FineBox, World};

    fn shaped(shape: Shape) -> Cell {
        Cell {
            shape: Some(shape),
            ..Cell::solid(0)
        }
    }

    fn overlaps(world: &World, centre: Vec3, half: f32) -> bool {
        world.intersects_box(centre - Vec3::splat(half), centre + Vec3::splat(half))
    }

    fn ray_meets_mesh(world: &World, origin: Vec3, direction: Vec3, distance: f32) -> bool {
        world.chunk_keys().into_iter().any(|key| {
            let mesh = world.mesh_chunk(key);
            mesh.vertices.as_chunks::<3>().0.iter().any(|v| {
                let [a, b, c] = [v[0].position, v[1].position, v[2].position].map(Vec3::from_array);
                // Möller–Trumbore, for the test only.
                let (e1, e2) = (b - a, c - a);
                let p = direction.cross(e2);
                let det = e1.dot(p);
                if det.abs() < 1.0e-9 {
                    return false;
                }
                let t = origin - a;
                let u = t.dot(p) / det;
                let q = t.cross(e1);
                let w = direction.dot(q) / det;
                u >= -1.0e-5
                    && w >= -1.0e-5
                    && u + w <= 1.0 + 1.0e-5
                    && (e2.dot(q) / det - distance).abs() < 1.0e-4
            })
        })
    }

    #[test]
    fn every_face_accepts_corner_and_edge_shapes_with_matching_queries() {
        for (face, corners) in SHAPE_FACES.iter().enumerate() {
            for edge in [false, true] {
                let mut shape = Shape::solid();
                let axis = face / 2;
                for &corner in &corners[..if edge { 2 } else { 1 }] {
                    shape.vertices[corner][axis] = if face % 2 == 0 { 2 } else { 6 };
                }
                assert!(shape.is_valid(), "face {face}, edge {edge}");
                let cell = shaped(shape);
                let mut world = World::empty();
                world.set([0, 0, 0], Some(cell));
                let mut origin = Vec3::splat(0.43);
                origin[axis] = if face % 2 == 0 { -1.0 } else { 2.0 };
                let mut direction = Vec3::ZERO;
                direction[axis] = if face % 2 == 0 { 1.0 } else { -1.0 };
                let hit = world
                    .raycast(origin, direction, 4.0)
                    .expect("visible shaped face");
                assert_eq!(hit.face, face as u8);
                let inside = hit.point - hit.normal * 0.03;
                let outside = hit.point + hit.normal * 0.03;
                assert!(overlaps(&world, inside, 0.004));
                assert!(!overlaps(&world, outside, 0.004));
                assert!(ray_meets_mesh(&world, origin, direction, hit.distance));
            }
        }
    }

    #[test]
    fn one_shape_retains_deformation_on_multiple_faces() {
        let mut shape = Shape::solid();
        shape.vertices[1][0] = 6;
        shape.vertices[7][1] = 6;
        shape.vertices[3][2] = 6;
        assert!(shape.is_valid());
        let cell = shaped(shape);
        let mut world = World::empty();
        world.set([0, 0, 0], Some(cell));
        for (origin, direction, face) in [
            (Vec3::new(2.0, 0.2, 0.2), -Vec3::X, 1),
            (Vec3::new(0.2, 2.0, 0.8), -Vec3::Y, 3),
            (Vec3::new(0.2, 0.2, 2.0), -Vec3::Z, 5),
        ] {
            let hit = world.raycast(origin, direction, 4.0).unwrap();
            assert_eq!(hit.face, face);
            assert!(hit.distance > 1.01);
        }
    }

    #[test]
    fn concave_boundaries_are_preserved_instead_of_using_a_convex_hull() {
        let legacy = Cell {
            heights: [2, 8, 2, 8],
            ..Cell::solid(1)
        };
        let shape = legacy.as_shape();
        assert!(shape.is_valid());
        let mut world = World::empty();
        world.set([0, 0, 0], Some(shaped(shape)));
        let in_notch = Vec3::new(0.5, 0.65, 0.5);
        assert!(!world.tree.contains_point(in_notch));
        assert!(!overlaps(&world, in_notch, 0.01));
        assert!(overlaps(&world, Vec3::splat(0.1), 0.05));
        assert!(world.intersects_box(Vec3::splat(-1.0), Vec3::splat(2.0)));
        let hit = world
            .raycast(Vec3::new(0.5, 2.0, 0.5), -Vec3::Y, 3.0)
            .unwrap();
        assert!((hit.point.y - 0.25).abs() < EPS);
    }

    #[test]
    fn shape_queries_scale_with_the_source_cube_without_resampling() {
        let mut shape = Shape::solid();
        shape.vertices[4][0] = 2;
        assert!(shape.is_valid());
        let cell = shaped(shape);
        let unit = CellFragment::from_cell([0, 0, 0], cell);
        // The same shape on a quarter-metre cube at an offset origin.
        let small = CellFragment {
            bounds: FineBox {
                min: [-36, 18, 72],
                max: [-34, 20, 74],
            },
            origin: [-36, 18, 72],
            size: 2,
            cell,
        };
        let origin = Vec3::new(-4.5, 2.25, 9.0);
        for point in [Vec3::new(0.1, 0.9, 0.1), Vec3::new(0.4, 0.5, 0.5)] {
            let half = Vec3::splat(0.01);
            let expected = crate::octree::fragment_overlaps_box(&unit, point - half, point + half);
            assert_eq!(
                crate::octree::fragment_overlaps_box(
                    &small,
                    origin + (point - half) * 0.25,
                    origin + (point + half) * 0.25
                ),
                expected
            );
        }
    }

    #[test]
    fn invalid_shapes_cannot_enter_the_world_or_saved_map() {
        let mut invalids = Vec::new();
        let mut outside = Shape::solid();
        outside.vertices[4][0] = 9;
        invalids.push(outside);
        let mut flat = Shape::solid();
        for p in &mut flat.vertices {
            p[1] = 0;
        }
        invalids.push(flat);
        let mut inverted = Shape::solid();
        for p in &mut inverted.vertices {
            p[0] = 8 - p[0];
        }
        invalids.push(inverted);
        let mut crossed = Shape::solid();
        crossed.vertices.swap(4, 6);
        invalids.push(crossed);
        let mut touching = Shape::solid();
        touching.vertices[4] = touching.vertices[2];
        invalids.push(touching);
        let mut bits = Shape::solid();
        bits.diagonals = 64;
        invalids.push(bits);
        for shape in invalids {
            assert!(!shape.is_valid(), "invalid shape accepted: {shape:?}");
            let mut world = World::empty();
            world.set([0, 0, 0], Some(Cell::solid(1)));
            world.set([0, 0, 0], Some(shaped(shape)));
            assert_eq!(world.get([0, 0, 0]), Some(Cell::solid(1)));
            let saved = [CellEdit {
                pos: [0, 0, 0],
                cell: Some(shaped(shape)),
            }];
            assert!(World::from_parts(&saved, &[]).is_err());
        }
    }

    #[test]
    fn mixed_partial_shapes_do_not_create_internal_ray_hits() {
        let mut world = World::empty();
        world.set(
            [0, 0, 0],
            Some(Cell {
                heights: [4; 4],
                ..Cell::solid(0)
            }),
        );
        let slope = Cell {
            heights: [2, 6, 6, 2],
            ..Cell::solid(1)
        };
        assert!(slope.as_shape().is_valid());
        world.set(
            [1, 0, 0],
            Some(Cell {
                shape: Some(slope.as_shape()),
                ..slope
            }),
        );
        let through = world
            .raycast(Vec3::new(0.5, 0.2, 0.4), Vec3::X, 4.0)
            .unwrap();
        assert!((through.point.x - 2.0).abs() < EPS);
        let reverse = world
            .raycast(Vec3::new(1.5, 0.2, 0.4), -Vec3::X, 4.0)
            .unwrap();
        assert!(reverse.point.x.abs() < EPS);
        let exposed = world
            .raycast(Vec3::new(0.5, 0.4, 0.4), Vec3::X, 4.0)
            .unwrap();
        assert!((exposed.point.x - 1.0).abs() < EPS);
    }

    #[test]
    fn height_field_cells_save_and_reload_with_their_geometry() {
        let legacy = Cell {
            heights: [2, 8, 2, 8],
            ..Cell::solid(2)
        };
        let mut world = World::from_parts(
            &[CellEdit {
                pos: [0, 0, 0],
                cell: Some(legacy),
            }],
            &[],
        )
        .unwrap();
        let cell = world.get([0, 0, 0]).unwrap();
        assert!(cell.shape.is_none());
        assert!(!serde_json::to_string(&cell).unwrap().contains("shape"));
        // Older maps can contain two wedges touching only along a collapsed
        // edge. Keep these legal legacy cells intact, even though explicit
        // shapes deliberately reject a non-manifold four-use boundary edge.
        let pinched = Cell {
            heights: [0, 8, 0, 8],
            ..Cell::solid(2)
        };
        assert!(pinched.is_valid());
        assert!(!pinched.as_shape().is_valid());
        world.set([2, 0, 0], Some(pinched));
        world.set(
            [1, 0, 0],
            Some(Cell {
                shape: Some(legacy.as_shape()),
                ..legacy
            }),
        );
        let restored = World::from_compact_parts(
            world.bounds(),
            &world.regions(),
            &world.cells(),
            &world.root_overrides(),
        )
        .unwrap();
        assert_eq!(restored.digest(), world.digest());
        assert_eq!(restored.cells(), world.cells());
    }
}
