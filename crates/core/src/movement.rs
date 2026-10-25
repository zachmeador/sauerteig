//! Movement profiles: the style and numbers that decide how a player moves.
//!
//! The Sauerbraten style is a Rust port of the velocity model in `modifyvelocity`,
//! `modifygravity` and `moveplayer` from `src/engine/physics.cpp` of the
//! Sauerbraten source (<https://svn.code.sf.net/p/sauerbraten/code/src/engine/physics.cpp>),
//! which is zlib licensed, Copyright (C) 2001-2020 Wouter van Oortmerssen, Lee
//! Salzman, Mike Dysart, Robert Pointon, and Quinton Reeves. See
//! `THIRD_PARTY_NOTICES.md`. Its numbers appear here in Sauerbraten's own units
//! and convert at the importer's 8 units per metre.

use crate::{EYE_HEIGHT, FIXED_DT, PLAYER_HEIGHT, PLAYER_RADIUS};
use serde::{Deserialize, Serialize};

/// Everything that decides how a player moves except the world they move in.
/// Distances are metres and speeds metres per second.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MovementProfile {
    pub style: MovementStyle,
    /// Top walking speed along the steering direction.
    pub move_speed: f32,
    /// Edit-mode flight speed.
    pub fly_speed: f32,
    pub jump_speed: f32,
    pub gravity: f32,
    pub radius: f32,
    pub height: f32,
    pub eye_height: f32,
    /// Tallest ledge a grounded player walks onto without jumping.
    pub step_height: f32,
    /// Height above the feet, as a fraction of `height`, where the world is
    /// sampled to decide whether the player is swimming.
    pub water_probe: f32,
}

/// The equations that steer a player, with the numbers only that style uses.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum MovementStyle {
    /// Quake-style: friction on the ground, then acceleration along the
    /// requested direction up to the walking speed.
    Arena(ArenaTuning),
    /// Sauerbraten: velocity eases toward the steering direction while gravity
    /// accumulates in a separate falling component.
    Sauerbraten(SauerbratenTuning),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ArenaTuning {
    /// Acceleration toward the wish speed, per second, as a multiple of that speed.
    pub ground_acceleration: f32,
    pub air_acceleration: f32,
    pub water_acceleration: f32,
    /// Ground friction removes this multiple of `max(speed, stop_speed)` per second.
    pub friction: f32,
    pub stop_speed: f32,
    pub swim_speed: f32,
    /// Fraction of velocity lost per second in water.
    pub water_drag: f32,
    /// Gravity multiplier in water.
    pub water_gravity: f32,
}

/// Sauerbraten's easing numbers are "per 20 ms": each interval, velocity closes
/// one `ease`th of its remaining gap to the target.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SauerbratenTuning {
    /// Speed multiplier while steering straight forward or back, without strafing.
    pub forward_boost: f32,
    /// Speed multiplier while airborne.
    pub air_boost: f32,
    pub ground_ease: f32,
    pub air_ease: f32,
    pub water_ease: f32,
    /// The same for the falling component, which only eases on the ground and in water.
    pub fall_ease: f32,
    pub water_fall_ease: f32,
    /// Share of the eased velocity that moves a swimmer.
    pub water_scale: f32,
    /// Entering water, and jumping in it, divide the eased velocity by this.
    pub water_divisor: f32,
    /// A swimmer stays a swimmer until the water drops below this fraction of
    /// the body height, so the water surface does not flicker the state.
    pub water_exit_probe: f32,
    /// Per-step easing factors for the fixed tick, built by `prepared`. Any
    /// profile whose numbers or tick differ from them still moves correctly,
    /// by deriving the factors on the spot.
    cache: Option<EaseTable>,
}

const UNITS_PER_METRE: f32 = 8.0;
// physent defaults and constants from ents.h and physics.cpp, in Sauerbraten units.
const SAUER_MAX_SPEED: f32 = 100.0;
const SAUER_JUMP_VELOCITY: f32 = 125.0;
const SAUER_GRAVITY: f32 = 200.0;
const SAUER_RADIUS: f32 = 4.1;
const SAUER_EYE_HEIGHT: f32 = 14.0;
const SAUER_ABOVE_EYE: f32 = 1.0;
const SAUER_STAIR_HEIGHT: f32 = 4.1;
// The floatspeed variable: edit-mode flight speed, in percent of maxspeed.
const SAUER_FLOAT_PERCENT: f32 = 100.0;
// moveplayer samples the material at the eye plus (3 * aboveeye - eyeheight) / 4,
// 11.25 units above the feet. A player already in water stays there while the
// eye plus (aboveeye - eyeheight) / 2, the middle of the body, is wet.
const SAUER_WATER_PROBE: f32 = (SAUER_EYE_HEIGHT
    + (3.0 * SAUER_ABOVE_EYE - SAUER_EYE_HEIGHT) / 4.0)
    / (SAUER_EYE_HEIGHT + SAUER_ABOVE_EYE);
const SAUER_WATER_EXIT_PROBE: f32 = (SAUER_EYE_HEIGHT + (SAUER_ABOVE_EYE - SAUER_EYE_HEIGHT) / 2.0)
    / (SAUER_EYE_HEIGHT + SAUER_ABOVE_EYE);
/// Sauerbraten's physics frame (`PHYSFRAMETIME`). One fixed tick runs the
/// velocity model in as many equal steps of about this length as fit.
const PHYSICS_FRAME: f32 = 0.005;
const EASE_INTERVAL: f64 = 0.02;

impl ArenaTuning {
    pub const DEFAULT: Self = Self {
        ground_acceleration: 14.0,
        air_acceleration: 3.0,
        water_acceleration: 6.0,
        friction: 7.0,
        stop_speed: 2.5,
        swim_speed: 4.5,
        water_drag: 3.0,
        water_gravity: 0.12,
    };
}

impl SauerbratenTuning {
    pub const DEFAULT: Self = Self {
        forward_boost: 1.3,
        air_boost: 1.3,
        ground_ease: 6.0,
        air_ease: 30.0,
        water_ease: 20.0,
        fall_ease: 6.0,
        water_fall_ease: 2.0,
        water_scale: 0.5,
        water_divisor: 8.0,
        water_exit_probe: SAUER_WATER_EXIT_PROBE,
        cache: None,
    };

    /// Derives the easing factors for the fixed tick once, so the simulation
    /// does not call `powf` on every tick.
    pub fn prepared(mut self) -> Self {
        let step = FIXED_DT / velocity_steps(FIXED_DT) as f32;
        self.cache = Some(EaseTable::new(&self, step));
        self
    }

    pub(crate) fn ease_table(&self, step: f32) -> EaseTable {
        match self.cache {
            Some(table) if table.matches(self, step) => table,
            _ => EaseTable::new(self, step),
        }
    }

    fn easing(&self) -> [f32; 5] {
        [
            self.ground_ease,
            self.air_ease,
            self.water_ease,
            self.fall_ease,
            self.water_fall_ease,
        ]
    }
}

/// The factors each velocity step multiplies the remaining gap by:
/// `(1 - 1/ease)` per 20 ms, raised to the step's share of that interval.
/// They are derived in f64 and rounded once, so native and wasm builds agree
/// on every bit unless the libm results straddle a rounding boundary.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct EaseTable {
    step: f32,
    source: [f32; 5],
    pub ground: f32,
    pub air: f32,
    pub water: f32,
    pub fall: f32,
    pub water_fall: f32,
}

impl EaseTable {
    fn new(tuning: &SauerbratenTuning, step: f32) -> Self {
        let source = tuning.easing();
        let factor = |ease: f32| {
            (1.0 - 1.0 / f64::from(ease.max(1.0))).powf(f64::from(step) / EASE_INTERVAL) as f32
        };
        Self {
            step,
            source,
            ground: factor(source[0]),
            air: factor(source[1]),
            water: factor(source[2]),
            fall: factor(source[3]),
            water_fall: factor(source[4]),
        }
    }

    fn matches(&self, tuning: &SauerbratenTuning, step: f32) -> bool {
        self.step.to_bits() == step.to_bits() && self.source == tuning.easing()
    }
}

/// How many equal steps the Sauerbraten velocity model takes in one tick: as
/// many as bring each step closest to its 5 ms physics frame. At 60 Hz that is
/// three steps of 5.6 ms.
pub(crate) fn velocity_steps(dt: f32) -> usize {
    (dt / PHYSICS_FRAME).round().clamp(1.0, 12.0) as usize
}

impl MovementProfile {
    /// Today's movement, exactly: Quake-style acceleration.
    pub const ARENA: Self = Self {
        style: MovementStyle::Arena(ArenaTuning::DEFAULT),
        move_speed: 9.0,
        fly_speed: 16.0,
        jump_speed: 8.2,
        gravity: 25.0,
        radius: PLAYER_RADIUS,
        height: PLAYER_HEIGHT,
        eye_height: EYE_HEIGHT,
        step_height: 0.56,
        water_probe: 0.5,
    };

    pub fn arena() -> Self {
        Self::ARENA
    }

    /// Sauerbraten's movement at the importer's 8 units per metre.
    pub fn sauerbraten() -> Self {
        Self {
            style: MovementStyle::Sauerbraten(SauerbratenTuning::DEFAULT.prepared()),
            move_speed: SAUER_MAX_SPEED / UNITS_PER_METRE,
            fly_speed: SAUER_MAX_SPEED * SAUER_FLOAT_PERCENT / 100.0 / UNITS_PER_METRE,
            jump_speed: SAUER_JUMP_VELOCITY / UNITS_PER_METRE,
            gravity: SAUER_GRAVITY / UNITS_PER_METRE,
            radius: SAUER_RADIUS / UNITS_PER_METRE,
            height: (SAUER_EYE_HEIGHT + SAUER_ABOVE_EYE) / UNITS_PER_METRE,
            eye_height: SAUER_EYE_HEIGHT / UNITS_PER_METRE,
            step_height: SAUER_STAIR_HEIGHT / UNITS_PER_METRE,
            water_probe: SAUER_WATER_PROBE,
        }
    }
}

impl Default for MovementProfile {
    fn default() -> Self {
        Self::ARENA
    }
}

/// Where a pressed jump stands. Sauerbraten fires one jump per press: the press
/// waits for the ground (or water) and a key still held after the jump does not
/// fire again.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JumpLatch {
    /// The key is up.
    #[default]
    Released,
    /// Pressed, and still waiting for something to push off.
    Pending,
    /// Pressed, and its jump already fired.
    Spent,
}

impl JumpLatch {
    fn is_released(&self) -> bool {
        *self == Self::Released
    }
}

/// Simulation state some styles carry between ticks beyond position and
/// velocity. It stays at its default under Arena, and then it is not serialized,
/// so Arena snapshots are unchanged.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct MovementState {
    /// Sauerbraten's separate gravity velocity (vertical, metres per second).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub falling: f32,
    #[serde(default, skip_serializing_if = "JumpLatch::is_released")]
    pub jump: JumpLatch,
    /// Whether the last tick ended in water.
    #[serde(default, skip_serializing_if = "is_false")]
    pub in_water: bool,
}

fn is_zero(value: &f32) -> bool {
    *value == 0.0
}

fn is_false(value: &bool) -> bool {
    !*value
}

impl MovementState {
    pub fn is_idle(&self) -> bool {
        *self == Self::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prepared_factors_match_the_ones_derived_on_the_spot() {
        let tuning = SauerbratenTuning::DEFAULT.prepared();
        let step = FIXED_DT / velocity_steps(FIXED_DT) as f32;
        assert_eq!(tuning.ease_table(step), EaseTable::new(&tuning, step));
        let cached = tuning.cache.unwrap();
        assert!(cached.matches(&tuning, step));
        let mut edited = tuning;
        edited.air_ease = 12.0;
        assert!(!cached.matches(&edited, step));
        assert_eq!(edited.ease_table(step), EaseTable::new(&edited, step));
        assert_ne!(edited.ease_table(step).air, cached.air);
        assert_ne!(tuning.ease_table(0.01).ground, cached.ground);
    }

    #[test]
    fn easing_closes_one_over_ease_of_the_gap_per_twenty_milliseconds() {
        let tuning = SauerbratenTuning::DEFAULT;
        let table = tuning.ease_table(0.02);
        assert!((table.ground - 5.0 / 6.0).abs() < 1.0e-7);
        assert!((table.air - 29.0 / 30.0).abs() < 1.0e-7);
        assert!((table.water - 19.0 / 20.0).abs() < 1.0e-7);
        assert!((table.water_fall - 0.5).abs() < 1.0e-7);
        let quarter = tuning.ease_table(0.005);
        assert!((quarter.ground.powi(4) - table.ground).abs() < 1.0e-6);
    }

    #[test]
    fn one_tick_takes_three_steps_near_the_physics_frame() {
        assert_eq!(velocity_steps(FIXED_DT), 3);
        assert_eq!(velocity_steps(0.05), 10);
        assert_eq!(velocity_steps(0.001), 1);
    }

    #[test]
    fn sauerbraten_numbers_are_the_originals_at_eight_units_per_metre() {
        let profile = MovementProfile::sauerbraten();
        assert_eq!(profile.move_speed, 12.5);
        assert_eq!(profile.jump_speed, 15.625);
        assert_eq!(profile.gravity, 25.0);
        assert_eq!(profile.height, 1.875);
        assert_eq!(profile.eye_height, 1.75);
        assert_eq!(profile.water_probe, 0.75);
        assert!((profile.radius - 0.5125).abs() < 1.0e-6);
        assert!((profile.step_height - 0.5125).abs() < 1.0e-6);
        let MovementStyle::Sauerbraten(tuning) = profile.style else {
            panic!("not Sauerbraten");
        };
        assert_eq!(tuning.water_exit_probe, 0.5);
    }

    #[test]
    fn idle_movement_state_is_not_serialized() {
        assert!(MovementState::default().is_idle());
        let json = serde_json::to_string(&MovementState::default()).unwrap();
        assert_eq!(json, "{}");
        let busy = MovementState {
            falling: -3.5,
            jump: JumpLatch::Spent,
            in_water: true,
        };
        let json = serde_json::to_string(&busy).unwrap();
        assert_eq!(json, r#"{"falling":-3.5,"jump":"spent","in_water":true}"#);
        assert_eq!(serde_json::from_str::<MovementState>(&json).unwrap(), busy);
        assert!(serde_json::from_str::<MovementState>("{}")
            .unwrap()
            .is_idle());
    }
}
