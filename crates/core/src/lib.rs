//! The renderer-independent world and simulation shared by every client and server.

mod fine;
mod geometry;
mod movement;
mod octree;
mod physics;
mod protocol;
mod volumes;
mod world;

pub use fine::*;
pub use glam::Vec3;
pub use movement::*;
pub use octree::{
    cube_material_for, volume_material_of, Format3, Format3Piece, Leaf, LeafGeometry, NodeBox,
    Octree, OctreeBuilder, PieceChanges, Source, TreeStats, MAT_AIR, MAT_GLASS, MAT_WATER,
    MAX_CONVERSION_LEAVES, MAX_ROOT_SIZE, MESH_UNIT_SIZE,
};
pub use physics::*;
pub use protocol::*;
pub use volumes::*;
pub use world::*;

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
