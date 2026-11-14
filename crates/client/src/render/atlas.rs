//! Shadow atlas bookkeeping: which tile of which page holds which face of which
//! light, when each was last rendered, and which faces to render next. It owns
//! no GPU resources, so the allocation and invalidation rules are testable alone.
use super::culling::Bounds;
use glam::Vec3;
use std::collections::HashMap;

pub const PAGE_SIZE: u32 = 1024;
/// Bits of a tile reference that hold the slot within its page.
pub const SLOT_BITS: u32 = 10;
/// A tile may not change size until it has been resident this many seconds.
pub const RESIDENCY_SECONDS: f64 = 1.0;
/// Projected radii must pass a size threshold by this factor to change tile size.
const HYSTERESIS: f32 = 1.5;
const MEDIUM_RADIUS: f32 = 120.0;
const LARGE_RADIUS: f32 = 400.0;
/// Lights whose tiles are being replaced at the same time, at most, so a
/// change of budget never holds two sets of tiles for every light at once.
pub const MAX_PENDING: usize = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum TileClass {
    /// 32², where an atlas that is out of room shrinks the farthest lights.
    Tiny,
    /// 64².
    Mini,
    /// 128², the size every light gets while the atlas has room.
    Small,
    Medium,
    Large,
}

impl TileClass {
    pub const ALL: [Self; 5] = [
        Self::Tiny,
        Self::Mini,
        Self::Small,
        Self::Medium,
        Self::Large,
    ];

    /// The value the shaders read from the page table.
    pub fn code(self) -> u32 {
        self as u32
    }

    pub fn size(self) -> u32 {
        32 << self.code()
    }

    pub fn per_row(self) -> u32 {
        PAGE_SIZE / self.size()
    }

    pub fn per_page(self) -> usize {
        (self.per_row() * self.per_row()) as usize
    }

    /// Texel origin of a slot within its page.
    pub fn origin(self, slot: u16) -> [u32; 2] {
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
            Self::Tiny | Self::Mini | Self::Small => 0.0,
            Self::Medium => MEDIUM_RADIUS,
            Self::Large => LARGE_RADIUS,
        }
    }

    /// The class a light whose range projects to `pixels` on screen asks for,
    /// from 128² up. A light that already has a class keeps it until the radius
    /// leaves the class's band by the hysteresis factor; a smaller one, which
    /// the atlas gave it for lack of room, counts as 128².
    pub fn for_radius(pixels: f32, current: Option<Self>) -> Self {
        let mut class = current.unwrap_or(Self::Small).max(Self::Small);
        let factor = if current.is_some() { HYSTERESIS } else { 1.0 };
        loop {
            if class
                .up()
                .is_some_and(|up| pixels >= up.threshold() * factor)
            {
                class = class.up().unwrap();
            } else if class > Self::Small && pixels < class.threshold() / factor {
                class = class.down().unwrap();
            } else {
                return class;
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TileRef {
    pub page: u16,
    pub slot: u16,
}

impl TileRef {
    /// The reference in light records: the page above the slot.
    pub fn packed(self) -> u32 {
        u32::from(self.page) << SLOT_BITS | u32::from(self.slot)
    }
}

/// Pages of one tile size each, handed out slot by slot and returned to the
/// pool, with their size forgotten, once empty. Pages are created when tiles
/// need them, up to a limit each request names: the level's bound for ordinary
/// tiles, past it only for lights that would otherwise have no shadow.
pub struct PagePool {
    classes: Vec<Option<TileClass>>,
    used: Vec<[u64; 16]>,
    bound: usize,
    ceiling: usize,
}

impl PagePool {
    pub fn new(bound: usize, ceiling: usize) -> Self {
        Self {
            classes: Vec::new(),
            used: Vec::new(),
            bound,
            ceiling: ceiling.max(bound),
        }
    }

    pub fn set_bound(&mut self, bound: usize) {
        self.bound = bound;
        self.ceiling = self.ceiling.max(bound);
    }

    /// The level's bound on pages, which ordinary tiles stay within.
    pub fn bound(&self) -> usize {
        self.bound
    }

    /// The most pages the device has layers for.
    pub fn ceiling(&self) -> usize {
        self.ceiling
    }

    /// Pages created so far.
    pub fn pages(&self) -> usize {
        self.classes.len()
    }

    pub fn class(&self, page: usize) -> Option<TileClass> {
        self.classes[page]
    }

    /// A slot of a page of `class`: the first page of that size with room, else
    /// the first page that is empty, else a new page while fewer than `limit`
    /// pages exist.
    pub fn alloc(&mut self, class: TileClass, limit: usize) -> Option<TileRef> {
        let page = self.find(class, self.pages()).or_else(|| {
            (self.pages() < limit.min(self.ceiling)).then(|| {
                self.classes.push(None);
                self.used.push([0; 16]);
                self.pages() - 1
            })
        })?;
        Some(self.take(page, class))
    }

    /// A slot on any page but `except`, never creating one. Tiles go to the
    /// first page of their size with room, then the first empty one.
    pub fn alloc_except(&mut self, class: TileClass, except: usize) -> Option<TileRef> {
        let usable = |p: &usize| *p != except;
        let page = (0..self.pages())
            .filter(usable)
            .find(|&p| self.classes[p] == Some(class) && self.free_slots(p) > 0)
            .or_else(|| {
                (0..self.pages())
                    .filter(usable)
                    .find(|&p| self.classes[p].is_none())
            })?;
        Some(self.take(page, class))
    }

    fn find(&self, class: TileClass, below: usize) -> Option<usize> {
        (0..below)
            .find(|&p| self.classes[p] == Some(class) && self.free_slots(p) > 0)
            .or_else(|| (0..below).find(|&p| self.classes[p].is_none()))
    }

    fn take(&mut self, page: usize, class: TileClass) -> TileRef {
        self.classes[page] = Some(class);
        let slot = self.used[page]
            .iter()
            .enumerate()
            .find(|(_, word)| **word != u64::MAX)
            .map(|(i, word)| i * 64 + (!word).trailing_zeros() as usize)
            .expect("the page has a free slot");
        self.used[page][slot / 64] |= 1 << (slot % 64);
        TileRef {
            page: page as u16,
            slot: slot as u16,
        }
    }

    /// Forget the empty pages at the end, so that a texture can shrink. Tiles
    /// go to the lowest page that has room, which keeps the used ones near the
    /// front.
    pub fn trim(&mut self) {
        while matches!(self.classes.last(), Some(None)) {
            self.classes.pop();
            self.used.pop();
        }
    }

    pub fn free(&mut self, tile: TileRef) {
        let page = usize::from(tile.page);
        let slot = usize::from(tile.slot);
        self.used[page][slot / 64] &= !(1 << (slot % 64));
        if self.used[page].iter().all(|&w| w == 0) {
            self.classes[page] = None;
        }
    }

    fn used_in(&self, page: usize) -> usize {
        self.used[page]
            .iter()
            .map(|w| w.count_ones() as usize)
            .sum()
    }

    fn free_slots(&self, page: usize) -> usize {
        self.classes[page].map_or(0, |class| class.per_page() - self.used_in(page))
    }

    /// Tiles of `class` obtainable on pages other than `except`.
    fn capacity_except(&self, class: TileClass, except: usize) -> usize {
        (0..self.pages())
            .filter(|&p| p != except)
            .map(|page| match self.classes[page] {
                Some(c) if c == class => self.free_slots(page),
                None => class.per_page(),
                _ => 0,
            })
            .sum()
    }

    /// Tiles of `class` obtainable without evicting anything, with pages made
    /// as far as `limit`.
    pub fn capacity(&self, class: TileClass, limit: usize) -> usize {
        self.capacity_except(class, usize::MAX)
            + limit.min(self.ceiling).saturating_sub(self.pages()) * class.per_page()
    }

    pub fn used_tiles(&self) -> usize {
        (0..self.pages()).map(|p| self.used_in(p)).sum()
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
    /// The frame this light was last a candidate.
    candidate: u64,
}

impl Entry {
    fn sets(&mut self) -> impl Iterator<Item = &mut TileSet> {
        self.current.iter_mut().chain(self.pending.iter_mut())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Residency {
    Resident,
    /// The device's page ceiling is reached: no tile of any size is left.
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
    /// Pages holding tiles, and pages created, which the texture has layers for.
    pub pages: usize,
    pub created: usize,
}

pub struct ShadowAtlas {
    pool: PagePool,
    entries: HashMap<u32, Entry>,
    epoch: u64,
    /// Lights with a replacement in progress.
    replacing: usize,
}

impl ShadowAtlas {
    pub fn new(bound: usize, ceiling: usize) -> Self {
        Self {
            pool: PagePool::new(bound, ceiling),
            entries: HashMap::new(),
            epoch: 1,
            replacing: 0,
        }
    }

    pub fn pool(&self) -> &PagePool {
        &self.pool
    }

    pub fn set_bound(&mut self, bound: usize) {
        self.pool.set_bound(bound);
    }

    /// Give up the empty pages at the end of the pool.
    pub fn trim(&mut self) {
        self.pool.trim();
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
            created: self.pool.pages(),
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
    /// `ensure`. Lights that are not candidates may lose their tiles to ones
    /// that are, and candidates never do. Each is `(id, center, radius)`.
    pub fn candidates(&mut self, frame: u64, order: &[(u32, Vec3, f32)]) {
        for &(id, center, radius) in order {
            let entry = self.entries.entry(id).or_insert_with(|| Entry {
                center,
                radius,
                current: None,
                pending: None,
                last_used: frame,
                claimed: 0,
                candidate: frame,
            });
            entry.candidate = frame;
        }
        self.replacing = self
            .entries
            .values()
            .filter(|e| e.pending.is_some())
            .count();
    }

    /// Keep or acquire tiles for one registered candidate. A light that has
    /// none takes the size asked for if it fits, then smaller ones, then the
    /// tiles of the least recently used light out of view, and only then pages
    /// past the bound: a candidate always gets a shadow.
    pub fn ensure(&mut self, request: &Request) -> Residency {
        let Some(entry) = self.entries.get_mut(&request.id) else {
            return Residency::OverBudget;
        };
        entry.claimed = request.frame;
        entry.last_used = request.frame;
        if let Some(current) = &entry.current {
            // A change of size waits for the current tiles to have been in use a
            // while, and for one of the few replacement slots. It takes tiles
            // the atlas has free, never another light's. Smaller tiles may make
            // a page when the atlas is past its bound, which only they can
            // bring it back from.
            let resize = current.class != request.class
                && request.now - current.allocated_at >= RESIDENCY_SECONDS
                && entry.pending.is_none()
                && self.replacing < MAX_PENDING;
            if resize {
                let shrinking = request.class < current.class;
                let limit = if shrinking && self.pool.pages() > self.pool.bound() {
                    self.pool.pages() + 1
                } else {
                    self.pool.bound()
                };
                if let Some(tiles) = self.allocate(request.class, request.faces, limit) {
                    self.entries.get_mut(&request.id).unwrap().pending = Some(TileSet {
                        class: request.class,
                        tiles,
                        allocated_at: request.now,
                    });
                    self.replacing += 1;
                }
            }
            return Residency::Resident;
        }
        let bound = self.pool.bound();
        loop {
            let found = (0..=request.class as usize)
                .rev()
                .map(|c| TileClass::ALL[c])
                .find_map(|class| {
                    self.allocate(class, request.faces, bound)
                        .map(|t| (class, t))
                });
            if let Some((class, tiles)) = found {
                self.entries.get_mut(&request.id).unwrap().current = Some(TileSet {
                    class,
                    tiles,
                    allocated_at: request.now,
                });
                return Residency::Resident;
            }
            match self.victim(request) {
                Some(victim) => self.release_tiles(victim),
                None => break,
            }
        }
        match self.allocate(TileClass::Tiny, request.faces, self.pool.ceiling()) {
            Some(tiles) => {
                self.entries.get_mut(&request.id).unwrap().current = Some(TileSet {
                    class: TileClass::Tiny,
                    tiles,
                    allocated_at: request.now,
                });
                Residency::Resident
            }
            None => Residency::OverBudget,
        }
    }

    fn allocate(&mut self, class: TileClass, faces: usize, limit: usize) -> Option<Vec<Tile>> {
        if self.pool.capacity(class, limit) < faces {
            return None;
        }
        let epoch = self.epoch;
        Some(
            (0..faces)
                .map(|_| Tile {
                    at: self.pool.alloc(class, limit).expect("capacity was checked"),
                    built: 0,
                    wanted: epoch,
                })
                .collect(),
        )
    }

    /// Empty a page by moving its tiles onto other pages that have room for
    /// all of them: the last page that holds any, when the others can take it,
    /// so that the pool can give it back, or else the emptiest page, when it is
    /// under a quarter full. The lights on it get replacement tiles of the same
    /// size, which render like any resize, a few lights at a time. This is how
    /// an atlas that was far past its bound comes down after its tiles have
    /// shrunk, and how pages left sparse by lights coming and going go.
    pub fn compact(&mut self, now: f64) {
        if self.replacing >= MAX_PENDING {
            return;
        }
        let typed: Vec<usize> = (0..self.pool.pages())
            .filter(|&p| self.pool.class(p).is_some())
            .collect();
        let movable = |page: usize| {
            let class = self.pool.class(page).unwrap();
            self.pool.capacity_except(class, page) >= self.pool.used_in(page)
        };
        let page = typed
            .last()
            .copied()
            .filter(|&last| movable(last))
            .or_else(|| {
                typed
                    .iter()
                    .copied()
                    .filter(|&p| {
                        self.pool.used_in(p) * 4 <= self.pool.class(p).unwrap().per_page()
                            && movable(p)
                    })
                    .min_by_key(|&p| (self.pool.used_in(p), p))
            });
        let Some(page) = page else { return };
        let mut movers: Vec<u32> = self
            .entries
            .iter()
            .filter(|(_, e)| {
                e.pending.is_none()
                    && e.current
                        .as_ref()
                        .is_some_and(|s| s.tiles.iter().any(|t| usize::from(t.at.page) == page))
            })
            .map(|(&id, _)| id)
            .collect();
        movers.sort_unstable();
        for id in movers.into_iter().take(MAX_PENDING - self.replacing) {
            let set = self.entries[&id].current.as_ref().unwrap();
            let (class, faces) = (set.class, set.tiles.len());
            if self.pool.capacity_except(class, page) < faces {
                continue;
            }
            let epoch = self.epoch;
            let tiles = (0..faces)
                .map(|_| Tile {
                    at: self
                        .pool
                        .alloc_except(class, page)
                        .expect("capacity was checked"),
                    built: 0,
                    wanted: epoch,
                })
                .collect();
            self.entries.get_mut(&id).unwrap().pending = Some(TileSet {
                class,
                tiles,
                allocated_at: now,
            });
            self.replacing += 1;
        }
    }

    /// The least recently used light that is not a candidate this frame.
    fn victim(&self, request: &Request) -> Option<u32> {
        self.entries
            .iter()
            .filter(|(&id, e)| {
                id != request.id
                    && e.candidate != request.frame
                    && (e.current.is_some() || e.pending.is_some())
            })
            .min_by_key(|(&id, e)| (e.last_used, id))
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

    fn atlas(pages: usize) -> ShadowAtlas {
        ShadowAtlas::new(pages, pages + 8)
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
        // A light shrunk for lack of room still asks for what its size on
        // screen calls for.
        assert_eq!(TileClass::for_radius(10.0, Some(Tiny)), Small);
        assert_eq!(TileClass::for_radius(700.0, Some(Mini)), Large);
        assert_eq!(TileClass::for_radius(500.0, Some(Mini)), Medium);
        assert_eq!(
            [Tiny, Mini, Small, Medium, Large].map(TileClass::size),
            [32, 64, 128, 256, 512]
        );
        assert_eq!(
            [Tiny, Mini, Small, Medium, Large].map(TileClass::per_page),
            [1024, 256, 64, 16, 4]
        );
        assert_eq!(Tiny.origin(33), [32, 32]);
        assert_eq!(Medium.origin(5), [256, 256]);
        assert_eq!(Large.origin(3), [512, 512]);
        for class in TileClass::ALL {
            assert_eq!(class.per_row() * class.size(), PAGE_SIZE);
            assert!(class.per_page() <= 1 << SLOT_BITS);
        }
    }

    #[test]
    fn pages_take_the_size_of_their_first_tile_and_return_when_empty() {
        let mut pool = PagePool::new(2, 3);
        assert_eq!(pool.pages(), 0, "pages are made when tiles need them");
        let a = pool.alloc(TileClass::Large, 2).unwrap();
        let b = pool.alloc(TileClass::Large, 2).unwrap();
        assert_eq!((a.page, a.slot, b.page, b.slot), (0, 0, 0, 1));
        assert_eq!(pool.pages(), 1);
        assert_eq!(pool.capacity(TileClass::Large, 2), 2 + 4);
        assert_eq!(pool.capacity(TileClass::Small, 2), 64);
        let c = pool.alloc(TileClass::Small, 2).unwrap();
        assert_eq!(c.page, 1);
        assert_eq!(pool.capacity(TileClass::Medium, 2), 0);
        assert_eq!(pool.capacity(TileClass::Medium, 3), 16);
        assert_eq!(pool.capacity(TileClass::Medium, 9), 16, "the ceiling is 3");
        pool.free(c);
        assert_eq!(pool.capacity(TileClass::Medium, 2), 16);
        pool.free(a);
        pool.free(b);
        assert_eq!(pool.class(0), None);
        assert_eq!(pool.used_tiles(), 0);
        for slot in 0..4 {
            assert_eq!(pool.alloc(TileClass::Large, 2).unwrap().slot, slot);
        }
        assert_eq!(TileRef { page: 3, slot: 9 }.packed(), 3 << 10 | 9);
    }

    #[test]
    fn a_page_of_tiny_tiles_fills_all_its_slots() {
        let mut pool = PagePool::new(1, 1);
        let slots: Vec<u16> = (0..1024)
            .map(|_| pool.alloc(TileClass::Tiny, 1).unwrap().slot)
            .collect();
        assert!(slots.iter().enumerate().all(|(i, &s)| s as usize == i));
        assert!(pool.alloc(TileClass::Tiny, 9).is_none(), "the ceiling is 1");
        pool.free(TileRef { page: 0, slot: 700 });
        assert_eq!(pool.alloc(TileClass::Tiny, 1).unwrap().slot, 700);
    }

    #[test]
    fn point_lights_take_six_tiles_and_spot_lights_one() {
        let mut atlas = atlas(2);
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
        assert_eq!(
            (stats.lights, stats.tiles, stats.pages, stats.created),
            (2, 7, 1, 1)
        );
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
    fn a_full_atlas_shrinks_tiles_and_then_grows_past_its_bound() {
        let mut atlas = atlas(1);
        // One page: 6 large tiles do not fit, so the page becomes a medium one,
        // which holds two point lights (12 of 16 tiles).
        serve(&mut atlas, &[1, 2, 3], TileClass::Large, 1, 0.0);
        assert_eq!(atlas.tiles(1).unwrap().0, TileClass::Medium);
        assert_eq!(atlas.tiles(2).unwrap().0, TileClass::Medium);
        // The third has nowhere within the bound, and no light out of view to
        // take tiles from, so it gets the smallest tiles on a page past it.
        assert_eq!(atlas.tiles(3).unwrap().0, TileClass::Tiny);
        assert_eq!(atlas.stats().created, 2);
        // Lights served earlier in the frame are never displaced for later ones.
        serve(&mut atlas, &[1, 2, 3, 4], TileClass::Small, 2, 0.1);
        assert!([1, 2, 3, 4].iter().all(|&id| atlas.tiles(id).is_some()));
        assert_eq!(atlas.tiles(1).unwrap().0, TileClass::Medium);
    }

    #[test]
    fn a_light_with_no_tile_left_at_the_device_ceiling_is_over_budget() {
        let ask = |id| Request {
            id,
            faces: 6,
            class: TileClass::Tiny,
            frame: 1,
            now: 0.0,
        };
        let mut full = ShadowAtlas::new(1, 1);
        let ids: Vec<u32> = (1..=200).collect();
        let order: Vec<_> = ids.iter().map(|&id| (id, center(id), 12.0)).collect();
        full.candidates(1, &order);
        let results: Vec<_> = ids.iter().map(|&id| full.ensure(&ask(id))).collect();
        // One page holds 170 point lights at 32², and no more.
        assert!(results[..170].iter().all(|&r| r == Residency::Resident));
        assert!(results[170..].iter().all(|&r| r == Residency::OverBudget));
    }

    #[test]
    fn lights_in_view_keep_their_tiles_and_newcomers_get_pages_past_the_bound() {
        let mut atlas = atlas(1);
        // Ten small lights fill a page (60 of 64 tiles).
        let first: Vec<u32> = (1..=10).collect();
        serve(&mut atlas, &first, TileClass::Small, 1, 0.0);
        assert!(first.iter().all(|&id| atlas.tiles(id).is_some()));
        // Light 11 is now the most important and 1..=10 trail it. Nothing in
        // view gives way, however long it has been resident.
        let order: Vec<u32> = std::iter::once(11).chain(first.iter().copied()).collect();
        serve(&mut atlas, &order, TileClass::Small, 2, 5.0);
        assert!(first.iter().all(|&id| atlas.tiles(id).is_some()));
        assert_eq!(atlas.tiles(11).unwrap().0, TileClass::Tiny);
        assert_eq!(atlas.stats().lights, 11);
        assert_eq!(atlas.pool().pages(), 2);
    }

    #[test]
    fn lights_out_of_view_are_evicted_before_visible_ones() {
        let mut atlas = atlas(1);
        let ids: Vec<u32> = (1..=10).collect();
        serve(&mut atlas, &ids, TileClass::Small, 1, 0.0);
        // Only lights 1 and 2 stay in view; light 11 arrives.
        serve(&mut atlas, &[11, 1, 2], TileClass::Small, 2, 0.1);
        assert_eq!(atlas.tiles(11).unwrap().0, TileClass::Small);
        assert!(atlas.tiles(1).is_some() && atlas.tiles(2).is_some());
        let evicted: Vec<_> = ids
            .iter()
            .filter(|&&id| atlas.tiles(id).is_none())
            .collect();
        // The least recently used of the unseen lights goes first, and only as
        // many as needed to free six tiles.
        assert_eq!(evicted.len(), 1, "{evicted:?}");
        assert_eq!(atlas.pool().pages(), 1);
    }

    #[test]
    fn an_edit_in_range_rerenders_a_tile_and_one_out_of_range_does_not() {
        let mut atlas = atlas(2);
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
        let mut atlas = atlas(1);
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
        let mut atlas = atlas(3);
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
        let mut atlas = atlas(2);
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
        assert_eq!(atlas.pool().capacity(TileClass::Large, 2), 4);
    }

    #[test]
    fn a_resize_never_displaces_other_lights() {
        let mut atlas = atlas(1);
        serve(&mut atlas, &[1, 2, 3], TileClass::Small, 1, 0.0);
        render_all(&mut atlas, &[1, 2, 3]);
        serve(&mut atlas, &[1, 2, 3], TileClass::Large, 5, 5.0);
        assert!(atlas.schedule(&[1, 2, 3], 18).is_empty());
        assert!([1, 2, 3]
            .iter()
            .all(|&id| atlas.tiles(id).unwrap().0 == TileClass::Small));
    }

    #[test]
    fn only_a_few_lights_change_size_at_once() {
        let mut atlas = atlas(8);
        let ids: Vec<u32> = (1..=20).collect();
        serve(&mut atlas, &ids, TileClass::Small, 1, 0.0);
        render_all(&mut atlas, &ids);
        // A smaller grant for everyone, as when the bound drops.
        serve(&mut atlas, &ids, TileClass::Mini, 2, 5.0);
        let jobs = atlas.schedule(&ids, usize::MAX);
        let lights: std::collections::HashSet<_> = jobs.iter().map(|j| j.light).collect();
        assert_eq!(lights.len(), MAX_PENDING);
        atlas.swap_finished();
        // The next frame takes the next few.
        serve(&mut atlas, &ids, TileClass::Mini, 3, 5.1);
        let more = atlas.schedule(&ids, usize::MAX);
        assert_eq!(
            more.iter()
                .map(|j| j.light)
                .collect::<std::collections::HashSet<_>>()
                .len(),
            MAX_PENDING
        );
    }

    #[test]
    fn sparse_pages_are_emptied_and_given_back() {
        let mut atlas = atlas(8);
        let ids: Vec<u32> = (1..=40).collect();
        serve(&mut atlas, &ids, TileClass::Small, 1, 0.0);
        render_all(&mut atlas, &ids);
        assert_eq!(atlas.pool().pages(), 4);
        // Every fourth light stays: 10 lights, 60 tiles, spread over 4 pages.
        atlas.forget_absent(|id| id % 4 == 0);
        let kept: Vec<u32> = (1..=10).map(|i| i * 4).collect();
        let mut seen = std::collections::HashSet::new();
        for frame in 2..40 {
            serve(
                &mut atlas,
                &kept,
                TileClass::Small,
                frame,
                f64::from(frame as u32),
            );
            atlas.compact(f64::from(frame as u32));
            // The old tiles keep shadowing until the new ones have rendered.
            assert!(kept.iter().all(|&id| atlas.ready(id)), "frame {frame}");
            atlas.schedule(&kept, usize::MAX);
            atlas.swap_finished();
            atlas.trim();
            seen.insert(atlas.pool().pages());
        }
        assert_eq!(atlas.pool().pages(), 1, "{seen:?}");
        assert_eq!(atlas.stats().tiles, 60);
        assert!(kept.iter().all(|&id| atlas.tiles(id).is_some()));
    }

    #[test]
    fn released_lights_free_their_tiles() {
        let mut atlas = atlas(1);
        serve(&mut atlas, &[1, 2], TileClass::Medium, 1, 0.0);
        assert_eq!(atlas.stats().tiles, 12);
        atlas.forget_absent(|id| id == 2);
        assert_eq!(atlas.stats().tiles, 6);
        assert!(atlas.tiles(1).is_none());
        atlas.release(2);
        let stats = atlas.stats();
        assert_eq!((stats.lights, stats.tiles, stats.pages), (0, 0, 0));
    }
}
