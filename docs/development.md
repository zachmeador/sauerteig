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

The server runs `Session` on a dedicated 60 Hz thread. Network tasks use typed
commands; they do not lock the simulation. One ordered edit worker validates,
clips, canonicalizes, and compiles replacements on a structurally shared world
view. The simulation checks current conflicts, budgets, and fighter overlap
before committing. Movement frames continue while that player's edit prepares;
mode changes remain barriers and save/sync requests wait for earlier edits.
Deferred commands are bounded to 32 MiB per peer. Save and join workers expand and serialize cheap authored
snapshots. Retired edit geometry, and the undo history of a peer that leaves, is
also freed on the worker. Commands a client sent before its socket closed still
apply, in order, even without a `Sync`; only their results have nowhere to go.

Reliable peer queues have a conservative 128 MiB payload budget, including the
packet being written. Before an edit commits, every queue must have room for the
exact patch it will broadcast, sized by the same function as the packet, plus
64 KiB for the result that follows. Otherwise the commit waits in the edit
worker, which retries for two seconds and then answers that edit with a "busy"
error and moves on, so one lagging peer never holds up the others' edits. The
simulation continues and the joining peer keeps its revision backlog.
Movement snapshots are replaceable. A world transfer times out only after 30
seconds without a successful TCP write, including transfers longer than a minute.
Server sockets enable TCP_NODELAY. The native client's locked blocking
`tungstenite` connector already enables it through `ewebsock`.

A simulation panic closes the room with an error, returns that error to the
embedded host, and skips its final save. A pending autosave checks for failure
before replacing the saved file. The home screen's existing host-error path
reports the failure; reopening uses the last successful save.

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

The scene renders in linear HDR, with a filmic exposure/tonemapping pass before
egui. Settings include bloom (zero disables it), anti-aliasing, render scale
(50–100%), exposure, and vsync. Anti-aliasing defaults to Auto: 4× MSAA where
supported while the scene has at most 2560×1440 pixels, and none above that,
because 4× MSAA on a 4K HDR scene measured over a 144 Hz frame budget on an
M3 Pro. On and Off override it. Render scale affects only the scene; egui
stays at output resolution. Vsync off selects Mailbox, then Immediate if
available, otherwise Fifo. The surface requests one frame of latency, clamped by
the backend. Local movement and remote player poses interpolate between 60 Hz
steps; mouse look remains immediate.

Lighting is dynamic. The sun shadow window follows the player in six-metre,
texel-aligned light-space steps; eight nearby light nodes cast shadows with 512 px
maps and light-volume culling. Static shadow geometry is cached until geometry,
light position or the sun window changes. Actors draw over fresh copies of those
caches each frame. Nodes have no visible model in play; their guides do not cast
shadows. Lamp props are independent decoration, with emissive surfaces but no
automatic local light. The current node renderer is still a fixed warm downward
spotlight with a 12 metre range, not an omnidirectional point light. Color, radius,
intensity, and cone settings are not editable or restored from imported maps.
Ambient fill keeps unlit areas
readable; there is no baked map lighting or global illumination. Guides, tracers,
and the first-person weapon do not cast shadows.

Terrain uses world-space texture coordinates. Materials share albedo/AO and
normal/roughness texture arrays with linear-light color mipmaps and renormalized
normals. Opaque terrain culls back faces except when the eye touches or enters
terrain; that fallback preserves interior faces during editor flight. Props,
procedural meshes, actors, guides, and transparent volumes remain two-sided.
Repeated props use GPU instancing, near/distant meshes, and distance and
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
They exclude GPU execution and networking. F3 shows rendering counts, CPU
edit/remesh timing, effective scene resolution/MSAA/present mode, static shadow
updates and per-pass GPU timestamps where supported. It explicitly reports
unavailable timestamps otherwise. The asynchronous profiler never waits for a
sample during play. Pass spans can overlap on Metal, so do not add them to obtain
a GPU frame time. Browser timestamps can also be quantized.

For fixed native renderer comparisons, source `scripts/env.sh`, then run:

```sh
cargo run --release --locked -p sauerteig-client --bin capture -- \
  --poses scripts/capture-poses.json --output .local/captures/review \
  --width 3840 --height 2160 --frames 60 --warmup 10
```

Each pose names a world file (or `arena`/`materials` test scene), eye, look-at
point, fixed animation time and optional guides. The supplied imported poses
expect a **copy** of the library's `the_cube1m` at `.local/the_cube1m.json`. Supply
your own pose manifest for other worlds. The tool refuses to replace existing
PNGs. `--settings FILE` accepts a JSON object with `scale`, `msaa`
(`"auto"`, `"on"` or `"off"`), `bloom`, `exposure` and `vsync`; omitted fields
take the normal defaults. Measurements include CPU submission, completed-frame
median/p95 and individual GPU pass medians. They exclude simulation,
presentation and PNG readback; they do not prove perceived smoothness on a
monitor.

`scripts/profile-browser.mjs URL OUTPUT_PREFIX` records five seconds of the
running browser client's timestamps and frame intervals at a verified 3840×2160,
plus a screenshot and validation errors. Use Playwright's isolated Chromium;
`PLAYWRIGHT_MODULE` can point to a temporary installation's `index.mjs`. Restart
the test server for each comparison so the assigned player/spawn is identical.
The native GPU cache regression can be run explicitly with
`cargo test -p sauerteig-client --lib gpu_cached_shadows -- --ignored` on a host
with GPU access. Normal workspace tests leave that hardware-dependent test
ignored.

To compare the built-in movement profiles with a literal port of Sauerbraten's
velocity math, run the example below. It prints top speeds, time to 90% of top
speed, jump apex and air time, and stopping distance at the fixed 60 Hz tick; the
core tests assert that Sauerbraten agrees with the reference within 3%.

```sh
cargo run -p sauerteig-core --example movement_profiles
```

The server's `/health` endpoint publishes player count, revision, tick, and
`timing`: p50/p99/max active tick work, scheduler lateness, the largest tick's
main cause, and per-operation timings (including edit checks/apply/publication).
Active tick work includes commands handled since the preceding tick plus that
physics step; it excludes time asleep. Percentiles use the most recent 36,000
samples per category; maxima and counts cover the process lifetime. Statistics
refresh once a second and appear in server logs every ten seconds. These are
wall-clock durations, so OS descheduling can contribute to an observed spike.

Scripted WebSocket clients in `crates/server/src/bots.rs` drive the acceptance
tests that check ticks keep running through saves, joins, and large edits.
