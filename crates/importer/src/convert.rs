use crate::{
    reflect::mirror_shape,
    transform::{ImportTransform, RegionOverlap},
    Cube, Inventory, Map,
};
use sauerteig_core::{
    validate_material_volumes, Cell, CellEdit, CellFragment, FineBox, Leaf, LeafGeometry,
    MaterialVolume, NodeBox, ObjectKind, Octree, OctreeBuilder, RootEdit, Shape, Source, Vec3,
    WorldBounds, WorldObject, MAX_ROOT_SIZE, SHAPE_FACES, WORLD_FORMAT_VERSION,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

// Canonical version-33 fields after translating the older byte encoding.
const MATF_INDEX: u16 = 0x03;
const MATF_VOLUME: u16 = 0x1c;
const MATF_CLIP: u16 = 0xe0;
const MAT_WATER: u16 = 4;
const MAT_GLASS: u16 = 12;
const MAT_CLIP: u16 = 64;

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
    pub volumes: Vec<MaterialVolume>,
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

/// An entity left out because its position is not finite or lies beyond the
/// importer's coordinate limit. The position is in source units; a position
/// that is not a number is written as null.
#[derive(Clone, Debug, Serialize)]
pub struct OmittedEntity {
    pub index: usize,
    pub kind: u8,
    pub position: [f32; 3],
}

/// The imported octree: its root in eighth-metres and its leaves by kind.
#[derive(Clone, Debug, Default, Serialize)]
pub struct TreeReport {
    pub root_min_eighths: [i32; 3],
    pub root_size_eighths: u32,
    pub leaves: usize,
    pub full_leaves: usize,
    pub shaped_leaves: usize,
    pub piece_leaves: usize,
    /// Empty leaves that hold a material such as water or glass.
    pub material_leaves: usize,
    pub internal_nodes: usize,
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
    pub imported_water_leaves: usize,
    pub imported_glass_leaves: usize,
    pub imported_volume_boxes: usize,
    pub volume_leaves_outside_region: usize,
    pub volume_leaves_clipped_at_region: usize,
    /// Normalized volume type/index values whose behavior is not imported.
    pub unsupported_volume_material_leaves: BTreeMap<u16, usize>,
    /// Independent source flags, excluding the supported glass clip flag.
    pub material_flags_not_imported: BTreeMap<u16, usize>,
    /// Non-default water/glass styles currently use the standard appearance.
    pub volume_variants_not_imported: BTreeMap<u16, usize>,
    /// Source glass with no clip or an override becomes collidable glass.
    pub glass_leaves_with_collision_not_reproduced: usize,
    pub texture_slot_mapping: BTreeMap<u16, u8>,
    pub unmapped_texture_slot_faces: BTreeMap<u16, usize>,
    pub leaves_with_multiple_face_textures: usize,
    pub imported_lights: usize,
    pub imported_spawns: usize,
    pub entity_kinds_not_imported: BTreeMap<u8, usize>,
    pub entities_outside_region: usize,
    /// Entities with no usable position, left out of the world and its bounds.
    pub entities_omitted_out_of_range: Vec<OmittedEntity>,
    /// Entities that would take the world's bounds or the root past their
    /// limits, left out of a full import.
    pub entities_omitted_beyond_world: Vec<OmittedEntity>,
    /// The octree the world file was written from.
    pub tree: TreeReport,
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
            imported_water_leaves: 0,
            imported_glass_leaves: 0,
            imported_volume_boxes: 0,
            volume_leaves_outside_region: 0,
            volume_leaves_clipped_at_region: 0,
            unsupported_volume_material_leaves: BTreeMap::new(),
            material_flags_not_imported: BTreeMap::new(),
            volume_variants_not_imported: BTreeMap::new(),
            glass_leaves_with_collision_not_reproduced: 0,
            texture_slot_mapping: options.materials.clone(),
            unmapped_texture_slot_faces: BTreeMap::new(),
            leaves_with_multiple_face_textures: 0,
            imported_lights: 0,
            imported_spawns: 0,
            entity_kinds_not_imported: BTreeMap::new(),
            entities_outside_region: 0,
            entities_omitted_out_of_range: Vec::new(),
            entities_omitted_beyond_world: Vec::new(),
            tree: TreeReport::default(),
            notes: map
                .notes
                .iter()
                .cloned()
                .chain([
                "Original source files are untouched. No CubeScript is executed, and no referenced asset or include is loaded automatically.".into(),
                "Imported geometry uses dynamic lighting. Baked lightmaps, merged render surfaces, PVS, blend maps and sky rendering are not imported.".into(),
                "Texture slot references and virtual-slot metadata are retained in this report. Unmapped slots use neutral material 0; texture imagery is not inside OGZ.".into(),
                "Sauerbraten's axes are left-handed, so the map is mirrored (x, y, z) -> (-x, z, -y) into Sauerteig's right-handed Y-up axes; source +y stays -Z and east and west trade places.".into(),
                "Each logical cube face retains its mapped material. World face order [-X,+X,-Y,+Y,-Z,+Z] maps to legacy face slots [1,0,4,5,3,2].".into(),
                "Every source leaf is a leaf of the imported octree and keeps its full 16-bit material value there, including empty leaves; the world file keeps only water and glass, as adjacent matching boxes joined without resampling. Textured terrain remains independent.".into(),
                "Water/glass use current appearance and behavior. Original styles, lava, independent clip/noclip/gameclip, death and alpha flags are counted explicitly when not acted on; raw and normalized source values remain in the inventory.".into(),
                "Lights retain placement only: radius, color and spotlight attributes remain in the entity report. Mapmodels, pickups, triggers and sounds need later adapters.".into(),
            ])
            .collect(),
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
        FACE_SOURCE.map(|face| *materials.get(&cube.textures[face]).unwrap_or(&0))
    }

    fn record_source_material(&mut self, material: u16) {
        let volume = material & MATF_VOLUME;
        let index = material & MATF_INDEX;
        let mut flags = material & !(MATF_VOLUME | MATF_INDEX);
        if volume == MAT_GLASS && flags & MATF_CLIP == MAT_CLIP {
            // Source glass commonly carries MAT_CLIP. Our glass already clips.
            flags &= !MAT_CLIP;
        }
        if flags != 0 {
            *self.material_flags_not_imported.entry(flags).or_default() += 1;
        }
        if volume != 0 && volume != MAT_WATER && volume != MAT_GLASS {
            *self
                .unsupported_volume_material_leaves
                .entry(volume | index)
                .or_default() += 1;
        }
        if (volume == MAT_WATER || volume == MAT_GLASS) && index != 0 {
            *self
                .volume_variants_not_imported
                .entry(volume | index)
                .or_default() += 1;
        }
        if volume == MAT_GLASS && material & MATF_CLIP != MAT_CLIP {
            self.glass_leaves_with_collision_not_reproduced += 1;
        }
    }
}

/// The root's largest size for an imported entity: an entity that would take
/// the root past it, or the bounds past the world's limits, is left out.
const MAX_ENTITY_ROOT_SIZE: u32 = 65_536;

/// Source faces in the file's order: -x, +x, -y, +y, -z, +z.
/// Sauerteig's faces -X, +X, -Y, +Y, -Z, +Z are source faces +x, -x, -z, +z,
/// +y, -y: the reflection (x, y, z) -> (-x, z, -y) swaps the sides of the X
/// axis and sends source +y to -Z.
const FACE_SOURCE: [usize; 6] = [1, 0, 4, 5, 3, 2];

/// Each source face's four corners as (x, y, z) bits, in the original game's
/// winding, which its convexity rule for the diagonal depends on.
const SOURCE_FACES: [[[usize; 3]; 4]; 6] = [
    [[0, 1, 1], [0, 1, 0], [0, 0, 0], [0, 0, 1]],
    [[1, 1, 1], [1, 0, 1], [1, 0, 0], [1, 1, 0]],
    [[1, 0, 1], [0, 0, 1], [0, 0, 0], [1, 0, 0]],
    [[0, 1, 0], [0, 1, 1], [1, 1, 1], [1, 1, 0]],
    [[0, 0, 0], [0, 1, 0], [1, 1, 0], [1, 0, 0]],
    [[0, 0, 1], [1, 0, 1], [1, 1, 1], [0, 1, 1]],
];

/// Converts the map's octree into a Sauerteig octree without resampling.
/// Each authored leaf stays one leaf: its eight corners, face materials and
/// full material value remain, and eight equal siblings merge as the tree's
/// canonical form says. The world file is the tree written as format 3, which
/// keeps only water and glass of the material values until compact saves
/// arrive. Texture imagery, scripts, mapmodels and baked render data are not
/// read.
pub fn convert(
    map: &Map,
    options: &ImportOptions,
) -> Result<(ImportedWorld, ImportReport), String> {
    convert_to_tree(map, options).map(|(_, world, report)| (world, report))
}

/// `convert`, and the octree the world file was written from. The tree keeps
/// every leaf's full material value; the file keeps water and glass.
pub fn convert_to_tree(
    map: &Map,
    options: &ImportOptions,
) -> Result<(Octree, ImportedWorld, ImportReport), String> {
    let transform = ImportTransform::new(map.header.world_size, options)?;
    if options
        .materials
        .values()
        .any(|&m| !Cell::solid(m).is_valid())
    {
        return Err("material mapping contains an invalid Sauerteig material ID".into());
    }
    let grid = transform.source_grid(map.header.world_size)?;
    let mut bounds = transform.bounds;
    if !bounds.is_valid() {
        return Err("map exceeds supported compact world bounds; choose an explicit --center, --units-per-metre, or --crop region".into());
    }
    let stride = largest_leaf(map, &transform);
    let mut omitted = Vec::new();
    let mut beyond = Vec::new();
    let mut placed: Vec<&crate::Entity> = Vec::new();
    let mut root_limit = MAX_ENTITY_ROOT_SIZE;
    if !options.crop {
        root_limit = root_limit.max(import_root(bounds, grid, stride)?.size);
    }
    for (index, entity) in map.entities.iter().enumerate() {
        let position = transform.position(entity.position).to_array();
        if !position
            .into_iter()
            .all(|coordinate| coordinate.is_finite() && coordinate.abs() <= 1_048_576.0)
        {
            omitted.push(OmittedEntity {
                index,
                kind: entity.kind,
                position: entity.position,
            });
            continue;
        }
        // An entity outside the nominal legacy cube is still part of a full
        // import: the root grows to hold it, up to the world's limits.
        if !options.crop {
            let mut grown = bounds;
            for (axis, coordinate) in position.into_iter().enumerate() {
                grown.min[axis] = grown.min[axis].min(coordinate.floor() as i32);
                grown.max[axis] = grown.max[axis].max(coordinate.floor() as i32 + 1);
            }
            let fits = grown.is_valid()
                && import_root(grown, grid, stride).is_ok_and(|root| root.size <= root_limit);
            if !fits {
                beyond.push(OmittedEntity {
                    index,
                    kind: entity.kind,
                    position: entity.position,
                });
                continue;
            }
            bounds = grown;
        }
        placed.push(entity);
    }
    let world_bounds = bounds.fine_box();
    let root = import_root(bounds, grid, stride)?;
    let mut builder = OctreeBuilder::new(bounds, root)?;
    let mut report = ImportReport::new(map, options, &transform);
    if transform.requested_center != transform.center {
        report.notes.push(format!(
            "The crop's centre {:?} moved to {:?}, the nearest whole metre, so that the crop's edges fall on the tree's one-metre grid.",
            transform.requested_center, transform.center
        ));
    }
    report.entities_omitted_out_of_range = omitted;
    report.entities_omitted_beyond_world = beyond;
    for cube in &map.cubes {
        let material = cube.normalized_material(map.header.version);
        report.record_source_material(material);
        let volume = matches!(material & MATF_VOLUME, MAT_WATER | MAT_GLASS);
        if cube.is_empty() && material == 0 {
            continue;
        }
        if transform.overlap(cube) == RegionOverlap::Outside {
            report.leaves_outside_region += usize::from(!cube.is_empty());
            report.volume_leaves_outside_region += usize::from(volume);
            continue;
        }
        let source_bounds = transform.cube_bounds(cube)?;
        if source_bounds.intersection(world_bounds).is_none() {
            report.leaves_outside_region += usize::from(!cube.is_empty());
            report.volume_leaves_outside_region += usize::from(volume);
            continue;
        }
        if source_bounds.intersection(world_bounds) != Some(source_bounds) {
            report.leaves_clipped_at_region += usize::from(!cube.is_empty());
            report.volume_leaves_clipped_at_region += usize::from(volume);
        }
        match material & MATF_VOLUME {
            MAT_WATER => report.imported_water_leaves += 1,
            MAT_GLASS => report.imported_glass_leaves += 1,
            _ => {}
        }
        let at = NodeBox {
            min: source_bounds.min,
            size: (source_bounds.max[0] - source_bounds.min[0]) as u32,
        };
        if cube.is_empty() {
            push_leaf(&mut builder, root, at, Leaf::empty(material))?;
            continue;
        }
        let faces = report.face_materials_for(cube, &options.materials);
        let geometry = if cube.is_solid() {
            LeafGeometry::Full
        } else {
            let shape = cube_shape(cube);
            if let Err(reason) = check_shape(shape) {
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
                if material != 0 {
                    push_leaf(&mut builder, root, at, Leaf::empty(material))?;
                }
                continue;
            }
            LeafGeometry::Shaped(shape)
        };
        let leaf = Leaf {
            geometry,
            faces,
            cube_material: material,
        };
        push_leaf(&mut builder, root, at, leaf)?;
    }
    if !options.crop
        && (report.leaves_outside_region > 0
            || report.leaves_clipped_at_region > 0
            || report.volume_leaves_outside_region > 0
            || report.volume_leaves_clipped_at_region > 0)
    {
        return Err(
            "source geometry or material volumes lie outside declared legacy world bounds".into(),
        );
    }
    let tree = builder
        .finish()
        .map_err(|e| format!("imported geometry validation failed: {e}"))?;
    let stats = tree.stats();
    if stats.leaves() == 0 {
        return Err("the chosen region contains no importable geometry or material volumes".into());
    }
    let format3 = tree.to_format3();
    // The tree is valid by construction; the file also needs its boxes to fit
    // the format's limits, as the server checks them on load.
    validate_material_volumes(&format3.volumes, world_bounds)
        .map_err(|e| format!("imported material-volume validation failed: {e}"))?;
    report.tree = TreeReport {
        root_min_eighths: root.min,
        root_size_eighths: root.size,
        leaves: stats.leaves(),
        full_leaves: stats.full_leaves,
        shaped_leaves: stats.shaped_leaves,
        piece_leaves: stats.piece_leaves,
        material_leaves: stats.material_leaves,
        internal_nodes: stats.internal_nodes,
    };
    report.imported_volume_boxes = format3.volumes.len();
    report.imported_coarse_cells = format3.cells.len();
    report.imported_detailed_roots = 0;
    report.imported_regions = format3.regions.len();
    report.imported_fragments = format3.regions.len() + format3.cells.len();
    if report.imported_fragments > sauerteig_core::MAX_WORLD_PIECES {
        return Err(format!(
            "converted geometry has {} pieces, past the server's 786,432-piece limit",
            report.imported_fragments
        ));
    }
    let mut objects = Vec::new();
    for entity in placed {
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
        tree,
        ImportedWorld {
            format_version: WORLD_FORMAT_VERSION,
            bounds,
            regions: format3.regions,
            volumes: format3.volumes,
            revision: 0,
            cells: format3.cells,
            roots: Vec::new(),
            objects,
        },
        report,
    ))
}

/// The largest leaf in eighth-metres; the grid the root's origin may step on
/// when no root lies on the source's own grid.
fn largest_leaf(map: &Map, transform: &ImportTransform) -> u32 {
    let size = map.cubes.iter().map(|cube| cube.size).max().unwrap_or(1);
    transform.fine_size(size).unwrap_or(1).max(1)
}

/// The smallest root that holds the bounds and leaves every source leaf on
/// its grid, one of:
/// - a power-of-two cube on the source cube's own grid, so that each cube
///   above any leaf's size is one of the source octree's own nodes;
/// - the source cube's parent, grandparent and so on, which the source cube
///   is a node of, placed as the bounds need;
/// - when bounds that reach past the source cube on both sides of a grid line
///   leave those two nothing but a large root, a smaller one whose origin
///   steps down on the grid of the largest leaf.
fn import_root(bounds: WorldBounds, grid: FineBox, stride: u32) -> Result<NodeBox, String> {
    let fine = bounds.fine_box();
    let extent = (0..3)
        .map(|a| i64::from(fine.max[a]) - i64::from(fine.min[a]))
        .max()
        .unwrap();
    let grid_size = i64::from(grid.max[0] - grid.min[0]);
    let origin_on = |step: i64| -> [i32; 3] {
        std::array::from_fn(|a| {
            let below = i64::from(fine.min[a]) - i64::from(grid.min[a]);
            (i64::from(grid.min[a]) + below.div_euclid(step) * step) as i32
        })
    };
    let size_from = |min: [i32; 3]| -> u64 {
        let extent = (0..3)
            .map(|a| i64::from(fine.max[a]) - i64::from(min[a]))
            .max()
            .unwrap();
        (extent as u64).next_power_of_two().max(8)
    };
    let mut best: Option<NodeBox> = None;
    let mut consider = |min: [i32; 3], size: u64| {
        if size <= u64::from(MAX_ROOT_SIZE) && best.is_none_or(|b| size < u64::from(b.size)) {
            best = Some(NodeBox {
                min,
                size: size as u32,
            });
        }
    };
    // Aligned on the grid at the root's own size.
    let mut size = (extent as u64).next_power_of_two().max(8);
    for _ in 0..3 {
        let min = origin_on(size as i64);
        if (0..3).all(|a| i64::from(min[a]) + size as i64 >= i64::from(fine.max[a])) {
            consider(min, size);
            break;
        }
        size *= 2;
    }
    let min = origin_on(grid_size.max(1));
    consider(min, size_from(min));
    let min = origin_on(i64::from(stride.max(1)));
    consider(min, size_from(min));
    best.ok_or_else(|| "the map needs a root larger than the largest tree; choose --crop".into())
}

/// Adds a source leaf. One that sits on the root's grid goes in as it is. A
/// leaf larger than the root, or off its grid, goes in as the part of itself
/// inside each node of the root's grid it covers: the same material, and for
/// a shape a piece of its source, which later edits cut exactly.
fn push_leaf(
    builder: &mut OctreeBuilder,
    root: NodeBox,
    at: NodeBox,
    leaf: Leaf,
) -> Result<(), String> {
    let on_grid = at.size <= root.size
        && (0..3).all(|a| {
            at.min[a] >= root.min[a]
                && at.min[a] + at.size as i32 <= root.min[a] + root.size as i32
                && (at.min[a] - root.min[a]) % at.size as i32 == 0
        });
    if on_grid {
        return builder.push(at, leaf);
    }
    let clip = at.fine_box();
    let mut nodes = Vec::new();
    nodes_inside(root, clip, &mut nodes);
    for node in nodes {
        let part = match leaf.geometry {
            LeafGeometry::Shaped(shape) => Leaf {
                geometry: LeafGeometry::Piece(Source {
                    origin: at.min,
                    size: at.size,
                    shape,
                }),
                ..leaf
            },
            _ => leaf,
        };
        builder.push(node, part)?;
    }
    Ok(())
}

/// The maximal nodes of the root's grid inside a box.
fn nodes_inside(node: NodeBox, clip: FineBox, out: &mut Vec<NodeBox>) {
    let own = node.fine_box();
    if (0..3).any(|a| own.max[a] <= clip.min[a] || own.min[a] >= clip.max[a]) {
        return;
    }
    if clip.contains(own) {
        out.push(node);
        return;
    }
    let half = node.size / 2;
    for i in 0..8 {
        nodes_inside(
            NodeBox {
                min: std::array::from_fn(|a| node.min[a] + (i >> a & 1) * half as i32),
                size: half,
            },
            clip,
            out,
        );
    }
}

/// Which source corner, as (x, y, z) bits, sits in each of the lattice's eight
/// slots under the reflection (x, y, z) -> (-x, z, -y): slot bits (X, Y, Z)
/// become source bits (1 - X, 1 - Z, Y).
const SLOT_SOURCE_BITS: [[usize; 3]; 8] = [
    [1, 1, 0],
    [0, 1, 0],
    [0, 0, 0],
    [1, 0, 0],
    [1, 1, 1],
    [0, 1, 1],
    [0, 0, 1],
    [1, 0, 1],
];

fn slot_source_bits(slot: usize) -> [usize; 3] {
    SLOT_SOURCE_BITS[slot]
}

/// The cube's corners from its twelve edge nibbles, reflected into
/// Sauerteig's axes, with the original game's convexity-selected diagonal for
/// each face.
fn cube_shape(cube: &Cube) -> Shape {
    // The original's getcubevector: each coordinate comes from the edges
    // along its own axis.
    let source_corner = |[x, y, z]: [usize; 3]| -> [u8; 3] {
        let edge = |axis: usize, u: usize, v: usize, side: usize| {
            let b = cube.edges[axis * 4 + v * 2 + u];
            if side == 0 {
                b & 15
            } else {
                b >> 4
            }
        };
        [edge(0, y, z, x), edge(1, z, x, y), edge(2, x, y, z)]
    };
    let mut vertices = [[0; 3]; 8];
    for (slot, vertex) in vertices.iter_mut().enumerate() {
        let [x, y, z] = source_corner(slot_source_bits(slot));
        *vertex = [8 - x, z, 8 - y];
    }
    let mut diagonals = 0;
    for (face, quad) in SHAPE_FACES.iter().enumerate() {
        let corners = SOURCE_FACES[FACE_SOURCE[face]];
        let [a, b, c, d] = corners.map(|bits| Vec3::from_array(source_corner(bits).map(f32::from)));
        let flip = (a - d).dot((b - a).cross(c - a)) < 0.0;
        let chosen = if flip {
            [corners[1], corners[3]]
        } else {
            [corners[0], corners[2]]
        };
        let slot_of = |bits: [usize; 3]| (0..8).find(|&slot| slot_source_bits(slot) == bits);
        let chosen = chosen.map(|bits| slot_of(bits).unwrap());
        if !(chosen.contains(&quad[0]) && chosen.contains(&quad[2])) {
            diagonals |= 1 << face;
        }
    }
    // A shape whose top is a height field keeps only the top's diagonal, as
    // `Cell::as_shape` gives it, and imports always did: the same shape
    // before the reflection, mirrored. The other faces are planar, so only a
    // triangular side could tell the difference, and it draws the same
    // triangle either way. This keeps a re-import equal to the old copy
    // reflected.
    let solid = Shape::solid();
    let height_field = (0..4).all(|i| {
        vertices[i] == solid.vertices[i]
            && vertices[i + 4][0] == solid.vertices[i + 4][0]
            && vertices[i + 4][2] == solid.vertices[i + 4][2]
    });
    if height_field {
        let rotated = mirror_shape(Shape {
            vertices,
            diagonals,
        });
        let kept = if rotated.diagonals & (1 << 3) != 0 {
            (1 << 2) | (1 << 3)
        } else {
            0
        };
        return mirror_shape(Shape {
            vertices: rotated.vertices,
            diagonals: kept,
        });
    }
    Shape {
        vertices,
        diagonals,
    }
}

fn check_shape(shape: Shape) -> Result<(), String> {
    if shape.is_valid() {
        Ok(())
    } else {
        Err("legacy degenerate or non-manifold corner shape is outside current editable-shape rules".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Header, LodNodes};
    use sauerteig_core::{
        coalesce_material_volumes, FineBox, MaterialVolume, VolumeMaterial, World,
    };
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
                legacy: None,
            },
            variables: BTreeMap::new(),
            entities: Vec::new(),
            vslots: Vec::new(),
            cubes,
            lod: LodNodes::default(),
            decompressed_bytes: 0,
            octree_end_offset: 0,
            nodes: 1,
            notes: Vec::new(),
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

    fn empty_volume(origin: [i32; 3], size: u32, material: u16) -> Cube {
        Cube {
            origin,
            size,
            kind: 1,
            edges: [0; 12],
            textures: [9; 6],
            volume_material: material,
        }
    }

    #[test]
    fn empty_water_and_glass_import_without_solid_geometry_and_survive_save() {
        let source = map(vec![
            empty_volume([0, 0, 0], 8, 4),
            empty_volume([16, 0, 0], 8, 76),
        ]);
        let (save, report) = convert(&source, &options()).unwrap();
        assert!(save.regions.is_empty() && save.cells.is_empty() && save.roots.is_empty());
        assert_eq!(report.imported_water_leaves, 1);
        assert_eq!(report.imported_glass_leaves, 1);
        assert_eq!(report.imported_volume_boxes, 2);
        assert!(report.material_flags_not_imported.is_empty());
        assert!(report.unmapped_texture_slot_faces.is_empty());
        let restored: ImportedWorld =
            serde_json::from_slice(&serde_json::to_vec(&save).unwrap()).unwrap();
        assert_eq!(restored.volumes, save.volumes);
        assert!(save.volumes.contains(&MaterialVolume {
            bounds: FineBox {
                min: [-8, 0, -8],
                max: [0, 8, 0],
            },
            material: VolumeMaterial::Water,
        }));
        assert!(save.volumes.contains(&MaterialVolume {
            bounds: FineBox {
                min: [-24, 0, -8],
                max: [-16, 8, 0],
            },
            material: VolumeMaterial::Glass,
        }));
    }

    #[test]
    fn deformed_terrain_and_its_empty_space_material_remain_independent() {
        let terrain = ramp();
        let mut with_water = terrain.clone();
        with_water.volume_material = 4;
        let (plain, _) = convert(&map(vec![terrain]), &options()).unwrap();
        let (water, report) = convert(&map(vec![with_water]), &options()).unwrap();
        assert_eq!(water.regions, plain.regions);
        assert_eq!(water.volumes.len(), 1);
        assert_eq!(
            water.volumes[0].bounds,
            plain.regions[0].source_bounds().unwrap()
        );
        assert_eq!(water.volumes[0].material, VolumeMaterial::Water);
        assert_eq!(report.imported_water_leaves, 1);
    }

    #[test]
    fn legacy_byte_and_current_materials_convert_to_the_same_volumes() {
        for version in 29..=33 {
            let mut source = map(vec![
                empty_volume([0, 0, 0], 8, if version <= 32 { 1 } else { 4 }),
                empty_volume([16, 0, 0], 8, if version <= 32 { 19 } else { 76 }),
            ]);
            source.header.version = version;
            let (save, report) = convert(&source, &options()).unwrap();
            assert_eq!(save.volumes.len(), 2);
            assert_eq!(report.imported_water_leaves, 1);
            assert_eq!(report.imported_glass_leaves, 1);
            assert!(report.material_flags_not_imported.is_empty());
        }
    }

    #[test]
    fn unsupported_source_behaviors_and_style_variants_are_reported() {
        let source = map(vec![
            empty_volume([0, 0, 0], 8, 7),     // water4
            empty_volume([8, 0, 0], 8, 79),    // glass4 + clip
            empty_volume([16, 0, 0], 8, 264),  // lava + death
            empty_volume([24, 0, 0], 8, 64),   // independent clip
            empty_volume([32, 0, 0], 8, 32),   // noclip
            empty_volume([40, 0, 0], 8, 96),   // gameclip
            empty_volume([48, 0, 0], 8, 1024), // alpha
            empty_volume([56, 0, 0], 8, 12),   // glass without clip
        ]);
        let (save, report) = convert(&source, &options()).unwrap();
        assert_eq!(save.volumes.len(), 3);
        assert_eq!(
            report.unsupported_volume_material_leaves,
            BTreeMap::from([(8, 1)])
        );
        assert_eq!(
            report.material_flags_not_imported,
            BTreeMap::from([(32, 1), (64, 1), (96, 1), (256, 1), (1024, 1)])
        );
        assert_eq!(
            report.volume_variants_not_imported,
            BTreeMap::from([(7, 1), (15, 1)])
        );
        assert_eq!(report.glass_leaves_with_collision_not_reproduced, 1);
        assert_eq!(report.source.volume_material_leaves.len(), 8);
    }

    #[test]
    fn material_volumes_keep_original_scale_and_require_explicit_crop() {
        let mut source = map(vec![empty_volume([0; 3], 1024, 4)]);
        source.header.world_size = 1024;
        let mut opts = options();
        let (full, report) = convert(&source, &opts).unwrap();
        assert_eq!(full.volumes[0].bounds, full.bounds.fine_box());
        assert_eq!(full.bounds.min, [-128, 0, -128]);
        assert_eq!(full.bounds.max, [0, 128, 0]);
        assert_eq!(report.volume_leaves_clipped_at_region, 0);
        opts.crop = true;
        let (cropped, report) = convert(&source, &opts).unwrap();
        assert_eq!(report.volume_leaves_clipped_at_region, 1);
        assert_eq!(report.leaves_clipped_at_region, 0);
        assert_eq!(
            cropped.volumes[0].bounds,
            FineBox {
                min: [-512, 0, -512],
                max: [0, 384, 0],
            }
        );
        let outside = map(vec![empty_volume([512, 0, 0], 8, 4)]);
        assert!(convert(&outside, &options())
            .unwrap_err()
            .contains("outside"));
        let too_fine = map(vec![empty_volume([0; 3], 1, 4)]);
        opts.crop = false;
        opts.units_per_metre = 16;
        assert!(convert(&too_fine, &opts)
            .unwrap_err()
            .contains("finer than"));
    }

    #[test]
    fn adjacent_volume_merging_preserves_gaps_and_material_boundaries() {
        let box_at = |min: [i32; 3], material| MaterialVolume {
            bounds: FineBox {
                min,
                max: min.map(|v| v + 8),
            },
            material,
        };
        let cubes: Vec<_> = (0..8)
            .map(|i| {
                box_at(
                    std::array::from_fn(|a| if i & (1 << a) == 0 { 0 } else { 8 }),
                    VolumeMaterial::Water,
                )
            })
            .collect();
        let merged = coalesce_material_volumes(cubes);
        assert_eq!(merged.len(), 1);
        assert_eq!(
            merged[0].bounds,
            FineBox {
                min: [0; 3],
                max: [16; 3]
            }
        );

        let original = vec![
            box_at([0, 0, 0], VolumeMaterial::Water),
            box_at([8, 0, 0], VolumeMaterial::Water),
            box_at([0, 8, 0], VolumeMaterial::Water),
            box_at([8, 8, 0], VolumeMaterial::Glass),
            box_at([24, 0, 0], VolumeMaterial::Water),
        ];
        let merged = coalesce_material_volumes(original.clone());
        assert_eq!(merged.len(), 4);
        for x in 0..32 {
            for y in 0..16 {
                let point = FineBox {
                    min: [x, y, 0],
                    max: [x + 1, y + 1, 1],
                };
                let material_at = |volumes: &[MaterialVolume]| {
                    volumes
                        .iter()
                        .find(|v| v.bounds.contains(point))
                        .map(|v| v.material)
                };
                assert_eq!(material_at(&original), material_at(&merged));
            }
        }
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
        // The ramp rises toward source +x, which is Sauerteig's -X.
        for x in [0.125, 1.25, 3.375, 6.875] {
            let hit = world
                .raycast(Vec3::new(-x, 12.0, -3.5), -Vec3::Y, 20.0)
                .unwrap();
            assert!((hit.point.y - x).abs() < 1e-4, "{x}: {:?}", hit.point);
        }
        assert!(world.intersects_box(Vec3::new(-2.1, 1.8, -3.0), Vec3::new(-2.0, 1.9, -2.9)));
        assert!(!world.intersects_box(Vec3::new(-2.1, 2.3, -3.0), Vec3::new(-2.0, 2.4, -2.9)));
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
        assert_eq!(full.bounds.min, [-128, 0, -128]);
        assert_eq!(full.bounds.max, [0, 128, 0]);
        opts.crop = true;
        let (cropped, report) = convert(&map, &opts).unwrap();
        assert_eq!(cropped.bounds, WorldBounds::default());
        // The clipped slab is a union of the tree's nodes, not one region.
        let volume = |b: FineBox| {
            (0..3)
                .map(|a| i64::from(b.max[a] - b.min[a]))
                .product::<i64>()
        };
        assert_eq!(
            cropped
                .regions
                .iter()
                .map(|r| volume(r.bounds))
                .sum::<i64>()
                + cropped.cells.len() as i64 * 512,
            // The crop holds a quarter of the source cube's footprint.
            512 * 384 * 512
        );
        assert_eq!(report.leaves_clipped_at_region, 1);
        assert!(report.crop_requested);
    }

    /// The mapping this importer used while it rotated instead of reflecting:
    /// (x, y, z) -> (x, z, -y), with its face order and diagonal choice.
    fn rotated_shape(cube: &Cube) -> Shape {
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
        let source_faces = [
            [4, 0, 3, 7],
            [5, 6, 2, 1],
            [6, 7, 3, 2],
            [0, 4, 5, 1],
            [3, 0, 1, 2],
            [7, 6, 5, 4],
        ];
        let mut diagonals = 0;
        for (face, &source_face) in [0, 1, 4, 5, 3, 2].iter().enumerate() {
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
        Shape {
            vertices,
            diagonals,
        }
    }

    #[test]
    fn the_reflected_mapping_is_the_old_rotation_mirrored_across_x() {
        let mut state = 0x2545_f491_4f6c_dd1du64;
        let mut next = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        let mut compared = 0;
        for round in 0..20_000 {
            let mut cube = ramp();
            for edge in &mut cube.edges {
                // Mostly the corners of a plain cube, with random pushes.
                let (lo, hi) = if next() % 3 == 0 {
                    ((next() % 9) as u8, (next() % 9) as u8)
                } else {
                    (0, 8)
                };
                *edge = lo | hi << 4;
            }
            if round % 50 == 0 {
                cube.edges = [0x80; 12];
            }
            let new = cube_shape(&cube);
            let rotated = rotated_shape(&cube);
            let mirrored = crate::reflect::mirror_shape(rotated);
            assert_eq!(new.vertices, mirrored.vertices, "{:?}", cube.edges);
            // A height field was a cell: only its top's diagonal survived, and
            // as_shape gave the others.
            let solid = Shape::solid();
            let height_field = (0..4).all(|i| {
                mirrored.vertices[i] == solid.vertices[i]
                    && mirrored.vertices[i + 4][0] == solid.vertices[i + 4][0]
                    && mirrored.vertices[i + 4][2] == solid.vertices[i + 4][2]
            });
            let mut expected = mirrored;
            if height_field {
                let kept = if rotated.diagonals & (1 << 3) != 0 {
                    12
                } else {
                    0
                };
                expected = crate::reflect::mirror_shape(Shape {
                    diagonals: kept,
                    ..rotated
                });
            }
            assert_eq!(new, expected, "{:?}", cube.edges);
            compared += 1;
        }
        assert_eq!(compared, 20_000);
    }

    /// A cube with one corner sheared, so all six faces keep triangles.
    fn sheared() -> Cube {
        let mut cube = ramp();
        cube.edges = [0x80; 12];
        cube.edges[2] = 0x82;
        cube
    }

    #[test]
    fn all_legacy_face_slots_follow_the_geometry_reflection() {
        let mut cube = sheared();
        cube.textures = [10, 11, 12, 13, 14, 15];
        let mut opts = options();
        opts.materials = (0..6)
            .map(|material| (material as u16 + 10, material))
            .collect();
        let (save, report) = convert(&map(vec![cube]), &opts).unwrap();
        let cell = save.regions[0].cell;
        // -X, +X, -Y, +Y, -Z, +Z come from source +x, -x, -z, +z, +y, -y.
        assert_eq!(cell.face_materials, Some([1, 0, 4, 5, 3, 2]));
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
        assert!(save.regions.is_empty() && save.roots.is_empty() && save.volumes.is_empty());
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
        assert_eq!(save.bounds.min[0], -65);
        assert_eq!(save.bounds.max, [0, 76, 2]);
    }
    #[test]
    fn entities_without_a_usable_position_are_left_out_and_listed() {
        let mut map = map(vec![ramp()]);
        for (kind, position) in [
            (3, [520.0, -8.0, 600.0]),
            (3, [0.0, 0.0, -134_217_728.0]),
            (1, [f32::NAN, 0.0, 0.0]),
            (2, [f32::INFINITY, 0.0, 0.0]),
        ] {
            map.entities.push(crate::Entity {
                position,
                attributes: [0; 5],
                kind,
                extra: Vec::new(),
            });
        }
        for crop in [false, true] {
            let mut opts = options();
            opts.crop = crop;
            let (save, report) = convert(&map, &opts).unwrap();
            let omitted: Vec<_> = report
                .entities_omitted_out_of_range
                .iter()
                .map(|e| (e.index, e.kind))
                .collect();
            assert_eq!(omitted, [(1, 3), (2, 1), (3, 2)]);
            assert_eq!(
                report.entities_omitted_out_of_range[0].position[2],
                -134_217_728.0
            );
            assert_eq!(report.imported_spawns, usize::from(!crop));
            assert_eq!(save.objects.len(), usize::from(!crop));
            if !crop {
                assert_eq!(save.bounds.min[0], -65);
                assert_eq!(save.bounds.max, [0, 76, 2]);
            }
        }
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

    fn leaves(tree: &Octree) -> Vec<(NodeBox, Leaf)> {
        let mut out = Vec::new();
        tree.for_each_leaf(|at, leaf| out.push((at, leaf)));
        out
    }

    #[test]
    fn the_root_is_the_source_cube_and_grows_for_entities_beyond_it() {
        let mut source = map(vec![ramp()]);
        let (tree, _, report) = convert_to_tree(&source, &options()).unwrap();
        // 512 units at 8 per metre is 64 m, 512 eighths, on the source's grid.
        assert_eq!(
            tree.root_box(),
            NodeBox {
                min: [-512, 0, -512],
                size: 512
            }
        );
        assert_eq!(report.tree.root_size_eighths, 512);
        // An entity past the cube's +x side, which is Sauerteig's -X, doubles
        // the root: the source cube becomes one of its children.
        source.entities.push(crate::Entity {
            position: [520.0, 0.0, 192.0],
            attributes: [0; 5],
            kind: 3,
            extra: Vec::new(),
        });
        let (tree, save, report) = convert_to_tree(&source, &options()).unwrap();
        assert_eq!(save.objects.len(), 1);
        assert!(report.entities_omitted_beyond_world.is_empty());
        let root = tree.root_box();
        assert_eq!(root.size, 1024);
        assert!(root.fine_box().contains(save.bounds.fine_box()));
        // The source cube is one of the root's nodes.
        assert_eq!((root.min[0] + 512).rem_euclid(512), 0);
        assert_eq!(root.min[2].rem_euclid(512), 0);
    }

    #[test]
    fn entities_that_would_take_the_world_past_its_limits_are_left_out_and_reported() {
        let mut source = map(vec![ramp()]);
        for position in [[0.0, 0.0, 192.0], [100_000.0, 0.0, 192.0]] {
            source.entities.push(crate::Entity {
                position,
                attributes: [0; 5],
                kind: 3,
                extra: Vec::new(),
            });
        }
        let (_, save, report) = convert_to_tree(&source, &options()).unwrap();
        assert_eq!(save.objects.len(), 1);
        assert_eq!(report.entities_omitted_beyond_world.len(), 1);
        assert_eq!(report.entities_omitted_beyond_world[0].index, 1);
        assert_eq!(
            report.entities_omitted_beyond_world[0].position[0],
            100_000.0
        );
        assert!(report.entities_omitted_out_of_range.is_empty());
        assert!(save.bounds.is_valid());
    }

    #[test]
    fn a_centre_off_whole_metres_keeps_every_leaf_on_the_root_grid() {
        let mut opts = options();
        // 12 units is 1.5 m: the source cube's edges fall between metres.
        opts.center = Some([12.0, 4.0, 192.0]);
        let (tree, save, report) = convert_to_tree(&map(vec![ramp()]), &opts).unwrap();
        assert!(tree.root_box().fine_box().contains(save.bounds.fine_box()));
        assert_eq!(report.tree.piece_leaves, 0);
        assert_eq!(report.tree.shaped_leaves, 1);
        let [(at, _)] = leaves(&tree)[..] else {
            panic!("one leaf")
        };
        assert_eq!(at.size, 64);
        assert_eq!(at.min, [12 - 64, 0, 4 - 64]);
    }

    #[test]
    fn leaves_keep_their_full_material_value_in_the_tree_and_the_file_keeps_water_and_glass() {
        let mut solid = ramp();
        solid.kind = 2;
        solid.edges = [0x80; 12];
        solid.volume_material = 79 + 1024; // glass4 with clip, plus alpha
        let source = map(vec![
            solid,
            empty_volume([64, 0, 0], 8, 264), // lava with death
            empty_volume([72, 0, 0], 8, 4),
            empty_volume([80, 0, 0], 8, 0),
        ]);
        let (tree, save, report) = convert_to_tree(&source, &options()).unwrap();
        let found = leaves(&tree);
        let materials: Vec<u16> = found.iter().map(|(_, leaf)| leaf.cube_material).collect();
        let mut sorted = materials.clone();
        sorted.sort_unstable();
        assert_eq!(sorted, [4, 264, 1103]);
        let geometry = |material: u16| {
            found
                .iter()
                .find(|(_, leaf)| leaf.cube_material == material)
                .unwrap()
                .1
                .geometry
        };
        assert!(matches!(geometry(1103), LeafGeometry::Full));
        assert!(matches!(geometry(264), LeafGeometry::Empty));
        assert_eq!(report.tree.material_leaves, 2);
        // Only the glass and the water reach the file.
        assert_eq!(save.volumes.len(), 2);
        assert_eq!(
            report.unsupported_volume_material_leaves,
            BTreeMap::from([(8, 1)])
        );
        assert_eq!(report.volume_variants_not_imported[&15], 1);
        assert_eq!(
            report.material_flags_not_imported,
            BTreeMap::from([(256, 1), (1024, 1)])
        );
    }

    #[test]
    fn an_unsupported_leaf_in_water_keeps_its_water_when_it_is_skipped() {
        let mut cube = ramp();
        cube.edges = [0; 12];
        cube.edges[0] = 0x80;
        cube.volume_material = 4;
        let source = map(vec![cube]);
        assert!(convert(&source, &options()).is_err());
        let mut opts = options();
        opts.skip_unsupported = true;
        let (tree, save, report) = convert_to_tree(&source, &opts).unwrap();
        assert_eq!(report.unsupported_leaves_omitted.len(), 1);
        assert_eq!(save.volumes.len(), 1);
        assert_eq!(report.tree.material_leaves, 1);
        assert_eq!(tree.stats().full_leaves + tree.stats().shaped_leaves, 0);
    }

    #[test]
    fn leaves_larger_than_a_cropped_root_become_pieces_of_themselves() {
        // A 1024-unit (128 m) ramp and a solid sibling, cropped to 128 m.
        let mut source = map(vec![]);
        source.header.world_size = 2048;
        let mut big = ramp();
        big.size = 1024;
        let mut solid = big.clone();
        solid.origin = [1024, 0, 0];
        solid.kind = 2;
        solid.edges = [0x80; 12];
        source.cubes = vec![big, solid];
        let mut opts = ImportOptions {
            crop: true,
            center: Some([1024.0, 512.0, 1024.0]),
            ..ImportOptions::default()
        };
        opts.materials.insert(9, 3);
        let (tree, save, report) = convert_to_tree(&source, &opts).unwrap();
        assert_eq!(report.leaves_clipped_at_region, 2);
        assert!(report.tree.piece_leaves > 0);
        assert!(save.regions.iter().all(|r| r.size <= 8192));
        // The solid half fills its side of the crop: source x >= 1024 is -X.
        let found = leaves(&tree);
        assert!(found
            .iter()
            .any(|(_, leaf)| matches!(leaf.geometry, LeafGeometry::Full) && leaf.faces == [3; 6]));
        let reloaded = Octree::from_format3(&tree.to_format3()).unwrap();
        assert_eq!(reloaded.bounds(), tree.bounds());
    }

    #[test]
    fn a_round_trip_through_the_file_keeps_the_tree() {
        let source = map(vec![
            ramp(),
            sheared(),
            empty_volume([128, 0, 0], 8, 4),
            empty_volume([136, 0, 0], 8, 12),
        ]);
        let mut opts = options();
        opts.materials.insert(9, 2);
        let mut shifted = source.clone();
        shifted.cubes[1].origin = [64, 0, 0];
        let (tree, save, _) = convert_to_tree(&shifted, &opts).unwrap();
        let loaded = Octree::from_parts(
            save.bounds,
            &save.regions,
            &save.cells,
            &save.roots,
            &save.volumes,
        )
        .unwrap();
        assert_eq!(loaded.digest(), tree.digest());
    }

    #[test]
    fn the_slot_table_follows_the_reflection() {
        for (slot, vertex) in Shape::solid().vertices.iter().enumerate() {
            let [x, y, z] = vertex.map(|v| usize::from(v == 8));
            assert_eq!(slot_source_bits(slot), [1 - x, 1 - z, y], "slot {slot}");
        }
    }

    #[test]
    fn a_crop_moves_its_centre_to_whole_metres_so_its_edges_cost_few_leaves() {
        // A 256 m solid cube, cropped to 128 m around a centre 3/8 m off the
        // metre grid, in the middle of the solid.
        let mut source = map(vec![]);
        source.header.world_size = 2048;
        source.cubes = vec![Cube {
            origin: [0; 3],
            size: 2048,
            kind: 2,
            edges: [0x80; 12],
            textures: [0; 6],
            volume_material: 0,
        }];
        let mut opts = ImportOptions {
            crop: true,
            center: Some([1027.0, 1024.0, 1024.0]),
            ..ImportOptions::default()
        };
        let (_, save, report) = convert_to_tree(&source, &opts).unwrap();
        assert_eq!(report.center_source_xyz, [1024.0; 3]);
        assert!(report
            .notes
            .iter()
            .any(|n| n.contains("nearest whole metre")));
        // The crop is 128 x 48 x 128 m, a union of a few hundred nodes; a cut
        // off the metre grid would take hundreds of thousands.
        assert!(report.tree.leaves < 2000, "{}", report.tree.leaves);
        assert_eq!(save.bounds, WorldBounds::default());
        // An explicit centre already on the metre grid is left alone, and a
        // whole map keeps its centre however it falls.
        opts.center = Some([1032.0, 1024.0, 1024.0]);
        let (_, _, report) = convert_to_tree(&source, &opts).unwrap();
        assert_eq!(report.center_source_xyz, [1032.0, 1024.0, 1024.0]);
        assert!(!report
            .notes
            .iter()
            .any(|n| n.contains("nearest whole metre")));
        opts.crop = false;
        opts.center = Some([1027.0, 1024.0, 1024.0]);
        let (_, _, report) = convert_to_tree(&source, &opts).unwrap();
        assert_eq!(report.center_source_xyz, [1027.0, 1024.0, 1024.0]);
    }
}
