use crate::{Vec3, CHUNK_SIZE};
use im::{OrdMap, OrdSet};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::{collections::BTreeSet, sync::Arc};

pub type CellPos = [i32; 3];
pub type ChunkKey = [i32; 3];
const CHUNK_VOLUME: usize = (CHUNK_SIZE * CHUNK_SIZE * CHUNK_SIZE) as usize;

/// Fallback colors for the stable material slots named by `MATERIAL_NAMES`.
pub const MATERIAL_COLORS: [[f32; 3]; 8] = [
    [0.23, 0.245, 0.25],
    [0.070, 0.085, 0.095],
    [0.29, 0.115, 0.045],
    [0.37, 0.355, 0.30],
    [0.39, 0.27, 0.07],
    [0.90, 0.43, 0.12],
    [0.24, 0.235, 0.22],
    [0.14, 0.12, 0.08],
];

pub const MATERIAL_NAMES: [&str; 8] = [
    "Concrete",
    "Gunmetal",
    "Rusted Steel",
    "Worn Concrete",
    "Hazard Paint",
    "Amber Light",
    "Rock",
    "Earth",
];

/// Logical faces -X, +X, -Y, +Y, -Z, +Z, wound outward. A clear diagonal
/// bit splits a quad a-b-c-d at a-c; a set bit splits it at b-d.
pub const SHAPE_FACES: [[usize; 4]; 6] = [
    [0, 3, 7, 4],
    [1, 5, 6, 2],
    [0, 1, 2, 3],
    [4, 7, 6, 5],
    [0, 4, 5, 1],
    [3, 2, 6, 7],
];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Shape {
    /// Bottom corners 0..3, then corresponding top corners 4..7, in eighths.
    pub vertices: [[u8; 3]; 8],
    pub diagonals: u8,
}

impl Shape {
    pub fn solid() -> Self {
        Self {
            vertices: [
                [0, 0, 0],
                [8, 0, 0],
                [8, 0, 8],
                [0, 0, 8],
                [0, 8, 0],
                [8, 8, 0],
                [8, 8, 8],
                [0, 8, 8],
            ],
            diagonals: 0,
        }
    }

    pub fn is_valid(self) -> bool {
        crate::geometry::shape_is_valid(self)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cell {
    pub material: u8,
    /// Optional logical face slots (-X,+X,-Y,+Y,-Z,+Z). Old cells use material.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub face_materials: Option<[u8; 6]>,
    /// (x0,z0), (x1,z0), (x1,z1), (x0,z1), in eighths of a cell.
    pub heights: [u8; 4],
    /// False joins top corners 0 to 2; true joins 1 to 3.
    #[serde(default, skip_serializing_if = "is_false")]
    pub diagonal: bool,
    /// Absent on old maps. When present this boundary replaces heights.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shape: Option<Shape>,
}

fn is_false(value: &bool) -> bool {
    !value
}

impl Cell {
    pub fn solid(material: u8) -> Self {
        Self {
            material,
            face_materials: None,
            heights: [8; 4],
            diagonal: false,
            shape: None,
        }
    }

    pub fn is_valid(self) -> bool {
        (self.material as usize) < MATERIAL_COLORS.len()
            && self
                .face_materials
                .is_none_or(|m| m.into_iter().all(|v| (v as usize) < MATERIAL_COLORS.len()))
            && self.shape.map_or_else(
                || self.heights.iter().all(|&h| h <= 8) && self.heights.iter().any(|&h| h > 0),
                Shape::is_valid,
            )
    }

    pub fn material_for_face(self, face: u8) -> u8 {
        self.face_materials
            .and_then(|m| m.get(face as usize).copied())
            .unwrap_or(self.material)
    }

    pub fn set_face_material(&mut self, face: u8, material: u8) {
        if face >= 6 || material as usize >= MATERIAL_COLORS.len() {
            return;
        }
        let mut faces = self.face_materials.unwrap_or([self.material; 6]);
        faces[face as usize] = material;
        self.face_materials = (faces != [self.material; 6]).then_some(faces);
    }

    pub fn set_material(&mut self, material: u8) {
        if (material as usize) < MATERIAL_COLORS.len() {
            self.material = material;
            self.face_materials = None;
        }
    }

    pub fn as_shape(self) -> Shape {
        self.shape.unwrap_or_else(|| {
            let mut shape = Shape::solid();
            for i in 0..4 {
                shape.vertices[i + 4][1] = self.heights[i];
            }
            if self.diagonal {
                shape.diagonals = (1 << 2) | (1 << 3);
            }
            shape
        })
    }

    pub fn is_full(self) -> bool {
        self.shape.map_or(self.heights == [8; 4], |s| {
            s.vertices == Shape::solid().vertices
        })
    }

    /// Height of the very same two top triangles used by the mesh and rays.
    pub fn height_at(self, x: f32, z: f32) -> f32 {
        let h = self.heights.map(|v| v as f32 / 8.0);
        let x = x.clamp(0.0, 1.0);
        let z = z.clamp(0.0, 1.0);
        if self.diagonal {
            if x + z <= 1.0 {
                h[0] + (h[1] - h[0]) * x + (h[3] - h[0]) * z
            } else {
                h[2] + (h[3] - h[2]) * (1.0 - x) + (h[1] - h[2]) * (1.0 - z)
            }
        } else if z <= x {
            h[0] + (h[1] - h[0]) * x + (h[2] - h[1]) * z
        } else {
            h[0] + (h[2] - h[3]) * x + (h[3] - h[0]) * z
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CellEdit {
    pub pos: CellPos,
    pub cell: Option<Cell>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Vertex {
    pub position: [f32; 3],
    pub normal: [f32; 3],
    pub color: [f32; 3],
    /// Stable material slot, independent of fallback/debug color.
    pub material: u8,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Mesh {
    pub vertices: Vec<Vertex>,
    pub indices: Vec<u32>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct RayHit {
    pub pos: CellPos,
    pub bounds: crate::FineBox,
    /// Logical face -X, +X, -Y, +Y, -Z, +Z; independent of a sloped normal.
    pub face: u8,
    pub normal: Vec3,
    pub point: Vec3,
    pub distance: f32,
}

#[derive(Clone, Debug)]
struct Chunk {
    cells: Box<[Option<Cell>; CHUNK_VOLUME]>,
    occupied: usize,
}

impl Default for Chunk {
    fn default() -> Self {
        Self {
            cells: Box::new([None; CHUNK_VOLUME]),
            occupied: 0,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct World {
    chunks: OrdMap<ChunkKey, Arc<Chunk>>,
    pub(crate) fine: OrdMap<CellPos, Arc<crate::fine::FineRoot>>,
    pub(crate) authored_roots: OrdMap<CellPos, Arc<Vec<crate::CellFragment>>>,
    pub(crate) bounds: crate::WorldBounds,
    pub(crate) compact: crate::compact::CompactWorld,
    pub(crate) overrides: OrdSet<CellPos>,
    pub(crate) volumes: Arc<crate::volumes::VolumeLayer>,
}

/// A consistent, constant-time copy of authored data. Expanding it to wire/save
/// vectors belongs on a worker; no triangles, mesh batches, or query indexes are
/// retained here. Edits copy only the affected persistent-map paths/chunks.
#[derive(Clone, Debug)]
pub struct WorldSnapshot {
    bounds: crate::WorldBounds,
    regions: Arc<Vec<crate::CellFragment>>,
    chunks: OrdMap<ChunkKey, Arc<Chunk>>,
    roots: OrdMap<CellPos, Arc<Vec<crate::CellFragment>>>,
    overrides: OrdSet<CellPos>,
    volumes: Arc<Vec<crate::MaterialVolume>>,
}
impl WorldSnapshot {
    pub fn bounds(&self) -> crate::WorldBounds {
        self.bounds
    }
    pub fn regions(&self) -> &[crate::CellFragment] {
        &self.regions
    }
    pub fn volumes(&self) -> &[crate::MaterialVolume] {
        &self.volumes
    }
    pub fn cells(&self) -> Vec<CellEdit> {
        stored_cells(&self.chunks)
    }
    pub fn root_overrides(&self) -> Vec<crate::RootEdit> {
        let mut roots: Vec<_> = self
            .roots
            .iter()
            .map(|(&pos, fragments)| crate::RootEdit {
                pos,
                fragments: fragments.as_ref().clone(),
            })
            .collect();
        roots.extend(
            self.overrides
                .iter()
                .filter(|&&pos| {
                    !self.roots.contains_key(&pos)
                        && self
                            .chunks
                            .get(&World::cell_chunk(pos))
                            .and_then(|c| c.cells[local_index(pos)])
                            .is_none()
                })
                .map(|&pos| crate::RootEdit {
                    pos,
                    fragments: Vec::new(),
                }),
        );
        roots.sort_by_key(|r| r.pos);
        roots
    }
}

// Old array/object saves remain readable; new snapshots name their format explicitly.
impl Serialize for World {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        if !self.has_regions()
            && self.bounds == crate::WorldBounds::default()
            && self.fine.is_empty()
            && self.volumes().is_empty()
        {
            return self.cells().serialize(serializer);
        }
        #[derive(Serialize)]
        struct Parts {
            format_version: u32,
            bounds: crate::WorldBounds,
            regions: Vec<crate::CellFragment>,
            cells: Vec<CellEdit>,
            roots: Vec<crate::RootEdit>,
            #[serde(skip_serializing_if = "Vec::is_empty")]
            volumes: Vec<crate::MaterialVolume>,
        }
        Parts {
            format_version: crate::WORLD_FORMAT_VERSION,
            bounds: self.bounds(),
            regions: self.regions(),
            cells: self.cells(),
            roots: self.root_overrides(),
            volumes: self.volumes().to_vec(),
        }
        .serialize(serializer)
    }
}
impl<'de> Deserialize<'de> for World {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Parts {
            Legacy(Vec<CellEdit>),
            Current {
                #[serde(default)]
                format_version: u32,
                #[serde(default)]
                bounds: crate::WorldBounds,
                #[serde(default)]
                regions: Vec<crate::CellFragment>,
                #[serde(default)]
                cells: Vec<CellEdit>,
                #[serde(default)]
                roots: Vec<crate::RootEdit>,
                #[serde(default)]
                volumes: Vec<crate::MaterialVolume>,
            },
        }
        match Parts::deserialize(deserializer)? {
            Parts::Legacy(cells) => Self::from_parts(&cells, &[]),
            Parts::Current {
                format_version,
                bounds,
                regions,
                cells,
                roots,
                volumes,
            } => {
                if format_version > crate::WORLD_FORMAT_VERSION {
                    return Err(serde::de::Error::custom("Unsupported world format version"));
                }
                Self::from_compact_parts(bounds, &regions, &cells, &roots).and_then(|mut world| {
                    world.replace_volumes(&volumes)?;
                    Ok(world)
                })
            }
        }
        .map_err(serde::de::Error::custom)
    }
}

fn local_index(pos: CellPos) -> usize {
    let [x, y, z] = pos.map(|v| v.rem_euclid(CHUNK_SIZE) as usize);
    x + CHUNK_SIZE as usize * (y + CHUNK_SIZE as usize * z)
}

impl World {
    pub fn empty() -> Self {
        Self::default()
    }

    /// Outer coordinate safety envelope; use contains_pos for this world's bounds.
    pub fn is_valid_pos(pos: CellPos) -> bool {
        pos.into_iter().all(|v| {
            (-crate::compact::MAX_WORLD_COORDINATE..crate::compact::MAX_WORLD_COORDINATE)
                .contains(&v)
        })
    }

    pub fn cell_chunk(pos: CellPos) -> ChunkKey {
        pos.map(|v| v.div_euclid(CHUNK_SIZE))
    }

    pub fn from_cells(cells: &[CellEdit]) -> Self {
        let mut world = Self::empty();
        for edit in cells {
            world.set(edit.pos, edit.cell);
        }
        world
    }

    pub fn get(&self, pos: CellPos) -> Option<Cell> {
        self.chunks
            .get(&Self::cell_chunk(pos))
            .and_then(|c| c.cells[local_index(pos)])
    }

    pub(crate) fn has_local_geometry(&self) -> bool {
        !self.chunks.is_empty() || !self.fine.is_empty()
    }

    /// Invalid data is ignored, rather than silently deleting an existing cell.
    pub fn set(&mut self, pos: CellPos, cell: Option<Cell>) {
        if !self.contains_pos(pos) || cell.is_some_and(|c| !c.is_valid()) {
            return;
        }
        if self.has_regions() {
            let edit = crate::RootEdit {
                pos,
                fragments: cell
                    .map(|c| vec![crate::CellFragment::from_cell(pos, c)])
                    .unwrap_or_default(),
            };
            let _ = self.apply_root_edits(&[edit]);
            return;
        }
        self.set_storage(pos, cell);
    }

    pub(crate) fn set_storage(&mut self, pos: CellPos, cell: Option<Cell>) {
        self.fine.remove(&pos);
        self.authored_roots.remove(&pos);
        let key = Self::cell_chunk(pos);
        let index = local_index(pos);
        if let Some(cell) = cell {
            let chunk = Arc::make_mut(self.chunks.entry(key).or_default());
            if chunk.cells[index].is_none() {
                chunk.occupied += 1;
            }
            chunk.cells[index] = Some(cell);
        } else if let Some(chunk) = self.chunks.get_mut(&key) {
            let chunk = Arc::make_mut(chunk);
            if chunk.cells[index].take().is_some() {
                chunk.occupied -= 1;
            }
            if chunk.occupied == 0 {
                self.chunks.remove(&key);
            }
        }
    }

    pub fn cells(&self) -> Vec<CellEdit> {
        stored_cells(&self.chunks)
    }

    pub fn snapshot(&self) -> WorldSnapshot {
        WorldSnapshot {
            bounds: self.bounds,
            regions: self.compact.sources.clone(),
            chunks: self.chunks.clone(),
            roots: self.authored_roots.clone(),
            overrides: self.overrides.clone(),
            volumes: self.volumes.authored(),
        }
    }

    pub fn chunk_keys(&self) -> Vec<ChunkKey> {
        self.chunks
            .keys()
            .copied()
            .chain(self.fine.keys().copied().map(Self::cell_chunk))
            .chain(self.compact.batches.keys().copied())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }

    pub fn apply_edits(&mut self, edits: &[CellEdit]) -> Vec<ChunkKey> {
        let roots: Vec<_> = edits
            .iter()
            .filter(|e| self.contains_pos(e.pos) && e.cell.is_none_or(Cell::is_valid))
            .map(|e| crate::RootEdit {
                pos: e.pos,
                fragments: e
                    .cell
                    .map(|c| vec![crate::CellFragment::from_cell(e.pos, c)])
                    .unwrap_or_default(),
            })
            .collect();
        self.apply_root_edits(&roots).unwrap_or_default()
    }

    /// Foundry: roofed perimeter bays around an open service yard. Upper routes
    /// cross lower ones, and the enclosed rooms are actual editable geometry.
    pub fn arena() -> Self {
        let mut w = Self::empty();
        w.fill([-24, 0, -24], [24, 1, 24], 0);

        // Flush drains and short loading-door markings keep the floor legible
        // without turning it into a checkerboard of different block colors.
        w.fill([-1, 0, -23], [0, 1, 23], 1);
        w.fill([-10, 0, -10], [10, 1, -9], 1);
        w.fill([-10, 0, 9], [10, 1, 10], 1);
        for x in [-12, 11] {
            for z in [-13, 10] {
                w.fill([x, 0, z], [x + 1, 1, z + 3], 4);
            }
        }
        for z in [-15, 14] {
            for x in [-8, 3] {
                w.fill([x, 0, z], [x + 5, 1, z + 1], 4);
            }
        }

        // Thick outside walls, with industrial roof caps above the side bays.
        w.fill([-24, 1, -24], [24, 9, -23], 0);
        w.fill([-24, 1, 23], [24, 9, 24], 0);
        w.fill([-24, 1, -23], [-23, 9, 23], 0);
        w.fill([23, 1, -23], [24, 9, 23], 0);
        w.fill([-24, 8, -24], [-10, 9, 24], 1);
        w.fill([10, 8, -24], [24, 9, 24], 1);
        w.fill([-10, 8, -24], [10, 9, -14], 1);
        // The lower south service passage feels different from the tall bays.
        w.fill([-10, 6, 14], [10, 7, 24], 1);
        w.fill([-24, 9, -24], [24, 10, -23], 3);
        for x in [-24, 23] {
            w.fill([x, 9, -24], [x + 1, 10, 24], 1);
        }

        // Three yard portals per side. The central opening is tall enough for
        // both the ground route and the catwalk; solid lintels sit above them.
        for x in [-12, 11] {
            for (a, b) in [(-23, -15), (-9, -4), (4, 9), (15, 23)] {
                w.fill([x, 1, a], [x + 1, 8, b], 0);
            }
            for (a, b) in [(-15, -9), (9, 15)] {
                w.fill([x - 1, 5, a], [x + 2, 8, b], 1);
                w.fill([x - 1, 7, a], [x + 2, 8, a + 1], 2);
            }
            w.fill([x - 1, 8, -4], [x + 2, 10, 4], 1);
        }
        // Paired north/south doors lead through roofed rooms, with real corners
        // to break sightlines rather than relying on fog to hide open ground.
        for z in [-15, 14] {
            let ceiling = if z < 0 { 8 } else { 6 };
            for (a, b) in [(-11, -8), (-3, 3), (8, 11)] {
                w.fill([a, 1, z], [b, ceiling, z + 1], 0);
            }
            for (a, b) in [(-8, -3), (3, 8)] {
                w.fill([a, 5, z - 1], [b, ceiling + 1, z + 2], 1);
            }
        }

        // Elevated loading decks leave walkable service space beneath them.
        for (a, b) in [(-18, -11), (11, 18)] {
            w.fill([a, 4, -8], [b, 5, 8], 1);
            for z in [-8, 7] {
                w.fill([a, 1, z], [a + 1, 4, z + 1], 0);
                w.fill([b - 1, 1, z], [b, 4, z + 1], 0);
            }
        }
        // Broad half-unit stairs climb into the west deck from the north.
        for step in 0..8 {
            let z = -16 + step;
            let level = 1 + step / 2;
            w.fill([-17, 1, z], [-12, level, z + 1], 0);
            for x in -17..-12 {
                w.set(
                    [x, level, z],
                    Some(Cell {
                        material: 1,
                        face_materials: None,
                        heights: [if step % 2 == 0 { 4 } else { 8 }; 4],
                        diagonal: false,
                        shape: None,
                    }),
                );
            }
        }
        // An uninterrupted deformed ramp provides the faster opposite route.
        for step in 0..8 {
            let z = 15 - step;
            let level = 1 + step / 2;
            let low = if step % 2 == 0 { 0 } else { 4 };
            w.fill([13, 1, z], [17, level, z + 1], 0);
            for x in 13..17 {
                w.set(
                    [x, level, z],
                    Some(Cell {
                        material: 1,
                        face_materials: None,
                        heights: [low + 4, low + 4, low, low],
                        diagonal: false,
                        shape: None,
                    }),
                );
            }
        }
        // A broad steel crossing makes a useful upper fight and a sheltered
        // lower passage. The low edge is a toe plate, not a one-meter railing.
        w.fill([-11, 4, -2], [11, 5, 2], 1);
        for x in -11..11 {
            for z in [-2, 1] {
                w.set(
                    [x, 5, z],
                    Some(Cell {
                        material: 2,
                        face_materials: None,
                        heights: [2; 4],
                        diagonal: false,
                        shape: None,
                    }),
                );
            }
        }
        for x in [-8, 7] {
            for z in [-2, 1] {
                w.fill([x, 1, z], [x + 1, 4, z + 1], 0);
            }
        }

        // The old crane frame gives the yard a single strong silhouette.
        for x in [-10, 9] {
            w.fill([x, 1, -8], [x + 1, 11, -6], 1);
        }
        w.fill([-10, 10, -8], [10, 12, -6], 1);
        w.fill([-9, 11, -8], [9, 12, -7], 2);
        w.fill([-2, 8, -8], [2, 10, -6], 2);

        // Machinery plinths and armored cabinets create cover in the yard and
        // inside the buildings. Chamfered tops read as equipment, not gardens.
        for (x, z) in [(-7, -8), (3, 5)] {
            w.fill([x, 1, z], [x + 4, 3, z + 4], 1);
            for sx in x..x + 4 {
                for sz in z..z + 4 {
                    let edge = sz == z || sz == z + 3;
                    w.set(
                        [sx, 3, sz],
                        Some(Cell {
                            material: 2,
                            face_materials: None,
                            heights: if edge { [2, 2, 4, 4] } else { [4; 4] },
                            diagonal: false,
                            shape: None,
                        }),
                    );
                }
            }
            w.fill([x, 1, z + 4], [x + 4, 2, z + 5], 0);
        }
        for z in [-21, 17] {
            w.fill([-2, 1, z], [2, 3, z + 4], 1);
            w.fill([-2, 3, z], [2, 4, z + 1], 2);
        }
        for x in [-23, 20] {
            w.fill([x, 1, -3], [x + 3, 4, 3], 1);
            w.fill([x, 4, -3], [x + 3, 5, 3], 2);
        }
        // Buttresses, beam sockets, and short amber status strips are sparse
        // architectural accents; no dense prop scatter or decorative trees.
        for x in [-23, 22] {
            for z in [-20, -8, 7, 19] {
                w.fill([x, 1, z], [x + 1, 8, z + 1], 1);
            }
        }
        for (x, z) in [(-12, -4), (11, 4), (-3, -15), (3, 14)] {
            w.set(
                [x, 4, z],
                Some(Cell {
                    material: 5,
                    face_materials: None,
                    heights: [1; 4],
                    diagonal: false,
                    shape: None,
                }),
            );
        }
        w
    }

    fn fill(&mut self, min: CellPos, max: CellPos, material: u8) {
        for z in min[2]..max[2] {
            for y in min[1]..max[1] {
                for x in min[0]..max[0] {
                    self.set([x, y, z], Some(Cell::solid(material)));
                }
            }
        }
    }
}

pub fn spawn_point(index: u32) -> Vec3 {
    const POINTS: [[f32; 3]; 8] = [
        [-19.0, 1.01, -12.0],
        [19.0, 1.01, 12.0],
        [-19.0, 1.01, 12.0],
        [19.0, 1.01, -12.0],
        [-6.0, 1.01, -19.0],
        [6.0, 1.01, 19.0],
        [6.0, 1.01, -19.0],
        [-6.0, 1.01, 19.0],
    ];
    Vec3::from_array(POINTS[index as usize % POINTS.len()])
}

pub fn arena_objects() -> Vec<crate::WorldObject> {
    use crate::{ObjectKind, WorldObject};
    let mut objects = Vec::new();
    // Independent light nodes in each enclosed bay. All eight fit the
    // renderer's local-light budget and remain shared editable objects.
    for (i, [x, y, z]) in [
        [-19.0, 6.8, -12.0],
        [-19.0, 6.8, 12.0],
        [19.0, 6.8, -12.0],
        [19.0, 6.8, 12.0],
        [-6.0, 6.8, -19.0],
        [6.0, 6.8, -19.0],
        [-6.0, 4.8, 19.0],
        [6.0, 4.8, 19.0],
    ]
    .into_iter()
    .enumerate()
    {
        objects.push(WorldObject {
            id: i as u32 + 1,
            position: Vec3::new(x, y, z),
            kind: ObjectKind::Light,
            rotation: if i < 4 {
                std::f32::consts::FRAC_PI_2
            } else {
                0.0
            },
        });
    }
    for i in 0..8 {
        objects.push(WorldObject {
            id: i + 9,
            position: spawn_point(i),
            kind: ObjectKind::Spawn,
            rotation: 0.0,
        });
    }
    objects
}

/// A single network edit group at the editor's supported coarse grid sizes.
/// The caller selects the origin; coordinates outside the world are omitted.
pub fn block_edits(origin: CellPos, grid: i32, cell: Option<Cell>) -> Vec<CellEdit> {
    block_edits_in(crate::WorldBounds::default(), origin, grid, cell)
}

pub fn block_edits_in(
    bounds: crate::WorldBounds,
    origin: CellPos,
    grid: i32,
    cell: Option<Cell>,
) -> Vec<CellEdit> {
    if ![1, 2, 4, 8].contains(&grid) || cell.is_some_and(|c| !c.is_valid()) {
        return Vec::new();
    }
    let mut edits = Vec::with_capacity((grid * grid * grid) as usize);
    for z in 0..grid {
        for y in 0..grid {
            for x in 0..grid {
                let pos = [origin[0] + x, origin[1] + y, origin[2] + z];
                if bounds.contains_pos(pos) {
                    edits.push(CellEdit { pos, cell });
                }
            }
        }
    }
    edits
}

fn stored_cells(chunks: &OrdMap<ChunkKey, Arc<Chunk>>) -> Vec<CellEdit> {
    let mut cells = Vec::new();
    for (&key, chunk) in chunks {
        for (index, &cell) in chunk.cells.iter().enumerate() {
            if cell.is_none() {
                continue;
            }
            let x = index as i32 % CHUNK_SIZE;
            let y = index as i32 / CHUNK_SIZE % CHUNK_SIZE;
            let z = index as i32 / (CHUNK_SIZE * CHUNK_SIZE);
            cells.push(CellEdit {
                pos: [
                    key[0] * CHUNK_SIZE + x,
                    key[1] * CHUNK_SIZE + y,
                    key[2] * CHUNK_SIZE + z,
                ],
                cell,
            });
        }
    }
    cells
}

#[cfg(test)]
mod foundry_tests {
    use super::*;
    use crate::{simulate_player, InputFrame, ObjectKind, Player, FIXED_DT};

    #[test]
    fn foundry_has_roofed_rooms_and_an_open_yard() {
        let world = World::arena();
        for index in 0..8 {
            assert!(world.raycast(spawn_point(index), Vec3::Y, 10.0).is_some());
        }
        assert!(world
            .raycast(Vec3::new(0.5, 1.1, 8.5), Vec3::Y, 20.0)
            .is_none());
        let objects = arena_objects();
        assert_eq!(
            objects
                .iter()
                .filter(|o| o.kind == ObjectKind::Light)
                .count(),
            8
        );
        assert!(!objects.iter().any(|o| o.kind == ObjectKind::Tree));
    }

    #[test]
    fn foundry_stairs_ramp_and_catwalk_are_walkable() {
        let world = World::arena();
        let mut stairs = Player::new(1, "stairs".into());
        stairs.position = Vec3::new(-14.5, 1.01, -17.5);
        for seq in 1..=100 {
            simulate_player(
                &world,
                &mut stairs,
                &InputFrame {
                    seq,
                    forward: 1.0,
                    yaw: std::f32::consts::PI,
                    ..Default::default()
                },
                FIXED_DT,
            );
        }
        assert!(
            stairs.position.z > -7.0,
            "stairs route blocked at {:?}",
            stairs.position
        );
        assert!(
            stairs.position.y > 4.99,
            "stairs did not reach deck: {:?}",
            stairs.position
        );

        let mut ramp = Player::new(2, "ramp".into());
        ramp.position = Vec3::new(15.0, 1.01, 16.5);
        for seq in 1..=100 {
            simulate_player(
                &world,
                &mut ramp,
                &InputFrame {
                    seq,
                    forward: 1.0,
                    ..Default::default()
                },
                FIXED_DT,
            );
        }
        assert!(
            ramp.position.z < 7.0,
            "ramp route blocked at {:?}",
            ramp.position
        );
        assert!(
            ramp.position.y > 4.99,
            "ramp did not reach deck: {:?}",
            ramp.position
        );

        let mut crossing = Player::new(3, "catwalk".into());
        crossing.position = Vec3::new(-14.0, 5.01, 0.0);
        for seq in 1..=190 {
            simulate_player(
                &world,
                &mut crossing,
                &InputFrame {
                    seq,
                    strafe: 1.0,
                    ..Default::default()
                },
                FIXED_DT,
            );
        }
        assert!(
            crossing.position.x > 12.0,
            "upper crossing blocked at {:?}",
            crossing.position
        );
        assert!(
            crossing.position.y > 4.99,
            "upper crossing has a gap: {:?}",
            crossing.position
        );
    }
}
