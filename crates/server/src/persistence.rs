//! World-file format, validation, and atomic disk writes.

use crate::{
    validate_cell, validate_object_position, validate_snapshot_geometry, MAX_OBJECTS,
    MAX_WORLD_FRAGMENTS, MAX_WORLD_SNAPSHOT_BYTES,
};
use sauerteig_core::{
    arena_objects, validate_root_edits, CellEdit, CellFragment, MaterialVolume, RootEdit, World,
    WorldBounds, WorldObject, WORLD_FORMAT_VERSION,
};
use serde::{Deserialize, Serialize};
use std::{collections::HashSet, path::Path};

const MAX_SAVE_BYTES: usize = MAX_WORLD_SNAPSHOT_BYTES;

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct SavedWorld {
    pub(super) revision: u64,
    #[serde(default = "legacy_format_version")]
    pub(super) format_version: u32,
    #[serde(default)]
    pub(super) bounds: WorldBounds,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(super) regions: Vec<CellFragment>,
    pub(super) cells: Vec<CellEdit>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(super) roots: Vec<RootEdit>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(super) volumes: Vec<MaterialVolume>,
    pub(super) objects: Vec<WorldObject>,
    #[serde(default)]
    pub(super) next_object: u32,
}

pub(super) fn load_world(path: &Path) -> Result<SavedWorld, String> {
    if !path.exists() {
        return Ok(SavedWorld {
            revision: 0,
            format_version: WORLD_FORMAT_VERSION,
            bounds: WorldBounds::default(),
            regions: Vec::new(),
            cells: World::arena().cells(),
            roots: Vec::new(),
            volumes: Vec::new(),
            objects: arena_objects(),
            next_object: 0,
        });
    }
    let metadata = std::fs::metadata(path).map_err(|e| e.to_string())?;
    if metadata.len() > MAX_SAVE_BYTES as u64 {
        return Err("Saved world exceeds the 64 MiB snapshot limit. Reduce geometry detail or import a smaller map.".into());
    }
    let saved: SavedWorld =
        serde_json::from_slice(&std::fs::read(path).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
    if !(1..=WORLD_FORMAT_VERSION).contains(&saved.format_version)
        || saved.objects.len() > MAX_OBJECTS
        || saved.regions.len()
            + saved.cells.len()
            + saved
                .roots
                .iter()
                .map(|r| r.fragments.len().max(1))
                .sum::<usize>()
            > MAX_WORLD_FRAGMENTS
    {
        return Err("Invalid saved world limits.".into());
    }
    validate_root_edits(&saved.roots)?;
    let mut seen = HashSet::new();
    for cell in &saved.cells {
        if !World::is_valid_pos(cell.pos)
            || cell.cell.is_none_or(|c| !validate_cell(c))
            || !seen.insert(cell.pos)
        {
            return Err("Saved world contains invalid or repeated cells.".into());
        }
    }
    for root in &saved.roots {
        if (saved.regions.is_empty() && root.fragments.is_empty()) || !seen.insert(root.pos) {
            return Err("Saved world contains empty or repeated detailed roots.".into());
        }
    }
    let mut ids = HashSet::new();
    for object in &saved.objects {
        validate_object_position(object.position, object.rotation)?;
        if !ids.insert(object.id) {
            return Err("Saved world contains repeated object IDs.".into());
        }
    }
    validate_snapshot_geometry(&saved.regions, &saved.cells, &saved.roots)?;
    if crate::snapshot_geometry_bytes(&saved.regions, &saved.cells, &saved.roots)
        + crate::json_bytes(&saved.volumes)
        > crate::MAX_GEOMETRY_BYTES
    {
        return Err(crate::SNAPSHOT_LIMIT_ERROR.into());
    }
    let mut world =
        World::from_compact_parts(saved.bounds, &saved.regions, &saved.cells, &saved.roots)?;
    world.replace_volumes(&saved.volumes)?;
    for object in &saved.objects {
        let position = object.position.floor().as_ivec3().to_array();
        if !saved.bounds.contains_pos(position) {
            return Err("Saved object is outside the world bounds.".into());
        }
    }
    Ok(saved)
}

pub(super) fn write_world(path: &Path, saved: &SavedWorld) -> Result<(), String> {
    use std::io::Write;
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let temporary = path.with_extension("json.tmp");
    let file = std::fs::File::create(&temporary).map_err(|e| e.to_string())?;
    let mut writer = std::io::BufWriter::new(file);
    serde_json::to_writer(&mut writer, saved).map_err(|e| e.to_string())?;
    writer.flush().map_err(|e| e.to_string())?;
    writer.get_ref().sync_all().map_err(|e| e.to_string())?;
    std::fs::rename(temporary, path).map_err(|e| e.to_string())
}

fn legacy_format_version() -> u32 {
    1
}
