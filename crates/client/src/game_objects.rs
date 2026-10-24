//! Object selection, placement, and editing share the geometry transaction queue.
use super::*;

#[derive(Clone)]
pub(super) struct ObjectSelection {
    pub id: u32,
    original: ObjectPlacement,
    pub draft: ObjectPlacement,
    pub base_revision: u64,
}

pub(super) struct PendingObjectEdit {
    pub request: u64,
    pub generation: u64,
    select_created: bool,
    pub created: Option<u32>,
}
impl PendingObjectEdit {
    pub fn observe_patch(&mut self, before: &[WorldObject], after: &[WorldObject]) {
        if self.select_created {
            // A successful create's patch is queued directly before its result
            // under the server lock. Earlier builders' patches may arrive while
            // we wait; each patch replaces this candidate until our result.
            self.created = after
                .iter()
                .find(|object| !before.iter().any(|old| old.id == object.id))
                .map(|object| object.id);
        }
    }
}

fn placement(object: &WorldObject) -> ObjectPlacement {
    ObjectPlacement {
        kind: object.kind,
        position: object.position,
        rotation: object.rotation,
    }
}

fn label(kind: ObjectKind) -> &'static str {
    crate::props::definition(kind).map_or(
        match kind {
            ObjectKind::Tree => "Tree",
            ObjectKind::Light => "Light",
            ObjectKind::Spawn => "Spawn",
            _ => "Object",
        },
        |prop| prop.name,
    )
}

fn bounds(kind: ObjectKind) -> (Vec3, Vec3) {
    if let Some(prop) = crate::props::definition(kind) {
        return (
            Vec3::from_array(prop.bounds_min),
            Vec3::from_array(prop.bounds_max),
        );
    }
    match kind {
        ObjectKind::Tree => (Vec3::new(-1.8, 0.0, -1.8), Vec3::new(1.8, 5.6, 1.8)),
        ObjectKind::Light => (Vec3::new(-0.8, -0.22, -0.25), Vec3::new(0.8, 1.32, 0.25)),
        _ => (Vec3::new(-0.43, 0.0, -0.43), Vec3::new(0.43, 0.08, 0.43)),
    }
}

fn ray_distance(object: &WorldObject, origin: Vec3, direction: Vec3) -> Option<f32> {
    if crate::props::definition(object.kind).is_some() {
        return crate::props::ray_distance(
            object.kind,
            object.position.to_array(),
            object.rotation,
            origin.to_array(),
            direction.to_array(),
        )
        .filter(|distance| *distance <= 60.0);
    }
    let rotation = glam::Mat3::from_rotation_y(-object.rotation);
    let origin = rotation * (origin - object.position);
    let direction = rotation * direction;
    let (min, max) = bounds(object.kind);
    let mut near = 0.0_f32;
    let mut far = 60.0_f32;
    for axis in 0..3 {
        let low = min[axis] - 0.06;
        let high = max[axis] + 0.06;
        if direction[axis].abs() < 1e-8 {
            if origin[axis] < low || origin[axis] > high {
                return None;
            }
        } else {
            let a = (low - origin[axis]) / direction[axis];
            let b = (high - origin[axis]) / direction[axis];
            near = near.max(a.min(b));
            far = far.min(a.max(b));
            if near > far {
                return None;
            }
        }
    }
    Some(near)
}

fn pick(objects: &[WorldObject], world: &World, origin: Vec3, direction: Vec3) -> Option<u32> {
    objects
        .iter()
        .filter_map(|object| {
            ray_distance(object, origin, direction)
                .filter(|distance| world.raycast(origin, direction, *distance).is_none())
                .map(|distance| (object.id, distance))
        })
        .min_by(|a, b| a.1.total_cmp(&b.1).then(a.0.cmp(&b.0)))
        .map(|(id, _)| id)
}

fn surface_placement(kind: ObjectKind, hit: &RayHit, yaw: f32) -> ObjectPlacement {
    let (position, rotation) = match kind {
        ObjectKind::WallLamp if hit.normal.y.abs() < 0.7 => (
            hit.point + hit.normal * 0.02,
            hit.normal.x.atan2(hit.normal.z),
        ),
        ObjectKind::HangingLamp => (
            hit.point + Vec3::Y * if hit.normal.y > 0.5 { 2.0 } else { -0.02 },
            yaw,
        ),
        ObjectKind::Light => (hit.point + Vec3::Y * 1.5, yaw),
        _ => (hit.point + Vec3::Y * 0.02, yaw),
    };
    ObjectPlacement {
        kind,
        position,
        rotation: rotation.rem_euclid(std::f32::consts::TAU),
    }
}

impl Game {
    pub(super) fn select_object(&mut self, id: Option<u32>) {
        self.selection_generation = self.selection_generation.wrapping_add(1);
        self.object_selection = id.and_then(|id| {
            self.objects
                .iter()
                .find(|object| object.id == id)
                .map(|object| ObjectSelection {
                    id,
                    original: placement(object),
                    draft: placement(object),
                    base_revision: self.revision,
                })
        });
    }

    pub(super) fn pick_object(&mut self) {
        let (origin, direction) = self.edit_ray();
        self.select_object(pick(&self.objects, &self.world, origin, direction));
    }

    pub(super) fn set_object_mode(&mut self, enabled: bool) {
        self.clear_selection();
        self.object_mode = enabled;
        self.paste_preview = false;
        self.notify(if enabled {
            "Object mode: click to select. Arrows move, Shift ↑/↓ raises/lowers, R rotates."
        } else {
            "Geometry mode: drag a volume to edit or copy geometry and fixtures together."
        });
    }

    pub(super) fn editing_busy(&self) -> bool {
        self.pending_edit.is_some() || self.pending_object.is_some() || !self.edit_queue.is_empty()
    }

    fn send_object_edit(&mut self, objects: Vec<ObjectEdit>, base_revision: u64) {
        if !self.connected || !self.local.editing {
            return;
        }
        if self.editing_busy() {
            self.notify("Wait for the current edit to finish.");
            return;
        }
        for edit in &objects {
            let object = match edit {
                ObjectEdit::Create { object } | ObjectEdit::Update { object, .. } => object,
                ObjectEdit::Delete { .. } => continue,
            };
            if !object.position.is_finite() || !object.rotation.is_finite() {
                self.notify("Object position and yaw must be finite numbers.");
                return;
            }
            if !self
                .world
                .contains_pos(object.position.floor().as_ivec3().to_array())
            {
                self.notify("Object must be inside the world bounds.");
                return;
            }
        }
        self.edit_request = self.edit_request.wrapping_add(1);
        self.pending_object = Some(PendingObjectEdit {
            request: self.edit_request,
            generation: self.selection_generation,
            select_created: matches!(objects.as_slice(), [ObjectEdit::Create { .. }]),
            created: None,
        });
        self.send(ClientMessage::EditTransaction {
            request: self.edit_request,
            base_revision,
            roots: Vec::new(),
            objects,
        });
    }

    pub(super) fn apply_object_draft(&mut self) {
        if let Some(selection) = &self.object_selection {
            let object = selection.draft.clone();
            self.send_object_edit(
                vec![ObjectEdit::Update {
                    id: selection.id,
                    object,
                }],
                selection.base_revision,
            );
        }
    }

    pub(super) fn nudge_object(&mut self, axis: usize, steps: f32) {
        if self.editing_busy() {
            self.notify("Wait for the current edit to finish.");
            return;
        }
        if let Some(selection) = &mut self.object_selection {
            selection.draft.position[axis] += steps * self.grid as f32 / 8.0;
            self.apply_object_draft();
        }
    }

    pub(super) fn rotate_object(&mut self, reverse: bool) {
        if self.editing_busy() {
            self.notify("Wait for the current edit to finish.");
            return;
        }
        if let Some(selection) = &mut self.object_selection {
            selection.draft.rotation = (selection.draft.rotation
                + if reverse { -1.0 } else { 1.0 } * std::f32::consts::FRAC_PI_2)
                .rem_euclid(std::f32::consts::TAU);
            self.apply_object_draft();
        }
    }

    pub(super) fn move_object_to_aim(&mut self) {
        if self.editing_busy() {
            self.notify("Wait for the current edit to finish.");
            return;
        }
        if let (Some(selection), Some(hit)) = (&mut self.object_selection, &self.selected) {
            selection.draft =
                surface_placement(selection.draft.kind, hit, selection.draft.rotation);
            self.apply_object_draft();
        }
    }

    pub(super) fn duplicate_object(&mut self) {
        if let Some(selection) = &self.object_selection {
            let mut object = selection.draft.clone();
            object.position.x += self.grid as f32 / 8.0;
            self.send_object_edit(vec![ObjectEdit::Create { object }], self.revision);
        }
    }

    pub(super) fn place(&mut self, kind: ObjectKind) {
        if let Some(hit) = &self.selected {
            let object = surface_placement(kind, hit, self.local.yaw);
            self.send_object_edit(vec![ObjectEdit::Create { object }], self.revision);
        }
    }

    pub(super) fn remove_object(&mut self) {
        if self.object_selection.is_none() {
            self.pick_object();
        }
        if let Some(selection) = &self.object_selection {
            self.send_object_edit(
                vec![ObjectEdit::Delete { id: selection.id }],
                selection.base_revision,
            );
        }
    }

    pub(super) fn draw_object_outline(mesh: &mut Geometry, object: &WorldObject, color: [f32; 3]) {
        let (min, max) = bounds(object.kind);
        let rotation = glam::Mat3::from_rotation_y(object.rotation);
        let corners: [Vec3; 8] = std::array::from_fn(|index| {
            object.position
                + rotation
                    * Vec3::from_array(std::array::from_fn(|axis| {
                        if index & (1 << axis) == 0 {
                            min[axis] - 0.02
                        } else {
                            max[axis] + 0.02
                        }
                    }))
        });
        for i in 0..8 {
            for bit in [1, 2, 4] {
                let j = i ^ bit;
                if i < j {
                    mesh.beam(corners[i], corners[j], 0.018, color, 0.9);
                }
            }
        }
        // Direction is visible even for rotationally symmetric props and spawns.
        mesh.beam(
            object.position,
            object.position + rotation * Vec3::Z * 0.7,
            0.025,
            color,
            0.9,
        );
    }

    pub(super) fn object_panel(&mut self, ctx: &egui::Context) {
        egui::Window::new("Objects")
            .anchor(egui::Align2::LEFT_TOP, [24.0, 100.0])
            .resizable(false)
            .default_width(290.0)
            .show(ctx, |ui| {
                if ui.button("Geometry mode (Q)").clicked() {
                    self.set_object_mode(false);
                    return;
                }
                ui.label("Click an object in the world to select it.");
                ui.horizontal(|ui| {
                    ui.label(format!("Grid {} m", self.grid as f32 / 8.0));
                    if ui.small_button("−").clicked() { self.change_grid(false); }
                    if ui.small_button("+").clicked() { self.change_grid(true); }
                });
                let busy = self.editing_busy();
                if let Some(selection) = &mut self.object_selection {
                    ui.separator();
                    ui.label(format!("{} #{}", label(selection.draft.kind), selection.id));
                    let missing = !self.objects.iter().any(|object| object.id == selection.id);
                    let changed = self.objects.iter().find(|object| object.id == selection.id)
                        .is_some_and(|object| placement(object) != selection.original);
                    if missing {
                        ui.colored_label(COPPER, "This object was deleted by another builder.");
                    } else if changed {
                        ui.colored_label(COPPER, "Changed by another builder. Refresh before applying.");
                    }
                    ui.add_enabled_ui(!busy && !missing, |ui| {
                        for axis in 0..3 {
                            ui.horizontal(|ui| {
                                ui.label(["X", "Y", "Z"][axis]);
                                ui.add(egui::DragValue::new(&mut selection.draft.position[axis]).speed(0.125).suffix(" m"));
                            });
                        }
                        let mut degrees = selection.draft.rotation.to_degrees();
                        if ui.add(egui::DragValue::new(&mut degrees).speed(5.0).prefix("Yaw ").suffix("°")).changed() {
                            selection.draft.rotation = degrees.to_radians().rem_euclid(std::f32::consts::TAU);
                        }
                    });
                    ui.add_enabled_ui(!busy, |ui| {
                        ui.horizontal(|ui| {
                            if ui.button("Apply").clicked() { self.apply_object_draft(); }
                            if ui.button("Refresh").clicked() {
                                let id = self.object_selection.as_ref().map(|selection| selection.id);
                                self.select_object(id);
                            }
                        });
                        ui.label(format!("Move by {} m", self.grid as f32 / 8.0));
                        for axis in 0..3 {
                            ui.horizontal(|ui| {
                                ui.label(["X", "Y", "Z"][axis]);
                                if ui.small_button("−").clicked() { self.nudge_object(axis, -1.0); }
                                if ui.small_button("+").clicked() { self.nudge_object(axis, 1.0); }
                            });
                        }
                        ui.horizontal(|ui| {
                            if ui.button("-90°").clicked() { self.rotate_object(true); }
                            if ui.button("+90°").clicked() { self.rotate_object(false); }
                        });
                        ui.horizontal(|ui| {
                            if ui.button("Duplicate").clicked() { self.duplicate_object(); }
                            if ui.button("Delete").clicked() { self.remove_object(); }
                        });
                    });
                }
                if busy { ui.label("Waiting for server…"); }
                ui.label(egui::RichText::new("Arrows move X/Z · Shift ↑/↓ moves Y\nR rotates · H moves to aimed surface\nInsert duplicates · Delete removes\nU undo · Shift U redo · Esc releases mouse").small().color(MUTED));
            });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duplicate_selection_tracks_its_patch_after_another_builders_create() {
        let object = WorldObject {
            id: 1,
            kind: ObjectKind::Light,
            position: Vec3::ZERO,
            rotation: 0.0,
        };
        let concurrent = WorldObject {
            id: 2,
            ..object.clone()
        };
        let duplicate = WorldObject {
            id: 3,
            ..object.clone()
        };
        let mut pending = PendingObjectEdit {
            request: 8,
            generation: 0,
            select_created: true,
            created: None,
        };
        pending.observe_patch(
            std::slice::from_ref(&object),
            &[object.clone(), concurrent.clone()],
        );
        pending.observe_patch(
            &[object.clone(), concurrent.clone()],
            &[object, concurrent, duplicate],
        );
        assert_eq!(pending.created, Some(3));
    }

    #[test]
    fn wall_placement_faces_the_surface_and_other_objects_keep_normalized_yaw() {
        let mut world = World::empty();
        world.set([0, 2, 0], Some(Cell::solid(0)));
        let hit = world
            .raycast(Vec3::new(2.0, 2.5, 0.5), -Vec3::X, 4.0)
            .unwrap();
        let lamp = surface_placement(ObjectKind::WallLamp, &hit, 5.0);
        let facing = glam::Mat3::from_rotation_y(lamp.rotation) * Vec3::Z;
        assert!(facing.abs_diff_eq(hit.normal, 1e-5));
        assert!(lamp
            .position
            .abs_diff_eq(hit.point + hit.normal * 0.02, 1e-5));
        let spawn = surface_placement(ObjectKind::Spawn, &hit, -0.25);
        assert!((0.0..std::f32::consts::TAU).contains(&spawn.rotation));
        assert!((spawn.rotation - (std::f32::consts::TAU - 0.25)).abs() < 1e-5);
    }

    #[test]
    fn picking_uses_nearest_object_and_geometry_occludes_every_kind() {
        for kind in [
            ObjectKind::Light,
            ObjectKind::Spawn,
            ObjectKind::Tree,
            ObjectKind::WallLamp,
        ] {
            let far = WorldObject {
                id: 1,
                kind,
                position: Vec3::new(0.5, 2.0, 8.5),
                rotation: 0.0,
            };
            let near = WorldObject {
                id: 2,
                position: Vec3::new(0.5, 2.0, 5.5),
                ..far.clone()
            };
            let origin = Vec3::new(0.5, 2.03, 0.5);
            let objects = [far, near];
            assert_eq!(
                pick(&objects, &World::empty(), origin, Vec3::Z),
                Some(2),
                "{kind:?}"
            );
            let mut wall = World::empty();
            wall.set([0, 2, 2], Some(Cell::solid(0)));
            assert_eq!(pick(&objects, &wall, origin, Vec3::Z), None, "{kind:?}");
        }
    }
}
