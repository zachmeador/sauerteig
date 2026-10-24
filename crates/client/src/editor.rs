//! Selection and sparse edit planning. Integer coordinates are eighth-metres;
//! camera rays and displayed face coordinates remain world metres.
use crate::regions::{RegionBox, RegionSet};
use sauerteig_core::{
    canonical_root_fragments, validate_root_edits, Cell, CellEdit, CellFragment, CellPos, FineBox,
    MaterialVolume, ObjectPlacement, RayHit, RootEdit, Shape, Vec3, VolumeEdit, VolumeMaterial,
    World, WorldObject, MATERIAL_COLORS, MAX_MATERIAL_VOLUMES, SHAPE_FACES,
};
use serde::{Deserialize, Deserializer, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
};

const MAX_EDIT_FRAGMENTS: usize = 65_536;
const MAX_EDIT_ROOTS: usize = 16_384;
const MAX_CLIPBOARD_OBJECTS: usize = 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EditSelection {
    pub min: CellPos,
    pub max: CellPos,
    pub axis: usize,
    pub sign: i32,
    pub grid: i32,
}
#[derive(Clone, Debug)]
pub struct SelectionDrag {
    anchor: EditSelection,
    plane: f32,
}
#[derive(Clone, Debug)]
pub struct EditPlan {
    /// Kept for the legacy protocol adapter; fine plans use roots exclusively.
    pub edits: Vec<CellEdit>,
    pub roots: Vec<RootEdit>,
    pub volumes: Vec<VolumeEdit>,
    pub selection_after: EditSelection,
}

/// Empty selected space is represented by a box union, not allocated voxels.
/// Sources remain intact under clipping, so copying part of a slope is exact.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Clipboard {
    dims: CellPos,
    mask: RegionSet,
    fragments: Vec<CellFragment>,
    /// Object pivots relative to the clipboard origin, in world metres.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    objects: Vec<ObjectPlacement>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    volumes: Vec<MaterialVolume>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditError {
    InvalidSelection,
    TooLarge,
    OutsideWorld,
    NoPlaneIntersection,
    InvalidMaterial,
    InvalidShape,
    InvalidCorner,
    UnsupportedTransform,
    ClippedShape,
    NoChanges,
    InvalidObject,
    InvalidVolume,
}
impl fmt::Display for EditError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidSelection => "Select a face using a grid from 0.125 to 8 metres.",
            Self::TooLarge => "This edit contains too much geometry. Use a smaller occupied region.",
            Self::OutsideWorld => "This edit extends outside the world. Move or resize the selection.",
            Self::NoPlaneIntersection => "Aim toward the selected face to extend the selection.",
            Self::InvalidMaterial => "Choose a valid material.",
            Self::InvalidShape => "That move would fold or cross the cell's faces. Use a smaller change.",
            Self::InvalidCorner => "Select one of the four face corners or edges.",
            Self::UnsupportedTransform => "Mirror supports the X or Z axis.",
            Self::ClippedShape => "This clipped slope retains its original shape. Shape the complete source cell, or add a new fine cube here.",
            Self::NoChanges => "The selected space already has that shape or material.",
            Self::InvalidObject => "An object has an invalid position or rotation in this blueprint.",
            Self::InvalidVolume => "Material volumes must have valid, non-overlapping bounds inside the selection.",
        })
    }
}
impl std::error::Error for EditError {}
fn region(bounds: FineBox) -> RegionBox {
    RegionBox {
        min: bounds.min,
        max: bounds.max,
    }
}
fn fine(bounds: RegionBox) -> FineBox {
    FineBox {
        min: bounds.min,
        max: bounds.max,
    }
}
fn one_region(bounds: FineBox) -> Result<RegionSet, EditError> {
    RegionSet::from_boxes([region(bounds)]).map_err(|_| EditError::InvalidSelection)
}
fn check_grid(grid: i32) -> Result<(), EditError> {
    if [1, 2, 4, 8, 16, 32, 64].contains(&grid) {
        Ok(())
    } else {
        Err(EditError::InvalidSelection)
    }
}
fn material(value: u8) -> Result<(), EditError> {
    if (value as usize) < MATERIAL_COLORS.len() {
        Ok(())
    } else {
        Err(EditError::InvalidMaterial)
    }
}
fn checked_dims(min: CellPos, max: CellPos) -> Result<CellPos, EditError> {
    let mut dims = [0; 3];
    for a in 0..3 {
        dims[a] = max[a]
            .checked_sub(min[a])
            .filter(|v| *v > 0)
            .ok_or(EditError::InvalidSelection)?;
    }
    Ok(dims)
}
fn translate(p: CellPos, offset: CellPos) -> Result<CellPos, EditError> {
    let mut out = p;
    for a in 0..3 {
        out[a] = p[a].checked_add(offset[a]).ok_or(EditError::OutsideWorld)?;
    }
    Ok(out)
}
fn translated_fragment(f: &CellFragment, offset: CellPos) -> Result<CellFragment, EditError> {
    Ok(CellFragment {
        bounds: FineBox {
            min: translate(f.bounds.min, offset)?,
            max: translate(f.bounds.max, offset)?,
        },
        origin: translate(f.origin, offset)?,
        size: f.size,
        cell: f.cell,
    })
}
fn capture(world: &World, mask: &RegionSet) -> Result<Vec<CellFragment>, EditError> {
    let mut fragments = Vec::new();
    let mut touched = BTreeSet::new();
    for &bounds in mask.boxes() {
        for root in world
            .try_roots_in(fine(bounds), MAX_EDIT_ROOTS)
            .map_err(|_| EditError::TooLarge)?
        {
            touched.insert(root.pos);
            if touched.len() > MAX_EDIT_ROOTS {
                return Err(EditError::TooLarge);
            }
            for fragment in root.fragments {
                if let Some(fragment) = fragment.clipped(fine(bounds)) {
                    if fragments.len() >= MAX_EDIT_FRAGMENTS {
                        return Err(EditError::TooLarge);
                    }
                    fragments.push(fragment);
                }
            }
        }
    }
    Ok(fragments)
}

impl EditSelection {
    pub fn from_hit(hit: &RayHit, grid: i32) -> Result<Self, EditError> {
        check_grid(grid)?;
        if hit.face >= 6
            || !hit.point.is_finite()
            || !hit.normal.is_finite()
            || hit.bounds.is_empty()
        {
            return Err(EditError::InvalidSelection);
        }
        let axis = hit.face as usize / 2;
        let sign = if hit.face % 2 == 1 { 1 } else { -1 };
        // Apply the inward bias in f64: at distant map coordinates an f32
        // subtraction can round away and select empty space across the face.
        let mut min = std::array::from_fn(|a| {
            let point = f64::from(hit.point[a]) * 8.0 - f64::from(hit.normal[a]) * 0.0001;
            (point.floor() as i32).div_euclid(grid) * grid
        });
        let mut max = min.map(|v| v + grid);
        // Fine selection addresses the cube containing the actual surface hit,
        // including low parts of a larger sloped source. Coarse selection keeps
        // the source face as its extrusion anchor (e.g. the one-metre floor).
        if grid >= hit.bounds.max[axis] - hit.bounds.min[axis] {
            if sign > 0 {
                max[axis] = hit.bounds.max[axis];
                min[axis] = max[axis] - grid;
            } else {
                min[axis] = hit.bounds.min[axis];
                max[axis] = min[axis] + grid;
            }
        }
        let result = Self {
            min,
            max,
            axis,
            sign,
            grid,
        };
        result.validate()?;
        Ok(result)
    }
    fn validate(&self) -> Result<(), EditError> {
        check_grid(self.grid)?;
        if self.axis >= 3 || ![-1, 1].contains(&self.sign) {
            return Err(EditError::InvalidSelection);
        }
        checked_dims(self.min, self.max)?;
        Ok(())
    }
    fn bounds(&self) -> FineBox {
        FineBox {
            min: self.min,
            max: self.max,
        }
    }
    pub fn dims(&self) -> CellPos {
        std::array::from_fn(|a| self.max[a] - self.min[a])
    }
    pub fn face_plane(&self) -> f32 {
        if self.sign > 0 {
            self.max[self.axis] as f32 / 8.0
        } else {
            self.min[self.axis] as f32 / 8.0
        }
    }
    pub fn begin_drag(&self, anchor_point: Vec3) -> SelectionDrag {
        SelectionDrag {
            anchor: self.clone(),
            plane: anchor_point[self.axis],
        }
    }
    pub fn extend_to_hit(&self, hit: &RayHit) -> Result<Self, EditError> {
        self.validate()?;
        let other = Self::from_hit(hit, self.grid)?;
        let result = Self {
            min: std::array::from_fn(|a| self.min[a].min(other.min[a])),
            max: std::array::from_fn(|a| self.max[a].max(other.max[a])),
            axis: other.axis,
            sign: other.sign,
            grid: self.grid,
        };
        result.validate()?;
        Ok(result)
    }
    pub fn resnap(&self, grid: i32) -> Result<Self, EditError> {
        self.validate()?;
        check_grid(grid)?;
        let mut selected = self.clone();
        selected.grid = grid;
        for a in 0..3 {
            if a != self.axis {
                selected.min[a] = self.min[a].div_euclid(grid) * grid;
                selected.max[a] = (self.max[a] - 1)
                    .div_euclid(grid)
                    .checked_mul(grid)
                    .and_then(|v| v.checked_add(grid))
                    .ok_or(EditError::OutsideWorld)?;
            }
        }
        let depth = if self.dims()[self.axis] == self.grid {
            grid
        } else {
            ((self.dims()[self.axis] - 1) / grid + 1)
                .checked_mul(grid)
                .ok_or(EditError::OutsideWorld)?
        };
        if self.sign > 0 {
            selected.min[self.axis] = self.max[self.axis]
                .checked_sub(depth)
                .ok_or(EditError::OutsideWorld)?;
        } else {
            selected.max[self.axis] = self.min[self.axis]
                .checked_add(depth)
                .ok_or(EditError::OutsideWorld)?;
        }
        selected.validate()?;
        Ok(selected)
    }
    fn slab(&self, outside: bool) -> Result<FineBox, EditError> {
        self.validate()?;
        let mut bounds = self.bounds();
        let plane = if self.sign > 0 {
            self.max[self.axis]
        } else {
            self.min[self.axis]
        };
        if (self.sign > 0) == outside {
            bounds.min[self.axis] = plane;
            bounds.max[self.axis] = plane
                .checked_add(self.grid)
                .ok_or(EditError::OutsideWorld)?;
        } else {
            bounds.min[self.axis] = plane
                .checked_sub(self.grid)
                .ok_or(EditError::OutsideWorld)?;
            bounds.max[self.axis] = plane;
        }
        Ok(bounds)
    }
    fn with_plane(&self, plane: i32) -> Result<Self, EditError> {
        let mut selected = self.clone();
        let depth = self.dims()[self.axis];
        if self.sign > 0 {
            selected.max[self.axis] = plane;
            selected.min[self.axis] = plane.checked_sub(depth).ok_or(EditError::OutsideWorld)?;
        } else {
            selected.min[self.axis] = plane;
            selected.max[self.axis] = plane.checked_add(depth).ok_or(EditError::OutsideWorld)?;
        }
        Ok(selected)
    }
    pub fn plan_extrude(&self, world: &World, value: u8) -> Result<EditPlan, EditError> {
        material(value)?;
        let bounds = self.slab(true)?;
        if !world.bounds().fine_box().contains(bounds) {
            return Err(EditError::OutsideWorld);
        }
        let fragments = filled(bounds, self.grid.min(8) as u8, Cell::solid(value))?;
        let plane = if self.sign > 0 {
            bounds.max[self.axis]
        } else {
            bounds.min[self.axis]
        };
        replace(
            world,
            &one_region(bounds)?,
            fragments,
            self.with_plane(plane)?,
            true,
        )
    }
    pub fn plan_volume_regions(
        &self,
        world: &World,
        mask: &RegionSet,
        material: Option<VolumeMaterial>,
    ) -> Result<EditPlan, EditError> {
        self.validate()?;
        if mask.boxes().is_empty() {
            return Err(EditError::InvalidSelection);
        }
        let volumes = mask
            .boxes()
            .iter()
            .map(|bounds| VolumeEdit {
                bounds: fine(*bounds),
                material,
            })
            .collect::<Vec<_>>();
        let result = world
            .prepare_volume_edits(&volumes)
            .map_err(|_| EditError::InvalidVolume)?;
        if result == world.volumes() {
            return Err(EditError::NoChanges);
        }
        Ok(EditPlan {
            edits: Vec::new(),
            roots: Vec::new(),
            volumes,
            selection_after: self.clone(),
        })
    }
    pub fn plan_carve(&self, world: &World) -> Result<EditPlan, EditError> {
        let bounds = self
            .slab(false)?
            .intersection(world.bounds().fine_box())
            .ok_or(EditError::NoChanges)?;
        let plane = if self.sign > 0 {
            bounds.min[self.axis]
        } else {
            bounds.max[self.axis]
        };
        replace(
            world,
            &one_region(bounds)?,
            Vec::new(),
            self.with_plane(plane)?,
            false,
        )
    }
    #[cfg(test)]
    pub fn plan_paint(&self, world: &World, value: u8) -> Result<EditPlan, EditError> {
        self.plan_paint_regions(world, &one_region(self.bounds())?, value)
    }
    pub fn plan_paint_regions(
        &self,
        world: &World,
        mask: &RegionSet,
        value: u8,
    ) -> Result<EditPlan, EditError> {
        self.validate()?;
        material(value)?;
        let mut fragments = capture(world, mask)?;
        let face = self.face_index() as u8;
        if fragments
            .iter()
            .all(|f| f.cell.material_for_face(face) == value)
        {
            return Err(EditError::NoChanges);
        }
        for fragment in &mut fragments {
            fragment.cell.set_face_material(face, value);
        }
        replace(world, mask, fragments, self.clone(), false)
    }
    pub fn plan_deform(
        &self,
        world: &World,
        corner: Option<usize>,
        delta: i8,
    ) -> Result<EditPlan, EditError> {
        if corner.is_some_and(|c| c >= 4) {
            return Err(EditError::InvalidCorner);
        }
        self.plan_deform_mask(world, corner.map_or(15, |c| 1 << c), delta)
    }
    pub fn plan_deform_edge(
        &self,
        world: &World,
        edge: usize,
        delta: i8,
    ) -> Result<EditPlan, EditError> {
        if edge >= 4 {
            return Err(EditError::InvalidCorner);
        }
        self.plan_deform_mask(world, (1 << edge) | (1 << ((edge + 1) % 4)), delta)
    }
    fn plan_deform_mask(&self, world: &World, mask: u8, delta: i8) -> Result<EditPlan, EditError> {
        self.validate()?;
        let mut bounds = self.bounds();
        if self.sign > 0 {
            bounds.min[self.axis] = bounds.max[self.axis] - self.grid.min(8);
        } else {
            bounds.max[self.axis] = bounds.min[self.axis] + self.grid.min(8);
        }
        let bounds = bounds
            .intersection(world.bounds().fine_box())
            .ok_or(EditError::NoChanges)?;
        let selected = one_region(bounds)?;
        let mut fragments = Vec::new();
        let mut changed = false;
        for original in capture(world, &selected)? {
            let sources = if original.cell.is_full() {
                // A solid cube can be refined without resampling any surface.
                let mut size = self.grid.min(original.size as i32).min(8) as u8;
                let dims = checked_dims(original.bounds.min, original.bounds.max)?;
                while dims.iter().any(|n| n % size as i32 != 0) {
                    size /= 2;
                }
                filled(original.bounds, size, original.cell)?
            } else if original.source_bounds() == Some(original.bounds)
                && original.size as i32 <= self.grid
            {
                vec![original]
            } else {
                return Err(EditError::ClippedShape);
            };
            for mut fragment in sources {
                if fragment.source_bounds() != Some(fragment.bounds) {
                    return Err(EditError::ClippedShape);
                }
                let before = fragment.cell.as_shape();
                let mut shape = before;
                for (corner, &vertex) in SHAPE_FACES[self.face_index()].iter().enumerate() {
                    if mask & (1 << corner) != 0 {
                        shape.vertices[vertex][self.axis] =
                            (shape.vertices[vertex][self.axis] as i16
                                + delta as i16 * self.sign as i16)
                                .clamp(0, 8) as u8;
                    }
                }
                changed |= shape != before;
                if (0..3).any(|a| shape.vertices.iter().all(|v| v[a] == shape.vertices[0][a])) {
                    continue;
                }
                if fragment.cell.shape.is_none() && self.face_index() == 3 {
                    fragment.cell.heights = std::array::from_fn(|i| shape.vertices[i + 4][1]);
                } else {
                    fragment.cell.shape = Some(shape);
                }
                if !fragment.cell.is_valid() {
                    return Err(EditError::InvalidShape);
                }
                fragments.push(fragment);
            }
        }
        if !changed {
            return Err(EditError::NoChanges);
        }
        replace(world, &selected, fragments, self.clone(), false)
    }
    pub fn face_index(&self) -> usize {
        self.axis * 2 + usize::from(self.sign > 0)
    }
    pub fn face_corner(&self, corner: usize) -> Option<Vec3> {
        if self.axis >= 3 || corner >= 4 {
            return None;
        }
        let vertex = Shape::solid().vertices[SHAPE_FACES[self.face_index()][corner]];
        Some(Vec3::from_array(std::array::from_fn(|a| {
            if vertex[a] == 0 {
                self.min[a] as f32 / 8.0
            } else {
                self.max[a] as f32 / 8.0
            }
        })))
    }
    pub fn cursor_corner(&self, point: Vec3) -> Option<usize> {
        if !point.is_finite() || self.axis >= 3 {
            return None;
        }
        (0..4).min_by(|&a, &b| {
            self.face_corner(a)
                .unwrap()
                .distance_squared(point)
                .total_cmp(&self.face_corner(b).unwrap().distance_squared(point))
        })
    }
    pub fn cursor_edge(&self, point: Vec3) -> Option<usize> {
        if !point.is_finite() || self.axis >= 3 {
            return None;
        }
        let distance = |edge| {
            let a = self.face_corner(edge).unwrap();
            let b = self.face_corner((edge + 1) % 4).unwrap();
            let line = b - a;
            let t = ((point - a).dot(line) / line.length_squared()).clamp(0.0, 1.0);
            (point - a - line * t).length_squared()
        };
        (0..4).min_by(|&a, &b| distance(a).total_cmp(&distance(b)))
    }
}
impl SelectionDrag {
    pub fn update(&self, origin: Vec3, direction: Vec3) -> Result<EditSelection, EditError> {
        if !origin.is_finite() || !direction.is_finite() || direction[self.anchor.axis].abs() < 1e-6
        {
            return Err(EditError::NoPlaneIntersection);
        }
        let t = (self.plane - origin[self.anchor.axis]) / direction[self.anchor.axis];
        if !t.is_finite() || t < 0.0 {
            return Err(EditError::NoPlaneIntersection);
        }
        self.update_point(origin + direction * t)
    }
    fn update_point(&self, point: Vec3) -> Result<EditSelection, EditError> {
        if !point.is_finite() || point.abs().max_element() > 1_000_000.0 {
            return Err(EditError::OutsideWorld);
        }
        let mut selected = self.anchor.clone();
        for a in 0..3 {
            if a != selected.axis {
                let lower =
                    ((point[a] * 8.0).floor() as i32).div_euclid(selected.grid) * selected.grid;
                selected.min[a] = selected.min[a].min(lower);
                selected.max[a] = selected.max[a].max(lower + selected.grid);
            }
        }
        selected.validate()?;
        Ok(selected)
    }
}

fn filled(bounds: FineBox, size: u8, cell: Cell) -> Result<Vec<CellFragment>, EditError> {
    if ![1, 2, 4, 8].contains(&size) || bounds.is_empty() {
        return Err(EditError::InvalidSelection);
    }
    let lengths = checked_dims(bounds.min, bounds.max)?;
    let count: u128 = lengths
        .into_iter()
        .map(|n| (n as u128).div_ceil(size as u128))
        .product();
    if count > MAX_EDIT_FRAGMENTS as u128 {
        return Err(EditError::TooLarge);
    }
    let mut out = Vec::with_capacity(count as usize);
    for x in (bounds.min[0]..bounds.max[0]).step_by(size as usize) {
        for y in (bounds.min[1]..bounds.max[1]).step_by(size as usize) {
            for z in (bounds.min[2]..bounds.max[2]).step_by(size as usize) {
                let origin = [x, y, z];
                out.push(CellFragment {
                    bounds: FineBox {
                        min: origin,
                        max: std::array::from_fn(|a| (origin[a] + size as i32).min(bounds.max[a])),
                    },
                    origin,
                    size: u32::from(size),
                    cell,
                });
            }
        }
    }
    Ok(out)
}
fn root_positions(bounds: FineBox) -> Result<Vec<CellPos>, EditError> {
    if bounds.is_empty() {
        return Err(EditError::InvalidSelection);
    }
    let min = bounds.min.map(|v| v.div_euclid(8));
    let max = bounds.max.map(|v| (v - 1).div_euclid(8));
    let count: u128 = (0..3)
        .map(|axis| (i64::from(max[axis]) - i64::from(min[axis]) + 1) as u128)
        .product();
    if count > MAX_EDIT_ROOTS as u128 {
        return Err(EditError::TooLarge);
    }
    let mut out = Vec::with_capacity(count as usize);
    for x in min[0]..=max[0] {
        for y in min[1]..=max[1] {
            for z in min[2]..=max[2] {
                out.push([x, y, z]);
            }
        }
    }
    Ok(out)
}
fn normalize(fragments: &mut Vec<CellFragment>) {
    // At most 512 pieces per root. Coalesce only identical source solids; this
    // preserves exact shapes and avoids bookkeeping-only changes on a repaint.
    loop {
        let mut pair = None;
        'outer: for i in 0..fragments.len() {
            for j in i + 1..fragments.len() {
                let a = &fragments[i];
                let b = &fragments[j];
                if a.origin != b.origin || a.size != b.size || a.cell != b.cell {
                    continue;
                }
                for axis in 0..3 {
                    if (0..3).filter(|v| *v != axis).all(|v| {
                        a.bounds.min[v] == b.bounds.min[v] && a.bounds.max[v] == b.bounds.max[v]
                    }) && (a.bounds.max[axis] == b.bounds.min[axis]
                        || b.bounds.max[axis] == a.bounds.min[axis])
                    {
                        pair = Some((i, j, axis));
                        break 'outer;
                    }
                }
            }
        }
        let Some((i, j, axis)) = pair else {
            break;
        };
        let other = fragments.remove(j);
        fragments[i].bounds.min[axis] = fragments[i].bounds.min[axis].min(other.bounds.min[axis]);
        fragments[i].bounds.max[axis] = fragments[i].bounds.max[axis].max(other.bounds.max[axis]);
    }
    fragments.sort_by_key(|f| (f.bounds, f.origin, f.size));
}
fn replace(
    world: &World,
    mask: &RegionSet,
    added: Vec<CellFragment>,
    selection_after: EditSelection,
    strict: bool,
) -> Result<EditPlan, EditError> {
    if strict
        && mask
            .boxes()
            .iter()
            .any(|b| !world.bounds().fine_box().contains(fine(*b)))
    {
        return Err(EditError::OutsideWorld);
    }
    if added.len() > MAX_EDIT_FRAGMENTS {
        return Err(EditError::TooLarge);
    }
    let mut roots: BTreeMap<CellPos, Vec<CellFragment>> = BTreeMap::new();
    for &bounds in mask.boxes() {
        for root in world
            .try_roots_in(fine(bounds), MAX_EDIT_ROOTS)
            .map_err(|_| EditError::TooLarge)?
        {
            roots.entry(root.pos).or_insert(root.fragments);
            if roots.len() > MAX_EDIT_ROOTS {
                return Err(EditError::TooLarge);
            }
        }
    }
    for f in &added {
        if !f.is_valid()
            || !mask.covers(region(f.bounds))
            || !world.bounds().fine_box().contains(f.bounds)
        {
            return Err(EditError::InvalidShape);
        }
        for pos in root_positions(f.bounds)? {
            roots
                .entry(pos)
                .or_insert_with(|| world.root_fragments(pos));
            if roots.len() > MAX_EDIT_ROOTS {
                return Err(EditError::TooLarge);
            }
        }
    }
    if roots.len() > MAX_EDIT_ROOTS {
        return Err(EditError::TooLarge);
    }
    for fragments in roots.values_mut() {
        let mut kept = Vec::new();
        for original in fragments.drain(..) {
            let mut pieces = vec![original.bounds];
            for &cut in mask.boxes() {
                pieces = pieces
                    .into_iter()
                    .flat_map(|b| b.subtract(fine(cut)))
                    .collect();
            }
            kept.extend(pieces.into_iter().map(|bounds| CellFragment {
                bounds,
                ..original.clone()
            }));
        }
        *fragments = kept;
    }
    for f in added {
        for pos in root_positions(f.bounds)? {
            roots
                .get_mut(&pos)
                .unwrap()
                .push(f.clipped(FineBox::root(pos)).unwrap());
        }
    }
    let mut edits = Vec::new();
    let mut count = 0;
    for (pos, mut fragments) in roots {
        fragments = canonical_root_fragments(pos, &fragments);
        let mut old = canonical_root_fragments(pos, &world.root_fragments(pos));
        normalize(&mut fragments);
        normalize(&mut old);
        if old == fragments {
            continue;
        }
        count += fragments.len();
        if count > MAX_EDIT_FRAGMENTS {
            return Err(EditError::TooLarge);
        }
        edits.push(RootEdit { pos, fragments });
    }
    if edits.is_empty() {
        return Err(EditError::NoChanges);
    }
    validate_root_edits(&edits).map_err(|_| EditError::InvalidShape)?;
    Ok(EditPlan {
        edits: Vec::new(),
        roots: edits,
        volumes: Vec::new(),
        selection_after,
    })
}

impl Clipboard {
    #[cfg(test)]
    pub fn copy(world: &World, selection: &EditSelection) -> Result<Self, EditError> {
        selection.validate()?;
        Self::copy_regions(world, &one_region(selection.bounds())?)
    }
    #[cfg(test)]
    pub fn copy_regions(world: &World, selection: &RegionSet) -> Result<Self, EditError> {
        Self::copy_regions_with_objects(world, selection, &[])
    }
    /// Capture object pivots in the exact selected union. Subtracted volumes do
    /// not contribute objects, even when they lie inside the union's bounds.
    pub fn copy_regions_with_objects(
        world: &World,
        selection: &RegionSet,
        objects: &[WorldObject],
    ) -> Result<Self, EditError> {
        let bounds = selection.bounds().ok_or(EditError::InvalidSelection)?;
        let dims = checked_dims(bounds.min, bounds.max)?;
        let mut offset = [0; 3];
        for (value, min) in offset.iter_mut().zip(bounds.min) {
            *value = min.checked_neg().ok_or(EditError::OutsideWorld)?;
        }
        let mask = selection
            .translated(offset)
            .map_err(|_| EditError::OutsideWorld)?;
        let fragments = capture(world, selection)?
            .iter()
            .map(|f| translated_fragment(f, offset))
            .collect::<Result<_, _>>()?;
        let origin = Vec3::from_array(bounds.min.map(|v| v as f32 / 8.0));
        let mut volumes = Vec::new();
        for selected in selection.boxes() {
            for volume in world.volume_regions(fine(*selected)) {
                if volumes.len() >= MAX_MATERIAL_VOLUMES {
                    return Err(EditError::TooLarge);
                }
                volumes.push(MaterialVolume {
                    bounds: FineBox {
                        min: translate(volume.bounds.min, offset)?,
                        max: translate(volume.bounds.max, offset)?,
                    },
                    material: volume.material,
                });
            }
        }
        let objects = objects
            .iter()
            .filter(|object| contains_object_pivot(selection, object.position, false))
            .map(|object| {
                if !object.rotation.is_finite() || object.rotation.abs() > 1_000_000.0 {
                    return Err(EditError::InvalidObject);
                }
                Ok(ObjectPlacement {
                    kind: object.kind,
                    position: object.position - origin,
                    rotation: normalized_yaw(object.rotation),
                })
            })
            .collect::<Result<_, _>>()?;
        let result = Self {
            dims,
            mask,
            fragments,
            objects,
            volumes,
        };
        result.validate()?;
        Ok(result)
    }
    pub fn dims(&self) -> CellPos {
        self.dims
    }
    pub fn mask(&self) -> &RegionSet {
        &self.mask
    }
    pub fn objects(&self) -> &[ObjectPlacement] {
        &self.objects
    }
    pub fn volumes(&self) -> &[MaterialVolume] {
        &self.volumes
    }
    /// Resolve object preview or transaction placements from an eighth-metre
    /// destination. IDs are allocated by the server when the paste is accepted.
    pub fn placed_objects(&self, origin: CellPos) -> Result<Vec<ObjectPlacement>, EditError> {
        self.validate()?;
        let offset = Vec3::from_array(origin.map(|v| v as f32 / 8.0));
        self.objects
            .iter()
            .map(|object| {
                let position = object.position + offset;
                if !position.is_finite()
                    || !World::is_valid_pos(position.floor().as_ivec3().to_array())
                {
                    return Err(EditError::OutsideWorld);
                }
                Ok(ObjectPlacement {
                    position,
                    ..object.clone()
                })
            })
            .collect()
    }
    fn validate(&self) -> Result<(), EditError> {
        if self.mask.bounds()
            != Some(RegionBox {
                min: [0; 3],
                max: self.dims,
            })
            || self.dims.iter().any(|v| *v <= 0)
        {
            return Err(EditError::InvalidSelection);
        }
        if self.fragments.len() > MAX_EDIT_FRAGMENTS || self.objects.len() > MAX_CLIPBOARD_OBJECTS {
            return Err(EditError::TooLarge);
        }
        if self.volumes.len() > MAX_MATERIAL_VOLUMES {
            return Err(EditError::TooLarge);
        }
        sauerteig_core::validate_material_volumes(
            &self.volumes,
            FineBox {
                min: [0; 3],
                max: self.dims,
            },
        )
        .map_err(|_| EditError::InvalidVolume)?;
        if self
            .volumes
            .iter()
            .any(|volume| !self.mask.covers(region(volume.bounds)))
        {
            return Err(EditError::InvalidVolume);
        }
        for object in &self.objects {
            // Reflection carries a pivot on a lower face onto the upper face.
            // Closed containment preserves that placement without rounding it
            // inward; initial capture still uses half-open selection semantics.
            if !object.position.is_finite()
                || !object.rotation.is_finite()
                || object.rotation.abs() > 1_000_000.0
                || !contains_object_pivot(&self.mask, object.position, true)
            {
                return Err(EditError::InvalidObject);
            }
        }
        let mut buckets: BTreeMap<CellPos, Vec<usize>> = BTreeMap::new();
        for (i, f) in self.fragments.iter().enumerate() {
            if !f.is_valid() || !self.mask.covers(region(f.bounds)) {
                return Err(EditError::InvalidShape);
            }
            for pos in root_positions(f.bounds)? {
                let previous = buckets.entry(pos).or_default();
                if previous
                    .iter()
                    .any(|&j| self.fragments[j].bounds.intersection(f.bounds).is_some())
                {
                    return Err(EditError::InvalidShape);
                }
                previous.push(i);
                if buckets.len() > MAX_EDIT_ROOTS {
                    return Err(EditError::TooLarge);
                }
            }
        }
        Ok(())
    }
    pub fn rotate_y(&mut self) -> Result<(), EditError> {
        self.validate()?;
        let width = self.dims[0];
        let mut result = self.clone();
        result.dims = [self.dims[2], self.dims[1], width];
        result.mask = self
            .mask
            .rotated_y(width)
            .map_err(|_| EditError::OutsideWorld)?;
        for f in &mut result.fragments {
            let old = f.clone();
            f.bounds = FineBox {
                min: [
                    old.bounds.min[2],
                    old.bounds.min[1],
                    width - old.bounds.max[0],
                ],
                max: [
                    old.bounds.max[2],
                    old.bounds.max[1],
                    width - old.bounds.min[0],
                ],
            };
            f.origin = [
                old.origin[2],
                old.origin[1],
                width
                    .checked_sub(old.origin[0])
                    .and_then(|v| v.checked_sub(old.size as i32))
                    .ok_or(EditError::OutsideWorld)?,
            ];
            let h = f.cell.heights;
            f.cell.heights = [h[1], h[2], h[3], h[0]];
            // (x,y,z) -> (z,y,-x) carries each logical face to its new axis.
            f.cell.face_materials = f
                .cell
                .face_materials
                .map(|m| [m[4], m[5], m[2], m[3], m[1], m[0]]);
            f.cell.diagonal = !f.cell.diagonal;
            f.cell.shape = f
                .cell
                .shape
                .map(|s| transform_shape(s, |[x, y, z]| [z, y, 8 - x]));
        }
        for object in &mut result.objects {
            let old = object.position;
            object.position = Vec3::new(old.z, old.y, width as f32 / 8.0 - old.x);
            object.rotation = normalized_yaw(object.rotation + std::f32::consts::FRAC_PI_2);
        }
        for volume in &mut result.volumes {
            let old = volume.bounds;
            volume.bounds = FineBox {
                min: [old.min[2], old.min[1], width - old.max[0]],
                max: [old.max[2], old.max[1], width - old.min[0]],
            };
        }
        result.validate()?;
        *self = result;
        Ok(())
    }
    pub fn mirror(&mut self, axis: usize) -> Result<(), EditError> {
        if axis != 0 && axis != 2 {
            return Err(EditError::UnsupportedTransform);
        }
        self.validate()?;
        let mut result = self.clone();
        let length = self.dims[axis];
        result.mask = self
            .mask
            .mirrored(axis, length)
            .map_err(|_| EditError::OutsideWorld)?;
        for f in &mut result.fragments {
            let lower = f.bounds.min[axis];
            f.bounds.min[axis] = length - f.bounds.max[axis];
            f.bounds.max[axis] = length - lower;
            f.origin[axis] = length
                .checked_sub(f.origin[axis])
                .and_then(|v| v.checked_sub(f.size as i32))
                .ok_or(EditError::OutsideWorld)?;
            let h = f.cell.heights;
            f.cell.heights = if axis == 0 {
                [h[1], h[0], h[3], h[2]]
            } else {
                [h[3], h[2], h[1], h[0]]
            };
            f.cell.diagonal = !f.cell.diagonal;
            if let Some(materials) = &mut f.cell.face_materials {
                materials.swap(axis * 2, axis * 2 + 1);
            }
            f.cell.shape = f.cell.shape.map(|s| {
                transform_shape(s, |mut v| {
                    v[axis] = 8 - v[axis];
                    v
                })
            });
        }
        for object in &mut result.objects {
            object.position[axis] = length as f32 / 8.0 - object.position[axis];
            object.rotation = normalized_yaw(if axis == 0 {
                -object.rotation
            } else {
                std::f32::consts::PI - object.rotation
            });
        }
        for volume in &mut result.volumes {
            let lower = volume.bounds.min[axis];
            volume.bounds.min[axis] = length - volume.bounds.max[axis];
            volume.bounds.max[axis] = length - lower;
        }
        result.validate()?;
        *self = result;
        Ok(())
    }
    pub fn plan_paste(
        &self,
        world: &World,
        selection: &EditSelection,
    ) -> Result<EditPlan, EditError> {
        selection.validate()?;
        self.validate()?;
        let mask = self
            .mask
            .translated(selection.min)
            .map_err(|_| EditError::OutsideWorld)?;
        // Selected empty space is also checked before any destination mutation.
        if mask
            .boxes()
            .iter()
            .any(|b| !world.bounds().fine_box().contains(fine(*b)))
        {
            return Err(EditError::OutsideWorld);
        }
        let fragments = self
            .fragments
            .iter()
            .map(|f| translated_fragment(f, selection.min))
            .collect::<Result<_, _>>()?;
        let selection_after = EditSelection {
            min: selection.min,
            max: translate(selection.min, self.dims)?,
            axis: selection.axis,
            sign: selection.sign,
            grid: selection.grid,
        };
        if self
            .placed_objects(selection.min)?
            .iter()
            .any(|object| !world.contains_pos(object.position.floor().as_ivec3().to_array()))
        {
            return Err(EditError::OutsideWorld);
        }
        let mut volumes = mask
            .boxes()
            .iter()
            .map(|bounds| VolumeEdit {
                bounds: fine(*bounds),
                material: None,
            })
            .collect::<Vec<_>>();
        for volume in &self.volumes {
            volumes.push(VolumeEdit {
                bounds: FineBox {
                    min: translate(volume.bounds.min, selection.min)?,
                    max: translate(volume.bounds.max, selection.min)?,
                },
                material: Some(volume.material),
            });
        }
        let next = world
            .prepare_volume_edits(&volumes)
            .map_err(|_| EditError::InvalidVolume)?;
        if next == world.volumes() {
            volumes.clear();
        }
        let mut plan = match replace(world, &mask, fragments, selection_after.clone(), true) {
            Err(EditError::NoChanges) if !self.objects.is_empty() || !volumes.is_empty() => {
                Ok(EditPlan {
                    edits: Vec::new(),
                    roots: Vec::new(),
                    volumes: Vec::new(),
                    selection_after,
                })
            }
            result => result,
        }?;
        plan.volumes = volumes;
        Ok(plan)
    }
}
impl<'de> Deserialize<'de> for Clipboard {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Raw {
            dims: CellPos,
            mask: RegionSet,
            fragments: Vec<CellFragment>,
            #[serde(default)]
            objects: Vec<ObjectPlacement>,
            #[serde(default)]
            volumes: Vec<MaterialVolume>,
        }
        let raw = Raw::deserialize(deserializer)?;
        let result = Self {
            dims: raw.dims,
            mask: raw.mask,
            fragments: raw.fragments,
            objects: raw.objects,
            volumes: raw.volumes,
        };
        result.validate().map_err(serde::de::Error::custom)?;
        Ok(result)
    }
}
fn contains_object_pivot(mask: &RegionSet, position: Vec3, include_upper_face: bool) -> bool {
    let point = position * 8.0;
    mask.boxes().iter().any(|bounds| {
        (0..3).all(|axis| {
            point[axis] >= bounds.min[axis] as f32
                && if include_upper_face {
                    point[axis] <= bounds.max[axis] as f32
                } else {
                    point[axis] < bounds.max[axis] as f32
                }
        })
    })
}
fn normalized_yaw(yaw: f32) -> f32 {
    yaw.rem_euclid(std::f32::consts::TAU)
}
/// Permute canonical vertices and carry the actual diagonal edge to each new
/// logical face. Canonical face winding also corrects reflected orientation.
fn transform_shape(shape: Shape, transform: impl Fn([u8; 3]) -> [u8; 3]) -> Shape {
    let canonical = Shape::solid().vertices;
    let mapping = canonical.map(|vertex| {
        canonical
            .iter()
            .position(|&candidate| candidate == transform(vertex))
            .unwrap()
    });
    let mut result = Shape::solid();
    for (new, vertex) in mapping.into_iter().zip(shape.vertices) {
        result.vertices[new] = transform(vertex);
    }
    result.diagonals = 0;
    for (old_face, &corners) in SHAPE_FACES.iter().enumerate() {
        let mapped = corners.map(|vertex| mapping[vertex]);
        let new_face = SHAPE_FACES
            .iter()
            .position(|face| mapped.iter().all(|vertex| face.contains(vertex)))
            .unwrap();
        let diagonal = if shape.diagonals & (1 << old_face) == 0 {
            [mapped[0], mapped[2]]
        } else {
            [mapped[1], mapped[3]]
        };
        let face = SHAPE_FACES[new_face];
        if !(diagonal.contains(&face[0]) && diagonal.contains(&face[2])) {
            result.diagonals |= 1 << new_face;
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use sauerteig_core::ObjectKind;
    use sauerteig_core::{WorldBounds, WORLD_HEIGHT, WORLD_MAX};

    fn object(id: u32, kind: ObjectKind, position: [f32; 3], rotation: f32) -> WorldObject {
        WorldObject {
            id,
            kind,
            position: Vec3::from_array(position),
            rotation,
        }
    }

    #[test]
    fn painting_and_clipboard_transforms_preserve_independent_face_materials() {
        let mut world = World::empty();
        world.set(
            [0, 0, 0],
            Some(Cell {
                face_materials: Some([0, 1, 2, 3, 4, 5]),
                ..Cell::solid(0)
            }),
        );
        let selected = selection([0; 3], [8; 3], 1, 1, 8);
        let plan = selected.plan_paint(&world, 7).unwrap();
        apply(&mut world, &plan);
        let painted = world.root_fragments([0; 3])[0].cell;
        assert_eq!(painted.face_materials, Some([0, 1, 2, 7, 4, 5]));
        assert_eq!(
            selected.plan_paint(&world, 7).unwrap_err(),
            EditError::NoChanges
        );
        let original = Clipboard::copy(&world, &selected).unwrap();
        let mut rotated = original.clone();
        rotated.rotate_y().unwrap();
        assert_eq!(
            rotated.fragments[0].cell.face_materials,
            Some([4, 5, 2, 7, 1, 0])
        );
        for _ in 0..3 {
            rotated.rotate_y().unwrap();
        }
        assert_eq!(rotated, original);
        let mut mirrored = original.clone();
        mirrored.mirror(0).unwrap();
        assert_eq!(
            mirrored.fragments[0].cell.face_materials,
            Some([1, 0, 2, 7, 4, 5])
        );
        mirrored.mirror(0).unwrap();
        assert_eq!(mirrored, original);
        mirrored.mirror(2).unwrap();
        assert_eq!(
            mirrored.fragments[0].cell.face_materials,
            Some([0, 1, 2, 7, 5, 4])
        );
    }

    #[test]
    fn compact_worlds_support_local_paint_copy_and_paste_beyond_the_starter_bounds() {
        let bounds = WorldBounds {
            min: [-512, -256, -512],
            max: [512, 256, 512],
        };
        let source = CellFragment {
            bounds: FineBox {
                min: [192 * 8, 0, 192 * 8],
                max: [208 * 8, 16 * 8, 208 * 8],
            },
            origin: [192 * 8, 0, 192 * 8],
            size: 128,
            cell: Cell::solid(0),
        };
        let mut world = World::from_compact_parts(bounds, &[source], &[], &[]).unwrap();
        let selected = selection(
            [200 * 8, 15 * 8, 200 * 8],
            [201 * 8, 16 * 8, 201 * 8],
            1,
            1,
            8,
        );
        let paint = selected.plan_paint(&world, 7).unwrap();
        apply(&mut world, &paint);
        assert_eq!(world.region_count(), 1);
        assert_eq!(
            world.root_fragments([200, 15, 200])[0]
                .cell
                .material_for_face(3),
            7
        );
        assert_eq!(
            world.root_fragments([201, 15, 200])[0]
                .cell
                .material_for_face(3),
            0
        );
        let clipboard = Clipboard::copy(&world, &selected).unwrap();
        let destination = selection(
            [220 * 8, 15 * 8, 220 * 8],
            [221 * 8, 16 * 8, 221 * 8],
            1,
            1,
            8,
        );
        let paste = clipboard.plan_paste(&world, &destination).unwrap();
        apply(&mut world, &paste);
        assert_eq!(
            world.root_fragments([220, 15, 220])[0]
                .cell
                .material_for_face(3),
            7
        );
        assert!(world
            .raycast(Vec3::new(220.5, 20.0, 220.5), -Vec3::Y, 10.0)
            .is_some());
    }

    #[test]
    fn oversized_compact_selections_and_clipboard_fragments_fail_before_expansion() {
        let bounds = WorldBounds {
            min: [0; 3],
            max: [4096; 3],
        };
        let source = CellFragment {
            bounds: bounds.fine_box(),
            origin: [0; 3],
            size: 32768,
            cell: Cell::solid(0),
        };
        let world =
            World::from_compact_parts(bounds, std::slice::from_ref(&source), &[], &[]).unwrap();
        let selected = selection([0; 3], [32768; 3], 1, 1, 8);
        assert_eq!(
            Clipboard::copy(&world, &selected).unwrap_err(),
            EditError::TooLarge
        );
        let encoded = serde_json::json!({
            "dims": [32768, 32768, 32768],
            "mask": one_region(source.bounds).unwrap(),
            "fragments": [source],
        });
        assert!(serde_json::from_value::<Clipboard>(encoded).is_err());
    }

    #[test]
    fn object_capture_respects_subtracted_volumes_and_half_open_edges() {
        let mut regions = mask(&[([-16, 0, -8], [16, 16, 16])]);
        regions
            .subtract(RegionBox::new([-8, 0, 0], [8, 16, 8]).unwrap())
            .unwrap();
        let objects = vec![
            object(11, ObjectKind::Light, [-1.5, 0.5, 0.5], 0.0),
            object(12, ObjectKind::Spawn, [0.0, 0.5, 0.5], 0.0),
            object(13, ObjectKind::Pipe, [1.0, 0.5, 0.5], 0.0),
            object(14, ObjectKind::Rock, [2.0, 0.5, 0.5], 0.0),
            object(15, ObjectKind::WallLamp, [-2.0, 1.0, -1.0], 0.0),
        ];
        let clip =
            Clipboard::copy_regions_with_objects(&World::empty(), &regions, &objects).unwrap();
        assert_eq!(
            clip.objects().iter().map(|o| o.kind).collect::<Vec<_>>(),
            vec![ObjectKind::Light, ObjectKind::Pipe, ObjectKind::WallLamp]
        );
        assert_eq!(clip.objects()[0].position, Vec3::new(0.5, 0.5, 1.5));
        let placed = clip.placed_objects([32, 8, 24]).unwrap();
        assert_eq!(placed[0].position, Vec3::new(4.5, 1.5, 4.5));
    }

    #[test]
    fn object_transforms_follow_geometry_and_preserve_facing() {
        let mut world = World::empty();
        world.set([0, 0, 0], Some(Cell::solid(2)));
        let objects = vec![
            object(1, ObjectKind::UtilityBox, [0.25, 0.5, 1.5], 0.3),
            object(2, ObjectKind::HangingLamp, [0.0, 1.0, 0.0], 0.0),
        ];
        let original = Clipboard::copy_regions_with_objects(
            &world,
            &mask(&[([0, 0, 0], [16, 16, 24])]),
            &objects,
        )
        .unwrap();
        let mut rotated = original.clone();
        rotated.rotate_y().unwrap();
        assert_eq!(rotated.dims(), [24, 16, 16]);
        assert_eq!(rotated.objects()[0].position, Vec3::new(1.5, 0.5, 1.75));
        let direction = |yaw: f32| Vec3::new(yaw.sin(), 0.0, yaw.cos());
        let old_facing = direction(original.objects()[0].rotation);
        let rotated_facing = direction(rotated.objects()[0].rotation);
        assert!((rotated_facing - Vec3::new(old_facing.z, 0.0, -old_facing.x)).length() < 1e-6);
        for _ in 0..3 {
            rotated.rotate_y().unwrap();
        }
        assert_eq!(rotated.fragments, original.fragments);
        assert_eq!(rotated.mask, original.mask);
        for (after, before) in rotated.objects().iter().zip(original.objects()) {
            assert_eq!(after.position, before.position);
            assert!((direction(after.rotation) - direction(before.rotation)).length() < 1e-6);
        }
        for axis in [0, 2] {
            let mut mirrored = original.clone();
            mirrored.mirror(axis).unwrap();
            let mut expected = old_facing;
            expected[axis] = -expected[axis];
            assert!((direction(mirrored.objects()[0].rotation) - expected).length() < 1e-6);
            mirrored.mirror(axis).unwrap();
            assert_eq!(mirrored.fragments, original.fragments);
            for (after, before) in mirrored.objects().iter().zip(original.objects()) {
                assert_eq!(after.position, before.position);
                assert!((direction(after.rotation) - direction(before.rotation)).length() < 1e-6);
            }
        }
    }

    #[test]
    fn object_only_paste_is_valid_and_checks_destination_bounds() {
        let world = World::empty();
        let region = mask(&[([0, 0, 0], [8, 8, 8])]);
        let target = selection([16, 8, 24], [24, 16, 32], 1, 1, 8);
        let empty = Clipboard::copy_regions(&world, &region).unwrap();
        assert_eq!(
            empty.plan_paste(&world, &target).unwrap_err(),
            EditError::NoChanges
        );
        let clip = Clipboard::copy_regions_with_objects(
            &world,
            &region,
            &[object(1, ObjectKind::Spawn, [0.5, 0.0, 0.5], 0.0)],
        )
        .unwrap();
        let plan = clip.plan_paste(&world, &target).unwrap();
        assert!(plan.roots.is_empty());
        assert_eq!(plan.selection_after, target);
        assert_eq!(
            clip.placed_objects([8192 * 8, 0, 0]).unwrap_err(),
            EditError::OutsideWorld
        );
        let mut boundary = Clipboard::copy_regions_with_objects(
            &world,
            &region,
            &[object(2, ObjectKind::Light, [0.0, 0.5, 0.5], 0.0)],
        )
        .unwrap();
        boundary.mirror(0).unwrap();
        let at_edge = selection([(WORLD_MAX - 1) * 8, 0, 0], [WORLD_MAX * 8, 8, 8], 1, 1, 8);
        assert_eq!(
            boundary.plan_paste(&world, &at_edge).unwrap_err(),
            EditError::OutsideWorld
        );
    }

    #[test]
    fn clipboard_object_deserialization_rejects_invalid_data_and_limits() {
        let clip = Clipboard::copy_regions_with_objects(
            &World::empty(),
            &mask(&[([0, 0, 0], [16, 16, 16])]),
            &[object(1, ObjectKind::Light, [0.5, 0.5, 0.5], 0.0)],
        )
        .unwrap();
        let json = serde_json::to_value(&clip).unwrap();
        assert_eq!(
            serde_json::from_value::<Clipboard>(json.clone()).unwrap(),
            clip
        );
        for position in [
            serde_json::json!([-0.01, 0.5, 0.5]),
            serde_json::json!([3.0, 0.5, 0.5]),
            serde_json::json!([null, 0.5, 0.5]),
        ] {
            let mut bad = json.clone();
            bad["objects"][0]["position"] = position;
            assert!(serde_json::from_value::<Clipboard>(bad).is_err());
        }
        let mut bad = json.clone();
        bad["objects"][0]["rotation"] = serde_json::json!(1_000_001.0);
        assert!(serde_json::from_value::<Clipboard>(bad).is_err());
        let mut bad = json.clone();
        bad["objects"] =
            serde_json::Value::Array(vec![json["objects"][0].clone(); MAX_CLIPBOARD_OBJECTS + 1]);
        assert!(serde_json::from_value::<Clipboard>(bad).is_err());
        let mut invalid = clip.clone();
        invalid.objects[0].rotation = f32::NAN;
        let previous = invalid.objects[0].position;
        assert_eq!(invalid.rotate_y().unwrap_err(), EditError::InvalidObject);
        assert_eq!(invalid.objects[0].position, previous);
    }
    fn selection(min: CellPos, max: CellPos, axis: usize, sign: i32, grid: i32) -> EditSelection {
        EditSelection {
            min,
            max,
            axis,
            sign,
            grid,
        }
    }
    fn hit(pos: CellPos, axis: usize, sign: i32) -> RayHit {
        let mut normal = Vec3::ZERO;
        normal[axis] = sign as f32;
        let mut point = Vec3::from_array(pos.map(|v| v as f32 + 0.5));
        point[axis] = (pos[axis] + i32::from(sign > 0)) as f32;
        RayHit {
            pos,
            bounds: FineBox::root(pos),
            face: (axis * 2 + usize::from(sign > 0)) as u8,
            normal,
            point,
            distance: 1.0,
        }
    }
    fn apply(world: &mut World, plan: &EditPlan) {
        world.apply_root_edits(&plan.roots).unwrap();
        world.apply_volume_edits(&plan.volumes).unwrap();
    }
    fn mask(boxes: &[(CellPos, CellPos)]) -> RegionSet {
        RegionSet::from_boxes(
            boxes
                .iter()
                .map(|&(min, max)| RegionBox::new(min, max).unwrap()),
        )
        .unwrap()
    }
    fn material_volume(min: CellPos, max: CellPos, material: VolumeMaterial) -> MaterialVolume {
        MaterialVolume {
            bounds: FineBox { min, max },
            material,
        }
    }
    #[test]
    fn material_assignment_uses_sparse_selection_and_preserves_solid_geometry() {
        let mut world = World::empty();
        world.set([0; 3], Some(Cell::solid(2)));
        let solids = world.cells();
        let selected = selection([0; 3], [8; 3], 1, 1, 8);
        let mask = mask(&[([0; 3], [8; 3]), ([128, 0, 128], [384, 128, 384])]);
        let plan = selected
            .plan_volume_regions(&world, &mask, Some(VolumeMaterial::Water))
            .unwrap();
        assert!(plan.roots.is_empty());
        assert_eq!(plan.volumes.len(), 2);
        apply(&mut world, &plan);
        assert_eq!(world.cells(), solids);
        assert_eq!(world.volumes().len(), 2);
        assert_eq!(world.volume_material_at(Vec3::splat(0.5)), None);
        assert_eq!(
            world.volume_material_at(Vec3::new(24.0, 1.0, 24.0)),
            Some(VolumeMaterial::Water)
        );
        assert_eq!(world.volume_material_at(Vec3::new(10.0, 1.0, 10.0)), None);
        assert_eq!(
            selected
                .plan_volume_regions(&world, &mask, Some(VolumeMaterial::Water))
                .unwrap_err(),
            EditError::NoChanges
        );
        let clear = selected.plan_volume_regions(&world, &mask, None).unwrap();
        apply(&mut world, &clear);
        assert!(world.volumes().is_empty());
        assert_eq!(world.cells(), solids);
    }
    #[test]
    fn material_copy_clips_to_selection_and_transforms_with_blueprint() {
        let mut world = World::empty();
        world
            .replace_volumes(&[material_volume(
                [-8, 0, -8],
                [48, 16, 48],
                VolumeMaterial::Water,
            )])
            .unwrap();
        let clip = Clipboard::copy_regions(
            &world,
            &mask(&[([0; 3], [8, 16, 8]), ([24, 0, 16], [32, 8, 24])]),
        )
        .unwrap();
        assert_eq!(clip.volumes.len(), 2);
        assert_eq!(
            clip.volumes[0].bounds,
            FineBox {
                min: [0; 3],
                max: [8, 16, 8]
            }
        );
        assert_eq!(
            clip.volumes[1].bounds,
            FineBox {
                min: [24, 0, 16],
                max: [32, 8, 24]
            }
        );
        let mut rotated = clip.clone();
        rotated.rotate_y().unwrap();
        assert_eq!(
            rotated.volumes[0].bounds,
            FineBox {
                min: [0, 0, 24],
                max: [8, 16, 32]
            }
        );
        assert_eq!(
            rotated.volumes[1].bounds,
            FineBox {
                min: [16, 0, 0],
                max: [24, 8, 8]
            }
        );
        for _ in 0..3 {
            rotated.rotate_y().unwrap();
        }
        assert_eq!(rotated, clip);
        let mut mirrored = clip.clone();
        mirrored.mirror(0).unwrap();
        assert_eq!(
            mirrored.volumes[0].bounds,
            FineBox {
                min: [24, 0, 0],
                max: [32, 16, 8]
            }
        );
        mirrored.mirror(0).unwrap();
        assert_eq!(mirrored, clip);
        let restored: Clipboard =
            serde_json::from_value(serde_json::to_value(&clip).unwrap()).unwrap();
        assert_eq!(restored, clip);
    }
    #[test]
    fn paste_replaces_materials_and_normal_space_only_inside_selected_union() {
        let mut world = World::empty();
        world
            .replace_volumes(&[
                material_volume([0; 3], [8; 3], VolumeMaterial::Glass),
                material_volume([80, 0, 0], [120, 8, 8], VolumeMaterial::Water),
            ])
            .unwrap();
        let clip =
            Clipboard::copy_regions(&world, &mask(&[([0; 3], [8; 3]), ([16, 0, 0], [24, 8, 8])]))
                .unwrap();
        let target = selection([80, 0, 0], [88, 8, 8], 1, 1, 8);
        let plan = clip.plan_paste(&world, &target).unwrap();
        assert!(plan.roots.is_empty());
        apply(&mut world, &plan);
        assert_eq!(
            world.volume_material_at(Vec3::new(10.5, 0.5, 0.5)),
            Some(VolumeMaterial::Glass)
        );
        assert_eq!(
            world.volume_material_at(Vec3::new(11.5, 0.5, 0.5)),
            Some(VolumeMaterial::Water)
        );
        assert_eq!(world.volume_material_at(Vec3::new(12.5, 0.5, 0.5)), None);
        assert_eq!(
            world.volume_material_at(Vec3::new(13.5, 0.5, 0.5)),
            Some(VolumeMaterial::Water)
        );
        assert_eq!(
            clip.plan_paste(&world, &target).unwrap_err(),
            EditError::NoChanges
        );
    }
    #[test]
    fn clipboard_rejects_overlapping_materials_and_materials_in_selection_gaps() {
        let mut world = World::empty();
        world
            .replace_volumes(&[material_volume([0; 3], [8; 3], VolumeMaterial::Glass)])
            .unwrap();
        let clip =
            Clipboard::copy_regions(&world, &mask(&[([0; 3], [8; 3]), ([16, 0, 0], [24, 8, 8])]))
                .unwrap();
        let valid = serde_json::to_value(&clip).unwrap();
        let mut bad = valid.clone();
        bad["volumes"]
            .as_array_mut()
            .unwrap()
            .push(valid["volumes"][0].clone());
        assert!(serde_json::from_value::<Clipboard>(bad).is_err());
        let mut bad = valid.clone();
        bad["volumes"][0]["bounds"] = serde_json::json!({"min":[8,0,0],"max":[16,8,8]});
        assert!(serde_json::from_value::<Clipboard>(bad).is_err());
        let mut legacy = valid;
        legacy.as_object_mut().unwrap().remove("volumes");
        assert!(serde_json::from_value::<Clipboard>(legacy)
            .unwrap()
            .volumes
            .is_empty());
    }
    #[test]
    fn crossing_materials_and_selection_strips_stop_at_the_clipboard_limit() {
        let mut world = World::empty();
        world
            .replace_volumes(
                &(0..128)
                    .map(|x| {
                        material_volume([x * 2, 0, 0], [x * 2 + 1, 8, 258], VolumeMaterial::Water)
                    })
                    .collect::<Vec<_>>(),
            )
            .unwrap();
        let selected = mask(
            &(0..129)
                .map(|z| ([0, 0, z * 2], [256, 8, z * 2 + 1]))
                .collect::<Vec<_>>(),
        );
        assert_eq!(
            Clipboard::copy_regions(&world, &selected).unwrap_err(),
            EditError::TooLarge
        );
    }
    #[test]
    fn fine_grid_selects_inside_every_face_at_distant_coordinates() {
        for coordinate in [-8191, -4096, 4096, 8190] {
            for axis in 0..3 {
                for sign in [-1, 1] {
                    let hit = hit([coordinate; 3], axis, sign);
                    let selected = EditSelection::from_hit(&hit, 1).unwrap();
                    let expected = coordinate * 8 + if sign > 0 { 7 } else { 0 };
                    assert_eq!(selected.min[axis], expected);
                    assert_eq!(selected.max[axis], expected + 1);
                    assert!(hit.bounds.contains(selected.bounds()));
                }
            }
        }
    }

    #[test]
    fn negative_coordinates_and_fine_grid_select_correct_face() {
        let s = EditSelection::from_hit(&hit([-2, 3, -4], 1, 1), 4).unwrap();
        assert_eq!(s.min, [-12, 28, -28]);
        assert_eq!(s.max, [-8, 32, -24]);
        assert_eq!(s.face_plane(), 4.0);
        let coarse = s.resnap(64).unwrap();
        assert_eq!(coarse.max[1], 32);
        assert_eq!(coarse.min[1], -32);
    }
    #[test]
    fn drag_spans_both_tangents_on_all_six_faces() {
        for axis in 0..3 {
            for sign in [-1, 1] {
                let h = hit([0, 2, 0], axis, sign);
                let s = EditSelection::from_hit(&h, 2).unwrap();
                let mut p = h.point;
                for a in 0..3 {
                    if a != axis {
                        p[a] += 1.25;
                    }
                }
                let drag = s.begin_drag(h.point);
                let next = drag.update(p + h.normal * 2.0, -h.normal).unwrap();
                assert_eq!(next.min[axis], s.min[axis]);
                assert_eq!(next.max[axis], s.max[axis]);
                for a in 0..3 {
                    if a != axis {
                        assert_eq!(next.max[a] - s.max[a], 10);
                    }
                }
            }
        }
    }
    #[test]
    fn fine_extrude_carve_roundtrip_and_noop_are_atomic() {
        let mut world = World::empty();
        let s = selection([0, 0, 0], [2, 2, 2], 1, 1, 2);
        let add = s.plan_extrude(&world, 2).unwrap();
        assert!(add.edits.is_empty());
        assert_eq!(add.roots.len(), 1);
        apply(&mut world, &add);
        let fragment = &world.root_fragments([0, 0, 0])[0];
        assert_eq!(fragment.size, 8); // Solid sources normalize to their containing root.
        assert_eq!(fragment.bounds.min, [0, 2, 0]);
        assert_eq!(s.plan_extrude(&world, 2).unwrap_err(), EditError::NoChanges);
        let remove = add.selection_after.plan_carve(&world).unwrap();
        apply(&mut world, &remove);
        assert!(world.root_fragments([0, 0, 0]).is_empty());
        assert_eq!(remove.selection_after, s);
    }
    #[test]
    fn coarse_floor_paint_carve_and_reverse_stay_in_bounds() {
        let mut world = World::empty();
        for x in 0..8 {
            for z in 0..8 {
                world.set([x, 0, z], Some(Cell::solid(0)));
            }
        }
        let s = selection([0, -56, 0], [64, 8, 64], 1, 1, 64);
        let paint = s.plan_paint(&world, 2).unwrap();
        assert_eq!(paint.roots.len(), 64);
        apply(&mut world, &paint);
        let carve = s.plan_carve(&world).unwrap();
        assert_eq!(carve.selection_after.max[1], 0);
        apply(&mut world, &carve);
        let reverse = carve.selection_after.plan_extrude(&world, 2).unwrap();
        assert!(reverse.roots.iter().all(|r| r.pos[1] >= 0));
        apply(&mut world, &reverse);
        assert!(world.get([0, 0, 0]).is_some());
    }
    #[test]
    fn coarse_source_refines_for_fine_corner_without_changing_other_space() {
        let mut world = World::empty();
        world.set([0, 0, 0], Some(Cell::solid(1)));
        let s = selection([0, 4, 0], [4, 8, 4], 1, 1, 4);
        let p = s.plan_deform(&world, Some(0), -4).unwrap();
        apply(&mut world, &p);
        let roots = world.root_fragments([0, 0, 0]);
        let shaped = roots.iter().find(|f| f.size == 4).unwrap();
        assert_eq!(shaped.bounds, s.bounds());
        assert!(shaped.cell.heights.contains(&4));
        let other = world
            .raycast(Vec3::new(0.75, 2.0, 0.75), -Vec3::Y, 4.0)
            .unwrap();
        assert!((other.point.y - 1.0).abs() < 1e-5);
    }
    #[test]
    fn clipped_warped_source_preserves_geometry_and_rejects_inexact_shaping() {
        let mut world = World::empty();
        let ramp = Cell {
            heights: [0, 8, 8, 0],
            ..Cell::solid(2)
        };
        world.set([0, 0, 0], Some(ramp));
        let s = selection([0, 0, 0], [4, 8, 8], 0, -1, 4);
        let clip = Clipboard::copy(&world, &s).unwrap();
        assert_eq!(clip.fragments.len(), 1);
        assert_eq!(clip.fragments[0].cell, ramp);
        assert_eq!(clip.fragments[0].size, 8);
        assert_eq!(
            s.plan_deform(&world, Some(0), 1).unwrap_err(),
            EditError::ClippedShape
        );
        let target = selection([16, 0, 0], [20, 8, 8], 0, -1, 4);
        let paste = clip.plan_paste(&world, &target).unwrap();
        apply(&mut world, &paste);
        let h = world
            .raycast(Vec3::new(2.25, 2.0, 0.5), -Vec3::Y, 4.0)
            .unwrap();
        assert!((h.point.y - 0.25).abs() < 1e-5);
    }
    #[test]
    fn all_faces_deform_and_flatten_without_folding() {
        for axis in 0..3 {
            for sign in [-1, 1] {
                let mut world = World::empty();
                world.set([0, 1, 0], Some(Cell::solid(1)));
                let s = selection([0, 8, 0], [8, 16, 8], axis, sign, 8);
                let p = s.plan_deform_edge(&world, 0, -1).unwrap();
                apply(&mut world, &p);
                assert!(world.root_fragments([0, 1, 0])[0].cell.is_valid());
                world.set([0, 1, 0], Some(Cell::solid(1)));
                let p = s.plan_deform(&world, None, -8).unwrap();
                apply(&mut world, &p);
                assert!(world.root_fragments([0, 1, 0]).is_empty());
            }
        }
    }
    #[test]
    fn union_clipboard_keeps_holes_and_selected_empty_space_distinct() {
        let mut world = World::empty();
        world.set([0, 0, 0], Some(Cell::solid(2)));
        let selected = mask(&[([0, 0, 0], [8, 8, 8]), ([16, 0, 0], [24, 8, 8])]);
        let clip = Clipboard::copy_regions(&world, &selected).unwrap();
        assert_eq!(clip.fragments.len(), 1);
        assert_eq!(clip.mask.volume(), 1024);
        world.set([0, 0, 0], Some(Cell::solid(5)));
        for x in 4..7 {
            world.set([x, 0, 0], Some(Cell::solid(4)));
        }
        let target = selection([32, 0, 0], [40, 8, 8], 1, 1, 8);
        let plan = clip.plan_paste(&world, &target).unwrap();
        apply(&mut world, &plan);
        assert_eq!(world.get([4, 0, 0]).unwrap().material, 2);
        assert_eq!(world.get([5, 0, 0]).unwrap().material, 4);
        assert!(world.get([6, 0, 0]).is_none());
        assert_eq!(world.get([0, 0, 0]).unwrap().material, 5);
    }
    #[test]
    fn sparse_large_selection_does_not_enumerate_empty_cells() {
        let mut world = World::empty();
        world.set([0, 0, 0], Some(Cell::solid(1)));
        let selected = mask(&[
            ([0, 0, 0], [800, 800, 2400]),
            ([800, 0, 0], [1200, 400, 400]),
        ]);
        let clip = Clipboard::copy_regions(&world, &selected).unwrap();
        assert_eq!(clip.fragments.len(), 1);
        assert_eq!(clip.mask.boxes().len(), 2);
        let paint = selection([0, 0, 0], [800, 800, 2400], 1, 1, 8)
            .plan_paint_regions(&world, &selected, 3)
            .unwrap();
        assert_eq!(paint.roots.len(), 1);
    }
    #[test]
    fn paste_bounds_failure_keeps_destination_unchanged_even_selected_empty_area() {
        let mut world = World::empty();
        world.set([0, 0, 0], Some(Cell::solid(2)));
        let clip = Clipboard::copy_regions(&world, &mask(&[([0, 0, 0], [16, 8, 8])])).unwrap();
        let target = selection([WORLD_MAX * 8 - 8, 0, 0], [WORLD_MAX * 8, 8, 8], 1, 1, 8);
        assert_eq!(
            clip.plan_paste(&world, &target).unwrap_err(),
            EditError::OutsideWorld
        );
        assert_eq!(world.get([0, 0, 0]).unwrap().material, 2);
        let huge = selection([0, 0, 0], [800, 8, 2400], 1, 1, 8);
        assert_eq!(
            huge.plan_extrude(&world, 1).unwrap_err(),
            EditError::OutsideWorld
        );
    }
    #[test]
    fn mask_transform_carries_clipped_sources_and_roundtrips_exactly() {
        let mut world = World::empty();
        world.set(
            [0, 0, 0],
            Some(Cell {
                heights: [8, 0, 8, 0],
                diagonal: true,
                ..Cell::solid(2)
            }),
        );
        let mut clip = Clipboard::copy_regions(
            &world,
            &mask(&[([2, 0, 0], [4, 8, 8]), ([8, 0, 8], [16, 8, 16])]),
        )
        .unwrap();
        let original = clip.clone();
        for _ in 0..4 {
            clip.rotate_y().unwrap();
        }
        assert_eq!(clip, original);
        for axis in [0, 2] {
            clip.mirror(axis).unwrap();
            clip.mirror(axis).unwrap();
            assert_eq!(clip, original);
        }
        assert_eq!(clip.mirror(1).unwrap_err(), EditError::UnsupportedTransform);
        assert_eq!(clip, original);
    }
    #[test]
    fn clipboard_deserialization_rejects_overlapping_or_outside_fragments() {
        let mut world = World::empty();
        world.set([0, 0, 0], Some(Cell::solid(1)));
        let clip = Clipboard::copy_regions(&world, &mask(&[([0, 0, 0], [8, 8, 8])])).unwrap();
        let json = serde_json::to_string(&clip).unwrap();
        assert_eq!(serde_json::from_str::<Clipboard>(&json).unwrap(), clip);
        let mut value: serde_json::Value = serde_json::from_str(&json).unwrap();
        let f = value["fragments"][0].clone();
        value["fragments"].as_array_mut().unwrap().push(f);
        assert!(serde_json::from_value::<Clipboard>(value).is_err());
    }
    #[test]
    fn every_face_uses_the_same_corner_edge_and_shape_mapping() {
        let mut world = World::empty();
        world.set([0, 3, 0], Some(Cell::solid(0)));
        for (face, vertices) in SHAPE_FACES.iter().enumerate() {
            let axis = face / 2;
            let sign = if face % 2 == 1 { 1 } else { -1 };
            let selection = EditSelection::from_hit(&hit([0, 3, 0], axis, sign), 8).unwrap();
            assert_eq!(selection.face_index(), face);
            for corner in 0..4 {
                assert_eq!(
                    selection.cursor_corner(selection.face_corner(corner).unwrap()),
                    Some(corner)
                );
                let a = selection.face_corner(corner).unwrap();
                let b = selection.face_corner((corner + 1) % 4).unwrap();
                assert_eq!(selection.cursor_edge((a + b) * 0.5), Some(corner));
                for edge in [false, true] {
                    let plan = if edge {
                        selection.plan_deform_edge(&world, corner, -1)
                    } else {
                        selection.plan_deform(&world, Some(corner), -1)
                    }
                    .unwrap();
                    let actual = plan.roots[0].fragments[0].cell.as_shape();
                    let mut expected = Shape::solid();
                    for (index, &vertex) in vertices.iter().enumerate() {
                        if index == corner || edge && index == (corner + 1) % 4 {
                            expected.vertices[vertex][axis] =
                                (expected.vertices[vertex][axis] as i32 - sign) as u8;
                        }
                    }
                    assert_eq!(
                        actual.vertices, expected.vertices,
                        "face {face}, corner {corner}, edge {edge}"
                    );
                    assert!(plan.roots[0].fragments[0].cell.is_valid());
                }
            }
        }
    }

    #[test]
    fn fine_selection_carves_the_actual_low_ramp_hit() {
        for grid in [1, 2] {
            let mut world = World::empty();
            world.set(
                [0, 0, 0],
                Some(Cell {
                    heights: [0, 8, 8, 0],
                    ..Cell::solid(1)
                }),
            );
            let h = world
                .raycast(Vec3::new(0.23, 2.0, 0.55), -Vec3::Y, 4.0)
                .unwrap();
            assert!((h.point.y - 0.23).abs() < 1e-5);
            let selected = EditSelection::from_hit(&h, grid).unwrap();
            assert!(selected.max[1] <= 2);
            let plan = selected.plan_carve(&world).unwrap();
            apply(&mut world, &plan);
            let changed = world.raycast(Vec3::new(0.23, 2.0, 0.55), -Vec3::Y, 4.0);
            assert!(changed.is_none_or(|h| h.point.y < 0.23 - 1e-5));
            let untouched = world
                .raycast(Vec3::new(0.8, 2.0, 0.55), -Vec3::Y, 4.0)
                .unwrap();
            assert!((untouched.point.y - 0.8).abs() < 1e-5);
        }
    }
    #[test]
    fn empty_clipboard_can_clear_selected_space_and_extend_reorients_only_on_request() {
        let mut world = World::empty();
        let mut s = selection([0, 0, 0], [8, 8, 8], 1, 1, 8);
        let clip = Clipboard::copy(&world, &s).unwrap();
        assert!(clip.fragments.is_empty());
        world.set([0, 0, 0], Some(Cell::solid(1)));
        let plan = clip.plan_paste(&world, &s).unwrap();
        apply(&mut world, &plan);
        assert!(world.get([0, 0, 0]).is_none());
        s = EditSelection::from_hit(&hit([0, 3, 0], 1, 1), 8).unwrap();
        let other = hit([2, 5, 2], 0, -1);
        let extended = s.extend_to_hit(&other).unwrap();
        assert_eq!(extended.dims(), [24, 24, 24]);
        assert_eq!(extended.axis, 0);
        assert_eq!(extended.sign, -1);
        let dragged = s
            .begin_drag(hit([0, 3, 0], 1, 1).point)
            .update_point(other.point)
            .unwrap();
        assert_eq!(dragged.axis, 1);
        assert_eq!(dragged.dims()[1], 8);
    }
    #[test]
    fn extrusion_and_carve_follow_face_on_all_six_faces() {
        for axis in 0..3 {
            for sign in [-1, 1] {
                let mut world = World::empty();
                let s = EditSelection::from_hit(&hit([4, 10, 4], axis, sign), 16).unwrap();
                let plan = s.plan_extrude(&world, 0).unwrap();
                assert_eq!(plan.roots.len(), 8);
                apply(&mut world, &plan);
                assert_eq!(
                    plan.selection_after.face_plane(),
                    s.face_plane() + sign as f32 * 2.0
                );
                let remove = plan.selection_after.plan_carve(&world).unwrap();
                assert_eq!(remove.selection_after, s);
                apply(&mut world, &remove);
                assert!(world.cells().is_empty());
                assert!(world.detailed_roots().is_empty());
            }
        }
        let s = EditSelection::from_hit(&hit([0, 0, 0], 1, 1), 64).unwrap();
        let p = s.plan_extrude(&World::empty(), 0).unwrap();
        assert_eq!(p.roots.len(), 512);
        assert_eq!(p.roots.iter().map(|r| r.pos[1]).min(), Some(1));
    }
    #[test]
    fn lower_and_upper_boundary_carve_support_reverse_extrusion() {
        for (height, sign, boundary) in [(0, 1, 0), (WORLD_HEIGHT - 1, -1, WORLD_HEIGHT)] {
            let mut world = World::empty();
            for x in 0..8 {
                for z in 0..8 {
                    world.set([x, height, z], Some(Cell::solid(0)));
                }
            }
            let s = EditSelection::from_hit(&hit([0, height, 0], 1, sign), 64).unwrap();
            let p = s.plan_carve(&world).unwrap();
            apply(&mut world, &p);
            assert_eq!(p.selection_after.face_plane(), boundary as f32);
            assert_eq!(p.selection_after.dims(), s.dims());
            let reverse = p.selection_after.plan_extrude(&world, 0).unwrap();
            assert_eq!(reverse.roots.len(), 512);
            apply(&mut world, &reverse);
            for x in 0..8 {
                for z in 0..8 {
                    assert!(world.get([x, height, z]).is_some());
                }
            }
        }
    }
    #[test]
    fn paint_and_multiple_face_deformations_preserve_existing_shape() {
        let mut world = World::empty();
        world.set(
            [0, 3, 0],
            Some(Cell {
                heights: [8, 6, 4, 2],
                ..Cell::solid(0)
            }),
        );
        let top = EditSelection::from_hit(&hit([0, 3, 0], 1, 1), 8).unwrap();
        assert_eq!(
            top.plan_paint(&world, 2).unwrap().roots[0].fragments[0]
                .cell
                .heights,
            [8, 6, 4, 2]
        );
        assert_eq!(
            top.plan_deform(&world, Some(1), -1).unwrap().roots[0].fragments[0]
                .cell
                .heights,
            [8, 6, 4, 1]
        );
        assert_eq!(
            top.plan_deform(&world, None, -2).unwrap().roots[0].fragments[0]
                .cell
                .heights,
            [6, 4, 2, 0]
        );
        world.set([0, 3, 0], Some(Cell::solid(0)));
        let side = EditSelection::from_hit(&hit([0, 3, 0], 0, 1), 8).unwrap();
        let p = side.plan_deform(&world, None, -1).unwrap();
        apply(&mut world, &p);
        let before = world.root_fragments([0, 3, 0])[0].cell.as_shape();
        let after = top.plan_deform(&world, Some(0), -1).unwrap().roots[0].fragments[0]
            .cell
            .as_shape();
        for v in 0..8 {
            assert_eq!(after.vertices[v][0], before.vertices[v][0]);
        }
        assert_eq!(after.vertices[4][1], 7);
    }
    #[test]
    fn folded_deformation_rejects_whole_selection() {
        let mut world = World::empty();
        world.set([0, 3, 0], Some(Cell::solid(0)));
        let mut shape = Shape::solid();
        for &v in &SHAPE_FACES[0] {
            shape.vertices[v][0] = 1;
        }
        world.set(
            [0, 3, 1],
            Some(Cell {
                shape: Some(shape),
                ..Cell::solid(0)
            }),
        );
        let s = selection([0, 24, 0], [8, 32, 16], 0, 1, 8);
        let before = world.cells();
        assert_eq!(
            s.plan_deform(&world, None, -8).unwrap_err(),
            EditError::InvalidShape
        );
        assert_eq!(world.cells(), before);
    }
    #[test]
    fn legacy_rotation_and_mirror_preserve_nonplanar_heights_for_both_diagonals() {
        for diagonal in [false, true] {
            let cell = Cell {
                heights: [8, 1, 6, 3],
                diagonal,
                ..Cell::solid(1)
            };
            let mut world = World::empty();
            world.set([0, 0, 0], Some(cell));
            let original =
                Clipboard::copy_regions(&world, &mask(&[([0, 0, 0], [16, 8, 24])])).unwrap();
            for operation in 0..3 {
                let mut clip = original.clone();
                if operation == 0 {
                    clip.rotate_y().unwrap();
                } else {
                    clip.mirror(if operation == 1 { 0 } else { 2 }).unwrap();
                }
                let transformed = clip.fragments[0].cell;
                for x in [0.1, 0.4, 0.7, 0.9] {
                    for z in [0.1, 0.3, 0.6, 0.9] {
                        let (nx, nz) = match operation {
                            0 => (z, 1.0 - x),
                            1 => (1.0 - x, z),
                            _ => (x, 1.0 - z),
                        };
                        assert!(
                            (cell.height_at(x, z) - transformed.height_at(nx, nz)).abs() < 1e-6
                        );
                    }
                }
            }
        }
    }
    fn triangle_signature(
        shape: Shape,
        transform: impl Fn([u8; 3]) -> [u8; 3],
    ) -> Vec<[[u8; 3]; 3]> {
        let mut triangles = Vec::new();
        for (face, &[a, b, c, d]) in SHAPE_FACES.iter().enumerate() {
            let split = if shape.diagonals & (1 << face) == 0 {
                [[a, b, c], [a, c, d]]
            } else {
                [[a, b, d], [b, c, d]]
            };
            for vertices in split {
                let mut triangle = vertices.map(|v| transform(shape.vertices[v]));
                triangle.sort();
                triangles.push(triangle);
            }
        }
        triangles.sort();
        triangles
    }
    #[test]
    fn generalized_transforms_preserve_every_triangle_and_reject_atomically() {
        let mut shape = Shape::solid();
        shape.diagonals = 0b101010;
        shape.vertices[4] = [1, 8, 0];
        shape.vertices[7] = [1, 8, 8];
        shape.vertices[5] = [7, 6, 0];
        shape.vertices[6] = [7, 6, 8];
        let mut world = World::empty();
        world.set(
            [0, 0, 0],
            Some(Cell {
                shape: Some(shape),
                ..Cell::solid(2)
            }),
        );
        let original = Clipboard::copy_regions(&world, &mask(&[([0, 0, 0], [8, 8, 8])])).unwrap();
        let mut rotated = original.clone();
        rotated.rotate_y().unwrap();
        assert_eq!(
            triangle_signature(shape, |[x, y, z]| [z, y, 8 - x]),
            triangle_signature(rotated.fragments[0].cell.as_shape(), |p| p)
        );
        for _ in 0..3 {
            rotated.rotate_y().unwrap();
        }
        assert_eq!(rotated, original);
        for axis in [0, 2] {
            let mut mirrored = original.clone();
            mirrored.mirror(axis).unwrap();
            assert_eq!(
                triangle_signature(shape, |mut p| {
                    p[axis] = 8 - p[axis];
                    p
                }),
                triangle_signature(mirrored.fragments[0].cell.as_shape(), |p| p)
            );
            mirrored.mirror(axis).unwrap();
            assert_eq!(mirrored, original);
        }
        let mut invalid = original.clone();
        invalid.fragments[0].cell.shape.as_mut().unwrap().vertices[0][0] = 9;
        let before = invalid.clone();
        assert_eq!(invalid.rotate_y().unwrap_err(), EditError::InvalidShape);
        assert_eq!(invalid, before);
    }
    #[test]
    fn ray_parallel_or_behind_does_not_change_selection() {
        let h = hit([0, 3, 0], 1, 1);
        let s = EditSelection::from_hit(&h, 8).unwrap();
        let drag = s.begin_drag(h.point);
        for direction in [Vec3::X, Vec3::Y] {
            assert_eq!(
                drag.update(Vec3::new(0.5, 8.0, 0.5), direction)
                    .unwrap_err(),
                EditError::NoPlaneIntersection
            );
        }
    }
    #[test]
    fn imported_large_source_survives_clipboard_serialization_transforms_and_paste() {
        let ramp = Cell {
            heights: [0, 8, 8, 0],
            ..Cell::solid(0)
        };
        let mut world = World::empty();
        let roots: Vec<_> = [[0, 0, 0], [1, 0, 0], [1, 1, 0]]
            .into_iter()
            .map(|pos| RootEdit {
                pos,
                fragments: vec![CellFragment {
                    bounds: FineBox::root(pos),
                    origin: [0; 3],
                    size: 64,
                    cell: ramp,
                }],
            })
            .collect();
        world.apply_root_edits(&roots).unwrap();
        let selected = selection([0, 0, 0], [16, 16, 8], 0, -1, 8);
        let clip = Clipboard::copy(&world, &selected).unwrap();
        let mut clip: Clipboard =
            serde_json::from_slice(&serde_json::to_vec(&clip).unwrap()).unwrap();
        for _ in 0..4 {
            clip.rotate_y().unwrap();
        }
        clip.mirror(0).unwrap();
        clip.mirror(0).unwrap();
        assert!(clip.fragments.iter().all(|f| f.size == 64));
        let target = selection([32, 0, 0], [48, 16, 8], 0, -1, 8);
        let plan = clip.plan_paste(&world, &target).unwrap();
        apply(&mut world, &plan);
        for x in [0.25, 0.75, 1.25, 1.75] {
            let hit = world
                .raycast(Vec3::new(x + 4.0, 4.0, 0.5), -Vec3::Y, 8.0)
                .unwrap();
            assert!((hit.point.y - x).abs() < 1e-5);
        }
    }
}
