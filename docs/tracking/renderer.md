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

## Later stages

- **Stage 2, many real lights:** point and spot lights with color, intensity,
  radius, cone, and a shadow mode (off, static, or dynamic) from the
  [world format](world-format.md). This completes most of the
  [lighting](lighting.md) outcome. Design decided after
  [research](../../research/many-lights.md):
  - Clustered forward shading with compute-built 256-bit light masks per cluster,
    and one shared lighting function for opaque geometry, props, water, and glass.
  - Water and glass switch to premultiplied alpha.
  - A paged shadow atlas: static caches keyed by tile and version, a small overlay
    for moving actors, and a per-frame update budget.
  - Three cached sun cascades.
  - 4× MSAA stays. No deferred shading and no radiance-hints global illumination.
  - Build order: lights and clusters, then the atlas, then cascades, then quality
    tiers that adapt to measured GPU time.

  The research file holds the budgets, fallbacks, and test plan. Until object
  properties land, the stage can be built and measured against synthetic lights.
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
