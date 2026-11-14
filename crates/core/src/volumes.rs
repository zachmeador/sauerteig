//! Water and glass as format 3's boxes. The tree holds them as cube material;
//! the boxes stay as loaded, and a change writes cube material only where it
//! differs, so unchanged glass keeps its boxes and their edge bands.
use crate::{
    cube_material_for, FineBox, RayHit, Vec3, World, FINE_UNITS, MAT_AIR, MAX_CONVERSION_LEAVES,
    MAX_WORLD_PIECES,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

pub const MAX_MATERIAL_VOLUMES: usize = 16_384;
/// A maximum-size pasted layer can also clear an equally detailed destination.
pub const MAX_VOLUME_EDITS: usize = MAX_MATERIAL_VOLUMES * 2;
/// Bound positive-area shared-face pairs of equal material so exterior surface
/// subtraction cannot produce an unbounded renderer mesh from crossing strips.
pub const MAX_VOLUME_FACE_CONTACTS: usize = 65_536;
const MAX_VOLUME_PIECES: usize = MAX_MATERIAL_VOLUMES * 6;
const TOO_DETAILED: &str =
    "This edit creates too much material volume detail; use a smaller selection.";

/// The most leaves cube-material writes may take a world to. An edit stops at
/// the world's guard, counted before anything is built, so a thin pane cannot
/// use up memory. Loading a world and applying a patch the server accepted
/// stop only at the memory limit conversion uses, so a world saved over the
/// guard still opens and a client never refuses what its server checked.
#[derive(Clone, Copy)]
enum Ceiling {
    Edit,
    Memory,
}
impl Ceiling {
    fn leaves(self) -> usize {
        match self {
            Self::Edit => MAX_WORLD_PIECES,
            Self::Memory => MAX_CONVERSION_LEAVES,
        }
    }
    fn error(self) -> &'static str {
        match self {
            Self::Edit => "This water or glass change would take the world past its 786,432-piece limit; use a smaller selection.",
            Self::Memory => "Water and glass would take this world past the 4,194,304 leaves it may hold in memory.",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VolumeMaterial {
    Water,
    Glass,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MaterialVolume {
    pub bounds: FineBox,
    pub material: VolumeMaterial,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VolumeEdit {
    pub bounds: FineBox,
    /// None restores normal space without changing terrain geometry.
    pub material: Option<VolumeMaterial>,
}

#[derive(Clone, Debug)]
struct Node {
    bounds: FineBox,
    max_id: usize,
    kind: NodeKind,
}
#[derive(Clone, Debug)]
enum NodeKind {
    Branch(usize, usize),
    Leaf(Vec<usize>),
}
#[derive(Clone, Debug, Default)]
struct BoxIndex {
    nodes: Vec<Node>,
}
impl BoxIndex {
    fn new(boxes: &[FineBox]) -> Self {
        let mut index = Self::default();
        if !boxes.is_empty() {
            index.build(boxes, (0..boxes.len()).collect());
        }
        index
    }
    fn build(&mut self, boxes: &[FineBox], mut ids: Vec<usize>) -> usize {
        let bounds = ids
            .iter()
            .map(|&id| boxes[id])
            .reduce(|a, b| FineBox {
                min: std::array::from_fn(|axis| a.min[axis].min(b.min[axis])),
                max: std::array::from_fn(|axis| a.max[axis].max(b.max[axis])),
            })
            .unwrap();
        let node = self.nodes.len();
        self.nodes.push(Node {
            bounds,
            max_id: ids.iter().copied().max().unwrap(),
            kind: NodeKind::Leaf(Vec::new()),
        });
        if ids.len() <= 8 {
            self.nodes[node].kind = NodeKind::Leaf(ids);
        } else {
            // Split by centroid spread, including long parallel slabs whose
            // longest box axis does not separate their positions.
            let axis = (0..3)
                .max_by_key(|&axis| {
                    let mut low = i32::MAX;
                    let mut high = i32::MIN;
                    for &id in &ids {
                        let center = boxes[id].min[axis] + boxes[id].max[axis];
                        low = low.min(center);
                        high = high.max(center);
                    }
                    high - low
                })
                .unwrap();
            ids.sort_unstable_by_key(|&id| boxes[id].min[axis] + boxes[id].max[axis]);
            let right = ids.split_off(ids.len() / 2);
            let left = self.build(boxes, ids);
            let right = self.build(boxes, right);
            self.nodes[node].kind = NodeKind::Branch(left, right);
        }
        node
    }
    fn query(&self, boxes: &[FineBox], bounds: FineBox) -> Vec<usize> {
        let mut result = Vec::new();
        if !self.nodes.is_empty() && !bounds.is_empty() {
            self.query_node(0, boxes, bounds, None, &mut result);
        }
        result
    }
    fn query_after(&self, boxes: &[FineBox], bounds: FineBox, after: usize) -> Vec<usize> {
        let mut result = Vec::new();
        if !self.nodes.is_empty() && !bounds.is_empty() {
            self.query_node(0, boxes, bounds, Some(after), &mut result);
        }
        result
    }
    fn has_covering_after(&self, boxes: &[FineBox], bounds: FineBox, after: Option<usize>) -> bool {
        !self.nodes.is_empty() && self.covering_node(0, boxes, bounds, after)
    }
    fn covering_node(
        &self,
        id: usize,
        boxes: &[FineBox],
        bounds: FineBox,
        after: Option<usize>,
    ) -> bool {
        let node = &self.nodes[id];
        if after.is_some_and(|after| node.max_id <= after) || !node.bounds.contains(bounds) {
            return false;
        }
        match &node.kind {
            NodeKind::Leaf(ids) => ids
                .iter()
                .rev()
                .any(|&id| after.is_none_or(|after| id > after) && boxes[id].contains(bounds)),
            NodeKind::Branch(a, b) => {
                let (first, second) = if self.nodes[*a].max_id > self.nodes[*b].max_id {
                    (*a, *b)
                } else {
                    (*b, *a)
                };
                self.covering_node(first, boxes, bounds, after)
                    || self.covering_node(second, boxes, bounds, after)
            }
        }
    }
    fn query_node(
        &self,
        node: usize,
        boxes: &[FineBox],
        bounds: FineBox,
        after: Option<usize>,
        out: &mut Vec<usize>,
    ) {
        let node = &self.nodes[node];
        if after.is_some_and(|after| node.max_id <= after)
            || node.bounds.intersection(bounds).is_none()
        {
            return;
        }
        match &node.kind {
            NodeKind::Branch(a, b) => {
                self.query_node(*a, boxes, bounds, after, out);
                self.query_node(*b, boxes, bounds, after, out);
            }
            NodeKind::Leaf(ids) => out.extend(ids.iter().copied().filter(|&id| {
                after.is_none_or(|after| id > after) && boxes[id].intersection(bounds).is_some()
            })),
        }
    }
}

fn edit_writes(edits: &[VolumeEdit]) -> Vec<(FineBox, u16)> {
    edits
        .iter()
        .map(|e| (e.bounds, e.material.map_or(MAT_AIR, cube_material_for)))
        .collect()
}

/// Validate bounded, disjoint material boxes without expanding world cells.
pub fn validate_material_volumes(
    volumes: &[MaterialVolume],
    bounds: FineBox,
) -> Result<(), String> {
    let limit = crate::world::MAX_WORLD_COORDINATE * FINE_UNITS;
    if bounds.is_empty()
        || bounds
            .min
            .into_iter()
            .chain(bounds.max)
            .any(|value| value < -limit || value > limit)
    {
        return Err("Invalid material volume coordinate bounds.".into());
    }
    if volumes.len() > MAX_MATERIAL_VOLUMES {
        return Err(
            "A world may contain at most 16,384 material volumes; simplify adjacent regions."
                .into(),
        );
    }
    if volumes.iter().any(|volume| !bounds.contains(volume.bounds)) {
        return Err("Material volumes must have nonempty bounds inside the world.".into());
    }
    let boxes: Vec<_> = volumes.iter().map(|volume| volume.bounds).collect();
    let index = BoxIndex::new(&boxes);
    for (id, &bounds) in boxes.iter().enumerate() {
        if index
            .query(&boxes, bounds)
            .into_iter()
            .any(|other| other != id)
        {
            return Err("Material volume boxes must not overlap.".into());
        }
    }
    let mut face_contacts = 0;
    for material in [VolumeMaterial::Water, VolumeMaterial::Glass] {
        let boxes: Vec<_> = volumes
            .iter()
            .filter(|volume| volume.material == material)
            .map(|volume| volume.bounds)
            .collect();
        let index = BoxIndex::new(&boxes);
        for &bounds in &boxes {
            for axis in 0..3 {
                // Integer coordinates let a one-unit outside slab select only
                // immediately adjacent boxes. Its unchanged footprint excludes
                // point/edge contacts. Prior disjoint validation rules out a
                // candidate crossing back through this face's interior.
                let mut face = bounds;
                face.min[axis] = bounds.max[axis];
                face.max[axis] = bounds.max[axis] + 1;
                face_contacts += index.query(&boxes, face).len();
                // Positive faces count each shared pair exactly once. Other
                // materials retain their interface rather than subtracting it.
                if face_contacts > MAX_VOLUME_FACE_CONTACTS {
                    return Err(
                        "Material volumes have too many shared faces; simplify touching regions."
                            .into(),
                    );
                }
            }
        }
    }
    Ok(())
}

/// Merge exactly adjacent boxes with matching material and rectangular faces.
/// This does not validate bounds or overlap, and can reduce an importer batch
/// before the stored layer's volume count limit is applied.
pub fn coalesce_material_volumes(mut volumes: Vec<MaterialVolume>) -> Vec<MaterialVolume> {
    loop {
        let before = volumes.len();
        for axis in 0..3 {
            let u = (axis + 1) % 3;
            let v = (axis + 2) % 3;
            volumes.sort_unstable_by_key(|volume| {
                (
                    volume.material,
                    volume.bounds.min[u],
                    volume.bounds.max[u],
                    volume.bounds.min[v],
                    volume.bounds.max[v],
                    volume.bounds.min[axis],
                    volume.bounds.max[axis],
                )
            });
            let mut merged: Vec<MaterialVolume> = Vec::with_capacity(volumes.len());
            for volume in volumes {
                if let Some(previous) = merged.last_mut() {
                    if previous.material == volume.material
                        && [u, v].into_iter().all(|a| {
                            previous.bounds.min[a] == volume.bounds.min[a]
                                && previous.bounds.max[a] == volume.bounds.max[a]
                        })
                        && previous.bounds.max[axis] == volume.bounds.min[axis]
                    {
                        previous.bounds.max[axis] = volume.bounds.max[axis];
                        continue;
                    }
                }
                merged.push(volume);
            }
            volumes = merged;
        }
        if volumes.len() == before {
            break;
        }
    }
    volumes.sort_unstable_by_key(|volume| (volume.bounds, volume.material));
    volumes
}

fn cut_pieces(
    bounds: FineBox,
    cuts: impl Iterator<Item = FineBox>,
    limit: usize,
) -> Result<Vec<FineBox>, String> {
    let mut pieces = vec![bounds];
    for cut in cuts {
        let mut next = Vec::new();
        for piece in pieces {
            next.extend(piece.subtract(cut));
            if next.len() > limit {
                return Err(TOO_DETAILED.into());
            }
        }
        pieces = next;
        if pieces.is_empty() {
            break;
        }
    }
    Ok(pieces)
}

impl World {
    /// Water and glass as format 3's boxes.
    pub fn volumes(&self) -> &[MaterialVolume] {
        &self.volumes
    }

    /// Replaces the water and glass boxes. Cube material is written only
    /// where it changes, and the written boxes are returned. Loads, patches
    /// and the server's undo and redo replace boxes, so the writes are bounded
    /// by memory (`MAX_CONVERSION_LEAVES`) rather than the world's guard.
    pub fn replace_volumes(&mut self, volumes: &[MaterialVolume]) -> Result<Vec<FineBox>, String> {
        validate_material_volumes(volumes, self.bounds().fine_box())?;
        self.set_volumes(coalesce_material_volumes(volumes.to_vec()))
    }

    fn set_volumes(&mut self, volumes: Vec<MaterialVolume>) -> Result<Vec<FineBox>, String> {
        if volumes == *self.volumes {
            return Ok(Vec::new());
        }
        let writes = self.cube_writes(&volumes);
        self.tree = self.written(&writes, Ceiling::Memory)?;
        self.volumes = Arc::new(volumes);
        Ok(writes.into_iter().map(|(bounds, _)| bounds).collect())
    }

    /// Where cube material changes from the current boxes to `next`: first
    /// plain space where a current box is gone and no new box covers it,
    /// then each new box's material where the current boxes held another.
    /// A box whose difference would take many pieces is written whole.
    fn cube_writes(&self, next: &[MaterialVolume]) -> Vec<(FineBox, u16)> {
        const PIECES: usize = 64;
        let current = self.volumes.as_slice();
        let set = |list: &[MaterialVolume]| -> std::collections::BTreeSet<_> {
            list.iter().map(|v| (v.bounds, v.material)).collect()
        };
        let (in_current, in_next) = (set(current), set(next));
        let current_boxes: Vec<_> = current.iter().map(|v| v.bounds).collect();
        let next_boxes: Vec<_> = next.iter().map(|v| v.bounds).collect();
        let (current_index, next_index) =
            (BoxIndex::new(&current_boxes), BoxIndex::new(&next_boxes));
        let mut writes = Vec::new();
        let mut cleared = Vec::new();
        for old in current
            .iter()
            .filter(|v| !in_next.contains(&(v.bounds, v.material)))
        {
            let mut cuts = next_index.query(&next_boxes, old.bounds);
            cuts.sort_unstable();
            match cut_pieces(
                old.bounds,
                cuts.into_iter().map(|id| next_boxes[id]),
                PIECES,
            ) {
                Ok(pieces) => writes.extend(pieces.into_iter().map(|p| (p, MAT_AIR))),
                Err(_) => {
                    writes.push((old.bounds, MAT_AIR));
                    cleared.push(old.bounds);
                }
            }
        }
        for new in next
            .iter()
            .filter(|v| !in_current.contains(&(v.bounds, v.material)))
        {
            let cube = cube_material_for(new.material);
            if cleared
                .iter()
                .any(|c: &FineBox| c.intersection(new.bounds).is_some())
            {
                writes.push((new.bounds, cube));
                continue;
            }
            let mut cuts: Vec<_> = current_index
                .query(&current_boxes, new.bounds)
                .into_iter()
                .filter(|&id| current[id].material == new.material)
                .collect();
            cuts.sort_unstable();
            match cut_pieces(
                new.bounds,
                cuts.into_iter().map(|id| current_boxes[id]),
                PIECES,
            ) {
                Ok(pieces) => writes.extend(pieces.into_iter().map(|p| (p, cube))),
                Err(_) => writes.push((new.bounds, cube)),
            }
        }
        writes
    }

    /// The tree with cube-material writes applied in order. Each write's new
    /// leaves are counted before it is built, and a change that would take
    /// the world past its ceiling is refused.
    fn written(
        &self,
        writes: &[(FineBox, u16)],
        ceiling: Ceiling,
    ) -> Result<crate::Octree, String> {
        let mut tree = self.tree.clone();
        let mut budget = ceiling.leaves().saturating_sub(tree.stats().leaves());
        for &(bounds, cube) in writes {
            let added = tree
                .count_cube_write(bounds, cube, budget)
                .ok_or(ceiling.error())?;
            budget -= added;
            tree.set_cube_material(bounds, cube);
        }
        Ok(tree)
    }

    /// Prepare a complete replacement for an atomic geometry/object/material edit.
    pub fn prepare_volume_edits(
        &self,
        edits: &[VolumeEdit],
    ) -> Result<Vec<MaterialVolume>, String> {
        let result = self.edited_volumes(edits)?;
        self.written(&edit_writes(edits), Ceiling::Edit)?;
        Ok(result)
    }

    /// The boxes after edits applied in order; a later edit wins.
    fn edited_volumes(&self, edits: &[VolumeEdit]) -> Result<Vec<MaterialVolume>, String> {
        if edits.len() > MAX_VOLUME_EDITS {
            return Err("An edit may contain at most 32,768 material region changes.".into());
        }
        if edits
            .iter()
            .any(|edit| !self.bounds().fine_box().contains(edit.bounds))
        {
            return Err("Material edits must have nonempty bounds inside the world.".into());
        }
        if edits.is_empty() {
            return Ok(self.volumes().to_vec());
        }
        let boxes: Vec<_> = edits.iter().map(|edit| edit.bounds).collect();
        let index = BoxIndex::new(&boxes);
        let mut result = Vec::new();
        // Only edits intersecting an authored box can split it. The same index
        // handles overlapping edits in request order; a later region wins.
        for volume in self.volumes() {
            if index.has_covering_after(&boxes, volume.bounds, None) {
                continue;
            }
            let mut cuts = index.query(&boxes, volume.bounds);
            cuts.sort_unstable();
            result.extend(
                cut_pieces(
                    volume.bounds,
                    cuts.into_iter().map(|id| boxes[id]),
                    MAX_VOLUME_PIECES,
                )?
                .into_iter()
                .map(|bounds| MaterialVolume {
                    bounds,
                    material: volume.material,
                }),
            );
            if result.len() > MAX_VOLUME_PIECES {
                return Err("This edit creates too much material volume detail.".into());
            }
        }
        for (id, edit) in edits.iter().enumerate() {
            let Some(material) = edit.material else {
                continue;
            };
            if index.has_covering_after(&boxes, edit.bounds, Some(id)) {
                continue;
            }
            let mut cuts = index.query_after(&boxes, edit.bounds, id);
            cuts.sort_unstable();
            result.extend(
                cut_pieces(
                    edit.bounds,
                    cuts.into_iter().map(|id| boxes[id]),
                    MAX_VOLUME_PIECES,
                )?
                .into_iter()
                .map(|bounds| MaterialVolume { bounds, material }),
            );
            if result.len() > MAX_VOLUME_PIECES {
                return Err("This edit creates too much material volume detail.".into());
            }
        }
        let result = coalesce_material_volumes(result);
        validate_material_volumes(&result, self.bounds().fine_box())?;
        Ok(result)
    }

    /// Applies edits in order, a later one winning. The tree takes the
    /// edits' own boxes, which is where cube material can change, and they
    /// may not take the world past `MAX_WORLD_PIECES`. `changed_chunks` gives
    /// the mesh units to build again.
    pub fn apply_volume_edits(&mut self, edits: &[VolumeEdit]) -> Result<(), String> {
        let volumes = self.edited_volumes(edits)?;
        if volumes == *self.volumes {
            return Ok(());
        }
        self.tree = self.written(&edit_writes(edits), Ceiling::Edit)?;
        self.volumes = Arc::new(volumes);
        Ok(())
    }

    /// Intersections are clipped to the query, for selections and copies.
    pub fn volume_regions(&self, bounds: FineBox) -> Vec<MaterialVolume> {
        let mut result: Vec<_> = self
            .volumes
            .iter()
            .filter_map(|volume| {
                Some(MaterialVolume {
                    bounds: volume.bounds.intersection(bounds)?,
                    ..*volume
                })
            })
            .collect();
        result.sort_unstable_by_key(|volume| (volume.bounds, volume.material));
        result
    }

    /// Water or glass at a point, unless solid geometry occupies it.
    pub fn volume_material_at(&self, point: Vec3) -> Option<VolumeMaterial> {
        self.tree.volume_material_at(point)
    }

    /// Whether a box overlaps glass, which blocks movement across its whole
    /// cube.
    pub fn intersects_solid_volume(&self, min: Vec3, max: Vec3) -> bool {
        self.tree.intersects_solid_volume(min, max)
    }

    pub fn raycast_volumes(
        &self,
        origin: Vec3,
        direction: Vec3,
        max_distance: f32,
    ) -> Option<RayHit> {
        self.tree.raycast_volumes(origin, direction, max_distance)
    }

    pub fn raycast_solid_volumes(
        &self,
        origin: Vec3,
        direction: Vec3,
        max_distance: f32,
    ) -> Option<RayHit> {
        self.tree
            .raycast_solid_volumes(origin, direction, max_distance)
    }
}
