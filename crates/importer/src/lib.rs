//! Non-executing, bounded reader for Cube 2 OCTA map versions 29 through 33.
//! Source format references are recorded in README.md; no engine code is linked.
mod convert;
mod transform;
pub use convert::{convert, GeometryIssue, ImportOptions, ImportReport, ImportedWorld};

use flate2::read::GzDecoder;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, io::Read};

pub const MAX_COMPRESSED_BYTES: usize = 64 * 1024 * 1024;
pub const MAX_DECOMPRESSED_BYTES: usize = 256 * 1024 * 1024;
const MAX_NODES: usize = 2_000_000;

#[derive(Clone, Debug, Serialize)]
pub struct Header {
    pub version: u32,
    pub world_size: u32,
    pub entities: u32,
    pub pvs_cells: u32,
    pub lightmaps: u32,
    pub blendmap: u32,
    pub variables: u32,
    pub vslots: u32,
    pub game: String,
}
#[derive(Clone, Debug, Serialize)]
pub struct Entity {
    pub position: [f32; 3],
    pub attributes: [i16; 5],
    pub kind: u8,
    /// Extra game-specific data is retained, never interpreted or executed.
    pub extra: Vec<u8>,
}
#[derive(Clone, Debug, Serialize)]
pub struct VSlot {
    pub index: u32,
    pub previous: i32,
    pub changed: u32,
    pub values: BTreeMap<String, serde_json::Value>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Cube {
    pub origin: [i32; 3],
    pub size: u32,
    pub kind: u8,
    pub edges: [u8; 12],
    pub textures: [u16; 6],
    /// Original on-disk value. Versions 29–32 use a packed byte; version 33
    /// uses the indexed ushort layout. Keep this value for source inspection.
    pub volume_material: u16,
}
impl Cube {
    pub fn is_empty(&self) -> bool {
        self.kind == 1 || self.edges[..4] == [0; 4]
    }
    pub fn is_solid(&self) -> bool {
        self.kind == 2 || self.edges == [0x80; 12]
    }

    /// Translate the older byte layout into the version-33 material bit fields.
    /// Volume (bits 2–4), clip (5–7), and flags (8–15) remain independent.
    pub fn normalized_material(&self, version: u32) -> u16 {
        let material = self.volume_material;
        if version <= 32 {
            ((material & 7) << 2) | (((material >> 3) & 3) << 5) | (((material >> 5) & 7) << 8)
        } else {
            material
        }
    }
}
#[derive(Clone, Debug, Serialize)]
pub struct Map {
    pub header: Header,
    pub variables: BTreeMap<String, serde_json::Value>,
    pub entities: Vec<Entity>,
    pub vslots: Vec<VSlot>,
    pub cubes: Vec<Cube>,
    pub decompressed_bytes: usize,
    pub octree_end_offset: usize,
    pub nodes: usize,
}
#[derive(Clone, Debug, Serialize)]
pub struct Inventory {
    pub header: Header,
    pub decompressed_bytes: usize,
    pub octree_end_offset: usize,
    pub render_data_bytes_not_imported: usize,
    pub octree_nodes: usize,
    pub nonempty_leaves: usize,
    pub deformed_leaves: usize,
    pub occupied_cube_bounds: Option<[[i32; 3]; 2]>,
    pub leaf_sizes: BTreeMap<u32, usize>,
    pub texture_slot_faces: BTreeMap<u16, usize>,
    /// Raw source values, retained for comparison with the original map.
    pub volume_material_leaves: BTreeMap<u16, usize>,
    /// Version-independent material values in the version-33 bit layout.
    pub normalized_material_leaves: BTreeMap<u16, usize>,
    pub variables: BTreeMap<String, serde_json::Value>,
    pub entities: Vec<Entity>,
    pub vslots: Vec<VSlot>,
}
impl Map {
    pub fn inventory(&self) -> Inventory {
        let mut result = Inventory {
            header: self.header.clone(),
            decompressed_bytes: self.decompressed_bytes,
            octree_end_offset: self.octree_end_offset,
            render_data_bytes_not_imported: self.decompressed_bytes - self.octree_end_offset,
            octree_nodes: self.nodes,
            nonempty_leaves: 0,
            deformed_leaves: 0,
            occupied_cube_bounds: None,
            leaf_sizes: BTreeMap::new(),
            texture_slot_faces: BTreeMap::new(),
            volume_material_leaves: BTreeMap::new(),
            normalized_material_leaves: BTreeMap::new(),
            variables: self.variables.clone(),
            entities: self.entities.clone(),
            vslots: self.vslots.clone(),
        };
        for cube in &self.cubes {
            if cube.volume_material != 0 {
                *result
                    .volume_material_leaves
                    .entry(cube.volume_material)
                    .or_default() += 1;
                *result
                    .normalized_material_leaves
                    .entry(cube.normalized_material(self.header.version))
                    .or_default() += 1;
            }
            if cube.is_empty() {
                continue;
            }
            result.nonempty_leaves += 1;
            result.deformed_leaves += usize::from(!cube.is_solid());
            *result.leaf_sizes.entry(cube.size).or_default() += 1;
            for texture in cube.textures {
                *result.texture_slot_faces.entry(texture).or_default() += 1;
            }
            let max = cube.origin.map(|v| v + cube.size as i32);
            let bounds = result
                .occupied_cube_bounds
                .get_or_insert([cube.origin, max]);
            for a in 0..3 {
                bounds[0][a] = bounds[0][a].min(cube.origin[a]);
                bounds[1][a] = bounds[1][a].max(max[a]);
            }
        }
        result
    }
}

struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}
impl<'a> Reader<'a> {
    fn bytes(&mut self, n: usize) -> Result<&'a [u8], String> {
        let end = self.pos.checked_add(n).ok_or("offset overflow")?;
        let result = self
            .data
            .get(self.pos..end)
            .ok_or_else(|| format!("truncated map at byte {} (need {n} bytes)", self.pos))?;
        self.pos = end;
        Ok(result)
    }
    fn skip(&mut self, n: usize) -> Result<(), String> {
        self.bytes(n).map(|_| ())
    }
    fn byte(&mut self) -> Result<u8, String> {
        Ok(self.bytes(1)?[0])
    }
    fn u16(&mut self) -> Result<u16, String> {
        Ok(u16::from_le_bytes(self.bytes(2)?.try_into().unwrap()))
    }
    fn i16(&mut self) -> Result<i16, String> {
        Ok(self.u16()? as i16)
    }
    fn u32(&mut self) -> Result<u32, String> {
        Ok(u32::from_le_bytes(self.bytes(4)?.try_into().unwrap()))
    }
    fn i32(&mut self) -> Result<i32, String> {
        Ok(self.u32()? as i32)
    }
    fn float(&mut self) -> Result<f32, String> {
        let v = f32::from_bits(self.u32()?);
        if !v.is_finite() {
            return Err(format!("non-finite number at byte {}", self.pos - 4));
        }
        Ok(v)
    }
    fn string(&mut self) -> Result<String, String> {
        let n = self.u16()? as usize;
        Ok(String::from_utf8_lossy(self.bytes(n)?).into_owned())
    }
}

pub fn read_ogz(compressed: &[u8]) -> Result<Map, String> {
    if compressed.len() > MAX_COMPRESSED_BYTES {
        return Err("compressed map exceeds 64 MiB limit".into());
    }
    let mut data = Vec::new();
    // Reading through EOF verifies gzip CRC and size. Corrupt archives are never
    // salvaged implicitly; original files remain available for a separate recovery.
    GzDecoder::new(compressed)
        .take(MAX_DECOMPRESSED_BYTES as u64 + 1)
        .read_to_end(&mut data)
        .map_err(|e| format!("invalid gzip stream (including checksum): {e}"))?;
    if data.len() > MAX_DECOMPRESSED_BYTES {
        return Err("decompressed map exceeds 256 MiB limit".into());
    }
    read_octa(&data)
}

pub fn read_octa(data: &[u8]) -> Result<Map, String> {
    if data.len() > MAX_DECOMPRESSED_BYTES {
        return Err("map exceeds decompressed size limit".into());
    }
    let mut r = Reader { data, pos: 0 };
    if r.bytes(4)? != b"OCTA" {
        return Err("not a Cube 2 OCTA map (Tesseract MAPZ is not supported yet)".into());
    }
    let version = r.u32()?;
    if !(29..=33).contains(&version) {
        return Err(format!(
            "unsupported OCTA version {version}; supported versions are 29 through 33"
        ));
    }
    let header_size = r.u32()?;
    let expected_header = if version == 29 { 36 } else { 40 };
    if header_size != expected_header {
        return Err(format!(
            "unexpected header size {header_size} for version {version}"
        ));
    }
    let mut header = Header {
        version,
        world_size: r.u32()?,
        entities: r.u32()?,
        pvs_cells: r.u32()?,
        lightmaps: r.u32()?,
        blendmap: r.u32()?,
        variables: r.u32()?,
        vslots: if version >= 30 { r.u32()? } else { 0 },
        game: String::new(),
    };
    if !header.world_size.is_power_of_two() || !(2..=1_048_576).contains(&header.world_size) {
        return Err("world size must be a power of two between 2 and 1048576".into());
    }
    if header.entities > 100_000 || header.variables > 65_536 || header.vslots > 1_000_000 {
        return Err("map header count exceeds import limits".into());
    }
    let mut variables = BTreeMap::new();
    for _ in 0..header.variables {
        let kind = r.byte()?;
        let name = r.string()?;
        let value = match kind {
            0 => serde_json::json!(r.i32()?),
            1 => serde_json::json!(r.float()?),
            2 => serde_json::json!(r.string()?),
            _ => return Err(format!("unknown variable type {kind}")),
        };
        variables.insert(name, value);
    }
    let game_len = r.byte()? as usize;
    let game_bytes = r.bytes(game_len + 1)?;
    if game_bytes[game_len] != 0 {
        return Err("game identifier is not NUL terminated".into());
    }
    header.game = String::from_utf8_lossy(&game_bytes[..game_len]).into_owned();
    let entity_extra = r.u16()? as usize;
    let game_extra = r.u16()? as usize;
    r.skip(game_extra)?;
    let mru_count = r.u16()? as usize;
    r.skip(mru_count * 2)?;
    let mut entities = Vec::with_capacity(header.entities as usize);
    for _ in 0..header.entities {
        let position = [r.float()?, r.float()?, r.float()?];
        let mut attributes = [r.i16()?, r.i16()?, r.i16()?, r.i16()?, r.i16()?];
        let kind = r.byte()?;
        r.skip(1)?; // entity struct's final reserved byte
        if version <= 30 && (kind == 2 || kind == 3) {
            attributes[0] = (i32::from(attributes[0]) + 180).rem_euclid(360) as i16;
        }
        if version <= 31 && kind == 2 {
            let yaw = i32::from(attributes[0]).rem_euclid(360) + 7;
            attributes[0] = (yaw - yaw % 15) as i16;
        }
        entities.push(Entity {
            position,
            attributes,
            kind,
            extra: r.bytes(entity_extra)?.to_vec(),
        });
    }
    let mut vslots = Vec::new();
    let mut index = 0;
    while index < header.vslots {
        let changed = r.i32()?;
        if changed < 0 {
            let count = changed.checked_neg().ok_or("invalid vslot run")? as u32;
            index = index
                .checked_add(count)
                .filter(|v| *v <= header.vslots)
                .ok_or("vslot run exceeds header count")?;
        } else {
            if changed & !255 != 0 {
                return Err(format!("unsupported vslot change flags {changed:#x}"));
            }
            let previous = r.i32()?;
            let mut values = BTreeMap::new();
            if changed & 1 != 0 {
                let count = r.u16()?;
                let mut params = Vec::new();
                for _ in 0..count {
                    let name = r.string()?;
                    params.push(serde_json::json!({"name":name,"value":[r.float()?,r.float()?,r.float()?,r.float()?]}));
                }
                values.insert("shader_parameters".into(), serde_json::json!(params));
            }
            if changed & 2 != 0 {
                values.insert("scale".into(), serde_json::json!(r.float()?));
            }
            if changed & 4 != 0 {
                values.insert("rotation".into(), serde_json::json!(r.i32()?));
            }
            if changed & 8 != 0 {
                values.insert("offset".into(), serde_json::json!([r.i32()?, r.i32()?]));
            }
            if changed & 16 != 0 {
                values.insert("scroll".into(), serde_json::json!([r.float()?, r.float()?]));
            }
            if changed & 32 != 0 {
                values.insert("layer".into(), serde_json::json!(r.i32()?));
            }
            if changed & 64 != 0 {
                values.insert("alpha".into(), serde_json::json!([r.float()?, r.float()?]));
            }
            if changed & 128 != 0 {
                values.insert(
                    "color".into(),
                    serde_json::json!([r.float()?, r.float()?, r.float()?]),
                );
            }
            vslots.push(VSlot {
                index,
                previous,
                changed: changed as u32,
                values,
            });
            index += 1;
        }
    }
    let mut cubes = Vec::new();
    let mut nodes = 0;
    read_children(
        &mut r,
        version,
        [0; 3],
        header.world_size / 2,
        &mut cubes,
        &mut nodes,
    )?;
    Ok(Map {
        header,
        variables,
        entities,
        vslots,
        cubes,
        decompressed_bytes: data.len(),
        octree_end_offset: r.pos,
        nodes,
    })
}
fn read_children(
    r: &mut Reader<'_>,
    version: u32,
    origin: [i32; 3],
    size: u32,
    cubes: &mut Vec<Cube>,
    nodes: &mut usize,
) -> Result<(), String> {
    if size == 0 {
        return Err("octree subdivides below one Cube unit".into());
    }
    for i in 0..8 {
        *nodes += 1;
        if *nodes > MAX_NODES {
            return Err("octree exceeds two million nodes".into());
        }
        let origin =
            std::array::from_fn(|a| origin[a] + if i & (1 << a) != 0 { size as i32 } else { 0 });
        let flags = r.byte()?;
        let kind = flags & 7;
        if kind == 0 {
            read_children(r, version, origin, size / 2, cubes, nodes)?;
            continue;
        }
        if !(1..=4).contains(&kind) {
            return Err(format!("invalid cube type {kind} at byte {}", r.pos - 1));
        }
        let edges = if kind == 3 {
            r.bytes(12)?.try_into().unwrap()
        } else if kind == 1 {
            [0; 12]
        } else {
            [0x80; 12]
        };
        if edges.iter().any(|e| e & 15 > 8 || e >> 4 > 8) {
            return Err(format!("cube has an edge outside 0..8 at {origin:?}"));
        }
        let textures = [r.u16()?, r.u16()?, r.u16()?, r.u16()?, r.u16()?, r.u16()?];
        let mut volume_material = 0;
        if version <= 31 {
            let mask = r.byte()?;
            if mask & 0x80 != 0 {
                volume_material = r.byte()? as u16;
            }
            let mut surfaces = 6;
            let mut face = 0;
            while face < surfaces {
                if face >= 6 || mask & (1 << face) != 0 {
                    let surface = r.bytes(16)?;
                    if face < 6 {
                        if mask & 0x40 != 0 {
                            r.skip(12)?;
                        }
                        if surface[15] & 2 != 0 {
                            surfaces += 1;
                        }
                    }
                }
                face += 1;
            }
            if flags & 0x80 != 0 {
                let merged = r.byte()?;
                if merged & 0x80 != 0 {
                    let merge_mask = r.byte()?;
                    r.skip((merge_mask & 63).count_ones() as usize * 8)?;
                }
            }
        } else {
            if flags & 0x40 != 0 {
                volume_material = if version == 32 {
                    r.byte()? as u16
                } else {
                    r.u16()?
                };
            }
            if flags & 0x80 != 0 {
                r.skip(1)?;
            }
            if flags & 0x20 != 0 {
                let surfmask = r.byte()?;
                let total = r.byte()? as usize;
                let mut count = 0;
                if surfmask & !63 != 0 {
                    return Err("invalid surface mask".into());
                }
                for face in 0..6 {
                    if surfmask & (1 << face) == 0 {
                        continue;
                    }
                    let surface = r.bytes(4)?;
                    let mask = surface[2];
                    let layer = surface[3] & 15;
                    let duplicate = surface[3] & 128 != 0;
                    count += layer as usize * if duplicate { 2 } else { 1 };
                    if layer == 0 {
                        continue;
                    }
                    let mut xyz = mask & 4 != 0;
                    let mut uv = mask & 64 != 0;
                    let mut normal = mask & 128 != 0;
                    if layer == 4 {
                        if xyz && mask & 1 != 0 {
                            r.skip(8)?;
                            xyz = false;
                        }
                        if uv && mask & 2 != 0 {
                            r.skip(if duplicate { 16 } else { 8 })?;
                            uv = false;
                        }
                    }
                    if normal && mask & 8 != 0 {
                        r.skip(2)?;
                        normal = false;
                    }
                    r.skip(
                        layer as usize
                            * (usize::from(xyz) * 4
                                + usize::from(uv) * 4
                                + usize::from(normal) * 2),
                    )?;
                    if duplicate && uv {
                        r.skip(layer as usize * 4)?;
                    }
                }
                if count > total {
                    return Err("surface vertices exceed allocated count".into());
                }
            }
        }
        if kind == 4 {
            read_children(r, version, origin, size / 2, cubes, nodes)?;
        } else {
            cubes.push(Cube {
                origin,
                size,
                kind,
                edges,
                textures,
                volume_material,
            });
        }
    }
    Ok(())
}

#[derive(Debug, Serialize)]
pub struct ConfigReferences {
    pub literal_commands: Vec<ConfigCommand>,
    pub unhandled_lines: Vec<usize>,
}
#[derive(Debug, Serialize)]
pub struct ConfigCommand {
    pub line: usize,
    pub command: String,
    pub arguments: Vec<String>,
}
/// A report of literal references only. No cfg text is executed, expanded,
/// fetched, or followed, including exec paths and parent-relative model paths.
pub fn inspect_cfg(text: &str) -> ConfigReferences {
    let mut result = ConfigReferences {
        literal_commands: Vec::new(),
        unhandled_lines: Vec::new(),
    };
    for (i, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with("//") {
            continue;
        }
        let mut words = Vec::new();
        let mut word = String::new();
        let mut quoted = false;
        let mut chars = line.chars().peekable();
        let mut dynamic = false;
        while let Some(c) = chars.next() {
            if !quoted && c == '/' && chars.peek() == Some(&'/') {
                break;
            }
            if c == '"' {
                quoted = !quoted;
            } else if !quoted && "[]();$^".contains(c) {
                dynamic = true;
                break;
            } else if !quoted && c.is_whitespace() {
                if !word.is_empty() {
                    words.push(std::mem::take(&mut word));
                }
            } else {
                word.push(c);
            }
        }
        if !word.is_empty() {
            words.push(word);
        }
        if dynamic || quoted || words.is_empty() {
            result.unhandled_lines.push(i + 1);
            continue;
        }
        let command = words.remove(0);
        if [
            "texture",
            "texload",
            "texsky",
            "texturereset",
            "mmodel",
            "mapmodel",
            "mapmodelreset",
            "mapsound",
            "exec",
        ]
        .contains(&command.as_str())
        {
            result.literal_commands.push(ConfigCommand {
                line: i + 1,
                command,
                arguments: words,
            });
        } else {
            result.unhandled_lines.push(i + 1);
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::{write::GzEncoder, Compression};
    use std::io::Write;
    fn fixture(version: u32) -> Vec<u8> {
        let mut data = b"OCTA".to_vec();
        let mut fields = vec![
            version,
            if version == 29 { 36 } else { 40 },
            16,
            0,
            0,
            0,
            0,
            0,
        ];
        if version >= 30 {
            fields.push(0);
        }
        for n in fields {
            data.extend(n.to_le_bytes());
        }
        data.extend(b"\x03fps\0\0\0\0\0\0\0");
        for i in 0..8 {
            data.push(if i == 0 { 2 } else { 1 });
            for _ in 0..6 {
                data.extend(7u16.to_le_bytes());
            }
            if version <= 31 {
                data.push(0);
            }
        }
        data
    }
    #[test]
    fn reads_supported_old_and_new_octree_layouts() {
        for version in 29..=33 {
            let data = fixture(version);
            let map = read_octa(&data).unwrap();
            assert_eq!(map.header.version, version);
            assert_eq!(map.cubes.len(), 8);
            assert_eq!(map.inventory().nonempty_leaves, 1);
            assert_eq!(map.octree_end_offset, data.len());
            assert_eq!(map.cubes[0].origin, [0, 0, 0]);
            assert_eq!(map.cubes[7].origin, [8, 8, 8]);
            assert_eq!(map.cubes[0].textures, [7; 6]);
        }
    }

    #[test]
    fn reads_and_normalizes_materials_in_every_supported_version() {
        for version in 29..=33 {
            for (old, current) in [(1u8, 4u16), (19, 76), (34, 264), (136, 1056)] {
                let mut data = fixture(version);
                let octree = if version == 29 { 36 } else { 40 } + 11;
                data[octree] = 1; // An empty geometry leaf can still hold a volume.
                let raw = if version <= 32 {
                    u16::from(old)
                } else {
                    current
                };
                if version <= 31 {
                    data.splice(octree + 13..octree + 14, [0x80, old]);
                } else {
                    data[octree] |= 0x40;
                    let bytes = if version == 32 {
                        vec![old]
                    } else {
                        current.to_le_bytes().to_vec()
                    };
                    data.splice(octree + 13..octree + 13, bytes);
                }
                let map = read_octa(&data).unwrap();
                assert!(map.cubes[0].is_empty());
                assert_eq!(map.cubes[0].volume_material, raw);
                assert_eq!(map.cubes[0].normalized_material(version), current);
                assert_eq!(map.inventory().volume_material_leaves[&raw], 1);
                assert_eq!(map.inventory().normalized_material_leaves[&current], 1);
                assert_eq!(map.octree_end_offset, data.len());
            }
        }
    }

    #[test]
    fn version_33_preserves_material_indices_and_high_flags() {
        let mut data = fixture(33);
        let octree = 40 + 11;
        data[octree] |= 0x40;
        let material = 0x854fu16; // glass4, clip, and independent high flags.
        data.splice(octree + 13..octree + 13, material.to_le_bytes());
        let map = read_octa(&data).unwrap();
        assert_eq!(map.cubes[0].normalized_material(33), material);
        assert_eq!(map.octree_end_offset, data.len());
    }
    #[test]
    fn every_truncated_prefix_is_an_error_without_panic() {
        let data = fixture(33);
        for i in 0..data.len() {
            assert!(read_octa(&data[..i]).is_err(), "prefix {i}");
        }
    }
    #[test]
    fn validates_gzip_checksum_and_bounded_header_counts() {
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(&fixture(29)).unwrap();
        let mut gzip = encoder.finish().unwrap();
        assert!(read_ogz(&gzip).is_ok());
        let n = gzip.len();
        gzip[n - 8] ^= 1;
        assert!(read_ogz(&gzip).unwrap_err().contains("checksum"));
        let mut data = fixture(33);
        data[16..20].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(read_octa(&data).is_err());
    }
    #[test]
    fn skips_old_lightmap_surface_normals_and_merges_exactly() {
        let mut data = fixture(29);
        let octree = 36 + 11;
        data[octree] |= 0x80;
        // First cube: one surface + normals + one extra layer, and one merge.
        let mask_position = octree + 13;
        let mut metadata = vec![0xc1, 2]; // normals, material, first face; volume 2
        let mut surface = [0u8; 16];
        surface[15] = 2;
        metadata.extend(surface);
        metadata.extend([0; 12]);
        metadata.extend([0; 16]);
        metadata.extend([0x81, 1]);
        metadata.extend([0; 8]);
        data.splice(mask_position..mask_position + 1, metadata);
        let map = read_octa(&data).unwrap();
        assert_eq!(map.cubes[0].volume_material, 2);
        assert_eq!(map.octree_end_offset, data.len());
    }
    #[test]
    fn skips_compact_new_surfaces_with_duplicate_uv_layers() {
        let mut data = fixture(33);
        let octree = 40 + 11;
        data[octree] |= 0x20;
        let mut surface = vec![1, 8, 0, 0, 0xcf, 0x84]; // face0, 8 vertices, xyz+uv+normal rectangles, duplicate
        surface.extend([0; 8]);
        surface.extend([0; 16]);
        surface.extend([0; 2]);
        data.splice(octree + 13..octree + 13, surface);
        let map = read_octa(&data).unwrap();
        assert_eq!(map.octree_end_offset, data.len());
    }
    #[test]
    fn cfg_is_literal_reference_inventory_never_a_script_runner() {
        let cfg="// ignore\nmmodel \"../hudgun/railgun\"\nexec \"../../not-opened.cfg\"\ntexture 0 \"walls/rust.png\" // trailing\nexec (concatword $x .cfg)\n[quit]\n";
        let report = inspect_cfg(cfg);
        assert_eq!(report.literal_commands.len(), 3);
        assert_eq!(report.unhandled_lines, vec![5, 6]);
        assert_eq!(report.literal_commands[0].arguments, ["../hudgun/railgun"]);
    }
}
