//! Home screen and remembered choices, independent of game and world formats.
use crate::user_data::SessionPreferences;

#[derive(Default)]
pub struct LaunchOptions {
    pub server: Option<String>,
    pub name: Option<String>,
}

pub enum LaunchAction {
    Join {
        endpoint: String,
        name: String,
    },
    Host {
        path: std::path::PathBuf,
        title: String,
        name: String,
        lan: bool,
    },
}

pub enum MenuAction {
    Resume,
    Launch(LaunchAction),
}

pub struct ActiveSession<'a> {
    pub title: &'a str,
    pub hosting_lan: bool,
}

pub struct Launcher {
    preferences: SessionPreferences,
    pub status: String,
    auto_join: bool,
    saves: Option<crate::world_saves::WorldSaves>,
    worlds: Vec<crate::world_saves::WorldInfo>,
    selected: Option<String>,
    new_name: String,
    import_path: String,
    lan: bool,
    show_join: bool,
}

impl Launcher {
    pub fn new(options: LaunchOptions) -> Self {
        let (mut preferences, status) = match SessionPreferences::load() {
            Ok(preferences) => (preferences, String::new()),
            Err(error) => (SessionPreferences::default(), error),
        };
        let auto_join = options.server.is_some();
        if let Some(server) = options.server {
            preferences.server = server;
        } else if preferences.server.is_empty() {
            preferences.server = default_server();
        }
        if let Some(name) = options.name {
            preferences.player_name = name;
        }
        let (saves, status) = match crate::user_data::data_directory()
            .and_then(crate::world_saves::WorldSaves::new)
        {
            Ok(saves) => (Some(saves), status),
            Err(error) => (None, format!("Saved worlds unavailable: {error}")),
        };
        let mut launcher = Self {
            selected: preferences.last_world.clone(),
            preferences,
            status,
            auto_join,
            saves,
            worlds: Vec::new(),
            new_name: "My world".into(),
            import_path: legacy_world_path(),
            lan: false,
            show_join: false,
        };
        launcher.refresh();
        launcher
    }

    pub fn initial_action(&mut self) -> Option<LaunchAction> {
        if std::mem::take(&mut self.auto_join) {
            self.join()
        } else {
            None
        }
    }

    pub fn open(&mut self) {
        self.refresh();
    }

    fn remember(&mut self) {
        self.status = match self.preferences.save() {
            Ok(()) => String::new(),
            Err(error) => format!("Your choices could not be saved: {error}"),
        };
    }

    fn player_name(&mut self) -> Option<String> {
        let name = self.preferences.player_name.trim();
        if name.is_empty() || name.chars().count() > 24 || name.chars().any(char::is_control) {
            self.status = "Choose a player name with 1–24 characters.".into();
            return None;
        }
        self.preferences.player_name = name.into();
        Some(self.preferences.player_name.clone())
    }

    fn join(&mut self) -> Option<LaunchAction> {
        let name = self.player_name()?;
        match normalize_server(&self.preferences.server) {
            Ok(endpoint) => {
                self.preferences.server = endpoint.clone();
                self.remember();
                Some(LaunchAction::Join { endpoint, name })
            }
            Err(error) => {
                self.status = error;
                None
            }
        }
    }

    fn refresh(&mut self) {
        if let Some(saves) = &self.saves {
            match saves.list() {
                Ok(worlds) => self.worlds = worlds,
                Err(error) => self.status = error,
            }
        }
    }

    fn host(&mut self, id: &str) -> Option<LaunchAction> {
        let name = self.player_name()?;
        let title = self
            .worlds
            .iter()
            .find(|world| world.id == id)?
            .name
            .clone();
        match self.saves.as_ref()?.world_path(id) {
            Ok(path) => {
                self.preferences.last_world = Some(id.into());
                self.selected = Some(id.into());
                self.remember();
                Some(LaunchAction::Host {
                    path,
                    title,
                    name,
                    lan: self.lan,
                })
            }
            Err(error) => {
                self.status = error;
                None
            }
        }
    }

    pub fn ui(
        &mut self,
        ctx: &egui::Context,
        current: Option<ActiveSession<'_>>,
    ) -> Option<MenuAction> {
        let mut action = None;
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(egui::Color32::from_rgba_unmultiplied(11, 16, 18, 235)))
            .show(ctx, |ui| {
                let width = 500.0_f32.min((ui.available_width() - 24.0).max(100.0));
                ui.vertical_centered(|ui| {
                    ui.add_space((ui.available_height() * 0.06).min(40.0));
                    ui.heading(egui::RichText::new("SAUERTEIG").size(30.0));
                    ui.label("Main menu");
                    ui.add_space(22.0);
                });
                let body_height = ui.available_height().max(80.0);
                ui.horizontal(|ui| {
                    ui.add_space(((ui.available_width() - width) * 0.5).max(0.0));
                    ui.vertical(|ui| {
                        ui.set_width(width);
                        ui.set_min_height(body_height);
                        egui::ScrollArea::vertical()
                            .max_height(body_height - 16.0)
                            .show(ui, |ui| {
                                if let Some(current) = &current {
                                    if ui
                                        .add_sized(
                                            [ui.available_width(), 40.0],
                                            egui::Button::new(format!(
                                                "Resume · {}",
                                                current.title
                                            )),
                                        )
                                        .clicked()
                                    {
                                        action = Some(MenuAction::Resume);
                                    }
                                    ui.weak("Your current world stays connected while you browse.");
                                    if current.hosting_lan {
                                        ui.weak("Switching worlds ends the hosted session.");
                                    }
                                    ui.add_space(16.0);
                                }
                                ui.label("Player name");
                                let response = ui.add(
                                    egui::TextEdit::singleline(&mut self.preferences.player_name)
                                        .desired_width(f32::INFINITY),
                                );
                                if response.lost_focus() {
                                    self.remember();
                                }
                                ui.add_space(14.0);
                                ui.horizontal(|ui| {
                                    ui.selectable_value(&mut self.show_join, false, "Your worlds");
                                    ui.selectable_value(&mut self.show_join, true, "Join server");
                                });
                                ui.separator();
                                let launch = if !self.show_join {
                                    self.worlds_ui(ui, current.is_some())
                                } else {
                                    self.join_ui(ui)
                                };
                                if let Some(launch) = launch {
                                    action = Some(MenuAction::Launch(launch));
                                }
                                if !self.status.is_empty() {
                                    ui.add_space(12.0);
                                    ui.label(
                                        egui::RichText::new(&self.status)
                                            .color(egui::Color32::from_rgb(237, 187, 127)),
                                    );
                                }
                            });
                    });
                });
            });
        action
    }

    fn join_ui(&mut self, ui: &mut egui::Ui) -> Option<LaunchAction> {
        ui.label("Server address");
        ui.add(
            egui::TextEdit::singleline(&mut self.preferences.server)
                .hint_text("ws://127.0.0.1:3000/ws")
                .desired_width(f32::INFINITY),
        );
        ui.weak("Use the address shared by the host.");
        ui.add_space(8.0);
        if ui
            .add_sized(
                [ui.available_width(), 36.0],
                egui::Button::new("Join server"),
            )
            .clicked()
        {
            return self.join();
        }
        None
    }

    fn worlds_ui(&mut self, ui: &mut egui::Ui, session_active: bool) -> Option<LaunchAction> {
        let mut action = None;
        let last = self
            .preferences
            .last_world
            .as_ref()
            .and_then(|id| self.worlds.iter().find(|world| &world.id == id))
            .cloned();
        if let Some(last) = last.filter(|_| !session_active) {
            if ui
                .add_sized(
                    [ui.available_width(), 36.0],
                    egui::Button::new(format!("Continue · {}", last.name)),
                )
                .clicked()
            {
                action = self.host(&last.id);
            }
        }
        ui.add_space(8.0);
        ui.checkbox(&mut self.lan, "Allow players on my local network");
        ui.weak(if self.lan {
            "Hosting will listen on your network interfaces."
        } else {
            "Only this computer can connect."
        });
        ui.add_space(12.0);
        ui.label("New world name");
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.new_name)
                    .desired_width((ui.available_width() - 110.0).max(80.0)),
            );
            if ui
                .add_enabled(self.saves.is_some(), egui::Button::new("New world"))
                .clicked()
                && self.player_name().is_some()
            {
                match self.saves.as_ref().unwrap().create(&self.new_name) {
                    Ok(world) => {
                        self.refresh();
                        action = self.host(&world.id);
                    }
                    Err(error) => self.status = error,
                }
            }
        });
        ui.add_space(12.0);
        ui.label("Saved worlds");
        if self.worlds.is_empty() {
            ui.weak("Create a world to get started.");
        } else {
            egui::ScrollArea::vertical()
                .id_salt("saved_worlds")
                .max_height(140.0)
                .show(ui, |ui| {
                    for world in &self.worlds {
                        ui.selectable_value(
                            &mut self.selected,
                            Some(world.id.clone()),
                            &world.name,
                        );
                    }
                });
            if let Some(world) = self
                .selected
                .as_ref()
                .and_then(|id| self.worlds.iter().find(|world| &world.id == id))
                .cloned()
            {
                ui.horizontal(|ui| {
                    if ui.button("Open world").clicked() {
                        action = self.host(&world.id);
                    }
                    if ui.button("Duplicate").clicked() {
                        let name =
                            format!("{} copy", world.name.chars().take(75).collect::<String>());
                        match self.saves.as_ref().unwrap().duplicate(&world.id, &name) {
                            Ok(copy) => {
                                self.selected = Some(copy.id);
                                self.refresh();
                                self.status =
                                    format!("Created ‘{}’. The original is unchanged.", copy.name);
                            }
                            Err(error) => self.status = error,
                        }
                    }
                });
            }
        }
        ui.add_space(12.0);
        ui.collapsing("Import an existing world", |ui| {
            ui.label("World file path");
            ui.add(
                egui::TextEdit::singleline(&mut self.import_path)
                    .desired_width(f32::INFINITY)
                    .hint_text("/path/to/world.json"),
            );
            ui.weak("Makes a copy in your saved worlds. Uses the new world name above.");
            if ui
                .add_enabled(
                    self.saves.is_some() && !self.import_path.trim().is_empty(),
                    egui::Button::new("Import copy"),
                )
                .clicked()
            {
                match self.saves.as_ref().unwrap().import(
                    std::path::Path::new(self.import_path.trim()),
                    &self.new_name,
                ) {
                    Ok(world) => {
                        self.selected = Some(world.id);
                        self.refresh();
                        self.status =
                            format!("Imported ‘{}’. The source file is unchanged.", world.name);
                    }
                    Err(error) => self.status = error,
                }
            }
        });
        action
    }
}

pub fn normalize_server(input: &str) -> Result<String, String> {
    let input = input.trim();
    if input.is_empty() {
        return Err("Enter a server address.".into());
    }
    let input = if input.contains("://") {
        input.to_string()
    } else {
        format!("ws://{input}")
    };
    let mut url = url::Url::parse(&input)
        .map_err(|_| "Enter a valid server address, such as ws://127.0.0.1:3000/ws.".to_string())?;
    let scheme = match url.scheme() {
        "ws" | "http" => "ws",
        "wss" | "https" => "wss",
        _ => return Err("Server addresses must use ws:// or wss://.".into()),
    };
    url.set_scheme(scheme)
        .map_err(|_| "Invalid server address".to_string())?;
    if url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err("Use a server address without login details or a #fragment.".into());
    }
    if url.path() == "/" || url.path().is_empty() {
        url.set_path("/ws");
    }
    Ok(url.into())
}

pub fn default_server() -> String {
    "ws://127.0.0.1:3000/ws".into()
}

fn legacy_world_path() -> String {
    let mut roots = Vec::new();
    if let Ok(path) = std::env::current_dir() {
        roots.push(path);
    }
    // A development .app lives in <checkout>/.local/Sauerteig.app. Search
    // ancestors for the old development saves without relying on Finder's cwd.
    if let Ok(exe) = std::env::current_exe() {
        roots.extend(
            exe.ancestors()
                .skip(1)
                .take(5)
                .map(std::path::Path::to_path_buf),
        );
    }
    for root in roots {
        for name in ["foundry.json", "world.json"] {
            let path = root.join(".local").join(name);
            if path.is_file() {
                return path.to_string_lossy().into_owned();
            }
        }
    }
    String::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn server_addresses_support_shared_http_links_and_ipv6() {
        assert_eq!(
            normalize_server(" localhost:3456 ").unwrap(),
            "ws://localhost:3456/ws"
        );
        assert_eq!(
            normalize_server("https://example.test").unwrap(),
            "wss://example.test/ws"
        );
        assert_eq!(
            normalize_server("ws://[::1]:4321/ws").unwrap(),
            "ws://[::1]:4321/ws"
        );
        assert_eq!(
            normalize_server("wss://example.test/room?x=1").unwrap(),
            "wss://example.test/room?x=1"
        );
        for input in [
            "",
            "ftp://host",
            "ws://user:secret@host",
            "ws://host/#x",
            "ws://",
        ] {
            assert!(normalize_server(input).is_err(), "{input}");
        }
    }
}
