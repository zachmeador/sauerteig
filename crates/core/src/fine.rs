//! Format 3's exact subdivision. A fragment retains its authored source cell
//! and clips that solid to an eighth-metre box; subdivision never resamples a
//! slope. The tree stores such a clip as exact pieces.
use crate::{Cell, CellEdit, CellPos, ChunkKey, Vec3, World};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

pub const FINE_UNITS: i32 = 8;
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

pub fn validate_root_edits(edits: &[RootEdit]) -> Result<(), String> {
    let mut roots = BTreeSet::new();
    for edit in edits {
        if !World::is_valid_pos(edit.pos) || !roots.insert(edit.pos) {
            return Err("Invalid or repeated detailed root.".into());
        }
        // Disjoint fragments inside one metre number at most 512, so this
        // check ends within the first 513.
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
                cell: Cell {
                    face_materials: fragment.cell.face_materials,
                    ..Cell::solid(fragment.cell.material)
                },
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
        && solids.iter().all(|f| f.cell == solids[0].cell)
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
                    f.cell.face_materials,
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
                    if last.cell == fragment.cell
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
        Self::from_compact_parts(crate::WorldBounds::default(), &[], cells, roots)
    }

    /// A metre's content in as few fragments as stay exact.
    pub fn root_fragments(&self, pos: CellPos) -> Vec<CellFragment> {
        self.tree.root_fragments(pos)
    }

    /// Metres that hold detail below a metre, with their content.
    pub fn detailed_roots(&self) -> Vec<RootEdit> {
        self.tree.detailed_roots()
    }

    /// Legacy convenience for bounded selections. Use try_roots_in to report limits.
    pub fn roots_in(&self, bounds: FineBox) -> Vec<RootEdit> {
        self.try_roots_in(bounds, crate::MAX_QUERY_ROOTS)
            .unwrap_or_default()
    }

    /// Replaces the geometry and surface materials of whole metres, keeping
    /// their cube material, and returns the mesh units to build again. Nothing
    /// changes unless every edit is valid. A write that changes nothing leaves
    /// the tree as it was.
    pub fn apply_root_edits(&mut self, edits: &[RootEdit]) -> Result<Vec<ChunkKey>, String> {
        let before = self.tree.clone();
        self.write_roots(edits)?;
        let changes = self.tree.changes_from(&before);
        if changes.changed.is_empty() {
            self.tree = before;
            return Ok(Vec::new());
        }
        Ok(self.tree.dirty_units(&changes))
    }

    /// Writes whole metres into the tree, after validating every edit.
    pub(crate) fn write_roots(&mut self, edits: &[RootEdit]) -> Result<(), String> {
        validate_root_edits(edits)?;
        if edits.iter().any(|e| !self.contains_pos(e.pos)) {
            return Err("Edit lies outside this world.".into());
        }
        let mut shapes = crate::octree::ShapeCache::default();
        let mut writes = Vec::with_capacity(edits.len());
        for edit in edits {
            let mut fragments = Vec::with_capacity(edit.fragments.len());
            for f in &edit.fragments {
                let (source, visible) =
                    shapes.source(crate::octree::source_of(f.origin, f.size, &f.cell))?;
                let faces =
                    crate::octree::lattice::fill_faces(visible, crate::octree::faces_of(&f.cell));
                fragments.push((f.bounds, source, faces));
            }
            writes.push((FineBox::root(edit.pos), fragments));
        }
        for (metre, fragments) in writes {
            self.tree.clear_geometry(metre);
            for &(bounds, source, faces) in &fragments {
                self.tree.insert_source(bounds, source, faces);
            }
            for &(_, source, faces) in &fragments {
                if source.size > 8 && !crate::octree::lattice::is_solid(source.shape) {
                    self.tree.remerge(source, faces, metre);
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{LeafGeometry, Shape};

    /// The volume of a world's solid geometry, from its leaves.
    fn solid_volume(world: &World) -> f32 {
        let signed = |triangles: Vec<[Vec3; 3]>| {
            triangles
                .iter()
                .map(|t| t[0].dot(t[1].cross(t[2])) / 6.0)
                .sum::<f32>()
        };
        let mut volume = 0.0;
        world.tree.for_each_leaf(|at, leaf| {
            let lattice = |shape: Shape, origin: [i32; 3], size: u32| -> Vec<[Vec3; 3]> {
                crate::octree::lattice::triangles(shape)
                    .as_slice()
                    .iter()
                    .map(|(t, _)| {
                        t.map(|p| {
                            Vec3::from_array(std::array::from_fn(|a| {
                                (origin[a] * 8 + p[a] * size as i32) as f32 / 64.0
                            }))
                        })
                    })
                    .collect()
            };
            volume += match leaf.geometry {
                LeafGeometry::Empty => 0.0,
                LeafGeometry::Full => {
                    let edge = at.size as f32 / 8.0;
                    edge * edge * edge
                }
                LeafGeometry::Shaped(shape) => signed(lattice(shape, at.min, at.size)),
                LeafGeometry::Piece(source) => signed(
                    crate::octree::mesh::piece_triangles(&source, at.fine_box())
                        .into_iter()
                        .map(|(t, _, _)| t.map(|p| p.as_vec3()))
                        .collect(),
                ),
            };
        });
        volume
    }

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
                // The hit's bounds are the leaf it struck, inside the fragment.
                assert!(fragment.bounds.contains(hit.bounds));
                assert_eq!(hit.face, (axis * 2 + 1) as u8);
                assert!((hit.point[axis] - fragment.bounds.world_max()[axis]).abs() < 1.0e-5);
            }
            assert!(world.intersects_box(center - Vec3::splat(0.01), center + Vec3::splat(0.01)));
            assert!(!world.intersects_box(Vec3::splat(0.001), Vec3::splat(0.1)));
            let volume = solid_volume(&world);
            assert!(
                (volume - {
                    let edge = size as f32 / 8.0;
                    edge * edge * edge
                })
                .abs()
                    < 1.0e-5,
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
            let after = solid_volume(&fine);
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
        let snapshot = world.snapshot();
        let restored = World::from_compact_parts(
            snapshot.bounds(),
            snapshot.regions(),
            &snapshot.cells(),
            &snapshot.root_overrides(),
        )
        .unwrap();
        assert_eq!(restored.digest(), world.digest());
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
            // Deep inside its source's solid, the clip is a full eighth.
            let kept = world.root_fragments([0, 1, 0]);
            assert_eq!(kept.len(), 1);
            assert_eq!(kept[0].bounds, fragment.bounds);
            assert!(kept[0].cell.is_full());
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
