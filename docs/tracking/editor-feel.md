# Editor feel

State: active.

## Outcome

Building feels as direct as Sauerbraten's editor for people with Sauerbraten
muscle memory, and the editor's small rough edges are gone. The
[Sauerbraten editor reference](../../research/sauerbraten-editor.md) records the
source-derived behavior to match.

## Known gaps

Ranked by how much each would bother a Sauerbraten builder. Several belong to
other tracks, as linked.

1. **Texture alignment tools are missing:** texture alignment itself, all-faces
   mode, a recent-texture list, an eyedropper, and replace. These come with the
   [world format](world-format.md).
2. **Delete doesn't empty a selection.** Stage 1.
3. **Extrusion uses the current material** instead of copying the faces behind.
   Stage 1.
4. **Selections can't be transformed in place.** They can't slide, move along
   their normal, cut-move, flip, or rotate.
5. **The keymap differs from Sauerbraten and can't be rebound.** This covers Q,
   1–8, M, redo, Ctrl+wheel, and Alt fine steps. Number keys 1 and 2 also set the
   weapon and the material at once; stage 1 fixes that part.
6. **No heightmap brush.**
7. **Object editing is limited.** It lacks 3D drag, group selection,
   wheel and number-key attributes, and select-in-box. Most of this comes with
   object properties in the [world format](world-format.md).
8. **Missing volume kinds and no material display.** These come with the
   [world format](world-format.md).
9. **Edits wait for the server one at a time.** This is
   [shared-sandbox](shared-sandbox.md) stage 3.
10. **Smaller differences:**
    - drag-select is locked to a plane;
    - there is no pass-through aiming;
    - undo drops the selection;
    - V pastes on press instead of on release.

Not a gap: hover-only extrusion builds a staircase at oblique angles in
Sauerbraten too. In both editors, pillars grow from an explicit selection.

## Stage 1: Sauerbraten fill and delete

State: active on branch `editor-fill-delete`.

Decisions:

- **Extrusion copies the faces behind**, like Sauerbraten's fill. Each new cell
  takes the material assignment of the solid geometry directly behind it along the
  extrusion direction, including all six face materials. This applies whether that
  geometry is a coarse cell, a refined root, or an imported compact region. Where
  nothing solid is behind, use the current material. Y+wheel and the asset browser
  remain the ways to paint.
- **Delete and Backspace empty the active selection**, like Sauerbraten's
  `delcube`. The active selection is the explicit selection, the region set, or
  the aimed cell, as `active_selection` defines it today. Emptying covers every
  volume of an additive selection, as one undoable edit. In object mode, the keys
  still remove the selected or aimed object.
- **Number keys follow the mode.** In edit mode they choose materials; in play
  mode they choose weapons. Neither changes the other.
- **Update the controls** in [the editor docs](../editor.md).

Not in this stage: the other gaps above.

Parallel work: sessions on shared-sandbox stage 1 and renderer stage 1 are changing
`game.rs`'s connection code and camera interpolation. A movement session is
changing `crates/core`.
- Put the planning work in `editor.rs`.
- Limit `game.rs` changes to key handling and the edit-intent hooks.
- Leave server, core, and renderer files untouched.

Acceptance:

- **Checks:** fmt, Clippy, all workspace tests, and native and browser builds pass.
- **New unit tests:**
  - fill copies per-face materials from coarse cells, refined roots, and imported
    compact regions;
  - delete plans cover multi-volume additive selections, and undoing them
    restores the geometry.
- **Observed in the running app:**
  - extruding a wall painted with several materials keeps the faces;
  - deleting a multi-volume selection and undoing it works;
  - number keys behave correctly in edit and play modes.

## Evidence

None yet for this outcome.
