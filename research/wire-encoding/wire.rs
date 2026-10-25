//! Wire mirrors of the game types, free of JSON-shaped serde attributes
//! (no skip_serializing_if, no defaults, no internal tags), so every
//! non-self-describing format can round-trip them. This is the "codec
//! boundary" the tracking doc asks for.
use crate::data::Saved;
use sauerteig_core as sc;

macro_rules! wire {
    ($($item:item)*) => { $(
        #[derive(Clone, Debug, PartialEq,
                 serde::Serialize, serde::Deserialize,
                 bitcode::Encode, bitcode::Decode,
                 bincode::Encode, bincode::Decode,
                 rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
        $item
    )* };
}

wire! {
    pub struct WBox { pub min: [i32; 3], pub max: [i32; 3] }
    pub struct WShape { pub vertices: [[u8; 3]; 8], pub diagonals: u8 }
    pub struct WCell {
        pub material: u8,
        pub face_materials: Option<[u8; 6]>,
        pub heights: [u8; 4],
        pub diagonal: bool,
        pub shape: Option<WShape>,
    }
    pub struct WFragment { pub bounds: WBox, pub origin: [i32; 3], pub size: u32, pub cell: WCell }
    pub struct WCellEdit { pub pos: [i32; 3], pub cell: Option<WCell> }
    pub struct WRootEdit { pub pos: [i32; 3], pub fragments: Vec<WFragment> }
    pub struct WVolume { pub bounds: WBox, pub material: u8 }
    pub struct WObject { pub id: u32, pub position: [f32; 3], pub kind: u8, pub rotation: f32 }
    pub struct WWorld {
        pub revision: u64,
        pub format_version: u32,
        pub bounds: WBox,
        pub regions: Vec<WFragment>,
        pub cells: Vec<WCellEdit>,
        pub roots: Vec<WRootEdit>,
        pub volumes: Vec<WVolume>,
        pub objects: Vec<WObject>,
        pub next_object: u32,
    }
}

impl Copy for WBox {}
impl From<sc::FineBox> for WBox {
    fn from(b: sc::FineBox) -> Self {
        Self { min: b.min, max: b.max }
    }
}
impl From<WBox> for sc::FineBox {
    fn from(b: WBox) -> Self {
        Self { min: b.min, max: b.max }
    }
}
impl From<sc::WorldBounds> for WBox {
    fn from(b: sc::WorldBounds) -> Self {
        Self { min: b.min, max: b.max }
    }
}
impl From<WBox> for sc::WorldBounds {
    fn from(b: WBox) -> Self {
        Self { min: b.min, max: b.max }
    }
}
impl From<&sc::Cell> for WCell {
    fn from(c: &sc::Cell) -> Self {
        Self {
            material: c.material,
            face_materials: c.face_materials,
            heights: c.heights,
            diagonal: c.diagonal,
            shape: c.shape.map(|s| WShape { vertices: s.vertices, diagonals: s.diagonals }),
        }
    }
}
impl From<&WCell> for sc::Cell {
    fn from(c: &WCell) -> Self {
        Self {
            material: c.material,
            face_materials: c.face_materials,
            heights: c.heights,
            diagonal: c.diagonal,
            shape: c.shape.as_ref().map(|s| sc::Shape { vertices: s.vertices, diagonals: s.diagonals }),
        }
    }
}
impl From<&sc::CellFragment> for WFragment {
    fn from(f: &sc::CellFragment) -> Self {
        Self { bounds: f.bounds.into(), origin: f.origin, size: f.size, cell: (&f.cell).into() }
    }
}
impl From<&WFragment> for sc::CellFragment {
    fn from(f: &WFragment) -> Self {
        Self { bounds: f.bounds.into(), origin: f.origin, size: f.size, cell: (&f.cell).into() }
    }
}

fn kind_to_u8(k: sc::ObjectKind) -> u8 {
    use sc::ObjectKind::*;
    match k {
        Tree => 0,
        Light => 1,
        Spawn => 2,
        WallLamp => 3,
        HangingLamp => 4,
        UtilityBox => 5,
        Pipe => 6,
        Rock => 7,
    }
}
fn kind_from_u8(k: u8) -> sc::ObjectKind {
    use sc::ObjectKind::*;
    [Tree, Light, Spawn, WallLamp, HangingLamp, UtilityBox, Pipe, Rock][k as usize]
}
fn volume_to_u8(m: sc::VolumeMaterial) -> u8 {
    match m {
        sc::VolumeMaterial::Water => 0,
        sc::VolumeMaterial::Glass => 1,
    }
}
fn volume_from_u8(m: u8) -> sc::VolumeMaterial {
    if m == 0 { sc::VolumeMaterial::Water } else { sc::VolumeMaterial::Glass }
}

impl WWorld {
    pub fn from_saved(s: &Saved) -> Self {
        Self {
            revision: s.revision,
            format_version: s.format_version,
            bounds: s.bounds.into(),
            regions: s.regions.iter().map(Into::into).collect(),
            cells: s
                .cells
                .iter()
                .map(|c| WCellEdit { pos: c.pos, cell: c.cell.as_ref().map(Into::into) })
                .collect(),
            roots: s
                .roots
                .iter()
                .map(|r| WRootEdit { pos: r.pos, fragments: r.fragments.iter().map(Into::into).collect() })
                .collect(),
            volumes: s
                .volumes
                .iter()
                .map(|v| WVolume { bounds: v.bounds.into(), material: volume_to_u8(v.material) })
                .collect(),
            objects: s
                .objects
                .iter()
                .map(|o| WObject {
                    id: o.id,
                    position: o.position.to_array(),
                    kind: kind_to_u8(o.kind),
                    rotation: o.rotation,
                })
                .collect(),
            next_object: s.next_object,
        }
    }

    pub fn to_saved(&self) -> Saved {
        Saved {
            revision: self.revision,
            format_version: self.format_version,
            bounds: self.bounds.into(),
            regions: self.regions.iter().map(Into::into).collect(),
            cells: self
                .cells
                .iter()
                .map(|c| sc::CellEdit { pos: c.pos, cell: c.cell.as_ref().map(Into::into) })
                .collect(),
            roots: self
                .roots
                .iter()
                .map(|r| sc::RootEdit { pos: r.pos, fragments: r.fragments.iter().map(Into::into).collect() })
                .collect(),
            volumes: self
                .volumes
                .iter()
                .map(|v| sc::MaterialVolume { bounds: v.bounds.into(), material: volume_from_u8(v.material) })
                .collect(),
            objects: self
                .objects
                .iter()
                .map(|o| sc::WorldObject {
                    id: o.id,
                    position: sc::Vec3::from_array(o.position),
                    kind: kind_from_u8(o.kind),
                    rotation: o.rotation,
                })
                .collect(),
            next_object: self.next_object,
        }
    }
}

/// Field-by-field equality of a decoded world against the loaded one, so a
/// codec cannot "win" on size by silently dropping information.
pub fn same_saved(a: &Saved, b: &Saved) -> bool {
    a.revision == b.revision
        && a.format_version == b.format_version
        && a.bounds == b.bounds
        && a.regions == b.regions
        && a.cells == b.cells
        && a.roots == b.roots
        && a.volumes == b.volumes
        && a.objects == b.objects
        && a.next_object == b.next_object
}
