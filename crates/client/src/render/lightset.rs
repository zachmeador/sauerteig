//! Per-frame light selection: which lights are drawn, with which shadow tiles,
//! and which tiles render this frame. Pure CPU bookkeeping over [`ShadowAtlas`].
use super::{
    atlas::{Request, ShadowAtlas, TileClass, TileRef},
    clusters::{cull_radius, ClusterGrid},
    culling::{Bounds, Frustum},
};
#[cfg(test)]
use crate::lighting::DEFAULT_INTENSITY;
use crate::lighting::{light_from_object, GpuLight, LightDesc, ShadowMode, NO_TILE, OVERLAY_NONE};
use glam::{Mat4, Vec3};
use sauerteig_core::{ObjectKind, WorldObject};
use std::collections::HashMap;

/// Lights fade in and out over this long.
pub const FADE_SECONDS: f32 = 0.25;
/// A light changed within this long ranks first for shadow updates.
const EDIT_PRIORITY_SECONDS: f64 = 1.0;
/// An eye jump beyond this many metres in one frame is a teleport.
const TELEPORT_DISTANCE: f32 = 24.0;
/// Frames of doubled update budget after a teleport, join or world load.
const BOOST_FRAMES: u32 = 3;
/// The most tile faces rendered in a frame: six faces, doubled after a join.
pub const MAX_TILE_JOBS: usize = 12;
/// Moving actors draw into this many overlay tiles at most.
pub const OVERLAY_SLOTS: usize = 15;
pub const OVERLAY_CLASS: TileClass = TileClass::Medium;
/// How many of the most important shadowed lights also shadow moving actors.
pub const ACTOR_SHADOW_LIGHTS: usize = 8;
/// A light that shadows actors keeps doing so until it ranks this many places
/// past the limit, so lights near it do not swap back and forth.
const ACTOR_SHADOW_MARGIN: usize = 2;
/// How far a prop, tree or pad casts a shadow, for invalidating tiles.
const OBJECT_REACH: f32 = 10.0;
/// More object changes than this at once invalidate everything.
const MAX_OBJECT_EDITS: usize = 64;

#[derive(Clone, Copy, Debug)]
pub struct LightConfig {
    pub grid: ClusterGrid,
    pub max_lights: usize,
    /// Lights evaluated per opaque fragment; water and glass evaluate fewer.
    pub light_cap: u32,
    pub pages: usize,
    pub faces_per_frame: usize,
    pub max_tile_class: TileClass,
    pub max_shadowed: usize,
    /// Shadowed lights, most important first, that also shadow actors.
    pub actor_lights: usize,
}

impl Default for LightConfig {
    fn default() -> Self {
        Self {
            grid: ClusterGrid::CLUSTERED,
            max_lights: 256,
            light_cap: 16,
            pages: 14,
            faces_per_frame: 6,
            max_tile_class: TileClass::Large,
            max_shadowed: usize::MAX,
            actor_lights: ACTOR_SHADOW_LIGHTS,
        }
    }
}

/// What happens to a light that needs shadows but cannot get a tile.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum OverBudget {
    /// It fades out rather than leak light through walls.
    #[default]
    FadeOut,
    /// It stays, unshadowed. For comparison captures only.
    Unshadowed,
}

struct Tracked {
    desc: LightDesc,
    fade: f32,
    seen: u64,
    edited_at: f64,
    /// The light was among the most important shadowed lights last frame, so
    /// moving actors shadow it too.
    actor_shadow: bool,
}

#[derive(Clone, Copy)]
pub struct View<'a> {
    pub eye: Vec3,
    pub frustum: &'a Frustum,
    pub view: Mat4,
    /// Pixels spanned by one radian at the screen centre: half the scene
    /// height over the tangent of half the vertical field of view.
    pub pixels_per_radian: f32,
}

pub struct FrameInput<'a> {
    pub objects: &'a [WorldObject],
    pub extra: &'a [LightDesc],
    pub view: View<'a>,
    pub dt: f32,
    /// Vertices of moving shadow casters; only read for dynamic lights.
    pub actors: &'a [Vec3],
    /// The most shadow faces the GPU can render this frame within its time
    /// budget, from measured cost; at least one face always renders.
    pub faces_limit: usize,
}

pub struct PlannedLight {
    #[cfg(test)]
    pub id: u32,
    pub gpu: GpuLight,
    /// View-space centre and inflated radius, for cluster culling.
    pub view: [f32; 4],
}

#[derive(Clone, Copy, Debug)]
pub struct TileJob {
    pub tile: TileRef,
    pub class: TileClass,
    pub matrix: Mat4,
    pub center: Vec3,
    pub range: f32,
    #[cfg(test)]
    pub light: u32,
    #[cfg(test)]
    pub face: usize,
}

#[derive(Clone, Copy, Debug)]
pub struct OverlayJob {
    pub slot: u8,
    pub matrix: Mat4,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, serde::Serialize)]
pub struct LightingStats {
    /// Light nodes in the world, and those inside the view.
    pub world_lights: usize,
    pub visible: usize,
    /// Lights uploaded this frame, including ones still fading in or out.
    pub drawn: usize,
    pub shadowed: usize,
    /// Shadowed lights that also shadow moving actors.
    pub actor_shadowed: usize,
    /// Lights waiting for a first render, and lights with no tile to hold one.
    pub waiting: usize,
    pub over_budget: usize,
    /// Shadow faces sampled, rendered this frame, and held by the atlas.
    pub tiles_used: usize,
    pub tiles_rendered: usize,
    pub tiles_resident: usize,
    pub pages: usize,
    pub overlay_tiles: usize,
    pub budget: usize,
}

pub struct FramePlan {
    pub lights: Vec<PlannedLight>,
    pub jobs: Vec<TileJob>,
    pub overlay: Vec<OverlayJob>,
    /// Two bits per atlas page, then the overlay page, giving each tile size.
    pub page_classes: u32,
    pub stats: LightingStats,
    /// Nothing is waiting to render or fade.
    pub settled: bool,
}

#[derive(Default)]
struct ObjectDigest {
    hash: u64,
    casters: HashMap<u32, (ObjectKind, Vec3, f32)>,
}

pub struct LightSet {
    config: LightConfig,
    over_budget: OverBudget,
    /// The shadow mode of lights placed by light nodes.
    node_shadow: ShadowMode,
    atlas: ShadowAtlas,
    tracked: HashMap<u32, Tracked>,
    objects: ObjectDigest,
    frame: u64,
    now: f64,
    boost: u32,
    last_eye: Option<Vec3>,
    descs: Vec<LightDesc>,
}

impl LightSet {
    pub fn new(config: LightConfig, over_budget: OverBudget, node_shadow: ShadowMode) -> Self {
        Self {
            config,
            over_budget,
            node_shadow,
            atlas: ShadowAtlas::new(config.pages),
            tracked: HashMap::new(),
            objects: ObjectDigest::default(),
            frame: 0,
            now: 0.0,
            boost: BOOST_FRAMES,
            last_eye: None,
            descs: Vec::new(),
        }
    }

    /// Whether any light may shadow moving actors, so their vertices are needed.
    pub fn shadows_actors(&self) -> bool {
        self.tracked.values().any(|t| {
            t.desc.shadow == ShadowMode::Dynamic
                || (self.config.actor_lights > 0 && t.desc.needs_shadow())
        })
    }

    /// Everything the tiles drew has changed.
    pub fn invalidate_all(&mut self) {
        self.atlas.invalidate_all();
    }

    /// Geometry changed inside `bounds`.
    pub fn invalidate_bounds(&mut self, bounds: Bounds) {
        self.atlas.invalidate_bounds(bounds);
    }

    pub fn plan(&mut self, input: &FrameInput) -> FramePlan {
        self.frame += 1;
        self.now += f64::from(input.dt);
        let view = &input.view;
        let node_shadow = self.node_shadow;
        self.descs.clear();
        self.descs.extend(
            input
                .objects
                .iter()
                .filter_map(light_from_object)
                .map(|light| LightDesc {
                    shadow: node_shadow,
                    ..light
                })
                .chain(input.extra.iter().filter_map(|l| l.sanitized())),
        );
        self.sync(input.objects);
        if self
            .last_eye
            .is_none_or(|last| last.distance(view.eye) > TELEPORT_DISTANCE)
        {
            self.boost = BOOST_FRAMES;
        }
        self.last_eye = Some(view.eye);

        let mut stats = LightingStats {
            world_lights: self.tracked.len(),
            ..Default::default()
        };
        let (visible, shadow_order) = self.select(view, &mut stats);
        self.serve_shadows(&shadow_order, view);

        let wanted = self.config.faces_per_frame * if self.boost > 0 { 2 } else { 1 };
        let budget = wanted.min(input.faces_limit.max(1));
        self.boost = self.boost.saturating_sub(1);
        let order: Vec<u32> = shadow_order.iter().map(|&(id, _)| id).collect();
        let faces = self.atlas.schedule(&order, budget);
        self.atlas.swap_finished();
        stats.budget = budget;
        stats.tiles_rendered = faces.len();
        let mut settled = faces.is_empty() && !self.atlas.dirty(order.iter().copied());

        let (lights, overlay) = self.records(&visible, input, &mut stats, &mut settled);
        let jobs = faces
            .iter()
            .map(|face| {
                let desc = &self.tracked[&face.light].desc;
                TileJob {
                    tile: face.tile,
                    class: face.class,
                    matrix: desc.tile_projection(face.face),
                    center: desc.position,
                    range: desc.range,
                    #[cfg(test)]
                    light: face.light,
                    #[cfg(test)]
                    face: face.face,
                }
            })
            .collect();
        let atlas = self.atlas.stats();
        stats.tiles_resident = atlas.tiles;
        stats.pages = atlas.pages;
        FramePlan {
            lights,
            jobs,
            overlay,
            page_classes: self.page_classes(),
            stats,
            settled,
        }
    }

    /// Give each light that wants shadows tiles of a size that fits the atlas,
    /// most important first.
    fn serve_shadows(&mut self, order: &[(u32, LightDesc)], view: &View) {
        let candidates: Vec<_> = order
            .iter()
            .map(|&(id, desc)| (id, desc.position, desc.range))
            .collect();
        self.atlas.candidates(self.frame, &candidates);
        let wanted: Vec<TileClass> = order
            .iter()
            .map(|&(id, desc)| {
                TileClass::for_radius(
                    desc.projected_radius(view.eye, view.pixels_per_radian),
                    self.atlas.class_of(id),
                )
                .min(self.config.max_tile_class)
            })
            .collect();
        let faces: Vec<usize> = order.iter().map(|(_, d)| d.face_count()).collect();
        let granted = grant_classes(&faces, &wanted, self.config.pages);
        // Lights past the atlas's capacity are not served this frame.
        for (&(id, desc), &class) in order.iter().zip(&granted) {
            self.atlas.ensure(&Request {
                id,
                faces: desc.face_count(),
                class,
                frame: self.frame,
                now: self.now,
            });
        }
    }

    /// The records of the lights to draw, in importance order, with fades
    /// advanced, and the overlay tiles their dynamic shadows need.
    fn records(
        &mut self,
        visible: &[u32],
        input: &FrameInput,
        stats: &mut LightingStats,
        settled: &mut bool,
    ) -> (Vec<PlannedLight>, Vec<OverlayJob>) {
        let mut lights = Vec::with_capacity(visible.len());
        let mut overlay = Vec::new();
        let step = input.dt / FADE_SECONDS;
        let actor_limit = self.config.actor_lights;
        let keep_limit = match actor_limit {
            0 => 0,
            n => n + ACTOR_SHADOW_MARGIN,
        };
        let mut shadow_rank = 0;
        for &id in visible {
            let tracked = self.tracked.get_mut(&id).unwrap();
            let mut desc = tracked.desc;
            let tiles = desc.needs_shadow().then(|| self.atlas.tiles(id)).flatten();
            let usable = tiles.is_some() && self.atlas.ready(id);
            let unshadowed = self.over_budget == OverBudget::Unshadowed && tiles.is_none();
            let wanted = if !desc.needs_shadow() || usable || unshadowed {
                1.0
            } else {
                0.0
            };
            tracked.fade = if wanted > tracked.fade {
                (tracked.fade + step).min(wanted)
            } else {
                (tracked.fade - step).max(wanted)
            };
            *settled &= tracked.fade == wanted;
            if desc.needs_shadow() && !usable {
                if tiles.is_none() {
                    stats.over_budget += 1;
                } else {
                    stats.waiting += 1;
                }
            }
            if tracked.fade <= 0.0 {
                continue;
            }
            // A light that lost its tiles while fading out is drawn unshadowed:
            // its old slots may already belong to another light.
            let refs = usable.then(|| {
                let mut packed = [NO_TILE; 6];
                for (slot, tile) in packed.iter_mut().zip(&tiles.as_ref().unwrap().1) {
                    *slot = tile.packed();
                }
                packed
            });
            let mut slots = None;
            if refs.is_some() {
                stats.shadowed += 1;
                stats.tiles_used += desc.face_count();
                if desc.shadow == ShadowMode::Static {
                    let limit = if tracked.actor_shadow {
                        keep_limit
                    } else {
                        actor_limit
                    };
                    tracked.actor_shadow = shadow_rank < limit;
                    if tracked.actor_shadow {
                        desc.shadow = ShadowMode::Dynamic;
                    }
                }
                shadow_rank += 1;
                if desc.shadow == ShadowMode::Dynamic {
                    stats.actor_shadowed += 1;
                    slots = assign_overlay(&desc, input.actors, &mut overlay);
                }
            } else {
                tracked.actor_shadow = false;
            }
            let center = input.view.view.transform_point3(desc.position);
            lights.push(PlannedLight {
                #[cfg(test)]
                id,
                gpu: desc.packed(tracked.fade, refs, slots),
                view: [center.x, center.y, center.z, cull_radius(desc.range)],
            });
        }
        stats.drawn = lights.len();
        stats.overlay_tiles = overlay.len();
        (lights, overlay)
    }

    fn page_classes(&self) -> u32 {
        let mut classes = OVERLAY_CLASS.code() << (2 * self.config.pages);
        for page in 0..self.config.pages {
            let code = self.atlas.pool().class(page).map_or(0, TileClass::code);
            classes |= code << (2 * page);
        }
        classes
    }

    /// Bring tracking in line with this frame's lights and objects.
    fn sync(&mut self, objects: &[WorldObject]) {
        let frame = self.frame;
        let was_empty = self.tracked.is_empty();
        for desc in &self.descs {
            match self.tracked.get_mut(&desc.id) {
                Some(tracked) => {
                    if tracked.desc != *desc {
                        if tracked.desc.face_count() == desc.face_count() {
                            self.atlas.light_changed(desc.id, desc.position, desc.range);
                        } else {
                            // A point light became a spot light or back: its
                            // tiles are the wrong number, so start afresh.
                            self.atlas.release(desc.id);
                        }
                        tracked.desc = *desc;
                        tracked.edited_at = self.now;
                    }
                    tracked.seen = frame;
                }
                None => {
                    self.tracked.insert(
                        desc.id,
                        Tracked {
                            desc: *desc,
                            fade: 0.0,
                            seen: frame,
                            edited_at: f64::NEG_INFINITY,
                            actor_shadow: false,
                        },
                    );
                }
            }
        }
        self.tracked.retain(|_, t| t.seen == frame);
        let tracked = &self.tracked;
        self.atlas.forget_absent(|id| tracked.contains_key(&id));
        if was_empty && !self.tracked.is_empty() {
            self.boost = BOOST_FRAMES;
        }
        self.sync_objects(objects);
    }

    /// Props, trees and pads cast into cached tiles: an object that appears,
    /// moves or disappears invalidates the lights around it.
    fn sync_objects(&mut self, objects: &[WorldObject]) {
        let casters = || objects.iter().filter(|o| o.kind != ObjectKind::Light);
        let mut hash = 0xcbf2_9ce4_8422_2325_u64;
        for o in casters() {
            let words = [
                u64::from(o.id),
                o.kind as u64,
                u64::from(o.position.x.to_bits()),
                u64::from(o.position.y.to_bits()),
                u64::from(o.position.z.to_bits()),
                u64::from(o.rotation.to_bits()),
            ];
            for word in words {
                hash = (hash ^ word).wrapping_mul(0x0000_0100_0000_01b3);
            }
        }
        if hash == self.objects.hash {
            return;
        }
        self.objects.hash = hash;
        let mut dirty = Vec::new();
        let mut next = HashMap::new();
        for o in casters() {
            let now = (o.kind, o.position, o.rotation);
            match self.objects.casters.get(&o.id) {
                Some(&before) if before == now => {}
                Some(&(_, position, _)) => dirty.extend([position, o.position]),
                None => dirty.push(o.position),
            }
            next.insert(o.id, now);
        }
        dirty.extend(
            self.objects
                .casters
                .iter()
                .filter(|(id, _)| !next.contains_key(id))
                .map(|(_, &(_, position, _))| position),
        );
        self.objects.casters = next;
        if dirty.len() > MAX_OBJECT_EDITS {
            self.atlas.invalidate_all();
        } else {
            for position in dirty {
                self.atlas
                    .invalidate_bounds(Bounds::around(position, OBJECT_REACH));
            }
        }
    }

    /// Visible lights by importance, and the lights that want shadows in the
    /// order they are served: those being edited first.
    fn select(&self, view: &View, stats: &mut LightingStats) -> (Vec<u32>, Vec<(u32, LightDesc)>) {
        let mut visible: Vec<(f32, u32)> = self
            .tracked
            .values()
            .filter(|t| {
                view.frustum
                    .intersects_sphere(t.desc.position, t.desc.range)
            })
            .map(|t| (t.desc.importance(view.eye), t.desc.id))
            .collect();
        stats.visible = visible.len();
        visible.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)));
        visible.truncate(self.config.max_lights);
        let ids: Vec<u32> = visible.iter().map(|&(_, id)| id).collect();
        let mut wanting: Vec<(bool, u32)> = ids
            .iter()
            .filter(|id| self.tracked[id].desc.needs_shadow())
            .map(|&id| {
                let edited = self.now - self.tracked[&id].edited_at < EDIT_PRIORITY_SECONDS;
                (edited, id)
            })
            .collect();
        // A stable sort keeps importance order within each group.
        wanting.sort_by_key(|&(edited, _)| !edited);
        wanting.truncate(self.config.max_shadowed);
        let order = wanting
            .into_iter()
            .map(|(_, id)| (id, self.tracked[&id].desc))
            .collect();
        (ids, order)
    }
}

/// Pages needed to hold this many tiles of each size.
fn pages_needed(tiles: [usize; 3]) -> usize {
    TileClass::ALL
        .iter()
        .map(|&class| tiles[class as usize].div_ceil(class.per_page()))
        .sum()
}

/// The size each light's tiles should have, in importance order, for an atlas
/// of `pages` pages. Every light that fits at the smallest size is covered
/// first; the most important then sharpen toward the size they asked for for
/// as long as the atlas has pages left. Lights past the end get no size.
fn grant_classes(faces: &[usize], wanted: &[TileClass], pages: usize) -> Vec<TileClass> {
    let mut counts = [0; 3];
    let mut granted = Vec::new();
    for &f in faces {
        counts[0] += f;
        if pages_needed(counts) > pages {
            counts[0] -= f;
            break;
        }
        granted.push(TileClass::Small);
    }
    for (i, grant) in granted.iter_mut().enumerate() {
        for &class in TileClass::ALL[1..=wanted[i] as usize].iter().rev() {
            let mut trial = counts;
            trial[0] -= faces[i];
            trial[class as usize] += faces[i];
            if pages_needed(trial) <= pages {
                counts = trial;
                *grant = class;
                break;
            }
        }
    }
    granted
}

/// Give each face of a light that shadows actors, and has some in reach, an
/// overlay tile. Returns the slots to write into the light's record.
fn assign_overlay(
    desc: &LightDesc,
    actors: &[Vec3],
    jobs: &mut Vec<OverlayJob>,
) -> Option<[u8; 6]> {
    let mut slots = [OVERLAY_NONE; 6];
    for (face, slot) in slots.iter_mut().enumerate().take(desc.face_count()) {
        if jobs.len() < OVERLAY_SLOTS && FaceReach::new(desc, face).touches_any(actors) {
            *slot = jobs.len() as u8;
            jobs.push(OverlayJob {
                slot: *slot,
                matrix: desc.tile_projection(face),
            });
        }
    }
    slots.iter().any(|&s| s != OVERLAY_NONE).then_some(slots)
}

/// What a face of a light's tiles can shadow: its guarded frustum, widened so
/// that an actor straddling an edge is never missed.
struct FaceReach {
    origin: Vec3,
    forward: Vec3,
    right: Vec3,
    up: Vec3,
    slope: f32,
    range_squared: f32,
}

impl FaceReach {
    fn new(desc: &LightDesc, face: usize) -> Self {
        let (forward, up) = desc.tile_axes(face);
        let right = forward.cross(up).normalize();
        Self {
            origin: desc.position,
            forward,
            right,
            up: right.cross(forward),
            slope: desc.tile_tan_half() * 1.25,
            range_squared: (desc.range + 1.0).powi(2),
        }
    }

    fn touches_any(&self, points: &[Vec3]) -> bool {
        points.iter().any(|&point| {
            let to_point = point - self.origin;
            if to_point.length_squared() > self.range_squared {
                return false;
            }
            let depth = to_point.dot(self.forward);
            let reach = self.slope * depth.max(0.0) + 0.5;
            depth > -0.5
                && to_point.dot(self.right).abs() <= reach
                && to_point.dot(self.up).abs() <= reach
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sauerteig_core::ObjectKind;

    struct Scene {
        set: LightSet,
        objects: Vec<WorldObject>,
        extra: Vec<LightDesc>,
        eye: Vec3,
        actors: Vec<Vec3>,
        faces_limit: usize,
    }

    fn node(id: u32, position: Vec3) -> WorldObject {
        WorldObject {
            id,
            position,
            kind: ObjectKind::Light,
            rotation: 0.0,
        }
    }

    impl Scene {
        fn new(positions: &[Vec3]) -> Self {
            Self {
                set: LightSet::new(
                    LightConfig::default(),
                    OverBudget::FadeOut,
                    ShadowMode::Static,
                ),
                objects: positions
                    .iter()
                    .enumerate()
                    .map(|(i, &p)| node(i as u32 + 1, p))
                    .collect(),
                extra: Vec::new(),
                eye: Vec3::new(0.0, 2.0, 0.0),
                actors: Vec::new(),
                faces_limit: usize::MAX,
            }
        }

        fn frame(&mut self) -> FramePlan {
            self.frame_dt(1.0 / 60.0)
        }

        fn frame_dt(&mut self, dt: f32) -> FramePlan {
            let view = Mat4::look_to_rh(self.eye, -Vec3::Z, Vec3::Y);
            let projection = Mat4::perspective_rh(1.4, 16.0 / 9.0, 0.06, 180.0);
            let frustum = Frustum::new(projection * view);
            self.set.plan(&FrameInput {
                objects: &self.objects,
                extra: &self.extra,
                view: View {
                    eye: self.eye,
                    frustum: &frustum,
                    view,
                    pixels_per_radian: 700.0,
                },
                dt,
                actors: &self.actors,
                faces_limit: self.faces_limit,
            })
        }

        /// Run until nothing waits; returns the frames taken.
        fn settle(&mut self) -> usize {
            for frame in 1..=600 {
                if self.frame().settled {
                    return frame;
                }
            }
            panic!("never settled");
        }
    }

    /// Lights down the view axis, the nearest 6 m ahead.
    fn line(count: usize, spacing: f32) -> Vec<Vec3> {
        (0..count)
            .map(|i| Vec3::new(0.0, 2.0, -6.0 - i as f32 * spacing))
            .collect()
    }

    #[test]
    fn lights_are_listed_by_importance_up_to_the_cap() {
        let mut scene = Scene::new(&[]);
        for i in 0..300 {
            let mut light = LightDesc::point(i + 1, Vec3::new(0.0, 2.0, -3.0 - i as f32 * 0.5));
            light.shadow = ShadowMode::Off;
            scene.extra.push(light);
        }
        scene.frame();
        let plan = scene.frame_dt(1.0);
        assert_eq!(plan.stats.world_lights, 300);
        assert_eq!(plan.lights.len(), 256);
        let ids: Vec<u32> = plan.lights.iter().map(|l| l.id).collect();
        assert_eq!(ids, (1..=256).collect::<Vec<u32>>(), "nearest first");
        // Lights behind the camera and out of reach are not listed.
        scene.extra = vec![
            LightDesc::point(1, Vec3::new(0.0, 0.0, 100.0)),
            LightDesc::point(2, Vec3::new(0.0, 0.0, -300.0)),
        ];
        assert!(scene.frame().lights.is_empty());
    }

    #[test]
    fn sizes_are_granted_by_importance_without_starving_coverage() {
        use TileClass::*;
        let granted = grant_classes(&[6; 12], &[Large; 12], 6);
        assert_eq!(granted[..3], [Large; 3]);
        assert_eq!(granted[3..], [Small; 9]);
        // Everything fits at the smallest size before anything sharpens.
        let granted = grant_classes(&[6; 100], &[Large; 100], 6);
        assert_eq!(granted.len(), 64);
        assert!(granted.iter().all(|&c| c == Small));
        // A light asking for less gets less.
        let granted = grant_classes(&[6, 6, 6], &[Medium, Small, Large], 6);
        assert_eq!(granted, [Medium, Small, Large]);
        // Spot lights are cheap.
        let granted = grant_classes(&[1; 20], &[Large; 20], 2);
        assert_eq!(granted.len(), 20);
        assert!(granted[0] == Large);
        assert!(pages_needed([20, 0, 0]) == 1 && pages_needed([0, 17, 5]) == 4);
    }

    #[test]
    fn a_light_fades_in_once_its_shadows_have_rendered() {
        // Twelve lights; two fit in the first frames' doubled budget of 12 faces.
        let mut scene = Scene::new(&line(12, 2.0));
        let first = scene.frame();
        assert_eq!(first.jobs.len(), 12);
        assert_eq!(first.lights.len(), 2, "only rendered lights are drawn");
        assert!(first.lights[0].gpu.color_scale[0] > 0.0);
        assert!(
            first.lights[0].gpu.color_scale[0] < DEFAULT_INTENSITY,
            "fading in"
        );
        assert_eq!(first.stats.waiting, 10);
        let mut drawn = first.lights.len();
        for _ in 0..4 {
            let plan = scene.frame();
            assert!(plan.lights.len() >= drawn);
            drawn = plan.lights.len();
        }
        let frames = scene.settle();
        assert!(frames < 80, "{frames}");
        let plan = scene.frame();
        assert_eq!(plan.lights.len(), 12);
        assert!(plan.jobs.is_empty());
        assert!(plan
            .lights
            .iter()
            .all(|l| (l.gpu.color_scale[0] - DEFAULT_INTENSITY).abs() < 1.0e-3));
        assert_eq!(plan.stats.tiles_rendered, 0);
        assert_eq!(plan.stats.shadowed, 12);
        assert_eq!(plan.stats.tiles_used, 72);
        assert_eq!(plan.stats.over_budget, 0);
    }

    #[test]
    fn updates_are_capped_per_frame_and_doubled_after_a_teleport() {
        let mut scene = Scene::new(&line(30, 1.0));
        let counts: Vec<usize> = (0..6).map(|_| scene.frame().jobs.len()).collect();
        assert_eq!(counts, [12, 12, 12, 6, 6, 6]);
        scene.settle();
        assert_eq!(scene.frame().stats.budget, 6);
        // Another group of lights far away, seen after a jump.
        let mut far = line(30, 1.0);
        far.iter_mut().for_each(|p| p.x += 500.0);
        let first_new = scene.objects.len() as u32 + 1;
        scene.objects.extend(
            far.iter()
                .enumerate()
                .map(|(i, &p)| node(first_new + i as u32, p)),
        );
        assert_eq!(scene.frame().stats.budget, 6, "nothing new in view yet");
        scene.eye.x += 500.0;
        let counts: Vec<usize> = (0..5).map(|_| scene.frame().jobs.len()).collect();
        assert_eq!(counts, [12, 12, 12, 6, 6]);
    }

    #[test]
    fn measured_cost_can_lower_the_update_budget_but_never_below_one_face() {
        let mut scene = Scene::new(&line(8, 2.0));
        scene.faces_limit = 3;
        let counts: Vec<usize> = (0..4).map(|_| scene.frame().jobs.len()).collect();
        assert_eq!(counts, [3, 3, 3, 3]);
        scene.faces_limit = 0;
        let plan = scene.frame();
        assert_eq!((plan.jobs.len(), plan.stats.budget), (1, 1));
        // Faces of one light spread over frames: it fades in only when all six
        // have rendered.
        let mut scene = Scene::new(&line(1, 2.0));
        scene.faces_limit = 2;
        let drawn: Vec<usize> = (0..5).map(|_| scene.frame().lights.len()).collect();
        assert_eq!(drawn, [0, 0, 1, 1, 1]);
    }

    #[test]
    fn lights_beyond_the_atlas_fade_out_instead_of_leaking() {
        let limited = LightConfig {
            max_shadowed: 4,
            ..LightConfig::default()
        };
        let mut scene = Scene::new(&line(10, 1.0));
        scene.set = LightSet::new(limited, OverBudget::FadeOut, ShadowMode::Static);
        scene.settle();
        let plan = scene.frame();
        assert_eq!(plan.stats.shadowed, 4);
        assert_eq!(plan.lights.len(), 4);
        assert_eq!(plan.stats.over_budget, 6);
        // For comparison captures they can stay, unshadowed.
        let mut scene = Scene::new(&line(10, 1.0));
        scene.set = LightSet::new(limited, OverBudget::Unshadowed, ShadowMode::Static);
        scene.settle();
        let plan = scene.frame();
        assert_eq!(plan.lights.len(), 10);
        let unshadowed = plan
            .lights
            .iter()
            .filter(|l| l.gpu.packed[0] >> 1 & 3 == 0)
            .count();
        assert_eq!(unshadowed, 6);
    }

    #[test]
    fn only_lights_near_an_edit_rerender_and_stale_tiles_keep_lighting() {
        let positions = [Vec3::new(0.0, 2.0, -8.0), Vec3::new(0.0, 2.0, -60.0)];
        let mut scene = Scene::new(&positions);
        scene.settle();
        assert!(scene.frame().jobs.is_empty());
        scene
            .set
            .invalidate_bounds(Bounds::around(Vec3::new(3.0, 2.0, -8.0), 0.5));
        let plan = scene.frame();
        assert_eq!(plan.jobs.len(), 6);
        assert!(plan.jobs.iter().all(|j| j.light == 1));
        assert_eq!(plan.lights.len(), 2, "both stay lit while one re-renders");
        scene
            .set
            .invalidate_bounds(Bounds::around(Vec3::new(30.0, 2.0, -30.0), 0.5));
        assert!(scene.frame().jobs.is_empty());
        // Everything is stale: the update cap spreads it over frames, and
        // every light keeps its stale shadows meanwhile.
        scene.set.invalidate_all();
        let plan = scene.frame();
        assert_eq!(plan.jobs.len(), 6);
        assert_eq!(plan.stats.tiles_used, 12);
        assert_eq!(scene.frame().jobs.len(), 6);
        assert!(scene.frame().jobs.is_empty());
    }

    #[test]
    fn a_moved_light_renders_first_and_stays_bright() {
        let mut scene = Scene::new(&line(4, 3.0));
        scene.settle();
        scene.objects[1].position.x += 1.0;
        let plan = scene.frame();
        assert_eq!(plan.jobs.len(), 6);
        assert!(plan.jobs.iter().all(|j| j.light == 2));
        let moved = plan.lights.iter().find(|l| l.id == 2).unwrap();
        assert!(
            moved.gpu.color_scale[0] > 0.95 * DEFAULT_INTENSITY,
            "no visible dip"
        );
        assert_eq!(moved.gpu.position_range[0], 1.0);
        // The tiles it rendered use the new position.
        let expected = LightDesc::point(2, scene.objects[1].position);
        assert_eq!(
            plan.jobs[0].matrix,
            expected.tile_projection(plan.jobs[0].face)
        );
        // A second light moved in the same frame waits its turn behind it.
        scene.objects[1].position.x += 1.0;
        scene.objects[2].position.x += 1.0;
        let plan = scene.frame();
        assert_eq!(plan.jobs.len(), 6);
        assert!(plan.jobs.iter().all(|j| j.light == 2));
        let plan = scene.frame();
        assert_eq!(plan.jobs.len(), 6);
        assert!(plan.jobs.iter().all(|j| j.light == 3));
    }

    #[test]
    fn a_light_that_changes_kind_gets_the_right_number_of_tiles() {
        let mut scene = Scene::new(&[]);
        scene
            .extra
            .push(LightDesc::point(1, Vec3::new(0.0, 2.0, -8.0)));
        scene.settle();
        assert_eq!(scene.frame().stats.tiles_resident, 6);
        scene.extra[0] = LightDesc::spot(1, Vec3::new(0.0, 2.0, -8.0), Vec3::Z, 0.2, 0.6);
        scene.settle();
        let plan = scene.frame();
        assert_eq!(plan.stats.tiles_resident, 1);
        assert_eq!(plan.stats.shadowed, 1);
        scene.extra[0] = LightDesc::point(1, Vec3::new(0.0, 2.0, -8.0));
        scene.settle();
        assert_eq!(scene.frame().stats.tiles_resident, 6);
    }

    #[test]
    fn deleted_lights_free_their_tiles_and_edited_objects_invalidate_nearby_tiles() {
        let mut scene = Scene::new(&line(2, 40.0));
        scene.settle();
        assert_eq!(scene.frame().stats.tiles_resident, 12);
        // A tree appears beside light 1 only.
        scene.objects.push(WorldObject {
            id: 99,
            position: Vec3::new(4.0, 0.0, -6.0),
            kind: ObjectKind::Tree,
            rotation: 0.0,
        });
        let plan = scene.frame();
        assert_eq!(plan.jobs.len(), 6);
        assert!(plan.jobs.iter().all(|j| j.light == 1));
        assert!(scene.frame().jobs.is_empty(), "unchanged objects are free");
        scene.objects.retain(|o| o.id != 1);
        let plan = scene.frame();
        assert_eq!(plan.stats.tiles_resident, 6);
        assert_eq!(plan.stats.world_lights, 1);
        // Removing the tree invalidates the lights near where it stood.
        scene.objects.retain(|o| o.id != 99);
        assert!(
            scene.frame().jobs.is_empty(),
            "light 1 is gone, light 2 is far"
        );
    }

    #[test]
    fn walking_through_a_dense_map_stays_within_the_update_budget() {
        // 340 lights scattered through a 130 x 160 x 100 m block, like an
        // imported map, and a player walking through it at 6 m/s.
        let mut state = 0x9e37_79b9_u32;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            state as f32 / u32::MAX as f32
        };
        let positions: Vec<Vec3> = (0..340)
            .map(|_| Vec3::new(next() * 130.0 - 60.0, next() * 100.0, next() * 160.0 - 75.0))
            .collect();
        let mut scene = Scene::new(&positions);
        let mut total = 0;
        let mut worst = 0;
        let mut shadowed = 0;
        let mut drawn = 0;
        for frame in 0..1200 {
            let t = frame as f32 / 60.0;
            scene.eye = Vec3::new(-55.0 + t * 6.0, 20.0, -40.0 + 30.0 * (t * 0.3).sin());
            let plan = scene.frame();
            total += plan.jobs.len();
            worst = worst.max(plan.jobs.len());
            shadowed = plan.stats.shadowed;
            drawn = plan.stats.drawn;
            assert!(plan.jobs.len() <= MAX_TILE_JOBS);
            assert!(plan.stats.tiles_resident <= scene.set.config.pages * 64);
        }
        assert!(
            total < 1200 * 6,
            "{total} faces over 1200 frames, worst {worst}, last frame {drawn} drawn / {shadowed} shadowed"
        );
        assert!(shadowed > 0);
    }

    #[test]
    fn dynamic_lights_give_actors_overlay_tiles_only_where_they_are() {
        let mut scene = Scene::new(&[]);
        scene.set.config.actor_lights = 0;
        let mut light = LightDesc::point(1, Vec3::new(0.0, 3.0, -8.0));
        light.shadow = ShadowMode::Dynamic;
        scene.extra.push(light);
        let mut stat = LightDesc::point(2, Vec3::new(0.0, 3.0, -12.0));
        stat.shadow = ShadowMode::Static;
        scene.extra.push(stat);
        scene.settle();
        // No actors: no overlay.
        assert!(scene.frame().overlay.is_empty());
        // An actor straight below the light and one far beyond its reach.
        scene.actors = vec![Vec3::new(0.0, 1.0, -8.0), Vec3::new(0.0, 1.0, -80.0)];
        let plan = scene.frame();
        assert_eq!(plan.overlay.len(), 1, "only the downward face has an actor");
        let record = plan.lights.iter().find(|l| l.id == 1).unwrap().gpu;
        let slots = record.packed[0] >> 3;
        // Face 3 is -Y; the others hold the empty value.
        assert_eq!(slots >> (4 * 3) & 15, 0);
        assert_eq!(slots & 15, OVERLAY_NONE as u32);
        let other = plan.lights.iter().find(|l| l.id == 2).unwrap().gpu;
        assert_eq!(other.packed[0] >> 1 & 3, 1, "static lights ignore actors");
        // Actors near an edge between faces reach both.
        scene.actors = vec![Vec3::new(2.0, 1.0, -8.0)];
        let plan = scene.frame();
        assert!(plan.overlay.len() >= 2, "{}", plan.overlay.len());
    }

    /// Twelve lights at the same distance whose ranges fall one by one, so a
    /// light's importance rank is its index.
    fn ranked_lights(scene: &mut Scene) {
        for i in 0..12 {
            let mut light = LightDesc::point(i + 1, Vec3::new(i as f32 * 0.5 - 3.0, 3.0, -8.0));
            light.range = 24.0 - i as f32;
            scene.extra.push(light);
        }
        scene.settle();
    }

    fn mode_of(plan: &FramePlan, id: u32) -> u32 {
        plan.lights.iter().find(|l| l.id == id).unwrap().gpu.packed[0] >> 1 & 3
    }

    #[test]
    fn the_most_important_shadowed_lights_shadow_actors() {
        let mut scene = Scene::new(&[]);
        ranked_lights(&mut scene);
        let plan = scene.frame();
        assert_eq!(plan.stats.shadowed, 12);
        assert_eq!(plan.stats.actor_shadowed, ACTOR_SHADOW_LIGHTS);
        for id in 1..=12 {
            let expected = if id as usize <= ACTOR_SHADOW_LIGHTS {
                2
            } else {
                1
            };
            assert_eq!(mode_of(&plan, id), expected, "light {id}");
        }
        // An actor under the most important light gets an overlay tile...
        scene.actors = vec![Vec3::new(-3.0, 1.0, -8.0)];
        let plan = scene.frame();
        assert!(!plan.overlay.is_empty());
        // ...and one that only a less important light reaches gets none.
        scene.extra.clear();
        scene.extra.push({
            let mut far = LightDesc::point(1, Vec3::new(0.0, 3.0, -60.0));
            far.range = 8.0;
            far
        });
        scene.extra.extend((0..8).map(|i| {
            let mut near = LightDesc::point(i + 2, Vec3::new(i as f32, 3.0, -4.0));
            near.range = 40.0;
            near
        }));
        scene.settle();
        scene.actors = vec![Vec3::new(0.0, 1.0, -60.0)];
        let plan = scene.frame();
        assert_eq!(mode_of(&plan, 1), 1);
        assert!(plan.overlay.is_empty());
    }

    #[test]
    fn actor_shadows_stay_until_a_light_falls_past_the_margin() {
        let mut scene = Scene::new(&[]);
        ranked_lights(&mut scene);
        let limit = ACTOR_SHADOW_LIGHTS as u32;
        assert_eq!(mode_of(&scene.frame(), limit + 1), 1);
        // Light 9 outranks every other light, which pushes light 8 to ninth.
        scene.extra[limit as usize].range = 40.0;
        scene.settle();
        let plan = scene.frame();
        assert_eq!(mode_of(&plan, limit + 1), 2);
        // Light 8 keeps its actor shadows for a while.
        assert_eq!(mode_of(&plan, limit), 2);
        assert_eq!(plan.stats.actor_shadowed, ACTOR_SHADOW_LIGHTS + 1);
        // Dimmed to the end of the list, it loses them.
        scene.extra[limit as usize - 1].range = 6.0;
        scene.settle();
        let plan = scene.frame();
        assert_eq!(mode_of(&plan, limit), 1);
        assert_eq!(plan.stats.actor_shadowed, ACTOR_SHADOW_LIGHTS);
    }

    #[test]
    fn nodes_shadow_actors_unless_the_limit_is_zero() {
        let positions: Vec<Vec3> = (0..4)
            .map(|i| Vec3::new(i as f32 * 3.0 - 4.5, 4.0, -6.0))
            .collect();
        let mut scene = Scene::new(&positions);
        scene.settle();
        scene.actors = vec![Vec3::new(-4.5, 1.0, -6.0)];
        assert!(scene.set.shadows_actors());
        let plan = scene.frame();
        assert_eq!(plan.stats.actor_shadowed, 4);
        assert!(!plan.overlay.is_empty());
        scene.set.config.actor_lights = 0;
        scene.set.invalidate_all();
        scene.settle();
        assert!(!scene.set.shadows_actors());
        let plan = scene.frame();
        assert_eq!(plan.stats.actor_shadowed, 0);
        assert!(plan.overlay.is_empty());
    }

    #[test]
    fn many_actors_never_use_more_overlay_tiles_than_exist() {
        let mut scene = Scene::new(&[]);
        for i in 0..8 {
            let mut light = LightDesc::point(i + 1, Vec3::new(i as f32 * 3.0 - 10.0, 5.0, -8.0));
            light.shadow = ShadowMode::Dynamic;
            scene.extra.push(light);
        }
        scene.settle();
        scene.actors = (0..400)
            .map(|i| {
                Vec3::new(
                    (i % 20) as f32 - 10.0,
                    1.0 + (i / 20) as f32 * 0.5,
                    -8.0 + (i % 7) as f32,
                )
            })
            .collect();
        let plan = scene.frame();
        assert_eq!(plan.overlay.len(), OVERLAY_SLOTS);
        let mut slots: Vec<u8> = plan.overlay.iter().map(|job| job.slot).collect();
        slots.sort_unstable();
        slots.dedup();
        assert_eq!(slots.len(), OVERLAY_SLOTS);
    }
}
