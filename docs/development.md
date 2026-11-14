# Development

## Workspace

- `crates/core`: editable geometry, chunk meshes, ray queries, collision,
  player movement, and the shared protocol. No graphics dependencies.
- `crates/client`: the desktop client, using wgpu, winit, and egui. Editing,
  local blueprints, rendering, and client prediction live here.
- `crates/server`: authoritative room simulation, edit validation and history,
  WebSocket connections, and world persistence.
- `crates/importer`: bounded legacy OGZ parser and command-line converter.
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
```

Install the checks once with `rustup component add rustfmt clippy`.

On macOS, `./scripts/build-macos-app.sh` packages an unsigned development app in
`.local/Sauerteig.app`. Local hosting is linked into the app. The script honors
`CARGO_TARGET_DIR`.

GitHub Actions checks formatting and Clippy, tests the workspace, and builds the
server and native client on Linux. Integration tests bind ephemeral loopback
ports and use temporary world files.

## Hosting and saves

The native home screen manages named worlds and starts a server for the selected
world on an available port. Local hosting binds loopback unless the LAN checkbox
is selected. **Main menu** releases gameplay input and keeps the current session
running; **Resume** returns to it. Choosing another world or server drains pending
edits and local writes, closes the old connection, and waits for a hosted world's
final save before opening the destination. Every way of quitting follows the same
save path: closing the window, including a minimized one, and the application's
own Quit, such as Cmd+Q on macOS, which waits in winit's `exiting` handler. The
per-frame updates (the host, the game's network and simulation, and a session
change) run in a tick that `about_to_wait` drives, once per drawn frame or every
50 ms while the window is minimized, covered or otherwise not redrawn, so a
change finishes without drawing. A sync or save error blocks the switch and
remains visible. A world lock prevents two app instances from hosting or
duplicating the same managed save.

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
| `--no-edit` | Editing allowed | — |

World changes save after 750 ms without edits, and at the latest 9 s after the
first unsaved change however long editing continues (the save worker polls once
a second, which keeps that under 10 s). They also save on explicit save and on
graceful shutdown, which writes only if something changed since the last save.
A world with no file yet is saved when the server starts. Writes are buffered
into a temporary file that is synced, then renamed over the world, and the
rename is made durable: macOS and Linux sync the world's folder, and Windows
renames with `MoveFileExW` and `MOVEFILE_WRITE_THROUGH`. Leaving a session
waits for queued edits, an ordered server acknowledgment, and local document writes.
An existing file is loaded at startup; an unused path starts a fresh
arena. Stop the server before replacing its save file.

The server runs `Session` on a dedicated 60 Hz thread. Network tasks use typed
commands; they do not lock the simulation. One ordered edit worker validates,
clips, canonicalizes, and compiles replacements on a structurally shared world
view. The simulation checks current conflicts, budgets, and fighter overlap
before committing. Movement frames continue while that player's edit prepares;
mode changes remain barriers and save/sync requests wait for earlier edits.
Deferred commands are bounded to 32 MiB per peer. Save and join workers expand
and serialize cheap authored snapshots. Retired edit geometry, and the undo
history of a peer that leaves, is also freed on the worker. Commands a client
sent before its socket closed still apply, in order, even without a `Sync`;
only their results have nowhere to go.

Reliable peer queues have a conservative 128 MiB payload budget, including the
packet being written. Before an edit commits, every queue must have room for the
exact patch it will broadcast, sized by the same function as the packet, plus
64 KiB for the result that follows. Otherwise the commit waits in the edit
worker, which retries for two seconds and then answers that edit with a "busy"
error and moves on. The room has one edit worker, so while a lagging peer's
queue lacks room, every builder's edits queue behind the waiting one, and each
that still doesn't fit after two seconds fails as busy. The simulation continues
and the joining peer keeps its revision backlog.
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

Opening `http://SERVER:3000/` in a web browser shows one line saying the server
is running, a quick way to check that the port is reachable. Game clients
connect to `/ws`. Names are display labels, with no account authentication.

Shared geometry, material volumes, and objects live in the server's JSON save.
Blueprints, settings, and the home screen's remembered choices are client-local,
in the OS application-data folder, and the home screen's choices save through
the same storage worker as the other documents. A failure to start, such as a
graphics adapter the renderer can't use, is shown in a native dialog
(`MessageBoxW` on Windows, `NSAlert` on macOS, `zenity` or `kdialog` on Linux
when installed), printed to stderr and appended to `sauerteig.log` in that
folder. The folder is `~/Library/Application Support/Sauerteig`
on macOS, `%LOCALAPPDATA%\Sauerteig` on Windows, and `$XDG_DATA_HOME/sauerteig`
(by default `~/.local/share/sauerteig`) on Linux. `SAUERTEIG_DATA_DIR`
overrides it.

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
(50–100%), exposure, and vsync. Anti-aliasing offers Off, 2×, 4× and 8× MSAA,
each only where the adapter supports that count for both `Rgba16Float` (with
resolve) and `Depth32Float`, and Auto, the default. Auto takes 4× while the
scene has at most 2560×1440 pixels, 2× up to 3840×2160, and none above that, and
never 8×; it follows from throughput measurements at 4K
(see [renderer](tracking/renderer.md)). A saved choice the adapter can't run
uses the next lower count it can, and saved settings that say `on` mean 4×.
Render scale affects only the scene; egui stays at output resolution. Vsync off
selects Mailbox, then Immediate if available, otherwise Fifo. The surface
requests one frame of latency, clamped by the backend. Local movement and remote
player poses interpolate between 60 Hz steps; mouse look remains immediate.

Lighting is dynamic. The sun shadow window follows the player in six-metre,
texel-aligned light-space steps; its static geometry cache is kept until geometry
or the window changes, and actors draw over a fresh copy each frame. Every light
node is a warm omnidirectional point light with a 12 metre range and static
shadows (`light_from_object` in `lighting.rs`). Spot lights, other colors, ranges,
and dynamic or no shadows exist in the renderer, but nothing places them until
object properties do. Moving actors cast shadows from the eight most important
shadowed lights, whatever their own mode.

Each frame the lights in view, at most 256 by importance, are culled by a
compute pass into a 16 × 9 × 24 grid of view clusters, and one shared WGSL
function lights terrain, props, actors, and water and glass from the lights of
the fragment's cluster: at most 16 per fragment, and 8 for water and glass,
which take only highlights. Lights too faint to see are skipped, and weak ones
filter their shadow with one tap instead of four. Light shadows come from a
paged atlas of 1024² depth pages (fourteen by default, 60 MiB with the overlay
page), each holding tiles of one size, 512², 256², or 128²: six per point light
and one per spot light, sized by the light's projected radius with hysteresis,
and shrunk for the least important lights so that more of them are covered; a
full atlas of 128² tiles holds 149 point lights. A tile re-renders only when a
chunk or object edit reaches its light's sphere, or the light changes: at most
six faces a frame (twelve for three frames after a jump), and fewer where
measured GPU time says a face costs more than a 1.5 ms budget allows. A new
light stays off, then fades in, until its shadow has rendered. A light whose
tiles are stale keeps using them; one that no tile can hold fades out rather
than leak light through walls. The eight most important shadowed lights (a
light that has them keeps them down to tenth place) also sample an overlay page
that is redrawn every frame with the moving actors near them, in at most 15
tiles; the other lights cast only static shadows.

The renderer asks the adapter for what it uses and no more (`render/device.rs`):
the Vulkan, DirectX 12 and Metal backends only (`WGPU_BACKEND` and
`WGPU_DX12_COMPILER` still override them, for testing), compute shaders,
anisotropic filtering, two storage buffers per shader stage, three bind groups,
64 array layers, 8192-pixel textures (more for a bigger window), and a
64-invocation compute workgroup. It also takes `TIMESTAMP_QUERY` for the GPU
timings and `TEXTURE_ADAPTER_SPECIFIC_FORMAT_FEATURES` for 2× and 8× MSAA, each
only where the adapter has it. An adapter that lacks a required limit or
ability is refused with a message that names it and the minimum specification.

Shaders compile without wgpu's forced loop bounding, through `shaders::trusted`
in `render/shaders.rs`. On Metal the
forced bounding makes the compiler keep every loop by reading a volatile flag in
each iteration, which made the clustered light loop about three times slower (stage
1's sun filtering sped up as well). Every embedded loop has a bounded trip count,
the clustered one at most 8 mask words of 32 bits that each lose a set bit per
iteration, and runtime bounds checks stay on. Keep both true when changing a shader,
as the function's `SAFETY` comment says.

`unsafe` appears in four places, each with a `SAFETY` comment: `shaders::trusted`,
the server's `durable_rename` on Windows (`MoveFileExW` in `persistence.rs`),
and the startup-failure dialogs in the client's `failure.rs` (`MessageBoxW` on
Windows and `NSAlert` on macOS). Keep new `unsafe` out of other code, and add it
to this list when it is unavoidable.

Nodes have no visible model in play; their guides do not cast shadows. Lamp props
are independent decoration, with emissive surfaces but no automatic local light.
Light color, radius, intensity, and cone settings are not editable or restored from
imported maps. Ambient fill keeps unlit areas readable; there is no baked map
lighting or global illumination. Guides, tracers, and the first-person weapon do not
cast shadows.

Terrain uses world-space texture coordinates. Materials share albedo/AO and
normal/roughness texture arrays with linear-light color mipmaps and renormalized
normals. Opaque terrain culls back faces except when the eye touches or enters
terrain; that fallback preserves interior faces during editor flight. Props,
procedural meshes, actors, guides, and transparent volumes remain two-sided.
Repeated props use GPU instancing, near/distant meshes, and distance and
light-range culling. Sources and conversion commands live with the
[materials](../assets/materials/README.md) and [props](../assets/props/README.md).

## Performance checks

The target is the
[minimum specification](tracking/platforms.md#minimum-specification): 8 GB of
memory and graphics about as fast as a Steam Deck's, with Vulkan, DirectX 12 or
Metal. Nobody has measured Sauerteig on such hardware yet; measurements so far
come from the maintainer's M5 Pro, and older ones from an M3 Pro. Use a
representative indoor/outdoor map. Include two fighters and a builder changing
geometry during combat; check save/rejoin and frame-time spikes as well as
average frame rate.

For repeatable CPU measurements:

```sh
cargo run --release -p sauerteig-core --example profile_world
cargo run --release -p sauerteig-core --example profile_fine
cargo run --release -p sauerteig-import --example profile_ogz -- path/to/map.ogz
```

These report meshing, edit, movement, and ray-query timings. The OGZ profile also
reports original-scale storage, load time, query percentiles, and dirty remeshing.
They exclude GPU execution and networking. F3 shows the adapter's name with its
graphics API and driver, rendering counts, CPU edit/remesh timing, effective
scene resolution/MSAA/present mode, shadow maps rendered, per-pass GPU
timestamps where supported, and how many lights are drawn, shadowed (and
shadowing actors), over budget or waiting. It explicitly reports unavailable
timestamps otherwise. The asynchronous profiler never waits for a sample during
play. Pass spans can overlap on Metal, so do not add them to obtain a GPU frame
time.

For fixed native renderer comparisons, source `scripts/env.sh`, then run:

```sh
cargo run --release --locked -p sauerteig-client --bin capture -- \
  --poses scripts/capture-poses.json --output .local/captures/review \
  --width 3840 --height 2160 --frames 60 --warmup 10
```

Each pose names a world file (or a test scene: `arena`, `materials`, and the
synthetic lighting scenes `lightroom`, `corridor`, `waterglass`, `lamphall`,
`lightshow` and `shadowtest`), eye, look-at point, fixed animation time and optional guides,
`actors` standing in the scene, `edit_storm` (invalidate every cached shadow each
frame), or a `path` of waypoints to walk at `speed` metres a second or jump to
and hold for `dwell` frames. Lights fade in and shadows render over many frames,
so each pose first draws until lighting settles; the report gives the frames that
took, the shadow faces rendered and reused per frame, the GPU time of the atlas
passes apart from the sun's, and how many lights reach each cluster that has any.
The render clock is fixed at 60 Hz. The supplied imported poses expect a **copy** of
the library's `the_cube1m` at `.local/the_cube1m.json`. Supply your own pose
manifest for other worlds. The tool refuses to replace existing PNGs.
`--settings FILE` accepts a JSON object with `scale`, `msaa` (`"auto"`, `"off"`,
`"x2"`, `"x4"` or `"x8"`; `"on"` means 4×), `bloom`, `exposure` and `vsync`;
omitted fields take the normal defaults. `--brute-force` evaluates every light
at every fragment (for comparing with the clusters), `--pages N` sets the shadow
atlas size, `--over-budget unshadowed` keeps lights that no tile can hold,
`--actor-lights N` sets how many of the most important shadowed lights shadow
actors, and `--node-shadows off|static|dynamic` sets what light nodes cast.
Measurements include CPU submission, completed-frame median/p95 and individual
GPU pass medians. They exclude simulation, presentation and PNG readback; they
do not prove perceived smoothness on a monitor.

Serial frame times include a fence wait after every frame, which on Metal sleeps
a millisecond per poll, so they overstate GPU time. To compare settings, add
`--throughput N --runs R`: after each pose settles, the tool submits N frames
back to back, waits once, and repeats that R times (five by default), and the
report gives each run's milliseconds a frame as `throughput_ms_per_frame`. Use
`--frames 1` to keep the serial loop short. Compare settings by running the
tool alternately for each setting, several rounds, and note what else the
machine was doing; for example, at 4K:

```sh
for round in 1 2 3 4 5; do
  for setting in off x2 x4 x8; do
    echo "{\"msaa\":\"$setting\"}" > .local/aa.json
    target/release/capture --poses scripts/capture-poses.json \
      --output ".local/captures/aa-$round-$setting" --width 3840 --height 2160 \
      --frames 1 --warmup 10 --throughput 120 --runs 5 --settings .local/aa.json
  done
done
```

The native GPU regressions run explicitly with
`cargo test -p sauerteig-client --lib gpu_ -- --ignored --test-threads=1` on a host
with GPU access. They check that cached shadows match fresh ones after edits, that
clusters built by the compute pass match the CPU's and give the same picture as a
brute-force render, that every sample count the adapter offers draws the same
scene, that a wall stops the next corridor's lights, and that spot lights and
actor overlays draw. Normal workspace tests leave these hardware-dependent
tests ignored.

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
