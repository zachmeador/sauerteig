# Many-lights renderer design

Lighting architecture for [renderer](../docs/tracking/renderer.md) stage 2: many
point and spot lights placed live by builders, on WebGPU in the browser and on
native, including integrated GPUs. A research agent compiled this on 2026-10-25.
Budgets are design targets, not measurements. **[U]** marks unverified claims and
estimates.

## Recommendation

**Clustered forward shading:**
- compute-built clusters, each holding a 256-bit light mask;
- one shared WGSL lighting function for opaque geometry, props, and water and
  glass;
- light data moved out of today's 944-byte camera uniform.

**Shadows:**
- a paged shadow atlas with static caches and a budgeted update queue;
- a three-cascade, cached sun.

**Keep 4× MSAA.** Skip deferred shading and radiance-hints global illumination.

**Build order:**
1. The light module and clusters.
2. The shadow atlas and caching.
3. Sun cascades.
4. Quality tiers that adapt to measured GPU time.

## Clustered forward versus deferred

- **Limits don't decide it.**
  - WebGPU's defaults allow eight storage buffers per stage. Compatibility mode
    allows none in vertex shaders and four in fragment shaders.
  - Clustering needs about three fragment storage buffers and a 54-workgroup
    dispatch. Both designs fit within `Limits::default()`.
- **MSAA favors forward.**
  - WebGPU offers only 1 or 4 samples.
  - A 4× deferred G-buffer at 16 bytes per sample is 133 MB at 1080p, which is up
    to 16 GB/s of writes and reads at 60 fps **[U]**.
  - The literature agrees: Olsson found clustered forward competitive with tiled
    deferred while keeping MSAA and custom materials, and Persson notes that
    MSAA, transparency, and depth discontinuities all just work in forward.
- **Transparency favors forward too.** Water and glass need a lit forward pass
  anyway, so deferred would mean two shading paths.
- **Tile GPUs** (Apple, Arm) penalize deferred unless the G-buffer stays in tile
  memory, which WebGPU cannot express. For those GPUs:
  - use MSAA with `StoreOp::Discard` plus resolve;
  - skip the depth pre-pass on Apple;
  - avoid `LoadOp::Load` on large targets.
- **Use 3D clusters, not 2D tiles.**
  - Clusters have no depth dependency, transparent surfaces use the same lookup,
    and the grid later serves decals.
  - There's no occlusion culling yet, so sort chunks front to back. Add a depth
    pre-pass only on non-Apple adapters if timestamps justify it.

## Shadows

- **Paged atlas.**
  - A `texture_depth_2d_array` of 1024² D32F pages, each holding one tile size:
    4 × 512², 16 × 256², or 64 × 128².
  - WebGPU depth copies must cover a whole subresource, so copying sub-rectangles
    within one large atlas is illegal.
  - Instead, render one pass per dirty page, placing tiles with viewport and
    scissor, and clear a tile by drawing a depth-1 quad.
- **Importance.**
  - Size tiles by projected radius with ×1.5 hysteresis and 1 second of
    residency, and serve the light being edited first.
  - Tesseract sizes tiles as `clamp(radius·k/√max(1, dist/radius), 96, 384)` px.
- **Point lights.**
  - Six atlas tiles per light, with major-axis face selection, a two-texel guard
    band, and culled faces.
  - Reject dual-paraboloid maps: they need tessellated casters, and large flat
    cube leaves are their worst case.
- **Static plus overlay.**
  - Re-render a tile only when its version changes: a light edit, or a chunk edit
    inside its sphere.
  - Moving actors go into a small overlay page, and lights with actors in range
    sample both maps.
- **Update budget.**
  - At most 6 faces per frame (12 for three frames after a teleport or join),
    capped at 1.5 ms by measured cost.
  - Stale tiles stay in use while waiting.
  - New lights fade in once their shadows are valid, and lights over budget fade
    out rather than leaking light through walls unshadowed.
- **Sun.**
  - Three cascades split by distance from the eye: about 15, 42, and 150 m
    (PSSM λ = 0.75). They are rotation-invariant and texel-snapped.
  - 1024² each, totaling 12 MiB (16 MiB today).
  - Estimated texel sizes are 3, 8, and 29 cm (9.4 cm today), re-rendered at about
    3, 1, and 0.3 Hz **[U]**. Actors overlay the first two cascades.

## What to take from Tesseract

- **Skip** its tiled deferred shading: an RGBA8 G-buffer, at most 10 × 10 tiles,
  and eight lights per pass, designed before compute shaders. Clustering replaces
  it.
- **Borrow** its atlas design.
  - A rectangle packer, with cached entries reserved first.
  - Cache keys from origin, radius, and direction.
  - An entry is cached only if no dynamic entity was drawn into it.
  - Per-light shadow flags. Add `shadow: off | static | dynamic` to light
    properties; imports default to static.
- **Skip** radiance hints. They are sun-only global illumination (about 48 m of
  coverage), don't light builders' rooms, and depend on sun settings.
- **Borrow** its large-scale habits: O(N) per-frame light collection, scissors,
  sorting, and occlusion queries for large shadowed lights. Its sun shadows reach
  only about 128 m.

## Reference code and traps

- **Bevy:** use it as the reference for clustering, not shadows.
  - It does clustering on the GPU (needs storage buffers) or the CPU.
  - Its uniform fallback holds up to 4,096 clusters and 256 lights.
  - It uses cube-array point shadows with no atlas.
- **wgpu's `shadow` example** chooses a storage or uniform light buffer from
  `max_storage_buffers_per_shader_stage`, with separate entry points.
- **Trap:** `ptr<storage>` function parameters fail in Naga and Firefox. Pass
  indices instead.

## Budgets (1080p, 4× MSAA; tunable)

| | Default (integrated GPU) | Fallback (uniform buffer ≤16 KiB) |
| --- | --- | --- |
| Visible lights | 256, sorted by importance | 64 |
| Cluster grid | 16 × 9 × 24 = 3,456 (120 px tiles; slice 0 covers 0.06–4 m, then 23 exponential slices to 180 m) | 12 × 7 × 16 |
| Cluster record | 256-bit mask, 110 KB | 64-bit mask, 11 KB |
| Light record | 64 B: position and range, color × intensity and flags, direction and packed cone, shadow tile and bias; 16 KB total | 4 KB |
| Lights per fragment | 16 (soft cap) | 8 |
| Shadowed lights | 12 per frame; 120 resident faces in 6 pages (24 MiB) | 4; 2 pages |
| Tile size by projected radius | ≥400 px: 512²; 120–400: 256²; 40–120: 128² | ≤256² |
| Shadow updates | ≤6 faces per frame | ≤2 |
| Sun | 3 × 1024² cascades | 1 × 2048² (today) |

Notes on the budgets:
- **Why masks.**
  - With at most 256 visible lights there are no atomics, prefix sums, or list
    overflow.
  - Truncating by rank degrades gracefully.
  - Dense imported maps may put 100–250 lights in view **[U]**.
  - Culling runs one thread per cluster, about 0.9 million sphere tests, with
    radii inflated slightly so slice rounding can't drop a light.
- **Cost.**
  - About 70 FLOP per unshadowed light per pixel, so four overlapping lights cost
    about 3 ms on a 0.4 TFLOPS integrated GPU at 50% efficiency **[U]**.
  - At most four compare taps per shadowed light, using
    `textureSampleCompareLevel` (the plain form needs uniform control flow).
- **Water and glass.**
  - They use the same function with the fragment's own cluster.
  - Switch them to premultiplied alpha (`color·α + specular`). Today's straight
    alpha wrongly scales highlights by α.
  - Cap transparent surfaces at specular-only with eight lights.
- **Fallbacks:**
  1. The default path.
  2. Compatibility-class adapters: keep fragment storage buffers at four or fewer
     and workgroups at 64. Where storage buffers or compute are missing, use
     uniform buffers with CPU-built masks. Choose another HDR format, or drop
     MSAA, where multisampled `rgba16float` is missing.
  3. Today's nearest-N lights.

  At runtime, step down in this order based on measured frame time: render scale,
  then shadowed lights, then visible lights.

## Test plan

- **Scenes.**
  - Fixed poses through the headless capture tool, with synthetic lights matching
    imported maps. The local OGZ inventories have 209–469 lights per map, with a
    median radius of 9 m, p90 17 m, and max 64 m.
  - Sweep world lights N ∈ {0, 32, 128, 512, 2048}, visible V ∈ {16, 64, 128, 256},
    and shadowed S ∈ {0, 4, 12, 24}.
  - Layouts:
    - an open lattice;
    - 64 lights in a room around the camera;
    - a corridor;
    - a scene heavy with water and glass;
    - an edit storm that invalidates everything;
    - a teleport.
  - Run with MSAA on and off, at 1080p, 0.75 render scale, and 4K.
- **Measurement.**
  - Put `TIMESTAMP_QUERY` only at pass boundaries; inside-pass timestamps are
    native-only and absent on tile GPUs.
  - Time shadow pages, sun, culling, pre-pass, main, transparent, and post, with
    triple-buffered readback.
  - Chrome quantizes timestamps to 100 µs unless
    `#enable-webgpu-developer-features` is on. Web runs need that flag or need to
    repeat passes to amplify the signal.
  - Log CPU encoding time, faces per frame, cache hit rate, lights per cluster
    (p50/p95/max), and memory.
- **Targets.**
  - Frame time ≤14 ms at p95.
  - Shadows ≤1.5 ms on average and ≤3 ms at p99.
  - Culling ≤0.1 ms; transparency ≤1 ms.
  - Run on this M3 Pro natively, in Chrome, and in Safari, plus a named
    integrated-GPU Windows laptop in Chrome.
- **Correctness.**
  - Compare clustered output with a brute-force all-lights render and with
    CPU-built masks.
  - Test shadow invalidation: an edit in range re-renders the tile; one out of
    range doesn't.
  - Validate every assembled shader with Naga.

## Risks

- The load/store cost of the atlas on tile GPUs and the cost of sampling two maps
  for the actor overlay are unmeasured **[U]**.
- Imported lights with radius 0 or 50–64 m touch every cluster, so they need a
  per-light policy at import. Tesseract treats radius ≤0 as world-sized;
  Sauerbraten's semantics are unverified.
- `PropRenderer::prepare` copies instances per light (capacity ×9). It must become
  on-demand lists for dirty tiles.
- Web timestamps are coarse, and transparent sorting stays centroid-based.
- Renderer stage 1's shadow cache should be keyed by tile and version, not by
  per-light arrays, so stage 2 can extend it.

## Sources

1. MDN GPUSupportedLimits; WebGPU spec, limits; wgpu-types 24 `Limits`.
2. MDN `copyTextureToTexture`; Chrome WebGPU developer features; wgpu 24 `Features`.
3. Olsson et al., *Clustered Deferred and Forward Shading*.
4. Persson, *Practical Clustered Shading*.
5. Courrèges, *DOOM (2016) graphics study*.
6. Apple, *Tailor your apps for Apple GPUs and tile-based deferred rendering*.
7. Arm, *Deferred shading on mobile*.
8. Dawn pixel local storage; Chrome 146 WebGPU notes; wgpu changelog; Unity
   `RenderBufferLoadAction`.
9. Godot and Flax shadow documentation.
10. Tesseract README and `renderlights.cpp`.
11. Bevy clustering sources and many-lights example; wgpu `shadow` example;
    Babylon/Naga `ptr<storage>` report.
12. Osman, dual-paraboloid shadow maps.
13. Cambridge tetrahedral shadow-map summary.
14. Papaioannou, *Radiance Hints*.
15. Dimitrov, *Cascaded Shadow Maps*.
16. WebGPU compatibility-mode proposal.
