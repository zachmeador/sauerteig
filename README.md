# Sauerteig

A multiplayer FPS and cooperative world-building game inspired by Quake,
Sauerbraten, and Tesseract. Press **E** to switch between fighting and building
while everyone else keeps playing in the same arena.

The goal is fast arena combat, expressive cube editing, modern live lighting,
and easy multiplayer from a browser or a native desktop app. The first Rust
prototype explores that shared loop; its visual quality, movement feel, and
performance still need playtesting on ordinary laptops with integrated graphics.

## Run locally

Install a current stable [Rust toolchain](https://rustup.rs/). Run the following
commands from this repository. If a project-local toolchain exists in `.tools/`,
`scripts/env.sh` selects it; otherwise it leaves your normal toolchain in place.

Linux native builds also need windowing and audio development libraries. On
Debian/Ubuntu:

```sh
sudo apt-get install build-essential pkg-config libxkbcommon-dev libwayland-dev \
  libx11-dev libxi-dev libxcursor-dev libxrandr-dev libasound2-dev
```

Start the shared server in one terminal:

```sh
. ./scripts/env.sh
cargo run -p sauerteig-server
```

Start a native client in another terminal:

```sh
. ./scripts/env.sh
cargo run -p sauerteig-client
```

Run additional clients to join the same room. The default server listens on
`127.0.0.1:3000`; the native client connects to `ws://127.0.0.1:3000/ws`.
To choose your display name or connect to another server:

```sh
cargo run -p sauerteig-client -- --name Player --server ws://127.0.0.1:3000/ws
```

On macOS, package the native client as an app:

```sh
./scripts/build-macos-app.sh
```

This produces `.local/Sauerteig.app`, an unsigned local development build with
high-resolution display support. Open it from Finder after starting the server;
the server still runs separately. Packaging does not install anything in your
Applications folder.

For the browser client, install the matching build tools once:

```sh
. ./scripts/env.sh
rustup target add wasm32-unknown-unknown
cargo install wasm-bindgen-cli --version 0.2.100 --locked
```

Then build the browser files:

```sh
./scripts/build-web.sh
```

Start or restart the server after the first browser build, then open
**http://localhost:3000** in a browser with WebGPU and hardware acceleration
enabled. The server serves `web/dist` and accepts WebSocket connections at
`/ws`. Native and browser clients join the same room. Rebuild the browser files
after changing the Rust client or the browser shell.

The browser accepts optional `name` and `server` query parameters, for example
`http://localhost:3000/?name=Player`. Names are display labels, not authenticated
accounts. The default display name starts with `Player`. An alternate server
URL must include its `/ws` path.

WebGPU requires a secure context: localhost works over HTTP, but another
computer's LAN address requires HTTPS. An HTTPS page also needs a secure
WebSocket (`wss://`) server. Native clients do not have that browser restriction.

### Server options

| Option | Default | Purpose |
| --- | --- | --- |
| `--bind IP:port` | `127.0.0.1:3000` | Address to listen on |
| `--world PATH` | `.local/world.json` | Shared world save file |
| `--web-dir DIR` | `web/dist` | Built browser files |
| `--no-edit` | Editing allowed | Run a combat-only room |

The first three options also accept `SAUERTEIG_BIND`, `SAUERTEIG_WORLD`, and
`SAUERTEIG_WEB_DIR` environment variables. For a trusted LAN session, use
`cargo run -p sauerteig-server -- --bind 0.0.0.0:3000`; native clients can connect
directly, while browser clients still need HTTPS away from localhost. World
changes are saved automatically after a short delay, on explicit save, and on
graceful shutdown. The server loads an existing save at startup.

A new save starts with an industrial arena. Existing saves keep their geometry.
If `.local/world.json` contains the earlier courtyard, stop that server and use
an unused save path to load the new starter layout:

```sh
cargo run -p sauerteig-server -- --world .local/foundry.json
```

The preview session uses `.local/foundry.json`. If that file already exists, it
is loaded too; choose another unused filename to start fresh. No existing save
is deleted or overwritten just to change the starter arena.

## Controls

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
| Place selected prop / prototype light / spawn | P / L / O |
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

T also chooses a wall lamp, hanging lamp, utility box, pipe, rock, or the earlier
prototype tree. P places the chosen prop. Wall lamps align to walls; hanging
lamps mount to ceilings, or stand two metres above an aimed floor. Both lamps
join the dynamic lighting budget. Props are decorative: use cube geometry for
collision underneath large barriers. Props do not yet belong to blueprints or
geometry undo history.

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

Selections are sparse boxes rather than lists of empty tiny cubes. Each edit is
bounded to 16,384 changed one-metre roots and 65,536 geometry fragments, with a
16 MiB network message ceiling. A very large or intricate operation can still
exceed those limits. Undo history is bounded by both operation count and stored
geometry complexity.

Changes are shared immediately. Personal undo is rejected if someone has
subsequently changed the same cells. Redo checks ownership too, including edits
that restored an identical value. A new edit clears your redo history. Adding
geometry through an active fighter is also rejected.

## How it fits together

- `crates/core` owns world cells, chunk meshes, geometry queries, player movement,
  and the shared message protocol. Both server and clients use it.
- `crates/server` runs the shared simulation and validates movement inputs,
  combat, edits, and saves. It also serves the browser build.
- `crates/importer` inspects legacy Cube 2 OGZ maps and converts explicitly selected
  regions; see [the importer guide](crates/importer/README.md) for compatibility limits.
- `crates/client` uses Rust, wgpu, winit, and egui for the native and WebAssembly
  clients. wgpu provides desktop GPU backends and browser WebGPU support.
- `web` contains the browser loading/error screen. `scripts/build-web.sh`
  compiles the client and generates its WebAssembly bindings in `web/dist`.

The server owns the world and combat outcomes. Rendering meshes are derived
from the editable geometry; changed chunks and their neighbors are rebuilt
after an edit. The same cell shapes drive rendering, aiming, and collision.

Lighting is evaluated at runtime, with no baked lightmaps. The sun and the
eight nearest industrial fixtures cast shadows that update every frame as
geometry, props, and other players change. Fixtures currently emit broad
downward beams; walls and ceilings block them. Each fixture uses a 512×512
shadow map and skips chunks outside its light volume. Editor guides, tracers,
and the first-person weapon do not cast shadows. A small ambient fill keeps
unlit areas readable; this is not bounced lighting or global illumination.

## Materials and props

The client embeds eight 512px material sets and five CC0 props. Color/AO and
normal/roughness textures use shared arrays with linear-light color mipmaps,
renormalized normal mipmaps, and anisotropic filtering. World and prop shaders
use explicit material IDs. Existing six material slots retain their identities.
Repeated props use GPU instancing, near/distant meshes, and local-light range
culling. The five near meshes range from 648 to 3,486 triangles.

All fifteen material layers use **40.0 MiB of texture storage including mipmaps**,
excluding geometry, shadows, framebuffers and driver overhead. Cooked PNGs and
prop meshes total about **14.1 MiB on disk**. These are allocation/file counts,
not measured GPU performance or an ordinary-laptop FPS result.

Sources, authors, licenses, modifications and hashes are kept in
[the material manifest](assets/materials/manifest.json) and
[the prop manifest](assets/props/manifest.json). Ordinary builds embed the cooked
files without network requests. Optional conversion tools are documented in
[the prop guide](assets/props/README.md); `scripts/cook-materials.py --offline`
rebuilds terrain textures from its local source cache. Code remains MIT;
these assets retain CC0. The larger [research shortlist](research/assets.md)
contains candidates beyond this shipped set.

## Prototype boundaries

This is an initial implementation of the loop, with deliberate limits:

- Geometry uses sparse one-metre roots with local subdivision down to 1/8 metre.
  Only refined roots store clipped fragments; this is not a fully adaptive octree
  with arbitrary world size. Existing one-metre saves remain readable.
- Deformation moves corners and edges on all six faces. The same geometry drives
  rendering, rays, and collision; invalid folded or pinched shapes are rejected.
  Existing legacy pinches retain their original representation. Clipping a warped
  source preserves it exactly, but a clipped shape that cannot be represented by
  one editable eight-vertex cell cannot itself be corner-deformed.
- One shared room supports up to 16 players. WebSocket networking establishes
  multiplayer behavior; it does not establish acceptable FPS responsiveness
  under WAN latency or packet loss.
- Trees remain procedural placeholders. The five imported props are decorative,
  with selection bounds but no gameplay collision proxies yet.
- Names are not accounts. Public hosting, moderation, and access control need
  further work before treating this as a public game service.
- Rendering and edit performance have not yet been validated against a measured
  integrated-GPU budget. A successful build is not that validation.

The intended art direction is moderately detailed, coherent, and efficient,
with textured architecture and convincing foliage rather than a toy-like
low-poly style. The [starter asset research](research/assets.md) contains a
wider shortlist, including two tree conversions that still need preparation.

## Development checks

```sh
. ./scripts/env.sh
cargo fmt --all -- --check
cargo test --workspace --locked
cargo build --locked -p sauerteig-server -p sauerteig-client
./scripts/build-web.sh
```

Local verification on October 24, 2026: **135 tests pass** across the client,
core, server, and importer. Native macOS and WebAssembly release builds succeed.

Browser checks cover additive selection, volume sizing, saving and renaming a
blueprint, folders, reload persistence, loading and placing a blueprint, undo,
and eighth-metre editing. Scroll inversion persists after reload. Browser text
entry now uses a DOM text bridge for bulk input, Unicode composition, paste, and
Ctrl/Cmd shortcuts; full names and pasted folder paths were checked interactively.
Desktop checks also confirmed clipboard text entry and settings persistence after
restarting the app. The embedded browser uses the right-drag fallback when it
refuses pointer lock.

Automated coverage includes exact clipping of warped and legacy pinched sources,
fine-grid collision and cap surfaces, all-face corner/edge shaping, mask holes
versus selected empty space, transform round trips, large sparse selections,
blueprint storage, and coalescing tiny solid fills without changing their shape.
A three-client WebSocket test pastes 1,024 sparse roots while a fighter moves and
fires, shares undo/redo, checks conflicts and overlap rejection, and reloads the
exact fine geometry after a server restart. Server tests also exercise the
16,384-root edit limit and bounded history. The live map was backed up and its
geometry and objects preserved when upgrading the server.

Current material/prop checks verified the picker and rendering in both the
browser and native app, plus painting and placing a prop in a separate browser
test room. The complete converted `test_weapon.ogz` was loaded in the browser.
A second three-client test injects 80 ms delay in each direction while fighters
fire and a builder changes new materials/props; clients converge, damage and
respawn continue, and a fresh-server rejoin preserves the saved result.

The legacy importer and its exact compatibility/appearance limits are documented
in [crates/importer/README.md](crates/importer/README.md). The supplied archives
stay local and are excluded from Git.

These checks do not establish a measured graphics budget across integrated GPUs.
Normal browser pointer lock, WAN behavior, and broader hardware playtesting
remain open.

GitHub Actions runs these checks on Linux and uploads the generated browser
files. The initial commit passed its hosted workflow; current local changes
need their own hosted run after publication. Interactive validation should include two fighters and one builder in
a mixed native/browser session: overlap edits, undo, reshape a route during a
fight, save, and rejoin. Measure frame times and editing stalls as well as
network behavior.

For a repeatable CPU sanity check of the starter arena:

```sh
cargo run --release -p sauerteig-core --example profile_world
```

This reports cell/triangle counts and measured p50/p95 times for full meshing,
one-cell edits with dirty-chunk remeshing, 512-cell shaped replacements,
16-player movement ticks, and world
raycasts. Results describe the machine running the command; they exclude GPU
rendering, browser execution, and networking and do not establish an FPS budget.
The `profile_fine` example additionally measures fine carving, a 4,096-root
thin-floor paste/removal, and rays through the resulting detailed geometry.

F3 also reports the last and peak CPU time spent applying a live edit and
rebuilding its chunk meshes, including GPU upload submission. It does not
measure GPU completion or network latency.

## License

Sauerteig's original code and documentation are licensed under the
[MIT License](LICENSE). Dependencies and any future third-party assets retain
their own licenses.
