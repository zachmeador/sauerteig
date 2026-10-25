# Sauerbraten editor reference

How Sauerbraten's editor behaves, read from its source rather than from memory.
This is reference material for [editor feel](../docs/tracking/editor-feel.md) and
the [world format](../docs/tracking/world-format.md).

Source: Sauerbraten SVN trunk r6889, plus a 2025 GitHub copy of Tesseract trunk
where noted. The files read were `src/engine/octaedit.cpp`, `texture.cpp`,
`octarender.cpp`, `world.cpp`, `material.cpp`, and `physics.cpp`;
`src/fpsgame/client.cpp` and `server.cpp`; and `data/stdedit.cfg`, `stdlib.cfg`,
and `defaults.cfg`. Line numbers refer to r6889. A research agent compiled this on
2026-10-25. The hover-selection and fill claims were checked against
`octaedit.cpp`; the rest relies on that agent's reading.

## Selection

- **Hover.** While nothing is explicitly selected, `rendereditcursor` rebuilds the
  selection every frame from the grid cell under the crosshair. It is oriented to
  the face the ray enters (`octaedit.cpp` ~L494).
- **Click and drag.** This selects a 3D box between the start cell and the cell
  now under the cursor; it is not locked to a plane. The selection persists until
  Space, a new click, a grid change, or leaving edit mode. Middle-drag keeps
  half-cell resolution for corner and edge sub-selections.
- **Right click (`editextend`).**
  - It extends the box in 3D to the hovered cell and reorients the selection to
    the face under the cursor.
  - While held, the box slides on that face's plane, and the wheel moves the box
    along its normal without editing (`pushsel`).
  - Keypad 4–9 also move the selection. Mouse back and forward cut and move its
    contents.
  - Holding Ctrl makes aiming pass through the selection box and entities.
- **Wheel.** It edits cubes when there is a selection or no entity is involved.
  Aiming at an entity makes the wheel edit that entity's attributes instead.
- **Pillar growth (`mpeditface` mode 1).**
  - Wheel-down steps the selection one grid outward and fills that layer,
    **copying all six textures of the cube behind**. The selection stays on the
    new layer.
  - Wheel-up empties the layer and steps inward.
  - This only works with an explicit selection. With hover alone the selection is
    rebuilt from the ray each frame, so at oblique angles Sauerbraten also builds
    a staircase.
- **Grid.** Grid power runs 0–12 (1–4,096 units; default 8) and changes with
  G+wheel. Changing it cancels the selection.
- **Deformation.** Steps are 1/8 of a cell edge.
  - F+wheel pushes all four corners of every selected cell.
  - Q+wheel pushes the corner of the quadrant under the crosshair, in every
    selected cell, with no click needed.
- **Toggles:**
  - 6 all faces;
  - 8 entity editing;
  - 9 entity snapping;
  - M material display;
  - `editinview` and `gridlookup`.

## Default binds

From `defaults.cfg` L196–343, with wheel modifiers in `stdlib.cfg` L102–179.

What the wheel does when combined with:

| Modifier | Wheel action |
| --- | --- |
| Nothing | Fill or empty cubes (up carves, down adds), or edit an entity attribute |
| F | Push the selected face |
| Q | Push a corner |
| G | Change grid size |
| R | Rotate the selected region 90° (entity yaw ±15°) |
| Y | Scroll recently used textures, applied at once |
| 1–5 | Entity attributes 1–5 when an entity is selected; otherwise texture offset X, offset Y, rotation, and scale (×2 or ÷2) |
| B | Change heightmap brush |
| Alt | Fine steps |

Keys:

| Key | Action |
| --- | --- |
| E | Toggle edit mode |
| Space | Cancel the selection |
| Shift | Fast flight |
| C | Copy |
| V | Paste: pressing shows a clipboard-sized box, releasing pastes. `opaquepaste` clears the box first |
| P | Paste highlight |
| X | Flip |
| U, Z | Undo |
| I | Redo |
| Delete | Empty the whole selection (`delcube`); deletes entities first if they are aimed at or selected |
| H | Heightmap mode |
| M | Show materials |
| F1 | Pick the aimed texture |
| F2 | Texture browser |
| Keypad Enter | Select the entities in the box |

Tesseract changes a few: O/P+wheel set the offset, comma rotates, and period
scales. Pressing H again smooths.

## Texture model

- **Storage.** Each cube face holds a 16-bit virtual-slot (vslot) index
  (`cube.texture[6]`, `octa.h` ~L195).
  - A slot is images plus a shader from the map config.
  - A vslot is a slot plus overrides: scale (default 1), rotation 0–7, offset in
    texels, scroll, layer, front and back alpha, color scale, shader parameters,
    and a mask of changed fields.
  - Edits intern deduplicated variants (`editvslot`, `texture.cpp` ~L2034). There
    are at most 65,536, compacted every 256 edits.
  - Only changed fields are saved or sent.
- **Editing** (`octaedit.cpp` ~L2239–2347; `mergevslot`, `texture.cpp` ~L1796).
  - The commands:
    - `vrotate` is absolute 0–7; deltas clamp.
    - `voffset` deltas wrap at the texture size.
    - `vscale` is absolute 1/8–8; deltas multiply.
    - Also `vscroll`, `vcolor`, `valpha`, `vlayer`, `vshaderparam`, `vreset`,
      and `vdelta`.
  - They apply to the selection's face, or to all six in all-faces mode.
  - Applying a texture from the browser uses the root vslot and resets alignment.
  - Recently used entries keep their variant, so F1 followed by Y re-applies an
    alignment.
  - `replace` and `replacesel` swap vslots across the map or within the selection.
- **Rotation indices** (`texrotations`, `texture.cpp` ~L442): 0 none, 1 = 90°,
  2 = 180°, 3 = 270°, 4 flip X, 5 flip Y, 6 transpose, 7 flipped transpose.
- **Mapping** (`calctexgen`, `octarender.cpp` ~L741). Textures use a world-space
  planar projection along the face's **nominal** axis, not its actual normal.
  - At scale 1 there are 8 texels per unit, so a 512 px texture repeats every
    64 units.
  - Opposite faces share a mirrored projection.
  - Slopes stretch.
- **Copy, paste, and rotate: alignment does not survive moves or rotations.**
  - Pasted cubes keep their vslots, but the projection is anchored to the world,
    so moving a block shifts its texture.
  - `rotatecube` and `flipcube` only permute which face holds which texture index.
    They never adjust the vslot rotation or offset.
  - Prefab files omit vslots.
  - Making alignment survive would need paste to add the translation to the
    offset, and rotate or flip to compose the rotation index and swap axes.
- **Tesseract.**
  - It uses 16 texels per unit, so 512 px repeats every 32 units.
  - It applies per-face signs in `calctexgen` so text reads correctly from both
    sides.
  - It adds `vrefract`, `vdetail`, and three color channels.
  - The merge rules are the same.

## Heightmap

From `octaedit.cpp` ~L1592–1943.

- **Entering and leaving.** H toggles heightmap mode, using the aimed face's axis
  as the height axis. Any selection cancel exits.
- **Mouse controls:**
  - Wheel up lowers terrain and wheel down raises it.
  - Right click smooths.
  - Middle click toggles the aimed face's texture in a filter list; only listed
    textures count as terrain.
- **Brushes.** B+wheel cycles ten brushes from `brushes_heightmap.cfg`: circles,
  squares, smoothing, and noise. Weights run 0–8 eighths per tick.
- **How it edits.** It crawls connected neighbours at the current grid size into
  a height lattice, paints or smooths it, and keeps neighbouring steps within one
  cell of each other.
- **Limits.** A selection clamps the affected range, and each stroke is one undo
  step. Heightmap edits are not sent in multiplayer.

## Entities

From `world.cpp` ~L272–1131 and `stdedit.cfg`.

- **Selecting and moving.**
  - Hovering an entity is an implicit selection.
  - Left click selects it and drags it in the plane of the hovered box face, free
    or grid-snapped.
  - Right click adds to a group.
  - The wheel while dragging pushes the entity along the face normal.
- **Placing.** `newent type a1..a5` places an entity at the selection face, with
  yaw seeded from the camera.
- **Editing attributes.**
  - The wheel on a selected or hovered entity edits a per-type attribute: light
    radius ±5, spotlight angle ±5, or mapmodel index ±1.
  - 1–5+wheel edit attributes 1–5 for the whole group.
  - R+wheel changes yaw by ±15°.
  - Period opens a prefilled `entset`.
- **Copying.** Copy captures the selected entities relative to the selection.

## Materials

From `iengine.h` ~L10–40 and `material.cpp` ~L138.

- **Storage.** Each cube has 16 bits of material:
  - a variant (water 1–4);
  - a volume (air, water, lava, or glass);
  - a clip kind (noclip, clip, or gameclip);
  - flags for death and alpha. Tesseract adds a "nogi" flag.
- **`editmat name [filter]`** applies to every leaf cube in the selection.
  - A volume clears the material bits and adds its implied clip (glass) or death
    (lava).
  - Clip kinds replace only the clip bits.
  - Death and alpha are independent of the rest.
  - Air clears everything.
  - Filters: empty, notempty, solid, notsolid, or a material.
- **What each one does:**
  - Noclip: collision ignores solid geometry.
  - Clip: an invisible wall for players, AI, and movable objects; shots pass
    through.
  - Glass: clip that also blocks shots.
  - Gameclip: solid for AI only.
  - Death: kills a player whose feet are inside (`physics.cpp` ~L1742).
  - Lava: liquid plus death.
  - Alpha: per-face blending (`valpha`).
- **Display.** M (`showmat`) makes volumes hoverable and draws them color-coded.

## Co-op editing

- **No server authority.** The server only relays messages; it has no world and
  validates nothing (`msgfilter`, `server.cpp` ~L1596).
- **Edits.** Each edit command is one message (`N_EDITF`: the full selection plus
  direction and mode), and every receiver replays it. The sender applies it
  locally at once.
- **Textures and entities.** Texture operations send vslot deltas. Entity edits
  send full entity state.
- **Clipboards** upload lazily (`N_CLIPBOARD`, 64 KiB packed). Larger clipboards
  become empty.
- **Undo** is per client and broadcasts the restored region, overwriting other
  players' later edits. In multiplayer it is refused beyond 10 blocks or 2,500
  cubes.
- **Single-player only:** heightmap editing and prefabs.
- **Whole-map transfer.** `/sendmap` uploads the whole map (4 MiB max), and
  `/getmap` replaces the local world. Late joiners get no edit history.

## Differences in Sauerteig

Ranked by how much they would bother a Sauerbraten builder, as of 2026-10-25:

1. **No texture alignment tools.** There is no per-face alignment, all-faces mode,
   recent-texture list, eyedropper, or replace. Sauerteig projects textures along
   the actual normal, with a tile size per material.
2. ~~Delete and Backspace don't empty a selection.~~ Fixed on 2026-10-25.
3. ~~Extrusion uses the current material instead of copying the faces behind.~~
   Fixed on 2026-10-25.
4. **Selections can't be transformed in place.** They can't slide, move along
   their normal, cut-move, flip, or rotate; R and M transform only the clipboard.
5. **Different keys, and no rebinding:**
   - Q is object mode (Sauerbraten: corner push).
   - 1–8 pick materials (Sauerbraten 1–5: entity attributes or texture
     alignment).
   - M mirrors (Sauerbraten: material display).
   - Redo is Shift+U (Sauerbraten: I).
   - Ctrl+wheel changes the grid (Sauerbraten: Ctrl is pass-through).
   - There is no Alt fine step.
6. **No heightmap brush.**
7. **Limited object editing.** Objects use click, arrow keys, and a numeric panel.
   There is no 3D drag, group selection, wheel or number-key attribute editing, or
   select-in-box.
8. **Missing volume kinds:** lava, clip, noclip, gameclip, death, and alpha, plus
   the material display and edit filters.
9. **Wheel edits wait for the server's acknowledgment, one at a time.**
   Sauerbraten applies them locally.
10. **Smaller differences:**
    - Drag-select is locked to a plane; right-click extend covers 3D.
    - There is no pass-through aiming.
    - Undo clears the selection instead of reselecting the undone region.
    - V pastes immediately instead of on release.

Where they match, or Sauerteig is better:
- Hover-only extrusion staircases in both editors.
- Both tile textures in world space.
- Sauerteig's conflict-checked undo is stricter than Sauerbraten's.
