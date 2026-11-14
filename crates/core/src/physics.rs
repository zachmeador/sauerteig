use crate::geometry::ray_box_interval;
use crate::movement::velocity_steps;
use crate::{
    forward, spawn_point, ArenaTuning, Cell, CellFragment, CellPos, InputFrame, JumpLatch,
    MovementProfile, MovementState, MovementStyle, Octree, Player, SauerbratenTuning, Vec3,
    VolumeMaterial, World, WorldBounds, PLAYER_HEIGHT, PLAYER_RADIUS,
};

const SKIN: f32 = 0.0002;

/// The box a movement query sweeps. Movement takes it from the profile; hit
/// detection keeps the shared player constants.
#[derive(Clone, Copy)]
pub(crate) struct Body {
    radius: f32,
    height: f32,
}

impl Body {
    #[cfg(test)]
    pub(crate) fn new(radius: f32, height: f32) -> Self {
        Self { radius, height }
    }

    const HIT: Self = Self {
        radius: PLAYER_RADIUS,
        height: PLAYER_HEIGHT,
    };

    fn of(profile: &MovementProfile) -> Self {
        Self {
            radius: profile.radius,
            height: profile.height,
        }
    }
}

// A fixed submillimetre bias can round away on an original-scale map. Keep
// contacts a few representable steps apart without changing the small arena.
fn contact_skin(position: Vec3) -> f32 {
    SKIN.max(position.abs().max_element() * f32::EPSILON * 2.0)
}

/// What the movement step asks of the world it moves through.
pub(crate) trait Space {
    fn space_bounds(&self) -> WorldBounds;
    /// Highest floor in `[min_y, max_y]` under the body, from solids and glass.
    fn support_between(&self, feet: Vec3, min_y: f32, max_y: f32, body: Body) -> Option<f32>;
    /// Lowest ceiling the head meets rising from `old_head` to `new_head`.
    fn first_ceiling(&self, feet: Vec3, old_head: f32, new_head: f32, body: Body) -> Option<f32>;
    fn free_at(&self, position: Vec3, body: Body) -> bool;
    fn swim_state(&self, feet: Vec3, probe_height: f32) -> bool;
}

/// The tree's single path: every leaf kind answers support, ceiling, overlap
/// and swimming queries once, whatever kind of world it came from.
impl Space for Octree {
    fn space_bounds(&self) -> WorldBounds {
        self.bounds()
    }
    fn support_between(&self, feet: Vec3, min_y: f32, max_y: f32, body: Body) -> Option<f32> {
        let (min, max) = body_bounds(feet, body);
        let skin = contact_skin(feet);
        let terrain = self.terrain_support(min, max, min_y - skin, max_y + skin, false);
        let glass = self.glass_support(min, max, min_y, max_y, skin, false);
        match (terrain, glass) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (a, b) => a.or(b),
        }
    }
    fn first_ceiling(&self, feet: Vec3, old_head: f32, new_head: f32, body: Body) -> Option<f32> {
        let (min, max) = body_bounds(feet, body);
        let skin = contact_skin(feet);
        let terrain = self.terrain_support(min, max, old_head - skin, new_head + skin, true);
        let glass = self.glass_support(min, max, old_head, new_head, skin, true);
        match (terrain, glass) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }
    fn free_at(&self, position: Vec3, body: Body) -> bool {
        let (min, max) = body_bounds(position, body);
        !self.intersects_box(min, max) && !self.intersects_solid_volume(min, max)
    }
    fn swim_state(&self, feet: Vec3, probe_height: f32) -> bool {
        self.volume_material_at(feet + Vec3::Y * probe_height) == Some(VolumeMaterial::Water)
    }
}

impl Octree {
    /// The movement step on the tree, as `simulate_player_with` steps it on a
    /// world.
    pub fn simulate_player_with(
        &self,
        player: &mut Player,
        input: &InputFrame,
        dt: f32,
        profile: &MovementProfile,
    ) {
        simulate_in(self, player, input, dt, profile);
    }
}

fn fallback_spawn<S: Space + ?Sized>(world: &S, player_id: u32, body: Body) -> Vec3 {
    let bounds = world.space_bounds();
    let mut position = spawn_point(player_id);
    let skin = contact_skin(bounds.world_min().abs().max(bounds.world_max().abs()));
    for axis in [0, 2] {
        let low = bounds.min[axis] as f32 + body.radius + skin;
        let high = (bounds.max[axis] as f32 - body.radius - skin).max(low);
        position[axis] = position[axis].clamp(low, high);
    }
    let low = bounds.min[1] as f32 + 0.05;
    let high = (bounds.max[1] as f32 - body.height - skin).max(low);
    position.y = position.y.clamp(low, high);
    position
}

fn axis(value: f32) -> f32 {
    if value.is_finite() {
        value.clamp(-1.0, 1.0)
    } else {
        0.0
    }
}

fn body_bounds(feet: Vec3, body: Body) -> (Vec3, Vec3) {
    (
        feet - Vec3::new(body.radius, 0.0, body.radius),
        feet + Vec3::new(body.radius, body.height, body.radius),
    )
}

pub fn player_intersects_cell(player: &Player, pos: CellPos, cell: Cell) -> bool {
    player_intersects_fragment(player, &CellFragment::from_cell(pos, cell))
}

/// Whether a player's body overlaps a fragment's solid, as a world holding
/// the fragment would block it.
pub fn player_intersects_fragment(player: &Player, fragment: &CellFragment) -> bool {
    let (min, max) = body_bounds(player.position, Body::HIT);
    crate::octree::fragment_overlaps_box(fragment, min, max)
}

pub fn player_intersects_volume(player: &Player, volume: &crate::MaterialVolume) -> bool {
    let (min, max) = body_bounds(player.position, Body::HIT);
    min.cmplt(volume.bounds.world_max()).all() && max.cmpgt(volume.bounds.world_min()).all()
}

pub fn ray_player(
    origin: Vec3,
    direction: Vec3,
    player: &Player,
    max_distance: f32,
) -> Option<f32> {
    if player.editing
        || player.health == 0
        || !origin.is_finite()
        || !direction.is_finite()
        || direction.length_squared() < 1.0e-10
        || !max_distance.is_finite()
        || max_distance < 0.0
    {
        return None;
    }
    let (min, max) = body_bounds(player.position, Body::HIT);
    let (enter, _) = ray_box_interval(origin, direction.normalize(), min, max)?;
    let distance = enter.max(0.0);
    (distance <= max_distance).then_some(distance)
}

/// Returns true when an obstacle stopped the move, after zeroing that axis of
/// the player's velocity.
fn move_horizontal<S: Space + ?Sized>(
    world: &S,
    player: &mut Player,
    body: Body,
    step_height: f32,
    axis: usize,
    amount: f32,
) -> bool {
    if amount.abs() < 1.0e-7 {
        return false;
    }
    let old = player.position;
    let mut target = old;
    let bounds = world.space_bounds();
    target[axis] = (target[axis] + amount).clamp(
        bounds.min[axis] as f32 + body.radius,
        bounds.max[axis] as f32 - body.radius,
    );
    if world.free_at(target, body) {
        player.position = target;
        return false;
    }
    if player.grounded {
        if let Some(height) = world.support_between(target, old.y, old.y + step_height, body) {
            let mut stepped = target;
            stepped.y = height + contact_skin(stepped);
            if world.free_at(stepped, body) {
                player.position = stepped;
                return false;
            }
        }
    }
    // Retain movement up to contact, instead of stopping one substep short.
    let mut low = 0.0;
    let mut high = 1.0;
    for _ in 0..9 {
        let fraction = (low + high) * 0.5;
        if world.free_at(old.lerp(target, fraction), body) {
            low = fraction;
        } else {
            high = fraction;
        }
    }
    player.position = old.lerp(target, low);
    player.velocity[axis] = 0.0;
    true
}

/// Returns true on contact with a ceiling or the ground, which also ends the
/// player's vertical velocity.
fn move_vertical<S: Space + ?Sized>(
    world: &S,
    player: &mut Player,
    body: Body,
    amount: f32,
) -> bool {
    let old_y = player.position.y;
    let new_y = old_y + amount;
    if amount > 0.0 {
        let contact = if let Some(ceiling) = world.first_ceiling(
            player.position,
            old_y + body.height,
            new_y + body.height,
            body,
        ) {
            player.position.y = ceiling - body.height - contact_skin(player.position);
            player.velocity.y = 0.0;
            player.movement.falling = 0.0;
            true
        } else {
            player.position.y = new_y;
            false
        };
        player.grounded = false;
        contact
    } else if let Some(ground) = world.support_between(player.position, new_y, old_y, body) {
        player.position.y = ground + contact_skin(player.position);
        player.velocity.y = 0.0;
        player.movement.falling = 0.0;
        player.grounded = true;
        true
    } else {
        player.position.y = new_y;
        player.grounded = false;
        false
    }
}

/// Fixed-step, prediction-friendly movement with the Arena profile. The caller
/// owns the clock; input never supplies a duration. Combat and editor-mode
/// permission stay outside.
pub fn simulate_player(world: &World, player: &mut Player, input: &InputFrame, dt: f32) {
    simulate_player_with(world, player, input, dt, &MovementProfile::ARENA);
}

/// Steps a player by one fixed tick under the given movement profile. The
/// server and every client's prediction must use the same profile.
pub fn simulate_player_with(
    world: &World,
    player: &mut Player,
    input: &InputFrame,
    dt: f32,
    profile: &MovementProfile,
) {
    simulate_in(&world.tree, player, input, dt, profile);
}

/// The movement step on any world the step can query.
pub(crate) fn simulate_in<S: Space + ?Sized>(
    world: &S,
    player: &mut Player,
    input: &InputFrame,
    dt: f32,
    profile: &MovementProfile,
) {
    player.last_input = input.seq;
    if input.yaw.is_finite() {
        player.yaw = input.yaw.rem_euclid(std::f32::consts::TAU);
    }
    if input.pitch.is_finite() {
        player.pitch = input.pitch.clamp(-1.53, 1.53);
    }
    if !dt.is_finite() || dt <= 0.0 || player.health == 0 {
        return;
    }
    let dt = dt.min(0.05);
    let body = Body::of(profile);
    if !player.position.is_finite() {
        player.position = fallback_spawn(world, player.id as u32, body);
    }
    if !player.velocity.is_finite() {
        player.velocity = Vec3::ZERO;
    }
    if !player.movement.falling.is_finite() {
        player.movement.falling = 0.0;
    }
    let right = Vec3::new(libm::cosf(player.yaw), 0.0, libm::sinf(player.yaw));
    if player.editing {
        let wish = forward(player.yaw, player.pitch) * axis(input.forward)
            + right * axis(input.strafe)
            + Vec3::Y * axis(input.vertical);
        player.velocity = wish.clamp_length_max(1.0) * profile.fly_speed;
        player.position += player.velocity * dt;
        let bounds = world.space_bounds();
        for axis in [0, 2] {
            player.position[axis] = player.position[axis].clamp(
                bounds.min[axis] as f32 + 0.05,
                bounds.max[axis] as f32 - 0.05,
            );
        }
        let floor_limit = bounds.min[1] as f32 + 0.05;
        let ceiling_limit = (bounds.max[1] as f32 - body.height).max(floor_limit);
        player.position.y = player.position.y.clamp(floor_limit, ceiling_limit);
        player.grounded = false;
        player.movement = MovementState::default();
        return;
    }

    let rising = match profile.style {
        MovementStyle::Arena(_) => player.velocity.y,
        MovementStyle::Sauerbraten(_) => player.velocity.y + player.movement.falling,
    };
    if rising <= 0.0 {
        if let Some(ground) = world.support_between(
            player.position,
            player.position.y - 0.075,
            player.position.y,
            body,
        ) {
            player.position.y = ground + contact_skin(player.position);
            player.grounded = true;
            player.velocity.y = 0.0;
            player.movement.falling = 0.0;
        } else {
            player.grounded = false;
        }
    } else {
        player.grounded = false;
    }

    let swimming = world.swim_state(player.position, profile.height * profile.water_probe);
    let mut motion = match &profile.style {
        MovementStyle::Arena(arena) => {
            player.movement = MovementState::default();
            arena_velocity(player, input, dt, profile, arena, right, swimming);
            player.velocity
        }
        MovementStyle::Sauerbraten(sauerbraten) => {
            sauerbraten_motion(player, input, dt, profile, sauerbraten, right, swimming)
        }
    };

    let substeps = ((motion.length() * dt / 0.15).ceil() as usize).clamp(1, 64);
    let step_dt = dt / substeps as f32;
    for _ in 0..substeps {
        for lane in [0, 2] {
            let amount = motion[lane] * step_dt;
            if move_horizontal(world, player, body, profile.step_height, lane, amount) {
                motion[lane] = 0.0;
            }
        }
        if move_vertical(world, player, body, motion.y * step_dt) {
            motion.y = 0.0;
        }
    }
    if let MovementStyle::Sauerbraten(sauerbraten) = &profile.style {
        let wet = swimming
            || (player.movement.in_water
                && world.swim_state(
                    player.position,
                    profile.height * sauerbraten.water_exit_probe,
                ));
        player.movement.in_water = wet;
    }
    if player.position.y < world.space_bounds().min[1] as f32 - 12.0 {
        player.position = fallback_spawn(world, player.id as u32, body);
        player.velocity = Vec3::ZERO;
        player.grounded = false;
        player.movement = MovementState::default();
    }
}

/// Updates the velocity for one Arena tick and applies gravity. Ground
/// friction, acceleration along the requested direction, and retained air
/// momentum give movement a fast arena-FPS feel rather than a lerp.
fn arena_velocity(
    player: &mut Player,
    input: &InputFrame,
    dt: f32,
    profile: &MovementProfile,
    arena: &ArenaTuning,
    right: Vec3,
    swimming: bool,
) {
    if swimming {
        player.velocity *= (1.0 - arena.water_drag * dt).max(0.0);
    } else if player.grounded && !input.jump {
        let speed = Vec3::new(player.velocity.x, 0.0, player.velocity.z).length();
        if speed > 0.0001 {
            let reduced =
                (speed - speed.max(arena.stop_speed) * arena.friction * dt).max(0.0) / speed;
            player.velocity.x *= reduced;
            player.velocity.z *= reduced;
        }
    }
    let wish = (forward(player.yaw, 0.0) * axis(input.forward) + right * axis(input.strafe))
        .clamp_length_max(1.0);
    let wish_speed = wish.length()
        * if swimming {
            arena.swim_speed
        } else {
            profile.move_speed
        };
    if wish_speed > 0.0 {
        let direction = wish.normalize();
        let needed = wish_speed - player.velocity.dot(direction);
        if needed > 0.0 {
            let acceleration = if swimming {
                arena.water_acceleration
            } else if player.grounded {
                arena.ground_acceleration
            } else {
                arena.air_acceleration
            };
            player.velocity += direction * needed.min(acceleration * wish_speed * dt);
        }
    }
    if swimming {
        let vertical = if input.jump {
            1.0
        } else {
            axis(input.vertical)
        };
        if vertical != 0.0 {
            let target = vertical * arena.swim_speed;
            let change = (target - player.velocity.y).clamp(
                -arena.swim_speed * arena.water_acceleration * dt,
                arena.swim_speed * arena.water_acceleration * dt,
            );
            player.velocity.y += change;
        }
        player.grounded = false;
    } else if player.grounded && input.jump {
        player.velocity.y = profile.jump_speed;
        player.grounded = false;
    }
    player.velocity.y -= profile.gravity * if swimming { arena.water_gravity } else { 1.0 } * dt;
}

/// Runs Sauerbraten's `modifygravity` and `modifyvelocity` for one tick, in the
/// equal steps `moveplayer` would take, and returns the average velocity to move
/// the player with. Contact with the world is left to the caller: the engine's
/// collision, floor and step handling are not part of this port. `velocity` is
/// the engine's `vel`, eased toward the steering target. `movement.falling` is
/// its `falling` vector, kept vertical because only sliding down a slope gives it
/// a horizontal part. A port of Sauerbraten's `physics.cpp` (zlib licensed; see
/// `THIRD_PARTY_NOTICES.md`).
fn sauerbraten_motion(
    player: &mut Player,
    input: &InputFrame,
    dt: f32,
    profile: &MovementProfile,
    tuning: &SauerbratenTuning,
    right: Vec3,
    swimming: bool,
) -> Vec3 {
    let steps = velocity_steps(dt);
    let step = dt / steps as f32;
    let ease = tuning.ease_table(step);

    // One jump per press: the press waits for the floor or water, and a key
    // still held after the jump fired does not fire again.
    player.movement.jump = match (player.movement.jump, input.jump) {
        (_, false) => JumpLatch::Released,
        (JumpLatch::Released, true) => JumpLatch::Pending,
        (latch, true) => latch,
    };

    let forward_axis = axis(input.forward);
    let strafe_axis = axis(input.strafe);
    // Swimmers also steer with the vertical keys, as in Arena.
    let vertical_axis = if swimming { axis(input.vertical) } else { 0.0 };
    let steering = forward_axis != 0.0 || strafe_axis != 0.0 || vertical_axis != 0.0;
    let straight = forward_axis != 0.0 && strafe_axis == 0.0;
    let mut wish = Vec3::ZERO;
    if steering {
        // vecfromyawpitch: only swimmers steer by pitch.
        let pitch = if swimming { player.pitch } else { 0.0 };
        wish = forward(player.yaw, pitch) * forward_axis
            + right * strafe_axis
            + Vec3::Y * vertical_axis;
        if swimming && player.grounded {
            wish.y = wish.y.max(0.0);
        }
        let strength = (forward_axis * forward_axis
            + strafe_axis * strafe_axis
            + vertical_axis * vertical_axis)
            .sqrt()
            .min(1.0);
        wish = wish.normalize_or_zero() * strength;
    }

    let mut floor = player.grounded;
    let mut entering = swimming && !player.movement.in_water;
    let mut displacement = Vec3::ZERO;
    for _ in 0..steps {
        if !floor && !(swimming && steering) {
            player.movement.falling -= profile.gravity * step;
        }
        if swimming || floor {
            player.movement.falling *= if swimming { ease.water_fall } else { ease.fall };
        }

        let mut launched = false;
        if swimming || floor {
            if entering {
                player.velocity /= tuning.water_divisor;
                entering = false;
            }
            if player.movement.jump == JumpLatch::Pending {
                player.movement.jump = JumpLatch::Spent;
                player.velocity.y = player.velocity.y.max(profile.jump_speed);
                if swimming {
                    player.velocity.x /= tuning.water_divisor;
                    player.velocity.z /= tuning.water_divisor;
                }
                launched = true;
            }
        }

        let mut target = wish * profile.move_speed;
        if !swimming {
            let boost = if straight { tuning.forward_boost } else { 1.0 }
                * if floor { 1.0 } else { tuning.air_boost };
            target *= boost;
        }
        let factor = if swimming {
            ease.water
        } else if floor {
            ease.ground
        } else {
            ease.air
        };
        player.velocity = target + (player.velocity - target) * factor;

        let mut moved = player.velocity * if swimming { tuning.water_scale } else { 1.0 };
        moved.y += player.movement.falling;
        displacement += moved * step;
        // A jump leaves the floor for the rest of the tick, but its own step
        // still eases with the floor's factor, as the engine's first step does.
        floor &= !launched;
    }
    displacement / dt
}
