//! The renderer-independent world and simulation shared by every client and server.

mod compact;
mod fine;
mod geometry;
mod movement;
mod physics;
mod protocol;
mod volumes;
mod world;

pub use compact::{
    WorldBounds, MAX_COMPACT_BATCH_TRIANGLES, MAX_COMPACT_REGIONS, MAX_QUERY_ROOTS,
    WORLD_FORMAT_VERSION,
};
pub use fine::*;
pub use glam::Vec3;
pub use movement::*;
pub use physics::*;
pub use protocol::*;
pub use volumes::*;
pub use world::*;

pub const CHUNK_SIZE: i32 = 8;
pub const WORLD_MIN: i32 = -64;
pub const WORLD_MAX: i32 = 64;
pub const WORLD_HEIGHT: i32 = 48;
// Hit detection, spawning, rendering and the camera use these dimensions.
// Movement takes its own from the `MovementProfile`; Arena's equal these.
pub const PLAYER_RADIUS: f32 = 0.3;
pub const PLAYER_HEIGHT: f32 = 1.75;
pub const EYE_HEIGHT: f32 = 1.55;
pub const FIXED_DT: f32 = 1.0 / 60.0;

/// Yaw zero faces -Z; positive yaw turns right and positive pitch looks up.
pub fn forward(yaw: f32, pitch: f32) -> Vec3 {
    Vec3::new(
        libm::sinf(yaw) * libm::cosf(pitch),
        libm::sinf(pitch),
        -libm::cosf(yaw) * libm::cosf(pitch),
    )
}
