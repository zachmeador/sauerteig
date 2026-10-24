//! Single-room authoritative prototype server. WebSocket is a bootstrap transport;
//! this does not establish suitable WAN FPS latency under packet loss.

use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        State,
    },
    http::StatusCode,
    response::{Html, IntoResponse, Response},
    routing::get,
    Json, Router,
};
use glam::Vec3;
use sauerteig_core::{
    arena_objects, forward, player_intersects_fragment, ray_player, simulate_player, spawn_point,
    validate_root_edits, Cell, CellEdit, CellFragment, CellPos, ClientMessage, FineBox, InputFrame,
    ObjectKind, Player, Projectile, RootEdit, ServerMessage, ShotEffect, World, WorldObject,
    FIXED_DT, PLAYER_HEIGHT, PLAYER_RADIUS, WORLD_HEIGHT, WORLD_MAX, WORLD_MIN,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    future::Future,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::{
    net::TcpListener,
    sync::{mpsc, watch},
    time::MissedTickBehavior,
};
use tower_http::services::ServeDir;

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
const MESSAGE_BYTES: usize = 16 * 1024 * 1024;
// A Welcome may contain every legal cell, including explicit corner shapes.
// Allow 256 JSON bytes per cell, plus objects and players. The remaining space
// covers the envelope and WebSocket framing/buffered writes.
// This is a growth limit, not an allocation per connection. Incoming messages
// retain their separate 16 MiB limit.
const OUTBOUND_WRITE_BYTES: usize = 256 * 1024 * 1024;
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

#[derive(Clone, Serialize, Deserialize)]
struct SavedWorld {
    revision: u64,
    cells: Vec<CellEdit>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    roots: Vec<RootEdit>,
    objects: Vec<WorldObject>,
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

struct App {
    session: Mutex<Session>,
    save_lock: tokio::sync::Mutex<()>,
    config: Config,
    stopped: watch::Sender<bool>,
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
        if self.tick % 3 == 0 {
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

fn load_world(path: &Path) -> Result<SavedWorld, String> {
    if !path.exists() {
        return Ok(SavedWorld {
            revision: 0,
            cells: World::arena().cells(),
            roots: Vec::new(),
            objects: arena_objects(),
        });
    }
    let metadata = std::fs::metadata(path).map_err(|e| e.to_string())?;
    if metadata.len() > OUTBOUND_WRITE_BYTES as u64 {
        return Err("Saved world exceeds 256 MiB.".into());
    }
    let saved: SavedWorld =
        serde_json::from_slice(&std::fs::read(path).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
    if saved.revision == u64::MAX
        || saved.objects.len() > MAX_OBJECTS
        || saved.cells.len() + saved.roots.iter().map(|r| r.fragments.len()).sum::<usize>()
            > MAX_WORLD_FRAGMENTS
    {
        return Err("Invalid saved world limits.".into());
    }
    validate_root_edits(&saved.roots)?;
    let mut seen = HashSet::new();
    for cell in &saved.cells {
        if !World::is_valid_pos(cell.pos)
            || cell.cell.is_none_or(|c| !validate_cell(c))
            || !seen.insert(cell.pos)
        {
            return Err("Saved world contains invalid or repeated cells.".into());
        }
    }
    for root in &saved.roots {
        if root.fragments.is_empty() || !seen.insert(root.pos) {
            return Err("Saved world contains empty or repeated detailed roots.".into());
        }
    }
    let mut ids = HashSet::new();
    for object in &saved.objects {
        validate_object_position(object.position, object.rotation)?;
        if !ids.insert(object.id) {
            return Err("Saved world contains repeated object IDs.".into());
        }
    }
    Ok(saved)
}

fn write_world(path: &Path, saved: &SavedWorld) -> Result<(), String> {
    use std::io::Write;
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let temporary = path.with_extension("json.tmp");
    let mut file = std::fs::File::create(&temporary).map_err(|e| e.to_string())?;
    serde_json::to_writer(&mut file, saved).map_err(|e| e.to_string())?;
    file.flush().map_err(|e| e.to_string())?;
    file.sync_all().map_err(|e| e.to_string())?;
    std::fs::rename(temporary, path).map_err(|e| e.to_string())
}

async fn save_current(app: &Arc<App>, force: bool) -> Result<(), String> {
    let _saving = app.save_lock.lock().await;
    let saved = {
        let session = app.session.lock().unwrap();
        if !force && (!session.dirty || session.last_edit.elapsed() < SAVE_DELAY) {
            return Ok(());
        }
        session.saved()
    };
    let revision = saved.revision;
    let path = app.config.world_path.clone();
    tokio::task::spawn_blocking(move || write_world(&path, &saved))
        .await
        .map_err(|e| e.to_string())??;
    let mut session = app.session.lock().unwrap();
    if session.revision == revision {
        session.dirty = false;
    }
    Ok(())
}

async fn health(State(app): State<Arc<App>>) -> Json<serde_json::Value> {
    let session = app.session.lock().unwrap();
    Json(
        serde_json::json!({"ok":true,"players":session.peers.len(),"tick":session.tick,"revision":session.revision}),
    )
}

async fn upgrade(ws: WebSocketUpgrade, State(app): State<Arc<App>>) -> Response {
    if app.session.lock().unwrap().peers.len() >= MAX_PLAYERS {
        return (StatusCode::SERVICE_UNAVAILABLE, "The room is full.").into_response();
    }
    ws.max_message_size(MESSAGE_BYTES)
        .max_frame_size(MESSAGE_BYTES)
        .write_buffer_size(16 * 1024)
        .max_write_buffer_size(OUTBOUND_WRITE_BYTES)
        .on_upgrade(move |socket| connection(socket, app))
}

async fn connection(mut socket: WebSocket, app: Arc<App>) {
    let (event_tx, mut events) = mpsc::channel(64);
    let (snapshot_tx, mut snapshots) = watch::channel(String::new());
    let joined = app.session.lock().unwrap().join(event_tx, snapshot_tx);
    let Ok((id, welcome)) = joined else {
        let _ = send_packet(&mut socket, Message::Close(None)).await;
        return;
    };
    if let Err(error) = send_packet(
        &mut socket,
        Message::Text(serde_json::to_string(&welcome).unwrap().into()),
    )
    .await
    {
        eprintln!("Could not send world snapshot to player {id}: {error}");
        app.session.lock().unwrap().peers.remove(&id);
        return;
    }
    let mut stopped = app.stopped.subscribe();
    let mut tokens: f64 = 240.0;
    let mut token_time = Instant::now();
    let mut last_packet = Instant::now();
    let mut ping = tokio::time::interval(Duration::from_secs(15));
    loop {
        tokio::select! {
            biased;
            _ = stopped.changed() => break,
            event = events.recv() => {
                let Some(text) = event else { break; };
                if send_packet(&mut socket, Message::Text(text.into())).await.is_err() { break; }
            }
            message = socket.recv() => {
                let Some(Ok(message)) = message else { break; };
                last_packet = Instant::now();
                match message {
                    Message::Text(text) => {
                        let now = Instant::now();
                        tokens = (tokens + now.duration_since(token_time).as_secs_f64() * 120.0).min(240.0);
                        token_time = now;
                        if tokens < 1.0 { break; }
                        tokens -= 1.0;
                        let result = serde_json::from_str::<ClientMessage>(&text).map_err(|e| format!("Invalid message: {e}"));
                        let mut session = app.session.lock().unwrap();
                        let (edit_request, result) = match result {
                            Ok(message) => {
                                let request = match &message {
                                    ClientMessage::Edit { request, .. }
                                    | ClientMessage::EditRoots { request, .. } => Some(*request),
                                    _ => None,
                                };
                                (request, session.handle(id, message))
                            }
                            Err(error) => (None, Err(error)),
                        };
                        // The patch is queued by handle() before this result. Only
                        // this matching acknowledgment completes a client's edit;
                        // another builder's patch may contain identical geometry.
                        if let Some(request) = edit_request {
                            session.send(id, ServerMessage::EditResult { request, error: result.err() });
                        } else if let Err(message) = result {
                            session.send(id, ServerMessage::Error { message });
                        }
                    }
                    Message::Ping(bytes) => if send_packet(&mut socket, Message::Pong(bytes)).await.is_err() { break; },
                    Message::Close(_) => break,
                    Message::Binary(_) => break,
                    Message::Pong(_) => {}
                }
            }
            changed = snapshots.changed() => {
                if changed.is_err() { break; }
                let text = snapshots.borrow_and_update().clone();
                if !text.is_empty() && send_packet(&mut socket, Message::Text(text.into())).await.is_err() { break; }
            }
            _ = ping.tick() => {
                if last_packet.elapsed() > Duration::from_secs(60) { break; }
                if send_packet(&mut socket, Message::Ping(Vec::new().into())).await.is_err() { break; }
            }
        }
    }
    app.session.lock().unwrap().peers.remove(&id);
    let _ = send_packet(&mut socket, Message::Close(None)).await;
}

async fn send_packet(socket: &mut WebSocket, message: Message) -> Result<(), String> {
    tokio::time::timeout(Duration::from_secs(5), socket.send(message))
        .await
        .map_err(|_| "WebSocket write timed out".to_string())?
        .map_err(|error| error.to_string())
}

/// Serve an already-bound listener. Keeping binding separate makes tests use
/// ephemeral loopback ports and lets the CLI print the actual listening address.
pub async fn serve(
    listener: TcpListener,
    config: Config,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> Result<(), String> {
    let saved = load_world(&config.world_path)?;
    let (stopped, _) = watch::channel(false);
    let app = Arc::new(App {
        session: Mutex::new(Session::new(saved, config.editing_allowed)),
        save_lock: tokio::sync::Mutex::new(()),
        config,
        stopped,
    });
    let router = Router::new()
        .route("/health", get(health))
        .route("/ws", get(upgrade));
    let router = if app.config.web_dir.join("index.html").is_file() {
        router.fallback_service(ServeDir::new(&app.config.web_dir))
    } else {
        router.fallback(|| async { Html("<!doctype html><title>Sauerteig server</title><h1>Sauerteig server is running</h1><p>The web client has not been built. Build it with the repository's web build script, then restart the server. Native clients can connect to this server's /ws endpoint now.</p>") })
    }.with_state(app.clone());
    let tick_app = app.clone();
    let tick_task = tokio::spawn(async move {
        let mut stop = tick_app.stopped.subscribe();
        let mut timer = tokio::time::interval(Duration::from_secs_f64(1.0 / 60.0));
        timer.set_missed_tick_behavior(MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                _ = stop.changed() => break,
                _ = timer.tick() => tick_app.session.lock().unwrap().step(),
            }
        }
    });
    let save_app = app.clone();
    let save_task = tokio::spawn(async move {
        let mut stop = save_app.stopped.subscribe();
        let mut timer = tokio::time::interval(Duration::from_secs(1));
        timer.set_missed_tick_behavior(MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                _ = stop.changed() => break,
                _ = timer.tick() => {
                    if let Err(error) = save_current(&save_app, false).await {
                        eprintln!("World save failed: {error}");
                        // Bound retries; simulation continues while persistence is unavailable.
                        tokio::time::sleep(Duration::from_secs(4)).await;
                    }
                }
            }
        }
    });
    let stop_app = app.clone();
    let result = axum::serve(listener, router)
        .with_graceful_shutdown(async move {
            shutdown.await;
            stop_app.stopped.send_replace(true);
        })
        .await
        .map_err(|e| e.to_string());
    app.stopped.send_replace(true);
    let _ = tick_task.await;
    let _ = save_task.await;
    save_current(&app, true).await?;
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn room() -> Session {
        let mut world = World::empty();
        for x in -8..8 {
            for z in -8..8 {
                world.set([x, 0, z], Some(Cell::solid(0)));
            }
        }
        Session::new(
            SavedWorld {
                revision: 0,
                cells: world.cells(),
                roots: Vec::new(),
                objects: Vec::new(),
            },
            true,
        )
    }

    fn peer(session: &mut Session, editing: bool) -> (u64, mpsc::Receiver<String>) {
        let (events, receiver) = mpsc::channel(256);
        let (snapshots, _) = watch::channel(String::new());
        let (id, _) = session.join(events, snapshots).unwrap();
        session.peers.get_mut(&id).unwrap().player.editing = editing;
        (id, receiver)
    }

    fn edit(material: u8) -> Vec<CellEdit> {
        vec![CellEdit {
            pos: [20, 4, 20],
            cell: Some(Cell::solid(material)),
        }]
    }

    fn fine_root(pos: CellPos, offset: [i32; 3], material: u8) -> RootEdit {
        let origin = std::array::from_fn(|i| pos[i] * 8 + offset[i]);
        RootEdit {
            pos,
            fragments: vec![CellFragment {
                bounds: FineBox {
                    min: origin,
                    max: origin.map(|v| v + 1),
                },
                origin,
                size: 1,
                cell: Cell::solid(material),
            }],
        }
    }

    fn assert_root(world: &World, root: &RootEdit) {
        assert_eq!(
            world.root_fragments(root.pos),
            sauerteig_core::canonical_root_fragments(root.pos, &root.fragments),
        );
    }

    #[test]
    fn sparse_history_preserves_legacy_ownership_and_stale_empty_roots() {
        let mut session = room();
        let (a, _a) = peer(&mut session, true);
        let (b, _b) = peer(&mut session, true);
        let detail = fine_root([20, 4, 20], [1, 2, 3], 2);
        session.edit_roots(a, 0, vec![detail.clone()]).unwrap();
        session.edit(b, edit(1)).unwrap();
        assert!(session.undo(a).unwrap_err().contains("later edit"));
        session.undo(b).unwrap();
        assert_root(&session.world, &detail);
        session.undo(a).unwrap();
        assert!(session.world.root_fragments(detail.pos).is_empty());
        assert!(!session.cell_revisions.contains_key(&detail.pos));
        // Clearing restored the old ownership lineage, but did not rewind the
        // stale-edit clock. Even a stale clear of the now-empty root is rejected.
        assert!(session
            .edit_roots(
                a,
                0,
                vec![RootEdit {
                    pos: detail.pos,
                    fragments: vec![]
                }]
            )
            .unwrap_err()
            .contains("changed after"));
        assert_eq!(session.revision, 4);
        // A shared boundary or an empty gap inside a selection's bounding box
        // does not make an unrelated root conflict with this old snapshot.
        let unrelated = fine_root([21, 4, 20], [0, 2, 3], 1);
        session.edit_roots(b, 0, vec![unrelated.clone()]).unwrap();
        assert_root(&session.world, &unrelated);
        session.redo(a).unwrap();
        assert_root(&session.world, &detail);
    }

    #[test]
    fn fine_pastes_and_redo_check_exact_fighter_overlap_atomically() {
        let mut session = room();
        let (builder, _a) = peer(&mut session, true);
        let (fighter, _b) = peer(&mut session, false);
        let detail = fine_root([2, 1, 2], [3, 1, 3], 2);
        session
            .edit_roots(builder, 0, vec![detail.clone()])
            .unwrap();
        session.undo(builder).unwrap();
        session.peers.get_mut(&fighter).unwrap().player.position = Vec3::new(2.5, 1.0, 2.5);
        assert!(session
            .redo(builder)
            .unwrap_err()
            .contains("inside a player"));
        assert_eq!(session.peers[&builder].redo.len(), 1);
        let safe = fine_root([20, 4, 20], [1, 1, 1], 1);
        assert!(session
            .edit_roots(builder, 2, vec![safe.clone(), detail])
            .unwrap_err()
            .contains("inside a player"));
        assert_eq!(session.revision, 2);
        assert!(session.world.root_fragments(safe.pos).is_empty());
        // This tiny fragment shares the fighter's root, but is outside the body.
        let beside = fine_root([2, 1, 2], [7, 1, 3], 1);
        session
            .edit_roots(builder, 2, vec![beside.clone()])
            .unwrap();
        assert_root(&session.world, &beside);
        assert!(!session.blocked(&session.peers[&fighter].player));
    }

    #[test]
    fn large_root_pastes_are_atomic_and_history_is_bounded_by_geometry() {
        let mut session = room();
        let (builder, _events) = peer(&mut session, true);
        let mut roots = Vec::new();
        for x in WORLD_MIN..WORLD_MAX {
            for z in WORLD_MIN..WORLD_MAX {
                roots.push(legacy_root(CellEdit {
                    pos: [x, 8, z],
                    cell: Some(Cell::solid(0)),
                }));
            }
        }
        assert_eq!(roots.len(), MAX_EDITS);
        for revision in 0..9 {
            for root in &mut roots {
                root.fragments[0].cell.material = (revision % 2) as u8;
            }
            // Test storage capacity independently of the wall-clock rate limit.
            session.peers.get_mut(&builder).unwrap().edit_tokens = EDIT_BURST;
            session
                .edit_roots(builder, revision, roots.clone())
                .unwrap();
            assert_eq!(
                session.revision,
                revision + 1,
                "one revision per whole paste"
            );
        }
        let history = &session.peers[&builder].undo;
        assert_eq!(
            history.len(),
            8,
            "large operations spend the history budget"
        );
        assert!(history.iter().map(|e| e.complexity).sum::<usize>() <= MAX_HISTORY_COMPLEXITY);
        let revision = session.revision;
        let mut invalid = roots;
        invalid.last_mut().unwrap().fragments[0].size = 3;
        assert!(session.edit_roots(builder, revision, invalid).is_err());
        assert_eq!(session.revision, revision);
    }

    #[test]
    fn legacy_snapshots_decode_without_detailed_roots() {
        let saved: SavedWorld = serde_json::from_value(serde_json::json!({
            "revision": 0, "cells": [], "objects": [],
        }))
        .unwrap();
        assert!(saved.roots.is_empty());
        let welcome: ServerMessage = serde_json::from_value(serde_json::json!({
            "type": "welcome", "id": 1, "revision": 0, "cells": [],
            "objects": [], "players": [], "editing_allowed": true,
        }))
        .unwrap();
        assert!(matches!(welcome, ServerMessage::Welcome { roots, .. } if roots.is_empty()));
    }

    #[test]
    fn empty_clipped_fragments_do_not_create_revisions_or_claim_ownership() {
        let mut session = room();
        let (builder, _events) = peer(&mut session, true);
        let mut shape = sauerteig_core::Shape::solid();
        for vertex in &mut shape.vertices {
            vertex[0] /= 2;
        }
        let pos = [20, 4, 20];
        let origin = pos.map(|v| v * 8);
        let mut min = origin;
        min[0] += 6;
        let empty = RootEdit {
            pos,
            fragments: vec![CellFragment {
                bounds: FineBox {
                    min,
                    max: origin.map(|v| v + 8),
                },
                origin,
                size: 8,
                cell: Cell {
                    shape: Some(shape),
                    ..Cell::solid(1)
                },
            }],
        };
        session.edit_roots(builder, 0, vec![empty.clone()]).unwrap();
        assert_eq!(session.revision, 0);
        assert!(session.peers[&builder].undo.is_empty());
        let actual = fine_root([21, 4, 20], [1, 1, 1], 2);
        session.edit_roots(builder, 0, vec![empty, actual]).unwrap();
        assert_eq!(session.revision, 1);
        assert!(!session.cell_revisions.contains_key(&pos));
        assert!(!session.root_changed_at.contains_key(&pos));
        assert_eq!(session.peers[&builder].undo.back().unwrap().before.len(), 1);
        session.undo(builder).unwrap();
        assert!(session.world.detailed_roots().is_empty());
    }

    #[test]
    fn precoalescing_saves_and_replacements_count_the_stored_geometry() {
        let pos = [20, 4, 20];
        let mut fragments = Vec::new();
        for x in 0..8 {
            for y in 0..8 {
                for z in 0..8 {
                    fragments.extend(fine_root(pos, [x, y, z], 0).fragments);
                }
            }
        }
        assert_eq!(fragments.len(), 512);
        let dir = temporary_directory("precoalescing-save");
        let path = dir.join("world.json");
        write_world(
            &path,
            &SavedWorld {
                revision: 0,
                cells: vec![],
                roots: vec![RootEdit {
                    pos,
                    fragments: fragments.clone(),
                }],
                objects: vec![],
            },
        )
        .unwrap();
        let mut session = Session::new(load_world(&path).unwrap(), true);
        assert_eq!(session.geometry_fragments, 1);
        assert_eq!(session.world.get(pos), Some(Cell::solid(0)));
        assert!(session.world.detailed_roots().is_empty());
        let (builder, _events) = peer(&mut session, true);
        for fragment in &mut fragments {
            fragment.cell.material = 2;
        }
        // Model an otherwise full room in the capacity counter: replacing its
        // one affected root with 512 coalescible pieces must spend no new space.
        session.geometry_fragments = MAX_WORLD_FRAGMENTS;
        session
            .edit_roots(builder, 0, vec![RootEdit { pos, fragments }])
            .unwrap();
        assert_eq!(session.geometry_fragments, MAX_WORLD_FRAGMENTS);
        assert_eq!(session.world.get(pos), Some(Cell::solid(2)));
        session.undo(builder).unwrap();
        assert_eq!(session.geometry_fragments, MAX_WORLD_FRAGMENTS);
        assert_eq!(session.world.get(pos), Some(Cell::solid(0)));
        // Restore the fixture's real occupancy and remove the loaded root.
        session.geometry_fragments = 1;
        session
            .edit(builder, vec![CellEdit { pos, cell: None }])
            .unwrap();
        assert_eq!(session.geometry_fragments, 0);
        session.undo(builder).unwrap();
        assert_eq!(session.geometry_fragments, 1);
        assert_eq!(session.world.get(pos), Some(Cell::solid(0)));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn outbound_limit_covers_a_full_bounded_world() {
        use sauerteig_core::{Shape, WORLD_HEIGHT, WORLD_MAX, WORLD_MIN};
        let widest_cell = CellEdit {
            pos: [WORLD_MIN, WORLD_HEIGHT - 1, WORLD_MIN],
            cell: Some(Cell {
                shape: Some(Shape {
                    diagonals: 63,
                    ..Shape::solid()
                }),
                diagonal: true,
                ..Cell::solid(5)
            }),
        };
        assert!(serde_json::to_vec(&widest_cell).unwrap().len() + 1 <= 256);
        let wide_root = legacy_root(widest_cell.clone());
        assert!(serde_json::to_vec(&wide_root).unwrap().len() + 1 <= 320);
        let cell_count = (WORLD_MAX - WORLD_MIN).pow(2) as usize * WORLD_HEIGHT as usize;
        let welcome_budget = cell_count * 256 + MAX_OBJECTS * 256 + MAX_PLAYERS * 1024 + 1024;
        assert!(welcome_budget + 16 * 1024 + 14 < OUTBOUND_WRITE_BYTES);
        let detailed_budget =
            MAX_WORLD_FRAGMENTS * 320 + MAX_OBJECTS * 256 + MAX_PLAYERS * 1024 + 1024;
        assert!(detailed_budget + 16 * 1024 + 14 < OUTBOUND_WRITE_BYTES);
        let request = ClientMessage::Edit {
            request: u64::MAX,
            edits: vec![widest_cell; MAX_EDITS],
        };
        assert!(serde_json::to_vec(&request).unwrap().len() < MESSAGE_BYTES);
    }

    #[test]
    fn undo_never_erases_an_intervening_builders_work() {
        let mut session = room();
        let (a, _a) = peer(&mut session, true);
        let (b, _b) = peer(&mut session, true);
        session.edit(a, edit(0)).unwrap();
        session.edit(b, edit(1)).unwrap();
        assert!(session.undo(a).unwrap_err().contains("later edit"));
        session.edit(a, edit(2)).unwrap();
        session.undo(a).unwrap();
        assert_eq!(session.world.get([20, 4, 20]), Some(Cell::solid(1)));
        // A's earlier edit must not become undoable just because A undid a
        // subsequent overwrite; B still owns the restored value.
        assert!(session.undo(a).unwrap_err().contains("later edit"));
        session.undo(b).unwrap();
        session.undo(a).unwrap();
        assert_eq!(session.world.get([20, 4, 20]), None);
        assert_eq!(session.revision, 6);
    }

    #[test]
    fn overlapping_history_can_be_undone_and_redone_repeatedly() {
        let mut session = room();
        let (a, _a) = peer(&mut session, true);
        session.edit(a, edit(0)).unwrap();
        session.edit(a, edit(2)).unwrap();
        for _ in 0..3 {
            session.undo(a).unwrap();
            session.undo(a).unwrap();
            assert_eq!(session.world.get([20, 4, 20]), None);
            session.redo(a).unwrap();
            assert_eq!(session.world.get([20, 4, 20]), Some(Cell::solid(0)));
            session.redo(a).unwrap();
            assert_eq!(session.world.get([20, 4, 20]), Some(Cell::solid(2)));
        }
        assert_eq!(session.revision, 14);
        session.undo(a).unwrap();
        session.edit(a, edit(1)).unwrap();
        assert_eq!(session.redo(a).unwrap_err(), "Nothing to redo.");
    }

    #[test]
    fn redo_checks_ownership_even_when_another_builder_restored_the_same_value() {
        let mut session = room();
        let (a, _a) = peer(&mut session, true);
        let (b, _b) = peer(&mut session, true);
        session.edit(a, edit(0)).unwrap();
        session.edit(a, edit(2)).unwrap();
        session.undo(a).unwrap();
        session.edit(b, edit(1)).unwrap();
        session.edit(b, edit(0)).unwrap();
        assert!(session.redo(a).unwrap_err().contains("later edit"));
        assert_eq!(session.world.get([20, 4, 20]), Some(Cell::solid(0)));
        session.undo(b).unwrap();
        session.undo(b).unwrap();
        session.redo(a).unwrap();
        assert_eq!(session.world.get([20, 4, 20]), Some(Cell::solid(2)));
    }

    #[test]
    fn redo_is_atomic_around_fighters_and_requires_edit_mode() {
        let mut session = room();
        let (a, _a) = peer(&mut session, true);
        let (b, _b) = peer(&mut session, false);
        session.edit(a, edit(0)).unwrap();
        session.undo(a).unwrap();
        session.peers.get_mut(&b).unwrap().player.position = Vec3::new(20.5, 4.0, 20.5);
        let revision = session.revision;
        assert!(session.redo(a).unwrap_err().contains("inside a player"));
        assert_eq!(session.revision, revision);
        assert_eq!(session.peers[&a].redo.len(), 1);
        session.peers.get_mut(&b).unwrap().player.position = spawn_point(0);
        session.peers.get_mut(&a).unwrap().player.editing = false;
        assert!(session.redo(a).is_err());
        session.peers.get_mut(&a).unwrap().player.editing = true;
        session.redo(a).unwrap();
        assert_eq!(session.world.get([20, 4, 20]), Some(Cell::solid(0)));
    }

    #[test]
    fn edits_are_atomic_and_cannot_embed_a_fighter() {
        let mut session = room();
        let (builder, _a) = peer(&mut session, true);
        let (fighter, _b) = peer(&mut session, false);
        session.peers.get_mut(&fighter).unwrap().player.position = Vec3::new(2.5, 1.0, 2.5);
        let edits = vec![
            CellEdit {
                pos: [20, 4, 20],
                cell: Some(Cell::solid(1)),
            },
            CellEdit {
                pos: [2, 1, 2],
                cell: Some(Cell::solid(1)),
            },
        ];
        assert!(session.edit(builder, edits).is_err());
        assert_eq!(session.revision, 0);
        assert_eq!(session.world.get([20, 4, 20]), None);
        assert!(session.edit(fighter, edit(0)).is_err());
        let mut malformed = edit(0);
        malformed[0].cell.as_mut().unwrap().heights = [9; 4];
        assert!(session.edit(builder, malformed).is_err());
        assert_eq!(session.revision, 0);
    }

    #[test]
    fn input_spam_cannot_advance_the_simulation() {
        let mut normal = room();
        let mut flooded = room();
        let (a, _a) = peer(&mut normal, false);
        let (b, _b) = peer(&mut flooded, false);
        let input = InputFrame {
            seq: 1,
            forward: 1.0,
            ..Default::default()
        };
        normal.handle(a, ClientMessage::Input { input }).unwrap();
        for seq in 1..=32 {
            flooded
                .handle(
                    b,
                    ClientMessage::Input {
                        input: InputFrame { seq, ..input },
                    },
                )
                .unwrap();
        }
        for _ in 0..1000 {
            flooded
                .handle(
                    b,
                    ClientMessage::Input {
                        input: InputFrame { seq: 32, ..input },
                    },
                )
                .unwrap();
        }
        normal.step();
        flooded.step();
        assert_eq!(
            normal.peers[&a].player.position,
            flooded.peers[&b].player.position
        );
        assert_eq!(flooded.peers[&b].player.last_input, 1);
        assert_eq!(flooded.peers[&b].inputs.len(), 31);
        assert_eq!(normal.tick, 1);
        assert!(flooded
            .handle(
                b,
                ClientMessage::Input {
                    input: InputFrame {
                        forward: f32::NAN,
                        ..input
                    }
                }
            )
            .is_err());
        assert!(flooded
            .handle(
                b,
                ClientMessage::Input {
                    input: InputFrame {
                        forward: 2.0,
                        ..input
                    }
                }
            )
            .is_err());
    }

    #[test]
    fn burst_inputs_are_consumed_once_in_order_and_idle_ticks_do_not_repeat_motion() {
        let mut session = room();
        let (id, _events) = peer(&mut session, true);
        let mut expected = session.peers[&id].player.clone();
        for seq in 1..=3 {
            session
                .handle(
                    id,
                    ClientMessage::Input {
                        input: InputFrame {
                            seq,
                            forward: 1.0,
                            ..Default::default()
                        },
                    },
                )
                .unwrap();
        }
        // Duplicate and stale packets must not add movement or replace a frame.
        for seq in [3, 2] {
            session
                .handle(
                    id,
                    ClientMessage::Input {
                        input: InputFrame {
                            seq,
                            forward: -1.0,
                            ..Default::default()
                        },
                    },
                )
                .unwrap();
        }
        assert_eq!(session.peers[&id].inputs.len(), 3);
        for seq in 1..=3 {
            let input = InputFrame {
                seq,
                forward: 1.0,
                ..Default::default()
            };
            simulate_player(&session.world, &mut expected, &input, FIXED_DT);
            session.step();
            assert_eq!(session.peers[&id].player.last_input, seq);
            assert_eq!(session.peers[&id].player.position, expected.position);
        }
        session.step();
        assert_eq!(session.peers[&id].player.last_input, 3);
        assert_eq!(session.peers[&id].player.position, expected.position);
    }

    #[test]
    fn missing_inputs_keep_gravity_running_without_acknowledging_new_frames() {
        let mut session = room();
        let (id, _events) = peer(&mut session, false);
        session.peers.get_mut(&id).unwrap().player.position = Vec3::new(0.5, 8.0, 0.5);
        session
            .handle(
                id,
                ClientMessage::Input {
                    input: InputFrame {
                        seq: 17,
                        ..Default::default()
                    },
                },
            )
            .unwrap();
        session.step();
        let first_height = session.peers[&id].player.position.y;
        session.step();
        assert!(session.peers[&id].player.position.y < first_height);
        assert_eq!(session.peers[&id].player.last_input, 17);
    }

    #[test]
    fn overflowing_input_queue_disconnects_instead_of_skipping_prediction_frames() {
        let mut session = room();
        let (id, mut events) = peer(&mut session, false);
        for seq in 1..=MAX_INPUT_BACKLOG as u32 + 1 {
            session
                .handle(
                    id,
                    ClientMessage::Input {
                        input: InputFrame {
                            seq,
                            ..Default::default()
                        },
                    },
                )
                .unwrap();
        }
        assert!(!session.peers.contains_key(&id));
        let message: ServerMessage = serde_json::from_str(&events.try_recv().unwrap()).unwrap();
        assert!(matches!(message, ServerMessage::Error { message } if message.contains("backlog")));
    }

    #[test]
    fn mode_acknowledges_request_and_discards_old_mode_inputs() {
        let mut session = room();
        let (id, mut events) = peer(&mut session, false);
        for seq in 1..=3 {
            session
                .handle(
                    id,
                    ClientMessage::Input {
                        input: InputFrame {
                            seq,
                            forward: 1.0,
                            fire: true,
                            ..Default::default()
                        },
                    },
                )
                .unwrap();
        }
        session
            .handle(
                id,
                ClientMessage::SetMode {
                    request: 7,
                    editing: true,
                },
            )
            .unwrap();
        let p = &session.peers[&id];
        assert!(p.inputs.is_empty());
        assert_eq!(p.player.last_input, 3);
        assert_eq!(p.player.mode_request, 7);
        assert!(p.player.editing);
        assert!(!p.input.fire);
        let message: ServerMessage = serde_json::from_str(&events.try_recv().unwrap()).unwrap();
        assert!(matches!(
            message,
            ServerMessage::ModeChanged {
                request: 7,
                editing: true
            }
        ));
        session
            .handle(
                id,
                ClientMessage::SetMode {
                    request: 7,
                    editing: false,
                },
            )
            .unwrap();
        assert!(
            session.peers[&id].player.editing,
            "duplicate request is idempotent"
        );
    }

    #[test]
    fn denied_edit_mode_still_acknowledges_authoritative_mode() {
        let mut session = room();
        session.editing_allowed = false;
        let (id, mut events) = peer(&mut session, false);
        assert!(session
            .handle(
                id,
                ClientMessage::SetMode {
                    request: 12,
                    editing: true
                }
            )
            .is_err());
        let message: ServerMessage = serde_json::from_str(&events.try_recv().unwrap()).unwrap();
        assert!(matches!(
            message,
            ServerMessage::ModeChanged {
                request: 12,
                editing: false
            }
        ));
        assert_eq!(session.peers[&id].player.mode_request, 12);
        assert!(!session.peers[&id].player.editing);
    }

    #[test]
    fn authoritative_hitscan_respects_walls_and_scores_a_kill() {
        let mut session = room();
        let (a, _a) = peer(&mut session, false);
        let (b, _b) = peer(&mut session, false);
        session.peers.get_mut(&a).unwrap().player.position = Vec3::new(0.5, 1.0, 4.5);
        session.peers.get_mut(&b).unwrap().player.position = Vec3::new(0.5, 1.0, 0.5);
        session.world.set([0, 2, 2], Some(Cell::solid(0)));
        let mut shots = Vec::new();
        session.fire(a, &mut shots);
        assert_eq!(session.peers[&b].player.health, 100);
        assert!(!shots.last().unwrap().hit);
        session.world.set([0, 2, 2], None);
        for _ in 0..3 {
            session.fire(a, &mut shots);
        }
        assert_eq!(session.peers[&a].player.score, 1);
        assert_eq!(session.peers[&b].player.health, 100);
    }

    #[test]
    fn weapon_cooldowns_are_owned_by_server_ticks() {
        let mut session = room();
        let (a, _a) = peer(&mut session, false);
        let (b, _b) = peer(&mut session, false);
        session.peers.get_mut(&a).unwrap().player.position = Vec3::new(0.5, 1.0, 4.5);
        session.peers.get_mut(&b).unwrap().player.position = Vec3::new(0.5, 1.0, 0.5);
        for seq in 1..=12 {
            session
                .handle(
                    a,
                    ClientMessage::Input {
                        input: InputFrame {
                            seq,
                            fire: true,
                            ..Default::default()
                        },
                    },
                )
                .unwrap();
            session.step();
        }
        assert_eq!(session.peers[&b].player.health, 66);
        session.step();
        assert_eq!(session.peers[&b].player.health, 32);
    }

    #[test]
    fn rockets_hit_and_respawn_a_target() {
        let mut session = room();
        let (a, _a) = peer(&mut session, false);
        let (b, _b) = peer(&mut session, false);
        session.peers.get_mut(&a).unwrap().player.position = Vec3::new(0.5, 1.0, 4.5);
        session.peers.get_mut(&b).unwrap().player.position = Vec3::new(0.5, 1.0, 0.5);
        session.peers.get_mut(&a).unwrap().input.weapon = 1;
        let mut shots = Vec::new();
        session.fire(a, &mut shots);
        assert_eq!(session.rockets.len(), 1);
        for _ in 0..12 {
            session.advance_rockets(&mut shots);
        }
        assert!(session.rockets.is_empty());
        assert_eq!(session.peers[&a].player.score, 1);
        assert_eq!(session.peers[&b].player.health, 100);
    }

    #[test]
    fn persistence_preserves_deformation_objects_and_revision() {
        let mut session = room();
        let (a, _a) = peer(&mut session, true);
        let slope = Cell {
            material: 3,
            heights: [2, 2, 8, 8],
            ..Cell::solid(3)
        };
        session
            .edit(
                a,
                vec![CellEdit {
                    pos: [12, 5, 12],
                    cell: Some(slope),
                }],
            )
            .unwrap();
        session
            .handle(
                a,
                ClientMessage::PlaceObject {
                    kind: ObjectKind::Light,
                    position: Vec3::new(1.0, 4.0, 1.0),
                    rotation: 0.5,
                },
            )
            .unwrap();
        let dir = temporary_directory("save");
        let path = dir.join("world.json");
        write_world(&path, &session.saved()).unwrap();
        let loaded = load_world(&path).unwrap();
        assert_eq!(loaded.revision, 2);
        assert_eq!(
            World::from_cells(&loaded.cells).get([12, 5, 12]),
            Some(slope)
        );
        assert_eq!(loaded.objects.len(), 1);
        assert!(!path.with_extension("json.tmp").exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    fn temporary_directory(label: &str) -> PathBuf {
        let time = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("sauerteig-{label}-{}-{time}", std::process::id()));
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    type ClientSocket = tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >;

    async fn send_client(socket: &mut ClientSocket, message: ClientMessage) {
        use futures_util::SinkExt;
        socket
            .send(tokio_tungstenite::tungstenite::Message::Text(
                serde_json::to_string(&message).unwrap().into(),
            ))
            .await
            .unwrap();
    }

    async fn receive_where(
        socket: &mut ClientSocket,
        matches: impl Fn(&ServerMessage) -> bool,
    ) -> ServerMessage {
        use futures_util::StreamExt;
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let message = socket
                    .next()
                    .await
                    .expect("connection open")
                    .expect("valid frame");
                if let tokio_tungstenite::tungstenite::Message::Text(text) = message {
                    let message: ServerMessage = serde_json::from_str(&text).unwrap();
                    if matches(&message) {
                        return message;
                    }
                }
            }
        })
        .await
        .expect("server response before timeout")
    }

    #[tokio::test]
    async fn websocket_large_arena_joins_and_edit_results_correlate_with_requests() {
        use futures_util::StreamExt;
        let dir = temporary_directory("large-arena-edit-results");
        let config = Config {
            world_path: dir.join("world.json"),
            web_dir: dir.join("web"),
            editing_allowed: true,
        };
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(serve(listener, config, async {
            let _ = stopped.await;
        }));
        let (mut first, _) = tokio_tungstenite::connect_async(format!("ws://{address}/ws"))
            .await
            .unwrap();
        let welcome = tokio::time::timeout(Duration::from_secs(5), first.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let tokio_tungstenite::tungstenite::Message::Text(welcome) = welcome else {
            panic!("first frame must contain the complete Welcome");
        };
        assert!(welcome.len() > 256 * 1024, "regress the old outbound cap");
        assert!(welcome.len() < OUTBOUND_WRITE_BYTES);
        assert!(matches!(
            serde_json::from_str::<ServerMessage>(&welcome).unwrap(),
            ServerMessage::Welcome { cells, .. } if cells.len() == World::arena().cells().len()
        ));
        send_client(
            &mut first,
            ClientMessage::SetMode {
                request: 1,
                editing: true,
            },
        )
        .await;
        receive_where(&mut first, |m| {
            matches!(m, ServerMessage::ModeChanged { .. })
        })
        .await;

        // An unrelated failure queued before an edit must not masquerade as
        // that edit's rejection. Changed edits publish their patch first.
        send_client(&mut first, ClientMessage::Undo).await;
        send_client(
            &mut first,
            ClientMessage::Edit {
                request: 9,
                edits: edit(1),
            },
        )
        .await;
        let next_event = |m: &ServerMessage| !matches!(m, ServerMessage::Snapshot { .. });
        assert!(matches!(receive_where(&mut first, next_event).await,
            ServerMessage::Error { message } if message.contains("Nothing to undo")));
        assert!(matches!(
            receive_where(&mut first, next_event).await,
            ServerMessage::WorldPatch { revision: 1, .. }
        ));
        assert!(matches!(
            receive_where(&mut first, next_event).await,
            ServerMessage::EditResult {
                request: 9,
                error: None
            }
        ));

        // A no-op still finishes its exact request, without a new patch.
        send_client(
            &mut first,
            ClientMessage::Edit {
                request: 10,
                edits: edit(1),
            },
        )
        .await;
        assert!(matches!(
            receive_where(&mut first, next_event).await,
            ServerMessage::EditResult {
                request: 10,
                error: None
            }
        ));
        send_client(
            &mut first,
            ClientMessage::Edit {
                request: 11,
                edits: vec![CellEdit {
                    pos: [1000, 4, 0],
                    cell: Some(Cell::solid(0)),
                }],
            },
        )
        .await;
        assert!(matches!(
            receive_where(&mut first, next_event).await,
            ServerMessage::EditResult {
                request: 11,
                error: Some(_)
            }
        ));

        let (mut second, _) = tokio_tungstenite::connect_async(format!("ws://{address}/ws"))
            .await
            .unwrap();
        assert!(matches!(
            receive_where(&mut second, |m| matches!(m, ServerMessage::Welcome { .. })).await,
            ServerMessage::Welcome { revision: 1, .. }
        ));
        send_client(
            &mut second,
            ClientMessage::SetMode {
                request: 1,
                editing: true,
            },
        )
        .await;
        receive_where(&mut second, |m| {
            matches!(m, ServerMessage::ModeChanged { .. })
        })
        .await;
        send_client(
            &mut second,
            ClientMessage::Edit {
                request: 21,
                edits: edit(2),
            },
        )
        .await;
        assert!(matches!(
            receive_where(&mut first, next_event).await,
            ServerMessage::WorldPatch { revision: 2, .. }
        ));
        // The peer's identical patch does not acknowledge our later request.
        send_client(
            &mut first,
            ClientMessage::Edit {
                request: 12,
                edits: edit(2),
            },
        )
        .await;
        assert!(matches!(
            receive_where(&mut first, next_event).await,
            ServerMessage::EditResult {
                request: 12,
                error: None
            }
        ));
        assert!(matches!(
            receive_where(&mut second, next_event).await,
            ServerMessage::WorldPatch { revision: 2, .. }
        ));
        assert!(matches!(
            receive_where(&mut second, next_event).await,
            ServerMessage::EditResult {
                request: 21,
                error: None
            }
        ));

        first.close(None).await.unwrap();
        second.close(None).await.unwrap();
        stop.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(5), server)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn websocket_clients_share_edits_reject_bad_inputs_and_rejoin_saved_world() {
        use futures_util::SinkExt;
        let dir = temporary_directory("websocket");
        let config = Config {
            world_path: dir.join("world.json"),
            web_dir: dir.join("web"),
            editing_allowed: true,
        };
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let server_config = config.clone();
        let server = tokio::spawn(serve(listener, server_config, async {
            let _ = stopped.await;
        }));
        let (mut first, _) = tokio_tungstenite::connect_async(format!("ws://{address}/ws"))
            .await
            .unwrap();
        receive_where(&mut first, |m| matches!(m, ServerMessage::Welcome { .. })).await;
        send_client(
            &mut first,
            ClientMessage::SetMode {
                request: 1,
                editing: true,
            },
        )
        .await;
        let slope = Cell {
            material: 2,
            heights: [2, 2, 8, 8],
            ..Cell::solid(2)
        };
        send_client(
            &mut first,
            ClientMessage::Edit {
                request: 1,
                edits: vec![CellEdit {
                    pos: [30, 8, 30],
                    cell: Some(slope),
                }],
            },
        )
        .await;
        receive_where(&mut first, |m| {
            matches!(m, ServerMessage::WorldPatch { revision: 1, .. })
        })
        .await;

        let (mut second, _) = tokio_tungstenite::connect_async(format!("ws://{address}/ws"))
            .await
            .unwrap();
        let welcome =
            receive_where(&mut second, |m| matches!(m, ServerMessage::Welcome { .. })).await;
        if let ServerMessage::Welcome {
            revision,
            cells,
            players,
            ..
        } = welcome
        {
            assert_eq!(revision, 1);
            assert_eq!(players.len(), 2);
            assert_eq!(World::from_cells(&cells).get([30, 8, 30]), Some(slope));
        } else {
            unreachable!();
        }

        send_client(
            &mut second,
            ClientMessage::SetMode {
                request: 1,
                editing: true,
            },
        )
        .await;
        send_client(
            &mut second,
            ClientMessage::Edit {
                request: 1,
                edits: vec![CellEdit {
                    pos: [30, 8, 30],
                    cell: Some(Cell::solid(4)),
                }],
            },
        )
        .await;
        receive_where(&mut first, |m| {
            matches!(m, ServerMessage::WorldPatch { revision: 2, .. })
        })
        .await;
        send_client(&mut first, ClientMessage::Undo).await;
        let error = receive_where(&mut first, |m| matches!(m, ServerMessage::Error { .. })).await;
        assert!(
            matches!(error, ServerMessage::Error { message } if message.contains("later edit"))
        );
        send_client(
            &mut first,
            ClientMessage::Input {
                input: InputFrame {
                    seq: 1,
                    forward: 2.0,
                    ..Default::default()
                },
            },
        )
        .await;
        receive_where(
            &mut first,
            |m| matches!(m, ServerMessage::Error { message } if message.contains("Invalid input")),
        )
        .await;
        first
            .send(tokio_tungstenite::tungstenite::Message::Text(
                "{\"type\":\"input\",\"input\":{\"yaw\":1e999}}".into(),
            ))
            .await
            .unwrap();
        receive_where(&mut first, |m| matches!(m, ServerMessage::Error { message } if message.contains("Invalid message"))).await;
        send_client(&mut first, ClientMessage::Save).await;
        receive_where(&mut first, |m| matches!(m, ServerMessage::Notice { .. })).await;
        first.close(None).await.unwrap();
        second.close(None).await.unwrap();
        stop.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(5), server)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(load_world(&config.world_path).unwrap().revision, 2);

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(serve(listener, config, async {
            let _ = stopped.await;
        }));
        let (mut rejoined, _) = tokio_tungstenite::connect_async(format!("ws://{address}/ws"))
            .await
            .unwrap();
        let welcome = receive_where(&mut rejoined, |m| {
            matches!(m, ServerMessage::Welcome { .. })
        })
        .await;
        if let ServerMessage::Welcome {
            revision, cells, ..
        } = welcome
        {
            assert_eq!(revision, 2);
            assert_eq!(
                World::from_cells(&cells).get([30, 8, 30]),
                Some(Cell::solid(4))
            );
        } else {
            unreachable!();
        }
        rejoined.close(None).await.unwrap();
        stop.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(5), server)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn websocket_combat_produces_authoritative_damage_score_and_respawn() {
        let dir = temporary_directory("combat");
        let config = Config {
            world_path: dir.join("world.json"),
            web_dir: dir.join("web"),
            editing_allowed: true,
        };
        let mut saved = room().saved();
        // Player IDs rotate authored spawn choices, so ID 1 starts at +Z and
        // can shoot straight toward ID 2 with yaw zero.
        saved.objects = vec![
            WorldObject {
                id: 1,
                kind: ObjectKind::Spawn,
                position: Vec3::new(0.5, 1.0, 0.5),
                rotation: 0.0,
            },
            WorldObject {
                id: 2,
                kind: ObjectKind::Spawn,
                position: Vec3::new(0.5, 1.0, 4.5),
                rotation: 0.0,
            },
        ];
        write_world(&config.world_path, &saved).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(serve(listener, config, async {
            let _ = stopped.await;
        }));
        let (mut attacker, _) = tokio_tungstenite::connect_async(format!("ws://{address}/ws"))
            .await
            .unwrap();
        let welcome = receive_where(&mut attacker, |m| {
            matches!(m, ServerMessage::Welcome { .. })
        })
        .await;
        let attacker_id = if let ServerMessage::Welcome { id, .. } = welcome {
            id
        } else {
            unreachable!()
        };
        let (mut target, _) = tokio_tungstenite::connect_async(format!("ws://{address}/ws"))
            .await
            .unwrap();
        let welcome =
            receive_where(&mut target, |m| matches!(m, ServerMessage::Welcome { .. })).await;
        let target_id = if let ServerMessage::Welcome { id, .. } = welcome {
            id
        } else {
            unreachable!()
        };
        let mut saw_damage = false;
        let mut saw_respawn = false;
        for seq in 1..=12 {
            send_client(
                &mut attacker,
                ClientMessage::Input {
                    input: InputFrame {
                        seq,
                        fire: true,
                        ..Default::default()
                    },
                },
            )
            .await;
            let snapshot = receive_where(&mut attacker, |m| {
                matches!(m, ServerMessage::Snapshot { .. })
            })
            .await;
            if let ServerMessage::Snapshot {
                players, revision, ..
            } = snapshot
            {
                assert_eq!(revision, 0);
                let victim = players.iter().find(|p| p.id == target_id).unwrap();
                let shooter = players.iter().find(|p| p.id == attacker_id).unwrap();
                saw_damage |= victim.health < 100;
                if shooter.score == 1 {
                    assert_eq!(victim.health, 100);
                    saw_respawn = true;
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(60)).await;
        }
        assert!(saw_damage, "client observed server-authoritative damage");
        assert!(saw_respawn, "client observed score and healthy respawn");
        attacker.close(None).await.unwrap();
        target.close(None).await.unwrap();
        stop.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(5), server)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        std::fs::remove_dir_all(dir).unwrap();
    }
}
