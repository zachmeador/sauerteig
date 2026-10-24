use super::*;

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;

    #[test]
    fn a_new_switch_retries_failed_documents_after_the_error_was_seen_and_cancelled() {
        use crate::user_data::{
            flush_pending,
            tests::{storage_at, QUEUE_TEST},
        };
        let _serial = QUEUE_TEST.lock().unwrap();
        let _ = flush_pending();
        let directory = std::env::temp_dir().join(format!(
            "sauerteig-switch-storage-retry-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&directory);
        let mut game = Game::new(String::new(), "storage switch test".into());
        game.storage = storage_at(&directory);
        flush_pending().unwrap();
        game.poll_storage();
        game.preferences.invert_scroll = true;
        // Turn a valid destination into a file so both document writes fail.
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::remove_dir_all(&directory).unwrap();
        std::fs::write(&directory, b"temporarily unavailable").unwrap();
        game.persist_settings();
        game.persist_library();
        let error = flush_pending().unwrap_err();
        assert!(error.contains("Could not save settings"));
        assert!(error.contains("Could not save blueprints"));
        game.poll_storage();
        assert!(game.settings_status.starts_with("Local storage failed"));
        assert!(game.library_status.starts_with("Local storage failed"));
        // The switch surfaced and consumed the error, then was cancelled.
        assert!(crate::user_data::take_write_error().is_none());
        std::fs::remove_file(&directory).unwrap();
        // This is also called by every fresh switch, so cancellation cannot
        // let the next world discard these still-unsaved in-memory values.
        game.retry_local_saves();
        flush_pending().unwrap();
        game.poll_storage();
        let saved: Preferences =
            serde_json::from_slice(&std::fs::read(directory.join("settings.json")).unwrap())
                .unwrap();
        assert!(saved.invert_scroll);
        assert!(directory.join("blueprints.json").is_file());
        assert_eq!(game.settings_status, "Saved locally");
        assert_eq!(game.library_status, "Saved locally");
        std::fs::remove_dir_all(directory).unwrap();
    }
}

impl Game {
    pub(super) fn poll_storage(&mut self) {
        for event in self.storage.poll() {
            match event {
                StorageEvent::Loaded(Document::Settings, value) => {
                    match value
                        .map(|json| serde_json::from_str::<Preferences>(&json))
                        .transpose()
                    {
                        Ok(preferences) => {
                            self.preferences = preferences.unwrap_or_default();
                            self.settings_ready = true;
                            self.settings_status = "Saved locally".into();
                        }
                        Err(error) => {
                            self.settings_status = format!("Cannot read settings: {error}")
                        }
                    }
                }
                StorageEvent::Loaded(Document::Blueprints, value) => {
                    match value
                        .map(|json| BlueprintLibrary::from_json(&json))
                        .transpose()
                    {
                        Ok(library) => {
                            self.library = library.unwrap_or_default();
                            self.library_ready = true;
                            self.library_status = "Saved locally".into();
                        }
                        Err(error) => self.library_status = format!("Cannot read library: {error}"),
                    }
                }
                StorageEvent::Saved(document) => {
                    let status = match document {
                        Document::Settings => &mut self.settings_status,
                        Document::Blueprints => &mut self.library_status,
                    };
                    *status = "Saved locally".into();
                }
                StorageEvent::Failed(document, message) => {
                    let status = match document {
                        Document::Settings => &mut self.settings_status,
                        Document::Blueprints => &mut self.library_status,
                    };
                    *status = format!("Local storage failed: {message}");
                }
            }
        }
    }

    pub(crate) fn retry_local_saves(&mut self) {
        self.poll_storage();
        if self.settings_ready && self.settings_status.starts_with("Local storage failed") {
            self.persist_settings();
        }
        if self.library_ready && self.library_status.starts_with("Local storage failed") {
            self.persist_library();
        }
    }

    fn persist_library(&mut self) {
        match self.library.to_json() {
            Ok(json) => {
                self.storage.save(Document::Blueprints, json);
                self.library_status = "Saving…".into();
            }
            Err(error) => self.library_status = error.to_string(),
        }
    }
    fn persist_settings(&mut self) {
        match serde_json::to_string(&self.preferences) {
            Ok(json) => {
                self.storage.save(Document::Settings, json);
                self.settings_status = "Saving…".into();
            }
            Err(error) => self.settings_status = error.to_string(),
        }
    }
    pub(super) fn selected_regions(&self) -> RegionSet {
        let mut regions = self.regions.clone();
        let selection = self.selection.clone().or_else(|| {
            if regions.is_empty() {
                self.active_selection()
            } else {
                None
            }
        });
        if let Some(selection) = selection {
            if let Ok(bounds) = RegionBox::new(selection.min, selection.max) {
                if self.selecting_subtract {
                    let _ = regions.subtract(bounds);
                } else {
                    let _ = regions.add(bounds);
                }
            }
        }
        regions
    }
    fn save_blueprint(&mut self) {
        if self.editing_busy() {
            self.library_status = "Wait for the current edit to finish.".into();
            return;
        }
        let result = Clipboard::copy_regions_with_objects(
            &self.world,
            &self.selected_regions(),
            &self.objects,
        );
        match result {
            Ok(clipboard) => match self.library.save(
                &self.library_ui.new_name,
                &self.library_ui.new_folder,
                clipboard,
            ) {
                Ok(id) => {
                    self.select_blueprint(id);
                    self.library_ui.new_name.clear();
                    self.persist_library();
                }
                Err(error) => self.library_status = error.to_string(),
            },
            Err(error) => self.library_status = error.to_string(),
        }
    }
    fn select_blueprint(&mut self, id: u64) {
        if let Some(entry) = self.library.get(id) {
            self.library_ui.selected = Some(id);
            self.library_ui.edit_name = entry.name.clone();
            self.library_ui.edit_folder = entry.folder.clone();
            self.library_ui.confirm_delete = false;
        }
    }
    pub(super) fn local_panels(&mut self, ctx: &egui::Context, window: &Window) {
        if self.show_settings {
            let mut open = true;
            egui::Window::new("Settings")
                .open(&mut open)
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .default_width(390.0)
                .show(ctx, |ui| {
                    ui.add_enabled_ui(self.settings_ready, |ui| {
                        if ui
                            .checkbox(
                                &mut self.preferences.invert_scroll,
                                "Invert editor scroll direction",
                            )
                            .changed()
                        {
                            self.wheel_remainder = 0.0;
                            self.persist_settings();
                        }
                        ui.add_space(6.0);
                        ui.label(if self.preferences.invert_scroll {
                            "Wheel up adds geometry; wheel down removes it."
                        } else {
                            "Wheel down adds geometry; wheel up removes it."
                        });
                        ui.label("Also reverses grid, shaping, and material wheel controls.");
                    });
                    ui.separator();
                    ui.label(
                        egui::RichText::new(&self.settings_status)
                            .small()
                            .color(MUTED),
                    );
                    if !self.settings_ready && ui.button("Retry loading").clicked() {
                        self.settings_status = "Loading…".into();
                        self.storage.load(Document::Settings);
                    }
                    if self.settings_ready
                        && self.settings_status.starts_with("Local storage failed")
                        && ui.button("Retry save").clicked()
                    {
                        self.persist_settings();
                    }
                    if ui.button("Back").clicked() {
                        self.show_settings = false;
                    }
                });
            self.show_settings &= open;
        }
        if self.show_blueprints {
            let mut open = true;
            egui::Window::new("Blueprints")
                .open(&mut open).collapsible(false).resizable(true)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, -10.0]).default_size([720.0, 470.0])
                .min_width(560.0)
                .show(ctx, |ui| {
                    ui.add_enabled_ui(self.library_ready, |ui| {
                        ui.horizontal(|ui| {
                            ui.label("Search");
                            ui.add(egui::TextEdit::singleline(&mut self.library_ui.search).hint_text("Name or folder").desired_width(260.0));
                            ui.label(format!("{} blueprints", self.library.entries().len()));
                        });
                        ui.separator();
                        ui.columns(2, |columns| {
                            let rows: Vec<_> = self.library.search(&self.library_ui.search, None).into_iter()
                                .map(|b| (b.id, b.name.clone(), b.folder.clone())).collect();
                            egui::ScrollArea::vertical().id_salt("blueprint-list").max_height(285.0).min_scrolled_height(220.0)
                                .show(&mut columns[0], |ui| {
                                    if rows.is_empty() { ui.label("No blueprints here yet."); }
                                    for (id, name, folder) in rows {
                                        let label = if folder.is_empty() { name } else { format!("{folder} / {name}") };
                                        if ui.selectable_label(self.library_ui.selected == Some(id), label).clicked() { self.select_blueprint(id); }
                                    }
                                });
                            let ui = &mut columns[1];
                            if let Some(id) = self.library_ui.selected {
                                if let Some(entry) = self.library.get(id) {
                                    let dims = entry.clipboard.dims().map(|v| v as f32 / 8.0);
                                    ui.label(format!("{} × {} × {} m", dims[0], dims[1], dims[2]));
                                    ui.label(format!("{} selection volumes · {} objects · {} material regions", entry.clipboard.mask().boxes().len(), entry.clipboard.objects().len(), entry.clipboard.volumes().len()));
                                    ui.add_space(8.0);
                                    ui.label("Name");
                                    ui.text_edit_singleline(&mut self.library_ui.edit_name);
                                    ui.label("Folder");
                                    ui.add(egui::TextEdit::singleline(&mut self.library_ui.edit_folder).hint_text("Structures / Foundry"));
                                    ui.horizontal(|ui| {
                                        if ui.button("Update").clicked() {
                                            match self.library.rename(id, &self.library_ui.edit_name) {
                                                Ok(()) => { let _ = self.library.move_to(id, &self.library_ui.edit_folder); self.persist_library(); }
                                                Err(e) => self.library_status = e.to_string(),
                                            }
                                        }
                                        if ui.button("Duplicate").clicked() {
                                            match self.library.duplicate(id, None) {
                                                Ok(id) => { self.select_blueprint(id); self.persist_library(); }
                                                Err(e) => self.library_status = e.to_string(),
                                            }
                                        }
                                        if ui.button("Delete").clicked() { self.library_ui.confirm_delete = true; }
                                    });
                                    if self.library_ui.confirm_delete {
                                        ui.horizontal(|ui| {
                                            ui.label("Delete this blueprint?");
                                            if ui.button("Delete it").clicked() {
                                                let _ = self.library.delete(id);
                                                self.library_ui.selected = None;
                                                self.library_ui.confirm_delete = false;
                                                self.persist_library();
                                            }
                                            if ui.button("Keep").clicked() { self.library_ui.confirm_delete = false; }
                                        });
                                    }
                                    ui.add_space(12.0);
                                    if ui.button("Load for placement").clicked() {
                                        self.clipboard = self.library.load(id);
                                        self.paste_preview = true;
                                        self.show_blueprints = false;
                                        self.object_mode = false;
                                        self.object_selection = None;
                                        self.selection_generation = self.selection_generation.wrapping_add(1);
                                        self.notify("Blueprint loaded. Select a destination; V places it. R rotates, M mirrors.");
                                        self.capture(window);
                                    }
                                }
                            } else { ui.label("Select a blueprint to place or organize it."); }
                        });
                        ui.separator();
                        ui.label("Save current selection");
                        ui.horizontal(|ui| {
                            ui.add(egui::TextEdit::singleline(&mut self.library_ui.new_name).hint_text("Blueprint name").desired_width(210.0));
                            ui.add(egui::TextEdit::singleline(&mut self.library_ui.new_folder).hint_text("Folder (optional)").desired_width(210.0));
                            if ui.add_enabled(self.local.editing && !self.selected_regions().is_empty(), egui::Button::new("Save selection")).clicked() { self.save_blueprint(); }
                        });
                    });
                    ui.add_space(8.0);
                    ui.label(egui::RichText::new(&self.library_status).color(COPPER).small());
                    ui.label(egui::RichText::new("Geometry, materials and objects · stored on this device").color(MUTED).small());
                    ui.horizontal(|ui| {
                        if !self.library_ready && ui.button("Retry loading").clicked() {
                            self.library_status = "Loading…".into();
                            self.storage.load(Document::Blueprints);
                        }
                        if self.library_ready && self.library_status.starts_with("Local storage failed") && ui.button("Retry save").clicked() { self.persist_library(); }
                        if ui.button("Back").clicked() { self.show_blueprints = false; }
                    });
                });
            self.show_blueprints &= open;
        }
        if !self.captured
            && self.local.editing
            && !self.show_settings
            && !self.show_blueprints
            && !self.show_assets
        {
            if self.object_mode {
                self.object_panel(ctx);
            } else {
                self.selection_panel(ctx);
            }
        }
    }
    fn selection_panel(&mut self, ctx: &egui::Context) {
        egui::Window::new("Selection")
            .anchor(egui::Align2::LEFT_TOP, [24.0, 100.0]).collapsible(true).resizable(false).default_width(270.0)
            .show(ctx, |ui| {
                if ui.button("Object mode (Q)").clicked() { self.set_object_mode(true); return; }
                ui.horizontal(|ui| {
                    for (mode, name) in [(SelectionMode::Replace, "Replace"), (SelectionMode::Add, "Add"), (SelectionMode::Subtract, "Subtract")] {
                        ui.selectable_value(&mut self.selection_mode, mode, name);
                    }
                });
                ui.label(egui::RichText::new("Choose a mode, then drag in the world.").small());
                ui.add_space(8.0);
                let grid = self.grid as f32 / 8.0;
                ui.horizontal(|ui| {
                    ui.label(format!("Grid {grid} m"));
                    if ui.small_button("−").clicked() { self.change_grid(false); }
                    if ui.small_button("+").clicked() { self.change_grid(true); }
                });
                if let Some(mut selection) = self.selection.clone() {
                    ui.label("Active volume size (grid cells)");
                    let mut changed = false;
                    ui.horizontal(|ui| {
                        for axis in 0..3 {
                            let mut count = (selection.max[axis] - selection.min[axis]) / self.grid;
                            ui.label(["X", "Y", "Z"][axis]);
                            if ui.add(egui::DragValue::new(&mut count).range(1..=1024).speed(1)).changed() {
                                selection.max[axis] = selection.min[axis] + count * self.grid;
                                changed = true;
                            }
                        }
                    });
                    if changed {
                        if (0..3).all(|a| selection.min[a] >= self.world.bounds().min[a] * 8 && selection.max[a] <= self.world.bounds().max[a] * 8) {
                            self.selection = Some(selection);
                            self.selection_generation = self.selection_generation.wrapping_add(1);
                            self.edit_queue.clear();
                        } else { self.notify("Selection reaches the world boundary."); }
                    }
                }
                let regions = self.selected_regions();
                ui.label(format!("{} volumes · {:.3} m³", regions.boxes().len(), regions.volume() as f64 / 512.0));
                ui.separator();
                ui.label("Volume material");
                ui.horizontal(|ui| {
                    egui::ComboBox::from_id_salt("volume-material")
                        .selected_text(match self.volume_material {
                            None => "Normal space",
                            Some(VolumeMaterial::Water) => "Water",
                            Some(VolumeMaterial::Glass) => "Glass",
                        })
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut self.volume_material, None, "Normal space");
                            ui.selectable_value(&mut self.volume_material, Some(VolumeMaterial::Water), "Water");
                            ui.selectable_value(&mut self.volume_material, Some(VolumeMaterial::Glass), "Glass");
                        });
                    if ui.add_enabled(!regions.boxes().is_empty() && !self.editing_busy(), egui::Button::new("Apply")).clicked() {
                        self.edit_queue.push_back(EditIntent::Volume(self.volume_material));
                    }
                });
                ui.weak("Fills selected space. Solid geometry stays in place.");
                ui.checkbox(&mut self.select_volumes, "Select water and glass surfaces");
                ui.separator();
                ui.horizontal(|ui| {
                    if ui.button("Copy").clicked() { self.copy_selection(); }
                    if ui.button("Blueprints").clicked() { self.show_blueprints = true; }
                    if ui.button("Clear").clicked() { self.clear_selection(); }
                });
                ui.label(egui::RichText::new("Alt drag adds · Alt Shift drag subtracts\nRMB / Shift LMB extends in depth").small().color(MUTED));
            });
    }
}

impl Game {
    pub(super) fn asset_panel(
        &mut self,
        ctx: &egui::Context,
        renderer: &Renderer,
        window: &Window,
    ) {
        if !self.show_assets {
            return;
        }
        if self.material_thumbnails.is_empty() {
            self.material_thumbnails = renderer
                .materials
                .thumbnails
                .iter()
                .enumerate()
                .map(|(i, image)| {
                    ctx.load_texture(
                        format!("material {i}"),
                        image.clone(),
                        egui::TextureOptions::LINEAR,
                    )
                })
                .collect();
        }
        let mut open = true;
        let mut resume = false;
        egui::Window::new("Materials and props")
            .open(&mut open)
            .collapsible(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .default_width(470.0)
            .show(ctx, |ui| {
                egui::ScrollArea::vertical()
                    .max_height((ctx.screen_rect().height() - 160.0).max(200.0))
                    .show(ui, |ui| {
                        egui::Grid::new("material library")
                            .num_columns(4)
                            .spacing([12.0, 10.0])
                            .show(ui, |ui| {
                                for (i, name) in MATERIAL_NAMES.iter().enumerate() {
                                    ui.vertical(|ui| {
                                        let image = egui::Image::new((
                                            self.material_thumbnails[i].id(),
                                            egui::vec2(72.0, 72.0),
                                        ));
                                        if ui
                                            .add(
                                                egui::ImageButton::new(image)
                                                    .selected(self.material as usize == i),
                                            )
                                            .clicked()
                                        {
                                            self.material = i as u8;
                                        }
                                        ui.label(egui::RichText::new(*name).small());
                                    });
                                    if i % 4 == 3 {
                                        ui.end_row();
                                    }
                                }
                            });
                        ui.horizontal(|ui| {
                            ui.label(format!(
                                "Selected: {}",
                                MATERIAL_NAMES[self.material as usize]
                            ));
                            if ui
                                .add_enabled(
                                    !self.selected_regions().is_empty()
                                        && self.edit_queue.len() < 32,
                                    egui::Button::new("Paint faces"),
                                )
                                .clicked()
                            {
                                self.edit_queue.push_back(EditIntent::Paint(self.material));
                            }
                        });
                        ui.separator();
                        ui.label("Props");
                        for prop in &crate::props::PROPS {
                            ui.selectable_value(&mut self.prop_kind, prop.kind, prop.name);
                        }
                        ui.selectable_value(
                            &mut self.prop_kind,
                            ObjectKind::Tree,
                            "Tree (prototype)",
                        );
                        ui.label(
                            egui::RichText::new(
                                "P places the selected prop. Wall lamps align to the aimed wall.",
                            )
                            .small()
                            .color(MUTED),
                        );
                        ui.label(
                            egui::RichText::new(
                                "Props are decorative; use cube geometry for solid barriers.",
                            )
                            .small()
                            .color(MUTED),
                        );
                        ui.separator();
                        if ui.button("Back to editor").clicked() {
                            resume = true;
                        }
                    });
            });
        if !open || resume {
            self.show_assets = false;
        }
        if resume {
            self.capture(window);
        }
    }
}
