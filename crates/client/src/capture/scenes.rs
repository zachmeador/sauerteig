//! Synthetic worlds for repeatable lighting captures. Each scene is built from
//! light nodes and solid cells only, so any renderer revision can draw it.
use crate::lighting::{LightDesc, ShadowMode};
use glam::Vec3;
use sauerteig_core::{
    Cell, FineBox, MaterialVolume, ObjectKind, VolumeMaterial, World, WorldObject,
};

/// A world, its objects, and lights that no object can express yet.
pub struct Scene {
    pub world: World,
    pub objects: Vec<WorldObject>,
    pub lights: Vec<LightDesc>,
}

impl From<(World, Vec<WorldObject>)> for Scene {
    fn from((world, objects): (World, Vec<WorldObject>)) -> Self {
        Self {
            world,
            objects,
            lights: Vec::new(),
        }
    }
}

fn fill(world: &mut World, min: [i32; 3], max: [i32; 3], material: u8) {
    for x in min[0]..max[0] {
        for y in min[1]..max[1] {
            for z in min[2]..max[2] {
                world.set([x, y, z], Some(Cell::solid(material)));
            }
        }
    }
}

fn carve(world: &mut World, min: [i32; 3], max: [i32; 3]) {
    for x in min[0]..max[0] {
        for y in min[1]..max[1] {
            for z in min[2]..max[2] {
                world.set([x, y, z], None);
            }
        }
    }
}

fn node(id: u32, x: f32, y: f32, z: f32) -> WorldObject {
    WorldObject {
        id,
        position: Vec3::new(x, y, z),
        rotation: 0.0,
        kind: ObjectKind::Light,
    }
}

fn volume(min: [i32; 3], max: [i32; 3], material: VolumeMaterial) -> MaterialVolume {
    MaterialVolume {
        bounds: FineBox { min, max },
        material,
    }
}

/// A closed 46 m room with pillars and crates, and 64 light nodes on an 8 x 8
/// grid around its center: far more lights overlap the middle than one fragment
/// may evaluate.
pub fn lightroom() -> (World, Vec<WorldObject>) {
    let mut world = World::empty();
    fill(&mut world, [-24, 0, -24], [24, 1, 24], 0);
    fill(&mut world, [-24, 11, -24], [24, 12, 24], 3);
    fill(&mut world, [-24, 1, -24], [-23, 11, 24], 3);
    fill(&mut world, [23, 1, -24], [24, 11, 24], 3);
    fill(&mut world, [-23, 1, -24], [23, 11, -23], 3);
    fill(&mut world, [-23, 1, 23], [23, 11, 24], 3);
    for ix in 0..4 {
        for iz in 0..4 {
            let (x, z) = (-15 + 10 * ix, -15 + 10 * iz);
            fill(&mut world, [x - 1, 1, z - 1], [x + 1, 11, z + 1], 2);
        }
    }
    for [x, z] in [[-9, -9], [9, -3], [-3, 10], [11, 12], [-12, 6]] {
        fill(&mut world, [x, 1, z], [x + 2, 3, z + 2], 1);
    }
    let mut objects = Vec::new();
    for i in 0..8 {
        for j in 0..8 {
            let height = if (i + j) % 2 == 0 { 5.5 } else { 8.0 };
            objects.push(node(
                (i * 8 + j) as u32 + 1,
                -17.5 + 5.0 * i as f32,
                height,
                -17.5 + 5.0 * j as f32,
            ));
        }
    }
    (world, objects)
}

/// Two parallel 60 m corridors separated by a one-metre wall, each with its own
/// row of lights. Lights in one must not show through the wall into the other.
pub fn corridor() -> (World, Vec<WorldObject>) {
    let mut world = World::empty();
    fill(&mut world, [-32, 0, -9], [32, 9, 13], 0);
    carve(&mut world, [-30, 1, -2], [30, 5, 2]);
    carve(&mut world, [-30, 1, 3], [30, 5, 7]);
    for x in [-15, -3, 9] {
        fill(&mut world, [x, 1, -2], [x + 1, 5, -1], 1);
    }
    fill(&mut world, [-30, 0, -2], [30, 1, 2], 3);
    let mut objects = Vec::new();
    for k in 0..10 {
        objects.push(node(k + 1, -27.0 + 6.0 * k as f32, 4.2, 0.0));
        objects.push(node(k + 11, -24.0 + 6.0 * k as f32, 4.2, 5.0));
    }
    (world, objects)
}

/// A closed 28 m hall under a low roof, lit by three light nodes and nothing
/// else, so that the shadow a person casts under a lamp shows plainly.
pub fn lamphall() -> (World, Vec<WorldObject>) {
    let mut world = World::empty();
    fill(&mut world, [-14, 0, -14], [14, 1, 14], 0);
    fill(&mut world, [-14, 7, -14], [14, 8, 14], 3);
    fill(&mut world, [-14, 1, -14], [-13, 7, 14], 3);
    fill(&mut world, [13, 1, -14], [14, 7, 14], 3);
    fill(&mut world, [-13, 1, -14], [13, 7, -13], 3);
    fill(&mut world, [-13, 1, 13], [13, 7, 14], 3);
    fill(&mut world, [3, 1, -4], [5, 3, -2], 1);
    let objects = vec![
        node(1, -4.0, 4.5, 0.0),
        node(2, 5.0, 4.5, -6.0),
        node(3, 0.0, 4.5, 7.0),
    ];
    (world, objects)
}

/// A shallow tank of water with glass walls and a row of glass panes above it,
/// under a roof and lit from eight nodes: most of the view is transparent
/// surface. `bare` leaves out the water and glass, to time them by difference.
pub fn waterglass(bare: bool) -> (World, Vec<WorldObject>) {
    let mut world = World::empty();
    fill(&mut world, [-30, 0, -30], [30, 1, 30], 6);
    fill(&mut world, [-30, 9, -30], [30, 10, 30], 3);
    let mut volumes = vec![
        volume([-128, 8, -128], [128, 24, 128], VolumeMaterial::Water),
        volume([-129, 8, -128], [-128, 48, 128], VolumeMaterial::Glass),
        volume([128, 8, -128], [129, 48, 128], VolumeMaterial::Glass),
        volume([-128, 8, -129], [128, 48, -128], VolumeMaterial::Glass),
        volume([-128, 8, 128], [128, 48, 129], VolumeMaterial::Glass),
    ];
    for z in [-8, -4, 0, 4, 8] {
        volumes.push(volume(
            [-120, 24, z * 8],
            [120, 56, z * 8 + 1],
            VolumeMaterial::Glass,
        ));
    }
    if bare {
        volumes.clear();
    }
    world
        .replace_volumes(&volumes)
        .expect("synthetic volumes are disjoint and in bounds");
    let mut objects = Vec::new();
    for (i, [x, z]) in [
        [-8.0, -6.0],
        [8.0, -6.0],
        [-8.0, -2.0],
        [8.0, -2.0],
        [-8.0, 2.0],
        [8.0, 2.0],
        [-8.0, 6.0],
        [8.0, 6.0],
    ]
    .into_iter()
    .enumerate()
    {
        objects.push(node(i as u32 + 1, x, 6.5, z));
    }
    (world, objects)
}

/// A closed 30 m hall with pillars, lit only by lights of every kind the
/// renderer supports: colored spot lights, a small colored point light, and a
/// light whose shadows include moving actors.
pub fn lightshow() -> Scene {
    let mut world = World::empty();
    fill(&mut world, [-16, 0, -16], [16, 1, 16], 0);
    fill(&mut world, [-16, 9, -16], [16, 10, 16], 3);
    fill(&mut world, [-16, 1, -16], [-15, 9, 16], 3);
    fill(&mut world, [15, 1, -16], [16, 9, 16], 3);
    fill(&mut world, [-15, 1, -16], [15, 9, -15], 3);
    fill(&mut world, [-15, 1, 15], [15, 9, 16], 3);
    for [x, z] in [[-6, -6], [6, -6], [-6, 6], [6, 6]] {
        fill(&mut world, [x - 1, 1, z - 1], [x + 1, 9, z + 1], 2);
    }
    fill(&mut world, [-3, 1, -1], [3, 3, 1], 1);
    let spot = |id, position: Vec3, toward: Vec3, inner: f32, outer: f32, color: [f32; 3]| {
        let mut light = LightDesc::spot(
            id,
            position,
            (toward - position).normalize(),
            inner.to_radians(),
            outer.to_radians(),
        );
        light.color = Vec3::from_array(color);
        light.intensity = 160.0;
        light.range = 24.0;
        light
    };
    let mut green = LightDesc::point(1004, Vec3::new(0.0, 1.6, 0.0));
    green.color = Vec3::new(0.3, 1.0, 0.4);
    green.intensity = 30.0;
    green.range = 8.0;
    let mut dynamic = LightDesc::point(1005, Vec3::new(3.0, 5.0, 3.0));
    dynamic.shadow = ShadowMode::Dynamic;
    dynamic.intensity = 60.0;
    dynamic.range = 14.0;
    Scene {
        world,
        objects: Vec::new(),
        lights: vec![
            spot(
                1001,
                Vec3::new(-9.0, 8.2, -9.0),
                Vec3::new(-6.0, 1.0, -3.0),
                10.0,
                28.0,
                [0.6, 0.8, 1.0],
            ),
            spot(
                1002,
                Vec3::new(9.0, 8.2, -9.0),
                Vec3::new(0.0, 1.0, 0.0),
                6.0,
                18.0,
                [1.0, 0.3, 0.2],
            ),
            spot(
                1003,
                Vec3::new(0.0, 8.5, 12.0),
                Vec3::new(0.0, 1.0, -2.0),
                14.0,
                34.0,
                [1.0, 0.74, 0.46],
            ),
            green,
            dynamic,
        ],
    }
}

/// One light in a closed hall with pillars, a wall and a platform: shadows
/// that cross the faces of the light's tiles, and floor and walls at grazing
/// angles to it.
pub fn shadowtest() -> Scene {
    let mut world = World::empty();
    fill(&mut world, [-14, 0, -14], [14, 1, 14], 0);
    fill(&mut world, [-14, 13, -14], [14, 14, 14], 3);
    fill(&mut world, [-14, 1, -14], [-13, 13, 14], 3);
    fill(&mut world, [13, 1, -14], [14, 13, 14], 3);
    fill(&mut world, [-13, 1, -14], [13, 13, -13], 3);
    fill(&mut world, [-13, 1, 13], [13, 13, 14], 3);
    for [x, z] in [[5, 3], [-5, 3], [0, -8], [8, -6]] {
        fill(&mut world, [x - 1, 1, z - 1], [x + 1, 13, z + 1], 2);
    }
    fill(&mut world, [-9, 1, -10], [-3, 2, -4], 1);
    fill(&mut world, [-9, 5, -10], [-3, 6, -4], 1);
    fill(&mut world, [3, 1, 8], [11, 4, 9], 6);
    let mut light = LightDesc::point(1001, Vec3::new(0.0, 7.0, 0.0));
    light.intensity = 40.0;
    light.range = 22.0;
    Scene {
        world,
        objects: Vec::new(),
        lights: vec![light],
    }
}

pub fn named(name: &str) -> Option<Scene> {
    match name {
        "lightroom" => Some(lightroom().into()),
        "corridor" => Some(corridor().into()),
        "lamphall" => Some(lamphall().into()),
        "waterglass" => Some(waterglass(false).into()),
        "waterglass-bare" => Some(waterglass(true).into()),
        "lightshow" => Some(lightshow()),
        "shadowtest" => Some(shadowtest()),
        _ => None,
    }
}
