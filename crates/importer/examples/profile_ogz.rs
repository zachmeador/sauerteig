//! Repeatable original-scale import/storage/CPU profile. This is not a GPU benchmark.
use sauerteig_core::{forward, ObjectKind, RootEdit, Vec3, World};
use sauerteig_import::{convert, read_ogz, ImportOptions, ImportedWorld};
use std::{hint::black_box, time::Instant};

fn elapsed_ms(start: Instant) -> f64 {
    start.elapsed().as_secs_f64() * 1000.0
}

fn distribution(mut values: Vec<f64>) -> serde_json::Value {
    values.sort_by(f64::total_cmp);
    serde_json::json!({
        "samples": values.len(),
        "p50_ms": values[(values.len() * 50).div_ceil(100) - 1],
        "p95_ms": values[(values.len() * 95).div_ceil(100) - 1],
    })
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let source = std::env::args()
        .nth(1)
        .ok_or("usage: profile_ogz MAP.ogz")?;
    let input = std::fs::read(&source)?;
    let start = Instant::now();
    let map = read_ogz(&input)?;
    let parse_ms = elapsed_ms(start);
    let start = Instant::now();
    let (save, report) = convert(&map, &ImportOptions::default())?;
    let import_ms = elapsed_ms(start);
    let start = Instant::now();
    let bytes = serde_json::to_vec(&save)?;
    let serialize_ms = elapsed_ms(start);
    let start = Instant::now();
    let restored: ImportedWorld = serde_json::from_slice(&bytes)?;
    let mut world = World::from_compact_parts(
        restored.bounds,
        &restored.regions,
        &restored.cells,
        &restored.roots,
    )?;
    world.replace_volumes(&restored.volumes)?;
    let deserialize_and_load_ms = elapsed_ms(start);
    let keys = world.chunk_keys();
    let start = Instant::now();
    let (mut vertices, mut triangles, mut max_batch_triangles) = (0, 0, 0);
    for &key in &keys {
        let mesh = world.mesh_chunk(key);
        vertices += mesh.vertices.len();
        triangles += mesh.indices.len() / 3;
        max_batch_triangles = max_batch_triangles.max(mesh.indices.len() / 3);
        black_box(mesh);
    }
    let full_mesh_ms = elapsed_ms(start);
    let eye = save
        .objects
        .iter()
        .find(|object| object.kind == ObjectKind::Spawn)
        .map(|object| object.position + Vec3::Y * 1.55)
        .unwrap_or_else(|| (world.bounds().world_min() + world.bounds().world_max()) / 2.0);
    let ray_distance = (world.bounds().world_max() - world.bounds().world_min()).length();
    let mut ray_ms = Vec::new();
    let mut box_ms = Vec::new();
    let mut hits = Vec::new();
    for i in 0..1000 {
        let direction = forward(i as f32 * 2.399_963_1, ((i % 17) as f32 - 8.0) * 0.1);
        let start = Instant::now();
        let hit = world.raycast(black_box(eye), black_box(direction), ray_distance);
        ray_ms.push(elapsed_ms(start));
        if let Some(hit) = hit {
            hits.push(hit);
        }
        let position = eye + direction * ((i % 31) as f32 + 0.5);
        let start = Instant::now();
        black_box(world.intersects_box(position, position + Vec3::new(0.6, 1.75, 0.6)));
        box_ms.push(elapsed_ms(start));
    }
    let mut edit_ms = Vec::new();
    let mut dirty_keys = 0;
    if let Some(hit) = hits.first() {
        let original = RootEdit {
            pos: hit.pos,
            fragments: world.root_fragments(hit.pos),
        };
        let clear = RootEdit {
            pos: hit.pos,
            fragments: Vec::new(),
        };
        for i in 0..32 {
            let start = Instant::now();
            let dirty = world.apply_root_edits(std::slice::from_ref(if i % 2 == 0 {
                &clear
            } else {
                &original
            }))?;
            dirty_keys = dirty_keys.max(dirty.len());
            for key in dirty {
                black_box(world.mesh_chunk(key));
            }
            edit_ms.push(elapsed_ms(start));
        }
    }
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "source": source,
            "build": if cfg!(debug_assertions) { "debug" } else { "release" },
            "target": format!("{}/{}", std::env::consts::OS, std::env::consts::ARCH),
            "units_per_metre": report.units_per_metre,
            "source_nonempty_leaves": report.source.nonempty_leaves,
            "source_deformed_leaves": report.source.deformed_leaves,
            "bounds_metres": save.bounds,
            "stored_regions": save.regions.len(),
            "stored_coarse_cells": save.cells.len(),
            "stored_root_overrides": save.roots.len(),
            "stored_material_volumes": save.volumes.len(),
            "saved_json_bytes": bytes.len(),
            "parse_ms": parse_ms,
            "import_and_validate_ms": import_ms,
            "serialize_ms": serialize_ms,
            "deserialize_and_load_ms": deserialize_and_load_ms,
            "mesh_batches": keys.len(),
            "vertices": vertices,
            "triangles": triangles,
            "largest_batch_triangles": max_batch_triangles,
            "full_mesh_ms": full_mesh_ms,
            "ray_origin": eye.to_array(),
            "ray_max_distance_metres": ray_distance,
            "ray_hits": hits.len(),
            "raycast": distribution(ray_ms),
            "collision_box": distribution(box_ms),
            "local_edit_and_dirty_meshes": if edit_ms.is_empty() { serde_json::Value::Null } else { distribution(edit_ms) },
            "max_dirty_keys": dirty_keys,
            "regions_after_local_edits": world.region_count(),
            "overrides_after_local_edits": world.override_count(),
            "cropped_leaves": report.leaves_outside_region,
            "clipped_leaves": report.leaves_clipped_at_region,
            "unsupported_omissions": report.unsupported_leaves_omitted.len(),
            "scope": "CPU-only native wall-clock measurements; no GPU uploads, frame times, or iGPU performance claim."
        }))?
    );
    Ok(())
}
