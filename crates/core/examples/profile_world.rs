//! Small repeatable CPU sanity check, not a graphics or end-to-end FPS benchmark.

use sauerteig_core::{
    forward, simulate_player, spawn_point, Cell, CellEdit, InputFrame, Player, Shape, Vec3, World,
    FIXED_DT,
};
use std::{hint::black_box, time::Instant};

fn measure(mut operation: impl FnMut(usize), warmup: usize, repetitions: usize) -> Vec<f64> {
    for sample in 0..warmup {
        operation(sample);
    }
    (0..repetitions)
        .map(|sample| {
            let start = Instant::now();
            operation(sample);
            start.elapsed().as_secs_f64() * 1_000.0
        })
        .collect()
}

fn report(name: &str, mut milliseconds: Vec<f64>) {
    milliseconds.sort_unstable_by(f64::total_cmp);
    let count = milliseconds.len();
    let percentile = |percent: usize| milliseconds[(count * percent).div_ceil(100) - 1];
    println!(
        "{name:<33} {count:>6} {median:>12.4} {p95:>12.4}",
        median = percentile(50),
        p95 = percentile(95),
    );
}

fn main() {
    let world = World::arena();
    let keys = world.chunk_keys();
    let cells = world.cells();
    let (vertices, triangles) = keys
        .iter()
        .map(|&key| {
            let mesh = world.mesh_chunk(key);
            (mesh.vertices.len(), mesh.indices.len() / 3)
        })
        .fold((0, 0), |(v, t), (next_v, next_t)| (v + next_v, t + next_t));

    println!("Sauerteig core CPU profile");
    println!(
        "Target: {} / {}",
        std::env::consts::OS,
        std::env::consts::ARCH
    );
    println!(
        "Build: {}",
        if cfg!(debug_assertions) {
            "debug (use --release)"
        } else {
            "release"
        }
    );
    println!(
        "Arena: {} occupied cells, {} chunks, {vertices} vertices, {triangles} triangles",
        cells.len(),
        keys.len()
    );
    println!("World construction and fixture preparation are excluded from timings.");
    println!("Meshing includes CPU allocation/generation/drop, without GPU upload.");
    println!();
    println!(
        "{:<33} {:>6} {:>12} {:>12}",
        "Operation", "Runs", "p50 (ms)", "p95 (ms)"
    );

    report(
        "Full arena mesh",
        measure(
            |_| {
                for &key in &keys {
                    black_box(black_box(&world).mesh_chunk(black_box(key)));
                }
            },
            24,
            200,
        ),
    );

    let mut edited_world = world.clone();
    // Floor/chunk boundary, bridge, pillar, wall, gallery, ramp, and stairs.
    let edit_positions = [
        [-24, 0, -24],
        [0, 0, 8],
        [0, 4, 0],
        [7, 2, 1],
        [23, 1, 0],
        [-15, 4, 0],
        [11, 1, 15],
        [-13, 4, -9],
    ];
    let originals = edit_positions.map(|pos| {
        edited_world
            .get(pos)
            .expect("profiling edit fixture must contain a cell")
    });
    report(
        "One edit + dirty chunk meshes",
        measure(
            |sample| {
                let index = (sample / 2) % edit_positions.len();
                // Each removal is immediately followed by restoration; every
                // timed edit changes geometry and the fixture stays bounded.
                let edit = CellEdit {
                    pos: edit_positions[index],
                    cell: if sample % 2 == 0 {
                        None
                    } else {
                        Some(originals[index])
                    },
                };
                let dirty = edited_world.apply_edits(black_box(&[edit]));
                black_box(dirty.len());
                for key in dirty {
                    black_box(edited_world.mesh_chunk(black_box(key)));
                }
            },
            64,
            512,
        ),
    );

    let mut shape = Shape::solid();
    shape.vertices[4][0] = 2;
    shape.vertices[7][0] = 2;
    let shaped = Cell {
        shape: Some(shape),
        ..Cell::solid(1)
    };
    assert!(shaped.is_valid());
    let large_edits: Vec<_> = (32..40)
        .flat_map(|x| {
            (8..16).flat_map(move |y| {
                (32..40).map(move |z| CellEdit {
                    pos: [x, y, z],
                    cell: Some(shaped),
                })
            })
        })
        .collect();
    let alternate: Vec<_> = large_edits
        .iter()
        .map(|e| CellEdit {
            pos: e.pos,
            cell: Some(Cell {
                material: 2,
                ..shaped
            }),
        })
        .collect();
    let mut shape_world = world.clone();
    report(
        "512 shaped replacements + meshes",
        measure(
            |sample| {
                let edits = if sample % 2 == 0 {
                    &large_edits
                } else {
                    &alternate
                };
                let dirty = shape_world.apply_edits(black_box(edits));
                for key in dirty {
                    black_box(shape_world.mesh_chunk(key));
                }
            },
            8,
            64,
        ),
    );

    let mut players: Vec<_> = (0..16)
        .map(|id| Player::new(id, format!("Profile{id}")))
        .collect();
    let mut tick = 0u32;
    // Each sample times a complete 16-player world-collision tick. Input setup
    // is included, as in a simulation loop; networking/combat/props are absent.
    report(
        "16-player movement tick",
        measure(
            |_| {
                tick += 1;
                for (index, player) in players.iter_mut().enumerate() {
                    // Revisit the arena instead of timing only players stuck
                    // against a wall after a long forward run.
                    if tick % 120 == 1 {
                        player.position = spawn_point(index as u32);
                        player.velocity = Vec3::ZERO;
                        player.grounded = false;
                    }
                    let to_center = -player.position;
                    let input = InputFrame {
                        seq: tick,
                        forward: 1.0,
                        strafe: if index % 2 == 0 { 0.35 } else { -0.35 },
                        yaw: to_center.x.atan2(-to_center.z),
                        jump: (tick + index as u32 * 7) % 90 == 0,
                        ..InputFrame::default()
                    };
                    simulate_player(black_box(&world), player, black_box(&input), FIXED_DT);
                }
                black_box(&players);
            },
            120,
            600,
        ),
    );

    // Deterministic mix of eyes at spawn and above the bridge, 360-degree aims,
    // and upward/downward shots. Each operation is one query, including misses.
    let rays: Vec<_> = (0..512)
        .map(|index| {
            let origin = if index % 3 == 0 {
                Vec3::new(0.5, 7.0, 0.5)
            } else {
                spawn_point(index % 8) + Vec3::Y * sauerteig_core::EYE_HEIGHT
            };
            let yaw = index as f32 * 2.399_963_1;
            let pitch = ((index % 9) as f32 - 4.0) * 0.13;
            (origin, forward(yaw, pitch))
        })
        .collect();
    let hit_count = rays
        .iter()
        .filter(|&&(origin, direction)| world.raycast(origin, direction, 128.0).is_some())
        .count();
    report(
        "One world raycast (128u max)",
        measure(
            |sample| {
                let (origin, direction) = rays[sample % rays.len()];
                black_box(black_box(&world).raycast(
                    black_box(origin),
                    black_box(direction),
                    128.0,
                ));
            },
            512,
            4096,
        ),
    );
    println!();
    println!("Ray fixture: {hit_count} hits / {} rays; edit invalidation rebuilds the touched chunk plus six neighbors.", rays.len());
    println!("Times are wall-clock samples on this machine. They are not GPU frame times,");
    println!("browser/WASM results, network measurements, or proof of a laptop FPS budget.");
}
