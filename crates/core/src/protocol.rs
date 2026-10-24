use crate::{spawn_point, CellEdit, RootEdit, Vec3, EYE_HEIGHT};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
pub struct InputFrame {
    pub seq: u32,
    pub forward: f32,
    pub strafe: f32,
    pub vertical: f32,
    pub yaw: f32,
    pub pitch: f32,
    pub jump: bool,
    pub fire: bool,
    pub weapon: u8,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Player {
    pub id: u64,
    pub name: String,
    /// Feet position, not the eye or the center of the collision volume.
    pub position: Vec3,
    pub velocity: Vec3,
    pub yaw: f32,
    pub pitch: f32,
    pub editing: bool,
    pub mode_request: u32,
    pub grounded: bool,
    pub health: u16,
    pub score: i32,
    pub last_input: u32,
}

impl Player {
    pub fn new(id: u64, name: String) -> Self {
        Self {
            id,
            name,
            position: spawn_point(id as u32),
            velocity: Vec3::ZERO,
            yaw: 0.0,
            pitch: 0.0,
            editing: false,
            mode_request: 0,
            grounded: false,
            health: 100,
            score: 0,
            last_input: 0,
        }
    }

    pub fn eye(&self) -> Vec3 {
        self.position + Vec3::Y * EYE_HEIGHT
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObjectKind {
    Tree,
    Light,
    Spawn,
    WallLamp,
    HangingLamp,
    UtilityBox,
    Pipe,
    Rock,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorldObject {
    pub id: u32,
    pub position: Vec3,
    pub kind: ObjectKind,
    pub rotation: f32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Projectile {
    pub id: u64,
    pub owner: u64,
    pub position: Vec3,
    pub velocity: Vec3,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ShotEffect {
    pub from: Vec3,
    pub to: Vec3,
    pub owner: u64,
    pub hit: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
    Hello {
        name: String,
    },
    Input {
        input: InputFrame,
    },
    SetMode {
        request: u32,
        editing: bool,
    },
    Edit {
        request: u64,
        edits: Vec<CellEdit>,
    },
    /// Replace only the sparse roots touched by a volume or blueprint edit.
    /// The server rejects a touched root changed after this snapshot revision.
    EditRoots {
        request: u64,
        base_revision: u64,
        roots: Vec<RootEdit>,
    },
    PlaceObject {
        kind: ObjectKind,
        position: Vec3,
        rotation: f32,
    },
    RemoveObject {
        id: u32,
    },
    Undo,
    Redo,
    Save,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMessage {
    ModeChanged {
        request: u32,
        editing: bool,
    },
    Welcome {
        id: u64,
        revision: u64,
        cells: Vec<CellEdit>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        roots: Vec<RootEdit>,
        objects: Vec<WorldObject>,
        players: Vec<Player>,
        editing_allowed: bool,
    },
    Snapshot {
        tick: u64,
        revision: u64,
        players: Vec<Player>,
        projectiles: Vec<Projectile>,
    },
    WorldPatch {
        revision: u64,
        edits: Vec<CellEdit>,
    },
    RootPatch {
        revision: u64,
        roots: Vec<RootEdit>,
    },
    EditResult {
        request: u64,
        error: Option<String>,
    },
    Objects {
        revision: u64,
        objects: Vec<WorldObject>,
    },
    Shots {
        shots: Vec<ShotEffect>,
    },
    Notice {
        message: String,
    },
    Error {
        message: String,
    },
}
