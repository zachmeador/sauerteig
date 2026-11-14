# Editor controls

Click the arena to capture the mouse. Press **Esc** to release it.

| Action | Control |
| --- | --- |
| Move / look | WASD / mouse |
| Jump in combat | Space |
| Switch your own fight / edit mode | E |
| Fire in combat | Left mouse |
| Choose hitscan / rocket weapon in combat | 1 / 2 |
| Fly along the view direction in edit mode | WASD |
| Fly vertically up / down in edit mode | Page Up / Page Down |
| Select a persistent face region | Left click and drag |
| Extend / reorient the selection | Right click, or left click while holding the left Shift key |
| Add / subtract a selection volume | Alt+left drag / Alt+Shift+left drag |
| Selection mode and numeric volume size | Esc → Selection |
| Assign normal space, water, or glass | Esc → Selection → Volume material → Apply |
| Clear the selection in edit mode | Space |
| Extrude / carve the selected or aimed face | Wheel down / up |
| Empty the selection, or the aimed cell without one | Delete / Backspace |
| Make the grid smaller / larger | G+wheel down / up; [ / ] also work |
| Select one corner on the active face | Middle mouse |
| Select one edge on the active face | Shift+middle mouse |
| Shape the selected corner | Wheel |
| Shape all face corners without a corner/edge selection | F+wheel |
| Cycle and apply material | Y+wheel |
| Choose material in edit mode | 1–8 |
| Browse materials and props | T, or Esc → Assets |
| Undo your latest geometry, volume, or object edit | U or Z |
| Redo your latest undone edit | Shift+U or Shift+Z |
| Copy / paste selected volumes | C / V |
| Save, organize, or load blueprints | B, or Esc → Blueprints |
| Settings, including scroll inversion | F2, or Esc → Settings |
| Rotate the clipboard 90° around Y | R |
| Mirror the clipboard along X / Z | M / Shift+M |
| Place selected prop / light / spawn | P / L / O |
| Remove the selected or aimed object in object mode | Delete / Backspace |
| Switch geometry / object selection | Q |
| Show controls / performance stats | F1 / F3 |
| Return to world picker without disconnecting | Esc → Main menu |
| Save the shared world on the server | Ctrl+S or Cmd+S |

A selection stays active after releasing the mouse. Editing commands apply to
that selection; without one, they apply to the face under the crosshair. Clicks
select geometry rather than adding or deleting it. Wheel down pulls geometry
outward; wheel up pushes it inward. Settings can invert all editor wheel controls.
Pulling copies the six face materials of the geometry behind the face, so a wall
painted with several materials keeps its pattern; where nothing solid is behind, it
uses the chosen material. Delete or Backspace empties every volume of the selection,
or the aimed cell when nothing is selected, as one undoable edit. A selected object,
such as one just placed, is removed first unless geometry has been selected since.
The grid ranges from 1/8 metre to 8 metres. The Escape selection panel also offers
Replace/Add/Subtract modes and numeric X/Y/Z sizes measured in the current grid.
Fine carving preserves the original shape of existing sloped and warped surfaces.

Materials 1–8 are **Concrete**, **Gunmetal**, **Rusted Steel**, **Worn Concrete**,
**Hazard Paint**, **Amber Light**, **Rock**, and **Earth**. In edit mode the number
keys choose them; in combat 1 and 2 choose the weapon instead. Use Y+wheel to cycle
and apply them, or T to browse thumbnails and paint a persistent selection. Painting
changes the selection's active face direction; other faces keep their materials.
Floors, ceilings, and each wall direction can have independent surfaces. Texture
scale uses world coordinates, so subdividing a surface does not shrink its pattern.

T also chooses a wall lamp, hanging lamp, utility box, pipe, rock, or a procedural
tree. P places the chosen prop. Wall lamps align to walls; hanging lamps mount to
ceilings, or stand two metres above an aimed floor. Lamp models are decoration:
place a separate light node with L where illumination is needed. Light nodes have
small editor-only markers and no physical fixture in play. Each is a warm point
light with a 12 metre range that casts shadows from the surrounding geometry and
from players near it; their color, range, and brightness are fixed until object
properties exist. Props
are decorative: use cube geometry for collision underneath large barriers.
Older Sauerteig worlds that relied on a lamp prop alone for illumination need a
separate light node beside it.

Press **Q** in edit mode to select objects. Click a prop, light marker, or spawn to select
it; the object panel exposes position and yaw. Arrow keys move it by the current
grid step, Shift+Up/Down moves vertically, R rotates 90°, H moves it to the aimed
surface, and Insert duplicates it. Delete or Backspace removes it, in geometry mode
too unless geometry has been selected since. These actions share undo and redo with
geometry.
Release the mouse with Esc to use the numeric controls.

## Volumes and blueprints

**Volume materials** describe the space between solid surfaces. Select a region,
open **Esc → Selection**, choose **Water** or **Glass**, and press **Apply**.
**Normal space** clears the material assignment. This leaves solid geometry in
place: carve a pool or opening first, then fill the space. The entire additive
selection is affected, including areas with no solid cubes. Glass is transparent
but blocks players and shots. Water is non-solid, slows movement and falling, and
Space swims upward. These are static regions, not flowing fluid simulations.

Water and glass surfaces can be selected like geometry. Turn off **Select water
and glass surfaces** in the selection panel to reach geometry behind them.
Wheel operations and Delete still edit solid geometry; use the volume material
control to add or remove water and glass. Surface textures and volume materials are
separate.

Selections can contain several volumes: select the main building, then add its
annex with Alt+drag. Overlaps count once. Subtract volumes with Alt+Shift+drag.
Copy captures geometry, empty space, material volumes, and objects whose pivots lie inside that
union. Paste replaces only
those volumes, leaving the gaps between them alone. Blue outlines preview the
clipboard at the active selection's minimum corner; V commits the placement.
R and M transform geometry, face materials, volume materials, the selection mask, and object
positions/facing. Object meshes retain their original shape when mirrored.
Pasted objects receive new identities; existing objects at the destination stay.

Press B to save a selection as a named blueprint. The library supports folders,
search, renaming, moving, duplication, deletion, and loading back into the
clipboard for placement. Blueprints include props, lights, and spawns together
with geometry and volume materials, so a building keeps its fixtures and windows.
Older blueprints have normal space throughout their selection. Pasting replaces
the destination's material assignments, including clearing regions that are normal
space in the blueprint. Older geometry-only blueprints
still load. Libraries and settings are local to
each client, separate from the shared map. They live in the OS application data
folder (on macOS, `~/Library/Application Support/Sauerteig`);
`SAUERTEIG_DATA_DIR` overrides it.

## Shared editing

Selections are sparse boxes rather than lists of empty tiny cubes. Each edit is
bounded to 16,384 changed one-metre roots and 65,536 geometry fragments, with a
16 MiB network message ceiling. A very large or intricate operation can still
exceed those limits. Undo history is bounded by both operation count and stored
geometry complexity. Material volumes use separate compact boxes, limited to
16,384 per world; their size does not expand into one-metre geometry edits.

Changes are shared immediately. Personal undo is rejected if someone has
subsequently changed the same cells or objects. Volume materials currently share
one conflict/undo ownership clock for the whole layer: another player's volume
edit can block your stale edit or undo even at a different location. A combined paste is one atomic
edit and one undo entry. Redo checks ownership too, including edits
that restored an identical value. A new edit clears your redo history. Adding
geometry through an active fighter is also rejected.
