#[path = "game_panels.rs"]
mod panels;
use crate::audio::{Audio, Cue};
use crate::blueprints::BlueprintLibrary;
use crate::editor::{Clipboard, EditError, EditPlan, EditSelection, SelectionDrag};
use crate::regions::{RegionBox, RegionSet};
use crate::user_data::{Document, Preferences, Storage, StorageEvent};
use crate::{render::Renderer, scene::Geometry};
use glam::Vec3;
use sauerteig_core::*;
use std::collections::{BTreeMap, HashSet, VecDeque};
use winit::{
    event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent},
    keyboard::{KeyCode, PhysicalKey},
    window::{CursorGrabMode, Window},
};

const PAPER: egui::Color32 = egui::Color32::from_rgb(214, 216, 211);
const MUTED: egui::Color32 = egui::Color32::from_rgb(164, 174, 169);
const COPPER: egui::Color32 = egui::Color32::from_rgb(192, 135, 66);

#[derive(Default)]
struct ModeState {
    request: u32,
    pending: Option<bool>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_snapshots_cannot_restore_a_previous_mode_even_after_ack() {
        let mut mode = ModeState::default();
        let request = mode.request(true);
        assert!(!mode.accepts_snapshot(0));
        assert_eq!(mode.pending, Some(true));
        assert_eq!(mode.acknowledge(request, true), Some(true));
        assert_eq!(mode.pending, None);
        assert!(!mode.accepts_snapshot(0));
        assert!(mode.accepts_snapshot(request));
    }

    #[test]
    fn rapid_toggles_ignore_earlier_acknowledgments() {
        let mut mode = ModeState::default();
        let first = mode.request(true);
        let second = mode.request(false);
        assert_eq!(mode.acknowledge(first, true), None);
        assert_eq!(mode.pending, Some(false));
        assert!(!mode.accepts_snapshot(first));
        assert_eq!(mode.acknowledge(second, false), Some(false));
        assert!(mode.accepts_snapshot(second));
    }

    #[test]
    fn mode_refusal_uses_the_authoritative_flag_and_request_ids_can_wrap() {
        let mut mode = ModeState {
            request: u32::MAX,
            pending: None,
        };
        let request = mode.request(true);
        assert_eq!(request, 0);
        assert!(!mode.accepts_snapshot(u32::MAX));
        assert_eq!(mode.acknowledge(request, false), Some(false));
        assert_eq!(mode.pending, None);
        assert_eq!(mode.acknowledge(request, true), None);
    }
}
impl ModeState {
    fn request(&mut self, editing: bool) -> u32 {
        self.request = self.request.wrapping_add(1);
        self.pending = Some(editing);
        self.request
    }

    fn acknowledge(&mut self, request: u32, editing: bool) -> Option<bool> {
        if request != self.request || self.pending.is_none() {
            return None;
        }
        self.pending = None;
        Some(editing)
    }

    fn accepts_snapshot(&self, request: u32) -> bool {
        request == self.request
    }
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum SelectionMode {
    #[default]
    Replace,
    Add,
    Subtract,
}
#[derive(Default)]
struct LibraryUi {
    search: String,
    new_name: String,
    new_folder: String,
    selected: Option<u64>,
    edit_name: String,
    edit_folder: String,
    confirm_delete: bool,
}

#[derive(Clone)]
enum EditIntent {
    Extrude,
    Carve,
    Deform(i8),
    Paint(u8),
    Paste(Clipboard),
}
struct PendingEdit {
    request: u64,
    plan: EditPlan,
    generation: u64,
    advance_selection: bool,
    regions_after: Option<RegionSet>,
}

pub struct Game {
    pub world: World,
    pub objects: Vec<WorldObject>,
    pub local: Player,
    players: Vec<Player>,
    projectiles: Vec<Projectile>,
    remote_positions: BTreeMap<u64, Vec3>,
    effects: Vec<(ShotEffect, f32)>,
    socket: Option<(ewebsock::WsSender, ewebsock::WsReceiver)>,
    pub connected: bool,
    pub captured: bool,
    pub pointer_locked: bool,
    drag_look: bool,
    last_cursor: Option<(f64, f64)>,
    editing_allowed: bool,
    endpoint: String,
    name: String,
    status: String,
    notice: String,
    notice_until: f32,
    reconnect_at: f32,
    keys: HashSet<KeyCode>,
    firing: bool,
    seq: u32,
    mode: ModeState,
    pending: VecDeque<InputFrame>,
    accumulator: f32,
    camera_offset: Vec3,
    pub time: f32,
    revision: u64,
    grid: i32,
    material: u8,
    corner: Option<usize>,
    edge: Option<usize>,
    weapon: u8,
    selected: Option<RayHit>,
    selection: Option<EditSelection>,
    selection_drag: Option<SelectionDrag>,
    selection_generation: u64,
    pending_edit: Option<PendingEdit>,
    edit_request: u64,
    edit_queue: VecDeque<EditIntent>,
    clipboard: Option<Clipboard>,
    wheel_remainder: f32,
    viewport: [f32; 2],
    show_help: bool,
    show_settings: bool,
    show_blueprints: bool,
    show_assets: bool,
    material_thumbnails: Vec<egui::TextureHandle>,
    prop_kind: ObjectKind,
    preferences: Preferences,
    settings_ready: bool,
    library_ready: bool,
    settings_status: String,
    library_status: String,
    storage: Storage,
    library: BlueprintLibrary,
    library_ui: LibraryUi,
    regions: RegionSet,
    selection_mode: SelectionMode,
    selecting_subtract: bool,
    paste_preview: bool,
    pub fps: f32,
    last_edit_ms: f32,
    peak_edit_ms: f32,
    hit_flash: f32,
    damage_flash: f32,
    show_stats: bool,
    audio: Audio,
    #[cfg(target_arch = "wasm32")]
    capture_failed: std::rc::Rc<std::cell::Cell<bool>>,
}
impl Game {
    pub fn new(endpoint: String, name: String) -> Self {
        let mut local = Player::new(0, name.clone());
        local.position = Vec3::new(12.5, 1.02, 17.5);
        local.yaw = -0.56;
        local.pitch = 0.04;
        Self {
            world: World::arena(),
            objects: arena_objects(),
            local,
            players: vec![],
            projectiles: vec![],
            remote_positions: BTreeMap::new(),
            effects: vec![],
            socket: None,
            connected: false,
            captured: false,
            pointer_locked: false,
            drag_look: false,
            last_cursor: None,
            editing_allowed: true,
            endpoint,
            name,
            status: "Connecting…".into(),
            notice: String::new(),
            notice_until: 0.0,
            reconnect_at: 0.0,
            keys: HashSet::new(),
            firing: false,
            seq: 0,
            mode: ModeState::default(),
            pending: VecDeque::new(),
            accumulator: 0.0,
            camera_offset: Vec3::ZERO,
            time: 0.0,
            revision: 0,
            grid: 8,
            material: 0,
            corner: None,
            edge: None,
            weapon: 0,
            selected: None,
            selection: None,
            selection_drag: None,
            selection_generation: 0,
            pending_edit: None,
            edit_request: 0,
            edit_queue: VecDeque::new(),
            clipboard: None,
            wheel_remainder: 0.0,
            viewport: [1280.0, 800.0],
            show_help: false,
            show_settings: false,
            show_blueprints: false,
            show_assets: false,
            material_thumbnails: Vec::new(),
            prop_kind: ObjectKind::UtilityBox,
            preferences: Preferences::default(),
            settings_ready: false,
            library_ready: false,
            settings_status: "Loading…".into(),
            library_status: "Loading…".into(),
            storage: Storage::new(),
            library: BlueprintLibrary::new(),
            library_ui: LibraryUi::default(),
            regions: RegionSet::new(),
            selection_mode: SelectionMode::Replace,
            selecting_subtract: false,
            paste_preview: false,
            fps: 60.0,
            last_edit_ms: 0.0,
            peak_edit_ms: 0.0,
            hit_flash: 0.0,
            damage_flash: 0.0,
            show_stats: false,
            audio: Audio::new(),
            #[cfg(target_arch = "wasm32")]
            capture_failed: std::rc::Rc::new(std::cell::Cell::new(false)),
        }
    }
    pub fn close_text_panel(&mut self) {
        self.show_settings = false;
        self.show_blueprints = false;
        self.show_assets = false;
    }
    pub fn eye(&self) -> Vec3 {
        self.local.eye() + self.camera_offset
    }
    pub fn direction(&self) -> Vec3 {
        forward(self.local.yaw, self.local.pitch)
    }
    fn send(&mut self, message: ClientMessage) {
        if let Some((sender, _)) = &mut self.socket {
            if let Ok(text) = serde_json::to_string(&message) {
                sender.send(ewebsock::WsMessage::Text(text));
            }
        }
    }
    fn notify(&mut self, message: impl Into<String>) {
        self.notice = message.into();
        self.notice_until = self.time + 4.0;
    }
    fn connect(&mut self) {
        self.status = "Connecting…".into();
        self.reconnect_at = self.time + 3.0;
        match ewebsock::connect(&self.endpoint, ewebsock::Options::default()) {
            Ok(socket) => self.socket = Some(socket),
            Err(error) => {
                self.status = format!("Server unavailable: {error}");
                self.socket = None;
            }
        }
    }
    pub fn update(&mut self, dt: f32, renderer: &mut Renderer) {
        self.poll_storage();
        #[cfg(target_arch = "wasm32")]
        if self.capture_failed.replace(false) {
            self.captured = true;
            self.pointer_locked = false;
            self.drag_look = false;
            self.last_cursor = None;
            self.notify("Mouse lock unavailable. Right-drag to look; left-drag to select.");
        }
        let dt = dt.clamp(0.0, 0.1);
        self.time += dt;
        if dt > 0.0 {
            self.fps = self.fps * 0.96 + (1.0 / dt) * 0.04;
        }
        if self.socket.is_none() && self.time >= self.reconnect_at {
            self.connect();
        }
        let mut events = vec![];
        if let Some((_, receiver)) = &self.socket {
            for _ in 0..256 {
                if let Some(event) = receiver.try_recv() {
                    events.push(event);
                } else {
                    break;
                }
            }
        }
        for event in events {
            match event {
                ewebsock::WsEvent::Opened => self.send(ClientMessage::Hello {
                    name: self.name.clone(),
                }),
                ewebsock::WsEvent::Message(ewebsock::WsMessage::Text(text)) => {
                    match serde_json::from_str::<ServerMessage>(&text) {
                        Ok(message) => self.receive(message, renderer),
                        Err(error) => log::warn!("Invalid server message: {error}"),
                    }
                }
                ewebsock::WsEvent::Closed | ewebsock::WsEvent::Error(_) => {
                    self.socket = None;
                    self.connected = false;
                    self.pending.clear();
                    self.pending_edit = None;
                    self.clear_selection();
                    self.firing = false;
                    self.status = "Connection lost · reconnecting…".into();
                    self.reconnect_at = self.time + 3.0;
                }
                _ => {}
            }
        }
        self.accumulator = (self.accumulator + dt).min(FIXED_DT * 6.0);
        while self.accumulator >= FIXED_DT {
            self.accumulator -= FIXED_DT;
            if self.connected {
                self.seq = self.seq.wrapping_add(1);
                let axis = |positive, negative| {
                    if self.captured {
                        self.keys.contains(&positive) as u8 as f32
                            - self.keys.contains(&negative) as u8 as f32
                    } else {
                        0.0
                    }
                };
                let input = InputFrame {
                    seq: self.seq,
                    forward: axis(KeyCode::KeyW, KeyCode::KeyS),
                    strafe: axis(KeyCode::KeyD, KeyCode::KeyA),
                    vertical: axis(KeyCode::PageUp, KeyCode::PageDown),
                    yaw: self.local.yaw,
                    pitch: self.local.pitch,
                    jump: self.captured
                        && !self.local.editing
                        && self.keys.contains(&KeyCode::Space),
                    fire: self.captured && self.firing && !self.local.editing,
                    weapon: self.weapon,
                };
                simulate_player(&self.world, &mut self.local, &input, FIXED_DT);
                self.pending.push_back(input);
                if self.pending.len() > 120 {
                    self.pending.pop_front();
                }
                self.send(ClientMessage::Input { input });
            }
        }
        self.camera_offset *= (-dt * 18.0).exp();
        self.update_selection();
        self.process_edit_queue();
        self.effects.retain(|(_, until)| *until > self.time);
        self.hit_flash = (self.hit_flash - dt).max(0.0);
        self.damage_flash = (self.damage_flash - dt).max(0.0);
        for player in &self.players {
            let position = self
                .remote_positions
                .entry(player.id)
                .or_insert(player.position);
            if position.distance(player.position) > 5.0 {
                *position = player.position;
            } else {
                *position = position.lerp(player.position, 1.0 - (-dt * 18.0).exp());
            }
        }
    }
    fn receive(&mut self, message: ServerMessage, renderer: &mut Renderer) {
        match message {
            ServerMessage::Welcome {
                id,
                revision,
                cells,
                roots,
                objects,
                players,
                editing_allowed,
            } => {
                let Ok(world) = World::from_parts(&cells, &roots) else {
                    self.notify("Server sent invalid world geometry.");
                    self.socket = None;
                    self.connected = false;
                    return;
                };
                self.world = world;
                self.objects = objects;
                self.players = players;
                self.revision = revision;
                self.editing_allowed = editing_allowed;
                self.local = self
                    .players
                    .iter()
                    .find(|p| p.id == id)
                    .cloned()
                    .unwrap_or_else(|| Player::new(id, self.name.clone()));
                self.mode = ModeState {
                    request: self.local.mode_request,
                    pending: None,
                };
                // Face the arena from the actual spawn, including authored spawns.
                self.local.yaw = (-self.local.position.x).atan2(self.local.position.z);
                self.local.pitch = -0.08;
                self.seq = self.local.last_input;
                self.pending.clear();
                self.camera_offset = Vec3::ZERO;
                self.connected = true;
                self.status = "Connected".into();
                renderer.rebuild_all(&self.world);
                self.rebuild_objects(renderer);
                self.clear_selection();
                self.pending_edit = None;
            }
            ServerMessage::Snapshot {
                revision,
                players,
                projectiles,
                ..
            } => {
                if revision != self.revision {
                    return;
                }
                if let Some(authoritative) = players
                    .iter()
                    .find(|p| p.id == self.local.id && self.mode.accepts_snapshot(p.mode_request))
                {
                    let old = self.local.position;
                    let yaw = self.local.yaw;
                    let pitch = self.local.pitch;
                    if authoritative.health < self.local.health {
                        self.damage_flash = 0.28;
                    }
                    self.pending.retain(|i| i.seq > authoritative.last_input);
                    self.local = authoritative.clone();
                    if let Some(editing) = self.mode.pending {
                        self.local.editing = editing;
                    }
                    for input in &self.pending {
                        simulate_player(&self.world, &mut self.local, input, FIXED_DT);
                    }
                    self.local.yaw = yaw;
                    self.local.pitch = pitch;
                    if old.distance(self.local.position) < 2.0 {
                        self.camera_offset += old - self.local.position;
                    } else {
                        self.camera_offset = Vec3::ZERO;
                    }
                }
                self.players = players;
                self.projectiles = projectiles;
            }
            ServerMessage::ModeChanged { request, editing } => {
                if let Some(editing) = self.mode.acknowledge(request, editing) {
                    self.local.editing = editing;
                    self.local.mode_request = request;
                    self.local.velocity = Vec3::ZERO;
                    self.firing = false;
                }
            }
            ServerMessage::WorldPatch { revision, edits } => {
                if revision <= self.revision {
                    return;
                }
                let start = web_time::Instant::now();
                let chunks = self.world.apply_edits(&edits);
                self.revision = revision;
                // World queries and visible meshes change together on this frame boundary.
                renderer.rebuild(&self.world, &chunks);
                self.last_edit_ms = start.elapsed().as_secs_f32() * 1000.0;
                self.peak_edit_ms = self.peak_edit_ms.max(self.last_edit_ms);
                if self.local.editing {
                    self.audio.play(Cue::Edit);
                }
            }
            ServerMessage::RootPatch { revision, roots } => {
                if revision <= self.revision {
                    return;
                }
                let start = web_time::Instant::now();
                match self.world.apply_root_edits(&roots) {
                    Ok(chunks) => {
                        self.revision = revision;
                        renderer.rebuild(&self.world, &chunks);
                        self.last_edit_ms = start.elapsed().as_secs_f32() * 1000.0;
                        self.peak_edit_ms = self.peak_edit_ms.max(self.last_edit_ms);
                        if self.local.editing {
                            self.audio.play(Cue::Edit);
                        }
                    }
                    Err(error) => {
                        self.notify(format!("Invalid geometry patch: {error}"));
                        self.socket = None;
                        self.connected = false;
                        self.reconnect_at = self.time + 1.0;
                    }
                }
            }
            ServerMessage::Objects { revision, objects } => {
                self.revision = revision;
                self.objects = objects;
                self.rebuild_objects(renderer);
            }
            ServerMessage::Shots { shots } => {
                for shot in shots {
                    if shot.owner == self.local.id {
                        self.audio.play(if self.weapon == 0 {
                            Cue::Shot
                        } else {
                            Cue::Rocket
                        });
                        if shot.hit {
                            self.hit_flash = 0.16;
                            self.audio.play(Cue::Hit);
                        }
                    }
                    self.effects.push((shot, self.time + 0.12));
                }
            }
            ServerMessage::Notice { message } => self.notify(message),
            ServerMessage::EditResult { request, error } => self.finish_edit(request, error),
            ServerMessage::Error { message } => self.notify(message),
        }
    }
    fn rebuild_objects(&self, renderer: &mut Renderer) {
        let mesh = Geometry::objects(&self.objects);
        renderer.objects(&mesh.vertices, &mesh.indices);
    }
    #[cfg(target_arch = "wasm32")]
    pub fn capture_denied(&mut self) {
        self.capture_failed.set(true);
    }
    pub fn capture(&mut self, window: &Window) {
        if !self.connected {
            return;
        }
        self.audio.unlock();
        #[cfg(target_arch = "wasm32")]
        {
            use wasm_bindgen::{JsCast, JsValue};
            use winit::platform::web::WindowExtWebSys;
            self.capture_failed.set(false);
            let request = (|| -> Result<JsValue, JsValue> {
                let canvas = window
                    .canvas()
                    .ok_or_else(|| JsValue::from_str("Game canvas is unavailable"))?;
                let method = js_sys::Reflect::get(
                    canvas.as_ref(),
                    &JsValue::from_str("requestPointerLock"),
                )?
                .dyn_into::<js_sys::Function>()?;
                method.call0(canvas.as_ref())
            })();
            match request {
                Ok(result) => {
                    if let Ok(promise) = result.dyn_into::<js_sys::Promise>() {
                        let failed = self.capture_failed.clone();
                        wasm_bindgen_futures::spawn_local(async move {
                            if let Err(error) = wasm_bindgen_futures::JsFuture::from(promise).await
                            {
                                log::warn!("Mouse capture was denied: {error:?}");
                                failed.set(true);
                            }
                        });
                    }
                }
                Err(error) => {
                    log::warn!("Mouse capture was denied: {error:?}");
                    self.capture_failed.set(true);
                }
            }
            // Only the document's pointerlockchange event confirms acquisition.
        }
        #[cfg(not(target_arch = "wasm32"))]
        if window
            .set_cursor_grab(CursorGrabMode::Locked)
            .or_else(|_| window.set_cursor_grab(CursorGrabMode::Confined))
            .is_ok()
        {
            self.captured = true;
            self.pointer_locked = true;
            window.set_cursor_visible(false);
        }
    }
    pub fn release(&mut self, window: &Window) {
        self.captured = false;
        self.pointer_locked = false;
        self.drag_look = false;
        self.selection_drag = None;
        self.last_cursor = None;
        self.keys.clear();
        self.firing = false;
        let _ = window.set_cursor_grab(CursorGrabMode::None);
        window.set_cursor_visible(true);
    }
    pub fn motion(&mut self, delta: (f64, f64)) {
        if self.captured && self.pointer_locked {
            self.look(delta);
        }
    }
    fn look(&mut self, delta: (f64, f64)) {
        self.local.yaw =
            (self.local.yaw + delta.0 as f32 * 0.0022).rem_euclid(std::f32::consts::TAU);
        self.local.pitch = (self.local.pitch - delta.1 as f32 * 0.0022).clamp(-1.53, 1.53);
    }
    pub fn event(&mut self, event: &WindowEvent, window: &Window, ui_consumed: bool) {
        let size = window.inner_size();
        self.viewport = [size.width.max(1) as f32, size.height.max(1) as f32];
        match event {
            WindowEvent::Focused(false) => self.release(window),
            WindowEvent::CursorMoved { position, .. } => {
                let next = (position.x, position.y);
                if self.captured && !self.pointer_locked && self.drag_look {
                    if let Some(previous) = self.last_cursor {
                        self.look((next.0 - previous.0, next.1 - previous.1));
                    }
                }
                self.last_cursor = Some(next);
                self.update_selection();
            }
            WindowEvent::KeyboardInput { event, .. } => {
                if let PhysicalKey::Code(key) = event.physical_key {
                    if event.state == ElementState::Released {
                        self.keys.remove(&key);
                        self.wheel_remainder = 0.0;
                        return;
                    }
                    if key == KeyCode::Escape
                        && (self.show_settings || self.show_blueprints || self.show_assets)
                    {
                        self.close_text_panel();
                        return;
                    }
                    if self.show_settings || self.show_blueprints || self.show_assets {
                        return;
                    }
                    if ui_consumed && !self.captured {
                        return;
                    }
                    self.keys.insert(key);
                    if event.repeat {
                        return;
                    }
                    match key {
                        KeyCode::Escape => self.release(window),
                        KeyCode::F1 => self.show_help = !self.show_help,
                        KeyCode::F3 => self.show_stats = !self.show_stats,
                        KeyCode::F2 => {
                            self.release(window);
                            self.show_settings = true;
                        }
                        KeyCode::KeyT if self.local.editing => {
                            self.release(window);
                            self.show_assets = true;
                        }
                        KeyCode::KeyB if self.local.editing => {
                            self.release(window);
                            self.show_blueprints = true;
                        }
                        KeyCode::KeyE if self.connected => {
                            if !self.editing_allowed {
                                self.notify("Editing is disabled on this server.");
                                return;
                            }
                            self.local.editing = !self.local.editing;
                            self.local.velocity = Vec3::ZERO;
                            self.firing = false;
                            self.pending.clear();
                            self.clear_selection();
                            let request = self.mode.request(self.local.editing);
                            self.send(ClientMessage::SetMode {
                                request,
                                editing: self.local.editing,
                            });
                        }
                        KeyCode::Space if self.local.editing => self.clear_selection(),
                        KeyCode::KeyS
                            if self.keys.contains(&KeyCode::ControlLeft)
                                || self.keys.contains(&KeyCode::SuperLeft) =>
                        {
                            self.send(ClientMessage::Save)
                        }
                        KeyCode::BracketLeft => self.change_grid(false),
                        KeyCode::BracketRight => self.change_grid(true),
                        KeyCode::Digit1 => {
                            self.material = 0;
                            self.weapon = 0;
                        }
                        KeyCode::Digit2 => {
                            self.material = 1;
                            self.weapon = 1;
                        }
                        KeyCode::Digit3 => self.material = 2,
                        KeyCode::Digit4 => self.material = 3,
                        KeyCode::Digit5 => self.material = 4,
                        KeyCode::Digit6 => self.material = 5,
                        KeyCode::Digit7 => self.material = 6,
                        KeyCode::Digit8 => self.material = 7,
                        KeyCode::KeyC if self.local.editing => self.copy_selection(),
                        KeyCode::KeyV if self.local.editing => self.paste_selection(),
                        KeyCode::KeyR if self.local.editing => self.transform_clipboard(true),
                        KeyCode::KeyM if self.local.editing => self.transform_clipboard(false),
                        KeyCode::KeyU | KeyCode::KeyZ if self.local.editing => {
                            self.clear_selection();
                            let redo = self.keys.contains(&KeyCode::ShiftLeft)
                                || self.keys.contains(&KeyCode::ShiftRight);
                            self.send(if redo {
                                ClientMessage::Redo
                            } else {
                                ClientMessage::Undo
                            });
                        }
                        KeyCode::KeyP if self.local.editing => self.place(self.prop_kind),
                        KeyCode::KeyL if self.local.editing => self.place(ObjectKind::Light),
                        KeyCode::KeyO if self.local.editing => self.place(ObjectKind::Spawn),
                        KeyCode::Delete | KeyCode::Backspace if self.local.editing => {
                            self.remove_object()
                        }
                        _ => {}
                    }
                }
            }
            WindowEvent::MouseInput { state, button, .. } => {
                if self.captured && !self.pointer_locked && *button == MouseButton::Right {
                    self.drag_look = *state == ElementState::Pressed;
                    self.last_cursor = None;
                    return;
                }
                if *state == ElementState::Released {
                    if *button == MouseButton::Left {
                        self.firing = false;
                        self.selection_drag = None;
                        if self.selecting_subtract {
                            self.regions = self.selected_regions();
                            self.selection = None;
                            self.selecting_subtract = false;
                        }
                    }
                    return;
                }
                if !self.captured {
                    if !ui_consumed
                        && !self.show_settings
                        && !self.show_blueprints
                        && !self.show_assets
                    {
                        self.capture(window);
                    }
                    return;
                }
                if ui_consumed && self.show_help {
                    return;
                }
                if self.local.editing {
                    self.update_selection();
                    match button {
                        MouseButton::Left => {
                            self.begin_selection(self.keys.contains(&KeyCode::ShiftLeft))
                        }
                        MouseButton::Right => self.begin_selection(true),
                        MouseButton::Middle => {
                            if self.selection.is_none() {
                                self.begin_selection(false);
                            }
                            self.edit_queue.clear();
                            self.selection_generation = self.selection_generation.wrapping_add(1);
                            self.selection_drag = None;
                            if let (Some(selection), Some(hit)) = (&self.selection, &self.selected)
                            {
                                if self.keys.contains(&KeyCode::ShiftLeft)
                                    || self.keys.contains(&KeyCode::ShiftRight)
                                {
                                    self.edge = selection.cursor_edge(hit.point);
                                    self.corner = None;
                                } else {
                                    self.corner = selection.cursor_corner(hit.point);
                                    self.edge = None;
                                }
                            }
                        }
                        _ => {}
                    }
                } else if *button == MouseButton::Left {
                    self.firing = true;
                }
            }
            WindowEvent::MouseWheel { delta, .. } if self.captured && self.local.editing => {
                self.selection_drag = None;
                self.wheel_remainder += match delta {
                    MouseScrollDelta::LineDelta(_, y) => *y,
                    MouseScrollDelta::PixelDelta(p) => p.y as f32 / 40.0,
                };
                let steps = self.wheel_remainder.trunc().clamp(-8.0, 8.0) as i32;
                self.wheel_remainder -= steps as f32;
                for _ in 0..steps.abs() {
                    self.wheel_step(steps > 0);
                }
            }
            _ => {}
        }
    }
    fn edit_ray(&self) -> (Vec3, Vec3) {
        let forward = self.direction();
        if self.captured && !self.pointer_locked && self.local.editing {
            if let Some((x, y)) = self.last_cursor {
                let nx = 2.0 * x as f32 / self.viewport[0] - 1.0;
                let ny = 1.0 - 2.0 * y as f32 / self.viewport[1];
                let right = forward.cross(Vec3::Y).normalize();
                let up = right.cross(forward);
                let scale = 39.0_f32.to_radians().tan();
                return (
                    self.eye(),
                    (forward
                        + right * nx * scale * self.viewport[0] / self.viewport[1]
                        + up * ny * scale)
                        .normalize(),
                );
            }
        }
        (self.eye(), forward)
    }
    fn update_selection(&mut self) {
        if !self.local.editing || !self.connected {
            self.selected = None;
            return;
        }
        let (origin, direction) = self.edit_ray();
        self.selected = self.world.raycast(origin, direction, 60.0);
        if let Some(drag) = &self.selection_drag {
            match drag.update(origin, direction) {
                Ok(selection) => self.selection = Some(selection),
                Err(EditError::TooLarge) => {
                    self.notify("Selection is too complex. Use fewer volumes.")
                }
                _ => {}
            }
        }
    }
    fn active_selection(&self) -> Option<EditSelection> {
        self.selection
            .clone()
            .or_else(|| {
                self.regions.bounds().map(|bounds| EditSelection {
                    min: bounds.min,
                    max: bounds.max,
                    axis: 1,
                    sign: 1,
                    grid: self.grid,
                })
            })
            .or_else(|| {
                self.selected
                    .as_ref()
                    .and_then(|hit| EditSelection::from_hit(hit, self.grid).ok())
            })
    }
    fn clear_selection(&mut self) {
        self.regions = RegionSet::new();
        self.selecting_subtract = false;
        self.selection = None;
        self.selection_drag = None;
        self.corner = None;
        self.edge = None;
        self.selection_generation = self.selection_generation.wrapping_add(1);
        self.edit_queue.clear();
        self.wheel_remainder = 0.0;
    }
    fn begin_selection(&mut self, extend: bool) {
        let alt = self.keys.contains(&KeyCode::AltLeft) || self.keys.contains(&KeyCode::AltRight);
        let shift =
            self.keys.contains(&KeyCode::ShiftLeft) || self.keys.contains(&KeyCode::ShiftRight);
        let mode = if alt {
            if shift {
                SelectionMode::Subtract
            } else {
                SelectionMode::Add
            }
        } else {
            self.selection_mode
        };
        let extend = extend && !alt;
        let Some(hit) = self.selected.clone() else {
            return;
        };
        let result = if extend {
            self.selection.as_ref().map_or_else(
                || EditSelection::from_hit(&hit, self.grid),
                |selection| selection.extend_to_hit(&hit),
            )
        } else {
            EditSelection::from_hit(&hit, self.grid)
        };
        match result {
            Ok(selection) => {
                if !extend {
                    if mode == SelectionMode::Replace {
                        self.regions = RegionSet::new();
                    } else {
                        self.regions = self.selected_regions();
                    }
                    self.selecting_subtract = mode == SelectionMode::Subtract;
                }
                self.corner = None;
                self.edge = None;
                self.edit_queue.clear();
                self.selection_generation = self.selection_generation.wrapping_add(1);
                self.selection_drag = if extend {
                    None
                } else {
                    Some(selection.begin_drag(hit.point))
                };
                self.selection = Some(selection);
            }
            Err(error) => self.notify(error.to_string()),
        }
    }
    fn change_grid(&mut self, larger: bool) {
        let grid = if larger {
            (self.grid * 2).min(64)
        } else {
            (self.grid / 2).max(1)
        };
        if grid == self.grid {
            return;
        }
        let result = self
            .selection
            .as_ref()
            .map(|selection| selection.resnap(grid))
            .transpose();
        match result {
            Ok(selection) => {
                self.selection_drag = None;
                self.corner = None;
                self.edge = None;
                self.edit_queue.clear();
                self.selection_generation = self.selection_generation.wrapping_add(1);
                self.grid = grid;
                self.selection = selection;
            }
            Err(error) => self.notify(error.to_string()),
        }
    }
    fn wheel_step(&mut self, up: bool) {
        let up = up ^ self.preferences.invert_scroll;
        if self.keys.contains(&KeyCode::KeyG) || self.keys.contains(&KeyCode::ControlLeft) {
            self.change_grid(up);
            return;
        }
        let intent = if self.keys.contains(&KeyCode::KeyY) {
            self.material = (self.material as i32 + if up { 1 } else { -1 })
                .rem_euclid(MATERIAL_NAMES.len() as i32) as u8;
            EditIntent::Paint(self.material)
        } else if self.keys.contains(&KeyCode::KeyF) || self.corner.is_some() || self.edge.is_some()
        {
            EditIntent::Deform(if up { -1 } else { 1 })
        } else if up {
            EditIntent::Carve
        } else {
            EditIntent::Extrude
        };
        if self.edit_queue.len() < 32 {
            self.edit_queue.push_back(intent);
        }
    }
    fn process_edit_queue(&mut self) {
        if !self.connected || !self.local.editing || self.pending_edit.is_some() {
            return;
        }
        let Some(intent) = self.edit_queue.pop_front() else {
            return;
        };
        let Some(selection) = self.active_selection() else {
            self.edit_queue.clear();
            return;
        };
        let pasting = matches!(&intent, EditIntent::Paste(_));
        let regions_after = match &intent {
            EditIntent::Paste(clipboard) => clipboard.mask().translated(selection.min).ok(),
            _ => None,
        };
        let plan = match intent {
            EditIntent::Extrude => selection.plan_extrude(&self.world, self.material),
            EditIntent::Carve => selection.plan_carve(&self.world),
            EditIntent::Paint(material) => {
                selection.plan_paint_regions(&self.world, &self.selected_regions(), material)
            }
            EditIntent::Deform(delta) => match self.edge {
                Some(edge) => selection.plan_deform_edge(&self.world, edge, delta),
                None => selection.plan_deform(&self.world, self.corner, delta),
            },
            EditIntent::Paste(clipboard) => clipboard.plan_paste(&self.world, &selection),
        };
        match plan {
            Ok(plan) => {
                self.edit_request = self.edit_request.wrapping_add(1);
                let message = if !plan.roots.is_empty() {
                    ClientMessage::EditRoots {
                        request: self.edit_request,
                        base_revision: self.revision,
                        roots: plan.roots.clone(),
                    }
                } else {
                    ClientMessage::Edit {
                        request: self.edit_request,
                        edits: plan.edits.clone(),
                    }
                };
                // Check the encoded size before sending, so a large blueprint
                // gets a useful refusal rather than a WebSocket disconnect.
                let text = match serde_json::to_string(&message) {
                    Ok(text) if text.len() <= 16 * 1024 * 1024 => text,
                    _ => {
                        self.edit_queue.clear();
                        self.notify(
                            "This edit exceeds the 16 MiB network limit. Copy a smaller piece.",
                        );
                        return;
                    }
                };
                let Some((sender, _)) = &mut self.socket else {
                    return;
                };
                sender.send(ewebsock::WsMessage::Text(text));
                self.pending_edit = Some(PendingEdit {
                    request: self.edit_request,
                    plan,
                    generation: self.selection_generation,
                    advance_selection: pasting || self.selection.is_some(),
                    regions_after,
                });
            }
            Err(error) => {
                self.edit_queue.clear();
                self.notify(error.to_string());
            }
        }
    }
    fn copy_selection(&mut self) {
        if self.pending_edit.is_some() || !self.edit_queue.is_empty() {
            self.notify("Wait for the current edit before copying.");
            return;
        }
        match Clipboard::copy_regions(&self.world, &self.selected_regions()) {
            Ok(clipboard) => {
                let [x, y, z] = clipboard.dims().map(|v| v as f32 / 8.0);
                self.clipboard = Some(clipboard);
                self.paste_preview = true;
                self.notify(format!(
                    "Copied {x} × {y} × {z} m. Select a destination and press V."
                ));
            }
            Err(error) => self.notify(error.to_string()),
        }
    }
    fn paste_selection(&mut self) {
        let Some(clipboard) = self.clipboard.clone() else {
            self.notify("Select a region and press C to copy it first.");
            return;
        };
        if self.edit_queue.len() < 32 {
            self.edit_queue.push_back(EditIntent::Paste(clipboard));
        }
    }
    fn transform_clipboard(&mut self, rotate: bool) {
        let Some(clipboard) = &mut self.clipboard else {
            self.notify("Select a region and press C to copy it first.");
            return;
        };
        if rotate {
            match clipboard.rotate_y() {
                Ok(()) => {
                    self.notify("Clipboard rotated 90° around Y. Select a destination and press V.")
                }
                Err(error) => self.notify(error.to_string()),
            }
        } else {
            let axis = if self.keys.contains(&KeyCode::ShiftLeft)
                || self.keys.contains(&KeyCode::ShiftRight)
            {
                2
            } else {
                0
            };
            match clipboard.mirror(axis) {
                Ok(()) => self.notify(format!(
                    "Clipboard mirrored along {}. Select a destination and press V.",
                    if axis == 0 { "X" } else { "Z" }
                )),
                Err(error) => self.notify(error.to_string()),
            }
        }
    }
    fn finish_edit(&mut self, request: u64, error: Option<String>) {
        if !self
            .pending_edit
            .as_ref()
            .is_some_and(|p| p.request == request)
        {
            return;
        }
        let pending = self.pending_edit.take().unwrap();
        if let Some(message) = error {
            self.edit_queue.clear();
            self.notify(message);
        } else if pending.advance_selection && pending.generation == self.selection_generation {
            // The server queues the patch before this request's result. A peer's
            // matching patch alone must never acknowledge our outstanding edit.
            if let Some(regions) = pending.regions_after {
                self.regions = regions;
                self.selection = None;
                self.selecting_subtract = false;
                self.paste_preview = false;
            } else {
                self.selection = Some(pending.plan.selection_after);
            }
        }
    }
    fn place(&mut self, kind: ObjectKind) {
        if let Some(hit) = &self.selected {
            let (position, rotation) = match kind {
                ObjectKind::WallLamp if hit.normal.y.abs() < 0.7 => (
                    hit.point + hit.normal * 0.02,
                    hit.normal.x.atan2(hit.normal.z),
                ),
                ObjectKind::HangingLamp => (
                    hit.point + Vec3::Y * if hit.normal.y > 0.5 { 2.0 } else { -0.02 },
                    self.local.yaw,
                ),
                ObjectKind::Light => (hit.point + Vec3::Y * 1.5, self.local.yaw),
                _ => (hit.point + Vec3::Y * 0.02, self.local.yaw),
            };
            self.send(ClientMessage::PlaceObject {
                kind,
                position,
                rotation,
            });
        }
    }
    fn remove_object(&mut self) {
        let (eye, dir) = self.edit_ray();
        let id = self
            .objects
            .iter()
            .filter_map(|o| {
                let key = match o.kind {
                    ObjectKind::WallLamp => Some("wall_lamp"),
                    ObjectKind::HangingLamp => Some("hanging_lamp"),
                    ObjectKind::UtilityBox => Some("utility_box"),
                    ObjectKind::Pipe => Some("pipe"),
                    ObjectKind::Rock => Some("rock"),
                    _ => None,
                };
                if let Some(key) = key {
                    return crate::props::ray_distance(
                        key,
                        o.position.to_array(),
                        o.rotation,
                        eye.to_array(),
                        dir.to_array(),
                    )
                    .filter(|&distance| {
                        distance <= 30.0 && self.world.raycast(eye, dir, distance).is_none()
                    })
                    .map(|distance| (o.id, distance));
                }
                let delta = o.position - eye;
                let along = delta.dot(dir);
                let radius = if matches!(o.kind, ObjectKind::Tree) {
                    2.0
                } else {
                    0.8
                };
                if along > 0.0 && along < 30.0 && (delta - dir * along).length() < radius {
                    Some((o.id, along))
                } else {
                    None
                }
            })
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|x| x.0);
        if let Some(id) = id {
            self.send(ClientMessage::RemoveObject { id });
        }
    }
    pub fn geometry(&self) -> Geometry {
        let mut mesh = Geometry::default();
        for player in &self.players {
            if player.id != self.local.id {
                let mut p = player.clone();
                if let Some(position) = self.remote_positions.get(&p.id) {
                    p.position = *position;
                }
                mesh.player(&p, self.time);
            }
        }
        mesh.shadow_index_count = mesh.indices.len() as u32;
        for player in &self.players {
            if player.id != self.local.id && player.editing {
                let mut p = player.clone();
                if let Some(position) = self.remote_positions.get(&p.id) {
                    p.position = *position;
                }
                mesh.player_guide(&p);
            }
        }
        for rocket in &self.projectiles {
            mesh.cuboid(
                rocket.position,
                Vec3::new(0.16, 0.16, 0.3),
                0.0,
                [1.0, 0.48, 0.12],
                1.0,
            );
            mesh.beam(
                rocket.position,
                rocket.position - rocket.velocity.normalize_or_zero() * 0.8,
                0.055,
                [0.9, 0.28, 0.10],
                1.0,
            );
        }
        for (effect, _) in &self.effects {
            mesh.beam(
                effect.from,
                effect.to,
                0.022,
                if effect.hit {
                    [0.9, 0.95, 0.75]
                } else {
                    [0.98, 0.6, 0.22]
                },
                1.0,
            );
        }
        if self.local.editing {
            for bounds in self.regions.boxes() {
                self.draw_volume(&mut mesh, *bounds, [0.25, 0.75, 0.62]);
            }
            if let Some(selection) = self.active_selection() {
                if self.selection.is_some() || self.regions.is_empty() {
                    self.draw_selection(&mut mesh, &selection);
                }
                if self.paste_preview {
                    if let Some(clipboard) = &self.clipboard {
                        for bounds in clipboard.mask().boxes() {
                            if let Ok(bounds) = bounds.translated(selection.min) {
                                self.draw_volume(&mut mesh, bounds, [0.25, 0.55, 0.9]);
                            }
                        }
                    }
                }
            }
        }
        if self.captured && !self.local.editing {
            let eye = self.eye();
            let f = self.direction();
            let right = f.cross(Vec3::Y).normalize();
            let bob = (self.time * 10.0).sin() * self.local.velocity.length().min(7.0) * 0.0012;
            let hand = eye + f * 0.47 + right * 0.23 - Vec3::Y * (0.24 + bob);
            mesh.beam(
                hand - f * 0.13,
                hand + f * 0.24,
                if self.weapon == 0 { 0.11 } else { 0.17 },
                [0.14, 0.17, 0.17],
                0.0,
            );
            mesh.beam(
                hand + f * 0.08 + Vec3::Y * 0.055,
                hand + f * 0.22 + Vec3::Y * 0.055,
                0.028,
                [0.25, 0.9, 0.79],
                0.9,
            );
            mesh.beam(
                hand - Vec3::Y * 0.05,
                hand - Vec3::Y * 0.16 - f * 0.04,
                0.085,
                [0.48, 0.29, 0.18],
                0.0,
            );
        }
        mesh
    }
    fn draw_volume(&self, mesh: &mut Geometry, bounds: RegionBox, color: [f32; 3]) {
        let corners: [Vec3; 8] = std::array::from_fn(|i| {
            Vec3::from_array(std::array::from_fn(|axis| {
                if i & (1 << axis) == 0 {
                    bounds.min[axis] as f32 / 8.0
                } else {
                    bounds.max[axis] as f32 / 8.0
                }
            }))
        });
        for i in 0..8 {
            for bit in [1, 2, 4] {
                let j = i ^ bit;
                if i < j {
                    mesh.beam(corners[i], corners[j], 0.012, color, 0.9);
                }
            }
        }
    }
    fn draw_selection(&self, mesh: &mut Geometry, selection: &EditSelection) {
        let min = Vec3::new(
            selection.min[0] as f32 / 8.0,
            selection.min[1] as f32 / 8.0,
            selection.min[2] as f32 / 8.0,
        );
        let max = Vec3::new(
            selection.max[0] as f32 / 8.0,
            selection.max[1] as f32 / 8.0,
            selection.max[2] as f32 / 8.0,
        );
        let mut corners = [Vec3::ZERO; 8];
        for (i, p) in corners.iter_mut().enumerate() {
            *p = Vec3::new(
                if i & 1 == 0 { min.x } else { max.x },
                if i & 2 == 0 { min.y } else { max.y },
                if i & 4 == 0 { min.z } else { max.z },
            );
        }
        let active = self.selection.is_some();
        let color = if active {
            [0.92, 0.91, 0.78]
        } else {
            [0.44, 0.51, 0.48]
        };
        for i in 0..8 {
            for bit in [1, 2, 4] {
                let j = i ^ bit;
                if i < j {
                    mesh.beam(
                        corners[i],
                        corners[j],
                        if active { 0.014 } else { 0.008 },
                        color,
                        0.8,
                    );
                }
            }
        }
        let tangents: Vec<_> = (0..3).filter(|axis| *axis != selection.axis).collect();
        for &axis in &tangents {
            let other = if axis == tangents[0] {
                tangents[1]
            } else {
                tangents[0]
            };
            for step in (selection.min[axis]..=selection.max[axis]).step_by(
                (selection.grid
                    * ((selection.max[axis] - selection.min[axis]) / selection.grid / 256 + 1))
                    as usize,
            ) {
                let mut a = min;
                let mut b = max;
                a[selection.axis] = selection.face_plane() + selection.sign as f32 * 0.015;
                b[selection.axis] = a[selection.axis];
                a[axis] = step as f32 / 8.0;
                b[axis] = a[axis];
                a[other] = min[other];
                b[other] = max[other];
                mesh.beam(
                    a,
                    b,
                    0.009,
                    if active {
                        [0.96, 0.52, 0.12]
                    } else {
                        [0.50, 0.57, 0.51]
                    },
                    0.9,
                );
            }
        }
        let face = selection.axis * 2 + usize::from(selection.sign > 0);
        let face_corner = |index: usize| {
            let vertex = Shape::solid().vertices[SHAPE_FACES[face][index]];
            min + (max - min) * Vec3::from_array(vertex.map(|v| v as f32 / 8.0))
        };
        if let Some(corner) = self.corner {
            mesh.cuboid(
                face_corner(corner),
                Vec3::splat(0.12),
                0.0,
                [0.95, 0.44, 0.06],
                1.0,
            );
        }
        if let Some(edge) = self.edge {
            mesh.beam(
                face_corner(edge),
                face_corner((edge + 1) % 4),
                0.04,
                [0.95, 0.44, 0.06],
                1.0,
            );
        }
    }
    pub fn ui(&mut self, ctx: &egui::Context, renderer: &Renderer, window: &Window) {
        let screen = ctx.screen_rect();
        let painter = ctx.layer_painter(egui::LayerId::new(
            egui::Order::Background,
            egui::Id::new("hud"),
        ));
        let shade = egui::Color32::from_rgba_unmultiplied(7, 10, 12, 205);
        for (rect, top, bottom) in [
            (
                egui::Rect::from_min_max(
                    screen.min,
                    egui::pos2(screen.right(), screen.top() + 90.0),
                ),
                shade,
                egui::Color32::TRANSPARENT,
            ),
            (
                egui::Rect::from_min_max(
                    egui::pos2(screen.left(), screen.bottom() - 125.0),
                    screen.max,
                ),
                egui::Color32::TRANSPARENT,
                shade,
            ),
        ] {
            let mut mesh = egui::Mesh::default();
            mesh.colored_vertex(rect.left_top(), top);
            mesh.colored_vertex(rect.right_top(), top);
            mesh.colored_vertex(rect.right_bottom(), bottom);
            mesh.colored_vertex(rect.left_bottom(), bottom);
            mesh.add_triangle(0, 1, 2);
            mesh.add_triangle(0, 2, 3);
            painter.add(egui::Shape::mesh(mesh));
        }
        let mono = |size| egui::FontId::monospace(size);
        painter.text(
            egui::pos2(24.0, 20.0),
            egui::Align2::LEFT_TOP,
            "SAUERTEIG",
            mono(19.0),
            PAPER,
        );
        let mode = if self.local.editing {
            "EDIT"
        } else {
            "DEATHMATCH"
        };
        painter.text(
            egui::pos2(screen.right() - 24.0, 23.0),
            egui::Align2::RIGHT_TOP,
            if self.connected {
                format!("{} PLAYERS / {mode}", self.players.len())
            } else {
                "CONNECTING".into()
            },
            mono(11.0),
            MUTED,
        );
        if self.show_stats {
            painter.text(
                egui::pos2(screen.right() - 24.0, 44.0),
                egui::Align2::RIGHT_TOP,
                format!(
                    "{:.0} fps / {:.1} ms / {} tris",
                    self.fps,
                    1000.0 / self.fps,
                    renderer.visible_triangles()
                ),
                mono(10.0),
                MUTED,
            );
            painter.text(
                egui::pos2(screen.right() - 24.0, 60.0),
                egui::Align2::RIGHT_TOP,
                &renderer.adapter_name,
                mono(10.0),
                MUTED,
            );
            painter.text(
                egui::pos2(screen.right() - 24.0, 76.0),
                egui::Align2::RIGHT_TOP,
                format!(
                    "edit CPU {:.2} ms / peak {:.2} ms / textures {:.1} MiB",
                    self.last_edit_ms,
                    self.peak_edit_ms,
                    renderer.materials.texture_bytes as f32 / 1048576.0
                ),
                mono(10.0),
                MUTED,
            );
        }
        if self.connected && (!self.local.editing || self.pointer_locked) {
            let c = screen.center();
            let color = if self.hit_flash > 0.0 { COPPER } else { PAPER };
            painter.line_segment(
                [c - egui::vec2(5.0, 0.0), c + egui::vec2(5.0, 0.0)],
                egui::Stroke::new(1.0_f32, color),
            );
            painter.line_segment(
                [c - egui::vec2(0.0, 5.0), c + egui::vec2(0.0, 5.0)],
                egui::Stroke::new(1.0_f32, color),
            );
        }
        if self.damage_flash > 0.0 {
            painter.rect_stroke(
                screen.shrink(3.0),
                0.0,
                egui::Stroke::new(
                    4.0_f32,
                    egui::Color32::from_rgba_unmultiplied(
                        190,
                        45,
                        20,
                        (self.damage_flash / 0.28 * 180.0) as u8,
                    ),
                ),
                egui::StrokeKind::Inside,
            );
        }
        let bottom = screen.bottom() - 22.0;
        if self.local.editing {
            let extent = self
                .active_selection()
                .map(|s| {
                    let d = s.dims().map(|v| v as f32 / 8.0);
                    format!("{}x{}x{}m", d[0], d[1], d[2])
                })
                .unwrap_or_else(|| "--".into());
            painter.text(
                egui::pos2(24.0, bottom - 47.0),
                egui::Align2::LEFT_BOTTOM,
                format!(
                    "GRID {}m   {} {}   {}",
                    self.grid as f32 / 8.0,
                    if self.selection.is_some() {
                        "SELECTED"
                    } else {
                        "AIM"
                    },
                    extent,
                    MATERIAL_NAMES[self.material as usize]
                ),
                mono(13.0),
                PAPER,
            );
            painter.text(
                egui::pos2(24.0, bottom - 22.0),
                egui::Align2::LEFT_BOTTOM,
                "LMB DRAG select   WHEEL push/pull   G+WHEEL grid   SPACE clear",
                mono(11.0),
                MUTED,
            );
            painter.text(
                egui::pos2(24.0, bottom),
                egui::Align2::LEFT_BOTTOM,
                "E play   T assets   B blueprints   U undo   F1 controls",
                mono(11.0),
                MUTED,
            );
        } else {
            painter.text(
                egui::pos2(24.0, bottom - 22.0),
                egui::Align2::LEFT_BOTTOM,
                format!(
                    "{:03} HEALTH    {:02} FRAGS",
                    self.local.health, self.local.score
                ),
                mono(21.0),
                PAPER,
            );
            painter.text(
                egui::pos2(24.0, bottom),
                egui::Align2::LEFT_BOTTOM,
                "E edit   1/2 weapon   TAB scores   F1 controls",
                mono(11.0),
                MUTED,
            );
            painter.text(
                egui::pos2(screen.right() - 24.0, bottom - 22.0),
                egui::Align2::RIGHT_BOTTOM,
                if self.weapon == 0 { "RIFLE" } else { "ROCKET" },
                mono(14.0),
                COPPER,
            );
        }
        if self.notice_until > self.time {
            painter.text(
                egui::pos2(24.0, screen.top() + 58.0),
                egui::Align2::LEFT_TOP,
                &self.notice,
                mono(11.0),
                COPPER,
            );
        }
        if self.show_help {
            egui::Window::new("Controls")
                .anchor(egui::Align2::LEFT_TOP, [24.0, 90.0])
                .collapsible(false)
                .resizable(false)
                .show(ctx, |ui| {
                    egui::ScrollArea::vertical()
                        .max_height((screen.height() - 180.0).max(160.0))
                        .show(ui, |ui| {
                            for line in [
                                "WASD          move / fly along view",
                                "E             edit / fight",
                                "Space         jump / clear selection",
                                "PageUp/Down   fly vertically",
                                "LMB drag      select region",
                                "RMB           extend / reorient selection",
                                "Wheel down/up pull / push geometry",
                                "G + wheel     grid size",
                                "F + wheel     shape selected face",
                                "MMB           select a face corner",
                                "Shift + MMB   select a face edge",
                                "Y + wheel     change selected material",
                                "1–8           choose material",
                                "U / Z         undo geometry",
                                "Shift U / Z   redo geometry",
                                "Alt + drag    add a selection volume",
                                "Alt Shift drag subtract a volume",
                                "C / V         copy / paste selected volumes",
                                "B             blueprint library",
                                "F2            settings / scroll direction",
                                "R             rotate clipboard 90° around Y",
                                "M / Shift M   mirror clipboard X / Z",
                                "T             materials and props",
                                "P / L / O     place chosen prop / light / spawn",
                                "Delete        remove aimed object",
                                "Ctrl/Cmd S    save",
                                "Esc           release mouse",
                                "F1            close controls",
                            ] {
                                ui.label(
                                    egui::RichText::new(line)
                                        .monospace()
                                        .size(12.0)
                                        .color(PAPER),
                                );
                            }
                        });
                    if !self.pointer_locked {
                        ui.separator();
                        ui.label("Without mouse lock: right-drag looks; Shift+LMB extends.");
                    }
                });
        }
        if self.keys.contains(&KeyCode::Tab) {
            egui::Window::new("Scores")
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .collapsible(false)
                .resizable(false)
                .show(ctx, |ui| {
                    for p in &self.players {
                        ui.label(format!(
                            "{:>3}    {}{}",
                            p.score,
                            p.name,
                            if p.editing { " [edit]" } else { "" }
                        ));
                    }
                });
        }
        if !self.captured && !self.show_settings && !self.show_blueprints && !self.show_assets {
            egui::Area::new(egui::Id::new("enter"))
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    egui::Frame::new()
                        .fill(egui::Color32::from_rgba_unmultiplied(13, 17, 19, 245))
                        .inner_margin(20.0)
                        .stroke(egui::Stroke::new(
                            1.0_f32,
                            egui::Color32::from_rgb(65, 70, 70),
                        ))
                        .show(ui, |ui| {
                            ui.set_width(230.0);
                            if self.connected {
                                if ui
                                    .add_sized(
                                        [230.0, 36.0],
                                        egui::Button::new(
                                            egui::RichText::new("ENTER").monospace().color(PAPER),
                                        ),
                                    )
                                    .clicked()
                                {
                                    self.capture(window);
                                }
                                ui.horizontal(|ui| {
                                    if ui.button("Settings").clicked() {
                                        self.show_settings = true;
                                    }
                                    if self.local.editing && ui.button("Assets").clicked() {
                                        self.show_assets = true;
                                    }
                                    if self.local.editing && ui.button("Blueprints").clicked() {
                                        self.show_blueprints = true;
                                    }
                                });
                                ui.add_space(8.0);
                                ui.label(
                                    egui::RichText::new("WASD move  /  E edit  /  F1 controls")
                                        .monospace()
                                        .size(10.0)
                                        .color(MUTED),
                                );
                            } else {
                                ui.label(&self.status);
                                if ui.button("Reconnect").clicked() {
                                    self.socket = None;
                                    self.reconnect_at = 0.0;
                                }
                            }
                        });
                });
        }
        self.asset_panel(ctx, renderer, window);
        self.local_panels(ctx, window);
    }
}
