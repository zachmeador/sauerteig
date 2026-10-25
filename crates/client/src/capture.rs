//! Native, surface-free capture of the same renderer used by the game.
use crate::{
    render::{settings::RenderSettings, Renderer},
    scene::Geometry,
};
use glam::Vec3;
use sauerteig_core::{
    arena_objects, Cell, CellEdit, CellFragment, FineBox, MaterialVolume, ObjectKind, Player,
    RootEdit, VolumeMaterial, World, WorldBounds, WorldObject,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    time::Instant,
};

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
}

fn load_world(name: &str) -> Result<(World, Vec<WorldObject>), String> {
    if name == "arena" {
        return Ok((World::arena(), arena_objects()));
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
        return Ok((world, objects));
    }
    let saved: WorldFile = serde_json::from_slice(&std::fs::read(name).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    let mut world =
        World::from_compact_parts(saved.bounds, &saved.regions, &saved.cells, &saved.roots)?;
    world.replace_volumes(&saved.volumes)?;
    Ok((world, saved.objects))
}

fn percentile(values: &mut [f64], fraction: f64) -> f64 {
    values.sort_by(f64::total_cmp);
    values[((values.len() - 1) as f64 * fraction).round() as usize]
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
    while let Some(arg) = args.next() {
        let value = args.next().ok_or("Expected a value after each flag")?;
        match arg.as_str() {
            "--poses" => poses = Some(PathBuf::from(value)),
            "--output" => output = Some(PathBuf::from(value)),
            "--width" => width = value.parse().map_err(|_| "Invalid width")?,
            "--height" => height = value.parse().map_err(|_| "Invalid height")?,
            "--frames" => frames = value.parse().map_err(|_| "Invalid frame count")?,
            "--warmup" => warmup = value.parse().map_err(|_| "Invalid warmup count")?,
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
        frames,
        warmup,
        settings.normalized(),
    ))
}

async fn capture(
    poses: &[Pose],
    output: &Path,
    [width, height]: [u32; 2],
    frames: usize,
    warmup: usize,
    settings: RenderSettings,
) -> Result<(), String> {
    let mut renderer = Renderer::headless(width, height).await?;
    renderer.apply_settings(settings);
    println!(
        "GPU: {} | {}x{} | timestamps: {}",
        renderer.adapter_name,
        width,
        height,
        renderer.gpu_times().is_some()
    );
    let context = egui::Context::default();
    let mut reports = Vec::new();
    let mut loaded = String::new();
    let mut world = World::empty();
    let mut objects = Vec::new();
    for pose in poses {
        if loaded != pose.world {
            (world, objects) = load_world(&pose.world)?;
            renderer.rebuild_all(&world);
            let mesh = Geometry::objects(&objects);
            renderer.objects(&mesh.vertices, &mesh.indices);
            loaded.clone_from(&pose.world);
        }
        let mut mesh = Geometry::default();
        if pose.world == "materials" {
            let mut player = Player::new(99, "capture actor".into());
            player.position = Vec3::new(0.0, 1.0, 1.0);
            mesh.player(&player, pose.time);
            mesh.shadow_index_count = mesh.indices.len() as u32;
            mesh.opaque_index_count = mesh.shadow_index_count;
        }
        if pose.guides {
            mesh.outline(Vec3::new(-1.0, 1.0, -3.0), 2.0, [1.0, 0.7, 0.2]);
            for object in objects.iter().filter(|o| o.kind == ObjectKind::Light) {
                mesh.light_guide(object.position);
            }
        }
        let mut cold_frame_ms = 0.0;
        let mut static_updates = Vec::new();
        let mut elapsed = Vec::new();
        let mut cpu = Vec::new();
        let mut gpu: BTreeMap<String, Vec<f64>> = BTreeMap::new();
        for frame in 0..warmup + frames {
            let ui = context.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(width as f32, height as f32),
                    )),
                    ..Default::default()
                },
                |_| {},
            );
            let start = Instant::now();
            renderer
                .draw(
                    pose.eye,
                    (pose.look_at - pose.eye).normalize(),
                    pose.time,
                    &world,
                    &objects,
                    &mesh.vertices,
                    &mesh.indices,
                    mesh.shadow_index_count,
                    mesh.opaque_index_count,
                    ui,
                    &context,
                )
                .map_err(|e| e.to_string())?;
            let submitted = start.elapsed().as_secs_f64() * 1000.0;
            let completed = renderer.wait_for_capture();
            let frame_ms = completed.duration_since(start).as_secs_f64() * 1000.0;
            if frame == 0 {
                cold_frame_ms = frame_ms;
            }
            if frame >= warmup {
                static_updates.push(renderer.shadow_updates);
                elapsed.push(frame_ms);
                cpu.push(submitted);
                if let Some(times) = renderer.gpu_times() {
                    for (label, value) in times {
                        gpu.entry(label.clone()).or_default().push(*value);
                    }
                }
            }
        }
        renderer.save_capture(&output.join(format!("{}.png", pose.name)))?;
        let gpu: BTreeMap<_, _> = gpu
            .into_iter()
            .map(|(k, mut v)| (k, percentile(&mut v, 0.5)))
            .collect();
        let report = serde_json::json!({
            "pose": pose, "first_frame_ms": cold_frame_ms, "static_updates": static_updates,
            "triangles": renderer.triangles, "visible_triangles": renderer.visible_triangles(),
            "frame_median_ms": percentile(&mut elapsed, 0.5), "frame_p95_ms": percentile(&mut elapsed, 0.95),
            "cpu_submit_median_ms": percentile(&mut cpu, 0.5), "gpu_median_ms": gpu,
        });
        println!("{}", serde_json::to_string(&report).unwrap());
        reports.push(report);
    }
    let report = serde_json::json!({"adapter": renderer.adapter_name, "width": width, "height": height,
        "frames": frames, "warmup": warmup, "settings": settings, "effective": renderer.render_description(), "timing": "headless serial submit + GPU completion; excludes PNG readback, UI generation, simulation and presentation",
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

    fn frame(
        renderer: &mut Renderer,
        world: &World,
        objects: &[WorldObject],
        actor: Option<Vec3>,
        eye: Vec3,
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
                (Vec3::new(0.0, 2.0, -2.0) - eye).normalize(),
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

    #[test]
    #[ignore = "requires a native GPU; run explicitly with --ignored"]
    fn gpu_cached_shadows_match_fresh_draws_after_actor_edits_lights_and_settings() {
        let (mut world, mut objects) = load_world("materials").unwrap();
        let mut renderer = pollster::block_on(Renderer::headless(321, 181)).unwrap();
        renderer.rebuild_all(&world);
        let mesh = Geometry::objects(&objects);
        renderer.objects(&mesh.vertices, &mesh.indices);
        let eye = Vec3::new(0.0, 3.0, 7.0);
        let a = frame(
            &mut renderer,
            &world,
            &objects,
            Some(Vec3::new(0.0, 1.0, 1.0)),
            eye,
        );
        assert!(renderer.shadow_updates > 0);
        let b = frame(
            &mut renderer,
            &world,
            &objects,
            Some(Vec3::new(2.0, 1.0, 1.0)),
            eye,
        );
        assert_eq!(renderer.shadow_updates, 0);
        assert_ne!(a, b, "moving actor must change the image");
        renderer.objects(&mesh.vertices, &mesh.indices);
        let fresh = frame(
            &mut renderer,
            &world,
            &objects,
            Some(Vec3::new(2.0, 1.0, 1.0)),
            eye,
        );
        assert_eq!(
            b, fresh,
            "cached map must not retain the previous actor's shadow"
        );
        let empty = frame(&mut renderer, &world, &objects, None, eye);
        renderer.objects(&mesh.vertices, &mesh.indices);
        assert_eq!(empty, frame(&mut renderer, &world, &objects, None, eye));

        let dirty = world.apply_edits(&[CellEdit {
            pos: [2, 1, 0],
            cell: Some(Cell::solid(2)),
        }]);
        renderer.rebuild(&world, &dirty);
        let edited = frame(&mut renderer, &world, &objects, None, eye);
        assert!(renderer.shadow_updates > 0);
        assert_ne!(edited, empty);
        renderer.rebuild_all(&world);
        assert_eq!(edited, frame(&mut renderer, &world, &objects, None, eye));

        objects[0].position.x += 2.0;
        let moved_light = frame(&mut renderer, &world, &objects, None, eye);
        assert_eq!(
            renderer.shadow_updates, 1,
            "only the moved light needs updating"
        );
        renderer.objects(&mesh.vertices, &mesh.indices);
        assert_eq!(
            moved_light,
            frame(&mut renderer, &world, &objects, None, eye)
        );
        let moved_eye = eye + Vec3::new(10.0, 2.0, 0.0);
        let moved_window = frame(&mut renderer, &world, &objects, None, moved_eye);
        assert_eq!(
            renderer.shadow_updates, 1,
            "only the sun window needs updating"
        );
        renderer.objects(&mesh.vertices, &mesh.indices);
        assert_eq!(
            moved_window,
            frame(&mut renderer, &world, &objects, None, moved_eye)
        );

        for settings in [
            RenderSettings {
                scale: 0.5,
                msaa: crate::render::settings::Antialiasing::Off,
                bloom: 0.0,
                ..Default::default()
            },
            RenderSettings::default(),
        ] {
            renderer.apply_settings(settings);
            let pixels = frame(
                &mut renderer,
                &world,
                &objects,
                None,
                Vec3::new(-4.0, 2.0, 5.0),
            );
            assert_eq!(pixels.len(), 321 * 181 * 4);
            assert!(pixels.as_chunks::<4>().0.iter().all(|p| p[3] == 255));
            assert!(pixels.as_chunks::<4>().0.iter().any(|p| p[1] > p[0]));
        }
    }
}
