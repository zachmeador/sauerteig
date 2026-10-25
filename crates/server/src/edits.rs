//! The ordered edit worker prepares geometry on a shared world view. The owner
//! alone checks current player state, ownership clocks, budgets, and commits.
use super::*;

/// What the player is told when a change cannot be published in time.
pub(crate) const BUSY: &str =
    "The server is still catching a joining or slow player up on earlier edits; try again in a moment.";

/// Why the owner did not commit a prepared edit.
#[derive(Debug)]
pub(crate) enum CommitError {
    /// The edit is invalid now; the text is the player's `EditResult` error.
    Rejected(String),
    /// Some peer's reliable queue cannot yet hold the patch. Nothing changed,
    /// so the same prepared edit can be offered again.
    Busy,
}
impl CommitError {
    pub fn message(self) -> String {
        match self {
            Self::Rejected(message) => message,
            Self::Busy => BUSY.into(),
        }
    }
}
impl From<String> for CommitError {
    fn from(message: String) -> Self {
        Self::Rejected(message)
    }
}
impl From<&str> for CommitError {
    fn from(message: &str) -> Self {
        Self::Rejected(message.into())
    }
}

pub(crate) enum EditAction {
    Raw(ClientMessage),
    Transaction {
        roots: Vec<RootEdit>,
        objects: Vec<ObjectEdit>,
        volumes: Vec<VolumeEdit>,
        base_revision: Option<u64>,
        patch: EditPatch,
    },
    Undo(UndoEntry),
    Redo(UndoEntry),
}
pub(crate) struct EditPlan {
    pub revision: u64,
    pub world: World,
    pub action: EditAction,
}
pub(crate) struct PreparedEdit {
    revision: u64,
    world: World,
    action: EditAction,
    roots: Vec<RootEdit>,
    before: Arc<Vec<RootEdit>>,
    history_after: Arc<Vec<RootEdit>>,
    retired_history: Vec<UndoEntry>,
    costs: Vec<RootCost>,
    volume_bytes: usize,
    volumes_before: Option<Vec<MaterialVolume>>,
    volumes_after: Option<Vec<MaterialVolume>>,
    charge: usize,
    pub timings: [Duration; 3],
}
impl EditPlan {
    // All validation, canonicalization, clipping, spatial-index building, and
    // JSON budget counting runs here, on the edit worker.
    pub fn prepare(mut self) -> Result<PreparedEdit, String> {
        if let EditAction::Raw(message) = self.action {
            self.action = transaction(message)?;
        }
        let old_volumes = self.world.volumes().to_vec();
        let (roots, charge) = match &self.action {
            EditAction::Transaction {
                roots,
                objects,
                volumes,
                ..
            } => {
                if roots.is_empty() && objects.is_empty() && volumes.is_empty() {
                    return Err(
                        "An edit must change geometry, objects, or material volumes.".into(),
                    );
                }
                if !roots.is_empty() {
                    validate_root_request(roots)?;
                }
                if objects.len() > MAX_OBJECTS * 2 {
                    return Err("An edit may contain at most 2,048 object changes.".into());
                }
                for edit in objects {
                    if let ObjectEdit::Create { object } | ObjectEdit::Update { object, .. } = edit
                    {
                        validate_object_position(object.position, object.rotation)?;
                        if !self
                            .world
                            .contains_pos(object.position.floor().as_ivec3().to_array())
                        {
                            return Err("Object must be inside the world bounds.".into());
                        }
                    }
                }
                if !volumes.is_empty() {
                    self.world.apply_volume_edits(volumes)?;
                }
                (roots.clone(), volumes.len())
            }
            EditAction::Raw(_) => return Err("Unprepared edit".into()),
            EditAction::Undo(entry) => {
                if let Some(volumes) = &entry.volumes_before {
                    self.world.replace_volumes(volumes)?;
                }
                (
                    entry.before.as_ref().clone(),
                    entry.volumes_before.as_ref().map_or(0, Vec::len),
                )
            }
            EditAction::Redo(entry) => {
                if let Some(volumes) = &entry.volumes_after {
                    self.world.replace_volumes(volumes)?;
                }
                (
                    entry.after.as_ref().clone(),
                    entry.volumes_after.as_ref().map_or(0, Vec::len),
                )
            }
        };
        let mut before: Vec<_> = roots
            .iter()
            .map(|r| RootEdit {
                pos: r.pos,
                fragments: self.world.root_fragments(r.pos),
            })
            .collect();
        // This is the one validation/canonicalization/compilation of each root.
        self.world.apply_root_edits(&roots)?;
        let mut after: Vec<_> = roots
            .iter()
            .map(|r| RootEdit {
                pos: r.pos,
                fragments: self.world.root_fragments(r.pos),
            })
            .collect();
        let mut i = 0;
        before.retain(|old| {
            let changed = *old != after[i];
            i += 1;
            changed
        });
        let changed: HashSet<_> = before.iter().map(|r| r.pos).collect();
        after.retain(|new| changed.contains(&new.pos));
        // `before` follows request order; use a set rather than assuming sorted requests.
        if roots_complexity(&before) > MAX_EDIT_FRAGMENTS {
            return Err(
                "This edit replaces too much detailed geometry at once; use a smaller selection."
                    .into(),
            );
        }
        let costs = after
            .iter()
            .map(|root| RootCost::new(root, self.world.has_regions()))
            .collect();
        let volumes_before = (old_volumes != self.world.volumes()).then_some(old_volumes);
        let volumes_after = volumes_before
            .as_ref()
            .map(|_| self.world.volumes().to_vec());
        let charge = charge + roots_complexity(&after);
        let volume_bytes = json_bytes(&self.world.volumes());
        Ok(PreparedEdit {
            revision: self.revision,
            world: self.world,
            action: self.action,
            history_after: Arc::new(after.clone()),
            retired_history: Vec::new(),
            roots: after,
            before: Arc::new(before),
            costs,
            volume_bytes,
            volumes_before,
            volumes_after,
            charge,
            timings: [Duration::ZERO; 3],
        })
    }
}
fn transaction(message: ClientMessage) -> Result<EditAction, String> {
    let action = match message {
        ClientMessage::Edit { edits, .. } => {
            // Check coordinates before legacy_root multiplies them. Shape
            // validation happens once in the core compiler on the worker.
            if edits.is_empty()
                || edits.len() > MAX_EDITS
                || edits.iter().any(|e| !World::is_valid_pos(e.pos))
            {
                return Err("Invalid cell edit count or position.".into());
            }
            EditAction::Transaction {
                roots: edits.into_iter().map(legacy_root).collect(),
                objects: vec![],
                volumes: vec![],
                base_revision: None,
                patch: EditPatch::Legacy,
            }
        }
        ClientMessage::EditRoots {
            roots,
            base_revision,
            ..
        } => EditAction::Transaction {
            roots,
            objects: vec![],
            volumes: vec![],
            base_revision: Some(base_revision),
            patch: EditPatch::Roots,
        },
        ClientMessage::EditTransaction {
            roots,
            objects,
            volumes,
            base_revision,
            ..
        } => EditAction::Transaction {
            roots,
            objects,
            volumes,
            base_revision: Some(base_revision),
            patch: EditPatch::Transaction,
        },
        ClientMessage::PlaceObject {
            kind,
            position,
            rotation,
        } => EditAction::Transaction {
            roots: vec![],
            objects: vec![ObjectEdit::Create {
                object: ObjectPlacement {
                    kind,
                    position,
                    rotation,
                },
            }],
            volumes: vec![],
            base_revision: None,
            patch: EditPatch::Legacy,
        },
        ClientMessage::RemoveObject { id } => EditAction::Transaction {
            roots: vec![],
            objects: vec![ObjectEdit::Delete { id }],
            volumes: vec![],
            base_revision: None,
            patch: EditPatch::Legacy,
        },
        _ => return Err("This message is not a transaction.".into()),
    };
    Ok(action)
}

impl Session {
    pub(crate) fn edit_plan(&self, id: u64, message: ClientMessage) -> Result<EditPlan, String> {
        self.require_editor(id)?;
        let action = match message {
            ClientMessage::Undo => EditAction::Undo(
                self.peer(id)?
                    .undo
                    .back()
                    .cloned()
                    .ok_or("Nothing to undo.")?,
            ),
            ClientMessage::Redo => EditAction::Redo(
                self.peer(id)?
                    .redo
                    .last()
                    .cloned()
                    .ok_or("Nothing to redo.")?,
            ),
            message => EditAction::Raw(message),
        };
        Ok(EditPlan {
            revision: self.revision,
            world: self.world.clone(),
            action,
        })
    }
    /// Leaves replaced geometry and retired history in the prepared edit so
    /// the worker frees them, including on rejection. The worker serializes edits, so revision cannot change during
    /// preparation; retain the check to fail safely if that ownership changes.
    ///
    /// Every check runs before anything changes. The last is room: each peer's
    /// reliable queue must hold the exact patch this commit will broadcast, plus
    /// follow-up space. `Busy` leaves the edit intact so it can be offered again.
    pub(crate) fn commit_prepared(
        &mut self,
        id: u64,
        edit: &mut PreparedEdit,
    ) -> Result<(), CommitError> {
        let started = Instant::now();
        self.require_editor(id)?;
        if self.revision != edit.revision {
            return Err("The world changed during edit preparation; try again.".into());
        }
        let mut next_object = self.next_object;
        let (objects_before, objects_after, patch, history) = match &edit.action {
            EditAction::Raw(_) => return Err("Unprepared edit".into()),
            EditAction::Transaction {
                roots,
                objects,
                volumes,
                base_revision,
                patch,
            } => {
                if let Some(base) = base_revision {
                    if *base > self.revision {
                        return Err("Edit revision is ahead of the server.".into());
                    }
                    if roots
                        .iter()
                        .any(|r| self.root_changed_at.get(&r.pos).is_some_and(|v| v > base))
                    {
                        return Err("These cells changed after the edit began; try again.".into());
                    }
                    if !volumes.is_empty() && self.volumes_changed_at > *base {
                        return Err(
                            "Material volumes changed after the edit began; try again.".into()
                        );
                    }
                }
                let mut seen = HashSet::new();
                let mut before = Vec::new();
                let mut after = Vec::new();
                for object in objects {
                    let (object_id, placement) = match object {
                        ObjectEdit::Create { object } => {
                            if next_object == u32::MAX {
                                return Err("No object IDs remain.".into());
                            }
                            let id = next_object;
                            next_object += 1;
                            (id, Some(object))
                        }
                        ObjectEdit::Update { id, object } => {
                            if !self.objects.iter().any(|o| o.id == *id) {
                                return Err("That object no longer exists.".into());
                            }
                            (*id, Some(object))
                        }
                        ObjectEdit::Delete { id } => {
                            if !self.objects.iter().any(|o| o.id == *id) {
                                return Err("That object no longer exists.".into());
                            }
                            (*id, None)
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
                    let value = placement.map(|o| WorldObject {
                        id: object_id,
                        position: o.position,
                        kind: o.kind,
                        rotation: o.rotation,
                    });
                    let old = self.objects.iter().find(|o| o.id == object_id).cloned();
                    if old != value {
                        before.push(ObjectState {
                            id: object_id,
                            value: old,
                        });
                        after.push(ObjectState {
                            id: object_id,
                            value,
                        });
                    }
                }
                (before, after, *patch, None)
            }
            EditAction::Undo(entry) | EditAction::Redo(entry) => {
                let undo = matches!(edit.action, EditAction::Undo(_));
                let current = if undo {
                    self.peer(id)?.undo.back()
                } else {
                    self.peer(id)?.redo.last()
                };
                if current.is_none_or(|e| e.revision != entry.revision) {
                    return Err("Edit history changed; try again.".into());
                }
                let verb = if undo { "undo" } else { "redo" };
                for (root, previous) in entry.before.iter().zip(&entry.previous_revisions) {
                    let expected = if undo {
                        Some(entry.revision)
                    } else {
                        *previous
                    };
                    if self.cell_revisions.get(&root.pos).copied() != expected {
                        return Err(
                            format!("Cannot {verb}: a later edit changed these cells.").into()
                        );
                    }
                }
                for (object, previous) in entry
                    .objects_before
                    .iter()
                    .zip(&entry.previous_object_revisions)
                {
                    let expected = if undo {
                        Some(entry.revision)
                    } else {
                        *previous
                    };
                    if self.object_revisions.get(&object.id).copied() != expected {
                        return Err(
                            format!("Cannot {verb}: a later edit changed these objects.").into(),
                        );
                    }
                }
                let expected = if undo {
                    Some(entry.revision)
                } else {
                    entry.previous_volume_revision
                };
                if entry.volumes_before.is_some() && self.volume_revision != expected {
                    return Err(
                        format!("Cannot {verb}: a later edit changed material volumes.").into(),
                    );
                }
                let (before, after) = if undo {
                    (&entry.objects_after, &entry.objects_before)
                } else {
                    (&entry.objects_before, &entry.objects_after)
                };
                (
                    before.clone(),
                    after.clone(),
                    entry.patch,
                    Some((undo, entry.clone())),
                )
            }
        };
        self.validate_object_count(&objects_after)?;
        self.validate_fighter_overlap(&edit.roots)?;
        if let Some(volumes) = &edit.volumes_after {
            self.validate_volume_fighter_overlap(volumes)?;
        }
        let mut fragments = self.geometry_fragments;
        let mut bytes = self.geometry_bytes;
        for (root, added) in edit.roots.iter().zip(&edit.costs) {
            let old = self
                .geometry_root_costs
                .get(&root.pos)
                .copied()
                .unwrap_or_default();
            fragments = fragments - old.fragments + added.fragments;
            bytes = bytes - old.bytes + added.bytes;
        }
        if fragments > MAX_WORLD_FRAGMENTS {
            return Err("The room has reached its detailed geometry limit.".into());
        }
        if bytes + edit.volume_bytes > MAX_GEOMETRY_BYTES {
            return Err(SNAPSHOT_LIMIT_ERROR.into());
        }
        if edit.roots.is_empty() && objects_after.is_empty() && edit.volumes_after.is_none() {
            return Ok(());
        }
        // The patch and the follow-up result must fit in every queue before the
        // room changes, or a peer that is still downloading would be dropped.
        let volumes = edit
            .volumes_after
            .as_ref()
            .map(|_| edit.world.volumes().len());
        let reserved = Publication::choose(
            patch,
            &edit.roots,
            !objects_after.is_empty(),
            edit.volumes_after.is_some(),
        )
        .cost(
            &edit.roots,
            self.object_count_after(&objects_after),
            volumes,
        ) + FOLLOW_UP_BYTES;
        if !self.peers_have_room(reserved) {
            return Err(CommitError::Busy);
        }
        self.charge_edit(id, edit.charge + objects_after.len())?;
        let previous_revisions = edit
            .roots
            .iter()
            .map(|r| self.cell_revisions.get(&r.pos).copied())
            .collect();
        let previous_object_revisions = objects_before
            .iter()
            .map(|o| self.object_revisions.get(&o.id).copied())
            .collect();
        let previous_volume_revision = self.volume_revision;
        edit.timings[0] = started.elapsed();
        let applying = Instant::now();
        std::mem::swap(&mut self.world, &mut edit.world);
        self.changed();
        self.geometry_fragments = fragments;
        self.geometry_bytes = bytes;
        self.volume_bytes = edit.volume_bytes;
        for (root, cost) in edit.roots.iter().zip(edit.costs.iter().copied()) {
            if cost.fragments == 0 {
                self.geometry_root_costs.remove(&root.pos);
            } else {
                self.geometry_root_costs.insert(root.pos, cost);
            }
            self.cell_revisions.insert(root.pos, self.revision);
            self.root_changed_at.insert(root.pos, self.revision);
        }
        self.apply_object_states(&objects_after);
        if edit.volumes_after.is_some() {
            self.volume_revision = Some(self.revision);
            self.volumes_changed_at = self.revision;
        }
        self.next_object = next_object;
        if let Some((undo, entry)) = history {
            for (root, previous) in entry.before.iter().zip(&entry.previous_revisions) {
                let revision = if undo {
                    *previous
                } else {
                    Some(entry.revision)
                };
                if let Some(r) = revision {
                    self.cell_revisions.insert(root.pos, r);
                } else {
                    self.cell_revisions.remove(&root.pos);
                }
            }
            for (object, previous) in entry
                .objects_before
                .iter()
                .zip(&entry.previous_object_revisions)
            {
                let revision = if undo {
                    *previous
                } else {
                    Some(entry.revision)
                };
                if let Some(r) = revision {
                    self.object_revisions.insert(object.id, r);
                } else {
                    self.object_revisions.remove(&object.id);
                }
            }
            if entry.volumes_before.is_some() {
                self.volume_revision = if undo {
                    entry.previous_volume_revision
                } else {
                    Some(entry.revision)
                };
            }
            let peer = self.peer_mut(id)?;
            if undo {
                peer.undo.pop_back();
                peer.redo.push(entry);
            } else {
                peer.redo.pop();
                peer.undo.push_back(entry);
            }
        } else {
            let complexity = roots_complexity(&edit.before)
                + roots_complexity(&edit.roots)
                + objects_before.len() * 2
                + edit.volumes_before.as_ref().map_or(0, Vec::len)
                + edit.volumes_after.as_ref().map_or(0, Vec::len);
            let entry = UndoEntry {
                revision: self.revision,
                before: edit.before.clone(),
                after: edit.history_after.clone(),
                previous_revisions,
                patch,
                objects_before,
                objects_after: objects_after.clone(),
                previous_object_revisions,
                volumes_before: edit.volumes_before.take(),
                volumes_after: edit.volumes_after.clone(),
                previous_volume_revision,
                complexity,
            };
            let peer = self.peer_mut(id)?;
            edit.retired_history.append(&mut peer.redo);
            let mut history_complexity = peer.undo.iter().map(|e| e.complexity).sum::<usize>();
            while peer.undo.len() >= 32 || history_complexity + complexity > MAX_HISTORY_COMPLEXITY
            {
                let Some(discarded) = peer.undo.pop_front() else {
                    break;
                };
                history_complexity -= discarded.complexity;
                edit.retired_history.push(discarded);
            }
            peer.undo.push_back(entry);
        }
        edit.timings[1] = applying.elapsed();
        let publishing = Instant::now();
        self.broadcast_change(
            std::mem::take(&mut edit.roots),
            patch,
            !objects_after.is_empty(),
            edit.volumes_after.is_some(),
        );
        edit.timings[2] = publishing.elapsed();
        Ok(())
    }
    #[cfg(test)]
    pub(crate) fn edit(&mut self, id: u64, edits: Vec<CellEdit>) -> Result<(), String> {
        self.handle(id, ClientMessage::Edit { request: 0, edits })
    }
    #[cfg(test)]
    pub(crate) fn edit_roots(
        &mut self,
        id: u64,
        base_revision: u64,
        roots: Vec<RootEdit>,
    ) -> Result<(), String> {
        self.handle(
            id,
            ClientMessage::EditRoots {
                request: 0,
                base_revision,
                roots,
            },
        )
    }
    #[cfg(test)]
    pub(crate) fn undo(&mut self, id: u64) -> Result<(), String> {
        self.handle(id, ClientMessage::Undo)
    }
    #[cfg(test)]
    pub(crate) fn redo(&mut self, id: u64) -> Result<(), String> {
        self.handle(id, ClientMessage::Redo)
    }
}
