# Renderer

State: active.

## Outcome

Built worlds look good and stay fast: many real lights with shadows, HDR with
bloom, a proper sky, and later particles and decals, with smooth motion at any
refresh rate. Performance is measured on named hardware in both clients, not
assumed.

## Current behavior

The 3D scene renders straight to the swapchain, with tonemapping and display
encoding inside each shader. There is no HDR target, post-processing, or
anti-aliasing. Lighting is one sun plus the eight nearest light nodes, all the
same warm downward 12 m cone ([lighting](lighting.md)), and all nine shadow maps
re-render every frame. The opaque world draws without back-face culling. Sun
direction, fog, field of view, and the 180 m far plane are constants. The camera
follows the 60 Hz simulation without interpolation, so motion steps visibly on
faster displays such as this Mac's 144 Hz monitor. F3 reports CPU timings only.

## Stage 1: render foundation

State: active on branch `render-foundation`.

Decisions:

- **Capture tool first.** Add a native headless capture mode that loads a world
  file, renders a list of fixed camera poses to PNG at a chosen resolution, and
  exits. Capture the current renderer before changing it and reuse the same
  poses afterwards. Cover the default arena, an imported map, underwater views,
  glass, emissive lamps, and shadowed areas.
- **HDR scene target.** Render the 3D scene to an offscreen `Rgba16Float` target
  with its own depth buffer. World, volume, and sky shaders output linear
  radiance only. One post pass applies exposure, a filmic tonemapper, and display
  encoding; egui draws afterwards on the swapchain.
- **Bloom** from the HDR target with a downsample and upsample chain, subtle by
  default, with a setting.
- **Anti-aliasing:** 4× MSAA on the scene where supported, resolved before post,
  with a setting to turn it off.
- **Render scale** setting for the 3D scene, for 4K displays and integrated GPUs,
  upscaled in the post pass.
- **Shared shader code:** one WGSL source for camera and light structures and the
  lighting functions, assembled into each shader at load time instead of copied.
- **Smooth motion:** draw the local player and other players interpolated between
  simulation steps using the accumulator remainder; mouse look stays per-frame.
  Add a vsync setting (Fifo, or Mailbox or Immediate where available) and use the
  lowest frame latency the surface allows.
- **Cheaper shadows:** cache static-geometry shadow maps per light and for the sun
  window, and re-render a cache only when geometry, the light, or the sun window
  changes. Draw moving actors over the cached static map each frame.
- **Back-face culling** for closed opaque geometry, with a fallback for any meshes
  that need both sides. Verify imported maps.
- **GPU timing:** per-pass GPU times in F3 from timestamp queries where the
  adapter supports them, and a clear "unavailable" otherwise.

Not in this stage: the light model itself (stage 2), sky (stage 3), particles and
decals (stage 4), and world-format changes. Lights keep today's fixed properties.

Parallel work: shared-sandbox stage 1 runs at the same time on another branch.
Do not modify `crates/server`, `crates/core`, or protocol types. In
`crates/client`, avoid connection and hosting code, and keep interpolation
changes in `game.rs` small and local.

Acceptance:

- fmt, Clippy, all workspace tests, and native and browser builds pass, including
  shader validation for every assembled shader.
- Before and after captures at identical poses show no lost shadows, transparency
  errors, or missing geometry; differences are the intended tonemapping, bloom,
  and anti-aliasing. Keep the images under `.local/` and list them in Evidence.
- Measured on this Mac (Apple M3 Pro, 3840×2160 at 144 Hz), natively and in the
  browser: frame time and per-pass GPU time for the arena and the largest library
  world, before and after.
- Observed: strafing and editor flight are smooth at 144 Hz. Selection outlines,
  guides, the first-person weapon, water and glass, underwater tint, and egui
  panels render correctly in both clients.

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

None yet for this outcome.
