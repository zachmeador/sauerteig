//! Authoritative room simulation, combat, and collaborative edit history.
//! Networking and world persistence are isolated in their own modules.

#[cfg(test)]
mod bots;
mod edits;
mod metrics;
mod network;
mod outbound;
mod persistence;
mod simulation;

pub use network::serve;

use outbound::{cost, Events, Packet, FOLLOW_UP_BYTES};
use persistence::SavedWorld;
use sauerteig_core::{
    forward, player_intersects_fragment, player_intersects_volume, ray_player, simulate_player,
    spawn_point, Cell, CellEdit, CellFragment, CellPos, ClientMessage, FineBox, InputFrame,
    MaterialVolume, ObjectEdit, ObjectKind, ObjectPlacement, Player, Projectile, RootEdit,
    ServerMessage, ShotEffect, Vec3, VolumeEdit, VolumeMaterial, World, WorldObject, FIXED_DT,
    PLAYER_HEIGHT, PLAYER_RADIUS,
};
use std::sync::Arc;
use std::{
    collections::{HashMap, HashSet, VecDeque},
    path::PathBuf,
    time::{Duration, Instant},
};
use tokio::sync::watch;

const MAX_PLAYERS: usize = 16;
const MAX_EDITS: usize = 16_384;
const MAX_EDIT_FRAGMENTS: usize = 65_536;
const MAX_HISTORY_COMPLEXITY: usize = 262_144;
// A full legacy world still fits. Fine detail spends this same occupied-geometry
// budget; it cannot expand all unit roots into hundreds of millions of leaves.
const MAX_WORLD_FRAGMENTS: usize = 786_432;
const EDIT_BURST: f64 = 131_072.0;
const EDIT_RATE: f64 = 65_536.0;
const MAX_OBJECTS: usize = 1024;
// Native ewebsock uses tungstenite's 64 MiB message limit. Geometry uses a
// conservative wire-size budget; reserve room for objects, players and metadata.
const MAX_WORLD_SNAPSHOT_BYTES: usize = 64 * 1024 * 1024;
const SNAPSHOT_ENVELOPE_BYTES: usize = 1024 * 1024;
const MAX_GEOMETRY_BYTES: usize = MAX_WORLD_SNAPSHOT_BYTES - SNAPSHOT_ENVELOPE_BYTES;
const SNAPSHOT_LIMIT_ERROR: &str =
    "This world exceeds the 64 MiB snapshot limit. Reduce geometry detail or import a smaller map.";
const MAX_PROJECTILES: usize = 64;
const MAX_INPUT_BACKLOG: usize = 64;
const SAVE_DELAY: Duration = Duration::from_millis(750);
const SHOT_DISTANCE: f32 = 128.0;

#[derive(Clone, Debug)]
pub struct Config {
    pub world_path: PathBuf,
    pub editing_allowed: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            world_path: ".local/world.json".into(),
            editing_allowed: true,
        }
    }
}

#[derive(Clone)]
struct ObjectState {
    id: u32,
    value: Option<WorldObject>,
}

#[derive(Clone, Copy)]
enum EditPatch {
    Legacy,
    Roots,
    Transaction,
}

/// The reliable message that announces a committed change. The room reserves
/// space for it before committing and broadcasts it afterwards; both start from
/// this choice and from `outbound::cost`, so they cannot disagree about its size.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Publication {
    Transaction,
    Objects,
    WorldPatch,
    RootPatch,
}
impl Publication {
    fn choose(
        patch: EditPatch,
        roots: &[RootEdit],
        object_patch: bool,
        volume_patch: bool,
    ) -> Self {
        if matches!(patch, EditPatch::Transaction) || volume_patch {
            Self::Transaction
        } else if object_patch {
            Self::Objects
        } else if !matches!(patch, EditPatch::Roots)
            && roots.iter().all(|root| root_as_legacy(root).is_some())
        {
            Self::WorldPatch
        } else {
            Self::RootPatch
        }
    }
    /// `objects` is the length of the object list after the change; `volumes` the
    /// length of the replacement layer when the change carries one.
    fn cost(self, roots: &[RootEdit], objects: usize, volumes: Option<usize>) -> usize {
        match self {
            Self::Transaction => cost::transaction_patch(roots, objects, volumes),
            Self::Objects => cost::objects_patch(objects),
            Self::WorldPatch => cost::world_patch(roots.len()),
            Self::RootPatch => cost::root_patch(roots),
        }
    }
}

#[derive(Clone)]
struct UndoEntry {
    revision: u64,
    before: Arc<Vec<RootEdit>>,
    after: Arc<Vec<RootEdit>>,
    previous_revisions: Vec<Option<u64>>,
    patch: EditPatch,
    objects_before: Vec<ObjectState>,
    objects_after: Vec<ObjectState>,
    previous_object_revisions: Vec<Option<u64>>,
    volumes_before: Option<Vec<MaterialVolume>>,
    volumes_after: Option<Vec<MaterialVolume>>,
    previous_volume_revision: Option<u64>,
    complexity: usize,
}

/// History that left the room with its builder. The simulation hands it to the
/// edit worker to free, as it does for replaced worlds.
type Retired = Vec<UndoEntry>;

struct Peer {
    player: Player,
    input: InputFrame,
    inputs: VecDeque<InputFrame>,
    received_input: bool,
    received_seq: u32,
    received_mode: bool,
    last_input_at: Instant,
    next_shot: u64,
    events: Events,
    // Replace old snapshots instead of accumulating stale movement updates.
    snapshots: watch::Sender<Option<Arc<Packet>>>,
    undo: VecDeque<UndoEntry>,
    redo: Vec<UndoEntry>,
    edit_tokens: f64,
    edit_token_time: Instant,
}

struct FlyingRocket {
    data: Projectile,
    ticks_left: u16,
}

#[derive(Clone, Copy, Default)]
struct RootCost {
    fragments: usize,
    bytes: usize,
}
impl RootCost {
    fn new(root: &RootEdit, keep_empty: bool) -> Self {
        if root.fragments.is_empty() && !keep_empty {
            return Self::default();
        }
        Self {
            fragments: root.fragments.len().max(1),
            bytes: json_bytes(root) + 1,
        }
    }
}

struct Session {
    world: World,
    objects: Arc<Vec<WorldObject>>,
    revision: u64,
    cell_revisions: HashMap<CellPos, u64>,
    // Unlike undo ownership, this clock never rewinds. A stale root replacement
    // must also be rejected after another builder edited and then cleared it.
    root_changed_at: HashMap<CellPos, u64>,
    object_revisions: HashMap<u32, u64>,
    object_changed_at: HashMap<u32, u64>,
    volume_revision: Option<u64>,
    volumes_changed_at: u64,
    volume_bytes: usize,
    geometry_fragments: usize,
    geometry_root_costs: HashMap<CellPos, RootCost>,
    geometry_bytes: usize,
    peers: HashMap<u64, Peer>,
    retired: Retired,
    rockets: Vec<FlyingRocket>,
    tick: u64,
    next_player: u64,
    next_projectile: u64,
    next_object: u32,
    editing_allowed: bool,
    dirty: bool,
    last_edit: Instant,
}

impl Session {
    #[cfg(test)]
    fn new(saved: SavedWorld, editing_allowed: bool) -> Self {
        Self::from_saved(saved, editing_allowed).unwrap()
    }

    fn from_saved(saved: SavedWorld, editing_allowed: bool) -> Result<Self, String> {
        let cell_revisions: HashMap<_, _> = saved
            .cells
            .iter()
            .map(|e| (e.pos, saved.revision))
            .chain(saved.roots.iter().map(|e| (e.pos, saved.revision)))
            .collect();
        let mut world =
            World::from_compact_parts(saved.bounds, &saved.regions, &saved.cells, &saved.roots)?;
        world.replace_volumes(&saved.volumes)?;
        let volume_bytes = json_bytes(&world.volumes());
        let volume_revision = (!world.volumes().is_empty()).then_some(saved.revision);
        let geometry_root_costs: HashMap<_, _> = world
            .cells()
            .iter()
            .map(|cell| {
                (
                    cell.pos,
                    RootCost::new(&legacy_root(cell.clone()), world.has_regions()),
                )
            })
            .chain(
                world
                    .root_overrides()
                    .iter()
                    .map(|root| (root.pos, RootCost::new(root, world.has_regions()))),
            )
            .collect();
        let geometry_fragments = world.region_count()
            + geometry_root_costs
                .values()
                .map(|cost| cost.fragments)
                .sum::<usize>();
        let geometry_bytes = world
            .regions()
            .iter()
            .map(|fragment| json_bytes(fragment) + 1)
            .sum::<usize>()
            + geometry_root_costs
                .values()
                .map(|cost| cost.bytes)
                .sum::<usize>();
        let object_revisions: HashMap<_, _> = saved
            .objects
            .iter()
            .map(|o| (o.id, saved.revision))
            .collect();
        let next_object = saved
            .objects
            .iter()
            .map(|o| o.id)
            .max()
            .unwrap_or(0)
            .saturating_add(1)
            .max(saved.next_object);
        Ok(Self {
            world,
            objects: Arc::new(saved.objects),
            revision: saved.revision,
            root_changed_at: cell_revisions.clone(),
            cell_revisions,
            object_changed_at: object_revisions.clone(),
            object_revisions,
            volume_revision,
            volumes_changed_at: saved.revision,
            volume_bytes,
            geometry_fragments,
            geometry_root_costs,
            geometry_bytes,
            peers: HashMap::new(),
            retired: Vec::new(),
            rockets: Vec::new(),
            tick: 0,
            next_player: 1,
            next_projectile: 1,
            next_object,
            editing_allowed,
            dirty: false,
            last_edit: Instant::now(),
        })
    }

    fn saved_snapshot(&self) -> persistence::SavedSnapshot {
        persistence::SavedSnapshot {
            revision: self.revision,
            world: self.world.snapshot(),
            objects: self.objects.clone(),
            next_object: self.next_object,
        }
    }
    #[cfg(test)]
    fn saved(&self) -> SavedWorld {
        self.saved_snapshot().expand()
    }

    fn peer(&self, id: u64) -> Result<&Peer, String> {
        self.peers
            .get(&id)
            .ok_or_else(|| "Session has ended; reconnect.".into())
    }
    fn peer_mut(&mut self, id: u64) -> Result<&mut Peer, String> {
        self.peers
            .get_mut(&id)
            .ok_or_else(|| "Session has ended; reconnect.".into())
    }

    fn changed(&mut self) {
        self.revision += 1;
        self.dirty = true;
        self.last_edit = Instant::now();
    }

    fn players(&self) -> Vec<Player> {
        let mut players: Vec<_> = self.peers.values().map(|p| p.player.clone()).collect();
        players.sort_by_key(|p| p.id);
        players
    }

    fn join(
        &mut self,
        events: Events,
        snapshots: watch::Sender<Option<Arc<Packet>>>,
    ) -> Result<(u64, simulation::Welcome), &'static str> {
        if self.peers.len() >= MAX_PLAYERS {
            return Err("This room is full (16 players).");
        }
        let id = self.next_player;
        self.next_player += 1;
        let mut player = Player::new(id, format!("Builder {id}"));
        player.position = self.safe_spawn(id as u32);
        self.peers.insert(
            id,
            Peer {
                player,
                input: InputFrame::default(),
                inputs: VecDeque::new(),
                received_input: false,
                received_seq: 0,
                received_mode: false,
                last_input_at: Instant::now(),
                next_shot: 0,
                events,
                snapshots,
                undo: VecDeque::new(),
                redo: Vec::new(),
                edit_tokens: EDIT_BURST,
                edit_token_time: Instant::now(),
            },
        );
        Ok((
            id,
            simulation::Welcome {
                id,
                saved: self.saved_snapshot(),
                players: self.players(),
                editing_allowed: self.editing_allowed,
            },
        ))
    }

    /// Removes a peer from the room. Its undo and redo history can be large, so
    /// it is retired for the edit worker to free rather than dropped here.
    fn remove_peer(&mut self, id: u64) {
        if let Some(peer) = self.peers.remove(&id) {
            let Peer { undo, redo, .. } = peer;
            self.retired.extend(undo);
            self.retired.extend(redo);
        }
    }
    /// Removes peers whose connection is gone.
    fn reap(&mut self) {
        let gone: Vec<u64> = self
            .peers
            .iter()
            .filter(|(_, peer)| peer.events.closed())
            .map(|(&id, _)| id)
            .collect();
        for id in gone {
            self.remove_peer(id);
        }
    }

    fn send(&mut self, id: u64, message: ServerMessage) {
        let packet = Packet::new(message);
        if self.peers.get(&id).is_some_and(|p| !p.events.send(packet)) {
            self.remove_peer(id);
        }
    }
    fn broadcast(&mut self, message: ServerMessage) {
        let packet = Packet::new(message);
        let gone: Vec<u64> = self
            .peers
            .iter()
            .filter(|(_, peer)| !peer.events.send(packet.clone()))
            .map(|(&id, _)| id)
            .collect();
        for id in gone {
            self.remove_peer(id);
        }
    }
    /// Whether every connected peer's reliable queue can take `bytes` more.
    fn peers_have_room(&self, bytes: usize) -> bool {
        self.peers
            .values()
            .all(|p| p.events.closed() || p.events.available(bytes))
    }
    /// Leaves exactly `leave` bytes free in a peer's queue, as if a slow writer
    /// were holding the rest.
    #[cfg(test)]
    fn fill_backlog(&self, id: u64, leave: usize) -> bool {
        let Some(peer) = self.peers.get(&id) else {
            return false;
        };
        let Some(bytes) = outbound::PEER_BACKLOG_BYTES.checked_sub(peer.events.used() + leave)
        else {
            return false;
        };
        peer.events.send(Packet::ballast(bytes))
    }
    /// Queues a message the peer's writer cannot encode, so its writer fails.
    #[cfg(test)]
    fn poison_peer(&self, id: u64) -> bool {
        self.peers
            .get(&id)
            .is_some_and(|peer| peer.events.send(Packet::poison()))
    }
    /// Like `fill_backlog`, but the peer's writer is wedged on the filler until
    /// the returned sender is used or dropped.
    #[cfg(test)]
    fn stall_peer(&self, id: u64, leave: usize) -> Option<std::sync::mpsc::Sender<()>> {
        let peer = self.peers.get(&id)?;
        let bytes = outbound::PEER_BACKLOG_BYTES.checked_sub(peer.events.used() + leave)?;
        let (packet, release) = Packet::stall(bytes);
        peer.events.send(packet).then_some(release)
    }
    fn snapshot(&self) {
        let packet = Packet::new(ServerMessage::Snapshot {
            tick: self.tick,
            revision: self.revision,
            players: self.players(),
            projectiles: self.rockets.iter().map(|r| r.data.clone()).collect(),
        });
        for peer in self.peers.values() {
            peer.snapshots.send_replace(Some(packet.clone()));
        }
    }
    #[cfg(test)]
    fn handle(&mut self, id: u64, message: ClientMessage) -> Result<(), String> {
        if simulation::is_edit(&message) {
            let mut edit = self.edit_plan(id, message)?.prepare()?;
            self.commit_prepared(id, &mut edit)
                .map_err(edits::CommitError::message)
        } else {
            self.handle_control(id, message)
        }
    }
    fn handle_control(&mut self, id: u64, message: ClientMessage) -> Result<(), String> {
        if !self.peers.contains_key(&id) {
            return Err("Session has ended; reconnect.".into());
        }
        match message {
            ClientMessage::Hello { name } => {
                let name: String = name
                    .trim()
                    .chars()
                    .filter(|c| !c.is_control())
                    .take(24)
                    .collect();
                if !name.is_empty() {
                    self.peer_mut(id)?.player.name = name;
                }
            }
            ClientMessage::Input { input } => {
                validate_input(&input)?;
                let p = self.peer_mut(id)?;
                let distance = input.seq.wrapping_sub(p.received_seq);
                if !p.received_input || (distance > 0 && distance < (1 << 31)) {
                    if p.inputs.len() == MAX_INPUT_BACKLOG {
                        self.send(
                            id,
                            ServerMessage::Error {
                                message: "Input backlog exceeded; reconnect to resynchronize."
                                    .into(),
                            },
                        );
                        self.remove_peer(id);
                        return Ok(());
                    }
                    p.inputs.push_back(input);
                    p.received_input = true;
                    p.received_seq = input.seq;
                }
            }
            ClientMessage::SetMode { request, editing } => {
                let peer = self.peer(id)?;
                let distance = request.wrapping_sub(peer.player.mode_request);
                if peer.received_mode && (distance == 0 || distance >= (1 << 31)) {
                    self.send(
                        id,
                        ServerMessage::ModeChanged {
                            request,
                            editing: peer.player.editing,
                        },
                    );
                    return Ok(());
                }
                let denied = editing && !self.editing_allowed;
                let p = &self.peer(id)?.player;
                let safe = if !denied && !editing && self.blocked(p) {
                    Some(self.safe_spawn(id as u32))
                } else {
                    None
                };
                let p = self.peer_mut(id)?;
                p.inputs.clear();
                if p.received_input {
                    p.player.last_input = p.received_seq;
                }
                p.input = InputFrame {
                    seq: p.player.last_input,
                    yaw: p.player.yaw,
                    pitch: p.player.pitch,
                    weapon: p.input.weapon,
                    ..Default::default()
                };
                p.received_mode = true;
                p.player.mode_request = request;
                if !denied {
                    p.player.editing = editing;
                }
                p.player.velocity = Vec3::ZERO;
                if let Some(position) = safe {
                    p.player.position = position;
                }
                let actual_mode = p.player.editing;
                self.send(
                    id,
                    ServerMessage::ModeChanged {
                        request,
                        editing: actual_mode,
                    },
                );
                self.snapshot();
                if denied {
                    return Err("Editing is disabled on this server.".into());
                }
            }
            ClientMessage::Sync { request } => self.send(id, ServerMessage::Synced { request }),

            ClientMessage::Save => {
                self.require_editor(id)?;
                self.dirty = true;
                self.last_edit = Instant::now() - SAVE_DELAY;
                self.send(
                    id,
                    ServerMessage::Notice {
                        message: "Save requested.".into(),
                    },
                );
            }
            _ => return Err("Edit requires worker preparation.".into()),
        }
        Ok(())
    }

    fn require_editor(&self, id: u64) -> Result<(), String> {
        if self.revision == u64::MAX {
            return Err("The room has exhausted its edit revisions.".into());
        }
        if !self.editing_allowed || !self.peer(id)?.player.editing {
            return Err("Enter edit mode before changing the world.".into());
        }
        Ok(())
    }

    fn charge_edit(&mut self, id: u64, count: usize) -> Result<(), String> {
        let peer = self.peer_mut(id)?;
        let now = Instant::now();
        peer.edit_tokens = (peer.edit_tokens
            + now.duration_since(peer.edit_token_time).as_secs_f64() * EDIT_RATE)
            .min(EDIT_BURST);
        peer.edit_token_time = now;
        if peer.edit_tokens < count as f64 {
            return Err("Editing too quickly; try a smaller selection.".into());
        }
        peer.edit_tokens -= count as f64;
        Ok(())
    }

    /// How many objects the room would hold after applying `states`.
    fn object_count_after(&self, states: &[ObjectState]) -> usize {
        let removed = states
            .iter()
            .filter(|s| self.objects.iter().any(|o| o.id == s.id))
            .count();
        let added = states.iter().filter(|s| s.value.is_some()).count();
        self.objects.len() - removed + added
    }

    fn validate_object_count(&self, states: &[ObjectState]) -> Result<(), String> {
        if self.object_count_after(states) > MAX_OBJECTS {
            return Err("The room has reached its object limit.".into());
        }
        Ok(())
    }

    fn apply_object_states(&mut self, states: &[ObjectState]) {
        for state in states {
            if let Some(index) = self.objects.iter().position(|o| o.id == state.id) {
                if let Some(object) = &state.value {
                    Arc::make_mut(&mut self.objects)[index] = object.clone();
                } else {
                    Arc::make_mut(&mut self.objects).remove(index);
                }
            } else if let Some(object) = &state.value {
                Arc::make_mut(&mut self.objects).push(object.clone());
            }
            // Deleted IDs keep their ownership token and monotonic clock.
            self.object_revisions.insert(state.id, self.revision);
            self.object_changed_at.insert(state.id, self.revision);
        }
        if !states.is_empty() {
            Arc::make_mut(&mut self.objects).sort_by_key(|o| o.id);
        }
    }

    fn validate_volume_fighter_overlap(&self, volumes: &[MaterialVolume]) -> Result<(), String> {
        for peer in self.peers.values().filter(|peer| !peer.player.editing) {
            if volumes.iter().any(|volume| {
                volume.material == VolumeMaterial::Glass
                    && player_intersects_volume(&peer.player, volume)
            }) {
                return Err("That change would place solid glass inside a player.".into());
            }
        }
        Ok(())
    }

    fn broadcast_change(
        &mut self,
        roots: Vec<RootEdit>,
        patch: EditPatch,
        object_patch: bool,
        volume_patch: bool,
    ) {
        let message = match Publication::choose(patch, &roots, object_patch, volume_patch) {
            Publication::Transaction => ServerMessage::TransactionPatch {
                revision: self.revision,
                roots,
                objects: self.objects.as_ref().clone(),
                volumes: volume_patch.then(|| self.world.volumes().to_vec()),
            },
            Publication::Objects => ServerMessage::Objects {
                revision: self.revision,
                objects: self.objects.as_ref().clone(),
            },
            Publication::WorldPatch => ServerMessage::WorldPatch {
                revision: self.revision,
                edits: roots.iter().filter_map(root_as_legacy).collect(),
            },
            Publication::RootPatch => ServerMessage::RootPatch {
                revision: self.revision,
                roots,
            },
        };
        self.broadcast(message);
    }

    fn validate_fighter_overlap(&self, roots: &[RootEdit]) -> Result<(), String> {
        for root in roots {
            let min = Vec3::from_array(root.pos.map(|v| v as f32));
            let max = min + Vec3::ONE;
            for peer in self.peers.values().filter(|p| !p.player.editing) {
                let feet = peer.player.position;
                let body_min = feet - Vec3::new(PLAYER_RADIUS, 0.0, PLAYER_RADIUS);
                let body_max = feet + Vec3::new(PLAYER_RADIUS, PLAYER_HEIGHT, PLAYER_RADIUS);
                if body_max.cmple(min).any() || body_min.cmpge(max).any() {
                    continue;
                }
                if root
                    .fragments
                    .iter()
                    .any(|f| player_intersects_fragment(&peer.player, f))
                {
                    return Err("That change would place solid geometry inside a player.".into());
                }
            }
        }
        Ok(())
    }

    fn blocked(&self, player: &Player) -> bool {
        let p = player.position;
        let body_min = p - Vec3::new(PLAYER_RADIUS, 0.0, PLAYER_RADIUS);
        let body_max = p + Vec3::new(PLAYER_RADIUS, PLAYER_HEIGHT, PLAYER_RADIUS);
        let bounds = self.world.bounds();
        if !p.is_finite()
            || body_min.cmplt(bounds.world_min()).any()
            || body_max.cmpge(bounds.world_max()).any()
        {
            return true;
        }
        self.world.intersects_box(
            p - Vec3::new(PLAYER_RADIUS, 0.0, PLAYER_RADIUS),
            p + Vec3::new(PLAYER_RADIUS, PLAYER_HEIGHT, PLAYER_RADIUS),
        ) || self.world.intersects_solid_volume(body_min, body_max)
    }

    fn safe_spawn(&self, index: u32) -> Vec3 {
        let mut candidates: Vec<_> = self
            .objects
            .iter()
            .filter(|o| matches!(o.kind, ObjectKind::Spawn))
            .map(|o| o.position)
            .collect();
        if !candidates.is_empty() {
            let offset = index as usize % candidates.len();
            candidates.rotate_left(offset);
        }
        for i in 0..8 {
            candidates.push(spawn_point(index.wrapping_add(i)));
        }
        let mut test = Player::new(0, String::new());
        for lift in [0.0, 2.0, 4.0, 8.0, 16.0, 32.0] {
            for &p in &candidates {
                test.position = p + Vec3::Y * lift;
                if !self.blocked(&test) {
                    return test.position;
                }
            }
        }
        // Last resort for a map whose authored spawn areas were all filled in.
        let bounds = self.world.bounds();
        Vec3::new(
            (bounds.min[0] + bounds.max[0]) as f32 * 0.5,
            bounds.max[1] as f32 - 3.0,
            (bounds.min[2] + bounds.max[2]) as f32 * 0.5,
        )
    }

    fn step(&mut self) {
        self.tick += 1;
        let now = Instant::now();
        for peer in self.peers.values_mut() {
            let input = if let Some(input) = peer.inputs.pop_front() {
                peer.input = input;
                peer.last_input_at = now;
                input
            } else {
                InputFrame {
                    seq: peer.player.last_input,
                    yaw: peer.player.yaw,
                    pitch: peer.player.pitch,
                    weapon: peer.input.weapon,
                    ..Default::default()
                }
            };
            simulate_player(&self.world, &mut peer.player, &input, FIXED_DT);
        }
        let fall_limit = self.world.bounds().min[1] as f32 - 8.0;
        let fallen: Vec<_> = self
            .peers
            .iter()
            .filter(|(_, p)| !p.player.editing && p.player.position.y < fall_limit)
            .map(|(&id, _)| id)
            .collect();
        for id in fallen {
            self.kill(id, id);
        }
        let shooters: Vec<_> = self
            .peers
            .iter()
            .filter(|(_, p)| {
                !p.player.editing
                    && p.input.fire
                    && now.duration_since(p.last_input_at) <= Duration::from_millis(300)
                    && self.tick >= p.next_shot
            })
            .map(|(&id, _)| id)
            .collect();
        let mut shots = Vec::new();
        for id in shooters {
            self.fire(id, &mut shots);
        }
        self.advance_rockets(&mut shots);
        if !shots.is_empty() {
            self.broadcast(ServerMessage::Shots { shots });
        }
        if self.tick.is_multiple_of(3) {
            self.snapshot();
        }
    }

    fn fire(&mut self, id: u64, shots: &mut Vec<ShotEffect>) {
        let Some(peer) = self.peers.get_mut(&id) else {
            return;
        };
        let origin = peer.player.eye();
        let direction = forward(peer.player.yaw, peer.player.pitch);
        let weapon = peer.input.weapon;
        peer.next_shot = self.tick + if weapon == 0 { 12 } else { 45 };
        if weapon == 1 {
            if self.rockets.len() < MAX_PROJECTILES {
                self.rockets.push(FlyingRocket {
                    data: Projectile {
                        id: self.next_projectile,
                        owner: id,
                        position: origin,
                        velocity: direction * 28.0,
                    },
                    ticks_left: 240,
                });
                self.next_projectile += 1;
            }
            return;
        }
        let (distance, target, _) = self.trace(origin, direction, SHOT_DISTANCE, id);
        shots.push(ShotEffect {
            from: origin,
            to: origin + direction * distance,
            owner: id,
            hit: target.is_some(),
        });
        if let Some(target) = target {
            self.damage(target, id, 34);
        }
    }

    fn trace(
        &self,
        origin: Vec3,
        direction: Vec3,
        max_distance: f32,
        owner: u64,
    ) -> (f32, Option<u64>, bool) {
        let world_hit = self.world.raycast(origin, direction, max_distance);
        let mut nearest = world_hit.as_ref().map_or(max_distance, |h| h.distance);
        let mut collided = world_hit.is_some();
        if let Some(glass) = self.world.raycast_solid_volumes(origin, direction, nearest) {
            nearest = glass.distance;
            collided = true;
        }
        let mut target = None;
        for (&id, peer) in &self.peers {
            if id == owner || peer.player.editing {
                continue;
            }
            if let Some(distance) = ray_player(origin, direction, &peer.player, nearest) {
                if distance < nearest {
                    nearest = distance;
                    target = Some(id);
                    collided = true;
                }
            }
        }
        (nearest, target, collided)
    }

    fn advance_rockets(&mut self, shots: &mut Vec<ShotEffect>) {
        let rockets = std::mem::take(&mut self.rockets);
        for mut rocket in rockets {
            let step = rocket.data.velocity * FIXED_DT;
            let distance = step.length();
            let direction = step / distance;
            let (hit_distance, target, collided) =
                self.trace(rocket.data.position, direction, distance, rocket.data.owner);
            rocket.ticks_left -= 1;
            if collided || rocket.ticks_left == 0 {
                let center = rocket.data.position + direction * hit_distance;
                self.explode(center, rocket.data.owner, target);
                shots.push(ShotEffect {
                    from: rocket.data.position,
                    to: center,
                    owner: rocket.data.owner,
                    hit: true,
                });
            } else {
                rocket.data.position += step;
                self.rockets.push(rocket);
            }
        }
    }

    fn explode(&mut self, center: Vec3, owner: u64, direct: Option<u64>) {
        let mut hits = Vec::new();
        for (&id, peer) in &self.peers {
            if peer.player.editing {
                continue;
            }
            let delta = peer.player.position + Vec3::Y * (PLAYER_HEIGHT * 0.5) - center;
            let distance = delta.length();
            if distance >= 3.5 && direct != Some(id) {
                continue;
            }
            let ray_origin = center + delta.normalize_or_zero() * 0.03;
            let ray_direction = delta.normalize_or_zero();
            let ray_distance = (distance - 0.06).max(0.0);
            let visible = distance < 0.05
                || (self
                    .world
                    .raycast(ray_origin, ray_direction, ray_distance)
                    .is_none()
                    && self
                        .world
                        .raycast_solid_volumes(ray_origin, ray_direction, ray_distance)
                        .is_none());
            if !visible && direct != Some(id) {
                continue;
            }
            let damage = if direct == Some(id) {
                100
            } else {
                ((1.0 - distance / 3.5).max(0.0) * 100.0) as u16
            };
            hits.push((id, damage, delta.normalize_or_zero() * 8.0));
        }
        for (id, damage, impulse) in hits {
            if let Some(p) = self.peers.get_mut(&id) {
                p.player.velocity += impulse;
            }
            self.damage(id, owner, damage);
        }
    }

    fn damage(&mut self, target: u64, owner: u64, damage: u16) {
        let Some(peer) = self.peers.get_mut(&target) else {
            return;
        };
        peer.player.health = peer.player.health.saturating_sub(damage);
        if peer.player.health == 0 {
            self.kill(target, owner);
        }
    }

    fn kill(&mut self, target: u64, owner: u64) {
        if owner == target {
            if let Some(p) = self.peers.get_mut(&target) {
                p.player.score -= 1;
            }
        } else if let Some(p) = self.peers.get_mut(&owner) {
            p.player.score += 1;
        }
        let position = self.safe_spawn((target as u32).wrapping_add(self.tick as u32));
        if let Some(peer) = self.peers.get_mut(&target) {
            peer.player.position = position;
            peer.player.velocity = Vec3::ZERO;
            peer.player.health = 100;
            peer.input.fire = false;
            peer.next_shot = self.tick + 30;
        }
    }
}

fn validate_input(input: &InputFrame) -> Result<(), String> {
    if [input.forward, input.strafe, input.vertical]
        .iter()
        .any(|x| !x.is_finite() || x.abs() > 1.0)
        || !input.yaw.is_finite()
        || input.yaw.abs() > 1_000_000.0
        || !input.pitch.is_finite()
        || input.pitch.abs() > std::f32::consts::FRAC_PI_2
        || input.weapon > 1
    {
        return Err("Invalid input values.".into());
    }
    Ok(())
}

fn validate_cell(cell: Cell) -> bool {
    cell.is_valid()
}

fn legacy_root(edit: CellEdit) -> RootEdit {
    let origin = edit.pos.map(|v| v * 8);
    RootEdit {
        pos: edit.pos,
        fragments: edit
            .cell
            .into_iter()
            .map(|cell| CellFragment {
                bounds: FineBox {
                    min: origin,
                    max: origin.map(|v| v + 8),
                },
                origin,
                size: 8,
                cell,
            })
            .collect(),
    }
}

fn root_as_legacy(root: &RootEdit) -> Option<CellEdit> {
    let cell = match root.fragments.as_slice() {
        [] => None,
        [fragment] => {
            let origin = root.pos.map(|v| v * 8);
            if fragment.origin != origin
                || fragment.size != 8
                || fragment.bounds.min != origin
                || fragment.bounds.max != origin.map(|v| v + 8)
            {
                return None;
            }
            Some(fragment.cell)
        }
        _ => return None,
    };
    Some(CellEdit {
        pos: root.pos,
        cell,
    })
}

fn roots_complexity(roots: &[RootEdit]) -> usize {
    // Empty roots still represent work and occupy undo/request storage.
    roots.iter().map(|root| root.fragments.len().max(1)).sum()
}

fn validate_root_request(roots: &[RootEdit]) -> Result<(), String> {
    if roots.is_empty() || roots.len() > MAX_EDITS {
        return Err("An edit must contain 1–16,384 sparse roots.".into());
    }
    if roots_complexity(roots) > MAX_EDIT_FRAGMENTS {
        return Err(
            "An edit may contain at most 65,536 geometry fragments; use a smaller selection."
                .into(),
        );
    }
    Ok(())
}

fn validate_object_position(position: Vec3, rotation: f32) -> Result<(), String> {
    if !position.is_finite()
        || !rotation.is_finite()
        || rotation.abs() > 1_000_000.0
        || !World::is_valid_pos(position.floor().as_ivec3().to_array())
    {
        return Err("Object must be inside the world with a finite rotation.".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests;

/// Count JSON without allocating a complete copy of a large map or root.
fn json_bytes(value: &impl serde::Serialize) -> usize {
    struct Counter(usize);
    impl std::io::Write for Counter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0 += bytes.len();
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut counter = Counter(0);
    if serde_json::to_writer(&mut counter, value).is_err() {
        return MAX_WORLD_SNAPSHOT_BYTES;
    }
    counter.0
}

fn snapshot_geometry_bytes(
    regions: &[CellFragment],
    cells: &[CellEdit],
    roots: &[RootEdit],
) -> usize {
    // RootEdit is more verbose than a legacy coarse cell, so this bounds both
    // snapshot encodings and remains valid when a cell becomes a detailed root.
    regions
        .iter()
        .map(|fragment| json_bytes(fragment) + 1)
        .sum::<usize>()
        + cells
            .iter()
            .map(|cell| json_bytes(&legacy_root(cell.clone())) + 1)
            .sum::<usize>()
        + roots
            .iter()
            .map(|root| {
                let normalized = RootEdit {
                    pos: root.pos,
                    fragments: sauerteig_core::canonical_root_fragments(root.pos, &root.fragments),
                };
                json_bytes(&normalized) + 1
            })
            .sum::<usize>()
}

fn validate_snapshot_geometry(
    regions: &[CellFragment],
    cells: &[CellEdit],
    roots: &[RootEdit],
) -> Result<(), String> {
    if snapshot_geometry_bytes(regions, cells, roots) > MAX_GEOMETRY_BYTES {
        return Err(SNAPSHOT_LIMIT_ERROR.into());
    }
    Ok(())
}
