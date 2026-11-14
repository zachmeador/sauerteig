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

/// Only authored state crosses to save/join workers.
#[derive(Clone)]
pub(crate) struct SavedSnapshot {
    pub revision: u64,
    pub world: sauerteig_core::WorldSnapshot,
    pub objects: std::sync::Arc<Vec<WorldObject>>,
    pub next_object: u32,
}
impl SavedSnapshot {
    pub fn expand(&self) -> SavedWorld {
        SavedWorld {
            revision: self.revision,
            format_version: WORLD_FORMAT_VERSION,
            bounds: self.world.bounds(),
            regions: self.world.regions().to_vec(),
            cells: self.world.cells(),
            roots: self.world.root_overrides(),
            volumes: self.world.volumes().to_vec(),
            objects: self.objects.as_ref().clone(),
            next_object: self.next_object,
        }
    }
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

#[cfg(test)]
pub(super) fn write_world(path: &Path, saved: &SavedWorld) -> Result<(), String> {
    write_guarded(path, saved, || true)
}
pub(crate) fn write_snapshot(
    path: &Path,
    snapshot: &SavedSnapshot,
    can_commit: impl FnOnce() -> bool,
) -> Result<(), String> {
    write_guarded(path, &snapshot.expand(), can_commit)
}
fn write_guarded(
    path: &Path,
    saved: &SavedWorld,
    can_commit: impl FnOnce() -> bool,
) -> Result<(), String> {
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
    if !can_commit() {
        let _ = std::fs::remove_file(&temporary);
        return Err("Save cancelled because the simulation failed.".into());
    }
    durable_rename(&temporary, path).map_err(|e| e.to_string())
}

/// Replaces `to` with `from` and returns once the replacement is on disk, so a
/// power cut right after a save keeps either the old world or the new one.
#[cfg(unix)]
fn durable_rename(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::rename(from, to)?;
    // The rename lives in the folder, not the file, so the folder needs its own
    // sync. A folder that cannot be opened to sync (as on some network shares)
    // is not a failed save.
    let folder = to.parent().filter(|p| !p.as_os_str().is_empty());
    if let Ok(folder) = std::fs::File::open(folder.unwrap_or(Path::new("."))) {
        folder.sync_all()?;
    }
    Ok(())
}
/// Windows has no folder sync; `MOVEFILE_WRITE_THROUGH` makes the move itself
/// return only once it is on disk.
#[cfg(windows)]
fn durable_rename(from: &Path, to: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
    };
    let wide = |path: &Path| -> Vec<u16> { path.as_os_str().encode_wide().chain([0]).collect() };
    let (from, to) = (wide(from), wide(to));
    // SAFETY: `from` and `to` are NUL-terminated UTF-16 buffers that stay alive
    // and unmoved for the whole call, and `MoveFileExW` keeps neither pointer.
    let moved = unsafe {
        MoveFileExW(
            from.as_ptr(),
            to.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if moved == 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}
#[cfg(not(any(unix, windows)))]
fn durable_rename(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::rename(from, to)
}

fn legacy_format_version() -> u32 {
    1
}
