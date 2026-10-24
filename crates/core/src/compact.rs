//! Authored large leaves remain compact; local metre-root overrides mask them.
//! The immutable source-box BVH indexes mutable active pieces without rebuilding.
use crate::fine::{clip_fragment, FineRoot, FineTriangle};
use crate::{
    CellEdit, CellFragment, CellPos, ChunkKey, FineBox, RootEdit, Vec3, World, CHUNK_SIZE,
    WORLD_HEIGHT, WORLD_MAX, WORLD_MIN,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub const WORLD_FORMAT_VERSION: u32 = 3;
pub const MAX_COMPACT_REGIONS: usize = 400_000;
pub const MAX_QUERY_ROOTS: usize = 32_768;
/// Bound GPU uploads and remeshing after repeated local cuts in a source batch.
pub const MAX_COMPACT_BATCH_TRIANGLES: usize = 32_768;
pub const MAX_WORLD_COORDINATE: i32 = 8192;
const MESH_BATCH_REGIONS: usize = 128;
const MESH_KEY_BASE: i32 = 1_000_000;
const MAX_REGION_PIECES: usize = 16_384;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorldBounds {
    pub min: CellPos,
    pub max: CellPos,
}
impl Default for WorldBounds {
    fn default() -> Self {
        Self {
            min: [WORLD_MIN, 0, WORLD_MIN],
            max: [WORLD_MAX, WORLD_HEIGHT, WORLD_MAX],
        }
    }
}
impl WorldBounds {
    pub fn is_valid(self) -> bool {
        (0..3).all(|a| {
            self.min[a] >= -MAX_WORLD_COORDINATE
                && self.max[a] <= MAX_WORLD_COORDINATE
                && self.min[a] < self.max[a]
                && self.max[a] - self.min[a] <= MAX_WORLD_COORDINATE
        })
    }
    pub fn contains_pos(self, pos: CellPos) -> bool {
        (0..3).all(|a| pos[a] >= self.min[a] && pos[a] < self.max[a])
    }
    pub fn fine_box(self) -> FineBox {
        FineBox {
            min: self.min.map(|v| v * 8),
            max: self.max.map(|v| v * 8),
        }
    }
    pub fn world_min(self) -> Vec3 {
        Vec3::from_array(self.min.map(|v| v as f32))
    }
    pub fn world_max(self) -> Vec3 {
        Vec3::from_array(self.max.map(|v| v as f32))
    }
}

#[derive(Clone, Debug)]
pub(crate) struct Region {
    pub source: CellFragment,
    pub active: FineRoot,
    masked: BTreeSet<CellPos>,
    pub key: ChunkKey,
}
#[derive(Clone, Debug)]
struct Node {
    bounds: FineBox,
    kind: NodeKind,
}
#[derive(Clone, Debug)]
enum NodeKind {
    Branch(usize, usize),
    Leaf(Vec<usize>),
}
#[derive(Clone, Debug, Default)]
pub(crate) struct CompactWorld {
    pub regions: Vec<Region>,
    nodes: Vec<Node>,
    pub batches: BTreeMap<ChunkKey, Vec<usize>>,
}

fn union(a: FineBox, b: FineBox) -> FineBox {
    FineBox {
        min: std::array::from_fn(|i| a.min[i].min(b.min[i])),
        max: std::array::from_fn(|i| a.max[i].max(b.max[i])),
    }
}

pub(crate) fn compile_fragments(fragments: Vec<CellFragment>) -> Result<FineRoot, String> {
    let mut kept = Vec::new();
    let mut triangles = Vec::new();
    for fragment in fragments {
        let source = clip_fragment(&fragment)?;
        if source.is_empty() {
            continue;
        }
        let index = kept.len();
        triangles.extend(source.into_iter().map(|(points, face)| FineTriangle {
            points,
            face,
            fragment: index,
        }));
        kept.push(fragment);
    }
    Ok(FineRoot {
        fragments: kept,
        triangles,
    })
}

impl CompactWorld {
    fn build_node(&mut self, mut ids: Vec<usize>) -> usize {
        let bounds = ids
            .iter()
            .map(|&i| self.regions[i].source.bounds)
            .reduce(union)
            .unwrap();
        let index = self.nodes.len();
        self.nodes.push(Node {
            bounds,
            kind: NodeKind::Leaf(Vec::new()),
        });
        if ids.len() <= 8 {
            self.nodes[index].kind = NodeKind::Leaf(ids);
        } else {
            let axis = (0..3)
                .max_by_key(|&a| bounds.max[a] - bounds.min[a])
                .unwrap();
            ids.sort_unstable_by_key(|&i| {
                let b = self.regions[i].source.bounds;
                b.min[axis] + b.max[axis]
            });
            let right = ids.split_off(ids.len() / 2);
            let left = self.build_node(ids);
            let right = self.build_node(right);
            self.nodes[index].kind = NodeKind::Branch(left, right);
        }
        index
    }
    pub fn query(&self, bounds: FineBox) -> Vec<usize> {
        let mut out = Vec::new();
        if !self.nodes.is_empty() {
            self.query_node(0, bounds, &mut out);
        }
        out
    }
    fn query_node(&self, index: usize, bounds: FineBox, out: &mut Vec<usize>) {
        let node = &self.nodes[index];
        if node.bounds.intersection(bounds).is_none() {
            return;
        }
        match &node.kind {
            NodeKind::Branch(a, b) => {
                self.query_node(*a, bounds, out);
                self.query_node(*b, bounds, out);
            }
            NodeKind::Leaf(ids) => out.extend(
                ids.iter()
                    .copied()
                    .filter(|&i| self.regions[i].source.bounds.intersection(bounds).is_some()),
            ),
        }
    }
    pub fn ray_candidates(&self, origin: Vec3, direction: Vec3, distance: f32) -> Vec<usize> {
        let mut out = Vec::new();
        if !self.nodes.is_empty() {
            self.ray_node(0, origin, direction, distance, &mut out);
        }
        out
    }
    fn ray_node(
        &self,
        i: usize,
        origin: Vec3,
        direction: Vec3,
        distance: f32,
        out: &mut Vec<usize>,
    ) {
        let node = &self.nodes[i];
        if !crate::geometry::ray_box_interval(
            origin,
            direction,
            node.bounds.world_min(),
            node.bounds.world_max(),
        )
        .is_some_and(|(a, b)| a <= distance && b >= 0.0)
        {
            return;
        }
        match &node.kind {
            NodeKind::Branch(a, b) => {
                self.ray_node(*a, origin, direction, distance, out);
                self.ray_node(*b, origin, direction, distance, out);
            }
            NodeKind::Leaf(ids) => out.extend(ids.iter().copied().filter(|&id| {
                let b = self.regions[id].source.bounds;
                crate::geometry::ray_box_interval(origin, direction, b.world_min(), b.world_max())
                    .is_some_and(|(a, b)| a <= distance && b >= 0.0)
            })),
        }
    }
    pub fn prepare_masks(
        &self,
        positions: &[CellPos],
    ) -> Result<Vec<(usize, FineRoot, BTreeSet<CellPos>)>, String> {
        let mut affected: BTreeMap<usize, BTreeSet<CellPos>> = BTreeMap::new();
        for &pos in positions {
            for id in self.query(FineBox::root(pos)) {
                if !self.regions[id].masked.contains(&pos) {
                    affected.entry(id).or_default().insert(pos);
                }
            }
        }
        let mut result = Vec::new();
        for (id, added) in affected {
            let region = &self.regions[id];
            let mut pieces = region.active.fragments.clone();
            for &pos in &added {
                pieces = pieces
                    .into_iter()
                    .flat_map(|f| {
                        f.bounds
                            .subtract(FineBox::root(pos))
                            .into_iter()
                            .map(move |bounds| CellFragment {
                                bounds,
                                ..f.clone()
                            })
                    })
                    .collect();
                if pieces.len() > MAX_REGION_PIECES {
                    return Err(
                        "Too many cuts in one authored region; simplify local edits.".into(),
                    );
                }
            }
            let mut masked = region.masked.clone();
            masked.extend(added);
            result.push((id, compile_fragments(pieces)?, masked));
        }
        let mut counts = BTreeMap::new();
        for (id, active, _) in &result {
            let key = self.regions[*id].key;
            let count = counts.entry(key).or_insert_with(|| {
                self.batches[&key]
                    .iter()
                    .map(|&i| self.regions[i].active.triangles.len())
                    .sum::<usize>()
            });
            *count = *count - self.regions[*id].active.triangles.len() + active.triangles.len();
        }
        if counts.values().any(|&n| n > MAX_COMPACT_BATCH_TRIANGLES) {
            return Err(
                "Local cuts exceed this mesh batch's triangle limit; simplify the edit.".into(),
            );
        }
        Ok(result)
    }
    pub fn apply_masks(&mut self, prepared: Vec<(usize, FineRoot, BTreeSet<CellPos>)>) {
        for (id, active, masked) in prepared {
            self.regions[id].active = active;
            self.regions[id].masked = masked;
        }
    }
}

impl World {
    pub fn bounds(&self) -> WorldBounds {
        self.bounds
    }
    pub fn contains_pos(&self, pos: CellPos) -> bool {
        self.bounds.contains_pos(pos)
    }
    pub fn has_regions(&self) -> bool {
        !self.compact.regions.is_empty()
    }
    pub fn region_count(&self) -> usize {
        self.compact.regions.len()
    }
    pub fn override_count(&self) -> usize {
        self.overrides.len()
    }
    pub fn regions(&self) -> Vec<CellFragment> {
        self.compact
            .regions
            .iter()
            .map(|r| r.source.clone())
            .collect()
    }
    /// Coarse overrides are already in cells(); include detailed and empty masks.
    pub fn root_overrides(&self) -> Vec<RootEdit> {
        let mut roots = self.detailed_roots();
        roots.extend(
            self.overrides
                .iter()
                .filter(|&&p| self.get(p).is_none() && !self.fine.contains_key(&p))
                .map(|&pos| RootEdit {
                    pos,
                    fragments: Vec::new(),
                }),
        );
        roots.sort_by_key(|r| r.pos);
        roots
    }
    pub fn from_compact_parts(
        bounds: WorldBounds,
        regions: &[CellFragment],
        cells: &[CellEdit],
        roots: &[RootEdit],
    ) -> Result<Self, String> {
        if !bounds.is_valid() {
            return Err("Invalid world bounds (maximum 8192 metres per axis).".into());
        }
        if regions.len() > MAX_COMPACT_REGIONS {
            return Err("Too many authored regions.".into());
        }
        let mut world = Self::default();
        world.bounds = bounds;
        for source in regions {
            if !source.is_valid() || !bounds.fine_box().contains(source.bounds) {
                return Err("Invalid compact region bounds or geometry.".into());
            }
            world.compact.regions.push(Region {
                source: source.clone(),
                active: compile_fragments(vec![source.clone()])?,
                masked: BTreeSet::new(),
                key: [0; 3],
            });
        }
        if !regions.is_empty() {
            world.compact.build_node((0..regions.len()).collect());
            for (i, source) in regions.iter().enumerate() {
                if world
                    .compact
                    .query(source.bounds)
                    .into_iter()
                    .any(|other| other != i)
                {
                    return Err("Compact region boxes must not overlap.".into());
                }
            }
            // BVH traversal is spatially coherent; each source belongs to one bounded batch.
            let order: Vec<_> = world
                .compact
                .nodes
                .iter()
                .filter_map(|n| match &n.kind {
                    NodeKind::Leaf(ids) => Some(ids),
                    _ => None,
                })
                .flatten()
                .copied()
                .collect();
            for (batch, ids) in order.chunks(MESH_BATCH_REGIONS).enumerate() {
                let key = [MESH_KEY_BASE + batch as i32, 0, 0];
                for &id in ids {
                    world.compact.regions[id].key = key;
                }
                world.compact.batches.insert(key, ids.to_vec());
            }
        }
        let mut seen = BTreeSet::new();
        let mut edits = Vec::with_capacity(cells.len() + roots.len());
        for c in cells {
            if !bounds.contains_pos(c.pos)
                || c.cell.is_some_and(|c| !c.is_valid())
                || !seen.insert(c.pos)
            {
                return Err("Invalid or repeated coarse cell.".into());
            }
            edits.push(RootEdit {
                pos: c.pos,
                fragments: c
                    .cell
                    .map(|cell| vec![CellFragment::from_cell(c.pos, cell)])
                    .unwrap_or_default(),
            });
        }
        for root in roots {
            if !seen.insert(root.pos) {
                return Err("A saved root has repeated geometry.".into());
            }
            edits.push(root.clone());
        }
        world.apply_root_edits(&edits)?;
        Ok(world)
    }
    /// Full effective fragments intersecting the query; query edges never add caps.
    pub fn fragments_in(&self, bounds: FineBox) -> Vec<CellFragment> {
        let Some(bounds) = bounds.intersection(self.bounds.fine_box()) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for id in self.compact.query(bounds) {
            out.extend(
                self.compact.regions[id]
                    .active
                    .fragments
                    .iter()
                    .filter(|f| f.bounds.intersection(bounds).is_some())
                    .cloned(),
            );
        }
        // Local query volume is usually a player box. Broad queries enumerate storage.
        let lo = bounds.min.map(|v| v.div_euclid(8));
        let hi = bounds.max.map(|v| (v - 1).div_euclid(8) + 1);
        let volume: i64 = (0..3)
            .map(|a| i64::from(hi[a]) - i64::from(lo[a]))
            .product();
        if volume <= MAX_QUERY_ROOTS as i64 {
            for x in lo[0]..hi[0] {
                for y in lo[1]..hi[1] {
                    for z in lo[2]..hi[2] {
                        let pos = [x, y, z];
                        if let Some(root) = self.fine.get(&pos) {
                            out.extend(
                                root.fragments
                                    .iter()
                                    .filter(|f| f.bounds.intersection(bounds).is_some())
                                    .cloned(),
                            );
                        } else if let Some(cell) = self.get(pos) {
                            out.push(CellFragment::from_cell(pos, cell));
                        }
                    }
                }
            }
        } else {
            out.extend(
                self.cells()
                    .into_iter()
                    .filter(|c| FineBox::root(c.pos).intersection(bounds).is_some())
                    .map(|c| CellFragment::from_cell(c.pos, c.cell.unwrap())),
            );
            out.extend(
                self.fine
                    .values()
                    .flat_map(|r| &r.fragments)
                    .filter(|f| f.bounds.intersection(bounds).is_some())
                    .cloned(),
            );
        }
        out
    }
    pub fn try_roots_in(&self, bounds: FineBox, max_roots: usize) -> Result<Vec<RootEdit>, String> {
        let Some(bounds) = bounds.intersection(self.bounds.fine_box()) else {
            return Ok(Vec::new());
        };
        let mut positions = BTreeSet::new();
        for f in self.fragments_in(bounds) {
            let clip = f.bounds.intersection(bounds).unwrap();
            let lo = clip.min.map(|v| v.div_euclid(8));
            let hi = clip.max.map(|v| (v - 1).div_euclid(8) + 1);
            let count: i64 = (0..3)
                .map(|a| i64::from(hi[a]) - i64::from(lo[a]))
                .product();
            if count > max_roots.min(MAX_QUERY_ROOTS) as i64 {
                return Err(
                    "Selection contains too many metre roots; select a smaller region.".into(),
                );
            }
            for x in lo[0]..hi[0] {
                for y in lo[1]..hi[1] {
                    for z in lo[2]..hi[2] {
                        positions.insert([x, y, z]);
                        if positions.len() > max_roots.min(MAX_QUERY_ROOTS) {
                            return Err(
                                "Selection contains too many metre roots; select a smaller region."
                                    .into(),
                            );
                        }
                    }
                }
            }
        }
        Ok(positions
            .into_iter()
            .filter_map(|pos| {
                let fragments = self.root_fragments(pos);
                (!fragments.is_empty()).then_some(RootEdit { pos, fragments })
            })
            .collect())
    }
    /// Conservative world-space bounds for an opaque mesh key.
    pub fn chunk_bounds(&self, key: ChunkKey) -> FineBox {
        self.compact
            .batches
            .get(&key)
            .map(|ids| {
                ids.iter()
                    .map(|&i| self.compact.regions[i].source.bounds)
                    .reduce(union)
                    .unwrap()
            })
            .unwrap_or_else(|| {
                let min = key.map(|v| v * CHUNK_SIZE * 8);
                FineBox {
                    min,
                    max: min.map(|v| v + CHUNK_SIZE * 8),
                }
            })
    }
    pub(crate) fn dirty_region_keys(&self, positions: &[CellPos]) -> BTreeSet<ChunkKey> {
        let mut keys = BTreeSet::new();
        for &pos in positions {
            let root = FineBox::root(pos);
            let nearby = FineBox {
                min: root.min.map(|v| v - 1),
                max: root.max.map(|v| v + 1),
            };
            keys.extend(
                self.compact
                    .query(nearby)
                    .into_iter()
                    .map(|id| self.compact.regions[id].key),
            );
        }
        keys
    }
}
