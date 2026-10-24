# Development

## Workspace

- `crates/core`: editable geometry, chunk meshes, ray queries, collision,
  player movement, and the shared protocol. No graphics dependencies.
- `crates/client`: native and WebAssembly clients using wgpu, winit, and egui.
  Editing, local blueprints, rendering, and client prediction live here.
- `crates/server`: authoritative room simulation, edit validation and history,
  WebSocket connections, world persistence, and browser file serving.
- `crates/importer`: bounded legacy OGZ parser and command-line converter.
- `web`: browser loading and error UI, copied into `web/dist` during a build.
- `assets`: cooked textures and meshes with source/license manifests.

Run commands from the repository root. Sourcing `scripts/env.sh` selects a
project-local Rust toolchain if one exists under `.tools/`; otherwise it uses
your installed toolchain. Runtime files belong in ignored `.local/`.

### Linux dependencies

Native builds on Debian/Ubuntu need:

```sh
sudo apt-get install build-essential pkg-config libxkbcommon-dev libwayland-dev \
  libx11-dev libxi-dev libxcursor-dev libxrandr-dev libasound2-dev
```

## Build and check

```sh
. ./scripts/env.sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
cargo build --locked -p sauerteig-server -p sauerteig-client
./scripts/build-web.sh
```

Install the checks once with `rustup component add rustfmt clippy`.

The browser build requires `wasm32-unknown-unknown` and `wasm-bindgen-cli`
0.2.100, matching the pinned Rust dependency. It embeds the cooked assets and
copies licenses and provenance manifests into `web/dist`. Rebuild after changes
to client code, assets, or the browser shell. Restart the server if the browser
files did not exist when it started.

On macOS, `./scripts/build-macos-app.sh` builds the browser client and packages an
unsigned development app in `.local/Sauerteig.app`. Local hosting is linked into
the native app; browser files are bundled in its Resources directory. Both build
scripts honor `CARGO_TARGET_DIR`.

GitHub Actions checks formatting and Clippy, tests the workspace, builds native and browser
clients on Linux, and uploads the browser files. Integration tests bind ephemeral
loopback ports and use temporary world files.

## Hosting and saves

The native home screen manages named worlds and starts a server for the selected
world on an available port. Local hosting binds loopback unless the LAN checkbox
is selected. **Main menu** releases gameplay input and keeps the current session
running; **Resume** returns to it. Choosing another world or server drains pending
edits and local writes, closes the old connection, and waits for a hosted world's
final save before opening the destination. Closing the window follows the same
save path. A sync or save error blocks the switch and remains visible. A world lock
prevents two app instances from hosting or duplicating the same managed save.

Managed saves live under the application-data directory in `worlds/<id>/`, with
separate display metadata and an opaque `world.json`. Import and duplicate copy
the source; they never move or overwrite it. The home screen remembers player
name, server address, and last world. Explicit `--server` launches still join
directly, for example `cargo run -p sauerteig-client -- --server localhost:3000`.

The standalone server remains available for dedicated hosting:

| Server option | Default | Environment variable |
| --- | --- | --- |
| `--bind IP:port` | `127.0.0.1:3000` | `SAUERTEIG_BIND` |
| `--world PATH` | `.local/world.json` | `SAUERTEIG_WORLD` |
| `--web-dir DIR` | `web/dist` | `SAUERTEIG_WEB_DIR` |
| `--no-edit` | Editing allowed | — |

World changes save after a short delay, on explicit save, and on graceful
shutdown, using buffered writes and an atomic file replacement. Leaving a session
waits for queued edits, an ordered server acknowledgment, and local document writes.
An existing file is loaded at startup; an unused path starts a fresh
arena. Stop the server before replacing its save file.

For a trusted LAN session:

```sh
cargo run -p sauerteig-server -- --bind 0.0.0.0:3000 --world .local/session.json
cargo run -p sauerteig-client -- --name Player --server ws://SERVER:3000/ws
```

Browser WebGPU requires a secure context. Localhost works over HTTP; other hosts
need HTTPS, with `/ws` proxied as a secure WebSocket. The browser accepts `name`
and `server` query parameters; an alternate server URL must include `/ws`.
Names are display labels, with no account authentication.

Shared geometry, material volumes, and objects live in the server's JSON save. Blueprints and
settings are client-local: IndexedDB for the browser's site origin, or the OS
application-data folder on desktop. Remembered browser session choices use
localStorage for that origin. On macOS the application-data folder is
`~/Library/Application Support/Sauerteig`; `SAUERTEIG_DATA_DIR` overrides it.

## Rendering and assets

Rendering meshes are derived from the editable geometry. Imported large leaves
remain compact and use a spatial query index. Local metre-root edits mask the
authored base without expanding the whole map. An edit rebuilds affected mesh
batches, local chunks, and their face neighbors; raycasts and collision use the
same cell shapes. Camera and shadow passes cull using actual mesh bounds.
The [core contract](../crates/core/CONTRACT.md) describes the geometry and protocol
invariants.

Water and glass occupy a separate sparse layer of disjoint eighth-metre boxes.
Volume edits rebuild cached boundary meshes; contiguous regions of the same
material omit internal faces. Transparent faces are culled and sorted behind
opaque geometry using the same depth buffer. This is alpha blending, with
procedural water motion and glass tint rather than scene refraction or reflected
scene captures. Material regions do not cast opaque shadows. Keep intersecting
transparent regions and large visible water surfaces in runtime checks: sorting
uses face centers and can show ordering artifacts for overlapping views of large
faces. An underwater eye adds blue-green distance fog and a subtle screen tint.

Lighting is dynamic. The sun shadow window follows the player with texel-snapped
motion; eight nearby light nodes cast shadows with 512 px maps and light-volume
culling. Nodes have no visible model in play; their editor guides do not cast
shadows. Lamp props are independent decoration, with emissive surfaces but no
automatic local light. The current node renderer is still a fixed warm downward
spotlight with a 12 metre range, not an omnidirectional point light. Color, radius,
intensity, and cone settings are not editable or restored from imported maps.
Ambient fill keeps unlit areas
readable; there is no baked map lighting or global illumination. Guides, tracers,
and the first-person weapon do not cast shadows.

Terrain uses world-space texture coordinates. Materials share albedo/AO and
normal/roughness texture arrays with linear-light color mipmaps and renormalized
normals. Repeated props use GPU instancing, near/distant meshes, and distance and
light-range culling. Sources and conversion commands live with the
[materials](../assets/materials/README.md) and [props](../assets/props/README.md).

## Performance checks

The target is recent ordinary laptops, including integrated graphics. Use a
representative indoor/outdoor map in both browser and native builds. Include two
fighters and a builder changing geometry during combat; check save/rejoin and
frame-time spikes as well as average frame rate.

For repeatable CPU measurements:

```sh
cargo run --release -p sauerteig-core --example profile_world
cargo run --release -p sauerteig-core --example profile_fine
cargo run --release -p sauerteig-import --example profile_ogz -- path/to/map.ogz
```

These report meshing, edit, movement, and ray-query timings. The OGZ profile also
reports original-scale storage, load time, query percentiles, and dirty remeshing.
They exclude GPU
execution and networking. F3 shows rendering counts and CPU edit/remesh timing,
including GPU upload submission; it does not measure GPU completion or latency.
