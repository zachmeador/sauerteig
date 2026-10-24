//! World-file format, validation, and atomic disk writes.

use crate::{validate_cell, validate_object_position, MAX_OBJECTS, MAX_WORLD_FRAGMENTS};
use sauerteig_core::{arena_objects, validate_root_edits, CellEdit, RootEdit, World, WorldObject};
use serde::{Deserialize, Serialize};
use std::{collections::HashSet, path::Path};

const MAX_SAVE_BYTES: usize = 256 * 1024 * 1024;

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct SavedWorld {
    pub(super) revision: u64,
    pub(super) cells: Vec<CellEdit>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(super) roots: Vec<RootEdit>,
    pub(super) objects: Vec<WorldObject>,
}

pub(super) fn load_world(path: &Path) -> Result<SavedWorld, String> {
    if !path.exists() {
        return Ok(SavedWorld {
            revision: 0,
            cells: World::arena().cells(),
            roots: Vec::new(),
            objects: arena_objects(),
        });
    }
    let metadata = std::fs::metadata(path).map_err(|e| e.to_string())?;
    if metadata.len() > MAX_SAVE_BYTES as u64 {
        return Err("Saved world exceeds 256 MiB.".into());
    }
    let saved: SavedWorld =
        serde_json::from_slice(&std::fs::read(path).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
    if saved.revision == u64::MAX
        || saved.objects.len() > MAX_OBJECTS
        || saved.cells.len() + saved.roots.iter().map(|r| r.fragments.len()).sum::<usize>()
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
        if root.fragments.is_empty() || !seen.insert(root.pos) {
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
    Ok(saved)
}

pub(super) fn write_world(path: &Path, saved: &SavedWorld) -> Result<(), String> {
    use std::io::Write;
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let temporary = path.with_extension("json.tmp");
    let mut file = std::fs::File::create(&temporary).map_err(|e| e.to_string())?;
    serde_json::to_writer(&mut file, saved).map_err(|e| e.to_string())?;
    file.flush().map_err(|e| e.to_string())?;
    file.sync_all().map_err(|e| e.to_string())?;
    std::fs::rename(temporary, path).map_err(|e| e.to_string())
}
