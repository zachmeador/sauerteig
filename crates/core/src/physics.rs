use crate::geometry::{cell_intersects_box, max_height, ray_box_interval, shape_support};
use crate::{
    forward, spawn_point, Cell, CellPos, InputFrame, Player, Vec3, World, PLAYER_HEIGHT,
    PLAYER_RADIUS, WORLD_HEIGHT, WORLD_MAX, WORLD_MIN,
};

pub const MOVE_SPEED: f32 = 9.0;
pub const FLY_SPEED: f32 = 16.0;
pub const JUMP_SPEED: f32 = 8.2;
pub const GRAVITY: f32 = 25.0;
const STEP_HEIGHT: f32 = 0.56;
const SKIN: f32 = 0.0002;

fn axis(value: f32) -> f32 {
    if value.is_finite() {
        value.clamp(-1.0, 1.0)
    } else {
        0.0
    }
}

fn body_bounds(feet: Vec3) -> (Vec3, Vec3) {
    (
        feet - Vec3::new(PLAYER_RADIUS, 0.0, PLAYER_RADIUS),
        feet + Vec3::new(PLAYER_RADIUS, PLAYER_HEIGHT, PLAYER_RADIUS),
    )
}

pub fn player_intersects_cell(player: &Player, pos: CellPos, cell: Cell) -> bool {
    let (min, max) = body_bounds(player.position);
    cell_intersects_box(pos, cell, min, max)
}

pub fn player_intersects_fragment(player: &Player, fragment: &crate::CellFragment) -> bool {
    let (min, max) = body_bounds(player.position);
    crate::fine::fragment_intersects_box(fragment, min, max)
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
    let (min, max) = body_bounds(player.position);
    let (enter, _) = ray_box_interval(origin, direction.normalize(), min, max)?;
    let distance = enter.max(0.0);
    (distance <= max_distance).then_some(distance)
}

/// Highest surface in the requested vertical interval under the player's box.
/// Each cell is two triangular prisms, rather than a flat average-height block.
fn support_between(world: &World, feet: Vec3, min_y: f32, max_y: f32) -> Option<f32> {
    let (min, max) = body_bounds(feet);
    let mut support: Option<f32> = None;
    let y0 = (min_y.floor() as i32 - 1).max(0);
    let y1 = (max_y.floor() as i32).min(WORLD_HEIGHT - 1);
    for z in min.z.floor() as i32..max.z.ceil() as i32 {
        for x in min.x.floor() as i32..max.x.ceil() as i32 {
            for y in y0..=y1 {
                if let Some(root) = world.fine.get(&[x, y, z]) {
                    if let Some(height) =
                        fine_support(root, min, max, min_y - SKIN, max_y + SKIN, false)
                    {
                        support = Some(support.map_or(height, |previous| previous.max(height)));
                    }
                    continue;
                }
                let Some(cell) = world.get([x, y, z]) else {
                    continue;
                };
                if let Some(shape) = cell.shape {
                    if let Some(height) = shape_support(
                        Vec3::new(x as f32, y as f32, z as f32),
                        1.0,
                        shape,
                        min,
                        max,
                        min_y - SKIN,
                        max_y + SKIN,
                        false,
                    ) {
                        support = Some(support.map_or(height, |previous| previous.max(height)));
                    }
                    continue;
                }
                let Some(h) = max_height(
                    cell,
                    min.x - x as f32,
                    min.z - z as f32,
                    max.x - x as f32,
                    max.z - z as f32,
                ) else {
                    continue;
                };
                if h <= 1.0e-5 {
                    continue;
                }
                let height = y as f32 + h;
                if height >= min_y - SKIN && height <= max_y + SKIN {
                    support = Some(support.map_or(height, |previous| previous.max(height)));
                }
            }
        }
    }
    support
}

fn first_ceiling(world: &World, feet: Vec3, old_head: f32, new_head: f32) -> Option<f32> {
    let (min, max) = body_bounds(feet);
    let mut ceiling: Option<f32> = None;
    let y0 = (old_head.floor() as i32).max(0);
    let y1 = (new_head.ceil() as i32).min(WORLD_HEIGHT - 1);
    for z in min.z.floor() as i32..max.z.ceil() as i32 {
        for x in min.x.floor() as i32..max.x.ceil() as i32 {
            for y in y0..=y1 {
                if let Some(root) = world.fine.get(&[x, y, z]) {
                    if let Some(height) =
                        fine_support(root, min, max, old_head - SKIN, new_head + SKIN, true)
                    {
                        ceiling = Some(ceiling.map_or(height, |previous| previous.min(height)));
                    }
                    continue;
                }
                let Some(cell) = world.get([x, y, z]) else {
                    continue;
                };
                if let Some(shape) = cell.shape {
                    if let Some(height) = shape_support(
                        Vec3::new(x as f32, y as f32, z as f32),
                        1.0,
                        shape,
                        min,
                        max,
                        old_head - SKIN,
                        new_head + SKIN,
                        true,
                    ) {
                        ceiling = Some(ceiling.map_or(height, |previous| previous.min(height)));
                    }
                    continue;
                }
                if (y as f32) < old_head - SKIN || y as f32 > new_head + SKIN {
                    continue;
                }
                let height = max_height(
                    cell,
                    min.x - x as f32,
                    min.z - z as f32,
                    max.x - x as f32,
                    max.z - z as f32,
                )
                .unwrap_or(0.0);
                if height > 1.0e-5 {
                    ceiling = Some(ceiling.map_or(y as f32, |previous| previous.min(y as f32)));
                }
            }
        }
    }
    ceiling
}

fn fine_support(
    root: &crate::fine::FineRoot,
    min: Vec3,
    max: Vec3,
    low: f32,
    high: f32,
    ceiling: bool,
) -> Option<f32> {
    let mut result: Option<f32> = None;
    for triangle in &root.triangles {
        let [a, b, c] = triangle.points;
        let normal = (b - a).cross(c - a);
        if if ceiling {
            normal.y >= -1.0e-9
        } else {
            normal.y <= 1.0e-9
        } {
            continue;
        }
        let Some((bottom, top)) =
            crate::geometry::triangle_height_interval(triangle.points, min, max)
        else {
            continue;
        };
        let height = if ceiling { bottom } else { top };
        if height < low || height > high {
            continue;
        }
        result = Some(result.map_or(height, |previous| {
            if ceiling {
                previous.min(height)
            } else {
                previous.max(height)
            }
        }));
    }
    result
}

fn free_at(world: &World, position: Vec3) -> bool {
    let (min, max) = body_bounds(position);
    !world.intersects_box(min, max)
}

fn move_horizontal(world: &World, player: &mut Player, axis: usize, amount: f32) {
    if amount.abs() < 1.0e-7 {
        return;
    }
    let old = player.position;
    let mut target = old;
    target[axis] = (target[axis] + amount).clamp(
        WORLD_MIN as f32 + PLAYER_RADIUS,
        WORLD_MAX as f32 - PLAYER_RADIUS,
    );
    if free_at(world, target) {
        player.position = target;
        return;
    }
    if player.grounded {
        if let Some(height) = support_between(world, target, old.y, old.y + STEP_HEIGHT) {
            let mut stepped = target;
            stepped.y = height + SKIN;
            if free_at(world, stepped) {
                player.position = stepped;
                return;
            }
        }
    }
    // Retain movement up to contact, instead of stopping one substep short.
    let mut low = 0.0;
    let mut high = 1.0;
    for _ in 0..9 {
        let fraction = (low + high) * 0.5;
        if free_at(world, old.lerp(target, fraction)) {
            low = fraction;
        } else {
            high = fraction;
        }
    }
    player.position = old.lerp(target, low);
    player.velocity[axis] = 0.0;
}

fn move_vertical(world: &World, player: &mut Player, amount: f32) {
    let old_y = player.position.y;
    let new_y = old_y + amount;
    if amount > 0.0 {
        if let Some(ceiling) = first_ceiling(
            world,
            player.position,
            old_y + PLAYER_HEIGHT,
            new_y + PLAYER_HEIGHT,
        ) {
            player.position.y = ceiling - PLAYER_HEIGHT - SKIN;
            player.velocity.y = 0.0;
        } else {
            player.position.y = new_y;
        }
        player.grounded = false;
    } else {
        if let Some(ground) = support_between(world, player.position, new_y, old_y) {
            player.position.y = ground + SKIN;
            player.velocity.y = 0.0;
            player.grounded = true;
        } else {
            player.position.y = new_y;
            player.grounded = false;
        }
    }
}

/// Fixed-step, prediction-friendly movement. The caller owns the clock; input
/// never supplies a duration. Combat and editor-mode permission stay outside.
pub fn simulate_player(world: &World, player: &mut Player, input: &InputFrame, dt: f32) {
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
    if !player.position.is_finite() {
        player.position = spawn_point(player.id as u32);
    }
    if !player.velocity.is_finite() {
        player.velocity = Vec3::ZERO;
    }
    let right = Vec3::new(player.yaw.cos(), 0.0, player.yaw.sin());
    if player.editing {
        let wish = forward(player.yaw, player.pitch) * axis(input.forward)
            + right * axis(input.strafe)
            + Vec3::Y * axis(input.vertical);
        player.velocity = wish.clamp_length_max(1.0) * FLY_SPEED;
        player.position += player.velocity * dt;
        player.position.x = player
            .position
            .x
            .clamp(WORLD_MIN as f32 + 0.05, WORLD_MAX as f32 - 0.05);
        player.position.z = player
            .position
            .z
            .clamp(WORLD_MIN as f32 + 0.05, WORLD_MAX as f32 - 0.05);
        player.position.y = player
            .position
            .y
            .clamp(0.05, WORLD_HEIGHT as f32 - PLAYER_HEIGHT);
        player.grounded = false;
        return;
    }

    if player.velocity.y <= 0.0 {
        if let Some(ground) = support_between(
            world,
            player.position,
            player.position.y - 0.075,
            player.position.y,
        ) {
            player.position.y = ground + SKIN;
            player.grounded = true;
            player.velocity.y = 0.0;
        } else {
            player.grounded = false;
        }
    } else {
        player.grounded = false;
    }

    // Ground friction, acceleration along the requested direction, and retained
    // air momentum give movement a fast arena-FPS feel rather than a lerp.
    if player.grounded && !input.jump {
        let speed = Vec3::new(player.velocity.x, 0.0, player.velocity.z).length();
        if speed > 0.0001 {
            let reduced = (speed - speed.max(2.5) * 7.0 * dt).max(0.0) / speed;
            player.velocity.x *= reduced;
            player.velocity.z *= reduced;
        }
    }
    let wish = (forward(player.yaw, 0.0) * axis(input.forward) + right * axis(input.strafe))
        .clamp_length_max(1.0);
    let wish_speed = wish.length() * MOVE_SPEED;
    if wish_speed > 0.0 {
        let direction = wish.normalize();
        let needed = wish_speed - player.velocity.dot(direction);
        if needed > 0.0 {
            let acceleration = if player.grounded { 14.0 } else { 3.0 };
            player.velocity += direction * needed.min(acceleration * wish_speed * dt);
        }
    }
    if player.grounded && input.jump {
        player.velocity.y = JUMP_SPEED;
        player.grounded = false;
    }
    player.velocity.y -= GRAVITY * dt;
    let substeps = ((player.velocity.length() * dt / 0.15).ceil() as usize).clamp(1, 64);
    let step_dt = dt / substeps as f32;
    for _ in 0..substeps {
        let dx = player.velocity.x * step_dt;
        move_horizontal(world, player, 0, dx);
        let dz = player.velocity.z * step_dt;
        move_horizontal(world, player, 2, dz);
        let dy = player.velocity.y * step_dt;
        move_vertical(world, player, dy);
    }
    if player.position.y < -12.0 {
        player.position = spawn_point(player.id as u32);
        player.velocity = Vec3::ZERO;
        player.grounded = false;
    }
}
