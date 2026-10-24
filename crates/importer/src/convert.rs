use crate::{Cube, Inventory, Map};
use sauerteig_core::{
    Cell, CellEdit, CellFragment, FineBox, ObjectKind, RootEdit, Shape, Vec3, World, WorldObject,
    MAX_FRAGMENT_SOURCE_SIZE, SHAPE_FACES, WORLD_HEIGHT, WORLD_MAX, WORLD_MIN,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug)]
pub struct ImportOptions {
    /// Power of two; eight Cube units approximate the game's existing metre.
    pub units_per_metre: u32,
    /// Source XYZ (Z up), mapped to Sauerteig [0,24,0].
    pub center: Option<[f64; 3]>,
    /// Explicit permission to keep only the current bounded world region.
    pub crop: bool,
    /// Unsupported degenerate solids are an error unless explicitly omitted.
    pub skip_unsupported: bool,
    /// Cube texture/vslot indices to current Sauerteig material IDs.
    pub materials: BTreeMap<u16, u8>,
}
impl Default for ImportOptions {
    fn default() -> Self {
        Self {
            units_per_metre: 8,
            center: None,
            crop: false,
            skip_unsupported: false,
            materials: BTreeMap::new(),
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ImportedWorld {
    pub revision: u64,
    pub cells: Vec<CellEdit>,
    pub roots: Vec<RootEdit>,
    pub objects: Vec<WorldObject>,
}
#[derive(Clone, Debug, Serialize)]
pub struct GeometryIssue {
    pub source_origin: [i32; 3],
    pub source_size: u32,
    pub reason: String,
}
#[derive(Clone, Debug, Serialize)]
pub struct ImportReport {
    pub source: Inventory,
    pub units_per_metre: u32,
    pub center_source_xyz: [f64; 3],
    pub retained_source_bounds: [[f64; 3]; 2],
    pub crop_requested: bool,
    pub leaves_outside_region: usize,
    pub leaves_clipped_at_region: usize,
    pub unsupported_leaves_omitted: Vec<GeometryIssue>,
    pub imported_coarse_cells: usize,
    pub imported_detailed_roots: usize,
    pub imported_fragments: usize,
    pub texture_slot_mapping: BTreeMap<u16, u8>,
    pub unmapped_texture_slot_faces: BTreeMap<u16, usize>,
    pub leaves_with_multiple_face_textures: usize,
    pub imported_lights: usize,
    pub imported_spawns: usize,
    pub entity_kinds_not_imported: BTreeMap<u8, usize>,
    pub entities_outside_region: usize,
    pub notes: Vec<String>,
}

/// Converts geometry without resampling. Large authored shapes are clipped into
/// the engine's one-metre roots; their original eighth-corner positions remain.
/// Texture imagery, scripts, mapmodels and baked render data are not executed.
pub fn convert(
    map: &Map,
    options: &ImportOptions,
) -> Result<(ImportedWorld, ImportReport), String> {
    if !options.units_per_metre.is_power_of_two() || options.units_per_metre > 1024 {
        return Err("units-per-metre must be a power of two from 1 through 1024".into());
    }
    if options
        .materials
        .values()
        .any(|&m| !Cell::solid(m).is_valid())
    {
        return Err("material mapping contains an invalid Sauerteig material ID".into());
    }
    let size = f64::from(map.header.world_size);
    let center = options.center.unwrap_or([size / 2.0; 3]);
    let factor = 8.0 / f64::from(options.units_per_metre);
    let mut center_fine = [0i32; 3];
    for a in 0..3 {
        let v = center[a] * factor;
        if !v.is_finite() || v.fract().abs() > 1e-8 || v.abs() > 8_388_608.0 {
            return Err("center must align to Sauerteig's eighth-metre grid and be within import coordinate limits".into());
        }
        center_fine[a] = v as i32;
    }
    let world_bounds = FineBox {
        min: [WORLD_MIN * 8, 0, WORLD_MIN * 8],
        max: [WORLD_MAX * 8, WORLD_HEIGHT * 8, WORLD_MAX * 8],
    };
    let inv = map.inventory();
    let mut report = ImportReport {
        source: inv, units_per_metre: options.units_per_metre, center_source_xyz: center,
        retained_source_bounds: [
            [center[0]+f64::from(WORLD_MIN)*f64::from(options.units_per_metre), center[1]-f64::from(WORLD_MAX)*f64::from(options.units_per_metre), center[2]-f64::from(WORLD_HEIGHT/2)*f64::from(options.units_per_metre)],
            [center[0]+f64::from(WORLD_MAX)*f64::from(options.units_per_metre), center[1]-f64::from(WORLD_MIN)*f64::from(options.units_per_metre), center[2]+f64::from(WORLD_HEIGHT/2)*f64::from(options.units_per_metre)]
        ], crop_requested: options.crop, leaves_outside_region:0, leaves_clipped_at_region:0,
        unsupported_leaves_omitted:Vec::new(), imported_coarse_cells:0, imported_detailed_roots:0, imported_fragments:0,
        texture_slot_mapping:options.materials.clone(), unmapped_texture_slot_faces:BTreeMap::new(), leaves_with_multiple_face_textures:0,
        imported_lights:0, imported_spawns:0, entity_kinds_not_imported:BTreeMap::new(), entities_outside_region:0,
        notes: vec![
            "Original source files are untouched. No CubeScript is executed, and no referenced asset or include is loaded automatically.".into(),
            "Imported geometry uses dynamic lighting. Baked lightmaps, merged render surfaces, PVS, blend maps and sky rendering are not imported.".into(),
            "Texture slot references and virtual-slot metadata are retained in this report. Unmapped slots use neutral material 0; texture imagery is not inside OGZ.".into(),
            "The current Cell has one material, so a multi-textured cube uses its most frequent face texture (lowest slot breaks ties). Per-face legacy appearance is not preserved.".into(),
            "Lights retain placement only: radius, color and spotlight attributes remain in the entity report. Mapmodels, pickups, triggers, sounds and volume effects need later adapters.".into(),
        ],
    };
    if !options.crop {
        let mut outside = 0;
        let mut clipped = 0;
        for cube in map.cubes.iter().filter(|c| !c.is_empty()) {
            let min = cube.origin.map(f64::from);
            let max = min.map(|v| v + f64::from(cube.size));
            if (0..3).any(|a| {
                max[a] <= report.retained_source_bounds[0][a]
                    || min[a] >= report.retained_source_bounds[1][a]
            }) {
                outside += 1;
            } else if (0..3).any(|a| {
                min[a] < report.retained_source_bounds[0][a]
                    || max[a] > report.retained_source_bounds[1][a]
            }) {
                clipped += 1;
            }
        }
        if outside != 0 || clipped != 0 {
            return Err(format!("map does not fit the current {}×{}×{}m world: {outside} leaves outside and {clipped} crossing its boundary. Nothing was written. Set an explicit --center and --crop to import a region, or choose an exact larger scale.", WORLD_MAX-WORLD_MIN,WORLD_HEIGHT,WORLD_MAX-WORLD_MIN));
        }
    }
    let mut by_root: BTreeMap<[i32; 3], Vec<CellFragment>> = BTreeMap::new();
    let mut fragments = 0usize;
    for cube in &map.cubes {
        if cube.is_empty() {
            continue;
        }
        // Test coarse source bounds before requiring sub-grid coordinate fidelity:
        // a crop can safely omit a distant fine leaf without rounding its geometry.
        let min = [
            f64::from(cube.origin[0]),
            f64::from(cube.origin[1]),
            f64::from(cube.origin[2]),
        ];
        let max = min.map(|v| v + f64::from(cube.size));
        if (0..3).any(|a| {
            max[a] <= report.retained_source_bounds[0][a]
                || min[a] >= report.retained_source_bounds[1][a]
        }) {
            report.leaves_outside_region += 1;
            continue;
        }
        let size_numerator = cube.size.checked_mul(8).ok_or("source scale overflow")?;
        if size_numerator % options.units_per_metre != 0 {
            return Err(format!(
                "leaf at {:?} is finer than 1/8m at this scale; choose fewer units per metre",
                cube.origin
            ));
        }
        let source_size = size_numerator / options.units_per_metre;
        if source_size == 0 || source_size > MAX_FRAGMENT_SOURCE_SIZE {
            return Err(format!("source leaf at {:?} exceeds supported authored size ({} eighths); choose a larger units-per-metre value", cube.origin, MAX_FRAGMENT_SOURCE_SIZE));
        }
        let mut scaled = [0; 3];
        for a in 0..3 {
            let n = cube.origin[a]
                .checked_mul(8)
                .ok_or("source origin overflow")?;
            if n % options.units_per_metre as i32 != 0 {
                return Err(format!(
                    "leaf at {:?} cannot align to the 1/8m grid at this scale",
                    cube.origin
                ));
            }
            scaled[a] = n / options.units_per_metre as i32 - center_fine[a];
        }
        // Right-handed rotation from legacy Z-up to Y-up: (x,y,z)->(x,z,-y).
        let origin = [
            scaled[0],
            scaled[2] + WORLD_HEIGHT * 4,
            -scaled[1] - source_size as i32,
        ];
        let source_bounds = FineBox {
            min: origin,
            max: origin.map(|v| v + source_size as i32),
        };
        let Some(bounds) = source_bounds.intersection(world_bounds) else {
            report.leaves_outside_region += 1;
            continue;
        };
        if bounds != source_bounds {
            report.leaves_clipped_at_region += 1;
        }
        let mut counts = BTreeMap::new();
        for slot in cube.textures {
            *counts.entry(slot).or_insert(0) += 1;
            if !options.materials.contains_key(&slot) {
                *report.unmapped_texture_slot_faces.entry(slot).or_default() += 1;
            }
        }
        report.leaves_with_multiple_face_textures += usize::from(counts.len() > 1);
        let mut selected = 0;
        let mut best = 0;
        for (&slot, &count) in &counts {
            if count > best {
                selected = slot;
                best = count;
            }
        }
        let material = *options.materials.get(&selected).unwrap_or(&0);
        let cell = match cube_cell(cube, material) {
            Ok(cell) => cell,
            Err(reason) => {
                if !options.skip_unsupported {
                    return Err(format!("unsupported solid at {:?} size {}: {reason}. Use --skip-unsupported only to explicitly omit it, or keep the original for future support.", cube.origin,cube.size));
                }
                report.unsupported_leaves_omitted.push(GeometryIssue {
                    source_origin: cube.origin,
                    source_size: cube.size,
                    reason,
                });
                continue;
            }
        };
        let root_min = bounds.min.map(|v| v.div_euclid(8));
        let root_max = bounds.max.map(|v| (v - 1).div_euclid(8));
        for x in root_min[0]..=root_max[0] {
            for y in root_min[1]..=root_max[1] {
                for z in root_min[2]..=root_max[2] {
                    fragments += 1;
                    if fragments > 786_432 {
                        return Err("conversion exceeds the server's 786432-fragment budget; choose a smaller region or less detailed source".into());
                    }
                    let pos = [x, y, z];
                    by_root.entry(pos).or_default().push(CellFragment {
                        bounds: bounds.intersection(FineBox::root(pos)).unwrap(),
                        origin,
                        size: source_size,
                        cell,
                    });
                }
            }
        }
    }
    if !options.crop && (report.leaves_outside_region > 0 || report.leaves_clipped_at_region > 0) {
        return Err(format!("map does not fit the current {}×{}×{}m world: {} leaves outside and {} crossing its boundary. Nothing was written. Set an explicit --center and --crop to import a region, or choose an exact larger scale.", WORLD_MAX-WORLD_MIN,WORLD_HEIGHT,WORLD_MAX-WORLD_MIN,report.leaves_outside_region,report.leaves_clipped_at_region));
    }
    let roots: Vec<_> = by_root
        .into_iter()
        .map(|(pos, fragments)| RootEdit { pos, fragments })
        .collect();
    let world = World::from_parts(&[], &roots)
        .map_err(|e| format!("imported geometry validation failed: {e}"))?;
    let cells = world.cells();
    let roots = world.detailed_roots();
    report.imported_coarse_cells = cells.len();
    report.imported_detailed_roots = roots.len();
    report.imported_fragments =
        cells.len() + roots.iter().map(|r| r.fragments.len()).sum::<usize>();
    if report.imported_fragments == 0 {
        return Err("the chosen region contains no importable geometry".into());
    }
    let mut objects = Vec::new();
    for entity in &map.entities {
        let p = entity.position.map(f64::from);
        let position = Vec3::new(
            ((p[0] - center[0]) / f64::from(options.units_per_metre)) as f32,
            ((p[2] - center[2]) / f64::from(options.units_per_metre)) as f32
                + WORLD_HEIGHT as f32 / 2.0,
            (-(p[1] - center[1]) / f64::from(options.units_per_metre)) as f32,
        );
        if position.x < WORLD_MIN as f32
            || position.x >= WORLD_MAX as f32
            || position.y < 0.0
            || position.y >= WORLD_HEIGHT as f32
            || position.z < WORLD_MIN as f32
            || position.z >= WORLD_MAX as f32
        {
            report.entities_outside_region += 1;
            continue;
        }
        let kind = match entity.kind {
            1 => {
                report.imported_lights += 1;
                ObjectKind::Light
            }
            3 => {
                report.imported_spawns += 1;
                ObjectKind::Spawn
            }
            kind => {
                *report.entity_kinds_not_imported.entry(kind).or_default() += 1;
                continue;
            }
        };
        objects.push(WorldObject {
            id: objects.len() as u32 + 1,
            position,
            kind,
            rotation: 0.0,
        });
    }
    if objects.len() > 1024 {
        return Err("converted entities exceed the server's 1024-object limit".into());
    }
    if !options.crop && report.entities_outside_region > 0 {
        return Err(format!(
            "{} entities lie outside the target region; --crop is required to omit them explicitly",
            report.entities_outside_region
        ));
    }
    Ok((
        ImportedWorld {
            revision: 0,
            cells,
            roots,
            objects,
        },
        report,
    ))
}

fn cube_cell(cube: &Cube, material: u8) -> Result<Cell, String> {
    if cube.is_solid() {
        return Ok(Cell::solid(material));
    }
    let corner_bits = [
        [0, 1, 0],
        [1, 1, 0],
        [1, 0, 0],
        [0, 0, 0],
        [0, 1, 1],
        [1, 1, 1],
        [1, 0, 1],
        [0, 0, 1],
    ];
    let mut vertices = [[0; 3]; 8];
    for (i, [x, y, z]) in corner_bits.into_iter().enumerate() {
        let edge = |axis: usize, u: usize, v: usize, side: usize| {
            let b = cube.edges[axis * 4 + v * 2 + u];
            if side == 0 {
                b & 15
            } else {
                b >> 4
            }
        };
        vertices[i] = [edge(0, y, z, x), edge(2, x, y, z), 8 - edge(1, z, x, y)];
    }
    // Original Cube2 face order, transformed into our vertex order. Preserve
    // its convexity-selected triangle diagonal, rather than guessing a ramp.
    let source_faces = [
        [4, 0, 3, 7],
        [5, 6, 2, 1],
        [6, 7, 3, 2],
        [0, 4, 5, 1],
        [3, 0, 1, 2],
        [7, 6, 5, 4],
    ];
    let face_mapping = [0, 1, 4, 5, 3, 2];
    let mut diagonals = 0;
    for (face, &source_face) in face_mapping.iter().enumerate() {
        let indices = source_faces[source_face];
        let [a, b, c, d] = indices.map(|i| Vec3::from_array(vertices[i].map(f32::from)));
        let flip = (a - d).dot((b - a).cross(c - a)) < 0.0;
        let chosen = if flip {
            [indices[1], indices[3]]
        } else {
            [indices[0], indices[2]]
        };
        let quad = SHAPE_FACES[face];
        if !(chosen.contains(&quad[0]) && chosen.contains(&quad[2])) {
            diagonals |= 1 << face;
        }
    }
    let shape = Shape {
        vertices,
        diagonals,
    };
    let solid = Shape::solid();
    let height_field = (0..4).all(|i| {
        vertices[i] == solid.vertices[i]
            && vertices[i + 4][0] == solid.vertices[i + 4][0]
            && vertices[i + 4][2] == solid.vertices[i + 4][2]
    });
    let cell = if height_field {
        Cell {
            heights: std::array::from_fn(|i| vertices[i + 4][1]),
            diagonal: diagonals & (1 << 3) != 0,
            ..Cell::solid(material)
        }
    } else {
        Cell {
            shape: Some(shape),
            ..Cell::solid(material)
        }
    };
    if cell.is_valid() {
        Ok(cell)
    } else {
        Err("legacy degenerate or non-manifold corner shape is outside current editable-shape rules".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Header;
    fn map(cubes: Vec<Cube>) -> Map {
        Map {
            header: Header {
                version: 33,
                world_size: 512,
                entities: 0,
                pvs_cells: 0,
                lightmaps: 0,
                blendmap: 0,
                variables: 0,
                vslots: 0,
                game: "fps".into(),
            },
            variables: BTreeMap::new(),
            entities: Vec::new(),
            vslots: Vec::new(),
            cubes,
            decompressed_bytes: 0,
            octree_end_offset: 0,
            nodes: 1,
        }
    }
    fn options() -> ImportOptions {
        ImportOptions {
            center: Some([0.0, 0.0, 192.0]),
            ..ImportOptions::default()
        }
    }
    fn ramp() -> Cube {
        let mut cube = Cube {
            origin: [0, 0, 0],
            size: 64,
            kind: 3,
            edges: [0x80; 12],
            textures: [9; 6],
            volume_material: 0,
        };
        cube.edges[8] = 0;
        cube.edges[10] = 0;
        cube
    }
    #[test]
    fn large_legacy_ramp_keeps_exact_surface_collision_and_save_geometry() {
        let map = map(vec![ramp()]);
        let (save, report) = convert(&map, &options()).unwrap();
        assert!(report.unsupported_leaves_omitted.is_empty());
        assert!(save
            .roots
            .iter()
            .flat_map(|r| &r.fragments)
            .any(|f| f.size == 64));
        let bytes = serde_json::to_vec(&save).unwrap();
        let restored: ImportedWorld = serde_json::from_slice(&bytes).unwrap();
        let world = World::from_parts(&restored.cells, &restored.roots).unwrap();
        for x in [0.125, 1.25, 3.375, 6.875] {
            let hit = world
                .raycast(Vec3::new(x, 12.0, -3.5), -Vec3::Y, 20.0)
                .unwrap();
            assert!((hit.point.y - x).abs() < 1e-4, "{x}: {:?}", hit.point);
        }
        assert!(world.intersects_box(Vec3::new(2.0, 1.8, -3.0), Vec3::new(2.1, 1.9, -2.9)));
        assert!(!world.intersects_box(Vec3::new(2.0, 2.3, -3.0), Vec3::new(2.1, 2.4, -2.9)));
        assert_eq!(report.unmapped_texture_slot_faces[&9], 6);
    }
    #[test]
    fn whole_map_never_silently_crops_and_explicit_crop_reports_loss() {
        let cube = Cube {
            origin: [0, 0, 0],
            size: 1024,
            kind: 2,
            edges: [0x80; 12],
            textures: [0; 6],
            volume_material: 0,
        };
        let map = map(vec![cube]);
        let mut opts = options();
        assert!(convert(&map, &opts).unwrap_err().contains("does not fit"));
        opts.crop = true;
        let (_, report) = convert(&map, &opts).unwrap();
        assert_eq!(report.leaves_clipped_at_region, 1);
        assert!(report.crop_requested);
    }
    #[test]
    fn unsupported_shape_and_subgrid_scale_never_silently_disappear() {
        let mut cube = ramp();
        cube.edges = [0; 12];
        cube.edges[0] = 0x80;
        let err = convert(&map(vec![cube]), &options()).unwrap_err();
        assert!(err.contains("unsupported solid"));
        let mut cube = ramp();
        cube.size = 1;
        let mut opts = options();
        opts.units_per_metre = 16;
        assert!(convert(&map(vec![cube]), &opts)
            .unwrap_err()
            .contains("finer than"));
    }
}
