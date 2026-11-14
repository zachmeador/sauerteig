//! Per-frame light selection: which lights are drawn, with which shadow tiles,
//! and which tiles render this frame. Pure CPU bookkeeping over [`ShadowAtlas`].
use super::{
    atlas::{Request, ShadowAtlas, TileClass, TileRef},
    clusters::{cull_radius, ClusterGrid},
    culling::{Bounds, Frustum},
};
#[cfg(test)]
use crate::lighting::DEFAULT_INTENSITY;
use crate::lighting::{light_from_object, GpuLight, LightDesc, ShadowMode, NO_TILE};
use glam::{Mat4, Vec3};
use sauerteig_core::{ObjectKind, WorldObject};
use std::collections::{HashMap, HashSet};

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
/// The most lights in view that are lit: what the cluster masks and the light
/// buffer fit in a 128 MiB storage binding, the smallest every adapter
/// guarantees. No scene comes near it.
pub const MAX_LIGHTS_IN_VIEW: usize = 262_144;
/// How far a prop, tree or pad casts a shadow, for invalidating tiles.
pub const OBJECT_REACH: f32 = 10.0;
/// More object changes than this at once invalidate everything.
pub const MAX_OBJECT_EDITS: usize = 64;
/// Pages left unplanned in the static atlas, so that pages typed for one tile
/// size, tiles being replaced and rounding never push it past its bound.
const GRANT_SLACK_PAGES: usize = 2;
/// Moving actors are found in cells this many metres across when asking which
/// lights reach one.
const ACTOR_CELL: f32 = 1.0;

#[derive(Clone, Copy, Debug)]
pub struct LightConfig {
    pub grid: ClusterGrid,
    /// The most lights a fragment shades. Nothing sets it in the game: it is
    /// `u32::MAX`, and a comparison capture can lower it.
    pub light_cap: u32,
    /// The atlas's memory in 1024² pages of 4 MiB, static tiles and per-frame
    /// tiles together. A level's shadow detail sets it.
    pub bound_pages: usize,
    /// The most pages the device has layers for.
    pub ceiling_pages: usize,
    pub faces_per_frame: usize,
    pub max_tile_class: TileClass,
    pub max_shadowed: usize,
}

/// Pages of the 15 at the lowest level that per-frame tiles may use before
/// the atlas counts as past its bound: an eighth of the pages, at least one.
pub fn frame_reserve(bound_pages: usize) -> usize {
    (bound_pages / 8).max(1)
}

impl LightConfig {
    /// Pages the static tiles may take.
    pub fn static_bound(&self) -> usize {
        self.bound_pages
            .saturating_sub(frame_reserve(self.bound_pages))
            .max(1)
    }

    /// Pages the planner shares out among the lights in view.
    fn grant_budget(&self) -> usize {
        self.static_bound().saturating_sub(GRANT_SLACK_PAGES).max(1)
    }
}

impl Default for LightConfig {
    fn default() -> Self {
        Self {
            grid: ClusterGrid::CLUSTERED,
            light_cap: u32::MAX,
            bound_pages: 15,
            ceiling_pages: 255,
            faces_per_frame: 6,
            max_tile_class: TileClass::Large,
            max_shadowed: usize::MAX,
        }
    }
}

struct Tracked {
    desc: LightDesc,
    fade: f32,
    seen: u64,
    edited_at: f64,
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

/// A per-frame tile: the actors that a light's face can see, drawn afresh each
/// frame into a texture of their own, so that no pass samples what it renders
/// into.
#[derive(Clone, Copy, Debug)]
pub struct FrameJob {
    /// The tile in the per-frame texture, and the static tile it belongs to.
    pub tile: TileRef,
    pub source: TileRef,
    pub class: TileClass,
    pub matrix: Mat4,
    #[cfg(test)]
    pub light: u32,
    #[cfg(test)]
    pub face: usize,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, serde::Serialize)]
pub struct LightingStats {
    /// Light nodes in the world, and those inside the view.
    pub world_lights: usize,
    pub visible: usize,
    /// Lights uploaded this frame, including ones still fading in or out.
    pub drawn: usize,
    pub shadowed: usize,
    /// Shadowed lights with a per-frame tile on some face, because an actor is
    /// in reach of it.
    pub actor_shadowed: usize,
    /// Lights waiting for a first render, and lights the device has no page
    /// left for, which stay dark rather than leak light through walls.
    pub waiting: usize,
    pub over_budget: usize,
    /// Shadow faces sampled, rendered this frame, and held by the atlas.
    pub tiles_used: usize,
    pub tiles_rendered: usize,
    pub tiles_resident: usize,
    /// Pages of the static atlas holding tiles, and pages it has made.
    pub pages: usize,
    pub pages_made: usize,
    /// Per-frame tiles this frame, and the pages of their texture.
    pub frame_tiles: usize,
    pub frame_pages: usize,
    /// The level's bound on pages, static and per-frame together.
    pub bound_pages: usize,
    pub budget: usize,
}

impl LightingStats {
    /// Atlas pages made past the level's bound, for lights that would
    /// otherwise have gone without a shadow.
    pub fn pages_past_bound(&self) -> usize {
        (self.pages_made + self.frame_pages).saturating_sub(self.bound_pages)
    }
}

pub struct FramePlan {
    pub lights: Vec<PlannedLight>,
    pub jobs: Vec<TileJob>,
    pub frame_jobs: Vec<FrameJob>,
    /// The tile size code of each page of the static atlas and of the
    /// per-frame texture, for the page table.
    pub page_classes: Vec<u32>,
    pub frame_page_classes: Vec<u32>,
    pub stats: LightingStats,
    /// Nothing is waiting to render or fade.
    pub settled: bool,
}

/// What the objects that cast shadows looked like last time, to find which
/// ones appeared, moved or went.
#[derive(Default)]
pub struct ObjectDigest {
    hash: u64,
    casters: HashMap<u32, (ObjectKind, Vec3, f32)>,
}

impl ObjectDigest {
    /// The positions, old and new, of every shadow-casting object that
    /// changed since the last call, or `None` when nothing did.
    pub fn update(&mut self, objects: &[WorldObject]) -> Option<Vec<Vec3>> {
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
        if hash == self.hash {
            return None;
        }
        self.hash = hash;
        let mut dirty = Vec::new();
        let mut next = HashMap::new();
        for o in casters() {
            let now = (o.kind, o.position, o.rotation);
            match self.casters.get(&o.id) {
                Some(&before) if before == now => {}
                Some(&(_, position, _)) => dirty.extend([position, o.position]),
                None => dirty.push(o.position),
            }
            next.insert(o.id, now);
        }
        dirty.extend(
            self.casters
                .iter()
                .filter(|(id, _)| !next.contains_key(id))
                .map(|(_, &(_, position, _))| position),
        );
        self.casters = next;
        Some(dirty)
    }
}

pub struct LightSet {
    config: LightConfig,
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
    pub fn new(config: LightConfig, node_shadow: ShadowMode) -> Self {
        Self {
            config,
            node_shadow,
            atlas: ShadowAtlas::new(config.static_bound(), config.ceiling_pages),
            tracked: HashMap::new(),
            objects: ObjectDigest::default(),
            frame: 0,
            now: 0.0,
            boost: BOOST_FRAMES,
            last_eye: None,
            descs: Vec::new(),
        }
    }

    /// The largest tile any light's shadow may use: a quality choice that
    /// changes detail, never which lights are shadowed.
    pub fn set_max_tile_class(&mut self, class: TileClass) {
        self.config.max_tile_class = class;
    }

    /// The atlas's memory bound in pages, which a quality level sets. Tiles
    /// shrink to fit a lower bound; no light loses its shadow.
    pub fn set_bound_pages(&mut self, pages: usize) {
        self.config.bound_pages = pages;
        self.atlas.set_bound(self.config.static_bound());
    }

    /// Whether any light may shadow moving actors, so their vertices are needed.
    pub fn shadows_actors(&self) -> bool {
        self.tracked
            .values()
            .any(|t| t.desc.shadow == ShadowMode::Dynamic && t.desc.needs_shadow())
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
        self.atlas.set_bound(self.config.static_bound());
        let (visible, shadow_order) = self.select(view, &mut stats);
        self.serve_shadows(&shadow_order, view);
        self.atlas.compact(self.now);

        let wanted = self.config.faces_per_frame * if self.boost > 0 { 2 } else { 1 };
        let budget = wanted.min(input.faces_limit.max(1));
        self.boost = self.boost.saturating_sub(1);
        let order: Vec<u32> = shadow_order.iter().map(|&(id, _)| id).collect();
        let faces = self.atlas.schedule(&order, budget);
        self.atlas.swap_finished();
        stats.budget = budget;
        stats.tiles_rendered = faces.len();
        let mut settled = faces.is_empty() && !self.atlas.dirty(order.iter().copied());

        let (lights, frame_jobs, frame_pool) =
            self.records(&visible, input, &mut stats, &mut settled);
        let jobs = faces
            .iter()
            .map(|face| {
                let desc = &self.tracked[&face.light].desc;
                TileJob {
                    tile: face.tile,
                    class: face.class,
                    matrix: desc.tile_projection(face.face, face.class.size()),
                    center: desc.position,
                    range: desc.range,
                    #[cfg(test)]
                    light: face.light,
                    #[cfg(test)]
                    face: face.face,
                }
            })
            .collect();
        self.atlas.trim();
        let atlas = self.atlas.stats();
        stats.tiles_resident = atlas.tiles;
        stats.pages = atlas.pages;
        stats.pages_made = atlas.created;
        stats.frame_tiles = frame_jobs.len();
        stats.frame_pages = frame_pool.classes.len();
        stats.bound_pages = self.config.bound_pages;
        FramePlan {
            lights,
            jobs,
            frame_jobs,
            page_classes: self.page_classes(),
            frame_page_classes: frame_pool.classes.iter().map(|c| c.code()).collect(),
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
        let granted = grant_classes(&faces, &wanted, self.config.grant_budget());
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
    /// advanced, and the per-frame tiles that actors in reach of them need.
    fn records(
        &mut self,
        visible: &[u32],
        input: &FrameInput,
        stats: &mut LightingStats,
        settled: &mut bool,
    ) -> (Vec<PlannedLight>, Vec<FrameJob>, FramePool) {
        let mut lights = Vec::with_capacity(visible.len());
        let mut frame_jobs = Vec::new();
        let mut pool = FramePool::default();
        let cells = actor_cells(input.actors);
        let step = input.dt / FADE_SECONDS;
        for &id in visible {
            let tracked = self.tracked.get_mut(&id).unwrap();
            let desc = tracked.desc;
            let tiles = desc.needs_shadow().then(|| self.atlas.tiles(id)).flatten();
            let usable = tiles.is_some() && self.atlas.ready(id);
            let wanted = if !desc.needs_shadow() || usable {
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
            let mut frame_refs = None;
            if refs.is_some() {
                stats.shadowed += 1;
                stats.tiles_used += desc.face_count();
                if desc.shadow == ShadowMode::Dynamic {
                    let (class, static_tiles) = tiles.as_ref().unwrap();
                    frame_refs = assign_frame_tiles(
                        &desc,
                        *class,
                        static_tiles,
                        &cells,
                        &mut pool,
                        &mut frame_jobs,
                        #[cfg(test)]
                        id,
                    );
                    stats.actor_shadowed += usize::from(frame_refs.is_some());
                }
            }
            let center = input.view.view.transform_point3(desc.position);
            lights.push(PlannedLight {
                #[cfg(test)]
                id,
                gpu: desc.packed(tracked.fade, refs, frame_refs),
                view: [center.x, center.y, center.z, cull_radius(desc.range)],
            });
        }
        stats.drawn = lights.len();
        (lights, frame_jobs, pool)
    }

    fn page_classes(&self) -> Vec<u32> {
        (0..self.atlas.pool().pages())
            .map(|page| self.atlas.pool().class(page).map_or(0, TileClass::code))
            .collect()
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
        let Some(dirty) = self.objects.update(objects) else {
            return;
        };
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
        visible.truncate(MAX_LIGHTS_IN_VIEW);
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
fn pages_needed(tiles: [usize; 5]) -> usize {
    TileClass::ALL
        .iter()
        .map(|&class| tiles[class as usize].div_ceil(class.per_page()))
        .sum()
}

/// The size each light's tiles should have, in importance order, for an atlas
/// of `pages` pages. Every light starts at the largest of 128², 64² and 32² at
/// which all of them fit together. Then, in importance order, each grows to
/// 64² and again to 128² while the pages hold it, and the most important
/// sharpen toward the size they asked for. While the atlas has room, every
/// light gets 128² before any gets more; when it hasn't, the least important
/// are the ones left small, and none is left out.
fn grant_classes(faces: &[usize], wanted: &[TileClass], pages: usize) -> Vec<TileClass> {
    let total: usize = faces.iter().sum();
    let fits = |class: TileClass| {
        let mut counts = [0; 5];
        counts[class as usize] = total;
        pages_needed(counts) <= pages
    };
    let floor = [TileClass::Small, TileClass::Mini]
        .into_iter()
        .find(|&class| fits(class))
        .unwrap_or(TileClass::Tiny);
    let mut granted: Vec<TileClass> = wanted.iter().map(|&w| w.min(floor)).collect();
    let mut counts = [0; 5];
    for (&class, &f) in granted.iter().zip(faces) {
        counts[class as usize] += f;
    }
    for target in [TileClass::Mini, TileClass::Small] {
        for (i, grant) in granted.iter_mut().enumerate() {
            if *grant >= target || wanted[i] < target {
                continue;
            }
            let mut trial = counts;
            trial[*grant as usize] -= faces[i];
            trial[target as usize] += faces[i];
            if pages_needed(trial) <= pages {
                counts = trial;
                *grant = target;
            }
        }
    }
    // The most important sharpen as far as they asked for, as far as the
    // pages go.
    for (i, grant) in granted.iter_mut().enumerate() {
        for target in (TileClass::Medium as usize..=wanted[i] as usize)
            .rev()
            .map(|c| TileClass::ALL[c])
        {
            let mut trial = counts;
            trial[*grant as usize] -= faces[i];
            trial[target as usize] += faces[i];
            if pages_needed(trial) <= pages {
                counts = trial;
                *grant = target;
                break;
            }
        }
    }
    granted
}

/// A frame's per-frame tiles, handed out page by page in the size of each
/// tile's static tile.
#[derive(Default)]
pub struct FramePool {
    pub classes: Vec<TileClass>,
    used: Vec<usize>,
}

impl FramePool {
    fn alloc(&mut self, class: TileClass) -> TileRef {
        let page = (0..self.classes.len())
            .find(|&p| self.classes[p] == class && self.used[p] < class.per_page())
            .unwrap_or_else(|| {
                self.classes.push(class);
                self.used.push(0);
                self.classes.len() - 1
            });
        self.used[page] += 1;
        TileRef {
            page: page as u16,
            slot: (self.used[page] - 1) as u16,
        }
    }
}

/// The centres of the cells, `ACTOR_CELL` metres across, that hold the
/// vertices of moving shadow casters.
fn actor_cells(points: &[Vec3]) -> Vec<Vec3> {
    let cells: HashSet<[i32; 3]> = points
        .iter()
        .map(|p| (*p / ACTOR_CELL).floor().as_ivec3().to_array())
        .collect();
    let mut centres: Vec<Vec3> = cells
        .into_iter()
        .map(|c| (Vec3::from(c.map(|v| v as f32)) + Vec3::splat(0.5)) * ACTOR_CELL)
        .collect();
    // A set has no order; sorted, the same actors give the same plan.
    centres.sort_by(|a, b| a.to_array().partial_cmp(&b.to_array()).unwrap());
    centres
}

/// Half the diagonal of an actor cell: how far a vertex can be from its cell's
/// centre.
const CELL_REACH: f32 = ACTOR_CELL * 0.866_026;

/// Give each face of a light, that has an actor in reach, a per-frame tile the
/// size of its static one. Returns the references to write into the light's
/// record, or `None` when no actor is in reach.
fn assign_frame_tiles(
    desc: &LightDesc,
    class: TileClass,
    static_tiles: &[TileRef],
    cells: &[Vec3],
    pool: &mut FramePool,
    jobs: &mut Vec<FrameJob>,
    #[cfg(test)] light: u32,
) -> Option<[u32; 6]> {
    let reach = desc.range + 1.0 + CELL_REACH;
    if !cells
        .iter()
        .any(|c| c.distance_squared(desc.position) <= reach * reach)
    {
        return None;
    }
    let mut refs = [NO_TILE; 6];
    for (face, &source) in static_tiles.iter().enumerate().take(desc.face_count()) {
        if FaceReach::new(desc, face).touches_any(cells) {
            let tile = pool.alloc(class);
            refs[face] = tile.packed();
            jobs.push(FrameJob {
                tile,
                source,
                class,
                matrix: desc.tile_projection(face, class.size()),
                #[cfg(test)]
                light,
                #[cfg(test)]
                face,
            });
        }
    }
    refs.iter().any(|&r| r != NO_TILE).then_some(refs)
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
            // The smallest tile has the widest guard band.
            slope: desc.tile_tan_half(32) * 1.25,
            range_squared: (desc.range + 1.0 + CELL_REACH).powi(2),
        }
    }

    /// Whether any of the actor cells' centres is in reach, with room for a
    /// vertex anywhere in the cell.
    fn touches_any(&self, centres: &[Vec3]) -> bool {
        centres.iter().any(|&point| {
            let to_point = point - self.origin;
            if to_point.length_squared() > self.range_squared {
                return false;
            }
            let depth = to_point.dot(self.forward);
            let reach = self.slope * depth.max(0.0) + 0.5 + CELL_REACH;
            depth > -0.5 - CELL_REACH
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
                set: LightSet::new(LightConfig::default(), ShadowMode::Dynamic),
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
    fn every_light_in_view_is_listed_by_importance_without_a_cap() {
        let mut scene = Scene::new(&[]);
        for i in 0..1000 {
            let mut light = LightDesc::point(i + 1, Vec3::new(0.0, 2.0, -3.0 - i as f32 * 0.1));
            light.shadow = ShadowMode::Off;
            scene.extra.push(light);
        }
        scene.frame();
        let plan = scene.frame_dt(1.0);
        assert_eq!(plan.stats.world_lights, 1000);
        assert_eq!(plan.lights.len(), 1000);
        let ids: Vec<u32> = plan.lights.iter().map(|l| l.id).collect();
        assert_eq!(ids, (1..=1000).collect::<Vec<u32>>(), "nearest first");
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
        // Room for everything at 128² and a little more: the most important
        // sharpen, in order.
        let granted = grant_classes(&[6; 12], &[Large; 12], 4);
        assert_eq!(granted[0], Large, "{granted:?}");
        assert!(granted[1..].iter().all(|&c| c >= Small), "{granted:?}");
        assert!(pages_needed(counts(&granted, 6)) <= 4);
        let granted = grant_classes(&[6; 12], &[Large; 12], 12);
        assert!(granted[..3].iter().all(|&c| c == Large), "{granted:?}");
        assert!(granted[3..].iter().all(|&c| c >= Small), "{granted:?}");
        // Short of room, nobody is left out: the least important shrink first.
        let granted = grant_classes(&[6; 100], &[Large; 100], 6);
        assert_eq!(granted.len(), 100);
        assert!(pages_needed(counts(&granted, 6)) <= 6);
        assert!(granted[0] > granted[99], "{granted:?}");
        assert_eq!(granted[99], Mini);
        let granted = grant_classes(&[6; 300], &[Large; 300], 6);
        assert!(pages_needed(counts(&granted, 6)) <= 6);
        assert_eq!(granted[299], Tiny);
        assert_eq!(granted[0], Mini);
        // Everything at 64² when that is what fits.
        let granted = grant_classes(&[6; 40], &[Small; 40], 1);
        assert!(granted.iter().all(|&c| c == Mini), "{granted:?}");
        // Past the floor, the lights still get the smallest tile.
        let granted = grant_classes(&[6; 400], &[Small; 400], 1);
        assert!(granted.iter().all(|&c| c == Tiny));
        // A light asking for less gets less.
        let granted = grant_classes(&[6, 6, 6], &[Medium, Small, Large], 6);
        assert_eq!(granted, [Medium, Small, Large]);
        // Spot lights are cheap.
        let granted = grant_classes(&[1; 20], &[Large; 20], 2);
        assert_eq!(granted.len(), 20);
        assert!(granted[0] == Large);
        assert_eq!(pages_needed([1025, 0, 0, 0, 0]), 2);
        assert_eq!(pages_needed([20, 0, 0, 0, 0]), 1);
        assert_eq!(pages_needed([0, 0, 0, 17, 5]), 4);
    }

    fn counts(granted: &[TileClass], faces: usize) -> [usize; 5] {
        let mut counts = [0; 5];
        for class in granted {
            counts[*class as usize] += faces;
        }
        counts
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
    fn no_light_fades_out_for_lack_of_a_tile() {
        // A bound of 3 pages leaves one for the static atlas at the lowest
        // grants: a hundred lights' 600 faces take 3 pages even at 32².
        let tight = LightConfig {
            bound_pages: 3,
            ..LightConfig::default()
        };
        let mut scene = Scene::new(&line(100, 0.4));
        scene.set = LightSet::new(tight, ShadowMode::Dynamic);
        for _ in 0..1200 {
            if scene.frame().settled {
                break;
            }
        }
        let plan = scene.frame();
        assert_eq!(plan.stats.drawn, 100);
        assert_eq!(plan.stats.shadowed, 100);
        assert_eq!(plan.stats.over_budget, 0);
        assert!(plan.stats.tiles_resident >= 600);
        // Tiles shrank rather than lights going dark.
        let classes: std::collections::HashSet<_> = plan.page_classes.iter().copied().collect();
        assert!(classes.contains(&0), "{classes:?}");
    }

    #[test]
    fn the_largest_shadow_tile_never_changes_which_lights_are_lit_or_shadowed() {
        // More lights than the lowest level holds at 128², nearest ahead.
        let positions = line(240, 0.5);
        let drawn = |class: TileClass, change_midway: bool| {
            let mut scene = Scene::new(&positions);
            scene.set.set_max_tile_class(class);
            if change_midway {
                // Start at the other end of the range, then switch.
                scene.set.set_max_tile_class(TileClass::Large);
                scene.settle();
                scene.set.set_max_tile_class(class);
                for _ in 0..180 {
                    scene.frame();
                }
            }
            scene.settle();
            let plan = scene.frame();
            let shadowed: Vec<_> = plan
                .lights
                .iter()
                .map(|l| (l.id, l.gpu.packed[0] >> 1 & 3))
                .collect();
            (plan.stats.shadowed, plan.stats.over_budget, shadowed)
        };
        let reference = drawn(TileClass::Large, false);
        assert!(reference.0 == 240 && reference.1 == 0, "{:?}", reference.0);
        for class in [TileClass::Medium, TileClass::Small] {
            for midway in [false, true] {
                assert_eq!(drawn(class, midway), reference, "{class:?}, {midway}");
            }
        }
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
            expected.tile_projection(plan.jobs[0].face, plan.jobs[0].class.size())
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
            assert_eq!(plan.stats.over_budget, 0);
        }
        assert!(
            total < 1200 * 6,
            "{total} faces over 1200 frames, worst {worst}, last frame {drawn} drawn / {shadowed} shadowed"
        );
        assert!(shadowed > 0);
    }

    fn frame_tiles_of(plan: &FramePlan, id: u32) -> [u32; 6] {
        plan.lights
            .iter()
            .find(|l| l.id == id)
            .unwrap()
            .gpu
            .frame_tiles
    }

    #[test]
    fn actors_get_per_frame_tiles_only_where_they_are() {
        let mut scene = Scene::new(&[]);
        let mut light = LightDesc::point(1, Vec3::new(0.0, 3.0, -8.0));
        light.shadow = ShadowMode::Dynamic;
        scene.extra.push(light);
        let mut stat = LightDesc::point(2, Vec3::new(0.0, 3.0, -12.0));
        stat.shadow = ShadowMode::Static;
        scene.extra.push(stat);
        scene.settle();
        // No actors: no per-frame tiles.
        let plan = scene.frame();
        assert!(plan.frame_jobs.is_empty() && plan.frame_page_classes.is_empty());
        // An actor straight below the light and one far beyond its reach.
        scene.actors = vec![Vec3::new(0.0, 1.0, -8.0), Vec3::new(0.0, 1.0, -80.0)];
        let plan = scene.frame();
        assert_eq!(
            plan.stats.actor_shadowed, 1,
            "the far actor is out of reach"
        );
        let tiles = frame_tiles_of(&plan, 1);
        // Face 3 is -Y, which looks at the actor; the faces facing away from
        // it, +Y and the ones beyond the light, have none.
        assert_ne!(tiles[3], NO_TILE);
        assert_eq!(tiles[2], NO_TILE);
        assert_eq!(tiles[5], NO_TILE);
        assert!(plan.frame_jobs.len() <= 3, "{}", plan.frame_jobs.len());
        let job = plan.frame_jobs.iter().find(|j| j.face == 3).unwrap();
        assert_eq!(job.light, 1);
        assert_eq!(job.tile.packed(), tiles[3]);
        assert_eq!(
            Some(job.source),
            scene.set.atlas.tiles(1).map(|(_, t)| t[3]),
            "the tile starts from its static tile's"
        );
        assert_eq!(plan.frame_page_classes, [job.class.code()]);
        assert_eq!(frame_tiles_of(&plan, 2), [NO_TILE; 6]);
        let other = plan.lights.iter().find(|l| l.id == 2).unwrap().gpu;
        assert_eq!(other.packed[0] >> 1 & 3, 1, "static lights ignore actors");
        // Actors near an edge between faces reach both.
        scene.actors = vec![Vec3::new(2.0, 1.0, -8.0)];
        let plan = scene.frame();
        assert!(plan.frame_jobs.len() >= 2, "{}", plan.frame_jobs.len());
    }

    /// Twelve lights at the same distance whose ranges fall one by one, so a
    /// light's importance rank is its index.
    fn ranked(scene: &mut Scene) {
        for i in 0..12 {
            let mut light = LightDesc::point(i + 1, Vec3::new(i as f32 * 0.5 - 3.0, 3.0, -8.0));
            light.range = 24.0 - i as f32;
            scene.extra.push(light);
        }
        scene.settle();
    }

    #[test]
    fn every_shadowed_light_in_reach_shadows_actors_whatever_its_rank() {
        let mut scene = Scene::new(&[]);
        ranked(&mut scene);
        let plan = scene.frame();
        assert_eq!(plan.stats.shadowed, 12);
        assert_eq!(plan.stats.actor_shadowed, 0, "no actor, no tile");
        // An actor under the middle of them: every light reaches it, the least
        // important as much as the most.
        scene.actors = vec![Vec3::new(-1.0, 1.0, -8.0)];
        let plan = scene.frame();
        assert_eq!(plan.stats.actor_shadowed, 12);
        for id in 1..=12 {
            assert!(
                frame_tiles_of(&plan, id).iter().any(|&t| t != NO_TILE),
                "light {id}"
            );
        }
        // Reshuffling the ranks changes nothing about who shadows the actor.
        scene.extra[11].range = 60.0;
        scene.extra[0].range = 9.0;
        scene.settle();
        let plan = scene.frame();
        assert_eq!(plan.stats.actor_shadowed, 12);
        // One that only a faraway light reaches gets a tile from that light.
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
        assert!(frame_tiles_of(&plan, 1).iter().any(|&t| t != NO_TILE));
    }

    #[test]
    fn nodes_shadow_actors_and_a_static_mode_turns_it_off() {
        let positions: Vec<Vec3> = (0..4)
            .map(|i| Vec3::new(i as f32 * 3.0 - 4.5, 4.0, -6.0))
            .collect();
        let mut scene = Scene::new(&positions);
        scene.settle();
        scene.actors = vec![Vec3::new(-4.5, 1.0, -6.0)];
        assert!(scene.set.shadows_actors());
        let plan = scene.frame();
        assert!(plan.stats.actor_shadowed >= 1);
        assert!(!plan.frame_jobs.is_empty());
        let mut fixed = Scene::new(&positions);
        fixed.set = LightSet::new(LightConfig::default(), ShadowMode::Static);
        fixed.actors = scene.actors.clone();
        fixed.settle();
        assert!(!fixed.set.shadows_actors());
        let plan = fixed.frame();
        assert_eq!(plan.stats.actor_shadowed, 0);
        assert!(plan.frame_jobs.is_empty());
    }

    #[test]
    fn many_actors_get_a_tile_on_every_face_in_reach_with_no_limit() {
        let mut scene = Scene::new(&[]);
        for i in 0..40 {
            let mut light = LightDesc::point(i + 1, Vec3::new(i as f32 * 1.5 - 30.0, 5.0, -8.0));
            light.shadow = ShadowMode::Dynamic;
            scene.extra.push(light);
        }
        for _ in 0..400 {
            if scene.frame().settled {
                break;
            }
        }
        scene.actors = (0..400)
            .map(|i| {
                Vec3::new(
                    (i % 20) as f32 * 3.0 - 30.0,
                    1.0 + (i / 20) as f32 * 0.5,
                    -8.0 + (i % 7) as f32,
                )
            })
            .collect();
        let plan = scene.frame();
        assert_eq!(plan.stats.actor_shadowed, 40);
        assert!(plan.frame_jobs.len() > 40, "{}", plan.frame_jobs.len());
        // Every tile is a distinct slot of a page of its own size.
        let mut seen = std::collections::HashSet::new();
        for job in &plan.frame_jobs {
            assert!(seen.insert((job.tile.page, job.tile.slot)));
            assert!((job.tile.slot as usize) < job.class.per_page());
            assert_eq!(
                plan.frame_page_classes[usize::from(job.tile.page)],
                job.class.code()
            );
        }
        assert_eq!(plan.stats.frame_tiles, plan.frame_jobs.len());
        assert_eq!(plan.stats.frame_pages, plan.frame_page_classes.len());
        // A frame later, with the actors gone, nothing is left over.
        scene.actors.clear();
        let plan = scene.frame();
        assert!(plan.frame_jobs.is_empty() && plan.frame_page_classes.is_empty());
    }

    #[test]
    fn every_tile_reference_names_a_slot_of_a_page_of_the_size_in_the_page_table() {
        let mut scene = Scene::new(&[]);
        for i in 0..150 {
            let (x, z) = ((i % 15) as f32 * 3.0 - 20.0, (i / 15) as f32 * 3.0 - 40.0);
            scene
                .extra
                .push(LightDesc::point(i + 1, Vec3::new(x, 3.0, z)));
        }
        scene.settle();
        scene.actors = vec![Vec3::new(0.0, 1.0, -20.0), Vec3::new(5.0, 1.0, -25.0)];
        let plan = scene.frame();
        assert!(!plan.frame_jobs.is_empty());
        let mut seen = std::collections::HashSet::new();
        for light in &plan.lights {
            let record = light.gpu;
            for (tiles, table) in [
                (record.tiles, &plan.page_classes),
                (record.frame_tiles, &plan.frame_page_classes),
            ] {
                for tile in tiles.into_iter().filter(|&t| t != NO_TILE) {
                    let (page, slot) = ((tile >> 10) as usize, tile & 1023);
                    let class = TileClass::ALL[table[page] as usize];
                    assert!((slot as usize) < class.per_page());
                    // No two faces share a slot of the same texture.
                    assert!(seen.insert((std::ptr::eq(table, &plan.page_classes), tile)));
                }
            }
        }
        assert!(plan.page_classes.len() as u32 <= crate::render::device::MAX_ARRAY_LAYERS);
    }

    #[test]
    fn a_thousand_lights_in_view_are_all_lit_and_shadowed_within_the_bound() {
        let mut scene = Scene::new(&[]);
        for i in 0..1000 {
            let (x, z) = ((i % 40) as f32 * 2.0 - 40.0, (i / 40) as f32 * 2.0 - 60.0);
            scene
                .extra
                .push(LightDesc::point(i + 1, Vec3::new(x, 3.0, z - 4.0)));
        }
        scene.eye = Vec3::new(0.0, 2.0, 20.0);
        let mut plan = scene.frame();
        for _ in 0..3000 {
            if plan.settled {
                break;
            }
            plan = scene.frame();
        }
        assert!(plan.settled);
        assert_eq!(plan.stats.drawn, 1000);
        assert_eq!(plan.stats.shadowed, 1000);
        assert_eq!(plan.stats.over_budget, 0);
        assert_eq!(plan.stats.tiles_resident, 6000);
        // At the lowest level's bound: the static atlas stays inside what is
        // left of 15 pages after the per-frame tiles' reserve.
        assert!(
            plan.stats.pages_made <= scene.set.config.static_bound(),
            "{:?}",
            plan.stats
        );
        assert_eq!(plan.stats.pages_past_bound(), 0, "{:?}", plan.stats);
    }

    #[test]
    fn a_lower_bound_shrinks_tiles_and_gives_pages_back_without_losing_a_light() {
        let mut scene = Scene::new(&[]);
        let big = LightConfig {
            bound_pages: 64,
            ..LightConfig::default()
        };
        scene.set = LightSet::new(big, ShadowMode::Dynamic);
        for i in 0..339 {
            let (x, z) = ((i % 20) as f32 * 2.5 - 25.0, (i / 20) as f32 * 2.5 - 50.0);
            scene
                .extra
                .push(LightDesc::point(i + 1, Vec3::new(x, 3.0, z)));
        }
        scene.eye = Vec3::new(0.0, 2.0, 20.0);
        let mut plan = scene.frame();
        for _ in 0..3000 {
            if plan.settled {
                break;
            }
            plan = scene.frame();
        }
        let before = plan.stats;
        assert_eq!(before.shadowed, 339);
        // 339 lights at 128² take about 32 pages, which the high bound allows.
        assert!(
            before.pages_made > 20 && before.pages_past_bound() == 0,
            "{before:?}"
        );
        scene.set.set_bound_pages(15);
        let mut fewest = usize::MAX;
        for _ in 0..4000 {
            plan = scene.frame();
            fewest = fewest.min(plan.stats.shadowed);
            if plan.settled && plan.stats.pages_made <= scene.set.config.static_bound() {
                break;
            }
        }
        assert_eq!(fewest, 339, "no light lost its shadow while tiles shrank");
        assert_eq!(plan.stats.drawn, 339);
        assert!(
            plan.stats.pages_made <= scene.set.config.static_bound(),
            "{:?}",
            plan.stats
        );
        assert_eq!(plan.stats.over_budget, 0);
    }
}
