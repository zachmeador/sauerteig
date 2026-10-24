mod audio;
mod blueprints;
mod editor;
mod game;
#[cfg(not(target_arch = "wasm32"))]
mod hosting;
mod launcher;
mod lighting;
mod materials;
mod props;
mod regions;
mod render;
mod scene;
mod user_data;
#[cfg(any(target_arch = "wasm32", test))]
mod web_text;
#[cfg(not(target_arch = "wasm32"))]
mod world_saves;

use game::Game;
use launcher::{LaunchAction, LaunchOptions, Launcher};
use render::Renderer;
use std::sync::Arc;
use web_time::Instant;
use winit::{
    application::ApplicationHandler,
    dpi::LogicalSize,
    event::{DeviceEvent, DeviceId, WindowEvent},
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy},
    window::{Window, WindowId},
};

struct Runtime {
    window: Arc<Window>,
    renderer: Renderer,
    game: Game,
    launcher: Launcher,
    session: Option<Session>,
    stopping: bool,
    leaving_since: Option<Instant>,
    leave_needs_sync: bool,
    leave_sync: Option<u64>,
    leave_unconfirmed: bool,
    close_after_stop: bool,
    exit_requested: bool,
    #[cfg(not(target_arch = "wasm32"))]
    host: Option<hosting::HostedServer>,
    context: egui::Context,
    egui: egui_winit::State,
    last_frame: Instant,
    #[cfg(target_arch = "wasm32")]
    web_text: web_text::WebText,
}
struct Session {
    title: String,
    endpoint: String,
    name: String,
    hosting: bool,
    lan_port: Option<u16>,
}
enum SessionAction {
    Leave,
    LeaveUnconfirmed,
    Rejoin,
}
impl Drop for Runtime {
    fn drop(&mut self) {
        self.game.release(&self.window);
        #[cfg(not(target_arch = "wasm32"))]
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
            stopping: false,
            leaving_since: None,
            leave_needs_sync: false,
            leave_sync: None,
            leave_unconfirmed: false,
            close_after_stop: false,
            exit_requested: false,
            #[cfg(not(target_arch = "wasm32"))]
            host: None,
            context,
            egui,
            last_frame: Instant::now(),
            #[cfg(target_arch = "wasm32")]
            web_text: web_text::WebText::new()?,
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
        if self.session.is_some() || self.stopping {
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
            #[cfg(not(target_arch = "wasm32"))]
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
        self.leave_unconfirmed = false;
    }

    fn leave(&mut self, close: bool) {
        self.close_after_stop |= close;
        if self.stopping {
            return;
        }
        self.game.release(&self.window);
        if self.leaving_since.is_none() {
            self.leaving_since = Some(Instant::now());
            self.leave_needs_sync = self.session.is_some() && self.game.connected;
            self.leave_sync = None;
        }
    }

    fn finish_leaving(&mut self) {
        self.leaving_since = None;
        self.leave_needs_sync = false;
        self.leave_sync = None;
        self.leave_unconfirmed = false;
        // Close the socket before graceful server shutdown, which waits for
        // active connections and the final world save.
        self.replace_game(String::new(), String::new());
        self.session = None;
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(host) = &mut self.host {
            host.stop();
            self.stopping = true;
            return;
        }
        self.exit_requested = self.close_after_stop;
        self.launcher.status = "Disconnected. You can join again below.".into();
    }

    fn poll_host(&mut self) {
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(result) = self.host.as_mut().and_then(hosting::HostedServer::poll) {
            self.host = None;
            self.replace_game(String::new(), String::new());
            self.session = None;
            self.launcher.status = match result {
                Ok(()) => {
                    self.exit_requested = self.close_after_stop;
                    "World saved. You can continue whenever you’re ready.".into()
                }
                Err(error) => {
                    // A failed final save must remain visible, including when
                    // the user initiated a window close.
                    self.close_after_stop = false;
                    format!("Local server stopped with an error: {error}")
                }
            };
            self.stopping = false;
            self.leaving_since = None;
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
                if session.hosting {
                    let browser_url = session.endpoint.replacen("ws://", "http://", 1);
                    let browser_url = format!("{}/", browser_url.trim_end_matches("/ws"));
                    ui.hyperlink_to("Open browser client", browser_url);
                }
                if !self.launcher.status.is_empty() {
                    ui.label(&self.launcher.status);
                }
                ui.horizontal(|ui| {
                    if !self.game.connected && ui.button("Rejoin now").clicked() {
                        action = Some(SessionAction::Rejoin);
                    }
                    if ui
                        .button(if session.hosting {
                            "Save and leave"
                        } else {
                            "Disconnect"
                        })
                        .clicked()
                    {
                        action = Some(SessionAction::Leave);
                    }
                });
                if self.leave_unconfirmed {
                    ui.weak("Unconfirmed edits may be lost.");
                    if ui.button("Leave without confirmation").clicked() {
                        action = Some(SessionAction::LeaveUnconfirmed);
                    }
                }
            });
        action
    }

    fn render(&mut self) {
        self.poll_host();
        let now = Instant::now();
        let dt = (now - self.last_frame).as_secs_f32();
        self.last_frame = now;
        if self.session.is_some() && !self.stopping {
            self.game.update(dt, &mut self.renderer);
            if !self.game.connected && self.game.captured {
                self.game.release(&self.window);
            }
        }
        if let Some(since) = self.leaving_since {
            let connection_lost = self.leave_needs_sync && !self.game.connected;
            if self.leave_needs_sync
                && self.leave_sync.is_none()
                && self.game.connected
                && !self.game.edits_pending()
            {
                // The ordered reply also covers Undo/Redo and other messages
                // sent before this barrier, which have no edit request token.
                self.leave_sync = self.game.request_sync();
            }
            let network_ready = !self.leave_needs_sync
                || self
                    .leave_sync
                    .is_some_and(|request| self.game.synced(request));
            if connection_lost || (!network_ready && since.elapsed().as_secs() >= 15) {
                self.leaving_since = None;
                self.close_after_stop = false;
                self.leave_unconfirmed = true;
                self.launcher.status = if connection_lost {
                    "Connection lost before your edits were confirmed. Rejoin to check the world, or leave without confirmation."
                } else {
                    "The server has not confirmed your edits. Try leaving again to wait, or leave without confirmation."
                }.into();
            } else if network_ready && !user_data::pending_writes() {
                if let Some(error) = user_data::take_write_error() {
                    self.leaving_since = None;
                    self.close_after_stop = false;
                    self.launcher.status = format!("Local changes could not be saved: {error}");
                } else {
                    self.finish_leaving();
                }
            }
        }
        let raw = self.egui.take_egui_input(&self.window);
        #[cfg(target_arch = "wasm32")]
        let raw = {
            let mut raw = raw;
            if self.web_text.drain(&mut raw) {
                self.game.close_text_panel();
            }
            raw
        };
        let mut launch_action = None;
        let mut session_action = None;
        let context = self.context.clone();
        let output = context.run(raw, |ctx| {
            if self.leaving_since.is_some() {
                egui::CentralPanel::default().show(ctx, |ui| {
                    ui.vertical_centered(|ui| {
                        ui.add_space(40.0);
                        ui.spinner();
                        ui.label("Finishing edits and local saves…");
                        if ui.button("Cancel").clicked() {
                            self.leaving_since = None;
                            self.close_after_stop = false;
                        }
                    });
                });
            } else if self.session.is_some() {
                self.game.ui(ctx, &self.renderer, &self.window);
                session_action = self.session_ui(ctx);
            } else {
                launch_action = self.launcher.ui(ctx, self.stopping);
            }
        });
        if let Some(action) = launch_action {
            self.launch(action);
        }
        match session_action {
            Some(SessionAction::Leave) => self.leave(false),
            Some(SessionAction::LeaveUnconfirmed) => {
                self.leave(false);
                self.leave_needs_sync = false;
                self.leave_sync = None;
            }
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
        #[cfg(target_arch = "wasm32")]
        {
            use winit::platform::web::WindowExtWebSys;
            if let Some(canvas) = self.window.canvas() {
                self.web_text.sync(
                    &output.platform_output,
                    !self.game.captured,
                    &canvas,
                    output.pixels_per_point,
                    self.window.scale_factor(),
                );
            }
        }
        let mesh = self.game.geometry();
        match self.renderer.draw(
            self.game.eye(),
            self.game.direction(),
            self.game.time,
            &self.game.objects,
            &mesh.vertices,
            &mesh.indices,
            mesh.shadow_index_count,
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
enum UserEvent {
    Ready(Result<Box<Runtime>, String>),
    #[cfg(target_arch = "wasm32")]
    PointerLock {
        locked: bool,
        failed: bool,
    },
}
struct App {
    runtime: Option<Box<Runtime>>,
    window: Option<Arc<Window>>,
    proxy: EventLoopProxy<UserEvent>,
    options: Option<LaunchOptions>,
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
                report_error(&e.to_string());
                event_loop.exit();
                return;
            }
        };
        #[cfg(target_arch = "wasm32")]
        {
            use wasm_bindgen::{closure::Closure, JsCast};
            use winit::platform::web::WindowExtWebSys;
            let document = web_sys::window().unwrap().document().unwrap();
            // Lock acquisition is asynchronous in browsers. Only a real change or
            // error event should resolve capture; an early redraw is not failure.
            for event_name in ["pointerlockchange", "pointerlockerror"] {
                let proxy = self.proxy.clone();
                let document = document.clone();
                let observed = document.clone();
                let listener = Closure::<dyn Fn()>::new(move || {
                    let locked = observed
                        .pointer_lock_element()
                        .is_some_and(|element| element.id() == "game-canvas");
                    let _ = proxy.send_event(UserEvent::PointerLock {
                        locked,
                        failed: event_name == "pointerlockerror",
                    });
                });
                document
                    .add_event_listener_with_callback(event_name, listener.as_ref().unchecked_ref())
                    .expect("pointer lock listener");
                listener.forget(); // One window owns these listeners for the app lifetime.
            }
            let canvas = window.canvas().expect("winit canvas");
            canvas.set_id("game-canvas");
            let _ =
                canvas.set_attribute("style", "display:block;width:100%;height:100%;outline:none");
            let _ = canvas.set_attribute("tabindex", "0");
            let _=canvas.set_attribute("aria-label","Sauerteig multiplayer arena. Click to enter, WASD to move, E to edit, Escape to release the mouse.");
            document
                .get_element_by_id("game-container")
                .unwrap_or_else(|| document.body().unwrap().into())
                .append_child(&canvas)
                .expect("append game canvas");
            let browser = web_sys::window().unwrap();
            let _ = window.request_inner_size(LogicalSize::new(
                browser.inner_width().unwrap().as_f64().unwrap_or(1280.0),
                browser.inner_height().unwrap().as_f64().unwrap_or(800.0),
            ));
        }
        self.window = Some(window.clone());
        let options = self.options.take().unwrap_or_default();
        #[cfg(not(target_arch = "wasm32"))]
        {
            let ready = pollster::block_on(Runtime::new(window, options)).map(Box::new);
            let _ = self.proxy.send_event(UserEvent::Ready(ready));
        }
        #[cfg(target_arch = "wasm32")]
        {
            let proxy = self.proxy.clone();
            wasm_bindgen_futures::spawn_local(async move {
                let ready = Runtime::new(window, options).await.map(Box::new);
                let _ = proxy.send_event(UserEvent::Ready(ready));
            });
        }
    }
    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: UserEvent) {
        match event {
            UserEvent::Ready(Ok(runtime)) => {
                self.runtime = Some(runtime);
                #[cfg(target_arch = "wasm32")]
                if let Ok(event) = web_sys::CustomEvent::new("sauerteig-ready") {
                    let _ = web_sys::window().unwrap().dispatch_event(&event);
                }
            }
            UserEvent::Ready(Err(error)) => {
                report_error(&error);
                event_loop.exit();
            }
            #[cfg(target_arch = "wasm32")]
            UserEvent::PointerLock { locked, failed } => {
                if let Some(runtime) = &mut self.runtime {
                    if locked && (runtime.session.is_none() || runtime.leaving_since.is_some()) {
                        runtime.game.release(&runtime.window);
                    } else if locked {
                        runtime.game.captured = true;
                        runtime.game.pointer_locked = true;
                    } else if failed {
                        runtime.game.capture_denied();
                    } else if runtime.game.pointer_locked {
                        runtime.game.release(&runtime.window);
                    }
                }
            }
        }
    }
    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        if matches!(event, WindowEvent::CloseRequested) {
            if let Some(runtime) = &mut self.runtime {
                runtime.leave(true);
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
        #[cfg(target_arch = "wasm32")]
        if matches!(event, WindowEvent::Focused(false)) && runtime.web_text.owns_focus() {
            // Moving keyboard focus from canvas to its text-input helper does
            // not mean the user has left the application.
            return;
        }
        let response = runtime.egui.on_window_event(&runtime.window, &event);
        if runtime.session.is_some() && !runtime.stopping && runtime.leaving_since.is_none() {
            runtime
                .game
                .event(&event, &runtime.window, response.consumed);
        }
        match event {
            WindowEvent::Resized(size) => runtime.renderer.resize(size.width, size.height),
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
                if runtime.session.is_some() && !runtime.stopping && runtime.leaving_since.is_none()
                {
                    runtime.game.motion(delta);
                }
            }
        }
    }
    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        if let Some(window) = &self.window {
            window.request_redraw();
        }
    }
}
fn report_error(error: &str) {
    log::error!("{error}");
    #[cfg(target_arch = "wasm32")]
    if let Some(document) = web_sys::window().and_then(|w| w.document()) {
        if let Some(panel) = document.get_element_by_id("loading-panel") {
            let _ = panel.set_attribute("hidden", "");
        }
        if let Some(panel) = document.get_element_by_id("error-panel") {
            let _ = panel.remove_attribute("hidden");
        }
        if let Some(message) = document.get_element_by_id("error-message") {
            message.set_text_content(Some(error));
        }
    }
}
fn options() -> LaunchOptions {
    #[cfg(not(target_arch = "wasm32"))]
    {
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
    #[cfg(target_arch = "wasm32")]
    {
        let location = web_sys::window().unwrap().location();
        let params =
            web_sys::UrlSearchParams::new_with_str(&location.search().unwrap_or_default()).unwrap();
        let name = params.get("name");
        let server = params
            .get("server")
            .or_else(|| name.as_ref().map(|_| launcher::default_server()));
        LaunchOptions { server, name }
    }
}
pub fn run() {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn"))
            .try_init();
    }
    let options = options();
    let event_loop = EventLoop::<UserEvent>::with_user_event()
        .build()
        .expect("create event loop");
    event_loop.set_control_flow(ControlFlow::Poll);
    let app = App {
        runtime: None,
        window: None,
        proxy: event_loop.create_proxy(),
        options: Some(options),
    };
    #[cfg(not(target_arch = "wasm32"))]
    {
        let mut app = app;
        event_loop.run_app(&mut app).expect("run game");
    }
    #[cfg(target_arch = "wasm32")]
    {
        use winit::platform::web::EventLoopExtWebSys;
        event_loop.spawn_app(app);
    }
}
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen(start)]
pub fn start() {
    console_error_panic_hook::set_once();
    let _ = console_log::init_with_level(log::Level::Warn);
    run();
}
