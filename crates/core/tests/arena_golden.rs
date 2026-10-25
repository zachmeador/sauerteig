//! Arena movement must keep reproducing the trajectories recorded from the
//! movement code that existed before movement profiles.
//!
//! Every scenario scripts inputs against a fixed world and compares the full
//! per-tick state (position, velocity, grounded) bit for bit with
//! `golden/arena_trajectories.txt`. Scenarios marked `trig` steer with
//! `sin`/`cos` of non-trivial angles, whose last bit differs between platform
//! math libraries; they are exact on the platform that recorded the file and
//! within `TRIG_TOLERANCE` elsewhere. Every other scenario is exact everywhere.
//!
//! To record again after an intentional change to Arena movement:
//! `SAUERTEIG_RECORD_GOLDEN=1 cargo test -p sauerteig-core --test arena_golden`

use sauerteig_core::*;
use std::f32::consts::{FRAC_PI_2, PI};
use std::fmt::Write as _;

const GOLDEN: &str = include_str!("golden/arena_trajectories.txt");
const TRIG_TOLERANCE: f32 = 2.0e-3;

struct Recorder {
    lines: Vec<String>,
    seq: u32,
}

impl Recorder {
    fn new() -> Self {
        Self {
            lines: Vec::new(),
            seq: 0,
        }
    }

    fn step(&mut self, world: &World, player: &mut Player, input: &InputFrame, dt: f32) {
        let mut explicit = player.clone();
        simulate_player_with(world, &mut explicit, input, dt, &MovementProfile::arena());
        simulate_player(world, player, input, dt);
        assert_eq!(player.position, explicit.position);
        assert_eq!(player.velocity, explicit.velocity);
        assert!(player.movement.is_idle(), "Arena leaves no movement state");
        let bits = |value: f32| format!("{:08x}", value.to_bits());
        self.lines.push(format!(
            "{} {} {} {} {} {} {}",
            bits(player.position.x),
            bits(player.position.y),
            bits(player.position.z),
            bits(player.velocity.x),
            bits(player.velocity.y),
            bits(player.velocity.z),
            player.grounded as u8
        ));
    }

    fn run(&mut self, world: &World, player: &mut Player, input: InputFrame, ticks: usize) {
        for _ in 0..ticks {
            self.seq += 1;
            let input = InputFrame {
                seq: self.seq,
                ..input
            };
            self.step(world, player, &input, FIXED_DT);
        }
    }
}

fn floor() -> World {
    let mut world = World::empty();
    for x in -20..20 {
        for z in -20..20 {
            world.set([x, 0, z], Some(Cell::solid(0)));
        }
    }
    world
}

fn player() -> Player {
    let mut player = Player::new(0, "golden".into());
    player.position = Vec3::new(0.5, 1.01, 0.5);
    player
}

fn input() -> InputFrame {
    InputFrame::default()
}

fn forward(value: f32) -> InputFrame {
    InputFrame {
        forward: value,
        ..input()
    }
}

fn strafe(value: f32) -> InputFrame {
    InputFrame {
        strafe: value,
        ..input()
    }
}

fn vertical(value: f32) -> InputFrame {
    InputFrame {
        vertical: value,
        ..input()
    }
}

fn jump() -> InputFrame {
    InputFrame {
        jump: true,
        ..input()
    }
}

fn stairs_world() -> World {
    let mut world = floor();
    for step in 0..8 {
        let x = 2 + step;
        let level = 1 + step / 2;
        for z in -1..2 {
            for y in 1..level {
                world.set([x, y, z], Some(Cell::solid(0)));
            }
            world.set(
                [x, level, z],
                Some(Cell {
                    material: 0,
                    heights: [if step % 2 == 0 { 4 } else { 8 }; 4],
                    diagonal: false,
                    shape: None,
                    face_materials: None,
                }),
            );
        }
    }
    world
}

fn ramps_world(explicit_shapes: bool) -> World {
    let mut world = floor();
    for step in 0..8 {
        let x = 2 + step;
        let level = 1 + step / 2;
        let low = if step % 2 == 0 { 0 } else { 4 };
        for z in -1..2 {
            for y in 1..level {
                world.set([x, y, z], Some(Cell::solid(0)));
            }
            let legacy = Cell {
                heights: [low, low + 4, low + 4, low],
                ..Cell::solid(3)
            };
            let cell = if explicit_shapes {
                Cell {
                    shape: Some(legacy.as_shape()),
                    ..legacy
                }
            } else {
                legacy
            };
            world.set([x, level, z], Some(cell));
        }
    }
    world
}

fn flat_ground(out: &mut Recorder) {
    let world = floor();
    let mut p = player();
    out.run(&world, &mut p, input(), 20);
    out.run(&world, &mut p, forward(1.0), 45);
    out.run(
        &world,
        &mut p,
        InputFrame {
            forward: 1.0,
            strafe: 1.0,
            ..input()
        },
        40,
    );
    out.run(&world, &mut p, strafe(-1.0), 30);
    out.run(&world, &mut p, forward(0.5), 20);
    out.run(&world, &mut p, input(), 60);
    out.run(&world, &mut p, forward(-1.0), 30);
    out.run(&world, &mut p, input(), 30);
}

fn varied_time_steps(out: &mut Recorder) {
    let world = floor();
    let mut p = player();
    out.run(&world, &mut p, input(), 10);
    let steps = [
        FIXED_DT,
        1.0 / 30.0,
        1.0 / 120.0,
        0.2,
        0.0,
        -1.0,
        f32::NAN,
        f32::INFINITY,
        0.05,
    ];
    for round in 0..4 {
        for (index, dt) in steps.into_iter().enumerate() {
            out.seq += 1;
            let frame = InputFrame {
                seq: out.seq,
                forward: 1.0,
                jump: round == 2 && index < 3,
                ..input()
            };
            out.step(&world, &mut p, &frame, dt);
        }
    }
    out.run(&world, &mut p, input(), 20);
}

fn stairs(out: &mut Recorder) {
    let world = stairs_world();
    let mut p = player();
    out.run(&world, &mut p, strafe(1.0), 85);
    out.run(&world, &mut p, input(), 20);
    out.run(&world, &mut p, strafe(-1.0), 95);
    out.run(&world, &mut p, input(), 30);
}

fn ramps(out: &mut Recorder, explicit_shapes: bool) {
    let world = ramps_world(explicit_shapes);
    let mut p = player();
    out.run(&world, &mut p, strafe(1.0), 85);
    out.run(&world, &mut p, input(), 20);
    out.run(&world, &mut p, strafe(-1.0), 95);
    out.run(&world, &mut p, input(), 30);
}

fn jumping(out: &mut Recorder) {
    let world = floor();
    let mut p = player();
    out.run(&world, &mut p, input(), 10);
    out.run(&world, &mut p, jump(), 1);
    out.run(&world, &mut p, input(), 70);
    out.run(
        &world,
        &mut p,
        InputFrame {
            forward: 1.0,
            jump: true,
            ..input()
        },
        100,
    );
    out.run(&world, &mut p, input(), 60);
}

fn ceiling_jump(out: &mut Recorder) {
    let mut world = floor();
    for x in -1..2 {
        for z in -1..2 {
            world.set(
                [x, 3, z],
                Some(Cell {
                    material: 0,
                    heights: [1, 4, 8, 4],
                    diagonal: false,
                    shape: None,
                    face_materials: None,
                }),
            );
        }
    }
    let mut p = player();
    out.run(&world, &mut p, jump(), 1);
    out.run(&world, &mut p, input(), 70);
}

fn air_strafing(out: &mut Recorder) {
    let world = floor();
    let mut p = player();
    out.run(&world, &mut p, forward(1.0), 30);
    out.run(
        &world,
        &mut p,
        InputFrame {
            forward: 1.0,
            jump: true,
            ..input()
        },
        1,
    );
    for strafe_axis in [1.0, -1.0, 1.0, -1.0] {
        out.run(
            &world,
            &mut p,
            InputFrame {
                forward: 1.0,
                strafe: strafe_axis,
                ..input()
            },
            10,
        );
    }
    out.run(&world, &mut p, forward(1.0), 20);
    out.run(&world, &mut p, input(), 40);
}

fn wall_slide(out: &mut Recorder) {
    let mut world = floor();
    for y in 1..5 {
        for z in -20..20 {
            world.set([3, y, z], Some(Cell::solid(1)));
        }
        world.set([2, y, -3], Some(Cell::solid(1)));
    }
    let mut p = player();
    out.run(&world, &mut p, strafe(1.0), 90);
    out.run(
        &world,
        &mut p,
        InputFrame {
            strafe: 1.0,
            forward: 1.0,
            ..input()
        },
        30,
    );
    out.run(
        &world,
        &mut p,
        InputFrame {
            strafe: -1.0,
            forward: 1.0,
            ..input()
        },
        20,
    );
    out.run(&world, &mut p, strafe(1.0), 30);
}

fn ledge_fall(out: &mut Recorder) {
    let mut world = floor();
    for x in -20..2 {
        for z in -20..20 {
            for y in 1..4 {
                world.set([x, y, z], Some(Cell::solid(2)));
            }
        }
    }
    let mut p = player();
    p.position = Vec3::new(0.5, 4.01, 0.5);
    out.run(&world, &mut p, input(), 10);
    out.run(&world, &mut p, strafe(1.0), 50);
    out.run(&world, &mut p, input(), 60);
}

fn pool_world() -> World {
    let mut world = floor();
    world
        .replace_volumes(&[
            MaterialVolume {
                bounds: FineBox {
                    min: [-64, 8, -64],
                    max: [0, 32, 64],
                },
                material: VolumeMaterial::Water,
            },
            MaterialVolume {
                bounds: FineBox {
                    min: [0, 8, -64],
                    max: [64, 13, 64],
                },
                material: VolumeMaterial::Water,
            },
        ])
        .unwrap();
    world
}

fn water(out: &mut Recorder) {
    let world = pool_world();
    let mut p = player();
    p.position = Vec3::new(-6.0, 6.0, 0.5);
    out.run(&world, &mut p, input(), 40);
    out.run(&world, &mut p, jump(), 40);
    out.run(
        &world,
        &mut p,
        InputFrame {
            strafe: 1.0,
            forward: 0.5,
            ..input()
        },
        50,
    );
    out.run(&world, &mut p, vertical(-1.0), 30);
    out.run(&world, &mut p, strafe(1.0), 90);
    out.run(&world, &mut p, jump(), 12);
    out.run(&world, &mut p, forward(-1.0), 40);
    out.run(&world, &mut p, input(), 40);
}

fn edit_flight(out: &mut Recorder) {
    let world = floor();
    let mut p = player();
    p.editing = true;
    p.position = Vec3::new(63.0, 45.0, -63.0);
    out.run(&world, &mut p, strafe(1.0), 12);
    out.run(&world, &mut p, vertical(1.0), 15);
    out.run(&world, &mut p, forward(1.0), 12);
    out.run(
        &world,
        &mut p,
        InputFrame {
            strafe: -1.0,
            forward: -1.0,
            vertical: -1.0,
            ..input()
        },
        30,
    );
    out.run(&world, &mut p, vertical(-1.0), 160);
    out.run(&world, &mut p, input(), 5);
    p.editing = false;
    out.run(&world, &mut p, input(), 30);
}

fn glass(out: &mut Recorder) {
    let mut world = World::empty();
    let volume = |min: [i32; 3], max: [i32; 3]| MaterialVolume {
        bounds: FineBox { min, max },
        material: VolumeMaterial::Glass,
    };
    world
        .replace_volumes(&[
            volume([-64, 0, -64], [64, 8, 64]),
            volume([16, 8, -16], [17, 48, 16]),
            volume([-8, 32, -8], [8, 33, 8]),
        ])
        .unwrap();
    let mut p = Player::new(1, "glass".into());
    p.position = Vec3::new(0.0, 3.0, 0.0);
    out.run(&world, &mut p, input(), 60);
    out.run(&world, &mut p, jump(), 30);
    out.run(&world, &mut p, strafe(1.0), 120);
    out.run(&world, &mut p, input(), 10);
}

fn compact_world(out: &mut Recorder) {
    let bounds = WorldBounds {
        min: [-256, -128, -256],
        max: [512, 256, 512],
    };
    let region = |origin: [i32; 3], size: u32, cell: Cell| CellFragment {
        origin,
        size,
        bounds: FineBox {
            min: origin,
            max: origin.map(|value| value + size as i32),
        },
        cell,
    };
    let floor = region([128 * 8, 96 * 8, 128 * 8], 128, Cell::solid(0));
    let mut ceiling = region([128 * 8, 114 * 8, 128 * 8], 128, Cell::solid(0));
    ceiling.bounds.max[1] = 115 * 8;
    let world = World::from_compact_parts(bounds, &[floor, ceiling], &[], &[]).unwrap();
    let mut p = Player::new(2, "compact".into());
    p.position = Vec3::new(136.5, 112.51, 136.5);
    out.run(&world, &mut p, input(), 10);
    out.run(&world, &mut p, jump(), 1);
    out.run(&world, &mut p, input(), 70);
    out.run(&world, &mut p, strafe(1.0), 40);
    out.run(&world, &mut p, forward(-1.0), 30);

    let slope = region(
        [128 * 8, 96 * 8, 128 * 8],
        128,
        Cell {
            heights: [0, 8, 8, 0],
            ..Cell::solid(0)
        },
    );
    let world = World::from_compact_parts(bounds, &[slope], &[], &[]).unwrap();
    let mut p = Player::new(2, "compact".into());
    p.position = Vec3::new(136.0, 104.31, 136.0);
    out.run(&world, &mut p, input(), 2);
    out.run(&world, &mut p, strafe(1.0), 60);
    out.run(&world, &mut p, strafe(-1.0), 60);
}

fn foundry(out: &mut Recorder) {
    let world = World::arena();
    let mut stairs = Player::new(1, "stairs".into());
    stairs.position = Vec3::new(-14.5, 1.01, -17.5);
    out.run(&world, &mut stairs, forward(-1.0), 100);
    let mut ramp = Player::new(2, "ramp".into());
    ramp.position = Vec3::new(15.0, 1.01, 16.5);
    out.run(&world, &mut ramp, forward(1.0), 100);
    let mut crossing = Player::new(3, "catwalk".into());
    crossing.position = Vec3::new(-14.0, 5.01, 0.0);
    out.run(&world, &mut crossing, strafe(1.0), 190);
}

fn void_respawn(out: &mut Recorder) {
    let world = World::empty();
    let mut p = player();
    out.run(&world, &mut p, forward(1.0), 130);
}

fn invalid_state(out: &mut Recorder) {
    let world = floor();
    let mut p = player();
    p.position = Vec3::new(f32::NAN, 1.0, 0.0);
    out.run(&world, &mut p, input(), 5);
    p.velocity = Vec3::new(f32::INFINITY, 0.0, f32::NAN);
    out.run(&world, &mut p, forward(1.0), 5);
    out.run(
        &world,
        &mut p,
        InputFrame {
            forward: f32::NAN,
            strafe: f32::INFINITY,
            vertical: f32::NEG_INFINITY,
            yaw: f32::NAN,
            pitch: f32::INFINITY,
            jump: true,
            ..input()
        },
        10,
    );
    out.run(&world, &mut p, forward(5.0), 20);
    out.run(&world, &mut p, strafe(7.5), 20);
    p.health = 0;
    out.run(&world, &mut p, forward(1.0), 5);
    p.health = 100;
    out.run(&world, &mut p, forward(1.0), 10);
}

fn turning(out: &mut Recorder) {
    let world = floor();
    let mut p = player();
    for tick in 0..140 {
        out.seq += 1;
        let frame = InputFrame {
            seq: out.seq,
            yaw: tick as f32 * 0.02 + 0.3,
            forward: 1.0,
            strafe: if tick % 40 < 20 { 0.0 } else { 1.0 },
            jump: tick == 12 || tick == 70,
            ..input()
        };
        out.step(&world, &mut p, &frame, FIXED_DT);
    }
    for tick in 0..60 {
        out.seq += 1;
        let frame = InputFrame {
            seq: out.seq,
            yaw: FRAC_PI_2 + tick as f32 * 0.05,
            ..input()
        };
        out.step(&world, &mut p, &frame, FIXED_DT);
    }
}

fn pitched_flight(out: &mut Recorder) {
    let world = floor();
    let mut p = player();
    p.editing = true;
    p.position = Vec3::new(0.0, 10.0, 0.0);
    for tick in 0..120 {
        out.seq += 1;
        let frame = InputFrame {
            seq: out.seq,
            yaw: PI * 0.25 + tick as f32 * 0.03,
            pitch: (tick as f32 * 0.07).sin() * 1.2,
            forward: 1.0,
            strafe: if tick > 60 { -1.0 } else { 0.0 },
            vertical: if tick % 50 > 40 { 1.0 } else { 0.0 },
            ..input()
        };
        out.step(&world, &mut p, &frame, FIXED_DT);
    }
}

type Scenario = (&'static str, bool, fn(&mut Recorder));

fn scenarios() -> Vec<Scenario> {
    vec![
        ("flat_ground", false, flat_ground),
        ("varied_time_steps", false, varied_time_steps),
        ("stairs", false, stairs),
        ("ramps", false, |out| ramps(out, false)),
        ("shape_ramps", false, |out| ramps(out, true)),
        ("jumping", false, jumping),
        ("ceiling_jump", false, ceiling_jump),
        ("air_strafing", false, air_strafing),
        ("wall_slide", false, wall_slide),
        ("ledge_fall", false, ledge_fall),
        ("water", false, water),
        ("edit_flight", false, edit_flight),
        ("glass", false, glass),
        ("compact_world", false, compact_world),
        ("foundry", false, foundry),
        ("void_respawn", false, void_respawn),
        ("invalid_state", false, invalid_state),
        ("turning", true, turning),
        ("pitched_flight", true, pitched_flight),
    ]
}

fn platform() -> String {
    format!("{}-{}", std::env::consts::ARCH, std::env::consts::OS)
}

fn render() -> String {
    let mut text = String::new();
    writeln!(
        text,
        "# Arena trajectories recorded from simulate_player before movement profiles."
    )
    .unwrap();
    writeln!(
        text,
        "# Per tick: position, velocity (f32 bit patterns), grounded."
    )
    .unwrap();
    writeln!(text, "platform {}", platform()).unwrap();
    for (name, trig, run) in scenarios() {
        let mut out = Recorder::new();
        run(&mut out);
        writeln!(text, "scenario {name}{}", if trig { " trig" } else { "" }).unwrap();
        for line in out.lines {
            writeln!(text, "{line}").unwrap();
        }
    }
    text
}

fn float(token: &str) -> f32 {
    f32::from_bits(u32::from_str_radix(token, 16).unwrap())
}

#[test]
fn arena_reproduces_the_recorded_trajectories() {
    if std::env::var_os("SAUERTEIG_RECORD_GOLDEN").is_some() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/golden/arena_trajectories.txt"
        );
        std::fs::write(path, render()).unwrap();
        return;
    }
    let recorded_platform = GOLDEN
        .lines()
        .find_map(|line| line.strip_prefix("platform "))
        .expect("golden file names its platform");
    let exact_platform = recorded_platform == platform();
    let mut recorded = GOLDEN
        .lines()
        .filter(|line| !line.starts_with('#') && !line.starts_with("platform "))
        .peekable();
    for (name, trig, run) in scenarios() {
        let header = format!("scenario {name}{}", if trig { " trig" } else { "" });
        assert_eq!(recorded.next(), Some(header.as_str()), "scenario order");
        let mut out = Recorder::new();
        run(&mut out);
        let exact = !trig || exact_platform;
        for (tick, line) in out.lines.iter().enumerate() {
            let expected = recorded
                .next_if(|line| !line.starts_with("scenario "))
                .unwrap_or_else(|| panic!("{name}: recording ends at tick {tick}"));
            if exact {
                assert_eq!(line, expected, "{name}: tick {tick} differs from recording");
                continue;
            }
            let (actual, wanted): (Vec<_>, Vec<_>) = (
                line.split(' ').collect::<Vec<_>>(),
                expected.split(' ').collect::<Vec<_>>(),
            );
            assert_eq!(actual[6], wanted[6], "{name}: tick {tick} grounded flag");
            for (a, w) in actual[..6].iter().zip(&wanted[..6]) {
                let (a, w) = (float(a), float(w));
                assert!(
                    (a - w).abs() <= TRIG_TOLERANCE,
                    "{name}: tick {tick} differs from recording by more than {TRIG_TOLERANCE}: {a} vs {w}"
                );
            }
        }
        assert!(
            recorded
                .peek()
                .is_none_or(|line| line.starts_with("scenario ")),
            "{name}: recording is longer than the scenario"
        );
    }
    assert_eq!(recorded.next(), None, "unused recorded scenarios");
}
