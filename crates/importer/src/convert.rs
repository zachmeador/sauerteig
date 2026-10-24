use crate::{
    transform::{ImportTransform, RegionOverlap},
    Cube, Inventory, Map,
};
use sauerteig_core::{
    Cell, CellEdit, CellFragment, ObjectKind, RootEdit, Shape, Vec3, World, WorldBounds,
    WorldObject, SHAPE_FACES, WORLD_FORMAT_VERSION,
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
    #[serde(default = "legacy_format_version")]
    pub format_version: u32,
    #[serde(default)]
    pub bounds: WorldBounds,
    #[serde(default)]
    pub regions: Vec<CellFragment>,
    #[serde(default)]
    pub revision: u64,
    #[serde(default)]
    pub cells: Vec<CellEdit>,
    #[serde(default)]
    pub roots: Vec<RootEdit>,
    #[serde(default)]
    pub objects: Vec<WorldObject>,
}

fn legacy_format_version() -> u32 {
    1
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
    pub imported_regions: usize,
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

impl ImportReport {
    fn new(map: &Map, options: &ImportOptions, transform: &ImportTransform) -> Self {
        Self {
            source: map.inventory(),
            units_per_metre: options.units_per_metre,
            center_source_xyz: transform.center,
            retained_source_bounds: transform.source_region,
            crop_requested: options.crop,
            leaves_outside_region: 0,
            leaves_clipped_at_region: 0,
            unsupported_leaves_omitted: Vec::new(),
            imported_coarse_cells: 0,
            imported_detailed_roots: 0,
            imported_regions: 0,
            imported_fragments: 0,
            texture_slot_mapping: options.materials.clone(),
            unmapped_texture_slot_faces: BTreeMap::new(),
            leaves_with_multiple_face_textures: 0,
            imported_lights: 0,
            imported_spawns: 0,
            entity_kinds_not_imported: BTreeMap::new(),
            entities_outside_region: 0,
            notes: vec![
                "Original source files are untouched. No CubeScript is executed, and no referenced asset or include is loaded automatically.".into(),
                "Imported geometry uses dynamic lighting. Baked lightmaps, merged render surfaces, PVS, blend maps and sky rendering are not imported.".into(),
                "Texture slot references and virtual-slot metadata are retained in this report. Unmapped slots use neutral material 0; texture imagery is not inside OGZ.".into(),
                "Each logical cube face retains its mapped material. World face order [-X,+X,-Y,+Y,-Z,+Z] maps to legacy face slots [0,1,4,5,3,2].".into(),
                "Lights retain placement only: radius, color and spotlight attributes remain in the entity report. Mapmodels, pickups, triggers, sounds and volume effects need later adapters.".into(),
            ],
        }
    }

    fn face_materials_for(&mut self, cube: &Cube, materials: &BTreeMap<u16, u8>) -> [u8; 6] {
        let mut counts = BTreeMap::new();
        for slot in cube.textures {
            *counts.entry(slot).or_insert(0) += 1;
            if !materials.contains_key(&slot) {
                *self.unmapped_texture_slot_faces.entry(slot).or_default() += 1;
            }
        }
        self.leaves_with_multiple_face_textures += usize::from(counts.len() > 1);
        [0, 1, 4, 5, 3, 2].map(|face| *materials.get(&cube.textures[face]).unwrap_or(&0))
    }
}

/// Converts geometry without resampling. Each authored octree leaf stays one
/// compact region; its original corner positions and logical face materials remain.
/// Texture imagery, scripts, mapmodels and baked render data are not executed.
pub fn convert(
    map: &Map,
    options: &ImportOptions,
) -> Result<(ImportedWorld, ImportReport), String> {
    let transform = ImportTransform::new(map.header.world_size, options)?;
    if options
        .materials
        .values()
        .any(|&m| !Cell::solid(m).is_valid())
    {
        return Err("material mapping contains an invalid Sauerteig material ID".into());
    }
    let mut bounds = transform.bounds;
    // An entity outside the nominal legacy cube is still part of a full import.
    // Include it in the explicit bounds instead of silently discarding it.
    if !options.crop {
        for entity in &map.entities {
            let position = transform.position(entity.position).to_array();
            for (axis, coordinate) in position.into_iter().enumerate() {
                if !coordinate.is_finite() || coordinate.abs() > 1_048_576.0 {
                    return Err("entity position exceeds supported import coordinates".into());
                }
                bounds.min[axis] = bounds.min[axis].min(coordinate.floor() as i32);
                bounds.max[axis] = bounds.max[axis].max(coordinate.floor() as i32 + 1);
            }
        }
    }
    if !bounds.is_valid() {
        return Err("map exceeds supported compact world bounds; choose an explicit --center, --units-per-metre, or --crop region".into());
    }
    let world_bounds = bounds.fine_box();
    let mut report = ImportReport::new(map, options, &transform);
    let mut regions = Vec::new();
    for cube in &map.cubes {
        if cube.is_empty() {
            continue;
        }
        if transform.overlap(cube) == RegionOverlap::Outside {
            report.leaves_outside_region += 1;
            continue;
        }
        let source_bounds = transform.cube_bounds(cube)?;
        let Some(bounds) = source_bounds.intersection(world_bounds) else {
            report.leaves_outside_region += 1;
            continue;
        };
        if bounds != source_bounds {
            report.leaves_clipped_at_region += 1;
        }
        let face_materials = report.face_materials_for(cube, &options.materials);
        let mut cell = match cube_cell(cube, face_materials[0]) {
            Ok(cell) => cell,
            Err(reason) => {
                if !options.skip_unsupported {
                    return Err(format!(
                        "unsupported solid at {:?} size {}: {reason}. Use --skip-unsupported \
                         only to explicitly omit it, or keep the original for future support.",
                        cube.origin, cube.size
                    ));
                }
                report.unsupported_leaves_omitted.push(GeometryIssue {
                    source_origin: cube.origin,
                    source_size: cube.size,
                    reason,
                });
                continue;
            }
        };
        if face_materials
            .iter()
            .any(|&material| material != cell.material)
        {
            cell.face_materials = Some(face_materials);
        }
        regions.push(CellFragment {
            bounds,
            origin: source_bounds.min,
            size: (source_bounds.max[0] - source_bounds.min[0]) as u32,
            cell,
        });
    }
    if !options.crop && (report.leaves_outside_region > 0 || report.leaves_clipped_at_region > 0) {
        return Err("source geometry lies outside its declared legacy world bounds".into());
    }
    let world = World::from_compact_parts(bounds, &regions, &[], &[])
        .map_err(|e| format!("imported geometry validation failed: {e}"))?;
    let regions = world.regions();
    let cells = world.cells();
    let roots = world.root_overrides();
    report.imported_coarse_cells = cells.len();
    report.imported_detailed_roots = roots.len();
    report.imported_regions = regions.len();
    report.imported_fragments =
        regions.len() + cells.len() + roots.iter().map(|r| r.fragments.len()).sum::<usize>();
    if report.imported_fragments == 0 {
        return Err("the chosen region contains no importable geometry".into());
    }
    let mut objects = Vec::new();
    for entity in &map.entities {
        let position = transform.position(entity.position);
        if (0..3).any(|axis| {
            position[axis] < bounds.min[axis] as f32 || position[axis] >= bounds.max[axis] as f32
        }) {
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
            format_version: WORLD_FORMAT_VERSION,
            bounds,
            regions,
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
        assert_eq!(save.regions.len(), 1);
        assert_eq!(save.regions[0].size, 64);
        assert!(save.cells.is_empty() && save.roots.is_empty());
        let bytes = serde_json::to_vec(&save).unwrap();
        let restored: ImportedWorld = serde_json::from_slice(&bytes).unwrap();
        let world = World::from_compact_parts(
            restored.bounds,
            &restored.regions,
            &restored.cells,
            &restored.roots,
        )
        .unwrap();
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
        let mut map = map(vec![cube]);
        map.header.world_size = 1024;
        let mut opts = options();
        let (full, report) = convert(&map, &opts).unwrap();
        assert_eq!(full.regions.len(), 1);
        assert_eq!(full.regions[0].size, 1024);
        assert_eq!(report.leaves_clipped_at_region, 0);
        assert_eq!(full.bounds.min, [0, 0, -128]);
        assert_eq!(full.bounds.max, [128, 128, 0]);
        opts.crop = true;
        let (cropped, report) = convert(&map, &opts).unwrap();
        assert_eq!(cropped.bounds, WorldBounds::default());
        assert_eq!(cropped.regions.len(), 1);
        assert_eq!(report.leaves_clipped_at_region, 1);
        assert!(report.crop_requested);
    }

    #[test]
    fn all_legacy_face_slots_follow_the_geometry_rotation() {
        let mut cube = ramp();
        cube.textures = [10, 11, 12, 13, 14, 15];
        let mut opts = options();
        opts.materials = (0..6)
            .map(|material| (material as u16 + 10, material))
            .collect();
        let (save, report) = convert(&map(vec![cube]), &opts).unwrap();
        let cell = save.regions[0].cell;
        assert_eq!(cell.face_materials, Some([0, 1, 4, 5, 3, 2]));
        assert!(report.unmapped_texture_slot_faces.is_empty());
        assert_eq!(report.leaves_with_multiple_face_textures, 1);
        let restored: ImportedWorld =
            serde_json::from_slice(&serde_json::to_vec(&save).unwrap()).unwrap();
        assert_eq!(restored.regions[0].cell, cell);
    }

    #[test]
    fn old_imported_worlds_default_to_legacy_bounds_and_material() {
        let save: ImportedWorld = serde_json::from_str(
            r#"{"cells":[{"pos":[0,0,0],"cell":{"material":3,"heights":[8,8,8,8]}}]}"#,
        )
        .unwrap();
        assert_eq!(save.format_version, 1);
        assert_eq!(save.bounds, WorldBounds::default());
        assert!(save.regions.is_empty() && save.roots.is_empty());
        assert_eq!(save.cells[0].cell.unwrap().material_for_face(5), 3);
    }

    #[test]
    fn full_import_keeps_entities_outside_the_nominal_source_cube() {
        let mut map = map(vec![ramp()]);
        map.entities.push(crate::Entity {
            position: [520.0, -8.0, 600.0],
            attributes: [0; 5],
            kind: 3,
            extra: Vec::new(),
        });
        let (save, report) = convert(&map, &options()).unwrap();
        assert_eq!(report.entities_outside_region, 0);
        assert_eq!(save.objects.len(), 1);
        assert_eq!(save.bounds.max, [66, 76, 2]);
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
