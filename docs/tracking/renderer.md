# Renderer

State: active.

## Outcome

Built worlds look good and stay fast: many real lights with shadows, HDR with
bloom, a proper sky, and later particles and decals, with smooth motion at any
refresh rate. The renderer runs on Vulkan, DirectX 12 and Metal, and its lowest
quality setting runs smoothly on the minimum specification. Performance is
measured on named hardware, not assumed.

## Current behavior

- **Stages 1 and 2 are in place.** The scene renders in linear HDR with bloom,
  filmic tonemapping, Auto 4× MSAA, render scaling, motion interpolation, and
  per-pass GPU timings in F3.
- **Lights.** Every light node in view is a clustered point light, up to 256
  ranked by range over distance. A cached atlas shadows up to 149 point lights,
  and lights it can't hold fade out rather than leak through walls, so
  the_cube1m draws 149 of its 339. A fragment shades at most 16 of the lights
  that reach it, 8 on water and glass, most important first. Players cast
  shadows under the eight most important lamps.
- **GPU requests.** The renderer asks wgpu for WebGPU's default limits and only
  the timestamp feature (`Renderer::create`), so 4× is the only MSAA sample
  count. The Uniform and Nearest lighting paths, written for browser and
  compatibility-class limits, are still in the code, although that request
  already turns away the adapters they were written for.
- **World meshes** use a 52-byte vertex and an index buffer that only counts
  up: 576 MiB for the_cube1m and 824 MiB for nub1, about 2 KB per piece
  (measured on the M5 Pro, 2026-11-12; Metal allocates 619 and 894 MiB). On
  unified-memory machines such as Apple silicon that is system memory; on a
  discrete GPU it is video memory. At least 61% of the_cube1m's triangles are
  faces buried between solid cubes, which [octree](octree.md) O1a leaves out.
- **Checked only on Metal.** All evidence here comes from the maintainer's
  Macs: an Apple M3 Pro, and an M5 Pro since 2026-11-11. None covers Vulkan,
  DirectX 12, or hardware like the minimum specification's.
- **Still constants:** light color and range (until object properties land), sun
  direction, fog, field of view, and the 180 m far plane.

## Stage 1: render foundation

State: merged on 2026-10-25. The maintainer has not yet checked smoothness and
look on the 144 Hz 4K monitor.

- **Tools.** The `capture` binary renders fixed camera poses headlessly to PNG
  with the real renderer, using `scripts/capture-poses.json`.
- **HDR pipeline.**
  - The scene renders to an `Rgba16Float` target.
  - Post-processing applies ACES-fitted tonemapping and five-level bloom
    (strength 0.06).
  - Render scale (0.5–1), exposure, bloom, and vsync are saved settings. egui
    draws at output resolution.
- **Anti-aliasing defaults to Auto.**
  - 4× MSAA while the scene has at most 2560×1440 pixels, none above. This was
    added at merge after the 4K measurements below.
  - On and Off override it.
- **Shared shader code.** One WGSL module is assembled into each shader, and all
  five validate with Naga.
- **Cached shadows.**
  - The sun window (192 m, moving in 6 m steps) keeps a static-geometry shadow
    cache, as local lights did until stage 2.
  - Any geometry or object rebuild invalidates the sun's.
  - Moving actors are drawn over a copy each frame.
- **Back-face culling** on opaque terrain.
  - Terrain falls back to two-sided drawing when the camera is inside geometry.
  - Objects, props, actors, guides, and volumes are always two-sided.
- **Smooth motion.**
  - The local player and other players are interpolated between 60 Hz steps;
    mouse look stays per-frame.
  - The surface asks for the lowest frame latency.
  - With vsync off, presentation prefers Mailbox, then Immediate.
- **GPU timing.** F3 shows per-pass GPU timings from timestamp queries where the
  adapter supports them.

Gaps:

- **Large imported worlds are expensive to draw.** On the M3 Pro they took
  16–38 ms per frame at 4K in serial frame times, 16 ms even without MSAA, and
  stage 2's shader change brought the same poses to 11–20 ms with 149 real
  lights. On the M5 Pro, nub1's heaviest pose takes 5.39–5.41 ms of GPU
  throughput at 4K, within 6.06 ms (measured 2026-11-12). They are limited by
  geometry more than pixels: [octree](octree.md) O1a leaves out hidden faces,
  and the [large-worlds](large-worlds.md) track handles distance.
- **Sun shadow invalidation is coarse.** Any geometry rebuild invalidates its
  cache. Stage 2 keys local light caches by tile and version; the sun's cascades
  (stage 2b) should too.
- **Untested hardware paths:** adapters without timestamps or MSAA, and non-Fifo
  native presentation.

## Stage 2: many real lights

State: merged on 2026-10-25. The maintainer has not yet looked at it in the
running app.

- **Light model.**
  - Light nodes are warm point lights: 12 m range, intensity 16, which keeps
    mean image brightness within 3% of stage 1 on the arena and spawn poses.
  - Each becomes a `LightDesc` through `light_from_object`. Spot lights work in
    data and shaders but have no editing UI yet.
- **Clustered forward shading.**
  - A 16×9×24 cluster grid with compute-built 256-bit masks over up to 256
    visible lights, ranked by range over distance.
  - One shared lighting function serves terrain, props, water, and glass, with
    up to 16 lights per fragment, 8 on water and glass.
  - Water and glass use premultiplied alpha.
- **Shadow atlas.**
  - 14 pages (60 MiB with the overlay page), holding 149 point lights at the
    smallest tile size. Lights beyond that fade out rather than leak through
    walls.
  - Static caches are keyed by tile and version.
  - The eight most important shadowed lights also sample an actor overlay, so
    players cast shadows under nearby lamps.
  - Filling the atlas after a join takes about 2.5 seconds, nearest lights
    first.
  - At least one face renders each frame.
- **Fallbacks.** A uniform-buffer path and today's nearest-eight path are
  settings of the same shaders.
- **One `unsafe` block.**
  - `render/shaders.rs::trusted` compiles our shaders without wgpu's forced
    loop bounding; bounds checks stay on.
  - The reason: on Metal that bounding made the clustered loop about three times
    slower.
  - Its `SAFETY` comment explains why every shader loop is bounded.
- **Tooling.** F3 shows the lighting path and the light and shadow counts. The
  capture tool gained synthetic scenes (light room, corridor, water and glass,
  lamp hall) and flags for comparing paths.

Gaps:

- **Busy 4K scenes can't hold 144 Hz** on the M3 Pro. Dense lights cost
  1.6–1.95× stage 1 at 4K, for example the imported tower at 19.7 ms in serial
  frame times. Stage 2b's quality tiers are the lever. At 1080p, stage 2 is
  faster than stage 1.
- **Imported maps lose lights.** the_cube1m draws 149 of its 339, and in dense
  clusters a fragment stops at 16 lights and drops the less important rest.
  Lights don't have per-light brightness until object properties arrive. N2
  shadows every light and removes the per-fragment cap.
- **Some targets are borderline** on the M3 Pro.
  - The largest imported map's p95 at 1080p is 13.2–13.6 ms against a 14 ms
    target, and some earlier sessions missed it.
  - An edit storm at 4K has a 4.16 ms p99 for atlas updates, against a 3 ms
    target.
- **The sun's static pass costs 7–9 ms** on the M3 Pro whenever its window or
  geometry changes. Stage 2b's cascades address it.
- **The actor overlay's 15 tiles run out** with four or more actors in a
  crowded, lit room. The lowest-ranked of the eight lights then drop actor
  shadows on some faces. N2 replaces the overlay.
- **Not exercised:**
  - the uniform and nearest paths, except when forced on the M3 Pro (N1
    deletes them);
  - the research's full sweeps of light counts, MSAA, and render scale.

## Next stages

The maintainer approved these on 2026-11-13 with the native-first plan for
[shared-sandbox](shared-sandbox.md) stage 2.

- **Order.** N1, 2b, N2 and N3 run one at a time, in that order, because each
  changes `render.rs`, the shadow code and the shaders. Each runs beside the
  other tracks' work orders:
  - N1 in wave 1;
  - 2b in wave 1 as well, starting when N1 merges;
  - N1b, the GPU tests on software drivers, once N1 and
    [platforms](platforms.md) stage 1 have merged, beside the other renderer
    stages;
  - N2 in wave 2, beside [octree](octree.md) O2, once 2b has merged;
  - N3 in wave 4, beside shared-sandbox 2e and 2f, once octree O1b has merged.
- **The octree changes what the renderer draws.** Octree O1a builds world
  meshes from one tree and leaves out hidden faces, behind today's
  `mesh_chunk`; O1b draws water and glass from the tree. O1b and 2b both
  change the rebuild paths in `render.rs`, so whichever starts second waits for
  the first to merge.
- **Decided:**
  - **Lighting rule** (maintainer, 2026-11-11): every light is lit and shadowed
    the same way. Quality settings vary only detail (render scale, shadow
    resolution and anti-aliasing), never which lights are lit, shadowed, or
    cast player shadows. No rank or distance cutoffs and no top-N rules.
  - **Native GPU features within that rule** (maintainer, 2026-11-13): every
    light shadowed, every light in view lit, 2× and 8× MSAA where supported,
    and lighter world meshes.
  - **Platforms** (maintainer, 2026-11-13): Windows, macOS and Linux are equal
    targets. Renderer work must run on Vulkan, DirectX 12 and Metal, and an
    optional GPU feature appears only on adapters that support it.
  - **Minimum specification** ([platforms](platforms.md), maintainer,
    2026-11-13): 8 GB of memory and graphics about as fast as a Steam Deck's,
    with Vulkan, DirectX 12 or Metal. Nobody has run Sauerteig on a reference
    machine yet, and the M5 Pro can't stand in for one, so checks that need one
    are marked **needs a tester**.
- **Measuring.**
  - Compare GPU throughput: N frames back to back, one wait, and alternating
    A/B runs. Serial frame times overstate GPU time on Metal, because
    wgpu-hal 24's fence wait sleeps 1 ms per poll.
  - The 4K budget on the maintainer's 165 Hz display is 6.06 ms.
  - Testers and contributors don't have the private library, so measure the
    generated benchmark world
    ([large worlds](large-worlds.md#stage-1-a-generated-benchmark-world)
    stage 1) and the capture tool's built-in scenes as well as library copies.
- **Vulkan and DirectX 12 without the hardware** (checked 2026-11-13). GitHub's
  standard runners have no GPU, but the GPU tests can run on software drivers,
  as wgpu's own CI does:
  - Mesa's lavapipe (Vulkan) on Linux. wgpu's CI downloads a prebuilt Mesa and
    points `VK_DRIVER_FILES` at its lavapipe driver; Ubuntu packages lavapipe
    in `mesa-vulkan-drivers`.
  - WARP (DirectX 12) on Windows. It ships with Windows; wgpu's CI puts a newer
    `d3d10warp.dll`, from the `Microsoft.Direct3D.WARP` NuGet package, beside
    its test binaries.
  - wgpu 24 ranks software adapters last but still returns one when no GPU is
    present, so the renderer should pick them without special code (read from
    wgpu-core's adapter choice, not yet tried).
  - They check correctness and API rules, not speed. CI runs none of the
    `gpu_` tests today: they are ignored by default and run by hand on a Mac.
    N1b adds a job for them, which follows [platforms](platforms.md) stage 1's
    schedule while the repository is private.

### N1: native GPU features

State: ready. Wave 1, small. It needs no other work order merged first.

- **Decided** (maintainer, 2026-11-13):
  - Request what the renderer's designs need, per adapter. Requesting
    everything an adapter offers is ruled out: wgpu's `Limits` documentation
    warns that better limits than needed may lower performance.
  - Anti-aliasing offers 2×, 4× and 8× only where the adapter supports that
    sample count for the scene's color and depth formats.
  - The Uniform and Nearest lighting paths go. They were written for browser
    and compatibility-class limits ([research](../../research/many-lights.md)).
    The browser removal (2026-11-12) took only wasm-only code and attributes,
    `web_time` and WebGPU comments out of the renderer; the paths, their shader
    variant and their tests remain (checked 2026-11-13).
- **Scope:**
  - **Device request.** In `Renderer::create`, replace `Limits::default()` and
    the timestamp-only feature mask with what the renderer uses. When an
    adapter lacks a required feature or limit, the game says which and names
    the minimum specification. The text is `Renderer::create`'s error, which
    reaches `report_error` in the client's `lib.rs`. That only logs today;
    shared-sandbox's save and quit fixes make it a visible message. If N1
    merges first, the design lead checks the visible message once both are
    in. Optional features are used only where present: `TIMESTAMP_QUERY`, as
    today, and `TEXTURE_ADAPTER_SPECIFIC_FORMAT_FEATURES`, without which wgpu
    allows only WebGPU's guaranteed sample counts, 1 and 4. Later stages add
    what they need here.
  - **Backends.** Ask for Vulkan, DirectX 12 and Metal adapters, the minimum
    specification's APIs, instead of `Backends::all()`, which includes OpenGL.
    Build the instance with `InstanceDescriptor::with_env`, so `WGPU_BACKEND`
    can choose Vulkan or DirectX 12 on Windows for testing, and
    `WGPU_DX12_COMPILER` the DirectX 12 shader compiler. Show the backend and
    driver from `AdapterInfo` after the adapter's name in F3 and in the capture
    report.
  - **Anti-aliasing.** Settings offers Off, 2×, 4× and 8×, each only where the
    adapter supports it for `Rgba16Float` with resolve and for `Depth32Float`,
    and Auto, which picks a supported count per scene resolution from the
    measurements below. Saved settings that say `on` keep meaning 4×.
  - **Delete** `LightingTier::Uniform` and `Nearest` with their configurations
    (`render/lightset.rs`), `lighting_tier` (`render.rs`, the function and the
    method), the uniform-buffer bindings (`UNIFORM_BINDINGS` in
    `render/shaders.rs`) and their branch in `LightGpu` (`render/lights.rs`),
    `ClusterGrid::COMPACT`, and the capture tool's `--tier` flag. Delete the
    tests that exist only for them,
    `fallback_buffers_fit_the_smallest_guaranteed_uniform_binding` and
    `gpu_uniform_tier_matches_the_clustered_tier_without_shadows`, and their
    cases in the light-set and cluster tests. Replace
    `entry_points_stay_within_compatibility_limits` with a check of every
    shader against the limits N1 requests. Keep `ClusterGrid::build_masks`,
    which the capture report's lights-per-cluster counts and the GPU mask test
    use.
  - **Throughput mode** in the capture tool: N frames back to back, one wait,
    repeated runs.
  - **Docs:** the rendering and performance sections of
    [development](../development.md).
- **Parallel work:** edits `render.rs` (`Renderer::create` and the pipelines
  per sample count), `render/settings.rs`, `render/post.rs`,
  `render/volumes.rs`, `render/lightset.rs`, `render/lights.rs`,
  `render/clusters.rs`, `render/shaders.rs`, `capture.rs`, the anti-aliasing
  menu in `game_panels.rs`, `game.rs` (getting the supported sample counts to
  that menu, since `local_panels` receives no renderer today), and the
  rendering section and performance checks of `docs/development.md`. It runs
  beside every wave-1 work order outside the renderer, sharing a few places:
  shared-sandbox 2b also edits `render.rs`, splitting `rebuild_all`, which N1
  leaves alone, and edits the F3 block of `Game::ui` in `game.rs`;
  large-worlds stage 1 also edits the performance checks, where N1 deletes
  `--tier`; octree O1b rewrites the meshing in `render/volumes.rs`, where N1
  changes the pipelines. The second to merge rebases onto the first. N1 doesn't
  touch `.github/workflows/`, which platforms stage 1 owns.
- **Acceptance:**
  - [ ] On the M5 Pro, Settings offers Off, 2×, 4×, 8× and Auto, and F3's
    sample count follows the choice. A unit test shows that an adapter without
    8× isn't offered it.
  - [ ] F3 shows the backend and driver after the adapter's name, and its
    lighting line no longer names a lighting path.
  - [ ] A unit test gives the adapter check limits and features without one
    that the renderer needs, and the message names it and the minimum
    specification.
  - [ ] 4K GPU throughput on the M5 Pro for imported-spawn and arena-yard at
    Off, 2×, 4× and 8×, against 6.06 ms, from alternating runs. Auto's rule
    follows from these numbers.
  - [ ] On the M5 Pro, the `gpu_` tests pass when run with `--ignored`,
    including the brute-force and CPU-mask comparisons, and the capture tool
    renders its built-in scenes.
  - [ ] Settings saved before N1 load, and every shader validates.
  - [ ] **Needs a tester:** the game starts and draws correctly on a real
    Vulkan GPU and a real DirectX 12 GPU, offering the sample counts that
    adapter supports.
- **Record at the end:** the features and limits requested and which design
  needs each; the sample counts offered on the M5 Pro; the throughput table;
  what was deleted; and the client's fixed GPU memory by part (render targets
  at 1280×800, 1920×1080 and 4K for each sample count, the shadow atlas, other
  buffers). Shared-sandbox's estimate assumes a fixed 0.6 GiB, from a capture
  at 4K, and 8× MSAA doubles the multisampled targets of 4×, so 2d needs these
  figures to set the fixed term per resolution and setting.

### N1b: GPU tests on software drivers

State: waits for N1 and [platforms](platforms.md) stage 1, which owns
`.github/workflows/`. Small to medium. The M5 Pro, where sessions run, has no
WARP or FXC and no lavapipe set up, so the design lead pushes the session's work
to a scratch branch, runs the job, and passes on its logs.

- **Scope:**
  - **A CI job** on Linux and Windows that runs the `gpu_` tests and the
    capture tool's built-in scenes on lavapipe and WARP, set up as described
    above, on platforms stage 1's schedule while the repository is private.
  - **Validation on.** In debug builds wgpu asks for the Khronos validation
    layer on Vulkan and the debug layer on DirectX 12
    (`InstanceFlags::from_build_config`), carries on without them if they
    aren't installed, and only logs what they report. So the job installs the
    Vulkan layer on Linux, and the tests fail on an error-level message from
    wgpu-hal. GitHub's Windows Server 2025 image lists Visual Studio's
    graphics tools component but not Windows' Graphics Tools feature, which
    holds the D3D12 debug layer, `D3D12SDKLayers.dll` (checked 2026-11-13).
    The job checks for the layer and adds the feature if it is missing
    (`Add-WindowsCapability -Online -Name Tools.Graphics.DirectX~~~~0.0.1.0`).
  - **DirectX 12 shader compiler.** wgpu 24 compiles DirectX 12 shaders with
    FXC unless told otherwise, and its documentation calls FXC old, slow and
    unmaintained. DXC needs two DLLs shipped with the game, or the `static-dxc`
    feature. Record whether every shader compiles with FXC on WARP and what it
    adds to startup; switching to DXC is the design lead's call.
  - **Bugs the drivers find** in renderer code are recorded here and fixed by
    the renderer stage that is open, or by N1b once that stage has merged.
  - **Docs:** how to run the GPU tests on a software driver, in the
    [development guide](../development.md#build-and-check).
- **Parallel work:** the GPU-test job in `.github/workflows/` and, where a
  driver needs it, the `gpu_` tests' setup in `capture.rs`. It runs beside the
  other renderer stages; 2b and N2 also edit `capture.rs`, so the second to
  merge rebases onto the first.
- **Acceptance:**
  - [ ] On the design lead's push, the job passes on lavapipe and WARP.
  - [ ] Its logs name each adapter, backend and driver, and show the
    validation layers loaded. A scratch run with a deliberate API error fails
    on both drivers; the design lead drops that change.
  - [ ] Every shader compiles with FXC on WARP, or the failures are listed.
- **Record at the end:** the driver versions; the sample counts lavapipe and
  WARP offer; whether the D3D12 debug layer was present or had to be added;
  FXC's results and startup cost on WARP; the job's minutes per run, for
  platforms stage 1's schedule.

### Stage 2b: sun cascades and quality tiers

State: waits for N1, which reworks anti-aliasing and removes the lighting
paths, so that "tier" means only the quality levels below. Wave 1: it starts
when N1 merges, runs beside the rest of wave 1, and may still be open in wave
2. N2 waits for it.

- **Decided:**
  - Quality tiers vary detail only: render scale, shadow resolution (atlas
    tiles and sun cascades) and anti-aliasing. They never drop shadowed or
    visible lights, and never change which lights shadow players (maintainer,
    2026-11-11). This replaces the earlier plan, which stepped down shadowed
    lights and then visible lights.
  - The lowest tier must run smoothly on the minimum specification
    (maintainer, 2026-11-13).
- **Scope:**
  - **Sun cascades.** Replace the single sun window (`sun_shadow_matrix` in
    `lighting.rs`: 192 m at 2048², 9.4 cm texels, moving in 6 m steps) with
    three cascades split by distance from the eye and snapped to texels, so
    they don't shimmer. The [research](../../research/many-lights.md)
    suggests splits at about 15, 42 and 150 m and 1024² each, an estimated 3,
    8 and 29 cm per texel. Actors draw over copies of the first two cascades,
    as they do over the single window today. Keep each cascade a whole texture
    or array layer, so those copies stay whole-subresource copies, which
    DirectX 12 allows (see N2).
  - **Chunk-scoped caches.** Today any rebuilt mesh bumps `geometry_revision`
    (`Renderer::rebuild`, `rebuild_all` and `objects`) and re-renders the
    whole static sun map, which cost 7–9 ms on the M3 Pro. Each cascade
    instead re-renders only the area a change touches: the old and new bounds
    of each rebuilt chunk or object, projected into the cascade, cleared by
    drawing as atlas tiles are, and redrawn with a scissor.
  - **Quality levels.** A Quality setting with fixed levels and Auto. A level
    sets render scale, shadow resolution, and an anti-aliasing count the
    adapter supports. Changing one of those by hand shows the level as
    Custom. Auto moves between levels from measured GPU frame time against the
    display's refresh interval, with hysteresis so it doesn't flicker; without
    timestamp queries it uses frame intervals.
  - **Docs:** the lighting and settings paragraphs of
    [development](../development.md).
- **Parallel work:** edits `lighting.rs` (`sun_shadow_matrix`), `render.rs`
  (the sun passes in `Renderer::draw`, `CameraUniform`, and the invalidation
  in `rebuild`, `rebuild_all` and `objects`), `render/shadows.rs`
  (`CachedShadow`, `ShadowStamp`), the sun binding in `common.wgsl` and its
  sampling in `world.wgsl`, `render/settings.rs`, `render/post.rs`, the tile
  sizes in `render/atlas.rs`, `render/timing.rs`, the settings menu in
  `game_panels.rs`, `capture.rs`, and `docs/development.md`. It runs beside
  the other tracks' work orders and N1b, but not beside N1, N2 or N3, and not
  beside octree O1b, which changes the same rebuild paths. Shared-sandbox 2b
  splits `rebuild_all`, and N1b may edit the `gpu_` tests' setup in
  `capture.rs`; when two of these are open at once, the second to merge
  rebases onto the first.
- **Acceptance:**
  - [ ] On the M5 Pro, a one-block edit inside the sun's range on a copy of
    the_cube1m re-renders only part of a cascade. Record the sun passes' GPU
    time for that frame before and after.
  - [ ] Captures of the arena, materials and imported poses show sharper near
    shadows, with no gaps or doubled shadows at cascade boundaries. Flying
    across the_cube1m in the app shows no shimmering at cascade edges.
  - [ ] At every level, F3 shows the same lights drawn and shadowed on
    imported-spawn, lightroom-center and lightroom-actors-16.
  - [ ] On the M5 Pro at 4K and 165 Hz, Auto steps down where the default
    level misses 6.06 ms and steps back up afterwards, without flickering.
  - [ ] The lowest level's GPU throughput on the M5 Pro at 1280×800 and
    1920×1080 is recorded for the scenes below, for comparison with a
    tester's.
  - [ ] **Needs a tester** ([platforms](platforms.md) stage 3): on a reference
    machine, the lowest level holds 60 frames a second (16.7 ms at the 95th
    percentile) at the machine's native resolution, 1280×800 on a Steam Deck.
    Use the benchmark world's capture poses and the capture tool's lighting
    scenes (lightroom, corridor, waterglass, lamphall), and record the
    machine, its system, backend and driver, and the capture report. The check
    stays open until someone runs it.
  - [ ] Tests: cascade matrices don't move while the eye moves within a texel
    or turns; cached cascades match fresh renders after edits, actor moves
    and level changes, extending the GPU test in `capture.rs` that compares
    cached shadows with fresh ones; Auto's choices, with hysteresis. The GPU
    tests pass on the M5 Pro, and in N1b's job once it exists.
- **Record at the end:** sun pass times before and after; cascade memory;
  each level's settings and M5 Pro throughput; what Auto did along the
  imported-flight path; the tester's results, or that none were available.

### N2: every light shadowed

State: waits for 2b. Wave 2, beside octree O2, once 2b has merged.

- **Decided** (maintainer, 2026-11-13, under the lighting rule):
  - Every light is shadowed: 339 of 339 on the_cube1m, up from 149. No light
    fades out for lack of a tile; tiles get smaller instead.
  - Every light in view is lit: the 256-light cap goes.
  - Every shadowed light shadows players and other moving actors, not only
    the eight most important.
- **Decided** (design lead, 2026-11-13):
  - Each quality level bounds the atlas's memory, static and per-frame tiles
    together. The lowest level keeps today's 60 MiB, 15 pages of 1024². The
    fixed 0.6 GiB of shared-sandbox's estimate already includes that, because
    it was measured with today's atlas, so the default world budget derived
    for the minimum specification stands. Every other level allows 256 MiB,
    which holds the_cube1m's 339 lights at 128² (about 128 MiB) with room for
    larger tiles near the eye; shared-sandbox's estimate counts only the
    lowest level's bound.
  - Today `grant_classes` (`render/lightset.rs`) first gives every light it
    can the smallest tiles, 128², and lets the most important sharpen while
    pages remain; lights past the end get none. Past a level's bound, tiles
    instead shrink on to 64² and 32², so every light keeps a tile. The lowest
    level's 15 pages hold 2,560 point lights at 32². If the lights in view
    need more even then, the atlas grows past its bound by 24 KiB per point
    light rather than drop a shadow, and F3 says so.
- **The copy rule** (documentation checked 2026-11-13). The approved plan gave
  each light actor shadows by copying its static tile, a sub-rectangle of a
  `Depth32Float` atlas page, into a per-frame tile and drawing the actors over
  it, and asked for the copy rule to be checked on DirectX 12 and Vulkan first.
  DirectX 12 doesn't allow such copies on every GPU, so N2 doesn't use them:
  - Vulkan allows them: the valid-usage rules of `vkCmdCopyImage` limit
    bounds, aspects and formats, not the size of the region.
  - DirectX 12 doesn't, by default. Microsoft's documentation says that a
    texture that can be a depth target
    (`D3D12_RESOURCE_FLAG_ALLOW_DEPTH_STENCIL`, which wgpu sets on the atlas)
    must be copied as whole subresources, and that an invalid
    `CopyTextureRegion` call is undefined behavior that can remove the device. A
    Microsoft engineer wrote in
    [gpuweb issue 997](https://github.com/gpuweb/gpuweb/issues/997) (2020)
    that GPUs with programmable sample positions, tier 1 or 2, allow partial
    copies. Direct3D's rule is why WebGPU forbids such copies, and wgpu 24
    doesn't expose the tier.
  - No such limit turned up in Apple's documentation, and a probe on the
    M5 Pro ran these copies without errors (2026-11-12).
  - wgpu 24 checks only bounds and alignment for texture copies
    (`validate_texture_copy_range` in wgpu-core 24.0.5), and its DirectX 12
    backend passes the region to `CopyTextureRegion` unchanged. Neither
    wgpu's checks nor a pass on WARP would show that partial copies work on
    every DirectX 12 GPU, since they depend on each GPU's sample-position
    tier.
- **Scope:**
  - **Atlas sized from the lights.** Replace the 32-bit table of page sizes
    (`FramePlan::page_classes`, which limits the atlas to `MAX_PAGES`, 14, in
    `render/shadows.rs`) with a storage-buffer page table, and grow the atlas
    with the lights that need tiles, up to the level's bound. Tile resolution
    follows screen size, and tiles shrink instead of lights fading out
    (`OverBudget::FadeOut` in `render/lightset.rs`). Shared-sandbox 2d
    replaces the object limit (`MAX_OBJECTS`, 1,024) with its memory budget,
    so no light count is fixed. Request larger limits, such as texture array
    layers, through N1's device request.
  - **Every light in view lit.** Replace the fixed 256-bit cluster masks
    (`ClusterGrid::CLUSTERED`) with masks or lists sized from the number of
    lights in view. Keep the `SAFETY` comment on `shaders::trusted` true:
    every shader loop still needs a bounded trip count.
  - **No per-fragment cap.** Today `shade_local_lights` (`common.wgsl`)
    returns once `cap` of the lights that reach the surface have been shaded:
    16 (`light_cap` in `TierConfig`) from `world.wgsl`, and 8
    (`TRANSPARENT_LIGHTS`) from `volume.wgsl` for water and glass. Lights come
    most important first, so a fragment in a dense cluster drops the rest, a
    top-N rule. Shade every light of the fragment's cluster that reaches it.
    The loop stays bounded by the cluster's light count, so the `SAFETY`
    comment stays true. The skip for lights too faint to see (`FAINT_LIGHT`)
    stays, because it treats every light alike. Measure the cost at 4K on the
    M5 Pro, with the cap as the only difference, on lightroom-center, whose
    64 lights overlap far more than 16 per fragment, on imported-tower and on
    imported-water. If the cost seems to force a cap, don't choose one: record
    the numbers as an open decision for the maintainer, and until it is made,
    say here and in [lighting](lighting.md) which limit remains instead of
    claiming that every light is lit.
  - **Actor shadows for every light,** replacing `ACTOR_SHADOW_LIGHTS` (8),
    `OVERLAY_SLOTS` (15) and the overlay page, without depth sub-rectangle
    copies. Two ways work on every backend: a draw that writes a light's
    static depth into its per-frame tile before the actors, or a per-frame
    actor tile that shading samples beside the static one, as the overlay
    does now for eight lights. Keep per-frame tiles in their own texture, so
    no pass samples the texture it renders into. Measure both ways with 4 and
    16 actors on the M5 Pro, then use one on every backend. Allocate
    per-frame tiles each frame from the actors present, for the faces of the
    lights whose range reaches one, within the level's bound. They don't
    depend on the server's player limit, which the client doesn't learn:
    2d's `Accept` doesn't carry it.
  - **A capture scene with 1,000 lights in view,** for the checks below.
  - **Docs:** the lighting paragraphs of [development](../development.md)
    and the capture flags `--pages`, `--over-budget` and `--actor-lights`.
- **Parallel work:** edits `render.rs` (the atlas passes in
  `Renderer::draw`), `render/atlas.rs`, `render/lightset.rs`,
  `render/shadows.rs`, `render/lights.rs`, `render/clusters.rs`,
  `render/shaders.rs`, `render/settings.rs` (each level's bound), `lighting.rs`
  (`GpuLight`), `common.wgsl`, `world.wgsl`, `volume.wgsl`, `clusters.wgsl`,
  `shadow.wgsl`, `capture.rs` and its scenes, and `docs/development.md`. It
  runs beside octree O2 and shared-sandbox 2d, 2e and 2f, which don't touch
  these files except that 2d's third session reads the GPU's memory in
  `Renderer::create`, and beside N1b, but not beside 2b or N3. Where two of
  these are open at once, the second to merge rebases onto the first.
- **Acceptance:**
  - [ ] On a copy of the_cube1m on the M5 Pro, once lighting settles, F3
    shows 339 of 339 lights drawn and shadowed, none over budget or fading.
  - [ ] The 1,000-light scene draws every light in view and matches a
    brute-force render.
  - [ ] A GPU test shows an opaque fragment reached by more than 16 lights,
    and a water or glass fragment reached by more than 8, shading all of
    them.
  - [ ] At the lowest level, the atlas stays within its bound on the_cube1m
    and on the 1,000-light scene, and every light keeps a shadow.
  - [ ] In lightroom-actors-4, lightroom-actors-16 and lamphall-actor, every
    light whose range reaches an actor casts that actor's shadow. Walking
    past lamps on the_cube1m in the app, player shadows never switch on or
    off as lights change rank.
  - [ ] 4K GPU throughput on the M5 Pro for the imported, lightroom and
    lamphall poses at the default and lowest levels, against 6.06 ms, and
    atlas update times for flights, teleports and the edit storm, compared
    with stage 2's.
  - [ ] Tests: per-frame tiles with actors match fresh renders, cached tiles
    match fresh ones after edits, and unit tests cover atlas sizing, each
    level's bound and the page table. They pass on the M5 Pro, and in N1b's
    job on lavapipe and WARP.
  - [ ] **Needs a tester:** on a real Vulkan GPU and a real DirectX 12 GPU,
    the GPU tests pass and players cast shadows under every lamp.
  - [ ] **Needs a tester:** 2b's check of the lowest level, repeated on a
    reference machine.
- **Record at the end:**
  - atlas pages and memory at each level for the_cube1m, the 1,000-light
    scene, nub1 and the benchmark world's nub1 preset;
  - the per-light term of shared-sandbox's memory estimate: a light's records
    and cluster entries, plus the atlas's growth past the lowest level's
    bound. the_cube1m's lights seem to cost 0.17–0.35 GiB today, inferred
    from whole-process figures on the M5 Pro (2026-11-12); find out whether
    that holds and what it is;
  - lights per cluster (p50, p95 and max) on the imported poses, and the cost
    of shading every light against stage 2's per-fragment cap;
  - both actor-shadow methods' cost and the choice.

### N3: lighter world meshes

State: waits for N2 and [octree](octree.md) O1b. Wave 4, beside shared-sandbox
2e and 2f.

- **Decided** (maintainer, 2026-11-13): lighter world meshes. The 8 GB minimum
  specification made N3 required; whether it still is, once the octree leaves
  out hidden faces, is checked against octree O1a's measurements.
- **Why.** On unified-memory machines, which include all three reference
  machines, world meshes are most of a client's memory. The octree leaves out
  at least 61% of an imported map's triangles, and N3 then cuts each remaining
  triangle from 168 B to 72 B.
- **Today** (measured on the M5 Pro, 2026-11-12, copies of the maps):
  - A world vertex is a 52-byte `GpuVertex`, and every chunk has an index
    buffer that only counts up (`append_triangle` in core's `geometry.rs`),
    so each triangle takes 168 B.
  - the_cube1m has 10,785,252 vertices in 576.0 MiB of buffers, and nub1
    15,420,594 in 823.6 MiB, about 1,980 B per piece. Metal allocates 619
    and 894 MiB, 2,120–2,160 B per piece.
  - On unified-memory machines, which include all three reference machines,
    these buffers are system memory. On a discrete GPU they are video
    memory, about 2 KB per piece and 0.9 GiB for nub1, which shared-sandbox's
    memory estimate counts separately.
- **Scope:**
  - A world-only vertex of 24 B or less: position, a packed normal and the
    material. World color and emission are constants (`rebuild_meshes` in
    `render.rs` writes white and zero), and the texture coordinate depends
    only on position, normal and material (`materials::world_uv`), so the
    shader computes it.
  - Draw world chunks without an index buffer.
  - Objects, props, actors and the dynamic mesh keep `GpuVertex`. The sun and
    atlas shadow passes get pipelines for the world vertex.
  - Pooled buffers and indirect draws only if measurements call for them.
  - **Docs:** the rendering section of [development](../development.md).
- **Expected** (computed from the vertex counts above, before the octree's
  culling): 24 B per vertex and 72 B per triangle; 246.9 MiB for the_cube1m
  and 352.9 MiB for nub1, about 850 B per piece. With at least 61% of
  the_cube1m's triangles already gone after octree O1a, the totals fall
  further.
- **Parallel work:** edits `render.rs` (`GpuVertex`, `GpuMesh`,
  `rebuild_meshes`, the world and shadow pipelines), `world.wgsl`,
  `shadow.wgsl`, `render/shadows.rs` (the atlas pipeline's vertex layouts),
  `materials.rs`, `capture.rs`, and `docs/development.md`. It builds on octree
  O1a's mesher and shared-sandbox 2b's mesh preparation on worker threads, and
  runs beside shared-sandbox 2e, 2f and 2g.
- **Acceptance:**
  - [ ] On the M5 Pro, F3's world mesh memory falls by about 57% on copies of
    the_cube1m and nub1, from its value after octree O1a. Shared-sandbox 2b
    adds that F3 line; add it here if it's missing. Record Metal's graphics
    footprint (`footprint -p`) before and after.
  - [ ] Captures of every pose in `scripts/capture-poses.json` match the
    captures from before N3 within tolerance.
  - [ ] GPU throughput on the imported poses at 4K is no worse.
  - [ ] Tests: the new vertices give the same triangles, normals, materials
    and texture coordinates as before, and a GPU capture comparison passes on
    the M5 Pro and in N1b's job on lavapipe and WARP.
  - [ ] **Needs a tester:** 2b's check of the lowest level, repeated on a
    reference machine.
- **Record at the end:** bytes per piece and per triangle, raw and as
  allocated, for the_cube1m and nub1, as the new figures for shared-sandbox
  2d's memory estimate, video memory included.

## Later stages

- **Stage 3, sky and atmosphere:** sun, sky, and fog from world settings instead
  of constants.
- **Stage 4, effects:** particles and decals for building and combat feedback.

## Evidence

Stage 1, 2026-10-25, at 3840×2160 on the maintainer's earlier Mac, an Apple M3
Pro:

- **Checks.** fmt, Clippy, 279 workspace tests, and the native release and
  browser builds passed. The design lead re-ran fmt, Clippy, the tests, and a
  wasm check after adding Auto anti-aliasing.
  - A GPU regression test, run explicitly, matched cached and freshly rendered
    shadows after actor, terrain, light, and sun-window changes.
- **Captures.** Nine before/after pairs from identical poses, covering the
  arena, the materials scene, and the largest library map. They are kept
  outside the repository in `.local/captures/render-foundation/` in the main
  checkout.
  - Geometry, shadows, water and glass boundaries, underwater tint, and guides
    are preserved.
  - The new look has stronger contrast and more saturated water and glass.
- **Native frame times** (serial submission to GPU completion, median ms, before
  → after, with MSAA on):

  | Pose | Before | After |
  | --- | ---: | ---: |
  | arena-yard | 5.13 | 10.99 |
  | materials-underwater | 6.07 | 18.56 |
  | imported-spawn | 28.40 | 26.49 |
  | imported-overlook | 30.31 | 37.77 |

  Alternatives measured for arena-yard and imported-spawn:
  - scale 0.75 with MSAA: 6.48 and 20.63 ms;
  - scale 1 without MSAA, Auto's 4K choice: 5.06 and 16.02 ms.

  The 144 Hz budget is 6.94 ms.
- **Browser** (Chromium, 4K canvas), median frame interval: arena
  8.30 → 8.30 ms; the_cube1m 24.10 → 12.80 ms.
- **Not observed:**
  - perceived smoothness at 144 Hz, mouse latency, and vsync on the actual
    monitor;
  - interaction in a native window;
  - shadow updates during continuous editing.

Stage 2, 2026-10-25, on the same M3 Pro. Captures and raw numbers are kept
outside the repository in `.local/captures/render-stage2/` in the main checkout.

- **Checks.**
  - fmt, Clippy, the workspace tests (363, plus 6 GPU tests run explicitly),
    the native builds, and the wasm check passed for the implementer and again
    in the design lead's review.
  - The GPU tests compare clustered output with a brute-force render, compute
    masks with CPU masks, and cached shadows with fresh ones.
  - All 18 assembled shaders validate with Naga and compile in Chrome 155.
- **Frame times** (median / p95 ms):

  | Pose | 1080p stage 1 | 1080p stage 2 | 4K stage 1 | 4K stage 2 |
  | --- | ---: | ---: | ---: | ---: |
  | arena-yard | 4.9 / 6.5 | 3.5 / 5.0 | 4.2 / 6.6 | 5.4 / 6.0 |
  | materials-lamp-shadow | 5.5 / 6.5 | 4.6 / 5.8 | 5.5 / 7.1 | 6.8 / 8.1 |
  | imported-spawn | 16.3 / 18.1 | 12.4 / 13.6 | 15.1 / 16.1 | 14.5 / 16.0 |
  | imported-overlook | 20.0 / 21.2 | 12.2 / 13.2 | 15.7 / 17.2 | 17.3 / 18.7 |
  | imported-tower | 13.1 / 14.7 | 8.5 / 9.0 | 12.1 / 13.0 | 19.7 / 20.1 |
  | lightroom-center | 8.0 / 9.5 | 5.5 / 6.8 | 8.0 / 8.3 | 15.6 / 17.1 |

  The 144 Hz budget is 6.94 ms. Gaps between sessions under 15% are noise on
  that shared machine.
- **Atlas updates** (mean / p99 ms):
  - flights and teleports: ≤0.51 / ≤2.05;
  - imported-map edit storm: 1.43 / 2.67 at 1080p and 1.30 / 4.16 at 4K.
- **Culling** takes 0.09–0.12 ms on imported maps.
- **Actor overlay.** The overlay pass costs 0.02–0.04 ms on the GPU and
  0.1–0.2 ms of CPU planning. With 4 or 16 actors, shading adds 0.4–1.4 ms.
- **Not observed:**
  - the look in a native window;
  - lights popping or shimmering while moving;
  - actor shadows switching as lights enter or leave the top eight;
  - 144 Hz feel.
