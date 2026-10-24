//! Authoritative room simulation, combat, and collaborative edit history.
//! Networking and world persistence are isolated in their own modules.

mod network;
mod persistence;

pub use network::serve;

use persistence::SavedWorld;
use sauerteig_core::{
    forward, player_intersects_fragment, ray_player, simulate_player, spawn_point,
    validate_root_edits, Cell, CellEdit, CellFragment, CellPos, ClientMessage, FineBox, InputFrame,
    ObjectKind, Player, Projectile, RootEdit, ServerMessage, ShotEffect, Vec3, World, WorldObject,
    FIXED_DT, PLAYER_HEIGHT, PLAYER_RADIUS, WORLD_HEIGHT, WORLD_MAX, WORLD_MIN,
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
const MAX_WORLD_FRAGMENTS: usize =
    ((WORLD_MAX - WORLD_MIN) * (WORLD_MAX - WORLD_MIN) * WORLD_HEIGHT) as usize;
const EDIT_BURST: f64 = 131_072.0;
const EDIT_RATE: f64 = 65_536.0;
const MAX_OBJECTS: usize = 1024;
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

struct UndoEntry {
    revision: u64,
    before: Vec<RootEdit>,
    after: Vec<RootEdit>,
    previous_revisions: Vec<Option<u64>>,
    root_patch: bool,
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

struct Session {
    world: World,
    objects: Vec<WorldObject>,
    revision: u64,
    cell_revisions: HashMap<CellPos, u64>,
    // Unlike undo ownership, this clock never rewinds. A stale root replacement
    // must also be rejected after another builder edited and then cleared it.
    root_changed_at: HashMap<CellPos, u64>,
    geometry_fragments: usize,
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
        let world = World::from_parts(&saved.cells, &saved.roots).expect("validated saved world");
        let geometry_fragments = world.cells().len()
            + world
                .detailed_roots()
                .iter()
                .map(|root| root.fragments.len())
                .sum::<usize>();
        let next_object = saved
            .objects
            .iter()
            .map(|o| o.id)
            .max()
            .unwrap_or(0)
            .saturating_add(1);
        Self {
            world,
            objects: saved.objects,
            revision: saved.revision,
            root_changed_at: cell_revisions.clone(),
            cell_revisions,
            geometry_fragments,
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
            cells: self.world.cells(),
            roots: self.world.detailed_roots(),
            objects: self.objects.clone(),
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
                cells: self.world.cells(),
                roots: self.world.detailed_roots(),
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
            ClientMessage::PlaceObject {
                kind,
                position,
                rotation,
            } => {
                self.require_editor(id)?;
                validate_object_position(position, rotation)?;
                if self.objects.len() >= MAX_OBJECTS {
                    return Err("The room has reached its object limit.".into());
                }
                if self.next_object == u32::MAX {
                    return Err("No object IDs remain.".into());
                }
                self.charge_edit(id, 1)?;
                self.objects.push(WorldObject {
                    id: self.next_object,
                    kind,
                    position,
                    rotation,
                });
                self.next_object += 1;
                self.changed();
                self.broadcast(ServerMessage::Objects {
                    revision: self.revision,
                    objects: self.objects.clone(),
                });
            }
            ClientMessage::RemoveObject { id: object_id } => {
                self.require_editor(id)?;
                self.charge_edit(id, 1)?;
                let old_len = self.objects.len();
                self.objects.retain(|o| o.id != object_id);
                if old_len == self.objects.len() {
                    return Err("That object no longer exists.".into());
                }
                self.changed();
                self.broadcast(ServerMessage::Objects {
                    revision: self.revision,
                    objects: self.objects.clone(),
                });
            }
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
        self.require_editor(id)?;
        validate_root_request(&roots)?;
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
        }
        self.charge_edit(id, roots_complexity(&roots))?;
        self.validate_fighter_overlap(&roots)?;
        let roots: Vec<_> = roots
            .into_iter()
            .filter(|root| self.world.root_fragments(root.pos) != root.fragments)
            .collect();
        if roots.is_empty() {
            return Ok(());
        }
        let mut before = Vec::with_capacity(roots.len());
        let mut before_complexity = 0;
        for root in &roots {
            let fragments = self.world.root_fragments(root.pos);
            before_complexity += fragments.len().max(1);
            if before_complexity > MAX_EDIT_FRAGMENTS {
                return Err("This edit replaces too much detailed geometry at once; use a smaller selection.".into());
            }
            before.push(RootEdit {
                pos: root.pos,
                fragments,
            });
        }
        let previous_revisions: Vec<_> = roots
            .iter()
            .map(|e| self.cell_revisions.get(&e.pos).copied())
            .collect();
        if !self.apply_roots(&roots)? {
            return Ok(());
        }
        let after: Vec<_> = roots
            .iter()
            .map(|e| RootEdit {
                pos: e.pos,
                fragments: self.world.root_fragments(e.pos),
            })
            .collect();
        // Exact clipping can discard a fragment that contains no solid
        // geometry. Such a root must not acquire ownership or an undo entry.
        let mut changed_before = Vec::new();
        let mut changed_after = Vec::new();
        let mut changed_previous = Vec::new();
        for ((before, after), previous) in before.into_iter().zip(after).zip(previous_revisions) {
            if before != after {
                changed_before.push(before);
                changed_after.push(after);
                changed_previous.push(previous);
            }
        }
        let before = changed_before;
        let after = changed_after;
        let previous_revisions = changed_previous;
        let complexity = roots_complexity(&before) + roots_complexity(&after);
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
        undo.push_back(UndoEntry {
            revision: self.revision,
            before,
            after: after.clone(),
            previous_revisions,
            root_patch,
            complexity,
        });
        self.broadcast_roots(after, root_patch);
        Ok(())
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
        self.validate_fighter_overlap(&entry.before)?;
        let edits = entry.before.clone();
        self.charge_edit(id, roots_complexity(&edits))?;
        self.apply_roots(&edits)?;
        let entry = self.peers.get_mut(&id).unwrap().undo.pop_back().unwrap();
        // Restore the exact ownership lineage as well as the value. Blindly
        // assigning an older personal revision would erase intervening edits
        // made by another builder when the user undoes a second time.
        for (e, previous) in edits.iter().zip(&entry.previous_revisions) {
            if let Some(revision) = previous {
                self.cell_revisions.insert(e.pos, *revision);
            } else {
                self.cell_revisions.remove(&e.pos);
            }
        }
        let root_patch = entry.root_patch;
        self.peers.get_mut(&id).unwrap().redo.push(entry);
        self.broadcast_roots(edits, root_patch);
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
        self.validate_fighter_overlap(&entry.after)?;
        let edits = entry.after.clone();
        self.charge_edit(id, roots_complexity(&edits))?;
        self.apply_roots(&edits)?;
        let entry = self.peers.get_mut(&id).unwrap().redo.pop().unwrap();
        // World revisions still increase. Cell ownership restores the original
        // operation token, so a chain of overlapping undos can be replayed while
        // retaining the same protection against another builder's edits.
        for edit in &edits {
            self.cell_revisions.insert(edit.pos, entry.revision);
        }
        let root_patch = entry.root_patch;
        self.peers.get_mut(&id).unwrap().undo.push_back(entry);
        self.broadcast_roots(edits, root_patch);
        Ok(())
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

    fn apply_roots(&mut self, roots: &[RootEdit]) -> Result<bool, String> {
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
        let removed = previous.iter().map(Vec::len).sum::<usize>();
        let added = roots.iter().map(|r| r.fragments.len()).sum::<usize>();
        if self.geometry_fragments - removed + added > MAX_WORLD_FRAGMENTS {
            return Err("The room has reached its detailed geometry limit.".into());
        }
        let changed_chunks = self.world.apply_root_edits(&roots)?;
        if changed_chunks.is_empty() {
            return Ok(false);
        }
        let after: Vec<_> = roots
            .iter()
            .map(|r| self.world.root_fragments(r.pos))
            .collect();
        let actual = after.iter().map(Vec::len).sum::<usize>();
        self.geometry_fragments = self.geometry_fragments - removed + actual;
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
        if !p.is_finite()
            || p.y < 0.0
            || p.y + PLAYER_HEIGHT >= sauerteig_core::WORLD_HEIGHT as f32
            || p.x.abs() + PLAYER_RADIUS >= sauerteig_core::WORLD_MAX as f32
            || p.z.abs() + PLAYER_RADIUS >= sauerteig_core::WORLD_MAX as f32
        {
            return true;
        }
        self.world.intersects_box(
            p - Vec3::new(PLAYER_RADIUS, 0.0, PLAYER_RADIUS),
            p + Vec3::new(PLAYER_RADIUS, PLAYER_HEIGHT, PLAYER_RADIUS),
        )
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
        Vec3::new(0.5, sauerteig_core::WORLD_HEIGHT as f32 - 3.0, 0.5)
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
        let fallen: Vec<_> = self
            .peers
            .iter()
            .filter(|(_, p)| !p.player.editing && p.player.position.y < -8.0)
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
            let visible = distance < 0.05
                || self
                    .world
                    .raycast(
                        center + delta.normalize_or_zero() * 0.03,
                        delta.normalize_or_zero(),
                        (distance - 0.06).max(0.0),
                    )
                    .is_none();
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
