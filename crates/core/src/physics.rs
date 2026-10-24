use crate::geometry::{cell_intersects_box, max_height, ray_box_interval, shape_support};
use crate::{
    forward, spawn_point, Cell, CellPos, InputFrame, Player, Vec3, World, PLAYER_HEIGHT,
    PLAYER_RADIUS,
};

pub const MOVE_SPEED: f32 = 9.0;
pub const FLY_SPEED: f32 = 16.0;
pub const JUMP_SPEED: f32 = 8.2;
pub const GRAVITY: f32 = 25.0;
const STEP_HEIGHT: f32 = 0.56;
const SKIN: f32 = 0.0002;

// A fixed submillimetre bias can round away on an original-scale map. Keep
// contacts a few representable steps apart without changing the small arena.
fn contact_skin(position: Vec3) -> f32 {
    SKIN.max(position.abs().max_element() * f32::EPSILON * 2.0)
}

fn fallback_spawn(world: &World, player_id: u32) -> Vec3 {
    let bounds = world.bounds();
    let mut position = spawn_point(player_id);
    let skin = contact_skin(bounds.world_min().abs().max(bounds.world_max().abs()));
    for axis in [0, 2] {
        let low = bounds.min[axis] as f32 + PLAYER_RADIUS + skin;
        let high = (bounds.max[axis] as f32 - PLAYER_RADIUS - skin).max(low);
        position[axis] = position[axis].clamp(low, high);
    }
    let low = bounds.min[1] as f32 + 0.05;
    let high = (bounds.max[1] as f32 - PLAYER_HEIGHT - skin).max(low);
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
    let skin = contact_skin(feet);
    if world.has_regions() {
        return compact_support(world, min, max, min_y - skin, max_y + skin, false);
    }
    let bounds = world.bounds();
    let mut support: Option<f32> = None;
    let y0 = (min_y.floor() as i32 - 1).max(bounds.min[1]);
    let y1 = (max_y.floor() as i32).min(bounds.max[1] - 1);
    for z in min.z.floor() as i32..max.z.ceil() as i32 {
        for x in min.x.floor() as i32..max.x.ceil() as i32 {
            for y in y0..=y1 {
                if let Some(root) = world.fine.get(&[x, y, z]) {
                    if let Some(height) =
                        fine_support(root, min, max, min_y - skin, max_y + skin, false)
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
                        shape,
                        min,
                        max,
                        min_y - skin,
                        max_y + skin,
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
                if height >= min_y - skin && height <= max_y + skin {
                    support = Some(support.map_or(height, |previous| previous.max(height)));
                }
            }
        }
    }
    support
}

fn first_ceiling(world: &World, feet: Vec3, old_head: f32, new_head: f32) -> Option<f32> {
    let (min, max) = body_bounds(feet);
    let skin = contact_skin(feet);
    if world.has_regions() {
        return compact_support(world, min, max, old_head - skin, new_head + skin, true);
    }
    let bounds = world.bounds();
    let mut ceiling: Option<f32> = None;
    let y0 = (old_head.floor() as i32).max(bounds.min[1]);
    let y1 = (new_head.ceil() as i32).min(bounds.max[1] - 1);
    for z in min.z.floor() as i32..max.z.ceil() as i32 {
        for x in min.x.floor() as i32..max.x.ceil() as i32 {
            for y in y0..=y1 {
                if let Some(root) = world.fine.get(&[x, y, z]) {
                    if let Some(height) =
                        fine_support(root, min, max, old_head - skin, new_head + skin, true)
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
                        shape,
                        min,
                        max,
                        old_head - skin,
                        new_head + skin,
                        true,
                    ) {
                        ceiling = Some(ceiling.map_or(height, |previous| previous.min(height)));
                    }
                    continue;
                }
                if (y as f32) < old_head - skin || y as f32 > new_head + skin {
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

// The query only selects nearby fragments. Their complete active boundaries
// must be retained: clipping to this scan interval would invent a floor or
// ceiling at the query's own limits.
fn compact_support(
    world: &World,
    min: Vec3,
    max: Vec3,
    low: f32,
    high: f32,
    ceiling: bool,
) -> Option<f32> {
    let query = crate::FineBox {
        min: (Vec3::new(min.x, low, min.z) * crate::FINE_UNITS as f32)
            .floor()
            .as_ivec3()
            .to_array(),
        max: (Vec3::new(max.x, high, max.z) * crate::FINE_UNITS as f32)
            .ceil()
            .as_ivec3()
            .to_array(),
    };
    let mut result: Option<f32> = None;
    for id in world.compact.query(query) {
        let root = &world.compact.regions[id].active;
        accumulate_support(
            root.triangles
                .iter()
                .filter(|triangle| {
                    root.fragments[triangle.fragment]
                        .bounds
                        .intersection(query)
                        .is_some()
                })
                .map(|triangle| triangle.points),
            min,
            max,
            low,
            high,
            ceiling,
            &mut result,
        );
    }
    let bounds = world.bounds();
    let lo = std::array::from_fn::<_, 3, _>(|axis| {
        query.min[axis]
            .div_euclid(crate::FINE_UNITS)
            .max(bounds.min[axis])
    });
    let hi = std::array::from_fn::<_, 3, _>(|axis| {
        ((query.max[axis] - 1).div_euclid(crate::FINE_UNITS) + 1).min(bounds.max[axis])
    });
    for x in lo[0]..hi[0] {
        for y in lo[1]..hi[1] {
            for z in lo[2]..hi[2] {
                let pos = [x, y, z];
                if let Some(root) = world.fine.get(&pos) {
                    accumulate_support(
                        root.triangles.iter().map(|triangle| triangle.points),
                        min,
                        max,
                        low,
                        high,
                        ceiling,
                        &mut result,
                    );
                } else if let Some(cell) = world.get(pos) {
                    accumulate_support(
                        crate::geometry::shape_triangles(
                            cell.as_shape(),
                            Vec3::new(x as f32, y as f32, z as f32),
                            1.0,
                        )
                        .into_iter()
                        .flatten()
                        .map(|(triangle, _)| triangle),
                        min,
                        max,
                        low,
                        high,
                        ceiling,
                        &mut result,
                    );
                }
            }
        }
    }
    result
}

fn fine_support(
    root: &crate::fine::FineRoot,
    min: Vec3,
    max: Vec3,
    low: f32,
    high: f32,
    ceiling: bool,
) -> Option<f32> {
    let mut result = None;
    accumulate_support(
        root.triangles.iter().map(|triangle| triangle.points),
        min,
        max,
        low,
        high,
        ceiling,
        &mut result,
    );
    result
}

fn accumulate_support(
    triangles: impl Iterator<Item = [Vec3; 3]>,
    min: Vec3,
    max: Vec3,
    low: f32,
    high: f32,
    ceiling: bool,
    result: &mut Option<f32>,
) {
    for triangle in triangles {
        let [a, b, c] = triangle;
        let normal = (b - a).cross(c - a);
        if if ceiling {
            normal.y >= -1.0e-9
        } else {
            normal.y <= 1.0e-9
        } {
            continue;
        }
        // Work near the player rather than at a far-away map origin. This
        // preserves small height differences during footprint clipping.
        let origin = Vec3::new(min.x, low, min.z);
        let Some((bottom, top)) = crate::geometry::triangle_height_interval(
            triangle.map(|point| point - origin),
            min - origin,
            max - origin,
        ) else {
            continue;
        };
        let height = origin.y + if ceiling { bottom } else { top };
        if height < low || height > high {
            continue;
        }
        *result = Some(result.map_or(height, |previous| {
            if ceiling {
                previous.min(height)
            } else {
                previous.max(height)
            }
        }));
    }
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
    let bounds = world.bounds();
    target[axis] = (target[axis] + amount).clamp(
        bounds.min[axis] as f32 + PLAYER_RADIUS,
        bounds.max[axis] as f32 - PLAYER_RADIUS,
    );
    if free_at(world, target) {
        player.position = target;
        return;
    }
    if player.grounded {
        if let Some(height) = support_between(world, target, old.y, old.y + STEP_HEIGHT) {
            let mut stepped = target;
            stepped.y = height + contact_skin(stepped);
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
            player.position.y = ceiling - PLAYER_HEIGHT - contact_skin(player.position);
            player.velocity.y = 0.0;
        } else {
            player.position.y = new_y;
        }
        player.grounded = false;
    } else {
        if let Some(ground) = support_between(world, player.position, new_y, old_y) {
            player.position.y = ground + contact_skin(player.position);
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
        player.position = fallback_spawn(world, player.id as u32);
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
        let bounds = world.bounds();
        for axis in [0, 2] {
            player.position[axis] = player.position[axis].clamp(
                bounds.min[axis] as f32 + 0.05,
                bounds.max[axis] as f32 - 0.05,
            );
        }
        let floor_limit = bounds.min[1] as f32 + 0.05;
        let ceiling_limit = (bounds.max[1] as f32 - PLAYER_HEIGHT).max(floor_limit);
        player.position.y = player.position.y.clamp(floor_limit, ceiling_limit);
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
            player.position.y = ground + contact_skin(player.position);
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
    if player.position.y < world.bounds().min[1] as f32 - 12.0 {
        player.position = fallback_spawn(world, player.id as u32);
        player.velocity = Vec3::ZERO;
        player.grounded = false;
    }
}
