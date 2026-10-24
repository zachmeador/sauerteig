//! Sparse exact subdivision. A fragment retains its authored source cell and
//! clips that solid to an eighth-metre box; subdivision never resamples a slope.
use crate::{Cell, CellEdit, CellPos, ChunkKey, Vec3, World};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

pub const FINE_UNITS: i32 = 8;
pub const MAX_ROOT_FRAGMENTS: usize = 512;
/// Legacy octree leaves may author a large slope once and clip it into many
/// one-metre roots. Bound the source to 8 km so coordinate math stays precise.
pub const MAX_FRAGMENT_SOURCE_SIZE: u32 = 65_536;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct FineBox {
    pub min: [i32; 3],
    pub max: [i32; 3],
}
impl FineBox {
    pub fn is_empty(self) -> bool {
        (0..3).any(|a| self.min[a] >= self.max[a])
    }
    pub fn intersection(self, other: Self) -> Option<Self> {
        let result = Self {
            min: std::array::from_fn(|a| self.min[a].max(other.min[a])),
            max: std::array::from_fn(|a| self.max[a].min(other.max[a])),
        };
        (!result.is_empty()).then_some(result)
    }
    pub fn subtract(self, other: Self) -> Vec<Self> {
        if self.is_empty() {
            return Vec::new();
        }
        let Some(cut) = self.intersection(other) else {
            return vec![self];
        };
        let mut remaining = self;
        let mut result = Vec::with_capacity(6);
        for a in 0..3 {
            if remaining.min[a] < cut.min[a] {
                let mut piece = remaining;
                piece.max[a] = cut.min[a];
                result.push(piece);
                remaining.min[a] = cut.min[a];
            }
            if remaining.max[a] > cut.max[a] {
                let mut piece = remaining;
                piece.min[a] = cut.max[a];
                result.push(piece);
                remaining.max[a] = cut.max[a];
            }
        }
        result
    }
    pub fn root(pos: CellPos) -> Self {
        let min = pos.map(|v| v * 8);
        Self {
            min,
            max: min.map(|v| v + 8),
        }
    }
    pub fn contains(self, other: Self) -> bool {
        !other.is_empty()
            && (0..3).all(|a| self.min[a] <= other.min[a] && self.max[a] >= other.max[a])
    }
    pub fn world_min(self) -> Vec3 {
        Vec3::from_array(self.min.map(|v| v as f32 / 8.0))
    }
    pub fn world_max(self) -> Vec3 {
        Vec3::from_array(self.max.map(|v| v as f32 / 8.0))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CellFragment {
    pub bounds: FineBox,
    pub origin: [i32; 3],
    pub size: u32,
    pub cell: Cell,
}
impl CellFragment {
    pub fn from_cell(pos: CellPos, cell: Cell) -> Self {
        Self {
            bounds: FineBox::root(pos),
            origin: pos.map(|v| v * 8),
            size: 8,
            cell,
        }
    }
    pub fn source_bounds(&self) -> Option<FineBox> {
        let mut max = [0; 3];
        for (a, v) in max.iter_mut().enumerate() {
            *v = self.origin[a].checked_add(i32::try_from(self.size).ok()?)?;
        }
        Some(FineBox {
            min: self.origin,
            max,
        })
    }
    pub fn is_valid(&self) -> bool {
        self.size.is_power_of_two()
            && self.size <= MAX_FRAGMENT_SOURCE_SIZE
            && self
                .source_bounds()
                .is_some_and(|b| b.contains(self.bounds))
            && self.cell.is_valid()
    }
    pub fn clipped(&self, bounds: FineBox) -> Option<Self> {
        let mut result = self.clone();
        result.bounds = self.bounds.intersection(bounds)?;
        Some(result)
    }
    pub fn world_origin(&self) -> Vec3 {
        Vec3::from_array(self.origin.map(|v| v as f32 / 8.0))
    }
    pub fn world_size(&self) -> f32 {
        self.size as f32 / 8.0
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RootEdit {
    pub pos: CellPos,
    pub fragments: Vec<CellFragment>,
}

#[derive(Clone, Debug)]
pub(crate) struct FineTriangle {
    pub points: [Vec3; 3],
    pub face: u8,
    pub fragment: usize,
}
#[derive(Clone, Debug)]
pub(crate) struct FineRoot {
    pub fragments: Vec<CellFragment>,
    pub triangles: Vec<FineTriangle>,
}

pub fn validate_root_edits(edits: &[RootEdit]) -> Result<(), String> {
    let mut roots = BTreeSet::new();
    for edit in edits {
        if !World::is_valid_pos(edit.pos) || !roots.insert(edit.pos) {
            return Err("Invalid or repeated detailed root.".into());
        }
        if edit.fragments.len() > MAX_ROOT_FRAGMENTS {
            return Err("A root has too many fragments; simplify the edit.".into());
        }
        let root = FineBox::root(edit.pos);
        for (i, f) in edit.fragments.iter().enumerate() {
            if !f.is_valid() || !root.contains(f.bounds) {
                return Err("Invalid fragment bounds or source geometry.".into());
            }
            if edit.fragments[..i]
                .iter()
                .any(|previous| previous.bounds.intersection(f.bounds).is_some())
            {
                return Err("Fragment boxes must not overlap.".into());
            }
        }
    }
    Ok(())
}

/// Full solids have no source-dependent surface to preserve. Give them the
/// root's source cube and merge matching rectangular faces before compiling
/// geometry. Warped sources and material boundaries remain untouched.
///
/// The caller must first establish that the fragments are valid, disjoint, and
/// contained in this root. This is normalization, not input validation.
pub fn canonical_root_fragments(pos: CellPos, fragments: &[CellFragment]) -> Vec<CellFragment> {
    let root = FineBox::root(pos);
    let mut solids = Vec::new();
    let mut others = Vec::new();
    for fragment in fragments {
        if fragment.cell.is_full() {
            solids.push(CellFragment {
                bounds: fragment.bounds,
                origin: root.min,
                size: 8,
                cell: Cell::solid(fragment.cell.material),
            });
        } else {
            others.push(fragment.clone());
        }
    }
    if solids.is_empty() {
        return others;
    }
    // Validation established disjoint boxes inside this root, so matching
    // integer volume proves full coverage even for a non-grid partition.
    if others.is_empty()
        && solids
            .iter()
            .all(|f| f.cell.material == solids[0].cell.material)
        && solids
            .iter()
            .map(|f| {
                (0..3)
                    .map(|a| f.bounds.max[a] - f.bounds.min[a])
                    .product::<i32>()
            })
            .sum::<i32>()
            == 8 * 8 * 8
    {
        return vec![CellFragment::from_cell(pos, solids[0].cell)];
    }
    loop {
        let before = solids.len();
        for axis in 0..3 {
            let u = (axis + 1) % 3;
            let v = (axis + 2) % 3;
            solids.sort_by_key(|f| {
                (
                    f.cell.material,
                    f.bounds.min[u],
                    f.bounds.max[u],
                    f.bounds.min[v],
                    f.bounds.max[v],
                    f.bounds.min[axis],
                    f.bounds.max[axis],
                )
            });
            let mut merged: Vec<CellFragment> = Vec::with_capacity(solids.len());
            for fragment in solids.drain(..) {
                if let Some(last) = merged.last_mut() {
                    if last.cell.material == fragment.cell.material
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
            solids = merged;
        }
        if solids.len() == before {
            break;
        }
    }
    others.extend(solids);
    // Input ordering cannot affect a coalesced root's snapshot or no-op check.
    others.sort_by_key(|f| f.bounds);
    others
}

impl World {
    pub fn from_parts(cells: &[CellEdit], roots: &[RootEdit]) -> Result<Self, String> {
        let mut seen = BTreeSet::new();
        for c in cells {
            if !Self::is_valid_pos(c.pos)
                || c.cell.is_some_and(|v| !v.is_valid())
                || !seen.insert(c.pos)
            {
                return Err("Invalid or repeated coarse cell.".into());
            }
        }
        if roots.iter().any(|r| seen.contains(&r.pos)) {
            return Err("A saved root has both coarse and detailed geometry.".into());
        }
        let mut world = Self::from_cells(cells);
        world.apply_root_edits(roots)?;
        Ok(world)
    }
    pub fn root_fragments(&self, pos: CellPos) -> Vec<CellFragment> {
        self.fine.get(&pos).map_or_else(
            || {
                self.get(pos)
                    .map(|cell| vec![CellFragment::from_cell(pos, cell)])
                    .unwrap_or_default()
            },
            |r| r.fragments.clone(),
        )
    }
    pub fn detailed_roots(&self) -> Vec<RootEdit> {
        self.fine
            .iter()
            .map(|(&pos, r)| RootEdit {
                pos,
                fragments: r.fragments.clone(),
            })
            .collect()
    }
    pub fn roots_in(&self, bounds: FineBox) -> Vec<RootEdit> {
        if bounds.is_empty() {
            return Vec::new();
        }
        // Enumerate occupied storage, never the volume of a user's selection.
        let mut roots: Vec<_> = self
            .cells()
            .into_iter()
            .filter(|c| FineBox::root(c.pos).intersection(bounds).is_some())
            .map(|c| RootEdit {
                pos: c.pos,
                fragments: vec![CellFragment::from_cell(c.pos, c.cell.unwrap())],
            })
            .collect();
        roots.extend(
            self.fine
                .iter()
                .filter(|(pos, _)| FineBox::root(**pos).intersection(bounds).is_some())
                .map(|(&pos, r)| RootEdit {
                    pos,
                    fragments: r.fragments.clone(),
                }),
        );
        roots.sort_by_key(|r| r.pos);
        roots
    }
    pub fn apply_root_edits(&mut self, edits: &[RootEdit]) -> Result<Vec<ChunkKey>, String> {
        validate_root_edits(edits)?;
        // Compile every replacement before mutating anything; clipping failures
        // cannot leave a half-applied collaborative operation.
        enum Prepared {
            Coarse(Cell),
            Detailed(FineRoot),
        }
        let mut prepared = Vec::with_capacity(edits.len());
        for edit in edits {
            if let [fragment] = edit.fragments.as_slice() {
                if fragment.bounds == FineBox::root(edit.pos)
                    && fragment.origin == fragment.bounds.min
                    && fragment.size == 8
                {
                    prepared.push((edit.pos, Prepared::Coarse(fragment.cell)));
                    continue;
                }
            }
            let normalized = canonical_root_fragments(edit.pos, &edit.fragments);
            if let [fragment] = normalized.as_slice() {
                if fragment.bounds == FineBox::root(edit.pos)
                    && fragment.origin == fragment.bounds.min
                    && fragment.size == 8
                {
                    prepared.push((edit.pos, Prepared::Coarse(fragment.cell)));
                    continue;
                }
            }
            let mut fragments = Vec::new();
            let mut triangles = Vec::new();
            for fragment in &normalized {
                if !fragment_intersects_box(
                    fragment,
                    fragment.bounds.world_min(),
                    fragment.bounds.world_max(),
                ) {
                    continue;
                }
                let source = clip_fragment(fragment)?;
                let index = fragments.len();
                for (points, face) in source {
                    triangles.push(FineTriangle {
                        points,
                        face,
                        fragment: index,
                    });
                }
                fragments.push(fragment.clone());
            }
            prepared.push((
                edit.pos,
                Prepared::Detailed(FineRoot {
                    fragments,
                    triangles,
                }),
            ));
        }
        let mut dirty = BTreeSet::new();
        for (pos, prepared) in prepared {
            match prepared {
                Prepared::Coarse(cell) => {
                    if self.get(pos) == Some(cell) && !self.fine.contains_key(&pos) {
                        continue;
                    }
                    self.set(pos, Some(cell));
                }
                Prepared::Detailed(root) => {
                    if self.root_fragments(pos) == root.fragments {
                        continue;
                    }
                    self.set(pos, None);
                    if !root.fragments.is_empty() {
                        self.fine.insert(pos, root);
                    }
                }
            }
            let key = Self::cell_chunk(pos);
            dirty.insert(key);
            for a in 0..3 {
                for step in [-1, 1] {
                    let mut next = key;
                    next[a] += step;
                    dirty.insert(next);
                }
            }
        }
        Ok(dirty.into_iter().collect())
    }
}

pub(crate) fn fragment_intersects_box(fragment: &CellFragment, min: Vec3, max: Vec3) -> bool {
    let min = min.max(fragment.bounds.world_min());
    let max = max.min(fragment.bounds.world_max());
    min.cmplt(max).all()
        && crate::geometry::cell_intersects_box_scaled(
            fragment.world_origin(),
            fragment.world_size(),
            fragment.cell,
            min,
            max,
        )
}

fn clip_fragment(fragment: &CellFragment) -> Result<Vec<([Vec3; 3], u8)>, String> {
    // The common fine floor/wall/box needs no polygon clipping or cap solving.
    if fragment.cell.is_full() {
        let min = fragment.bounds.world_min();
        let span = fragment.bounds.world_max() - min;
        return Ok(
            crate::geometry::shape_triangles(crate::Shape::solid(), Vec3::ZERO, 1.0)
                .into_iter()
                .flatten()
                .map(|(t, face)| (t.map(|p| min + p * span), face))
                .collect(),
        );
    }
    let origin = glam::DVec3::from_array(fragment.origin.map(|v| v as f64 / 8.0));
    let size = fragment.size as f64 / 8.0;
    let mut triangles: Vec<_> =
        crate::geometry::shape_triangles(fragment.cell.as_shape(), Vec3::ZERO, 1.0)
            .into_iter()
            .flatten()
            .map(|(t, f)| (t.map(|p| origin + p.as_dvec3() * size), f))
            .collect();
    for axis in 0..3 {
        for (plane, keep_sign, face) in [
            (
                fragment.bounds.min[axis] as f64 / 8.0,
                1.0,
                (axis * 2) as u8,
            ),
            (
                fragment.bounds.max[axis] as f64 / 8.0,
                -1.0,
                (axis * 2 + 1) as u8,
            ),
        ] {
            triangles = clip_plane(triangles, axis, plane, keep_sign, face)?;
        }
    }
    Ok(triangles
        .into_iter()
        .map(|(t, f)| (t.map(|p| p.as_vec3()), f))
        .collect())
}

const CLIP_EPS: f64 = 1.0e-9;
type DTriangle = ([glam::DVec3; 3], u8);
fn same_point(a: glam::DVec3, b: glam::DVec3) -> bool {
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
    for (triangle, face) in triangles {
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
            result.push((triangle, face));
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
            if (t[1] - t[0]).cross(t[2] - t[0]).length_squared() > CLIP_EPS.powi(4) {
                result.push((t, face));
            }
        }
    }
    // Split collinear segments at every endpoint before cancelling opposite
    // copies. Existing triangulation can put a T junction on a cut plane.
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
    let mut edges: Vec<(glam::DVec3, glam::DVec3)> = Vec::new();
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
    let mut loops: Vec<Vec<glam::DVec3>> = Vec::new();
    while let Some((start, mut end)) = edges.pop() {
        let mut points = vec![start];
        let mut previous = start;
        let mut guard = 0;
        while !same_point(end, start) {
            points.push(end);
            guard += 1;
            if guard > 256 {
                return Err("Subdivision cap exceeded its boundary budget.".into());
            }
            let incoming = end - previous;
            let candidates: Vec<_> = edges
                .iter()
                .enumerate()
                .filter(|(_, e)| same_point(e.0, end))
                .collect();
            let chosen = candidates
                .into_iter()
                .max_by(|(_, a), (_, b)| {
                    let turn = |to: glam::DVec3| {
                        let outgoing = to - end;
                        let cross = incoming[u] * outgoing[v] - incoming[v] * outgoing[u];
                        (-keep_sign * cross)
                            .atan2(incoming[u] * outgoing[u] + incoming[v] * outgoing[v])
                    };
                    turn(a.1).total_cmp(&turn(b.1))
                })
                .map(|(index, _)| index)
                .ok_or_else(|| "Subdivision cap boundary did not close.".to_string())?;
            let next = edges.swap_remove(chosen);
            previous = end;
            end = next.1;
        }
        if points.len() >= 3 {
            loops.push(points);
        }
    }
    let area = |points: &[glam::DVec3]| {
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
        return Err("Subdivision cap has no exterior boundary.".into());
    }
    let contains = |ring: &[glam::DVec3], point: glam::DVec3| {
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
            .ok_or_else(|| "Subdivision cap hole has no exterior.".to_string())?;
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
            .map_err(|e| format!("Subdivision cap triangulation failed: {e:?}"))?;
        let mut actual_area = 0.0;
        for ids in indices.as_chunks::<3>().0 {
            let mut t = [points[ids[0]], points[ids[1]], points[ids[2]]];
            let cross = (t[1] - t[0]).cross(t[2] - t[0]);
            actual_area += cross[axis].abs() * 0.5;
            if cross[axis] * (-keep_sign) < 0.0 {
                t.swap(1, 2);
            }
            result.push((t, cap_face));
        }
        if (actual_area - expected_area).abs() > 1.0e-8_f64.max(expected_area * 1.0e-7) {
            return Err("Subdivision cap triangulation did not preserve its area.".into());
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn refined(cell: Cell, step: i32) -> RootEdit {
        let source = CellFragment::from_cell([0, 0, 0], cell);
        let mut fragments = Vec::new();
        for x in (0..8).step_by(step as usize) {
            for y in (0..8).step_by(step as usize) {
                for z in (0..8).step_by(step as usize) {
                    fragments.push(
                        source
                            .clipped(FineBox {
                                min: [x, y, z],
                                max: [x + step, y + step, z + step],
                            })
                            .unwrap(),
                    );
                }
            }
        }
        RootEdit {
            pos: [0, 0, 0],
            fragments,
        }
    }

    #[test]
    fn fine_boxes_subtract_without_enumerating_their_volume() {
        let bounds = FineBox {
            min: [-800, -800, -800],
            max: [800, 800, 2400],
        };
        let cut = FineBox {
            min: [0, 0, 0],
            max: [400, 400, 400],
        };
        let pieces = bounds.subtract(cut);
        assert!(pieces.len() <= 6);
        let volume = |b: FineBox| {
            (0..3)
                .map(|a| (b.max[a] - b.min[a]) as i64)
                .product::<i64>()
        };
        assert_eq!(
            pieces.iter().copied().map(volume).sum::<i64>() + volume(cut),
            volume(bounds)
        );
        for (i, p) in pieces.iter().enumerate() {
            assert!(p.intersection(cut).is_none());
            assert!(pieces[..i]
                .iter()
                .all(|other| p.intersection(*other).is_none()));
        }
    }

    #[test]
    fn all_fine_grid_sizes_have_real_caps_and_collision() {
        for size in [1, 2, 4] {
            let mut world = World::empty();
            let fragment = CellFragment {
                bounds: FineBox {
                    min: [1, 1, 1],
                    max: [1 + size, 1 + size, 1 + size],
                },
                origin: [1, 1, 1],
                size: size as u32,
                cell: Cell::solid(1),
            };
            world
                .apply_root_edits(&[RootEdit {
                    pos: [0, 0, 0],
                    fragments: vec![fragment.clone()],
                }])
                .unwrap();
            let center = (fragment.bounds.world_min() + fragment.bounds.world_max()) * 0.5;
            for axis in 0..3 {
                let mut origin = center;
                origin[axis] = 2.0;
                let mut direction = Vec3::ZERO;
                direction[axis] = -1.0;
                let hit = world.raycast(origin, direction, 4.0).unwrap();
                assert_eq!(hit.bounds, fragment.bounds);
                assert_eq!(hit.face, (axis * 2 + 1) as u8);
                assert!((hit.point[axis] - fragment.bounds.world_max()[axis]).abs() < 1.0e-5);
            }
            assert!(world.intersects_box(center - Vec3::splat(0.01), center + Vec3::splat(0.01)));
            assert!(!world.intersects_box(Vec3::splat(0.001), Vec3::splat(0.1)));
            let volume = world.fine[&[0, 0, 0]]
                .triangles
                .iter()
                .map(|t| t.points[0].dot(t.points[1].cross(t.points[2])) / 6.0)
                .sum::<f32>();
            assert!(
                (volume - (size as f32 / 8.0).powi(3)).abs() < 1.0e-5,
                "volume {volume}"
            );
        }
        let source = CellFragment::from_cell([0, 0, 0], Cell::solid(0));
        let mut world = World::empty();
        world
            .apply_root_edits(&[RootEdit {
                pos: [0, 0, 0],
                fragments: vec![source
                    .clipped(FineBox {
                        min: [0, 0, 0],
                        max: [4, 8, 8],
                    })
                    .unwrap()],
            }])
            .unwrap();
        let hit = world
            .raycast(Vec3::new(2.0, 0.37, 0.47), -Vec3::X, 3.0)
            .unwrap();
        assert!((hit.point.x - 0.5).abs() < 1.0e-5);
        assert_eq!(hit.face, 1);
    }

    #[test]
    fn tiny_solid_fills_coalesce_and_clear_restore_exactly() {
        let tiny = |x, y, z, material| CellFragment {
            bounds: FineBox {
                min: [x, y, z],
                max: [x + 1, y + 1, z + 1],
            },
            origin: [x, y, z],
            size: 1,
            cell: Cell::solid(material),
        };
        let mut half = Vec::new();
        let mut rest = Vec::new();
        for x in 0..8 {
            for y in 0..8 {
                for z in 0..8 {
                    if x < 4 {
                        half.push(tiny(x, y, z, 2));
                    } else {
                        rest.push(tiny(x, y, z, 2));
                    }
                }
            }
        }
        let mut world = World::empty();
        world
            .apply_root_edits(&[RootEdit {
                pos: [0, 0, 0],
                fragments: half.clone(),
            }])
            .unwrap();
        let expected = CellFragment::from_cell([0, 0, 0], Cell::solid(2))
            .clipped(FineBox {
                min: [0, 0, 0],
                max: [4, 8, 8],
            })
            .unwrap();
        assert_eq!(world.root_fragments([0, 0, 0]), vec![expected.clone()]);
        assert_eq!(world.fine[&[0, 0, 0]].triangles.len(), 12);
        half.reverse();
        assert!(world
            .apply_root_edits(&[RootEdit {
                pos: [0, 0, 0],
                fragments: half.clone(),
            }])
            .unwrap()
            .is_empty());
        let cap = world
            .raycast(Vec3::new(2.0, 0.3, 0.7), -Vec3::X, 3.0)
            .unwrap();
        assert!((cap.point.x - 0.5).abs() < 1.0e-6);
        assert!(world.intersects_box(Vec3::new(0.1, 0.1, 0.1), Vec3::new(0.4, 0.9, 0.9)));
        assert!(!world.intersects_box(Vec3::new(0.6, 0.1, 0.1), Vec3::new(0.9, 0.9, 0.9)));

        // A new fill adjoining a previously coalesced half also becomes coarse.
        rest.push(expected);
        world
            .apply_root_edits(&[RootEdit {
                pos: [0, 0, 0],
                fragments: rest,
            }])
            .unwrap();
        assert_eq!(world.get([0, 0, 0]), Some(Cell::solid(2)));
        assert!(world.detailed_roots().is_empty());
        let full = world.root_fragments([0, 0, 0]);
        world
            .apply_root_edits(&[RootEdit {
                pos: [0, 0, 0],
                fragments: Vec::new(),
            }])
            .unwrap();
        assert!(world.root_fragments([0, 0, 0]).is_empty());
        world
            .apply_root_edits(&[RootEdit {
                pos: [0, 0, 0],
                fragments: full.clone(),
            }])
            .unwrap();
        assert_eq!(world.root_fragments([0, 0, 0]), full);

        // Restoring all 512 independently authored tiny cubes has the same state.
        let mut complete = half;
        for x in 4..8 {
            for y in 0..8 {
                for z in 0..8 {
                    complete.push(tiny(x, y, z, 2));
                }
            }
        }
        assert_eq!(complete.len(), 512);
        assert!(world
            .apply_root_edits(&[RootEdit {
                pos: [0, 0, 0],
                fragments: complete
            }])
            .unwrap()
            .is_empty());
    }

    #[test]
    fn solid_coalescing_preserves_materials_and_warped_sources() {
        let mut edit = refined(Cell::solid(0), 1);
        for f in &mut edit.fragments {
            f.cell.material = u8::from(f.bounds.min[0] >= 4);
            f.origin = f.bounds.min;
            f.size = 1;
        }
        let mut world = World::empty();
        world.apply_root_edits(&[edit]).unwrap();
        let halves = world.root_fragments([0, 0, 0]);
        assert_eq!(halves.len(), 2);
        assert_eq!(halves[0].cell.material, 0);
        assert_eq!(halves[1].cell.material, 1);
        assert_eq!(halves[0].bounds.max[0], 4);
        assert_eq!(halves[1].bounds.min[0], 4);
        assert_eq!(world.get([0, 0, 0]), None);

        let warp = CellFragment::from_cell(
            [0, 0, 0],
            Cell {
                heights: [2, 8, 6, 3],
                ..Cell::solid(1)
            },
        )
        .clipped(halves[0].bounds)
        .unwrap();
        world
            .apply_root_edits(&[RootEdit {
                pos: [0, 0, 0],
                fragments: vec![halves[1].clone(), warp.clone()],
            }])
            .unwrap();
        assert_eq!(
            world.root_fragments([0, 0, 0]),
            vec![warp.clone(), halves[1].clone()]
        );
        let hit = world
            .raycast(Vec3::new(0.2, 2.0, 0.3), -Vec3::Y, 3.0)
            .unwrap();
        assert!((hit.point.y - warp.cell.height_at(0.2, 0.3)).abs() < 1.0e-6);
    }

    #[test]
    fn exact_refinement_preserves_warped_and_pinched_legacy_sources() {
        for heights in [[1, 8, 3, 6], [0, 8, 0, 8], [8, 0, 8, 0], [0, 0, 8, 8]] {
            for diagonal in [false, true] {
                let cell = Cell {
                    heights,
                    diagonal,
                    ..Cell::solid(2)
                };
                let mut original = World::empty();
                original.set([0, 0, 0], Some(cell));
                for step in [4, 2, 1] {
                    let mut world = World::empty();
                    world
                        .apply_root_edits(&[refined(cell, step)])
                        .unwrap_or_else(|e| {
                            panic!("{heights:?}, diagonal {diagonal}, step {step}: {e}")
                        });
                    for sample in 0..90 {
                        let x = ((sample * 37 % 89) as f32 + 0.31) / 90.0;
                        let z = ((sample * 53 % 89) as f32 + 0.27) / 90.0;
                        let origin = Vec3::new(x, 2.0, z);
                        let old = original.raycast(origin, -Vec3::Y, 4.0);
                        let new = world.raycast(origin, -Vec3::Y, 4.0);
                        assert_eq!(old.is_some(), new.is_some());
                        if let (Some(a), Some(b)) = (old, new) {
                            assert!(
                                (a.distance - b.distance).abs() < 2.0e-5,
                                "ray {heights:?} step {step} {origin:?}: {} != {}",
                                a.distance,
                                b.distance
                            );
                        }
                        let center = Vec3::new(x, (sample * 17 % 89) as f32 / 90.0, z);
                        let half = Vec3::splat(0.037);
                        assert_eq!(
                            original.intersects_box(center - half, center + half),
                            world.intersects_box(center - half, center + half),
                            "collision {heights:?} {center:?}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn generalized_sources_keep_volume_and_queries_after_exact_clipping() {
        let mut seed = 0x8fa3_6172_u32;
        let mut next = || {
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            seed
        };
        let mut checked = 0;
        for _ in 0..64 {
            let mut shape = crate::Shape::solid();
            for point in &mut shape.vertices {
                for coordinate in point {
                    let inset = (next() % 4) as u8;
                    *coordinate = if *coordinate == 0 { inset } else { 8 - inset };
                }
            }
            shape.diagonals = (next() % 64) as u8;
            if !shape.is_valid() {
                continue;
            }
            let cell = Cell {
                shape: Some(shape),
                ..Cell::solid(1)
            };
            let mut source = World::empty();
            source.set([0, 0, 0], Some(cell));
            let mut fine = World::empty();
            fine.apply_root_edits(&[refined(cell, if checked < 3 { 1 } else { 2 })])
                .unwrap();
            let volume = |triangles: Vec<[Vec3; 3]>| {
                triangles
                    .iter()
                    .map(|t| t[0].dot(t[1].cross(t[2])) / 6.0)
                    .sum::<f32>()
            };
            let before = volume(
                crate::geometry::shape_triangles(shape, Vec3::ZERO, 1.0)
                    .into_iter()
                    .flatten()
                    .map(|(t, _)| t)
                    .collect(),
            );
            let after = volume(
                fine.fine[&[0, 0, 0]]
                    .triangles
                    .iter()
                    .map(|t| t.points)
                    .collect(),
            );
            assert!(
                (before - after).abs() < 2.0e-5,
                "volume {before} != {after}"
            );
            for sample in 0..240 {
                let mut origin = Vec3::new(
                    (next() % 1000) as f32 / 1000.0,
                    (next() % 1000) as f32 / 1000.0,
                    (next() % 1000) as f32 / 1000.0,
                );
                let axis = sample % 3;
                origin[axis] = if sample % 2 == 0 { -1.0 } else { 2.0 };
                let mut direction = Vec3::ZERO;
                direction[axis] = if sample % 2 == 0 { 1.0 } else { -1.0 };
                let a = source.raycast(origin, direction, 4.0);
                let b = fine.raycast(origin, direction, 4.0);
                assert_eq!(
                    a.is_some(),
                    b.is_some(),
                    "shape {shape:?}, ray {origin:?} {direction:?}"
                );
                if let (Some(a), Some(b)) = (a, b) {
                    assert!(
                        (a.distance - b.distance).abs() < 3.0e-5,
                        "ray {} != {} for {origin:?} {direction:?}",
                        a.distance,
                        b.distance
                    );
                }
                let center = Vec3::new(
                    (next() % 1000) as f32 / 1000.0,
                    (next() % 1000) as f32 / 1000.0,
                    (next() % 1000) as f32 / 1000.0,
                );
                let half = Vec3::splat(0.03);
                assert_eq!(
                    source.intersects_box(center - half, center + half),
                    fine.intersects_box(center - half, center + half)
                );
            }
            checked += 1;
            if checked == 12 {
                break;
            }
        }
        assert_eq!(checked, 12);
    }

    #[test]
    fn clipped_warped_cut_has_visible_cap_and_keeps_the_remaining_slope() {
        let cell = Cell {
            heights: [2, 8, 6, 3],
            ..Cell::solid(2)
        };
        let source = CellFragment::from_cell([0, 0, 0], cell);
        let cut = FineBox {
            min: [3, 0, 2],
            max: [5, 8, 6],
        };
        let fragments = source
            .bounds
            .subtract(cut)
            .into_iter()
            .map(|b| source.clipped(b).unwrap())
            .collect();
        let mut world = World::empty();
        world
            .apply_root_edits(&[RootEdit {
                pos: [0, 0, 0],
                fragments,
            }])
            .unwrap();
        assert!(!world.intersects_box(Vec3::new(0.4, 0.01, 0.4), Vec3::new(0.6, 0.2, 0.6)));
        let hit = world
            .raycast(Vec3::new(0.5, 0.1, 0.5), -Vec3::X, 1.0)
            .unwrap();
        assert!((hit.point.x - 0.375).abs() < 1.0e-5);
        assert_eq!(hit.face, 1);
        let top = world
            .raycast(Vec3::new(0.2, 2.0, 0.2), -Vec3::Y, 4.0)
            .unwrap();
        assert!((top.point.y - cell.height_at(0.2, 0.2)).abs() < 1.0e-5);
    }

    #[test]
    fn root_replacements_are_atomic_and_snapshots_keep_sparse_detail() {
        let mut world = World::empty();
        world.set([1, 0, 0], Some(Cell::solid(3)));
        let edit = refined(
            Cell {
                heights: [1, 6, 7, 2],
                ..Cell::solid(0)
            },
            2,
        );
        world.apply_root_edits(&[edit]).unwrap();
        let before = world.clone();
        let duplicate = world.root_fragments([0, 0, 0])[0].clone();
        assert!(world
            .apply_root_edits(&[
                RootEdit {
                    pos: [1, 0, 0],
                    fragments: Vec::new()
                },
                RootEdit {
                    pos: [0, 0, 0],
                    fragments: vec![duplicate.clone(), duplicate]
                }
            ])
            .is_err());
        assert_eq!(world.cells(), before.cells());
        assert_eq!(world.detailed_roots(), before.detailed_roots());
        let json = serde_json::to_string(&world).unwrap();
        let restored: World = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.cells(), world.cells());
        assert_eq!(restored.detailed_roots(), world.detailed_roots());
        assert_eq!(
            world
                .roots_in(FineBox {
                    min: [-1_000_000; 3],
                    max: [1_000_000; 3]
                })
                .len(),
            2
        );
        world.set([0, 0, 0], Some(Cell::solid(4)));
        assert!(world.detailed_roots().is_empty());
        assert_eq!(world.get([0, 0, 0]), Some(Cell::solid(4)));
    }

    #[test]
    fn movement_stands_on_an_eighth_metre_fragment_and_collision_updates_immediately() {
        let source = CellFragment::from_cell([0, 1, 0], Cell::solid(0));
        let fragment = source
            .clipped(FineBox {
                min: [0, 8, 0],
                max: [8, 9, 8],
            })
            .unwrap();
        let mut world = World::empty();
        world
            .apply_root_edits(&[RootEdit {
                pos: [0, 1, 0],
                fragments: vec![fragment.clone()],
            }])
            .unwrap();
        let mut player = crate::Player::new(1, "fine".into());
        player.position = Vec3::new(0.5, 1.5, 0.5);
        for _ in 0..60 {
            crate::simulate_player(
                &world,
                &mut player,
                &crate::InputFrame::default(),
                crate::FIXED_DT,
            );
        }
        assert!(player.grounded);
        assert!((player.position.y - 1.125).abs() < 0.001);
        assert!(!crate::player_intersects_fragment(&player, &fragment));
        world
            .apply_root_edits(&[RootEdit {
                pos: [0, 1, 0],
                fragments: Vec::new(),
            }])
            .unwrap();
        crate::simulate_player(
            &world,
            &mut player,
            &crate::InputFrame::default(),
            crate::FIXED_DT,
        );
        assert!(!player.grounded);
        assert!(player.position.y < 1.125);
    }
    #[test]
    fn maximum_legacy_source_retains_tiny_clips_and_world_space_collision() {
        let height = Cell {
            heights: [8, 7, 7, 8],
            ..Cell::solid(0)
        };
        for cell in [
            height,
            Cell {
                shape: Some(height.as_shape()),
                ..Cell::solid(0)
            },
        ] {
            let fragment = CellFragment {
                bounds: FineBox {
                    min: [0, 8, 0],
                    max: [1, 9, 1],
                },
                origin: [-32768; 3],
                size: MAX_FRAGMENT_SOURCE_SIZE,
                cell,
            };
            assert!(fragment.is_valid());
            let mut world = World::empty();
            world
                .apply_root_edits(&[RootEdit {
                    pos: [0, 1, 0],
                    fragments: vec![fragment.clone()],
                }])
                .unwrap();
            assert_eq!(world.root_fragments([0, 1, 0]), vec![fragment]);
            assert!(world.intersects_box(Vec3::new(0.02, 1.02, 0.02), Vec3::new(0.04, 1.04, 0.04)));
            let hit = world
                .raycast(Vec3::new(0.0625, 3.0, 0.0625), -Vec3::Y, 4.0)
                .unwrap();
            assert!((hit.point.y - 1.125).abs() < 1.0e-5);
            let mut player = crate::Player::new(1, "legacy".into());
            player.position = Vec3::new(0.0625, 1.5, 0.0625);
            for _ in 0..60 {
                crate::simulate_player(
                    &world,
                    &mut player,
                    &crate::InputFrame::default(),
                    crate::FIXED_DT,
                );
            }
            assert!(player.grounded);
            assert!((player.position.y - 1.125).abs() < 0.001);
        }
    }

    #[test]
    fn oversized_or_overflowing_legacy_sources_are_rejected() {
        let mut source = CellFragment::from_cell([0, 0, 0], Cell::solid(0));
        source.size = MAX_FRAGMENT_SOURCE_SIZE * 2;
        assert!(!source.is_valid());
        source.size = 16;
        source.origin = [i32::MAX; 3];
        assert!(source.source_bounds().is_none());
        assert!(!source.is_valid());
    }
}
