# Editor controls

Click the arena to capture the mouse. Press **Esc** to release it. In an embedded
browser that refuses mouse lock, hold **right mouse to look** and use
**Shift+left click** to extend a selection instead of right click.

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
| Extend / reorient the selection | Right click; Shift+left click in drag-look mode |
| Add / subtract a selection volume | Alt+left drag / Alt+Shift+left drag |
| Selection mode and numeric volume size | Esc → Selection |
| Clear the selection in edit mode | Space |
| Extrude / carve the selected or aimed face | Wheel down / up |
| Make the grid smaller / larger | G+wheel down / up; [ / ] also work |
| Select one corner on the active face | Middle mouse |
| Select one edge on the active face | Shift+middle mouse |
| Shape the selected corner | Wheel |
| Shape all face corners without a corner/edge selection | F+wheel |
| Cycle and apply material | Y+wheel |
| Choose material | 1–8 |
| Browse materials and props | T, or Esc → Assets |
| Undo your latest geometry edit | U or Z |
| Redo your latest undone geometry edit | Shift+U or Shift+Z |
| Copy / paste selected volumes | C / V |
| Save, organize, or load blueprints | B, or Esc → Blueprints |
| Settings, including scroll inversion | F2, or Esc → Settings |
| Rotate the clipboard 90° around Y | R |
| Mirror the clipboard along X / Z | M / Shift+M |
| Place selected prop / light / spawn | P / L / O |
| Remove an aimed object | Delete / Backspace |
| Show controls / performance stats | F1 / F3 |
| Save the shared world on the server | Ctrl+S or Cmd+S |

A selection stays active after releasing the mouse. Editing commands apply to
that selection; without one, they apply to the face under the crosshair. Clicks
select geometry rather than adding or deleting it. Wheel down pulls geometry
outward; wheel up pushes it inward. Settings can invert all editor wheel controls.
The grid ranges from 1/8 metre to 8 metres. The Escape selection panel also offers
Replace/Add/Subtract modes and numeric X/Y/Z sizes measured in the current grid.
Fine carving preserves the original shape of existing sloped and warped surfaces.

Materials 1–8 are **Concrete**, **Gunmetal**, **Rusted Steel**, **Worn Concrete**,
**Hazard Paint**, **Amber Light**, **Rock**, and **Earth**. Use Y+wheel to cycle and
apply them, or T to browse thumbnails and paint a persistent selection. Texture
scale uses world coordinates, so subdividing a surface does not shrink its pattern.

T also chooses a wall lamp, hanging lamp, utility box, pipe, rock, or a procedural tree. P places the chosen prop. Wall lamps align to walls; hanging
lamps mount to ceilings, or stand two metres above an aimed floor. Both lamps
join the dynamic lighting budget. Props are decorative: use cube geometry for
collision underneath large barriers. Props do not yet belong to blueprints or
geometry undo history.

## Volumes and blueprints

Selections can contain several volumes: select the main building, then add its
annex with Alt+drag. Overlaps count once. Subtract volumes with Alt+Shift+drag.
Copy captures geometry and empty space inside that union. Paste replaces only
those volumes, leaving the gaps between them alone. Blue outlines preview the
clipboard at the active selection's minimum corner; V commits the placement.
R and M transform both the geometry and its selection mask.

Press B to save a selection as a named blueprint. The library supports folders,
search, renaming, moving, duplication, deletion, and loading back into the
clipboard for placement. Blueprints currently contain geometry only; trees,
lights, and other objects are not captured. Libraries and settings are local to
each client, separate from the shared map. Browser data lives in IndexedDB for
that site origin. Desktop data uses the OS application data folder (on macOS,
`~/Library/Application Support/Sauerteig`); `SAUERTEIG_DATA_DIR` overrides it.

## Shared editing

Selections are sparse boxes rather than lists of empty tiny cubes. Each edit is
bounded to 16,384 changed one-metre roots and 65,536 geometry fragments, with a
16 MiB network message ceiling. A very large or intricate operation can still
exceed those limits. Undo history is bounded by both operation count and stored
geometry complexity.

Changes are shared immediately. Personal undo is rejected if someone has
subsequently changed the same cells. Redo checks ownership too, including edits
that restored an identical value. A new edit clears your redo history. Adding
geometry through an active fighter is also rejected.
