# Renderer

State: active.

## Outcome

Built worlds look good and stay fast: many real lights with shadows, HDR with
bloom, a proper sky, and later particles and decals, with smooth motion at any
refresh rate. Performance is measured on named hardware in both clients, not
assumed.

## Current behavior

- **Stages 1 and 2 are in place.** The scene renders in linear HDR with bloom,
  filmic tonemapping, Auto 4× MSAA, render scaling, motion interpolation, and
  per-pass GPU timings in F3.
- **Lights.** Every light node in view is a clustered point light, with shadows
  from a cached atlas for up to 149 lights. Players cast shadows under the eight
  most important lamps.
- **Still constants:** light color and range (until object properties land), sun
  direction, fog, field of view, and the 180 m far plane.

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

- **Large imported worlds stay slow.** They took 16–38 ms per frame at 4K even
  without MSAA; stage 2's shader change brings the same poses to 11–20 ms with 149
  real lights. They are limited by geometry more than pixels, which is a job for
  the [large-worlds](large-worlds.md) track.
- **Sun shadow invalidation is coarse.** Any geometry rebuild invalidates its
  cache. Stage 2 keys local light caches by tile and version; the sun's cascades
  (stage 2b) should too.
- **Untested hardware paths:** adapters without timestamps or MSAA, and non-Fifo
  native presentation.

## Stage 2: many real lights

State: merged on 2026-10-25. The maintainer has not yet looked at it natively or
in the browser.

- **Light model.**
  - Light nodes are warm point lights: 12 m range, intensity 16, which keeps mean
    image brightness within 3% of stage 1 on the arena and spawn poses.
  - Each becomes a `LightDesc` through `light_from_object`. Spot lights work in
    data and shaders but have no editing UI yet.
- **Clustered forward shading.**
  - A 16×9×24 cluster grid with compute-built 256-bit masks over up to 256
    visible lights, ranked by range over distance.
  - One shared lighting function serves terrain, props, water, and glass, with up
    to 16 lights per fragment.
  - Water and glass use premultiplied alpha.
- **Shadow atlas.**
  - 14 pages (60 MiB with the overlay page), holding 149 point lights at the
    smallest tile size. Lights beyond that fade out rather than leak through
    walls.
  - Static caches are keyed by tile and version.
  - The eight most important shadowed lights also sample an actor overlay, so
    players cast shadows under nearby lamps.
  - Filling the atlas after a join takes about 2.5 seconds, nearest lights first.
  - At least one face renders each frame.
- **Fallbacks.** A uniform-buffer path and today's nearest-eight path are
  settings of the same shaders.
- **One `unsafe` block.**
  - `render/shaders.rs::trusted` compiles our shaders without wgpu's forced
    loop bounding; bounds checks stay on.
  - The reason: on Metal that bounding made the clustered loop about three times
    slower.
  - Its `SAFETY` comment explains why every shader loop is bounded.
  - WebGPU ignores the option.
- **Tooling.** F3 shows the lighting path and the light and shadow counts. The
  capture tool gained synthetic scenes (light room, corridor, water and glass,
  lamp hall) and flags for comparing paths.

Gaps:

- **Busy 4K scenes can't hold 144 Hz.** Dense lights cost 1.6–1.95× stage 1 at
  4K, for example the imported tower at 19.7 ms. Stage 2b's quality tiers are
  the lever. At 1080p, stage 2 is faster than stage 1.
- **Imported maps draw 149 of their 339 lights.** Lights don't have
  per-light brightness until object properties arrive, and the 16-light cap per
  fragment can bind in dense clusters.
- **Some targets are borderline.**
  - The largest imported map's p95 at 1080p is 13.2–13.6 ms against a 14 ms
    target, and some earlier sessions missed it.
  - An edit storm at 4K has a 4.16 ms p99 for atlas updates, against a 3 ms
    target.
- **The sun's static pass costs 7–9 ms** whenever its window or geometry changes.
  Stage 2b's cascades address it.
- **The actor overlay's 15 tiles run out** with four or more actors in a crowded,
  lit room. The lowest-ranked of the eight lights then drop actor shadows on some
  faces.
- **Not exercised:**
  - the uniform and nearest paths, except when forced on this Mac;
  - browser frames (the shaders do compile in Chrome 155);
  - the research's full sweeps of light counts, MSAA, and render scale.

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

Stage 2, 2026-10-25, on this Mac's Apple M3 Pro. Captures and raw numbers are
kept outside the repository in `.local/captures/render-stage2/` in the main
checkout.

- **Checks.**
  - fmt, Clippy, the workspace tests (363, plus 6 GPU tests run explicitly), the
    native builds, and the wasm check passed for the implementer and again in the
    design lead's review.
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

  The 144 Hz budget is 6.94 ms. Gaps between sessions under 15% are noise on this
  shared machine.
- **Atlas updates** (mean / p99 ms):
  - flights and teleports: ≤0.51 / ≤2.05;
  - imported-map edit storm: 1.43 / 2.67 at 1080p and 1.30 / 4.16 at 4K.
- **Culling** takes 0.09–0.12 ms on imported maps.
- **Actor overlay.** The overlay pass costs 0.02–0.04 ms on the GPU and 0.1–0.2 ms
  of CPU planning. With 4 or 16 actors, shading adds 0.4–1.4 ms.
- **Not observed:**
  - the look in a native window or in the browser;
  - lights popping or shimmering while moving;
  - actor shadows switching as lights enter or leave the top eight;
  - 144 Hz feel.
