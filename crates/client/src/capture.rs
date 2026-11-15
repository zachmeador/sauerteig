//! Native, surface-free capture of the same renderer used by the game.
mod scenes;

use crate::{
    lighting::ShadowMode,
    render::{settings::RenderSettings, Renderer, RendererOptions},
    scene::Geometry,
};
use glam::Vec3;
use sauerteig_core::{
    arena_objects, Cell, CellEdit, CellFragment, FineBox, MaterialVolume, ObjectKind, Player,
    RootEdit, VolumeMaterial, World, WorldBounds, WorldObject,
};
use scenes::Scene;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    time::Instant,
};

/// Frames drawn while waiting for shadows and fades to finish, at most.
const SETTLE_LIMIT: usize = 2400;

#[derive(Deserialize)]
struct WorldFile {
    #[serde(default)]
    bounds: WorldBounds,
    #[serde(default)]
    regions: Vec<CellFragment>,
    #[serde(default)]
    cells: Vec<CellEdit>,
    #[serde(default)]
    roots: Vec<RootEdit>,
    #[serde(default)]
    volumes: Vec<MaterialVolume>,
    #[serde(default)]
    objects: Vec<WorldObject>,
}

#[derive(Deserialize, Serialize)]
struct Pose {
    name: String,
    world: String,
    eye: Vec3,
    look_at: Vec3,
    #[serde(default)]
    time: f32,
    #[serde(default)]
    guides: bool,
    /// Invalidate every cached shadow before each frame.
    #[serde(default)]
    edit_storm: bool,
    /// Feet positions of actors standing in the scene.
    #[serde(default)]
    actors: Vec<Vec3>,
    /// Waypoints after the pose itself, all measured: walk to each at `speed`
    /// metres a second, or jump to it and hold for `dwell` frames.
    #[serde(default)]
    path: Vec<Waypoint>,
    #[serde(default = "default_speed")]
    speed: f32,
    #[serde(default = "default_dwell")]
    dwell: usize,
    /// A cell to change and change back, one block at a time, after the pose's
    /// frames: the report gives the GPU time of the sun's shadow passes in the
    /// frame after each edit.
    #[serde(default)]
    block_edit: Option<[i32; 3]>,
}

#[derive(Clone, Deserialize, Serialize)]
struct Waypoint {
    eye: Vec3,
    look_at: Vec3,
    #[serde(default)]
    jump: bool,
}

fn default_speed() -> f32 {
    6.0
}

fn default_dwell() -> usize {
    120
}

/// The camera of every measured frame, and the frames at which it jumped.
fn camera_frames(pose: &Pose, frames: usize) -> (Vec<(Vec3, Vec3)>, Vec<usize>) {
    if pose.path.is_empty() {
        return (vec![(pose.eye, pose.look_at); frames], Vec::new());
    }
    let mut cameras = Vec::new();
    let mut jumps = Vec::new();
    let (mut eye, mut look_at) = (pose.eye, pose.look_at);
    for waypoint in &pose.path {
        if waypoint.jump {
            jumps.push(cameras.len());
            (eye, look_at) = (waypoint.eye, waypoint.look_at);
            cameras.extend(std::iter::repeat_n((eye, look_at), pose.dwell.max(1)));
        } else {
            let steps = ((waypoint.eye.distance(eye) / (pose.speed / 60.0)).ceil() as usize).max(1);
            for step in 1..=steps {
                let t = step as f32 / steps as f32;
                cameras.push((eye.lerp(waypoint.eye, t), look_at.lerp(waypoint.look_at, t)));
            }
            (eye, look_at) = (waypoint.eye, waypoint.look_at);
        }
    }
    (cameras, jumps)
}

fn load_world(name: &str) -> Result<Scene, String> {
    if name == "arena" {
        return Ok((World::arena(), arena_objects()).into());
    }
    if let Some(scene) = scenes::named(name) {
        return Ok(scene);
    }
    if name == "materials" {
        let mut world = World::empty();
        for (min, max, material) in [
            ([-12, 0, -12], [12, 1, 12], 0),
            ([-8, 1, -6], [8, 8, -5], 2),
            ([-8, 8, -6], [8, 9, 2], 2),
            ([-1, 1, -3], [1, 4, -1], 1),
        ] {
            for x in min[0]..max[0] {
                for y in min[1]..max[1] {
                    for z in min[2]..max[2] {
                        world.set([x, y, z], Some(Cell::solid(material)));
                    }
                }
            }
        }
        world.replace_volumes(&[
            MaterialVolume {
                bounds: FineBox {
                    min: [-56, 8, 8],
                    max: [-8, 32, 56],
                },
                material: VolumeMaterial::Water,
            },
            MaterialVolume {
                bounds: FineBox {
                    min: [24, 8, 0],
                    max: [25, 40, 48],
                },
                material: VolumeMaterial::Glass,
            },
        ])?;
        let objects = vec![
            WorldObject {
                id: 1,
                position: Vec3::new(0.0, 6.5, -2.0),
                rotation: 0.0,
                kind: ObjectKind::Light,
            },
            WorldObject {
                id: 2,
                position: Vec3::new(0.0, 6.5, -2.0),
                rotation: 0.0,
                kind: ObjectKind::HangingLamp,
            },
        ];
        return Ok((world, objects).into());
    }
    let saved: WorldFile = serde_json::from_slice(&std::fs::read(name).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    let mut world =
        World::from_compact_parts(saved.bounds, &saved.regions, &saved.cells, &saved.roots)?;
    world.replace_volumes(&saved.volumes)?;
    Ok((world, saved.objects).into())
}

fn percentile(values: &mut [f64], fraction: f64) -> f64 {
    values.sort_by(f64::total_cmp);
    values[((values.len() - 1) as f64 * fraction).round() as usize]
}

fn summary(values: &[f64]) -> serde_json::Value {
    let mut values = values.to_vec();
    if values.is_empty() {
        return serde_json::Value::Null;
    }
    let mean = values.iter().sum::<f64>() / values.len() as f64;
    serde_json::json!({
        "mean": mean,
        "p50": percentile(&mut values, 0.5),
        "p95": percentile(&mut values, 0.95),
        "p99": percentile(&mut values, 0.99),
        "max": percentile(&mut values, 1.0),
    })
}

/// Passes that update shadow maps, in both the cone-light and atlas renderers.
fn is_shadow_pass(label: &str) -> bool {
    ["sun", "local", "shadow"]
        .iter()
        .any(|prefix| label.starts_with(prefix))
}

/// The sun's passes that draw static geometry, not the actors over it.
fn is_sun_static_pass(label: &str) -> bool {
    label.starts_with("sun static") || label.starts_with("sun cascade")
}

/// The atlas's tile and actor-tile passes alone, without the sun's window.
fn is_atlas_pass(label: &str) -> bool {
    label.starts_with("shadow")
}

pub fn run() -> Result<(), String> {
    let mut args = std::env::args().skip(1);
    let mut poses = None;
    let mut output = None;
    let mut width = 1920u32;
    let mut height = 1080u32;
    let mut frames = 60usize;
    let mut warmup = 10usize;
    let mut settings = RenderSettings::default();
    let mut throughput = Throughput { frames: 0, runs: 5 };
    let mut options = RendererOptions {
        fixed_frame_time: Some(1.0 / 60.0),
        ..Default::default()
    };
    while let Some(arg) = args.next() {
        if arg == "--brute-force" {
            options.brute_force = true;
            continue;
        }
        let value = args.next().ok_or("Expected a value after each flag")?;
        match arg.as_str() {
            "--node-shadows" => {
                options.node_shadows = match value.as_str() {
                    "off" => ShadowMode::Off,
                    "static" => ShadowMode::Static,
                    "dynamic" => ShadowMode::Dynamic,
                    _ => return Err("Use --node-shadows off, static or dynamic".into()),
                }
            }
            "--pages" => {
                options.atlas_pages = Some(value.parse().map_err(|_| "Invalid page count")?)
            }
            "--light-cap" => {
                options.light_cap = Some(value.parse().map_err(|_| "Invalid light cap")?)
            }
            "--display-hz" => {
                options.display_hz = Some(value.parse().map_err(|_| "Invalid refresh rate")?)
            }
            "--poses" => poses = Some(PathBuf::from(value)),
            "--output" => output = Some(PathBuf::from(value)),
            "--width" => width = value.parse().map_err(|_| "Invalid width")?,
            "--height" => height = value.parse().map_err(|_| "Invalid height")?,
            "--frames" => frames = value.parse().map_err(|_| "Invalid frame count")?,
            "--warmup" => warmup = value.parse().map_err(|_| "Invalid warmup count")?,
            "--throughput" => {
                throughput.frames = value
                    .parse()
                    .map_err(|_| "Invalid throughput frame count")?
            }
            "--runs" => throughput.runs = value.parse().map_err(|_| "Invalid run count")?,
            "--settings" => {
                settings = serde_json::from_slice(&std::fs::read(value).map_err(|e| e.to_string())?)
                    .map_err(|e| e.to_string())?
            }
            _ => return Err(format!("Unknown flag: {arg}")),
        }
    }
    if width == 0 || height == 0 || width > 8192 || height > 8192 || frames == 0 {
        return Err("Use dimensions 1..8192 and at least one measured frame".into());
    }
    if throughput.runs == 0 {
        return Err("Use at least one throughput run".into());
    }
    let poses = poses.ok_or("Usage: capture --poses poses.json --output directory [--width 3840 --height 2160 --frames 60 --warmup 10]")?;
    let output = output.ok_or("Missing --output")?;
    let poses: Vec<Pose> =
        serde_json::from_slice(&std::fs::read(poses).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
    for pose in &poses {
        if pose.name.is_empty()
            || !pose
                .name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
            || !pose.eye.is_finite()
            || !pose.look_at.is_finite()
            || !pose.time.is_finite()
            || (pose.look_at - pose.eye).cross(Vec3::Y).length_squared() < 1e-8
        {
            return Err(format!("Invalid capture pose: {}", pose.name));
        }
        if output.join(format!("{}.png", pose.name)).exists() {
            return Err(format!(
                "Capture already exists: {} (choose a new directory)",
                pose.name
            ));
        }
    }
    std::fs::create_dir_all(&output).map_err(|e| e.to_string())?;
    pollster::block_on(capture(
        &poses,
        &output,
        [width, height],
        Frames {
            measured: frames,
            warmup,
            throughput,
        },
        settings.normalized(),
        options,
    ))
}

/// Change one cell and change it back, `EDIT_REPEATS` times each way, drawing
/// the frame after every edit. Returns the GPU time of the sun's static passes
/// in those frames, and how many shadow maps they rendered.
fn measure_block_edit(
    renderer: &mut Renderer,
    scene: &mut Scene,
    mesh: &Geometry,
    pose: &Pose,
    context: &egui::Context,
    size: [u32; 2],
    pos: [i32; 3],
) -> Result<serde_json::Value, String> {
    const EDIT_REPEATS: usize = 6;
    let original = scene.world.get(pos);
    let changed = if original.is_some() {
        None
    } else {
        Some(Cell::solid(2))
    };
    let mut sun_ms = Vec::new();
    let mut maps = Vec::new();
    for edit in 0..EDIT_REPEATS * 2 {
        let cell = if edit % 2 == 0 { changed } else { original };
        let dirty = scene.world.apply_edits_changes(&[CellEdit { pos, cell }]);
        renderer.rebuild(&scene.world, &dirty);
        let shot = Shot {
            pose,
            world: &scene.world,
            objects: &scene.objects,
            mesh,
            context,
            size,
        };
        shot.draw(renderer)?;
        maps.push(renderer.shadow_updates as f64);
        let total = renderer.gpu_times().map(|times| {
            times
                .iter()
                .filter(|(label, _)| is_sun_static_pass(label))
                .map(|(_, ms)| ms)
                .sum::<f64>()
        });
        sun_ms.extend(total);
        for _ in 0..SETTLE_LIMIT {
            if renderer.lighting_settled() {
                break;
            }
            shot.draw(renderer)?;
        }
    }
    Ok(serde_json::json!({
        "cell": pos, "edits": EDIT_REPEATS * 2, "sun_static_gpu_ms": summary(&sun_ms),
        "shadow_maps_rendered": summary(&maps),
    }))
}

/// Throughput mode: draw `frames` frames back to back, wait once, and repeat
/// for `runs` runs (five unless `--runs` says otherwise). Zero frames leaves it
/// off.
#[derive(Clone, Copy)]
struct Throughput {
    frames: usize,
    runs: usize,
}

/// How many frames each pose draws.
#[derive(Clone, Copy)]
struct Frames {
    /// Serially timed frames, each waited for.
    measured: usize,
    /// Frames drawn first, before settling.
    warmup: usize,
    throughput: Throughput,
}

/// What one captured pose draws each frame.
struct Shot<'a> {
    pose: &'a Pose,
    world: &'a World,
    objects: &'a [WorldObject],
    mesh: &'a Geometry,
    context: &'a egui::Context,
    size: [u32; 2],
}

impl Shot<'_> {
    /// Draw one frame and wait for it. Returns CPU submission and total ms.
    fn draw(&self, renderer: &mut Renderer) -> Result<(f64, f64), String> {
        self.draw_from(renderer, self.pose.eye, self.pose.look_at)
    }

    fn draw_from(
        &self,
        renderer: &mut Renderer,
        eye: Vec3,
        look_at: Vec3,
    ) -> Result<(f64, f64), String> {
        let start = Instant::now();
        let submitted = self.submit_from(renderer, eye, look_at)?;
        let completed = renderer.wait_for_capture();
        Ok((
            submitted,
            completed.duration_since(start).as_secs_f64() * 1000.0,
        ))
    }

    /// Submit one frame without waiting for it. Returns CPU submission ms.
    fn submit_from(
        &self,
        renderer: &mut Renderer,
        eye: Vec3,
        look_at: Vec3,
    ) -> Result<f64, String> {
        let ui = self.context.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(self.size[0] as f32, self.size[1] as f32),
                )),
                ..Default::default()
            },
            |_| {},
        );
        if self.pose.edit_storm {
            renderer.invalidate_shadows();
        }
        let start = Instant::now();
        renderer
            .draw(
                eye,
                (look_at - eye).normalize(),
                self.pose.time,
                self.world,
                self.objects,
                &self.mesh.vertices,
                &self.mesh.indices,
                self.mesh.shadow_index_count,
                self.mesh.opaque_index_count,
                ui,
                self.context,
            )
            .map_err(|e| e.to_string())?;
        Ok(start.elapsed().as_secs_f64() * 1000.0)
    }
}

/// GPU throughput: milliseconds a frame over `frames` frames submitted back to
/// back and waited for once, for each run. Serial frame times add a fence wait
/// to every frame, which on Metal sleeps a millisecond a poll.
fn measure_throughput(
    shot: &Shot,
    renderer: &mut Renderer,
    cameras: &[(Vec3, Vec3)],
    throughput: Throughput,
) -> Result<serde_json::Value, String> {
    let mut runs = Vec::new();
    for _ in 0..throughput.runs {
        let start = Instant::now();
        for frame in 0..throughput.frames {
            let (eye, look_at) = cameras[frame % cameras.len()];
            shot.submit_from(renderer, eye, look_at)?;
        }
        let completed = renderer.wait_for_capture();
        runs.push(
            completed.duration_since(start).as_secs_f64() * 1000.0 / throughput.frames as f64,
        );
    }
    let mut sorted = runs.clone();
    Ok(serde_json::json!({
        "frames_per_run": throughput.frames, "runs": runs,
        "median": percentile(&mut sorted, 0.5), "min": percentile(&mut sorted, 0.0),
        "max": percentile(&mut sorted, 1.0),
    }))
}

async fn capture(
    poses: &[Pose],
    output: &Path,
    [width, height]: [u32; 2],
    Frames {
        measured: frames,
        warmup,
        throughput,
    }: Frames,
    settings: RenderSettings,
    options: RendererOptions,
) -> Result<(), String> {
    let created = Instant::now();
    let mut renderer = Renderer::headless_with(width, height, options).await?;
    let created_ms = created.elapsed().as_secs_f64() * 1000.0;
    renderer.apply_settings(settings);
    println!(
        "GPU: {} | created in {created_ms:.0} ms | {}x{} | timestamps: {} | {}",
        renderer.adapter_description,
        width,
        height,
        renderer.gpu_times().is_some(),
        renderer.render_description(),
    );
    let context = egui::Context::default();
    let mut reports = Vec::new();
    let mut loaded = String::new();
    let mut scene = Scene::from((World::empty(), Vec::new()));
    for pose in poses {
        if loaded != pose.world {
            let loading = Instant::now();
            scene = load_world(&pose.world)?;
            renderer.rebuild_all(&scene.world);
            let mesh = Geometry::objects(&scene.objects);
            renderer.objects(&mesh.vertices, &mesh.indices);
            renderer.set_extra_lights(scene.lights.clone());
            loaded.clone_from(&pose.world);
            let (chunks, bytes, ms) = renderer.fingerprint_stats();
            println!(
                "loaded {} in {:.0} ms: {chunks} chunk meshes, fingerprints {:.1} MiB made in {ms:.0} ms",
                pose.world,
                loading.elapsed().as_secs_f64() * 1000.0,
                bytes as f64 / 1048576.0,
            );
        }
        let mut mesh = Geometry::default();
        let mut actors = pose.actors.clone();
        if pose.world == "materials" {
            actors.push(Vec3::new(0.0, 1.0, 1.0));
        }
        for (i, &position) in actors.iter().enumerate() {
            let mut player = Player::new(99 + i as u64, "capture actor".into());
            player.position = position;
            mesh.player(&player, pose.time);
        }
        mesh.shadow_index_count = mesh.indices.len() as u32;
        mesh.opaque_index_count = mesh.shadow_index_count;
        if pose.guides {
            mesh.outline(Vec3::new(-1.0, 1.0, -3.0), 2.0, [1.0, 0.7, 0.2]);
            for object in scene.objects.iter().filter(|o| o.kind == ObjectKind::Light) {
                mesh.light_guide(object.position);
            }
        }
        let shot = Shot {
            pose,
            world: &scene.world,
            objects: &scene.objects,
            mesh: &mesh,
            context: &context,
            size: [width, height],
        };
        let mut cold_frame_ms = 0.0;
        let mut static_updates = Vec::new();
        let mut elapsed = Vec::new();
        let mut cpu = Vec::new();
        let mut gpu: BTreeMap<String, Vec<f64>> = BTreeMap::new();
        let mut shadow_gpu = Vec::new();
        let mut atlas_gpu = Vec::new();
        let mut gpu_frames = 0usize;
        let mut rendered = Vec::new();
        let mut used = Vec::new();
        let mut plan_ms = Vec::new();
        let mut settle_frames = 0;
        renderer.reset_auto_quality();
        for frame in 0..warmup {
            let (_, frame_ms) = shot.draw(&mut renderer)?;
            if frame == 0 {
                cold_frame_ms = frame_ms;
            }
        }
        // Shadows render a few faces a frame and lights fade in: wait for the
        // picture to stop changing, unless every frame deliberately changes it.
        while !pose.edit_storm && !renderer.lighting_settled() && settle_frames < SETTLE_LIMIT {
            shot.draw(&mut renderer)?;
            settle_frames += 1;
        }
        let settled = renderer.lighting_settled();
        let (cameras, jumps) = camera_frames(pose, frames);
        let mut jump_settle: Vec<Option<usize>> = vec![None; jumps.len()];
        let mut quality_changes = vec![(0, renderer.auto_level().index())];
        for (index, &(eye, look_at)) in cameras.iter().enumerate() {
            let (submitted, frame_ms) = shot.draw_from(&mut renderer, eye, look_at)?;
            if renderer.lighting_settled() {
                // The last jump at or before this frame has settled.
                let last = jumps.iter().rposition(|&j| j <= index);
                if let Some(last) = last.filter(|&last| jump_settle[last].is_none()) {
                    jump_settle[last] = Some(index - jumps[last] + 1);
                }
            }
            let level = renderer.auto_level().index();
            if quality_changes
                .last()
                .is_some_and(|&(_, last)| last != level)
            {
                quality_changes.push((index, level));
            }
            static_updates.push(renderer.shadow_updates);
            elapsed.push(frame_ms);
            cpu.push(submitted);
            let stats = renderer.lighting_stats();
            rendered.push(stats.tiles_rendered as f64);
            used.push(stats.tiles_used as f64);
            plan_ms.push(renderer.lighting_plan_ms());
            if let Some(times) = renderer.gpu_times() {
                gpu_frames += 1;
                let total = |keep: fn(&str) -> bool| {
                    times
                        .iter()
                        .filter(|(label, _)| keep(label))
                        .map(|(_, ms)| ms)
                        .fold(0.0, |a, b| a + b)
                };
                shadow_gpu.push(total(is_shadow_pass));
                atlas_gpu.push(total(is_atlas_pass));
                for (label, value) in times {
                    gpu.entry(label.clone()).or_default().push(*value);
                }
            }
        }
        let throughput_ms = if throughput.frames > 0 {
            Some(measure_throughput(
                &shot,
                &mut renderer,
                &cameras,
                throughput,
            )?)
        } else {
            None
        };
        renderer.save_capture(&output.join(format!("{}.png", pose.name)))?;
        let block_edit = match pose.block_edit {
            Some(pos) => Some(measure_block_edit(
                &mut renderer,
                &mut scene,
                &mesh,
                pose,
                &context,
                [width, height],
                pos,
            )?),
            None => None,
        };
        let lit_clusters: Vec<f64> = renderer
            .cluster_counts()
            .into_iter()
            .filter(|&n| n > 0)
            .map(f64::from)
            .collect();
        let updates: Vec<f64> = static_updates.iter().map(|&n| n as f64).collect();
        // Passes that ran in only some frames are summarized over the frames
        // they ran in; the per-frame shadow total includes the idle frames.
        let gpu_stats: BTreeMap<_, _> = gpu
            .iter()
            .map(|(label, values)| {
                let mut stats = summary(values);
                stats["frames"] = serde_json::json!(values.len());
                (label.clone(), stats)
            })
            .collect();
        let gpu: BTreeMap<_, _> = gpu
            .into_iter()
            .map(|(k, mut v)| (k, percentile(&mut v, 0.5)))
            .collect();
        let (rendered_total, used_total) = (rendered.iter().sum::<f64>(), used.iter().sum::<f64>());
        let report = serde_json::json!({
            "pose": pose, "first_frame_ms": cold_frame_ms, "static_updates": static_updates,
            "triangles": renderer.triangles, "visible_triangles": renderer.visible_triangles(),
            "transparent_units": renderer.transparent_stats().0, "transparent_quads": renderer.transparent_stats().1,
            "transparent_mesh_ms": renderer.remeshed.transparent_ms,
            "frame_median_ms": percentile(&mut elapsed, 0.5), "frame_p95_ms": percentile(&mut elapsed, 0.95),
            "frame_ms": summary(&elapsed), "cpu_submit_ms": summary(&cpu),
            "cpu_submit_median_ms": percentile(&mut cpu, 0.5), "gpu_median_ms": gpu,
            "gpu_ms": gpu_stats, "gpu_frames": gpu_frames,
            "shadow_gpu_ms_per_frame": summary(&shadow_gpu), "atlas_gpu_ms_per_frame": summary(&atlas_gpu),
            "shadow_updates_per_frame": summary(&updates),
            "lighting": renderer.lighting_stats(), "settled": settled, "settle_frames": settle_frames,
            "shadow_atlas_bytes": renderer.shadow_atlas_bytes(),
            "shadow_atlas_layers": renderer.shadow_atlas_layers(),
            "light_buffer_bytes": renderer.light_buffer_bytes(),
            "measured_frames": cameras.len(), "frames_to_settle_after_jump": jump_settle,
            "faces_rendered_per_frame": summary(&rendered), "faces_used_per_frame": summary(&used),
            "cache_hit_rate": (used_total > 0.0).then(|| 1.0 - rendered_total / used_total),
            "lighting_plan_cpu_ms": summary(&plan_ms),
            "lights_per_lit_cluster": summary(&lit_clusters), "lit_clusters": lit_clusters.len(),
            "throughput_ms_per_frame": throughput_ms, "block_edit": block_edit,
            "auto_level_changes": quality_changes,
        });
        println!("{}", serde_json::to_string(&report).unwrap());
        reports.push(report);
    }
    let report = serde_json::json!({"adapter": renderer.adapter_name,
        "adapter_description": renderer.adapter_description,
        "material_texture_bytes": renderer.materials.texture_bytes, "width": width, "height": height,
        "frames": frames, "warmup": warmup, "settings": settings, "effective": renderer.render_description(),
        "sun_cascade_bytes": renderer.sun_cascade_bytes(),
        "sample_counts": renderer.sample_counts(), "brute_force": options.brute_force,
        "light_cap": options.light_cap,
        "timing": "headless serial submit + GPU completion; excludes PNG readback, UI generation, simulation and presentation",
        "poses": reports});
    std::fs::write(
        output.join("measurements.json"),
        serde_json::to_vec_pretty(&report).unwrap(),
    )
    .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        lighting::{LightDesc, ShadowMode},
        render::clusters::ClusterGrid,
        render::settings::Quality,
    };
    use sauerteig_core::VolumeEdit;

    const SIZE: [u32; 2] = [321, 181];

    fn headless(options: RendererOptions) -> Renderer {
        let options = RendererOptions {
            fixed_frame_time: Some(1.0 / 60.0),
            ..options
        };
        let mut renderer =
            pollster::block_on(Renderer::headless_with(SIZE[0], SIZE[1], options)).unwrap();
        renderer.apply_settings(RenderSettings::default());
        renderer
    }

    fn frame(
        renderer: &mut Renderer,
        world: &World,
        objects: &[WorldObject],
        actor: Option<Vec3>,
        eye: Vec3,
        look_at: Vec3,
    ) -> Vec<u8> {
        let mut mesh = Geometry::default();
        if let Some(position) = actor {
            let mut player = Player::new(99, "test".into());
            player.position = position;
            mesh.player(&player, 2.0);
        }
        let context = egui::Context::default();
        let output = context.run(egui::RawInput::default(), |_| {});
        renderer
            .draw(
                eye,
                (look_at - eye).normalize(),
                2.0,
                world,
                objects,
                &mesh.vertices,
                &mesh.indices,
                mesh.indices.len() as u32,
                mesh.indices.len() as u32,
                output,
                &context,
            )
            .unwrap();
        renderer.wait_for_capture();
        renderer.capture_pixels().unwrap()
    }

    /// Draw until every shadow face has rendered and every light has faded.
    fn settled(
        renderer: &mut Renderer,
        world: &World,
        objects: &[WorldObject],
        actor: Option<Vec3>,
        eye: Vec3,
        look_at: Vec3,
    ) -> Vec<u8> {
        for _ in 0..SETTLE_LIMIT {
            let pixels = frame(renderer, world, objects, actor, eye, look_at);
            if renderer.lighting_settled() {
                return pixels;
            }
        }
        panic!("lighting never settled: {:?}", renderer.lighting_stats());
    }

    fn load(renderer: &mut Renderer, name: &str) -> Scene {
        let scene = load_world(name).unwrap();
        renderer.rebuild_all(&scene.world);
        let mesh = Geometry::objects(&scene.objects);
        renderer.objects(&mesh.vertices, &mesh.indices);
        renderer.set_extra_lights(scene.lights.clone());
        scene
    }

    fn differing(a: &[u8], b: &[u8]) -> usize {
        a.iter().zip(b).filter(|(a, b)| a != b).count()
    }

    #[test]
    #[ignore = "requires a native GPU; run explicitly with --ignored"]
    fn gpu_cached_shadows_match_fresh_draws_after_actor_edits_lights_and_settings() {
        let mut renderer = headless(RendererOptions::default());
        let Scene {
            mut world,
            mut objects,
            ..
        } = load(&mut renderer, "materials");
        let mesh = Geometry::objects(&objects);
        let eye = Vec3::new(0.0, 3.0, 7.0);
        let look_at = Vec3::new(0.0, 2.0, -2.0);
        let actor = |x| Some(Vec3::new(x, 1.0, 1.0));
        let a = settled(&mut renderer, &world, &objects, actor(0.0), eye, look_at);
        assert!(
            renderer.shadow_updates == 0,
            "settled means nothing renders"
        );
        let b = frame(&mut renderer, &world, &objects, actor(2.0), eye, look_at);
        assert_eq!(renderer.shadow_updates, 0, "actors never redraw a cache");
        assert_ne!(a, b, "moving actor must change the image");
        renderer.objects(&mesh.vertices, &mesh.indices);
        let fresh = frame(&mut renderer, &world, &objects, actor(2.0), eye, look_at);
        assert_eq!(b, fresh, "cached maps must not retain the previous actor");
        let empty = settled(&mut renderer, &world, &objects, None, eye, look_at);

        // An edit beside the light redraws its tiles; the result equals a
        // renderer that built everything from the edited world.
        let dirty = world.apply_edits_changes(&[CellEdit {
            pos: [2, 1, 0],
            cell: Some(Cell::solid(2)),
        }]);
        renderer.rebuild(&world, &dirty);
        frame(&mut renderer, &world, &objects, None, eye, look_at);
        assert!(renderer.shadow_updates > 0);
        let edited = settled(&mut renderer, &world, &objects, None, eye, look_at);
        assert_ne!(edited, empty);
        renderer.rebuild_all(&world);
        let rebuilt = settled(&mut renderer, &world, &objects, None, eye, look_at);
        assert_eq!(edited, rebuilt);
        let mut fresh_renderer = headless(RendererOptions::default());
        fresh_renderer.rebuild_all(&world);
        fresh_renderer.objects(&mesh.vertices, &mesh.indices);
        let from_scratch = settled(&mut fresh_renderer, &world, &objects, None, eye, look_at);
        assert_eq!(edited, from_scratch);

        // Moving the light redraws only that light's tiles.
        objects[0].position.x += 2.0;
        let moved = settled(&mut renderer, &world, &objects, None, eye, look_at);
        renderer.rebuild_all(&world);
        assert_eq!(
            moved,
            settled(&mut renderer, &world, &objects, None, eye, look_at)
        );
        let moved_eye = eye + Vec3::new(10.0, 2.0, 0.0);
        let moved_window = settled(&mut renderer, &world, &objects, None, moved_eye, look_at);
        renderer.objects(&mesh.vertices, &mesh.indices);
        assert_eq!(
            moved_window,
            settled(&mut renderer, &world, &objects, None, moved_eye, look_at)
        );

        for settings in [
            RenderSettings {
                quality: Quality::Custom,
                scale: 0.5,
                msaa: crate::render::settings::Antialiasing::Off,
                bloom: 0.0,
                ..Default::default()
            },
            RenderSettings::default(),
        ] {
            renderer.apply_settings(settings);
            let pixels = settled(
                &mut renderer,
                &world,
                &objects,
                None,
                Vec3::new(-4.0, 2.0, 5.0),
                look_at,
            );
            assert_eq!(pixels.len(), (SIZE[0] * SIZE[1] * 4) as usize);
            assert!(pixels.as_chunks::<4>().0.iter().all(|p| p[3] == 255));
            assert!(pixels.as_chunks::<4>().0.iter().any(|p| p[1] > p[0]));
        }
    }

    /// A renderer built from nothing but this scene, drawn until settled.
    fn from_scratch(
        world: &World,
        objects: &[WorldObject],
        settings: RenderSettings,
        eye: Vec3,
        look_at: Vec3,
    ) -> Vec<u8> {
        let mut renderer = headless(RendererOptions::default());
        renderer.apply_settings(settings);
        renderer.rebuild_all(world);
        let mesh = Geometry::objects(objects);
        renderer.objects(&mesh.vertices, &mesh.indices);
        settled(&mut renderer, world, objects, None, eye, look_at)
    }

    #[test]
    #[ignore = "requires a native GPU; run explicitly with --ignored"]
    fn gpu_sun_cascades_match_fresh_renders_after_edits_moves_and_level_changes() {
        let eye = Vec3::new(0.0, 3.0, 7.0);
        let look_at = Vec3::new(0.0, 2.0, -2.0);
        let mut renderer = headless(RendererOptions::default());
        let Scene {
            mut world,
            mut objects,
            ..
        } = load(&mut renderer, "materials");
        let before = settled(&mut renderer, &world, &objects, None, eye, look_at);
        assert_eq!(
            before,
            from_scratch(&world, &objects, RenderSettings::default(), eye, look_at)
        );
        frame(&mut renderer, &world, &objects, None, eye, look_at);
        assert_eq!(renderer.sun_texels, 0, "nothing changed, nothing redrawn");

        // One block redraws a small part of the cascades, and the result is
        // what a renderer that never saw the old block draws.
        let dirty = world.apply_edits_changes(&[CellEdit {
            pos: [2, 1, 0],
            cell: Some(Cell::solid(2)),
        }]);
        renderer.rebuild(&world, &dirty);
        frame(&mut renderer, &world, &objects, None, eye, look_at);
        let whole = 3 * 1024 * 1024;
        println!("one block redrew {} of {whole} texels", renderer.sun_texels);
        assert!(renderer.sun_texels > 0);
        assert!(renderer.sun_texels < whole / 3, "{}", renderer.sun_texels);
        let edited = settled(&mut renderer, &world, &objects, None, eye, look_at);
        assert_ne!(edited, before, "the block must change the picture");
        assert_eq!(
            edited,
            from_scratch(&world, &objects, RenderSettings::default(), eye, look_at)
        );

        // Removing it again, and a prop that moves, redraw their old and new
        // places.
        let dirty = world.apply_edits_changes(&[CellEdit {
            pos: [2, 1, 0],
            cell: None,
        }]);
        renderer.rebuild(&world, &dirty);
        let removed = settled(&mut renderer, &world, &objects, None, eye, look_at);
        assert_eq!(removed, before);
        objects[1].position.x += 2.5;
        frame(&mut renderer, &world, &objects, None, eye, look_at);
        assert!(renderer.sun_texels > 0 && renderer.sun_texels < whole / 3);
        let moved = settled(&mut renderer, &world, &objects, None, eye, look_at);
        assert_ne!(moved, before);
        assert_eq!(
            moved,
            from_scratch(&world, &objects, RenderSettings::default(), eye, look_at)
        );

        // Walking far enough moves the windows, and they redraw whole.
        let walked = eye + Vec3::new(9.0, 1.0, -3.0);
        frame(&mut renderer, &world, &objects, None, walked, look_at);
        assert!(renderer.sun_texels >= 1024 * 1024);
        let after = settled(&mut renderer, &world, &objects, None, walked, look_at);
        assert_eq!(
            after,
            from_scratch(&world, &objects, RenderSettings::default(), walked, look_at)
        );

        // Each fixed level draws what a renderer started at that level does.
        // The eye is near the lamp, whose shadow tiles then ask for the
        // largest size at every level that allows it, so the tile sizes'
        // hysteresis cannot make two histories differ.
        let near = Vec3::new(1.0, 5.0, -1.0);
        for quality in [
            Quality::Low,
            Quality::Ultra,
            Quality::Medium,
            Quality::High,
            Quality::Auto,
        ] {
            let settings = RenderSettings {
                quality,
                ..Default::default()
            };
            renderer.apply_settings(settings);
            // Tiles of a new size wait out their residency before they change.
            for _ in 0..90 {
                frame(&mut renderer, &world, &objects, None, near, look_at);
            }
            let pixels = settled(&mut renderer, &world, &objects, None, near, look_at);
            let fresh = from_scratch(&world, &objects, settings, near, look_at);
            println!("{quality:?}: {} bytes differ", differing(&pixels, &fresh));
            assert_eq!(pixels, fresh, "{quality:?}");
        }
    }

    #[test]
    #[ignore = "requires a native GPU; run explicitly with --ignored"]
    fn gpu_actors_shadow_the_near_cascades_and_nothing_stays_behind() {
        let eye = Vec3::new(0.0, 3.0, 7.0);
        let look_at = Vec3::new(0.0, 2.0, -2.0);
        let mut renderer = headless(RendererOptions::default());
        let scene = load(&mut renderer, "materials");
        let empty = settled(
            &mut renderer,
            &scene.world,
            &scene.objects,
            None,
            eye,
            look_at,
        );
        let actor = |x| Some(Vec3::new(x, 1.0, 1.0));
        let there = frame(
            &mut renderer,
            &scene.world,
            &scene.objects,
            actor(-2.0),
            eye,
            look_at,
        );
        assert_eq!(renderer.sun_texels, 0, "an actor never redraws a cache");
        assert_ne!(there, empty);
        let gone = frame(
            &mut renderer,
            &scene.world,
            &scene.objects,
            None,
            eye,
            look_at,
        );
        assert_eq!(gone, empty, "the actor's shadow must not stay in the cache");
    }

    #[test]
    #[ignore = "requires a native GPU; run explicitly with --ignored"]
    fn gpu_clustered_output_matches_a_brute_force_render() {
        let eye = Vec3::new(0.3, 2.6, 0.3);
        let look_at = Vec3::new(-14.0, 4.0, -16.0);
        let mut images = Vec::new();
        for brute_force in [false, true] {
            let mut renderer = headless(RendererOptions {
                brute_force,
                ..Default::default()
            });
            let scene = load(&mut renderer, "lightroom");
            let pixels = settled(
                &mut renderer,
                &scene.world,
                &scene.objects,
                None,
                eye,
                look_at,
            );
            assert!(renderer.lighting_stats().shadowed >= 10);
            images.push(pixels);
        }
        assert_eq!(differing(&images[0], &images[1]), 0);
        assert!(images[0].iter().map(|&b| u32::from(b)).sum::<u32>() > 100_000);
    }

    #[test]
    #[ignore = "requires a native GPU; run explicitly with --ignored"]
    fn gpu_cluster_masks_match_the_cpu_builder() {
        let mut renderer = headless(RendererOptions::default());
        let scene = load(&mut renderer, "lightroom");
        settled(
            &mut renderer,
            &scene.world,
            &scene.objects,
            None,
            Vec3::new(0.3, 2.6, 0.3),
            Vec3::new(-14.0, 4.0, -16.0),
        );
        let (gpu, spheres, scale) = renderer.cluster_readback();
        assert!(spheres.len() >= 30, "{} lights", spheres.len());
        let grid = renderer.cluster_grid();
        let lights = |factor: f32| -> Vec<glam::Vec4> {
            spheres
                .iter()
                .map(|s| glam::Vec4::new(s[0], s[1], s[2], s[3] * factor))
                .collect()
        };
        // Float rounding may differ between the two builders at a touching
        // sphere; everything clearly inside or outside must agree.
        let (inside, outside) = (
            grid.build_masks(scale, &lights(0.999)),
            grid.build_masks(scale, &lights(1.001)),
        );
        let words = ClusterGrid::mask_words(spheres.len());
        let mut set = 0;
        for cluster in 0..grid.count() {
            for word in 0..words {
                let i = cluster * words + word;
                assert_eq!(
                    gpu[i] & inside[i],
                    inside[i],
                    "cluster {cluster} dropped a light"
                );
                assert_eq!(gpu[i] & !outside[i], 0, "cluster {cluster} gained a light");
                set += gpu[i].count_ones();
            }
        }
        assert!(set > 1000, "{set} bits set");
    }

    #[test]
    #[ignore = "requires a native GPU; run explicitly with --ignored"]
    fn gpu_every_offered_sample_count_renders_the_same_scene() {
        use crate::render::settings::Antialiasing;
        let eye = Vec3::new(0.0, 3.0, 7.0);
        let look_at = Vec3::new(0.0, 2.0, -2.0);
        let mut renderer = headless(RendererOptions::default());
        let counts = renderer.sample_counts().to_vec();
        println!("sample counts: {counts:?}");
        assert_eq!(counts[0], 1);
        assert!(counts.contains(&4), "{counts:?}");
        let scene = load(&mut renderer, "materials");
        let mut images = Vec::new();
        for choice in Antialiasing::choices(&counts) {
            renderer.apply_settings(RenderSettings {
                quality: Quality::Custom,
                msaa: choice,
                ..Default::default()
            });
            let pixels = settled(
                &mut renderer,
                &scene.world,
                &scene.objects,
                None,
                eye,
                look_at,
            );
            let expected = choice.samples(u64::from(SIZE[0] * SIZE[1]), &counts);
            assert!(
                renderer
                    .render_description()
                    .contains(&format!("{expected}x MSAA")),
                "{choice:?}: {}",
                renderer.render_description()
            );
            assert!(
                mean_brightness(&pixels) > 5.0,
                "{choice:?} drew a dark frame"
            );
            images.push((choice, pixels));
        }
        // Multisampling changes edges only: every count agrees on most pixels.
        let (_, reference) = &images[0];
        for (choice, pixels) in &images {
            let differing = differing(reference, pixels);
            assert!(
                differing < reference.len() / 10,
                "{choice:?}: {differing} of {} bytes differ",
                reference.len()
            );
        }
    }

    fn mean_brightness(pixels: &[u8]) -> f64 {
        let total: u64 = pixels
            .as_chunks::<4>()
            .0
            .iter()
            .map(|p| u64::from(p[0]) + u64::from(p[1]) + u64::from(p[2]))
            .sum();
        total as f64 / (pixels.len() / 4 * 3) as f64
    }

    #[test]
    #[ignore = "requires a native GPU; run explicitly with --ignored"]
    fn gpu_walls_stop_the_light_of_the_next_corridor() {
        // Lights only in the corridor beside the one the camera is in: with
        // shadows the view stays near ambient, and without them it floods.
        let eye = Vec3::new(-21.0, 2.5, -1.5);
        let look_at = Vec3::new(-10.0, 3.0, 1.8);
        let brightness = |mode: ShadowMode, lit: bool| {
            let mut renderer = headless(RendererOptions {
                node_shadows: mode,
                ..Default::default()
            });
            let scene = load(&mut renderer, "corridor");
            let objects: Vec<WorldObject> = scene
                .objects
                .into_iter()
                .filter(|o| lit && o.id > 10)
                .collect();
            renderer.objects(&[], &[]);
            let pixels = settled(&mut renderer, &scene.world, &objects, None, eye, look_at);
            mean_brightness(&pixels)
        };
        let dark = brightness(ShadowMode::Static, false);
        let shadowed = brightness(ShadowMode::Static, true);
        let leaking = brightness(ShadowMode::Off, true);
        assert!(leaking > dark + 20.0, "{dark} {shadowed} {leaking}");
        assert!(
            shadowed < dark + 0.05 * (leaking - dark),
            "light leaks through the wall: {dark} {shadowed} {leaking}"
        );
    }

    #[test]
    #[ignore = "requires a native GPU; run explicitly with --ignored"]
    fn gpu_spot_lights_and_actor_tiles_render() {
        let eye = Vec3::new(-4.0, 3.2, 11.0);
        let look_at = Vec3::new(1.5, 1.2, 3.0);
        let with_actor = Some(Vec3::new(1.5, 1.0, 4.5));
        let mut renderer = headless(RendererOptions::default());
        let scene = load(&mut renderer, "lightshow");
        let empty = settled(
            &mut renderer,
            &scene.world,
            &scene.objects,
            None,
            eye,
            look_at,
        );
        let stats = renderer.lighting_stats();
        assert_eq!(stats.shadowed, 5, "{stats:?}");
        assert_eq!(stats.frame_tiles, 0);
        let a = settled(
            &mut renderer,
            &scene.world,
            &scene.objects,
            with_actor,
            eye,
            look_at,
        );
        assert!(renderer.lighting_stats().frame_tiles > 0);
        let b = settled(
            &mut renderer,
            &scene.world,
            &scene.objects,
            Some(Vec3::new(-1.0, 1.0, 5.0)),
            eye,
            look_at,
        );
        let (moved, appeared) = (differing(&a, &b), differing(&empty, &a));
        assert!(appeared > 0 && moved > 0, "{appeared} {moved}");
        // The actor is gone again, and nothing of it is left in a tile.
        let gone = settled(
            &mut renderer,
            &scene.world,
            &scene.objects,
            None,
            eye,
            look_at,
        );
        assert_eq!(differing(&gone, &empty), 0);
        // Making every light static removes the lights' actor shadows,
        // leaving only the sun's.
        let mut lights = scene.lights.clone();
        lights.iter_mut().for_each(|l| {
            if l.shadow == ShadowMode::Dynamic {
                l.shadow = ShadowMode::Static
            }
        });
        renderer.set_extra_lights(lights);
        let static_a = settled(
            &mut renderer,
            &scene.world,
            &scene.objects,
            with_actor,
            eye,
            look_at,
        );
        assert_eq!(renderer.lighting_stats().frame_tiles, 0);
        assert!(differing(&a, &static_a) > 0);
    }

    /// The lightfield, where a floor point has dozens of lights in reach.
    fn lightfield_pixels(options: RendererOptions) -> (Vec<u8>, Renderer) {
        let mut renderer = headless(options);
        let scene = load(&mut renderer, "lightfield");
        let pixels = settled(
            &mut renderer,
            &scene.world,
            &scene.objects,
            None,
            Vec3::new(3.5, 2.6, 3.5),
            Vec3::new(-30.0, 4.0, -20.0),
        );
        (pixels, renderer)
    }

    #[test]
    #[ignore = "requires a native GPU; run explicitly with --ignored"]
    fn gpu_a_fragment_reached_by_more_than_sixteen_lights_shades_all_of_them() {
        let (all, renderer) = lightfield_pixels(RendererOptions::default());
        let (_, spheres, scale) = renderer.cluster_readback();
        // Count what reaches the clusters: some must hold far more than 16.
        let grid = renderer.cluster_grid();
        let lights: Vec<_> = spheres.iter().map(|&s| glam::Vec4::from_array(s)).collect();
        let masks = grid.build_masks(scale, &lights);
        let words = ClusterGrid::mask_words(lights.len());
        let most = masks
            .chunks(words)
            .map(|c| c.iter().map(|w| w.count_ones()).sum::<u32>())
            .max()
            .unwrap();
        assert!(most > 40, "{most} lights in the busiest cluster");
        let (capped, _) = lightfield_pixels(RendererOptions {
            light_cap: Some(16),
            ..Default::default()
        });
        assert!(
            differing(&all, &capped) > 1000,
            "a cap of 16 changed {} bytes",
            differing(&all, &capped)
        );
        // With no cap the clusters give what evaluating every light does.
        let (brute, _) = lightfield_pixels(RendererOptions {
            brute_force: true,
            ..Default::default()
        });
        assert_eq!(differing(&all, &brute), 0);
    }

    #[test]
    #[ignore = "requires a native GPU; run explicitly with --ignored"]
    fn gpu_a_water_fragment_reached_by_more_than_eight_lights_shades_all_of_them() {
        let eye = Vec3::new(-19.0, 4.5, -19.0);
        let look_at = Vec3::new(2.0, 2.5, 2.0);
        let draw = |options: RendererOptions| {
            let mut renderer = headless(options);
            let scene = load(&mut renderer, "waterglass");
            // Twelve more lights over the tank, to the eight it has.
            let mut lights = scene.lights.clone();
            lights.extend((0..12).map(|i| {
                LightDesc::point(
                    500 + i,
                    Vec3::new(
                        -6.0 + (i % 4) as f32 * 4.0,
                        3.2,
                        -4.0 + (i / 4) as f32 * 4.0,
                    ),
                )
            }));
            renderer.set_extra_lights(lights);
            let pixels = settled(
                &mut renderer,
                &scene.world,
                &scene.objects,
                None,
                eye,
                look_at,
            );
            (pixels, renderer)
        };
        let (all, renderer) = draw(RendererOptions::default());
        let (_, spheres, _) = renderer.cluster_readback();
        // A point in the middle of the tank's surface.
        let water = Vec3::new(0.0, 2.0, 0.0);
        let eye_to_water = |s: &[f32; 4]| {
            let view = glam::Mat4::look_to_rh(eye, (look_at - eye).normalize(), Vec3::Y);
            let center = view.inverse().transform_point3(Vec3::new(s[0], s[1], s[2]));
            center.distance(water) < s[3] / 1.002
        };
        let reaching = spheres.iter().filter(|s| eye_to_water(s)).count();
        assert!(reaching > 8, "{reaching} lights reach the water");
        let (capped, _) = draw(RendererOptions {
            light_cap: Some(8),
            ..Default::default()
        });
        assert!(differing(&all, &capped) > 0);
        let (brute, _) = draw(RendererOptions {
            brute_force: true,
            ..Default::default()
        });
        assert_eq!(differing(&all, &brute), 0);
    }

    #[test]
    #[ignore = "requires a native GPU; run explicitly with --ignored"]
    fn gpu_a_thousand_lights_match_a_brute_force_render_within_the_lowest_bound() {
        let eye = Vec3::new(3.5, 2.6, 3.5);
        let look_at = Vec3::new(-30.0, 4.0, -20.0);
        let mut images = Vec::new();
        for brute_force in [false, true] {
            let mut renderer = headless(RendererOptions {
                brute_force,
                // The lowest level's bound.
                atlas_pages: Some(15),
                ..Default::default()
            });
            let scene = load(&mut renderer, "lightfield");
            let pixels = settled(
                &mut renderer,
                &scene.world,
                &scene.objects,
                None,
                eye,
                look_at,
            );
            let stats = renderer.lighting_stats();
            println!("{stats:?}");
            assert!(stats.drawn > 300, "{stats:?}");
            assert_eq!(stats.shadowed, stats.drawn);
            assert_eq!(stats.over_budget, 0);
            assert_eq!(stats.pages_past_bound(), 0, "{stats:?}");
            let (atlas, frame) = renderer.shadow_atlas_layers();
            assert!(atlas + frame <= 16, "{atlas} + {frame} layers");
            images.push(pixels);
        }
        assert_eq!(differing(&images[0], &images[1]), 0);
        assert!(images[0].iter().map(|&b| u32::from(b)).sum::<u32>() > 100_000);
    }
    #[test]
    #[ignore = "requires a native GPU; run explicitly with --ignored"]
    fn gpu_water_and_glass_edits_redraw_no_shadows_and_match_fresh_renders() {
        let eye = Vec3::new(-9.0, 6.0, 10.0);
        let look_at = Vec3::new(-3.0, 2.0, 3.0);
        let mut renderer = headless(RendererOptions::default());
        let Scene {
            mut world, objects, ..
        } = load(&mut renderer, "materials");
        let before = settled(&mut renderer, &world, &objects, None, eye, look_at);
        let (units, quads) = renderer.transparent_stats();
        assert!(units > 0 && quads > 0);
        frame(&mut renderer, &world, &objects, None, eye, look_at);
        assert_eq!(renderer.sun_texels, 0, "nothing changed, nothing redrawn");

        // A glass pane across the water's end, and water through part of the
        // floor, which splits the floor's solid leaves.
        let edits = [
            VolumeEdit {
                bounds: FineBox {
                    min: [-64, 8, 56],
                    max: [-8, 32, 57],
                },
                material: Some(VolumeMaterial::Glass),
            },
            VolumeEdit {
                bounds: FineBox {
                    min: [-37, 2, -37],
                    max: [-27, 12, -27],
                },
                material: Some(VolumeMaterial::Water),
            },
        ];
        let old = world.clone();
        world.apply_volume_edits(&edits).unwrap();
        let changes = world.mesh_changes(&old);
        assert!(!changes.transparent.is_empty());
        assert!(!changes.opaque.is_empty(), "the floor's leaves split");
        assert!(changes.shadows.is_empty(), "{changes:?}");
        renderer.rebuild(&world, &changes);
        assert_eq!(renderer.remeshed.opaque, changes.opaque.len());
        assert_eq!(renderer.remeshed.transparent, changes.transparent.len());
        frame(&mut renderer, &world, &objects, None, eye, look_at);
        assert_eq!(renderer.sun_texels, 0, "water and glass move no shadow");
        assert_eq!(renderer.shadow_updates, 0, "no light redraws a tile");
        let edited = settled(&mut renderer, &world, &objects, None, eye, look_at);
        assert_ne!(edited, before, "the pane must change the picture");
        assert_eq!(
            edited,
            from_scratch(&world, &objects, RenderSettings::default(), eye, look_at)
        );

        // Undoing both brings the first picture back, again without shadows.
        let old = world.clone();
        world
            .apply_volume_edits(&edits.map(|e| VolumeEdit {
                material: None,
                ..e
            }))
            .unwrap();
        let changes = world.mesh_changes(&old);
        assert!(changes.shadows.is_empty(), "{changes:?}");
        renderer.rebuild(&world, &changes);
        frame(&mut renderer, &world, &objects, None, eye, look_at);
        assert_eq!(renderer.sun_texels, 0);
        assert_eq!(renderer.shadow_updates, 0);
        let undone = settled(&mut renderer, &world, &objects, None, eye, look_at);
        assert_eq!(undone, before);
    }
}
