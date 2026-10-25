# Renderer

State: active.

## Outcome

Built worlds look good and stay fast: many real lights with shadows, HDR with
bloom, a proper sky, and later particles and decals, with smooth motion at any
refresh rate. Performance is measured on named hardware in both clients, not
assumed.

## Current behavior

- **Stage 1 is in place.** The scene renders in linear HDR with bloom, filmic
  tonemapping, Auto 4× MSAA, render scaling, cached static shadows, motion
  interpolation between fixed steps, and per-pass GPU timings in F3.
- **Lighting is still limited.** It is one sun plus the eight nearest light
  nodes, all with the same fixed warm downward 12 m cone ([lighting](lighting.md)).
- **Still constants:** sun direction, fog, field of view, and the 180 m far
  plane.

## Stage 1: render foundation

State: merged on 2026-10-25. The maintainer has not yet checked smoothness and
look on the 144 Hz 4K monitor, natively or in the browser.

- **Tools.**
  - The `capture` binary renders fixed camera poses headlessly to PNG with the
    real renderer, using `scripts/capture-poses.json`.
  - `scripts/profile-browser.mjs` measures the browser client in Playwright's
    Chromium.
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
  - The sun window (192 m, moving in 6 m steps) and local lights keep
    static-geometry shadow caches.
  - Any geometry or object rebuild invalidates them.
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

- **Large imported worlds stay slow.** They take 16–38 ms per frame at 4K even
  without MSAA. They are limited by geometry rather than pixels, which is a job
  for the [large-worlds](large-worlds.md) track.
- **Shadow invalidation is coarse.** Any geometry rebuild invalidates every cache.
  Stage 2's atlas should key caches by tile and version.
- **Untested hardware paths:** adapters without timestamps or MSAA, and non-Fifo
  native presentation.

## Stage 2: many real lights

State: active on branch `render-many-lights`.

Every light node in view lights the scene as a real light with shadows, instead
of only the nearest eight downward cones. Imported maps render with all of their
hundreds of lights. The design follows the [research](../../research/many-lights.md),
which holds the budgets and test plan.

Decisions:

- **Light model.**
  - Each light becomes a `LightDesc`: position, linear color, intensity, range,
    kind (point, or spot with direction and inner and outer cone), and a shadow
    mode (off, static, or dynamic).
  - Light nodes become **point lights** by default: warm `[1.0, 0.74, 0.46]`,
    12 m range, static shadows.
  - One function maps a `WorldObject` to a `LightDesc`. The world format's object
    properties will fill it from real values later.
  - Spot lights exist in the data and shaders now, exercised by tests and
    synthetic captures.
- **Clustered forward shading.**
  - A 16×9×24 view-cluster grid: slice 0 runs from the near plane to 4 m, then
    slices grow exponentially to the far plane.
  - A compute pass builds a 256-bit light mask per cluster from up to 256 visible
    lights, sorted by importance.
  - Light records are 64 bytes each, in a storage buffer.
  - One shared WGSL lighting function serves the world, props, and water and
    glass, with a soft cap of 16 lights per fragment.
  - Draw chunks front to back. No depth pre-pass in this stage.
- **Paged shadow atlas.**
  - A `texture_depth_2d_array` of 1024² D32F pages, each holding one tile size:
    4 × 512², 16 × 256², or 64 × 128².
  - Point lights use six tiles, with major-axis face selection and a two-texel
    guard band. Spot lights use one tile.
  - Tile size comes from projected radius, with ×1.5 hysteresis and 1 second of
    residency. The light being edited ranks first, and lights under 2 m radius
    are unshadowed.
  - Render one pass per dirty page, placing tiles with viewport and scissor and
    clearing a tile by drawing a depth-1 quad.
- **Static caches and an actor overlay.**
  - A tile re-renders only when its version changes: a light edit, or a chunk
    edit inside the light's sphere. Caches are keyed by tile and version,
    replacing stage 1's per-light caches.
  - Moving actors go into a small overlay page that lights with actors in range
    also sample.
  - At most 6 faces render per frame (12 for three frames after a teleport or
    join), capped at 1.5 ms by measured cost.
  - Stale tiles stay in use while waiting.
  - New lights fade in once their shadows are valid, and lights over budget fade
    out rather than leak light through walls.
- **Water and glass** use the shared lighting function with premultiplied alpha
  (`color·α + specular`), capped at specular-only with 8 lights.
- **Fallbacks.**
  - Keep fragment storage buffers at four or fewer and workgroups at 64, so
    compatibility-class adapters still run the main path.
  - Adapters without compute or storage buffers use uniform buffers with CPU-built
    64-bit masks and 64 lights.
  - Today's nearest-eight path stays as the last resort.

Not in this stage:
- sun cascades and quality tiers (stage 2b);
- editing light properties ([world format](world-format.md));
- importing light radius and color;
- sky (stage 3).

Parallel work: the shared-sandbox stage 1 fixes are being finished on another
branch. Leave `crates/server`, `crates/core`, and protocol types untouched; stay
within client rendering, `lighting.rs`, and the capture tool.

Acceptance:

- **Checks.** fmt, Clippy, all workspace tests, the native build, and a wasm
  `cargo check` pass.
  - Disk is tight: run release builds only for the capture binary, delete
    `target/release` afterwards, and skip `build-web.sh`.
- **Correctness.**
  - Clustered output matches a brute-force all-lights render, and compute-built
    masks match CPU-built ones, on synthetic scenes.
  - Shadow invalidation tests: an edit in range re-renders a tile, an edit out of
    range doesn't, and a moved light re-renders.
  - Every assembled shader validates with Naga.
- **Captures** with the stage 1 tool, kept under the worktree's `.local/captures/`
  and listed in Evidence:
  - the arena and the materials scene;
  - the largest imported map from three poses, before (eight cones) and after
    (all lights);
  - the research's synthetic scenes: 64 lights in a room around the camera, a
    corridor, a pose heavy with water and glass, and an edit storm that
    invalidates everything.
- **Measurements** on this Mac, natively, at 1080p and 3840×2160:
  - frame time and per-pass GPU time for the arena, the largest imported map, and
    the 64-light room;
  - shadow updates per frame and cache hit rate.

  Targets from the research: shadows ≤1.5 ms on average and ≤3 ms at p99; culling
  ≤0.1 ms; and the largest imported map at ≤14 ms p95 at 1080p on this Mac. If a
  target is missed, record by how much and why.
- **Not observable by the session:** feel in a native window and in the browser.
  List these checks precisely as not yet observed.

## Later stages

- **Stage 2b, sun cascades and quality tiers:** three cached sun cascades, and
  tiers that step render scale, then shadowed lights, then visible lights down
  from measured frame time.
- **Stage 3, sky and atmosphere:** sun, sky, and fog from world settings instead
  of constants.
- **Stage 4, effects:** particles and decals for building and combat feedback.

## Evidence

Stage 1, 2026-10-25, on this Mac's Apple M3 Pro at 3840×2160:

- **Checks.** fmt, Clippy, 279 workspace tests, and the native release and
  browser builds passed. The design lead re-ran fmt, Clippy, the tests, and a
  wasm check after adding Auto anti-aliasing.
  - A GPU regression test, run explicitly, matched cached and freshly rendered
    shadows after actor, terrain, light, and sun-window changes.
- **Captures.** Nine before/after pairs from identical poses, covering the arena,
  the materials scene, and the largest library map. They are kept outside the
  repository in `.local/captures/render-foundation/` in the main checkout.
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
- **Browser** (Chromium, 4K canvas), median frame interval: arena 8.30 → 8.30 ms;
  the_cube1m 24.10 → 12.80 ms.
- **Not observed:**
  - perceived smoothness at 144 Hz, mouse latency, and vsync on the actual
    monitor;
  - interaction in a native window;
  - shadow updates during continuous editing.
