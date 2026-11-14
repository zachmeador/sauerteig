//! A generated world for benchmarks. The same seed and preset give the same
//! world on every system: the generator uses integer arithmetic and a fixed
//! pseudo-random generator, never the system's math library.
//!
//! The world is described as ordered operations, each a box, a heightfield
//! surface, a shaped cube or a cube material, and built by walking the tree
//! once: a node that one operation settles becomes a leaf, and a node that an
//! operation only partly covers splits. Leaves therefore come out at the
//! sizes the scene needs, as in an imported map: large cubes in solid ground
//! and open air, an eighth of a metre where detail is cut.

use crate::{
    Cell, CellFragment, Format3, Leaf, LeafGeometry, NodeBox, ObjectKind, Octree, OctreeBuilder,
    Vec3, WorldBounds, WorldObject, MAT_AIR, MAT_GLASS, MAT_WATER,
};
use serde::Serialize;
use std::collections::BTreeSet;

/// The seed the presets' recorded digests use.
pub const BENCHMARK_SEED: u64 = 0x5eed_0001;

/// How big a generated world is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BenchmarkPreset {
    /// A few thousand leaves; keeps tests fast.
    Test,
    /// About the size of the_cube, an imported map of 306,094 leaves.
    Small,
    /// About the size of nub1, an imported map of 434,233 leaves.
    Nub1,
}

impl BenchmarkPreset {
    pub const ALL: [Self; 3] = [Self::Test, Self::Small, Self::Nub1];

    pub fn name(self) -> &'static str {
        match self {
            Self::Test => "test",
            Self::Small => "small",
            Self::Nub1 => "nub1",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.name() == name)
    }

    fn plan(self) -> Plan {
        match self {
            Self::Test => Plan {
                width_m: 256,
                below_m: 16,
                above_m: 48,
                patch_m: 256,
                grid: 2,
                pitch_m: 32,
                building_m: (12, 16),
                hills: 0,
                lakes: 1,
                lake_radius_m: (8, 12),
                lights: 12,
                spawns: 4,
                windows: 70,
                trim: 10,
                clutter: 10,
                strata: 0,
                niches: 6,
                pits: 4,
                blocks: 6,
                shaped: 6,
                plazas: 25,
            },
            Self::Small => Plan {
                width_m: 1024,
                below_m: 64,
                above_m: 192,
                patch_m: 512,
                grid: 6,
                pitch_m: 32,
                building_m: (16, 24),
                hills: 16,
                lakes: 7,
                lake_radius_m: (32, 46),
                lights: 339,
                spawns: 16,
                windows: 60,
                trim: 45,
                clutter: 150,
                strata: 0,
                niches: 40,
                pits: 40,
                blocks: 60,
                shaped: 80,
                plazas: 22,
            },
            Self::Nub1 => Plan {
                width_m: 2048,
                below_m: 128,
                above_m: 128,
                patch_m: 1024,
                grid: 8,
                pitch_m: 32,
                building_m: (16, 24),
                hills: 0,
                lakes: 7,
                lake_radius_m: (36, 56),
                lights: 155,
                spawns: 16,
                windows: 40,
                trim: 0,
                clutter: 0,
                strata: 0,
                niches: 40,
                pits: 40,
                blocks: 60,
                shaped: 80,
                plazas: 15,
            },
        }
    }

    fn edited_by_default(self) -> bool {
        !matches!(self, Self::Nub1)
    }
}

/// What to generate.
#[derive(Clone, Copy, Debug)]
pub struct BenchmarkOptions {
    pub preset: BenchmarkPreset,
    pub seed: u64,
    /// The edited layer: small cubes cut into large ones, shaped cubes with
    /// mixed face materials and scattered blocks.
    pub edited: bool,
}

impl BenchmarkOptions {
    /// The preset's recorded seed, with its default edited layer: on for the
    /// test and small presets, off for nub1, whose unedited file already fills
    /// the importer's limit.
    pub fn new(preset: BenchmarkPreset) -> Self {
        Self {
            preset,
            seed: BENCHMARK_SEED,
            edited: preset.edited_by_default(),
        }
    }
}

/// A camera for the capture tool, in metres. The importer adds the world's
/// file name.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct BenchmarkPose {
    pub name: String,
    pub eye: [f32; 3],
    pub look_at: [f32; 3],
    pub time: f32,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub edit_storm: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub path: Vec<BenchmarkWaypoint>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub speed: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dwell: Option<usize>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct BenchmarkWaypoint {
    pub eye: [f32; 3],
    pub look_at: [f32; 3],
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub jump: bool,
}

/// A generated world.
#[derive(Clone, Debug)]
pub struct BenchmarkWorld {
    pub bounds: WorldBounds,
    pub tree: Octree,
    pub objects: Vec<WorldObject>,
    pub poses: Vec<BenchmarkPose>,
    /// Water and glass boxes, lights and spawns as the tree and objects
    /// count them, for reports.
    pub features: Features,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Features {
    pub buildings: usize,
    pub rooms: usize,
    pub lakes: usize,
    pub pools: usize,
    pub windows: usize,
    pub lights: usize,
    pub spawns: usize,
}

impl BenchmarkWorld {
    /// The world in format 3's parts as a world file stores them. Like an
    /// imported map, a file holds only regions, in tree order, and no cells:
    /// the server opens such a file several times faster than one that mixes
    /// in cells (which is how the server itself saves a world).
    pub fn format3(&self) -> Format3 {
        let mut parts = self.tree.to_format3();
        let root = self.tree.root_box().min;
        for edit in std::mem::take(&mut parts.cells) {
            if let Some(cell) = edit.cell {
                parts.regions.push(CellFragment::from_cell(edit.pos, cell));
            }
        }
        // The importer writes a height field only for a cube whose corners
        // happen to form one, and the corners of most cubes in a real map
        // are deformed otherwise, so it writes their shapes explicitly. The
        // server builds those several times slower than heights, so the file
        // writes nine of every twenty shaped cubes that way, picked by position.
        for region in &mut parts.regions {
            let at = region.origin;
            let pick = mix(u64::from(at[0] as u32) << 40
                ^ u64::from(at[1] as u32) << 20
                ^ u64::from(at[2] as u32));
            if region.cell.shape.is_none() && region.cell.heights != [8; 4] && pick % 20 >= 11 {
                let shape = region.cell.as_shape();
                region.cell = Cell {
                    shape: Some(shape),
                    heights: [8; 4],
                    diagonal: false,
                    ..region.cell
                };
            }
        }
        let key = |r: &CellFragment| {
            let d = std::array::from_fn::<u64, 3, _>(|a| (r.bounds.min[a] - root[a]) as u64);
            let mut morton = 0u64;
            for bit in 0..20 {
                for (a, v) in d.iter().enumerate() {
                    morton |= (v >> bit & 1) << (3 * bit + a);
                }
            }
            (morton, std::cmp::Reverse(r.size))
        };
        parts.regions.sort_by_key(key);
        parts
    }

    /// A hash of the objects and the poses, to go with the tree's digest.
    pub fn extras_digest(&self) -> u64 {
        let mut h = Fnv::new();
        for o in &self.objects {
            h.u32(o.id);
            h.u32(o.kind as u32);
            for v in o.position.to_array() {
                h.u32(v.to_bits());
            }
            h.u32(o.rotation.to_bits());
        }
        for p in &self.poses {
            h.bytes(p.name.as_bytes());
            for v in p.eye.iter().chain(&p.look_at) {
                h.u32(v.to_bits());
            }
            for w in &p.path {
                for v in w.eye.iter().chain(&w.look_at) {
                    h.u32(v.to_bits());
                }
                h.u32(u32::from(w.jump));
            }
        }
        h.0
    }
}

struct Fnv(u64);
impl Fnv {
    fn new() -> Self {
        Self(0xcbf2_9ce4_8422_2325)
    }
    fn bytes(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 = (self.0 ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    fn u32(&mut self, v: u32) {
        self.bytes(&v.to_le_bytes());
    }
}

/// Generates a world. Nothing here depends on the system's floating-point
/// math, so equal options give equal worlds everywhere.
pub fn generate_benchmark_world(options: BenchmarkOptions) -> Result<BenchmarkWorld, String> {
    Generator::new(options).run()
}

// ---------------------------------------------------------------------------
// Numbers: a fixed pseudo-random generator and value noise.

fn mix(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9e37_79b9_7f4a_7c15);
    x = (x ^ (x >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    x ^ (x >> 31)
}

/// SplitMix64.
struct Rng(u64);

impl Rng {
    fn new(seed: u64, tag: u64, a: i32, b: i32) -> Self {
        let s = mix(seed ^ mix(tag) ^ mix(u64::from(a as u32) << 32 | u64::from(b as u32)));
        Self(s)
    }
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }
    /// 0 to n - 1.
    fn below(&mut self, n: u32) -> u32 {
        (((self.next() >> 32) * u64::from(n)) >> 32) as u32
    }
    /// lo to hi, inclusive.
    fn range(&mut self, lo: i32, hi: i32) -> i32 {
        lo + self.below((hi - lo + 1) as u32) as i32
    }
    fn chance(&mut self, percent: u32) -> bool {
        self.below(100) < percent
    }
    fn pick<T: Copy>(&mut self, items: &[T]) -> T {
        items[self.below(items.len() as u32) as usize]
    }
}

fn lattice_value(seed: u64, i: i32, j: i32) -> i64 {
    let h = mix(seed ^ mix(u64::from(i as u32) << 32 | u64::from(j as u32)));
    ((h >> 40) & 0x1ff) as i64 - 256
}

/// Smooth value noise in -256 to 256 with the given wavelength in metres.
fn noise(seed: u64, x: i32, z: i32, wavelength: i32) -> i64 {
    let (i, j) = (x.div_euclid(wavelength), z.div_euclid(wavelength));
    let fx = i64::from(x.rem_euclid(wavelength)) * 256 / i64::from(wavelength);
    let fz = i64::from(z.rem_euclid(wavelength)) * 256 / i64::from(wavelength);
    let smooth = |t: i64| (t * t * (768 - 2 * t)) >> 16;
    let (sx, sz) = (smooth(fx), smooth(fz));
    let a = lattice_value(seed, i, j);
    let b = lattice_value(seed, i + 1, j);
    let c = lattice_value(seed, i, j + 1);
    let d = lattice_value(seed, i + 1, j + 1);
    let top = a + (b - a) * sx / 256;
    let bottom = c + (d - c) * sx / 256;
    top + (bottom - top) * sz / 256
}

// ---------------------------------------------------------------------------
// The scene: ordered operations over the world's eighth-metre coordinates,
// with x and z centred on the world and y zero at the ground.

const M: i32 = 8;

struct Plan {
    width_m: i32,
    below_m: i32,
    above_m: i32,
    patch_m: i32,
    grid: i32,
    pitch_m: i32,
    building_m: (i32, i32),
    hills: i32,
    lakes: usize,
    lake_radius_m: (i32, i32),
    lights: usize,
    spawns: usize,
    /// Percent of outer-wall spans that get a window.
    windows: u32,
    /// Percent of rooms with trim along their walls.
    trim: u32,
    /// Crates per hundred square metres of room floor.
    clutter: i32,
    /// Layers of mixed ground under the play area.
    strata: i32,
    /// Edited layer, per building.
    niches: u32,
    pits: u32,
    blocks: u32,
    shaped: u32,
    /// Percent of plots that are plazas.
    plazas: u32,
}

#[derive(Clone, Copy)]
struct BBox {
    min: [i32; 3],
    max: [i32; 3],
}

impl BBox {
    fn of(at: NodeBox) -> Self {
        let s = at.size as i32;
        Self {
            min: at.min,
            max: at.min.map(|v| v + s),
        }
    }
    fn overlaps(&self, at: &NodeBox) -> bool {
        let s = at.size as i32;
        (0..3).all(|a| self.min[a] < at.min[a] + s && at.min[a] < self.max[a])
    }
    fn covers(&self, at: &NodeBox) -> bool {
        let s = at.size as i32;
        (0..3).all(|a| self.min[a] <= at.min[a] && at.min[a] + s <= self.max[a])
    }
}

enum GeoKind {
    /// Everything in the box becomes this leaf.
    Box(Leaf),
    Surface(Box<Surface>),
    /// One node, exactly.
    Stamp(Leaf),
}

struct GeoOp {
    bb: BBox,
    kind: GeoKind,
}

struct MatOp {
    bb: BBox,
    material: u16,
}

enum Cover {
    Disjoint,
    Partial,
    Covers(Leaf),
}

/// A heightfield over a rectangle of one-metre cells: solid below the
/// surface and empty above it, between two heights. Every cell's four
/// corners lie in one metre band, so a cell is one shaped cube.
struct Surface {
    x0: i32,
    z0: i32,
    nx: i32,
    nz: i32,
    y_lo: i32,
    y_hi: i32,
    verts: Vec<i32>,
    faces: [u8; 6],
    /// Per level, the lowest and highest corner of blocks of 2^level cells.
    levels: Vec<Level>,
}

struct Level {
    w: i32,
    min: Vec<i32>,
    max: Vec<i32>,
}

impl Surface {
    fn new(
        origin: [i32; 2],
        cells: [i32; 2],
        y_range: [i32; 2],
        verts: Vec<i32>,
        faces: [u8; 6],
    ) -> Result<Self, String> {
        let (nx, nz) = (cells[0], cells[1]);
        if verts.len() != ((nx + 1) * (nz + 1)) as usize {
            return Err("surface vertices do not match its cells".into());
        }
        let vw = nx + 1;
        let v = |i: i32, j: i32| verts[(j * vw + i) as usize];
        let mut min = Vec::new();
        let mut max = Vec::new();
        for j in 0..nz {
            for i in 0..nx {
                let c = [v(i, j), v(i + 1, j), v(i, j + 1), v(i + 1, j + 1)];
                let (lo, hi) = (c.into_iter().min().unwrap(), c.into_iter().max().unwrap());
                if hi > (lo.div_euclid(M) + 1) * M {
                    return Err("a surface cell spans more than one metre band".into());
                }
                min.push(lo);
                max.push(hi);
            }
        }
        let mut levels = vec![Level { w: nx, min, max }];
        loop {
            let last = levels.last().unwrap();
            let h = (last.min.len() as i32) / last.w;
            if last.w < 2 || h < 2 {
                break;
            }
            let (w, h) = (last.w / 2, h / 2);
            let mut lo = Vec::with_capacity((w * h) as usize);
            let mut hi = Vec::with_capacity((w * h) as usize);
            for j in 0..h {
                for i in 0..w {
                    let at = |di: i32, dj: i32| ((2 * j + dj) * last.w + 2 * i + di) as usize;
                    let cells = [at(0, 0), at(1, 0), at(0, 1), at(1, 1)];
                    lo.push(cells.iter().map(|&k| last.min[k]).min().unwrap());
                    hi.push(cells.iter().map(|&k| last.max[k]).max().unwrap());
                }
            }
            levels.push(Level {
                w,
                min: lo,
                max: hi,
            });
        }
        Ok(Self {
            x0: origin[0],
            z0: origin[1],
            nx,
            nz,
            y_lo: y_range[0],
            y_hi: y_range[1],
            verts,
            faces,
            levels,
        })
    }

    fn bbox(&self) -> BBox {
        BBox {
            min: [self.x0, self.y_lo, self.z0],
            max: [self.x0 + self.nx * M, self.y_hi, self.z0 + self.nz * M],
        }
    }

    fn vert(&self, i: i32, j: i32) -> i32 {
        self.verts[(j * (self.nx + 1) + i) as usize]
    }

    fn range(&self, ci: i32, cj: i32, n: i32) -> (i32, i32) {
        let level = n.trailing_zeros() as usize;
        if ci % n == 0 && cj % n == 0 && level < self.levels.len() {
            let l = &self.levels[level];
            let k = ((cj / n) * (self.levels[0].w >> level) + ci / n) as usize;
            if (self.levels[0].w >> level) > 0 && k < l.min.len() {
                return (l.min[k], l.max[k]);
            }
        }
        let (mut lo, mut hi) = (i32::MAX, i32::MIN);
        for j in cj..cj + n {
            for i in ci..ci + n {
                let k = (j * self.levels[0].w + i) as usize;
                lo = lo.min(self.levels[0].min[k]);
                hi = hi.max(self.levels[0].max[k]);
            }
        }
        (lo, hi)
    }

    fn cell_leaf(&self, ci: i32, cj: i32, y: i32) -> Leaf {
        let h = |i: i32, j: i32| (self.vert(i, j) - y).clamp(0, M) as u8;
        let heights = [h(ci, cj), h(ci + 1, cj), h(ci + 1, cj + 1), h(ci, cj + 1)];
        if heights == [8; 4] {
            return Leaf::full(self.faces);
        }
        if heights == [0; 4] {
            return Leaf::empty(MAT_AIR);
        }
        let cell = Cell {
            material: 0,
            face_materials: None,
            heights,
            diagonal: u16::from(heights[0]) + u16::from(heights[2])
                < u16::from(heights[1]) + u16::from(heights[3]),
            shape: None,
        };
        Leaf {
            geometry: LeafGeometry::Shaped(cell.as_shape()),
            faces: self.faces,
            cube_material: MAT_AIR,
        }
    }

    fn cover(&self, at: &NodeBox) -> Result<Cover, String> {
        let bb = self.bbox();
        if !bb.overlaps(at) {
            return Ok(Cover::Disjoint);
        }
        if !bb.covers(at) {
            return Ok(Cover::Partial);
        }
        let s = at.size as i32;
        let (x, y, z) = (at.min[0], at.min[1], at.min[2]);
        if s >= M && ((x - self.x0) % M != 0 || (z - self.z0) % M != 0) {
            return Err("a surface is not on the tree's grid".into());
        }
        let (ci, cj) = ((x - self.x0).div_euclid(M), (z - self.z0).div_euclid(M));
        if s >= M {
            let n = s / M;
            let (lo, hi) = self.range(ci, cj, n);
            if y + s <= lo {
                return Ok(Cover::Covers(Leaf::full(self.faces)));
            }
            if y >= hi {
                return Ok(Cover::Covers(Leaf::empty(MAT_AIR)));
            }
            if s == M {
                return Ok(Cover::Covers(self.cell_leaf(ci, cj, y)));
            }
            return Ok(Cover::Partial);
        }
        let k = (cj * self.nx + ci) as usize;
        let (lo, hi) = (self.levels[0].min[k], self.levels[0].max[k]);
        if lo != hi {
            return Err("a later operation cuts into a sloped cell".into());
        }
        if y + s <= lo {
            Ok(Cover::Covers(Leaf::full(self.faces)))
        } else if y >= lo {
            Ok(Cover::Covers(Leaf::empty(MAT_AIR)))
        } else {
            Ok(Cover::Partial)
        }
    }
}

/// A point of interest, for lights and poses.
#[derive(Clone)]
struct RoomSpot {
    /// Centre of the room's floor, in eighths.
    centre: [i32; 3],
    /// A floor corner a metre inside the room, in eighths.
    corner: [i32; 3],
    lit: bool,
    storey: i32,
}

struct Generator {
    options: BenchmarkOptions,
    plan: Plan,
    root_min: [i32; 3],
    geo: Vec<GeoOp>,
    mat: Vec<MatOp>,
    stamps: BTreeSet<[i32; 3]>,
    rooms: Vec<RoomSpot>,
    outdoor_lights: Vec<[i32; 3]>,
    extra_lights: Vec<[i32; 3]>,
    spawn_spots: Vec<[i32; 3]>,
    windows: Vec<([i32; 3], [i32; 3])>,
    lakes: Vec<([i32; 2], i32)>,
    ramps: Vec<([i32; 3], [i32; 3])>,
    features: Features,
}

const CONCRETE: u8 = 0;
const GUNMETAL: u8 = 1;
const RUSTED: u8 = 2;
const WORN: u8 = 3;
const HAZARD: u8 = 4;
const AMBER: u8 = 5;
const ROCK: u8 = 6;
const EARTH: u8 = 7;

/// A building's footprint in whole metres.
#[derive(Clone, Copy)]
struct Plot {
    /// The building or plaza.
    rect: [i32; 4],
    /// The district's flat plot around it.
    cell: [i32; 4],
    plaza: bool,
}

impl Generator {
    fn new(options: BenchmarkOptions) -> Self {
        let plan = options.preset.plan();
        let root_min = [
            -plan.width_m * M / 2,
            -plan.below_m * M,
            -plan.width_m * M / 2,
        ];
        Self {
            options,
            plan,
            root_min,
            geo: Vec::new(),
            mat: Vec::new(),
            stamps: BTreeSet::new(),
            rooms: Vec::new(),
            outdoor_lights: Vec::new(),
            extra_lights: Vec::new(),
            spawn_spots: Vec::new(),
            windows: Vec::new(),
            lakes: Vec::new(),
            ramps: Vec::new(),
            features: Features::default(),
        }
    }

    fn bounds(&self) -> WorldBounds {
        let w = self.plan.width_m / 2;
        WorldBounds {
            min: [-w, -self.plan.below_m, -w],
            max: [w, self.plan.above_m, w],
        }
    }

    fn solid(&mut self, min: [i32; 3], max: [i32; 3], faces: [u8; 6]) {
        self.geo.push(GeoOp {
            bb: BBox { min, max },
            kind: GeoKind::Box(Leaf::full(faces)),
        });
    }

    fn air(&mut self, min: [i32; 3], max: [i32; 3]) {
        self.geo.push(GeoOp {
            bb: BBox { min, max },
            kind: GeoKind::Box(Leaf::empty(MAT_AIR)),
        });
    }

    fn material(&mut self, min: [i32; 3], max: [i32; 3], material: u16) {
        self.mat.push(MatOp {
            bb: BBox { min, max },
            material,
        });
    }

    fn surface(&mut self, surface: Surface) {
        let bb = surface.bbox();
        self.geo.push(GeoOp {
            bb,
            kind: GeoKind::Surface(Box::new(surface)),
        });
    }

    /// A single shaped or full cube, if its metre is free of other stamps.
    fn stamp(&mut self, at: NodeBox, leaf: Leaf) -> bool {
        let key = at.min.map(|v| v.div_euclid(M));
        if !self.stamps.insert(key) {
            return false;
        }
        self.geo.push(GeoOp {
            bb: BBox::of(at),
            kind: GeoKind::Stamp(leaf),
        });
        true
    }

    fn run(mut self) -> Result<BenchmarkWorld, String> {
        let seed = self.options.seed;
        let bounds = self.bounds();
        // The ground: rock below, and a heightfield of earth over the middle.
        let w = self.plan.width_m * M / 2;
        let bottom = -self.plan.below_m * M;
        self.solid([-w, bottom, -w], [w, 0, w], [ROCK; 6]);
        let plots = self.plots();
        self.terrain(&plots)?;
        self.strata();
        for (index, plot) in plots.iter().enumerate() {
            let (i, j) = (index as i32 % self.plan.grid, index as i32 / self.plan.grid);
            let mut rng = Rng::new(seed, 1, i, j);
            if plot.plaza {
                self.plaza(&mut rng, plot.rect);
            } else {
                self.building(&mut rng, plot.rect);
            }
        }
        for (centre, radius) in self.lakes.clone() {
            self.lake_water(centre, radius);
        }
        if self.options.edited {
            for (index, plot) in plots.iter().enumerate() {
                let (i, j) = (index as i32 % self.plan.grid, index as i32 / self.plan.grid);
                let mut rng = Rng::new(seed, 2, i, j);
                self.edits(&mut rng, *plot);
            }
        }
        let tree = self.build_tree(bounds)?;
        let (objects, poses) = self.objects_and_poses(&plots);
        self.features.lights = objects
            .iter()
            .filter(|o| o.kind == ObjectKind::Light)
            .count();
        self.features.spawns = objects
            .iter()
            .filter(|o| o.kind == ObjectKind::Spawn)
            .count();
        Ok(BenchmarkWorld {
            bounds,
            tree,
            objects,
            poses,
            features: self.features,
        })
    }

    fn plots(&self) -> Vec<Plot> {
        let p = &self.plan;
        let mut plots = Vec::new();
        let half = p.pitch_m / 2 - 4;
        for j in 0..p.grid {
            for i in 0..p.grid {
                let mut rng = Rng::new(self.options.seed, 0, i, j);
                let cx = (2 * i - (p.grid - 1)) * p.pitch_m / 2;
                let cz = (2 * j - (p.grid - 1)) * p.pitch_m / 2;
                let plaza = rng.chance(p.plazas);
                let span = 2 * half;
                let bx = if plaza {
                    span - 2
                } else {
                    rng.range(p.building_m.0.min(span - 2), p.building_m.1.min(span - 2))
                };
                let bz = if plaza {
                    span - 2
                } else {
                    rng.range(p.building_m.0.min(span - 2), p.building_m.1.min(span - 2))
                };
                let x0 = cx - half + 1 + rng.range(0, span - 2 - bx);
                let z0 = cz - half + 1 + rng.range(0, span - 2 - bz);
                plots.push(Plot {
                    rect: [x0, z0, x0 + bx, z0 + bz],
                    cell: [cx - half, cz - half, cx + half, cz + half],
                    plaza,
                });
            }
        }
        plots
    }

    // -- Terrain -----------------------------------------------------------

    fn terrain(&mut self, plots: &[Plot]) -> Result<(), String> {
        let seed = self.options.seed;
        let s = self.plan.patch_m;
        let half = s / 2;
        // Lakes sit in the ring around the plots.
        let reach = self.plan.grid * self.plan.pitch_m / 2;
        let mut rng = Rng::new(seed, 3, 0, 0);
        let mut lakes: Vec<([i32; 2], i32)> = Vec::new();
        let mut tries = 0;
        while lakes.len() < self.plan.lakes && tries < 500 {
            tries += 1;
            let r = rng.range(self.plan.lake_radius_m.0, self.plan.lake_radius_m.1);
            let limit = half - r - 24;
            if limit <= reach {
                break;
            }
            let c = [rng.range(-limit, limit), rng.range(-limit, limit)];
            let from_plots = c[0].abs().max(c[1].abs()) - r;
            let apart = lakes
                .iter()
                .all(|(o, ro)| (o[0] - c[0]).abs().max((o[1] - c[1]).abs()) > r + ro + 16);
            if from_plots > reach + 8 && apart {
                lakes.push((c, r));
            }
        }
        let depth = 64;
        let hills = self.plan.hills;
        let vw = s + 1;
        let mut verts = vec![0i32; (vw * vw) as usize];
        for vj in 0..vw {
            for vi in 0..vw {
                let (x, z) = (vi - half, vj - half);
                let mut flat = i32::MAX;
                for plot in plots {
                    let r = plot.cell;
                    let dx = (r[0] - 4 - x).max(x - (r[2] + 4)).max(0);
                    let dz = (r[1] - 4 - z).max(z - (r[3] + 4)).max(0);
                    flat = flat.min(dx.max(dz));
                }
                let mut basin = 0i64;
                for &(c, r) in &lakes {
                    let dx = (c[0] - x).abs();
                    let dz = (c[1] - z).abs();
                    flat = flat.min((dx.max(dz) - r - 4).max(0));
                    let d2 = i64::from(dx * dx + dz * dz);
                    let r2 = i64::from(r * r);
                    if d2 < r2 {
                        basin += i64::from(depth) * (r2 - d2) * 256 / r2;
                    }
                }
                let weight = (i64::from(flat) * 256 / 10).clamp(0, 256);
                let edge = (vi.min(s - vi)).min(vj.min(s - vj));
                let rim = (i64::from(edge) * 256 / 24).clamp(0, 256);
                // Hills stand where the broad noise is high; elsewhere the
                // ground stays flat.
                let rise = (noise(seed ^ 11, x, z, 56) - 128).max(0);
                let h = (rise * i64::from(hills)
                    + rise * noise(seed ^ 12, x, z, 9) * i64::from(hills) / 512)
                    / 128;
                let bed = if basin > 0 {
                    noise(seed ^ 13, x, z, 7) * 24 / 256 * basin.min(1024) / 1024
                } else {
                    0
                };
                let t = h * weight / 256 * rim / 256 - basin / 256 + bed;
                verts[(vj * vw + vi) as usize] = t as i32;
            }
        }
        // Lower corners until every cell fits one metre band.
        loop {
            let mut changed = false;
            for j in 0..s {
                for i in 0..s {
                    let at = |di: i32, dj: i32| ((j + dj) * vw + i + di) as usize;
                    let ks = [at(0, 0), at(1, 0), at(0, 1), at(1, 1)];
                    let lo = ks.iter().map(|&k| verts[k]).min().unwrap();
                    let top = (lo.div_euclid(M) + 1) * M;
                    for k in ks {
                        if verts[k] > top {
                            verts[k] = top;
                            changed = true;
                        }
                    }
                }
            }
            if !changed {
                break;
            }
        }
        let surface = Surface::new(
            [-half * M, -half * M],
            [s, s],
            [-self.plan.below_m * M, self.plan.above_m * M],
            verts,
            [ROCK, ROCK, ROCK, EARTH, ROCK, ROCK],
        )?;
        self.surface(surface);
        self.lakes = lakes;
        self.features.lakes = self.lakes.len();
        Ok(())
    }

    /// Layers of ground in cells of different materials under the play area,
    /// as a map textured cube by cube has: leaves that no face shows.
    fn strata(&mut self) {
        let reach = self.plan.grid * self.plan.pitch_m / 2 + 4;
        let mut rng = Rng::new(self.options.seed, 5, 0, 0);
        for layer in 0..self.plan.strata {
            let y = -M * (2 + layer);
            for z in -reach..reach {
                for x in -reach..reach {
                    let m = rng.pick(&[ROCK, EARTH, CONCRETE, WORN, RUSTED, GUNMETAL]);
                    self.solid([x * M, y, z * M], [(x + 1) * M, y + M, (z + 1) * M], [m; 6]);
                }
            }
        }
    }

    fn lake_water(&mut self, centre: [i32; 2], radius: i32) {
        let d = 64 + 8;
        self.material(
            [(centre[0] - radius) * M, -d - 8, (centre[1] - radius) * M],
            [(centre[0] + radius) * M, -8, (centre[1] + radius) * M],
            MAT_WATER,
        );
    }

    // -- Buildings ---------------------------------------------------------

    fn palette(rng: &mut Rng) -> [u8; 4] {
        let walls = rng.pick(&[CONCRETE, WORN, GUNMETAL, RUSTED]);
        let floors = rng.pick(&[GUNMETAL, WORN, CONCRETE]);
        let ceilings = rng.pick(&[CONCRETE, WORN]);
        let trim = rng.pick(&[HAZARD, AMBER, RUSTED]);
        [walls, floors, ceilings, trim]
    }

    fn building(&mut self, rng: &mut Rng, rect: [i32; 4]) {
        self.features.buildings += 1;
        let fp = rect.map(|v| v * M);
        let floors = rng.pick(&[1, 1, 2, 2, 3]);
        let [walls, floor_m, ceil_m, trim_m] = Self::palette(rng);
        let top = 40 * floors;
        // The shell, then the rooms cut out of it storey by storey.
        self.solid(
            [fp[0], 0, fp[1]],
            [fp[2], top, fp[3]],
            [walls, walls, floor_m, ceil_m, walls, walls],
        );
        let inner = [fp[0] + 8, fp[1] + 8, fp[2] - 8, fp[3] - 8];
        let mut rooms = Vec::new();
        Self::split(rng, inner, 0, &mut rooms);
        for storey in 0..floors {
            let y = 40 * storey;
            for room in &rooms {
                self.air([room.x0, y, room.z0], [room.x1, y + 36, room.z1]);
                self.features.rooms += 1;
                let cx = (room.x0 + room.x1) / 2 / 4 * 4;
                let cz = (room.z0 + room.z1) / 2 / 4 * 4;
                if (room.x1 - room.x0).max(room.z1 - room.z0) >= 64 {
                    for f in [1, 3] {
                        let (qx, qz) = if room.x1 - room.x0 >= room.z1 - room.z0 {
                            ((room.x0 + (room.x1 - room.x0) * f / 4) / 4 * 4, cz)
                        } else {
                            (cx, (room.z0 + (room.z1 - room.z0) * f / 4) / 4 * 4)
                        };
                        self.extra_lights.push([qx, y + 30, qz]);
                    }
                }
                self.rooms.push(RoomSpot {
                    centre: [cx, y, cz],
                    corner: [room.x0 + 8, y, room.z0 + 8],
                    lit: false,
                    storey,
                });
            }
            self.doors(rng, &rooms, y);
        }
        // Entrances on the ground floor.
        for (n, room) in rooms.iter().enumerate() {
            let sides = [
                (room.x0 == inner[0], 0usize, true),
                (room.x1 == inner[2], 0, false),
                (room.z0 == inner[1], 1, true),
                (room.z1 == inner[3], 1, false),
            ];
            for (touches, axis, low) in sides {
                if touches && rng.chance(if n == 0 { 100 } else { 18 }) {
                    self.entrance(rng, &fp, room, axis, low);
                }
            }
        }
        // Windows and skylights.
        for storey in 0..floors {
            for room in &rooms {
                self.windows_for(rng, &fp, &inner, room, 40 * storey);
            }
        }
        // Furniture, trim and pools.
        for room in &rooms {
            for storey in 0..floors {
                self.clutter(rng, room, 40 * storey);
            }
            let (w, d) = (room.x1 - room.x0, room.z1 - room.z0);
            if w >= 64 && d >= 64 && rng.chance(60) {
                self.pillars(room, 0, walls, trim_m);
            }
            if rng.chance(self.plan.trim) {
                self.trim(room, 0, trim_m);
            }
            if w >= 72 && d >= 72 && rng.chance(25) {
                self.pool(rng, *room);
            }
        }
        // The roof.
        let roof = rng.below(100);
        if roof < 45 {
            if let Some(room) = rooms.iter().max_by_key(|r| (r.x1 - r.x0) * (r.z1 - r.z0)) {
                if rng.chance(70) {
                    self.skylight(room, top);
                }
            }
        } else if roof < 85 {
            self.roof(&fp, top, 1, [WORN, WORN, WORN, RUSTED, WORN, WORN]);
        } else {
            self.roof(&fp, top, 2, [WORN, WORN, WORN, RUSTED, WORN, WORN]);
        }
        // Stairs between storeys go last: nothing may cut into their sloped
        // cells afterwards.
        let long_rooms: Vec<Rect> = rooms
            .iter()
            .copied()
            .filter(|r| {
                (r.x1 - r.x0).max(r.z1 - r.z0) >= 96 && (r.x1 - r.x0).min(r.z1 - r.z0) >= 32
            })
            .collect();
        if !long_rooms.is_empty() {
            for storey in 0..floors - 1 {
                let room = rng.pick(&long_rooms);
                self.stair(room, 40 * storey);
            }
        }
    }

    fn skylight(&mut self, room: &Rect, top: i32) {
        let (w, d) = (
            (room.x1 - room.x0 - 16).min(64) / 8 * 8,
            (room.z1 - room.z0 - 16).min(64) / 8 * 8,
        );
        if w < 16 || d < 16 {
            return;
        }
        let x = (room.x0 + room.x1 - w) / 2 / 8 * 8;
        let z = (room.z0 + room.z1 - d) / 2 / 8 * 8;
        self.air([x, top - 4, z], [x + w, top, z + d]);
        self.material([x, top - 4, z], [x + w, top, z + d], MAT_GLASS);
    }

    fn split(rng: &mut Rng, rect: [i32; 4], depth: u32, out: &mut Vec<Rect>) {
        let (w, d) = (rect[2] - rect[0], rect[3] - rect[1]);
        let min = 32;
        let long_x = w >= d;
        let long = if long_x { w } else { d };
        let stop = depth >= 4 || long < 2 * min + 4 || (depth > 0 && rng.chance(28));
        if stop {
            out.push(Rect {
                x0: rect[0],
                z0: rect[1],
                x1: rect[2],
                z1: rect[3],
            });
            return;
        }
        let lo = min;
        let hi = long - min - 4;
        let at = (rng.range(lo, hi) / 4) * 4;
        let (a, b) = if long_x {
            (
                [rect[0], rect[1], rect[0] + at, rect[3]],
                [rect[0] + at + 4, rect[1], rect[2], rect[3]],
            )
        } else {
            (
                [rect[0], rect[1], rect[2], rect[1] + at],
                [rect[0], rect[1] + at + 4, rect[2], rect[3]],
            )
        };
        Self::split(rng, a, depth + 1, out);
        Self::split(rng, b, depth + 1, out);
    }

    /// Doors through the walls between adjacent rooms until every room
    /// connects, then a few more.
    fn doors(&mut self, rng: &mut Rng, rooms: &[Rect], y: i32) {
        let mut pairs = Vec::new();
        for (a, ra) in rooms.iter().enumerate() {
            for (b, rb) in rooms.iter().enumerate().skip(a + 1) {
                for (first, second) in [(ra, rb), (rb, ra)] {
                    if first.x1 + 4 == second.x0 {
                        let (lo, hi) = (first.z0.max(second.z0), first.z1.min(second.z1));
                        if hi - lo >= 28 {
                            pairs.push((a, b, 0usize, first.x1, lo, hi));
                        }
                    }
                    if first.z1 + 4 == second.z0 {
                        let (lo, hi) = (first.x0.max(second.x0), first.x1.min(second.x1));
                        if hi - lo >= 28 {
                            pairs.push((a, b, 1usize, first.z1, lo, hi));
                        }
                    }
                }
            }
        }
        let mut parent: Vec<usize> = (0..rooms.len()).collect();
        fn find(parent: &mut [usize], mut i: usize) -> usize {
            while parent[i] != i {
                parent[i] = parent[parent[i]];
                i = parent[i];
            }
            i
        }
        for k in (1..pairs.len()).rev() {
            let j = rng.below(k as u32 + 1) as usize;
            pairs.swap(k, j);
        }
        for (a, b, axis, wall, lo, hi) in pairs {
            let (ra, rb) = (find(&mut parent, a), find(&mut parent, b));
            if ra == rb && !rng.chance(20) {
                continue;
            }
            parent[ra] = rb;
            let mid = ((lo + hi) / 2 / 4) * 4;
            let (min, max) = if axis == 0 {
                ([wall, y, mid - 8], [wall + 4, y + 24, mid + 8])
            } else {
                ([mid - 8, y, wall], [mid + 8, y + 24, wall + 4])
            };
            self.air(min, max);
        }
    }

    fn entrance(&mut self, rng: &mut Rng, fp: &[i32; 4], room: &Rect, axis: usize, low: bool) {
        let (lo, hi) = if axis == 0 {
            (room.z0, room.z1)
        } else {
            (room.x0, room.x1)
        };
        if hi - lo < 28 {
            return;
        }
        let mid = ((rng.range(lo + 12, hi - 12)) / 4) * 4;
        let (min, max) = match (axis, low) {
            (0, true) => ([fp[0], 0, mid - 8], [fp[0] + 8, 24, mid + 8]),
            (0, false) => ([fp[2] - 8, 0, mid - 8], [fp[2], 24, mid + 8]),
            (_, true) => ([mid - 8, 0, fp[1]], [mid + 8, 24, fp[1] + 8]),
            _ => ([mid - 8, 0, fp[3] - 8], [mid + 8, 24, fp[3]]),
        };
        self.air(min, max);
        let outside = match (axis, low) {
            (0, true) => [fp[0] - 16, 8, mid],
            (0, false) => [fp[2] + 16, 8, mid],
            (_, true) => [mid, 8, fp[1] - 16],
            _ => [mid, 8, fp[3] + 16],
        };
        if self.spawn_spots.len() < 64 {
            self.spawn_spots.push([outside[0], 0, outside[2]]);
        }
    }

    fn windows_for(&mut self, rng: &mut Rng, fp: &[i32; 4], inner: &[i32; 4], room: &Rect, y: i32) {
        // Skylight on the top is added by the roof; windows go in the walls.
        let sides = [
            (room.x0 == inner[0], 0usize, true),
            (room.x1 == inner[2], 0, false),
            (room.z0 == inner[1], 1, true),
            (room.z1 == inner[3], 1, false),
        ];
        for (touches, axis, low) in sides {
            if !touches {
                continue;
            }
            let (lo, hi) = if axis == 0 {
                (room.z0, room.z1)
            } else {
                (room.x0, room.x1)
            };
            if hi - lo < 56 || !rng.chance(self.plan.windows) {
                continue;
            }
            // One ribbon window along the room's outer wall.
            let w0 = lo + 16;
            let w1 = hi - 16;
            let (y0, y1) = (y + 10, y + 28);
            let (min, max, pane_min, pane_max) = match (axis, low) {
                (0, true) => (
                    [fp[0], y0, w0],
                    [fp[0] + 8, y1, w1],
                    [fp[0], y0, w0],
                    [fp[0] + 2, y1, w1],
                ),
                (0, false) => (
                    [fp[2] - 8, y0, w0],
                    [fp[2], y1, w1],
                    [fp[2] - 2, y0, w0],
                    [fp[2], y1, w1],
                ),
                (_, true) => (
                    [w0, y0, fp[1]],
                    [w1, y1, fp[1] + 8],
                    [w0, y0, fp[1]],
                    [w1, y1, fp[1] + 2],
                ),
                _ => (
                    [w0, y0, fp[3] - 8],
                    [w1, y1, fp[3]],
                    [w0, y0, fp[3] - 2],
                    [w1, y1, fp[3]],
                ),
            };
            self.air(min, max);
            self.material(pane_min, pane_max, MAT_GLASS);
            self.windows.push((pane_min, pane_max));
            self.features.windows += 1;
        }
    }

    fn stair(&mut self, room: Rect, y: i32) {
        let (w, d) = (room.x1 - room.x0, room.z1 - room.z0);
        // Slope one in two over ten metres, or one in one over five.
        let long_z = d >= w;
        let run = if (if long_z { d } else { w }) >= 112 {
            10
        } else {
            5
        };
        let rise = if run == 10 { 4 } else { 8 };
        let (x, z) = ((room.x0 + 15) / 8 * 8, (room.z0 + 15) / 8 * 8);
        let (nx, nz) = if long_z { (2, run) } else { (run, 2) };
        let mut verts = Vec::new();
        for j in 0..=nz {
            for i in 0..=nx {
                let t = if long_z { j } else { i };
                verts.push(y + rise * t);
            }
        }
        if let Ok(surface) = Surface::new(
            [x, z],
            [nx, nz],
            [y, y + 40],
            verts,
            [GUNMETAL, GUNMETAL, GUNMETAL, HAZARD, GUNMETAL, GUNMETAL],
        ) {
            self.ramps
                .push(([x, y, z], [x + nx * M, y + 40, z + nz * M]));
            self.surface(surface);
        }
    }

    fn pillars(&mut self, room: &Rect, y: i32, walls: u8, trim: u8) {
        let mut x = room.x0 + 24;
        while x + 24 <= room.x1 {
            let mut z = room.z0 + 24;
            while z + 24 <= room.z1 {
                self.solid(
                    [x, y, z],
                    [x + 8, y + 36, z + 8],
                    [walls, walls, walls, walls, walls, walls],
                );
                self.solid([x - 2, y + 32, z - 2], [x + 10, y + 36, z + 10], [trim; 6]);
                self.solid([x - 1, y, z - 1], [x + 9, y + 3, z + 9], [trim; 6]);
                z += 32;
            }
            x += 32;
        }
    }

    /// Crates and shelves standing in a room, clear of its walls and doors.
    fn clutter(&mut self, rng: &mut Rng, room: &Rect, y: i32) {
        let (w, d) = (room.x1 - room.x0 - 32, room.z1 - room.z0 - 32);
        if w < 16 || d < 16 {
            return;
        }
        let area_m2 = (w / M) * (d / M);
        let count = area_m2 * self.plan.clutter / 100;
        for _ in 0..count {
            let size = rng.pick(&[4, 8, 8, 8, 16]);
            let x = (room.x0 + 16 + rng.range(0, w - size)) / size * size;
            let z = (room.z0 + 16 + rng.range(0, d - size)) / size * size;
            if x < room.x0 + 16
                || z < room.z0 + 16
                || x + size > room.x1 - 16
                || z + size > room.z1 - 16
            {
                continue;
            }
            let faces = [rng.pick(&[RUSTED, WORN, HAZARD, GUNMETAL, AMBER, CONCRETE]); 6];
            self.solid([x, y, z], [x + size, y + size, z + size], faces);
            if size <= 8 && rng.chance(30) {
                self.solid([x, y + size, z], [x + size, y + 2 * size, z + size], faces);
            }
        }
    }

    fn trim(&mut self, room: &Rect, y: i32, trim: u8) {
        let f = [trim; 6];
        let h = 3;
        self.solid([room.x0, y, room.z0], [room.x1, y + h, room.z0 + 1], f);
        self.solid([room.x0, y, room.z1 - 1], [room.x1, y + h, room.z1], f);
        self.solid(
            [room.x0, y, room.z0 + 1],
            [room.x0 + 1, y + h, room.z1 - 1],
            f,
        );
        self.solid(
            [room.x1 - 1, y, room.z0 + 1],
            [room.x1, y + h, room.z1 - 1],
            f,
        );
    }

    fn pool(&mut self, rng: &mut Rng, room: Rect) {
        let w = (((room.x1 - room.x0) - 32) / 8 * 8).min(96);
        let d = (((room.z1 - room.z0) - 32) / 8 * 8).min(96);
        if w < 24 || d < 24 {
            return;
        }
        let x = ((room.x0 + 16 + rng.range(0, (room.x1 - room.x0 - 32 - w).max(0))) / 8) * 8;
        let z = ((room.z0 + 16 + rng.range(0, (room.z1 - room.z0 - 32 - d).max(0))) / 8) * 8;
        self.air([x, -16, z], [x + w, 0, z + d]);
        self.material([x, -16, z], [x + w, -4, z + d], MAT_WATER);
        self.features.pools += 1;
    }

    fn roof(&mut self, fp: &[i32; 4], top: i32, kind: u8, faces: [u8; 6]) {
        if kind == 0 {
            // A parapet and, now and then, a skylight are enough.
            return;
        }
        let (nx, nz) = ((fp[2] - fp[0]) / M, (fp[3] - fp[1]) / M);
        let mut verts = Vec::new();
        for j in 0..=nz {
            for i in 0..=nx {
                let edge = if kind == 1 {
                    i.min(nx - i).min(j.min(nz - j))
                } else if nx <= nz {
                    i.min(nx - i)
                } else {
                    j.min(nz - j)
                };
                verts.push(top + (4 * edge).min(40));
            }
        }
        if let Ok(surface) = Surface::new([fp[0], fp[1]], [nx, nz], [top, top + 64], verts, faces) {
            self.surface(surface);
        }
    }

    // -- Plazas ------------------------------------------------------------

    fn plaza(&mut self, rng: &mut Rng, rect: [i32; 4]) {
        let r = rect.map(|v| v * M);
        let (w, d) = (r[2] - r[0], r[3] - r[1]);
        // A pool, a glass canopy on pillars and a few ruined walls.
        let pw = (rng.range(6, 12)) * M;
        let pd = (rng.range(6, 12)) * M;
        let px = r[0] + (rng.range(2, ((w - pw) / M - 2).max(2))) * M;
        let pz = r[1] + (rng.range(2, ((d - pd) / M - 2).max(2))) * M;
        self.air([px, -24, pz], [px + pw, 0, pz + pd]);
        self.material([px, -24, pz], [px + pw, -8, pz + pd], MAT_WATER);
        self.features.pools += 1;
        // Rim and pillars.
        let rim = [CONCRETE; 6];
        self.solid([px - 8, 0, pz - 8], [px + pw + 8, 6, pz], rim);
        self.solid([px - 8, 0, pz + pd], [px + pw + 8, 6, pz + pd + 8], rim);
        self.solid([px - 8, 0, pz], [px, 6, pz + pd], rim);
        self.solid([px + pw, 0, pz], [px + pw + 8, 6, pz + pd], rim);
        let (cx, cz) = (px - 24, pz - 24);
        for (ox, oz) in [(0, 0), (pw + 40, 0), (0, pd + 40), (pw + 40, pd + 40)] {
            self.solid(
                [cx + ox, 0, cz + oz],
                [cx + ox + 8, 40, cz + oz + 8],
                [WORN; 6],
            );
        }
        self.solid(
            [cx - 4, 40, cz - 4],
            [cx + pw + 52, 44, cz + pd + 52],
            [GUNMETAL; 6],
        );
        self.air([cx + 8, 40, cz + 8], [cx + pw + 40, 44, cz + pd + 40]);
        self.material(
            [cx + 8, 40, cz + 8],
            [cx + pw + 40, 44, cz + pd + 40],
            MAT_GLASS,
        );
        self.outdoor_lights.push([px + pw / 2, 48, pz + pd / 2]);
        self.spawn_spots.push([r[0] + 16, 0, r[1] + 16]);
        // Ruined walls.
        for _ in 0..rng.range(2, 5) {
            let wx = r[0] + rng.range(1, (w / M - 8).max(1)) * M;
            let wz = r[1] + rng.range(1, (d / M - 8).max(1)) * M;
            let len = rng.range(3, 7) * M;
            let high = rng.range(2, 4) * M;
            let material = rng.pick(&[WORN, RUSTED, CONCRETE]);
            // Keep walls off the pool and its canopy.
            if wx + len > px - 48 && wx < px + pw + 48 && wz + 8 > pz - 48 && wz < pz + pd + 48 {
                continue;
            }
            if rng.chance(50) {
                self.solid([wx, 0, wz], [wx + len, high, wz + 4], [material; 6]);
            } else {
                self.solid([wx, 0, wz], [wx + 4, high, wz + len], [material; 6]);
            }
        }
    }

    // -- The edited layer --------------------------------------------------

    fn edits(&mut self, rng: &mut Rng, plot: Plot) {
        let r = plot.rect.map(|v| v * M);
        let c = plot.cell.map(|v| v * M);
        // Small cubes cut into the large ones of a building's walls.
        for _ in 0..if plot.plaza { 0 } else { self.plan.niches } {
            let sx = rng.range(1, 4);
            let sy = rng.range(1, 4);
            let sz = rng.range(1, 4);
            let y = rng.range(8, 30);
            let t = rng.range(0, 7 - sx.max(sz).min(7));
            let (x, z) = match rng.below(4) {
                0 => (r[0] + t, r[1] + rng.range(0, (r[3] - r[1]) - 4)),
                1 => (r[2] - 8 + t, r[1] + rng.range(0, (r[3] - r[1]) - 4)),
                2 => (r[0] + rng.range(0, (r[2] - r[0]) - 4), r[1] + t),
                _ => (r[0] + rng.range(0, (r[2] - r[0]) - 4), r[3] - 8 + t),
            };
            self.air([x, y, z], [x + sx, y + sy, z + sz]);
        }
        // Small pits cut into the plot's ground.
        for _ in 0..if plot.plaza { 0 } else { self.plan.pits } {
            let (sx, sy, sz) = (rng.range(1, 6), rng.range(1, 6), rng.range(1, 6));
            let x = c[0] + rng.range(0, c[2] - c[0] - sx);
            let z = c[1] + rng.range(0, c[3] - c[1] - sz);
            if !(x + sx > r[0] - 8 && x < r[2] + 8 && z + sz > r[1] - 8 && z < r[3] + 8) {
                self.air([x, -sy, z], [x + sx, 0, z + sz]);
            }
        }
        let free = |x: i32, z: i32, size: i32| {
            x >= c[0]
                && z >= c[1]
                && x + size <= c[2]
                && z + size <= c[3]
                && !(x + size > r[0] - 8 && x < r[2] + 8 && z + size > r[1] - 8 && z < r[3] + 8)
        };
        // Scattered blocks on the plot's ground.
        for _ in 0..self.plan.blocks {
            let size = rng.pick(&[2, 4, 4, 8, 8, 16]);
            let x = (c[0] + rng.range(0, c[2] - c[0] - size)) / size * size;
            let z = (c[1] + rng.range(0, c[3] - c[1] - size)) / size * size;
            if !free(x, z, size) || plot.plaza {
                continue;
            }
            let faces = [rng.pick(&[CONCRETE, WORN, RUSTED, HAZARD, GUNMETAL]); 6];
            let high = rng.range(2, 12).min(size.max(4));
            self.solid([x, 0, z], [x + size, high, z + size], faces);
        }
        // Shaped cubes with a different material on every face.
        for _ in 0..self.plan.shaped {
            let size = rng.pick(&[4i32, 8, 8]);
            let x = (c[0] + rng.range(0, c[2] - c[0] - size)) / size * size;
            let z = (c[1] + rng.range(0, c[3] - c[1] - size)) / size * size;
            if !free(x, z, size) || plot.plaza {
                continue;
            }
            let faces: [u8; 6] = std::array::from_fn(|_| rng.below(8) as u8);
            let leaf = Leaf {
                geometry: LeafGeometry::Shaped(random_shape(rng)),
                faces,
                cube_material: MAT_AIR,
            };
            self.stamp(
                NodeBox {
                    min: [x, 0, z],
                    size: size as u32,
                },
                leaf,
            );
        }
    }

    // -- Putting it together ----------------------------------------------

    fn build_tree(&mut self, bounds: WorldBounds) -> Result<Octree, String> {
        let root = Octree::new(bounds)?.root_box();
        if root.min != self.root_min {
            return Err("the world's root is not where the generator assumes".into());
        }
        let mut builder = OctreeBuilder::new(bounds, root)?;
        let geo: Vec<u32> = (0..self.geo.len() as u32).collect();
        let mat: Vec<u32> = (0..self.mat.len() as u32).collect();
        self.walk(&mut builder, bounds, root, &geo, &mat)?;
        builder.finish()
    }

    fn walk(
        &self,
        builder: &mut OctreeBuilder,
        bounds: WorldBounds,
        at: NodeBox,
        geo: &[u32],
        mat: &[u32],
    ) -> Result<(), String> {
        let fine = bounds.fine_box();
        let s = at.size as i32;
        if (0..3).any(|a| at.min[a] >= fine.max[a] || at.min[a] + s <= fine.min[a]) {
            return Ok(());
        }
        let inside = (0..3).all(|a| at.min[a] >= fine.min[a] && at.min[a] + s <= fine.max[a]);
        if inside {
            if let Some(leaf) = self.settle(at, geo, mat)? {
                if leaf.geometry == LeafGeometry::Empty && leaf.cube_material == MAT_AIR {
                    return Ok(());
                }
                return builder.push(at, leaf);
            }
        }
        if at.size == 1 {
            return Err("a node of one unit cannot split".into());
        }
        for i in 0..8 {
            let child = at.child(i);
            let g: Vec<u32> = geo
                .iter()
                .copied()
                .filter(|&k| self.geo[k as usize].bb.overlaps(&child))
                .collect();
            let m: Vec<u32> = mat
                .iter()
                .copied()
                .filter(|&k| self.mat[k as usize].bb.overlaps(&child))
                .collect();
            self.walk(builder, bounds, child, &g, &m)?;
        }
        Ok(())
    }

    /// The leaf that fills a node, or none when it must split.
    fn settle(&self, at: NodeBox, geo: &[u32], mat: &[u32]) -> Result<Option<Leaf>, String> {
        let mut geometry = Leaf::empty(MAT_AIR);
        for &k in geo.iter().rev() {
            let op = &self.geo[k as usize];
            let cover = match &op.kind {
                GeoKind::Box(leaf) => {
                    if op.bb.covers(&at) {
                        Cover::Covers(*leaf)
                    } else {
                        Cover::Partial
                    }
                }
                GeoKind::Surface(surface) => surface.cover(&at)?,
                GeoKind::Stamp(leaf) => {
                    let size = (op.bb.max[0] - op.bb.min[0]) as u32;
                    if op.bb.min == at.min && size == at.size {
                        Cover::Covers(*leaf)
                    } else if at.size > size {
                        Cover::Partial
                    } else {
                        return Err("a later operation cuts into a shaped cube".into());
                    }
                }
            };
            match cover {
                Cover::Disjoint => {}
                Cover::Partial => return Ok(None),
                Cover::Covers(leaf) => {
                    geometry = leaf;
                    break;
                }
            }
        }
        if geometry.geometry == LeafGeometry::Full {
            return Ok(Some(geometry));
        }
        for &k in mat.iter().rev() {
            let op = &self.mat[k as usize];
            if op.bb.covers(&at) {
                geometry.cube_material = op.material;
                return Ok(Some(geometry));
            }
            if op.bb.overlaps(&at) {
                return Ok(None);
            }
        }
        Ok(Some(geometry))
    }

    fn objects_and_poses(&mut self, plots: &[Plot]) -> (Vec<WorldObject>, Vec<BenchmarkPose>) {
        let mut rng = Rng::new(self.options.seed, 4, 0, 0);
        // Lights: a shuffled share of the rooms' centres and the plazas'
        // lamps, in position order.
        let mut candidates: Vec<(bool, usize, [i32; 3])> = Vec::new();
        for (k, room) in self.rooms.iter().enumerate() {
            candidates.push((
                true,
                k,
                [room.centre[0], room.centre[1] + 30, room.centre[2]],
            ));
        }
        let extra = self.extra_lights.clone();
        for (k, spot) in extra.into_iter().enumerate() {
            candidates.push((false, 1_000_000 + k, spot));
        }
        for (k, spot) in self.outdoor_lights.iter().enumerate() {
            candidates.push((false, k, *spot));
        }
        for k in (1..candidates.len()).rev() {
            let j = rng.below(k as u32 + 1) as usize;
            candidates.swap(k, j);
        }
        candidates.truncate(self.plan.lights);
        for &(is_room, k, _) in &candidates {
            if is_room {
                self.rooms[k].lit = true;
            }
        }
        let mut lights: Vec<[i32; 3]> = candidates.iter().map(|c| c.2).collect();
        lights.sort();
        let mut spawns = self.spawn_spots.clone();
        for k in (1..spawns.len()).rev() {
            let j = rng.below(k as u32 + 1) as usize;
            spawns.swap(k, j);
        }
        spawns.truncate(self.plan.spawns);
        spawns.sort();
        let to_m = |p: [i32; 3]| Vec3::new(p[0] as f32 / 8.0, p[1] as f32 / 8.0, p[2] as f32 / 8.0);
        let mut objects = Vec::new();
        for p in spawns.iter() {
            objects.push(WorldObject {
                id: objects.len() as u32 + 1,
                position: to_m(*p),
                kind: ObjectKind::Spawn,
                rotation: 0.0,
            });
        }
        for p in &lights {
            objects.push(WorldObject {
                id: objects.len() as u32 + 1,
                position: to_m(*p),
                kind: ObjectKind::Light,
                rotation: 0.0,
            });
        }
        let poses = self.poses(plots, &spawns);
        (objects, poses)
    }

    fn poses(&self, plots: &[Plot], spawns: &[[i32; 3]]) -> Vec<BenchmarkPose> {
        // Everything is whole eighths until the last step, so the poses are
        // the same numbers everywhere.
        let m = |p: [i32; 3]| p.map(|v| v as f32 / 8.0);
        let eye = |p: [i32; 3]| m([p[0], p[1] + 12, p[2]]);
        let pose = |name: &str, eye: [f32; 3], look_at: [f32; 3]| BenchmarkPose {
            name: name.to_string(),
            eye,
            look_at,
            time: 2.0,
            edit_storm: false,
            path: Vec::new(),
            speed: None,
            dwell: None,
        };
        let mut poses = Vec::new();
        let reach = self.plan.grid * self.plan.pitch_m / 2 * M;
        let spawn = spawns.first().copied().unwrap_or([0, 0, 0]);
        poses.push(pose("spawn", eye(spawn), m([0, 24, 0])));
        poses.push(pose(
            "overlook",
            m([-reach * 9 / 10, 360, reach * 11 / 10]),
            m([0, 16, 0]),
        ));
        let lit: Vec<&RoomSpot> = self.rooms.iter().filter(|r| r.lit).collect();
        let ground: Vec<&&RoomSpot> = lit.iter().filter(|r| r.storey == 0).collect();
        let towards = |room: &RoomSpot| m([room.centre[0], room.centre[1] + 20, room.centre[2]]);
        for (n, room) in ground.iter().take(2).enumerate() {
            poses.push(pose(
                &format!("room-{}", n + 1),
                eye(room.corner),
                towards(room),
            ));
        }
        if let Some(room) = lit.iter().find(|r| r.storey > 0) {
            poses.push(pose("upstairs", eye(room.corner), towards(room)));
        }
        if let Some(&(c, r)) = self.lakes.first() {
            let (cx, cz) = (c[0] * M, c[1] * M);
            let shore = r * M * 9 / 10;
            poses.push(pose(
                "water",
                m([cx + shore, 32, cz + shore]),
                m([cx, -8, cz]),
            ));
            let near = r * M * 3 / 10;
            poses.push(pose(
                "underwater",
                m([cx + near, -32, cz + near]),
                m([cx - near, -40, cz - near]),
            ));
        }
        if let Some(&(lo, hi)) = self.windows.first() {
            let mid = [
                (lo[0] + hi[0]) / 2,
                (lo[1] + hi[1]) / 2,
                (lo[2] + hi[2]) / 2,
            ];
            // From the open side of the window's wall.
            let from = if hi[0] - lo[0] <= 4 {
                [mid[0] + if mid[0] > 0 { 48 } else { -48 }, mid[1], mid[2]]
            } else {
                [mid[0], mid[1], mid[2] + if mid[2] > 0 { 48 } else { -48 }]
            };
            poses.push(pose("glass", m(from), m(mid)));
        }
        if let Some(&(lo, hi)) = self.ramps.first() {
            poses.push(pose(
                "stairs",
                m([lo[0] + 8, lo[1] + 12, lo[2] + 4]),
                m([(lo[0] + hi[0]) / 2, lo[1] + 24, hi[2]]),
            ));
        }
        let plot = plots.first().map_or([0, 0, 0, 0], |p| p.cell);
        let street = [plot[0] * M - 48, 12, plot[1] * M - 48];
        let mut flight = pose(
            "flight",
            m(street),
            m([street[0] + 160, 24, street[2] + 40]),
        );
        flight.path = vec![
            BenchmarkWaypoint {
                eye: m([-reach / 2, 12, -reach / 2]),
                look_at: m([-reach / 2 + 160, 24, -reach / 2 + 40]),
                jump: false,
            },
            BenchmarkWaypoint {
                eye: m([reach / 2, 12, -reach / 2]),
                look_at: m([reach / 2, 24, -reach / 2 + 160]),
                jump: false,
            },
        ];
        flight.speed = Some(10.0);
        poses.push(flight);
        let mut teleports = pose("teleports", poses[0].eye, poses[0].look_at);
        teleports.dwell = Some(150);
        teleports.path = poses[1..poses.len().min(5)]
            .iter()
            .map(|p| BenchmarkWaypoint {
                eye: p.eye,
                look_at: p.look_at,
                jump: true,
            })
            .collect();
        poses.push(teleports);
        let mut storm = pose("storm", poses[0].eye, poses[0].look_at);
        if let Some(room) = ground.first() {
            storm.eye = eye(room.corner);
            storm.look_at = towards(room);
        }
        storm.edit_storm = true;
        poses.push(storm);
        poses
    }
}

#[derive(Clone, Copy)]
struct Rect {
    x0: i32,
    z0: i32,
    x1: i32,
    z1: i32,
}

/// A valid shape with a corner or an edge pushed in: a wedge, a pulled-down
/// corner, a slanted wall or a lifted bottom corner.
fn random_shape(rng: &mut Rng) -> crate::Shape {
    let mut shape = crate::Shape::solid();
    match rng.below(4) {
        0 => {
            // Wedge: the +X top corners drop.
            let h = rng.range(0, 6) as u8;
            shape.vertices[5][1] = h;
            shape.vertices[6][1] = h;
        }
        1 => {
            // One top corner pulled down.
            let k = 4 + rng.below(4) as usize;
            shape.vertices[k][1] = rng.range(0, 5) as u8;
        }
        2 => {
            // A slanted wall: the top shifts in x.
            let shift = rng.range(1, 5) as u8;
            shape.vertices[4][0] = shift;
            shape.vertices[7][0] = shift;
        }
        _ => {
            // A bottom corner lifted: an overhanging edge.
            let k = rng.below(4) as usize;
            shape.vertices[k][1] = rng.range(1, 5) as u8;
        }
    }
    if rng.chance(50) {
        shape.diagonals = (rng.below(64)) as u8;
    }
    if shape.is_valid() {
        shape
    } else {
        crate::Shape::solid()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::World;

    fn hex(digest: [u8; 16]) -> String {
        digest.iter().map(|b| format!("{b:02x}")).collect()
    }

    fn test_world(edited: bool) -> BenchmarkWorld {
        generate_benchmark_world(BenchmarkOptions {
            edited,
            ..BenchmarkOptions::new(BenchmarkPreset::Test)
        })
        .unwrap()
    }

    /// The recorded digests of the test preset with its default seed. They
    /// change only when the generator does; update them with the reason in
    /// the stage's notes. They must come out the same on every system: the
    /// generator uses integers only, and the Linux job checks it.
    #[test]
    fn the_test_preset_has_its_recorded_digest() {
        let world = test_world(true);
        assert_eq!(
            (hex(world.tree.digest()), world.extras_digest()),
            (TEST_TREE_DIGEST.to_string(), TEST_EXTRAS_DIGEST),
            "{:?}",
            world.tree.stats()
        );
        let plain = test_world(false);
        assert_eq!(
            (hex(plain.tree.digest()), plain.extras_digest()),
            (TEST_PLAIN_TREE_DIGEST.to_string(), TEST_PLAIN_EXTRAS_DIGEST),
            "{:?}",
            plain.tree.stats()
        );
    }

    const TEST_TREE_DIGEST: &str = "ab23c3eaa1b078f729cc5c000118565c";
    const TEST_EXTRAS_DIGEST: u64 = 17568549161982122090;
    const TEST_PLAIN_TREE_DIGEST: &str = "f3515db81f0f4158732c48ea922e553e";
    const TEST_PLAIN_EXTRAS_DIGEST: u64 = 17568549161982122090;

    #[test]
    fn generation_depends_only_on_seed_preset_and_edits() {
        let a = test_world(true);
        let b = test_world(true);
        assert_eq!(a.tree.digest(), b.tree.digest());
        assert_eq!(a.extras_digest(), b.extras_digest());
        assert_ne!(a.tree.digest(), test_world(false).tree.digest());
        let other = generate_benchmark_world(BenchmarkOptions {
            seed: BENCHMARK_SEED + 1,
            ..BenchmarkOptions::new(BenchmarkPreset::Test)
        })
        .unwrap();
        assert_ne!(a.tree.digest(), other.tree.digest());
    }

    #[test]
    fn the_test_world_has_every_kind_of_structure() {
        for edited in [false, true] {
            let world = test_world(edited);
            let stats = world.tree.stats();
            assert!(stats.full_leaves > 1_000 && stats.shaped_leaves > 100);
            assert!(stats.material_leaves > 100 && stats.internal_nodes > 1_000);
            let (mut water, mut glass) = (false, false);
            let mut sizes = BTreeSet::new();
            world.tree.for_each_leaf(|at, leaf| {
                sizes.insert(at.size);
                if leaf.geometry == LeafGeometry::Empty {
                    water |= leaf.cube_material == MAT_WATER;
                    glass |= leaf.cube_material == MAT_GLASS;
                }
            });
            assert!(water && glass);
            // From an eighth of a metre to many metres.
            assert!(sizes.contains(&1) && sizes.iter().any(|&s| s >= 128));
            assert_eq!(world.features.lights, 12);
            assert_eq!(world.features.spawns, 4);
            assert!(world.features.rooms >= 10 && world.features.buildings >= 2);
            assert!(world.poses.len() >= 8);
        }
        // The edited layer cuts small cubes into large ones and adds shaped
        // cubes and blocks.
        let (plain, edited) = (test_world(false), test_world(true));
        assert!(edited.tree.stats().leaves() > plain.tree.stats().leaves());
        assert!(edited.tree.stats().shaped_leaves > plain.tree.stats().shaped_leaves);
    }

    /// The tree survives the file form, with explicit shapes and no cells,
    /// and the lights and spawns stand in open space.
    #[test]
    fn the_file_form_loads_back_to_the_same_world() {
        let world = test_world(true);
        let parts = world.format3();
        assert!(parts.cells.is_empty() && parts.roots.is_empty());
        assert!(parts.regions.iter().any(|r| r.cell.shape.is_some()));
        let back = Octree::from_format3(&parts).unwrap();
        assert_eq!(back.digest(), world.tree.digest());
        let mut play = World::from_compact_parts(parts.bounds, &parts.regions, &[], &[]).unwrap();
        play.replace_volumes(&parts.volumes).unwrap();
        for object in &world.objects {
            let p = object.position;
            let (min, max) = match object.kind {
                ObjectKind::Spawn => (
                    p + Vec3::new(-0.3, 0.05, -0.3),
                    p + Vec3::new(0.3, 1.7, 0.3),
                ),
                _ => (p - Vec3::splat(0.1), p + Vec3::splat(0.1)),
            };
            assert!(
                !play.intersects_box(min, max),
                "{object:?} is inside geometry"
            );
        }
        let ids: BTreeSet<u32> = world.objects.iter().map(|o| o.id).collect();
        assert_eq!(ids.len(), world.objects.len());
    }

    /// Every preset builds, and the smaller one with its edited layer fits
    /// the limits of today's saves and joins.
    #[test]
    #[ignore = "builds the large presets; run with --release"]
    fn the_large_presets_build_within_the_world_guard() {
        for preset in [BenchmarkPreset::Small, BenchmarkPreset::Nub1] {
            let world = generate_benchmark_world(BenchmarkOptions::new(preset)).unwrap();
            let leaves = world.tree.stats().leaves();
            assert!(
                leaves < crate::MAX_WORLD_PIECES / 8 * 7,
                "{preset:?}: {leaves}"
            );
            assert!(world.objects.len() <= 1024);
        }
        let plain = generate_benchmark_world(BenchmarkOptions {
            edited: false,
            ..BenchmarkOptions::new(BenchmarkPreset::Small)
        })
        .unwrap();
        assert!(plain.tree.stats().leaves() > 300_000);
    }
}
