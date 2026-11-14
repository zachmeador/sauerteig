//! The built-in movement profiles in the real simulation: Sauerbraten against a
//! literal port of its velocity math, and the state and size rules each style
//! brings with it.

mod oracle;

use oracle::{measure_oracle, measure_profile, moveplayer, Ent, Metrics, FRAME};
use sauerteig_core::*;

const TOLERANCE: f32 = 0.03;
const UNITS_PER_METRE: f32 = 8.0;

fn floor() -> World {
    let mut world = World::empty();
    for x in -64..64 {
        for z in -64..64 {
            world.set([x, 0, z], Some(Cell::solid(0)));
        }
    }
    world
}

fn ocean() -> World {
    let bounds = WorldBounds {
        min: [-64, -200, -64],
        max: [64, 200, 64],
    };
    let mut world = World::from_compact_parts(bounds, &[], &[], &[]).unwrap();
    world
        .replace_volumes(&[MaterialVolume {
            bounds: FineBox {
                min: [-512, -1600, -512],
                max: [512, 1600, 512],
            },
            material: VolumeMaterial::Water,
        }])
        .unwrap();
    world
}

fn player_at(position: Vec3) -> Player {
    let mut player = Player::new(1, "profile".into());
    player.position = position;
    player
}

fn stand() -> Player {
    player_at(Vec3::new(0.5, 1.01, 0.5))
}

/// A player already swimming, 30 m up in the middle of a very deep ocean.
fn swimmer() -> Player {
    let mut player = player_at(Vec3::new(0.0, 30.0, 0.0));
    player.movement.in_water = true;
    player
}

fn run(
    world: &World,
    player: &mut Player,
    profile: &MovementProfile,
    input: InputFrame,
    ticks: usize,
) {
    for _ in 0..ticks {
        simulate_player_with(world, player, &input, FIXED_DT, profile);
    }
}

fn jump() -> InputFrame {
    InputFrame {
        jump: true,
        ..Default::default()
    }
}

fn forward() -> InputFrame {
    InputFrame {
        forward: 1.0,
        ..Default::default()
    }
}

fn strafe() -> InputFrame {
    InputFrame {
        strafe: 1.0,
        ..Default::default()
    }
}

fn within(actual: f32, expected: f32, tolerance: f32) -> bool {
    (actual - expected).abs() <= expected.abs() * tolerance
}

fn horizontal_speed(player: &Player) -> f32 {
    libm::hypotf(player.velocity.x, player.velocity.z)
}

#[test]
fn sauerbraten_profile_matches_the_reference_oracle_within_three_percent() {
    let reference = measure_oracle().values();
    let measured = measure_profile(&MovementProfile::sauerbraten()).values();
    for (index, name) in Metrics::NAMES.iter().enumerate() {
        assert!(
            within(measured[index], reference[index], TOLERANCE),
            "{name}: profile {} against oracle {}",
            measured[index],
            reference[index]
        );
    }
}

#[test]
fn the_oracle_agrees_with_closed_form_sauerbraten_numbers() {
    let metrics = measure_oracle();
    assert!(within(metrics.top_forward, 100.0 * 1.3 / 8.0, 1.0e-4));
    assert!(within(metrics.top_strafe, 100.0 / 8.0, 1.0e-4));
    assert!(within(
        metrics.top_airborne,
        100.0 * 1.3 * 1.3 / 8.0,
        1.0e-3
    ));
    // The eased gap closes by 5/6 every 20 ms: 90% is reached after
    // ln(0.1) / ln(5/6) intervals.
    let ground = 5.0_f64 / 6.0;
    assert!(within(
        metrics.time_to_90,
        (0.02 * libm::log(0.1) / libm::log(ground)) as f32,
        5.0e-3
    ));
    // Without input the speed keeps 5/6 of itself per interval. Frame by 5 ms
    // frame the distance is a geometric series that stops at 1% of the speed.
    let per_frame = libm::pow(ground, 0.25);
    let frames = (libm::log(0.01) / libm::log(per_frame)).ceil() as i32;
    let series = per_frame * (1.0 - libm::pow(per_frame, f64::from(frames))) / (1.0 - per_frame);
    let expected = metrics.top_forward as f64 * 0.005 * series;
    assert!(
        within(metrics.stop_distance, expected as f32, 1.0e-3),
        "{} against {expected}",
        metrics.stop_distance
    );
}

#[test]
fn arena_numbers_are_the_documented_ones() {
    let arena = measure_profile(&MovementProfile::arena());
    assert!(within(arena.top_forward, 9.0, 1.0e-4));
    assert!(within(arena.top_strafe, 9.0, 1.0e-4));
    assert!(within(arena.top_airborne, 9.0, 1.0e-3));
    // 8.2 m/s against 25 m/s^2 peaks at 1.345 m; the fixed step lands a little under.
    assert!(
        (1.25..1.35).contains(&arena.jump_apex),
        "{}",
        arena.jump_apex
    );
}

#[test]
fn sauerbraten_profile_uses_its_own_player_size() {
    let mut wall = floor();
    for y in 1..5 {
        for z in -20..20 {
            wall.set([3, y, z], Some(Cell::solid(1)));
        }
    }
    for (profile, radius) in [
        (MovementProfile::arena(), PLAYER_RADIUS),
        (MovementProfile::sauerbraten(), 4.1 / 8.0),
    ] {
        let mut player = stand();
        run(&wall, &mut player, &profile, strafe(), 120);
        assert!(
            (player.position.x - (3.0 - radius)).abs() < 0.003,
            "stopped at {}",
            player.position.x
        );
    }

    let mut ceiling = floor();
    for x in -2..3 {
        for z in -2..3 {
            ceiling.set([x, 4, z], Some(Cell::solid(0)));
        }
    }
    let profile = MovementProfile::sauerbraten();
    let mut player = stand();
    run(&ceiling, &mut player, &profile, InputFrame::default(), 30);
    let mut peak: f32 = 0.0;
    for tick in 0..80 {
        let input = InputFrame {
            jump: tick == 0,
            ..Default::default()
        };
        simulate_player_with(&ceiling, &mut player, &input, FIXED_DT, &profile);
        peak = peak.max(player.position.y);
    }
    assert!(
        (peak - (4.0 - 1.875)).abs() < 0.003,
        "the head stopped the feet at {peak}"
    );
    assert!(player.grounded);
}

#[test]
fn both_profiles_walk_up_half_metre_steps_but_not_taller_ones() {
    let ledge = |rise: u8| {
        let mut world = floor();
        for z in -3..4 {
            world.set(
                [3, 1, z],
                Some(Cell {
                    heights: [rise; 4],
                    ..Cell::solid(0)
                }),
            );
        }
        world
    };
    for profile in [MovementProfile::arena(), MovementProfile::sauerbraten()] {
        let climb = |rise: u8| {
            let world = ledge(rise);
            let mut player = stand();
            let (mut peak, mut reach) = (0.0_f32, 0.0_f32);
            for _ in 0..80 {
                simulate_player_with(&world, &mut player, &strafe(), FIXED_DT, &profile);
                peak = peak.max(player.position.y);
                reach = reach.max(player.position.x);
            }
            (peak, reach)
        };
        let (peak, reach) = climb(4);
        assert!(reach > 4.0 && (peak - 1.5).abs() < 0.01, "{peak} {reach}");
        let (peak, reach) = climb(5);
        assert!(reach < 3.0 && (peak - 1.0).abs() < 0.01, "{peak} {reach}");
    }
}

/// How many times the player launches into a jump while `presses` says the key
/// is down, counting ticks that start a fast climb.
fn launches(profile: &MovementProfile, presses: impl Fn(usize) -> bool, ticks: usize) -> usize {
    let world = floor();
    let mut player = stand();
    run(&world, &mut player, profile, InputFrame::default(), 20);
    let climb = |player: &Player| player.velocity.y + player.movement.falling;
    let mut count = 0;
    for tick in 0..ticks {
        let before = climb(&player);
        let input = InputFrame {
            jump: presses(tick),
            ..Default::default()
        };
        simulate_player_with(&world, &mut player, &input, FIXED_DT, profile);
        if before <= 5.0 && climb(&player) > 5.0 {
            count += 1;
        }
    }
    count
}

#[test]
fn a_jump_fires_once_per_press() {
    let sauerbraten = MovementProfile::sauerbraten();
    // A key held through the whole flight and landing does not repeat.
    assert_eq!(launches(&sauerbraten, |_| true, 200), 1);
    // Releasing it and pressing again does.
    assert_eq!(
        launches(&sauerbraten, |tick| !(3..70).contains(&tick), 200),
        2
    );
    // Arena keeps hopping while the key is held.
    assert!(launches(&MovementProfile::arena(), |_| true, 200) >= 4);
}

#[test]
fn a_press_made_in_the_air_waits_for_the_floor_while_held() {
    let sauerbraten = MovementProfile::sauerbraten();
    // The first jump lands near tick 42; a key pressed at tick 20 and still
    // down then jumps again at once.
    let held = |tick: usize| !(3..20).contains(&tick);
    assert_eq!(launches(&sauerbraten, held, 200), 2);
    // Released before the landing, the press is forgotten.
    let released = |tick: usize| tick < 3 || (20..30).contains(&tick);
    assert_eq!(launches(&sauerbraten, released, 200), 1);
}

#[test]
fn the_first_press_on_the_floor_jumps_in_the_same_tick() {
    let world = floor();
    let profile = MovementProfile::sauerbraten();
    let mut player = stand();
    run(&world, &mut player, &profile, InputFrame::default(), 20);
    assert!(player.grounded);
    run(&world, &mut player, &profile, jump(), 1);
    assert!(!player.grounded);
    assert!(player.position.y > 1.05);
    assert_eq!(player.movement.jump, JumpLatch::Spent);
    run(&world, &mut player, &profile, InputFrame::default(), 1);
    assert_eq!(player.movement.jump, JumpLatch::Released);
}

#[test]
fn gravity_accumulates_apart_from_the_eased_velocity_and_landing_clears_it() {
    let world = floor();
    let profile = MovementProfile::sauerbraten();
    let mut player = player_at(Vec3::new(0.5, 6.0, 0.5));
    run(&world, &mut player, &profile, InputFrame::default(), 20);
    assert!(
        player.movement.falling < -7.0,
        "{}",
        player.movement.falling
    );
    assert_eq!(player.velocity, Vec3::ZERO);
    run(&world, &mut player, &profile, InputFrame::default(), 60);
    assert!(player.grounded);
    assert_eq!(player.movement.falling, 0.0);
    assert_eq!(player.velocity.y, 0.0);
}

#[test]
fn the_forward_boost_applies_to_straight_movement_only() {
    let world = floor();
    let profile = MovementProfile::sauerbraten();
    let top = |start: Vec3, input: InputFrame| {
        let mut player = player_at(start);
        run(&world, &mut player, &profile, input, 90);
        horizontal_speed(&player)
    };
    let from = Vec3::new(0.0, 1.01, 0.0);
    let backward = InputFrame {
        forward: -1.0,
        ..Default::default()
    };
    let diagonal = InputFrame {
        forward: 1.0,
        strafe: 1.0,
        ..Default::default()
    };
    // The target is 12.5 m/s, times 1.3 while steering straight forward or back.
    assert!(within(top(from, forward()), 16.25, 1.0e-3));
    assert!(within(top(from, backward), 16.25, 1.0e-3));
    assert!(within(top(from, strafe()), 12.5, 1.0e-3));
    assert!(within(top(from, diagonal), 12.5, 1.0e-3));
}

#[test]
fn partial_stick_input_scales_the_target_speed() {
    let world = floor();
    let profile = MovementProfile::sauerbraten();
    let mut player = stand();
    let half = InputFrame {
        strafe: 0.5,
        ..Default::default()
    };
    run(&world, &mut player, &profile, half, 120);
    assert!(
        within(player.velocity.x, 6.25, 1.0e-3),
        "{}",
        player.velocity.x
    );
}

#[test]
fn flight_in_edit_mode_uses_the_profile_and_clears_movement_state() {
    let world = floor();
    let profile = MovementProfile::sauerbraten();
    let mut player = player_at(Vec3::new(0.0, 6.0, 0.0));
    run(&world, &mut player, &profile, jump(), 10);
    assert!(!player.movement.is_idle());
    player.editing = true;
    run(&world, &mut player, &profile, forward(), 1);
    assert!(player.movement.is_idle());
    assert!(within(player.velocity.length(), 12.5, 1.0e-4));
    player.position = Vec3::new(0.0, 44.0, 0.0);
    let rise = InputFrame {
        vertical: 1.0,
        ..Default::default()
    };
    run(&world, &mut player, &profile, rise, 40);
    assert!((player.position.y - (48.0 - 1.875)).abs() < 1.0e-4);
}

fn swim(
    profile: &MovementProfile,
    input: impl Fn(usize) -> InputFrame,
    ticks: usize,
) -> Vec<Player> {
    let world = ocean();
    let mut player = swimmer();
    (0..ticks)
        .map(|tick| {
            simulate_player_with(&world, &mut player, &input(tick), FIXED_DT, profile);
            player.clone()
        })
        .collect()
}

/// The oracle swimming in the same ocean, already in the water, high above
/// any floor.
fn swim_oracle(frames: usize, press: impl Fn(&mut Ent, usize)) -> Vec<Ent> {
    let mut pl = Ent::airborne();
    pl.inwater = true;
    (0..frames)
        .map(|frame| {
            press(&mut pl, frame);
            moveplayer(&mut pl, true, 5);
            pl
        })
        .collect()
}

fn after(samples: &[Player], seconds: f32) -> &Player {
    &samples[(seconds / FIXED_DT).round() as usize - 1]
}

fn oracle_after(samples: &[Ent], seconds: f32) -> &Ent {
    &samples[(seconds / FRAME).round() as usize - 1]
}

fn oracle_rise(pl: &Ent) -> f32 {
    (pl.o.z - Ent::airborne().o.z) / UNITS_PER_METRE
}

#[test]
fn an_idle_swimmer_sinks_slowly_like_the_reference() {
    let profile = MovementProfile::sauerbraten();
    let samples = swim(&profile, |_| InputFrame::default(), 240);
    let reference = swim_oracle(800, |_, _| {});
    for seconds in [1.0, 2.0, 3.0] {
        let sunk = 30.0 - after(&samples, seconds).position.y;
        let expected = -oracle_rise(oracle_after(&reference, seconds));
        assert!(
            within(sunk, expected, TOLERANCE),
            "{seconds}s: {sunk} against {expected}"
        );
    }
    let rate = after(&samples, 3.0).movement.falling;
    assert!((-0.7..-0.6).contains(&rate), "sink rate {rate}");
}

#[test]
fn steering_in_water_stops_gravity_and_follows_the_view_pitch() {
    let profile = MovementProfile::sauerbraten();
    let level = swim(&profile, |_| forward(), 300);
    let last = level.last().unwrap();
    assert!(last.movement.falling.abs() < 1.0e-6);
    assert!(last.velocity.y.abs() < 1.0e-4);
    assert!(within(last.velocity.length(), 12.5, 1.0e-3));

    let pitch = 0.5_f32;
    let look_up = |_| InputFrame {
        forward: 1.0,
        pitch,
        ..Default::default()
    };
    let climbing = swim(&profile, look_up, 300);
    let last = climbing.last().unwrap();
    let slope = last.velocity.y / libm::hypotf(last.velocity.x, last.velocity.z);
    assert!(within(slope, libm::tanf(pitch), 1.0e-3), "{slope}");
    assert!(last.position.y > 30.0 + 10.0);
    // A swimmer moves by half the eased velocity.
    let speed = (last.position - after(&climbing, 4.0).position).length() / 1.0;
    assert!(within(speed, 6.25, 0.02), "{speed}");
}

#[test]
fn the_vertical_keys_swim_up_and_down() {
    let profile = MovementProfile::sauerbraten();
    for sign in [1.0, -1.0] {
        let samples = swim(
            &profile,
            |_| InputFrame {
                vertical: sign,
                ..Default::default()
            },
            300,
        );
        let last = samples.last().unwrap();
        assert!(
            within(last.velocity.y, sign * 12.5, 1.0e-3),
            "{}",
            last.velocity.y
        );
        assert!(last.movement.falling.abs() < 1.0e-6);
        assert!((last.position.y - 30.0) * sign > 12.0);
    }
}

#[test]
fn a_jump_press_in_water_kicks_upward_like_the_reference() {
    let profile = MovementProfile::sauerbraten();
    let tap = |tick: usize| InputFrame {
        jump: tick == 0,
        ..Default::default()
    };
    let samples = swim(&profile, tap, 240);
    let reference = swim_oracle(800, |pl, frame| pl.jumping = frame == 0);
    let apex = samples
        .iter()
        .map(|player| player.position.y - 30.0)
        .fold(f32::MIN, f32::max);
    let expected = reference.iter().map(oracle_rise).fold(f32::MIN, f32::max);
    assert!(apex > 2.0, "{apex}");
    assert!(
        within(apex, expected, TOLERANCE),
        "apex {apex} against {expected}"
    );
    for seconds in [1.0, 2.0, 3.0] {
        let height = after(&samples, seconds).position.y - 30.0;
        let wanted = oracle_rise(oracle_after(&reference, seconds));
        assert!(
            within(height, wanted, 0.05),
            "{seconds}s: {height} against {wanted}"
        );
    }
}

#[test]
fn a_held_key_in_water_kicks_once_not_every_tick() {
    let profile = MovementProfile::sauerbraten();
    let held = swim(&profile, |_| jump(), 240);
    let tap = |tick: usize| InputFrame {
        jump: tick == 0,
        ..Default::default()
    };
    let tapped = swim(&profile, tap, 240);
    // The held key is the same press, so the two swimmers move identically.
    assert_eq!(
        held.last().unwrap().position,
        tapped.last().unwrap().position
    );
}

#[test]
fn entering_water_slows_the_eased_velocity_once() {
    let profile = MovementProfile::sauerbraten();
    let world = ocean();
    let mut player = player_at(Vec3::new(0.0, 30.0, 0.0));
    player.velocity = Vec3::new(8.0, 0.0, 0.0);
    // Per tick the gap to a standstill closes by (19/20) over a 20 ms interval.
    let ease = libm::pow(19.0_f64 / 20.0, f64::from(FIXED_DT) / 0.02) as f32;
    run(&world, &mut player, &profile, InputFrame::default(), 1);
    assert!(
        within(player.velocity.x, ease, 1.0e-3),
        "{}",
        player.velocity.x
    );
    assert!(player.movement.in_water);
    run(&world, &mut player, &profile, InputFrame::default(), 1);
    assert!(
        within(player.velocity.x, ease * ease, 1.0e-3),
        "{}",
        player.velocity.x
    );
}

#[test]
fn a_swimmer_stays_one_until_the_middle_of_the_body_is_dry() {
    let profile = MovementProfile::sauerbraten();
    let mut world = World::empty();
    // Water up to 1.5 m. The 1.875 m body counts as swimming while the water
    // reaches three quarters of its height (1.41 m) and, once swimming, until
    // it drops below half (0.94 m).
    world
        .replace_volumes(&[MaterialVolume {
            bounds: FineBox {
                min: [-512, 0, -512],
                max: [512, 12, 512],
            },
            material: VolumeMaterial::Water,
        }])
        .unwrap();
    let after_one_tick = |y: f32, swimming: bool| {
        let mut player = player_at(Vec3::new(0.0, y, 0.0));
        player.movement.in_water = swimming;
        run(&world, &mut player, &profile, InputFrame::default(), 1);
        player.movement.in_water
    };
    // Feet at 0.05 m: both probes are wet.
    assert!(after_one_tick(0.05, false));
    assert!(after_one_tick(0.05, true));
    // Feet at 0.5 m: the upper probe is dry, the middle one still wet.
    assert!(!after_one_tick(0.5, false));
    assert!(after_one_tick(0.5, true));
    // Feet at 1.2 m: both are dry.
    assert!(!after_one_tick(1.2, false));
    assert!(!after_one_tick(1.2, true));
}

#[test]
fn arena_never_touches_the_movement_state_and_serializes_as_before() {
    let world = floor();
    let mut player = stand();
    player.movement.falling = -9.0;
    player.movement.jump = JumpLatch::Pending;
    player.movement.in_water = true;
    run(&world, &mut player, &MovementProfile::arena(), jump(), 1);
    assert!(player.movement.is_idle());

    let json = serde_json::to_value(&player).unwrap();
    assert!(json.get("movement").is_none());
    // Older saves and servers know nothing of it.
    let restored: Player = serde_json::from_value(json).unwrap();
    assert!(restored.movement.is_idle());

    player.movement.falling = -2.5;
    let json = serde_json::to_string(&player).unwrap();
    assert!(json.contains(r#""movement":{"falling":-2.5}"#), "{json}");
}

#[test]
fn switching_styles_mid_flight_stays_finite_and_arena_clears_the_state() {
    let world = floor();
    let mut player = stand();
    let (arena, sauerbraten) = (MovementProfile::arena(), MovementProfile::sauerbraten());
    run(&world, &mut player, &sauerbraten, jump(), 20);
    assert!(!player.movement.is_idle());
    run(&world, &mut player, &arena, InputFrame::default(), 1);
    assert!(player.movement.is_idle());
    run(&world, &mut player, &sauerbraten, forward(), 200);
    assert!(player.position.is_finite() && player.velocity.is_finite());
    assert!(player.grounded);
}

#[test]
fn identical_inputs_replay_to_identical_states_and_a_snapshot_resumes_the_same() {
    let world = floor();
    let profile = MovementProfile::sauerbraten();
    let script = |tick: usize| InputFrame {
        seq: tick as u32 + 1,
        forward: if tick % 90 < 60 { 1.0 } else { 0.0 },
        strafe: if tick % 50 < 20 { 1.0 } else { -1.0 },
        yaw: tick as f32 * 0.03,
        jump: tick % 70 < 3,
        ..Default::default()
    };
    let (mut a, mut b) = (stand(), stand());
    let mut snapshot = None;
    for tick in 0..300 {
        simulate_player_with(&world, &mut a, &script(tick), FIXED_DT, &profile);
        simulate_player_with(&world, &mut b, &script(tick), FIXED_DT, &profile);
        if tick == 100 {
            // What a client does with a server snapshot: take the state, replay on.
            let json = serde_json::to_string(&b).unwrap();
            snapshot = Some(serde_json::from_str::<Player>(&json).unwrap());
        }
    }
    assert_eq!(a.position, b.position);
    assert_eq!(a.velocity, b.velocity);
    assert_eq!(a.movement, b.movement);
    let mut resumed = snapshot.unwrap();
    for tick in 101..300 {
        simulate_player_with(&world, &mut resumed, &script(tick), FIXED_DT, &profile);
    }
    assert!(resumed.position.distance(a.position) < 1.0e-3);
    assert_eq!(resumed.movement.jump, a.movement.jump);
}

#[test]
fn derived_factors_are_an_optimization_not_a_different_model() {
    let world = floor();
    let prepared = MovementProfile::sauerbraten();
    let mut unprepared = prepared;
    unprepared.style = MovementStyle::Sauerbraten(SauerbratenTuning::DEFAULT);
    assert_ne!(prepared, unprepared);
    let (mut a, mut b) = (stand(), stand());
    for tick in 0..200 {
        let input = InputFrame {
            forward: 1.0,
            jump: tick % 40 < 2,
            ..Default::default()
        };
        simulate_player_with(&world, &mut a, &input, FIXED_DT, &prepared);
        simulate_player_with(&world, &mut b, &input, FIXED_DT, &unprepared);
    }
    assert_eq!(a.position, b.position);
    assert_eq!(a.velocity, b.velocity);
}

#[test]
fn other_tick_lengths_reach_the_same_top_speeds() {
    let world = floor();
    let profile = MovementProfile::sauerbraten();
    for dt in [1.0 / 30.0, 1.0 / 120.0, 0.05] {
        let mut player = player_at(Vec3::new(0.5, 1.01, 60.0));
        for _ in 0..(2.0 / dt) as usize {
            simulate_player_with(&world, &mut player, &forward(), dt, &profile);
        }
        assert!(
            within(horizontal_speed(&player), 16.25, 1.0e-3),
            "dt {dt}: {}",
            horizontal_speed(&player)
        );
    }
}

#[test]
fn the_foundry_routes_are_walkable_with_either_profile() {
    let world = World::arena();
    for profile in [MovementProfile::arena(), MovementProfile::sauerbraten()] {
        // The stairs and the ramp lead from the yard to the 5 m deck.
        for (name, start, forward) in [
            ("stairs", Vec3::new(-14.5, 1.01, -17.5), -1.0),
            ("ramp", Vec3::new(15.0, 1.01, 16.5), 1.0),
        ] {
            let mut player = player_at(start);
            let input = InputFrame {
                forward,
                ..Default::default()
            };
            let mut peak: f32 = 0.0;
            for _ in 0..120 {
                simulate_player_with(&world, &mut player, &input, FIXED_DT, &profile);
                peak = peak.max(player.position.y);
            }
            assert!(peak > 4.99, "{name} stops at {peak}");
        }
        // The catwalk crosses the yard at deck height.
        let mut player = player_at(Vec3::new(-14.0, 5.01, 0.0));
        let mut lowest: f32 = 5.01;
        for _ in 0..240 {
            simulate_player_with(&world, &mut player, &strafe(), FIXED_DT, &profile);
            if player.position.x < 12.0 {
                lowest = lowest.min(player.position.y);
            }
        }
        assert!(player.position.x > 12.0 && lowest > 4.99, "{lowest}");
    }
}

/// The keys held `seconds` into the composite run, which changes them only on
/// multiples of 0.05 s so that the 60 Hz tick and the 5 ms frame both line up
/// with every change: run forward, jump while running, strafe in the air, jump
/// again, then let go.
fn composite_keys(seconds: f32) -> (f32, f32, bool) {
    let step = (seconds / 0.05 + 1.0e-3).floor() as i32;
    let forward = if (0..40).contains(&step) { 1.0 } else { 0.0 };
    let strafe = if (12..36).contains(&step) { 1.0 } else { 0.0 };
    let jump = (10..13).contains(&step) || (30..32).contains(&step);
    (forward, strafe, jump)
}

#[test]
fn a_scripted_run_jump_and_stop_follows_the_reference_trajectory() {
    let world = floor();
    let profile = MovementProfile::sauerbraten();
    let start = Vec3::new(0.0, 1.0, 60.0);
    let mut player = player_at(start);
    let mut ours = Vec::new();
    for tick in 0..240 {
        let (forward, strafe, jump) = composite_keys(tick as f32 * FIXED_DT);
        let input = InputFrame {
            forward,
            strafe,
            jump,
            ..Default::default()
        };
        simulate_player_with(&world, &mut player, &input, FIXED_DT, &profile);
        ours.push(player.position - start);
    }

    let mut pl = Ent::standing();
    let mut key_down = false;
    let mut theirs = Vec::new();
    for frame in 0..800 {
        let (forward, strafe, jump) = composite_keys(frame as f32 * FRAME);
        pl.mv = forward as i32;
        pl.strafe = strafe as i32;
        // The engine latches a press when the key goes down and drops it when
        // the key goes up.
        pl.jumping = if jump { pl.jumping || !key_down } else { false };
        key_down = jump;
        moveplayer(&mut pl, false, 5);
        // The reference has z up and y forward; the simulation has y up and -z forward.
        theirs.push(Vec3::new(pl.o.x, pl.feet(), -pl.o.y) / UNITS_PER_METRE);
    }

    for seconds in [0.5, 1.0, 1.5, 2.0, 2.5, 3.0, 3.5, 4.0] {
        let here = ours[(seconds / FIXED_DT).round() as usize - 1];
        let there = theirs[(seconds / FRAME).round() as usize - 1];
        assert!(
            here.distance(there) <= TOLERANCE * there.length().max(1.0),
            "{seconds}s: {here:?} against {there:?}"
        );
    }
    assert!(player.grounded && player.velocity.length() < 1.0e-3);
}
