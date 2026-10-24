//! Authoritative room simulation, combat, and collaborative edit history.
//! Networking and world persistence are isolated in their own modules.

mod network;
mod persistence;

pub use network::serve;

use persistence::SavedWorld;
use sauerteig_core::{
    forward, player_intersects_fragment, player_intersects_volume, ray_player, simulate_player,
    spawn_point, validate_root_edits, Cell, CellEdit, CellFragment, CellPos, ClientMessage,
    FineBox, InputFrame, MaterialVolume, ObjectEdit, ObjectKind, ObjectPlacement, Player,
    Projectile, RootEdit, ServerMessage, ShotEffect, Vec3, VolumeEdit, VolumeMaterial, World,
    WorldObject, FIXED_DT, PLAYER_HEIGHT, PLAYER_RADIUS, WORLD_FORMAT_VERSION,
};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    path::PathBuf,
    time::{Duration, Instant},
};
use tokio::sync::{mpsc, watch};

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
    pub web_dir: PathBuf,
    pub editing_allowed: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            world_path: ".local/world.json".into(),
            web_dir: "web/dist".into(),
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

struct UndoEntry {
    revision: u64,
    before: Vec<RootEdit>,
    after: Vec<RootEdit>,
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

struct Peer {
    player: Player,
    input: InputFrame,
    inputs: VecDeque<InputFrame>,
    received_input: bool,
    received_seq: u32,
    received_mode: bool,
    last_input_at: Instant,
    next_shot: u64,
    events: mpsc::Sender<String>,
    // Replace old snapshots instead of accumulating stale movement updates.
    snapshots: watch::Sender<String>,
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
    objects: Vec<WorldObject>,
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
    fn new(saved: SavedWorld, editing_allowed: bool) -> Self {
        let cell_revisions: HashMap<_, _> = saved
            .cells
            .iter()
            .map(|e| (e.pos, saved.revision))
            .chain(saved.roots.iter().map(|e| (e.pos, saved.revision)))
            .collect();
        let mut world =
            World::from_compact_parts(saved.bounds, &saved.regions, &saved.cells, &saved.roots)
                .expect("validated saved world");
        world
            .replace_volumes(&saved.volumes)
            .expect("validated saved material volumes");
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
        Self {
            world,
            objects: saved.objects,
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
            rockets: Vec::new(),
            tick: 0,
            next_player: 1,
            next_projectile: 1,
            next_object,
            editing_allowed,
            dirty: false,
            last_edit: Instant::now(),
        }
    }

    fn saved(&self) -> SavedWorld {
        SavedWorld {
            revision: self.revision,
            format_version: WORLD_FORMAT_VERSION,
            bounds: self.world.bounds(),
            regions: self.world.regions(),
            cells: self.world.cells(),
            roots: self.world.root_overrides(),
            volumes: self.world.volumes().to_vec(),
            objects: self.objects.clone(),
            next_object: self.next_object,
        }
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
        events: mpsc::Sender<String>,
        snapshots: watch::Sender<String>,
    ) -> Result<(u64, ServerMessage), &'static str> {
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
            ServerMessage::Welcome {
                id,
                revision: self.revision,
                format_version: WORLD_FORMAT_VERSION,
                bounds: self.world.bounds(),
                regions: self.world.regions(),
                cells: self.world.cells(),
                roots: self.world.root_overrides(),
                volumes: self.world.volumes().to_vec(),
                objects: self.objects.clone(),
                players: self.players(),
                editing_allowed: self.editing_allowed,
            },
        ))
    }

    fn send(&mut self, id: u64, message: ServerMessage) {
        let text = serde_json::to_string(&message).expect("finite server message");
        if self
            .peers
            .get(&id)
            .is_some_and(|p| p.events.try_send(text).is_err())
        {
            // Reliable edits cannot be silently dropped. A slow peer must rejoin.
            self.peers.remove(&id);
        }
    }

    fn broadcast(&mut self, message: ServerMessage) {
        let text = serde_json::to_string(&message).expect("finite server message");
        self.peers
            .retain(|_, p| p.events.try_send(text.clone()).is_ok());
    }

    fn snapshot(&self) {
        let message = ServerMessage::Snapshot {
            tick: self.tick,
            revision: self.revision,
            players: self.players(),
            projectiles: self.rockets.iter().map(|r| r.data.clone()).collect(),
        };
        let text = serde_json::to_string(&message).expect("finite snapshot");
        for peer in self.peers.values() {
            peer.snapshots.send_replace(text.clone());
        }
    }

    fn handle(&mut self, id: u64, message: ClientMessage) -> Result<(), String> {
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
                    self.peers.get_mut(&id).unwrap().player.name = name;
                }
            }
            ClientMessage::Input { input } => {
                validate_input(&input)?;
                let p = self.peers.get_mut(&id).unwrap();
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
                        self.peers.remove(&id);
                        return Ok(());
                    }
                    p.inputs.push_back(input);
                    p.received_input = true;
                    p.received_seq = input.seq;
                }
            }
            ClientMessage::SetMode { request, editing } => {
                let peer = &self.peers[&id];
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
                let p = &self.peers[&id].player;
                let safe = if !denied && !editing && self.blocked(p) {
                    Some(self.safe_spawn(id as u32))
                } else {
                    None
                };
                let p = self.peers.get_mut(&id).unwrap();
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
            ClientMessage::Edit { edits, .. } => self.edit(id, edits)?,
            ClientMessage::EditRoots {
                base_revision,
                roots,
                ..
            } => {
                self.edit_roots(id, base_revision, roots)?;
            }
            ClientMessage::EditTransaction {
                base_revision,
                roots,
                objects,
                volumes,
                ..
            } => {
                self.commit_transaction(
                    id,
                    roots,
                    objects,
                    volumes,
                    Some(base_revision),
                    EditPatch::Transaction,
                )?;
            }
            ClientMessage::PlaceObject {
                kind,
                position,
                rotation,
            } => {
                self.commit_transaction(
                    id,
                    vec![],
                    vec![ObjectEdit::Create {
                        object: ObjectPlacement {
                            kind,
                            position,
                            rotation,
                        },
                    }],
                    vec![],
                    None,
                    EditPatch::Legacy,
                )?;
            }
            ClientMessage::RemoveObject { id: object_id } => {
                self.commit_transaction(
                    id,
                    vec![],
                    vec![ObjectEdit::Delete { id: object_id }],
                    vec![],
                    None,
                    EditPatch::Legacy,
                )?;
            }
            ClientMessage::Sync { request } => self.send(id, ServerMessage::Synced { request }),
            ClientMessage::Undo => self.undo(id)?,
            ClientMessage::Redo => self.redo(id)?,
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
        }
        Ok(())
    }

    fn require_editor(&self, id: u64) -> Result<(), String> {
        if self.revision == u64::MAX {
            return Err("The room has exhausted its edit revisions.".into());
        }
        if !self.editing_allowed || !self.peers[&id].player.editing {
            return Err("Enter edit mode before changing the world.".into());
        }
        Ok(())
    }

    fn charge_edit(&mut self, id: u64, count: usize) -> Result<(), String> {
        let peer = self.peers.get_mut(&id).unwrap();
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

    fn edit(&mut self, id: u64, edits: Vec<CellEdit>) -> Result<(), String> {
        validate_edits(&edits)?;
        self.commit_roots(
            id,
            edits.into_iter().map(legacy_root).collect(),
            None,
            false,
        )
    }

    fn edit_roots(
        &mut self,
        id: u64,
        base_revision: u64,
        roots: Vec<RootEdit>,
    ) -> Result<(), String> {
        self.commit_roots(id, roots, Some(base_revision), true)
    }

    fn commit_roots(
        &mut self,
        id: u64,
        roots: Vec<RootEdit>,
        base_revision: Option<u64>,
        root_patch: bool,
    ) -> Result<(), String> {
        validate_root_request(&roots)?;
        self.commit_transaction(
            id,
            roots,
            vec![],
            vec![],
            base_revision,
            if root_patch {
                EditPatch::Roots
            } else {
                EditPatch::Legacy
            },
        )
    }

    fn commit_transaction(
        &mut self,
        id: u64,
        roots: Vec<RootEdit>,
        objects: Vec<ObjectEdit>,
        volumes: Vec<VolumeEdit>,
        base_revision: Option<u64>,
        patch: EditPatch,
    ) -> Result<(), String> {
        self.require_editor(id)?;
        if roots.is_empty() && objects.is_empty() && volumes.is_empty() {
            return Err("An edit must change geometry, objects, or material volumes.".into());
        }
        if !roots.is_empty() {
            validate_root_request(&roots)?;
        }
        if roots.iter().any(|root| !self.world.contains_pos(root.pos)) {
            return Err("These cells are outside the world bounds.".into());
        }
        if objects.len() > MAX_OBJECTS * 2 {
            return Err("An edit may contain at most 2,048 object changes.".into());
        }
        if let Some(base_revision) = base_revision {
            if base_revision > self.revision {
                return Err("Edit revision is ahead of the server.".into());
            }
            if roots.iter().any(|root| {
                self.root_changed_at
                    .get(&root.pos)
                    .is_some_and(|&r| r > base_revision)
            }) {
                return Err("These cells changed after the edit began; try again.".into());
            }
            if !volumes.is_empty() && self.volumes_changed_at > base_revision {
                return Err("Material volumes changed after the edit began; try again.".into());
            }
        }
        let volumes_after = if volumes.is_empty() {
            None
        } else {
            let after = self.world.prepare_volume_edits(&volumes)?;
            (after != self.world.volumes()).then_some(after)
        };
        let volume_bytes = volumes_after.as_ref().map_or(self.volume_bytes, json_bytes);
        let volumes_before = volumes_after
            .as_ref()
            .map(|_| self.world.volumes().to_vec());
        if let Some(after) = &volumes_after {
            self.validate_volume_fighter_overlap(after)?;
        }
        // Resolve every object before mutating either part of the world. IDs
        // are reserved locally and become consumed only on a successful commit.
        let mut next_object = self.next_object;
        let mut seen = HashSet::new();
        let mut objects_before = Vec::new();
        let mut objects_after = Vec::new();
        for edit in objects {
            let (object_id, placement) = match edit {
                ObjectEdit::Create { object } => {
                    if next_object == u32::MAX {
                        return Err("No object IDs remain.".into());
                    }
                    let object_id = next_object;
                    next_object += 1;
                    (object_id, Some(object))
                }
                ObjectEdit::Update { id, object } => {
                    if !self.objects.iter().any(|o| o.id == id) {
                        return Err("That object no longer exists.".into());
                    }
                    (id, Some(object))
                }
                ObjectEdit::Delete { id } => {
                    if !self.objects.iter().any(|o| o.id == id) {
                        return Err("That object no longer exists.".into());
                    }
                    (id, None)
                }
            };
            if !seen.insert(object_id) {
                return Err("An object may be changed only once per edit.".into());
            }
            if base_revision.is_some_and(|base| {
                self.object_changed_at
                    .get(&object_id)
                    .is_some_and(|&r| r > base)
            }) {
                return Err("This object changed after the edit began; try again.".into());
            }
            let value = if let Some(object) = placement {
                validate_object_position(object.position, object.rotation)?;
                if !self
                    .world
                    .contains_pos(object.position.floor().as_ivec3().to_array())
                {
                    return Err("Object must be inside the world bounds.".into());
                }
                Some(WorldObject {
                    id: object_id,
                    kind: object.kind,
                    position: object.position,
                    rotation: object.rotation,
                })
            } else {
                None
            };
            let before = self.objects.iter().find(|o| o.id == object_id).cloned();
            if before != value {
                objects_before.push(ObjectState {
                    id: object_id,
                    value: before,
                });
                objects_after.push(ObjectState {
                    id: object_id,
                    value,
                });
            }
        }
        self.validate_object_count(&objects_after)?;
        let roots: Vec<_> = roots
            .into_iter()
            .map(|root| RootEdit {
                pos: root.pos,
                fragments: sauerteig_core::canonical_root_fragments(root.pos, &root.fragments),
            })
            .filter(|root| self.world.root_fragments(root.pos) != root.fragments)
            .collect();
        if roots.is_empty() && objects_after.is_empty() && volumes_after.is_none() {
            return Ok(());
        }
        self.validate_fighter_overlap(&roots)?;
        let before: Vec<_> = roots
            .iter()
            .map(|root| RootEdit {
                pos: root.pos,
                fragments: self.world.root_fragments(root.pos),
            })
            .collect();
        if roots_complexity(&before) > MAX_EDIT_FRAGMENTS {
            return Err(
                "This edit replaces too much detailed geometry at once; use a smaller selection."
                    .into(),
            );
        }
        let previous_revisions: Vec<_> = roots
            .iter()
            .map(|e| self.cell_revisions.get(&e.pos).copied())
            .collect();
        let previous_object_revisions = objects_before
            .iter()
            .map(|o| self.object_revisions.get(&o.id).copied())
            .collect();
        self.charge_edit(
            id,
            roots_complexity(&roots) + objects_after.len() + volumes.len(),
        )?;
        let geometry_changed = self.apply_roots(&roots, volume_bytes)?;
        if !geometry_changed && objects_after.is_empty() && volumes_after.is_none() {
            return Ok(());
        }
        if !geometry_changed {
            self.changed();
        }
        let mut changed_before = Vec::new();
        let mut changed_after = Vec::new();
        let mut changed_previous = Vec::new();
        for (before, previous) in before.into_iter().zip(previous_revisions) {
            let after = RootEdit {
                pos: before.pos,
                fragments: self.world.root_fragments(before.pos),
            };
            if before != after {
                changed_before.push(before);
                changed_after.push(after);
                changed_previous.push(previous);
            }
        }
        let (before, roots, previous_revisions) = (changed_before, changed_after, changed_previous);
        self.apply_object_states(&objects_after);
        let previous_volume_revision = self.volume_revision;
        if let Some(after) = &volumes_after {
            self.apply_volumes(after);
        }
        self.next_object = next_object;
        let complexity = roots_complexity(&before)
            + roots_complexity(&roots)
            + objects_before.len() * 2
            + volumes_before.as_ref().map_or(0, Vec::len)
            + volumes_after.as_ref().map_or(0, Vec::len);
        let entry = UndoEntry {
            revision: self.revision,
            before,
            after: roots.clone(),
            previous_revisions,
            patch,
            objects_before,
            objects_after,
            previous_object_revisions,
            volumes_before,
            volumes_after,
            previous_volume_revision,
            complexity,
        };
        let peer = self.peers.get_mut(&id).unwrap();
        peer.redo.clear();
        let undo = &mut peer.undo;
        let mut history_complexity = undo.iter().map(|entry| entry.complexity).sum::<usize>();
        while undo.len() >= 32 || history_complexity + complexity > MAX_HISTORY_COMPLEXITY {
            let Some(discarded) = undo.pop_front() else {
                break;
            };
            history_complexity -= discarded.complexity;
        }
        let object_patch = !entry.objects_after.is_empty();
        let volume_patch = entry.volumes_after.is_some();
        undo.push_back(entry);
        self.broadcast_change(roots, patch, object_patch, volume_patch);
        Ok(())
    }

    fn validate_object_count(&self, states: &[ObjectState]) -> Result<(), String> {
        let removed = states
            .iter()
            .filter(|s| self.objects.iter().any(|o| o.id == s.id))
            .count();
        let added = states.iter().filter(|s| s.value.is_some()).count();
        if self.objects.len() - removed + added > MAX_OBJECTS {
            return Err("The room has reached its object limit.".into());
        }
        Ok(())
    }

    fn apply_object_states(&mut self, states: &[ObjectState]) {
        for state in states {
            if let Some(index) = self.objects.iter().position(|o| o.id == state.id) {
                if let Some(object) = &state.value {
                    self.objects[index] = object.clone();
                } else {
                    self.objects.remove(index);
                }
            } else if let Some(object) = &state.value {
                self.objects.push(object.clone());
            }
            // Deleted IDs keep their ownership token and monotonic clock.
            self.object_revisions.insert(state.id, self.revision);
            self.object_changed_at.insert(state.id, self.revision);
        }
        self.objects.sort_by_key(|o| o.id);
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

    fn apply_volumes(&mut self, volumes: &[MaterialVolume]) {
        self.world
            .replace_volumes(volumes)
            .expect("previously validated material volume state");
        self.volume_bytes = json_bytes(&self.world.volumes());
        self.volume_revision = Some(self.revision);
        self.volumes_changed_at = self.revision;
    }

    fn undo(&mut self, id: u64) -> Result<(), String> {
        self.require_editor(id)?;
        let Some(entry) = self.peers[&id].undo.back() else {
            return Err("Nothing to undo.".into());
        };
        if entry
            .before
            .iter()
            .any(|e| self.cell_revisions.get(&e.pos) != Some(&entry.revision))
        {
            return Err("Cannot undo: a later edit changed these cells.".into());
        }
        if entry
            .objects_before
            .iter()
            .any(|o| self.object_revisions.get(&o.id) != Some(&entry.revision))
        {
            return Err("Cannot undo: a later edit changed these objects.".into());
        }
        if entry.volumes_before.is_some() && self.volume_revision != Some(entry.revision) {
            return Err("Cannot undo: a later edit changed material volumes.".into());
        }
        self.validate_fighter_overlap(&entry.before)?;
        self.validate_object_count(&entry.objects_before)?;
        if let Some(volumes) = &entry.volumes_before {
            self.validate_volume_fighter_overlap(volumes)?;
        }
        let edits = entry.before.clone();
        let objects = entry.objects_before.clone();
        let volumes = entry.volumes_before.clone();
        let volume_bytes = volumes.as_ref().map_or(self.volume_bytes, json_bytes);
        self.charge_edit(
            id,
            roots_complexity(&edits) + objects.len() + volumes.as_ref().map_or(0, Vec::len),
        )?;
        if !self.apply_roots(&edits, volume_bytes)? {
            self.changed();
        }
        self.apply_object_states(&objects);
        if let Some(volumes) = &volumes {
            self.apply_volumes(volumes);
        }
        let entry = self.peers.get_mut(&id).unwrap().undo.pop_back().unwrap();
        // Restore ownership lineage; monotonic stale-snapshot clocks never rewind.
        for (e, previous) in edits.iter().zip(&entry.previous_revisions) {
            if let Some(revision) = previous {
                self.cell_revisions.insert(e.pos, *revision);
            } else {
                self.cell_revisions.remove(&e.pos);
            }
        }
        for (object, previous) in objects.iter().zip(&entry.previous_object_revisions) {
            if let Some(revision) = previous {
                self.object_revisions.insert(object.id, *revision);
            } else {
                self.object_revisions.remove(&object.id);
            }
        }
        if volumes.is_some() {
            self.volume_revision = entry.previous_volume_revision;
        }
        let patch = entry.patch;
        self.peers.get_mut(&id).unwrap().redo.push(entry);
        self.broadcast_change(edits, patch, !objects.is_empty(), volumes.is_some());
        Ok(())
    }

    fn redo(&mut self, id: u64) -> Result<(), String> {
        self.require_editor(id)?;
        let Some(entry) = self.peers[&id].redo.last() else {
            return Err("Nothing to redo.".into());
        };
        if entry
            .before
            .iter()
            .zip(&entry.previous_revisions)
            .any(|(e, previous)| self.cell_revisions.get(&e.pos).copied() != *previous)
        {
            return Err("Cannot redo: a later edit changed these cells.".into());
        }
        if entry
            .objects_before
            .iter()
            .zip(&entry.previous_object_revisions)
            .any(|(o, previous)| self.object_revisions.get(&o.id).copied() != *previous)
        {
            return Err("Cannot redo: a later edit changed these objects.".into());
        }
        if entry.volumes_after.is_some() && self.volume_revision != entry.previous_volume_revision {
            return Err("Cannot redo: a later edit changed material volumes.".into());
        }
        self.validate_fighter_overlap(&entry.after)?;
        self.validate_object_count(&entry.objects_after)?;
        if let Some(volumes) = &entry.volumes_after {
            self.validate_volume_fighter_overlap(volumes)?;
        }
        let edits = entry.after.clone();
        let objects = entry.objects_after.clone();
        let volumes = entry.volumes_after.clone();
        let volume_bytes = volumes.as_ref().map_or(self.volume_bytes, json_bytes);
        self.charge_edit(
            id,
            roots_complexity(&edits) + objects.len() + volumes.as_ref().map_or(0, Vec::len),
        )?;
        if !self.apply_roots(&edits, volume_bytes)? {
            self.changed();
        }
        self.apply_object_states(&objects);
        if let Some(volumes) = &volumes {
            self.apply_volumes(volumes);
        }
        let entry = self.peers.get_mut(&id).unwrap().redo.pop().unwrap();
        for edit in &edits {
            self.cell_revisions.insert(edit.pos, entry.revision);
        }
        for object in &objects {
            self.object_revisions.insert(object.id, entry.revision);
        }
        if volumes.is_some() {
            self.volume_revision = Some(entry.revision);
        }
        let patch = entry.patch;
        self.peers.get_mut(&id).unwrap().undo.push_back(entry);
        self.broadcast_change(edits, patch, !objects.is_empty(), volumes.is_some());
        Ok(())
    }

    fn broadcast_change(
        &mut self,
        roots: Vec<RootEdit>,
        patch: EditPatch,
        object_patch: bool,
        volume_patch: bool,
    ) {
        if matches!(patch, EditPatch::Transaction) || volume_patch {
            self.broadcast(ServerMessage::TransactionPatch {
                revision: self.revision,
                roots,
                objects: self.objects.clone(),
                volumes: volume_patch.then(|| self.world.volumes().to_vec()),
            });
        } else if object_patch {
            self.broadcast(ServerMessage::Objects {
                revision: self.revision,
                objects: self.objects.clone(),
            });
        } else {
            self.broadcast_roots(roots, matches!(patch, EditPatch::Roots));
        }
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

    fn apply_roots(&mut self, roots: &[RootEdit], volume_bytes: usize) -> Result<bool, String> {
        // Requests were validated before reaching this method; history entries
        // are previously accepted state. Charge the room budget for the same
        // canonical representation that the world will store, not its authored
        // subdivision count.
        let roots: Vec<_> = roots
            .iter()
            .map(|root| RootEdit {
                pos: root.pos,
                fragments: sauerteig_core::canonical_root_fragments(root.pos, &root.fragments),
            })
            .collect();
        let previous: Vec<_> = roots
            .iter()
            .map(|r| self.world.root_fragments(r.pos))
            .collect();
        let removed: Vec<_> = roots
            .iter()
            .map(|root| {
                self.geometry_root_costs
                    .get(&root.pos)
                    .copied()
                    .unwrap_or_default()
            })
            .collect();
        let added: Vec<_> = roots
            .iter()
            .map(|root| RootCost::new(root, self.world.has_regions()))
            .collect();
        if self.geometry_fragments - removed.iter().map(|cost| cost.fragments).sum::<usize>()
            + added.iter().map(|cost| cost.fragments).sum::<usize>()
            > MAX_WORLD_FRAGMENTS
        {
            return Err("The room has reached its detailed geometry limit.".into());
        }
        if self.geometry_bytes - removed.iter().map(|cost| cost.bytes).sum::<usize>()
            + added.iter().map(|cost| cost.bytes).sum::<usize>()
            + volume_bytes
            > MAX_GEOMETRY_BYTES
        {
            return Err(SNAPSHOT_LIMIT_ERROR.into());
        }
        self.world.apply_root_edits(&roots)?;
        let after: Vec<_> = roots
            .iter()
            .map(|r| self.world.root_fragments(r.pos))
            .collect();
        let mut changed = false;
        for ((root, before), after) in roots.iter().zip(&previous).zip(&after) {
            if before != after {
                changed = true;
                let cost = RootCost::new(
                    &RootEdit {
                        pos: root.pos,
                        fragments: after.clone(),
                    },
                    self.world.has_regions(),
                );
                let old = if cost.fragments == 0 {
                    self.geometry_root_costs
                        .remove(&root.pos)
                        .unwrap_or_default()
                } else {
                    self.geometry_root_costs
                        .insert(root.pos, cost)
                        .unwrap_or_default()
                };
                self.geometry_fragments = self.geometry_fragments - old.fragments + cost.fragments;
                self.geometry_bytes = self.geometry_bytes - old.bytes + cost.bytes;
            }
        }
        if !changed {
            return Ok(false);
        }
        self.changed();
        for ((root, before), after) in roots.iter().zip(previous).zip(after) {
            if before != after {
                self.cell_revisions.insert(root.pos, self.revision);
                self.root_changed_at.insert(root.pos, self.revision);
            }
        }
        Ok(true)
    }

    fn broadcast_roots(&mut self, roots: Vec<RootEdit>, root_patch: bool) {
        if !root_patch {
            if let Some(edits) = roots.iter().map(root_as_legacy).collect::<Option<Vec<_>>>() {
                self.broadcast(ServerMessage::WorldPatch {
                    revision: self.revision,
                    edits,
                });
                return;
            }
        }
        self.broadcast(ServerMessage::RootPatch {
            revision: self.revision,
            roots,
        });
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
        let peer = self.peers.get_mut(&id).unwrap();
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
    validate_root_edits(roots)
}

fn validate_edits(edits: &[CellEdit]) -> Result<(), String> {
    if edits.is_empty() || edits.len() > MAX_EDITS {
        return Err("An edit must contain 1–16,384 cells.".into());
    }
    let mut seen = HashSet::new();
    for e in edits {
        if !World::is_valid_pos(e.pos)
            || e.cell.is_some_and(|c| !validate_cell(c))
            || !seen.insert(e.pos)
        {
            return Err("Invalid cell position, shape, material, or duplicate cell.".into());
        }
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
    serde_json::to_writer(&mut counter, value).expect("valid world JSON");
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
