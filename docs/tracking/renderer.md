# Renderer

State: active.

## Outcome

Built worlds look good and stay fast: many real lights with shadows, HDR with
bloom, a proper sky, and later particles and decals, with smooth motion at any
refresh rate. The renderer runs on Vulkan, DirectX 12 and Metal, and its lowest
quality setting runs smoothly on the minimum specification. Performance is
measured on named hardware, not assumed.

## Current behavior

- **Stages 1, 2 and 2b and N1 are in place.** The scene renders in linear HDR
  with bloom, filmic tonemapping, MSAA (Off, 2×, 4×, 8× where the adapter
  supports them, and Auto), render scaling, motion interpolation, and per-pass
  GPU timings in F3.
- **Quality levels** (Low, Medium, High, Ultra, Custom, and Auto, the default)
  set render scale, anti-aliasing and shadow detail, and nothing else: every
  level lights and shadows the same lights. Auto moves between levels from GPU
  frame time against the display's refresh interval.
- **The sun** has three cascades centred on the eye, reaching 15, 42 and 150 m,
  that don't shimmer as the eye moves or turns. A rebuilt chunk or object
  redraws only the texels it touches in each cascade.
- **Lights.** Every light node in view is a clustered point light, up to 256
  ranked by range over distance. A cached atlas shadows up to 149 point lights,
  and lights it can't hold fade out rather than leak through walls, so
  the_cube1m draws 149 of its 339. A fragment shades at most 16 of the lights
  that reach it, 8 on water and glass, most important first. Players cast
  shadows under the eight most important lamps.
- **GPU requests.** `Renderer::create` asks Vulkan, DirectX 12 and Metal
  adapters for the features and limits the renderer uses (`render/device.rs`;
  see N1 below), and refuses an adapter that lacks a required one with a
  message that names it and the minimum specification.
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
    added at merge after the 4K measurements below. N1 changed the rule and the
    choices: Off, 2×, 4× and 8× override Auto.
- **Shared shader code.** One WGSL module is assembled into each shader, and all
  five validate with Naga.
- **Cached shadows.**
  - The sun kept a static-geometry shadow cache in a single 192 m window, as
    local lights did until stage 2; 2b replaced it with three cascades, and any
    geometry or object rebuild redrew the whole window until then.
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
- **One `unsafe` block.**
  - `render/shaders.rs::trusted` compiles our shaders without wgpu's forced
    loop bounding; bounds checks stay on.
  - The reason: on Metal that bounding made the clustered loop about three times
    slower.
  - Its `SAFETY` comment explains why every shader loop is bounded.
- **Tooling.** F3 shows the light and shadow counts. The capture tool gained
  synthetic scenes (light room, corridor, water and glass, lamp hall) and flags
  for comparing paths.

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
- **The sun's static pass cost 7–9 ms** on the M3 Pro whenever its window or
  geometry changed. Stage 2b's cascades and chunk-scoped redraws replaced it.
- **The actor overlay's 15 tiles run out** with four or more actors in a
  crowded, lit room. The lowest-ranked of the eight lights then drop actor
  shadows on some faces. N2 replaces the overlay.
- **Not exercised:** the research's full sweeps of light counts, MSAA, and
  render scale.

## Next stages

The maintainer approved these on 2026-11-13 with the native-first plan for
[shared-sandbox](shared-sandbox.md) stage 2.

- **Order.** N1, 2b, N2 and N3 run one at a time, in that order, because each
  changes `render.rs`, the shadow code and the shaders. Each runs beside the
  other tracks' work orders:
  - N1 in wave 1;
  - 2b in wave 1 as well, starting when N1 merges;
  - N1b, the GPU tests on software drivers, parked with
    [platforms](platforms.md) stage 1 until releases are near;
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
    N1b adds a job for them once [platforms](platforms.md) stage 1 runs; both
    are parked until releases are near (maintainer, 2026-11-14).

### N1: native GPU features

State: merged on 2026-11-14. The maintainer hasn't yet checked the Settings menu
and F3 in the app, and nobody has run it on Vulkan or DirectX 12 hardware.

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
    8× isn't offered it. *Not observed in the app.* The unit tests
    (`settings_offer_only_the_sample_counts_the_adapter_supports`,
    `sample_counts_need_both_targets_and_a_resolve`) pass, and the GPU test
    `gpu_every_offered_sample_count_renders_the_same_scene` rendered
    1, 2, 4 and 8 samples on the M5 Pro and checked the sample count in the
    effective-settings text that F3 prints.
  - [ ] F3 shows the backend and driver after the adapter's name, and its
    lighting line no longer names a lighting path. *Not observed in the app.*
    The code and a unit test for the text
    (`the_adapter_description_names_backend_and_driver`) are in place; the
    capture tool's first output line and report show the same text, `Apple M5
    Pro (Metal)`.
  - [x] A unit test gives the adapter check limits and features without one
    that the renderer needs, and the message names it and the minimum
    specification (`a_missing_limit_is_named_with_the_minimum_specification`,
    `a_missing_downlevel_ability_is_named`).
  - [x] 4K GPU throughput on the M5 Pro for imported-spawn and arena-yard at
    Off, 2×, 4× and 8×, against 6.06 ms, from alternating runs. Auto's rule
    follows from these numbers. See the table below.
  - [x] On the M5 Pro, the `gpu_` tests pass when run with `--ignored`,
    including the brute-force and CPU-mask comparisons, and the capture tool
    renders its built-in scenes (all 30 poses of `scripts/capture-poses.json`
    at 1920×1080).
  - [x] Settings saved before N1 load, and every shader validates
    (`saved_antialiasing_choices_load`, `every_assembled_shader_validates`).
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

#### N1 results

Measured and written on 2026-11-14 on the maintainer's M5 Pro (Apple M5 Pro,
Metal).

- **Features and limits requested** (`render/device.rs`). Everything not
  named keeps wgpu's downlevel default, which Vulkan, DirectX 12 and Metal all
  guarantee, so the request is smaller than the `Limits::default()` it replaces.

  | Request | Value | Needed by |
  | --- | --- | --- |
  | Backends | Vulkan, DirectX 12, Metal | the minimum specification; OpenGL is gone, and `WGPU_BACKEND` still overrides |
  | `TIMESTAMP_QUERY` (optional) | where present | the per-pass GPU timings in F3 and the capture tool |
  | `TEXTURE_ADAPTER_SPECIFIC_FORMAT_FEATURES` (optional) | where present | 2× and 8× MSAA; without it wgpu allows only 1 and 4 samples |
  | Compute shaders (required) | | the light culling pass |
  | Anisotropic filtering (required) | | the material sampler (`anisotropy_clamp: 4`) |
  | 32-bit indices over their whole range (required) | | world meshes use `u32` indices |
  | `max_texture_dimension_1d/2d` | 8192, or the window's longer side if larger | scene targets and the surface |
  | `max_texture_array_layers` | 64 | the material arrays (`MAX_MATERIALS`); the shadow atlas needs 15 |
  | `max_bind_groups` | 3 | camera, lights and shadows, materials |
  | `max_storage_buffers_per_shader_stage` | 2 | the light list and cluster masks; the culling pass's light spheres and masks |
  | Compute workgroup | 64 × 1 × 1, 64 invocations | `clusters.wgsl` |

  The shader test `entry_points_stay_within_the_requested_limits` checks every
  entry point's storage buffers, uniform buffers, sampled textures, samplers
  and workgroup size against these limits, and
  `the_light_buffers_fit_the_requested_binding_sizes` checks the binding
  sizes. Choices where the order was open: the minimum 8192 texture size keeps
  an 8K display working on every adapter; the downlevel defaults stay for the
  limits the renderer doesn't push (for example 16 KiB uniform bindings, 128 MiB
  storage bindings, 256 MiB buffers). The five limits lowered below the
  downlevel defaults (array layers, bind groups, storage buffers and the
  workgroup) are the ones a shader test can check, so the request names what the
  renderer uses.
- **Sample counts offered on the M5 Pro:** 1 (Off), 2, 4 and 8, so Settings
  lists Auto, Off, 2×, 4× MSAA and 8×. The check requires both `Rgba16Float`
  (with resolve) and `Depth32Float` to allow the count.
- **4K throughput** (3840×2160, release build, `--throughput 120 --runs 5`,
  `--frames 1 --warmup 10`; milliseconds a frame, median over five alternating
  rounds of Off, 2×, 4×, 8×, with the range of the rounds' medians in
  brackets). The budget is 6.06 ms.

  | Pose | Off | 2× | 4× | 8× |
  | --- | ---: | ---: | ---: | ---: |
  | arena-yard | 2.43 (2.40–2.63) | 2.46 (2.45–2.67) | 2.53 (2.48–2.70) | 2.65 (2.63–2.69) |
  | imported-spawn | 5.99 (5.79–6.13) | 5.92 (5.89–6.34) | 6.11 (6.05–6.23) | 6.52 (6.35–8.15) |

  Other sessions were compiling throughout: the one-minute load average ran
  from 6 to 15 across the 20 invocations, and the first round was the slowest
  in every column. So differences under about 0.15 ms are noise. Read against
  Off, 2× costs nothing measurable, 4× about 0.1 ms, and 8× 0.2 ms on
  arena-yard and 0.5 ms on imported-spawn, which also puts 8× over the budget.
  Metal's tile memory makes multisampling cheap here; the earlier M3 Pro
  serial measurement (4× at 4K 11 ms against 5 ms without) and every
  immediate-mode GPU would pay more.
- **Auto's rule** (`Antialiasing::wanted`): 4× while the scene has at most
  2560×1440 pixels, 2× up to 3840×2160, none above, and never 8×, falling to the
  highest supported count at or below. 4× stays the choice up to 1440p, where
  it's cheap on every GPU measured; the measured 4K costs don't justify
  stretching it to 4K for hardware nobody has measured, so 4K gets 2×, which
  was free here and costs less than 4× on any GPU; 8× is a choice for GPUs that
  have time to spare. A later stage can replace the rule with one that reacts to
  measured frame times. The rule is from this table and the M3 Pro's old
  numbers only; nobody has run it on a reference machine.
- **Fixed GPU memory** (MiB, computed from the formats, not read back from the
  driver, so Metal's padding is not included). Scene targets are the
  single-sample HDR image, the multisampled HDR and depth targets, and the five
  bloom levels (`post::target_bytes`, with a unit test):

  | Scene size | Off | 2× | 4× | 8× |
  | --- | ---: | ---: | ---: | ---: |
  | 1280×800 | 14.3 | 33.9 | 57.3 | 104.2 |
  | 1920×1080 | 29.0 | 68.5 | 116.0 | 210.9 |
  | 3840×2160 | 116.0 | 274.2 | 464.0 | 843.7 |

  Besides the targets: the shadow atlas, 15 layers of 1024² `Depth32Float`,
  60 MiB; the sun's working shadow map and its cache, 2048² each, 32 MiB; the
  material arrays, 40.0 MiB (`texture_bytes`, with mips); and the light, mask,
  camera and timestamp buffers, under 0.2 MiB together. That is 132 MiB that
  doesn't depend on the scene size, so 4K with 4× is about 596 MiB, close to
  shared-sandbox's 0.6 GiB, and 4K with 8× is about 976 MiB. World meshes and
  egui are not included.
- **Deleted:** `LightingTier` (Clustered, Uniform, Nearest) and its
  `config()`, replaced by one `LightConfig` with a `Default`; `lighting_tier`
  (the function and `Renderer::lighting_tier`); `UNIFORM_BINDINGS`, the
  `//LIGHT_STORAGE` placeholder and `STORAGE_BINDINGS` (the storage bindings now
  sit in `common.wgsl`), `LightLayout::storage`, `mask_vectors` and `MASK_VECS`;
  the uniform branch of `LightGpu` and its CPU mask upload (`cpu_masks`);
  `ClusterGrid::COMPACT`; `RendererOptions::tier`; the capture tool's `--tier`
  flag and the report's `lighting_tier` (it reports `sample_counts` instead);
  `fallback_buffers_fit_the_smallest_guaranteed_uniform_binding`,
  `gpu_uniform_tier_matches_the_clustered_tier_without_shadows`, and the
  compact cases in the cluster and light-set tests (the over-budget test now
  sets `max_shadowed: 4`). `entry_points_stay_within_compatibility_limits`
  became `entry_points_stay_within_the_requested_limits`.
  `ClusterGrid::build_masks` stays. F3's lighting line no longer starts with a
  path name.
- **Also changed:** the capture tool's `--throughput N` and `--runs R`
  (documented in the development guide); `Antialiasing::On` is now `X4`, saved
  as `x4` with `on` still read; the capture report adds `adapter_description`,
  `material_texture_bytes` and each pose's `throughput_ms_per_frame`, and its
  first line shows how long the renderer took to create (145–185 ms at
  1920×1080, with pipelines for all four sample counts built up front).
- **Checks run** from `/Users/zach/gits/sauerteig-worktrees/renderer-n1`, after
  `. ./scripts/env.sh`:
  - `cargo fmt --all`;
  - `cargo clippy --workspace --all-targets --locked -- -D warnings`;
  - `cargo test --workspace --locked`;
  - `cargo test -p sauerteig-client --lib gpu_ -- --ignored --test-threads=1`
    (six tests, including the new sample-count test) on the M5 Pro;
  - `target/release/capture --poses scripts/capture-poses.json --output ...
    --width 1920 --height 1080 --frames 5 --warmup 5 --throughput 30 --runs 2`,
    which rendered all 30 poses.
- **Not observed:**
  - the Settings menu and F3 in a running window (no one could see one);
  - a saved `8×` setting loaded on an adapter without 8×, beyond the unit
    test of the fallback;
  - any adapter other than the M5 Pro, so Vulkan and DirectX 12 behavior,
    their sample counts, and the error text on a real failing adapter are only
    unit-tested;
  - the memory table against Metal's own allocation.

### N1b: GPU tests on software drivers

State: parked with [platforms](platforms.md) stage 1, which owns
`.github/workflows/`, until releases are near (maintainer, 2026-11-14). Small to
medium. The M5 Pro, where sessions run, has no
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

State: merged on 2026-11-14. The maintainer hasn't seen it in the app, and
nobody has run it on a reference machine or on Vulkan or DirectX 12 hardware.

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
  - [x] On the M5 Pro, a one-block edit inside the sun's range on a copy of
    the_cube1m re-renders only part of a cascade: 3.05 ms of sun passes before
    and 0.18 ms after beside the spawn, 3.5 to 4.3 ms before and 0.95 ms after
    in the tower. See the results below.
  - [ ] Captures of the arena, materials and imported poses show sharper near
    shadows, with no gaps or doubled shadows at cascade boundaries. Flying
    across the_cube1m in the app shows no shimmering at cascade edges. *Not
    observed in the app.* The session looked at stills of the arena, materials
    and imported poses from before and after and saw no seams, but none was
    placed on a cascade boundary on purpose, and nobody could fly.
  - [x] At every level, F3 shows the same lights drawn and shadowed on
    imported-spawn, lightroom-center and lightroom-actors-16. Checked from the
    capture report's lighting counts at all four levels (table below); F3
    itself was not looked at.
  - [ ] On the M5 Pro at 4K and 165 Hz, Auto steps down where the default
    level misses 6.06 ms and steps back up afterwards, without flickering.
    *Not observed in a live window.* The capture tool drove Auto along
    imported-flight and a longer path at 4K and 165 Hz and did exactly that
    (see below), but with serial frames, not a presented window.
  - [x] The lowest level's GPU throughput on the M5 Pro at 1280×800 and
    1920×1080 is recorded for the scenes below, for comparison with a
    tester's.
  - [ ] **Needs a tester** ([platforms](platforms.md) stage 3): on a reference
    machine, the lowest level holds 60 frames a second (16.7 ms at the 95th
    percentile) at the machine's native resolution, 1280×800 on a Steam Deck.
    Use the benchmark world's capture poses and the capture tool's lighting
    scenes (lightroom, corridor, waterglass, lamphall), and record the
    machine, its system, backend and driver, and the capture report. The check
    stays open until someone runs it. No tester was available.
  - [x] Tests: cascade matrices don't move while the eye moves within a texel
    or turns; cached cascades match fresh renders after edits, actor moves
    and level changes, extending the GPU test in `capture.rs` that compares
    cached shadows with fresh ones; Auto's choices, with hysteresis. The GPU
    tests pass on the M5 Pro; N1b's job doesn't exist yet. Test names are in
    the results.

#### 2b results

Measured and written on 2026-11-14 on the maintainer's M5 Pro (Apple M5 Pro,
Metal), release builds. Other sessions were compiling throughout, so the
one-minute load average at the start of each measured invocation is given.

- **What changed.**
  - `sun_cascades` in `lighting.rs` replaces `sun_shadow_matrix`. Each cascade
    is a square window centred on the eye that reaches 15, 42 and 150 m in
    every direction, so it depends on the eye's position and never on where the
    eye looks. It moves in steps of 64 texels, with its depth origin snapped
    the same way, which keeps a cached map valid while the eye walks inside a
    step, and each window is the split divided by 1 − 64/size wide, so the
    circle of the split fits however the eye sits in its step. At 1024² that
    is 3.1, 8.8 and 31 cm a texel.
  - `render/sun.rs` holds the maps, the three cascades' caches and the planner.
    The working maps are one three-layer depth array that the shaders sample;
    the near two cascades draw into a two-layer cache that is copied, layer by
    layer, over their working layers each frame, and the moving actors draw
    over the copy. The far cascade draws straight into its working layer.
    Every copy is a whole layer, which DirectX 12 allows for depth.
  - A rebuilt chunk marks the texels its changed triangles reach in each
    cascade, and the planner turns them into at most four scissor rectangles
    (one around everything past four). A cascade whose window moved, or that
    256 changes reached, redraws whole. An edit clears its rectangles by
    drawing a far-depth triangle and redraws what overlaps them.
  - `render/changes.rs` finds where a chunk's mesh changed. See the choice
    below.
  - Objects: the sun diffs the object list each frame (`ObjectDigest`, shared
    with the atlas) and marks 10 m around each object that appeared, moved or
    went. `Renderer::objects` no longer invalidates anything by itself.
  - `common.wgsl` has `sun_visibility`, which samples one cascade with the
    3 × 3 filter and a receiver-plane depth gradient as before, offsets the
    receiver along its normal by 0.13 texels of that cascade (4 mm at least),
    and blends front to back across each cascade's outer tenth. `world.wgsl`
    no longer computes a light position in the vertex stage, and the single sun
    pipeline and its `vs_shadow` are gone.
  - `render/settings.rs`: `Quality` (Auto, Low, Medium, High, Ultra, Custom),
    `QualityLevel`, `ShadowDetail` and `RenderSettings::resolved`. Saved
    settings gained `quality` and `shadows`; a file without `quality` loads as
    Auto when it has the old default scale and anti-aliasing, and as Custom
    otherwise, so nobody's saved choice is overridden.
  - `render/quality.rs` is Auto's arithmetic, without a GPU.
    `render/timing.rs` also reports the span of a frame's timed passes. The
    Settings menu has Quality, Anti-aliasing, Shadow detail and Render scale;
    choosing a level fills the three, and changing one by hand shows Custom.
    F3's render line shows the sun's size and the quality.
  - `render/lightset.rs`: `set_max_tile_class`, so a level's shadow detail
    bounds the atlas's largest tile. `capture.rs`: `block_edit` poses,
    `--display-hz`, the report's `auto_level_changes`, `sun_cascade_bytes`
    and the chunk fingerprint figures.
- **Choices where the order was open.**
  - **Cascades centred on the eye, not fitted to the view's slice.** A fitted
    window moves when the eye turns, which the test requires it not to, and the
    research's 3, 8 and 29 cm a texel come out of the eye-centred circles.
  - **Changed regions come from a fingerprint, not from chunk bounds.** The
    order asked for the old and new bounds of each rebuilt chunk, and the first
    version did that. It made the tower edit slower, 4.0 ms to 6.0 ms: a
    chunk of the_cube1m is about 250 KB of vertices (2392 chunk meshes), so
    every chunk a rectangle touches is drawn whole in all three cascades, and
    the rectangle of an 8 m chunk touches a column of them. Now each rebuilt
    chunk is fingerprinted from its triangles' positions on a 4 × 4 × 4 grid
    of 2 m cells, 512 bytes, and only the cells whose sum changed count. It
    reads the vertices the mesher produced, so octree O1a and its new mesh keys
    don't touch it, and a triangle that reaches outside its chunk's cube is
    tracked separately. Fingerprints take 1.2 MiB for the_cube1m and 23 ms of
    its 1.8 s load.
  - **Shadow detail changes only the largest atlas tile** (128², 256² or 512²).
    The atlas gives every light that fits at the smallest size a tile before
    any light gets a larger one, so the number of shadowed lights doesn't depend
    on it; a test in `render/lightset.rs` checks that with 240 lights,
    including a change at run time.
  - **Levels.** Auto starts at High, which is about what the default was.
    Low is the only level below full scale; the others differ in anti-aliasing
    and shadow detail. The levels were not tuned beyond the measurements below.
  - **Actors cast into the near two cascades only** (to 42 m), as the order
    says.
  - **Auto needs a known refresh rate.** With no window, or a monitor that
    reports none, it stays put, so captures stay at High unless `--display-hz`
    is given. It reads the window's monitor every 240 frames.
- **Sun passes for a one-block edit** (3840×2160, GPU time of the sun's static
  passes in the frame after the edit, median of six edits and six undos; the
  edit toggles a cell of a copy of the_cube1m; three alternating rounds of
  before and after, load average 3.8 to 8.3):

  | Edit | Before | After | Maps rendered in the frame |
  | --- | ---: | ---: | --- |
  | beside the spawn, cell 96, 14, 32 | 3.04–3.07 ms | 0.18–0.19 ms | 1 → 3 |
  | in the tower, cell −25, 59, 30 | 3.46–4.30 ms | 0.92–0.96 ms | 7 → 9 |

  "Before" is main at 1564597 with the same capture harness. The after frames
  render three cascade rectangles rather than one window; the GPU test
  `gpu_sun_cascades_match_fresh_renders_after_edits_moves_and_level_changes`
  counts 16,124 texels redrawn of 3,145,728 for one block in the materials
  scene. The M3 Pro's 7–9 ms for the old pass isn't comparable.
- **Cascade memory** (one three-layer working array and a two-layer cache of
  32-bit depth, `sun_cascade_bytes`): 5 MiB at 512², 20 MiB at 1024², 80 MiB at
  2048². The single window it replaces held 32 MiB (2048², map and cache), so
  High is 12 MiB lighter and Ultra 48 MiB heavier. The 132 MiB of fixed memory
  in the N1 results becomes 120 MiB at High (the sun's 32 MiB becomes 20),
  105 MiB at Low and 180 MiB at Ultra.
- **Levels and 4K throughput** (3840×2160, release, `--throughput 120 --runs
  5`, `--frames 1 --warmup 10`; milliseconds a frame, median over three
  alternating rounds with the range of the rounds' medians; load average 2.7
  to 3.8 at the start of each invocation). Budget 6.06 ms at 165 Hz. "Main" is
  1564597 at its defaults, which at 4K are 2× MSAA, scale 1 and one 2048²
  sun window.

  | Level | Scale, AA, sun, tile | arena-yard | imported-spawn | imported-overlook | lightroom-center |
  | --- | --- | ---: | ---: | ---: | ---: |
  | Main | 100%, 2×, 2048² | 2.39 (2.38–2.43) | 5.77 (5.61–5.88) | 8.75 (8.45–8.77) | 9.05 (8.72–9.22) |
  | Low | 75%, Off, 512², 128² | 1.49 (1.43–1.52) | 4.42 (4.20–4.49) | 5.79 (5.57–5.80) | 5.39 (5.16–5.48) |
  | Medium | 100%, 2×, 1024², 256² | 2.45 (2.42–2.51) | 5.40 (5.19–5.53) | 8.49 (8.17–8.60) | 9.29 (9.26–9.47) |
  | High | 100%, 4×, 1024², 512² | 2.60 (2.59–2.63) | 5.58 (5.57–5.59) | 8.93 (8.86–8.93) | 9.50 (9.19–9.51) |
  | Ultra | 100%, 8×, 2048², 512² | 2.75 (2.71–2.83) | 6.14 (5.93–6.19) | 9.72 (9.59–9.77) | 9.75 (9.49–9.79) |

  Differences under about 0.2 ms are noise on this machine. At 4K every level
  fits 6.06 ms on arena-yard. imported-spawn fits at every level but Ultra
  (6.14 ms), though Medium and High are over the 5.45 ms (90%) at which Auto
  steps down. imported-overlook and lightroom-center miss at Medium, High and
  Ultra and fit only at Low (5.79 and 5.39 ms). On Metal the levels cost
  mostly pixels (Low's 0.56×) and shadow detail, not anti-aliasing.
- **Lowest level, for a tester to compare** (Low; milliseconds a frame, median
  over three rounds, release, `--throughput 120 --runs 5`; High beside it;
  load average 2.2 to 3.2). Low draws at 75% of the window, which is 1440×810
  and 960×600 here.

  | Scene | 1920×1080 Low | 1920×1080 High | 1280×800 Low | 1280×800 High |
  | --- | ---: | ---: | ---: | ---: |
  | arena-yard | 0.47 | 0.76 | 0.33 | 0.48 |
  | imported-spawn | 3.36 | 3.83 | 3.25 | 3.56 |
  | imported-overlook | 3.29 | 4.22 | 2.91 | 3.40 |
  | lightroom-center | 1.53 | 2.70 | 0.99 | 1.49 |
  | corridor-view | 1.01 | 1.41 | 0.64 | 0.98 |
  | waterglass-view | 0.95 | 1.42 | 0.65 | 1.03 |
  | lamphall-actor | 1.06 | 1.34 | 0.52 | 0.98 |

  The imported poses barely drop with the window: the_cube1m is limited by
  geometry (vertex work), not pixels, and the M5 Pro is far faster than the
  minimum specification. These are an M5 Pro's numbers, not a prediction for a
  Steam Deck.
- **Same lights at every level** (1920×1080, capture report): imported-spawn
  draws and shadows 149 of 339 (333 in view, 107 over the atlas's budget, 8
  shadow actors); lightroom-center and lightroom-actors-16 draw and shadow 44
  of 64 in view; at Low, Medium, High and Ultra alike.
- **Auto along the imported-flight path** (capture tool, `--display-hz`, GPU
  time per frame from timestamps, serial frames, levels listed as the one at
  the start of the measured frames, then each change with its frame):

  | Display and size | imported-flight (788 frames) |
  | --- | --- |
  | 165 Hz, 3840×2160 | Medium, then Low at frame 25 and no more |
  | 165 Hz, 1920×1080 | Medium, no change |
  | 60 Hz, 3840×2160 | High, then Ultra at frame 251 |

  Auto starts each pose at High, so at 4K and 165 Hz it had already left High
  during the settling frames before the walk began. A longer path from the same
  world, overlook for 900 frames, then a view of the sky for 900, then the
  overlook again (2,700 frames), at 165 Hz and 4K: Medium, Low at 36, Medium at
  1282, High at 1696, Medium at 1830 and Low at 1924. That is a step down while
  the view is heavy, one level up about 380 frames after the view cleared
  (the window it needs is 360 frames), another about 410 frames later, and two
  steps down within 125 frames of the heavy view returning, with no change in
  between. At 1080p and 165 Hz the same path went High, then Ultra at 1222.
  The frame-interval path (no timestamps) was only unit-tested.
- **Redraws while walking** (imported-flight at 4K, High, 788 frames, GPU time
  of each cascade's pass in the frames it ran): cascade 1 in 110 frames (0.38
  ms median, 2.2 ms at most), cascade 2 in 38 frames (2.3 ms, 3.9 ms at most),
  cascade 3 in 11 frames (3.5 ms, 5.1 ms at most). The single window redrew
  every 6 m for 3 to 4 ms where the edit table measured it. All three
  cascades can redraw in one frame.
- **Checks run** from the worktree `renderer-sun-quality`, after
  `. ./scripts/env.sh`:
  - `cargo fmt --all`;
  - `cargo clippy --workspace --all-targets --locked -- -D warnings`;
  - `cargo test --workspace --locked` (205 client tests pass, 8 ignored);
  - `cargo test -p sauerteig-client --lib gpu_ --locked -- --ignored
    --test-threads=1` (8 pass on the M5 Pro, including
    `gpu_sun_cascades_match_fresh_renders_after_edits_moves_and_level_changes`
    and `gpu_actors_shadow_the_near_cascades_and_nothing_stays_behind`);
  - the captures above with `target/release/capture` and `block_edit` poses.
  - New unit tests, in `lighting.rs`:
    `every_cascade_holds_the_ball_around_the_eye_that_it_promises`,
    `cascade_windows_move_in_whole_steps_and_ignore_where_the_eye_looks` and
    `the_far_cascade_holds_the_whole_world_with_positive_depth_and_padding`;
    the planner's in `render/sun.rs`; the fingerprint's in `render/changes.rs`;
    Auto's in `render/quality.rs` (steady, down, up, a spike, convergence from
    every start, failed tries thinning out, frame intervals); the settings
    tests for old files and levels; and in `render/lightset.rs`
    `the_largest_shadow_tile_never_changes_which_lights_are_lit_or_shadowed`.
- **Not observed:**
  - the Settings menu, F3, a level change or Auto in a running window, and the
    monitor's refresh rate as winit reports it;
  - flying across the_cube1m for shimmering, and any cascade boundary placed on
    purpose in a still;
  - Auto's frame-interval path on real hardware, and any adapter other than the
    M5 Pro;
  - the cascade memory against Metal's own allocation;
  - the lowest level on a reference machine.
- **Gaps.**
  - **Shadows 42 to 96 m away are coarser than before** at High and below:
    31 cm a texel at 1024² against the old window's 9.4 cm (16 cm at Ultra).
    The old window had finer texels at that range and coarser near the eye.
  - **A cascade whose window steps redraws whole**, and the far one costs 3 to
    5 ms when it does (see above). Spreading that over frames, or scrolling the
    cache, would flatten the spike; depth copies must be whole subresources.
  - Actors cast no sun shadow past 42 m, and the sun is unshadowed past 150 m
    (the far plane is 180 m).
  - Low at 4K takes 5.79 ms on imported-overlook, under 6.06 ms but over the
    5.45 ms at which Auto steps down. Low is the lowest level, so Auto stays.
- **Record at the end:** the tester's results, or that none were available:
  none were.

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
    job on lavapipe and WARP once it runs.
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
    the M5 Pro and in N1b's job on lavapipe and WARP once it runs.
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
