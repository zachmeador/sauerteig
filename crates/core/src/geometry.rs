use crate::{
    Cell, CellPos, ChunkKey, Mesh, RayHit, Shape, Vec3, Vertex, World, CHUNK_SIZE, MATERIAL_COLORS,
    SHAPE_FACES, WORLD_HEIGHT, WORLD_MAX, WORLD_MIN,
};

const EPS: f32 = 1.0e-5;
const CORNERS: [[f32; 2]; 5] = [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0], [0.5, 0.5]];
const TOP_TRIANGLES: [[usize; 3]; 2] = [[0, 2, 1], [0, 3, 2]];
const OTHER_TRIANGLES: [[usize; 3]; 2] = [[0, 3, 1], [1, 3, 2]];
const FAN_TRIANGLES: [[usize; 3]; 4] = [[0, 4, 1], [1, 4, 2], [2, 4, 3], [3, 4, 0]];

/// Emits the actual boundary of the cell's two joined triangular prisms.
/// Ray tests use this too, so a hidden internal face cannot become an edit hit.
fn surfaces(world: &World, pos: CellPos, cell: Cell, mut emit: impl FnMut([Vec3; 3], u8)) {
    let origin = Vec3::new(pos[0] as f32, pos[1] as f32, pos[2] as f32);
    if let Some(shape) = cell.shape {
        for (triangle, face) in shape_triangles(shape, origin, 1.0).into_iter().flatten() {
            // Only cull an entire triangle when a full neighbor proves it is
            // hidden. Partial arbitrary faces keep their original triangles;
            // opaque depth testing hides their internal portions.
            let mut hidden = false;
            for axis in 0..3 {
                for sign in [-1, 1] {
                    let plane = origin[axis] + if sign > 0 { 1.0 } else { 0.0 };
                    if triangle.iter().all(|p| (p[axis] - plane).abs() < EPS) {
                        let mut adjacent = pos;
                        adjacent[axis] += sign;
                        hidden |= world.get(adjacent).is_some_and(Cell::is_full);
                    }
                }
            }
            if !hidden {
                emit(triangle, face);
            }
        }
        return;
    }
    let bottom = CORNERS.map(|[x, z]| origin + Vec3::new(x, 0.0, z));
    let heights = CORNERS.map(|[x, z]| cell.height_at(x, z));
    let top = std::array::from_fn::<_, 5, _>(|i| bottom[i] + Vec3::Y * heights[i]);
    let height_neighbor = |pos| {
        world.get(pos).and_then(|c| {
            if c.shape.is_none() {
                Some(c)
            } else if c.is_full() {
                Some(Cell::solid(c.material))
            } else {
                None
            }
        })
    };
    let above = height_neighbor([pos[0], pos[1] + 1, pos[2]]);
    let below = height_neighbor([pos[0], pos[1] - 1, pos[2]]);
    // Opposite diagonals require a common four-triangle partition when hiding
    // touching horizontal faces, especially collapsed half-cells.
    let split = above.is_some_and(|c| c.diagonal != cell.diagonal)
        || below.is_some_and(|c| c.diagonal != cell.diagonal);
    let triangles: &[[usize; 3]] = if split {
        &FAN_TRIANGLES
    } else if cell.diagonal {
        &OTHER_TRIANGLES
    } else {
        &TOP_TRIANGLES
    };
    for &triangle in triangles {
        if triangle.iter().all(|&i| heights[i] <= EPS) {
            continue;
        }
        let center = triangle
            .map(|i| CORNERS[i])
            .into_iter()
            .fold([0.0, 0.0], |sum, p| {
                [sum[0] + p[0] / 3.0, sum[1] + p[1] / 3.0]
            });
        let covered_top = triangle.iter().all(|&i| heights[i] >= 1.0 - EPS)
            && above.is_some_and(|c| c.height_at(center[0], center[1]) > EPS);
        if !covered_top {
            emit(triangle.map(|i| top[i]), 3);
        }
        let covered_bottom = below.is_some_and(|c| {
            triangle
                .iter()
                .all(|&i| c.height_at(CORNERS[i][0], CORNERS[i][1]) >= 1.0 - EPS)
        });
        if !covered_bottom {
            emit(
                [
                    bottom[triangle[0]],
                    bottom[triangle[2]],
                    bottom[triangle[1]],
                ],
                2,
            );
        }
    }

    // Each adjacent top edge is linear. Clip the side to the interval where
    // this cell is taller, then emit only the exposed band above its neighbor.
    // This handles opposing ramps without cracks or overlapping internal faces.
    const EDGES: [(usize, usize, CellPos, usize, usize); 4] = [
        (0, 1, [0, 0, -1], 3, 2),
        (1, 2, [1, 0, 0], 0, 3),
        (2, 3, [0, 0, 1], 1, 0),
        (3, 0, [-1, 0, 0], 2, 1),
    ];
    for (edge_index, (a, b, offset, na, nb)) in EDGES.into_iter().enumerate() {
        let adjacent = [pos[0] + offset[0], pos[1], pos[2] + offset[2]];
        let neighbor = height_neighbor(adjacent);
        let a_top = cell.heights[a] as f32 / 8.0;
        let b_top = cell.heights[b] as f32 / 8.0;
        let a_lower = neighbor.map_or(0.0, |n| n.heights[na] as f32 / 8.0);
        let b_lower = neighbor.map_or(0.0, |n| n.heights[nb] as f32 / 8.0);
        let da = a_top - a_lower;
        let db = b_top - b_lower;
        if da <= 0.0 && db <= 0.0 {
            continue;
        }
        let start = if da < 0.0 { -da / (db - da) } else { 0.0 };
        let end = if db < 0.0 { da / (da - db) } else { 1.0 };
        if end - start <= EPS {
            continue;
        }
        let at = |t: f32, h0: f32, h1: f32| {
            bottom[a].lerp(bottom[b], t) + Vec3::Y * (h0 + (h1 - h0) * t)
        };
        let lo_a = at(start, a_lower, b_lower);
        let lo_b = at(end, a_lower, b_lower);
        let hi_a = at(start, a_top, b_top);
        let hi_b = at(end, a_top, b_top);
        let face = [4, 1, 5, 0][edge_index];
        emit([lo_a, hi_a, hi_b], face);
        emit([lo_a, hi_b, lo_b], face);
    }
}

impl World {
    pub fn mesh_chunk(&self, key: ChunkKey) -> Mesh {
        let mut mesh = Mesh::default();
        for z in 0..CHUNK_SIZE {
            for y in 0..CHUNK_SIZE {
                for x in 0..CHUNK_SIZE {
                    let pos = [
                        key[0] * CHUNK_SIZE + x,
                        key[1] * CHUNK_SIZE + y,
                        key[2] * CHUNK_SIZE + z,
                    ];
                    if let Some(root) = self.fine.get(&pos) {
                        for triangle in &root.triangles {
                            append_triangle(
                                &mut mesh,
                                triangle.points,
                                root.fragments[triangle.fragment].cell.material,
                            );
                        }
                        continue;
                    }
                    let Some(cell) = self.get(pos) else {
                        continue;
                    };
                    let color = MATERIAL_COLORS[cell.material as usize];
                    surfaces(self, pos, cell, |triangle, _| {
                        let cross = (triangle[1] - triangle[0]).cross(triangle[2] - triangle[0]);
                        if cross.length_squared() <= EPS * EPS {
                            return;
                        }
                        let normal = cross.normalize().to_array();
                        let start = mesh.vertices.len() as u32;
                        mesh.vertices.extend(triangle.map(|position| Vertex {
                            position: position.to_array(),
                            normal,
                            color,
                            material: cell.material,
                        }));
                        mesh.indices.extend([start, start + 1, start + 2]);
                    });
                }
            }
        }
        mesh
    }

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
        let bounds_min = Vec3::new(WORLD_MIN as f32, 0.0, WORLD_MIN as f32);
        let bounds_max = Vec3::new(WORLD_MAX as f32, WORLD_HEIGHT as f32, WORLD_MAX as f32);
        let (enter, exit) = ray_box_interval(origin, direction, bounds_min, bounds_max)?;
        let mut distance = enter.max(0.0);
        let end = exit.min(max_distance);
        if distance > end {
            return None;
        }
        // Bias only cell selection, never the reported ray distance.
        let p = origin + direction * (distance + EPS);
        let mut cell_pos = p.floor().as_ivec3().to_array();
        let mut step = [0; 3];
        let mut t_max = [f32::INFINITY; 3];
        let mut t_delta = [f32::INFINITY; 3];
        for axis in 0..3 {
            if direction[axis].abs() <= EPS {
                continue;
            }
            step[axis] = if direction[axis] > 0.0 { 1 } else { -1 };
            let boundary = (cell_pos[axis] + if step[axis] > 0 { 1 } else { 0 }) as f32;
            t_max[axis] = (boundary - origin[axis]) / direction[axis];
            t_delta[axis] = 1.0 / direction[axis].abs();
        }
        while distance <= end + EPS && Self::is_valid_pos(cell_pos) {
            let next = t_max[0].min(t_max[1]).min(t_max[2]).min(end);
            let mut hit: Option<RayHit> = None;
            let mut test_triangle = |triangle: [Vec3; 3], face, bounds| {
                let Some(t) = ray_triangle(origin, direction, triangle) else {
                    return;
                };
                if t + EPS < distance || t > next + EPS || t > max_distance {
                    return;
                }
                if hit.as_ref().is_some_and(|previous| previous.distance <= t) {
                    return;
                }
                let normal = (triangle[1] - triangle[0])
                    .cross(triangle[2] - triangle[0])
                    .normalize();
                let point = origin + direction * t;
                // General neighbors may cover only part of this triangle.
                // Test this exact hit, never its centroid or a convex hull.
                // Sample along the ray, not a single face normal: at a
                // concave crease that normal can enter the other face.
                if self.contains_point(point - direction * (EPS * 8.0))
                    && self.contains_point(point + direction * (EPS * 8.0))
                {
                    return;
                }
                hit = Some(RayHit {
                    pos: cell_pos,
                    bounds,
                    face,
                    normal,
                    point,
                    distance: t,
                });
            };
            if let Some(root) = self.fine.get(&cell_pos) {
                for triangle in &root.triangles {
                    test_triangle(
                        triangle.points,
                        triangle.face,
                        root.fragments[triangle.fragment].bounds,
                    );
                }
            } else if let Some(cell) = self.get(cell_pos) {
                surfaces(self, cell_pos, cell, |triangle, face| {
                    test_triangle(triangle, face, crate::FineBox::root(cell_pos))
                });
            }
            if hit.is_some() {
                return hit;
            }
            if next >= end {
                break;
            }
            // Advance every tied axis so rays through an edge do not loop.
            for axis in 0..3 {
                if t_max[axis] <= next + EPS {
                    cell_pos[axis] += step[axis];
                    t_max[axis] += t_delta[axis];
                }
            }
            distance = next;
        }
        None
    }

    pub fn intersects_box(&self, min: Vec3, max: Vec3) -> bool {
        if !min.is_finite() || !max.is_finite() || min.cmpge(max).any() {
            return false;
        }
        let lo = min.floor().as_ivec3();
        let hi = max.ceil().as_ivec3();
        for z in lo.z.max(WORLD_MIN)..hi.z.min(WORLD_MAX) {
            for y in lo.y.max(0)..hi.y.min(WORLD_HEIGHT) {
                for x in lo.x.max(WORLD_MIN)..hi.x.min(WORLD_MAX) {
                    let pos = [x, y, z];
                    if self.fine.get(&pos).is_some_and(|r| {
                        r.fragments
                            .iter()
                            .any(|f| crate::fine::fragment_intersects_box(f, min, max))
                    }) {
                        return true;
                    }
                    if self
                        .get(pos)
                        .is_some_and(|cell| cell_intersects_box(pos, cell, min, max))
                    {
                        return true;
                    }
                }
            }
        }
        false
    }

    fn contains_point(&self, point: Vec3) -> bool {
        let pos = point.floor().as_ivec3().to_array();
        if let Some(root) = self.fine.get(&pos) {
            return root.fragments.iter().any(|f| {
                point.cmpge(f.bounds.world_min()).all()
                    && point.cmple(f.bounds.world_max()).all()
                    && cell_contains(f.cell, (point - f.world_origin()) / f.world_size())
            });
        }
        self.get(pos).is_some_and(|cell| {
            cell_contains(cell, point - Vec3::from_array(pos.map(|v| v as f32)))
        })
    }
}

/// Exact maximum of a piecewise linear top over an axis-aligned footprint.
/// Its extrema occur at rectangle corners or the cell diagonal's crossings.
pub(crate) fn max_height(
    cell: Cell,
    min_x: f32,
    min_z: f32,
    max_x: f32,
    max_z: f32,
) -> Option<f32> {
    max_height_with_epsilon(cell, min_x, min_z, max_x, max_z, EPS)
}

fn max_height_with_epsilon(
    cell: Cell,
    min_x: f32,
    min_z: f32,
    max_x: f32,
    max_z: f32,
    epsilon: f32,
) -> Option<f32> {
    let x0 = min_x.max(0.0);
    let x1 = max_x.min(1.0);
    let z0 = min_z.max(0.0);
    let z1 = max_z.min(1.0);
    if x1 - x0 <= epsilon || z1 - z0 <= epsilon {
        return None;
    }
    let mut height = cell
        .height_at(x0, z0)
        .max(cell.height_at(x1, z0))
        .max(cell.height_at(x1, z1))
        .max(cell.height_at(x0, z1));
    let start = if cell.diagonal {
        x0.max(1.0 - z1)
    } else {
        x0.max(z0)
    };
    let end = if cell.diagonal {
        x1.min(1.0 - z0)
    } else {
        x1.min(z1)
    };
    if start <= end {
        height = height
            .max(cell.height_at(start, if cell.diagonal { 1.0 - start } else { start }))
            .max(cell.height_at(end, if cell.diagonal { 1.0 - end } else { end }));
    }
    Some(height)
}

pub(crate) fn cell_intersects_box(pos: CellPos, cell: Cell, min: Vec3, max: Vec3) -> bool {
    if cell.shape.is_some() {
        return cell_intersects_box_scaled(
            Vec3::from_array(pos.map(|v| v as f32)),
            1.0,
            cell,
            min,
            max,
        );
    }
    if max.y <= pos[1] as f32 + EPS {
        return false;
    }
    let Some(height) = max_height(
        cell,
        min.x - pos[0] as f32,
        min.z - pos[2] as f32,
        max.x - pos[0] as f32,
        max.z - pos[2] as f32,
    ) else {
        return false;
    };
    height > EPS && min.y < pos[1] as f32 + height - EPS
}

pub(crate) fn ray_box_interval(
    origin: Vec3,
    direction: Vec3,
    min: Vec3,
    max: Vec3,
) -> Option<(f32, f32)> {
    let mut enter = f32::NEG_INFINITY;
    let mut exit = f32::INFINITY;
    for axis in 0..3 {
        if direction[axis].abs() <= EPS {
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

fn ray_triangle(origin: Vec3, direction: Vec3, [a, b, c]: [Vec3; 3]) -> Option<f32> {
    let edge1 = b - a;
    let edge2 = c - a;
    let p = direction.cross(edge2);
    let det = edge1.dot(p);
    // Subdivision can leave a small, perfectly valid triangle. Compare the
    // ray angle to its area instead of imposing a minimum world-space area.
    let area = edge1.cross(edge2).length();
    if area < 1.0e-12 || det.abs() < area * 1.0e-7 {
        return None;
    }
    let inv = 1.0 / det;
    let t = origin - a;
    let u = t.dot(p) * inv;
    if !(-EPS..=1.0 + EPS).contains(&u) {
        return None;
    }
    let q = t.cross(edge1);
    let v = direction.dot(q) * inv;
    if v < -EPS || u + v > 1.0 + EPS {
        return None;
    }
    let distance = edge2.dot(q) * inv;
    if distance >= -EPS {
        Some(distance.max(0.0))
    } else {
        None
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
                > EPS * EPS * size.powi(4)
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

fn shape_contains(shape: Shape, point: Vec3) -> bool {
    // Solid angle is insensitive to rays through shared edges and works for
    // concave closed surfaces. Use f64 only on this uncommon containment path.
    let mut angle = 0.0_f64;
    for (t, _) in shape_triangles(shape, Vec3::ZERO, 1.0)
        .into_iter()
        .flatten()
    {
        let [a, b, c] = t.map(|v| (v - point).as_dvec3());
        let denominator = a.length() * b.length() * c.length()
            + a.dot(b) * c.length()
            + b.dot(c) * a.length()
            + c.dot(a) * b.length();
        angle += 2.0 * a.dot(b.cross(c)).atan2(denominator);
    }
    angle > std::f64::consts::TAU
}

pub(crate) fn cell_contains(cell: Cell, point: Vec3) -> bool {
    if point.cmplt(Vec3::ZERO).any() || point.cmpgt(Vec3::ONE).any() {
        return false;
    }
    if let Some(shape) = cell.shape {
        shape_contains(shape, point)
    } else {
        point.y >= 0.0 && point.y <= cell.height_at(point.x, point.z)
    }
}

/// Origin/scale parameters keep the narrow phase reusable for future leaves.
pub(crate) fn cell_intersects_box_scaled(
    origin: Vec3,
    size: f32,
    cell: Cell,
    min: Vec3,
    max: Vec3,
) -> bool {
    // Keep tolerance in world metres when an authored source spans many roots.
    let epsilon = EPS / size;
    let min = (min - origin) / size + Vec3::splat(epsilon);
    let max = (max - origin) / size - Vec3::splat(epsilon);
    if min.cmpge(max).any() || max.cmple(Vec3::ZERO).any() || min.cmpge(Vec3::ONE).any() {
        return false;
    }
    let Some(shape) = cell.shape else {
        return max_height_with_epsilon(cell, min.x, min.z, max.x, max.z, epsilon)
            .is_some_and(|h| h > epsilon && min.y < h && max.y > 0.0);
    };
    for (triangle, _) in shape_triangles(shape, Vec3::ZERO, 1.0)
        .into_iter()
        .flatten()
    {
        if triangle_intersects_box(triangle, min, max) {
            return true;
        }
    }
    shape_contains(shape, (min + max) * 0.5)
}

/// Heights of a triangle clipped to the body's horizontal footprint. Keeping
/// the polygon (rather than sampling corners) catches narrow slanted ledges.
pub(crate) fn triangle_height_interval(
    triangle: [Vec3; 3],
    min: Vec3,
    max: Vec3,
) -> Option<(f32, f32)> {
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

pub(crate) fn shape_support(
    origin: Vec3,
    size: f32,
    shape: Shape,
    min: Vec3,
    max: Vec3,
    low: f32,
    high: f32,
    ceiling: bool,
) -> Option<f32> {
    let mut result: Option<f32> = None;
    for (triangle, _) in shape_triangles(shape, origin, size).into_iter().flatten() {
        let normal = (triangle[1] - triangle[0]).cross(triangle[2] - triangle[0]);
        if if ceiling {
            normal.y >= -EPS
        } else {
            normal.y <= EPS
        } {
            continue;
        }
        let Some((bottom, top)) = triangle_height_interval(triangle, min, max) else {
            continue;
        };
        let height = if ceiling { bottom } else { top };
        if height < low - EPS || height > high + EPS {
            continue;
        }
        result = Some(result.map_or(height, |old| {
            if ceiling {
                old.min(height)
            } else {
                old.max(height)
            }
        }));
    }
    result
}

#[cfg(test)]
mod shape_tests {
    use super::*;

    fn shaped(shape: Shape) -> Cell {
        Cell {
            shape: Some(shape),
            ..Cell::solid(0)
        }
    }

    #[test]
    fn every_face_accepts_corner_and_edge_shapes_with_matching_queries() {
        for face in 0..6 {
            for edge in [false, true] {
                let mut shape = Shape::solid();
                let axis = face / 2;
                for &corner in &SHAPE_FACES[face][..if edge { 2 } else { 1 }] {
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
                let half = Vec3::splat(0.004);
                assert!(cell_intersects_box(
                    [0, 0, 0],
                    cell,
                    inside - half,
                    inside + half
                ));
                assert!(!cell_intersects_box(
                    [0, 0, 0],
                    cell,
                    outside - half,
                    outside + half
                ));
                assert!(world
                    .mesh_chunk([0, 0, 0])
                    .vertices
                    .chunks_exact(3)
                    .any(|v| {
                        let triangle =
                            [v[0].position, v[1].position, v[2].position].map(Vec3::from_array);
                        ray_triangle(origin, direction, triangle)
                            .is_some_and(|distance| (distance - hit.distance).abs() < EPS)
                    }));
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
        let cell = shaped(shape);
        let in_notch = Vec3::new(0.5, 0.65, 0.5);
        assert!(!cell_contains(cell, in_notch));
        assert!(!cell_intersects_box(
            [0, 0, 0],
            cell,
            in_notch - Vec3::splat(0.01),
            in_notch + Vec3::splat(0.01)
        ));
        assert!(cell_intersects_box(
            [0, 0, 0],
            cell,
            Vec3::splat(0.05),
            Vec3::splat(0.15)
        ));
        assert!(cell_intersects_box(
            [0, 0, 0],
            cell,
            Vec3::splat(-1.0),
            Vec3::splat(2.0)
        ));
        let mut world = World::empty();
        world.set([0, 0, 0], Some(cell));
        let hit = world
            .raycast(Vec3::new(0.5, 2.0, 0.5), -Vec3::Y, 3.0)
            .unwrap();
        assert!((hit.point.y - 0.25).abs() < EPS);
    }

    #[test]
    fn general_shape_queries_reuse_origin_and_scale_without_resampling() {
        let mut shape = Shape::solid();
        shape.vertices[4][0] = 2;
        assert!(shape.is_valid());
        let cell = shaped(shape);
        let origin = Vec3::new(-4.5, 2.25, 9.0);
        for point in [Vec3::new(0.1, 0.9, 0.1), Vec3::new(0.4, 0.5, 0.5)] {
            let half = Vec3::splat(0.01);
            let expected =
                cell_intersects_box_scaled(Vec3::ZERO, 1.0, cell, point - half, point + half);
            assert_eq!(
                cell_intersects_box_scaled(
                    origin,
                    0.25,
                    cell,
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
            let json = serde_json::to_value(vec![crate::CellEdit {
                pos: [0, 0, 0],
                cell: Some(shaped(shape)),
            }])
            .unwrap();
            assert!(serde_json::from_value::<World>(json).is_err());
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
    fn shaped_save_roundtrip_and_legacy_decode_preserve_geometry() {
        let old = r#"[{"pos":[0,0,0],"cell":{"material":2,"heights":[2,8,2,8]}}]"#;
        let mut world: World = serde_json::from_str(old).unwrap();
        let legacy = world.get([0, 0, 0]).unwrap();
        assert!(legacy.shape.is_none());
        assert!(!serde_json::to_string(&legacy).unwrap().contains("shape"));
        // Older maps can contain two wedges touching only along a collapsed
        // edge. Keep these legal legacy cells intact, even though new explicit
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
        let restored: World =
            serde_json::from_str(&serde_json::to_string(&world).unwrap()).unwrap();
        assert_eq!(restored.cells(), world.cells());
    }
}

fn append_triangle(mesh: &mut Mesh, triangle: [Vec3; 3], material: u8) {
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
