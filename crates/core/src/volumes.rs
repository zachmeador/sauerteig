//! Sparse material space, independent of the terrain's surface textures.
use crate::{FineBox, RayHit, Vec3, World, FINE_UNITS};
use serde::{Deserialize, Serialize};

pub const MAX_MATERIAL_VOLUMES: usize = 16_384;
/// A maximum-size pasted layer can also clear an equally detailed destination.
pub const MAX_VOLUME_EDITS: usize = MAX_MATERIAL_VOLUMES * 2;
/// Bound positive-area shared-face pairs of equal material so exterior surface
/// subtraction cannot produce an unbounded renderer mesh from crossing strips.
pub const MAX_VOLUME_FACE_CONTACTS: usize = 65_536;
const MAX_VOLUME_PIECES: usize = MAX_MATERIAL_VOLUMES * 6;

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
    fn ray_query(
        &self,
        boxes: &[FineBox],
        origin: Vec3,
        direction: Vec3,
        distance: f32,
    ) -> Vec<usize> {
        let mut result = Vec::new();
        if !self.nodes.is_empty() {
            self.ray_node(0, boxes, origin, direction, distance, &mut result);
        }
        result
    }
    fn ray_node(
        &self,
        node: usize,
        boxes: &[FineBox],
        origin: Vec3,
        direction: Vec3,
        distance: f32,
        out: &mut Vec<usize>,
    ) {
        let node = &self.nodes[node];
        let intersects = |bounds: FineBox| {
            crate::geometry::ray_box_interval(
                origin,
                direction,
                bounds.world_min(),
                bounds.world_max(),
            )
            .is_some_and(|(enter, exit)| enter <= distance && exit >= 0.0)
        };
        if !intersects(node.bounds) {
            return;
        }
        match &node.kind {
            NodeKind::Branch(a, b) => {
                self.ray_node(*a, boxes, origin, direction, distance, out);
                self.ray_node(*b, boxes, origin, direction, distance, out);
            }
            NodeKind::Leaf(ids) => {
                out.extend(ids.iter().copied().filter(|&id| intersects(boxes[id])))
            }
        }
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct VolumeLayer {
    volumes: Vec<MaterialVolume>,
    boxes: Vec<FineBox>,
    index: BoxIndex,
}
impl VolumeLayer {
    fn new(volumes: Vec<MaterialVolume>) -> Self {
        let boxes: Vec<_> = volumes.iter().map(|volume| volume.bounds).collect();
        let index = BoxIndex::new(&boxes);
        Self {
            volumes,
            boxes,
            index,
        }
    }

    fn material_at(&self, point: Vec3) -> Option<VolumeMaterial> {
        let min = (point * FINE_UNITS as f32).floor().as_ivec3().to_array();
        let max = min.map(|value| value.saturating_add(1));
        self.index
            .query(&self.boxes, FineBox { min, max })
            .into_iter()
            .find_map(|id| {
                let volume = self.volumes[id];
                (point.cmpge(volume.bounds.world_min()).all()
                    && point.cmplt(volume.bounds.world_max()).all())
                .then_some(volume.material)
            })
    }
}

/// Validate bounded, disjoint material boxes without expanding world cells.
pub fn validate_material_volumes(
    volumes: &[MaterialVolume],
    bounds: FineBox,
) -> Result<(), String> {
    let limit = crate::compact::MAX_WORLD_COORDINATE * FINE_UNITS;
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
) -> Result<Vec<FineBox>, String> {
    let mut pieces = vec![bounds];
    for cut in cuts {
        let mut next = Vec::new();
        for piece in pieces {
            next.extend(piece.subtract(cut));
            if next.len() > MAX_VOLUME_PIECES {
                return Err(
                    "This edit creates too much material volume detail; use a smaller selection."
                        .into(),
                );
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
    pub fn volumes(&self) -> &[MaterialVolume] {
        &self.volumes.volumes
    }

    pub fn replace_volumes(&mut self, volumes: &[MaterialVolume]) -> Result<(), String> {
        validate_material_volumes(volumes, self.bounds().fine_box())?;
        self.volumes = VolumeLayer::new(coalesce_material_volumes(volumes.to_vec()));
        Ok(())
    }

    /// Prepare a complete replacement for an atomic geometry/object/material edit.
    pub fn prepare_volume_edits(
        &self,
        edits: &[VolumeEdit],
    ) -> Result<Vec<MaterialVolume>, String> {
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
                cut_pieces(volume.bounds, cuts.into_iter().map(|id| boxes[id]))?
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
                cut_pieces(edit.bounds, cuts.into_iter().map(|id| boxes[id]))?
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

    pub fn apply_volume_edits(&mut self, edits: &[VolumeEdit]) -> Result<(), String> {
        self.volumes = VolumeLayer::new(self.prepare_volume_edits(edits)?);
        Ok(())
    }

    /// Intersections are clipped to the query, for selections and copies.
    pub fn volume_regions(&self, bounds: FineBox) -> Vec<MaterialVolume> {
        let mut result: Vec<_> = self
            .volumes
            .index
            .query(&self.volumes.boxes, bounds)
            .into_iter()
            .map(|id| {
                let volume = self.volumes.volumes[id];
                MaterialVolume {
                    bounds: volume.bounds.intersection(bounds).unwrap(),
                    ..volume
                }
            })
            .collect();
        result.sort_unstable_by_key(|volume| (volume.bounds, volume.material));
        result
    }

    pub fn volume_material_at(&self, point: Vec3) -> Option<VolumeMaterial> {
        if !point.is_finite() {
            return None;
        }
        if point.cmplt(self.bounds().world_min()).any()
            || point.cmpge(self.bounds().world_max()).any()
        {
            return None;
        }
        let material = self.volumes.material_at(point)?;
        // Imported material space can surround walls and slopes. Terrain owns
        // occupied points, so water cannot turn a solid wall into swimmable air.
        let epsilon = (point.abs().max_element() * f32::EPSILON * 2.0).max(0.00001);
        (!self.intersects_box(point - Vec3::splat(epsilon), point + Vec3::splat(epsilon)))
            .then_some(material)
    }

    pub(crate) fn solid_volumes_in_box(&self, min: Vec3, max: Vec3) -> Vec<MaterialVolume> {
        if !min.is_finite() || !max.is_finite() || !min.cmplt(max).all() {
            return Vec::new();
        }
        let bounds = FineBox {
            min: (min * FINE_UNITS as f32).floor().as_ivec3().to_array(),
            max: (max * FINE_UNITS as f32).ceil().as_ivec3().to_array(),
        };
        self.volumes
            .index
            .query(&self.volumes.boxes, bounds)
            .into_iter()
            .filter_map(|id| {
                let volume = self.volumes.volumes[id];
                (volume.material == VolumeMaterial::Glass
                    && min.cmplt(volume.bounds.world_max()).all()
                    && max.cmpgt(volume.bounds.world_min()).all())
                .then_some(volume)
            })
            .collect()
    }

    pub fn intersects_solid_volume(&self, min: Vec3, max: Vec3) -> bool {
        !self.solid_volumes_in_box(min, max).is_empty()
    }

    pub fn raycast_volumes(
        &self,
        origin: Vec3,
        direction: Vec3,
        max_distance: f32,
    ) -> Option<RayHit> {
        self.raycast_volume_material(origin, direction, max_distance, None)
    }

    pub fn raycast_solid_volumes(
        &self,
        origin: Vec3,
        direction: Vec3,
        max_distance: f32,
    ) -> Option<RayHit> {
        self.raycast_volume_material(origin, direction, max_distance, Some(VolumeMaterial::Glass))
    }

    fn raycast_volume_material(
        &self,
        origin: Vec3,
        direction: Vec3,
        max_distance: f32,
        material: Option<VolumeMaterial>,
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
        let mut nearest = None;
        let mut distance_limit = max_distance;
        for id in self
            .volumes
            .index
            .ray_query(&self.volumes.boxes, origin, direction, max_distance)
        {
            let volume = self.volumes.volumes[id];
            if material.is_some_and(|material| material != volume.material) {
                continue;
            }
            let min = volume.bounds.world_min();
            let max = volume.bounds.world_max();
            let Some((enter, exit)) =
                crate::geometry::ray_box_interval(origin, direction, min, max)
            else {
                continue;
            };
            for distance in [enter, exit] {
                if distance < 0.0 || distance > distance_limit {
                    continue;
                }
                let point = origin + direction * distance;
                let (face, _) = (0..6)
                    .map(|face| {
                        let axis = face / 2;
                        let plane = if face % 2 == 0 { min[axis] } else { max[axis] };
                        (face, (point[axis] - plane).abs())
                    })
                    .min_by(|a, b| a.1.total_cmp(&b.1))
                    .unwrap();
                let mut normal = Vec3::ZERO;
                normal[face / 2] = if face % 2 == 0 { -1.0 } else { 1.0 };
                let epsilon = (point.abs().max_element() * f32::EPSILON * 2.0).max(0.001);
                // Equal material continues across a partial shared face. Test
                // both box boundaries so a ray inside joined space can still
                // reach the exposed exterior after skipping that hidden join.
                if self.volumes.material_at(point + normal * epsilon) == Some(volume.material) {
                    continue;
                }
                nearest = Some(RayHit {
                    pos: ((point - normal * epsilon).floor()).as_ivec3().to_array(),
                    bounds: volume.bounds,
                    face: face as u8,
                    normal,
                    point,
                    distance,
                });
                distance_limit = distance;
            }
        }
        nearest
    }
}
