mod audio;
mod blueprints;
pub mod capture;
mod editor;
mod failure;
mod game;
mod hosting;
mod launcher;
mod lighting;
mod materials;
mod motion;
mod network;
mod props;
mod regions;
mod render;
mod scene;
mod user_data;
mod world_saves;

use game::Game;
use launcher::{ActiveSession, LaunchAction, LaunchOptions, Launcher, MenuAction};
use render::Renderer;
use std::{sync::Arc, time::Instant};
use winit::{
    application::ApplicationHandler,
    dpi::LogicalSize,
    event::{DeviceEvent, DeviceId, ElementState, WindowEvent},
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy},
    keyboard::{KeyCode, PhysicalKey},
    window::{Window, WindowId},
};

struct Runtime {
    window: Arc<Window>,
    renderer: Renderer,
    game: Game,
    launcher: Launcher,
    session: Option<Session>,
    menu_open: bool,
    change: Option<SessionChange>,
    exit_requested: bool,
    host: Option<hosting::HostedServer>,
    context: egui::Context,
    egui: egui_winit::State,
    last_tick: Instant,
    last_redraw: Instant,
    ticked_this_frame: bool,
    occluded: bool,
}
struct Session {
    title: String,
    endpoint: String,
    name: String,
    hosting: bool,
    lan_port: Option<u16>,
}
enum SessionAction {
    MainMenu,
    Rejoin,
}

enum Destination {
    Launch(LaunchAction),
    Close,
}

#[derive(Debug, PartialEq, Eq)]
enum ChangePhase {
    Draining,
    SavingHost,
    Blocked { unconfirmed: bool },
}

struct SessionChange {
    destination: Destination,
    phase: ChangePhase,
    since: Instant,
    needs_sync: bool,
    sync: Option<u64>,
}

#[derive(Debug, PartialEq, Eq)]
enum DrainProgress {
    Waiting,
    Ready,
    Blocked(String),
}

impl SessionChange {
    fn new(destination: Destination, needs_sync: bool) -> Self {
        Self {
            destination,
            phase: ChangePhase::Draining,
            since: Instant::now(),
            needs_sync,
            sync: None,
        }
    }

    fn retry(&mut self, connected: bool) {
        // A failed connection cannot turn an unconfirmed switch into a safe
        // one. Only an explicit override may skip the server barrier.
        self.needs_sync |= connected;
        self.sync = None;
        self.since = Instant::now();
        self.phase = ChangePhase::Draining;
    }

    fn needs_sync_request(&self, connected: bool, edits_pending: bool) -> bool {
        self.phase == ChangePhase::Draining
            && self.needs_sync
            && self.sync.is_none()
            && connected
            && !edits_pending
    }

    fn poll_drain(
        &mut self,
        connected: bool,
        synced: bool,
        writes_pending: bool,
        now: Instant,
        write_error: impl FnOnce() -> Option<String>,
    ) -> DrainProgress {
        if self.phase != ChangePhase::Draining {
            return DrainProgress::Waiting;
        }
        let network_ready = !self.needs_sync || (self.sync.is_some() && synced);
        let connection_lost = self.needs_sync && !connected;
        if connection_lost || (!network_ready && (now - self.since).as_secs() >= 15) {
            self.phase = ChangePhase::Blocked { unconfirmed: true };
            return DrainProgress::Blocked(if connection_lost {
                "Connection lost before your edits were confirmed. Reconnect and retry, or continue without confirmation."
            } else {
                "The server has not confirmed your edits. Retry to wait again, or continue without confirmation."
            }.into());
        }
        if !network_ready || writes_pending {
            return DrainProgress::Waiting;
        }
        if let Some(error) = write_error() {
            self.phase = ChangePhase::Blocked { unconfirmed: false };
            return DrainProgress::Blocked(format!("Local changes could not be saved: {error}"));
        }
        self.phase = ChangePhase::SavingHost;
        DrainProgress::Ready
    }

    fn finish(self, saved: Result<(), String>) -> Result<Destination, String> {
        debug_assert_eq!(self.phase, ChangePhase::SavingHost);
        saved.map(|()| self.destination)
    }
}

enum ChangeAction {
    Cancel,
    Retry,
    ContinueUnconfirmed,
}

impl Drop for Runtime {
    fn drop(&mut self) {
        self.game.release(&self.window);
        if let Err(error) = user_data::flush_pending() {
            log::error!("Local changes could not be saved during shutdown: {error}");
        }
        // Fields drop in declaration order: Game closes its socket before the
        // HostedServer fallback waits for its final save.
    }
}
impl Runtime {
    async fn new(window: Arc<Window>, options: LaunchOptions) -> Result<Self, String> {
        let mut renderer = Renderer::new(window.clone()).await?;
        let mut launcher = Launcher::new(options);
        let initial_action = launcher.initial_action();
        let game = Game::new(String::new(), String::new());
        renderer.rebuild_all(&game.world);
        let objects = scene::Geometry::objects(&game.objects);
        renderer.objects(&objects.vertices, &objects.indices);
        let context = egui::Context::default();
        let mut style = (*context.style()).clone();
        style.visuals = egui::Visuals::dark();
        style.visuals.selection.bg_fill = egui::Color32::from_rgb(49, 76, 70);
        style.visuals.widgets.active.bg_fill = egui::Color32::from_rgb(112, 82, 57);
        style.spacing.item_spacing = egui::vec2(8.0, 6.0);
        context.set_style(style);
        let egui = egui_winit::State::new(
            context.clone(),
            egui::ViewportId::ROOT,
            window.as_ref(),
            Some(window.scale_factor() as f32),
            None,
            None,
        );
        let mut runtime = Self {
            window,
            renderer,
            game,
            launcher,
            session: None,
            menu_open: true,
            change: None,
            exit_requested: false,
            host: None,
            context,
            egui,
            last_tick: Instant::now(),
            last_redraw: Instant::now(),
            ticked_this_frame: false,
            occluded: false,
        };
        if let Some(action) = initial_action {
            runtime.launch(action);
        }
        Ok(runtime)
    }

    fn replace_game(&mut self, endpoint: String, name: String) {
        self.game.release(&self.window);
        // Dropping Game closes its socket. No old input or edits survive a
        // session switch, and an old Welcome cannot enter the new session.
        self.game = Game::new(endpoint, name);
        self.renderer.rebuild_all(&self.game.world);
        let objects = scene::Geometry::objects(&self.game.objects);
        self.renderer.objects(&objects.vertices, &objects.indices);
    }

    fn launch(&mut self, action: LaunchAction) {
        if self.session.is_some() || self.change.is_some() {
            return;
        }
        let session = match action {
            LaunchAction::Join { endpoint, name } => Session {
                title: "Server session".into(),
                endpoint,
                name,
                hosting: false,
                lan_port: None,
            },
            LaunchAction::Host {
                path,
                title,
                name,
                lan,
            } => match hosting::HostedServer::start(path, lan) {
                Ok(host) => {
                    let session = Session {
                        title,
                        endpoint: host.endpoint(),
                        name,
                        hosting: true,
                        lan_port: lan.then_some(host.address().port()),
                    };
                    self.host = Some(host);
                    session
                }
                Err(error) => {
                    self.launcher.status = format!("Could not open world: {error}");
                    return;
                }
            },
        };
        self.replace_game(session.endpoint.clone(), session.name.clone());
        self.session = Some(session);
        self.menu_open = false;
    }

    fn main_menu(&mut self) {
        self.game.release(&self.window);
        self.game.close_text_panel();
        self.menu_open = true;
        self.launcher.open();
    }

    fn resume(&mut self) {
        if self.session.is_some() && self.change.is_none() {
            self.menu_open = false;
            self.game.capture(&self.window);
        }
    }

    fn accepts_game_input(&self) -> bool {
        self.session.is_some() && !self.menu_open && self.change.is_none()
    }

    fn begin_change(&mut self, destination: Destination) {
        self.main_menu();
        if let Some(change) = &mut self.change {
            // A window close overrides a queued world, including while the
            // old host's final save is in progress.
            if matches!(destination, Destination::Close) {
                change.destination = destination;
            }
            return;
        }
        // Cancelling an earlier blocked switch must not forget documents
        // whose failure has already been reported and consumed.
        self.game.retry_local_saves();
        self.change = Some(SessionChange::new(
            destination,
            self.session.is_some() && self.game.needs_edit_confirmation(),
        ));
    }

    fn finish_change(&mut self, saved: Result<(), String>) {
        let Some(change) = self.change.take() else {
            return;
        };
        match change.finish(saved) {
            Ok(Destination::Launch(action)) => self.launch(action),
            Ok(Destination::Close) => self.exit_requested = true,
            Err(error) => {
                // A failed final save never opens the queued destination or
                // dismisses the error by closing the window.
                self.menu_open = true;
                self.launcher.status = format!("Local server stopped with an error: {error}");
            }
        }
    }

    fn stop_session(&mut self) {
        // Close the socket before graceful server shutdown, which waits for
        // active connections and the final world save.
        self.replace_game(String::new(), String::new());
        self.session = None;
        if let Some(host) = &mut self.host {
            host.stop();
            return;
        }
        self.finish_change(Ok(()));
    }

    fn poll_change(&mut self) {
        let Some(change) = &mut self.change else {
            return;
        };
        if change.needs_sync_request(self.game.connected, self.game.edits_pending()) {
            // The ordered reply also covers Undo/Redo and other messages
            // sent before this barrier, which have no edit request token.
            change.sync = self.game.request_sync();
        }
        let synced = change.sync.is_some_and(|request| self.game.synced(request));
        match change.poll_drain(
            self.game.connected,
            synced,
            user_data::pending_writes(),
            Instant::now(),
            user_data::take_write_error,
        ) {
            DrainProgress::Ready => self.stop_session(),
            DrainProgress::Blocked(error) => self.launcher.status = error,
            DrainProgress::Waiting => {}
        }
    }

    fn poll_host(&mut self) {
        if let Some(result) = self.host.as_mut().and_then(hosting::HostedServer::poll) {
            self.host = None;
            if self
                .change
                .as_ref()
                .is_some_and(|change| change.phase == ChangePhase::SavingHost)
            {
                self.finish_change(result);
                return;
            }
            // Unexpected shutdown or startup failure also returns home, but
            // cannot complete an in-progress switch whose edits never synced.
            self.replace_game(String::new(), String::new());
            self.session = None;
            self.change = None;
            self.menu_open = true;
            self.launcher.status = match result {
                Ok(()) => "Local server stopped. Your saved worlds are below.".into(),
                Err(error) => format!("Local server stopped with an error: {error}"),
            };
        }
    }

    fn session_ui(&self, ctx: &egui::Context) -> Option<SessionAction> {
        let session = self.session.as_ref()?;
        if self.game.captured {
            return None;
        }
        let mut action = None;
        egui::Window::new("Session")
            .anchor(egui::Align2::LEFT_BOTTOM, [16.0, -16.0])
            .resizable(false)
            .collapsible(false)
            .default_width(300.0)
            .show(ctx, |ui| {
                if ui
                    .add_sized([ui.available_width(), 36.0], egui::Button::new("Main menu"))
                    .clicked()
                {
                    action = Some(SessionAction::MainMenu);
                }
                ui.separator();
                ui.strong(&session.title);
                ui.label(format!("Playing as {}", session.name));
                ui.label(if self.game.connected {
                    "Connected"
                } else {
                    "Connecting / reconnecting…"
                });
                ui.horizontal_wrapped(|ui| {
                    ui.label("Server:");
                    ui.monospace(&session.endpoint);
                    if ui.small_button("Copy").clicked() {
                        ui.ctx().copy_text(session.endpoint.clone());
                    }
                });
                if let Some(port) = session.lan_port {
                    ui.label(format!("LAN hosting · port {port}"));
                    ui.weak(format!(
                        "Other computers: ws://<this computer’s LAN IP>:{port}/ws"
                    ));
                } else if session.hosting {
                    ui.weak("Local hosting · only this computer can connect.");
                }
                if !self.launcher.status.is_empty() {
                    ui.label(&self.launcher.status);
                }
                if !self.game.connected && ui.button("Rejoin now").clicked() {
                    action = Some(SessionAction::Rejoin);
                }
            });
        action
    }

    /// True while the window is being redrawn. Some systems send no redraws to
    /// a minimized or covered window, and some report neither state, so a
    /// window that has gone quiet also counts as not drawing.
    fn drawing(&self) -> bool {
        !self.occluded
            && self.window.is_minimized() != Some(true)
            && self.last_redraw.elapsed() < REDRAW_TIMEOUT
    }

    /// The updates that never depend on drawing: the host, the game's network
    /// and simulation, and a session change in progress. `about_to_wait` runs
    /// it once per drawn frame, or every `IDLE_TICK` while nothing draws, so a
    /// minimized window keeps draining and can finish closing.
    fn tick(&mut self) {
        let now = Instant::now();
        let dt = (now - self.last_tick).as_secs_f32();
        self.last_tick = now;
        self.ticked_this_frame = true;
        self.poll_host();
        if self.session.is_some() {
            self.game.update(dt, &mut self.renderer);
            if self.game.captured && (!self.game.connected || !self.accepts_game_input()) {
                self.game.release(&self.window);
            }
        }
        self.poll_change();
    }

    /// Leaving through the application's own quit, which ends the event loop
    /// without a close request: starts the same close a window's close button
    /// does and runs it to its end, so the host's final save finishes first.
    fn finish_for_exit(&mut self) {
        if self.exit_requested {
            return;
        }
        self.begin_change(Destination::Close);
        let started = Instant::now();
        while !self.exit_requested {
            self.tick();
            let waiting = match &self.change {
                None => false,
                Some(change) if change.phase == ChangePhase::SavingHost => true,
                // Waiting on the server or local writes gets a limit: its
                // own, then this one. The host's save is then waited for by
                // `Drop`, which has none.
                Some(change) => {
                    change.phase == ChangePhase::Draining && started.elapsed() < EXIT_DRAIN_LIMIT
                }
            };
            if !waiting {
                break;
            }
            std::thread::sleep(IDLE_TICK / 10);
        }
        if !self.exit_requested && self.session.is_none() && self.change.is_none() {
            // The final save failed: the dialog is the only place left to say so.
            failure::show_problem("Sauerteig could not save your world", &self.launcher.status);
        }
    }

    fn render(&mut self) {
        self.last_redraw = Instant::now();
        self.ticked_this_frame = false;
        let raw = self.egui.take_egui_input(&self.window);
        let mut menu_action = None;
        let mut session_action = None;
        let mut change_action = None;
        let context = self.context.clone();
        let output = context.run(raw, |ctx| {
            if let Some(change) = &self.change {
                egui::CentralPanel::default().show(ctx, |ui| {
                    ui.vertical_centered(|ui| {
                        ui.add_space(40.0);
                        match change.phase {
                            ChangePhase::Draining => {
                                ui.spinner();
                                ui.label("Finishing edits and local saves…");
                                if ui.button("Cancel").clicked() {
                                    change_action = Some(ChangeAction::Cancel);
                                }
                            }
                            ChangePhase::SavingHost => {
                                ui.spinner();
                                ui.label("Saving world and stopping server…");
                            }
                            ChangePhase::Blocked { unconfirmed } => {
                                ui.heading("Could not finish the current session");
                                ui.label(&self.launcher.status);
                                ui.add_space(12.0);
                                if ui.button("Retry").clicked() {
                                    change_action = Some(ChangeAction::Retry);
                                }
                                if unconfirmed {
                                    ui.weak("Unconfirmed edits may be lost.");
                                    let label = if matches!(change.destination, Destination::Close)
                                    {
                                        "Close without confirmation"
                                    } else {
                                        "Switch without confirmation"
                                    };
                                    if ui.button(label).clicked() {
                                        change_action = Some(ChangeAction::ContinueUnconfirmed);
                                    }
                                }
                                if ui.button("Return to main menu").clicked() {
                                    change_action = Some(ChangeAction::Cancel);
                                }
                            }
                        }
                    });
                });
            } else if self.menu_open {
                menu_action = self.launcher.ui(
                    ctx,
                    self.session.as_ref().map(|session| ActiveSession {
                        title: &session.title,
                        hosting_lan: session.lan_port.is_some(),
                    }),
                );
            } else if self.session.is_some() {
                self.game.ui(ctx, &self.renderer, &self.window);
                session_action = self.session_ui(ctx);
            }
        });
        match menu_action {
            Some(MenuAction::Resume) => self.resume(),
            Some(MenuAction::Launch(action)) if self.session.is_some() => {
                self.begin_change(Destination::Launch(action));
            }
            Some(MenuAction::Launch(action)) => self.launch(action),
            None => {}
        }
        match change_action {
            Some(ChangeAction::Cancel) => {
                self.change = None;
            }
            Some(ChangeAction::Retry) => {
                self.game.retry_local_saves();
                if let Some(change) = &mut self.change {
                    change.retry(self.game.connected);
                }
            }
            Some(ChangeAction::ContinueUnconfirmed) => {
                self.game.retry_local_saves();
                if let Some(change) = &mut self.change {
                    change.retry(false);
                    change.needs_sync = false;
                }
            }
            None => {}
        }
        match session_action {
            Some(SessionAction::MainMenu) => self.main_menu(),
            Some(SessionAction::Rejoin) => {
                if let Some(session) = &self.session {
                    self.replace_game(session.endpoint.clone(), session.name.clone());
                }
            }
            None => {}
        }
        if !self.game.captured {
            self.egui
                .handle_platform_output(&self.window, output.platform_output.clone());
        }
        self.renderer.apply_settings(self.game.render_settings());
        let mesh = self.game.geometry();
        match self.renderer.draw(
            self.game.eye(),
            self.game.direction(),
            self.game.time,
            &self.game.world,
            &self.game.objects,
            &mesh.vertices,
            &mesh.indices,
            mesh.shadow_index_count,
            mesh.opaque_index_count,
            output,
            &self.context,
        ) {
            Ok(()) => {}
            Err(wgpu::SurfaceError::Lost | wgpu::SurfaceError::Outdated) => {
                let size = self.window.inner_size();
                self.renderer.resize(size.width, size.height);
            }
            Err(wgpu::SurfaceError::OutOfMemory) => log::error!("GPU memory exhausted"),
            Err(error) => log::warn!("Frame skipped: {error}"),
        }
    }
}
/// The longest a drawn window waits between redraws before it counts as not
/// drawing.
const REDRAW_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(250);
/// The tick rate while nothing draws.
const IDLE_TICK: std::time::Duration = std::time::Duration::from_millis(50);
/// How long quitting waits for the server to confirm edits before it stops
/// waiting and lets the host's own final save finish.
const EXIT_DRAIN_LIMIT: std::time::Duration = std::time::Duration::from_secs(20);

enum UserEvent {
    Ready(Result<Box<Runtime>, String>),
}
struct App {
    runtime: Option<Box<Runtime>>,
    window: Option<Arc<Window>>,
    proxy: EventLoopProxy<UserEvent>,
    options: Option<LaunchOptions>,
    failed: bool,
}
impl App {
    fn fail(&mut self, error: String, event_loop: &ActiveEventLoop) {
        failure::report(&error);
        self.failed = true;
        event_loop.exit();
    }
}
impl ApplicationHandler<UserEvent> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let attrs = Window::default_attributes()
            .with_title("Sauerteig")
            .with_inner_size(LogicalSize::new(1280.0, 800.0));
        let window = match event_loop.create_window(attrs) {
            Ok(window) => Arc::new(window),
            Err(e) => {
                self.fail(e.to_string(), event_loop);
                return;
            }
        };
        self.window = Some(window.clone());
        let options = self.options.take().unwrap_or_default();
        let ready = pollster::block_on(Runtime::new(window, options)).map(Box::new);
        let _ = self.proxy.send_event(UserEvent::Ready(ready));
    }
    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: UserEvent) {
        match event {
            UserEvent::Ready(Ok(runtime)) => {
                self.runtime = Some(runtime);
            }
            UserEvent::Ready(Err(error)) => self.fail(error, event_loop),
        }
    }
    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        if matches!(event, WindowEvent::CloseRequested) {
            if let Some(runtime) = &mut self.runtime {
                runtime.begin_change(Destination::Close);
                if runtime.exit_requested {
                    event_loop.exit();
                }
            } else {
                event_loop.exit();
            }
            return;
        }
        let Some(runtime) = &mut self.runtime else {
            return;
        };
        let response = runtime.egui.on_window_event(&runtime.window, &event);
        let menu_escape = runtime.menu_open
            && runtime.session.is_some()
            && runtime.change.is_none()
            && !response.consumed
            && matches!(
                &event,
                WindowEvent::KeyboardInput { event, .. }
                    if event.physical_key == PhysicalKey::Code(KeyCode::Escape)
                        && event.state == ElementState::Pressed
                        && !event.repeat
            );
        if menu_escape {
            runtime.resume();
        } else if runtime.accepts_game_input() {
            runtime
                .game
                .event(&event, &runtime.window, response.consumed);
        }
        match event {
            WindowEvent::Resized(size) => runtime.renderer.resize(size.width, size.height),
            WindowEvent::Occluded(occluded) => runtime.occluded = occluded,
            WindowEvent::RedrawRequested => runtime.render(),
            _ => {}
        }
        if runtime.exit_requested {
            event_loop.exit();
        }
    }
    fn device_event(
        &mut self,
        _event_loop: &ActiveEventLoop,
        _device_id: DeviceId,
        event: DeviceEvent,
    ) {
        if let DeviceEvent::MouseMotion { delta } = event {
            if let Some(runtime) = &mut self.runtime {
                if runtime.accepts_game_input() {
                    runtime.game.motion(delta);
                }
            }
        }
    }
    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let Some(runtime) = &mut self.runtime else {
            if let Some(window) = &self.window {
                window.request_redraw();
            }
            return;
        };
        let drawing = runtime.drawing();
        let due = if drawing {
            !runtime.ticked_this_frame
        } else {
            runtime.last_tick.elapsed() >= IDLE_TICK
        };
        if due {
            runtime.tick();
        }
        if runtime.exit_requested {
            event_loop.exit();
            return;
        }
        // While drawing, each redraw paces the loop. Otherwise it sleeps until
        // the next tick, and still asks for a redraw so that it notices when
        // drawing resumes.
        runtime.window.request_redraw();
        event_loop.set_control_flow(if drawing {
            ControlFlow::Poll
        } else {
            ControlFlow::WaitUntil(runtime.last_tick + IDLE_TICK)
        });
    }
    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        // Every way out ends here, including the menu's Quit and Cmd+Q on
        // macOS, which end the process right after this returns.
        if let Some(mut runtime) = self.runtime.take() {
            runtime.finish_for_exit();
        }
    }
}
fn options() -> LaunchOptions {
    let mut options = LaunchOptions::default();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--server" => {
                if let Some(value) = args.next() {
                    options.server = Some(value);
                }
            }
            "--name" => {
                if let Some(value) = args.next() {
                    options.name = Some(value);
                }
            }
            "--help" | "-h" => {
                println!("Sauerteig\n  Open without arguments to create or continue a saved world.\n  --server ws://127.0.0.1:3000/ws  Join directly\n  --name Player\n\nWASD move · Space jump · E edit · Esc release mouse");
                std::process::exit(0);
            }
            _ => {}
        }
    }
    options
}
pub fn run() {
    let _ = env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn"))
        .try_init();
    let options = options();
    let event_loop = match EventLoop::<UserEvent>::with_user_event().build() {
        Ok(event_loop) => event_loop,
        Err(error) => {
            failure::report(&format!("The window system could not be started: {error}"));
            std::process::exit(1);
        }
    };
    event_loop.set_control_flow(ControlFlow::Poll);
    let mut app = App {
        runtime: None,
        window: None,
        proxy: event_loop.create_proxy(),
        options: Some(options),
        failed: false,
    };
    let result = event_loop.run_app(&mut app);
    // Dropped before the process ends, so a failed start never leaves a window
    // or a locked world behind.
    drop(app.runtime.take());
    drop(app.window.take());
    if let Err(error) = result {
        failure::record(&error.to_string());
        std::process::exit(1);
    }
    if app.failed {
        std::process::exit(1);
    }
}

#[cfg(test)]
mod session_change_tests {
    use super::*;
    use std::time::Duration;

    fn switching(needs_sync: bool) -> SessionChange {
        SessionChange::new(
            Destination::Launch(LaunchAction::Join {
                endpoint: "ws://next-world:3000/ws".into(),
                name: "Player".into(),
            }),
            needs_sync,
        )
    }

    #[test]
    fn switching_waits_for_edits_sync_local_writes_and_final_host_save() {
        let mut change = switching(true);
        let now = change.since;
        assert!(!change.needs_sync_request(true, true));
        assert_eq!(
            change.poll_drain(true, false, false, now, || panic!(
                "too early to take errors"
            )),
            DrainProgress::Waiting
        );
        assert!(change.needs_sync_request(true, false));
        change.sync = Some(7);
        assert!(!change.needs_sync_request(true, false));
        assert_eq!(
            change.poll_drain(true, false, false, now, || panic!("sync still pending")),
            DrainProgress::Waiting
        );
        assert_eq!(
            change.poll_drain(true, true, true, now, || panic!("writes still pending")),
            DrainProgress::Waiting
        );
        assert_eq!(
            change.poll_drain(true, true, false, now, || None),
            DrainProgress::Ready
        );
        assert_eq!(change.phase, ChangePhase::SavingHost);
        assert_eq!(
            change.poll_drain(true, true, false, now, || panic!("host save still pending")),
            DrainProgress::Waiting
        );
        let Ok(Destination::Launch(LaunchAction::Join { endpoint, .. })) = change.finish(Ok(()))
        else {
            panic!("the queued server should launch after a successful final save")
        };
        assert_eq!(endpoint, "ws://next-world:3000/ws");
    }

    #[test]
    fn a_lost_connection_cannot_become_a_clean_switch_by_retrying_while_disconnected() {
        let mut change = switching(true);
        assert!(matches!(
            change.poll_drain(false, false, false, change.since, || None),
            DrainProgress::Blocked(_)
        ));
        assert_eq!(change.phase, ChangePhase::Blocked { unconfirmed: true });
        change.retry(false);
        assert!(change.needs_sync);
        assert!(matches!(
            change.poll_drain(false, false, false, change.since, || None),
            DrainProgress::Blocked(_)
        ));
        change.retry(true);
        change.sync = Some(8);
        assert_eq!(
            change.poll_drain(true, true, false, change.since, || None),
            DrainProgress::Ready
        );
    }

    #[test]
    fn a_sync_timeout_keeps_the_destination_and_requires_a_fresh_barrier_on_retry() {
        let mut change = switching(true);
        change.sync = Some(7);
        assert!(matches!(
            change.poll_drain(
                true,
                false,
                false,
                change.since + Duration::from_secs(15),
                || None
            ),
            DrainProgress::Blocked(_)
        ));
        change.retry(true);
        assert_eq!(change.sync, None);
        assert_eq!(
            change.poll_drain(true, true, false, change.since, || None),
            DrainProgress::Waiting
        );
        assert!(matches!(change.destination, Destination::Launch(_)));
    }

    #[test]
    fn failed_local_writes_block_session_teardown_until_a_successful_retry() {
        let mut change = switching(false);
        assert!(matches!(
            change.poll_drain(false, false, false, change.since, || Some(
                "disk unavailable".into()
            )),
            DrainProgress::Blocked(_)
        ));
        assert_eq!(change.phase, ChangePhase::Blocked { unconfirmed: false });
        assert_eq!(
            change.poll_drain(false, false, false, change.since, || None),
            DrainProgress::Waiting
        );
        change.retry(false);
        assert_eq!(
            change.poll_drain(false, false, true, change.since, || None),
            DrainProgress::Waiting
        );
        assert_eq!(
            change.poll_drain(false, false, false, change.since, || None),
            DrainProgress::Ready
        );
    }

    #[test]
    fn failed_final_save_never_returns_the_queued_destination() {
        let mut change = switching(false);
        assert_eq!(
            change.poll_drain(false, false, false, change.since, || None),
            DrainProgress::Ready
        );
        assert!(
            matches!(change.finish(Err("final save failed".into())), Err(error) if error == "final save failed")
        );
    }

    #[test]
    fn closing_during_host_save_overrides_the_queued_world() {
        let mut change = switching(false);
        assert_eq!(
            change.poll_drain(false, false, false, change.since, || None),
            DrainProgress::Ready
        );
        change.destination = Destination::Close;
        assert!(matches!(change.finish(Ok(())), Ok(Destination::Close)));
    }
}
