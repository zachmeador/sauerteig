//! Shadow atlas bookkeeping: which tile of which page holds which face of which
//! light, when each was last rendered, and which faces to render next. It owns
//! no GPU resources, so the allocation and invalidation rules are testable alone.
use super::culling::Bounds;
use glam::Vec3;
use std::collections::HashMap;

pub const PAGE_SIZE: u32 = 1024;
/// A tile may not change size until it has been resident this many seconds.
pub const RESIDENCY_SECONDS: f64 = 1.0;
/// Projected radii must pass a size threshold by this factor to change tile size.
const HYSTERESIS: f32 = 1.5;
const MEDIUM_RADIUS: f32 = 120.0;
const LARGE_RADIUS: f32 = 400.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum TileClass {
    Small,
    Medium,
    Large,
}

impl TileClass {
    pub const ALL: [Self; 3] = [Self::Small, Self::Medium, Self::Large];

    /// The value the shaders read from the page class table.
    pub fn code(self) -> u32 {
        self as u32
    }

    pub fn size(self) -> u32 {
        128 << self.code()
    }

    pub fn per_row(self) -> u32 {
        PAGE_SIZE / self.size()
    }

    pub fn per_page(self) -> usize {
        (self.per_row() * self.per_row()) as usize
    }

    /// Texel origin of a slot within its page.
    pub fn origin(self, slot: u8) -> [u32; 2] {
        let slot = u32::from(slot);
        [
            slot % self.per_row() * self.size(),
            slot / self.per_row() * self.size(),
        ]
    }

    fn up(self) -> Option<Self> {
        Self::ALL.get(self as usize + 1).copied()
    }

    fn down(self) -> Option<Self> {
        (self as usize).checked_sub(1).map(|i| Self::ALL[i])
    }

    fn threshold(self) -> f32 {
        match self {
            Self::Small => 0.0,
            Self::Medium => MEDIUM_RADIUS,
            Self::Large => LARGE_RADIUS,
        }
    }

    /// The class for a light whose range projects to `pixels` on screen. A
    /// light that already has a class keeps it until the radius leaves the
    /// class's band by the hysteresis factor.
    pub fn for_radius(pixels: f32, current: Option<Self>) -> Self {
        let mut class = current.unwrap_or(Self::Small);
        let factor = if current.is_some() { HYSTERESIS } else { 1.0 };
        loop {
            if class
                .up()
                .is_some_and(|up| pixels >= up.threshold() * factor)
            {
                class = class.up().unwrap();
            } else if class.down().is_some() && pixels < class.threshold() / factor {
                class = class.down().unwrap();
            } else {
                return class;
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TileRef {
    pub page: u8,
    pub slot: u8,
}

impl TileRef {
    /// The 16-bit reference in light records.
    pub fn packed(self) -> u16 {
        u16::from(self.page) << 8 | u16::from(self.slot)
    }
}

/// Pages of one tile size each, handed out slot by slot and returned to the
/// pool, with their size forgotten, once empty.
pub struct PagePool {
    classes: Vec<Option<TileClass>>,
    used: Vec<u64>,
}

impl PagePool {
    pub fn new(pages: usize) -> Self {
        Self {
            classes: vec![None; pages],
            used: vec![0; pages],
        }
    }

    pub fn pages(&self) -> usize {
        self.classes.len()
    }

    pub fn class(&self, page: usize) -> Option<TileClass> {
        self.classes[page]
    }

    pub fn alloc(&mut self, class: TileClass) -> Option<TileRef> {
        let page = (0..self.pages())
            .find(|&p| self.classes[p] == Some(class) && self.free_slots(p) > 0)
            .or_else(|| (0..self.pages()).find(|&p| self.classes[p].is_none()))?;
        self.classes[page] = Some(class);
        let slot = (!self.used[page]).trailing_zeros() as u8;
        self.used[page] |= 1 << slot;
        Some(TileRef {
            page: page as u8,
            slot,
        })
    }

    pub fn free(&mut self, tile: TileRef) {
        let page = usize::from(tile.page);
        self.used[page] &= !(1 << tile.slot);
        if self.used[page] == 0 {
            self.classes[page] = None;
        }
    }

    fn free_slots(&self, page: usize) -> usize {
        self.classes[page].map_or(0, |class| {
            class.per_page() - self.used[page].count_ones() as usize
        })
    }

    /// Tiles of `class` obtainable without evicting anything.
    pub fn capacity(&self, class: TileClass) -> usize {
        (0..self.pages())
            .map(|page| match self.classes[page] {
                Some(c) if c == class => self.free_slots(page),
                None => class.per_page(),
                _ => 0,
            })
            .sum()
    }

    pub fn used_tiles(&self) -> usize {
        self.used.iter().map(|u| u.count_ones() as usize).sum()
    }
}

#[derive(Clone, Copy, Debug)]
struct Tile {
    at: TileRef,
    /// The epoch of the last render, or 0 if this tile has no valid contents.
    built: u64,
    /// The epoch of the last change that affects this tile's contents.
    wanted: u64,
}

#[derive(Clone, Debug)]
struct TileSet {
    class: TileClass,
    tiles: Vec<Tile>,
    allocated_at: f64,
}

impl TileSet {
    fn ready(&self) -> bool {
        self.tiles.iter().all(|t| t.built != 0)
    }
}

struct Entry {
    center: Vec3,
    radius: f32,
    current: Option<TileSet>,
    /// A replacement in another size, rendering while `current` stays in use.
    pending: Option<TileSet>,
    last_used: u64,
    /// The frame this light was last served by `ensure`.
    claimed: u64,
    /// The frame this light was last a candidate, and its importance rank then.
    candidate: u64,
    rank: usize,
}

impl Entry {
    fn sets(&mut self) -> impl Iterator<Item = &mut TileSet> {
        self.current.iter_mut().chain(self.pending.iter_mut())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Residency {
    Resident,
    /// The atlas is full of more important lights.
    OverBudget,
}

pub struct Request {
    pub id: u32,
    pub faces: usize,
    pub class: TileClass,
    pub frame: u64,
    pub now: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Job {
    pub light: u32,
    pub face: usize,
    pub tile: TileRef,
    pub class: TileClass,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AtlasStats {
    pub lights: usize,
    pub tiles: usize,
    pub pages: usize,
}

pub struct ShadowAtlas {
    pool: PagePool,
    entries: HashMap<u32, Entry>,
    epoch: u64,
}

impl ShadowAtlas {
    pub fn new(pages: usize) -> Self {
        Self {
            pool: PagePool::new(pages),
            entries: HashMap::new(),
            epoch: 1,
        }
    }

    pub fn pool(&self) -> &PagePool {
        &self.pool
    }

    pub fn stats(&self) -> AtlasStats {
        AtlasStats {
            lights: self
                .entries
                .values()
                .filter(|e| e.current.is_some())
                .count(),
            tiles: self.pool.used_tiles(),
            pages: (0..self.pool.pages())
                .filter(|&p| self.pool.class(p).is_some())
                .count(),
        }
    }

    pub fn invalidate_all(&mut self) {
        self.epoch += 1;
        let epoch = self.epoch;
        for entry in self.entries.values_mut() {
            for tile in entry.sets().flat_map(|s| s.tiles.iter_mut()) {
                tile.wanted = epoch;
            }
        }
    }

    /// Geometry changed inside `bounds`: every light whose sphere reaches it
    /// re-renders, and the rest keep their tiles.
    pub fn invalidate_bounds(&mut self, bounds: Bounds) {
        self.epoch += 1;
        let epoch = self.epoch;
        for entry in self.entries.values_mut() {
            if bounds.within_radius(entry.center, entry.radius) {
                for tile in entry.sets().flat_map(|s| s.tiles.iter_mut()) {
                    tile.wanted = epoch;
                }
            }
        }
    }

    /// The light moved or changed shape, so its tiles no longer match the
    /// projection the shaders will use: they are invalid until re-rendered.
    pub fn light_changed(&mut self, id: u32, center: Vec3, radius: f32) {
        self.epoch += 1;
        let epoch = self.epoch;
        if let Some(entry) = self.entries.get_mut(&id) {
            entry.center = center;
            entry.radius = radius;
            for tile in entry.sets().flat_map(|s| s.tiles.iter_mut()) {
                tile.built = 0;
                tile.wanted = epoch;
            }
        }
    }

    pub fn release(&mut self, id: u32) {
        if self.entries.contains_key(&id) {
            self.release_tiles(id);
            self.entries.remove(&id);
        }
    }

    pub fn forget_absent(&mut self, keep: impl Fn(u32) -> bool) {
        let gone: Vec<u32> = self
            .entries
            .keys()
            .copied()
            .filter(|&id| !keep(id))
            .collect();
        for id in gone {
            self.release(id);
        }
    }

    /// Register this frame's shadow candidates in importance order, before any
    /// `ensure`, so a more important light can displace a less important
    /// resident one. Each is `(id, center, radius)`.
    pub fn candidates(&mut self, frame: u64, order: &[(u32, Vec3, f32)]) {
        for (rank, &(id, center, radius)) in order.iter().enumerate() {
            let entry = self.entries.entry(id).or_insert_with(|| Entry {
                center,
                radius,
                current: None,
                pending: None,
                last_used: frame,
                claimed: 0,
                candidate: frame,
                rank,
            });
            entry.candidate = frame;
            entry.rank = rank;
        }
    }

    /// Keep or acquire tiles for one registered candidate, in importance order.
    pub fn ensure(&mut self, request: &Request) -> Residency {
        let Some(entry) = self.entries.get_mut(&request.id) else {
            return Residency::OverBudget;
        };
        entry.claimed = request.frame;
        entry.last_used = request.frame;
        if let Some(current) = &entry.current {
            // A resize uses free tiles only: displacing other lights to improve
            // one light's resolution would cost more lights than it gains.
            let resize = current.class != request.class
                && request.now - current.allocated_at >= RESIDENCY_SECONDS
                && entry.pending.is_none();
            if resize {
                if let Some(tiles) = self.allocate(request.class, request.faces) {
                    self.entries.get_mut(&request.id).unwrap().pending = Some(TileSet {
                        class: request.class,
                        tiles,
                        allocated_at: request.now,
                    });
                }
            }
            return Residency::Resident;
        }
        let rank = entry.rank;
        loop {
            // The wanted size if it fits, then smaller ones; only when none
            // fits does the least valuable resident light give up its tiles.
            let found = (0..=request.class as usize)
                .rev()
                .map(|c| TileClass::ALL[c])
                .find_map(|class| self.allocate(class, request.faces).map(|t| (class, t)));
            if let Some((class, tiles)) = found {
                self.entries.get_mut(&request.id).unwrap().current = Some(TileSet {
                    class,
                    tiles,
                    allocated_at: request.now,
                });
                return Residency::Resident;
            }
            match self.victim(request, rank) {
                Some(victim) => self.release_tiles(victim),
                None => return Residency::OverBudget,
            }
        }
    }

    fn allocate(&mut self, class: TileClass, faces: usize) -> Option<Vec<Tile>> {
        if self.pool.capacity(class) < faces {
            return None;
        }
        let epoch = self.epoch;
        Some(
            (0..faces)
                .map(|_| Tile {
                    at: self.pool.alloc(class).expect("capacity was checked"),
                    built: 0,
                    wanted: epoch,
                })
                .collect(),
        )
    }

    /// Lights not yet served this frame, least valuable first: those that are
    /// not candidates at all (least recently used first), then the least
    /// important candidates that have been resident long enough.
    fn victim(&self, request: &Request, rank: usize) -> Option<u32> {
        self.entries
            .iter()
            .filter(|(&id, e)| {
                id != request.id
                    && e.claimed != request.frame
                    && (e.current.is_some() || e.pending.is_some())
                    && (e.candidate != request.frame
                        || (e.rank > rank
                            && e.current
                                .as_ref()
                                .is_none_or(|s| request.now - s.allocated_at >= RESIDENCY_SECONDS)))
            })
            .min_by_key(|(&id, e)| {
                let candidate = e.candidate == request.frame;
                let value = if candidate {
                    usize::MAX - e.rank
                } else {
                    e.last_used as usize
                };
                (candidate, value, id)
            })
            .map(|(&id, _)| id)
    }

    fn release_tiles(&mut self, id: u32) {
        if let Some(entry) = self.entries.get_mut(&id) {
            for set in [entry.current.take(), entry.pending.take()]
                .into_iter()
                .flatten()
            {
                for tile in set.tiles {
                    self.pool.free(tile.at);
                }
            }
        }
    }

    /// Whether the light's tiles all hold a render, however stale.
    pub fn ready(&self, id: u32) -> bool {
        self.entries
            .get(&id)
            .and_then(|e| e.current.as_ref())
            .is_some_and(TileSet::ready)
    }

    /// Tile references of the light's in-use set, with that set's class.
    pub fn tiles(&self, id: u32) -> Option<(TileClass, Vec<TileRef>)> {
        let set = self.entries.get(&id)?.current.as_ref()?;
        Some((set.class, set.tiles.iter().map(|t| t.at).collect()))
    }

    pub fn class_of(&self, id: u32) -> Option<TileClass> {
        self.entries.get(&id)?.current.as_ref().map(|s| s.class)
    }

    /// Whether any tile of a light in `ids` still needs rendering.
    pub fn dirty(&self, ids: impl IntoIterator<Item = u32>) -> bool {
        ids.into_iter().any(|id| {
            self.entries.get(&id).is_some_and(|e| {
                e.current
                    .iter()
                    .chain(e.pending.iter())
                    .any(|s| s.tiles.iter().any(|t| t.built != t.wanted))
            })
        })
    }

    /// Choose up to `budget` faces to render this frame, in the given light
    /// order: new tiles first, then stale ones, then size changes. Chosen faces
    /// count as rendered from here on.
    pub fn schedule(&mut self, order: &[u32], budget: usize) -> Vec<Job> {
        let mut jobs = Vec::new();
        for pass in 0..3 {
            for &id in order {
                let Some(entry) = self.entries.get_mut(&id) else {
                    continue;
                };
                let (set, is_pending) = match pass {
                    2 => (entry.pending.as_mut(), true),
                    _ => (entry.current.as_mut(), false),
                };
                let Some(set) = set else { continue };
                let class = set.class;
                for (face, tile) in set.tiles.iter_mut().enumerate() {
                    let due = match (pass, is_pending) {
                        (0, false) => tile.built == 0,
                        (1, false) => tile.built != 0 && tile.built != tile.wanted,
                        (2, true) => tile.built != tile.wanted,
                        _ => false,
                    };
                    if due && jobs.len() < budget {
                        tile.built = tile.wanted;
                        jobs.push(Job {
                            light: id,
                            face,
                            tile: tile.at,
                            class,
                        });
                    }
                }
            }
        }
        jobs
    }

    /// Replace each light's in-use tiles with its finished resize.
    pub fn swap_finished(&mut self) {
        for entry in self.entries.values_mut() {
            if entry.pending.as_ref().is_some_and(TileSet::ready) {
                let old = std::mem::replace(&mut entry.current, entry.pending.take());
                for tile in old.into_iter().flat_map(|s| s.tiles) {
                    self.pool.free(tile.at);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn center(id: u32) -> Vec3 {
        Vec3::new(id as f32 * 100.0, 0.0, 0.0)
    }

    /// One frame of requests in the given importance order.
    fn serve(atlas: &mut ShadowAtlas, ids: &[u32], class: TileClass, frame: u64, now: f64) {
        let order: Vec<_> = ids.iter().map(|&id| (id, center(id), 12.0)).collect();
        atlas.candidates(frame, &order);
        for &id in ids {
            atlas.ensure(&Request {
                id,
                faces: 6,
                class,
                frame,
                now,
            });
        }
    }

    fn render_all(atlas: &mut ShadowAtlas, ids: &[u32]) -> Vec<Job> {
        atlas.schedule(ids, usize::MAX)
    }

    #[test]
    fn size_classes_follow_projected_radius_with_hysteresis() {
        use TileClass::*;
        assert_eq!(TileClass::for_radius(10.0, None), Small);
        assert_eq!(TileClass::for_radius(119.0, None), Small);
        assert_eq!(TileClass::for_radius(120.0, None), Medium);
        assert_eq!(TileClass::for_radius(399.0, None), Medium);
        assert_eq!(TileClass::for_radius(400.0, None), Large);
        // A band is held until the radius leaves it by 1.5x.
        assert_eq!(TileClass::for_radius(130.0, Some(Small)), Small);
        assert_eq!(TileClass::for_radius(180.0, Some(Small)), Medium);
        assert_eq!(TileClass::for_radius(100.0, Some(Medium)), Medium);
        assert_eq!(TileClass::for_radius(79.0, Some(Medium)), Small);
        assert_eq!(TileClass::for_radius(500.0, Some(Medium)), Medium);
        assert_eq!(TileClass::for_radius(600.0, Some(Medium)), Large);
        assert_eq!(TileClass::for_radius(300.0, Some(Large)), Large);
        assert_eq!(TileClass::for_radius(260.0, Some(Large)), Medium);
        assert_eq!([Small, Medium, Large].map(TileClass::size), [128, 256, 512]);
        assert_eq!([Small, Medium, Large].map(TileClass::per_page), [64, 16, 4]);
        assert_eq!(Medium.origin(5), [256, 256]);
        assert_eq!(Large.origin(3), [512, 512]);
    }

    #[test]
    fn pages_take_the_size_of_their_first_tile_and_return_when_empty() {
        let mut pool = PagePool::new(2);
        let a = pool.alloc(TileClass::Large).unwrap();
        let b = pool.alloc(TileClass::Large).unwrap();
        assert_eq!((a.page, a.slot, b.page, b.slot), (0, 0, 0, 1));
        assert_eq!(pool.capacity(TileClass::Large), 2 + 4);
        assert_eq!(pool.capacity(TileClass::Small), 64);
        let c = pool.alloc(TileClass::Small).unwrap();
        assert_eq!(c.page, 1);
        assert_eq!(pool.capacity(TileClass::Medium), 0);
        pool.free(c);
        assert_eq!(pool.capacity(TileClass::Medium), 16);
        pool.free(a);
        pool.free(b);
        assert_eq!(pool.class(0), None);
        assert_eq!(pool.used_tiles(), 0);
        for slot in 0..4 {
            assert_eq!(pool.alloc(TileClass::Large).unwrap().slot, slot);
        }
        assert_eq!(TileRef { page: 3, slot: 9 }.packed(), 0x0309);
    }

    #[test]
    fn point_lights_take_six_tiles_and_spot_lights_one() {
        let mut atlas = ShadowAtlas::new(2);
        atlas.candidates(1, &[(1, center(1), 12.0), (2, center(2), 12.0)]);
        let point = Request {
            id: 1,
            faces: 6,
            class: TileClass::Medium,
            frame: 1,
            now: 0.0,
        };
        assert_eq!(atlas.ensure(&point), Residency::Resident);
        let spot = Request {
            id: 2,
            faces: 1,
            ..point
        };
        assert_eq!(atlas.ensure(&spot), Residency::Resident);
        assert_eq!(atlas.tiles(1).unwrap().1.len(), 6);
        assert_eq!(atlas.tiles(2).unwrap().1.len(), 1);
        let stats = atlas.stats();
        assert_eq!((stats.lights, stats.tiles, stats.pages), (2, 7, 1));
        let refs: Vec<_> = atlas
            .tiles(1)
            .unwrap()
            .1
            .iter()
            .map(|t| t.packed())
            .collect();
        assert_eq!(refs, [0, 1, 2, 3, 4, 5]);
    }

    #[test]
    fn a_full_atlas_shrinks_tiles_then_gives_up() {
        let mut atlas = ShadowAtlas::new(1);
        // One page: 6 large tiles do not fit, so the page becomes a medium one,
        // which holds two point lights (12 of 16 tiles) and no third.
        serve(&mut atlas, &[1, 2, 3], TileClass::Large, 1, 0.0);
        assert_eq!(atlas.tiles(1).unwrap().0, TileClass::Medium);
        assert_eq!(atlas.tiles(2).unwrap().0, TileClass::Medium);
        assert!(atlas.tiles(3).is_none());
        // Lights served earlier in the frame are never displaced for later ones.
        serve(&mut atlas, &[1, 2, 3, 4], TileClass::Small, 2, 0.1);
        assert!(atlas.tiles(1).is_some() && atlas.tiles(2).is_some());
        assert!(atlas.tiles(3).is_none() && atlas.tiles(4).is_none());
    }

    #[test]
    fn residents_are_protected_at_first_then_the_least_important_gives_way() {
        let mut atlas = ShadowAtlas::new(1);
        // Ten small lights fill a page (60 of 64 tiles).
        let first: Vec<u32> = (1..=10).collect();
        serve(&mut atlas, &first, TileClass::Small, 1, 0.0);
        assert!(first.iter().all(|&id| atlas.tiles(id).is_some()));
        // Light 11 is now the most important and 1..=10 trail it.
        let order: Vec<u32> = std::iter::once(11).chain(first.iter().copied()).collect();
        serve(&mut atlas, &order, TileClass::Small, 2, 0.5);
        assert!(
            atlas.tiles(11).is_none(),
            "residents are protected at first"
        );
        assert!(first.iter().all(|&id| atlas.tiles(id).is_some()));
        serve(&mut atlas, &order, TileClass::Small, 3, 2.0);
        assert!(atlas.tiles(11).is_some());
        assert!(atlas.tiles(10).is_none(), "the least important gave way");
        assert!(first[..9].iter().all(|&id| atlas.tiles(id).is_some()));
        // Light 10 trails everything, so it cannot take its tiles back.
        serve(&mut atlas, &order, TileClass::Small, 4, 3.0);
        assert!(atlas.tiles(10).is_none());
        assert_eq!(atlas.stats().lights, 10);
    }

    #[test]
    fn lights_out_of_view_are_evicted_before_visible_ones() {
        let mut atlas = ShadowAtlas::new(1);
        let ids: Vec<u32> = (1..=10).collect();
        serve(&mut atlas, &ids, TileClass::Small, 1, 0.0);
        // Only lights 1 and 2 stay in view; light 11 arrives.
        serve(&mut atlas, &[11, 1, 2], TileClass::Small, 2, 0.1);
        assert!(atlas.tiles(11).is_some());
        assert!(atlas.tiles(1).is_some() && atlas.tiles(2).is_some());
        let evicted: Vec<_> = ids
            .iter()
            .filter(|&&id| atlas.tiles(id).is_none())
            .collect();
        // The least recently used of the unseen lights goes first, and only as
        // many as needed to free six tiles.
        assert_eq!(evicted.len(), 1, "{evicted:?}");
    }

    #[test]
    fn an_edit_in_range_rerenders_a_tile_and_one_out_of_range_does_not() {
        let mut atlas = ShadowAtlas::new(2);
        serve(&mut atlas, &[1, 2], TileClass::Medium, 1, 0.0);
        assert_eq!(render_all(&mut atlas, &[1, 2]).len(), 12);
        assert!(atlas.ready(1) && atlas.ready(2));
        assert!(render_all(&mut atlas, &[1, 2]).is_empty());
        // Light 1 is centred at x = 100, light 2 at x = 200, both with radius 12.
        atlas.invalidate_bounds(Bounds::around(Vec3::new(105.0, 0.0, 0.0), 1.0));
        let jobs = render_all(&mut atlas, &[1, 2]);
        assert_eq!(jobs.len(), 6);
        assert!(jobs.iter().all(|j| j.light == 1));
        // Stale tiles stay in use while waiting.
        atlas.invalidate_bounds(Bounds::around(Vec3::new(105.0, 0.0, 0.0), 1.0));
        assert!(atlas.ready(1) && atlas.dirty([1]));
        assert_eq!(render_all(&mut atlas, &[1]).len(), 6);
        // An edit beyond the sphere misses it.
        atlas.invalidate_bounds(Bounds::around(Vec3::new(130.0, 0.0, 0.0), 1.0));
        assert!(!atlas.dirty([1, 2]));
        assert!(render_all(&mut atlas, &[1, 2]).is_empty());
        atlas.invalidate_all();
        assert_eq!(render_all(&mut atlas, &[1, 2]).len(), 12);
    }

    #[test]
    fn a_moved_light_is_invalid_until_rerendered() {
        let mut atlas = ShadowAtlas::new(1);
        serve(&mut atlas, &[1], TileClass::Medium, 1, 0.0);
        render_all(&mut atlas, &[1]);
        assert!(atlas.ready(1));
        atlas.light_changed(1, Vec3::new(500.0, 0.0, 0.0), 12.0);
        assert!(!atlas.ready(1), "a stale projection must not be sampled");
        assert!(atlas.dirty([1]));
        assert_eq!(render_all(&mut atlas, &[1]).len(), 6);
        assert!(atlas.ready(1) && !atlas.dirty([1]));
        // Edits near the new position reach it; edits at the old one do not.
        atlas.invalidate_bounds(Bounds::around(Vec3::new(100.0, 0.0, 0.0), 1.0));
        assert!(render_all(&mut atlas, &[1]).is_empty());
        atlas.invalidate_bounds(Bounds::around(Vec3::new(505.0, 0.0, 0.0), 1.0));
        assert_eq!(render_all(&mut atlas, &[1]).len(), 6);
    }

    #[test]
    fn updates_are_budgeted_and_new_tiles_precede_stale_ones() {
        let mut atlas = ShadowAtlas::new(3);
        serve(&mut atlas, &[1, 2, 3], TileClass::Medium, 1, 0.0);
        let first = atlas.schedule(&[1, 2, 3], 6);
        assert_eq!(first.len(), 6);
        assert!(first.iter().all(|j| j.light == 1), "faces stay together");
        // A stale tile of light 1 waits behind the unrendered tiles of 2 and 3.
        atlas.invalidate_bounds(Bounds::around(Vec3::new(100.0, 0.0, 0.0), 1.0));
        let second = atlas.schedule(&[1, 2, 3], 6);
        assert!(second.iter().all(|j| j.light == 2));
        let third = atlas.schedule(&[1, 2, 3], 12);
        assert_eq!(third.iter().filter(|j| j.light == 3).count(), 6);
        assert_eq!(third.iter().filter(|j| j.light == 1).count(), 6);
        assert!(third[..6].iter().all(|j| j.light == 3));
        assert!(atlas.schedule(&[1, 2, 3], 12).is_empty());
    }

    #[test]
    fn a_resize_waits_for_residency_and_swaps_when_built() {
        let mut atlas = ShadowAtlas::new(2);
        serve(&mut atlas, &[1], TileClass::Small, 1, 0.0);
        render_all(&mut atlas, &[1]);
        let before = atlas.tiles(1).unwrap();
        assert_eq!(before.0, TileClass::Small);
        serve(&mut atlas, &[1], TileClass::Medium, 2, 0.5);
        assert!(atlas.schedule(&[1], 6).is_empty(), "too soon to resize");
        serve(&mut atlas, &[1], TileClass::Medium, 3, 1.5);
        // The old tiles remain in use while the new ones render.
        assert_eq!(atlas.tiles(1).unwrap(), before);
        assert!(atlas.ready(1));
        let jobs = atlas.schedule(&[1], 6);
        assert_eq!(jobs.len(), 6);
        assert!(jobs.iter().all(|j| j.class == TileClass::Medium));
        atlas.swap_finished();
        assert_eq!(atlas.tiles(1).unwrap().0, TileClass::Medium);
        assert_eq!(atlas.stats().tiles, 6);
        // The small page returned to the pool when its tiles were freed.
        assert_eq!(atlas.pool().capacity(TileClass::Large), 4);
    }

    #[test]
    fn a_resize_never_displaces_other_lights() {
        let mut atlas = ShadowAtlas::new(1);
        serve(&mut atlas, &[1, 2, 3], TileClass::Small, 1, 0.0);
        render_all(&mut atlas, &[1, 2, 3]);
        serve(&mut atlas, &[1, 2, 3], TileClass::Large, 5, 5.0);
        assert!(atlas.schedule(&[1, 2, 3], 18).is_empty());
        assert!([1, 2, 3]
            .iter()
            .all(|&id| atlas.tiles(id).unwrap().0 == TileClass::Small));
    }

    #[test]
    fn released_lights_free_their_tiles() {
        let mut atlas = ShadowAtlas::new(1);
        serve(&mut atlas, &[1, 2], TileClass::Medium, 1, 0.0);
        assert_eq!(atlas.stats().tiles, 12);
        atlas.forget_absent(|id| id == 2);
        assert_eq!(atlas.stats().tiles, 6);
        assert!(atlas.tiles(1).is_none());
        atlas.release(2);
        assert_eq!(atlas.stats(), AtlasStats::default());
    }
}
