mod audio;
mod blueprints;
mod editor;
mod game;
mod lighting;
mod regions;
mod render;
mod scene;
mod user_data;
#[cfg(any(target_arch = "wasm32", test))]
mod web_text;

use game::Game;
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
    context: egui::Context,
    egui: egui_winit::State,
    last_frame: Instant,
    #[cfg(target_arch = "wasm32")]
    web_text: web_text::WebText,
}
impl Runtime {
    async fn new(window: Arc<Window>, endpoint: String, name: String) -> Result<Self, String> {
        let mut renderer = Renderer::new(window.clone()).await?;
        let game = Game::new(endpoint, name);
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
        Ok(Self {
            window,
            renderer,
            game,
            context,
            egui,
            last_frame: Instant::now(),
            #[cfg(target_arch = "wasm32")]
            web_text: web_text::WebText::new()?,
        })
    }
    fn render(&mut self) {
        let now = Instant::now();
        let dt = (now - self.last_frame).as_secs_f32();
        self.last_frame = now;
        self.game.update(dt, &mut self.renderer);
        let raw = self.egui.take_egui_input(&self.window);
        #[cfg(target_arch = "wasm32")]
        let raw = {
            let mut raw = raw;
            if self.web_text.drain(&mut raw) {
                self.game.close_text_panel();
            }
            raw
        };
        let output = self
            .context
            .run(raw, |ctx| self.game.ui(ctx, &self.renderer, &self.window));
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
    endpoint: String,
    name: String,
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
        let endpoint = self.endpoint.clone();
        let name = self.name.clone();
        #[cfg(not(target_arch = "wasm32"))]
        {
            let ready = pollster::block_on(Runtime::new(window, endpoint, name)).map(Box::new);
            let _ = self.proxy.send_event(UserEvent::Ready(ready));
        }
        #[cfg(target_arch = "wasm32")]
        {
            let proxy = self.proxy.clone();
            wasm_bindgen_futures::spawn_local(async move {
                let ready = Runtime::new(window, endpoint, name).await.map(Box::new);
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
                    if locked {
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
            event_loop.exit();
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
        runtime
            .game
            .event(&event, &runtime.window, response.consumed);
        match event {
            WindowEvent::Resized(size) => runtime.renderer.resize(size.width, size.height),
            WindowEvent::RedrawRequested => runtime.render(),
            _ => {}
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
                runtime.game.motion(delta);
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
fn options() -> (String, String) {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let mut endpoint = "ws://127.0.0.1:3000/ws".to_string();
        let mut name = format!("Player{}", std::process::id() % 1000);
        let mut args = std::env::args().skip(1);
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--server" => {
                    if let Some(value) = args.next() {
                        endpoint = value;
                    }
                }
                "--name" => {
                    if let Some(value) = args.next() {
                        name = value;
                    }
                }
                "--help" | "-h" => {
                    println!("Sauerteig\n  --server ws://127.0.0.1:3000/ws\n  --name Player\n\nWASD move · Space jump · E edit · Esc release mouse");
                    std::process::exit(0);
                }
                _ => {}
            }
        }
        (endpoint, name)
    }
    #[cfg(target_arch = "wasm32")]
    {
        let location = web_sys::window().unwrap().location();
        let params =
            web_sys::UrlSearchParams::new_with_str(&location.search().unwrap_or_default()).unwrap();
        let protocol = if location.protocol().unwrap_or_default() == "https:" {
            "wss"
        } else {
            "ws"
        };
        let endpoint = params
            .get("server")
            .unwrap_or_else(|| format!("{protocol}://{}/ws", location.host().unwrap_or_default()));
        let name = params
            .get("name")
            .unwrap_or_else(|| format!("Player{}", (js_sys::Math::random() * 1000.0) as u32));
        (endpoint, name)
    }
}
pub fn run() {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn"))
            .try_init();
    }
    let (endpoint, name) = options();
    let event_loop = EventLoop::<UserEvent>::with_user_event()
        .build()
        .expect("create event loop");
    event_loop.set_control_flow(ControlFlow::Poll);
    let app = App {
        runtime: None,
        window: None,
        proxy: event_loop.create_proxy(),
        endpoint,
        name,
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
