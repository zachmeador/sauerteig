//! A reference for Sauerbraten movement: a literal port of the velocity math in
//! `modifygravity`, `modifyvelocity` and the velocity part of `moveplayer` from
//! `src/engine/physics.cpp` of the Sauerbraten source, in Sauerbraten units
//! (8 per metre; positions in units, speeds in units per second, z up, angles in
//! degrees). It has no collision: a flat floor at z = 0 stands in for the
//! engine's `move`, and the engine's 5 ms physics frames are kept.
//!
//! The same module measures the quantities the movement-profile tests compare,
//! for the oracle and for a [`MovementProfile`] running in the real simulation.
//!
//! Sauerbraten is zlib licensed, Copyright (C) 2001-2020 Wouter van Oortmerssen,
//! Lee Salzman, Mike Dysart, Robert Pointon, and Quinton Reeves; see
//! `THIRD_PARTY_NOTICES.md`.

#![allow(dead_code)]

use sauerteig_core::*;

const GRAVITY: f32 = 200.0;
const JUMPVEL: f32 = 125.0;
const SLOPEZ: f32 = 0.5;
const FLOORZ: f32 = 0.867;
const RAD: f32 = std::f32::consts::PI / 180.0;
const PHYSFRAMETIME: i32 = 5;
const UNITS_PER_METRE: f32 = 8.0;

const PHYS_FALL: u8 = 1;
const PHYS_SLOPE: u8 = 3;
const PHYS_FLOOR: u8 = 4;

/// The `physent` fields the velocity math reads and writes.
#[derive(Clone, Copy, Debug)]
pub struct Ent {
    /// Eye position, as in the engine.
    pub o: Vec3,
    pub vel: Vec3,
    pub falling: Vec3,
    pub floor: Vec3,
    pub yaw: f32,
    pub pitch: f32,
    pub mv: i32,
    pub strafe: i32,
    pub jumping: bool,
    pub inwater: bool,
    pub physstate: u8,
    pub maxspeed: f32,
    pub eyeheight: f32,
}

impl Ent {
    /// A player standing on the floor.
    pub fn standing() -> Self {
        Self {
            o: Vec3::new(0.0, 0.0, 14.0),
            vel: Vec3::ZERO,
            falling: Vec3::ZERO,
            floor: Vec3::new(0.0, 0.0, 1.0),
            yaw: 0.0,
            pitch: 0.0,
            mv: 0,
            strafe: 0,
            jumping: false,
            inwater: false,
            physstate: PHYS_FLOOR,
            maxspeed: 100.0,
            eyeheight: 14.0,
        }
    }

    /// A player high above the floor, with a long way to fall.
    pub fn airborne() -> Self {
        Self {
            o: Vec3::new(0.0, 0.0, 10_000.0),
            physstate: PHYS_FALL,
            ..Self::standing()
        }
    }

    pub fn feet(&self) -> f32 {
        self.o.z - self.eyeheight
    }
}

fn vecfromyawpitch(yaw: f32, pitch: f32, mv: i32, strafe: i32) -> Vec3 {
    let mut m = Vec3::ZERO;
    if mv != 0 {
        m.x = mv as f32 * -(RAD * yaw).sin();
        m.y = mv as f32 * (RAD * yaw).cos();
    }
    if pitch != 0.0 {
        m.x *= (RAD * pitch).cos();
        m.y *= (RAD * pitch).cos();
        m.z = mv as f32 * (RAD * pitch).sin();
    }
    if strafe != 0 {
        m.x += strafe as f32 * (RAD * yaw).cos();
        m.y += strafe as f32 * (RAD * yaw).sin();
    }
    m
}

fn modifygravity(pl: &mut Ent, water: bool, curtime: i32) {
    let secs = curtime as f32 / 1000.0;
    let mut g = Vec3::ZERO;
    if pl.physstate == PHYS_FALL {
        g.z -= GRAVITY * secs;
    } else if pl.floor.z > 0.0 && pl.floor.z < FLOORZ {
        g.z = -1.0;
        g -= pl.floor * g.dot(pl.floor);
        g /= g.length();
        g *= GRAVITY * secs;
    }
    if !water || (pl.mv == 0 && pl.strafe == 0) {
        pl.falling += g;
    }

    if water || pl.physstate >= PHYS_SLOPE {
        let fric: f32 = if water { 2.0 } else { 6.0 };
        let c = if water {
            1.0
        } else {
            ((pl.floor.z - SLOPEZ) / (FLOORZ - SLOPEZ)).clamp(0.0, 1.0)
        };
        pl.falling *= (1.0 - c / fric).powf(curtime as f32 / 20.0);
    }
}

fn modifyvelocity(pl: &mut Ent, water: bool, curtime: i32) {
    if pl.physstate >= PHYS_SLOPE || water {
        if water && !pl.inwater {
            pl.vel /= 8.0;
        }
        if pl.jumping {
            pl.jumping = false;
            pl.vel.z = pl.vel.z.max(JUMPVEL);
            if water {
                pl.vel.x /= 8.0;
                pl.vel.y /= 8.0;
            }
        }
    }

    let mut m = Vec3::ZERO;
    if pl.mv != 0 || pl.strafe != 0 {
        m = vecfromyawpitch(pl.yaw, if water { pl.pitch } else { 0.0 }, pl.mv, pl.strafe);
        if pl.physstate >= PHYS_SLOPE {
            let dz = -(m.x * pl.floor.x + m.y * pl.floor.y) / pl.floor.z;
            m.z = if water { m.z.max(dz) } else { dz };
        }
        m /= m.length();
    }

    let mut d = m * pl.maxspeed;
    if !water {
        d *= (if pl.mv != 0 && pl.strafe == 0 {
            1.3
        } else {
            1.0
        }) * (if pl.physstate < PHYS_SLOPE { 1.3 } else { 1.0 });
    }
    let fric: f32 = if water {
        20.0
    } else if pl.physstate >= PHYS_SLOPE {
        6.0
    } else {
        30.0
    };
    let f = (1.0 - 1.0 / fric).powf(curtime as f32 / 20.0);
    pl.vel = Vec3::new(
        d.x + (pl.vel.x - d.x) * f,
        d.y + (pl.vel.y - d.y) * f,
        d.z + (pl.vel.z - d.z) * f,
    );
}

/// One physics frame; returns the displacement the frame asked for. The
/// collision in `move` becomes a flat floor at z = 0: crossing it lands the
/// player (`switchfloor` and `landing` clear the falling vector and the
/// downward velocity), and being above it leaves them falling.
pub fn moveplayer(pl: &mut Ent, water: bool, curtime: i32) -> Vec3 {
    let secs = curtime as f32 / 1000.0;
    modifygravity(pl, water, curtime);
    modifyvelocity(pl, water, curtime);

    let mut d = pl.vel;
    if water {
        d *= 0.5;
    }
    d += pl.falling;
    d *= secs;
    pl.o += d;

    if pl.feet() <= 0.0 && d.z <= 0.0 {
        pl.o.z = pl.eyeheight;
        pl.physstate = PHYS_FLOOR;
        pl.floor = Vec3::new(0.0, 0.0, 1.0);
        pl.falling = Vec3::ZERO;
        pl.vel.z = 0.0;
    } else if pl.feet() > 0.0 {
        pl.physstate = PHYS_FALL;
    }
    pl.inwater = water;
    d
}

/// Seconds per oracle frame.
pub const FRAME: f32 = PHYSFRAMETIME as f32 / 1000.0;

/// Everything the profile comparison measures, in metres and seconds.
#[derive(Clone, Copy, Debug, Default)]
pub struct Metrics {
    pub top_forward: f32,
    pub top_strafe: f32,
    pub top_airborne: f32,
    pub time_to_90: f32,
    pub jump_apex: f32,
    pub air_time: f32,
    pub stop_distance: f32,
}

impl Metrics {
    pub const NAMES: [&'static str; 7] = [
        "top speed, forward (m/s)",
        "top speed, strafing (m/s)",
        "top speed, airborne (m/s)",
        "time to 90% of top speed (s)",
        "jump apex height (m)",
        "jump air time (s)",
        "stopping distance (m)",
    ];

    pub fn values(&self) -> [f32; 7] {
        [
            self.top_forward,
            self.top_strafe,
            self.top_airborne,
            self.time_to_90,
            self.jump_apex,
            self.air_time,
            self.stop_distance,
        ]
    }
}

/// First time a sampled rising signal reaches `level`, interpolated linearly
/// inside the sample that crosses it.
fn rise_time(samples: &[f32], interval: f32, level: f32) -> f32 {
    let mut previous = 0.0;
    for (index, &value) in samples.iter().enumerate() {
        if value >= level {
            let fraction = (level - previous) / (value - previous);
            return (index as f32 + fraction) * interval;
        }
        previous = value;
    }
    f32::NAN
}

/// When a body that fell from `height` with vertical speed `climb` reaches the
/// floor, under constant gravity `g`.
fn time_to_floor(height: f32, climb: f32, g: f32) -> f32 {
    (climb + (climb * climb + 2.0 * g * height).sqrt()) / g
}

fn horizontal(x: f32, y: f32) -> f32 {
    (x * x + y * y).sqrt()
}

/// The oracle's measurements, run at the engine's 5 ms frames.
pub fn measure_oracle() -> Metrics {
    let frames = |seconds: f32| (seconds / FRAME) as usize;
    let run = |mut pl: Ent, mv: i32, strafe: i32, count: usize| -> Vec<Ent> {
        pl.mv = mv;
        pl.strafe = strafe;
        (0..count)
            .map(|_| {
                moveplayer(&mut pl, false, PHYSFRAMETIME);
                pl
            })
            .collect()
    };
    let speed = |pl: &Ent| horizontal(pl.vel.x, pl.vel.y) / UNITS_PER_METRE;

    let forward = run(Ent::standing(), 1, 0, frames(3.0));
    let top_forward = speed(forward.last().unwrap());
    let top_strafe = speed(run(Ent::standing(), 0, 1, frames(3.0)).last().unwrap());
    let top_airborne = speed(run(Ent::airborne(), 1, 0, frames(6.0)).last().unwrap());
    let speeds: Vec<f32> = forward.iter().map(speed).collect();
    let time_to_90 = rise_time(&speeds, FRAME, 0.9 * top_forward);

    let mut jumper = Ent::standing();
    jumper.jumping = true;
    let (mut apex, mut air_time) = (0.0_f32, f32::NAN);
    let mut airborne = false;
    for frame in 0..frames(3.0) {
        let above = jumper.feet();
        let moved = moveplayer(&mut jumper, false, PHYSFRAMETIME);
        apex = apex.max(jumper.feet());
        if jumper.physstate == PHYS_FALL {
            airborne = true;
        } else if airborne {
            // The frame that reached the floor, up to where it crossed it.
            let fraction = (above / -moved.z).clamp(0.0, 1.0);
            air_time = (frame as f32 + fraction) * FRAME;
            break;
        }
    }

    let mut runner = Ent::standing();
    runner.mv = 1;
    for _ in 0..frames(3.0) {
        moveplayer(&mut runner, false, PHYSFRAMETIME);
    }
    let (release, top) = (runner.o, speed(&runner));
    runner.mv = 0;
    let mut stop_distance = f32::NAN;
    for _ in 0..frames(10.0) {
        moveplayer(&mut runner, false, PHYSFRAMETIME);
        if speed(&runner) < 0.01 * top {
            let travelled = runner.o - release;
            stop_distance = horizontal(travelled.x, travelled.y) / UNITS_PER_METRE;
            break;
        }
    }

    Metrics {
        top_forward,
        top_strafe,
        top_airborne,
        time_to_90,
        jump_apex: apex / UNITS_PER_METRE,
        air_time,
        stop_distance,
    }
}

fn flat_world() -> World {
    let mut world = World::empty();
    for x in -64..64 {
        for z in -64..64 {
            world.set([x, 0, z], Some(Cell::solid(0)));
        }
    }
    world
}

fn sky_world() -> World {
    let bounds = WorldBounds {
        min: [-512, -1024, -512],
        max: [512, 1024, 512],
    };
    World::from_compact_parts(bounds, &[], &[], &[]).unwrap()
}

/// The same measurements, taken from a profile in the real simulation at the
/// fixed 60 Hz tick: a flat world for the ground quantities, and an empty tall
/// one in which the player stays airborne long enough to reach top speed.
pub fn measure_profile(profile: &MovementProfile) -> Metrics {
    let flat = flat_world();
    let sky = sky_world();
    let step = |world: &World, player: &mut Player, input: InputFrame| {
        simulate_player_with(world, player, &input, FIXED_DT, profile);
    };
    let settled = |x: f32, z: f32| {
        let mut player = Player::new(1, "measure".into());
        player.position = Vec3::new(x, 1.01, z);
        for _ in 0..30 {
            step(&flat, &mut player, InputFrame::default());
        }
        assert!(player.grounded);
        player
    };
    let forward = InputFrame {
        forward: 1.0,
        ..Default::default()
    };
    let strafe = InputFrame {
        strafe: 1.0,
        ..Default::default()
    };
    let speed = |player: &Player| horizontal(player.velocity.x, player.velocity.z);

    let mut runner = settled(0.5, 60.0);
    let mut speeds = Vec::new();
    for _ in 0..150 {
        step(&flat, &mut runner, forward);
        speeds.push(speed(&runner));
    }
    let top_forward = *speeds.last().unwrap();
    let time_to_90 = rise_time(&speeds, FIXED_DT, 0.9 * top_forward);

    let mut strafer = settled(-60.0, 0.5);
    for _ in 0..150 {
        step(&flat, &mut strafer, strafe);
    }
    let top_strafe = speed(&strafer);

    let mut flyer = Player::new(1, "measure".into());
    flyer.position = Vec3::new(0.0, 900.0, 0.0);
    for _ in 0..360 {
        step(&sky, &mut flyer, forward);
    }
    assert!(!flyer.grounded);
    let top_airborne = speed(&flyer);

    let release = runner.position;
    let mut stop_distance = f32::NAN;
    for _ in 0..600 {
        step(&flat, &mut runner, InputFrame::default());
        if speed(&runner) < 0.01 * top_forward {
            let travelled = runner.position - release;
            stop_distance = horizontal(travelled.x, travelled.z);
            break;
        }
    }

    let mut jumper = settled(0.5, 0.5);
    let floor = jumper.position.y;
    let (mut apex, mut air_time) = (0.0_f32, f32::NAN);
    let mut airborne = false;
    for tick in 0..180 {
        let above = jumper.position.y - floor;
        let climb = jumper.velocity.y + jumper.movement.falling;
        step(
            &flat,
            &mut jumper,
            InputFrame {
                jump: tick == 0,
                ..Default::default()
            },
        );
        apex = apex.max(jumper.position.y - floor);
        if !jumper.grounded {
            airborne = true;
        } else if airborne {
            // Landed during this tick; the tick's own start says when.
            let wait = time_to_floor(above, climb, profile.gravity).min(FIXED_DT);
            air_time = tick as f32 * FIXED_DT + wait;
            break;
        }
    }

    Metrics {
        top_forward,
        top_strafe,
        top_airborne,
        time_to_90,
        jump_apex: apex,
        air_time,
        stop_distance,
    }
}
