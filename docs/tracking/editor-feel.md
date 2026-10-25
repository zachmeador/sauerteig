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
2. ~~Delete doesn't empty a selection.~~ Fixed in stage 1.
3. ~~Extrusion uses the current material instead of copying the faces behind.~~
   Fixed in stage 1.
4. **Selections can't be transformed in place.** They can't slide, move along
   their normal, cut-move, flip, or rotate.
5. **The keymap differs from Sauerbraten and can't be rebound.** This covers Q,
   1–8, M, redo, Ctrl+wheel, and Alt fine steps. (Stage 1 stopped number keys from
   changing the weapon and the material at once.)
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

State: merged on 2026-10-25. Play-testing in the app is pending.

- **Extrusion copies the faces behind.**
  - Each new cell takes all six face materials from the nearest solid piece in
    the layer one grid step inside the face, which is the layer wheel-up would
    carve.
  - Refined roots copy piece by piece, so a painted patch repeats at its own size.
  - A warped cell counts only if its solid reaches that layer.
  - Columns with nothing behind use the current material.
- **Delete and Backspace empty the active selection** (every volume, one undoable
  edit), or the aimed cell without a selection. They touch solid geometry only,
  and the selection stays in place.
  - A selected object, such as one just placed, is removed first unless geometry
    has been selected since; the design lead added this precedence at merge.
  - Object mode always removes objects.
- **Number keys follow the mode:** materials in edit mode, weapons 1–2 in play
  mode.

Gaps: fill and Delete share every edit's size limits (16,384 roots, 65,536
fragments). Extruding also reads the layer behind.

## Evidence

Stage 1, 2026-10-25:

- **Checks.** fmt, Clippy, all workspace tests, and native builds passed. The
  browser build ran at merge.
- **Unit tests in `editor.rs`** cover:
  - six-face copying on every axis;
  - refined-root patches;
  - imported regions;
  - nearest-first stacking;
  - warped cells;
  - a randomized per-unit oracle;
  - multi-volume Delete with undo;
  - Delete on imported and empty space.
- **End-to-end test.** Fill and Delete each go to the real embedded server as one
  `EditRoots`, and one `Undo` restores the world.
- **Mutation checks.** Hand-made mutations of the fill order, column claiming, and
  key routing each failed the tests.
- **Not observed yet in the running app:**
  - a multi-material wall keeping its faces after extrusion;
  - multi-volume Delete followed by undo;
  - number keys in both modes.
